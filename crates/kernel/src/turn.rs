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
use loong_eventlog::{LogError, SessionLog};
use loong_providers::{
    ChatMessage, CompletionRequest, Model, ModelError, ToolCallRequest, Usage,
};
use loong_sandbox::Pdp;
use loong_schema::generated as gen;
use loong_schema::generated::{ActionClass, ApprovalScope, Json, Role, Verdict};
use loong_hooks::{payload, HookEvent, HookReport, HookRunner, HookSet};
use loong_sandbox::landlock::WorkspacePolicy;
use loong_tools::{Confinement, ExecContext, NoConfinement, Registry};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    pub max_steps: usize,
}

impl KernelConfig {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            profile: "default".to_string(),
            budget_tokens: None,
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
    model: Box<dyn Model>,
    config: KernelConfig,
    /// The typed conversation the model sees. Kept in step with the ledger by
    /// construction: every mutation goes through `record_*`.
    messages: Vec<ChatMessage>,
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
        Self::build(SessionLog::create(log_path)?, config, model)
    }

    pub fn open(
        log_path: impl AsRef<Path>,
        config: KernelConfig,
        model: Box<dyn Model>,
    ) -> Result<Self, KernelError> {
        Self::build(SessionLog::open(log_path)?, config, model)
    }

    fn build(
        log: SessionLog,
        config: KernelConfig,
        model: Box<dyn Model>,
    ) -> Result<Self, KernelError> {
        let ledger = Ledger::new(config.budget_tokens);
        let index_path = log.path().with_extension("sqlite");
        // Rebuild rather than attach incrementally: the index is disposable, and
        // a stale index that silently disagrees with the log is worse than a
        // few milliseconds of replay.
        let index = loong_index::Index::rebuild_into(&index_path, log.path())?;
        let mut kernel;
        // The PEP is chosen here, once, and handed to every tool call.
        let (confinement, confinement_note): (Box<dyn Confinement>, String) =
            match WorkspacePolicy::new(&config.cwd) {
                Ok(policy) => (
                    Box::new(LandlockConfinement { policy }),
                    "landlock: writes are confined to the workspace".to_string(),
                ),
                Err(e) => (
                    Box::new(NoConfinement),
                    format!("unconfined: {e}"),
                ),
            };
        // A malformed manifest stopped a guard from existing: that is fatal, not
        // a warning. Untrusted hooks load but do not run (see `HookSet::active`).
        let hook_set = HookSet::load(&config.cwd).map_err(|e| KernelError::Hooks(e.to_string()))?;
        let hooks_note = hook_set.summary();
        let hooks = HookRunner::new(&config.cwd, hook_set.active().to_vec());
        let session_start_fields = json!({"cwd": config.cwd, "profile": config.profile});
        kernel = Self {
            log,
            ledger,
            tools: Registry::with_builtins(),
            pdp: Pdp::new(),
            model,
            config,
            messages: Vec::new(),
            pending: None,
            approvals: 0,
            confinement,
            confinement_note,
            hooks,
            hooks_note,
            index,
            index_path,
            index_stale: None,
        };
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
    pub fn event_children(&self, parent_id: &str) -> Result<Vec<loong_index::EventRow>, KernelError> {
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

    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    /// Submit a user turn and run the loop until it finishes, asks, or hits the
    /// step limit.
    pub fn submit(&mut self, text: &str) -> Result<TurnOutcome, KernelError> {
        if let Some(p) = &self.pending {
            return Err(KernelError::OutOfOrder(format!(
                "approval {} is still pending",
                p.request_id
            )));
        }
        let mut events = vec![self.record_user(text)?];
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
        self.run(events)
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
        self.run(events)
    }

    // ------------------------------------------------------------- the loop

    fn run(&mut self, mut events: Vec<u64>) -> Result<TurnOutcome, KernelError> {
        for step in 1..=self.config.max_steps {
            let request = CompletionRequest {
                messages: self.messages.clone(),
                tools: self.tool_specs(),
            };
            let response = self.model.complete(&request).map_err(KernelError::Model)?;
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
            &gen::MessageAppended { role: Role::User, text: text.to_string() },
        )?;
        self.messages.push(ChatMessage::User { text: text.to_string() });
        self.ledger.add(Category::History, "user", text, Lifetime::Session, false);
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
            (Some(t), Some(_)) => format!("{t}{}", serde_json::to_string(calls).unwrap_or_default()),
            (None, Some(_)) => serde_json::to_string(calls).unwrap_or_default(),
            (None, None) => String::new(),
        };
        self.ledger.add(Category::History, "assistant", &accounting, Lifetime::Session, false);
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
            &gen::ToolResult { call_id: call_id.to_string(), ok, output: output.to_string() },
        )?;
        self.ledger.add(
            Category::History,
            format!("tool:{call_id}"),
            output,
            Lifetime::Session,
            false,
        );
        self.messages.push(ChatMessage::Tool { call_id: call_id.to_string(), output: output.to_string() });
        Ok(seq)
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
            .map(|(name, description, parameters)| loong_providers::ToolSpec {
                name,
                description,
                parameters,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loong_providers::{CompletionResponse, ScriptedModel};
    use loong_schema::generated::event as ev;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

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
            loong_hooks::HookSet::load(dir).unwrap().trust_now().unwrap();
        }
    }

    fn call(id: &str, tool: &str, args: serde_json::Value) -> ToolCallRequest {
        ToolCallRequest { call_id: id.into(), tool: tool.into(), args }
    }

    fn response(calls: Vec<ToolCallRequest>, text: Option<&str>) -> CompletionResponse {
        CompletionResponse {
            text: text.map(|t| t.to_string()),
            tool_calls: calls,
            usage: Usage { tokens_in: 7, tokens_out: 3 },
        }
    }

    fn kinds(log: &Path) -> Vec<String> {
        loong_eventlog::replay(log).unwrap().events.into_iter().map(|e| e.kind).collect()
    }

    #[test]
    fn a_tool_runs_and_the_turn_finishes() {
        let t = Temp::new();
        std::fs::write(t.dir.join("hello.txt"), "hi from the workspace").unwrap();
        let model = ScriptedModel::new(vec![
            response(vec![call("c1", "read_file", json!({"path": "hello.txt"}))], None),
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
        let policy = events.iter().find(|e| e.kind == ev::POLICY_DECIDED).unwrap();
        assert_eq!(policy.data["verdict"], "allow");
        assert_eq!(policy.data["rule"], "builtin:write_workspace");
        let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
        assert_eq!(result.data["ok"], true);
        assert!(result.data["output"].as_str().unwrap().contains("hi from the workspace"));
    }

    #[test]
    fn an_out_of_bounds_write_asks_then_resumes_on_the_answer() {
        let t = Temp::new();
        let escape = t.dir.parent().unwrap().join("loong-escape.txt");
        let _ = std::fs::remove_file(&escape);
        let model = ScriptedModel::new(vec![
            response(
                vec![call("c1", "write_file", json!({"path": escape.to_string_lossy(), "content": "x"}))],
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

        let resumed = k.respond_to_approval(&pending.request_id, false, ApprovalScope::Once, Some("no"))
            .unwrap();
        assert_eq!(resumed.final_text.as_deref(), Some("understood"));
        assert!(!escape.exists());
        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let decided = events.iter().find(|e| e.kind == ev::APPROVAL_DECIDED).unwrap();
        assert_eq!(decided.data["granted"], false);
        assert_eq!(decided.data["scope"], "once");
        assert_eq!(decided.data["reason"], "no");
        let denied = events.iter().filter(|e| e.kind == ev::TOOL_RESULT).next().unwrap();
        assert_eq!(denied.data["ok"], false);
    }

    #[test]
    fn a_granted_approval_still_cannot_escape_the_workspace() {
        let t = Temp::new();
        let escape = t.dir.parent().unwrap().join("loong-escape-2.txt");
        let _ = std::fs::remove_file(&escape);
        let model = ScriptedModel::new(vec![
            response(
                vec![call("c1", "write_file", json!({"path": escape.to_string_lossy(), "content": "x"}))],
                None,
            ),
            response(vec![], Some("ok")),
        ]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        let pending = k.submit("write outside").unwrap().pending_approval.unwrap();
        k.respond_to_approval(&pending.request_id, true, ApprovalScope::Once, None).unwrap();
        assert!(!escape.exists(), "the tool is the second line of defence");
        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
        assert_eq!(result.data["ok"], false);
        assert!(result.data["output"].as_str().unwrap().contains("outside the workspace"));
    }

    #[test]
    fn a_tightened_policy_denies_without_asking() {
        let t = Temp::new();
        let model = ScriptedModel::new(vec![
            response(vec![call("c1", "read_file", json!({"path": "/etc/hostname"}))], None),
            response(vec![], Some("could not read that")),
        ]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        k.pdp_mut().tighten(ActionClass::OutOfBounds, Verdict::Deny).unwrap();
        let out = k.submit("read /etc/hostname").unwrap();
        assert!(out.pending_approval.is_none(), "deny never asks");
        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let policy = events.iter().find(|e| e.kind == ev::POLICY_DECIDED).unwrap();
        assert_eq!(policy.data["verdict"], "deny");
        assert_eq!(policy.data["rule"], "session:out_of_bounds");
        let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
        assert!(result.data["output"].as_str().unwrap().contains("denied by policy"));
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
        assert!(result.data["output"].as_str().unwrap().contains("unknown tool"));
    }

    #[test]
    fn the_step_limit_stops_a_spinning_model() {
        let t = Temp::new();
        std::fs::write(t.dir.join("a.txt"), "x").unwrap();
        let spin = |_: usize| response(vec![call("c", "read_file", json!({"path": "a.txt"}))], None);
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
            response(vec![call("c1", "read_file", json!({"path": "a.txt"}))], None),
            response(vec![], Some("done")),
        ]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        k.submit("go").unwrap();
        let history = k
            .ledger()
            .entries()
            .iter()
            .filter(|e| e.category == Category::History)
            .count();
        assert_eq!(history, k.messages().len(), "every message is a ledger entry");
        assert!(k.ledger().explain().iter().any(|r| r.source == "user"));
    }

    #[test]
    fn the_pep_stops_an_escape_the_classifier_cannot_see() {
        let t = Temp::new();
        let escaped_name = format!("{}-escaped.txt", t.dir.file_name().unwrap().to_string_lossy());
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
        let policy = events.iter().find(|e| e.kind == ev::POLICY_DECIDED).unwrap();
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
            response(vec![call("c1", "shell", json!({"command": "touch inside-control.txt"}))], None),
            response(vec![], Some("done")),
        ]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        k.submit("write a file inside").unwrap();
        assert!(t.dir.join("inside-control.txt").exists(), "the allowed write happened");
        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
        assert_eq!(result.data["ok"], true, "{:?}", result.data["output"]);
    }

    #[test]
    fn a_read_only_shell_command_is_an_investigate_action() {
        let t = Temp::new();
        std::fs::write(t.dir.join("a.txt"), "content here").unwrap();
        let model = ScriptedModel::new(vec![
            response(vec![call("c1", "shell", json!({"command": "cat a.txt"}))], None),
            response(vec![], Some("read")),
        ]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        k.submit("show me a.txt").unwrap();
        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let policy = events.iter().find(|e| e.kind == ev::POLICY_DECIDED).unwrap();
        assert_eq!(policy.data["action_class"], "investigate");
        assert_eq!(policy.data["verdict"], "allow");
        let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
        assert!(result.data["output"].as_str().unwrap().contains("content here"));
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
            response(vec![call("c1", "shell", json!({"command": "touch made.txt"}))], None),
            response(vec![], Some("understood")),
        ]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        assert!(k.hooks_note().contains("trusted"), "{}", k.hooks_note());
        k.submit("go").unwrap();

        let events = loong_eventlog::replay(&t.log).unwrap().events;
        let fired = events.iter().find(|e| e.kind == ev::HOOK_FIRED).expect("a hook.fired event");
        assert_eq!(fired.data["hook_id"], "guard");
        assert_eq!(fired.data["verdict"], "deny");
        assert_eq!(fired.data["failed"], false);
        assert!(fired.data["reason"].as_str().unwrap().contains("no shell commands"));

        // The classifier said write_workspace and the PDP would have allowed it:
        // the hook is what changed the outcome, and the rule names it.
        let policy = events.iter().find(|e| e.kind == ev::POLICY_DECIDED).unwrap();
        assert_eq!(policy.data["verdict"], "deny");
        assert_eq!(policy.data["rule"], "hook:guard");
        let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
        assert_eq!(result.data["ok"], false);
        assert!(!t.dir.join("made.txt").exists(), "the denied tool must never run");
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
            response(vec![call("c1", "shell", json!({"command": "touch asked.txt"}))], None),
            response(vec![], Some("done")),
        ]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        let outcome = k.submit("go").unwrap();

        let pending = outcome.pending_approval.expect("the hook asked, so the turn stops");
        assert!(pending.reason.contains("double-check"), "{}", pending.reason);
        assert_eq!(pending.rule, "hook:checker");
        assert!(!t.dir.join("asked.txt").exists(), "nothing runs before the answer");

        // The approval path is the same one the PDP uses, so answering resumes.
        let request_id = pending.request_id.clone();
        let resumed = k.respond_to_approval(&request_id, true, ApprovalScope::Once, None).unwrap();
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
            vec!["pre_tool", "permission_requested", "permission_decided", "post_tool"]
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
            response(vec![call("c1", "shell", json!({"command": "touch ran.txt"}))], None),
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
        assert_eq!(user_turns, 1, "the prompt itself is still recorded as the truth");
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
        let fired = events.iter().find(|e| e.kind == ev::HOOK_FIRED).expect("session_start fired");
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
            response(vec![call("c1", "read_file", json!({"path": "a.txt"}))], None),
            response(vec![], Some("read it")),
        ]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        k.submit("read a.txt").unwrap();

        assert!(k.index_stale().is_none(), "a healthy session keeps a fresh index");
        assert!(t.log.with_extension("sqlite").exists(), "the index sits beside the log");
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
        assert!(!k.search("hello").unwrap().is_empty(), "rebuilt from the log alone");
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
        assert!(!k.search("one").unwrap().is_empty(), "the old turn is indexed again");
        let out = k.submit("two").unwrap();
        assert_eq!(out.final_text.as_deref(), Some("second"));
    }

    #[test]
    fn submitting_while_an_approval_is_pending_is_refused() {
        let t = Temp::new();
        let escape = t.dir.parent().unwrap().join("loong-escape-3.txt");
        let model = ScriptedModel::new(vec![
            response(vec![call("c1", "write_file", json!({"path": escape.to_string_lossy(), "content": "x"}))], None),
            response(vec![], Some("ok")),
        ]);
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        let pending = k.submit("go").unwrap().pending_approval.unwrap();
        assert!(matches!(k.submit("again"), Err(KernelError::OutOfOrder(_))));
        assert!(matches!(
            k.respond_to_approval("appr-999", false, ApprovalScope::Once, None),
            Err(KernelError::OutOfOrder(_))
        ));
        k.respond_to_approval(&pending.request_id, false, ApprovalScope::Once, None).unwrap();
    }
}
