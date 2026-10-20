//! Purpose: The derived sidecar SQLite store over the JSONL truth: atoms, entities, summaries and their lineage.
//!
//! Serves: one disposable place where incremental ingest, recall and verification read and write, with the log always authoritative.
//!
//! # Contract
//! - **Design:** `docs/context/data-model.md` §5.2; `docs/design/context-ledger.md` §1 and §7.
//! - **Upstream:** `loong-kernel`'s `ContextEngine`, `turn/recall.rs`, and `loongagent context status/verify/rebuild` open and drive it.
//! - **Downstream:** `crate::summaries`, `crate::synthesis`, `crate::worker` and `crate::tail` use its connection and rows.
//! - **Verify:** `cargo test -p loong-context` · `./scripts/check.sh`.

use crate::materialize::materialize;
use loong_eventlog::{replay_lenient, LogError};
use rusqlite::{params, params_from_iter, Connection};
use std::path::Path;

/// Bumped whenever the derived schema changes in a way that requires a rebuild
/// rather than a migration. The database is disposable, so "refuse and rebuild"
/// is a legitimate answer — and a safer one than guessing.
pub const SCHEMA_VERSION: u32 = 5;

/// How much of an atom's text a search result carries. The full extract (still
/// capped at [`crate::materialize::MAX_TEXT_CHARS`]) is one `fetch` away.
const PREVIEW_CHARS: usize = 200;

/// Purpose: Everything that can go wrong while opening, reading or writing the derived store.
/// Serves: callers can name a schema mismatch or a malformed tail instead of reading a stringly error.
/// Design: `docs/context/data-model.md` §5.2.
/// Up: Every `Store` method; `loongagent context` maps it to a message.
/// Down: The `Display` and `From` impls below.
/// Verify: `cargo test -p loong-context --test retrieval a_store_written_by_another_schema`.
#[derive(Debug)]
pub enum StoreError {
    Sql(rusqlite::Error),
    Io(std::io::Error),
    Log(LogError),
    Encode(serde_json::Error),
    /// The sidecar was written by a different schema version. Rebuild it.
    SchemaVersion {
        found: u32,
        expected: u32,
    },
    /// The log contradicts itself, or a stored citation points at nothing.
    Inconsistent(String),
    /// Reading only the appended part of the log failed (a malformed event at a
    /// known offset, or sequence numbers that do not continue).
    Tail(String),
}

impl std::fmt::Display for StoreError {
    /// Purpose: Render a `StoreError` as the message a person sees.
    /// Serves: schema and tail failures that say what to do (rebuild, fix the log).
    /// Design: `docs/context/data-model.md` §5.2.
    /// Up: `StoreError`'s `Display` contract.
    /// Down: Writes each variant; no callee.
    /// Verify: `cargo test -p loong-context --test retrieval a_store_written_by_another_schema`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Sql(e) => write!(f, "sqlite: {e}"),
            StoreError::Io(e) => write!(f, "io: {e}"),
            StoreError::Log(e) => write!(f, "log: {e}"),
            StoreError::Encode(e) => write!(f, "encode: {e}"),
            StoreError::SchemaVersion { found, expected } => write!(
                f,
                "the derived context store is schema v{found}, this build expects v{expected}: \
                 rebuild it (the log is untouched)"
            ),
            StoreError::Inconsistent(m) => write!(f, "inconsistent: {m}"),
            StoreError::Tail(m) => write!(f, "tail: {m}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    /// Purpose: Wrap a rusqlite failure.
    /// Contract: `store.rs` error plumbing.
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Sql(e)
    }
}
impl From<LogError> for StoreError {
    /// Purpose: Wrap a log failure.
    /// Contract: `store.rs` error plumbing.
    fn from(e: LogError) -> Self {
        StoreError::Log(e)
    }
}
impl From<std::io::Error> for StoreError {
    /// Purpose: Wrap an I/O failure.
    /// Contract: `store.rs` error plumbing.
    fn from(e: std::io::Error) -> Self {
        StoreError::Io(e)
    }
}

impl From<serde_json::Error> for StoreError {
    /// Purpose: Wrap a JSON encoding failure.
    /// Contract: `store.rs` error plumbing.
    fn from(e: serde_json::Error) -> Self {
        StoreError::Encode(e)
    }
}

/// Purpose: One search result: a citation and enough context to decide whether to fetch the original.
/// Serves: `recall` answering with `seq` handles rather than a copy of the record.
/// Design: `docs/context/recall.md` §7.2.
/// Up: `Store::search` and `Store::atom`; `loong-kernel`'s `turn/recall.rs` renders it.
/// Down: `Hit::citation`.
/// Verify: `cargo test -p loong-context --test retrieval results_are_newest_first`.
/// One search result: enough to decide whether to fetch the original, and no
/// more. `seq` is the citation — everything else is a convenience.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub seq: u64,
    pub id: String,
    pub time_ms: i64,
    pub kind: String,
    pub role: Option<String>,
    pub tool: Option<String>,
    pub ok: Option<bool>,
    /// A truncated view of the atom's text.
    pub preview: String,
    pub entities: Vec<(String, String)>,
}

impl Hit {
    /// Purpose: The stable citation handle for a hit.
    /// Serves: callers carrying a `seq` forward without inventing a second identifier.
    /// Design: `docs/context/recall.md` §7.2.
    /// Up: `loong-kernel`'s `turn/recall.rs`.
    /// Down: Returns `(seq, seq)`; no callee.
    /// Verify: `cargo test -p loong-context --test retrieval one_atom_can_be_expanded`.
    /// The citation a caller should carry forward: `seq` is the stable handle.
    pub fn citation(&self) -> (u64, u64) {
        (self.seq, self.seq)
    }
}

/// Purpose: What one incremental look at the log produced.
/// Serves: `sync` telling a worker whether it synced, idled or must rebuild.
/// Design: `docs/context/data-model.md` §5.2; `docs/context/pipeline.md` §6.6.
/// Up: `Store::sync`, `crate::worker::step` and the kernel's `refresh`.
/// Down: Its variants are matched by the worker and the tests.
/// Verify: `cargo test -p loong-context --test worker sync_reads_once_and_then_idles`.
/// What one incremental look at the log produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncOutcome {
    /// New events were read and materialised.
    Synced { events: usize, atoms: u64 },
    /// Nothing was appended since the last look — the steady state.
    Idle,
    /// The log is shorter than the cursor (truncated or replaced). The caller
    /// should call [`Store::rebuild`]; `sync` will not silently do it, because
    /// throwing away a derived store is a decision worth announcing.
    NeedsRebuild,
}

/// Purpose: A recall request: words plus optional deterministic filters.
/// Serves: one shape for the atom search and the summary search, with implicit AND between terms.
/// Design: `docs/context/recall.md` §7.2.
/// Up: `loong-kernel`'s `turn/recall.rs` and the retrieval/summaries tests.
/// Down: `Store::search` and `Store::recall_at`; the builder methods below.
/// Verify: `cargo test -p loong-context --test retrieval several_terms_narrow`.
/// A recall request. Kept deliberately shape-simple: the caller (eventually the
/// model, through a tool) supplies words and optional deterministic filters.
#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    /// Words to look for. All of them must appear (implicit AND), each matched
    /// independently — see [`Store::search`] for why that matters in Chinese.
    pub terms: Vec<String>,
    /// Restrict to an entity by `kind:name` or a bare name (any kind).
    pub entity: Option<String>,
    /// Restrict to one tool, e.g. `read_file`.
    pub tool: Option<String>,
    /// Restrict to one event kind, e.g. `tool.result`.
    pub kind: Option<String>,
    /// Restrict to messages from one role.
    pub role: Option<String>,
    /// Inclusive lower bound on `seq`, for "since we last looked".
    pub seq_from: Option<u64>,
    pub limit: usize,
}

impl SearchQuery {
    /// Purpose: A query for the given terms with the default limit.
    /// Serves: one constructor so callers never spell the term list by hand.
    /// Design: `docs/context/recall.md` §7.2.
    /// Up: `loong-kernel`'s `turn/recall.rs` and the tests.
    /// Down: `SearchQuery::default`; no other callee.
    /// Verify: `cargo test -p loong-context --test retrieval`.
    pub fn new(terms: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            terms: terms.into_iter().map(Into::into).collect(),
            limit: 8,
            ..Default::default()
        }
    }

    /// Purpose: Set the result limit.
    /// Serves: bounding a recall answer in one place.
    /// Design: `docs/context/recall.md` §7.2.
    /// Up: `loong-kernel`'s `turn/recall.rs` and the tests.
    /// Down: Mutates the query; no callee.
    /// Verify: `cargo test -p loong-context --test retrieval results_are_newest_first`.
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// Purpose: Restrict a query to an entity by `kind:name` or a bare name.
    /// Serves: deterministic filtering answering "which module".
    /// Design: `docs/context/recall.md` §7.2.
    /// Up: The retrieval tests.
    /// Down: Mutates the query; no callee.
    /// Verify: `cargo test -p loong-context --test retrieval entity_filters`.
    pub fn entity(mut self, entity: impl Into<String>) -> Self {
        self.entity = Some(entity.into());
        self
    }

    /// Purpose: Restrict a query to one tool.
    /// Serves: the filter shape a caller may use for `read_file`-only searches.
    /// Design: `docs/context/recall.md` §7.2.
    /// Up: No caller in the tree today.
    /// Down: Mutates the query; no callee.
    /// Verify: `cargo test -p loong-context --test retrieval`.
    pub fn tool(mut self, tool: impl Into<String>) -> Self {
        self.tool = Some(tool.into());
        self
    }

    /// Purpose: Restrict a query to one event kind.
    /// Serves: the filter shape a caller may use for a kind-only search.
    /// Design: `docs/context/recall.md` §7.2.
    /// Up: No caller in the tree today.
    /// Down: Mutates the query; no callee.
    /// Verify: `cargo test -p loong-context --test retrieval`.
    pub fn kind(mut self, kind: impl Into<String>) -> Self {
        self.kind = Some(kind.into());
        self
    }

    /// Purpose: Bound a query to events at or after a `seq`.
    /// Serves: asking "what happened since we last looked".
    /// Design: `docs/context/recall.md` §7.2.
    /// Up: No caller in the tree today.
    /// Down: Mutates the query; no callee.
    /// Verify: `cargo test -p loong-context --test retrieval`.
    pub fn seq_from(mut self, seq: u64) -> Self {
        self.seq_from = Some(seq);
        self
    }
}

/// Purpose: What `Store::verify` found: counts, watermark and contradictions.
/// Serves: `loongagent context verify` answering "can I trust this derived store?".
/// Design: `docs/context/status.md` §11.1.
/// Up: `Store::verify`; `loongagent context verify`.
/// Down: `VerifyReport::is_healthy`.
/// Verify: `cargo test -p loong-context --test retrieval verify_reports_a_healthy_store`.
/// What a [`Store::verify`] found. An empty report is the healthy state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerifyReport {
    pub atoms: u64,
    pub entities: u64,
    pub links: u64,
    /// The newest atom's `seq`, or 0 when empty.
    pub head_seq: u64,
    /// The ingest watermark — every event up to here has been considered.
    pub watermark: u64,
    /// Event sequences deliberately not recorded (no searchable content).
    pub skipped: u64,
    /// Expected atoms that are not there. Non-zero means a truncated log or an
    /// ingest interrupted mid-batch — the derived state is not to be trusted.
    pub missing_atoms: u64,
    /// Entity links pointing at an atom that is not there.
    pub orphan_links: u64,
    /// Entity links pointing at an entity that is not there.
    pub dangling_entity_links: u64,
}

impl VerifyReport {
    /// Purpose: Whether the report found no missing atoms or dangling links.
    /// Serves: the one boolean a status command branches on.
    /// Design: `docs/context/status.md` §11.1.
    /// Up: `loongagent context verify` and the retrieval tests.
    /// Down: Compares the report's counters; no callee.
    /// Verify: `cargo test -p loong-context --test retrieval verify_reports_a_healthy_store`.
    pub fn is_healthy(&self) -> bool {
        self.orphan_links == 0 && self.dangling_entity_links == 0 && self.missing_atoms == 0
    }
}

/// Purpose: The live sidecar connection.
/// Serves: one owner of the SQLite handle, so summaries cannot open a second connection by accident.
/// Design: `docs/context/data-model.md` §5.2.
/// Up: `Store::open` and `Store::in_memory`; the summary layer writes through it.
/// Down: `Store::meta`, `Store::set_meta` and the `summaries` queries.
/// Verify: `cargo test -p loong-context`.
pub struct Store {
    conn: Connection,
}

impl std::fmt::Debug for Store {
    /// Purpose: Debug shape: watermark and atom count, not the non-`Debug` connection.
    /// Contract: `store.rs` diagnostics.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `rusqlite::Connection` is not `Debug`; the shape a caller cares about is
        // how far the ingest got.
        f.debug_struct("Store")
            .field("watermark", &self.watermark())
            .field("atoms", &self.atom_count())
            .finish()
    }
}

impl Store {
    /// Purpose: Open or create a sidecar store, refusing a schema it does not understand.
    /// Serves: rebuilding is cheaper and safer than half-reading an old schema.
    /// Design: `docs/context/data-model.md` §5.2.
    /// Up: `loong-kernel`'s `ContextEngine`, `loongagent context`, and the tests.
    /// Down: `Store::init`.
    /// Verify: `cargo test -p loong-context --test retrieval a_store_written_by_another_schema`.
    /// Open (or create) a sidecar store. A store written by an older schema is
    /// refused rather than half-read: rebuilding is cheap and safe.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        let store = Self { conn };
        store.init()?;
        Ok(store)
    }

    /// Purpose: An in-memory store for tests and one-shot builds.
    /// Serves: the same schema and behavior without a file to clean up.
    /// Design: `docs/context/data-model.md` §5.2.
    /// Up: The tests and `build`.
    /// Down: `Store::init`.
    /// Verify: `cargo test -p loong-context`.
    pub fn in_memory() -> Result<Self, StoreError> {
        let store = Self {
            conn: Connection::open_in_memory()?,
        };
        store.init()?;
        Ok(store)
    }

    /// Purpose: Create the schema, pragmas and schema-version row, or refuse a mismatch.
    /// Serves: the four summary tables and the two-writer pragmas existing exactly once.
    /// Design: `docs/context/data-model.md` §5.2; `docs/context/status.md` §11.1.
    /// Up: `Store::open` and `Store::in_memory`.
    /// Down: `Store::meta` and `Store::set_meta`; the DDL.
    /// Verify: `cargo test -p loong-context --test two_writers`.
    fn init(&self) -> Result<(), StoreError> {
        // One writer per session; WAL keeps the reader (a status panel) from
        // blocking the ingest. Foreign keys are enforced for the atom/entity
        // links, so a half-written ingest cannot leave a dangling reference.
        //
        // `busy_timeout` exists because there are now **two** writers on this file:
        // the foreground derives the deterministic layers, and the enrichment worker
        // writes summaries from its own connection (ADR-0017 §4.6 — a second `Store`
        // rather than a shared mutex, so the foreground can never be made to wait on a
        // model call). SQLite still serialises writers; the timeout makes the loser
        // wait for the winner's short transaction instead of failing with
        // `SQLITE_BUSY`.
        //
        // Measured, and worth naming: `rusqlite` already installs a 5 s busy timeout
        // on every connection it opens (`inner_connection.rs`), so this line is
        // currently redundant. It is written anyway because the guarantee is ours to
        // keep — a library default is not a contract, and the two-writer arrangement
        // is a property of this design, not of the driver. `tests/two_writers.rs`
        // pins the behaviour (and shows what its absence looks like).
        //
        // 5 s is deliberately far below any model timeout: if the foreground ever
        // waited seconds here, that would be the contention this design forbids, and
        // it should surface as a degraded memory rather than as a stall.
        self.conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;",
        )?;
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (
                 key   TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS atom (
                 seq          INTEGER PRIMARY KEY,
                 id           TEXT    NOT NULL UNIQUE,
                 time_ms      INTEGER NOT NULL,
                 kind         TEXT    NOT NULL,
                 role         TEXT,
                 tool         TEXT,
                 ok           INTEGER,
                 text         TEXT    NOT NULL,
                 content_hash INTEGER NOT NULL,
                 is_write     INTEGER NOT NULL DEFAULT 0,
                 call_id      TEXT,
                 write_attempt TEXT
             );
             CREATE INDEX IF NOT EXISTS atom_kind ON atom(kind);
             CREATE INDEX IF NOT EXISTS atom_tool ON atom(tool);
             CREATE INDEX IF NOT EXISTS atom_hash ON atom(content_hash);
             CREATE INDEX IF NOT EXISTS atom_time ON atom(time_ms);
             CREATE TABLE IF NOT EXISTS entity (
                 id   INTEGER PRIMARY KEY,
                 kind TEXT NOT NULL,
                 name TEXT NOT NULL,
                 UNIQUE(kind, name)
             );
             CREATE TABLE IF NOT EXISTS atom_entity (
                 seq       INTEGER NOT NULL REFERENCES atom(seq),
                 entity_id INTEGER NOT NULL REFERENCES entity(id),
                 PRIMARY KEY (seq, entity_id)
             );
             CREATE INDEX IF NOT EXISTS atom_entity_e ON atom_entity(entity_id);

             -- Summaries (L2/L3/L4) land in P2. They are derived like everything
             -- else here: deleting them loses only the cost of deriving them.
             CREATE TABLE IF NOT EXISTS summary (
                 id            TEXT PRIMARY KEY,
                 level         INTEGER NOT NULL,
                 scope         TEXT    NOT NULL,
                 title         TEXT    NOT NULL,
                 content       TEXT    NOT NULL,
                 status        TEXT    NOT NULL,
                 producer      TEXT    NOT NULL,
                 schema_ver    INTEGER NOT NULL,
                 seq_from      INTEGER NOT NULL,
                 seq_to        INTEGER NOT NULL,
                 superseded_by TEXT,
                 domain        TEXT,
                 tokens        INTEGER NOT NULL,
                 created_at    INTEGER NOT NULL,
                 updated_at    INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS summary_level ON summary(level, scope);
             CREATE INDEX IF NOT EXISTS summary_range ON summary(seq_from, seq_to);

             -- Derivation lineage. This is what makes the 'never summarise a
             -- summary' rule mechanically checkable instead of a convention.
             -- input_id NULL means 'a range of raw events'.
             CREATE TABLE IF NOT EXISTS summary_inputs (
                 summary_id TEXT    NOT NULL,
                 input_id   TEXT,
                 seq_from   INTEGER,
                 seq_to     INTEGER
             );
             CREATE INDEX IF NOT EXISTS summary_inputs_s ON summary_inputs(summary_id);

             -- Citations: a summary -> the raw evidence it rests on.
             CREATE TABLE IF NOT EXISTS citation (
                 summary_id TEXT    NOT NULL,
                 seq_from   INTEGER NOT NULL,
                 seq_to     INTEGER NOT NULL,
                 PRIMARY KEY (summary_id, seq_from, seq_to)
             );

             CREATE TABLE IF NOT EXISTS summary_entity (
                 summary_id TEXT    NOT NULL,
                 entity_id  INTEGER NOT NULL,
                 PRIMARY KEY (summary_id, entity_id)
             );
             CREATE INDEX IF NOT EXISTS summary_entity_e ON summary_entity(entity_id);

             -- No full-text table over summaries, deliberately. Measured at
             -- 5,000 summaries: an FTS5 trigram MATCH is 0.12 ms and a LIKE scan
             -- is 1.1-2.2 ms — both far below anything a person or a model can
             -- perceive, while an external-content FTS5 table costs an entire
             -- class of correctness traps (it answers a rowid query from the
             -- *content* table, its delete-by-rowid fails on rows it never
             -- indexed while reporting 'database disk image is malformed', and it
             -- has to be kept in step by hand on every write). At this corpus size
             -- the scan is the honest engineering choice; the measurement lives in
             -- tests/search_cost.rs so the call can be revisited on evidence.",
        )?;
        let found: u32 = self
            .meta("schema_version")?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if found == 0 {
            self.set_meta("schema_version", &SCHEMA_VERSION.to_string())?;
        } else if found != SCHEMA_VERSION {
            return Err(StoreError::SchemaVersion {
                found,
                expected: SCHEMA_VERSION,
            });
        }
        Ok(())
    }

    /// Purpose: Borrow the connection immutably.
    /// Serves: the summary layer running its reads through one place.
    /// Design: `docs/context/data-model.md` §5.2.
    /// Up: `Store::atom_views`, `Store::put_summary`, `Store::summary`, `Store::recall_at` and the other readers.
    /// Down: Returns `&Connection`; no other callee.
    /// Verify: `cargo test -p loong-context`.
    /// Borrow the connection. Kept crate-internal so the summary layer
    /// (`summaries.rs`) writes through one place rather than opening its own.
    pub(crate) fn conn_ref(&self) -> &Connection {
        &self.conn
    }

    /// Purpose: Borrow the connection mutably.
    /// Serves: the summary layer opening a write transaction through one place.
    /// Design: `docs/context/data-model.md` §5.2.
    /// Up: `Store::put_summary`, `Store::replace_overlapping` and `Store::rebuild_summaries`.
    /// Down: Returns `&mut Connection`; no other callee.
    /// Verify: `cargo test -p loong-context`.
    pub(crate) fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// Purpose: Read one `meta` value.
    /// Serves: watermarks, cursors and skipped decisions kept as auditable rows.
    /// Design: `docs/context/data-model.md` §5.2.
    /// Up: `Store::init`, `Store::watermark`, `Store::cursor`, `Store::skipped` and `Store::digested_through`.
    /// Down: SQL over `meta`; no other callee.
    /// Verify: `cargo test -p loong-context --test retrieval ingest_is_idempotent`.
    pub(crate) fn meta(&self, key: &str) -> Result<Option<String>, StoreError> {
        let mut stmt = self.conn.prepare("SELECT value FROM meta WHERE key = ?1")?;
        let mut rows = stmt.query(params![key])?;
        Ok(match rows.next()? {
            Some(row) => Some(row.get(0)?),
            None => None,
        })
    }

    /// Purpose: Write one `meta` value.
    /// Serves: advancing a watermark or recording a skipped decision.
    /// Design: `docs/context/data-model.md` §5.2.
    /// Up: `Store::init`, `Store::ingest`, `Store::absorb`, `Store::sync`, `Store::rebuild` and `Store::digest`.
    /// Down: SQL over `meta`; no other callee.
    /// Verify: `cargo test -p loong-context --test retrieval ingest_is_idempotent`.
    pub(crate) fn set_meta(&self, key: &str, value: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    /// Purpose: The highest `seq` the store has considered, or 0 for an empty store.
    /// Serves: ingest resuming exactly where it stopped, including events deliberately not materialised.
    /// Design: `docs/context/data-model.md` §5.2.
    /// Up: `Store::ingest`, `Store::cursor` and `Store::verify`; the debug impl reads it too.
    /// Down: SQL over `meta`/`atom`; no other callee.
    /// Verify: `cargo test -p loong-context --test retrieval ingest_is_idempotent`.
    /// The highest `seq` ingested, or 0 for an empty store. Ingest resumes here.
    ///
    /// This is the *ingest* watermark, not the highest stored atom: events the
    /// materialiser deliberately ignores still advance it, so "have I seen this
    /// log up to here?" and "which atom is newest?" stay different questions.
    pub fn watermark(&self) -> Result<u64, StoreError> {
        let stored: Option<i64> = self
            .conn
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'watermark'",
                [],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        match stored {
            Some(seq) => Ok(seq as u64),
            None => {
                let n: Option<i64> = self
                    .conn
                    .query_row("SELECT MAX(seq) FROM atom", [], |r| r.get(0))?;
                Ok(n.unwrap_or(0) as u64)
            }
        }
    }

    /// Purpose: The newest stored atom's `seq`.
    /// Serves: the difference between "seen up to here" and "the newest searchable row".
    /// Design: `docs/context/data-model.md` §5.2.
    /// Up: `Store::verify`; no caller outside it.
    /// Down: SQL over `atom`; no other callee.
    /// Verify: `cargo test -p loong-context --test retrieval verify_reports_a_healthy_store`.
    /// The newest atom's `seq`, which may be below the watermark when the tail
    /// of the log produced nothing searchable.
    pub fn head_atom_seq(&self) -> Result<u64, StoreError> {
        let n: Option<i64> = self
            .conn
            .query_row("SELECT MAX(seq) FROM atom", [], |r| r.get(0))?;
        Ok(n.unwrap_or(0) as u64)
    }

    /// Purpose: Every atom as `(seq, time_ms, kind, role)`, ascending.
    /// Serves: the segmenter working on sequence properties without pulling 512-char bodies.
    /// Design: `docs/context/pipeline.md` §6.3.
    /// Up: `Store::digest` and `Store::digest_pending`.
    /// Down: SQL over `atom`; no other callee.
    /// Verify: `cargo test -p loong-context --test summaries episodes_split`.
    /// Every materialised atom as `(seq, time_ms, kind, role)`, ascending. The
    /// segmenter works off this rather than off the stored text: boundaries are a
    /// property of the sequence, and pulling 512-char bodies to compute them
    /// would be waste.
    pub fn atom_index(&self) -> Result<Vec<(u64, i64, String, Option<String>)>, StoreError> {
        let mut stmt = self
            .conn_ref()
            .prepare("SELECT seq, time_ms, kind, role FROM atom ORDER BY seq")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)? as u64,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Purpose: How many atoms are stored.
    /// Serves: the honesty surface and the worker's "is there anything to digest?" check.
    /// Design: `docs/context/data-model.md` §5.2.
    /// Up: `Store`'s debug impl, `Store::verify`, `crate::worker::step` and the tests.
    /// Down: SQL over `atom`; no other callee.
    /// Verify: `cargo test -p loong-context --test retrieval`.
    pub fn atom_count(&self) -> Result<u64, StoreError> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM atom", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    /// Purpose: Read one atom in full, as a `Hit`.
    /// Serves: the "expand" half of search-then-expand, with the citation intact.
    /// Design: `docs/context/recall.md` §7.2.
    /// Up: The retrieval tests; `recall`'s evidence path uses `Store::expand` instead.
    /// Down: `Store::entities_of` and `row_to_hit`.
    /// Verify: `cargo test -p loong-context --test retrieval one_atom_can_be_expanded`.
    /// Read one atom in full — the "expand" half of search-then-expand.
    pub fn atom(&self, seq: u64) -> Result<Option<Hit>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, id, time_ms, kind, role, tool, ok, text FROM atom WHERE seq = ?1",
        )?;
        let mut rows = stmt.query(params![seq as i64])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let mut hit = row_to_hit(row)?;
        hit.entities = self.entities_of(seq)?;
        Ok(Some(hit))
    }

    /// Purpose: The entities linked to one atom.
    /// Serves: returning a hit's filters with the hit, and building `AtomView`s.
    /// Design: `docs/context/data-model.md` §5.4.
    /// Up: `Store::atom`, `Store::search` and `crate::summaries::atom_views`.
    /// Down: SQL over `atom_entity`; no other callee.
    /// Verify: `cargo test -p loong-context --test retrieval results_are_newest_first`.
    pub(crate) fn entities_of(&self, seq: u64) -> Result<Vec<(String, String)>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT e.kind, e.name FROM atom_entity a JOIN entity e ON e.id = a.entity_id
             WHERE a.seq = ?1 ORDER BY e.kind, e.name",
        )?;
        let rows = stmt.query_map(params![seq as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Purpose: Ingest everything in the log the store has not seen, reading the whole file.
    /// Serves: the one-shot build path: idempotent, resumable and linear in the log.
    /// Design: `docs/context/data-model.md` §5.2.
    /// Up: `Store::rebuild` and `loongagent context build`.
    /// Down: `replay_lenient`, `Store::skipped`, `materialize`, `insert_atom` and `Store::set_meta`.
    /// Verify: `cargo test -p loong-context --test retrieval ingest_is_idempotent`.
    /// Ingest everything in `log` that the store has not seen. Idempotent: it
    /// can be called repeatedly, and after an interruption, with the same result.
    ///
    /// This reads the whole log rather than tailing it. That is honest but linear;
    /// the background worker in P3 will need a byte-offset tail, and the design
    /// note says so. What matters now is that the *state* it converges to is
    /// exactly the same either way.
    pub fn ingest(&mut self, log: impl AsRef<Path>) -> Result<u64, StoreError> {
        let path = log.as_ref();
        let replayed = replay_lenient(path)?;
        let watermark = self.watermark()?;
        let mut skipped: Vec<u64> = self.skipped()?;
        self.set_meta("log_path", &path.display().to_string())?;

        let tx = self.conn.transaction()?;
        let mut ingested = 0u64;
        for env in replayed.events.iter().filter(|e| e.seq > watermark) {
            // Advance past every event seen, including the ones that carry no
            // searchable content. Without this, a kind we deliberately do not
            // record (`policy.decided`) would look like a hole in the sequence
            // forever, and `verify` would cry wolf.
            tx.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('watermark', ?1)",
                params![env.seq as i64],
            )?;
            let Some(atom) = materialize(env) else {
                // Record the decision, so "this seq has no atom" is a fact the
                // store can check rather than something `verify` has to infer.
                skipped.push(env.seq);
                continue;
            };
            insert_atom(&tx, &atom)?;
            ingested += 1;
        }
        if !skipped.is_empty() {
            skipped.sort_unstable();
            skipped.dedup();
            let encoded = skipped
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .join(",");
            tx.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('skipped', ?1)",
                params![encoded],
            )?;
        }
        // The byte cursor, so a resident worker can resume from here instead of
        // re-reading the whole log. A full read always leaves us at the end of
        // the complete lines in the file.
        let len = std::fs::metadata(path)?.len();
        tx.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('cursor_offset', ?1)",
            params![len as i64],
        )?;
        tx.commit()?;
        Ok(ingested)
    }

    /// Purpose: Where a resident worker should resume reading.
    /// Serves: tailing only what was appended instead of re-reading the session.
    /// Design: `docs/context/pipeline.md` §6.6.
    /// Up: `Store::sync`; no caller outside it.
    /// Down: `Store::meta` and `Store::watermark`.
    /// Verify: `cargo test -p loong-context --test worker sync_reads_once_and_then_idles`.
    /// Where a worker should resume reading, or `None` when this store has never
    /// read a log.
    pub fn cursor(&self) -> Result<Option<crate::tail::Cursor>, StoreError> {
        let Some(path) = self.meta("log_path")? else {
            return Ok(None);
        };
        let offset = self
            .meta("cursor_offset")?
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        Ok(Some(crate::tail::Cursor {
            path: std::path::PathBuf::from(path),
            offset,
            len: offset,
            last_seq: self.watermark()?,
        }))
    }

    /// Purpose: Read only what was appended since the last look.
    /// Serves: staying current costing what was appended, not what the session has accumulated.
    /// Design: `docs/context/pipeline.md` §6.6.
    /// Up: `crate::worker::step` and the kernel's `ContextEngine::refresh`.
    /// Down: `crate::tail::read_new_bounded`, `Store::absorb` and `Store::set_meta`.
    /// Verify: `cargo test -p loong-context --test worker sync_reads_once_and_then_idles`.
    /// Read only what was appended since the last look — the incremental step a
    /// resident worker runs, as opposed to [`Store::ingest`]'s full read.
    ///
    /// Never rebuilds on its own: a truncated or replaced log comes back as
    /// [`SyncOutcome::NeedsRebuild`], because discarding a derived store is a
    /// decision worth announcing rather than a convenience worth taking.
    pub fn sync(&mut self, max_events: usize) -> Result<SyncOutcome, StoreError> {
        let Some(cursor) = self.cursor()? else {
            return Ok(SyncOutcome::NeedsRebuild);
        };
        let batch = match crate::tail::read_new_bounded(&cursor, max_events) {
            Ok(batch) => batch,
            Err(crate::tail::TailError::Shrank) => return Ok(SyncOutcome::NeedsRebuild),
            Err(e) => return Err(StoreError::Tail(e.to_string())),
        };

        // The offset advances over the bytes actually parsed, whatever became of
        // the events, so a batch of kinds we deliberately do not record cannot
        // leave the cursor stuck.
        self.set_meta("cursor_offset", &batch.cursor.offset.to_string())?;
        if batch.is_empty() {
            return Ok(SyncOutcome::Idle);
        }
        let events = batch.events;
        let atoms = self.absorb(&events)?;
        Ok(SyncOutcome::Synced {
            events: events.len(),
            atoms,
        })
    }

    /// Purpose: Materialise envelopes read straight from the tail.
    /// Serves: one write path shared with `ingest`, so a tailed and a rebuilt session cannot drift.
    /// Design: `docs/context/pipeline.md` §6.6.
    /// Up: `Store::sync`.
    /// Down: `materialize`, `insert_atom`, `Store::skipped` and `Store::set_meta`.
    /// Verify: `cargo test -p loong-context --test worker what_the_worker_derives_equals_what_a_full_rebuild`.
    /// Materialise envelopes read straight off the tail (they carry no path to
    /// re-read). Shares one implementation with [`Store::ingest`], so a tailed
    /// session and a rebuilt one cannot drift.
    fn absorb(
        &mut self,
        events: &[loong_schema::generated::EventEnvelope],
    ) -> Result<u64, StoreError> {
        let mut skipped: Vec<u64> = self.skipped()?;
        let tx = self.conn.transaction()?;
        let mut ingested = 0u64;
        for env in events {
            tx.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('watermark', ?1)",
                params![env.seq as i64],
            )?;
            let Some(atom) = materialize(env) else {
                skipped.push(env.seq);
                continue;
            };
            insert_atom(&tx, &atom)?;
            ingested += 1;
        }
        if !skipped.is_empty() {
            skipped.sort_unstable();
            skipped.dedup();
            let encoded = skipped
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .join(",");
            tx.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('skipped', ?1)",
                params![encoded],
            )?;
        }
        tx.commit()?;
        Ok(ingested)
    }

    /// Purpose: The event sequences the materialiser deliberately ignored.
    /// Serves: recording a decision so `verify` can distinguish a hole from a choice.
    /// Design: `docs/context/data-model.md` §5.2.
    /// Up: `Store::ingest`, `Store::absorb`, `Store::verify` and `Store::skipped_count`.
    /// Down: Parses the `skipped` meta row; no other callee.
    /// Verify: `cargo test -p loong-context --test retrieval verify_reports_a_healthy_store`.
    /// Event sequences the materialiser deliberately ignored. Stored rather than
    /// recomputed: what was skipped is a decision, and a decision that is not
    /// recorded cannot be audited.
    fn skipped(&self) -> Result<Vec<u64>, StoreError> {
        let raw = self.meta("skipped")?.unwrap_or_default();
        Ok(raw
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect())
    }

    /// Purpose: How many events were deliberately skipped.
    /// Serves: a status surface able to report the count without parsing the row itself.
    /// Design: `docs/context/data-model.md` §5.2.
    /// Up: No caller in the tree today.
    /// Down: `Store::skipped`.
    /// Verify: `cargo test -p loong-context --test retrieval`.
    pub fn skipped_count(&self) -> Result<u64, StoreError> {
        Ok(self.skipped()?.len() as u64)
    }

    /// Purpose: Throw the derived state away and rebuild it from the log.
    /// Serves: the promise that makes the whole design safe: nothing here is authoritative.
    /// Design: `docs/design/context-ledger.md` §7.
    /// Up: `loongagent context rebuild`, `build`, and `crate::worker::step` when the tail shrank.
    /// Down: `Store::ingest` and `Store::set_meta`.
    /// Verify: `cargo test -p loong-context --test retrieval a_deleted_store_rebuilds`.
    /// Throw the derived state away and rebuild it from the log. This is the
    /// promise that makes the whole design safe: nothing here is authoritative.
    pub fn rebuild(&mut self, log: impl AsRef<Path>) -> Result<u64, StoreError> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM atom_entity", [])?;
        tx.execute("DELETE FROM entity", [])?;
        tx.execute("DELETE FROM atom", [])?;
        tx.execute("DELETE FROM meta", [])?;
        tx.commit()?;
        self.set_meta("schema_version", &SCHEMA_VERSION.to_string())?;
        self.ingest(log)
    }

    /// Purpose: Search the retrieval layer with implicit AND over terms.
    /// Serves: the raw-event fallback of `recall`, with no FTS5 cliff under a two-character Chinese word.
    /// Design: `docs/context/recall.md` §7.2.
    /// Up: `loong-kernel`'s `turn/recall.rs` and the retrieval tests.
    /// Down: `Store::entities_of`, `row_to_hit`, `truncate` and the SQL it builds.
    /// Verify: `cargo test -p loong-context --test retrieval a_two_character_chinese_word_is_findable`.
    /// Search the retrieval layer.
    ///
    /// Every term becomes its own `LIKE` predicate and all of them must match
    /// (implicit AND). Terms are matched *independently on purpose*: SQLite's
    /// FTS5 treats a multi-word query as a phrase set, and a single term shorter
    /// than the trigram width makes the entire query return nothing, silently.
    /// A `LIKE` scan has no such cliff — it simply finds Chinese two-character
    /// words, which is the common case here. The measured cost (5 ms over 20k
    /// rows) and the plan for changing it are in the design note §5.3.
    pub fn search(&self, query: &SearchQuery) -> Result<Vec<Hit>, StoreError> {
        let mut where_parts: Vec<String> = Vec::new();
        let mut values: Vec<rusqlite::types::Value> = Vec::new();

        for term in &query.terms {
            let term = term.trim();
            if term.is_empty() {
                continue;
            }
            where_parts.push(
                "(atom.text LIKE ?1 OR atom.kind LIKE ?1 OR IFNULL(atom.tool,'') LIKE ?1)"
                    .replace("?1", &format!("?{}", values.len() + 1)),
            );
            values.push(rusqlite::types::Value::Text(format!("%{term}%")));
        }
        if let Some(entity) = query.entity.as_deref().filter(|e| !e.is_empty()) {
            let (kind, name) = match entity.split_once(':') {
                Some((kind, name)) => (Some(kind), name),
                None => (None, entity),
            };
            where_parts.push(format!(
                "atom.seq IN (SELECT a.seq FROM atom_entity a JOIN entity e ON e.id = a.entity_id
                              WHERE e.name = ?{} {})",
                values.len() + 1,
                if kind.is_some() {
                    format!("AND e.kind = ?{}", values.len() + 2)
                } else {
                    String::new()
                }
            ));
            values.push(rusqlite::types::Value::Text(name.to_string()));
            if let Some(kind) = kind {
                values.push(rusqlite::types::Value::Text(kind.to_string()));
            }
        }
        if let Some(tool) = query.tool.as_deref().filter(|t| !t.is_empty()) {
            where_parts.push(format!("atom.tool = ?{}", values.len() + 1));
            values.push(rusqlite::types::Value::Text(tool.to_string()));
        }
        if let Some(kind) = query.kind.as_deref().filter(|k| !k.is_empty()) {
            where_parts.push(format!("atom.kind = ?{}", values.len() + 1));
            values.push(rusqlite::types::Value::Text(kind.to_string()));
        }
        if let Some(role) = query.role.as_deref().filter(|r| !r.is_empty()) {
            where_parts.push(format!("atom.role = ?{}", values.len() + 1));
            values.push(rusqlite::types::Value::Text(role.to_string()));
        }
        if let Some(seq_from) = query.seq_from {
            where_parts.push(format!("atom.seq >= ?{}", values.len() + 1));
            values.push(rusqlite::types::Value::Integer(seq_from as i64));
        }

        // Newest first: for a grep-like tool over a session, recency is the most
        // useful default ordering. The design note explains why bm25 is not used
        // for ranking here (its document-length normalisation is script-dependent).
        let sql = format!(
            "SELECT seq, id, time_ms, kind, role, tool, ok, text FROM atom {} ORDER BY seq DESC LIMIT ?{}",
            if where_parts.is_empty() {
                String::new()
            } else {
                format!("WHERE {}", where_parts.join(" AND "))
            },
            values.len() + 1
        );
        values.push(rusqlite::types::Value::Integer(query.limit.max(1) as i64));

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(values), |row| {
            let mut hit = row_to_hit(row)?;
            hit.preview = truncate(&hit.preview, PREVIEW_CHARS);
            Ok(hit)
        })?;
        let mut hits = rows.collect::<Result<Vec<_>, _>>()?;
        for hit in &mut hits {
            hit.entities = self.entities_of(hit.seq)?;
        }
        Ok(hits)
    }

    /// Purpose: Entities matching a partial name, with how many atoms reference each.
    /// Serves: answering "what modules and commands does this session even touch?".
    /// Design: `docs/context/data-model.md` §5.4.
    /// Up: `loongagent context status` and the retrieval/summaries tests.
    /// Down: SQL over `entity` and `atom_entity`; no other callee.
    /// Verify: `cargo test -p loong-context --test retrieval entity_filters`.
    /// Entities matching a partial name, with how many atoms reference each.
    /// This is the "what modules does this session even touch" query.
    pub fn entities(
        &self,
        needle: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, String, u64)>, StoreError> {
        let sql = "SELECT e.kind, e.name, COUNT(a.seq) AS n
                   FROM entity e LEFT JOIN atom_entity a ON a.entity_id = e.id
                   WHERE (?1 IS NULL OR e.name LIKE ?1)
                   GROUP BY e.id ORDER BY n DESC, e.kind, e.name LIMIT ?2";
        let pattern = needle.map(|n| format!("%{n}%"));
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![pattern, limit as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)? as u64,
            ))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Purpose: Check the derived state for internal contradictions.
    /// Serves: a stale or truncated store saying so instead of claiming to be fine.
    /// Design: `docs/context/status.md` §11.1.
    /// Up: `loongagent context verify` and the retrieval tests.
    /// Down: `Store::atom_count`, `Store::head_atom_seq`, `Store::watermark` and `Store::skipped`.
    /// Verify: `cargo test -p loong-context --test retrieval a_store_missing_a_row_is_not_reported_as_healthy`.
    /// Check the derived state for internal contradictions. A healthy report is
    /// how `loong context verify` answers "can I trust what I am about to read?"
    pub fn verify(&self) -> Result<VerifyReport, StoreError> {
        let mut report = VerifyReport::default();
        report.atoms = self.atom_count()?;
        report.head_seq = self.head_atom_seq()?;
        report.entities = self
            .conn
            .query_row("SELECT COUNT(*) FROM entity", [], |r| r.get::<_, i64>(0))?
            as u64;
        report.links = self
            .conn
            .query_row("SELECT COUNT(*) FROM atom_entity", [], |r| {
                r.get::<_, i64>(0)
            })? as u64;
        report.orphan_links = self.conn.query_row(
            "SELECT COUNT(*) FROM atom_entity a LEFT JOIN atom t ON t.seq = a.seq WHERE t.seq IS NULL",
            [],
            |r| r.get::<_, i64>(0),
        )? as u64;
        report.dangling_entity_links = self.conn.query_row(
            "SELECT COUNT(*) FROM atom_entity a LEFT JOIN entity e ON e.id = a.entity_id WHERE e.id IS NULL",
            [],
            |r| r.get::<_, i64>(0),
        )? as u64;

        // Sequence holes below the ingest watermark are real: the log numbers
        // events from 1 with no gaps, so every event we have already passed must
        // either have produced an atom or been deliberately skipped. Skipped ones
        // are recorded, so what remains is a genuine hole (a truncated log, or an
        // ingest interrupted mid-batch).
        let watermark = self.watermark()?;
        report.watermark = watermark;
        let skipped = self.skipped()?;
        report.skipped = skipped.len() as u64;
        let skipped_set: std::collections::BTreeSet<u64> = skipped.into_iter().collect();
        let mut stmt = self
            .conn
            .prepare("SELECT COUNT(*), MIN(seq), MAX(seq) FROM atom")?;
        let (count, min_seq, max_seq): (i64, Option<i64>, Option<i64>) =
            stmt.query_row([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        if let (Some(min_seq), Some(max_seq)) = (min_seq, max_seq) {
            // Every seq in the stored span must be an atom, unless we recorded
            // that we skipped it. A leading run of skipped events (nothing before
            // the first atom) is not a hole.
            let span = (max_seq - min_seq + 1) as u64;
            let skipped_in_span = skipped_set
                .iter()
                .filter(|s| **s >= min_seq as u64 && **s <= max_seq as u64)
                .count() as u64;
            let expected = span - skipped_in_span;
            report.missing_atoms = expected.saturating_sub(count as u64);
        }
        Ok(report)
    }
}

/// Purpose: Write one atom and replace its entity links.
/// Serves: one writer for both the replay and tail paths, idempotent when extraction rules change.
/// Design: `docs/context/data-model.md` §5.2.
/// Up: `Store::ingest` and `Store::absorb`.
/// Down: `EntityKind::as_str`; SQL over `atom`, `entity` and `atom_entity`.
/// Verify: `cargo test -p loong-context --test retrieval ingest_is_idempotent`.
/// Write one atom and its entity links. One implementation for the replay path and
/// the tail path, so a rebuilt session and a tailed one cannot diverge.
fn insert_atom(
    tx: &rusqlite::Transaction<'_>,
    atom: &crate::materialize::Atom,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT OR REPLACE INTO atom
           (seq, id, time_ms, kind, role, tool, ok, text, content_hash, is_write, call_id,
            write_attempt)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            atom.seq as i64,
            atom.id,
            atom.time_ms,
            atom.kind,
            atom.role,
            atom.tool,
            atom.ok.map(|b| b as i64),
            atom.text,
            atom.content_hash as i64,
            atom.is_write as i64,
            atom.call_id,
            atom.write_attempt,
        ],
    )?;
    // Replacing the links rather than only adding them keeps the ingest
    // idempotent even if the extraction rules change between runs.
    tx.execute(
        "DELETE FROM atom_entity WHERE seq = ?1",
        params![atom.seq as i64],
    )?;
    for entity in &atom.entities {
        tx.execute(
            "INSERT OR IGNORE INTO entity (kind, name) VALUES (?1, ?2)",
            params![entity.kind.as_str(), entity.name],
        )?;
        let entity_id: i64 = tx.query_row(
            "SELECT id FROM entity WHERE kind = ?1 AND name = ?2",
            params![entity.kind.as_str(), entity.name],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO atom_entity (seq, entity_id) VALUES (?1, ?2)",
            params![atom.seq as i64, entity_id],
        )?;
    }
    Ok(())
}

/// Purpose: Read a SQL row into a `Hit`.
/// Serves: one column order shared by the search and atom lookups.
/// Design: `docs/context/data-model.md` §5.2.
/// Up: `Store::atom` and `Store::search`.
/// Down: Maps the columns; no other callee.
/// Verify: `cargo test -p loong-context --test retrieval one_atom_can_be_expanded`.
fn row_to_hit(row: &rusqlite::Row<'_>) -> rusqlite::Result<Hit> {
    Ok(Hit {
        seq: row.get::<_, i64>(0)? as u64,
        id: row.get(1)?,
        time_ms: row.get(2)?,
        kind: row.get(3)?,
        role: row.get(4)?,
        tool: row.get(5)?,
        ok: row.get::<_, Option<i64>>(6)?.map(|v| v != 0),
        preview: row.get(7)?,
        entities: Vec::new(),
    })
}

/// Purpose: Cut a preview to a character limit and mark the cut.
/// Serves: a truncated hit saying so, rather than reading as the whole record.
/// Design: `docs/context/recall.md` §7.2.
/// Up: `Store::search`.
/// Down: Counts characters; no other callee.
/// Verify: `cargo test -p loong-context --test search_cost`.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}…")
}

/// Purpose: Build a ready store from a whole log.
/// Serves: one call for a caller that has the log and wants a current store.
/// Design: `docs/context/data-model.md` §5.2.
/// Up: The retrieval tests; no caller in `crates/kernel` or `crates/cli`.
/// Down: `Store::in_memory` and `Store::rebuild`.
/// Verify: `cargo test -p loong-context --test retrieval a_deleted_store_rebuilds`.
/// Convenience for a caller that has a whole log and wants a ready store.
pub fn build(log: impl AsRef<Path>) -> Result<Store, StoreError> {
    let mut store = Store::in_memory()?;
    store.rebuild(log)?;
    Ok(store)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Purpose: Pin that truncation keeps a marked cut and leaves a short string unchanged.
    #[test]
    fn truncate_marks_where_it_cut() {
        assert_eq!(truncate("abcdef", 3), "abc…");
        assert_eq!(truncate("abc", 3), "abc");
    }
}
