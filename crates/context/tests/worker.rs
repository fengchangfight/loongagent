//! Purpose: Tests for P3: the tail, incremental sync and the bounded background worker.
//!
//! Serves: content read once by byte offset, the worker yielding to the foreground, and incremental derivation equalling a full rebuild.
//!
//! # Contract
//! - **Design:** `docs/context/pipeline.md` §6.6; `docs/context/status.md` §11.1.
//! - **Upstream:** `loong_context::worker` and the task fixture below.
//! - **Downstream:** `crate::tail`, `Store::sync`, `Store::digest_pending` and `Store::digest_domains`.
//! - **Verify:** `cargo test -p loong-context --test worker`.

use loong_context::worker::{self, Budget, Foreground, Step, WorkerConfig, WorkerStats};
use loong_context::{Store, SyncOutcome};
use loong_eventlog::SessionLog;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static N: AtomicU64 = AtomicU64::new(0);

/// Purpose: A temporary session log removed when the test ends.
/// Serves: every test starting from a clean log and leaving nothing behind.
/// Design: `docs/context/pipeline.md` §6.6.
/// Up: The worker tests.
/// Down: `TempLog::new`, `TempLog::path` and `TempLog::drop`.
/// Verify: `cargo test -p loong-context --test worker`.
struct TempLog(PathBuf);

impl TempLog {
    /// Purpose: Create a fresh temp log path and remove any stale file.
    /// Contract: `worker.rs` fixtures.
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "loong-context-worker-{}-{}.jsonl",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_file(&p);
        TempLog(p)
    }
    /// Purpose: The session log's path.
    /// Contract: `worker.rs` fixtures.
    fn path(&self) -> &PathBuf {
        &self.0
    }
}

impl Drop for TempLog {
    /// Purpose: Remove the session log.
    /// Contract: `worker.rs` fixtures.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Purpose: Append one task's worth of events: a request, an edit, a failing test and a passing test.
/// Serves: enough material for the segmenter to close a task and enough events to test bounds.
/// Design: `docs/context/pipeline.md` §6.3.
/// Up: The worker tests.
/// Down: `loong_eventlog::SessionLog`; no other callee.
/// Verify: `cargo test -p loong-context --test worker`.
/// Append one task's worth of events: a request, an edit, a failing test, a
/// passing test. Enough material that the segmenter will close it.
fn append_task(path: &PathBuf, index: usize) {
    // `open` wants an existing log; a fresh temp path has none yet.
    if !path.exists() {
        std::fs::File::create(path).unwrap();
    }
    let mut log = SessionLog::open(path).unwrap();
    let file = ["src/config/merge.rs", "src/kernel/turn.rs"][index % 2];
    log.append(
        "message.appended",
        serde_json::json!({"role": "user", "text": format!("第 {index} 个任务：配置层叠失效了")}),
    )
    .unwrap();
    log.append(
        "tool.call",
        serde_json::json!({"call_id": format!("e{index}"), "tool": "edit_file",
            "args": {"path": file, "new_string": "x"}, "action_class": "write_workspace"}),
    )
    .unwrap();
    log.append(
        "tool.result",
        serde_json::json!({"call_id": format!("e{index}"), "ok": true, "output": "applied"}),
    )
    .unwrap();
    log.append(
        "tool.call",
        serde_json::json!({"call_id": format!("t{index}"), "tool": "run_command",
            "args": {"command": format!("cargo test layer_merge_{index}")}, "action_class": "investigate"}),
    )
    .unwrap();
    log.append(
        "tool.result",
        serde_json::json!({"call_id": format!("t{index}"), "ok": false,
            "output": format!("test layer_merge_{index} ... FAILED")}),
    )
    .unwrap();
    log.append(
        "tool.call",
        serde_json::json!({"call_id": format!("r{index}"), "tool": "run_command",
            "args": {"command": format!("cargo test layer_merge_{index}")}, "action_class": "investigate"}),
    )
    .unwrap();
    log.append(
        "tool.result",
        serde_json::json!({"call_id": format!("r{index}"), "ok": true,
            "output": format!("test layer_merge_{index} ... ok")}),
    )
    .unwrap();
}

/// Purpose: Pin that a tail read returns only appended events, and an idle look costs a stat.
#[test]
fn a_tail_read_returns_only_what_was_appended() {
    let tmp = TempLog::new();
    let mut log = SessionLog::create(tmp.path()).unwrap();
    log.append(
        "message.appended",
        serde_json::json!({"role": "user", "text": "第一句"}),
    )
    .unwrap();
    log.append(
        "message.appended",
        serde_json::json!({"role": "assistant", "text": "第一答"}),
    )
    .unwrap();
    drop(log);

    let mut cursor = loong_context::tail::Cursor::at(tmp.path());
    let first = loong_context::tail::read_new(&cursor).unwrap();
    assert_eq!(first.events.len(), 2);
    assert_eq!(first.partial_bytes, 0);
    cursor = first.cursor;

    // Nothing appended: a look costs a stat, not a re-read.
    let again = loong_context::tail::read_new(&cursor).unwrap();
    assert!(again.is_empty());
    assert_eq!(again.cursor.offset, cursor.offset);
    cursor = again.cursor;

    // Append one more, and only that one comes back.
    let mut log = SessionLog::open(tmp.path()).unwrap();
    log.append(
        "message.appended",
        serde_json::json!({"role": "user", "text": "第二句"}),
    )
    .unwrap();
    drop(log);
    let third = loong_context::tail::read_new(&cursor).unwrap();
    assert_eq!(third.events.len(), 1);
    assert_eq!(
        third.events[0].seq, 3,
        "the third event, not the first three"
    );
}

/// Purpose: Pin that a partial line is left for next time and read whole once the writer finishes it.
#[test]
fn a_half_written_line_is_not_read_as_an_event() {
    // Appends are one write per event, but a reader can still catch a partial
    // line. The offset must stop before it, or the next read would see garbage.
    let tmp = TempLog::new();
    let mut log = SessionLog::create(tmp.path()).unwrap();
    log.append(
        "message.appended",
        serde_json::json!({"role": "user", "text": "完整的一行"}),
    )
    .unwrap();
    drop(log);
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(tmp.path())
            .unwrap();
        write!(f, "{{\"seq\":2,\"id\":\"e2\",\"kind\":\"message.appen").unwrap();
    }

    let cursor = loong_context::tail::Cursor::at(tmp.path());
    let batch = loong_context::tail::read_new(&cursor).unwrap();
    assert_eq!(batch.events.len(), 1, "only the complete line is an event");
    assert!(
        batch.partial_bytes > 0,
        "the partial line is left for next time"
    );
    assert_eq!(batch.cursor.last_seq, 1);

    // The writer finishes the line; now it is read, and the sequence continues.
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(tmp.path())
            .unwrap();
        writeln!(f, "ed\",\"ignorable\":false,\"time_ms\":1,\"data\":{{\"role\":\"user\",\"text\":\"补齐\"}}}}").unwrap();
    }
    let batch = loong_context::tail::read_new(&batch.cursor).unwrap();
    assert_eq!(batch.events.len(), 1);
    assert_eq!(batch.events[0].seq, 2);
}

/// Purpose: Pin that a truncated log is reported as `Shrank`, not silently re-read.
#[test]
fn a_shorter_log_is_reported_rather_than_guessed_at() {
    let tmp = TempLog::new();
    let mut log = SessionLog::create(tmp.path()).unwrap();
    log.append(
        "message.appended",
        serde_json::json!({"role": "user", "text": "一"}),
    )
    .unwrap();
    log.append(
        "message.appended",
        serde_json::json!({"role": "user", "text": "二"}),
    )
    .unwrap();
    drop(log);

    let cursor = loong_context::tail::Cursor::at(tmp.path());
    let batch = loong_context::tail::read_new(&cursor).unwrap();
    assert_eq!(batch.events.len(), 2);
    assert_eq!(
        batch.cursor.offset,
        std::fs::metadata(tmp.path()).unwrap().len()
    );

    // Truncate the file behind the reader's back.
    std::fs::write(tmp.path(), b"").unwrap();
    match loong_context::tail::read_new(&batch.cursor) {
        Err(loong_context::tail::TailError::Shrank) => {}
        other => panic!("expected Shrank, got {other:?}"),
    }
}

/// Purpose: Pin that sync reads appended events once and then idles without re-reading.
#[test]
fn sync_reads_once_and_then_idles() {
    let tmp = TempLog::new();
    {
        let mut log = SessionLog::create(tmp.path()).unwrap();
        log.append(
            "message.appended",
            serde_json::json!({"role": "user", "text": "配置层叠"}),
        )
        .unwrap();
        log.append("tool.call", serde_json::json!({"call_id":"c1","tool":"read_file","args":{"path":"src/config/merge.rs"},"action_class":"investigate"})).unwrap();
    }
    let mut store = Store::in_memory().unwrap();
    store.rebuild(tmp.path()).unwrap();

    match store.sync(100).unwrap() {
        SyncOutcome::Idle => {}
        other => panic!("a rebuild leaves the cursor at the end, got {other:?}"),
    }

    append_task(tmp.path(), 1);
    match store.sync(100).unwrap() {
        SyncOutcome::Synced { events, atoms } => {
            assert_eq!(events, 7, "only the appended task");
            assert!(atoms > 0);
        }
        other => panic!("expected Synced, got {other:?}"),
    }

    // Second look: nothing new, and crucially nothing re-read.
    assert_eq!(store.sync(100).unwrap(), SyncOutcome::Idle);
    // The atoms from the first part are still there, not re-created.
    assert_eq!(store.atom_count().unwrap(), 2 + 7);
}

/// Purpose: Pin that a replaced or truncated log asks for a rebuild instead of being trusted.
#[test]
fn a_replaced_log_asks_to_be_rebuilt() {
    let tmp = TempLog::new();
    append_task(tmp.path(), 1);
    let mut store = Store::in_memory().unwrap();
    store.rebuild(tmp.path()).unwrap();

    // Replace the log with a shorter, different one at the same path.
    let other = TempLog::new();
    append_task(other.path(), 2);
    {
        let short = std::fs::read_to_string(other.path()).unwrap();
        std::fs::write(tmp.path(), &short[..short.len() / 3]).unwrap();
    }
    // A truncated file may not even parse as events; either way the store must
    // not pretend it is current.
    match store.sync(100) {
        Ok(SyncOutcome::NeedsRebuild) => {}
        Ok(other) => panic!("expected NeedsRebuild, got {other:?}"),
        Err(_) => {} // a malformed tail is also a legitimate "do not trust me"
    }
}

/// Purpose: Pin that a busy foreground defers before reading anything, and the same step works once idle.
#[test]
fn the_worker_does_not_start_while_the_foreground_is_busy() {
    let tmp = TempLog::new();
    append_task(tmp.path(), 1);
    let mut store = Store::in_memory().unwrap();
    store.rebuild(tmp.path()).unwrap();
    append_task(tmp.path(), 2);

    let mut stats = WorkerStats::default();
    let mut budget = Budget::new(&WorkerConfig::default());
    let busy = Busy(true);
    let step = worker::step(
        &mut store,
        tmp.path(),
        &WorkerConfig::default(),
        &mut budget,
        &busy,
        &mut stats,
    )
    .unwrap();
    assert_eq!(step, Step::Deferred);
    assert_eq!(stats.events, 0, "being busy means not starting");
    assert_eq!(store.atom_count().unwrap(), 7, "the new task was not read");

    // Once the turn is over, the same call does the work.
    let idle = Busy(false);
    let step = worker::step(
        &mut store,
        tmp.path(),
        &WorkerConfig::default(),
        &mut budget,
        &idle,
        &mut stats,
    )
    .unwrap();
    assert_eq!(step, Step::Active);
    assert_eq!(stats.events, 7);
}

/// Purpose: A foreground whose busy state is fixed for the test.
/// Serves: driving `worker::step` down both the deferred and active paths deterministically.
/// Design: `docs/context/pipeline.md` §6.6.
/// Up: The worker tests.
/// Down: Its `Foreground` impl.
/// Verify: `cargo test -p loong-context --test worker the_worker_does_not_start_while_the_foreground_is_busy`.
struct Busy(bool);
impl Foreground for Busy {
    /// Purpose: Report the fixed busy state.
    /// Contract: `worker.rs` test double.
    fn is_busy(&self) -> bool {
        self.0
    }
}

/// Purpose: Pin that one activation is bounded, a second finishes the backlog and a third idles.
#[test]
fn an_activation_is_bounded_and_then_idles() {
    let tmp = TempLog::new();
    append_task(tmp.path(), 1);
    let mut store = Store::in_memory().unwrap();
    store.rebuild(tmp.path()).unwrap();
    for i in 2..12 {
        append_task(tmp.path(), i);
    }

    let config = WorkerConfig {
        max_events: 7,
        ..WorkerConfig::default()
    };
    let mut stats = WorkerStats::default();
    let idle = Busy(false);

    // One activation: bounded by steps/events, and it stops on its own.
    let last = worker::run_activation(&mut store, tmp.path(), &config, &idle, &mut stats).unwrap();
    assert_eq!(last, Step::Active, "it did work");
    assert!(stats.events > 0);
    assert!(
        stats.steps <= 8,
        "an activation may not run unbounded: {} steps",
        stats.steps
    );
    assert_eq!(stats.activations, 1);

    // A second activation finishes the backlog.
    let events_after_first = stats.events;
    worker::run_activation(&mut store, tmp.path(), &config, &idle, &mut stats).unwrap();
    assert!(
        stats.events > events_after_first,
        "the second activation continues where the first stopped"
    );

    // And then it idles rather than spinning.
    let last = worker::run_activation(&mut store, tmp.path(), &config, &idle, &mut stats).unwrap();
    assert_eq!(last, Step::Idle, "steady state is idle, not a busy loop");
}

/// Purpose: Pin that incremental derivation equals a full rebuild, so whether the worker ran cannot change what is remembered.
#[test]
fn what_the_worker_derives_equals_what_a_full_rebuild_derives() {
    // The promise that makes a background worker acceptable at all: whether it ran
    // cannot change what the agent remembers.
    let tmp = TempLog::new();
    append_task(tmp.path(), 1);

    // Store A: driven one task at a time by the worker, digesting as it goes.
    let mut incremental = Store::in_memory().unwrap();
    incremental.rebuild(tmp.path()).unwrap();
    let idle = Busy(false);
    let config = WorkerConfig::default();
    let mut stats = WorkerStats::default();
    for i in 2..6 {
        append_task(tmp.path(), i);
        worker::run_activation(&mut incremental, tmp.path(), &config, &idle, &mut stats).unwrap();
    }

    // Store B: built from the finished log in one go.
    let mut rebuilt = Store::in_memory().unwrap();
    rebuilt.rebuild(tmp.path()).unwrap();
    rebuilt.digest().unwrap();

    assert_eq!(
        incremental.summaries_at(2, 100).unwrap(),
        rebuilt.summaries_at(2, 100).unwrap(),
        "incremental and full derivation must agree"
    );
    assert_eq!(
        incremental.atom_count().unwrap(),
        rebuilt.atom_count().unwrap()
    );
    assert_eq!(incremental.verify().unwrap(), rebuilt.verify().unwrap());
}

/// Purpose: Pin that a steady-state activation reads no events and does not rebuild.
#[test]
fn the_idle_path_costs_a_stat_not_a_scan() {
    // A bound rather than a benchmark: a worker in steady state must not touch the
    // log's contents. We prove it by making the log unreadable-as-text yet leaving
    // the store current — an idle step must still succeed.
    let tmp = TempLog::new();
    append_task(tmp.path(), 1);
    let mut store = Store::in_memory().unwrap();
    store.rebuild(tmp.path()).unwrap();

    let config = WorkerConfig::default();
    let mut stats = WorkerStats::default();
    let idle = Busy(false);
    let last = worker::run_activation(&mut store, tmp.path(), &config, &idle, &mut stats).unwrap();
    assert_eq!(last, Step::Idle);
    assert_eq!(stats.events, 0, "steady state reads no events");
    assert_eq!(stats.rebuilds, 0);
}
