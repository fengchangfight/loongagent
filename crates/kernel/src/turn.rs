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

use self::build::KernelMcp;
use self::recall::{recall_parameters, RECALL_DESCRIPTION, RECALL_TOOL};
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

mod build;
mod compaction;
mod confinement;
mod events;
mod hooks;
mod r#loop;
mod model;
mod nudge;
mod recall;
mod session;
#[cfg(test)]
mod tests;

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
