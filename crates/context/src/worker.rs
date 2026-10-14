//! The background worker: incremental, budgeted, and idle when there is nothing
//! to do (ADR-0017 decision 7 and 8).
//!
//! The whole design here is about *not* fighting the foreground:
//!
//! * **Bounded batches.** Every step reads at most [`WorkerConfig::max_events`]
//!   events and re-derives at most one summary — the open tail episode, which is
//!   the only one that can have changed. Nothing here scales with session age.
//! * **A budget per activation.** [`Budget`] counts steps, events and model calls
//!   so a worker woken by a burst cannot run away with the machine.
//! * **Yielding, not pre-empting.** [`Foreground::is_busy`] is consulted before
//!   each step and between them. The worker cannot interrupt a turn; it only
//!   declines to start work while one is running.
//! * **Sleeping, not spinning.** A step with nothing to do reports [`Step::Idle`]
//!   and the caller sleeps. There is no busy loop, and no timer that keeps the CPU
//!   warm for no reason.
//!
//! The step logic is deliberately a plain function, not a thread: a test can drive
//! it deterministically, and "the worker does not slow a turn down" becomes a
//! measurable statement rather than a hopeful one.

use crate::store::{Store, StoreError, SyncOutcome};
use std::time::{Duration, Instant};

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
    fn default() -> Self {
        Self {
            max_events: 256,
            max_time: Duration::from_millis(500),
            idle_sleep: Duration::from_millis(250),
            digest: true,
        }
    }
}

/// Whether the thing the user is waiting on is running. The worker never starts a
/// step while this says yes.
pub trait Foreground {
    fn is_busy(&self) -> bool;
}

/// A foreground that is never busy — for a one-shot build or a test. Named rather
/// than folded into an `Option` so the intent at a call site is legible.
pub struct IdleForeground;

impl Foreground for IdleForeground {
    fn is_busy(&self) -> bool {
        false
    }
}

impl<F: Fn() -> bool> Foreground for F {
    fn is_busy(&self) -> bool {
        self()
    }
}

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

    pub fn spent(&self) -> bool {
        self.steps_left == 0 || self.events_left == 0 || Instant::now() >= self.deadline
    }
}

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
    /// Did the last activation actually do anything?
    pub fn did_work(&self) -> bool {
        self.events > 0 || self.digest_changes > 0
    }
}

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

    #[test]
    fn a_spent_budget_stops_stepping() {
        let config = WorkerConfig {
            max_time: Duration::from_millis(0),
            ..WorkerConfig::default()
        };
        let budget = Budget::new(&config);
        assert!(budget.spent(), "a zero-length activation is already over");
    }

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
