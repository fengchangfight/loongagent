//! The turn loop — the fixed kernel (ADR-0004).
//!
//! A turn is: user message → model → tool calls → **PDP verdict per call** →
//! execute (or ask, or deny) → feed results back → repeat, bounded by a step
//! limit. Everything the loop does is an event (ADR-0003), and every block that
//! reaches the model goes through the ledger (ADR-0006).
//!
//! Approvals stop the turn instead of blocking it: `submit` returns a
//! [`PendingCall`], the surface asks the human, and
//! [`Kernel::respond_to_approval`] records the decision and resumes. That keeps
//! the kernel synchronous and the approval itself an auditable event with a
//! scope (ADR-0007 §6).

use crate::ledger::{Category, Ledger, Lifetime};
use loong_config::Profile;
use loong_eventlog::{LogError, SessionLog};
use loong_hooks::McpCaller;
use loong_hooks::{payload, HookEvent, HookReport, HookRunner, HookSet};
use loong_mcp::{McpClient, McpSet, McpTool};
use loong_providers::{ChatMessage, CompletionRequest, Model, ModelError, ToolCallRequest, Usage};
use loong_sandbox::landlock::WorkspacePolicy;
use loong_sandbox::Pdp;
use loong_schema::generated as gen;
use loong_schema::generated::{ActionClass, ApprovalScope, Json, Role, Verdict};
use loong_skills::{parse_invocation, Origin, PromptFetcher, Skill, SkillSet, SkillTool};
use loong_tools::{Confinement, ExecContext, NoConfinement, Registry};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub enum KernelError {
    Log(LogError),
    Model(ModelError),
    /// The caller used the API out of order (e.g. resumed with nothing pending).
    OutOfOrder(String),
    Encode(serde_json::Error),
    /// The derived index could not be built or queried (the log is unaffected).
    Index(loong_index::IndexError),
    /// The hook manifest is malformed. Refusing beats silently dropping a guard.
    Hooks(String),
    /// A required MCP server could not be used.
    Mcp(String),
    /// A skill or command is malformed. A workflow that stopped working must say so.
    Skills(String),
    /// Configuration contradicts itself (a layer tried to widen a cap).
    Config(String),
    /// The session's own record cannot be honoured: a payload that does not match its
    /// kind, an orphan tool call, a missing ancestor, a cycle in the lineage. Saying so
    /// beats continuing with a context that is quietly wrong.
    Session(String),
}

impl std::fmt::Display for KernelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KernelError::Log(e) => write!(f, "log: {e}"),
            KernelError::Model(e) => write!(f, "model: {e}"),
            KernelError::OutOfOrder(m) => write!(f, "out of order: {m}"),
            KernelError::Encode(e) => write!(f, "encode: {e}"),
            KernelError::Index(e) => write!(f, "index: {e}"),
            KernelError::Hooks(m) => write!(f, "hooks: {m}"),
            KernelError::Mcp(m) => write!(f, "mcp: {m}"),
            KernelError::Skills(m) => write!(f, "skills: {m}"),
            KernelError::Config(m) => write!(f, "config: {m}"),
            KernelError::Session(m) => write!(f, "session: {m}"),
        }
    }
}

impl std::error::Error for KernelError {}

impl From<LogError> for KernelError {
    fn from(e: LogError) -> Self {
        KernelError::Log(e)
    }
}

impl From<serde_json::Error> for KernelError {
    fn from(e: serde_json::Error) -> Self {
        KernelError::Encode(e)
    }
}

impl From<loong_index::IndexError> for KernelError {
    fn from(e: loong_index::IndexError) -> Self {
        KernelError::Index(e)
    }
}

/// A tool call waiting for a human decision.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingCall {
    pub request_id: String,
    pub call_id: String,
    pub tool: String,
    pub args: Json,
    pub class: ActionClass,
    /// The rule that produced the `ask` (ADR-0007 §1: always record the rule).
    pub rule: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TurnOutcome {
    pub steps: usize,
    pub final_text: Option<String>,
    pub pending_approval: Option<PendingCall>,
    /// Set when a `user_prompt_submit` hook refused the turn before it started.
    pub refused: Option<String>,
    /// Sequences appended by this turn, in order.
    pub events: Vec<u64>,
    /// The person stopped it (`turn.interrupted` says so in the log).
    pub interrupted: bool,
}

/// How many model calls one turn may take before the loop gives up.
///
/// A **step is a model completion**, and every tool round trip costs one: read, edit, run,
/// fix is four, and a page of HTML that has to actually work is dozens. The limit is there
/// to stop a model that *spins* — the same call over and over — not to decide how large a
/// task may be, so it has to sit well above what honest work needs. It was 8, which is how
/// an agent writing one file came to die mid-edit with "stopped after 8 steps".
///
/// A step is not how cost is bounded: the token budget and compaction are, and those are
/// what a person actually chooses. This is the backstop behind them, and a config layer may
/// still narrow it (`max_steps`, ADR-0008 §2).
pub const DEFAULT_MAX_STEPS: usize = 100;

/// How much room to leave in the model's window for its own answer, by default.
///
/// A request carries the prompt **and** the completion, and both fit in one window. Fill it
/// with prompt and the model has nowhere to write, so compaction has to fire while there is
/// still room. 16 Ki is about what a long answer needs plus a margin; it is a narrow-only
/// key (`reserved_context_size`), and a bigger reserve is the *stricter* setting, so a layer
/// may raise it but never shrink it.
pub const DEFAULT_RESERVED_CONTEXT: u64 = 16_384;

/// A configured step count, with `0` meaning **no limit**.
///
/// Zero is never a sensible cap — it would end the turn before the model was asked anything —
/// so it reads as the absence of one, the way `--budget 0` already means unbounded. That is
/// the convention other agent loops use too: kimi-cli's `loop_control.max_steps_per_turn`
/// documents "unset or `0` means unlimited", and Claude Code's `CLAUDE_CODE_MAX_TURNS`
/// applies only "when no explicit limit is passed" — the limit is a knob a person turns, not
/// a small number they discover by hitting it.
///
/// [`usize::MAX`] is the "no limit" value internally, so narrowing stays a plain `min` and a
/// layer's ceiling still holds.
pub fn steps_or_unlimited(steps: usize) -> usize {
    if steps == 0 {
        usize::MAX
    } else {
        steps
    }
}

#[derive(Debug, Clone)]
pub struct KernelConfig {
    pub cwd: PathBuf,
    pub profile: String,
    pub budget_tokens: Option<u64>,
    /// The newest context left verbatim when compaction runs (ADR-0006 §4).
    pub keep_newest_tokens: u64,
    /// Compact once context passes this fraction of the budget (80%).
    pub compact_at_percent: u64,
    pub max_steps: usize,
    /// Tokens held back for the model's answer: compaction fires while the room left in the
    /// model's window is smaller than this. See [`DEFAULT_RESERVED_CONTEXT`].
    pub reserved_context_size: u64,
    /// The resolved layered settings (ADR-0008) — distinct from `profile`, which is
    /// the profile's *name*. The kernel uses it for the two things it owns:
    /// narrowing the PDP ladder, and which skills exist.
    pub settings: Profile,
    /// The layered context engine (ADR-0017). `None` means the kernel behaves
    /// exactly as it did before that decision — compaction included.
    pub context_engine: Option<crate::context_engine::ContextEngineConfig>,
    /// Skills whose **bodies** are seated in the system prompt, by name.
    ///
    /// An installed plugin declares these (ADR-0018): it is the native form of the
    /// `SessionStart` hook this class of plugin ships on other harnesses — "load this
    /// instruction before the first turn" — carried by the loader that already
    /// exists, instead of by a script nobody reviewed. Empty is the default, and the
    /// rollback path.
    pub bootstrap_skills: Vec<String>,
    /// Adaptation lines seated above those bodies, for a plugin written for a harness
    /// whose tools have different names.
    pub bootstrap_notes: Vec<String>,
}

impl KernelConfig {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            profile: "default".to_string(),
            budget_tokens: None,
            keep_newest_tokens: 20_000,
            compact_at_percent: 80,
            settings: Profile::default(),
            max_steps: DEFAULT_MAX_STEPS,
            reserved_context_size: DEFAULT_RESERVED_CONTEXT,
            context_engine: None,
            bootstrap_skills: Vec::new(),
            bootstrap_notes: Vec::new(),
        }
    }

    /// Turn the layered memory on (ADR-0017). Off is the default, and off is the
    /// rollback path: the old compaction behaviour, byte for byte.
    pub fn with_context_engine(
        mut self,
        engine: crate::context_engine::ContextEngineConfig,
    ) -> Self {
        self.context_engine = Some(engine);
        self
    }
}

/// The Landlock PEP, installed with `pre_exec` so the restriction lands in the
/// forked child and can never leak into the server (or into a sibling tool).
struct LandlockConfinement {
    // Read only inside the `#[cfg(unix)]` half of `confine` below; off Linux there is no
    // policy to apply and the field is carried for the shape of the type.
    #[cfg_attr(not(unix), allow(dead_code))]
    policy: WorkspacePolicy,
}

impl Confinement for LandlockConfinement {
    fn confine(&self, command: &mut Command) {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let policy = self.policy.clone();
            // SAFETY: `apply` is documented to do no allocation on its success
            // path, which is what makes it safe between fork and exec.
            unsafe {
                command.pre_exec(move || {
                    policy.apply().map_err(|e| {
                        std::io::Error::new(std::io::ErrorKind::PermissionDenied, e.to_string())
                    })
                });
            }
        }
        #[cfg(not(unix))]
        let _ = command;
    }
}

/// The bridge from `mcp` hooks to this session's servers. The hook engine stays
/// ignorant of MCP; the kernel, which owns the servers, supplies the capability.
struct KernelMcp {
    clients: Vec<Arc<Mutex<McpClient>>>,
}

impl PromptFetcher for KernelMcp {
    fn fetch(
        &self,
        server: &str,
        prompt: &str,
        argument_names: &[String],
        args: &str,
    ) -> Result<String, String> {
        let client = self
            .clients
            .iter()
            .find(|c| c.lock().map(|c| c.name() == server).unwrap_or(false))
            .ok_or_else(|| format!("no MCP server named {server:?} is running"))?;
        let arguments = loong_mcp::map_prompt_arguments(argument_names, args);
        let mut client = client
            .lock()
            .map_err(|_| format!("the {server} client is poisoned"))?;
        client
            .get_prompt(prompt, &arguments)
            .map_err(|e| e.to_string())
    }
}

impl McpCaller for KernelMcp {
    fn call(&self, server: &str, tool: &str, args: &Json) -> Result<String, String> {
        let client = self
            .clients
            .iter()
            .find(|c| c.lock().map(|c| c.name() == server).unwrap_or(false))
            .ok_or_else(|| format!("no MCP server named {server:?} is running"))?;
        let mut client = client
            .lock()
            .map_err(|_| format!("the {server} client is poisoned"))?;
        client.call_tool(tool, args).map_err(|e| e.to_string())
    }
}

/// Start the trusted MCP servers and register their tools.
///
/// A server that is `required` (the default) and cannot be used is fatal: the
/// operator asked for it, and quietly running without a guard rail they believe is
/// present would be the worst outcome. `required = false` opts into a note.
struct AttachedMcp {
    note: String,
    clients: Vec<Arc<Mutex<McpClient>>>,
    prompt_skills: Vec<Skill>,
}

fn attach_mcp(workspace: &Path, tools: &mut Registry) -> Result<AttachedMcp, KernelError> {
    let set = McpSet::load(workspace).map_err(|e| KernelError::Mcp(e.to_string()))?;
    let mut note = set.summary();
    let mut clients = Vec::new();
    let mut prompt_skills = Vec::new();
    for spec in set.active() {
        let mut client = match McpClient::start(spec) {
            Ok(client) => client,
            Err(e) if spec.required => {
                return Err(KernelError::Mcp(format!("server {:?}: {e}", spec.name)))
            }
            Err(e) => {
                note.push_str(&format!("; {} unavailable: {e}", spec.name));
                continue;
            }
        };
        let listed = match client.list_tools() {
            Ok(listed) => listed,
            Err(e) if spec.required => {
                return Err(KernelError::Mcp(format!("server {:?}: {e}", spec.name)))
            }
            Err(e) => {
                note.push_str(&format!("; {} unavailable: {e}", spec.name));
                continue;
            }
        };
        let count = listed.len();
        let client = Arc::new(Mutex::new(client));
        for info in listed {
            let class = spec.class_for(&info.name);
            tools.register(McpTool::new(&spec.name, info, class, Arc::clone(&client)));
        }
        // A server's prompts become commands: same catalogue, same /name syntax,
        // body fetched from the server on invoke.
        let prompts = match client.lock() {
            Ok(mut locked) => locked.list_prompts(),
            Err(_) => Err(loong_mcp::McpError(format!(
                "the {} client is poisoned",
                spec.name
            ))),
        };
        match prompts {
            Ok(prompts) => {
                if !prompts.is_empty() {
                    note.push_str(&format!(", {} prompt(s)", prompts.len()));
                }
                for prompt in prompts {
                    let names: Vec<String> =
                        prompt.arguments.iter().map(|a| a.name.clone()).collect();
                    let hint = if names.is_empty() {
                        None
                    } else {
                        Some(
                            names
                                .iter()
                                .map(|n| {
                                    if prompt.arguments.iter().any(|a| &a.name == n && a.required) {
                                        format!("<{n}>")
                                    } else {
                                        format!("[{n}]")
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join(" "),
                        )
                    };
                    prompt_skills.push(Skill {
                        // Namespaced like MCP tools, so a prompt can never shadow a
                        // workspace skill.
                        name: format!("mcp__{}__{}", spec.name, prompt.name),
                        description: prompt.description,
                        argument_hint: hint,
                        allowed_tools: None,
                        origin: Origin::Mcp {
                            server: spec.name.clone(),
                            prompt: prompt.name,
                            arguments: names,
                        },
                    });
                }
            }
            Err(e) => {
                note.push_str(&format!("; {} prompts unavailable: {e}", spec.name));
            }
        }
        clients.push(client);
        note.push_str(&format!("; {} exposed {count} tool(s)", spec.name));
    }
    Ok(AttachedMcp {
        note,
        clients,
        prompt_skills,
    })
}

/// The strictest of a caller's budget and the configuration's: `None` means
/// unbounded, which is the widest, so a limit from either side holds.
/// The context budget a session runs with.
///
/// The strictest of three things: what the caller asked for (`--budget`), the session-wide
/// `budget_tokens` key, and whatever cap a **configured model** declares. That last one was
/// parsed, merged, and then read by nothing — the pack this repository ships declares
/// `budget_tokens = 128000` on its only model, and the session it configured ran with no
/// budget at all: compaction never ran, and the context grew until the provider refused it.
/// A model the router may pick is a model the cap has to cover, so the strictest one holds.
pub fn effective_budget(caller: Option<u64>, settings: &Profile) -> Option<u64> {
    let declared = settings
        .models
        .iter()
        .filter_map(|model| model.budget_tokens)
        .min();
    narrow_budget(narrow_budget(caller, settings.budget_tokens), declared)
}

fn narrow_budget(caller: Option<u64>, configured: Option<u64>) -> Option<u64> {
    match (caller, configured) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// A hook verdict replaces the policy's only when it is stricter (hooks tighten,
/// never widen). The rule records which hook objected, so the log says why.
fn tighten(ruling: &loong_sandbox::Ruling, report: &HookReport) -> Option<loong_sandbox::Ruling> {
    if !report.tightening() {
        return None;
    }
    let rank = |v: &Verdict| match v {
        Verdict::Allow => 0,
        Verdict::Ask => 1,
        Verdict::Deny => 2,
    };
    if rank(&report.verdict) <= rank(&ruling.verdict) {
        return None;
    }
    let ids: Vec<&str> = report
        .outcomes
        .iter()
        .filter(|o| rank(&o.verdict) == rank(&report.verdict))
        .map(|o| o.id.as_str())
        .collect();
    Some(loong_sandbox::Ruling {
        verdict: report.verdict,
        rule: format!("hook:{}", ids.join(",")),
        reason: report.reason(),
    })
}

enum Dispatch {
    /// The call finished; every sequence it appended is already in `events`.
    Done,
    Pending(PendingCall),
}

pub struct Kernel {
    log: SessionLog,
    ledger: Ledger,
    tools: Registry,
    pdp: Pdp,
    /// Every model this session may use, by the name the profile gave it.
    models: std::collections::BTreeMap<String, Box<dyn Model>>,
    /// The routing policy from the merged configuration (ADR-0008). Which model to
    /// use is decided per call from facts about the session — and recorded.
    router: loong_config::Router,
    /// The model in use, so a switch is visible in the log.
    selected_model: String,
    config: KernelConfig,
    /// The typed conversation the model sees. Kept in step with the ledger by
    /// construction: every mutation goes through `record_*`.
    messages: Vec<ChatMessage>,
    /// Called for every event the moment it is written (the server's liveness hook).
    event_sink: Option<Box<dyn FnMut(&gen::EventEnvelope)>>,
    /// Approval prompts are off for this session (ADR-0007 §5: YOLO is a visible opt-out,
    /// and opting out is itself an event). The sandbox is a different axis and is
    /// untouched: an out-of-bounds *write* still cannot leave the workspace.
    auto_approve: bool,
    /// Set from outside — a signal handler, in practice — to stop the running turn at the
    /// next step boundary. Cooperative on purpose: a tool that has begun finishes, and the
    /// turn stops between steps rather than in the middle of one.
    interrupted: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Ledger entry per message, in lockstep with `messages` (ADR-0006: the
    /// ledger decides, the wire view follows).
    entry_of: Vec<Option<u64>>,
    pending: Option<PendingCall>,
    approvals: u64,
    /// The PEP's policy for anything a tool spawns (ADR-0007). The kernel owns
    /// the policy; the tool only applies it, in the child.
    confinement: Box<dyn Confinement>,
    /// What confinement is actually in force, for diagnostics and the startup
    /// warning. Never silent: "unconfined" is a fact the operator should see.
    confinement_note: String,
    /// The lifecycle hooks this session runs (empty until the manifest is
    /// trusted). Hooks can tighten a decision, never widen one.
    hooks: HookRunner,
    hooks_note: String,
    /// Live MCP servers, kept for the session's lifetime: dropping the kernel
    /// kills them (the client's `Drop` tears down the process group).
    mcp_clients: Vec<Arc<Mutex<McpClient>>>,
    mcp_note: String,
    /// Skills and commands: descriptions in context, bodies on invoke (T-18).
    skills: Arc<SkillSet>,
    /// Names config switched off, reported once at session start.
    disabled_skills: Vec<String>,
    /// Rendered bodies of the skills an installed plugin seats at session start
    /// (ADR-0018), in catalogue order. Empty unless a plugin asked for one.
    bootstrap: Vec<String>,
    /// Adaptation lines that travel above those bodies.
    bootstrap_notes: Vec<String>,
    /// The bridge to this session's MCP servers, for `mcp` hooks and for prompts
    /// invoked as commands.
    mcp_caller: Option<Arc<KernelMcp>>,
    /// Derived, disposable SQLite view of the log (ADR-0003). It trails the log
    /// by design: the log is the truth, this only makes queries fast. A failure
    /// to index never loses an event — it marks the index stale instead.
    index: loong_index::Index,
    index_path: PathBuf,
    index_stale: Option<String>,
    /// The layered memory (ADR-0017), when the profile asked for it. Its absence
    /// is not a degraded state: it is the documented rollback path.
    context_engine: Option<crate::context_engine::ContextEngine>,
    /// Ledger entry holding the L4 map, so it is refreshed in place rather than
    /// appended again on every step.
    map_entry: Option<u64>,
    /// Index of the L4 map message in `messages`, kept in step with `entry_of`.
    map_index: Option<usize>,
}

impl Kernel {
    pub fn create(
        log_path: impl AsRef<Path>,
        config: KernelConfig,
        model: Box<dyn Model>,
    ) -> Result<Self, KernelError> {
        Self::build(
            SessionLog::create(log_path)?,
            config,
            vec![("default".to_string(), model)],
        )
    }

    pub fn open(
        log_path: impl AsRef<Path>,
        config: KernelConfig,
        model: Box<dyn Model>,
    ) -> Result<Self, KernelError> {
        Self::build(
            SessionLog::open(log_path)?,
            config,
            vec![("default".to_string(), model)],
        )
    }

    /// A session with several models and the router that chooses between them.
    pub fn create_with_models(
        log_path: impl AsRef<Path>,
        config: KernelConfig,
        models: Vec<(String, Box<dyn Model>)>,
    ) -> Result<Self, KernelError> {
        Self::build(SessionLog::create(log_path)?, config, models)
    }

    pub fn open_with_models(
        log_path: impl AsRef<Path>,
        config: KernelConfig,
        models: Vec<(String, Box<dyn Model>)>,
    ) -> Result<Self, KernelError> {
        Self::build(SessionLog::open(log_path)?, config, models)
    }

    fn build(
        log: SessionLog,
        mut config: KernelConfig,
        models: Vec<(String, Box<dyn Model>)>,
    ) -> Result<Self, KernelError> {
        // Open the layered memory before anything else can fail, so a store that
        // cannot be opened is reported once, at session start, rather than
        // discovered later as a missing map.
        let engine = match config.context_engine.take() {
            Some(engine_config) if engine_config.enabled => {
                match crate::context_engine::ContextEngine::open(engine_config) {
                    Ok(engine) => Some(engine),
                    Err(e) => {
                        // Not fatal: the session runs without the map and the
                        // reason is kept for the status surface.
                        eprintln!("context engine unavailable: {e}");
                        None
                    }
                }
            }
            _ => None,
        };
        let router = config.settings.router.clone();
        let mut by_name = std::collections::BTreeMap::new();
        for (name, model) in models {
            by_name.insert(name, model);
        }
        // The router's default, else whatever exists. A rule that names a model this
        // session does not have falls back here instead of failing the turn.
        let selected_model = router
            .default
            .clone()
            .filter(|name| by_name.contains_key(name))
            .or_else(|| by_name.keys().next().cloned())
            .ok_or_else(|| {
                KernelError::Model(ModelError::Transport("no model configured".to_string()))
            })?;
        // Narrow-only keys (ADR-0008 §2): the strictest of what the caller asked for
        // and what the layered configuration says. One place does this arithmetic,
        // so a session flag can never widen a cap that config set.
        config.budget_tokens = effective_budget(config.budget_tokens, &config.settings);
        // Larger is stricter for a reserve — keeping more room for the answer is the
        // conservative choice — so a layer may raise it but never shrink it. `0` and unset
        // both mean "use the default".
        if let Some(size) = config
            .settings
            .reserved_context_size
            .filter(|size| *size > 0)
        {
            config.reserved_context_size = config.reserved_context_size.max(size);
        }
        config.keep_newest_tokens = config
            .keep_newest_tokens
            .min(config.settings.keep_newest_tokens.unwrap_or(u64::MAX));
        config.compact_at_percent = config
            .compact_at_percent
            .min(config.settings.compact_at_percent.unwrap_or(u64::MAX));
        config.max_steps = steps_or_unlimited(config.max_steps).min(
            config
                .settings
                .max_steps
                .map(steps_or_unlimited)
                .unwrap_or(usize::MAX),
        );
        let ledger = Ledger::new(config.budget_tokens);
        let index_path = log.path().with_extension("sqlite");
        // Rebuild rather than attach incrementally: the index is disposable, and
        // a stale index that silently disagrees with the log is worse than a
        // few milliseconds of replay.
        let index = loong_index::Index::rebuild_into(&index_path, log.path())?;
        // External tools join the same registry as the built-ins (T-15): one
        // interface, one PDP, one event stream, one audit trail.
        let mut tools = Registry::with_builtins();
        let attached = attach_mcp(&config.cwd, &mut tools)?;
        let (mcp_note, mcp_clients, mut prompt_skills) =
            (attached.note, attached.clients, attached.prompt_skills);
        // Configuration can disable an entry by identity (ADR-0008 add-only rule:
        // content is advisory, so a project may switch off an org's skill — and it
        // is recorded rather than silently dropped).
        let disabled_config = &config.settings.disable_skills;
        let mut disabled: Vec<String> = Vec::new();
        prompt_skills.retain(|skill| {
            if disabled_config.iter().any(|name| name == &skill.name) {
                disabled.push(skill.name.clone());
                false
            } else {
                true
            }
        });
        let mcp_caller: Option<Arc<KernelMcp>> = (!mcp_clients.is_empty()).then(|| {
            Arc::new(KernelMcp {
                clients: mcp_clients.clone(),
            })
        });
        // Skills are one concept with commands (T-18) — and MCP prompts are skills
        // whose body lives on the server (T-15): the model sees the catalogue and
        // asks for a body by name.
        let skills = Arc::new(
            SkillSet::load_in(&config.cwd, &config.settings.skill_dirs, disabled_config)
                .map_err(|e| KernelError::Skills(e.to_string()))?
                .with_external(prompt_skills)
                .map_err(|e| KernelError::Skills(e.to_string()))?,
        );
        disabled.extend(skills.disabled.iter().cloned());
        disabled.sort();
        disabled.dedup();
        // A plugin can ask for a skill's body to be seated at session start (ADR-0018).
        // The name is resolved against the catalogue that was just built, so what the
        // prompt carries and what `/help` lists cannot disagree.
        let mut bootstrap = Vec::new();
        for name in &config.bootstrap_skills {
            // Config disabling the name is a deliberate "not here", not a broken
            // declaration: the plugin is installed, this session turned it off.
            if disabled_config.iter().any(|off| off == name) {
                continue;
            }
            let skill = skills.get(name).ok_or_else(|| {
                KernelError::Skills(format!(
                    "{name:?} is declared as a plugin bootstrap skill but is not in this \
                     session's catalogue — reinstall the plugin that declares it"
                ))
            })?;
            let body = skill.body().ok_or_else(|| {
                KernelError::Skills(format!(
                    "{name:?} is declared as a plugin bootstrap skill but its body lives on an \
                     MCP server, which cannot be seated before the session starts"
                ))
            })?;
            bootstrap.push(format!(
                "# Skill: {} (from {})\n\n{}",
                skill.name,
                skill.source_label(),
                body
            ));
        }
        match &mcp_caller {
            Some(caller) => tools.register(SkillTool::with_fetcher(
                Arc::clone(&skills),
                Arc::clone(caller) as Arc<dyn PromptFetcher>,
            )),
            None => tools.register(SkillTool::new(Arc::clone(&skills))),
        }
        let bootstrap_notes = config.bootstrap_notes.clone();

        // Configuration narrows the ladder before the first decision (ADR-0008 §2:
        // narrow-only keys, strictest wins — and the rule records which layer won).
        let mut pdp = Pdp::new();
        for class in [
            ActionClass::Investigate,
            ActionClass::WriteWorkspace,
            ActionClass::OutOfBounds,
        ] {
            if let Some((verdict, layer)) = config.settings.permission_with_source(class) {
                // The built-in baseline is not "a profile": name it as what it is.
                let source = match layer {
                    loong_config::Layer::Builtin => "builtin".to_string(),
                    other => format!("profile:{}", other.as_str()),
                };
                pdp.tighten_from(class, verdict, &source)
                    .map_err(|e| KernelError::Config(e.to_string()))?;
            }
        }

        let mut kernel;
        // The PEP is chosen here, once, and handed to every tool call.
        let (confinement, confinement_note): (Box<dyn Confinement>, String) =
            match WorkspacePolicy::new(&config.cwd) {
                Ok(policy) => (
                    Box::new(LandlockConfinement { policy }),
                    "landlock: writes are confined to the workspace".to_string(),
                ),
                Err(e) => (Box::new(NoConfinement), format!("unconfined: {e}")),
            };
        // A malformed manifest stopped a guard from existing: that is fatal, not
        // a warning. Untrusted hooks load but do not run (see `HookSet::active`).
        let hook_set = HookSet::load(&config.cwd).map_err(|e| KernelError::Hooks(e.to_string()))?;
        let hooks_note = hook_set.summary();
        // `mcp` hooks reach the servers this session started; without any, they
        // fail closed and say why.
        let caller: Option<Arc<dyn McpCaller>> = mcp_caller
            .as_ref()
            .map(|caller| Arc::clone(caller) as Arc<dyn McpCaller>);
        let hooks = HookRunner::new(&config.cwd, hook_set.active().to_vec()).with_mcp(caller);
        let session_start_fields = json!({"cwd": config.cwd, "profile": config.profile});
        kernel = Self {
            log,
            ledger,
            tools,
            pdp,
            models: by_name,
            router,
            selected_model,
            config,
            messages: Vec::new(),
            auto_approve: false,
            event_sink: None,
            interrupted: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            entry_of: Vec::new(),
            pending: None,
            approvals: 0,
            confinement,
            confinement_note,
            hooks,
            hooks_note,
            mcp_clients,
            mcp_note,
            skills,
            disabled_skills: disabled,
            bootstrap,
            bootstrap_notes,
            mcp_caller,
            index,
            index_path,
            index_stale: None,
            context_engine: engine,
            map_entry: None,
            map_index: None,
        };
        // The catalogue enters context before any work does.
        kernel.seat_system_prompt()?;
        // Then the conversation the log already holds — its own events, preceded by any
        // ancestor prefix (ADR-0015). Without this, resuming a session hands the model a
        // conversation it cannot see and an instruction to carry on.
        kernel.replay_history()?;
        // The session's first moment (ADR-0007 §7): what a guard sees before any
        // work happens.
        let mut startup = Vec::new();
        kernel.fire_hooks(HookEvent::SessionStart, session_start_fields, &mut startup)?;
        Ok(kernel)
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    pub fn pdp_mut(&mut self) -> &mut Pdp {
        &mut self.pdp
    }

    pub fn tools_mut(&mut self) -> &mut Registry {
        &mut self.tools
    }

    pub fn pending(&self) -> Option<&PendingCall> {
        self.pending.as_ref()
    }

    /// What confinement is in force for spawned processes.
    pub fn confinement_note(&self) -> &str {
        &self.confinement_note
    }

    /// The hook set's status, for the startup log and `hooks list`.
    pub fn hooks_note(&self) -> &str {
        &self.hooks_note
    }

    pub fn hooks(&self) -> &HookRunner {
        &self.hooks
    }

    /// What MCP servers are in play, for the startup log and `mcp list`.
    pub fn mcp_note(&self) -> &str {
        &self.mcp_note
    }

    pub fn mcp_servers(&self) -> usize {
        self.mcp_clients.len()
    }

    /// The skills and commands this workspace offers.
    pub fn skills(&self) -> &SkillSet {
        &self.skills
    }

    /// Fetch an MCP prompt's body for a command invocation, naming its arguments.
    fn fetch_mcp_prompt(&self, name: &str, args: &str) -> Result<(String, u64), String> {
        let skill = self
            .skills
            .get(name)
            .ok_or_else(|| format!("no skill named {name:?}"))?;
        let Origin::Mcp {
            server,
            prompt,
            arguments,
        } = &skill.origin
        else {
            return Err(format!("{name} has no body to fetch"));
        };
        let Some(caller) = &self.mcp_caller else {
            return Err(format!(
                "MCP server {server:?} is not running in this session"
            ));
        };
        let body = caller.fetch(server, prompt, arguments, args)?;
        Ok((body.clone(), loong_skills::estimate_tokens(&body)))
    }

    /// The one place the system prompt is built: a short frame, the description-only
    /// skill catalogue (ADR-0006 progressive disclosure), and the bodies of any skills
    /// an installed plugin asked to have seated here (ADR-0018).
    fn seat_system_prompt(&mut self) -> Result<(), KernelError> {
        // The tool names are listed rather than left to the schema, and the last
        // paragraph is not politeness. Measured on this project's own sessions: when
        // a person asked a local model to write a single-page HTML game, the model
        // pasted the whole file into the reply and told them to save it themselves —
        // 6 assistant messages and 0 tool calls in the log — and answered "I will
        // create typing.html now" while emitting no call at all. Small models follow
        // what the prompt says about *how to work*; a prompt that never mentions
        // tools gets prose instead of actions.
        let tool_names: Vec<String> = self.tools.names().into_iter().map(String::from).collect();
        let mut body = format!(
            "You are loongagent, a coding agent working in a user's workspace. Prefer reading \
             over guessing, keep changes small, and say plainly when you are unsure.\n\n\
             You change the workspace only by calling tools. The tools available are: {}.\n\n\
             Write files with `write_file`, change them with `edit_file`, and run commands with \
             the shell tool. Never paste a file's contents into your reply and tell the person \
             to save it: that is not doing the task. Never write a tool call as text either — \
             no `<tool_call>` blocks, no JSON like {{\"name\": \"write_file\", ...}} in the \
             reply; a call that is printed instead of emitted runs nothing. When someone asks \
             you to create or change a file, emit the tool call, then report what you did in one \
             or two sentences. A claim that you created a file must be true — the person can see \
             the workspace.\n\n\
             Skills and commands available here are listed below as `/name — description`. They \
             are not loaded yet: call the `skill` tool with a name to load its full instructions \
             before doing work that matches it.\n\n{}",
            tool_names.join(", "),
            self.skills.catalogue()
        );
        // The always-on half: a plugin can ask for a skill's body to be here from the
        // first turn rather than behind a description (ADR-0018). It is appended to the
        // same pinned entry, so it is as inspectable — and as un-evictable — as the
        // catalogue it sits under.
        if !self.bootstrap.is_empty() {
            body.push_str(
                "\n\nThe skills below are loaded in full because an installed plugin asks for \
                 them at the start of every session. Their instructions apply from now on; the \
                 skills listed above still load on demand.\n",
            );
            for note in &self.bootstrap_notes {
                body.push_str(&format!("\n- {note}"));
            }
            for block in &self.bootstrap {
                body.push_str("\n\n");
                body.push_str(block);
            }
        }
        // Pinned and Persistent: system prompt tokens are never evicted, and
        // compaction must not touch them (ADR-0006).
        let entry = self.ledger.add(
            Category::SystemPrompt,
            "system",
            &body,
            Lifetime::Persistent,
            true,
        );
        self.messages.insert(0, ChatMessage::System { text: body });
        self.entry_of.insert(0, Some(entry));
        self.append(
            gen::event::SKILLS_LOADED,
            &gen::SkillsLoaded {
                count: self.skills.skills.len() as u64,
                tokens: loong_skills::estimate_tokens(&self.skills.catalogue()),
                names: self.skills.names().join(", "),
                disabled: self.disabled_skills.join(", "),
            },
        )?;
        Ok(())
    }

    /// Fire the hooks registered for `event`, record every answer, and report the
    /// aggregate verdict (deny > ask > allow).
    fn fire_hooks(
        &mut self,
        event: HookEvent,
        fields: Json,
        events: &mut Vec<u64>,
    ) -> Result<HookReport, KernelError> {
        if self.hooks.registered_for(event) == 0 {
            return Ok(HookReport::allow());
        }
        let payload = payload(event, fields);
        let report = self.hooks.fire(event, &payload);
        for outcome in &report.outcomes {
            events.push(self.append(
                gen::event::HOOK_FIRED,
                &gen::HookFired {
                    event: outcome.event.wire().to_string(),
                    hook_id: outcome.id.clone(),
                    kind: outcome.kind.to_string(),
                    verdict: outcome.verdict,
                    reason: outcome.reason.clone(),
                    duration_ms: outcome.duration_ms as i64,
                    failed: outcome.failed,
                },
            )?);
        }
        Ok(report)
    }

    pub fn index_path(&self) -> &Path {
        &self.index_path
    }

    /// `Some(reason)` when an index write failed and the index no longer mirrors
    /// the log. Queries then refuse rather than answer from a stale view.
    pub fn index_stale(&self) -> Option<&str> {
        self.index_stale.as_deref()
    }

    /// Substring search over event payloads (ADR-0003: the convenience path).
    pub fn search(&self, needle: &str) -> Result<Vec<loong_index::EventRow>, KernelError> {
        self.require_fresh_index()?;
        Ok(self.index.search(needle)?)
    }

    /// Children of an event — the event tree inside a session (ADR-0015).
    pub fn event_children(
        &self,
        parent_id: &str,
    ) -> Result<Vec<loong_index::EventRow>, KernelError> {
        self.require_fresh_index()?;
        Ok(self.index.children(parent_id)?)
    }

    /// Rebuild the derived index from the log and clear the stale marker.
    pub fn rebuild_index(&mut self) -> Result<(), KernelError> {
        self.index = loong_index::Index::rebuild_into(&self.index_path, self.log.path())?;
        self.index_stale = None;
        Ok(())
    }

    fn require_fresh_index(&self) -> Result<(), KernelError> {
        match &self.index_stale {
            None => Ok(()),
            Some(reason) => Err(KernelError::OutOfOrder(format!(
                "the derived index is stale ({reason}); call rebuild_index() first"
            ))),
        }
    }

    pub fn last_seq(&self) -> u64 {
        self.log.last_seq()
    }

    pub fn log_path(&self) -> &std::path::Path {
        self.log.path()
    }

    /// Every event appended after `seq` — what the server streams to clients.
    /// Reads the log rather than keeping a mirror, so the stream cannot drift
    /// from the truth. Unknown *ignorable* kinds are relayed too (a newer client
    /// may understand them); an unknown non-ignorable kind fails loudly.
    pub fn events_since(&self, seq: u64) -> Result<Vec<gen::EventEnvelope>, KernelError> {
        Ok(loong_eventlog::replay(self.log.path())?
            .events
            .into_iter()
            .filter(|e| e.seq > seq)
            .collect())
    }

    /// Rebuild the conversation the log already holds, plus any ancestor prefix.
    ///
    /// The log is the record (ADR-0003), so a kernel that starts with empty `messages`
    /// continues the session *and forgets it*: the model is asked to carry on a
    /// conversation it cannot see. This is the fix, and it is also what ADR-0015's
    /// lineage needs — a child's context is its ancestors' prefix stitched to its own
    /// events, and both are replayed by exactly this code.
    fn replay_history(&mut self) -> Result<(), KernelError> {
        let chain = self.lineage()?;
        let depth = chain.len();
        for (index, (path, upto)) in chain.into_iter().enumerate() {
            // History that came from an ancestor says so (ADR-0015): the panel can then
            // answer "where did this come from?" for a session that inherited it.
            let from = (index + 1 < depth).then(|| {
                path.file_stem()
                    .map(|stem| {
                        let stem = stem.to_string_lossy().to_string();
                        if stem == "session" {
                            "default".to_string()
                        } else {
                            stem
                        }
                    })
                    .unwrap_or_else(|| "ancestor".to_string())
            });
            let replay = loong_eventlog::replay_lenient(&path)?;
            // A turn with several tool calls writes **one** assistant message and then
            // each call's events in order (call, verdict, result, call, verdict, result…),
            // so calls and results both accumulate here and are flushed together when the
            // tool phase ends. Flushing at the first result would strand the second call.
            type Pending = (
                Option<String>,
                Vec<loong_providers::ToolCallRequest>,
                Vec<(String, String)>,
            );
            let mut pending: Option<Pending> = None;
            for event in replay.events {
                if event.seq > upto {
                    break;
                }
                match event.kind.as_str() {
                    gen::event::SESSION_FORKED => {}
                    gen::event::MESSAGE_APPENDED => {
                        // The assistant message and its tool calls are recorded as
                        // separate events (the call payloads carry the args), so an
                        // assistant turn is held back until its calls are known — the
                        // alternative is a tool result with no matching call, which is
                        // not a conversation a provider will accept.
                        self.flush_pending(&mut pending)?;
                        let appended: gen::MessageAppended =
                            serde_json::from_value(event.data.clone()).map_err(|e| {
                                KernelError::Session(format!(
                                    "{} at seq {} does not match its kind: {e}",
                                    event.kind, event.seq
                                ))
                            })?;
                        match appended.role {
                            gen::Role::User => {
                                self.messages.push(ChatMessage::User {
                                    text: appended.text.clone(),
                                });
                                let source = match &from {
                                    Some(ancestor) => format!("user from {ancestor}"),
                                    None => "user".to_string(),
                                };
                                let entry = self.ledger.add(
                                    Category::History,
                                    &source,
                                    &appended.text,
                                    Lifetime::Session,
                                    false,
                                );
                                self.entry_of.push(Some(entry));
                            }
                            gen::Role::Assistant => {
                                pending = Some((
                                    (!appended.text.is_empty()).then(|| appended.text.clone()),
                                    Vec::new(),
                                    Vec::new(),
                                ));
                            }
                            // The log records only what the conversation holds: a user
                            // turn and an assistant turn. Anything else here is a
                            // schema change this replay has not been taught.
                            other => {
                                return Err(KernelError::Session(format!(
                                    "message.appended at seq {} has role {other:?}, which is \
                                     not part of a conversation",
                                    event.seq
                                )));
                            }
                        }
                    }
                    gen::event::TOOL_CALL => {
                        let call: gen::ToolCall = serde_json::from_value(event.data.clone())
                            .map_err(|e| {
                                KernelError::Session(format!(
                                    "tool.call at seq {} is malformed: {e}",
                                    event.seq
                                ))
                            })?;
                        // Attach it to the assistant turn it belongs to. A call with no
                        // assistant message in front of it still happened — an older
                        // writer, or a hand-edited log — so it gets the turn it needs
                        // rather than making the session unopenable.
                        let slot = pending.get_or_insert_with(|| (None, Vec::new(), Vec::new()));
                        slot.1.push(loong_providers::ToolCallRequest {
                            call_id: call.call_id.clone(),
                            tool: call.tool.clone(),
                            args: call.args.clone(),
                        });
                    }
                    gen::event::TOOL_RESULT => {
                        let result: gen::ToolResult = serde_json::from_value(event.data.clone())
                            .map_err(|e| {
                                KernelError::Session(format!(
                                    "{} at seq {} does not match its kind: {e}",
                                    event.kind, event.seq
                                ))
                            })?;
                        // Held back until the tool phase ends: the assistant message
                        // comes first in the conversation, and it must carry every call
                        // this turn made — not just the first.
                        if let Some(slot) = pending.as_mut() {
                            slot.2.push((result.call_id.clone(), result.output.clone()));
                        }
                    }
                    gen::event::COMPACTION_APPLIED => {
                        // Compaction clears tool output, so the state it acts on has to be
                        // complete before it is replayed.
                        self.flush_pending(&mut pending)?;
                        let applied: gen::CompactionApplied =
                            serde_json::from_value(event.data.clone()).map_err(|e| {
                                KernelError::Session(format!(
                                    "{} at seq {} does not match its kind: {e}",
                                    event.kind, event.seq
                                ))
                            })?;
                        self.replay_compaction(&applied);
                    }
                    // Provenance, cost, approvals, hooks: records of what happened
                    // around the conversation, not parts of it.
                    _ => {}
                }
            }
            self.flush_pending(&mut pending)?;
        }
        Ok(())
    }

    /// Push the assistant turn that was being held, now that its calls are known.
    fn flush_pending(
        &mut self,
        pending: &mut Option<(
            Option<String>,
            Vec<loong_providers::ToolCallRequest>,
            Vec<(String, String)>,
        )>,
    ) -> Result<(), KernelError> {
        if let Some((text, calls, results)) = pending.take() {
            self.push_assistant(None, text, &calls);
            for (call_id, output) in results {
                self.push_tool_result(None, &call_id, &output);
            }
        }
        Ok(())
    }

    /// The same ledger accounting, message push and index bookkeeping the live turn
    /// does — one implementation, so a replayed session and a live one cannot drift.
    fn push_assistant(
        &mut self,
        from: Option<&str>,
        text: Option<String>,
        calls: &[loong_providers::ToolCallRequest],
    ) {
        let accounting = match (text.as_deref(), calls.first()) {
            (Some(t), None) => t.to_string(),
            (Some(t), Some(_)) => {
                format!("{t}{}", serde_json::to_string(calls).unwrap_or_default())
            }
            (None, Some(_)) => serde_json::to_string(calls).unwrap_or_default(),
            (None, None) => String::new(),
        };
        let source = match from {
            Some(ancestor) => format!("assistant from {ancestor}"),
            None => "assistant".to_string(),
        };
        let entry = self.ledger.add(
            Category::History,
            &source,
            &accounting,
            Lifetime::Session,
            false,
        );
        self.entry_of.push(Some(entry));
        self.messages.push(ChatMessage::Assistant {
            text,
            tool_calls: calls.to_vec(),
        });
    }

    fn push_tool_result(&mut self, from: Option<&str>, call_id: &str, output: &str) {
        let source = match from {
            Some(ancestor) => format!("tool:{call_id} from {ancestor}"),
            None => format!("tool:{call_id}"),
        };
        let entry = self
            .ledger
            .add(Category::History, &source, output, Lifetime::Turn, false);
        self.entry_of.push(Some(entry));
        self.messages.push(ChatMessage::Tool {
            call_id: call_id.to_string(),
            output: output.to_string(),
        });
    }

    /// Reproduce a compaction exactly: the event records how many tool outputs were
    /// cleared and how many messages dropped, so a replay does not have to guess at a
    /// policy that may since have changed.
    fn replay_compaction(&mut self, applied: &gen::CompactionApplied) {
        let marker = |entry: &crate::ledger::Entry| {
            format!(
                "[output of {} cleared to save context — re-run the command if you need it again]",
                entry.source
            )
        };
        let cleared = self.ledger.clear_tool_outputs(applied.kept_tokens, marker);
        for entry in cleared
            .cleared
            .iter()
            .take(applied.cleared_tool_outputs as usize)
        {
            if let Some(index) = self.entry_of.iter().position(|id| *id == Some(entry.id)) {
                let call_id = match &self.messages[index] {
                    ChatMessage::Tool { call_id, .. } => call_id.clone(),
                    _ => continue,
                };
                self.messages[index] = ChatMessage::Tool {
                    call_id,
                    output: format!(
                        "[output of {} cleared to save context — re-run the command if you need it again]",
                        entry.source
                    ),
                };
            }
        }
        for _ in 0..applied.dropped_messages {
            if self.drop_oldest_exchange() == 0 {
                break;
            }
        }
    }

    /// Where this session came from, oldest ancestor first. A child stores the
    /// reference (ADR-0015), so the chain is read from the child's own first event and
    /// each ancestor's own first event after it. A missing ancestor is fatal and named:
    /// without its prefix the session is not merely unexplained, it is inoperable.
    fn lineage(&self) -> Result<Vec<(std::path::PathBuf, u64)>, KernelError> {
        let mut chain: Vec<(std::path::PathBuf, u64)> = Vec::new();
        let mut path = self.log.path().to_path_buf();
        let mut seen: Vec<String> = Vec::new();
        loop {
            let replay = loong_eventlog::replay_lenient(&path)?;
            let first = replay.events.first();
            let Some(first) = first else { break };
            if first.kind != gen::event::SESSION_FORKED {
                break;
            }
            let forked: gen::SessionForked = serde_json::from_value(first.data.clone())
                .map_err(|e| KernelError::Session(format!("session.forked is malformed: {e}")))?;
            if seen.contains(&forked.parent_session) {
                return Err(KernelError::Session(format!(
                    "session lineage is cyclic: {} appears twice",
                    forked.parent_session
                )));
            }
            seen.push(forked.parent_session.clone());
            let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
            // A child lives beside its parent, which is what makes the reference a file
            // name rather than a path that could go stale.
            let parent = if forked.parent_session == "default" {
                dir.join("session.jsonl")
            } else {
                dir.join(format!("{}.jsonl", forked.parent_session))
            };
            if !parent.exists() {
                let mut named: Vec<String> = seen.clone();
                named.reverse();
                return Err(KernelError::Session(format!(
                    "{} is a child of {}, which is not in {}: the chain is {}{} — export the \
                     ancestors with it (ADR-0014)",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    forked.parent_session,
                    dir.display(),
                    named.join(" -> "),
                    format!(
                        " -> {}",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    )
                )));
            }
            chain.push((parent.clone(), forked.parent_seq));
            path = parent;
        }
        // Oldest first: the prefix has to be in place before what came after it.
        chain.reverse();
        chain.push((self.log.path().to_path_buf(), u64::MAX));
        Ok(chain)
    }

    /// What the next request will actually carry, and why — the debug view behind a
    /// "context" panel. It changes nothing: no event, no ledger movement.
    /// The models this session can switch to, in the order the configuration declared
    /// them.
    ///
    /// Declaration order, **not** the map's: that order is the profile's, which is the
    /// user's own ranking from least to most capable, and it is what a status bar
    /// counts when it labels a model `1`, `2`, `3`. Sorting these alphabetically
    /// would silently renumber everybody's keyboard.
    pub fn model_names(&self) -> Vec<String> {
        let declared: Vec<String> = self
            .config
            .settings
            .models
            .iter()
            .map(|spec| spec.name.clone())
            .filter(|name| self.models.contains_key(name))
            .collect();
        if declared.is_empty() {
            // No declaration to go on (a bare `--model`, or a session opened without a
            // profile): fall back to whatever exists, so the list is never empty when
            // models are.
            return self.models.keys().cloned().collect();
        }
        // A model built but not declared (nothing does this today) would otherwise be
        // unreachable; appending keeps it switchable rather than invisible.
        let mut names = declared;
        for name in self.models.keys() {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        names
    }

    /// What the session is running, which is what a switch changes.
    pub fn selected_model(&self) -> &str {
        &self.selected_model
    }

    /// Everything a status bar needs about the models on offer.
    pub fn model_list(&self) -> Vec<gen::ModelOffering> {
        let declared: std::collections::BTreeMap<&str, &loong_config::ModelSpec> = self
            .config
            .settings
            .models
            .iter()
            .map(|spec| (spec.name.as_str(), spec))
            .collect();
        self.model_names()
            .into_iter()
            .map(|name| {
                let spec = declared.get(name.as_str());
                gen::ModelOffering {
                    model: spec
                        .map(|s| s.model.clone())
                        .unwrap_or_else(|| name.clone()),
                    provider: spec.and_then(|s| s.provider.clone()),
                    context_window: spec.and_then(|s| s.max_context_size),
                    name,
                }
            })
            .collect()
    }

    /// Switch the model this session uses, from now on.
    ///
    /// Recorded as a `model.selected` event with the same shape a routing rule writes,
    /// so "why this model?" is answered the same way whether a rule chose it or a
    /// person did. An unknown name is refused rather than stored — a session that
    /// silently kept its old model while reporting a new one would be worse than an
    /// error.
    pub fn select_model(&mut self, name: &str) -> Result<(), KernelError> {
        if !self.models.contains_key(name) {
            return Err(KernelError::OutOfOrder(format!(
                "no model named {name:?} in this session (it has: {})",
                self.model_names().join(", ")
            )));
        }
        if name == self.selected_model {
            return Ok(());
        }
        let facts = loong_config::Facts {
            context_tokens: self.ledger.total_tokens(),
            step: 0,
        };
        self.record_model_selected(name, Some("chosen by the user".to_string()), &facts)?;
        // The router's `default` **is** "the model a turn starts with" — that is what
        // `Profile::default_model` says, and what routing reads on every call. So a
        // person's choice has to become it. Without this the next call would route
        // straight back to whatever the profile declared and the switch would appear to
        // have done nothing, which is the worst possible outcome for a control somebody
        // has just used. Rules still outrank it, so an escalation still fires.
        self.router.default = Some(name.to_string());
        Ok(())
    }

    pub fn context_explain(&self) -> gen::ContextExplain {
        let by_category = self.ledger.tokens_by_category();
        let mut categories: Vec<gen::ContextCategory> = by_category
            .into_iter()
            .map(|(category, tokens)| gen::ContextCategory {
                category: category.to_string(),
                tokens,
            })
            .collect();
        // Largest first: the panel's job is to show where the budget went.
        categories.sort_by(|a, b| b.tokens.cmp(&a.tokens).then(a.category.cmp(&b.category)));
        // The memory's own state, reported rather than inferred. With no engine the
        // mode is empty and the worker state is `off` — the surface must never have
        // to guess "off" from "the ledger happens to hold no cognitive map yet",
        // because a semantic session's first turns look exactly like that.
        let (memory_mode, memory_summarizer, memory_degraded, memory_layers, memory_worker) =
            match self.context_engine.as_ref() {
                None => (
                    String::new(),
                    String::new(),
                    String::new(),
                    Vec::new(),
                    gen::ContextWorker {
                        state: "off".to_string(),
                        windows: 0,
                        model_calls: 0,
                        enriched: 0,
                        failed: 0,
                        pending: 0,
                        deferrals: 0,
                        last_error: String::new(),
                    },
                ),
                Some(engine) => {
                    let status = engine.memory_status();
                    let worker = status.worker;
                    (
                        status.mode,
                        status.summarizer,
                        status.degraded,
                        status
                            .layers
                            .into_iter()
                            .map(|layer| gen::ContextLayer {
                                level: layer.level,
                                scope: layer.scope.to_string(),
                                count: layer.count,
                                tokens: layer.tokens,
                            })
                            .collect(),
                        gen::ContextWorker {
                            state: worker.state.to_string(),
                            windows: worker.windows,
                            model_calls: worker.model_calls,
                            enriched: worker.enriched,
                            failed: worker.failed,
                            pending: worker.pending,
                            deferrals: worker.deferrals,
                            last_error: worker.last_error,
                        },
                    )
                }
            };
        gen::ContextExplain {
            context_tokens: self.ledger.total_tokens(),
            budget_tokens: self.ledger.budget().unwrap_or(0),
            messages: self.messages.len() as u64,
            tools: self.tool_specs().len() as u64,
            // Read from the derived index rather than a counter, so a resumed session
            // reports its whole history.
            compactions: self
                .index
                .kinds()
                .map(|kinds| {
                    kinds
                        .iter()
                        .filter(|k| k.kind == gen::event::COMPACTION_APPLIED)
                        .map(|k| k.count)
                        .sum()
                })
                .unwrap_or(0),
            entries: self
                .ledger
                .entries()
                .iter()
                .map(|entry| gen::ContextEntry {
                    id: entry.id,
                    category: entry.category.as_str().to_string(),
                    source: entry.source.clone(),
                    tokens: entry.tokens,
                    lifetime: match entry.lifetime {
                        Lifetime::Turn => "turn".to_string(),
                        Lifetime::Session => "session".to_string(),
                        Lifetime::Persistent => "persistent".to_string(),
                    },
                    pinned: entry.pinned,
                    cleared: entry.cleared,
                })
                .collect(),
            categories,
            memory_mode,
            memory_summarizer,
            memory_degraded,
            memory_layers,
            memory_worker,
        }
    }

    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    /// Submit a user turn and run the loop until it finishes, asks, or hits the
    /// step limit.
    pub fn submit(&mut self, text: &str) -> Result<TurnOutcome, KernelError> {
        self.submit_with(text, &mut |_| {})
    }

    /// As [`Kernel::submit`], but reporting the model's text **as it is written**.
    ///
    /// Deltas are a preview, never the record: they are not logged, not added to the
    /// ledger, and do not change the outcome. The final `message.appended` is the
    /// answer, which is why a client that misses every delta has missed nothing.
    pub fn submit_with(
        &mut self,
        text: &str,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<TurnOutcome, KernelError> {
        // A stop belongs to the turn it was asked for: a person who interrupts once and
        // then asks something new has not asked for that to stop too. Cleared here rather
        // than in the loop, because the loop also resumes a turn after an approval — which
        // is the same turn, and its stop still stands.
        self.interrupted
            .store(false, std::sync::atomic::Ordering::SeqCst);
        if let Some(p) = &self.pending {
            return Err(KernelError::OutOfOrder(format!(
                "approval {} is still pending",
                p.request_id
            )));
        }
        // `/name args` is an invocation (T-18). A sentence that merely starts with
        // a path (`/etc/hosts is wrong`) is a sentence, and `parse_invocation` is
        // strict about the difference.
        let (prompt, invocation) = match parse_invocation(text) {
            Some((name, args)) => match self.skills.invoke(&name, &args) {
                Some(expansion) => {
                    let source = expansion.source.clone();
                    // An MCP prompt has no local body: fetch it from the server that
                    // owns it, and refuse the turn if that is not possible rather
                    // than sending the model a command name it cannot resolve.
                    let fetched = match expansion.prompt {
                        Some(prompt) => Ok((prompt, expansion.tokens)),
                        None => self.fetch_mcp_prompt(&expansion.name, &args),
                    };
                    match fetched {
                        Ok((prompt, tokens)) => (
                            prompt,
                            Some(gen::SkillInvoked {
                                name: expansion.name,
                                source,
                                args,
                                tokens,
                                error: None,
                            }),
                        ),
                        Err(e) => {
                            let event = self.append(
                                gen::event::SKILL_INVOKED,
                                &gen::SkillInvoked {
                                    name: expansion.name.clone(),
                                    source,
                                    args,
                                    tokens: 0,
                                    error: Some(e.clone()),
                                },
                            )?;
                            return Ok(TurnOutcome {
                                steps: 0,
                                final_text: None,
                                pending_approval: None,
                                refused: Some(format!(
                                    "/{} could not be loaded: {e}",
                                    expansion.name
                                )),
                                events: vec![event],
                                interrupted: false,
                            });
                        }
                    }
                }
                None => {
                    // An unknown command is refused rather than sent to the model as
                    // a prompt: guessing what `/reveiw` meant is not a service.
                    let event = self.append(
                        gen::event::SKILL_INVOKED,
                        &gen::SkillInvoked {
                            name: name.clone(),
                            source: "<none>".to_string(),
                            args,
                            tokens: 0,
                            error: Some(format!(
                                "no skill named {name:?}; /help lists what exists"
                            )),
                        },
                    )?;
                    return Ok(TurnOutcome {
                        steps: 0,
                        final_text: None,
                        pending_approval: None,
                        refused: Some(format!(
                            "unknown command /{name} — /help lists the skills and commands available here"
                        )),
                        events: vec![event],
                        interrupted: false,
                    });
                }
            },
            None => (text.to_string(), None),
        };
        let mut events = Vec::new();
        if let Some(invocation) = invocation {
            events.push(self.append(gen::event::SKILL_INVOKED, &invocation)?);
        }
        events.push(self.record_user(&prompt)?);
        let report = self.fire_hooks(
            HookEvent::UserPromptSubmit,
            json!({"text": text, "profile": self.config.profile}),
            &mut events,
        )?;
        if report.tightening() {
            // There is no approval seam for a prompt, so "ask" has no answerer
            // here and must not be read as consent (ADR-0007: unanswered asks
            // fail closed).
            let why = report.reason();
            return Ok(TurnOutcome {
                steps: 0,
                final_text: None,
                pending_approval: None,
                refused: Some(format!("refused by hook: {why}")),
                events,
                interrupted: false,
            });
        }
        self.run(events, on_delta)
    }

    /// Answer a pending approval; the decision is an event either way.
    pub fn respond_to_approval(
        &mut self,
        request_id: &str,
        granted: bool,
        scope: ApprovalScope,
        reason: Option<&str>,
    ) -> Result<TurnOutcome, KernelError> {
        let Some(pending) = self.pending.take() else {
            return Err(KernelError::OutOfOrder("no approval is pending".into()));
        };
        if pending.request_id != request_id {
            let want = pending.request_id.clone();
            self.pending = Some(pending);
            return Err(KernelError::OutOfOrder(format!(
                "{request_id} is not pending (waiting on {want})"
            )));
        }
        let mut events = vec![self.append(
            gen::event::APPROVAL_DECIDED,
            &gen::ApprovalDecided {
                request_id: request_id.to_string(),
                granted,
                scope,
                reason: reason.map(|r| r.to_string()),
            },
        )?];
        self.fire_hooks(
            HookEvent::PermissionDecided,
            json!({
                "request_id": request_id,
                "tool": pending.tool,
                "granted": granted,
                "scope": scope,
            }),
            &mut events,
        )?;
        if granted {
            let (ok, output) = self.execute(&pending.tool, &pending.args);
            events.push(self.record_tool_result(&pending.call_id, ok, &output)?);
            let tool_name = pending.tool.clone();
            let call_id = pending.call_id.clone();
            // The same seam as the un-gated path: a tool that ran after an
            // approval is still a tool that ran.
            self.fire_hooks(
                HookEvent::PostTool,
                json!({"tool": tool_name, "call_id": call_id, "ok": ok, "output": output}),
                &mut events,
            )?;
        } else {
            let output = match reason {
                Some(r) => format!("denied by the user: {r}"),
                None => "denied by the user".to_string(),
            };
            events.push(self.record_tool_result(&pending.call_id, false, &output)?);
        }
        // Resuming after an approval has no observer: the surface that answered the
        // question already has the flow, and a new one would have to attach first.
        self.run(events, &mut |_| {})
    }

    // ------------------------------------------------------------- the loop

    /// Run one turn while telling the context engine that the foreground is busy.
    ///
    /// The busy flag is what keeps background enrichment off the critical path
    /// (ADR-0017 §4.6): the worker's own thread reads it and never opens a window
    /// while a turn is running. Every path through [`Kernel::run_inner`] — an
    /// answer, a stop, an approval question — clears it, including the error paths,
    /// because a flag left set would starve the background for the session's life.
    fn run(
        &mut self,
        events: Vec<u64>,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<TurnOutcome, KernelError> {
        let engine = self.context_engine.as_ref();
        if let Some(engine) = engine {
            engine.set_foreground_busy(true);
        }
        let result = self.run_inner(events, on_delta);
        if let Some(engine) = self.context_engine.as_ref() {
            engine.set_foreground_busy(false);
        }
        result
    }

    fn run_inner(
        &mut self,
        mut events: Vec<u64>,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<TurnOutcome, KernelError> {
        // Which tools have actually run this turn, and whether we have already objected
        // to a reply that only *described* the change. Both exist for one measured
        // failure: a local model asked to write a file pasted the file into the reply
        // instead, and the turn ended as if it had succeeded. The names (not a bool)
        // matter for the *printed call* variant: a model that read a file and then
        // printed `write_file` must still be corrected, while one that wrote the file and
        // then quoted the call in its summary must not be asked to write it again.
        let mut ran: Vec<String> = Vec::new();
        let mut nudged = false;
        for step in 1..=self.config.max_steps {
            // A stop asked for between steps stops here: after the last tool finished,
            // before the next model call. Nothing half-done is left behind.
            if self.interrupted() {
                events.push(self.append(
                    gen::event::TURN_INTERRUPTED,
                    &gen::TurnInterrupted {
                        reason: "interrupted".to_string(),
                    },
                )?);
                return Ok(TurnOutcome {
                    steps: step - 1,
                    final_text: None,
                    pending_approval: None,
                    refused: None,
                    events,
                    interrupted: true,
                });
            }
            self.refresh_map();
            self.compact_if_needed(&mut events)?;
            let request = CompletionRequest {
                messages: self.messages.clone(),
                tools: self.tool_specs(),
            };
            // Routing happens here, per call, from facts the session can explain —
            // and a change of model is an event, not a hidden optimisation.
            let facts = loong_config::Facts {
                context_tokens: self.ledger.total_tokens(),
                step: step as u64,
            };
            let (chosen, rule) = self.router.select(&facts);
            let chosen = chosen
                .filter(|name| self.models.contains_key(*name))
                .unwrap_or(self.selected_model.as_str())
                .to_string();
            if chosen != self.selected_model {
                events.push(self.record_model_selected(
                    &chosen,
                    rule.map(|r| r.condition.render()),
                    &facts,
                )?);
            }
            let response = self
                .models
                .get_mut(&chosen)
                .expect("the chosen model exists")
                .complete_streaming(&request, on_delta)
                .map_err(KernelError::Model)?;
            events.push(self.record_cost(&response.usage)?);

            if response.tool_calls.is_empty() {
                let text = response.text.unwrap_or_default();
                // A reply that pastes a file, or prints a tool call, and never *calls* a
                // tool is not a finished task. Object once, in as many words, and give the
                // model a turn to act; if it does it again, accept the answer and let the
                // person see what happened rather than looping.
                if !nudged {
                    // Two shapes of the same failure, and they need different words. A
                    // printed call (measured with `qwen2.5:7b-instruct` on Ollama's
                    // OpenAI shim: `{"name":"write_file",…}` inside `content`) named the
                    // tool it wanted, so the correction can name it too; a pasted file
                    // names nothing and the correction has to say `write_file` itself.
                    let printed = leaked_tool_call(&text).filter(|tool| {
                        // Printing a call for a tool that already ran this turn is a
                        // summary of what happened, not an attempt that failed.
                        !ran.iter().any(|done| done == tool)
                    });
                    // A pasted file is only suspect before anything has run: afterwards a
                    // reply may quote code freely, which is the guard this keeps.
                    let pasted =
                        printed.is_none() && ran.is_empty() && looks_like_pasted_file(&text);
                    if printed.is_some() || pasted {
                        nudged = true;
                        let correction = match &printed {
                            Some(tool) => format!(
                                "You wrote the `{tool}` call as text — a `<tool_call>` block or \
                                 JSON in your reply — instead of emitting a real tool call, so \
                                 nothing ran and the workspace is unchanged. Emit `{tool}` now \
                                 as an actual tool call with the arguments you intended. Do not \
                                 print the call and do not wrap it in `<tool_call>`: printing it \
                                 is what made it do nothing."
                            ),
                            None => "You answered with the file's contents in chat and did not \
                                     call a tool, so nothing was written to the workspace. Call \
                                     `write_file` now with the full content in the `content` \
                                     argument, then tell me the path you wrote."
                                .to_string(),
                        };
                        self.messages.push(ChatMessage::User { text: correction });
                        // Not recorded as a message event: this is the harness
                        // course-correcting, not something the person said. The retry's
                        // assistant message is what the log will show, which is the answer
                        // that actually mattered.
                        continue;
                    }
                }
                events.push(self.record_assistant(Some(&text), &[])?);
                return Ok(TurnOutcome {
                    steps: step,
                    final_text: Some(text),
                    pending_approval: None,
                    refused: None,
                    events,
                    interrupted: false,
                });
            }

            for call in &response.tool_calls {
                ran.push(call.tool.clone());
            }
            events.push(self.record_assistant(response.text.as_deref(), &response.tool_calls)?);
            for (at, call) in response.tool_calls.iter().enumerate() {
                match self.dispatch(call, &mut events)? {
                    Dispatch::Done => {}
                    Dispatch::Pending(pending) => {
                        // The rest of this message was never dispatched, and the provider
                        // requires an answer for **every** call it was told about: a request
                        // carrying one assistant message with three calls and one result is
                        // rejected outright, which is how a model that batches its calls — and
                        // asks for permission on the first of them — breaks its own session.
                        // Say they did not run; the model re-issues what it still wants.
                        for skipped in &response.tool_calls[at + 1..] {
                            events.push(self.record_tool_result(
                                &skipped.call_id,
                                false,
                                &format!(
                                    "not run: the turn stopped to ask about {} first",
                                    pending.request_id
                                ),
                            )?);
                        }
                        self.pending = Some(pending.clone());
                        return Ok(TurnOutcome {
                            steps: step,
                            final_text: None,
                            pending_approval: Some(pending),
                            refused: None,
                            events,
                            interrupted: false,
                        });
                    }
                }
            }
        }
        let note = format!(
            "stopped after {} steps without finishing (step limit) — reply to continue, or \
             raise `max_steps` in the configuration",
            self.config.max_steps
        );
        events.push(self.record_assistant(Some(&note), &[])?);
        Ok(TurnOutcome {
            steps: self.config.max_steps,
            final_text: Some(note),
            pending_approval: None,
            refused: None,
            events,
            interrupted: false,
        })
    }

    fn dispatch(
        &mut self,
        call: &ToolCallRequest,
        events: &mut Vec<u64>,
    ) -> Result<Dispatch, KernelError> {
        // `recall` is answered from this session's derived memory rather than from
        // the filesystem, so it is not in the registry — but it is still a tool the
        // model may call. It bypasses the registry *and* the PDP, on purpose: it is
        // read-only over the session's own record, which is data the model has
        // already been given in full, so there is nothing to gate. Doing this before
        // the registry lookup is what makes the tool exist at all; without it the
        // model would be told the tool is unknown.
        if call.tool == RECALL_TOOL {
            events.push(self.append(
                gen::event::TOOL_CALL,
                &gen::ToolCall {
                    call_id: call.call_id.clone(),
                    tool: RECALL_TOOL.to_string(),
                    args: call.args.clone(),
                    action_class: ActionClass::Investigate,
                },
            )?);
            let (ok, output) = self.recall_tool(&call.args);
            events.push(self.record_tool_result(&call.call_id, ok, &output)?);
            return Ok(Dispatch::Done);
        }
        let Some(tool) = self.tools.get(&call.tool) else {
            events.push(self.record_tool_result(
                &call.call_id,
                false,
                &format!("unknown tool {:?}", call.tool),
            )?);
            return Ok(Dispatch::Done);
        };
        let tool_name = tool.name().to_string();
        let class = tool.action_class(&call.args, &self.config.cwd);
        let mut ruling = self.pdp.decide(class, &tool_name);

        // The attempted call, then the hooks' answers, then the verdict, then
        // whatever happened.
        events.push(self.append(
            gen::event::TOOL_CALL,
            &gen::ToolCall {
                call_id: call.call_id.clone(),
                tool: tool_name.clone(),
                args: call.args.clone(),
                action_class: class,
            },
        )?);
        let report = self.fire_hooks(
            HookEvent::PreTool,
            json!({
                "tool": tool_name,
                "call_id": call.call_id,
                "args": call.args,
                "action_class": class,
                "policy_verdict": ruling.verdict,
                "policy_rule": ruling.rule,
            }),
            events,
        )?;
        // Hooks tighten, never widen (ADR-0008): their verdict replaces the
        // policy's only when it is stricter.
        if let Some(tighter) = tighten(&ruling, &report) {
            ruling = tighter;
        }
        events.push(self.append(
            gen::event::POLICY_DECIDED,
            &gen::PolicyDecided {
                action_class: class,
                verdict: ruling.verdict,
                rule: ruling.rule.clone(),
            },
        )?);

        match ruling.verdict {
            Verdict::Allow => {
                let (ok, output) = self.execute(&call.tool, &call.args);
                events.push(self.record_tool_result(&call.call_id, ok, &output)?);
                // Post hooks cannot undo anything; their answers are recorded so
                // the guard's behaviour stays auditable.
                self.fire_hooks(
                    HookEvent::PostTool,
                    json!({"tool": tool_name, "call_id": call.call_id, "ok": ok, "output": output}),
                    events,
                )?;
                Ok(Dispatch::Done)
            }
            // Opted out: the ruling is allowed instead of asking, and the event says the
            // autonomy was relaxed rather than that the policy changed.
            // Opted out: the ruling is allowed instead of asking, and the event says the
            // autonomy was relaxed rather than that the policy changed (ADR-0007 §5).
            Verdict::Ask if self.auto_approve => {
                events.push(self.append(
                    gen::event::POLICY_DECIDED,
                    &json!({
                        "verdict": Verdict::Allow,
                        "rule": format!("yolo:{class:?}"),
                        "tool": tool_name,
                    }),
                )?);
                events.push(self.append(
                    gen::event::TOOL_CALL,
                    &gen::ToolCall {
                        call_id: call.call_id.clone(),
                        tool: tool_name.clone(),
                        args: call.args.clone(),
                        action_class: class,
                    },
                )?);
                let (ok, output) = self.execute(&call.tool, &call.args);
                events.push(self.record_tool_result(&call.call_id, ok, &output)?);
                self.fire_hooks(
                    HookEvent::PostTool,
                    json!({"tool": tool_name, "call_id": call.call_id, "ok": ok, "output": output}),
                    events,
                )?;
                Ok(Dispatch::Done)
            }
            Verdict::Ask => {
                let request_id = format!("appr-{}", self.approvals + 1);
                self.approvals += 1;
                self.fire_hooks(
                    HookEvent::PermissionRequested,
                    json!({
                        "request_id": request_id,
                        "tool": tool_name,
                        "action_class": class,
                        "reason": ruling.reason,
                    }),
                    events,
                )?;
                events.push(self.append(
                    gen::event::APPROVAL_REQUESTED,
                    &gen::ApprovalRequested {
                        request_id: request_id.clone(),
                        action_class: class,
                        reason: ruling.reason.clone(),
                    },
                )?);
                Ok(Dispatch::Pending(PendingCall {
                    request_id,
                    call_id: call.call_id.clone(),
                    tool: tool_name,
                    args: call.args.clone(),
                    class,
                    rule: ruling.rule,
                    reason: ruling.reason,
                }))
            }
            Verdict::Deny => {
                let output = format!("denied by policy ({}): {}", ruling.rule, ruling.reason);
                events.push(self.record_tool_result(&call.call_id, false, &output)?);
                Ok(Dispatch::Done)
            }
        }
    }

    /// Replace the confinement policy applied to every process a tool spawns.
    pub fn set_confinement(&mut self, confinement: Box<dyn Confinement>) {
        self.confinement = confinement;
    }

    /// The PEPs: the tool runner (which refuses to write outside the workspace)
    /// and whatever confinement the kernel was given, applied to each child.
    fn execute(&self, tool: &str, args: &Json) -> (bool, String) {
        // `recall` is not in the registry: it answers from this session's derived
        // memory rather than from the filesystem, so it needs state the registry
        // deliberately has none of. It is still a tool from the model's side, and it
        // is still reported the same way — as a `tool.result` event.
        //
        // It deliberately does **not** add a `Category::Retrieval` ledger entry: the
        // answer becomes a tool-result message, which the ledger already counts. A
        // second entry for the same bytes would double-count them against the
        // context budget, which is the one number a person uses to decide whether
        // the session is affordable.
        if tool == RECALL_TOOL {
            return self.recall_tool(args);
        }
        match self.tools.get(tool) {
            Some(t) => {
                let ctx = ExecContext::new(&self.config.cwd, self.confinement.as_ref());
                match t.execute(args, &ctx) {
                    Ok(out) => (true, out),
                    Err(e) => (false, e.to_string()),
                }
            }
            None => (false, format!("unknown tool {tool:?}")),
        }
    }

    /// Put the current L4 map in front of the model, refreshing it in place.
    ///
    /// Placed as its own message rather than folded into the system prompt: the
    /// system prompt is the operator's text and should not be rewritten on every
    /// step, while the map is expected to change. The ledger entry is created once
    /// and then refreshed with [`Ledger::replace`], so a long session has exactly
    /// one map entry rather than one per step — "why is this in my context?" has one
    /// answer, not a growing pile of them.
    ///
    /// Does nothing when the engine is off, which is the rollback path.
    fn refresh_map(&mut self) {
        let Some(engine) = self.context_engine.as_mut() else {
            return;
        };
        engine.refresh();
        let Some(text) = engine.map_text() else {
            return;
        };
        match (self.map_entry, self.map_index) {
            (Some(entry), Some(index)) if index < self.messages.len() => {
                self.messages[index] = ChatMessage::System { text: text.clone() };
                self.ledger.replace(entry, &text);
            }
            _ => {
                let entry = self.ledger.add(
                    Category::CognitiveMap,
                    "context-engine:L4",
                    &text,
                    Lifetime::Session,
                    // Pinned: the map is the one block whose value is that it is
                    // always there. An evictable map is a map that disappears
                    // exactly when the context is under pressure, which is when it
                    // is worth the most.
                    true,
                );
                // After the system prompt, before the conversation.
                let at = usize::from(matches!(
                    self.messages.first(),
                    Some(ChatMessage::System { .. })
                ));
                self.messages.insert(at, ChatMessage::System { text });
                self.entry_of.insert(at, Some(entry));
                self.map_entry = Some(entry);
                self.map_index = Some(at);
            }
        }
    }

    /// The `recall` tool: search the derived memory, or expand one summary into the
    /// evidence beneath it.
    ///
    /// Read-only by construction — it never materialises, digests or writes; it asks
    /// questions of what is already there. That is why it needs no approval step:
    /// the answer is a *view* of the session's own record, which the model has
    /// already been given the whole of.
    fn recall_tool(&self, args: &Json) -> (bool, String) {
        let Some(engine) = self.context_engine.as_ref() else {
            return (
                false,
                "no memory is available in this session (the context engine is off)".to_string(),
            );
        };
        if let Some(id) = args.get("expand").and_then(|v| v.as_str()) {
            // Expansion stands alone: mixing it with a search would make the result
            // shape depend on which keyword happened to be present.
            return match engine.store().expand(id, 24) {
                Ok(Some(expansion)) => (
                    true,
                    cap_chars(&render_expansion(&expansion), RECALL_MAX_CHARS),
                ),
                Ok(None) => (false, format!("no summary with id {id:?}")),
                Err(e) => (false, format!("expanding {id:?} failed: {e}")),
            };
        }

        let terms: Vec<String> = args
            .get("query")
            .and_then(|v| v.as_str())
            .map(split_terms)
            .unwrap_or_default();
        let entity = args.get("entity").and_then(|v| v.as_str());
        let tool = args.get("tool").and_then(|v| v.as_str());
        let level = args.get("level").and_then(|v| v.as_i64()).map(|l| l as u32);
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(8)
            .clamp(1, 32) as usize;

        // A bare `entity` or `tool` with no words is a legitimate way to ask "what
        // touched src/config?" — it is not an error, so it is not reported as one.
        if terms.is_empty() && entity.is_none() && tool.is_none() {
            return (
                false,
                "give a `query`, an `entity`, a `tool`, or an `expand` id".to_string(),
            );
        }

        let search = loong_context::SearchQuery {
            terms,
            limit,
            ..Default::default()
        };
        // Summaries first: they are the concentrated answer. Raw events are the
        // fallback, and they are what makes the memory complete rather than merely
        // convenient.
        let summaries = match engine.store().recall_at(level, &search) {
            Ok(found) => found,
            Err(e) => return (false, format!("searching summaries failed: {e}")),
        };
        let events = if summaries.is_empty() {
            let mut search = search.clone();
            search.kind = None;
            // Entity and tool filters apply to events; summaries have no tool.
            search.entity = entity.map(|e| e.to_string());
            search.tool = tool.map(|t| t.to_string());
            match engine.store().search(&search) {
                Ok(found) => found,
                Err(e) => return (false, format!("searching events failed: {e}")),
            }
        } else {
            Vec::new()
        };

        if summaries.is_empty() && events.is_empty() {
            return (
                true,
                "nothing in this session's memory matches. That is a real answer, not an \
                 error: do not conclude from it that nothing happened — try different words."
                    .to_string(),
            );
        }
        let rendered = render_recall(&summaries, &events);
        (true, cap_chars(&rendered, RECALL_MAX_CHARS))
    }

    /// ADR-0006 §4: compaction, in a fixed order, and visible.
    ///
    /// Clearing comes first because stale tool output is the bulk of a long
    /// session's context and the least valuable part of it — the model can re-run
    /// a command, but it cannot re-read a decision. Only then are the oldest whole
    /// exchanges dropped, and never across a tool pair: an assistant's tool call
    /// keeps its answer, because a conversation that splits them is invalid to
    /// every provider.
    /// How much input one request may carry, right now.
    ///
    /// Two ceilings, and the stricter wins because either one overflowing ends the same
    /// way. The **budget** is what a person or a pack chose: a cap on the input of **one
    /// request**, which it reaches at `compact_at_percent` of itself. The **window** is what
    /// the model can physically hold, and it is reached when the room left for the answer
    /// drops below the reserve. `None` from both means nothing bounds the context — which is
    /// a thing the CLI says out loud, because it means compaction never runs.
    ///
    /// Neither is a cumulative quota. What is measured is `ledger.total_tokens()`: the size
    /// of the prompt about to be sent, kept history included. Twenty calls in one turn are
    /// twenty measurements of a context, not twenty terms in a sum — a session is free to
    /// spend far more than the ceiling in total, and nothing meters that.
    fn input_ceiling(&self) -> Option<u64> {
        let from_budget = self
            .ledger
            .budget()
            .map(|budget| budget.saturating_mul(self.config.compact_at_percent) / 100);
        let from_window = self.current_window().map(|window| {
            // A reserve larger than the window would hold back everything; half the window
            // is as much as it can sensibly keep for the answer.
            let reserve = self.config.reserved_context_size.min(window / 2);
            window.saturating_sub(reserve)
        });
        match (from_budget, from_window) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The window of the model this session is on. Routing is picked per step and the
    /// selection is recorded before the call, so a switch is picked up on the next step;
    /// the reserve makes one stale step harmless.
    fn current_window(&self) -> Option<u64> {
        self.models
            .get(&self.selected_model)
            .and_then(|model| model.context_window())
    }

    fn compact_if_needed(&mut self, events: &mut Vec<u64>) -> Result<(), KernelError> {
        let Some(ceiling) = self.input_ceiling() else {
            return Ok(());
        };
        let before = self.ledger.total_tokens();
        if before <= ceiling {
            return Ok(());
        }
        let keep = self.config.keep_newest_tokens.min(ceiling / 2);

        // Phase 1: clear stale tool output, oldest first, outside the new window.
        let cleared = self.ledger.clear_tool_outputs(keep, |entry| {
            format!(
                "[output of {} cleared to save context — re-run the command if you need it again]",
                entry.source
            )
        });
        for entry in &cleared.cleared {
            if let Some(index) = self.entry_of.iter().position(|id| *id == Some(entry.id)) {
                let call_id = match &self.messages[index] {
                    ChatMessage::Tool { call_id, .. } => call_id.clone(),
                    _ => continue,
                };
                self.messages[index] = ChatMessage::Tool {
                    call_id,
                    output: format!(
                        "[output of {} cleared to save context — re-run the command if you need it again]",
                        entry.source
                    ),
                };
            }
        }

        // Phase 2: only if still over, drop the oldest whole exchanges — unless the
        // layered engine is on, in which case the exchange must survive: the whole
        // claim of ADR-0017 is that the original stays reachable, and a dropped
        // message is exactly the lossy step it exists to remove. Stale tool output
        // was already cleared above, which is where the bulk of the saving is.
        let mut dropped_messages = 0;
        let may_drop = self.context_engine.is_none();
        while may_drop && self.ledger.total_tokens() > ceiling {
            let removed = self.drop_oldest_exchange();
            if removed == 0 {
                break; // nothing safe left to drop (a single long turn)
            }
            dropped_messages += removed;
        }

        // A compaction that could not free anything (a single long turn, nothing
        // stale) is not an event: `compaction.applied` means the context changed.
        if cleared.cleared.is_empty() && dropped_messages == 0 {
            return Ok(());
        }

        let after = self.ledger.total_tokens();
        events.push(self.append(
            gen::event::COMPACTION_APPLIED,
            &gen::CompactionApplied {
                tokens_before: before,
                tokens_after: after,
                kept_tokens: keep,
                cleared_tool_outputs: cleared.cleared.len() as u64,
                dropped_messages: dropped_messages as u64,
                summary: format!(
                    "cleared {} tool output(s) older than the newest {keep} tokens and dropped                      {dropped_messages} oldest message(s): {before} -> {after} tokens",
                    cleared.cleared.len()
                ),
            },
        )?);
        Ok(())
    }

    /// Drop the oldest exchange (a user message and everything up to the next
    /// user message) so the conversation stays well-formed. Returns how many
    /// messages went.
    fn drop_oldest_exchange(&mut self) -> usize {
        if self.messages.len() < 2 {
            return 0;
        }
        // The system prompt is not an exchange: it stays for the session's life.
        let start = usize::from(matches!(
            self.messages.first(),
            Some(ChatMessage::System { .. })
        ));
        let end = self
            .messages
            .iter()
            .enumerate()
            .skip(start + 1)
            .find(|(_, m)| matches!(m, ChatMessage::User { .. }))
            .map(|(index, _)| index)
            .unwrap_or(0);
        // `end == 0` means the whole conversation is one turn: dropping it would
        // throw away the prompt we are answering.
        if end <= start || end >= self.messages.len() {
            return 0;
        }
        self.messages.drain(start..end);
        let ids: Vec<u64> = self.entry_of.drain(start..end).flatten().collect();
        let _ = self.ledger.forget(&ids);
        end - start
    }

    // --------------------------------------------------- recording helpers

    fn append<T: serde::Serialize>(&mut self, kind: &str, payload: &T) -> Result<u64, KernelError> {
        let data = serde_json::to_value(payload)?;
        let envelope = self.log.append(kind, data)?;
        // The event is already durable; an index failure must not fail the turn,
        // but it must not pass silently either.
        if self.index_stale.is_none() {
            if let Err(e) = self.index.insert(&envelope) {
                self.index_stale = Some(e.to_string());
            }
        }
        // Whoever is watching hears about it **now**, at the moment it became true —
        // not when the turn is over. This is what lets a surface show the work as it
        // happens (ADR-0009), and it is the same guarantee the log gives: nothing is
        // announced that was not written first.
        if let Some(sink) = self.event_sink.as_mut() {
            sink(&envelope);
        }
        Ok(envelope.seq)
    }

    /// Turn approval prompts off or on. Recorded, because a session that never asks is a
    /// session whose log has to say why (ADR-0007 §5).
    pub fn set_auto_approve(&mut self, on: bool) -> Result<(), KernelError> {
        if self.auto_approve == on {
            return Ok(());
        }
        self.auto_approve = on;
        self.append(
            gen::event::PERMISSIONS_RELAXED,
            &gen::PermissionsRelaxed {
                auto_approve: on,
                reason: "the person asked for it with /yolo".to_string(),
            },
        )?;
        Ok(())
    }

    pub fn auto_approve(&self) -> bool {
        self.auto_approve
    }

    /// The flag a signal handler sets to stop the running turn. Handing it out is how the
    /// server lets a person interrupt without a protocol message: the process is ours, and
    /// Ctrl-C already means "stop what you are doing".
    pub fn interruptor(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        std::sync::Arc::clone(&self.interrupted)
    }

    /// Use a flag someone else made. The server needs this: a signal handler can only
    /// reach a value that lives for the process, so the process owns it and the kernel
    /// borrows it.
    pub fn use_interruptor(&mut self, flag: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        self.interrupted = flag;
    }

    fn interrupted(&self) -> bool {
        self.interrupted.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Watch events as they are recorded. Scoped by the caller (the server installs one
    /// for the duration of a turn and clears it afterwards), because a sink that
    /// outlives its transport would write into nothing.
    pub fn set_event_sink(&mut self, sink: Option<Box<dyn FnMut(&gen::EventEnvelope)>>) {
        self.event_sink = sink;
    }

    fn record_user(&mut self, text: &str) -> Result<u64, KernelError> {
        let seq = self.append(
            gen::event::MESSAGE_APPENDED,
            &gen::MessageAppended {
                role: Role::User,
                text: text.to_string(),
            },
        )?;
        self.messages.push(ChatMessage::User {
            text: text.to_string(),
        });
        let entry = self
            .ledger
            .add(Category::History, "user", text, Lifetime::Session, false);
        self.entry_of.push(Some(entry));
        Ok(seq)
    }

    fn record_assistant(
        &mut self,
        text: Option<&str>,
        calls: &[ToolCallRequest],
    ) -> Result<u64, KernelError> {
        let seq = self.append(
            gen::event::MESSAGE_APPENDED,
            &gen::MessageAppended {
                role: Role::Assistant,
                text: text.unwrap_or_default().to_string(),
            },
        )?;
        // An assistant turn occupies context even when it carries only tool
        // calls: the call payloads cost tokens too (ADR-0006 accounting).
        let accounting = match (text, calls.first()) {
            (Some(t), None) => t.to_string(),
            (Some(t), Some(_)) => {
                format!("{t}{}", serde_json::to_string(calls).unwrap_or_default())
            }
            (None, Some(_)) => serde_json::to_string(calls).unwrap_or_default(),
            (None, None) => String::new(),
        };
        let entry = self.ledger.add(
            Category::History,
            "assistant",
            &accounting,
            Lifetime::Session,
            false,
        );
        self.entry_of.push(Some(entry));
        self.messages.push(ChatMessage::Assistant {
            text: text.map(|t| t.to_string()),
            tool_calls: calls.to_vec(),
        });
        Ok(seq)
    }

    fn record_tool_result(
        &mut self,
        call_id: &str,
        ok: bool,
        output: &str,
    ) -> Result<u64, KernelError> {
        let seq = self.append(
            gen::event::TOOL_RESULT,
            &gen::ToolResult {
                call_id: call_id.to_string(),
                ok,
                output: output.to_string(),
            },
        )?;
        // Tool output is the first thing compaction clears, so it carries a
        // turn-scoped lifetime (ADR-0006 §4).
        let entry = self.ledger.add(
            Category::History,
            format!("tool:{call_id}"),
            output,
            Lifetime::Turn,
            false,
        );
        self.entry_of.push(Some(entry));
        self.messages.push(ChatMessage::Tool {
            call_id: call_id.to_string(),
            output: output.to_string(),
        });
        Ok(seq)
    }

    /// Record that a session is running on a control-plane pack (ADR-0016 §7). This
    /// is provenance, not configuration: it answers "what policy was in force?" for a
    /// session that has already happened.
    pub fn record_control_synced(
        &mut self,
        provenance: &loong_config::ControlProvenance,
        from_cache: bool,
        note: Option<&str>,
    ) -> Result<u64, KernelError> {
        Ok(self
            .log
            .append(
                gen::event::CONTROL_SYNCED,
                json!({
                    "issuer": provenance.issuer,
                    "version": provenance.version,
                    "key_id": provenance.key_id,
                    "profile": provenance.profile,
                    "from_cache": from_cache,
                    "note": note.unwrap_or(""),
                }),
            )?
            .seq)
    }

    /// Record the named profile a session runs, with what the user was told about it.
    pub fn record_profile_selected(
        &mut self,
        profile: &loong_config::NamedProfile,
    ) -> Result<u64, KernelError> {
        let models = profile
            .models
            .iter()
            .map(|m| m.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        Ok(self
            .log
            .append(
                gen::event::PROFILE_SELECTED,
                json!({
                    "profile": profile.name,
                    "description": profile.description,
                    "models": models,
                }),
            )?
            .seq)
    }

    fn record_model_selected(
        &mut self,
        model: &str,
        rule: Option<String>,
        facts: &loong_config::Facts,
    ) -> Result<u64, KernelError> {
        let previous = std::mem::replace(&mut self.selected_model, model.to_string());
        // `self.append`, not `self.log.append`: a switch is news, and news arrives when
        // it happens. Writing straight to the log skipped both the live sink and the
        // derived index, so a routing rule could change the model mid-turn and nothing
        // watching would hear about it until the turn was over — which made every
        // switch invisible in exactly the moment it mattered.
        self.append(
            gen::event::MODEL_SELECTED,
            &json!({
                "model": model,
                "previous": previous,
                "rule": rule.unwrap_or_else(|| "the router's default".to_string()),
                "context_tokens": facts.context_tokens,
                "step": facts.step,
            }),
        )
    }

    fn record_cost(&mut self, usage: &Usage) -> Result<u64, KernelError> {
        // usd_micros stays 0 until pricing/profiles land; tokens are real.
        self.append(
            gen::event::COST_RECORDED,
            &gen::CostRecorded {
                profile: self.config.profile.clone(),
                tokens_in: usage.tokens_in,
                tokens_out: usage.tokens_out,
                usd_micros: 0,
            },
        )
    }

    fn tool_specs(&self) -> Vec<loong_providers::ToolSpec> {
        let mut specs: Vec<loong_providers::ToolSpec> = self
            .tools
            .specs()
            .into_iter()
            .map(
                |(name, description, parameters)| loong_providers::ToolSpec {
                    name,
                    description,
                    parameters,
                },
            )
            .collect();
        // `recall` is offered only when there is a memory behind it. A tool the
        // model can call but that cannot answer is worse than an absent one: it
        // teaches the model that asking about the past is useless.
        if self.context_engine.is_some() {
            specs.push(loong_providers::ToolSpec {
                name: RECALL_TOOL.to_string(),
                description: RECALL_DESCRIPTION.to_string(),
                parameters: recall_parameters(),
            });
        }
        specs
    }
}

/// The tool's name, in one place: it is used by `tool_specs`, by `dispatch` and by
/// the executor, and three spellings of a tool name is how a tool stops working.
const RECALL_TOOL: &str = "recall";

/// Whether a reply is a **tool call printed as text** rather than emitted.
///
/// Measured in this project's own log (`47x75pvh.jsonl`, `qwen2.5:7b-instruct` on
/// Ollama's OpenAI shim): asked to summarise a report into a file, the model answered
/// with `{"name": "write_file", "arguments": {…}}` wrapped in `<tool_call>` **inside
/// `content`**. The provider passes that through as ordinary text, so no `tool.call`
/// event exists, the turn ends "successfully", and the workspace is unchanged — the
/// same class of failure as pasting the file, with a different serialization.
///
/// It returns the tool's name so the correction can name it too (telling a model that
/// tried to `edit_file` to call `write_file` would be a second wrong instruction). The
/// match is deliberately narrow — a quoted `"name"` next to `"arguments"`, or an
/// explicit `<tool_call>` marker — so prose that merely *mentions* a tool is not
/// mistaken for an attempted call.
fn leaked_tool_call(text: &str) -> Option<String> {
    const KNOWN: &[&str] = &[
        "write_file",
        "edit_file",
        "read_file",
        "run_command",
        "glob",
        "grep",
        "recall",
        "skill",
    ];
    // `"name": "write_file"` or `"name":"write_file"`, wherever it sits. The text may be
    // cut off on either side (the measured case began mid-word), so this scans rather
    // than parsing the object.
    let quoted_name = || -> Option<String> {
        let at = text.find("\"name\"")?;
        let rest = text[at + "\"name\"".len()..].trim_start();
        let rest = rest.strip_prefix(':')?.trim_start();
        let rest = rest.strip_prefix('"')?;
        Some(rest.split('"').next()?.to_string())
    };
    let has_arguments = text.contains("\"arguments\"");
    if let Some(name) = quoted_name() {
        if has_arguments && KNOWN.contains(&name.as_str()) {
            return Some(name);
        }
    }
    let tagged = text.contains("<tool_call>") || text.contains("</tool_call>");
    if tagged {
        // A marker with no parseable name still says what happened; name the tool if the
        // text names one, so the correction is not generic for no reason.
        if let Some(name) = KNOWN.iter().find(|tool| text.contains(**tool)) {
            return Some((*name).to_string());
        }
    }
    None
}

/// Whether a reply is a file pasted into chat rather than a report.
///
/// Deliberately narrow: a fenced block whose opening line names a file, or a shell
/// paste that names a tool. Merely quoting code is normal — explaining a fix with a
/// snippet is not the failure this looks for. The failure is a *whole file* offered
/// for the person to save, which leaves the workspace unchanged while the reply
/// reads like success.
fn looks_like_pasted_file(text: &str) -> bool {
    let mut in_fence = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            if in_fence {
                in_fence = false;
                continue;
            }
            in_fence = true;
            let info = trimmed.trim_start_matches("```").trim().to_lowercase();
            // A fence labelled with a file, a language, or a tool name. An unlabelled
            // fence is a snippet and is left alone.
            if info.is_empty() {
                continue;
            }
            if info.contains('.')
                || [
                    "html",
                    "css",
                    "javascript",
                    "js",
                    "python",
                    "sh",
                    "bash",
                    "yaml",
                    "json",
                    "toml",
                    "sql",
                    "rust",
                    "go",
                    "java",
                    "ts",
                    "typescript",
                ]
                .iter()
                .any(|lang| info.starts_with(lang))
            {
                return true;
            }
            if info.starts_with("write_file") || info.starts_with("edit_file") {
                return true;
            }
        }
        // `write_file path - "..."`: a tool call written out as prose.
        if trimmed.starts_with("write_file ") || trimmed.starts_with("edit_file ") {
            return true;
        }
    }
    false
}

/// The description the model reads. Written as an instruction because that is what
/// it is: the design's discipline ("recall before answering about the past, cite
/// evidence") only works if the tool tells the model how it is meant to be used.
const RECALL_DESCRIPTION: &str = "Search this session's memory for earlier work. Returns summaries with citations ([seq]) into the raw record. Pass \"expand\" with a summary id to get the summary together with the original events it rests on — do that before asserting that something was verified or fixed. Summaries are a derived index, not the record: when they disagree with the evidence, the evidence is right.";

/// The tool's schema. Every field is optional except that `expand` stands alone.
fn recall_parameters() -> Json {
    json!({
        "type": "object",
        "properties": {
            "query": {
                "type": "string",
                "description": "Words to look for. All of them must appear (AND). Chinese words of two characters work."
            },
            "entity": {
                "type": "string",
                "description": "Restrict to a file, module, tool, command, test or error, e.g. \"src/config\" or \"module:src/kernel\"."
            },
            "tool": {"type": "string", "description": "Restrict to one tool, e.g. \"edit_file\"."},
            "level": {
                "type": "integer",
                "description": "2 = task summary, 3 = module map, 4 = global map. Omit for all."
            },
            "expand": {
                "type": "string",
                "description": "A summary id from a previous recall; returns it with the original events it cites."
            },
            "limit": {"type": "integer", "description": "Maximum results (default 8, capped at 32)."}
        }
    })
}

/// How many characters of retrieved memory one `recall` may put in front of the
/// model. The whole point of the thin foreground is that retrieval competes for
/// budget like everything else, so this is a ceiling rather than a hope.
const RECALL_MAX_CHARS: usize = 4_800;

/// Split a query into terms. Whitespace and the punctuation that would otherwise
/// glue words together; the two-character Chinese word is preserved, which is the
/// case a trigram index gets silently wrong.
fn split_terms(query: &str) -> Vec<String> {
    query
        .split(|c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    ',' | ';'
                        | '，'
                        | '；'
                        | '、'
                        | '。'
                        | '：'
                        | '（'
                        | '）'
                        | '('
                        | ')'
                        | '"'
                        | '\''
                        | '“'
                        | '”'
                        | '|'
                        | '/'
                )
        })
        .map(|term| term.trim())
        .filter(|term| !term.is_empty())
        .map(|term| term.to_string())
        .collect()
}

/// Truncate and say so. A silently shortened answer would read as "that is all
/// there was", which is the one thing a retrieval result must never imply.
fn cap_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}\n…[truncated — narrow the query for the rest]")
}

/// Render summaries as an index the model can jump from. The citation is part of
/// every line on purpose: a summary the model cannot trace is a claim, not memory.
fn render_recall(summaries: &[loong_context::Summary], events: &[loong_context::Hit]) -> String {
    let mut out = String::new();
    if !summaries.is_empty() {
        out.push_str("memory (summaries; expand one before relying on it):\n");
        for summary in summaries {
            let ranges: Vec<String> = summary
                .citations
                .iter()
                .map(|(from, to)| {
                    if from == to {
                        format!("{from}")
                    } else {
                        format!("{from}-{to}")
                    }
                })
                .collect();
            let preview: String = summary
                .content
                .lines()
                .take(6)
                .collect::<Vec<_>>()
                .join(" | ");
            out.push_str(&format!(
                "- [{}] L{} {} ({}) seq[{}]\n  {}\n",
                summary.id,
                summary.level,
                summary.title,
                summary.status,
                ranges.join(","),
                preview
            ));
        }
    }
    if !events.is_empty() {
        out.push_str("memory (raw events; these are the record):\n");
        for hit in events {
            let who = hit
                .tool
                .clone()
                .or_else(|| hit.role.clone())
                .unwrap_or_else(|| hit.kind.clone());
            out.push_str(&format!(
                "- [{}] {} {}: {}\n",
                hit.seq,
                hit.kind,
                who,
                hit.preview.replace('\n', " ")
            ));
        }
    }
    out
}

/// Render a summary *with* the evidence it cites. The evidence is not a footnote:
/// returning it in the same response is the mechanism that stops a derived index
/// from being read as the record.
fn render_expansion(expansion: &loong_context::Expansion) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "summary [{}] L{} {} ({})\n{}\n",
        expansion.summary.id,
        expansion.summary.level,
        expansion.summary.title,
        expansion.summary.status,
        expansion.summary.content
    ));
    if expansion.evidence.is_empty() {
        out.push_str("(this summary cites no events)\n");
        return out;
    }
    out.push_str("evidence (the original events this summary rests on):\n");
    for atom in &expansion.evidence {
        let who = if !atom.tool.is_empty() {
            atom.tool.clone()
        } else if !atom.role.is_empty() {
            atom.role.clone()
        } else {
            atom.kind.clone()
        };
        let outcome = match atom.ok {
            Some(true) => " ok",
            Some(false) => " FAILED",
            None => "",
        };
        let text: String = atom.text.chars().take(300).collect();
        out.push_str(&format!(
            "[{}] {}{} {}: {}\n",
            atom.seq, atom.kind, outcome, who, text
        ));
    }
    out
}

#[cfg(test)]
mod tests;
