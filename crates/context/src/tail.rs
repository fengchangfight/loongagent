//! Purpose: Tail the event log by byte offset, so a resident worker reads only what was appended.
//!
//! Serves: staying current costing what the session appended rather than what it has accumulated.
//!
//! # Contract
//! - **Design:** `docs/context/pipeline.md` §6.6.
//! - **Upstream:** `Store::sync` calls `read_new_bounded`; the tests call `read_new`.
//! - **Downstream:** The worker's incremental step and the store's cursor.
//! - **Verify:** `cargo test -p loong-context` · `./scripts/check.sh`.

use loong_eventlog::LogError;
use loong_schema::generated::EventEnvelope;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

/// Purpose: Everything that can go wrong while reading the appended part of the log.
/// Serves: a truncated file, a malformed line or a sequence jump being loud instead of silently skipped.
/// Design: `docs/context/pipeline.md` §6.6.
/// Up: `read_new_with` returns it; `Store::sync` maps it.
/// Down: The `Display` impl below.
/// Verify: `cargo test -p loong-context --test worker a_shorter_log_is_reported`.
#[derive(Debug)]
pub enum TailError {
    Io(std::io::Error),
    /// One complete line is not a valid event envelope. Refusing beats skipping:
    /// an event we cannot read is a hole in the memory, and holes must be loud.
    Malformed {
        offset: u64,
        detail: String,
    },
    /// The sequence numbers are not contiguous from where we left off.
    SeqOutOfOrder {
        expected: u64,
        found: u64,
    },
    /// The log is shorter than the cursor: it was truncated, replaced, or a
    /// different session is now at this path. The only sensible response is to
    /// rebuild, which is why this carries no numbers to reason about.
    Shrank,
}

impl std::fmt::Display for TailError {
    /// Purpose: Render a tail error as the message a person sees.
    /// Serves: a malformed line pointing at its byte offset.
    /// Design: `docs/context/pipeline.md` §6.6.
    /// Up: `TailError`'s `Display` contract.
    /// Down: Writes each variant; no callee.
    /// Verify: `cargo test -p loong-context --test worker a_half_written_line_is_not_read`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TailError::Io(e) => write!(f, "io: {e}"),
            TailError::Malformed { offset, detail } => {
                write!(f, "malformed event at byte {offset}: {detail}")
            }
            TailError::SeqOutOfOrder { expected, found } => {
                write!(f, "expected seq {expected} next, found {found}")
            }
            TailError::Shrank => write!(f, "the log is shorter than the cursor: rebuild"),
        }
    }
}

impl std::error::Error for TailError {}

impl From<std::io::Error> for TailError {
    /// Purpose: Wrap an I/O failure.
    /// Contract: `tail.rs` error plumbing.
    fn from(e: std::io::Error) -> Self {
        TailError::Io(e)
    }
}

impl From<LogError> for TailError {
    /// Purpose: Wrap a log failure as a malformed tail.
    /// Contract: `tail.rs` error plumbing.
    fn from(e: LogError) -> Self {
        match e {
            LogError::Io(e) => TailError::Io(e),
            other => TailError::Malformed {
                offset: 0,
                detail: other.to_string(),
            },
        }
    }
}

/// Purpose: Where a reader has got to, and enough about the file to notice it changed.
/// Serves: resuming at a byte offset rather than re-reading the session.
/// Design: `docs/context/pipeline.md` §6.6.
/// Up: `Store::cursor`, `read_new` and `read_new_bounded`; the tests.
/// Down: `read_new_with` and `TailBatch.cursor`.
/// Verify: `cargo test -p loong-context --test worker a_tail_read_returns_only_what_was_appended`.
/// Where a reader has got to, and enough about the file to notice it changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    pub path: PathBuf,
    /// Byte offset of the first unread byte.
    pub offset: u64,
    /// The file's length when the offset was taken, to detect truncation.
    pub len: u64,
    /// The highest `seq` read, or 0 before anything was read.
    pub last_seq: u64,
}

impl Cursor {
    /// Purpose: A cursor at the start of a path.
    /// Serves: a test or a new worker having a well-defined starting point.
    /// Design: `docs/context/pipeline.md` §6.6.
    /// Up: The worker tests; no caller in production code.
    /// Down: Constructs the struct; no other callee.
    /// Verify: `cargo test -p loong-context --test worker a_tail_read_returns_only_what_was_appended`.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            offset: 0,
            len: 0,
            last_seq: 0,
        }
    }
}

/// Purpose: What one look at the log produced: events, the next cursor and any partial line.
/// Serves: the cursor advancing only past complete lines.
/// Design: `docs/context/pipeline.md` §6.6.
/// Up: `read_new` and `read_new_bounded`; `Store::sync`.
/// Down: `TailBatch::is_empty` and the store's cursor update.
/// Verify: `cargo test -p loong-context --test worker a_half_written_line_is_not_read`.
/// What one look at the log produced.
#[derive(Debug, Clone, PartialEq)]
pub struct TailBatch {
    pub events: Vec<EventEnvelope>,
    /// The cursor to carry forward, whether or not anything was read.
    pub cursor: Cursor,
    /// Bytes sitting past the last complete line. Normally 0; non-zero means a
    /// writer was interrupted mid-line and the tail will pick it up next time.
    pub partial_bytes: u64,
}

impl TailBatch {
    /// Purpose: Whether this look found no events.
    /// Serves: the worker's idle check without inspecting the vector.
    /// Design: `docs/context/pipeline.md` §6.6.
    /// Up: `Store::sync`; the tests.
    /// Down: Reads `events`; no callee.
    /// Verify: `cargo test -p loong-context --test worker a_tail_read_returns_only_what_was_appended`.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// Purpose: Read everything appended past a cursor.
/// Serves: an unbounded read for a test, expressed through the same bounded implementation.
/// Design: `docs/context/pipeline.md` §6.6.
/// Up: The worker tests; no caller in production code.
/// Down: `read_new_with`.
/// Verify: `cargo test -p loong-context --test worker a_tail_read_returns_only_what_was_appended`.
/// Read everything appended past `cursor`.
///
/// A cursor for a different path starts from the beginning; a cursor past the
/// end of a shorter file is [`TailError::Shrank`].
pub fn read_new(cursor: &Cursor) -> Result<TailBatch, TailError> {
    read_new_with(cursor, None)
}

/// Purpose: Read new events, stopping before `max` of them.
/// Serves: one bounded batch, with the rest read next time.
/// Design: `docs/context/pipeline.md` §6.6.
/// Up: `Store::sync`.
/// Down: `read_new_with`.
/// Verify: `cargo test -p loong-context --test worker sync_reads_once_and_then_idles`.
/// Read new events, stopping before `max` of them. The cursor advances only past
/// what was actually returned, so the rest is simply read next time — which is
/// what keeps one batch bounded when a session suddenly appends a lot.
pub fn read_new_bounded(cursor: &Cursor, max: usize) -> Result<TailBatch, TailError> {
    read_new_with(cursor, Some(max))
}

/// Purpose: Read complete lines past a cursor, preserving any partial line for next time.
/// Serves: the mechanism that keeps a tail correct while a writer is mid-line.
/// Design: `docs/context/pipeline.md` §6.6.
/// Up: `read_new` and `read_new_bounded`.
/// Down: `std::fs`, `serde_json::from_str` and the cursor arithmetic.
/// Verify: `cargo test -p loong-context --test worker a_half_written_line_is_not_read`.
fn read_new_with(cursor: &Cursor, max: Option<usize>) -> Result<TailBatch, TailError> {
    let meta = std::fs::metadata(&cursor.path)?;
    let len = meta.len();
    if len < cursor.offset {
        return Err(TailError::Shrank);
    }
    if len == cursor.offset {
        let mut next = cursor.clone();
        next.len = len;
        return Ok(TailBatch {
            events: Vec::new(),
            cursor: next,
            partial_bytes: 0,
        });
    }

    let mut file = File::open(&cursor.path)?;
    file.seek(SeekFrom::Start(cursor.offset))?;
    let mut buffer = Vec::with_capacity((len - cursor.offset) as usize);
    file.read_to_end(&mut buffer)?;

    let mut events = Vec::new();
    let mut consumed = 0usize;
    let mut expected = cursor.last_seq + 1;
    let mut last_seq = cursor.last_seq;

    for line in buffer.split_inclusive(|b| *b == b'\n') {
        if !line.ends_with(b"\n") {
            // A partial line: keep the offset before it.
            break;
        }
        if let Some(limit) = max {
            if events.len() >= limit {
                break;
            }
        }
        let start = consumed;
        consumed += line.len();
        let text =
            std::str::from_utf8(&line[..line.len() - 1]).map_err(|e| TailError::Malformed {
                offset: cursor.offset + start as u64,
                detail: e.to_string(),
            })?;
        if text.trim().is_empty() {
            continue;
        }
        let envelope: EventEnvelope =
            serde_json::from_str(text).map_err(|e| TailError::Malformed {
                offset: cursor.offset + start as u64,
                detail: e.to_string(),
            })?;
        // The log numbers events from 1 with no holes; a jump means we are
        // reading a different file than the one we were following.
        if expected != 0 && envelope.seq != expected {
            return Err(TailError::SeqOutOfOrder {
                expected,
                found: envelope.seq,
            });
        }
        expected = envelope.seq + 1;
        last_seq = envelope.seq;
        events.push(envelope);
    }

    let next = Cursor {
        path: cursor.path.clone(),
        offset: cursor.offset + consumed as u64,
        len,
        last_seq,
    };
    Ok(TailBatch {
        events,
        cursor: next,
        partial_bytes: (buffer.len() - consumed) as u64,
    })
}
