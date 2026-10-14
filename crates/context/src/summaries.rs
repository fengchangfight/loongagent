//! L2/L3/L4 summaries: the derived comprehension layer (ADR-0017).
//!
//! Four tables have to agree for a summary to exist — `summary`, `citation`,
//! `summary_entity`, `summary_inputs` — so they are written in exactly one place
//! ([`Store::put_summary`]) and read back through one shape ([`Summary`]).
//!
//! The rule this module exists to enforce is in `docs/06-context-engineering.md`
//! §4.2.1: **a summary is always re-derived from its current children, never from
//! its own previous version.** Doing the latter would compound error once per
//! level and make it unattributable. [`Summary::inputs`] is the record that makes
//! the rule checkable after the fact: for an L2 it points at an event range, and
//! for L3/L4 it will point at child summaries — never at a prior version of
//! itself.

use crate::materialize::EntityKind;
use crate::store::{SearchQuery, Store, StoreError};
use rusqlite::{params, params_from_iter};

/// Bumped when the digest's shape changes, so a stale summary is recognisable
/// rather than silently mixed with a new one.
pub const SUMMARY_SCHEMA_VERSION: u32 = 1;

/// What one idle window of semantic enrichment may spend (ADR-0017 §4.6).
///
/// Both bounds are hard. `calls` counts **attempts**, not successes, so an
/// endpoint that is down cannot turn one window into a sweep over the whole
/// session; `tokens` is an estimate from the same counter the rest of the crate
/// uses, charged before each call and after each answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowBudget {
    pub calls: usize,
    pub tokens: u64,
}

impl WindowBudget {
    /// A window bounded by calls alone — the historical `synthesize` behaviour.
    pub fn calls(calls: usize) -> Self {
        Self {
            calls,
            tokens: u64::MAX,
        }
    }

    pub fn new(calls: usize, tokens: u64) -> Self {
        Self { calls, tokens }
    }
}

/// What one window actually did. The worker accumulates this so "what has the
/// background cost me?" is a query rather than an inference (ADR-0017 §10).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowOutcome {
    /// Model requests sent, successful or not.
    pub calls: u64,
    /// Episodes whose stored summary gained semantic fields.
    pub enriched: u64,
    /// Requests that failed; each one degraded exactly its own episode.
    pub failed: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// L2 episodes still lacking semantic fields after this window.
    pub remaining: u64,
    /// The most recent failure, so a surface can say *why* an episode degraded.
    pub last_error: Option<String>,
}

impl WindowOutcome {
    /// Did this window do anything at all?
    pub fn did_work(&self) -> bool {
        self.calls > 0
    }
}

/// One derived summary at one level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub id: String,
    /// 2 = episode, 3 = domain/module, 4 = global map.
    pub level: u32,
    /// `episode:<id>` | `domain:<module>` | `global`.
    pub scope: String,
    pub title: String,
    pub content: String,
    /// Computed from evidence, never generated (see ADR-0017 §4).
    pub status: String,
    /// `deterministic` | `digest+model:<name>`.
    pub producer: String,
    pub schema_ver: u32,
    pub seq_from: u64,
    pub seq_to: u64,
    /// Set when a later conclusion replaced this one. Superseded, never deleted:
    /// "we used to think X" is itself a fact worth keeping.
    pub superseded_by: Option<String>,
    /// The L3 domain this summary rolls up into (`src/kernel`, `crates/context`).
    /// Stored rather than re-derived from the text, because a map built by
    /// re-parsing prose is a map that breaks the first time the prose changes.
    pub domain: Option<String>,
    pub tokens: u64,
    pub created_at: i64,
    pub updated_at: i64,
    pub citations: Vec<(u64, u64)>,
    pub inputs: Vec<SummaryInput>,
}

/// Where a summary came from: a child summary, or a range of raw events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryInput {
    /// `None` means the input is the raw event range in `seq_from`/`seq_to`.
    pub summary_id: Option<String>,
    pub seq_from: Option<u64>,
    pub seq_to: Option<u64>,
}

/// A summary together with the raw evidence it cites — the only shape a caller
/// should hand to a model, because it cannot then mistake the summary for the
/// record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expansion {
    pub summary: Summary,
    pub evidence: Vec<AtomView>,
}

/// The part of an atom the digest needs.
///
/// A deliberately narrow view rather than the whole `Atom`: the digest must not
/// be able to depend on anything the store did not record, or a rebuild would
/// stop reproducing the summaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtomView {
    pub seq: u64,
    pub kind: String,
    pub role: String,
    pub tool: String,
    pub ok: Option<bool>,
    pub text: String,
    pub entities: Vec<(EntityKind, String)>,
    /// Whether this atom's call changed a file (recorded, not re-derived).
    pub is_write: bool,
    /// The tool-call id, tying a result back to the call it answers.
    pub call_id: Option<String>,
    /// What a write attempt was, in a few words.
    pub write_attempt: Option<String>,
    /// Names of tests this atom's output mentions.
    pub tests: Vec<String>,
}

impl Store {
    /// Load the narrow view the digest needs, for a span of `seq`.
    pub fn atom_views(&self, seq_from: u64, seq_to: u64) -> Result<Vec<AtomView>, StoreError> {
        let mut stmt = self.conn_ref().prepare(
            "SELECT seq, kind, IFNULL(role,''), IFNULL(tool,''), ok, text, is_write, call_id,
                    write_attempt
             FROM atom WHERE seq >= ?1 AND seq <= ?2 ORDER BY seq",
        )?;
        // Collected before the per-atom entity lookups start. `entities_of` runs
        // its own query on this connection, and rusqlite steps the inner
        // statement while the outer one is still open: SQLite answers that with
        // "database disk image is malformed", which reads like corruption and is
        // not. Draining first also matches what `search` already does.
        let rows: Vec<_> = stmt
            .query_map(params![seq_from as i64, seq_to as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)? as u64,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<i64>>(4)?.map(|v| v != 0),
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)? != 0,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut views = Vec::new();
        for row in rows {
            let (seq, kind, role, tool, ok, text, is_write, call_id, write_attempt) = row;
            let mut entities = Vec::new();
            let mut tests = Vec::new();
            for (kind_name, name) in self.entities_of(seq)? {
                if let Some(kind) = entity_kind_from_str(&kind_name) {
                    if kind == EntityKind::Test {
                        tests.push(name.clone());
                    }
                    entities.push((kind, name));
                }
            }
            views.push(AtomView {
                seq,
                kind,
                role,
                tool,
                ok,
                text,
                entities,
                is_write,
                call_id,
                write_attempt,
                tests,
            });
        }
        Ok(views)
    }
}

fn entity_kind_from_str(value: &str) -> Option<EntityKind> {
    [
        EntityKind::Path,
        EntityKind::Module,
        EntityKind::Symbol,
        EntityKind::Tool,
        EntityKind::Command,
        EntityKind::Test,
        EntityKind::Error,
    ]
    .into_iter()
    .find(|k| k.as_str() == value)
}

impl Store {
    /// Insert or replace a summary and its derived rows: citations, entity links
    /// and lineage. One function, so the four representations can never
    /// disagree.
    pub fn put_summary(&mut self, summary: &Summary) -> Result<(), StoreError> {
        let entity_ids = self.entity_ids_for_range(summary.seq_from, summary.seq_to)?;
        let conn = self.conn_mut();
        let tx = conn.transaction()?;
        let label = |what: &str, e: rusqlite::Error| {
            StoreError::Inconsistent(format!("writing summary {} ({what}): {e}", summary.id))
        };
        tx.execute(
            "INSERT OR REPLACE INTO summary
               (id, level, scope, title, content, status, producer, schema_ver,
                seq_from, seq_to, superseded_by, domain, tokens, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
            params![
                summary.id,
                summary.level as i64,
                summary.scope,
                summary.title,
                summary.content,
                summary.status,
                summary.producer,
                summary.schema_ver as i64,
                summary.seq_from as i64,
                summary.seq_to as i64,
                summary.superseded_by,
                summary.domain,
                summary.tokens as i64,
                summary.created_at,
                summary.updated_at,
            ],
        )
        .map_err(|e| label("summary row", e))?;
        tx.execute(
            "DELETE FROM citation WHERE summary_id = ?1",
            params![summary.id],
        )
        .map_err(|e| label("clear citations", e))?;
        for (from, to) in &summary.citations {
            tx.execute(
                "INSERT OR IGNORE INTO citation (summary_id, seq_from, seq_to) VALUES (?1,?2,?3)",
                params![summary.id, *from as i64, *to as i64],
            )
            .map_err(|e| label("citation", e))?;
        }
        tx.execute(
            "DELETE FROM summary_entity WHERE summary_id = ?1",
            params![summary.id],
        )
        .map_err(|e| label("clear entity links", e))?;
        for entity_id in &entity_ids {
            tx.execute(
                "INSERT OR IGNORE INTO summary_entity (summary_id, entity_id) VALUES (?1,?2)",
                params![summary.id, entity_id],
            )
            .map_err(|e| label("entity link", e))?;
        }
        tx.execute(
            "DELETE FROM summary_inputs WHERE summary_id = ?1",
            params![summary.id],
        )
        .map_err(|e| label("clear lineage", e))?;
        for input in &summary.inputs {
            tx.execute(
                "INSERT INTO summary_inputs (summary_id, input_id, seq_from, seq_to)
                 VALUES (?1,?2,?3,?4)",
                params![
                    summary.id,
                    input.summary_id,
                    input.seq_from.map(|s| s as i64),
                    input.seq_to.map(|s| s as i64),
                ],
            )
            .map_err(|e| label("lineage", e))?;
        }
        tx.commit()?;
        Ok(())
    }

    fn entity_ids_for_range(&self, seq_from: u64, seq_to: u64) -> Result<Vec<i64>, StoreError> {
        let mut stmt = self
            .conn_ref()
            .prepare("SELECT DISTINCT entity_id FROM atom_entity WHERE seq >= ?1 AND seq <= ?2")?;
        let rows = stmt.query_map(params![seq_from as i64, seq_to as i64], |r| r.get(0))?;
        Ok(rows.collect::<Result<Vec<i64>, _>>()?)
    }

    /// Build the L2 digests for every episode from the atoms. Deterministic and
    /// idempotent: running it twice replaces summaries with identical rows.
    pub fn digest(&mut self) -> Result<u64, StoreError> {
        let index = self.atom_index()?;
        let segments = crate::digest::segment(&index);
        let mut written = 0;
        let mut highest = 0u64;
        for segment in segments {
            let views = self.atom_views(segment.seq_from, segment.seq_to)?;
            let end = segment.seq_to;
            let digest = crate::digest::build(segment, &views);
            self.put_summary(&tail_summary(&digest))?;
            highest = highest.max(end);
            written += 1;
        }
        self.set_meta("digested_through", &highest.to_string())?;
        Ok(written)
    }

    /// Derive every summary that is behind, oldest first, up to `max_episodes`.
    ///
    /// Not just the tail: a store that has never been digested has a whole backlog
    /// to catch up on, and a worker that only ever looks at the newest episode
    /// would leave episode 1 underived forever. The watermark makes this
    /// incremental *and* complete — [`Store::digest`] does the same work in one
    /// pass, and the two are asserted to agree.
    ///
    /// Only the last episode can change once it exists: a closed episode is a
    /// finished span of `seq`, and nothing appended later alters what is inside it.
    /// So re-deriving is bounded by what is actually out of date.
    ///
    /// Returns how many summaries were written.
    pub fn digest_pending(&mut self, max_episodes: usize) -> Result<u64, StoreError> {
        let index = self.atom_index()?;
        if index.is_empty() {
            return Ok(0);
        }
        let segments = crate::digest::segment(&index);
        let digested_through = self.digested_through()?;
        let last = segments.len().saturating_sub(1);
        let pending: Vec<_> = segments
            .into_iter()
            .enumerate()
            .filter(|(position, segment)| *position == last || segment.seq_to > digested_through)
            .map(|(_, segment)| segment)
            .take(max_episodes.max(1))
            .collect();
        if pending.is_empty() {
            return Ok(0);
        }

        let mut written = 0u64;
        let mut highest = digested_through;
        for segment in pending {
            let views = self.atom_views(segment.seq_from, segment.seq_to)?;
            let end = segment.seq_to;
            let digest = crate::digest::build(segment, &views);
            let candidate = tail_summary(&digest);
            // The last episode is always a candidate, but it is usually already
            // derived and unchanged. Writing it again would be harmless to the
            // data and fatal to the steady state: the worker would never idle.
            let unchanged = self
                .summary(&candidate.id)?
                .map(|existing| same_summary(&existing, &candidate))
                .unwrap_or(false);
            if unchanged {
                highest = highest.max(end);
                continue;
            }
            self.replace_overlapping(&digest)?;
            self.put_summary(&candidate)?;
            highest = highest.max(end);
            written += 1;
        }
        // Advanced only to what was actually written, so an interrupted backlog
        // resumes instead of being skipped.
        self.set_meta("digested_through", &highest.to_string())?;
        Ok(written)
    }

    /// The `seq` through which episodes have been derived.
    fn digested_through(&self) -> Result<u64, StoreError> {
        Ok(self
            .meta("digested_through")?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0))
    }

    /// Remove stored summaries whose citations fall inside the digest being
    /// written — bounded, not a scan of every summary in the session.
    ///
    /// An earlier version loaded *all* summaries and filtered them in Rust, which
    /// made one appended task cost time proportional to the session's whole
    /// history: exactly the scaling this design exists to avoid. The candidate set
    /// is found in SQL by span overlap (an index exists on
    /// `summary(seq_from, seq_to)`), which leaves only the handful that can
    /// possibly intersect. Citations are still checked exactly, because a citation
    /// is the proven link to the raw events and leaving one dangling is the failure
    /// `verify` exists to catch.
    fn replace_overlapping(&mut self, digest: &crate::digest::Digest) -> Result<(), StoreError> {
        let (from, to) = (digest.segment.seq_from, digest.segment.seq_to);
        let candidates: Vec<String> = {
            let mut stmt = self.conn_ref().prepare(
                "SELECT id FROM summary
                 WHERE id != ?1 AND seq_from <= ?3 AND seq_to >= ?2
                 ORDER BY seq_to DESC",
            )?;
            let rows = stmt.query_map(params![digest.episode_id, from as i64, to as i64], |r| {
                r.get(0)
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let covered: std::collections::BTreeSet<u64> =
            digest.contributors.iter().copied().collect();
        let mut stale: Vec<String> = Vec::new();
        for id in candidates {
            let citations = self.citations_of(&id)?;
            if citations
                .iter()
                .any(|(a, b)| (*a..=*b).any(|seq| covered.contains(&seq)))
            {
                stale.push(id);
            }
        }
        if stale.is_empty() {
            return Ok(());
        }
        let conn = self.conn_mut();
        let tx = conn.transaction()?;
        for id in &stale {
            tx.execute("DELETE FROM citation WHERE summary_id = ?1", params![id])?;
            tx.execute(
                "DELETE FROM summary_entity WHERE summary_id = ?1",
                params![id],
            )?;
            tx.execute(
                "DELETE FROM summary_inputs WHERE summary_id = ?1",
                params![id],
            )?;
            tx.execute("DELETE FROM summary WHERE id = ?1", params![id])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Ask a model for the semantic fields of the L2 summaries that lack them.
    ///
    /// One call per episode, bounded by `max_calls`, and every call is charged
    /// against the caller's budget. The deterministic fields are never re-derived
    /// and never overwritten: the model only adds `rejected`, `why`,
    /// `open_questions` and a `conclusion` (the design note records why the rest is
    /// computed — a 3B model called a committed, passing task `open`).
    ///
    /// Returns how many summaries were enriched. A model failure degrades that one
    /// episode — it keeps its digest and is simply not enriched — rather than
    /// failing the pass, because a summariser that cannot answer must not be able to
    /// make the memory unusable.
    pub fn synthesize(
        &mut self,
        summarizer: &mut dyn crate::summarizer::Summarizer,
        max_calls: usize,
    ) -> Result<u64, StoreError> {
        Ok(self
            .synthesize_window(
                summarizer,
                WindowBudget::calls(max_calls),
                &crate::worker::IdleForeground,
            )?
            .enriched)
    }

    /// One **idle window** of semantic enrichment (ADR-0017 §4.6).
    ///
    /// The same work as [`Store::synthesize`], with the bounds the design actually
    /// names: how many model calls and how many tokens a window may spend, and
    /// whether the foreground is still quiet. The distinction from `synthesize` is
    /// not cosmetic — it is the unit the background worker runs and sleeps between,
    /// and the unit a measurement can quote ("one window enriched N episodes in T").
    ///
    /// Three rules that keep a dead endpoint from becoming a busy loop, and a busy
    /// foreground from waiting on one:
    ///
    /// * **A failure counts against the call budget.** Otherwise an unreachable
    ///   model would be re-asked once per stored episode in a single window.
    /// * **A failure is terminal for that model.** The `producer` records
    ///   `digest+model:<name> (failed: …)`, and such an episode is skipped next
    ///   time. Degrading to the digest is the designed outcome; retrying forever
    ///   would be the worker never reaching the steady state §4.5 requires. A
    ///   *different* model name is a different question and is asked.
    /// * **The foreground is re-checked before every call**, so a turn that starts
    ///   in the middle of a window takes effect at the next call boundary rather
    ///   than after the whole window (§4.6's yield).
    ///
    /// The input token estimate is charged before the call and the answer's estimate
    /// after, so a window that hits its token bound stops before sending rather than
    /// after.
    pub fn synthesize_window(
        &mut self,
        summarizer: &mut dyn crate::summarizer::Summarizer,
        budget: WindowBudget,
        foreground: &dyn crate::worker::Foreground,
    ) -> Result<WindowOutcome, StoreError> {
        let mut outcome = WindowOutcome::default();
        if budget.calls == 0 || budget.tokens == 0 {
            outcome.remaining = self.pending_synthesis()?;
            return Ok(outcome);
        }
        let producer_marker = format!("digest+model:{}", summarizer.name());
        let failed_prefix = format!("{producer_marker} (failed:");
        // Select the candidates in SQL rather than paging the whole level with
        // `OFFSET`. An `OFFSET` walk re-reads every already-enriched episode on
        // every window, which turns "enrich the few that are new" into work
        // proportional to the session's whole history — the exact scaling this
        // design exists to avoid. `instr(...) <> 1` is the "does not start with"
        // test, used instead of `NOT LIKE` because a model name may contain `_`,
        // which `LIKE` treats as a wildcard.
        let wanted = (budget.calls * 2).max(1) as i64;
        let candidates: Vec<String> = {
            let mut stmt = self.conn_ref().prepare(
                "SELECT id FROM summary
                 WHERE level = 2 AND producer <> ?1 AND instr(producer, ?2) <> 1
                 ORDER BY seq_to DESC LIMIT ?3",
            )?;
            let rows = stmt.query_map(params![producer_marker, failed_prefix, wanted], |r| {
                r.get::<_, String>(0)
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut spent = 0u64;
        for id in candidates {
            if outcome.calls as usize >= budget.calls {
                break;
            }
            // A turn may have started while this window was running: yield at the
            // call boundary rather than finish the batch. The store is untouched, so
            // the work simply waits for the next window.
            if foreground.is_busy() {
                break;
            }
            // Re-read by id: a foreground digest may have replaced this episode
            // between the candidate query and now (the worker is asked to yield
            // between windows, not in the middle of one). If it is gone or already
            // handled, this window simply skips it.
            let Some(current) = self.summary(&id)? else {
                continue;
            };
            if current.producer == producer_marker || current.producer.starts_with(&failed_prefix) {
                continue;
            }
            let views = self.atom_views(current.seq_from, current.seq_to)?;
            let digest = crate::digest::build(
                crate::digest::Segment {
                    seq_from: current.seq_from,
                    seq_to: current.seq_to,
                    events: views.len(),
                    first_time_ms: current.created_at,
                    last_time_ms: current.updated_at,
                },
                &views,
            );
            let input = crate::summarizer::DigestInput::from_digest(&digest);
            let input_tokens = crate::digest::estimate_tokens(&input.prompt());
            // Stop before a call that would blow the token budget — but never before
            // the first one, or a window smaller than one prompt would do nothing
            // forever.
            if outcome.calls > 0 && spent + input_tokens > budget.tokens {
                break;
            }
            outcome.calls += 1;
            outcome.input_tokens += input_tokens;
            spent += input_tokens;
            match summarizer.summarize(&input) {
                Ok(synthesis) if !synthesis.is_empty() => {
                    let output_tokens = synthesis.estimated_tokens();
                    let content = crate::digest::merge_synthesis(&current.content, &synthesis);
                    let tokens = crate::digest::estimate_tokens(&current.title)
                        + crate::digest::estimate_tokens(&content);
                    // Conditional: if the foreground re-derived this episode while the
                    // model was answering, this write is a no-op and the episode is
                    // enriched in a later window instead of a stale one being restored.
                    if self.record_synthesis(&current, &content, &producer_marker, tokens)? {
                        outcome.enriched += 1;
                        outcome.output_tokens += output_tokens;
                        spent += output_tokens;
                    }
                }
                Ok(_) => {
                    // An empty answer is a legitimate outcome: there was nothing to
                    // say. Marked so it is not asked again.
                    self.record_synthesis(
                        &current,
                        &current.content,
                        &producer_marker,
                        current.tokens,
                    )?;
                }
                Err(e) => {
                    // Degrade this episode and keep going. The reason travels with
                    // the summary rather than being swallowed.
                    let reason = e.to_string();
                    let producer = format!("{producer_marker} (failed: {reason})");
                    self.record_synthesis(&current, &current.content, &producer, current.tokens)?;
                    outcome.failed += 1;
                    outcome.last_error = Some(reason);
                }
            }
        }
        outcome.remaining = self.pending_synthesis()?;
        Ok(outcome)
    }

    /// Record a model's contribution **only if the episode is still the one the
    /// model was asked about**, and report whether the write happened.
    ///
    /// The worker runs outside the foreground's turn, but a *window* can straddle a
    /// new turn: the foreground may re-digest the open episode while the model is
    /// answering. Writing the old digest back would undo the foreground's work and
    /// resurrect a stale boundary — and `put_summary` is a replace, so it would do
    /// exactly that. This is one conditional `UPDATE`, so the check and the write are
    /// atomic and the loser is simply left for a later window. The deterministic
    /// fields are not touched at all: this is the "the model may add, never rewrite"
    /// rule enforced at the storage layer.
    pub(crate) fn record_synthesis(
        &mut self,
        before: &Summary,
        content: &str,
        producer: &str,
        tokens: u64,
    ) -> Result<bool, StoreError> {
        let changed = self.conn_ref().execute(
            "UPDATE summary SET content = ?2, producer = ?3, tokens = ?4
             WHERE id = ?1 AND content = ?5",
            params![before.id, content, producer, tokens as i64, before.content],
        )?;
        Ok(changed == 1)
    }

    /// L2 episodes that still lack semantic fields — the worker's queue depth.
    ///
    /// An episode a model attempted (successfully or not) is not pending: the
    /// failure is recorded in its `producer`, and [`Store::degraded_synthesis`]
    /// counts those. "Nothing left to enrich" and "enrichment tried and failed" are
    /// different facts and must not be folded into one number.
    pub fn pending_synthesis(&self) -> Result<u64, StoreError> {
        let n: i64 = self.conn_ref().query_row(
            "SELECT COUNT(*) FROM summary WHERE level = 2 AND producer NOT LIKE 'digest+model:%'",
            [],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// L2 episodes whose enrichment failed and was recorded as such.
    pub fn degraded_synthesis(&self) -> Result<u64, StoreError> {
        let n: i64 = self.conn_ref().query_row(
            "SELECT COUNT(*) FROM summary WHERE producer LIKE '%(failed:%'",
            [],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// `(count, tokens)` for one layer, for the honesty surface. A layer the store
    /// has no rows for is `(0, 0)` rather than an error.
    pub fn layer_stats(&self, level: u32) -> Result<(u64, u64), StoreError> {
        let (count, tokens): (i64, Option<i64>) = self.conn_ref().query_row(
            "SELECT COUNT(*), SUM(tokens) FROM summary WHERE level = ?1",
            params![level as i64],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((count as u64, tokens.unwrap_or(0).max(0) as u64))
    }

    /// Discard every derived summary and rebuild it from the atoms.
    pub fn rebuild_summaries(&mut self) -> Result<u64, StoreError> {
        {
            let conn = self.conn_mut();
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM summary_inputs", [])?;
            tx.execute("DELETE FROM citation", [])?;
            tx.execute("DELETE FROM summary_entity", [])?;
            tx.execute("DELETE FROM summary", [])?;
            tx.execute("DELETE FROM meta WHERE key = 'digested_through'", [])?;
            tx.commit()?;
        }
        self.digest()
    }

    pub fn summary(&self, id: &str) -> Result<Option<Summary>, StoreError> {
        let mut stmt = self.conn_ref().prepare(
            "SELECT id, level, scope, title, content, status, producer, schema_ver,
                    seq_from, seq_to, superseded_by, domain, tokens, created_at, updated_at
             FROM summary WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let mut summary = row_to_summary(row)?;
        drop(rows);
        summary.citations = self.citations_of(&summary.id)?;
        summary.inputs = self.inputs_of(&summary.id)?;
        Ok(Some(summary))
    }

    fn citations_of(&self, id: &str) -> Result<Vec<(u64, u64)>, StoreError> {
        let mut stmt = self.conn_ref().prepare(
            "SELECT seq_from, seq_to FROM citation WHERE summary_id = ?1 ORDER BY seq_from",
        )?;
        let rows = stmt.query_map(params![id], |r| {
            Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    fn inputs_of(&self, id: &str) -> Result<Vec<SummaryInput>, StoreError> {
        let mut stmt = self.conn_ref().prepare(
            "SELECT input_id, seq_from, seq_to FROM summary_inputs WHERE summary_id = ?1",
        )?;
        let rows = stmt.query_map(params![id], |r| {
            Ok(SummaryInput {
                summary_id: r.get(0)?,
                seq_from: r.get::<_, Option<i64>>(1)?.map(|v| v as u64),
                seq_to: r.get::<_, Option<i64>>(2)?.map(|v| v as u64),
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Summaries at one level, newest first.
    pub fn summaries_at(&self, level: u32, limit: usize) -> Result<Vec<Summary>, StoreError> {
        let mut stmt = self.conn_ref().prepare(
            "SELECT id, level, scope, title, content, status, producer, schema_ver,
                    seq_from, seq_to, superseded_by, domain, tokens, created_at, updated_at
             FROM summary WHERE level = ?1 ORDER BY seq_to DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![level as i64, limit as i64], row_to_summary)?;
        let mut summaries = rows.collect::<Result<Vec<_>, _>>()?;
        for summary in &mut summaries {
            summary.citations = self.citations_of(&summary.id)?;
            summary.inputs = self.inputs_of(&summary.id)?;
        }
        Ok(summaries)
    }

    pub fn summary_count(&self, level: u32) -> Result<u64, StoreError> {
        let n: i64 = self.conn_ref().query_row(
            "SELECT COUNT(*) FROM summary WHERE level = ?1",
            params![level as i64],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// Search summaries by keyword.
    ///
    /// Every term is its own `LIKE` and all of them must match. The same rule as
    /// the atoms, for the same reason: a term shorter than the trigram width (the
    /// common Chinese two-character word) makes an entire FTS5 `MATCH` query
    /// return nothing, silently. Measured at 5,000 summaries a scan costs 1-2 ms,
    /// which is why there is no full-text table here at all — the reasoning and
    /// the numbers are in `crates/context/src/store.rs` and `tests/search_cost.rs`.
    pub fn recall(&self, query: &SearchQuery) -> Result<Vec<Summary>, StoreError> {
        self.recall_at(None, query)
    }

    /// The same, restricted to one level (2, 3 or 4).
    pub fn recall_at(
        &self,
        level: Option<u32>,
        query: &SearchQuery,
    ) -> Result<Vec<Summary>, StoreError> {
        let mut where_parts: Vec<String> = Vec::new();
        let mut values: Vec<rusqlite::types::Value> = Vec::new();
        for term in &query.terms {
            let term = term.trim();
            if term.is_empty() {
                continue;
            }
            where_parts.push(format!(
                "(s.title LIKE ?{n} OR s.content LIKE ?{n})",
                n = values.len() + 1
            ));
            values.push(rusqlite::types::Value::Text(format!("%{term}%")));
        }
        if let Some(level) = level {
            where_parts.push(format!("s.level = ?{}", values.len() + 1));
            values.push(rusqlite::types::Value::Integer(level as i64));
        }
        values.push(rusqlite::types::Value::Integer(query.limit.max(1) as i64));
        let sql = format!(
            "SELECT s.id, s.level, s.scope, s.title, s.content, s.status, s.producer, s.schema_ver,
                    s.seq_from, s.seq_to, s.superseded_by, s.domain, s.tokens, s.created_at, s.updated_at
             FROM summary s {where_clause} ORDER BY s.seq_to DESC LIMIT ?{limit}",
            where_clause = if where_parts.is_empty() {
                String::new()
            } else {
                format!("WHERE {}", where_parts.join(" AND "))
            },
            limit = values.len(),
        );
        let mut stmt = self.conn_ref().prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(values), row_to_summary)?;
        let mut summaries = rows.collect::<Result<Vec<_>, _>>()?;
        for summary in &mut summaries {
            summary.citations = self.citations_of(&summary.id)?;
            summary.inputs = self.inputs_of(&summary.id)?;
        }
        Ok(summaries)
    }

    /// Expand a summary: its full text **plus the original evidence it cites**.
    ///
    /// Returning the raw evidence is not a convenience — it is the mechanism that
    /// stops a summary from being read as the truth. The caller always sees the
    /// events the summary claims to rest on, in the same response.
    pub fn expand(&self, id: &str, max_atoms: usize) -> Result<Option<Expansion>, StoreError> {
        let Some(summary) = self.summary(id)? else {
            return Ok(None);
        };
        let mut evidence = Vec::new();
        for (from, to) in &summary.citations {
            if evidence.len() >= max_atoms {
                break;
            }
            for atom in self.atom_views(*from, *to)? {
                if evidence.len() >= max_atoms {
                    break;
                }
                evidence.push(atom);
            }
        }
        Ok(Some(Expansion { summary, evidence }))
    }
}

/// Whether two stored summaries say the same thing. Timestamps are excluded on
/// purpose: they are derived from the evidence, and an episode whose events have
/// not changed must not look changed because a worker looked at it again.
fn same_summary(a: &Summary, b: &Summary) -> bool {
    a.title == b.title
        && a.content == b.content
        && a.status == b.status
        && a.seq_from == b.seq_from
        && a.seq_to == b.seq_to
        && a.citations == b.citations
        && a.superseded_by == b.superseded_by
}

/// The L2 row for a digest. One constructor, so the incremental and the full path
/// cannot produce different summaries for the same episode.
fn tail_summary(digest: &crate::digest::Digest) -> Summary {
    Summary {
        id: digest.episode_id.clone(),
        level: 2,
        scope: format!("episode:{}", digest.episode_id),
        title: digest.title.clone(),
        content: digest.content.clone(),
        status: digest.status.clone(),
        producer: "deterministic".to_string(),
        schema_ver: SUMMARY_SCHEMA_VERSION,
        seq_from: digest.segment.seq_from,
        seq_to: digest.segment.seq_to,
        superseded_by: None,
        // The domain comes from the file the episode actually changed. Using the
        // `module` entities instead would let an arbitrary token in prose decide
        // which subsystem an episode belongs to — which is how `src/kernel` once
        // came out as `2：src/kernel`.
        domain: digest
            .touched_files
            .first()
            .map(|file| crate::synthesis::domain_of(file))
            .or_else(|| {
                digest
                    .modules
                    .first()
                    .map(|m| crate::synthesis::domain_of(m))
            }),
        tokens: digest.tokens,
        // From the evidence, not the clock: a digest of the same events must be
        // the same digest, timestamps included.
        created_at: digest.segment.last_time_ms,
        updated_at: digest.segment.last_time_ms,
        citations: digest.citations.clone(),
        inputs: vec![SummaryInput {
            summary_id: None,
            seq_from: Some(digest.segment.seq_from),
            seq_to: Some(digest.segment.seq_to),
        }],
    }
}

fn row_to_summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<Summary> {
    Ok(Summary {
        id: row.get(0)?,
        level: row.get::<_, i64>(1)? as u32,
        scope: row.get(2)?,
        title: row.get(3)?,
        content: row.get(4)?,
        status: row.get(5)?,
        producer: row.get(6)?,
        schema_ver: row.get::<_, i64>(7)? as u32,
        seq_from: row.get::<_, i64>(8)? as u64,
        seq_to: row.get::<_, i64>(9)? as u64,
        superseded_by: row.get(10)?,
        domain: row.get(11)?,
        tokens: row.get::<_, i64>(12)? as u64,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
        citations: Vec::new(),
        inputs: Vec::new(),
    })
}
