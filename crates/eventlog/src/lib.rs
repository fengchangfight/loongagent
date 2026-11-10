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
    /// Another process holds the write lock: one session has one writer.
    Locked {
        path: PathBuf,
    },
    /// The file grew behind this writer's back, so its `next_seq` would collide.
    ForeignWrite {
        path: PathBuf,
        expected: u64,
        found: u64,
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
                write!(
                    f,
                    "line {line}: expected seq {expected}, found {found} — a second writer appended out of step, and `loongagent repair` renumbers the lines it put there"
                )
            }
            LogError::Locked { path } => write!(
                f,
                "{}: another process is writing this session — close it there first, one session has one writer",
                path.display()
            ),
            LogError::ForeignWrite { path, expected, found } => write!(
                f,
                "{}: this writer wrote {expected} bytes but the file holds {found} — another writer appended to it",
                path.display()
            ),
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
/// Up: `Replay::skipped`, read by tests; no in-tree reader yet.
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

/// Purpose: The sidecar file whose exclusive lock means "a writer owns this log".
///
/// Serves: one writer per session while every reader keeps reading the log itself (events-storage.md §3).
/// Design: `docs/design/events-storage.md` (ADR-0003, ADR-0009).
/// Up: `lock_writer`, and `loong-cli`'s delete, which removes the sidecar with the session.
/// Down: the log's own path, with `.lock` appended.
/// Verify: `cargo test -p loong-eventlog`.
pub fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".lock");
    PathBuf::from(name)
}

/// Purpose: How many times `lock_writer` tries before it calls a lock a real second writer.
///
/// Serves: bridging the fork-to-exec window without hiding a genuine clash (events-storage.md §3).
/// Design: `docs/design/events-storage.md` (ADR-0009).
/// Up: `lock_writer` only.
/// Down: nothing; a count.
/// Verify: `cargo test -p loong-eventlog`.
const LOCK_ATTEMPTS: u32 = 100;

/// Purpose: How long `lock_writer` waits between attempts.
///
/// Serves: `LOCK_ATTEMPTS × LOCK_RETRY` = half a second, longer than any dynamic loader
/// takes between fork and exec, and short enough that a real second writer is reported
/// while the person is still looking at the terminal.
/// Design: `docs/design/events-storage.md` (ADR-0009).
/// Up: `lock_writer` only.
/// Down: nothing; a duration.
/// Verify: `cargo test -p loong-eventlog`.
const LOCK_RETRY: std::time::Duration = std::time::Duration::from_millis(5);

/// Purpose: Take the log's write lock on its sidecar, waiting out a lock that a forked
/// child still holds, or say this log already has a writer.
///
/// Serves: ADR-0009 §5's one writer, enforced before the log is opened or replayed (events-storage.md §3).
/// Design: `docs/design/events-storage.md` (ADR-0003, ADR-0009).
/// Up: `SessionLog::create` and `SessionLog::open`.
/// Down: `lock_path`, `File::try_lock`, `LOCK_ATTEMPTS` and `LogError::Locked`.
/// Verify: `cargo test -p loong-eventlog`.
fn lock_writer(path: &Path) -> Result<File, LogError> {
    // Read and write, because a Windows handle that is only for append cannot be locked.
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path(path))?;
    // A forked child inherits every open descriptor until it execs, so a lock this
    // process has just released can still be held by a child another thread was
    // spawning — and `try_lock` then fails on a log nobody is writing. Wait that
    // copy out. A real second writer keeps the lock for as long as it runs, so the
    // wait cannot turn a genuine clash into a silent second writer (ADR-0009 §5).
    for attempt in 1..=LOCK_ATTEMPTS {
        match lock.try_lock() {
            Ok(()) => return Ok(lock),
            Err(std::fs::TryLockError::WouldBlock) if attempt == LOCK_ATTEMPTS => break,
            Err(std::fs::TryLockError::WouldBlock) => std::thread::sleep(LOCK_RETRY),
            Err(std::fs::TryLockError::Error(e)) => return Err(LogError::Io(e)),
        }
    }
    Err(LogError::Locked {
        path: path.to_path_buf(),
    })
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
    /// Held for as long as this writer lives: dropping it releases the lock. Never read.
    _lock: File,
    next_seq: Seq,
    /// seq -> id, for parent lookups.
    ids: BTreeMap<Seq, String>,
    /// Bytes this writer has put in the file, compared before every append.
    written: u64,
}

impl SessionLog {
    /// Create a new log, failing if the path exists (never silently clobber a
    /// session). Takes the write lock on the log's sidecar first.
    /// Purpose: Create a new log with `create_new`, so an existing session is never clobbered.
    ///
    /// Serves: append-only truth: a second session cannot overwrite the first file (events-storage.md §3).
    /// Design: `docs/design/events-storage.md` (ADR-0003).
    /// Up: the session-creation path in the kernel.
    /// Down: `lock_writer`, `OpenOptions::create_new` and the `SessionLog` fields.
    /// Verify: `cargo test -p loong-eventlog`.
    pub fn create(path: impl AsRef<Path>) -> Result<Self, LogError> {
        let path = path.as_ref().to_path_buf();
        let lock = lock_writer(&path)?;
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&path)?;
        Ok(Self {
            path,
            file,
            _lock: lock,
            next_seq: 1,
            ids: BTreeMap::new(),
            written: 0,
        })
    }

    /// Open an existing log for appending: take its sidecar's write lock, then replay it so
    /// the next sequence number and the id table are correct.
    /// Purpose: Lock an existing log for this writer, then replay it to recover the next sequence and ids.
    ///
    /// Serves: sequence continuity across a restart, and one writer per session (events-storage.md §3).
    /// Design: `docs/design/events-storage.md` (ADR-0003).
    /// Up: the kernel when a session is reopened.
    /// Down: `lock_writer`, `replay` and the `SessionLog` fields.
    /// Verify: `cargo test -p loong-eventlog`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, LogError> {
        let path = path.as_ref().to_path_buf();
        let lock = lock_writer(&path)?;
        let file = OpenOptions::new().append(true).open(&path)?;
        let replay = replay(&path)?;
        let next_seq = replay.events.last().map(|e| e.seq + 1).unwrap_or(1);
        let ids = replay
            .events
            .iter()
            .map(|e| (e.seq, e.id.clone()))
            .collect();
        let written = file.metadata()?.len();
        Ok(Self {
            path,
            file,
            _lock: lock,
            next_seq,
            ids,
            written,
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
        let found = self.file.metadata()?.len();
        if found != self.written {
            return Err(LogError::ForeignWrite {
                path: self.path.clone(),
                expected: self.written,
                found,
            });
        }
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
        let bytes = line.as_bytes();
        self.file.write_all(bytes)?;
        self.file.flush()?;
        self.written += bytes.len() as u64;
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

/// What one repair pass found in a log and what it did to it.
///
/// Purpose: Report a repair pass so the command that ran it can say what changed.
///
/// Serves: `loongagent repair`'s report, and a test that wants the counts without a file (events-storage.md §3).
/// Design: `docs/design/events-storage.md` (ADR-0003); `docs/design/sessions-lineage.md` §5.
/// Up: `repair`, `repair_dry_run` and `loong-cli`'s repair command.
/// Down: `RepairReport::changed`, and the numbers the caller prints.
/// Verify: `cargo test -p loong-eventlog`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairReport {
    /// The log this pass was about.
    pub path: PathBuf,
    /// How many events the repaired log holds.
    pub events: usize,
    /// How many lines were given a new sequence number and id.
    pub renumbered: usize,
    /// How many lines were dropped as an exact repeat of the line above.
    pub dropped: usize,
    /// The copy taken before the rewrite, when there was one.
    pub backup: Option<PathBuf>,
}

impl RepairReport {
    /// Purpose: Say whether the log's sequence was broken, so a caller can tell "in order" from "rewritten".
    ///
    /// Serves: the command's summary line and its exit advice (sessions-lineage.md §5).
    /// Design: `docs/design/events-storage.md` (ADR-0003).
    /// Up: `loong-cli`'s repair command.
    /// Down: the `renumbered` and `dropped` counts.
    /// Verify: `cargo test -p loong-eventlog`.
    pub fn changed(&self) -> bool {
        self.renumbered > 0 || self.dropped > 0
    }
}

/// Report what repairing a log would change, without writing anything.
///
/// Purpose: Let a caller look before it leaps, and let `--all --check` sweep a project without touching it.
///
/// Serves: `loongagent repair --check` (sessions-lineage.md §5).
/// Design: `docs/design/events-storage.md` (ADR-0003).
/// Up: `loong-cli`'s repair command.
/// Down: `plan_repair`.
/// Verify: `cargo test -p loong-eventlog`.
pub fn repair_dry_run(path: impl AsRef<Path>) -> Result<RepairReport, LogError> {
    let path = path.as_ref().to_path_buf();
    let (_, report) = plan_repair(&path)?;
    Ok(report)
}

/// Rewrite a log whose sequence went backwards, keeping every event.
///
/// Purpose: Heal a log a second writer broke, instead of leaving the session unopenable forever.
///
/// Serves: recovery from the duplicate-sequence corruption a lockless writer leaves behind (events-storage.md §3).
/// Design: `docs/design/events-storage.md` (ADR-0003); the rule and the command in `docs/design/sessions-lineage.md` §5.
/// Up: `loong-cli`'s `repair` command.
/// Down: `lock_writer`, `plan_repair`, `backup_path`, `replay` and the log file itself.
/// Verify: `cargo test -p loong-eventlog`.
pub fn repair(path: impl AsRef<Path>) -> Result<RepairReport, LogError> {
    let path = path.as_ref().to_path_buf();
    // The lock is the whole safety story: a live writer means the file is not ours to rewrite.
    let lock = lock_writer(&path)?;
    let (bytes, mut report) = plan_repair(&path)?;
    if !report.changed() {
        return Ok(report);
    }
    let backup = backup_path(&path);
    std::fs::copy(&path, &backup)?;
    report.backup = Some(backup);
    let mut name = path.as_os_str().to_os_string();
    name.push(".repair-tmp");
    let tmp = PathBuf::from(name);
    std::fs::write(&tmp, &bytes)?;
    // Prove the rewritten log reads strictly before it replaces the original: a
    // repair that leaves a log `replay` still refuses has fixed nothing.
    if let Err(e) = replay(&tmp) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Ok(meta) = std::fs::metadata(&path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, &path)?;
    drop(lock);
    Ok(report)
}

/// Purpose: The path a repair pass backs the log up to, never overwriting an older backup.
///
/// Serves: the pre-repair bytes, so a repair that surprises someone can be undone (events-storage.md §3).
/// Design: `docs/design/events-storage.md` (ADR-0003).
/// Up: `repair` only.
/// Down: the log's own path with `.broken-bak` appended, numbered when that name is taken.
/// Verify: `cargo test -p loong-eventlog`.
fn backup_path(path: &Path) -> PathBuf {
    let mut candidate = path.as_os_str().to_os_string();
    candidate.push(".broken-bak");
    let mut backup = PathBuf::from(candidate);
    let mut n = 1;
    while backup.exists() {
        let mut candidate = path.as_os_str().to_os_string();
        candidate.push(format!(".broken-bak.{n}"));
        backup = PathBuf::from(candidate);
        n += 1;
    }
    backup
}

/// Purpose: One out-of-step line, rewritten with the `seq` and id its position asks for.
///
/// Serves: a repair that changes two numbers and nothing else, so the diff is auditable (events-storage.md §3).
/// Design: `docs/design/events-storage.md` (ADR-0003).
/// Up: `plan_repair` only.
/// Down: the line's own text after the `{"seq":…,"id":…,` prefix, or `EventEnvelope`'s serialiser when the line is laid out another way.
/// Verify: `cargo test -p loong-eventlog`.
fn renumbered_line(line: &[u8], env: &EventEnvelope, seq: Seq) -> Vec<u8> {
    let id = format!("e{seq:08}");
    // The writer emits `{"seq":N,"id":"eNNNNNNNN",` first, so everything after
    // that prefix can be copied across untouched. A line written any other way
    // falls back to a full re-serialisation.
    let head = format!("{{\"seq\":{},\"id\":\"{}\",", env.seq, env.id);
    match std::str::from_utf8(line)
        .ok()
        .and_then(|text| text.strip_prefix(&head))
    {
        Some(rest) => format!("{{\"seq\":{seq},\"id\":\"{id}\",{rest}").into_bytes(),
        None => {
            let rewritten = EventEnvelope {
                seq,
                id,
                ..env.clone()
            };
            // An envelope that parsed is an envelope that serialises: no floats,
            // no map keys, nothing a `Value` cannot write back out.
            serde_json::to_vec(&rewritten).expect("a parsed envelope serialises")
        }
    }
}

/// Purpose: Read the log the way a repair sees it: which lines are out of step, and what a rewrite would hold.
///
/// Serves: one scan behind both the write and the dry run, so `--check` cannot disagree with the repair (events-storage.md §3).
/// Design: `docs/design/events-storage.md` (ADR-0003).
/// Up: `repair` and `repair_dry_run` only.
/// Down: `serde_json`'s decoding, `renumbered_line`, `RepairReport` and the bytes a rewrite would write.
/// Verify: `cargo test -p loong-eventlog`.
fn plan_repair(path: &Path) -> Result<(Vec<u8>, RepairReport), LogError> {
    let text = std::fs::read(path)?;
    let mut out = Vec::with_capacity(text.len());
    let mut report = RepairReport {
        path: path.to_path_buf(),
        events: 0,
        renumbered: 0,
        dropped: 0,
        backup: None,
    };
    let mut expected: Seq = 1;
    let mut previous: Option<Vec<u8>> = None;
    let mut tree = false;
    for (i, raw) in text.split_inclusive(|b| *b == b'\n').enumerate() {
        let line_no = i + 1;
        let body = raw.strip_suffix(b"\n").unwrap_or(raw);
        let body = body.strip_suffix(b"\r").unwrap_or(body);
        if body.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let env: EventEnvelope = serde_json::from_slice(body).map_err(|e| LogError::Malformed {
            line: line_no,
            detail: e.to_string(),
        })?;
        tree |= env.parent_id.is_some();
        if previous.as_deref() == Some(body) {
            // The same record twice: one write that reached the file twice. It
            // carries no event the first copy did not, so it is dropped.
            report.dropped += 1;
            continue;
        }
        if env.seq == expected {
            out.extend_from_slice(body);
        } else {
            out.extend_from_slice(&renumbered_line(body, &env, expected));
            report.renumbered += 1;
        }
        out.push(b'\n');
        previous = Some(body.to_vec());
        report.events += 1;
        expected += 1;
    }
    if report.renumbered > 0 && tree {
        return Err(LogError::Malformed {
            line: 0,
            detail: "this log names parents; renumbering would move ids the event tree points at, \
                     so it is left as it is"
                .into(),
        });
    }
    Ok((out, report))
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
        /// Purpose: Remove the temporary file, its lock sidecar and any repair backup when the fixture goes out of scope.
        ///
        /// Contract: `crates/eventlog/src/lib.rs`.
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
            let _ = std::fs::remove_file(lock_path(&self.0));
            for suffix in [".broken-bak", ".broken-bak.1", ".repair-tmp"] {
                let mut name = self.0.as_os_str().to_os_string();
                name.push(suffix);
                let _ = std::fs::remove_file(PathBuf::from(name));
            }
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

    /// Purpose: Pins that a lock a forked child still holds for a moment is waited out, so a
    /// log nobody is writing is not refused as "another process is writing this session".
    #[test]
    fn a_lock_that_a_child_still_holds_briefly_is_waited_out() {
        let t = Temp::new();
        {
            let mut log = SessionLog::create(t.path()).unwrap();
            log.append("message.appended", json!({})).unwrap();
        }
        // A child forked between `fork` and `exec` inherits the sidecar's descriptor, so
        // the lock outlives the release that dropped it. A duplicated descriptor handed to
        // a short-lived thread reproduces exactly that: same lock, released a few ms later.
        let inherited = {
            let lock = lock_writer(t.path()).unwrap();
            let copy = lock.try_clone().unwrap();
            drop(lock);
            copy
        };
        let holder = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            drop(inherited);
        });
        // Without the wait this fails: the descriptor the child would be holding is
        // still open, and the log looks written when it is not.
        SessionLog::open(t.path()).expect("the transient copy is waited out, not refused");
        holder.join().unwrap();
    }

    /// Purpose: Pins that a second writer is refused while the first still holds the log.
    #[test]
    fn a_second_writer_cannot_open_the_same_log() {
        let t = Temp::new();
        let log = SessionLog::create(t.path()).unwrap();
        assert!(matches!(
            SessionLog::open(t.path()),
            Err(LogError::Locked { .. })
        ));
        drop(log);
        assert!(SessionLog::open(t.path()).is_ok());
    }

    /// Purpose: Pins that the lock is a sidecar, so replaying a log a writer holds still works.
    #[test]
    fn a_locked_log_still_replays() {
        let t = Temp::new();
        let mut log = SessionLog::create(t.path()).unwrap();
        log.append("message.appended", json!({})).unwrap();
        assert!(
            lock_path(t.path()).exists(),
            "the lock lives beside the log"
        );
        assert_eq!(replay(t.path()).unwrap().events.len(), 1);
        assert_eq!(replay_lenient(t.path()).unwrap().events.len(), 1);
    }

    /// Purpose: Pins that an append refuses when the file grew behind this writer's back.
    #[test]
    fn an_append_after_a_foreign_write_is_refused() {
        let t = Temp::new();
        let mut log = SessionLog::create(t.path()).unwrap();
        log.append("message.appended", json!({})).unwrap();
        append_raw(
            t.path(),
            json!({"seq": 2, "id": "e00000002", "kind": "message.appended", "ignorable": false, "time_ms": 0, "data": {}}),
        );
        assert!(matches!(
            log.append("message.appended", json!({})),
            Err(LogError::ForeignWrite { .. })
        ));
    }

    /// Purpose: Pins that a repair renumbers a second writer's block, keeps every event, and backs the log up.
    #[test]
    fn repair_renumbers_a_second_writers_block_and_keeps_every_event() {
        let t = Temp::new();
        let mut log = SessionLog::create(t.path()).unwrap();
        log.append("message.appended", json!({"role": "user", "text": "一"}))
            .unwrap();
        log.append(
            "message.appended",
            json!({"role": "assistant", "text": "二"}),
        )
        .unwrap();
        drop(log);
        // The shape a lockless writer leaves: it kept its own counter from an
        // earlier replay and writes 2, 3, 4 over the log's own 2.
        for text in ["三", "四", "五"] {
            append_raw(
                t.path(),
                json!({"seq": 2, "id": "e00000002", "parent_id": null, "kind": "message.appended", "ignorable": false, "time_ms": 0, "data": {"role": "user", "text": text}}),
            );
        }
        assert!(
            matches!(replay(t.path()), Err(LogError::SeqOutOfOrder { .. })),
            "the log does not open: that is the symptom being repaired"
        );

        let planned = repair_dry_run(t.path()).unwrap();
        assert_eq!(
            (planned.events, planned.renumbered, planned.dropped),
            (5, 3, 0)
        );
        assert!(planned.changed());
        assert!(planned.backup.is_none(), "a dry run takes no backup");
        assert!(
            replay(t.path()).is_err(),
            "and it leaves the file exactly as it found it"
        );

        let report = repair(t.path()).unwrap();
        assert_eq!(
            (report.events, report.renumbered, report.dropped),
            (5, 3, 0)
        );
        let backup = report.backup.expect("a changed log is backed up");
        assert!(
            replay(&backup).is_err(),
            "the backup holds the broken bytes"
        );

        let r = replay(t.path()).expect("the repaired log opens strictly");
        assert_eq!(r.events.len(), 5);
        assert_eq!(
            r.events.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5]
        );
        assert_eq!(
            r.events.iter().map(|e| e.id.clone()).collect::<Vec<_>>(),
            (1..=5).map(|s| format!("e{s:08}")).collect::<Vec<_>>()
        );
        let texts: Vec<_> = r
            .events
            .iter()
            .map(|e| e.data["text"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            texts,
            vec!["一", "二", "三", "四", "五"],
            "no event is lost"
        );
    }

    /// Purpose: Pins that a repair rewrites only the two numbers and leaves the rest of a line alone.
    #[test]
    fn repair_keeps_the_rest_of_a_renumbered_line_byte_for_byte() {
        let t = Temp::new();
        let mut log = SessionLog::create(t.path()).unwrap();
        log.append("message.appended", json!({})).unwrap();
        drop(log);
        // A line as the writer lays one out, including a `data` object whose keys
        // are not in alphabetical order: renumbering must not touch it.
        let line = r#"{"seq":1,"id":"e00000001","parent_id":null,"kind":"message.appended","ignorable":false,"time_ms":7,"data":{"role":"user","text":"hi","z":1,"a":2}}"#;
        append_line(t.path(), line);
        repair(t.path()).unwrap();
        let text = std::fs::read_to_string(t.path()).unwrap();
        assert!(
            text.contains(r#"{"seq":2,"id":"e00000002","parent_id":null,"kind":"message.appended","ignorable":false,"time_ms":7,"data":{"role":"user","text":"hi","z":1,"a":2}}"#),
            "the record is the same record, two numbers further on: {text}"
        );
    }

    /// Purpose: Pins that a line the file holds twice is dropped, because it carries no event the first copy did not.
    #[test]
    fn repair_drops_a_line_written_twice_and_keeps_a_second_event() {
        let t = Temp::new();
        let mut log = SessionLog::create(t.path()).unwrap();
        log.append("message.appended", json!({"role": "user", "text": "one"}))
            .unwrap();
        drop(log);
        let repeated = json!({"seq": 1, "id": "e00000001", "parent_id": null, "kind": "message.appended", "ignorable": false, "time_ms": 5, "data": {"role": "user", "text": "two"}});
        append_raw(t.path(), repeated.clone());
        append_raw(t.path(), repeated);
        let report = repair(t.path()).unwrap();
        assert_eq!(
            (report.events, report.renumbered, report.dropped),
            (2, 1, 1)
        );
        let r = replay(t.path()).unwrap();
        let texts: Vec<_> = r
            .events
            .iter()
            .map(|e| e.data["text"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(texts, vec!["one", "two"]);
    }

    /// Purpose: Pins that a log in order is left alone: no rewrite, no backup.
    #[test]
    fn repair_leaves_a_log_in_order_alone() {
        let t = Temp::new();
        let mut log = SessionLog::create(t.path()).unwrap();
        log.append("message.appended", json!({})).unwrap();
        log.append("message.appended", json!({})).unwrap();
        drop(log);
        let before = std::fs::read(t.path()).unwrap();
        let report = repair(t.path()).unwrap();
        assert!(!report.changed());
        assert_eq!(
            (report.events, report.renumbered, report.dropped),
            (2, 0, 0)
        );
        assert!(report.backup.is_none());
        assert_eq!(std::fs::read(t.path()).unwrap(), before);
    }

    /// Purpose: Pins that a repair refuses a live writer's log rather than rewriting it underneath.
    #[test]
    fn repair_refuses_a_log_a_writer_holds() {
        let t = Temp::new();
        let mut log = SessionLog::create(t.path()).unwrap();
        log.append("message.appended", json!({})).unwrap();
        assert!(matches!(repair(t.path()), Err(LogError::Locked { .. })));
        drop(log);
        assert!(repair(t.path()).is_ok(), "with the writer gone it is ours");
    }

    /// Purpose: Pins that a repair refuses a log whose ids an event tree names, instead of breaking the tree.
    #[test]
    fn repair_refuses_a_log_that_names_parents() {
        let t = Temp::new();
        let mut log = SessionLog::create(t.path()).unwrap();
        let first = log.append("message.appended", json!({})).unwrap();
        log.append_with_parent("message.appended", json!({}), Some(first.id))
            .unwrap();
        drop(log);
        append_raw(
            t.path(),
            json!({"seq": 2, "id": "e00000002", "parent_id": null, "kind": "message.appended", "ignorable": false, "time_ms": 0, "data": {}}),
        );
        let before = std::fs::read(t.path()).unwrap();
        assert!(matches!(repair(t.path()), Err(LogError::Malformed { .. })));
        assert_eq!(std::fs::read(t.path()).unwrap(), before);
    }

    /// Purpose: Append a raw JSON line, bypassing the writer's validation.
    ///
    /// Contract: `crates/eventlog/src/lib.rs`.
    fn append_raw(path: &Path, value: serde_json::Value) {
        append_line(path, &value.to_string());
    }

    /// Purpose: Append one already-serialised line, to reproduce a writer's own byte layout.
    ///
    /// Contract: `crates/eventlog/src/lib.rs`.
    fn append_line(path: &Path, line: &str) {
        let mut f = OpenOptions::new().append(true).open(path).unwrap();
        writeln!(f, "{line}").unwrap();
    }
}
