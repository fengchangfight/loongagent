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
    /// The resolved layered settings (ADR-0008) — distinct from `profile`, which is
    /// the profile's *name*. The kernel uses it for the two things it owns:
    /// narrowing the PDP ladder, and which skills exist.
    pub settings: Profile,
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
            max_steps: 8,
        }
    }
}

/// The Landlock PEP, installed with `pre_exec` so the restriction lands in the
/// forked child and can never leak into the server (or into a sibling tool).
struct LandlockConfinement {
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
    /// The bridge to this session's MCP servers, for `mcp` hooks and for prompts
    /// invoked as commands.
    mcp_caller: Option<Arc<KernelMcp>>,
    /// Derived, disposable SQLite view of the log (ADR-0003). It trails the log
    /// by design: the log is the truth, this only makes queries fast. A failure
    /// to index never loses an event — it marks the index stale instead.
    index: loong_index::Index,
    index_path: PathBuf,
    index_stale: Option<String>,
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
        config.budget_tokens = narrow_budget(config.budget_tokens, config.settings.budget_tokens);
        config.keep_newest_tokens = config
            .keep_newest_tokens
            .min(config.settings.keep_newest_tokens.unwrap_or(u64::MAX));
        config.compact_at_percent = config
            .compact_at_percent
            .min(config.settings.compact_at_percent.unwrap_or(u64::MAX));
        config.max_steps = config.max_steps.min(
            config
                .settings
                .max_steps
                .map(|steps| steps.max(1))
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
        match &mcp_caller {
            Some(caller) => tools.register(SkillTool::with_fetcher(
                Arc::clone(&skills),
                Arc::clone(caller) as Arc<dyn PromptFetcher>,
            )),
            None => tools.register(SkillTool::new(Arc::clone(&skills))),
        }

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
            mcp_caller,
            index,
            index_path,
            index_stale: None,
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

    /// The one place the system prompt is built: a short frame plus the
    /// description-only skill catalogue (ADR-0006 progressive disclosure).
    fn seat_system_prompt(&mut self) -> Result<(), KernelError> {
        let body = format!(
            "You are loongagent, a coding agent working in a user's workspace. Prefer reading \
             over guessing, keep changes small, and say plainly when you are unsure.\n\n             Skills and commands available here are listed below as `/name — description`. They \
             are not loaded yet: call the `skill` tool with a name to load its full instructions \
             before doing work that matches it.\n\n{}",
            self.skills.catalogue()
        );
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
            let mut pending: Option<(Option<String>, Vec<loong_providers::ToolCallRequest>)> = None;
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
                        // Attach it to the assistant turn it belongs to; a call with no
                        // assistant message in front of it is a broken log, and saying
                        // so beats inventing one.
                        let Some((_, calls)) = pending.as_mut() else {
                            return Err(KernelError::Session(format!(
                                "tool.call {} at seq {} has no assistant message before it",
                                call.call_id, event.seq
                            )));
                        };
                        calls.push(loong_providers::ToolCallRequest {
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
                        let (text, calls) = pending.take().unwrap_or((None, Vec::new()));
                        self.push_assistant(from.as_deref(), text.clone(), &calls);
                        self.push_tool_result(from.as_deref(), &result.call_id, &result.output);
                    }
                    gen::event::COMPACTION_APPLIED => {
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
        pending: &mut Option<(Option<String>, Vec<loong_providers::ToolCallRequest>)>,
    ) -> Result<(), KernelError> {
        if let Some((text, calls)) = pending.take() {
            self.push_assistant(None, text, &calls);
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

    fn run(
        &mut self,
        mut events: Vec<u64>,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<TurnOutcome, KernelError> {
        for step in 1..=self.config.max_steps {
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
                events.push(self.record_assistant(Some(&text), &[])?);
                return Ok(TurnOutcome {
                    steps: step,
                    final_text: Some(text),
                    pending_approval: None,
                    refused: None,
                    events,
                });
            }

            events.push(self.record_assistant(response.text.as_deref(), &response.tool_calls)?);
            for call in &response.tool_calls {
                match self.dispatch(call, &mut events)? {
                    Dispatch::Done => {}
                    Dispatch::Pending(pending) => {
                        self.pending = Some(pending.clone());
                        return Ok(TurnOutcome {
                            steps: step,
                            final_text: None,
                            pending_approval: Some(pending),
                            refused: None,
                            events,
                        });
                    }
                }
            }
        }
        let note = format!(
            "stopped after {} steps without finishing (step limit)",
            self.config.max_steps
        );
        events.push(self.record_assistant(Some(&note), &[])?);
        Ok(TurnOutcome {
            steps: self.config.max_steps,
            final_text: Some(note),
            pending_approval: None,
            refused: None,
            events,
        })
    }

    fn dispatch(
        &mut self,
        call: &ToolCallRequest,
        events: &mut Vec<u64>,
    ) -> Result<Dispatch, KernelError> {
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

    /// ADR-0006 §4: compaction, in a fixed order, and visible.
    ///
    /// Clearing comes first because stale tool output is the bulk of a long
    /// session's context and the least valuable part of it — the model can re-run
    /// a command, but it cannot re-read a decision. Only then are the oldest whole
    /// exchanges dropped, and never across a tool pair: an assistant's tool call
    /// keeps its answer, because a conversation that splits them is invalid to
    /// every provider.
    fn compact_if_needed(&mut self, events: &mut Vec<u64>) -> Result<(), KernelError> {
        let Some(budget) = self.ledger.budget() else {
            return Ok(());
        };
        let threshold = budget.saturating_mul(self.config.compact_at_percent) / 100;
        let before = self.ledger.total_tokens();
        if before <= threshold {
            return Ok(());
        }
        let keep = self.config.keep_newest_tokens.min(budget / 2);

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

        // Phase 2: only if still over, drop the oldest whole exchanges.
        let mut dropped_messages = 0;
        while self.ledger.total_tokens() > threshold {
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
        Ok(envelope.seq)
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
        Ok(self
            .log
            .append(
                gen::event::MODEL_SELECTED,
                json!({
                    "model": model,
                    "previous": previous,
                    "rule": rule.unwrap_or_else(|| "the router's default".to_string()),
                    "context_tokens": facts.context_tokens,
                }),
            )?
            .seq)
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
        self.tools
            .specs()
            .into_iter()
            .map(
                |(name, description, parameters)| loong_providers::ToolSpec {
                    name,
                    description,
                    parameters,
                },
            )
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loong_providers::{
        CompletionRequest, CompletionResponse, Model, ModelError, ScriptedModel,
    };
    use loong_schema::generated::event as ev;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    static N: AtomicU64 = AtomicU64::new(0);

    struct Temp {
        dir: PathBuf,
        log: PathBuf,
    }
    impl Temp {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "loong-kernel-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let log = dir.join("session.jsonl");
            Temp { dir, log }
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Install a hook manifest (and its scripts) in the workspace. `trust`
    /// decides whether the hooks are allowed to run at all.
    fn install_hooks(dir: &Path, manifest: &str, scripts: &[(&str, &str)], trust: bool) {
        let hooks = dir.join(".loongagent");
        std::fs::create_dir_all(&hooks).unwrap();
        for (name, body) in scripts {
            std::fs::write(hooks.join(name), body).unwrap();
        }
        std::fs::write(hooks.join("hooks.toml"), manifest).unwrap();
        if trust {
            loong_hooks::HookSet::load(dir)
                .unwrap()
                .trust_now()
                .unwrap();
        }
    }

    fn call(id: &str, tool: &str, args: serde_json::Value) -> ToolCallRequest {
        ToolCallRequest {
            call_id: id.into(),
            tool: tool.into(),
            args,
        }
    }

    fn response(calls: Vec<ToolCallRequest>, text: Option<&str>) -> CompletionResponse {
        CompletionResponse {
            text: text.map(|t| t.to_string()),
            tool_calls: calls,
            usage: Usage {
                tokens_in: 7,
                tokens_out: 3,
            },
        }
    }

    fn kinds(log: &Path) -> Vec<String> {
        loong_eventlog::replay(log)
            .unwrap()
            .events
            .into_iter()
            .map(|e| e.kind)
            .collect()
    }

    #[test]
    fn a_tool_runs_and_the_turn_finishes() {
        let t = Temp::new();
        std::fs::write(t.dir.join("hello.txt"), "hi from the workspace").unwrap();
        let model = ScriptedModel::new(vec![
            response(
                vec![call("c1", "read_file", json!({"path": "hello.txt"}))],
                None,
            ),
            response(vec![], Some("the file says hi")),
        ]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        let out = k.submit("read hello.txt").unwrap();

        assert_eq!(out.pending_approval, None);
        assert_eq!(out.final_text.as_deref(), Some("the file says hi"));
        assert_eq!(out.steps, 2);
        assert_eq!(
            kinds(&t.log),
            vec![
                // The session's own opening: what entered context before the work.
                ev::SKILLS_LOADED,
                ev::MESSAGE_APPENDED,
                ev::COST_RECORDED,
                ev::MESSAGE_APPENDED,
                ev::TOOL_CALL,
                ev::POLICY_DECIDED,
                ev::TOOL_RESULT,
                ev::COST_RECORDED,
                ev::MESSAGE_APPENDED,
            ]
        );
        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let policy = events
            .iter()
            .find(|e| e.kind == ev::POLICY_DECIDED)
            .unwrap();
        assert_eq!(policy.data["verdict"], "allow");
        assert_eq!(
            policy.data["rule"], "builtin:investigate",
            "reading a file inside the workspace is an investigate action"
        );
        let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
        assert_eq!(result.data["ok"], true);
        assert!(result.data["output"]
            .as_str()
            .unwrap()
            .contains("hi from the workspace"));
    }

    #[test]
    fn an_out_of_bounds_write_asks_then_resumes_on_the_answer() {
        let t = Temp::new();
        let escape = t.dir.parent().unwrap().join("loong-escape.txt");
        let _ = std::fs::remove_file(&escape);
        let model = ScriptedModel::new(vec![
            response(
                vec![call(
                    "c1",
                    "write_file",
                    json!({"path": escape.to_string_lossy(), "content": "x"}),
                )],
                None,
            ),
            response(vec![], Some("understood")),
        ]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        let out = k.submit("write outside").unwrap();

        let pending = out.pending_approval.expect("out-of-bounds asks");
        assert_eq!(pending.class, ActionClass::OutOfBounds);
        assert_eq!(pending.rule, "builtin:out_of_bounds");
        assert!(!escape.exists(), "nothing is written before the answer");

        let resumed = k
            .respond_to_approval(&pending.request_id, false, ApprovalScope::Once, Some("no"))
            .unwrap();
        assert_eq!(resumed.final_text.as_deref(), Some("understood"));
        assert!(!escape.exists());
        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let decided = events
            .iter()
            .find(|e| e.kind == ev::APPROVAL_DECIDED)
            .unwrap();
        assert_eq!(decided.data["granted"], false);
        assert_eq!(decided.data["scope"], "once");
        assert_eq!(decided.data["reason"], "no");
        let denied = events
            .iter()
            .filter(|e| e.kind == ev::TOOL_RESULT)
            .next()
            .unwrap();
        assert_eq!(denied.data["ok"], false);
    }

    #[test]
    fn a_granted_approval_still_cannot_escape_the_workspace() {
        let t = Temp::new();
        let escape = t.dir.parent().unwrap().join("loong-escape-2.txt");
        let _ = std::fs::remove_file(&escape);
        let model = ScriptedModel::new(vec![
            response(
                vec![call(
                    "c1",
                    "write_file",
                    json!({"path": escape.to_string_lossy(), "content": "x"}),
                )],
                None,
            ),
            response(vec![], Some("ok")),
        ]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        let pending = k.submit("write outside").unwrap().pending_approval.unwrap();
        k.respond_to_approval(&pending.request_id, true, ApprovalScope::Once, None)
            .unwrap();
        assert!(!escape.exists(), "the tool is the second line of defence");
        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
        assert_eq!(result.data["ok"], false);
        assert!(result.data["output"]
            .as_str()
            .unwrap()
            .contains("outside the workspace"));
    }

    #[test]
    fn a_tightened_policy_denies_without_asking() {
        let t = Temp::new();
        let model = ScriptedModel::new(vec![
            response(
                vec![call("c1", "read_file", json!({"path": "/etc/hostname"}))],
                None,
            ),
            response(vec![], Some("could not read that")),
        ]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        k.pdp_mut()
            .tighten(ActionClass::OutOfBounds, Verdict::Deny)
            .unwrap();
        let out = k.submit("read /etc/hostname").unwrap();
        assert!(out.pending_approval.is_none(), "deny never asks");
        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let policy = events
            .iter()
            .find(|e| e.kind == ev::POLICY_DECIDED)
            .unwrap();
        assert_eq!(policy.data["verdict"], "deny");
        assert_eq!(policy.data["rule"], "session:out_of_bounds");
        let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
        assert!(result.data["output"]
            .as_str()
            .unwrap()
            .contains("denied by policy"));
    }

    #[test]
    fn an_unknown_tool_is_reported_not_fatal() {
        let t = Temp::new();
        let model = ScriptedModel::new(vec![
            response(vec![call("c1", "no_such_tool", json!({}))], None),
            response(vec![], Some("fine")),
        ]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        let out = k.submit("go").unwrap();
        assert_eq!(out.final_text.as_deref(), Some("fine"));
        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
        assert!(result.data["output"]
            .as_str()
            .unwrap()
            .contains("unknown tool"));
    }

    #[test]
    fn the_step_limit_stops_a_spinning_model() {
        let t = Temp::new();
        std::fs::write(t.dir.join("a.txt"), "x").unwrap();
        let spin =
            |_: usize| response(vec![call("c", "read_file", json!({"path": "a.txt"}))], None);
        let model = ScriptedModel::new((0..10).map(spin).collect());
        let mut config = KernelConfig::new(&t.dir);
        config.max_steps = 3;
        let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
        let out = k.submit("go").unwrap();
        assert_eq!(out.steps, 3);
        assert!(out.final_text.unwrap().contains("step limit"));
    }

    #[test]
    fn the_ledger_and_the_conversation_stay_in_step() {
        let t = Temp::new();
        std::fs::write(t.dir.join("a.txt"), "x").unwrap();
        let model = ScriptedModel::new(vec![
            response(
                vec![call("c1", "read_file", json!({"path": "a.txt"}))],
                None,
            ),
            response(vec![], Some("done")),
        ]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        k.submit("go").unwrap();
        // Every message has exactly one ledger entry — the system prompt included,
        // which is why the count is over all entries rather than History.
        assert_eq!(
            k.ledger().entries().len(),
            k.messages().len(),
            "every message is a ledger entry, and none is missing one"
        );
        assert!(k.ledger().explain().iter().any(|r| r.source == "user"));
        assert!(
            k.ledger().explain().iter().any(|r| r.source == "system"),
            "the system prompt is accounted for"
        );
    }

    #[test]
    fn the_pep_stops_an_escape_the_classifier_cannot_see() {
        let t = Temp::new();
        let escaped_name = format!(
            "{}-escaped.txt",
            t.dir.file_name().unwrap().to_string_lossy()
        );
        let escape = t.dir.parent().unwrap().join(&escaped_name);
        let _ = std::fs::remove_file(&escape);

        // `touch ../<file>`: the classifier cannot see through `..` here, so it
        // says write_workspace and the PDP allows it. The OS is what stops it.
        let command = format!("touch ../{escaped_name}");
        let model = ScriptedModel::new(vec![
            response(vec![call("c1", "shell", json!({"command": command}))], None),
            response(vec![], Some("blocked")),
        ]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        assert!(
            k.confinement_note().contains("landlock"),
            "the kernel must confine tool children: {}",
            k.confinement_note()
        );
        k.submit("try to write outside").unwrap();

        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let policy = events
            .iter()
            .find(|e| e.kind == ev::POLICY_DECIDED)
            .unwrap();
        assert_eq!(policy.data["verdict"], "allow", "the classifier allowed it");
        assert_eq!(policy.data["action_class"], "write_workspace");
        let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
        assert_eq!(result.data["ok"], false, "the OS refused it");
        let output = result.data["output"].as_str().unwrap();
        assert!(output.contains("Permission denied"), "output was: {output}");
        assert!(!escape.exists(), "the escaped file must not exist");
    }

    #[test]
    fn the_pep_still_allows_a_workspace_write_through_the_shell() {
        let t = Temp::new();
        let model = ScriptedModel::new(vec![
            response(
                vec![call(
                    "c1",
                    "shell",
                    json!({"command": "touch inside-control.txt"}),
                )],
                None,
            ),
            response(vec![], Some("done")),
        ]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        k.submit("write a file inside").unwrap();
        assert!(
            t.dir.join("inside-control.txt").exists(),
            "the allowed write happened"
        );
        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
        assert_eq!(result.data["ok"], true, "{:?}", result.data["output"]);
    }

    #[test]
    fn a_read_only_shell_command_is_an_investigate_action() {
        let t = Temp::new();
        std::fs::write(t.dir.join("a.txt"), "content here").unwrap();
        let model = ScriptedModel::new(vec![
            response(
                vec![call("c1", "shell", json!({"command": "cat a.txt"}))],
                None,
            ),
            response(vec![], Some("read")),
        ]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        k.submit("show me a.txt").unwrap();
        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let policy = events
            .iter()
            .find(|e| e.kind == ev::POLICY_DECIDED)
            .unwrap();
        assert_eq!(policy.data["action_class"], "investigate");
        assert_eq!(policy.data["verdict"], "allow");
        let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
        assert!(result.data["output"]
            .as_str()
            .unwrap()
            .contains("content here"));
    }

    fn fake_mcp() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../mcp/tests/fake_mcp_server.py")
    }

    fn install_mcp(dir: &Path, action_class: &str, trust: bool) -> bool {
        install_mcp_mode(dir, action_class, trust, "normal")
    }

    fn install_mcp_mode(dir: &Path, action_class: &str, trust: bool, mode: &str) -> bool {
        let script = fake_mcp();
        if !script.exists()
            || std::process::Command::new("python3")
                .arg("--version")
                .output()
                .is_err()
        {
            return false;
        }
        let hooks = dir.join(".loongagent");
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::write(
            hooks.join("mcp.toml"),
            format!(
                "[[server]]\nname = \"fake\"\ncommand = \"python3\"\nargs = [\"{}\", \"normal\"]\n\
                 action_class = \"{action_class}\"\n",
                script.display()
            )
            .replace("\"normal\"", &format!("\"{mode}\"")),
        )
        .unwrap();
        if trust {
            loong_mcp::McpSet::load(dir).unwrap().trust_now().unwrap();
        }
        true
    }

    #[test]
    fn an_mcp_prompt_becomes_a_command_with_its_body_fetched_on_invoke() {
        let t = Temp::new();
        if !install_mcp(&t.dir, "investigate", true) {
            return;
        }
        let (model, seen) = RecordingModel::new(vec![response(vec![], Some("reviewed"))]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();

        // The catalogue advertises the server's prompts as commands…
        assert!(
            k.skills().names().contains(&"mcp__fake__review"),
            "{:?}",
            k.skills().names()
        );
        let sent = seen.lock().unwrap();
        assert!(sent.is_empty(), "nothing has been asked of the model yet");
        drop(sent);

        // …and invoking one fetches the body from the server, with the argument
        // mapped onto the name the prompt declared.
        k.submit("/mcp__fake__review HEAD~1").unwrap();
        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let invoked = events.iter().find(|e| e.kind == ev::SKILL_INVOKED).unwrap();
        assert_eq!(invoked.data["name"], "mcp__fake__review");
        assert_eq!(invoked.data["source"], "mcp:fake/review");
        assert!(
            invoked.data["tokens"].as_u64().unwrap() > 0,
            "the fetched body is accounted for"
        );
        assert!(invoked.data["error"].is_null());

        let sent = seen.lock().unwrap();
        let user = sent[0]
            .iter()
            .find_map(|m| match m {
                ChatMessage::User { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap();
        assert!(
            user.contains("Review HEAD~1 and report ONLY real bugs"),
            "the argument reached the server and the body came back: {user}"
        );
    }

    #[test]
    fn the_model_can_fetch_an_mcp_prompt_body_with_the_tool() {
        let t = Temp::new();
        if !install_mcp(&t.dir, "investigate", true) {
            return;
        }
        let model = ScriptedModel::new(vec![
            response(
                vec![call("c1", "skill", json!({"name": "mcp__fake__review"}))],
                None,
            ),
            response(vec![], Some("loaded")),
        ]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        k.submit("get the review checklist").unwrap();

        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
        assert_eq!(result.data["ok"], true, "{:?}", result.data["output"]);
        let output = result.data["output"].as_str().unwrap();
        assert!(
            output.contains("Review HEAD and report ONLY real bugs"),
            "{output}"
        );
        assert!(
            output.contains("mcp:fake/review"),
            "the origin is named: {output}"
        );
    }

    #[test]
    fn help_lists_the_servers_prompts_too() {
        let t = Temp::new();
        if !install_mcp(&t.dir, "investigate", true) {
            return;
        }
        let (model, seen) = RecordingModel::new(vec![response(vec![], Some("listed"))]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        k.submit("/help").unwrap();
        let sent = seen.lock().unwrap();
        let user = sent[0]
            .iter()
            .find_map(|m| match m {
                ChatMessage::User { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap();
        assert!(user.contains("/mcp__fake__review"), "{user}");
        assert!(user.contains("/mcp__fake__release"), "{user}");
        // A prompt with no declared arguments gets no hint.
        assert!(user.contains("Walk the release checklist"), "{user}");
    }

    #[test]
    fn a_server_without_prompts_contributes_none() {
        let t = Temp::new();
        if !install_mcp_mode(&t.dir, "investigate", true, "no-prompts") {
            return;
        }
        let model = ScriptedModel::saying("hi");
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        assert!(
            !k.skills().names().iter().any(|n| n.starts_with("mcp__")),
            "a server that advertises no prompts adds no commands: {:?}",
            k.skills().names()
        );
        // Its tools still work.
        assert!(k.tools_mut().names().contains(&"mcp__fake__ping"));
    }

    #[test]
    fn a_prompt_that_fails_to_load_refuses_the_turn_instead_of_reaching_the_model() {
        let t = Temp::new();
        if !install_mcp(&t.dir, "investigate", true) {
            return;
        }
        // An empty script: if the model were called the turn would fail instead.
        let model = ScriptedModel::new(vec![]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        assert!(k.skills().names().contains(&"mcp__fake__broken"));

        let outcome = k.submit("/mcp__fake__broken").unwrap();
        let refused = outcome
            .refused
            .expect("a prompt that cannot load is refused");
        assert!(refused.contains("could not be loaded"), "{refused}");

        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let invoked = events.iter().find(|e| e.kind == ev::SKILL_INVOKED).unwrap();
        assert_eq!(invoked.data["name"], "mcp__fake__broken");
        assert!(
            invoked.data["error"]
                .as_str()
                .unwrap()
                .contains("prompt handler is broken"),
            "{:?}",
            invoked.data
        );
        assert!(
            !events.iter().any(|e| e.kind == ev::COST_RECORDED),
            "the model was never called"
        );
    }

    #[test]
    fn an_mcp_tool_is_just_another_tool() {
        let t = Temp::new();
        if !install_mcp(&t.dir, "investigate", true) {
            eprintln!("skipping: no python3 or no fixture");
            return;
        }
        let model = ScriptedModel::new(vec![
            response(
                vec![call("c1", "mcp__fake__ping", json!({"message": "hello"}))],
                None,
            ),
            response(vec![], Some("done")),
        ]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        assert!(k.mcp_note().contains("trusted"), "{}", k.mcp_note());
        assert_eq!(k.mcp_servers(), 1);
        assert!(k.tools_mut().names().contains(&"mcp__fake__ping"));

        k.submit("ping the server").unwrap();
        let events = loong_eventlog::replay(&t.log).unwrap().events;
        // Same events as any other tool: call, decision by declared class, result.
        let call_event = events.iter().find(|e| e.kind == ev::TOOL_CALL).unwrap();
        assert_eq!(call_event.data["tool"], "mcp__fake__ping");
        assert_eq!(call_event.data["action_class"], "investigate");
        let policy = events
            .iter()
            .find(|e| e.kind == ev::POLICY_DECIDED)
            .unwrap();
        assert_eq!(policy.data["verdict"], "allow");
        let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
        assert_eq!(result.data["ok"], true, "{:?}", result.data["output"]);
        assert!(result.data["output"]
            .as_str()
            .unwrap()
            .contains("pong: hello"));
    }

    #[test]
    fn an_mcp_tool_with_the_default_class_asks_first() {
        let t = Temp::new();
        if !install_mcp(&t.dir, "out_of_bounds", true) {
            return;
        }
        let model = ScriptedModel::new(vec![response(
            vec![call("c1", "mcp__fake__ping", json!({"message": "hi"}))],
            None,
        )]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        let outcome = k.submit("ping").unwrap();
        let pending = outcome.pending_approval.expect("the cautious default asks");
        assert!(
            pending.reason.contains("out_of_bounds"),
            "{}",
            pending.reason
        );

        // Answering resumes through the same seam as any built-in tool.
        k.respond_to_approval(&pending.request_id, true, ApprovalScope::Once, None)
            .unwrap();
        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
        assert_eq!(result.data["ok"], true, "{:?}", result.data["output"]);
        assert!(result.data["output"].as_str().unwrap().contains("pong: hi"));
    }

    #[test]
    fn an_mcp_hook_can_gate_a_builtin_tool() {
        let t = Temp::new();
        if !install_mcp(&t.dir, "investigate", true) {
            return;
        }
        install_hooks(
            &t.dir,
            "[[hook]]\nid = \"policy-svc\"\nevent = \"pre_tool\"\nkind = \"mcp\"\n\
             server = \"fake\"\ntool = \"verdict_deny\"\n",
            &[],
            true,
        );
        let model = ScriptedModel::new(vec![
            response(
                vec![call("c1", "shell", json!({"command": "touch gated.txt"}))],
                None,
            ),
            response(vec![], Some("understood")),
        ]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        k.submit("go").unwrap();

        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let fired = events
            .iter()
            .find(|e| e.kind == ev::HOOK_FIRED)
            .expect("the hook answered");
        assert_eq!(fired.data["kind"], "mcp");
        assert_eq!(fired.data["hook_id"], "policy-svc");
        assert_eq!(fired.data["verdict"], "deny");
        assert_eq!(fired.data["failed"], false);
        assert!(fired.data["reason"]
            .as_str()
            .unwrap()
            .contains("policy service says deny"));

        // A built-in tool, classified as an ordinary workspace write, stopped by an
        // external service — and the log says which one.
        let policy = events
            .iter()
            .find(|e| e.kind == ev::POLICY_DECIDED)
            .unwrap();
        assert_eq!(policy.data["verdict"], "deny");
        assert_eq!(policy.data["rule"], "hook:policy-svc");
        assert!(
            !t.dir.join("gated.txt").exists(),
            "the gated tool never ran"
        );
    }

    #[test]
    fn an_mcp_hook_without_a_server_fails_closed() {
        let t = Temp::new();
        // A hook that names a server the session does not have.
        install_hooks(
            &t.dir,
            "[[hook]]\nid = \"policy-svc\"\nevent = \"pre_tool\"\nkind = \"mcp\"\n\
             server = \"absent\"\ntool = \"decide\"\n",
            &[],
            true,
        );
        let model = ScriptedModel::new(vec![
            response(
                vec![call("c1", "shell", json!({"command": "touch nope.txt"}))],
                None,
            ),
            response(vec![], Some("understood")),
        ]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        k.submit("go").unwrap();
        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let fired = events.iter().find(|e| e.kind == ev::HOOK_FIRED).unwrap();
        assert_eq!(fired.data["verdict"], "deny");
        assert_eq!(fired.data["failed"], true);
        assert!(!t.dir.join("nope.txt").exists());
    }

    #[test]
    fn an_untrusted_mcp_manifest_starts_nothing() {
        let t = Temp::new();
        if !install_mcp(&t.dir, "investigate", false) {
            return;
        }
        let model = ScriptedModel::saying("hi");
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        assert_eq!(k.mcp_servers(), 0, "untrusted servers are never started");
        assert!(k.mcp_note().contains("INERT"), "{}", k.mcp_note());
        assert!(!k.tools_mut().names().iter().any(|n| n.starts_with("mcp__")));
    }

    #[test]
    fn a_required_server_that_cannot_start_is_fatal() {
        let t = Temp::new();
        let hooks = t.dir.join(".loongagent");
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::write(
            hooks.join("mcp.toml"),
            "[[server]]\nname = \"broken\"\ncommand = \"definitely-not-a-program\"\n",
        )
        .unwrap();
        loong_mcp::McpSet::load(&t.dir)
            .unwrap()
            .trust_now()
            .unwrap();
        let model = ScriptedModel::saying("hi");
        let err = match Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)) {
            Ok(_) => panic!("a required server that cannot start must be fatal"),
            Err(e) => e,
        };
        assert!(matches!(err, KernelError::Mcp(_)), "{err}");
    }

    #[test]
    fn an_optional_server_is_only_a_note() {
        let t = Temp::new();
        let hooks = t.dir.join(".loongagent");
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::write(
            hooks.join("mcp.toml"),
            "[[server]]\nname = \"broken\"\ncommand = \"definitely-not-a-program\"\nrequired = false\n",
        )
        .unwrap();
        loong_mcp::McpSet::load(&t.dir)
            .unwrap()
            .trust_now()
            .unwrap();
        let model = ScriptedModel::saying("hi");
        let k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        assert_eq!(k.mcp_servers(), 0);
        assert!(k.mcp_note().contains("unavailable"), "{}", k.mcp_note());
    }

    /// A model that answers from a script and records what it was shown, which is
    /// the only way to check what compaction actually sent.
    struct RecordingModel {
        responses: std::collections::VecDeque<CompletionResponse>,
        seen: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
    }

    impl RecordingModel {
        fn new(responses: Vec<CompletionResponse>) -> (Self, Arc<Mutex<Vec<Vec<ChatMessage>>>>) {
            let seen = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    responses: responses.into(),
                    seen: Arc::clone(&seen),
                },
                seen,
            )
        }
    }

    impl Model for RecordingModel {
        fn complete(
            &mut self,
            request: &CompletionRequest,
        ) -> Result<CompletionResponse, ModelError> {
            self.seen.lock().unwrap().push(request.messages.clone());
            self.responses
                .pop_front()
                .ok_or_else(|| ModelError::Transport("the script is exhausted".into()))
        }
    }

    /// A file big enough that its output has to be cleared.
    fn big_file(dir: &Path, name: &str, lines: usize) -> PathBuf {
        let body: String = (0..lines)
            .map(|i| format!("line {i} of a large file\n"))
            .collect();
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    /// A resolved profile from a real file, so the provenance (which the PDP rule
    /// string is built from) is the real thing rather than a hand-made struct.
    fn settings_from(dir: &Path, body: &str) -> Profile {
        let path = dir.join("profile.toml");
        std::fs::write(&path, body).unwrap();
        loong_config::resolve(&[(loong_config::Layer::User, path)], dir).unwrap()
    }

    fn install_command(dir: &Path, name: &str, body: &str) {
        let commands = dir.join(".loongagent/commands");
        std::fs::create_dir_all(&commands).unwrap();
        std::fs::write(commands.join(format!("{name}.md")), body).unwrap();
    }

    #[test]
    fn a_command_expands_into_its_body_and_the_event_names_the_file() {
        let t = Temp::new();
        install_command(
            &t.dir,
            "review",
            "---
description: Review a diff for real bugs
---

Look at $ARGUMENTS and report only real bugs.
",
        );
        let (model, seen) = RecordingModel::new(vec![response(vec![], Some("reviewed"))]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        k.submit("/review HEAD~1").unwrap();

        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let invoked = events
            .iter()
            .find(|e| e.kind == ev::SKILL_INVOKED)
            .expect("skill.invoked");
        assert_eq!(invoked.data["name"], "review");
        assert_eq!(invoked.data["args"], "HEAD~1");
        assert!(invoked.data["source"]
            .as_str()
            .unwrap()
            .ends_with("review.md"));
        assert!(invoked.data["tokens"].as_u64().unwrap() > 0);

        // What the model received is the body, with the arguments substituted — and
        // the user's own words are still recoverable from the event above.
        let sent = seen.lock().unwrap();
        let first = sent.first().unwrap();
        let user = first
            .iter()
            .find_map(|m| match m {
                ChatMessage::User { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap();
        assert!(
            user.contains("Look at HEAD~1 and report only real bugs."),
            "{user}"
        );
    }

    #[test]
    fn an_unknown_command_is_refused_and_never_reaches_the_model() {
        let t = Temp::new();
        // An empty script: if the model were called, the turn would fail instead.
        let model = ScriptedModel::new(vec![]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        let outcome = k.submit("/reveiw HEAD~1").unwrap();

        let refused = outcome.refused.expect("a typo is refused, not guessed at");
        assert!(refused.contains("unknown command /reveiw"), "{refused}");
        assert!(refused.contains("/help"), "the way out is named: {refused}");

        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let invoked = events.iter().find(|e| e.kind == ev::SKILL_INVOKED).unwrap();
        assert!(
            invoked.data["error"]
                .as_str()
                .unwrap()
                .contains("no skill named"),
            "{:?}",
            invoked.data
        );
        assert!(
            !events.iter().any(|e| e.kind == ev::COST_RECORDED),
            "the model was never called"
        );
    }

    #[test]
    fn a_sentence_that_starts_with_a_path_is_a_normal_prompt() {
        let t = Temp::new();
        let (model, seen) = RecordingModel::new(vec![response(vec![], Some("noted"))]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        k.submit("/etc/hosts is misconfigured, please look")
            .unwrap();

        let sent = seen.lock().unwrap();
        assert!(
            sent[0]
                .iter()
                .any(|m| matches!(m, ChatMessage::User { text } if text.starts_with("/etc/hosts"))),
            "a path sentence reaches the model as itself"
        );
        let events = loong_eventlog::replay(&t.log).unwrap().events;
        assert!(!events.iter().any(|e| e.kind == ev::SKILL_INVOKED));
    }

    #[test]
    fn the_catalogue_is_in_context_and_the_body_is_not() {
        let t = Temp::new();
        install_command(
            &t.dir,
            "release",
            "---
description: Cut a release
---

SECRET-BODY-STEPS
",
        );
        let (model, seen) = RecordingModel::new(vec![response(vec![], Some("ok"))]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        k.submit("say hi").unwrap();

        let sent = seen.lock().unwrap();
        let system = match sent[0].first() {
            Some(ChatMessage::System { text }) => text.clone(),
            other => panic!("the prompt starts with a system message, got {other:?}"),
        };
        assert!(system.contains("/release — Cut a release"), "{system}");
        assert!(system.contains("/help"), "{system}");
        assert!(
            !system.contains("SECRET-BODY-STEPS"),
            "bodies load on invoke only: {system}"
        );

        // …and the catalogue is accounted for as a pinned ledger entry.
        let explain = k.ledger().explain();
        let row = explain
            .iter()
            .find(|r| r.source == "system")
            .expect("the system prompt");
        assert!(row.pinned);
        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let loaded = events.iter().find(|e| e.kind == ev::SKILLS_LOADED).unwrap();
        assert!(loaded.data["names"].as_str().unwrap().contains("release"));
    }

    #[test]
    fn the_model_can_load_a_body_by_name() {
        let t = Temp::new();
        install_command(
            &t.dir,
            "release",
            "---
description: Cut a release
---

Step one: breathe.
",
        );
        let model = ScriptedModel::new(vec![
            response(vec![call("c1", "skill", json!({"name": "release"}))], None),
            response(vec![], Some("loaded")),
        ]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        k.submit("do a release").unwrap();

        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
        assert_eq!(result.data["ok"], true, "{:?}", result.data["output"]);
        assert!(result.data["output"]
            .as_str()
            .unwrap()
            .contains("Step one: breathe."));
        let policy = events
            .iter()
            .find(|e| e.kind == ev::POLICY_DECIDED)
            .unwrap();
        assert_eq!(
            policy.data["action_class"], "investigate",
            "loading a body is read-only"
        );
    }

    #[test]
    fn compaction_never_drops_the_system_prompt() {
        let t = Temp::new();
        let filler = "x".repeat(2_000);
        let (model, seen) = RecordingModel::new(vec![
            response(vec![], Some("first")),
            response(vec![], Some("second")),
            response(vec![], Some("third")),
        ]);
        let mut config = KernelConfig::new(&t.dir);
        config.budget_tokens = Some(1_200);
        config.keep_newest_tokens = 200;
        let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
        for turn in 1..=3 {
            k.submit(&format!("turn {turn} {filler}")).unwrap();
        }
        let sent = seen.lock().unwrap();
        for (index, messages) in sent.iter().enumerate() {
            assert!(
                matches!(messages.first(), Some(ChatMessage::System { .. })),
                "turn {index}: the system prompt is always first"
            );
        }
        assert!(!sent.last().unwrap().is_empty());
    }

    struct StreamingModel {
        pieces: Vec<String>,
    }

    impl Model for StreamingModel {
        fn complete(
            &mut self,
            _request: &CompletionRequest,
        ) -> Result<CompletionResponse, ModelError> {
            unreachable!("this model is only used through its streaming path")
        }
        fn complete_streaming(
            &mut self,
            _request: &CompletionRequest,
            on_delta: &mut dyn FnMut(&str),
        ) -> Result<CompletionResponse, ModelError> {
            for piece in &self.pieces {
                on_delta(piece);
            }
            Ok(CompletionResponse {
                text: Some(self.pieces.concat()),
                tool_calls: Vec::new(),
                usage: Usage {
                    tokens_in: 3,
                    tokens_out: 9,
                },
            })
        }
    }

    #[test]
    fn deltas_are_a_preview_and_never_the_record() {
        let t = Temp::new();
        let model = StreamingModel {
            pieces: vec!["Hel".to_string(), "lo, ".to_string(), "world".to_string()],
        };
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();

        let mut seen = Vec::new();
        let outcome = k
            .submit_with("say hi", &mut |piece| seen.push(piece.to_string()))
            .unwrap();
        assert_eq!(
            seen,
            vec!["Hel", "lo, ", "world"],
            "in the order they were written"
        );
        assert_eq!(outcome.final_text.as_deref(), Some("Hello, world"));

        // The record is the message, not the fragments: nothing in the log mentions
        // a delta, and the assistant message holds the whole answer.
        let events = loong_eventlog::replay(&t.log).unwrap().events;
        assert!(
            events.iter().all(|e| e.kind != "model.delta"),
            "deltas are not logged"
        );
        let answer = events
            .iter()
            .filter(|e| e.kind == ev::MESSAGE_APPENDED && e.data["role"] == "assistant")
            .next_back()
            .unwrap();
        assert_eq!(answer.data["text"], "Hello, world");
        // …and the ledger accounted for the answer once, not once per fragment.
        let history = k
            .ledger()
            .entries()
            .iter()
            .filter(|e| e.source == "assistant")
            .count();
        assert_eq!(history, 1, "one assistant entry, not one per delta");
    }

    /// Write the child the way `loongagent fork` does: one lineage event, nothing copied.
    fn fork_log(child: &Path, parent_id: &str, at: u64) {
        let mut log = loong_eventlog::SessionLog::create(child).unwrap();
        log.append(
            gen::event::SESSION_FORKED,
            serde_json::json!({
                "origin": "fork",
                "parent_session": parent_id,
                "parent_seq": at,
            }),
        )
        .unwrap();
    }

    fn saw(seen: &Arc<Mutex<Vec<Vec<ChatMessage>>>>, needle: &str) -> bool {
        seen.lock()
            .unwrap()
            .iter()
            .flatten()
            .any(|message| match message {
                ChatMessage::User { text } => text == needle,
                ChatMessage::Assistant { text, .. } => text.as_deref() == Some(needle),
                ChatMessage::Tool { output, .. } => output == needle,
                ChatMessage::System { text } => text == needle,
            })
    }

    #[test]
    fn a_reopened_session_hands_the_model_its_own_history() {
        // The bug this exists for: `messages` started empty, so resuming a session
        // asked the model to carry on a conversation it could not see. It answered
        // "this is the first message in our conversation" — correctly.
        let t = Temp::new();
        let (model, _) = RecordingModel::new(vec![response(vec![], Some("stored"))]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        k.submit("remember 4242").unwrap();
        drop(k);

        let (model, seen) = RecordingModel::new(vec![response(vec![], Some("4242"))]);
        let mut k = Kernel::open(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        k.submit("what number?").unwrap();

        assert!(saw(&seen, "remember 4242"), "the earlier user turn");
        assert!(saw(&seen, "stored"), "and the answer to it");
        assert!(saw(&seen, "what number?"), "and the new turn");
        // The context view agrees with the messages.
        assert!(k
            .context_explain()
            .entries
            .iter()
            .any(|entry| entry.source == "user"));
    }

    #[test]
    fn a_child_sees_its_parents_prefix_without_copying_it() {
        let t = Temp::new();
        let parent = t.dir.join("parent.jsonl");
        let (model, _) = RecordingModel::new(vec![response(vec![], Some("stored"))]);
        let mut k = Kernel::create(&parent, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        k.submit("remember 4242").unwrap();
        let at = k.last_seq();
        drop(k);

        let child = t.dir.join("child.jsonl");
        fork_log(&child, "parent", at);
        assert_eq!(
            loong_eventlog::replay(&child).unwrap().events.len(),
            1,
            "a child references; it does not copy"
        );

        let (model, seen) = RecordingModel::new(vec![response(vec![], Some("4242"))]);
        let mut k = Kernel::open(&child, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        k.submit("what number?").unwrap();
        assert!(saw(&seen, "remember 4242"), "the parent's prefix");
        assert!(saw(&seen, "what number?"), "and the child's own turn");

        // And the panel can say where the inherited history came from.
        assert!(
            k.context_explain()
                .entries
                .iter()
                .any(|entry| entry.source == "user from parent"),
            "{:?}",
            k.context_explain()
                .entries
                .iter()
                .map(|e| e.source.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn forking_earlier_leaves_the_later_parent_turns_behind() {
        let t = Temp::new();
        let parent = t.dir.join("parent.jsonl");
        let (model, _) = RecordingModel::new(vec![
            response(vec![], Some("first")),
            response(vec![], Some("second")),
        ]);
        let mut k = Kernel::create(&parent, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        k.submit("turn one").unwrap();
        let early = k.last_seq();
        k.submit("turn two").unwrap();
        drop(k);

        let child = t.dir.join("child.jsonl");
        fork_log(&child, "parent", early);
        let (model, seen) = RecordingModel::new(vec![response(vec![], Some("ok"))]);
        let mut k = Kernel::open(&child, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        k.submit("continue").unwrap();
        assert!(saw(&seen, "turn one"), "what the fork point includes");
        assert!(!saw(&seen, "turn two"), "and what it does not");
    }

    #[test]
    fn a_chain_of_children_is_stitched_oldest_first() {
        let t = Temp::new();
        let a = t.dir.join("a.jsonl");
        let (model, _) = RecordingModel::new(vec![response(vec![], Some("a-answer"))]);
        let mut k = Kernel::create(&a, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        k.submit("from a").unwrap();
        let a_at = k.last_seq();
        drop(k);

        let b = t.dir.join("b.jsonl");
        fork_log(&b, "a", a_at);
        let (model, _) = RecordingModel::new(vec![response(vec![], Some("b-answer"))]);
        let mut k = Kernel::open(&b, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        k.submit("from b").unwrap();
        let b_at = k.last_seq();
        drop(k);

        let c = t.dir.join("c.jsonl");
        fork_log(&c, "b", b_at);
        let (model, seen) = RecordingModel::new(vec![response(vec![], Some("c-answer"))]);
        let mut k = Kernel::open(&c, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        k.submit("from c").unwrap();

        {
            let requests = seen.lock().unwrap();
            let last = requests.last().unwrap();
            let texts: Vec<String> = last
                .iter()
                .filter_map(|message| match message {
                    ChatMessage::User { text } => Some(text.clone()),
                    _ => None,
                })
                .collect();
            assert_eq!(texts, vec!["from a", "from b", "from c"], "oldest first");
        }
        // And the provenance is per ancestor, not one anonymous prefix.
        let sources: Vec<String> = k
            .context_explain()
            .entries
            .iter()
            .map(|entry| entry.source.clone())
            .collect();
        assert!(sources.iter().any(|s| s == "user from a"), "{sources:?}");
        assert!(sources.iter().any(|s| s == "user from b"), "{sources:?}");
    }

    #[test]
    fn a_child_without_its_parent_says_which_chain_is_broken() {
        let t = Temp::new();
        let child = t.dir.join("child.jsonl");
        fork_log(&child, "long-gone", 7);
        let error = match Kernel::open(
            &child,
            KernelConfig::new(&t.dir),
            Box::new(ScriptedModel::new(vec![])),
        ) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("a child without its parent must refuse to open"),
        };
        assert!(error.contains("long-gone"), "{error}");
        assert!(error.contains("child.jsonl"), "names the child: {error}");
        assert!(error.contains("ADR-0014"), "and the way out: {error}");
    }

    #[test]
    fn a_cycle_in_the_lineage_is_refused() {
        let t = Temp::new();
        let a = t.dir.join("a.jsonl");
        let b = t.dir.join("b.jsonl");
        fork_log(&b, "a", 1);
        fork_log(&a, "b", 1);
        let error = match Kernel::open(
            &a,
            KernelConfig::new(&t.dir),
            Box::new(ScriptedModel::new(vec![])),
        ) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("a cyclic lineage must refuse to open"),
        };
        assert!(error.contains("cyclic"), "{error}");
    }

    #[test]
    fn the_context_view_says_what_the_next_request_will_carry() {
        let t = Temp::new();
        let (model, _seen) = RecordingModel::new(vec![response(vec![], Some("hello there"))]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        let before = loong_eventlog::replay(&t.log).unwrap().events.len();
        k.submit("hi").unwrap();

        let explained = k.context_explain();
        assert!(
            explained.context_tokens > 0,
            "the ledger accounts for something"
        );
        assert!(
            explained.messages >= 2,
            "the user turn and the answer: {}",
            explained.messages
        );
        assert!(explained.tools > 0, "the tools are described to the model");
        assert_eq!(explained.compactions, 0, "nothing has been compacted yet");

        let categories: Vec<&str> = explained
            .categories
            .iter()
            .map(|c| c.category.as_str())
            .collect();
        assert!(categories.contains(&"system_prompt"), "{categories:?}");
        assert!(categories.contains(&"history"), "{categories:?}");
        assert!(
            explained
                .categories
                .windows(2)
                .all(|w| w[0].tokens >= w[1].tokens),
            "largest first: {:?}",
            explained.categories
        );
        assert!(!explained.entries.is_empty());
        assert!(explained
            .entries
            .iter()
            .all(|e| !e.lifetime.is_empty() && !e.category.is_empty()));

        // Asking changes nothing: it is a question about the session, not a fact in it.
        let after_turn = loong_eventlog::replay(&t.log).unwrap().events.len();
        assert!(after_turn > before, "the turn itself recorded something");
        let _ = k.context_explain();
        assert_eq!(
            loong_eventlog::replay(&t.log).unwrap().events.len(),
            after_turn,
            "and reading the context added nothing"
        );
    }

    #[test]
    fn the_router_can_switch_models_and_the_log_says_why() {
        let t = Temp::new();
        // The first model answers with a tool call, so the turn needs a second model
        // call — and the rule fires on the step number.
        let (first, seen_first) = RecordingModel::new(vec![
            response(
                vec![call("c1", "read_file", json!({"path": "note.txt"}))],
                None,
            ),
            response(vec![], Some("the first model finished it")),
        ]);
        let (second, seen_second) = RecordingModel::new(vec![response(vec![], Some("second"))]);
        std::fs::write(t.dir.join("note.txt"), "hi\n").unwrap();

        let mut config = KernelConfig::new(&t.dir);
        config.settings.router = loong_config::Router {
            default: Some("first".to_string()),
            rules: vec![loong_config::RouterRule {
                condition: loong_config::parse_condition("step > 1").unwrap(),
                model: "second".to_string(),
            }],
        };
        let mut k = Kernel::create_with_models(
            &t.log,
            config,
            vec![
                ("first".to_string(), Box::new(first)),
                ("second".to_string(), Box::new(second)),
            ],
        )
        .unwrap();

        let outcome = k.submit("read the note").unwrap();
        assert_eq!(
            outcome.final_text.as_deref(),
            Some("second"),
            "the second call went to the routed model"
        );
        assert_eq!(
            seen_first.lock().unwrap().len(),
            1,
            "the first model was used once"
        );
        assert_eq!(seen_second.lock().unwrap().len(), 1);

        // The switch is a fact in the log, with the rule that caused it.
        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let switch = events
            .iter()
            .find(|e| e.kind == ev::MODEL_SELECTED)
            .expect("a model switch is recorded");
        assert_eq!(switch.data["model"], "second");
        assert_eq!(switch.data["previous"], "first");
        assert_eq!(switch.data["rule"], "step > 1");
        assert!(
            switch.data["context_tokens"].as_u64().is_some(),
            "with the fact it used"
        );
    }

    #[test]
    fn a_profile_narrows_the_ladder_and_the_rule_names_the_layer() {
        let t = Temp::new();
        std::fs::write(t.dir.join("a.txt"), "content").unwrap();
        let model = ScriptedModel::new(vec![
            response(
                vec![call("c1", "read_file", json!({"path": "a.txt"}))],
                None,
            ),
            response(vec![], Some("done")),
        ]);
        let mut config = KernelConfig::new(&t.dir);
        config.settings = settings_from(&t.dir, "[permissions]\ninvestigate = \"deny\"\n");
        let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
        k.submit("read it").unwrap();

        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let policy = events
            .iter()
            .find(|e| e.kind == ev::POLICY_DECIDED)
            .unwrap();
        assert_eq!(policy.data["verdict"], "deny");
        assert_eq!(
            policy.data["rule"], "profile:user:investigate",
            "the rule that fired names the layer that narrowed it"
        );
        let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
        assert_eq!(result.data["ok"], false);
        assert!(result.data["output"]
            .as_str()
            .unwrap()
            .contains("denied by policy"));
    }

    #[test]
    fn a_profile_can_only_narrow_never_widen() {
        let t = Temp::new();
        let model = ScriptedModel::new(vec![
            response(
                vec![call(
                    "c1",
                    "shell",
                    json!({"command": "curl http://example.invalid"}),
                )],
                None,
            ),
            response(vec![], Some("done")),
        ]);
        let mut config = KernelConfig::new(&t.dir);
        // The profile asks for out-of-bounds actions to be free. The built-in
        // baseline is `ask`, and no layer may loosen it.
        config.settings = settings_from(&t.dir, "[permissions]\nout_of_bounds = \"allow\"\n");
        let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
        let outcome = k.submit("fetch something").unwrap();
        assert!(
            outcome.pending_approval.is_some(),
            "the request still stops for a human: {:?}",
            outcome.final_text
        );
        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let policy = events
            .iter()
            .find(|e| e.kind == ev::POLICY_DECIDED)
            .unwrap();
        assert_eq!(policy.data["verdict"], "ask");
        assert_eq!(policy.data["rule"], "builtin:out_of_bounds");
    }

    #[test]
    fn narrow_only_numbers_are_the_strictest_of_the_two() {
        let t = Temp::new();
        let model = ScriptedModel::new(vec![
            response(
                vec![call("c1", "read_file", json!({"path": "a.txt"}))],
                None,
            ),
            response(
                vec![call("c2", "read_file", json!({"path": "a.txt"}))],
                None,
            ),
            response(vec![], Some("done")),
        ]);
        std::fs::write(t.dir.join("a.txt"), "x").unwrap();
        let mut config = KernelConfig::new(&t.dir);
        config.budget_tokens = Some(50_000);
        config.max_steps = 8;
        config.settings = settings_from(
            &t.dir,
            "budget_tokens = 1000\nmax_steps = 1\nkeep_newest_tokens = 100\n",
        );
        let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();

        assert_eq!(k.ledger().budget(), Some(1_000), "the config ceiling holds");
        let outcome = k.submit("go").unwrap();
        assert_eq!(outcome.steps, 1, "the config's step ceiling holds");
        let events = loong_eventlog::replay(&t.log).unwrap().events;
        assert_eq!(
            events
                .iter()
                .filter(|e| e.kind == ev::COST_RECORDED)
                .count(),
            1,
            "the model was called exactly once"
        );
    }

    #[test]
    fn the_session_may_narrow_but_not_widen_a_config_budget() {
        let t = Temp::new();
        let config_from_file = settings_from(&t.dir, "budget_tokens = 1000\n");
        // The session asks for more than config allows: the ceiling wins.
        assert_eq!(
            narrow_budget(Some(50_000), config_from_file.budget_tokens),
            Some(1_000)
        );
        // The session asks for less: it gets less.
        assert_eq!(
            narrow_budget(Some(100), config_from_file.budget_tokens),
            Some(100)
        );
        // The session asks for "unbounded": it still gets the ceiling.
        assert_eq!(
            narrow_budget(None, config_from_file.budget_tokens),
            Some(1_000)
        );
        // Nothing set anywhere: unbounded stays unbounded.
        assert_eq!(narrow_budget(None, None), None);
    }

    #[test]
    fn a_profile_can_add_skill_roots_and_disable_names_by_identity() {
        let t = Temp::new();
        // A workspace skill that config will disable, and a team root that config adds.
        install_command(&t.dir, "local-loud", "---\ndescription: noisy\n---\nbody\n");
        let team = t.dir.join("team-skills");
        std::fs::create_dir_all(team.join("from-team")).unwrap();
        std::fs::write(
            team.join("from-team/SKILL.md"),
            "---\ndescription: From the team profile\n---\nteam body\n",
        )
        .unwrap();

        let model = ScriptedModel::saying("hi");
        let mut config = KernelConfig::new(&t.dir);
        config.settings = settings_from(
            &t.dir,
            "skill_dirs = [\"team-skills\"]\ndisable_skills = [\"local-loud\"]\n",
        );
        let k = Kernel::create(&t.log, config, Box::new(model)).unwrap();

        assert!(
            k.skills().names().contains(&"from-team"),
            "{:?}",
            k.skills().names()
        );
        assert!(
            !k.skills().names().contains(&"local-loud"),
            "a disabled name is gone: {:?}",
            k.skills().names()
        );
        assert!(!k.skills().catalogue().contains("local-loud"));
        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let loaded = events.iter().find(|e| e.kind == ev::SKILLS_LOADED).unwrap();
        assert_eq!(loaded.data["disabled"], "local-loud", "{:?}", loaded.data);
    }

    #[test]
    fn a_broken_skill_file_is_fatal() {
        let t = Temp::new();
        let dir = t.dir.join(".loongagent/skills/broken");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            "no frontmatter at all
",
        )
        .unwrap();
        let model = ScriptedModel::saying("hi");
        let err = match Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)) {
            Ok(_) => panic!("a malformed skill must be fatal, not silently skipped"),
            Err(e) => e,
        };
        assert!(matches!(err, KernelError::Skills(_)), "{err}");
        assert!(err.to_string().contains("frontmatter"));
    }

    #[test]
    fn compaction_clears_stale_tool_output_before_dropping_anything() {
        let t = Temp::new();
        big_file(&t.dir, "big.txt", 400); // ~4000 chars ≈ 1000 tokens
        let (model, seen) = RecordingModel::new(vec![
            response(
                vec![call("c1", "read_file", json!({"path": "big.txt"}))],
                None,
            ),
            response(
                vec![call("c2", "read_file", json!({"path": "big.txt"}))],
                None,
            ),
            response(
                vec![call("c3", "read_file", json!({"path": "big.txt"}))],
                None,
            ),
            response(vec![], Some("read three times")),
        ]);
        let mut config = KernelConfig::new(&t.dir);
        config.budget_tokens = Some(3_000); // threshold 2400
        config.keep_newest_tokens = 800;
        let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
        k.submit("read it a few times").unwrap();

        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let compaction = events
            .iter()
            .find(|e| e.kind == ev::COMPACTION_APPLIED)
            .expect("a compaction event");
        assert!(
            compaction.data["cleared_tool_outputs"].as_u64().unwrap() >= 1,
            "{:?}",
            compaction.data
        );
        assert_eq!(
            compaction.data["dropped_messages"], 0,
            "clearing alone was enough"
        );
        assert!(
            compaction.data["tokens_after"].as_u64().unwrap()
                < compaction.data["tokens_before"].as_u64().unwrap(),
            "compaction must reduce the context: {:?}",
            compaction.data
        );

        // The model was shown a marker where the stale output used to be, and the
        // newest output stayed verbatim.
        let seen = seen.lock().unwrap();
        let last = seen.last().unwrap();
        let outputs: Vec<&String> = last
            .iter()
            .filter_map(|m| match m {
                ChatMessage::Tool { output, .. } => Some(output),
                _ => None,
            })
            .collect();
        assert!(
            outputs
                .iter()
                .any(|o| o.contains("cleared to save context")),
            "a stale tool output is cleared: {outputs:?}"
        );
        assert!(
            outputs.iter().any(|o| o.contains("line 399")),
            "the newest tool output is kept verbatim: {outputs:?}"
        );
    }

    #[test]
    fn compaction_never_splits_a_tool_pair() {
        let t = Temp::new();
        big_file(&t.dir, "big.txt", 400);
        let (model, seen) = RecordingModel::new(vec![
            response(
                vec![call("c1", "read_file", json!({"path": "big.txt"}))],
                None,
            ),
            response(
                vec![call("c2", "read_file", json!({"path": "big.txt"}))],
                None,
            ),
            response(
                vec![call("c3", "read_file", json!({"path": "big.txt"}))],
                None,
            ),
            response(vec![], Some("done")),
        ]);
        let mut config = KernelConfig::new(&t.dir);
        config.budget_tokens = Some(1_200); // tiny: forces both phases
        config.keep_newest_tokens = 200;
        let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
        k.submit("read it a few times").unwrap();

        for (turn, messages) in seen.lock().unwrap().iter().enumerate() {
            let mut answered: Vec<String> = Vec::new();
            for message in messages {
                match message {
                    ChatMessage::Assistant { tool_calls, .. } => {
                        for call in tool_calls {
                            assert!(
                                !answered.contains(&call.call_id),
                                "turn {turn}: {} was answered twice",
                                call.call_id
                            );
                            answered.push(call.call_id.clone());
                        }
                    }
                    ChatMessage::Tool { call_id, .. } => {
                        assert!(
                            answered.contains(call_id),
                            "turn {turn}: tool result {call_id} has no preceding call — the \
                             conversation would be invalid"
                        );
                    }
                    _ => {}
                }
            }
            assert!(
                !messages.is_empty(),
                "turn {turn}: the prompt was never dropped"
            );
        }
    }

    #[test]
    fn compaction_drops_the_oldest_exchange_when_clearing_is_not_enough() {
        let t = Temp::new();
        let filler = "x".repeat(2_000); // ~500 tokens per turn, no tool output to clear
        let (model, seen) = RecordingModel::new(vec![
            response(vec![], Some("first")),
            response(vec![], Some("second")),
            response(vec![], Some("third")),
        ]);
        let mut config = KernelConfig::new(&t.dir);
        config.budget_tokens = Some(1_200); // threshold 960
        config.keep_newest_tokens = 200;
        let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
        k.submit(&format!("turn one {filler}")).unwrap();
        k.submit(&format!("turn two {filler}")).unwrap();
        k.submit(&format!("turn three {filler}")).unwrap();

        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let dropped: u64 = events
            .iter()
            .filter(|e| e.kind == ev::COMPACTION_APPLIED)
            .map(|e| e.data["dropped_messages"].as_u64().unwrap_or(0))
            .sum();
        assert!(dropped >= 1, "the oldest exchange was dropped: {dropped}");

        // The prompt being answered is always still there.
        let seen = seen.lock().unwrap();
        assert!(seen.iter().all(|messages| !messages.is_empty()));
        let last = seen.last().unwrap();
        assert!(
            last.iter()
                .any(|m| matches!(m, ChatMessage::User { text } if text.contains("turn three"))),
            "the current prompt survives compaction"
        );
    }

    #[test]
    fn a_budget_that_cannot_be_met_stops_dropping_rather_than_emptying_the_prompt() {
        let t = Temp::new();
        let (model, seen) = RecordingModel::new(vec![response(vec![], Some("hi"))]);
        let mut config = KernelConfig::new(&t.dir);
        config.budget_tokens = Some(10); // absurdly small
        config.keep_newest_tokens = 5;
        let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
        k.submit("a single long turn that cannot fit").unwrap();
        let seen = seen.lock().unwrap();
        assert!(
            !seen[0].is_empty(),
            "the prompt we are answering is never dropped"
        );
    }

    #[test]
    fn a_pre_tool_hook_can_deny_what_the_policy_would_allow() {
        let t = Temp::new();
        install_hooks(
            &t.dir,
            "[[hook]]\nid = \"guard\"\nevent = \"pre_tool\"\nkind = \"shell\"\nscript = \"deny.sh\"\n",
            &[("deny.sh", "cat > /dev/null\necho 'no shell commands in this repo' >&2\nexit 2\n")],
            true,
        );
        let model = ScriptedModel::new(vec![
            response(
                vec![call("c1", "shell", json!({"command": "touch made.txt"}))],
                None,
            ),
            response(vec![], Some("understood")),
        ]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        assert!(k.hooks_note().contains("trusted"), "{}", k.hooks_note());
        k.submit("go").unwrap();

        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let fired = events
            .iter()
            .find(|e| e.kind == ev::HOOK_FIRED)
            .expect("a hook.fired event");
        assert_eq!(fired.data["hook_id"], "guard");
        assert_eq!(fired.data["verdict"], "deny");
        assert_eq!(fired.data["failed"], false);
        assert!(fired.data["reason"]
            .as_str()
            .unwrap()
            .contains("no shell commands"));

        // The classifier said write_workspace and the PDP would have allowed it:
        // the hook is what changed the outcome, and the rule names it.
        let policy = events
            .iter()
            .find(|e| e.kind == ev::POLICY_DECIDED)
            .unwrap();
        assert_eq!(policy.data["verdict"], "deny");
        assert_eq!(policy.data["rule"], "hook:guard");
        let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
        assert_eq!(result.data["ok"], false);
        assert!(
            !t.dir.join("made.txt").exists(),
            "the denied tool must never run"
        );
    }

    #[test]
    fn a_hook_that_asks_stops_the_turn_for_a_human() {
        let t = Temp::new();
        install_hooks(
            &t.dir,
            "[[hook]]\nid = \"checker\"\nevent = \"pre_tool\"\nkind = \"shell\"\nscript = \"ask.sh\"\n\n\
             [[hook]]\nid = \"seam-req\"\nevent = \"permission_requested\"\nkind = \"shell\"\nscript = \"ok.sh\"\n\n\
             [[hook]]\nid = \"seam-dec\"\nevent = \"permission_decided\"\nkind = \"shell\"\nscript = \"ok.sh\"\n\n\
             [[hook]]\nid = \"after\"\nevent = \"post_tool\"\nkind = \"shell\"\nscript = \"ok.sh\"\n",
            &[
                ("ask.sh", "cat > /dev/null\nprintf '{\"verdict\":\"ask\",\"reason\":\"double-check this\"}'\n"),
                ("ok.sh", "cat > /dev/null\nexit 0\n"),
            ],
            true,
        );
        let model = ScriptedModel::new(vec![
            response(
                vec![call("c1", "shell", json!({"command": "touch asked.txt"}))],
                None,
            ),
            response(vec![], Some("done")),
        ]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        let outcome = k.submit("go").unwrap();

        let pending = outcome
            .pending_approval
            .expect("the hook asked, so the turn stops");
        assert!(
            pending.reason.contains("double-check"),
            "{}",
            pending.reason
        );
        assert_eq!(pending.rule, "hook:checker");
        assert!(
            !t.dir.join("asked.txt").exists(),
            "nothing runs before the answer"
        );

        // The approval path is the same one the PDP uses, so answering resumes.
        let request_id = pending.request_id.clone();
        let resumed = k
            .respond_to_approval(&request_id, true, ApprovalScope::Once, None)
            .unwrap();
        assert!(resumed.pending_approval.is_none());
        assert!(t.dir.join("asked.txt").exists(), "the granted call ran");

        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
        assert!(kinds.contains(&ev::APPROVAL_REQUESTED));
        assert!(kinds.contains(&ev::APPROVAL_DECIDED));
        // Every seam fired once: pre_tool, permission_requested, permission_decided,
        // post_tool — the guards see the whole shape of the episode.
        let fired: Vec<&str> = events
            .iter()
            .filter(|e| e.kind == ev::HOOK_FIRED)
            .map(|e| e.data["event"].as_str().unwrap())
            .collect();
        assert_eq!(
            fired,
            vec![
                "pre_tool",
                "permission_requested",
                "permission_decided",
                "post_tool"
            ]
        );
    }

    #[test]
    fn an_untrusted_hook_is_inert() {
        let t = Temp::new();
        install_hooks(
            &t.dir,
            "[[hook]]\nid = \"guard\"\nevent = \"pre_tool\"\nkind = \"shell\"\nscript = \"deny.sh\"\n",
            &[("deny.sh", "cat > /dev/null\nexit 2\n")],
            false,
        );
        let model = ScriptedModel::new(vec![
            response(
                vec![call("c1", "shell", json!({"command": "touch ran.txt"}))],
                None,
            ),
            response(vec![], Some("ok")),
        ]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        assert!(k.hooks_note().contains("INERT"), "{}", k.hooks_note());
        k.submit("go").unwrap();

        let events = loong_eventlog::replay(&t.log).unwrap().events;
        assert!(
            !events.iter().any(|e| e.kind == ev::HOOK_FIRED),
            "an untrusted hook must not even be asked"
        );
        assert!(t.dir.join("ran.txt").exists(), "the call proceeded");
    }

    #[test]
    fn a_prompt_hook_refuses_the_turn_before_the_model_is_called() {
        let t = Temp::new();
        install_hooks(
            &t.dir,
            "[[hook]]\nid = \"gate\"\nevent = \"user_prompt_submit\"\nkind = \"shell\"\nscript = \"gate.sh\"\n",
            &[("gate.sh", "cat > /dev/null\necho 'this repository is read-only today' >&2\nexit 2\n")],
            true,
        );
        // An empty script: if the model were called, the turn would fail instead.
        let model = ScriptedModel::new(vec![]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        let outcome = k.submit("please refactor everything").unwrap();

        let refused = outcome.refused.expect("the hook refused the turn");
        assert!(refused.contains("read-only today"), "{refused}");
        assert_eq!(outcome.steps, 0);
        assert!(outcome.final_text.is_none());

        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
        assert!(kinds.contains(&ev::HOOK_FIRED));
        assert!(
            !kinds.contains(&ev::COST_RECORDED),
            "the model was never called: no cost, no assistant message"
        );
        let user_turns = events
            .iter()
            .filter(|e| e.kind == ev::MESSAGE_APPENDED && e.data["role"] == "user")
            .count();
        assert_eq!(
            user_turns, 1,
            "the prompt itself is still recorded as the truth"
        );
    }

    #[test]
    fn session_start_hooks_fire_when_the_session_opens() {
        let t = Temp::new();
        install_hooks(
            &t.dir,
            "[[hook]]\nid = \"boot\"\nevent = \"session_start\"\nkind = \"shell\"\nscript = \"boot.sh\"\n",
            &[("boot.sh", "cat > /dev/null\necho '{\"verdict\":\"allow\"}'\n")],
            true,
        );
        let model = ScriptedModel::saying("hi");
        let _k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let fired = events
            .iter()
            .find(|e| e.kind == ev::HOOK_FIRED)
            .expect("session_start fired");
        assert_eq!(fired.data["event"], "session_start");
        assert_eq!(fired.data["verdict"], "allow");
    }

    #[test]
    fn a_malformed_manifest_refuses_to_open_a_session() {
        let t = Temp::new();
        install_hooks(&t.dir, "not toml at all {{{", &[], false);
        let model = ScriptedModel::saying("hi");
        let err = match Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)) {
            Ok(_) => panic!("a malformed manifest must be fatal, not silently dropped"),
            Err(e) => e,
        };
        assert!(matches!(err, KernelError::Hooks(_)), "{err}");
        assert!(err.to_string().contains("hooks.toml"));
    }

    #[test]
    fn the_derived_index_answers_queries_over_the_session() {
        let t = Temp::new();
        std::fs::write(t.dir.join("a.txt"), "needle in the workspace").unwrap();
        let model = ScriptedModel::new(vec![
            response(
                vec![call("c1", "read_file", json!({"path": "a.txt"}))],
                None,
            ),
            response(vec![], Some("read it")),
        ]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        k.submit("read a.txt").unwrap();

        assert!(
            k.index_stale().is_none(),
            "a healthy session keeps a fresh index"
        );
        assert!(
            t.log.with_extension("sqlite").exists(),
            "the index sits beside the log"
        );
        let hits = k.search("needle in the workspace").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, "tool.result");
        assert!(k.search("nothing like this").unwrap().is_empty());
    }

    #[test]
    fn a_stale_index_refuses_to_answer_instead_of_lying() {
        let t = Temp::new();
        let model = ScriptedModel::saying("hi");
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        k.submit("hello").unwrap();
        // Simulate the index going bad (a write failure on a locked or full disk).
        k.index_stale = Some("disk full".to_string());
        assert!(matches!(k.search("hello"), Err(KernelError::OutOfOrder(_))));
        k.rebuild_index().unwrap();
        assert!(k.index_stale().is_none());
        assert!(
            !k.search("hello").unwrap().is_empty(),
            "rebuilt from the log alone"
        );
    }

    #[test]
    fn reopening_a_session_rebuilds_the_index_from_the_log() {
        let t = Temp::new();
        {
            let model = ScriptedModel::saying("first");
            let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
            k.submit("one").unwrap();
        }
        // Delete the index: it is derived, so the session must still work.
        std::fs::remove_file(t.log.with_extension("sqlite")).unwrap();
        let model = ScriptedModel::saying("second");
        let mut k = Kernel::open(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        assert!(k.index_stale().is_none());
        assert!(
            !k.search("one").unwrap().is_empty(),
            "the old turn is indexed again"
        );
        let out = k.submit("two").unwrap();
        assert_eq!(out.final_text.as_deref(), Some("second"));
    }

    #[test]
    fn submitting_while_an_approval_is_pending_is_refused() {
        let t = Temp::new();
        let escape = t.dir.parent().unwrap().join("loong-escape-3.txt");
        let model = ScriptedModel::new(vec![
            response(
                vec![call(
                    "c1",
                    "write_file",
                    json!({"path": escape.to_string_lossy(), "content": "x"}),
                )],
                None,
            ),
            response(vec![], Some("ok")),
        ]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        let pending = k.submit("go").unwrap().pending_approval.unwrap();
        assert!(matches!(k.submit("again"), Err(KernelError::OutOfOrder(_))));
        assert!(matches!(
            k.respond_to_approval("appr-999", false, ApprovalScope::Once, None),
            Err(KernelError::OutOfOrder(_))
        ));
        k.respond_to_approval(&pending.request_id, false, ApprovalScope::Once, None)
            .unwrap();
    }
}
