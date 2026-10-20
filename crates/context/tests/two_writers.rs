//! Purpose: Two writers on one sidecar: the foreground and the enrichment worker must queue, not fail.
//!
//! Serves: the design's two-connection arrangement, with a control showing that the waiting property is the crate's guarantee rather than a driver default.
//!
//! # Contract
//! - **Design:** `docs/context/status.md` §11.1; `docs/context/pipeline.md` §6.6.
//! - **Upstream:** `loong_context::Store` and a raw `rusqlite::Connection` in the control test.
//! - **Downstream:** `Store::put_summary`, the busy-timeout pragma and SQLite's short write transactions.
//! - **Verify:** `cargo test -p loong-context --test two_writers`.

use loong_context::{Store, Summary, SUMMARY_SCHEMA_VERSION};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

static N: AtomicU64 = AtomicU64::new(0);

/// Purpose: A unique temporary database path.
/// Contract: `two_writers.rs` fixtures.
fn temp_db() -> PathBuf {
    std::env::temp_dir().join(format!(
        "loong-context-two-writers-{}-{}.db",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ))
}

/// Purpose: Remove the database and its WAL sidecars.
/// Contract: `two_writers.rs` fixtures.
fn cleanup(path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

/// Purpose: One summary row to write from a named side.
/// Serves: the interleaved writers touching the same id without conflicting shapes.
/// Design: `docs/context/data-model.md` §5.2.
/// Up: The two-writer tests.
/// Down: `Summary` fields only; no other callee.
/// Verify: `cargo test -p loong-context --test two_writers`.
fn row(side: &str, index: usize) -> Summary {
    Summary {
        id: "ep-1".to_string(),
        level: 2,
        scope: "episode:ep-1".to_string(),
        title: format!("{side} {index}"),
        content: format!("L2 · {side} wrote this ({index})"),
        status: "open".to_string(),
        producer: "test".to_string(),
        schema_ver: SUMMARY_SCHEMA_VERSION,
        seq_from: 1,
        seq_to: 1,
        superseded_by: None,
        domain: None,
        tokens: 1,
        created_at: 0,
        updated_at: 0,
        citations: Vec::new(),
        inputs: Vec::new(),
    }
}

/// Purpose: Hold the write lock for a duration, run an attempt while it is held, and return how long the attempt took.
/// Serves: measuring that a `Store` waits rather than failing with `SQLITE_BUSY`.
/// Design: `docs/context/status.md` §11.1.
/// Up: `a_store_writer_waits_instead_of_failing`.
/// Down: A holder thread, `rusqlite::Connection` and the attempt closure.
/// Verify: `cargo test -p loong-context --test two_writers a_store_writer_waits_instead_of_failing`.
/// Hold the write lock for `held_for` and run `attempt` while it is held, returning
/// how long `attempt` took.
fn under_a_held_write_lock(
    path: &Path,
    held_for: Duration,
    attempt: impl FnOnce() -> Result<(), String>,
) -> Duration {
    let holder_path = path.to_path_buf();
    let (locked, is_locked) = channel();
    let holder = std::thread::spawn(move || {
        let conn = Connection::open(&holder_path).unwrap();
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 0;")
            .unwrap();
        // `IMMEDIATE` takes the write lock now rather than at the first write, so the
        // signal below means "the lock is held", not "a statement was prepared".
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('locker', '1')",
            [],
        )
        .unwrap();
        locked.send(()).unwrap();
        std::thread::sleep(held_for);
        conn.execute_batch("COMMIT").unwrap();
    });
    is_locked.recv().unwrap();
    let started = Instant::now();
    let result = attempt();
    let waited = started.elapsed();
    holder.join().unwrap();
    assert!(
        result.is_ok(),
        "the second writer failed: {:?}",
        result.err()
    );
    waited
}

/// Purpose: Pin the control: with no busy timeout the same write fails at once, so the waiting property is not a driver default.
/// The control: with no timeout, the same write fails at once. Without this, the
/// test below could be passing because of a SQLite default rather than because of
/// anything this crate does.
#[test]
fn a_writer_with_no_timeout_fails_immediately() {
    let path = temp_db();
    cleanup(&path);
    // A store to hold the schema, then a raw connection with no timeout of its own.
    let _schema = Store::open(&path).unwrap();
    let unguarded = Connection::open(&path).unwrap();
    unguarded.execute_batch("PRAGMA busy_timeout = 0;").unwrap();

    let held_for = Duration::from_millis(300);
    let holder_path = path.clone();
    let (locked, is_locked) = channel();
    let holder = std::thread::spawn(move || {
        let conn = Connection::open(&holder_path).unwrap();
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 0;")
            .unwrap();
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('locker', '1')",
            [],
        )
        .unwrap();
        locked.send(()).unwrap();
        std::thread::sleep(held_for);
        conn.execute_batch("COMMIT").unwrap();
    });
    is_locked.recv().unwrap();
    let started = Instant::now();
    let failed = unguarded.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('x', '1')",
        [],
    );
    let elapsed = started.elapsed();
    holder.join().unwrap();

    assert!(
        failed.is_err(),
        "SQLite must refuse rather than wait when no timeout is set"
    );
    assert!(
        elapsed < held_for / 2,
        "and it must refuse immediately ({elapsed:?})"
    );
    cleanup(&path);
}

/// Purpose: Pin that a `Store` writer waits out a held lock and then succeeds.
/// The property the engine relies on: a `Store` write waits out the other writer.
#[test]
fn a_store_writer_waits_instead_of_failing() {
    let path = temp_db();
    cleanup(&path);

    let mut waiting = Store::open(&path).unwrap();
    let held_for = Duration::from_millis(300);
    let waited = under_a_held_write_lock(&path, held_for, || {
        waiting
            .put_summary(&row("waiting", 1))
            .map_err(|e| e.to_string())
    });

    assert!(
        waited >= held_for / 2,
        "it waited for the lock ({waited:?} of {held_for:?})"
    );
    assert_eq!(waiting.summary_count(2).unwrap(), 1);
    cleanup(&path);
}

/// Purpose: Pin that interleaved writes from two connections neither error nor duplicate the row.
/// Interleaved writes from two connections: neither may error, and the row must be
/// there exactly once however the races fell.
#[test]
fn two_connections_can_write_the_same_sidecar() {
    let path = temp_db();
    cleanup(&path);

    let mut foreground = Store::open(&path).unwrap();
    let mut worker = Store::open(&path).unwrap();
    let rounds = 200;

    let handle = std::thread::spawn(move || {
        for index in 0..rounds {
            worker.put_summary(&row("worker", index)).unwrap();
        }
    });
    for index in 0..rounds {
        foreground.put_summary(&row("foreground", index)).unwrap();
    }
    handle.join().expect("the worker thread must not panic");

    assert_eq!(foreground.summary_count(2).unwrap(), 1);
    let stored = foreground.summary("ep-1").unwrap().expect("the row exists");
    assert!(
        stored.content.contains("worker") || stored.content.contains("foreground"),
        "{}",
        stored.content
    );
    cleanup(&path);
}
