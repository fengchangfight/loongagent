//! Purpose: Write and read the append-only JSONL event log that is one session's source of truth.
//!
//! # Contract
//! - **Serves:** durable session history that survives a restart and stays readable by any tool (events-storage.md §3).
//! - **Design:** `docs/design/events-storage.md` (ADR-0003, ADR-0015).
//! - **Upstream:** the kernel appends every event; the kernel and `loong-index` replay the log.
//! - **Downstream:** `EventEnvelope` and `is_ignorable_event` from `loong-schema`, and the JSONL file itself.
//! - **Verify:** `cargo test -p loong-eventlog` · `./scripts/check.sh`
//!
//! Append-only JSONL event log — the source of truth (ADR-0003).
//!
//! Two rules from the decision record are enforced here, not documented and
//! hoped for:
//!
//! 1. **The envelope is authoritative for unknown kinds.** A reader that meets a
//!    `kind` it does not know may skip the record when `ignorable` is true, and
//!    MUST fail loudly when it is false — never render a session it does not
//!    understand.
//! 2. **Known kinds must agree with the schema.** If a record's `ignorable`
//!    marker contradicts what the schema declares for that kind, the log is
//!    inconsistent and reading fails rather than guessing.
//!
//! The SQLite index lives in a later crate; this one stays dependency-light so
//! the truth is readable by `cat` and by any tool.

use loong_schema::generated::{is_ignorable_event, EventEnvelope, Json, Seq};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Why a log could not be read or written.
#[derive(Debug)]
/// Purpose: Why a log could not be read or written, with the line that failed.
///
/// Serves: a reader that refuses to guess when a session is inconsistent (events-storage.md §3).
/// Design: `docs/design/events-storage.md` (ADR-0003).
/// Up: `SessionLog` and `replay` return it to the kernel and the index.
/// Down: the JSONL line and the schema's ignorable marker it describes.
/// Verify: `cargo test -p loong-eventlog`.
pub enum LogError {
    Io(std::io::Error),
    /// The line is not valid JSON, or not a valid event envelope.
    Malformed {
        line: usize,
        detail: String,
    },
    /// Unknown kind that the envelope says must not be ignored.
    UnknownCoreEvent {
        line: usize,
        kind: String,
    },
    /// A known kind whose `ignorable` marker contradicts the schema.
    InconsistentEnvelope {
        line: usize,
        kind: String,
        declared: bool,
        found: bool,
    },
    /// Sequence numbers must be strictly increasing from 1.
    SeqOutOfOrder {
        line: usize,
        expected: Seq,
        found: Seq,
    },
}

impl std::fmt::Display for LogError {
    /// Purpose: Render a log failure as the one line a caller or a test reads.
    ///
    /// Serves: an error that names the line and the disagreement, not just "invalid".
    /// Design: `docs/design/events-storage.md` (ADR-0003).
    /// Up: `Display` for `LogError`, used by the kernel and the index when a read fails.
    /// Down: the fields of `LogError`.
    /// Verify: `cargo test -p loong-eventlog`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogError::Io(e) => write!(f, "io: {e}"),
            LogError::Malformed { line, detail } => {
                write!(f, "line {line}: malformed event: {detail}")
            }
            LogError::UnknownCoreEvent { line, kind } => write!(
                f,
                "line {line}: unknown event kind {kind:?} is not ignorable — refusing to read a session we do not understand"
            ),
            LogError::InconsistentEnvelope { line, kind, declared, found } => write!(
                f,
                "line {line}: event kind {kind:?} declares ignorable={declared} in the schema but the record says {found}"
            ),
            LogError::SeqOutOfOrder { line, expected, found } => {
                write!(f, "line {line}: expected seq {expected}, found {found}")
            }
        }
    }
}

impl std::error::Error for LogError {}

impl From<std::io::Error> for LogError {
    /// Purpose: Turn an I/O failure into `LogError::Io`.
    ///
    /// Contract: `crates/eventlog/src/lib.rs`.
    fn from(e: std::io::Error) -> Self {
        LogError::Io(e)
    }
}

/// A record that a lenient reader skipped because it did not understand it.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Purpose: One record a lenient reader skipped because it did not understand the kind.
///
/// Serves: visibility into what a lenient replay dropped (events-storage.md §3).
/// Design: `docs/design/events-storage.md` (ADR-0003).
/// Up: `Replay::skipped`, read by a surface replaying a session.
/// Down: the line number and kind of the skipped envelope.
/// Verify: `cargo test -p loong-eventlog`.
pub struct Skipped {
    pub line: usize,
    pub kind: String,
}

/// Purpose: Read the wall clock in Unix milliseconds for an envelope's `time_ms`.
///
/// Contract: `crates/eventlog/src/lib.rs`.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// What a replay produced.
#[derive(Debug, Default)]
/// Purpose: What one replay produced: the events it kept and the unknown ignorable ones it skipped.
///
/// Serves: a caller that can tell a complete read from a lossy one (events-storage.md §3).
/// Design: `docs/design/events-storage.md` (ADR-0003).
/// Up: `replay`, `replay_lenient`, `SessionLog::open` and the index's `fill`.
/// Down: `EventEnvelope` and `Skipped`.
/// Verify: `cargo test -p loong-eventlog`.
pub struct Replay {
    pub events: Vec<EventEnvelope>,
    pub skipped: Vec<Skipped>,
}

/// An append-only event log for one session.
#[derive(Debug)]
/// Purpose: The append-only writer for one session's JSONL file, with its next sequence and id table.
///
/// Serves: the record that makes a session survive a restart (events-storage.md §3).
/// Design: `docs/design/events-storage.md` (ADR-0003).
/// Up: the kernel, which appends every event and reopens the log on restart.
/// Down: the JSONL file, `EventEnvelope` and the schema's ignorable marker.
/// Verify: `cargo test -p loong-eventlog`.
pub struct SessionLog {
    path: PathBuf,
    file: File,
    next_seq: Seq,
    /// seq -> id, for parent lookups.
    ids: BTreeMap<Seq, String>,
}

impl SessionLog {
    /// Create a new log, failing if the path exists (never silently clobber a
    /// session).
    /// Purpose: Create a new log with `create_new`, so an existing session is never clobbered.
    ///
    /// Serves: append-only truth: a second session cannot overwrite the first file (events-storage.md §3).
    /// Design: `docs/design/events-storage.md` (ADR-0003).
    /// Up: the session-creation path in the kernel.
    /// Down: `OpenOptions::create_new` and the `SessionLog` fields.
    /// Verify: `cargo test -p loong-eventlog`.
    pub fn create(path: impl AsRef<Path>) -> Result<Self, LogError> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&path)?;
        Ok(Self {
            path,
            file,
            next_seq: 1,
            ids: BTreeMap::new(),
        })
    }

    /// Open an existing log for appending, replaying it first so the next
    /// sequence number and the id table are correct.
    /// Purpose: Open an existing log for appending after replaying it to recover the next sequence and ids.
    ///
    /// Serves: sequence continuity across a restart (events-storage.md §3).
    /// Design: `docs/design/events-storage.md` (ADR-0003).
    /// Up: the kernel when a session is reopened.
    /// Down: `replay` and the `SessionLog` fields.
    /// Verify: `cargo test -p loong-eventlog`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, LogError> {
        let path = path.as_ref().to_path_buf();
        let replay = replay(&path)?;
        let next_seq = replay.events.last().map(|e| e.seq + 1).unwrap_or(1);
        let ids = replay
            .events
            .iter()
            .map(|e| (e.seq, e.id.clone()))
            .collect();
        let file = OpenOptions::new().append(true).open(&path)?;
        Ok(Self {
            path,
            file,
            next_seq,
            ids,
        })
    }

    /// Purpose: Report the file this log writes.
    ///
    /// Serves: a caller that needs the session file's path for its sibling index or for a delete.
    /// Design: `docs/design/events-storage.md` (ADR-0003).
    /// Up: `Kernel`'s session and build paths, which pair the log with its index.
    /// Down: the `path` field.
    /// Verify: `cargo test -p loong-eventlog`.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Purpose: Report the highest sequence number written so far.
    ///
    /// Serves: the attach handshake's existing-event count and a branch's fork point.
    /// Design: `docs/design/events-storage.md` (ADR-0003, ADR-0015).
    /// Up: `Kernel::last_seq` and the server's `session.attach` answer.
    /// Down: the `next_seq` field.
    /// Verify: `cargo test -p loong-eventlog`.
    pub fn last_seq(&self) -> Seq {
        self.next_seq - 1
    }

    /// Append an event. `ignorable` comes from the schema, so a writer cannot
    /// mislabel a core event.
    /// Purpose: Append one event, taking the ignorable marker from the schema.
    ///
    /// Serves: the write half of the JSONL truth, where a writer cannot mislabel a core event (events-storage.md §3).
    /// Design: `docs/design/events-storage.md` (ADR-0003).
    /// Up: the kernel's append path for every durable event.
    /// Down: `append_with_parent` and `is_ignorable_event`.
    /// Verify: `cargo test -p loong-eventlog`.
    pub fn append(&mut self, kind: &str, data: Json) -> Result<EventEnvelope, LogError> {
        self.append_with_parent(kind, data, None)
    }

    /// Append an event that continues from a specific earlier event (the event
    /// tree inside a session, ADR-0015).
    /// Purpose: Append an event that continues from an earlier one, refusing a dangling parent.
    ///
    /// Serves: the event tree inside a session, validated before it is written (events-storage.md §5).
    /// Design: `docs/design/events-storage.md` (ADR-0003, ADR-0015).
    /// Up: tests today; production `append` passes `None`.
    /// Down: the id table, `EventEnvelope` and the JSONL file.
    /// Verify: `cargo test -p loong-eventlog`.
    pub fn append_with_parent(
        &mut self,
        kind: &str,
        data: Json,
        parent_id: Option<String>,
    ) -> Result<EventEnvelope, LogError> {
        if let Some(p) = &parent_id {
            if !self.ids.values().any(|id| id == p) {
                return Err(LogError::Malformed {
                    line: 0,
                    detail: format!("parent {p:?} is not in this log"),
                });
            }
        }
        let seq = self.next_seq;
        let env = EventEnvelope {
            seq,
            id: format!("e{seq:08}"),
            parent_id,
            kind: kind.to_string(),
            // A kind we know gets its declared marker; an unknown kind is the
            // caller's business (default: ignorable) and is validated on read.
            ignorable: is_ignorable_event(kind).unwrap_or(true),
            time_ms: now_ms(),
            data,
        };
        let mut line = serde_json::to_string(&env).map_err(|e| LogError::Malformed {
            line: 0,
            detail: e.to_string(),
        })?;
        line.push('\n');
        self.file.write_all(line.as_bytes())?;
        self.file.flush()?;
        self.ids.insert(seq, env.id.clone());
        self.next_seq = seq + 1;
        Ok(env)
    }
}

/// Read every event strictly: unknown non-ignorable kinds are an error.
/// Purpose: Read every event strictly, failing on an unknown non-ignorable kind.
///
/// Serves: a reader that must be sure it understood the whole session (events-storage.md §3).
/// Design: `docs/design/events-storage.md` (ADR-0003).
/// Up: `SessionLog::open`, `Index::fill` and the kernel's strict reads.
/// Down: `read`.
/// Verify: `cargo test -p loong-eventlog`.
pub fn replay(path: impl AsRef<Path>) -> Result<Replay, LogError> {
    read(path, false)
}

/// Read every event leniently: unknown *ignorable* kinds are skipped and
/// reported instead of failing. Use this for display; use [`replay`] when the
/// caller must be sure it understood everything.
/// Purpose: Read every event leniently, skipping unknown ignorable kinds and reporting them.
///
/// Serves: a reader that may meet a session written by a newer module (events-storage.md §3).
/// Design: `docs/design/events-storage.md` (ADR-0003).
/// Up: `Kernel::replay_history`, `Kernel::lineage` and surface replay.
/// Down: `read`.
/// Verify: `cargo test -p loong-eventlog`.
pub fn replay_lenient(path: impl AsRef<Path>) -> Result<Replay, LogError> {
    read(path, true)
}

/// Purpose: The shared reader: validate the sequence, the schema's marker and the ignorable rule.
///
/// Serves: one implementation of the strict and lenient read rules.
/// Design: `docs/design/events-storage.md` (ADR-0003).
/// Up: `replay` and `replay_lenient`.
/// Down: `EventEnvelope` decoding, `is_ignorable_event`, `Skipped` and `LogError`.
/// Verify: `cargo test -p loong-eventlog`.
fn read(path: impl AsRef<Path>, lenient: bool) -> Result<Replay, LogError> {
    let file = File::open(path.as_ref())?;
    let mut out = Replay::default();
    let mut expected: Seq = 1;
    for (i, line) in BufReader::new(file).lines().enumerate() {
        let line_no = i + 1;
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let env: EventEnvelope = serde_json::from_str(&line).map_err(|e| LogError::Malformed {
            line: line_no,
            detail: e.to_string(),
        })?;
        if env.seq != expected {
            return Err(LogError::SeqOutOfOrder {
                line: line_no,
                expected,
                found: env.seq,
            });
        }
        expected += 1;
        match is_ignorable_event(&env.kind) {
            // A kind we know: the record must agree with the schema.
            Some(declared) if declared != env.ignorable => {
                return Err(LogError::InconsistentEnvelope {
                    line: line_no,
                    kind: env.kind,
                    declared,
                    found: env.ignorable,
                });
            }
            Some(_) => out.events.push(env),
            // A kind we do not know: the envelope decides.
            None if env.ignorable && lenient => {
                out.skipped.push(Skipped {
                    line: line_no,
                    kind: env.kind,
                });
            }
            None if env.ignorable => out.events.push(env),
            None => {
                return Err(LogError::UnknownCoreEvent {
                    line: line_no,
                    kind: env.kind,
                });
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    /// Purpose: A unique temporary JSONL path that removes itself on drop.
    ///
    /// Serves: tests that need a real file without leaking it.
    /// Design: `docs/design/events-storage.md` (ADR-0003).
    /// Up: every file-backed test in this module.
    /// Down: the process id, an atomic counter and `std::fs::remove_file`.
    /// Verify: `cargo test -p loong-eventlog`.
    struct Temp(PathBuf);
    impl Temp {
        /// Purpose: Reserve a fresh temporary path.
        ///
        /// Contract: `crates/eventlog/src/lib.rs`.
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "loong-eventlog-{}-{}.jsonl",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = std::fs::remove_file(&p);
            Temp(p)
        }
        /// Purpose: Return the temporary path.
        ///
        /// Contract: `crates/eventlog/src/lib.rs`.
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Temp {
        /// Purpose: Remove the temporary file when the fixture goes out of scope.
        ///
        /// Contract: `crates/eventlog/src/lib.rs`.
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// Purpose: Pins that appended events replay in order with contiguous sequence numbers.
    #[test]
    fn append_then_replay_roundtrips() {
        let t = Temp::new();
        let mut log = SessionLog::create(t.path()).unwrap();
        log.append("message.appended", json!({"role": "user", "text": "hi"}))
            .unwrap();
        log.append(
            "tool.call",
            json!({"call_id": "c1", "tool": "read", "args": {}, "action_class": "investigate"}),
        )
        .unwrap();
        let r = replay(t.path()).unwrap();
        assert_eq!(r.events.len(), 2);
        assert!(r.skipped.is_empty());
        assert_eq!(r.events[0].seq, 1);
        assert_eq!(r.events[1].seq, 2);
        assert_eq!(r.events[0].kind, "message.appended");
        assert!(!r.events[0].ignorable, "core events are never ignorable");
    }

    /// Purpose: Pins that reopening a log continues the sequence instead of restarting it.
    #[test]
    fn reopening_continues_the_sequence() {
        let t = Temp::new();
        {
            let mut log = SessionLog::create(t.path()).unwrap();
            log.append("message.appended", json!({})).unwrap();
        }
        let mut log = SessionLog::open(t.path()).unwrap();
        let e = log.append("message.appended", json!({})).unwrap();
        assert_eq!(e.seq, 2, "sequence continues across reopen");
        assert_eq!(log.last_seq(), 2);
    }

    /// Purpose: Pins that `append_with_parent` stores an existing parent and refuses a dangling one.
    #[test]
    fn parent_must_exist() {
        let t = Temp::new();
        let mut log = SessionLog::create(t.path()).unwrap();
        let first = log.append("message.appended", json!({})).unwrap();
        let child = log
            .append_with_parent("message.appended", json!({}), Some(first.id.clone()))
            .unwrap();
        assert_eq!(child.parent_id.as_deref(), Some(first.id.as_str()));
        assert!(
            log.append_with_parent("message.appended", json!({}), Some("e999".into()))
                .is_err(),
            "a dangling parent is rejected"
        );
    }

    /// Purpose: Pins the one difference between `replay_lenient` and `replay`: an unknown ignorable kind.
    #[test]
    fn unknown_ignorable_event_is_skipped_leniently_and_kept_strictly() {
        let t = Temp::new();
        let mut log = SessionLog::create(t.path()).unwrap();
        log.append("message.appended", json!({})).unwrap();
        drop(log);
        // a future module adds a new kind and marks it ignorable
        append_raw(
            t.path(),
            json!({"seq": 2, "id": "e2", "kind": "future.thing", "ignorable": true, "time_ms": 0, "data": {}}),
        );

        let lenient = replay_lenient(t.path()).unwrap();
        assert_eq!(lenient.events.len(), 1);
        assert_eq!(
            lenient.skipped,
            vec![Skipped {
                line: 2,
                kind: "future.thing".into()
            }]
        );

        let strict = replay(t.path()).unwrap();
        assert_eq!(
            strict.events.len(),
            2,
            "strict readers keep unknown ignorable events"
        );
    }

    /// Purpose: Pins that an unknown non-ignorable kind fails both readers.
    #[test]
    fn unknown_non_ignorable_event_fails_loudly() {
        let t = Temp::new();
        let mut log = SessionLog::create(t.path()).unwrap();
        log.append("message.appended", json!({})).unwrap();
        drop(log);
        append_raw(
            t.path(),
            json!({"seq": 2, "id": "e2", "kind": "future.core", "ignorable": false, "time_ms": 0, "data": {}}),
        );
        match replay_lenient(t.path()) {
            Err(LogError::UnknownCoreEvent { kind, .. }) => assert_eq!(kind, "future.core"),
            other => panic!("expected UnknownCoreEvent, got {other:?}"),
        }
    }

    /// Purpose: Pins that a record whose `ignorable` contradicts the schema is refused.
    #[test]
    fn envelope_contradicting_the_schema_is_rejected() {
        let t = Temp::new();
        let mut log = SessionLog::create(t.path()).unwrap();
        log.append("message.appended", json!({})).unwrap();
        drop(log);
        // message.appended is declared non-ignorable; a record claiming otherwise
        // must not be trusted.
        append_raw(
            t.path(),
            json!({"seq": 2, "id": "e2", "kind": "message.appended", "ignorable": true, "time_ms": 0, "data": {}}),
        );
        assert!(matches!(
            replay_lenient(t.path()),
            Err(LogError::InconsistentEnvelope { .. })
        ));
    }

    /// Purpose: Pins that a missing sequence number is an error, not a silent gap.
    #[test]
    fn sequence_gaps_are_rejected() {
        let t = Temp::new();
        let mut log = SessionLog::create(t.path()).unwrap();
        log.append("message.appended", json!({})).unwrap();
        drop(log);
        append_raw(
            t.path(),
            json!({"seq": 5, "id": "e5", "kind": "message.appended", "ignorable": false, "time_ms": 0, "data": {}}),
        );
        assert!(matches!(
            replay(t.path()),
            Err(LogError::SeqOutOfOrder {
                expected: 2,
                found: 5,
                ..
            })
        ));
    }

    /// Purpose: Append a raw JSON line, bypassing the writer's validation.
    ///
    /// Contract: `crates/eventlog/src/lib.rs`.
    fn append_raw(path: &Path, value: serde_json::Value) {
        let mut f = OpenOptions::new().append(true).open(path).unwrap();
        writeln!(f, "{value}").unwrap();
    }
}
