//! Purpose: Measurements kept in the repository: why retrieval uses `LIKE`, and why staying current does not grow with the log.
//!
//! Serves: the numbers behind the no-FTS and byte-tail decisions, re-runnable rather than remembered.
//!
//! # Contract
//! - **Design:** `docs/context/data-model.md` §5.3; `docs/context/pipeline.md` §6.6.
//! - **Upstream:** `loong_context::{SearchQuery, Store}` and the session fixtures below.
//! - **Downstream:** `Store::rebuild`, `Store::search`, `Store::recall`, `Store::sync` and `Store::digest_pending`.
//! - **Verify:** `cargo test -p loong-context --test search_cost -- --ignored --nocapture`.

use loong_context::{SearchQuery, Store};
use loong_eventlog::SessionLog;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

static N: AtomicU64 = AtomicU64::new(0);

const EVENTS: usize = 20_000;

/// Purpose: A plausible long session: tool calls with paths, tool results with output, and Chinese user messages.
/// Serves: measuring search on the shape of a real coding conversation rather than a synthetic table.
/// Design: `docs/context/data-model.md` §5.3.
/// Up: `like_search_cost_on_a_long_session`.
/// Down: `loong_eventlog::SessionLog`; no other callee.
/// Verify: `cargo test -p loong-context --test search_cost -- --ignored --nocapture`.
/// A plausible long session: tool calls with paths, tool results with output,
/// and Chinese user messages — the shape of a real coding conversation.
fn big_log(path: &PathBuf, events: usize) -> usize {
    let mut log = SessionLog::create(path).unwrap();
    let files = [
        "src/config/merge.rs",
        "src/kernel/turn.rs",
        "crates/context/src/store.rs",
        "docs/06-context-engineering.md",
    ];
    let messages = [
        "配置层叠失效了，帮我查一下",
        "这个 bug 的根因是深拷贝，不是合并顺序",
        "方案 B 通过了测试，方案 A 被否决",
    ];
    let mut atoms = 0;
    for i in 0..events {
        let file = files[i % files.len()];
        let call_id = format!("c{i}");
        log.append(
            "tool.call",
            serde_json::json!({"call_id": call_id, "tool": "read_file",
                               "args": {"path": file}, "action_class": "investigate"}),
        )
        .unwrap();
        atoms += 1;
        log.append(
            "tool.result",
            serde_json::json!({"call_id": call_id, "ok": i % 7 != 0,
                "output": format!("{} lines from {file}; test layer_merge {}", i % 100, if i % 7 != 0 { "ok" } else { "FAILED" })}),
        )
        .unwrap();
        atoms += 1;
        if i % 5 == 0 {
            log.append(
                "message.appended",
                serde_json::json!({"role": "user", "text": messages[i % messages.len()]}),
            )
            .unwrap();
            atoms += 1;
        }
    }
    atoms
}

/// Purpose: Measure `LIKE` search cost on a long session, including the worst case that answers "nothing here".
#[test]
#[ignore = "a measurement, not a gate; run it explicitly"]
fn like_search_cost_on_a_long_session() {
    let path = std::env::temp_dir().join(format!(
        "loong-context-bench-{}-{}.jsonl",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_file(&path);

    let t = Instant::now();
    let atoms = big_log(&path, EVENTS);
    println!(
        "wrote {} events ({} searchable) in {:?}",
        EVENTS * 2,
        atoms,
        t.elapsed()
    );

    let t = Instant::now();
    let mut store = Store::in_memory().unwrap();
    store.rebuild(&path).unwrap();
    println!(
        "ingested {} atoms in {:?}",
        store.atom_count().unwrap(),
        t.elapsed()
    );

    let queries: &[(&str, Vec<&str>)] = &[
        ("2-char Chinese (the FTS5 trigram killer)", vec!["层叠"]),
        ("3-char Chinese", vec!["深拷贝"]),
        ("identifier", vec!["layer_merge"]),
        ("path fragment", vec!["src/config"]),
        ("ASCII, common word", vec!["FAILED"]),
        ("two terms, both present", vec!["层叠", "配置"]),
    ];
    for (label, terms) in queries {
        let query = SearchQuery::new(terms.iter().copied()).limit(8);
        // Warm, then measure; a cold page cache is a different question.
        let _ = store.search(&query).unwrap();
        let runs = 20;
        let t = Instant::now();
        let mut hits = 0;
        for _ in 0..runs {
            hits += store.search(&query).unwrap().len();
        }
        let each = t.elapsed() / runs;
        println!(
            "{label:42} {each:>10.2?}  ({} hits/query)",
            hits / runs as usize
        );
    }

    // The worst case worth knowing: when no row can satisfy every term, the scan
    // cannot stop early. This is the *only* query shape that costs milliseconds,
    // and it is the one that answers "there is nothing here" — so it must stay
    // honest rather than be optimised into an empty fast path.
    let impossible = SearchQuery::new(["层叠", "FAILED"]).limit(8);
    assert!(store.search(&impossible).unwrap().is_empty());
    let t = Instant::now();
    for _ in 0..20 {
        let _ = store.search(&impossible).unwrap();
    }
    println!(
        "{:42} {:>10.2?}  (worst case: no row can match all terms)",
        "two terms, cannot co-occur",
        t.elapsed() / 20
    );

    // The disk question the design note cares about: how big is the derived
    // store next to the log it was derived from?
    let log_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    println!(
        "log is {:.1} MB for {} events (atoms are length-capped, so the store stays proportional)",
        log_bytes as f64 / 1e6,
        EVENTS * 2
    );

    let db = path.with_extension("jsonl.context.db");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&db);
}

/// Purpose: Measure summary recall over 4,000 summaries, the evidence behind having no full-text table.
/// The measurement behind *not* having a full-text table over summaries.
///
/// An FTS5 trigram index over summaries is faster per query (0.12 ms vs 1-2 ms at
/// 5,000 summaries) but costs a whole class of correctness traps, so the call was
/// made on the numbers rather than on the principle. Re-run this if a session ever
/// holds enough summaries for a scan to matter — that is the evidence that should
/// reverse the decision, and nothing else.
#[test]
#[ignore = "a measurement, not a gate; run it explicitly"]
fn summary_recall_cost() {
    let path = std::env::temp_dir().join(format!(
        "loong-context-recall-bench-{}-{}.jsonl",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_file(&path);
    console_session(&path, 4_000);
    let mut store = Store::in_memory().unwrap();
    store.rebuild(&path).unwrap();

    let t = Instant::now();
    let written = store.digest().unwrap();
    println!(
        "digested {written} episodes from {} atoms in {:?}",
        store.atom_count().unwrap(),
        t.elapsed()
    );

    for (label, terms) in [
        ("2-char Chinese (the FTS5 trigram killer)", vec!["层叠"]),
        ("3-char Chinese", vec!["深拷贝"]),
        ("two terms, both present", vec!["层叠", "配置"]),
        ("absent term (worst case)", vec!["zzzz"]),
    ] {
        let query = SearchQuery::new(terms).limit(8);
        let _ = store.recall(&query).unwrap();
        let runs = 20;
        let t = Instant::now();
        let mut hits = 0;
        for _ in 0..runs {
            hits += store.recall(&query).unwrap().len();
        }
        println!(
            "{label:42} {:>10.2?}  ({} hits/query over {} summaries)",
            t.elapsed() / runs,
            hits / runs as usize,
            store.summary_count(2).unwrap()
        );
    }
    let _ = std::fs::remove_file(&path);
}

/// Purpose: A long session of many small tasks, so the digest has thousands of episodes.
/// Serves: measuring recall and tail cost at a realistic session length.
/// Design: `docs/context/data-model.md` §5.3.
/// Up: `summary_recall_cost` and `staying_current_does_not_grow_with_the_log`.
/// Down: `loong_eventlog::SessionLog`; no other callee.
/// Verify: `cargo test -p loong-context --test search_cost -- --ignored --nocapture`.
/// A long session of many small tasks, so the digest has thousands of episodes to
/// work with rather than one enormous one.
fn console_session(path: &PathBuf, tasks: usize) {
    let mut log = SessionLog::create(path).unwrap();
    for i in 0..tasks {
        let file = [
            "src/config/merge.rs",
            "src/kernel/turn.rs",
            "crates/context/src/store.rs",
        ][i % 3];
        log.append(
            "message.appended",
            serde_json::json!({"role": "user", "text": format!("第 {i} 个任务：配置层叠失效了，帮我查一下")}),
        )
        .unwrap();
        log.append(
            "tool.call",
            serde_json::json!({"call_id": format!("c{i}"), "tool": "edit_file",
                "args": {"path": file, "new_string": "x"}, "action_class": "write_workspace"}),
        )
        .unwrap();
        log.append(
            "tool.result",
            serde_json::json!({"call_id": format!("c{i}"), "ok": false,
                "output": format!("test layer_merge_{i} ... FAILED")}),
        )
        .unwrap();
        log.append(
            "tool.call",
            serde_json::json!({"call_id": format!("t{i}"), "tool": "run_command",
                "args": {"command": format!("cargo test layer_merge_{i}")}, "action_class": "investigate"}),
        )
        .unwrap();
        log.append(
            "tool.result",
            serde_json::json!({"call_id": format!("t{i}"), "ok": true,
                "output": format!("test layer_merge_{i} ... ok")}),
        )
        .unwrap();
    }
}

/// Purpose: Measure that staying current costs what was appended, not the length of the log.
/// The claim the resident worker rests on: staying current costs a function of
/// what was *appended*, not of how long the session has been running. If this
/// regresses, a worker on a long session becomes a worker that re-reads it, which
/// is the thing `tail.rs` exists to prevent.
///
/// Run with `cargo test -p loong-context --test search_cost -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, not a gate; run it explicitly"]
fn staying_current_does_not_grow_with_the_log() {
    use loong_context::{Store, SyncOutcome};

    let path = std::env::temp_dir().join(format!(
        "loong-context-tail-bench-{}-{}.jsonl",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_file(&path);

    // A long session, then keep it current one task at a time.
    console_session(&path, 2_000);
    let mut store = Store::in_memory().unwrap();
    let t = Instant::now();
    store.rebuild(&path).unwrap();
    store.digest().unwrap();
    println!(
        "cold build: {} atoms, {} summaries in {:?} (log is {:.1} MB)",
        store.atom_count().unwrap(),
        store.summary_count(2).unwrap(),
        t.elapsed(),
        std::fs::metadata(&path).unwrap().len() as f64 / 1e6
    );

    // Nothing appended: a look is a stat.
    let t = Instant::now();
    for _ in 0..100 {
        assert_eq!(store.sync(256).unwrap(), SyncOutcome::Idle);
    }
    println!("idle look: {:>10.3?}  (nothing to read)", t.elapsed() / 100);

    // One more task appended to a log that is already long.
    let before = std::fs::metadata(&path).unwrap().len();
    append_one_task(&path, 99_999);
    let appended = std::fs::metadata(&path).unwrap().len() - before;
    let t = Instant::now();
    let outcome = store.sync(256).unwrap();
    let elapsed = t.elapsed();
    let changed = store.digest_pending(4).unwrap();
    println!(
        "one task appended ({appended} bytes) to a {:.1} MB log: sync {:?} + digest {changed} in {:?}",
        before as f64 / 1e6,
        outcome,
        elapsed + t.elapsed()
    );
    assert!(matches!(outcome, SyncOutcome::Synced { .. }));

    let _ = std::fs::remove_file(&path);
}

/// Purpose: Append one task's events to an existing log.
/// Serves: measuring the incremental cost of one appended task on an already-long session.
/// Design: `docs/context/pipeline.md` §6.6.
/// Up: `staying_current_does_not_grow_with_the_log`.
/// Down: `loong_eventlog::SessionLog`; no other callee.
/// Verify: `cargo test -p loong-context --test search_cost -- --ignored --nocapture`.
/// Append a single task's events to an existing log, for the incremental test.
fn append_one_task(path: &PathBuf, index: usize) {
    let mut log = SessionLog::open(path).unwrap();
    log.append(
        "message.appended",
        serde_json::json!({"role": "user", "text": format!("增量任务 {index}：配置层叠失效了")}),
    )
    .unwrap();
    log.append(
        "tool.call",
        serde_json::json!({"call_id": format!("e{index}"), "tool": "edit_file",
            "args": {"path": "src/config/merge.rs", "new_string": "x"}, "action_class": "write_workspace"}),
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
        serde_json::json!({"call_id": format!("t{index}"), "ok": true,
            "output": format!("test layer_merge_{index} ... ok")}),
    )
    .unwrap();
}
