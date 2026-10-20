//! Purpose: The background worker: incremental, budgeted, yielding to the foreground and idle when there is nothing to do.
//!
//! Serves: the worker never slowing a turn down and never changing what the agent remembers, whether or not it ran.
//!
//! # Contract
//! - **Design:** `docs/context/pipeline.md` §6.6; `docs/context/status.md` §11.1.
//! - **Upstream:** The kernel's `ContextEngine::refresh` and its worker thread drive it; the worker tests drive it directly.
//! - **Downstream:** `Store::sync`, `Store::digest_pending` and `Store::digest_domains`.
//! - **Verify:** `cargo test -p loong-context` · `./scripts/check.sh`.

use crate::store::{Store, StoreError, SyncOutcome};
use std::time::{Duration, Instant};

/// Purpose: The bounds of one activation: events, wall clock, sleep and whether to digest.
/// Serves: a burst being bounded by configuration rather than by how much happened.
/// Design: `docs/context/pipeline.md` §6.6.
/// Up: `Budget::new`, `step` and `run_activation`; the worker tests.
/// Down: `Default` and `Budget::new`.
/// Verify: `cargo test -p loong-context --test worker an_activation_is_bounded`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerConfig {
    /// The most events one step will read off the tail.
    pub max_events: usize,
    /// Wall-clock ceiling for one activation.
    pub max_time: Duration,
    /// How long an idle worker sleeps before looking again.
    pub idle_sleep: Duration,
    /// Do the deterministic L1 digest after ingesting. Cheap, and it is what
    /// makes "the original is always findable" true without a model.
    pub digest: bool,
}

impl Default for WorkerConfig {
    /// Purpose: The measured defaults: 256 events, 500 ms, 250 ms sleep, digest on.
    /// Contract: `worker.rs` configuration.
    fn default() -> Self {
        Self {
            max_events: 256,
            max_time: Duration::from_millis(500),
            idle_sleep: Duration::from_millis(250),
            digest: true,
        }
    }
}

/// Purpose: Whether the thing the user is waiting on is running.
/// Serves: the worker declining to start while a turn is in flight.
/// Design: `docs/context/pipeline.md` §6.6.
/// Up: `worker::step`, `Store::synthesize_window` and the kernel's busy flag.
/// Down: `is_busy` below.
/// Verify: `cargo test -p loong-context --test worker the_worker_does_not_start_while_the_foreground_is_busy`.
/// Whether the thing the user is waiting on is running. The worker never starts a
/// step while this says yes.
pub trait Foreground {
    /// Purpose: Answer whether a turn is currently running.
    fn is_busy(&self) -> bool;
}

/// Purpose: A foreground that is never busy.
/// Serves: one-shot builds and tests having a named, legible idle signal.
/// Design: `docs/context/pipeline.md` §6.6.
/// Up: The worker tests and the kernel's measurement; no production caller.
/// Down: Its `Foreground` impl.
/// Verify: `cargo test -p loong-context --test worker`.
/// A foreground that is never busy — for a one-shot build or a test. Named rather
/// than folded into an `Option` so the intent at a call site is legible.
pub struct IdleForeground;

impl Foreground for IdleForeground {
    /// Purpose: Always idle.
    /// Contract: `worker.rs` `Foreground` impl.
    fn is_busy(&self) -> bool {
        false
    }
}

impl<F: Fn() -> bool> Foreground for F {
    /// Purpose: Adapt a closure into a `Foreground`.
    /// Contract: `worker.rs` `Foreground` impl for test and kernel closures.
    fn is_busy(&self) -> bool {
        self()
    }
}

/// Purpose: What one activation may spend: steps, events, model calls and a deadline.
/// Serves: a burst not being absorbed by a long-running worker while a turn waits.
/// Design: `docs/context/pipeline.md` §6.6.
/// Up: `worker::step`, `run_activation` and the worker tests.
/// Down: `Budget::new` and `Budget::spent`.
/// Verify: `cargo test -p loong-context --test worker an_activation_is_bounded`.
/// What one activation may spend. Counts rather than sleeps, so a burst cannot be
/// absorbed by a long-running worker while a turn waits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Budget {
    pub steps_left: usize,
    pub events_left: usize,
    /// Model calls, which are the expensive part. This deterministic worker spends
    /// none — the semantic enrichment worker in the kernel carries its own
    /// [`crate::WindowBudget`] — but the accounting lives here so a caller can see
    /// the shape of an activation before there is anything to charge for it.
    pub model_calls_left: usize,
    pub deadline: Instant,
}

impl Budget {
    /// Purpose: A budget derived from the config, with fixed step and model-call ceilings.
    /// Serves: one activation being a slice of idle time, not a shift.
    /// Design: `docs/context/pipeline.md` §6.6.
    /// Up: `run_activation` and the worker tests.
    /// Down: Reads `WorkerConfig`; no other callee.
    /// Verify: `cargo test -p loong-context --test worker a_spent_budget_stops_stepping`.
    pub fn new(config: &WorkerConfig) -> Self {
        Self {
            // A bounded number of steps per activation, whatever the budget's
            // shape: an activation is a slice of idle time, not a shift.
            steps_left: 8,
            events_left: config.max_events * 8,
            model_calls_left: 4,
            deadline: Instant::now() + config.max_time,
        }
    }

    /// Purpose: Whether the activation has no steps, events or time left.
    /// Serves: the loop stopping without needing a clock check of its own.
    /// Design: `docs/context/pipeline.md` §6.6.
    /// Up: `worker::step` and `run_activation`.
    /// Down: Reads the counters and the deadline; no callee.
    /// Verify: `cargo test -p loong-context --test worker a_spent_budget_stops_stepping`.
    pub fn spent(&self) -> bool {
        self.steps_left == 0 || self.events_left == 0 || Instant::now() >= self.deadline
    }
}

/// Purpose: What one step did: active, idle or deferred.
/// Serves: a caller knowing whether to sleep without measuring the machine.
/// Design: `docs/context/pipeline.md` §6.6.
/// Up: `worker::step` and `run_activation`; the worker tests.
/// Down: Matched by the activation loop and the tests.
/// Verify: `cargo test -p loong-context --test worker the_idle_path_costs_a_stat_not_a_scan`.
/// What a single step did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// New events were materialised, or a summary was re-derived.
    Active,
    /// Nothing to do: the tail is where it should be and the digest is current.
    Idle,
    /// The foreground is running, or the budget is spent.
    Deferred,
}

/// Purpose: Running totals of what the background spent.
/// Serves: "what has the background cost me?" being queryable rather than inferred.
/// Design: `docs/context/status.md` §11.1.
/// Up: `worker::step` and `run_activation`; the worker tests.
/// Down: `WorkerStats::did_work`.
/// Verify: `cargo test -p loong-context --test worker an_activation_is_bounded`.
/// Running totals, so "what has the background cost me?" is answerable — the
/// design note requires the worker's spend to be queryable rather than inferred.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkerStats {
    pub activations: u64,
    pub steps: u64,
    pub events: u64,
    pub atoms: u64,
    pub digest_changes: u64,
    /// L3 domain maps re-derived.
    pub domain_changes: u64,
    pub rebuilds: u64,
    pub deferrals: u64,
}

impl WorkerStats {
    /// Purpose: Whether the last activation changed anything.
    /// Serves: a caller distinguishing real work from a wake-up.
    /// Design: `docs/context/status.md` §11.1.
    /// Up: No caller in the tree today.
    /// Down: Reads the counters; no callee.
    /// Verify: `cargo test -p loong-context --test worker`.
    /// Did the last activation actually do anything?
    pub fn did_work(&self) -> bool {
        self.events > 0 || self.digest_changes > 0
    }
}

/// Purpose: One bounded step of background work.
/// Serves: reading only what was appended, deriving only what is behind and deferring while a turn runs.
/// Design: `docs/context/pipeline.md` §6.6.
/// Up: `run_activation` and the worker tests; the kernel drives its own loop over `Store` directly.
/// Down: `Store::sync`, `Store::rebuild`, `Store::digest_pending`, `Store::digest_domains` and `Foreground::is_busy`.
/// Verify: `cargo test -p loong-context --test worker the_worker_does_not_start_while_the_foreground_is_busy`.
/// One step of background work on a store.
///
/// Returns [`Step::Deferred`] when the foreground is busy or the budget is spent,
/// [`Step::Idle`] when there is genuinely nothing to do, and [`Step::Active`]
/// when it changed something.
///
/// A log that is shorter than the cursor (truncated or replaced) is rebuilt here
/// rather than reported: unlike [`Store::sync`], a worker is the component whose
/// job it is to keep going, and the derived store is disposable by construction.
pub fn step(
    store: &mut Store,
    log: &std::path::Path,
    config: &WorkerConfig,
    budget: &mut Budget,
    foreground: &dyn Foreground,
    stats: &mut WorkerStats,
) -> Result<Step, StoreError> {
    if foreground.is_busy() {
        stats.deferrals += 1;
        return Ok(Step::Deferred);
    }
    if budget.spent() {
        stats.deferrals += 1;
        return Ok(Step::Deferred);
    }

    let max = config.max_events.min(budget.events_left);
    let outcome = store.sync(max)?;
    match outcome {
        SyncOutcome::NeedsRebuild => {
            store.rebuild(log)?;
            stats.rebuilds += 1;
            budget.steps_left -= 1;
            return Ok(Step::Active);
        }
        SyncOutcome::Synced { events, atoms } => {
            budget.events_left -= events.min(budget.events_left);
            budget.steps_left -= 1;
            stats.steps += 1;
            stats.events += events as u64;
            stats.atoms += atoms;
            if config.digest {
                // Bounded per step: an activation must not turn into a shift.
                let written = store.digest_pending(4)?;
                stats.digest_changes += written;
                // L3 rolls up from the episodes that just changed. Cheap, and it
                // reads summaries rather than events, so it cannot grow with the
                // session's history.
                stats.domain_changes += store.digest_domains()?;
            }
            return Ok(Step::Active);
        }
        SyncOutcome::Idle => {
            // The tail is where it should be. Re-deriving the open episode is
            // still worth a look the first time (a store that was built but never
            // digested), and cheap afterwards because `digest_tail` reports
            // "unchanged" without writing.
            if config.digest && store.atom_count()? > 0 {
                // Catch up on anything not yet derived, then report Idle. The
                // watermark makes this a no-op in the steady state rather than a
                // re-derivation of the tail on every wake-up.
                budget.steps_left -= 1;
                let written = store.digest_pending(4)?;
                let domains = store.digest_domains()?;
                stats.steps += 1;
                stats.domain_changes += domains;
                if written > 0 || domains > 0 {
                    stats.digest_changes += written;
                    return Ok(Step::Active);
                }
            }
            return Ok(Step::Idle);
        }
    }
}

/// Purpose: Drive one activation until idle, deferred or out of budget.
/// Serves: the thread body's loop being the embedder's business, not this crate's.
/// Design: `docs/context/pipeline.md` §6.6.
/// Up: The worker tests; no caller in production code.
/// Down: `Budget::new` and `worker::step`.
/// Verify: `cargo test -p loong-context --test worker an_activation_is_bounded_and_then_idles`.
/// Drive one activation: keep stepping until idle, deferred or out of budget.
///
/// Returns the last step, so a caller can decide whether to sleep. This is the
/// function a thread body calls in a loop; the loop itself is the embedder's
/// business, which is what keeps this crate free of a runtime.
pub fn run_activation(
    store: &mut Store,
    log: &std::path::Path,
    config: &WorkerConfig,
    foreground: &dyn Foreground,
    stats: &mut WorkerStats,
) -> Result<Step, StoreError> {
    let mut budget = Budget::new(config);
    stats.activations += 1;
    let mut last = Step::Idle;
    while !budget.spent() {
        last = step(store, log, config, &mut budget, foreground, stats)?;
        if last != Step::Active {
            break;
        }
    }
    Ok(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Purpose: Pin that a zero-length activation is already spent and stops stepping.
    #[test]
    fn a_spent_budget_stops_stepping() {
        let config = WorkerConfig {
            max_time: Duration::from_millis(0),
            ..WorkerConfig::default()
        };
        let budget = Budget::new(&config);
        assert!(budget.spent(), "a zero-length activation is already over");
    }

    /// Purpose: Pin that a busy foreground defers before reading anything from the log.
    #[test]
    fn the_foreground_predicate_is_what_defers() {
        let mut stats = WorkerStats::default();
        let mut budget = Budget::new(&WorkerConfig::default());
        let mut store = Store::in_memory().unwrap();
        let path = std::path::Path::new("/nonexistent");
        let busy = || true;
        let step = step(
            &mut store,
            path,
            &WorkerConfig::default(),
            &mut budget,
            &busy,
            &mut stats,
        )
        .unwrap();
        assert_eq!(step, Step::Deferred);
        assert_eq!(stats.deferrals, 1);
        // Nothing was read from the log: being busy means not starting.
        assert_eq!(stats.events, 0);
    }
}
