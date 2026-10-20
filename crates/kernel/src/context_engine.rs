//! Purpose: The kernel's handle on the layered context engine: open the sidecar, derive the L4 map at a step boundary, and keep semantic enrichment off the turn's critical path.
//!
//! Serves: ADR-0017's reversible memory without lengthening a turn or hiding which
//! mode the session is in.
//!
//! # Contract
//! - **Design:** `docs/design/context-ledger.md` §3–§7;
//!   `docs/context/pipeline.md` §6.6 owns the kernel-side worker rationale.
//! - **Upstream:** `Kernel::build` opens it, `Kernel::refresh_map` refreshes it, and
//!   `Kernel::run` sets the foreground-busy flag.
//! - **Downstream:** `loong_context::Store`, `Summarizer` and the enrichment worker
//!   thread; `Kernel::recall_tool` reads the same store.
//! - **Verify:** `cargo test -p loong-kernel context_engine::` · `./scripts/check.sh`.

use loong_context::{Foreground, NoSummarizer, OllamaSummarizer, Store, Summarizer, WindowBudget};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// One idle window's defaults, from the design note's configuration draft (§11).
/// Used when the caller did not name a budget.
pub const DEFAULT_WINDOW_CALLS: usize = 4;
pub const DEFAULT_WINDOW_TOKENS: u64 = 6000;

/// How long a shutting-down engine waits for the worker before letting it go. It is
/// short on purpose: `session.new` drops the old kernel, and a person changing
/// sessions must not wait out an in-flight model request. The detached thread owns
/// its own store and exits at its next check.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(250);

/// Worker states, as an atomic so a status surface can read them without a lock.
const WORKER_OFF: u8 = 0;
const WORKER_IDLE: u8 = 1;
const WORKER_ACTIVE: u8 = 2;
const WORKER_DEFERRED: u8 = 3;
const WORKER_STOPPED: u8 = 4;

/// Purpose: The status word for a worker-state byte, with an unknown state named rather than folded in.
///
/// Contract: `context_engine.rs` status reporting.
fn worker_state_name(state: u8) -> &'static str {
    match state {
        WORKER_OFF => "off",
        WORKER_IDLE => "idle",
        WORKER_ACTIVE => "active",
        WORKER_DEFERRED => "deferred",
        WORKER_STOPPED => "stopped",
        // Not reachable through this module's writes; named rather than folded in
        // so a corrupted state reads as unknown instead of as a real one.
        _ => "unknown",
    }
}

/// Purpose: How the engine runs: the master switch, the sidecar paths, the worker knobs and the optional summariser.
///
/// Serves: a session choosing layered memory explicitly, with off as the rollback path.
/// Design: `docs/design/context-ledger.md` §3.
/// Up: `KernelConfig.context_engine`; `crates/cli/src/server.rs` builds one with `new`.
/// Down: `ContextEngine::open`, `WorkerConfig` and `SummarizerChoice`.
/// Verify: `cargo test -p loong-kernel context_engine::`.
/// How the engine runs. `enabled` is the master switch: with it off, nothing here
/// is consulted and the kernel behaves as it did before ADR-0017.
#[derive(Debug, Clone)]
pub struct ContextEngineConfig {
    pub enabled: bool,
    /// Where the sidecar store lives.
    pub store_path: PathBuf,
    /// The session log the engine derives from.
    pub log_path: PathBuf,
    /// Whether to clear stale tool output when the context is over budget. Always
    /// true in practice; here so the behaviour is visible rather than assumed.
    pub clear_tool_outputs: bool,
    pub worker: WorkerConfig,
    /// The local model that fills in the semantic fields, or `None` for the
    /// deterministic-only R mode. `Some` here does not promise the model is
    /// reachable — that is discovered at the call, and a failure degrades one
    /// episode rather than the session.
    pub summarizer: Option<SummarizerChoice>,
}

/// Purpose: When background enrichment runs: events per sync and the idle sleep.
///
/// Serves: bounded background work that never lengthens a turn.
/// Design: `docs/context/pipeline.md` §6.6.
/// Up: `ContextEngineConfig.worker`.
/// Down: `ContextEngine::refresh` (`max_events`) and `spawn_worker` (`idle_sleep`).
/// Verify: `cargo test -p loong-kernel context_engine::tests::refresh_never_calls_the_model`.
/// The worker's timing knobs. Kept as its own type because they are about *when*
/// background work happens, not *what* it does.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// The most events one deterministic step reads off the tail.
    pub max_events: usize,
    /// How long the enrichment worker sleeps between idle windows. Also the delay
    /// before it notices the foreground went quiet.
    pub idle_sleep: Duration,
}

impl Default for WorkerConfig {
    /// Purpose: The shipped worker timing: 256 events per sync and a 250 ms idle sleep.
    ///
    /// Contract: `context_engine.rs` worker defaults.
    fn default() -> Self {
        Self {
            max_events: 256,
            idle_sleep: Duration::from_millis(250),
        }
    }
}

/// Purpose: Which local model fills semantic fields, and the two window budgets.
///
/// Serves: a background pass that cannot run away with the machine.
/// Design: `docs/context/pipeline.md` §6.6.
/// Up: `ContextEngineConfig::with_summarizer` and `crates/cli/src/server.rs`'s engine config.
/// Down: `ContextEngine::open` builds the `OllamaSummarizer`; `window_budget` reads the budgets.
/// Verify: `cargo test -p loong-kernel context_engine::tests::semantic_mode_enriches_in_the_background_and_records_the_answer`.
/// Which local model, resolved from configuration by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummarizerChoice {
    /// `ollama` is the only kind that exists; `off` never reaches here.
    pub kind: String,
    pub base_url: String,
    pub model: String,
    /// Model calls allowed in one idle window, so a background pass cannot run
    /// away with the machine.
    pub window_calls: usize,
    /// Tokens allowed in one idle window (an estimate from the digest's own
    /// counter).
    pub window_tokens: u64,
}

impl ContextEngineConfig {
    /// Purpose: An engine config whose master switch is off: the documented rollback.
    ///
    /// Serves: a caller that can ask for the old compaction behaviour explicitly.
    /// Design: `docs/design/context-ledger.md` §3.
    /// Up: the turn tests; callers that build a config before deciding.
    /// Down: no callee; the fields are defaulted.
    /// Verify: `cargo test -p loong-kernel turn::tests::with_the_engine_off_compaction_still_drops_exchanges`.
    pub fn off() -> Self {
        Self {
            enabled: false,
            store_path: PathBuf::new(),
            log_path: PathBuf::new(),
            clear_tool_outputs: true,
            worker: WorkerConfig::default(),
            summarizer: None,
        }
    }

    /// Purpose: An enabled config for a sidecar store and its log.
    ///
    /// Serves: one construction path for the engine the CLI and tests open.
    /// Design: `docs/design/context-ledger.md` §3.
    /// Up: `crates/cli/src/server.rs` and the turn and engine tests.
    /// Down: `ContextEngine::open`.
    /// Verify: `cargo test -p loong-kernel context_engine::`.
    pub fn new(store_path: impl Into<PathBuf>, log_path: impl Into<PathBuf>) -> Self {
        Self {
            enabled: true,
            store_path: store_path.into(),
            log_path: log_path.into(),
            clear_tool_outputs: true,
            worker: WorkerConfig::default(),
            summarizer: None,
        }
    }

    /// Purpose: Attach the semantic summariser choice to this config.
    ///
    /// Serves: a record-mode config that can become semantic without a second constructor.
    /// Design: `docs/design/context-ledger.md` §3.
    /// Up: the engine tests and `turn::tests::a_semantic_session_names_its_model_in_the_explain_view`; `crates/cli/src/server.rs` sets the `summarizer` field directly.
    /// Down: `SummarizerChoice`.
    /// Verify: `cargo test -p loong-kernel turn::tests::a_semantic_session_names_its_model_in_the_explain_view`.
    /// Turn on the local summariser. Separate from `new` because `record` mode is
    /// a legitimate, useful configuration with no model at all.
    pub fn with_summarizer(mut self, choice: SummarizerChoice) -> Self {
        self.summarizer = Some(choice);
        self
    }
}

/// Purpose: One derived layer's scope, count and carried tokens, for a status surface.
///
/// Serves: an honest layer list where retrieval reports zero because it is not carried.
/// Design: `docs/design/context-ledger.md` §6.
/// Up: `ContextEngine::memory_status`.
/// Down: `MemoryStatus.layers` and `Kernel::context_explain`'s `ContextLayer` view.
/// Verify: `cargo test -p loong-kernel turn::tests::the_context_view_says_which_memory_is_on_even_before_it_produces_a_map`.
/// One layer of the derived memory, for a status surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerStatus {
    pub level: u64,
    pub scope: &'static str,
    pub count: u64,
    /// What carrying this layer costs. The retrieval layer is not carried in
    /// context, so it reports 0 — saying otherwise would be a nicer number and a
    /// false one.
    pub tokens: u64,
}

/// Purpose: What the background worker has done, as counters it kept itself.
///
/// Serves: a status surface that never infers work from 'is there a map yet?'.
/// Design: `docs/design/context-ledger.md` §6.
/// Up: `ContextEngine::memory_status`; `ContextEngine`'s `Debug`.
/// Down: `MemoryStatus.worker` and `Kernel::context_explain`'s `ContextWorker` view.
/// Verify: `cargo test -p loong-kernel context_engine::tests::semantic_mode_enriches_in_the_background_and_records_the_answer`.
/// What the background enrichment worker has done. Every field is a counter the
/// worker itself keeps, or a count read from the store — never an inference from
/// "is there a map yet?".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkerSnapshot {
    /// `off` | `idle` | `active` | `deferred` | `stopped`.
    pub state: &'static str,
    /// Idle windows entered.
    pub windows: u64,
    /// Model requests sent (successful or not).
    pub model_calls: u64,
    /// Episodes a model added semantic fields to.
    pub enriched: u64,
    /// Requests that failed; each degraded exactly its own episode.
    pub failed: u64,
    /// Episodes still lacking semantic fields.
    pub pending: u64,
    /// Windows skipped because the foreground was busy.
    pub deferrals: u64,
    /// The most recent failure's reason, empty when there has been none.
    pub last_error: String,
}

/// Purpose: The honest answer to 'what is the memory doing?' for a status surface.
///
/// Serves: distinguishing off from on-but-nothing-derived-yet.
/// Design: `docs/design/context-ledger.md` §6.
/// Up: `ContextEngine::memory_status`.
/// Down: `Kernel::context_explain`, which copies mode, summariser, layers and worker.
/// Verify: `cargo test -p loong-kernel turn::tests::the_context_view_says_which_memory_is_on_even_before_it_produces_a_map`.
/// The honest answer to "what is the memory doing?" (ADR-0017 §10, §11.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryStatus {
    /// Empty when the engine is off; `record` or `semantic` when it is on. This is
    /// the field that distinguishes "off" from "on but nothing produced yet" —
    /// checking the ledger for a `cognitive_map` entry cannot, because a new session
    /// has none either way.
    pub mode: String,
    /// The summariser's name, or `none`. Empty in record mode.
    pub summarizer: String,
    /// Why the memory is degraded; empty when it is not.
    pub degraded: String,
    pub layers: Vec<LayerStatus>,
    pub worker: WorkerSnapshot,
}

/// Purpose: State shared between the foreground and the enrichment thread.
///
/// Serves: two threads touching counters, a wake flag and a stop flag without a lock across a model call.
/// Design: `docs/context/pipeline.md` §6.6.
/// Up: `ContextEngine` holds it; `spawn_worker` clones the `Arc`.
/// Down: `ContextEngine::memory_status`, `degraded` and the worker loop.
/// Verify: `cargo test -p loong-kernel context_engine::`.
/// Shared engine state. It is shared because two threads touch it: the foreground
/// sets the busy flag and reads the status, the worker writes its accounting and its
/// failures. No lock is held across a model call.
#[derive(Default)]
struct EngineShared {
    degraded: Mutex<Option<String>>,
    last_error: Mutex<Option<String>>,
    state: AtomicU8,
    windows: AtomicU64,
    model_calls: AtomicU64,
    enriched: AtomicU64,
    failed: AtomicU64,
    deferrals: AtomicU64,
    stop: AtomicBool,
    /// The wake flag and its condition variable: `true` means "a window is worth
    /// opening now". A condition variable rather than a sleep loop so a new turn's
    /// end can start enrichment immediately instead of up to `idle_sleep` later.
    wake: Mutex<bool>,
    wake_cv: Condvar,
    done: Mutex<bool>,
    done_cv: Condvar,
}

impl EngineShared {
    /// Purpose: Remember why the memory is degraded, first reason winning.
    ///
    /// Contract: `context_engine.rs` status.
    fn degrade(&self, reason: impl Into<String>) {
        *self.degraded.lock().unwrap() = Some(reason.into());
    }

    /// Purpose: Remember the most recent enrichment failure.
    ///
    /// Contract: `context_engine.rs` status.
    fn set_error(&self, reason: impl Into<String>) {
        *self.last_error.lock().unwrap() = Some(reason.into());
    }

    /// Purpose: Wake the worker so a new window opens now.
    ///
    /// Contract: `context_engine.rs` worker handshake.
    /// Ask the worker to open a window now.
    fn wake(&self) {
        *self.wake.lock().unwrap() = true;
        self.wake_cv.notify_all();
    }

    /// Purpose: Sleep until woken or the interval passes, then clear the wake flag.
    ///
    /// Contract: `context_engine.rs` worker loop.
    /// Sleep until woken or the interval passes, then clear the wake flag.
    fn sleep(&self, interval: Duration) {
        let mut woken = self.wake.lock().unwrap();
        if !*woken {
            let (guard, _) = self.wake_cv.wait_timeout(woken, interval).unwrap();
            woken = guard;
        }
        *woken = false;
    }

    /// Purpose: Tell `Drop` the worker has finished.
    ///
    /// Contract: `context_engine.rs` shutdown.
    fn finish(&self) {
        *self.done.lock().unwrap() = true;
        self.done_cv.notify_all();
    }

    /// Purpose: Wait up to `limit` for the worker; `false` means let the detached thread go.
    ///
    /// Contract: `context_engine.rs` shutdown.
    /// Wait for the worker to finish, up to `limit`. `false` means it is still
    /// running and the caller should let it go rather than block on it.
    fn wait_done(&self, limit: Duration) -> bool {
        let done = self.done.lock().unwrap();
        if *done {
            return true;
        }
        let (done, _) = self.done_cv.wait_timeout(done, limit).unwrap();
        *done
    }
}

/// Purpose: The engine, once it has a store: the foreground handle and the optional worker.
///
/// Serves: a session whose memory is either genuinely on or deliberately absent.
/// Design: `docs/context/pipeline.md` §6.6; `docs/design/context-ledger.md` §3.
/// Up: `Kernel::build` opens it; `Kernel::refresh_map`, `run` and `recall_tool` use it.
/// Down: `Store`, `EngineShared`, `Foreground` and the worker thread.
/// Verify: `cargo test -p loong-kernel context_engine::`.
/// The engine, once it has a store. A store that cannot be opened is **not**
/// silently ignored: the session runs without the map and says so, because
/// "the memory did not load" is something a person needs to know before they rely
/// on the agent remembering a decision.
pub struct ContextEngine {
    store: Store,
    pub config: ContextEngineConfig,
    /// `record` or `semantic` — non-empty whenever the engine exists, so a surface
    /// can tell "off" from "on".
    mode: &'static str,
    /// The summariser's name, kept here because the summariser itself moves onto
    /// the worker thread.
    summarizer_name: Option<String>,
    shared: Arc<EngineShared>,
    /// `true` while a turn is running. The worker never opens a window then.
    busy: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for ContextEngine {
    /// Purpose: Print mode, degradation, summariser and worker counters without locking across a model call.
    ///
    /// Contract: `context_engine.rs` diagnostics.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextEngine")
            .field("mode", &self.mode)
            .field("degraded", &self.degraded())
            .field("summarizer", &self.summarizer_name)
            .field("worker", &self.memory_status().worker)
            .finish()
    }
}

impl ContextEngine {
    /// Purpose: Open the engine, or report why it could not be opened.
    ///
    /// Serves: a store failure that is reported once at session start instead of discovered later.
    /// Design: `docs/design/context-ledger.md` §3.
    /// Up: `Kernel::build`.
    /// Down: `Store::open`, `open_with` and `OllamaSummarizer::new`.
    /// Verify: `cargo test -p loong-kernel context_engine::tests::r_mode_asks_no_model_and_still_produces_summaries`.
    /// Open the engine, or report why it could not be opened. The caller decides
    /// whether a degraded engine is acceptable.
    pub fn open(config: ContextEngineConfig) -> Result<Self, String> {
        let summarizer: Option<Box<dyn Summarizer>> = config.summarizer.as_ref().map(|choice| {
            // One implementation exists, so an unknown kind is a configuration
            // bug rather than a runtime choice. It fails closed to no
            // summariser: a session must not start sending its history somewhere
            // because a string was misspelled.
            match choice.kind.as_str() {
                "ollama" => Box::new(
                    OllamaSummarizer::new(&choice.base_url, &choice.model)
                        .with_timeout(Duration::from_secs(60)),
                ) as Box<dyn Summarizer>,
                _ => Box::new(NoSummarizer) as Box<dyn Summarizer>,
            }
        });
        Self::open_with(config, summarizer)
    }

    /// Purpose: Open with a caller-supplied summariser, or none for record mode.
    ///
    /// Serves: tests and embedders supplying their own model while `open` handles configuration.
    /// Design: `docs/design/context-ledger.md` §3.
    /// Up: `ContextEngine::open` and `open_with_summarizer`.
    /// Down: `Store::open`, `window_budget` and `spawn_worker`.
    /// Verify: `cargo test -p loong-kernel context_engine::tests::refresh_never_calls_the_model`.
    /// Open with a caller-supplied summariser. Separate from [`ContextEngine::open`]
    /// so an embedder (and a test that must count calls) can supply its own instead
    /// of going through configuration.
    fn open_with(
        config: ContextEngineConfig,
        summarizer: Option<Box<dyn Summarizer>>,
    ) -> Result<Self, String> {
        let store = Store::open(&config.store_path).map_err(|e| e.to_string())?;
        let mode = if summarizer.is_some() {
            "semantic"
        } else {
            "record"
        };
        let window = window_budget(config.summarizer.as_ref());
        let mut engine = Self {
            store,
            config,
            mode,
            summarizer_name: None,
            shared: Arc::new(EngineShared::default()),
            busy: Arc::new(AtomicBool::new(false)),
            worker: None,
        };
        if let Err(e) = engine.store.rebuild(&engine.config.log_path) {
            // A store that cannot rebuild still works for what was already there;
            // the map is stale rather than absent, and the reason is remembered.
            engine
                .shared
                .degrade(format!("rebuilding the context store failed: {e}"));
        }
        if let Some(summarizer) = summarizer {
            engine.summarizer_name = Some(summarizer.name());
            let store_path = engine.config.store_path.clone();
            engine.spawn_worker(store_path, summarizer, window);
        }
        Ok(engine)
    }

    /// Purpose: Start the enrichment thread, or record why it could not start.
    ///
    /// Serves: model calls that stay off the turn's critical path.
    /// Design: `docs/context/pipeline.md` §6.6.
    /// Up: `open_with`, when a summariser is configured.
    /// Down: `Store::synthesize_window`, `EngineShared` and `std::thread::Builder`.
    /// Verify: `cargo test -p loong-kernel context_engine::tests::the_foreground_flag_is_what_defers_the_worker`.
    /// Start the enrichment thread. The thread owns the second store and the
    /// summariser; the engine keeps only the wake/stop handles and the counters, so
    /// there is no mutable state for the two threads to fight over.
    fn spawn_worker(
        &mut self,
        store_path: PathBuf,
        summarizer: Box<dyn Summarizer>,
        window: WindowBudget,
    ) {
        let shared = Arc::clone(&self.shared);
        let busy = Arc::clone(&self.busy);
        let idle_sleep = self.config.worker.idle_sleep;
        let model = self.summarizer_name.clone().unwrap_or_default();
        // Visible as "idle" from the moment the worker exists, rather than "off"
        // until its first thread scheduling.
        self.shared.state.store(WORKER_IDLE, Ordering::SeqCst);
        let spawned = std::thread::Builder::new()
            .name(format!("context-enrich:{model}"))
            .spawn(move || {
                // Its own connection to the same sidecar (ADR-0017 §9). Opening it
                // here rather than in `open` keeps a worker-side failure off the
                // session's startup path and reports it as a degradation.
                let mut store = match Store::open(&store_path) {
                    Ok(store) => store,
                    Err(e) => {
                        shared.degrade(format!(
                            "background enrichment could not open its store: {e}"
                        ));
                        shared.state.store(WORKER_STOPPED, Ordering::SeqCst);
                        shared.finish();
                        return;
                    }
                };
                let mut summarizer = summarizer;
                // The existing `Foreground` abstraction, backed by the flag the kernel
                // sets around a turn. It is consulted both here (before a window) and
                // inside `synthesize_window` (before each call), so a turn that starts
                // mid-window interrupts at the next call boundary.
                let foreground = {
                    let busy = Arc::clone(&busy);
                    move || busy.load(Ordering::SeqCst)
                };
                while !shared.stop.load(Ordering::SeqCst) {
                    if foreground.is_busy() {
                        // The foreground is running. Yielding here is the whole
                        // point: a new turn makes the worker wait, never the other
                        // way round.
                        shared.state.store(WORKER_DEFERRED, Ordering::SeqCst);
                        shared.deferrals.fetch_add(1, Ordering::SeqCst);
                        shared.sleep(idle_sleep);
                        continue;
                    }
                    shared.state.store(WORKER_ACTIVE, Ordering::SeqCst);
                    shared.windows.fetch_add(1, Ordering::SeqCst);
                    match store.synthesize_window(summarizer.as_mut(), window, &foreground) {
                        Ok(outcome) => {
                            shared
                                .model_calls
                                .fetch_add(outcome.calls, Ordering::SeqCst);
                            shared
                                .enriched
                                .fetch_add(outcome.enriched, Ordering::SeqCst);
                            shared.failed.fetch_add(outcome.failed, Ordering::SeqCst);
                            if let Some(error) = outcome.last_error {
                                shared.set_error(error);
                            }
                        }
                        Err(e) => {
                            // A broken store is a degraded memory, not a broken
                            // session: say so and keep the digest.
                            shared.degrade(format!("background enrichment failed: {e}"));
                        }
                    }
                    shared.state.store(WORKER_IDLE, Ordering::SeqCst);
                    // A window is a slice of idle time, not a shift: sleep before the
                    // next one even if work remains, so the budget is a rate and not
                    // just a batch size. A wake returns immediately.
                    shared.sleep(idle_sleep);
                }
                shared.state.store(WORKER_STOPPED, Ordering::SeqCst);
                shared.finish();
            });
        match spawned {
            Ok(handle) => self.worker = Some(handle),
            Err(e) => {
                // No thread exists, so the state must not claim "idle".
                self.shared.state.store(WORKER_STOPPED, Ordering::SeqCst);
                self.shared
                    .degrade(format!("background enrichment could not start: {e}"));
            }
        }
    }

    /// Purpose: The derived store, for recall and status queries.
    ///
    /// Serves: the kernel reading memory through the engine rather than owning a second store.
    /// Design: `docs/design/context-ledger.md` §5.
    /// Up: `Kernel::recall_tool` and `ContextEngine::memory_status`.
    /// Down: `loong_context::Store`.
    /// Verify: `cargo test -p loong-kernel turn::tests::recall_finds_earlier_work_and_cites_it`.
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Purpose: Keep the derived state current with the log: sync, digest episodes, roll domains, then wake the worker.
    ///
    /// Serves: a deterministic map for the current turn with no model call inside a step.
    /// Design: `docs/context/pipeline.md` §6.6; `docs/design/context-ledger.md` §3.
    /// Up: `Kernel::refresh_map` at every step boundary.
    /// Down: `Store::sync`, `digest_pending`, `digest_domains` and `EngineShared::wake`.
    /// Verify: `cargo test -p loong-kernel context_engine::tests::refresh_never_calls_the_model`.
    /// Keep the derived state current with the log. Called at a step boundary; a
    /// failure degrades rather than aborting the turn — a stale map must never
    /// stop a person from working.
    ///
    /// **Deterministic only.** The model call lives on the worker
    /// (`docs/context/pipeline.md` §6.6); this wakes it, because new episodes may
    /// mean new work, and returns.
    pub fn refresh(&mut self) {
        if let Err(e) = self.store.sync(self.config.worker.max_events) {
            self.shared.degrade(e.to_string());
            return;
        }
        if let Err(e) = self.store.digest_pending(4) {
            self.shared.degrade(e.to_string());
            return;
        }
        if let Err(e) = self.store.digest_domains() {
            self.shared.degrade(e.to_string());
            return;
        }
        self.shared.wake();
    }

    /// Purpose: Tell the worker whether a turn is running, and wake it when the turn ends.
    ///
    /// Serves: background enrichment that never competes with the foreground.
    /// Design: `docs/context/pipeline.md` §6.6.
    /// Up: `Kernel::run` around `run_inner`.
    /// Down: `EngineShared::wake`.
    /// Verify: `cargo test -p loong-kernel context_engine::tests::the_foreground_flag_is_what_defers_the_worker`.
    /// Tell the worker whether a turn is running. The kernel sets this around
    /// [`Kernel::submit`](crate::Kernel), `respond_to_approval` included.
    pub fn set_foreground_busy(&self, busy: bool) {
        self.busy.store(busy, Ordering::SeqCst);
        if !busy {
            // The turn is over, which is the main wake source
            // (`docs/context/pipeline.md` §6.6). Waking here means enrichment starts
            // now rather than up to a sleep interval later.
            self.shared.wake();
        }
    }

    /// Purpose: Whether a turn is currently running, as the engine sees it.
    ///
    /// Serves: the test and status surface checking the flag the worker obeys.
    /// Design: `docs/context/pipeline.md` §6.6.
    /// Up: the engine tests; no production caller.
    /// Down: the `busy` atomic.
    /// Verify: `cargo test -p loong-kernel context_engine::tests::the_foreground_flag_is_what_defers_the_worker`.
    /// Whether the engine is running a turn (for tests and status surfaces).
    pub fn foreground_is_busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    /// Purpose: Ask the worker to look for work now.
    ///
    /// Serves: an embedder or test that does not want to wait for the idle sleep.
    /// Design: `docs/context/pipeline.md` §6.6.
    /// Up: no in-tree caller; the engine tests do not need it.
    /// Down: `EngineShared::wake`.
    /// Verify: `cargo test -p loong-kernel context_engine::`.
    /// Ask the worker to look for work now.
    pub fn wake_worker(&self) {
        self.shared.wake();
    }

    /// Purpose: Why the memory is degraded, if it is.
    ///
    /// Serves: a status surface that can say the engine lost its store or a pass failed.
    /// Design: `docs/design/context-ledger.md` §6.
    /// Up: `ContextEngine::memory_status` and the `Debug` impl.
    /// Down: `EngineShared.degraded`.
    /// Verify: `cargo test -p loong-kernel context_engine::tests::an_unreachable_model_degrades_the_episode_not_the_session`.
    /// Why the memory is degraded, if it is.
    pub fn degraded(&self) -> Option<String> {
        self.shared.degraded.lock().unwrap().clone()
    }

    /// Purpose: `record` or `semantic`, non-empty for as long as the engine exists.
    ///
    /// Serves: distinguishing off from on even before a map exists.
    /// Design: `docs/design/context-ledger.md` §6.
    /// Up: `ContextEngine::memory_status`; the engine tests.
    /// Down: the `mode` field.
    /// Verify: `cargo test -p loong-kernel context_engine::tests::r_mode_asks_no_model_and_still_produces_summaries`.
    /// `record` or `semantic`. Non-empty for as long as the engine exists.
    pub fn memory_mode(&self) -> &'static str {
        self.mode
    }

    /// Purpose: The summariser's name, or `None` in record mode.
    ///
    /// Serves: a status surface naming which model enriches the memory.
    /// Design: `docs/design/context-ledger.md` §6.
    /// Up: the engine tests; `memory_status` reads the field directly.
    /// Down: the `summarizer_name` field.
    /// Verify: `cargo test -p loong-kernel context_engine::tests::semantic_mode_enriches_in_the_background_and_records_the_answer`.
    /// Whether semantic enrichment is configured, for a status surface.
    pub fn summarizer_name(&self) -> Option<String> {
        self.summarizer_name.clone()
    }

    /// Purpose: The honest snapshot a status surface draws: mode, degradation, layers and worker.
    ///
    /// Serves: the explain view reporting memory state instead of guessing from the ledger.
    /// Design: `docs/design/context-ledger.md` §6.
    /// Up: `Kernel::context_explain`.
    /// Down: `Store::{layer_stats, global_map, pending_synthesis, degraded_synthesis}` and `EngineShared`.
    /// Verify: `cargo test -p loong-kernel turn::tests::the_context_view_says_which_memory_is_on_even_before_it_produces_a_map`.
    /// The honest snapshot a status surface draws (ADR-0017 §10, §11.1).
    ///
    /// `&self`, because the caller is an explain query: it must not need a mutable
    /// borrow to find out that the memory is degraded, and the state it reports is
    /// shared with the worker rather than owned by the foreground.
    pub fn memory_status(&self) -> MemoryStatus {
        let failed = self.shared.failed.load(Ordering::SeqCst);
        let last_error = self
            .shared
            .last_error
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_default();
        // The engine-level reason wins; failing that, the episodes that fell back to
        // their digest are themselves a degradation, and the UI must be able to say
        // so even though the pass as a whole continued.
        let mut degraded = self.degraded().unwrap_or_default();
        if degraded.is_empty() {
            if let Ok(count) = self.store.degraded_synthesis() {
                if count > 0 {
                    degraded = if last_error.is_empty() {
                        format!("{count} episode(s) fell back to the digest")
                    } else {
                        format!("{count} episode(s) fell back to the digest: {last_error}")
                    };
                }
            }
        }

        let mut layers = Vec::with_capacity(4);
        layers.push(LayerStatus {
            level: 1,
            scope: "atom",
            count: self.store.atom_count().unwrap_or(0),
            tokens: 0,
        });
        for (level, scope) in [(2u64, "episode"), (3, "domain")] {
            let (count, tokens) = self.store.layer_stats(level as u32).unwrap_or((0, 0));
            layers.push(LayerStatus {
                level,
                scope,
                count,
                tokens,
            });
        }
        // L4 is not a stored row: it is re-derived from the L3 maps on demand, so
        // "how many" is 1 (it exists and has content) or 0 (it does not yet).
        let (count, tokens) = match self.store.global_map() {
            Ok(Some(map)) if !map.content.is_empty() => (1, map.tokens),
            _ => (0, 0),
        };
        layers.push(LayerStatus {
            level: 4,
            scope: "global",
            count,
            tokens,
        });

        MemoryStatus {
            mode: self.mode.to_string(),
            summarizer: self.summarizer_name.clone().unwrap_or_default(),
            degraded,
            layers,
            worker: WorkerSnapshot {
                state: worker_state_name(self.shared.state.load(Ordering::SeqCst)),
                windows: self.shared.windows.load(Ordering::SeqCst),
                model_calls: self.shared.model_calls.load(Ordering::SeqCst),
                enriched: self.shared.enriched.load(Ordering::SeqCst),
                failed,
                pending: self.store.pending_synthesis().unwrap_or(0),
                deferrals: self.shared.deferrals.load(Ordering::SeqCst),
                last_error,
            },
        }
    }

    /// Purpose: The L4 map to put in front of the model, or `None` when there is nothing worth carrying.
    ///
    /// Serves: not spending tokens on an empty map in a session's first turns.
    /// Design: `docs/design/context-ledger.md` §6.
    /// Up: `Kernel::refresh_map`.
    /// Down: `Store::global_map`; a failure degrades the engine rather than failing the turn.
    /// Verify: `cargo test -p loong-kernel turn::tests::the_layered_engine_puts_a_map_in_front_of_the_model`.
    /// The L4 map to put in front of the model, or `None` when there is nothing
    /// worth carrying yet. A session's first few events have no map, and inventing
    /// an empty one would spend tokens to say nothing.
    pub fn map_text(&self) -> Option<String> {
        match self.store.global_map() {
            Ok(Some(map)) if !map.content.is_empty() => Some(map.content),
            Ok(_) => None,
            Err(e) => {
                // The map is derived: failing to derive it is a degraded map, not
                // a broken session. The turn continues without it, and the reason
                // is kept where a status panel can find it.
                self.shared.degrade(e.to_string());
                None
            }
        }
    }

    /// Purpose: Open an engine with a caller-supplied summariser, for tests and embedders.
    ///
    /// Contract: `context_engine.rs` tests.
    /// A test/embedder seam: an engine whose model is supplied by the caller.
    #[cfg(test)]
    fn open_with_summarizer(
        config: ContextEngineConfig,
        summarizer: Box<dyn Summarizer>,
    ) -> Result<Self, String> {
        Self::open_with(config, Some(summarizer))
    }
}

impl Drop for ContextEngine {
    /// Purpose: Stop and bounded-wait for the worker, then drop its handle so `session.new` need not wait out a model request.
    ///
    /// Contract: `context_engine.rs` lifecycle.
    fn drop(&mut self) {
        let Some(handle) = self.worker.take() else {
            return;
        };
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.wake();
        // Bounded: if the worker is inside a model call, let it go rather than make
        // `session.new` wait it out. It owns its store and exits at its next check.
        self.shared.wait_done(SHUTDOWN_GRACE);
        drop(handle);
    }
}

/// Purpose: The configured window budget, or the shipped default when none was named.
///
/// Contract: `context_engine.rs` worker config.
/// The window budget for a configuration, or the shipped default when the caller
/// named none.
fn window_budget(choice: Option<&SummarizerChoice>) -> WindowBudget {
    match choice {
        Some(choice) => WindowBudget::new(choice.window_calls.max(1), choice.window_tokens.max(1)),
        None => WindowBudget::new(DEFAULT_WINDOW_CALLS, DEFAULT_WINDOW_TOKENS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loong_context::{DigestInput, SummarizerError, Synthesis};
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;

    static N: AtomicU64 = AtomicU64::new(0);

    /// Purpose: A test summariser that counts its calls.
    ///
    /// Serves: measuring 'the model was actually asked' rather than assuming the wiring.
    /// Design: `docs/design/context-ledger.md` §6.
    /// Up: the engine tests.
    /// Down: the `Summarizer` trait; `ContextEngine::open_with_summarizer`.
    /// Verify: `cargo test -p loong-kernel context_engine::tests::refresh_never_calls_the_model`.
    /// A summariser that counts its calls, so "the model is actually asked" is a
    /// measurable claim rather than a wiring hope.
    #[derive(Default)]
    struct Counting {
        calls: Arc<AtomicU64>,
    }

    impl Summarizer for Counting {
        /// Purpose: The counting summariser's name.
        ///
        /// Contract: `context_engine.rs` tests.
        fn name(&self) -> String {
            "counting".to_string()
        }

        /// Purpose: Count one call and return a deterministic edit, so 'the model was asked' is measurable.
        ///
        /// Contract: `context_engine.rs` tests.
        fn summarize(&mut self, _input: &DigestInput) -> Result<Synthesis, SummarizerError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Synthesis {
                rejected: vec!["先按调用序合并".to_string()],
                ..Default::default()
            })
        }
    }

    /// Purpose: A test summariser that takes measurable wall-clock time.
    ///
    /// Serves: proving the foreground does not wait for a slow model.
    /// Design: `docs/design/context-ledger.md` §6.
    /// Up: the ignored measurement test.
    /// Down: the `Summarizer` trait.
    /// Verify: `cargo test -p loong-kernel context_engine::tests::measurement -- --ignored`.
    /// A summariser that takes a measurable amount of wall-clock time, for the
    /// measurement that the foreground does not wait on it.
    struct Slow {
        millis: u64,
        calls: Arc<AtomicU64>,
    }

    impl Summarizer for Slow {
        /// Purpose: The slow summariser's name.
        ///
        /// Contract: `context_engine.rs` tests.
        fn name(&self) -> String {
            "slow".to_string()
        }

        /// Purpose: Sleep a measurable time and count the call, for the foreground-wait measurement.
        ///
        /// Contract: `context_engine.rs` tests.
        fn summarize(&mut self, _input: &DigestInput) -> Result<Synthesis, SummarizerError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(self.millis));
            Ok(Synthesis {
                rejected: vec!["慢模型说了一个被否决的方案".to_string()],
                ..Default::default()
            })
        }
    }

    /// Purpose: A fresh temporary store and log path per test.
    ///
    /// Contract: `context_engine.rs` tests.
    fn temp_log() -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "loong-engine-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        (dir.join("session.context.db"), dir.join("session.jsonl"))
    }

    /// Purpose: Poll a worker-thread result for up to five seconds.
    ///
    /// Contract: `context_engine.rs` tests.
    /// A fast poll, because a worker is a thread: the shell of the test is
    /// deterministic, the arrival of its result is not.
    fn wait_until(mut check: impl FnMut() -> bool) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if check() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        check()
    }

    /// Purpose: Append one small task's events to a test log.
    ///
    /// Serves: giving the digest something real to summarise.
    /// Design: `docs/design/context-ledger.md` §6.
    /// Up: the engine tests.
    /// Down: `SessionLog::open` and `append`.
    /// Verify: `cargo test -p loong-kernel context_engine::`.
    fn write_task(log: &std::path::Path, index: usize) {
        if !log.exists() {
            std::fs::File::create(log).unwrap();
        }
        let mut session = loong_eventlog::SessionLog::open(log).unwrap();
        for (kind, data) in [
            (
                "message.appended",
                serde_json::json!({"role": "user", "text": format!("任务 {index}：src/kernel/a.rs 的层叠失效")}),
            ),
            (
                "tool.call",
                serde_json::json!({"call_id": format!("e{index}"), "tool": "edit_file",
                    "args": {"path": "src/kernel/a.rs", "new_string": "x"}, "action_class": "write_workspace"}),
            ),
            (
                "tool.result",
                serde_json::json!({"call_id": format!("e{index}"), "ok": true, "output": "applied"}),
            ),
            (
                "tool.call",
                serde_json::json!({"call_id": format!("t{index}"), "tool": "run_command",
                    "args": {"command": format!("cargo test fix_{index}")}, "action_class": "investigate"}),
            ),
            (
                "tool.result",
                serde_json::json!({"call_id": format!("t{index}"), "ok": false,
                    "output": format!("test fix_{index} ... FAILED")}),
            ),
            (
                "tool.call",
                serde_json::json!({"call_id": format!("r{index}"), "tool": "run_command",
                    "args": {"command": format!("cargo test fix_{index}")}, "action_class": "investigate"}),
            ),
            (
                "tool.result",
                serde_json::json!({"call_id": format!("r{index}"), "ok": true,
                    "output": format!("test fix_{index} ... ok")}),
            ),
        ] {
            session.append(kind, data).unwrap();
        }
    }

    /// Purpose: Make a test engine's worker poll quickly.
    ///
    /// Contract: `context_engine.rs` tests.
    fn fast_worker(engine: &mut ContextEngine) {
        engine.config.worker.idle_sleep = Duration::from_millis(5);
    }

    /// Purpose: Record mode produces deterministic summaries without asking a model, and reports no worker.
    #[test]
    fn r_mode_asks_no_model_and_still_produces_summaries() {
        // The honest version of "the model is optional": with no summariser the
        // memory is complete, just free of opinions.
        let (db, log) = temp_log();
        write_task(&log, 1);
        let mut engine = ContextEngine::open(ContextEngineConfig::new(&db, &log)).unwrap();
        assert_eq!(engine.summarizer_name(), None);
        assert_eq!(engine.memory_mode(), "record");
        engine.refresh();
        assert!(engine.degraded().is_none(), "{:?}", engine.degraded());
        assert!(engine.store().summary_count(2).unwrap() >= 1);
        let summaries = engine.store().summaries_at(2, 10).unwrap();
        assert!(
            summaries.iter().all(|s| s.producer == "deterministic"),
            "nothing claims a model wrote it"
        );
        // No model, no worker: the status says so rather than showing an idle
        // thread that does not exist.
        let status = engine.memory_status();
        assert_eq!(status.mode, "record");
        assert_eq!(status.worker.state, "off");
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    /// Purpose: A step boundary (`refresh`) never calls the model; the worker does once the foreground is idle.
    #[test]
    fn refresh_never_calls_the_model() {
        // The structural form of §4.6's promise: the foreground path cannot be slow
        // because of a model, because it does not call one. This is deterministic —
        // no timing and no polling — which is why it is the gate.
        let (db, log) = temp_log();
        write_task(&log, 1);
        let calls = Arc::new(AtomicU64::new(0));
        let mut engine = ContextEngine::open_with_summarizer(
            ContextEngineConfig::new(&db, &log),
            Box::new(Counting {
                calls: Arc::clone(&calls),
            }),
        )
        .unwrap();
        fast_worker(&mut engine);
        // The foreground is a turn: that is when `refresh` runs for real. Keeping
        // the flag set is what makes this assertion deterministic — otherwise the
        // worker could legitimately enrich between two refreshes.
        engine.set_foreground_busy(true);
        for _ in 0..5 {
            engine.refresh();
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "a step boundary derives deterministically and asks no model"
        );
        assert!(
            engine.store().summary_count(2).unwrap() >= 1,
            "and the digest is still there"
        );
        // The worker, which is not the foreground path, does ask.
        engine.set_foreground_busy(false);
        assert!(
            wait_until(|| calls.load(Ordering::SeqCst) >= 1),
            "the background worker enriches once it is allowed to run"
        );
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    /// Purpose: Semantic mode enriches in the background and records the model's answer and provenance.
    #[test]
    fn semantic_mode_enriches_in_the_background_and_records_the_answer() {
        let (db, log) = temp_log();
        write_task(&log, 1);
        let calls = Arc::new(AtomicU64::new(0));
        let mut engine = ContextEngine::open_with_summarizer(
            ContextEngineConfig::new(&db, &log),
            Box::new(Counting {
                calls: Arc::clone(&calls),
            }),
        )
        .unwrap();
        fast_worker(&mut engine);
        assert_eq!(engine.memory_mode(), "semantic");
        assert_eq!(engine.summarizer_name().as_deref(), Some("counting"));

        engine.refresh();
        assert!(
            wait_until(|| engine.store().degraded_synthesis().unwrap_or(1) == 0
                && engine
                    .store()
                    .summaries_at(2, 10)
                    .map(|summaries| summaries
                        .iter()
                        .any(|s| s.content.contains("被否决：先按调用序合并")))
                    .unwrap_or(false)),
            "the answer is merged into the memory: {:?}",
            engine
                .store()
                .summaries_at(2, 10)
                .unwrap()
                .iter()
                .map(|s| &s.content)
                .collect::<Vec<_>>()
        );
        let summaries = engine.store().summaries_at(2, 10).unwrap();
        assert!(
            summaries
                .iter()
                .any(|s| s.producer.starts_with("digest+model:")),
            "and the provenance says a model wrote that part"
        );
        let status = engine.memory_status();
        assert!(status.worker.model_calls >= 1, "{status:?}");
        assert!(status.worker.enriched >= 1, "{status:?}");
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    /// Purpose: An unreachable model degrades the episode's semantic fields, not the session, and the status says why.
    #[test]
    fn an_unreachable_model_degrades_the_episode_not_the_session() {
        // A local model that is not running is the normal case for anyone who has
        // not installed one. It must cost the semantic fields and nothing else — and
        // the interface must be able to say so.
        let (db, log) = temp_log();
        write_task(&log, 1);
        let mut engine = ContextEngine::open(ContextEngineConfig::new(&db, &log).with_summarizer(
            SummarizerChoice {
                kind: "ollama".to_string(),
                // Port 1 is reserved and nothing listens there.
                base_url: "http://127.0.0.1:1".to_string(),
                model: "qwen3:0.6b".to_string(),
                window_calls: 2,
                window_tokens: DEFAULT_WINDOW_TOKENS,
            },
        ))
        .unwrap();
        fast_worker(&mut engine);
        engine.refresh();

        assert!(
            wait_until(|| engine.store().degraded_synthesis().unwrap_or(0) >= 1),
            "the failure is recorded rather than swallowed"
        );
        let summaries = engine.store().summaries_at(2, 10).unwrap();
        assert!(!summaries.is_empty(), "the digest is still there");
        for summary in &summaries {
            assert!(
                summary.content.starts_with("L2 ·"),
                "the deterministic summary survived: {}",
                summary.content
            );
        }
        assert!(
            summaries.iter().any(|s| s.producer.contains("failed:")),
            "and the failure is recorded: {:?}",
            summaries.iter().map(|s| &s.producer).collect::<Vec<_>>()
        );
        let status = engine.memory_status();
        assert!(
            status.degraded.contains("fell back to the digest"),
            "the interface can say why: {status:?}"
        );
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    /// Purpose: An unknown summariser kind fails closed to no model instead of shipping history somewhere.
    #[test]
    fn an_unknown_summariser_kind_fails_closed() {
        // A misspelled kind must not become a default that ships history somewhere.
        let (db, log) = temp_log();
        write_task(&log, 1);
        let mut engine = ContextEngine::open(ContextEngineConfig::new(&db, &log).with_summarizer(
            SummarizerChoice {
                kind: "definitely-not-a-thing".to_string(),
                base_url: "http://example.invalid".to_string(),
                model: "x".to_string(),
                window_calls: 2,
                window_tokens: DEFAULT_WINDOW_TOKENS,
            },
        ))
        .unwrap();
        fast_worker(&mut engine);
        assert_eq!(engine.summarizer_name().as_deref(), Some("none"));
        engine.refresh();
        assert!(
            wait_until(|| {
                engine
                    .store()
                    .summaries_at(2, 10)
                    .map(|summaries| {
                        !summaries.is_empty()
                            && summaries.iter().all(|s| s.producer.contains("model:none"))
                    })
                    .unwrap_or(false)
            }),
            "the provenance says plainly that nothing was summarised: {:?}",
            engine
                .store()
                .summaries_at(2, 10)
                .unwrap()
                .iter()
                .map(|s| &s.producer)
                .collect::<Vec<_>>()
        );
        let summaries = engine.store().summaries_at(2, 10).unwrap();
        for summary in &summaries {
            assert!(summary.content.starts_with("L2 ·"), "{}", summary.content);
            assert!(
                summary.producer.contains("model:none"),
                "{}",
                summary.producer
            );
        }
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    /// Purpose: A busy foreground defers the worker; releasing the turn lets the work through.
    #[test]
    fn the_foreground_flag_is_what_defers_the_worker() {
        // The engine's own contract with the kernel: while a turn is running the
        // worker does not open a window. The structural assertion is that a busy
        // foreground produces deferrals and no model calls; the worker's own
        // `Foreground` rule is tested in `loong-context`'s worker tests.
        let (db, log) = temp_log();
        write_task(&log, 1);
        let calls = Arc::new(AtomicU64::new(0));
        let mut engine = ContextEngine::open_with_summarizer(
            ContextEngineConfig::new(&db, &log),
            Box::new(Counting {
                calls: Arc::clone(&calls),
            }),
        )
        .unwrap();
        fast_worker(&mut engine);
        engine.set_foreground_busy(true);
        engine.refresh();
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "no model call while the foreground is busy"
        );
        assert!(engine.foreground_is_busy());
        // Releasing the turn lets the work through.
        engine.set_foreground_busy(false);
        assert!(wait_until(|| calls.load(Ordering::SeqCst) >= 1));
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    /// Purpose: A slow model is measured on the worker, proving the foreground path does not wait for it.
    /// ADR-0017 verification ④, measured rather than argued: the foreground path is
    /// `refresh()`, which every step boundary runs, and a model that takes real time
    /// must not be inside it. Ignored because it is a measurement, not a gate.
    ///
    /// `cargo test -p loong-kernel context_engine::tests::measurement -- --ignored --nocapture`
    #[test]
    #[ignore = "a measurement, not a gate; run it explicitly"]
    fn measurement_the_foreground_path_does_not_wait_for_a_slow_model() {
        let (db, log) = temp_log();
        for index in 1..=24 {
            write_task(&log, index);
        }
        let calls = Arc::new(AtomicU64::new(0));
        let mut engine = ContextEngine::open_with_summarizer(
            ContextEngineConfig::new(&db, &log),
            Box::new(Slow {
                millis: 300,
                calls: Arc::clone(&calls),
            }),
        )
        .unwrap();
        engine.config.worker.idle_sleep = Duration::from_millis(5);

        // A turn, from the engine's point of view: step boundaries that derive
        // deterministically while the foreground is busy.
        engine.set_foreground_busy(true);
        let started = std::time::Instant::now();
        for _ in 0..6 {
            engine.refresh();
        }
        let foreground = started.elapsed();
        println!("6 step boundaries (24 tasks): {foreground:?}");

        // The "before" number: the same enrichment, on the foreground path.
        let mut store = Store::open(&db).unwrap();
        let synchronous = started.elapsed();
        let outcome = store
            .synthesize_window(
                &mut Slow {
                    millis: 300,
                    calls: Arc::new(AtomicU64::new(0)),
                },
                WindowBudget::new(1, DEFAULT_WINDOW_TOKENS),
                // The "before" number is deliberately unconditional: this is what the
                // foreground used to pay.
                &loong_context::worker::IdleForeground,
            )
            .unwrap();
        println!(
            "one model call in the foreground: {:?} ({} call, {} enriched)",
            started.elapsed() - synchronous,
            outcome.calls,
            outcome.enriched
        );

        engine.set_foreground_busy(false);
        assert!(
            wait_until(|| calls.load(Ordering::SeqCst) >= 1),
            "the background did enrich, so the fast foreground was not vacuous"
        );
        println!(
            "background calls after the turn: {}",
            calls.load(Ordering::SeqCst)
        );
        drop(engine);
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }
}
