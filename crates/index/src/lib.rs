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
pub enum IndexError {
    Sql(rusqlite::Error),
    Log(LogError),
    Encode(serde_json::Error),
}

impl std::fmt::Display for IndexError {
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
    fn from(e: rusqlite::Error) -> Self {
        IndexError::Sql(e)
    }
}
impl From<LogError> for IndexError {
    fn from(e: LogError) -> Self {
        IndexError::Log(e)
    }
}
impl From<serde_json::Error> for IndexError {
    fn from(e: serde_json::Error) -> Self {
        IndexError::Encode(e)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRow {
    pub seq: u64,
    pub kind: String,
    pub ignorable: bool,
    pub time_ms: i64,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindCount {
    pub kind: String,
    pub count: u64,
}

pub struct Index {
    conn: Connection,
}

impl Index {
    /// Open (or create) an index file. An existing, stale, or corrupt index is
    /// not a problem in principle — call [`Index::rebuild_into`] to replace it.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, IndexError> {
        let conn = Connection::open(path)?;
        let index = Self { conn };
        index.init()?;
        Ok(index)
    }

    pub fn in_memory() -> Result<Self, IndexError> {
        let index = Self {
            conn: Connection::open_in_memory()?,
        };
        index.init()?;
        Ok(index)
    }

    /// Build a fresh in-memory index from a log.
    pub fn rebuild(log: impl AsRef<Path>) -> Result<Self, IndexError> {
        let mut index = Self::in_memory()?;
        index.fill(log)?;
        Ok(index)
    }

    /// Replace the contents of an on-disk index from a log.
    pub fn rebuild_into(path: impl AsRef<Path>, log: impl AsRef<Path>) -> Result<Self, IndexError> {
        let mut index = Self::open(path)?;
        index.conn.execute("DELETE FROM events", [])?;
        index.fill(log)?;
        Ok(index)
    }

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

    pub fn count(&self) -> Result<u64, IndexError> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?;
        Ok(n as u64)
    }

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
    pub fn children(&self, parent_id: &str) -> Result<Vec<EventRow>, IndexError> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, kind, ignorable, time_ms, data FROM events
             WHERE parent_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([parent_id], row_from)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

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

    fn temp_log() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "loong-index-{}-{}.jsonl",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

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
