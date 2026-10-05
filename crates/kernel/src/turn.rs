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
use loong_tools::Registry;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum KernelError {
    Log(LogError),
    Model(ModelError),
    /// The caller used the API out of order (e.g. resumed with nothing pending).
    OutOfOrder(String),
    Encode(serde_json::Error),
    /// The derived index could not be built or queried (the log is unaffected).
    Index(loong_index::IndexError),
}

impl std::fmt::Display for KernelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KernelError::Log(e) => write!(f, "log: {e}"),
            KernelError::Model(e) => write!(f, "model: {e}"),
            KernelError::OutOfOrder(m) => write!(f, "out of order: {m}"),
            KernelError::Encode(e) => write!(f, "encode: {e}"),
            KernelError::Index(e) => write!(f, "index: {e}"),
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
        Ok(Self {
            log,
            ledger,
            tools: Registry::with_builtins(),
            pdp: Pdp::new(),
            model,
            config,
            messages: Vec::new(),
            pending: None,
            approvals: 0,
            index,
            index_path,
            index_stale: None,
        })
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
        let events = vec![self.record_user(text)?];
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
        if granted {
            let (ok, output) = self.execute(&pending.tool, &pending.args);
            events.push(self.record_tool_result(&pending.call_id, ok, &output)?);
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
        Ok(TurnOutcome { steps: self.config.max_steps, final_text: Some(note), pending_approval: None, events })
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
        let ruling = self.pdp.decide(class, &tool_name);

        // The attempted call, then the verdict, then whatever happened.
        events.push(self.append(
            gen::event::TOOL_CALL,
            &gen::ToolCall {
                call_id: call.call_id.clone(),
                tool: tool_name.clone(),
                args: call.args.clone(),
                action_class: class,
            },
        )?);
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
                Ok(Dispatch::Done)
            }
            Verdict::Ask => {
                let request_id = format!("appr-{}", self.approvals + 1);
                self.approvals += 1;
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

    /// The PEP we have today: the tool runner. (The OS sandbox plugs in at the
    /// same boundary; `Registry`'s write tool refuses to leave the workspace.)
    fn execute(&self, tool: &str, args: &Json) -> (bool, String) {
        match self.tools.get(tool) {
            Some(t) => match t.execute(args, &self.config.cwd) {
                Ok(out) => (true, out),
                Err(e) => (false, e.to_string()),
            },
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
