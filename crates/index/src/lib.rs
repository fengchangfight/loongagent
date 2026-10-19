//! Purpose: Answer tree, search and count queries from a disposable SQLite copy of the JSONL log.
//!
//! # Contract
//! - **Serves:** fast navigation of a session without ever becoming the truth (events-storage.md §4).
//! - **Design:** `docs/design/events-storage.md` (ADR-0003, ADR-0015).
//! - **Upstream:** the kernel inserts each event after appending it, and rebuilds the index from the log.
//! - **Downstream:** `loong_eventlog::replay`, `rusqlite`, and `loong_schema::generated::EventEnvelope`.
//! - **Verify:** `cargo test -p loong-index` · `./scripts/check.sh`
//!
//! Derived, disposable SQLite index over the JSONL truth (ADR-0003).
//!
//! The index answers the questions JSONL is bad at — *tree navigation, search,
//! statistics* — and nothing else. It is **never** the source of truth: delete
//! the file and [`Index::rebuild`] reconstitutes it from the log. A test asserts
//! exactly that, and another asserts that an event appended after the build is
//! absent from the index until it is inserted.

use loong_eventlog::{replay, LogError};
use loong_schema::generated::EventEnvelope;
use rusqlite::{params, Connection};
use std::path::Path;

#[derive(Debug)]
/// Purpose: Why an index query or rebuild failed: SQLite, the log, or payload encoding.
///
/// Serves: one error type at the index boundary (events-storage.md §4).
/// Design: `docs/design/events-storage.md` (ADR-0003).
/// Up: every `Index` method returns it to the kernel.
/// Down: `rusqlite::Error`, `LogError` and `serde_json::Error`.
/// Verify: `cargo test -p loong-index`.
pub enum IndexError {
    Sql(rusqlite::Error),
    Log(LogError),
    Encode(serde_json::Error),
}

impl std::fmt::Display for IndexError {
    /// Purpose: Render an index failure as one line.
    ///
    /// Contract: `crates/index/src/lib.rs`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexError::Sql(e) => write!(f, "sqlite: {e}"),
            IndexError::Log(e) => write!(f, "log: {e}"),
            IndexError::Encode(e) => write!(f, "encode: {e}"),
        }
    }
}

impl std::error::Error for IndexError {}

impl From<rusqlite::Error> for IndexError {
    /// Purpose: Turn a SQLite failure into `IndexError::Sql`.
    ///
    /// Contract: `crates/index/src/lib.rs`.
    fn from(e: rusqlite::Error) -> Self {
        IndexError::Sql(e)
    }
}
impl From<LogError> for IndexError {
    /// Purpose: Turn a log failure into `IndexError::Log`.
    ///
    /// Contract: `crates/index/src/lib.rs`.
    fn from(e: LogError) -> Self {
        IndexError::Log(e)
    }
}
impl From<serde_json::Error> for IndexError {
    /// Purpose: Turn a payload encoding failure into `IndexError::Encode`.
    ///
    /// Contract: `crates/index/src/lib.rs`.
    fn from(e: serde_json::Error) -> Self {
        IndexError::Encode(e)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Purpose: One indexed event without its payload parsed: the row `search` and `children` return.
///
/// Serves: a query result the kernel can pass on without re-reading the log (events-storage.md §4).
/// Design: `docs/design/events-storage.md` (ADR-0003).
/// Up: `search` and `children`.
/// Down: the `events` table columns and their row decoding.
/// Verify: `cargo test -p loong-index`.
pub struct EventRow {
    pub seq: u64,
    pub kind: String,
    pub ignorable: bool,
    pub time_ms: i64,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Purpose: One event kind and how many events carry it.
///
/// Serves: the only aggregate the index offers today (events-storage.md §4).
/// Design: `docs/design/events-storage.md` (ADR-0003).
/// Up: `kinds`.
/// Down: the `GROUP BY kind` query.
/// Verify: `cargo test -p loong-index`.
pub struct KindCount {
    pub kind: String,
    pub count: u64,
}

/// Purpose: A SQLite connection over one session's events, disposable by design.
///
/// Serves: tree navigation, substring search and counts without becoming the truth (events-storage.md §4).
/// Design: `docs/design/events-storage.md` (ADR-0003, ADR-0015).
/// Up: the kernel when it builds, inserts, searches and rebuilds.
/// Down: the `events` table and the log that `fill` replays.
/// Verify: `cargo test -p loong-index`.
pub struct Index {
    conn: Connection,
}

impl Index {
    /// Open (or create) an index file. An existing, stale, or corrupt index is
    /// not a problem in principle — call [`Index::rebuild_into`] to replace it.
    /// Purpose: Open or create an index file and ensure its table and indexes exist.
    ///
    /// Serves: a caller that may reuse or rebuild a stale index at any time (events-storage.md §4).
    /// Design: `docs/design/events-storage.md` (ADR-0003).
    /// Up: `rebuild_into`; the kernel reaches the index through it.
    /// Down: `Connection::open` and `Index::init`.
    /// Verify: `cargo test -p loong-index`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, IndexError> {
        let conn = Connection::open(path)?;
        let index = Self { conn };
        index.init()?;
        Ok(index)
    }

    /// Purpose: Open an index in memory, for a build that needs no file.
    ///
    /// Serves: a disposable index that needs no cleanup (events-storage.md §4).
    /// Design: `docs/design/events-storage.md` (ADR-0003).
    /// Up: `rebuild`.
    /// Down: `Connection::open_in_memory` and `Index::init`.
    /// Verify: `cargo test -p loong-index`.
    pub fn in_memory() -> Result<Self, IndexError> {
        let index = Self {
            conn: Connection::open_in_memory()?,
        };
        index.init()?;
        Ok(index)
    }

    /// Build a fresh in-memory index from a log.
    /// Purpose: Build a fresh in-memory index from a log.
    ///
    /// Serves: the proof that the index is disposable: delete it and replay the JSONL (events-storage.md §4).
    /// Design: `docs/design/events-storage.md` (ADR-0003).
    /// Up: tests and callers that need a read-only view.
    /// Down: `in_memory` and `fill`.
    /// Verify: `cargo test -p loong-index`.
    pub fn rebuild(log: impl AsRef<Path>) -> Result<Self, IndexError> {
        let mut index = Self::in_memory()?;
        index.fill(log)?;
        Ok(index)
    }

    /// Replace the contents of an on-disk index from a log.
    /// Purpose: Replace an on-disk index's rows from a log.
    ///
    /// Serves: a rebuild after a stale index or a failed insert (events-storage.md §4).
    /// Design: `docs/design/events-storage.md` (ADR-0003).
    /// Up: the kernel's `rebuild_index` and its build path.
    /// Down: `open`, `fill` and the `DELETE FROM events` statement.
    /// Verify: `cargo test -p loong-index`.
    pub fn rebuild_into(path: impl AsRef<Path>, log: impl AsRef<Path>) -> Result<Self, IndexError> {
        let mut index = Self::open(path)?;
        index.conn.execute("DELETE FROM events", [])?;
        index.fill(log)?;
        Ok(index)
    }

    /// Purpose: Create the `events` table and the kind and parent indexes if they are absent.
    ///
    /// Serves: an index that can be created from nothing (events-storage.md §4).
    /// Design: `docs/design/events-storage.md` (ADR-0003).
    /// Up: `open` and `in_memory`.
    /// Down: the `execute_batch` schema statement.
    /// Verify: `cargo test -p loong-index`.
    fn init(&self) -> Result<(), IndexError> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS events (
                 seq       INTEGER PRIMARY KEY,
                 id        TEXT NOT NULL UNIQUE,
                 parent_id TEXT,
                 kind      TEXT NOT NULL,
                 ignorable INTEGER NOT NULL,
                 time_ms   INTEGER NOT NULL,
                 data      TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS events_kind   ON events(kind);
             CREATE INDEX IF NOT EXISTS events_parent ON events(parent_id);",
        )?;
        Ok(())
    }

    /// Purpose: Replay a log strictly and insert every event inside one transaction.
    ///
    /// Serves: an index rebuilt from the record alone (events-storage.md §4).
    /// Design: `docs/design/events-storage.md` (ADR-0003).
    /// Up: `rebuild` and `rebuild_into`.
    /// Down: `loong_eventlog::replay` and the `INSERT OR REPLACE` statement.
    /// Verify: `cargo test -p loong-index`.
    fn fill(&mut self, log: impl AsRef<Path>) -> Result<(), IndexError> {
        let replayed = replay(log)?;
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO events (seq, id, parent_id, kind, ignorable, time_ms, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for env in &replayed.events {
                stmt.execute(params![
                    env.seq as i64,
                    env.id,
                    env.parent_id,
                    env.kind,
                    env.ignorable as i64,
                    env.time_ms,
                    serde_json::to_string(&env.data)?,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Insert one event (the append path uses this alongside the JSONL write).
    /// Purpose: Insert or replace one event after it was written to the JSONL.
    ///
    /// Serves: keeping the derived copy close to the truth on the append path (events-storage.md §4).
    /// Design: `docs/design/events-storage.md` (ADR-0003).
    /// Up: the kernel after `SessionLog::append`.
    /// Down: the `INSERT OR REPLACE` statement.
    /// Verify: `cargo test -p loong-index`.
    pub fn insert(&mut self, env: &EventEnvelope) -> Result<(), IndexError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO events (seq, id, parent_id, kind, ignorable, time_ms, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                env.seq as i64,
                env.id,
                env.parent_id,
                env.kind,
                env.ignorable as i64,
                env.time_ms,
                serde_json::to_string(&env.data)?,
            ],
        )?;
        Ok(())
    }

    /// Purpose: Count the indexed events.
    ///
    /// Serves: a status reader (no in-tree caller); tests use it to pin trailing behaviour.
    /// Design: `docs/design/events-storage.md` (ADR-0003).
    /// Up: a status reader (no in-tree caller).
    /// Down: the `SELECT COUNT(*)` query.
    /// Verify: `cargo test -p loong-index`.
    pub fn count(&self) -> Result<u64, IndexError> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    /// Purpose: Count events grouped by kind, sorted by kind.
    ///
    /// Serves: the compaction check that asks what a session contains (events-storage.md §4).
    /// Design: `docs/design/events-storage.md` (ADR-0003).
    /// Up: `Kernel`'s compaction path.
    /// Down: the `GROUP BY kind` query and `KindCount`.
    /// Verify: `cargo test -p loong-index`.
    pub fn kinds(&self) -> Result<Vec<KindCount>, IndexError> {
        let mut stmt = self
            .conn
            .prepare("SELECT kind, COUNT(*) FROM events GROUP BY kind ORDER BY kind")?;
        let rows = stmt.query_map([], |r| {
            Ok(KindCount {
                kind: r.get(0)?,
                count: r.get::<_, i64>(1)? as u64,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Substring search over event payloads (the JSONL stays the truth; this is
    /// the convenience path the UI uses).
    /// Purpose: Substring-search event payloads and return matching rows in sequence order.
    ///
    /// Serves: the convenience search the kernel offers, while JSONL stays the truth (events-storage.md §4).
    /// Design: `docs/design/events-storage.md` (ADR-0003).
    /// Up: `Kernel::search`.
    /// Down: the `LIKE` query and `row_from`.
    /// Verify: `cargo test -p loong-index`.
    pub fn search(&self, needle: &str) -> Result<Vec<EventRow>, IndexError> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, kind, ignorable, time_ms, data FROM events
             WHERE data LIKE ?1 ORDER BY seq",
        )?;
        let pattern = format!("%{needle}%");
        let rows = stmt.query_map([pattern], row_from)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Children of an event — the event tree inside a session (ADR-0015).
    /// Purpose: List the events whose `parent_id` is the given id, in sequence order.
    ///
    /// Serves: the event tree inside a session (events-storage.md §5).
    /// Design: `docs/design/events-storage.md` (ADR-0003, ADR-0015).
    /// Up: `Kernel::event_children`.
    /// Down: the `parent_id` query and `row_from`.
    /// Verify: `cargo test -p loong-index`.
    pub fn children(&self, parent_id: &str) -> Result<Vec<EventRow>, IndexError> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, kind, ignorable, time_ms, data FROM events
             WHERE parent_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([parent_id], row_from)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

/// Purpose: Decode one SQLite row into an `EventRow`.
///
/// Contract: `crates/index/src/lib.rs`.
fn row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<EventRow> {
    Ok(EventRow {
        seq: r.get::<_, i64>(0)? as u64,
        kind: r.get(1)?,
        ignorable: r.get::<_, i64>(2)? != 0,
        time_ms: r.get(3)?,
        data: r.get(4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use loong_eventlog::SessionLog;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    /// Purpose: Reserve a unique temporary JSONL path for a test.
    ///
    /// Contract: `crates/index/src/lib.rs`.
    fn temp_log() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "loong-index-{}-{}.jsonl",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    /// Purpose: Write three sample events, including one parented event, and return the two ids.
    ///
    /// Serves: a shared fixture for the index tests.
    /// Design: `docs/design/events-storage.md` (ADR-0003).
    /// Up: the rebuild, search and disposable-index tests.
    /// Down: `SessionLog::create`, `append` and `append_with_parent`.
    /// Verify: `cargo test -p loong-index`.
    fn sample_log(path: &Path) -> (String, String) {
        let mut log = SessionLog::create(path).unwrap();
        let first = log
            .append(
                "message.appended",
                json!({"role": "user", "text": "find the widget"}),
            )
            .unwrap();
        log.append("tool.call", json!({"call_id": "c1", "tool": "read_file", "args": {"path": "a"}, "action_class": "investigate"}))
            .unwrap();
        let child = log
            .append_with_parent(
                "message.appended",
                json!({"role": "assistant", "text": "ok"}),
                Some(first.id.clone()),
            )
            .unwrap();
        (first.id, child.id)
    }

    /// Purpose: Pins that a rebuild counts the log and aggregates the kinds.
    #[test]
    fn rebuild_counts_and_aggregates_kinds() {
        let path = temp_log();
        sample_log(&path);
        let index = Index::rebuild(&path).unwrap();
        assert_eq!(index.count().unwrap(), 3);
        let kinds = index.kinds().unwrap();
        assert_eq!(
            kinds[0],
            KindCount {
                kind: "message.appended".into(),
                count: 2
            }
        );
        assert_eq!(
            kinds[1],
            KindCount {
                kind: "tool.call".into(),
                count: 1
            }
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Purpose: Pins that substring search and the parent query work from the derived index.
    #[test]
    fn search_and_tree_navigation_work() {
        let path = temp_log();
        let (first_id, _) = sample_log(&path);
        let index = Index::rebuild(&path).unwrap();
        let hits = index.search("widget").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, "message.appended");
        assert!(hits[0].data.contains("widget"));
        let children = index.children(&first_id).unwrap();
        assert_eq!(
            children.len(),
            1,
            "the parented event is reachable via the tree"
        );
        assert_eq!(children[0].data, r#"{"role":"assistant","text":"ok"}"#);
        let _ = std::fs::remove_file(&path);
    }

    /// Purpose: Pins that deleting the index and rebuilding it from the log loses nothing.
    #[test]
    fn the_index_is_disposable() {
        let path = temp_log();
        sample_log(&path);
        let index_path = path.with_extension("sqlite");
        let _ = std::fs::remove_file(&index_path);
        {
            let i = Index::rebuild_into(&index_path, &path).unwrap();
            assert_eq!(i.count().unwrap(), 3);
        }
        std::fs::remove_file(&index_path).unwrap();
        let rebuilt = Index::rebuild_into(&index_path, &path).unwrap();
        assert_eq!(rebuilt.count().unwrap(), 3, "rebuilds from the log alone");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&index_path);
    }

    /// Purpose: Pins that an event appended after the build appears only after `insert`.
    #[test]
    fn the_index_trails_the_log_until_it_is_told() {
        let path = temp_log();
        let mut log = SessionLog::create(&path).unwrap();
        log.append("message.appended", json!({"role": "user", "text": "one"}))
            .unwrap();
        let mut index = Index::rebuild(&path).unwrap();
        assert_eq!(index.count().unwrap(), 1);

        let second = log
            .append("message.appended", json!({"role": "user", "text": "two"}))
            .unwrap();
        assert_eq!(
            index.count().unwrap(),
            1,
            "the index is derived, not the truth"
        );

        index.insert(&second).unwrap();
        assert_eq!(index.count().unwrap(), 2);
    }
}
