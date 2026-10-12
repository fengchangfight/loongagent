//! Terminal client (ADR-0009): renders the event stream and sends intents.
//!
//! It holds **no agent logic** — no routing, no permissions, no context
//! assembly. Everything it shows arrived as an event, which is what keeps "one
//! brain, many faces" true instead of aspirational: the same rendering works for
//! the TUI, the web client, and an IDE, because all three receive the same
//! stream.

use loong_protocol::{Notification, Request, Response};
use loong_schema::generated as gen;
use loong_schema::generated::{ApprovalScope, Json};
use serde_json::{json, Value};

/// Whatever carries the lines: a child process's pipes, a socket, a test double.
pub trait Transport {
    fn send(&mut self, line: &str) -> Result<(), String>;
    fn recv_line(&mut self) -> Result<Option<String>, String>;
}

pub struct Client<T: Transport> {
    transport: T,
    session_id: String,
    next_id: u64,
    pending: Option<PendingDecision>,
    /// The `tool.call` seen most recently, kept so an approval can name the call it is about.
    last_tool_call: Option<Value>,
    /// Whether the last submitted prompt was accepted (a hook can refuse one).
    last_accepted: bool,
}

/// A decision the session is waiting for, with the detail needed to make one.
///
/// Assembled from the events as they are parsed rather than by whoever asked, because a
/// question can be met in two places: a submit that stops for it, and a **resume** after the
/// previous answer, which is where the next one is usually met. Only the first of those had a
/// caller assembling this, so the second question arrived with no detail at all — the panel
/// said "waiting for the details" about details the connection already had.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingDecision {
    pub request_id: String,
    /// The action class that needs a verdict.
    pub class: String,
    /// Why the policy asked, in its own words.
    pub reason: String,
    /// The tool the call is on, and its arguments: what is about to happen.
    pub tool: String,
    pub args: String,
}

/// Presentation only: turn one event into a line for a human.
///
/// Free rather than a method so a caller can render **inside** the notification
/// callback — which is how a surface shows work while it is still happening. Long tool
/// output is summarised here; [`render_event_full`] is what `/expand` asks for.
pub fn render_event(envelope: &Value) -> Option<String> {
    render_event_with(envelope, false)
}

/// The same event, with nothing summarised.
pub fn render_event_full(envelope: &Value) -> Option<String> {
    render_event_with(envelope, true)
}

fn render_event_with(envelope: &Value, full: bool) -> Option<String> {
    let kind = envelope.get("kind")?.as_str()?;
    let data = envelope.get("data").cloned().unwrap_or(Value::Null);
    match kind {
        gen::event::MESSAGE_APPENDED => {
            let role = data.get("role").and_then(|r| r.as_str()).unwrap_or("?");
            let text = data.get("text").and_then(|t| t.as_str()).unwrap_or("");
            match role {
                "assistant" if !text.is_empty() => Some(text.to_string()),
                "user" => Some(format!("you: {text}")),
                _ => None,
            }
        }
        // What the tool is doing, in the words of its own argument: a path, a
        // command, a pattern. Raw JSON buries the one thing worth reading.
        gen::event::TOOL_CALL => {
            let tool = data.get("tool").and_then(|t| t.as_str()).unwrap_or("?");
            let args = data.get("args").cloned().unwrap_or(Value::Null);
            Some(call_text(tool, &args))
        }
        gen::event::TOOL_RESULT => {
            let ok = data.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
            let output = data.get("output").and_then(|o| o.as_str()).unwrap_or("");
            Some(if full {
                format!("← {} {output}", if ok { "ok" } else { "failed" })
            } else {
                collapse_result(ok, output)
            })
        }
        gen::event::TURN_INTERRUPTED => Some(format!(
            "⏹ interrupted — stopped by {}",
            if data.get("reason").and_then(|r| r.as_str()) == Some("interrupted") {
                "you"
            } else {
                "a limit"
            }
        )),
        gen::event::POLICY_DECIDED => Some(format!(
            "· policy: {} by {}",
            data.get("verdict").and_then(|v| v.as_str()).unwrap_or("?"),
            data.get("rule").and_then(|v| v.as_str()).unwrap_or("?")
        )),
        gen::event::APPROVAL_REQUESTED => {
            let request_id = data
                .get("request_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            Some(format!(
                "! approval required ({}): {} [{}]",
                data.get("action_class")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?"),
                data.get("reason").and_then(|v| v.as_str()).unwrap_or(""),
                request_id
            ))
        }
        gen::event::APPROVAL_DECIDED => Some(format!(
            "· approval {} ({})",
            if data
                .get("granted")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                "granted"
            } else {
                "denied"
            },
            data.get("scope").and_then(|v| v.as_str()).unwrap_or("?")
        )),
        gen::event::SKILLS_LOADED => Some(format!(
            "· skills available ({}): {}",
            data.get("count").and_then(|v| v.as_u64()).unwrap_or(0),
            data.get("names").and_then(|v| v.as_str()).unwrap_or("")
        )),
        gen::event::SKILL_INVOKED => {
            let name = data.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            if let Some(error) = data.get("error").and_then(|v| v.as_str()) {
                Some(format!("! /{name}: {error}"))
            } else {
                let args = data.get("args").and_then(|v| v.as_str()).unwrap_or("");
                Some(format!(
                    "· /{name} {args} ({} tokens from {})",
                    data.get("tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                    data.get("source").and_then(|v| v.as_str()).unwrap_or("?")
                ))
            }
        }
        gen::event::HOOK_FIRED => {
            let id = data.get("hook_id").and_then(|v| v.as_str()).unwrap_or("?");
            let fired = data.get("event").and_then(|v| v.as_str()).unwrap_or("?");
            let verdict = data
                .get("verdict")
                .and_then(|v| v.as_str())
                .unwrap_or("allow");
            let reason = data.get("reason").and_then(|v| v.as_str()).unwrap_or("");
            let failed = data
                .get("failed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            match verdict {
                "allow" => Some(format!("· hook {id} allowed ({fired})")),
                _ => Some(format!(
                    "! hook {id} {verdict}{} on {fired}: {reason}",
                    if failed { " (could not answer)" } else { "" }
                )),
            }
        }
        // A model change is never silent. It is the one event whose whole purpose is
        // that a person can see which model is answering — and, when a rule did it,
        // which rule, so "why did this suddenly cost more?" has an answer on screen
        // rather than only in the log.
        gen::event::MODEL_SELECTED => {
            let model = data.get("model").and_then(|v| v.as_str()).unwrap_or("?");
            let previous = data.get("previous").and_then(|v| v.as_str()).unwrap_or("");
            let rule = data.get("rule").and_then(|v| v.as_str()).unwrap_or("");
            Some(if previous.is_empty() {
                format!("⇄ model {model} ({rule})")
            } else {
                format!("⇄ model {previous} → {model} ({rule})")
            })
        }
        gen::event::COST_RECORDED => Some(format!(
            "· tokens {} in / {} out ({})",
            data.get("tokens_in").and_then(|v| v.as_u64()).unwrap_or(0),
            data.get("tokens_out").and_then(|v| v.as_u64()).unwrap_or(0),
            data.get("profile").and_then(|v| v.as_str()).unwrap_or("?")
        )),
        gen::event::COMPACTION_APPLIED => Some(format!(
            "· compacted: {} -> {} tokens (cleared {} tool output(s), dropped {} message(s), \
             newest {} kept)",
            data.get("tokens_before")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            data.get("tokens_after")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            data.get("cleared_tool_outputs")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            data.get("dropped_messages")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            data.get("kept_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
        )),
        other => Some(format!("· {other}")),
    }
}

/// Where a fold begins and what points at it. Shared with the interface that draws the
/// markers, so "is this line a fold?" cannot drift away from "does this result get
/// folded?" — the two questions have to have the same answer or the keyboard would land
/// on lines that are not there.
pub const FOLD_PREFIX: &str = "  … ";
pub const FOLD_POINTER: &str = "· /expand";

/// The line a summarised result ends with.
pub fn is_fold_marker(line: &str) -> bool {
    line.starts_with(FOLD_PREFIX) && line.ends_with(FOLD_POINTER)
}

/// Whether this rendered result was summarised (and so has a marker to move to).
pub fn folded_output(text: &str) -> bool {
    text.lines().any(is_fold_marker)
}

/// How many lines of a tool result the transcript keeps before folding the rest away.
///
/// It was three, which is enough to see that a command ran and not enough to see what it
/// said. The fold pointer is always there for the rest, so the only cost of a larger window
/// is scrollback — and a reader watching a turn would rather scroll than squint.
const RESULT_ROWS: usize = 10;

/// How wide a kept line may be. A line wider than this is clipped, and the fold marker says
/// so rather than claiming zero lines were hidden.
const RESULT_COLUMNS: usize = 160;

/// A tool result, bounded. A chatty command must not turn the transcript into a wall of
/// text, so a long one is summarised with the count and the way to see the rest.
fn collapse_result(ok: bool, output: &str) -> String {
    let verdict = if ok { "ok" } else { "failed" };
    let lines: Vec<&str> = output.lines().collect();
    let shown: Vec<String> = lines
        .iter()
        .take(RESULT_ROWS)
        .map(|line| truncate(line, RESULT_COLUMNS))
        .collect();
    let clipped = lines
        .iter()
        .take(RESULT_ROWS)
        .zip(shown.iter())
        .any(|(raw, clipped)| raw.chars().count() > clipped.chars().count());
    let mut text = format!("← {verdict} {}", shown.first().cloned().unwrap_or_default());
    for line in shown.iter().skip(1) {
        text.push('\n');
        text.push_str("  ");
        text.push_str(line);
    }
    // Say what is actually missing. Testing "was anything cut?" rather than "were lines
    // hidden?" is what stops `… 0 more line(s)` — a pointer that opened nothing — from
    // appearing whenever a single line was clipped.
    let hidden = lines.len().saturating_sub(RESULT_ROWS);
    if hidden > 0 {
        text.push('\n');
        text.push_str(&format!(
            "{FOLD_PREFIX}{hidden} more line(s), {} {FOLD_POINTER}",
            human_bytes(output.len())
        ));
    } else if clipped {
        text.push('\n');
        text.push_str(&format!(
            "{FOLD_PREFIX}line(s) clipped to {RESULT_COLUMNS} columns, {} {FOLD_POINTER}",
            human_bytes(output.len())
        ));
    }
    text
}

/// How many lines one tool call may put on screen: what it *is*, plus what it carries.
const CALL_ROWS: usize = 8;
/// How wide a call's line may be before it is clipped. Commands are read, not parsed.
const CALL_COLUMNS: usize = 160;

/// A tool call, in the words of its own arguments.
///
/// The first line says what the call **is** — a command, a path, a pattern. What it
/// *carries* follows, because `→ write_file game.html` tells a reader nothing about the
/// game being written, and "what did it just do to my file?" is the question a transcript
/// should be able to answer. Added text is marked the way a diff marks it, so an edit reads
/// as a change rather than as two blocks of text.
///
/// The elision note deliberately does **not** look like a fold pointer: folds are tool
/// *results*, numbered from the log, and a marker here would put the keyboard on a line the
/// log cannot answer for.
fn call_text(tool: &str, args: &Value) -> String {
    let arg = |key: &str| args.get(key).and_then(|v| v.as_str()).unwrap_or("");
    let primary = [
        "command", "path", "pattern", "query", "url", "name", "prompt",
    ]
    .iter()
    .map(|key| arg(key))
    .find(|value| !value.is_empty())
    .map(|value| value.to_string())
    .unwrap_or_else(|| truncate(&args.to_string(), 60));
    let mut rows: Vec<String> = primary
        .lines()
        .enumerate()
        .map(|(at, line)| {
            let indent = if at == 0 { "" } else { "  " };
            format!("{indent}{}", truncate(line, CALL_COLUMNS))
        })
        .collect();
    if rows.is_empty() {
        rows.push(String::new());
    }
    for (sign, key) in [('-', "old_string"), ('+', "new_string"), ('+', "content")] {
        let value = arg(key);
        if !value.is_empty() {
            rows.extend(
                value
                    .lines()
                    .map(|line| format!("  {sign} {}", truncate(line, CALL_COLUMNS - 4))),
            );
        }
    }
    let hidden = rows.len().saturating_sub(CALL_ROWS);
    rows.truncate(CALL_ROWS);
    let mut text = format!("→ {tool} {}", rows.remove(0));
    for row in rows {
        text.push('\n');
        text.push_str(&row);
    }
    if hidden > 0 {
        text.push('\n');
        text.push_str(&format!("  ⋯ {hidden} more line(s) of the call"));
    }
    text
}

fn human_bytes(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    }
}

/// Turns a stream of events into lines, remembering the one timing a person cares about:
/// how long a tool took. The envelopes carry `time_ms`, so the answer comes from the
/// record rather than from a stopwatch in the interface.
#[derive(Default)]
pub struct Renderer {
    last_tool_call_ms: Option<u64>,
}

impl Renderer {
    pub fn render(&mut self, envelope: &Value) -> Option<String> {
        let kind = envelope.get("kind").and_then(|k| k.as_str())?;
        let at = envelope.get("time_ms").and_then(|v| v.as_u64());
        if kind == gen::event::TOOL_CALL {
            self.last_tool_call_ms = at;
            return render_event(envelope);
        }
        if kind != gen::event::TOOL_RESULT {
            return render_event(envelope);
        }
        let base = render_event(envelope)?;
        let took = match (self.last_tool_call_ms.take(), at) {
            (Some(start), Some(end)) if end >= start => Some(end - start),
            _ => None,
        };
        match took {
            // On the first line: the summary line below already ends with a pointer to
            // `/expand`, and a duration after that reads like part of the pointer.
            Some(ms) => {
                let mut lines = base.split('\n');
                let first = lines.next().unwrap_or_default();
                let rest: Vec<&str> = lines.collect();
                Some(if rest.is_empty() {
                    format!("{first}  ({})", human_ms(ms))
                } else {
                    format!("{first}  ({})\n{}", human_ms(ms), rest.join("\n"))
                })
            }
            None => Some(base),
        }
    }
}

fn human_ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

/// Ask the server process to stop the turn it is running. The process is our own child,
/// so this is a signal rather than a protocol message: the client is blocked reading the
/// turn's own stream, and a signal does not need the transport to be free.
pub fn interrupt(pid: u32) {
    // Safety: `kill` with a signal number on a pid we spawned; the failure mode is a
    // process that has already exited, which the return value reports and nothing needs.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGINT);
    }
}

impl<T: Transport> Client<T> {
    pub fn new(transport: T, session_id: impl Into<String>) -> Self {
        Self {
            transport,
            session_id: session_id.into(),
            next_id: 1,
            pending: None,
            last_tool_call: None,
            last_accepted: true,
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn pending_approval(&self) -> Option<&str> {
        self.pending
            .as_ref()
            .map(|pending| pending.request_id.as_str())
    }

    /// The same question with the detail a person needs to answer it — however it was met.
    pub fn pending_decision(&self) -> Option<&PendingDecision> {
        self.pending.as_ref()
    }

    /// False when a `user_prompt_submit` hook refused the turn. The reason
    /// arrived as a `hook.fired` event in the same stream.
    pub fn last_accepted(&self) -> bool {
        self.last_accepted
    }

    pub fn into_transport(self) -> T {
        self.transport
    }

    pub fn attach(&mut self, cwd: &str, client: &str) -> Result<gen::SessionAttachResult, String> {
        let params = gen::SessionAttachParams {
            cwd: cwd.to_string(),
            client: client.to_string(),
        };
        let (_, result) = self.call(
            gen::method::SESSION_ATTACH,
            serde_json::to_value(params).unwrap(),
        )?;
        let attached: gen::SessionAttachResult =
            serde_json::from_value(result).map_err(|e| format!("attach result: {e}"))?;
        self.session_id = attached.session_id.clone();
        Ok(attached)
    }

    /// The most recent tool result, in full — what `/expand` shows. It comes from the
    /// session's log through `session.replay`, so the summary in the transcript is never
    /// the only copy of anything.
    pub fn expand_last_tool_result(&mut self) -> Result<Vec<String>, String> {
        let results = self.tool_results()?;
        Ok(
            match results.last().and_then(|event| render_event_full(event)) {
                Some(text) => text.split('\n').map(|line| line.to_string()).collect(),
                None => Vec::new(),
            },
        )
    }

    /// Every **summarised** tool result in this session's log, in full, oldest first.
    ///
    /// This is the list both sides count. The transcript names a fold by where it sits
    /// among the others — the keyboard walks the markers, and the marker's place is what
    /// gets asked for — so the log has to agree on that order, and this is the one
    /// definition of it. Folding is the client's own decision (`folded_output`), so which
    /// results are on the list cannot drift from which lines get a marker.
    pub fn folded_results(&mut self) -> Result<Vec<String>, String> {
        Ok(self
            .tool_results()?
            .iter()
            .filter(|event| render_event(event).is_some_and(|text| folded_output(&text)))
            .filter_map(render_event_full)
            .collect())
    }

    /// One summarised tool result in full, by its place counting from the **oldest** fold,
    /// which is the order the transcript draws them in.
    ///
    /// From the oldest rather than from the newest on purpose: the log only grows, so
    /// "the third fold" means the same result before and after the next tool runs. A count
    /// from the end would quietly point at a different result the moment the turn produced
    /// another one — which is exactly what happens to a request made while a turn is
    /// running and answered after it.
    pub fn expand_tool_result(&mut self, at: usize) -> Result<Vec<String>, String> {
        Ok(match self.folded_results()?.get(at) {
            Some(text) => text.split('\n').map(|line| line.to_string()).collect(),
            None => Vec::new(),
        })
    }

    /// Every tool result this session's log holds, oldest first.
    fn tool_results(&mut self) -> Result<Vec<Value>, String> {
        let params = gen::SessionOnlyParams {
            session_id: self.session_id.clone(),
        };
        let (_, result) = self.call(
            gen::method::SESSION_REPLAY,
            serde_json::to_value(params).unwrap(),
        )?;
        let replay: gen::SessionReplayResult =
            serde_json::from_value(result).map_err(|e| format!("session.replay: {e}"))?;
        Ok(replay
            .events
            .into_iter()
            .filter(|event| {
                event.get("kind").and_then(|k| k.as_str()) == Some(gen::event::TOOL_RESULT)
            })
            .collect())
    }

    /// Turn this session's approval prompts off or on. The sandbox is a different axis and
    /// is untouched (ADR-0007 §5).
    pub fn set_auto_approve(&mut self, on: bool) -> Result<bool, String> {
        let params = gen::PermissionsSetParams {
            session_id: self.session_id.clone(),
            auto_approve: on,
        };
        let (_, result) = self.call(
            gen::method::PERMISSIONS_SET,
            serde_json::to_value(params).unwrap(),
        )?;
        let state: gen::PermissionsState =
            serde_json::from_value(result).map_err(|e| format!("permissions.set: {e}"))?;
        Ok(state.auto_approve)
    }

    /// The session so far, rendered exactly the way live events are. Attaching to a
    /// session that already has a past should show that past; an empty screen that is
    /// quietly appending to yesterday's conversation is worse than useless.
    pub fn replay(&mut self) -> Result<Vec<String>, String> {
        let params = gen::SessionOnlyParams {
            session_id: self.session_id.clone(),
        };
        let (_, result) = self.call(
            gen::method::SESSION_REPLAY,
            serde_json::to_value(params).unwrap(),
        )?;
        let replay: gen::SessionReplayResult =
            serde_json::from_value(result).map_err(|e| format!("session.replay: {e}"))?;
        Ok(replay
            .events
            .iter()
            .filter_map(|event| render_event(event))
            .collect())
    }

    /// What the model is about to be sent, and why. Read-only: the kernel records
    /// nothing for it, so a debug panel cannot alter the thing it is showing.
    pub fn context(&mut self) -> Result<gen::ContextExplain, String> {
        let params = gen::SessionOnlyParams {
            session_id: self.session_id.clone(),
        };
        let (_, result) = self.call(
            gen::method::CONTEXT_EXPLAIN,
            serde_json::to_value(params).unwrap(),
        )?;
        serde_json::from_value(result).map_err(|e| format!("context.explain: {e}"))
    }

    /// The models this session offers, in the profile's own order, and which is
    /// current. Read-only: asking which models exist changes nothing.
    pub fn models(&mut self) -> Result<gen::ModelList, String> {
        let params = gen::SessionOnlyParams {
            session_id: self.session_id.clone(),
        };
        let (_, result) = self.call(
            gen::method::MODEL_LIST,
            serde_json::to_value(params).unwrap(),
        )?;
        serde_json::from_value(result).map_err(|e| format!("model.list: {e}"))
    }

    /// Switch the model, and get back the new state — the same shape `models()`
    /// returns, so a caller never has to ask twice to find out what happened.
    pub fn select_model(&mut self, name: &str) -> Result<gen::ModelList, String> {
        let params = gen::ModelSelectParams {
            session_id: self.session_id.clone(),
            name: name.to_string(),
        };
        let (_, result) = self.call(
            gen::method::MODEL_SELECT,
            serde_json::to_value(params).unwrap(),
        )?;
        serde_json::from_value(result).map_err(|e| format!("model.select: {e}"))
    }

    /// Submit a turn; returns the lines to display (events, then the answer).
    pub fn submit(&mut self, text: &str) -> Result<Vec<String>, String> {
        self.submit_with(text, &mut |_, _| {})
    }

    /// Submit a turn, handing every notification to `on_notification` **as it
    /// arrives** — which is how a surface shows text being written
    /// (`model.delta`) instead of only the finished message. The returned lines are
    /// unchanged, so a caller that ignores the observer behaves exactly as before.
    pub fn submit_with(
        &mut self,
        text: &str,
        on_notification: &mut dyn FnMut(&str, &Value),
    ) -> Result<Vec<String>, String> {
        let params = gen::PromptSubmitParams {
            session_id: self.session_id.clone(),
            text: text.to_string(),
        };
        let (lines, result) = self.call_with(
            gen::method::PROMPT_SUBMIT,
            serde_json::to_value(params).unwrap(),
            on_notification,
        )?;
        // A refused turn is a fact the surface must show, not swallow.
        self.last_accepted = result
            .get("accepted")
            .and_then(|a| a.as_bool())
            .unwrap_or(true);
        Ok(lines)
    }

    /// Answer a pending approval (ADR-0007 §6: the decision is an event).
    pub fn respond(
        &mut self,
        request_id: &str,
        granted: bool,
        scope: ApprovalScope,
        reason: Option<&str>,
    ) -> Result<Vec<String>, String> {
        let params = gen::ApprovalRespondParams {
            session_id: self.session_id.clone(),
            request_id: request_id.to_string(),
            granted,
            scope,
            reason: reason.map(|r| r.to_string()),
        };
        let (lines, _) = self.call_with(
            gen::method::APPROVAL_RESPOND,
            serde_json::to_value(params).unwrap(),
            &mut |_, _| {},
        )?;
        // Only *this* question is answered. A new one can be met while resuming from this
        // one — the turn continues, another tool needs a verdict — and clearing
        // unconditionally threw that question away. Nothing said so: the panel simply never
        // appeared for the second question of a session, while the transcript showed the
        // `! approval required` line for it.
        if self.pending_approval() == Some(request_id) {
            self.pending = None;
        }
        Ok(lines)
    }

    fn call(&mut self, method: &str, params: Json) -> Result<(Vec<String>, Json), String> {
        self.call_with(method, params, &mut |_, _| {})
    }

    /// Send a request and read until its response arrives, rendering every
    /// notification on the way and reporting each one raw.
    fn call_with(
        &mut self,
        method: &str,
        params: Json,
        on_notification: &mut dyn FnMut(&str, &Value),
    ) -> Result<(Vec<String>, Json), String> {
        let id = self.next_id;
        self.next_id += 1;
        let request = Request::new(id, method, params);
        self.transport.send(&request.to_line())?;

        let mut rendered = Vec::new();
        loop {
            let Some(line) = self.transport.recv_line()? else {
                return Err("the server closed the connection".to_string());
            };
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(response) = Response::parse(&line) {
                if !response.id.is_null() {
                    if let Some(error) = response.error {
                        return Err(format!("{}: {}", error.code, error.message));
                    }
                    return Ok((rendered, response.result.unwrap_or(Value::Null)));
                }
            }
            if let Ok(notification) = serde_json::from_str::<Notification>(&line) {
                // The method matters: `model.delta` carries its kind there, while an
                // `event` carries it in `params.kind`.
                on_notification(&notification.method, &notification.params);
                if notification.method == loong_protocol::EVENT_NOTIFICATION {
                    // State the connection needs to keep, which rendering does not own.
                    if notification.params.get("kind").and_then(|k| k.as_str())
                        == Some(gen::event::TOOL_CALL)
                    {
                        self.last_tool_call = notification.params.get("data").cloned();
                    }
                    if notification.params.get("kind").and_then(|k| k.as_str())
                        == Some(gen::event::APPROVAL_REQUESTED)
                    {
                        let data = notification
                            .params
                            .get("data")
                            .cloned()
                            .unwrap_or(Value::Null);
                        let call = self.last_tool_call.clone().unwrap_or(Value::Null);
                        let text = |value: &Value, key: &str| {
                            value
                                .get(key)
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string()
                        };
                        self.pending = Some(PendingDecision {
                            request_id: text(&data, "request_id"),
                            class: match text(&data, "action_class") {
                                class if class.is_empty() => "?".to_string(),
                                class => class,
                            },
                            reason: text(&data, "reason"),
                            tool: match text(&call, "tool") {
                                tool if tool.is_empty() => "?".to_string(),
                                tool => tool,
                            },
                            args: call
                                .get("args")
                                .map(|args| args.to_string())
                                .unwrap_or_default(),
                        });
                    }
                    if let Some(text) = render_event(&notification.params) {
                        rendered.push(text);
                    }
                }
                continue;
            }
            return Err(format!("unparseable line from the server: {line}"));
        }
    }
}

fn truncate(text: &str, max: usize) -> String {
    let flat = text.replace('\n', " ");
    if flat.chars().count() <= max {
        flat
    } else {
        let cut: String = flat.chars().take(max).collect();
        format!("{cut}…")
    }
}

/// A request builder for callers outside this crate (kept tiny on purpose).
pub fn intent(id: u64, method: &str, params: Json) -> String {
    Request::new(id, method, params).to_line()
}

pub fn prompt_params(session_id: &str, text: &str) -> Json {
    json!({"session_id": session_id, "text": text})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// A transport that replays canned server lines and records what the client
    /// sent — enough to test the protocol conversation without a process.
    #[derive(Default)]
    struct Fake {
        inbound: VecDeque<String>,
        outbound: Vec<String>,
    }

    impl Fake {
        fn with(lines: Vec<Value>) -> Self {
            Self {
                inbound: lines.into_iter().map(|v| v.to_string()).collect(),
                outbound: Vec::new(),
            }
        }
    }

    impl Transport for Fake {
        fn send(&mut self, line: &str) -> Result<(), String> {
            self.outbound.push(line.to_string());
            Ok(())
        }
        fn recv_line(&mut self) -> Result<Option<String>, String> {
            Ok(self.inbound.pop_front())
        }
    }

    fn event(seq: u64, kind: &str, data: Value) -> Value {
        json!({"jsonrpc": "2.0", "method": "event", "params": {"seq": seq, "id": format!("e{seq}"), "kind": kind, "ignorable": false, "time_ms": 0, "data": data}})
    }

    fn response(id: u64, result: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "result": result})
    }

    /// An event the way `session.replay` returns it: the envelope's params, not the
    /// notification wrapped around them.
    fn bare(seq: u64, kind: &str, data: Value) -> Value {
        json!({"seq": seq, "id": format!("e{seq}"), "kind": kind, "ignorable": false, "time_ms": 0, "data": data})
    }

    #[test]
    fn attach_sends_the_declared_method_and_learns_the_session() {
        let fake = Fake::with(vec![response(
            1,
            json!({"session_id": "s-1", "schema_version": gen::SCHEMA_VERSION, "existing_events": 4}),
        )]);
        let mut client = Client::new(fake, "unset");
        let attached = client.attach("/work", "tui").unwrap();
        assert_eq!(attached.session_id, "s-1");
        assert_eq!(attached.existing_events, 4);
        let sent: Value = serde_json::from_str(&client.transport.outbound[0]).unwrap();
        assert_eq!(sent["method"], "session.attach");
        assert_eq!(sent["params"]["cwd"], "/work");
    }

    #[test]
    fn a_model_switch_is_visible_and_says_what_caused_it() {
        // It used to render as nothing at all, so a routing rule could move a session
        // onto a different model — and a different price — without a word on screen.
        let switched = json!({
            "kind": gen::event::MODEL_SELECTED,
            "time_ms": 1000,
            "data": {
                "model": "strong",
                "previous": "cheap",
                "rule": "step > 1",
                "context_tokens": 10,
                "step": 2,
            },
        });
        let line = render_event(&switched).expect("a switch is drawn");
        assert!(line.contains("cheap"), "{line}");
        assert!(line.contains("strong"), "{line}");
        assert!(
            line.contains("step > 1"),
            "and the rule that did it, so 'why is this costing more?' has an answer on screen: {line}"
        );

        // A session's first call has no previous model to name.
        let first = json!({
            "kind": gen::event::MODEL_SELECTED,
            "time_ms": 1000,
            "data": {"model": "cheap", "previous": "", "rule": "the router's default"},
        });
        let line = render_event(&first).expect("a first choice is drawn");
        assert!(line.contains("cheap"), "{line}");
        assert!(
            !line.contains("→") || !line.contains("  → "),
            "with no arrow pointing from nothing: {line}"
        );
    }

    #[test]
    fn long_tool_output_is_summarised_and_expandable() {
        let noisy = (1..=200)
            .map(|n| format!("line {n} of output"))
            .collect::<Vec<_>>()
            .join("\n");
        let event = json!({
            "kind": gen::event::TOOL_RESULT,
            "time_ms": 1000,
            "data": {"ok": true, "output": noisy},
        });
        let collapsed = render_event(&event).unwrap();
        let rows: Vec<&str> = collapsed.lines().collect();
        assert_eq!(
            rows.len(),
            RESULT_ROWS + 1,
            "bounded, whatever the command printed: {rows:?}"
        );
        assert!(rows[0].starts_with("← ok line 1 of output"), "{rows:?}");
        assert!(rows[RESULT_ROWS].contains("190 more line(s)"), "{rows:?}");
        assert!(
            rows[RESULT_ROWS].contains("/expand"),
            "and says how to see them: {rows:?}"
        );
        assert!(rows[RESULT_ROWS].contains("KiB"), "{rows:?}");

        // `/expand` is the other half: the log has everything, so nothing is lost.
        let expanded = render_event_full(&event).unwrap();
        assert!(
            expanded.contains("line 200 of output"),
            "{}",
            &expanded[..80]
        );
        assert!(expanded.lines().count() > 100);

        // Lines within the window are not summarised at all: a pointer to nothing is noise,
        // and `… 0 more line(s)` is what a clipped *line* used to produce.
        let exact = (1..=RESULT_ROWS)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let fits = json!({
            "kind": gen::event::TOOL_RESULT,
            "data": {"ok": true, "output": exact},
        });
        let collapsed = render_event(&fits).unwrap();
        assert_eq!(collapsed.lines().count(), RESULT_ROWS, "{collapsed}");
        assert!(!collapsed.contains("/expand"), "{collapsed}");

        // A single too-wide line *is* cut, and the marker says which of the two happened.
        let one = json!({
            "kind": gen::event::TOOL_RESULT,
            "data": {"ok": true, "output": "x".repeat(500)},
        });
        let collapsed = render_event(&one).unwrap();
        assert!(collapsed.lines().count() <= 3, "{collapsed}");
        assert!(collapsed.contains("clipped to"), "{collapsed}");
        assert!(
            !collapsed.contains("0 more line(s)"),
            "a clipped line is not a hidden line: {collapsed}"
        );
        assert!(collapsed.contains("/expand"), "{collapsed}");

        // Short output is left exactly as it was.
        let short = json!({
            "kind": gen::event::TOOL_RESULT,
            "data": {"ok": true, "output": "hello"},
        });
        assert_eq!(render_event(&short).unwrap(), "← ok hello");
    }

    #[test]
    fn a_tool_result_says_how_long_the_tool_took() {
        let mut renderer = Renderer::default();
        let called = json!({
            "kind": gen::event::TOOL_CALL,
            "time_ms": 10_000,
            "data": {"tool": "shell", "args": {"command": "cargo test"}},
        });
        let finished = json!({
            "kind": gen::event::TOOL_RESULT,
            "time_ms": 12_400,
            "data": {"ok": true, "output": "all good"},
        });
        assert_eq!(renderer.render(&called).unwrap(), "→ shell cargo test");
        assert_eq!(renderer.render(&finished).unwrap(), "← ok all good  (2.4s)");

        // Sub-second work reads in milliseconds, and a result with no call before it is
        // left alone rather than given an invented duration.
        let quick = json!({
            "kind": gen::event::TOOL_RESULT,
            "time_ms": 10_250,
            "data": {"ok": true, "output": "hi"},
        });
        let mut renderer = Renderer::default();
        assert_eq!(renderer.render(&quick).unwrap(), "← ok hi");
    }

    #[test]
    fn a_call_shows_what_it_carries_not_just_where_it_points() {
        // An edit is a change: the transcript should read like one, because "what did it
        // just do to my file?" is the question a reader actually has.
        let edit = json!({
            "kind": gen::event::TOOL_CALL,
            "data": {"tool": "edit", "args": {
                "path": "game.html",
                "old_string": "<canvas></canvas>",
                "new_string": "<canvas id=\"c\"></canvas>\n<script>start()</script>",
            }},
        });
        assert_eq!(
            render_event(&edit).unwrap(),
            "→ edit game.html\n  \
             - <canvas></canvas>\n  \
             + <canvas id=\"c\"></canvas>\n  \
             + <script>start()</script>"
        );

        // A file being written shows the beginning of it, bounded — a page is not a
        // transcript.
        let page: Vec<String> = (1..=50).map(|n| format!("<p>{n}</p>")).collect();
        let write = json!({
            "kind": gen::event::TOOL_CALL,
            "data": {"tool": "write_file", "args": {"path": "game.html", "content": page.join("\n")}},
        });
        let rendered = render_event(&write).unwrap();
        let rows: Vec<&str> = rendered.lines().collect();
        assert_eq!(rows.len(), CALL_ROWS + 1, "{rows:?}");
        assert_eq!(rows[0], "→ write_file game.html");
        assert_eq!(rows[1], "  + <p>1</p>", "{rows:?}");
        assert!(
            rows[CALL_ROWS].contains("more line(s) of the call"),
            "{rows:?}"
        );
        // And it does not pretend to be a fold. Folds are the log's tool *results*, counted
        // from the oldest, so a marker on a call would put the keyboard on a line the log
        // cannot answer for — it would open somebody else's output.
        assert!(
            !rendered.lines().any(is_fold_marker),
            "a call is not a fold: {rendered}"
        );

        // A one-line call reads exactly as it always did.
        let shell = json!({
            "kind": gen::event::TOOL_CALL,
            "data": {"tool": "shell", "args": {"command": "cargo test"}},
        });
        assert_eq!(render_event(&shell).unwrap(), "→ shell cargo test");
    }

    #[test]
    fn a_turn_renders_the_events_then_the_answer() {
        let fake = Fake::with(vec![
            event(
                1,
                "message.appended",
                json!({"role": "user", "text": "read it"}),
            ),
            event(
                2,
                "tool.call",
                json!({"call_id": "c1", "tool": "read_file", "args": {"path": "a.txt"}, "action_class": "write_workspace"}),
            ),
            event(
                3,
                "policy.decided",
                json!({"action_class": "write_workspace", "verdict": "allow", "rule": "builtin:write_workspace"}),
            ),
            event(
                4,
                "tool.result",
                json!({"call_id": "c1", "ok": true, "output": "hello"}),
            ),
            event(
                5,
                "message.appended",
                json!({"role": "assistant", "text": "it says hello"}),
            ),
            response(2, json!({"turn_id": 1, "accepted": true})),
        ]);
        let mut client = Client::new(fake, "s-1");
        let lines = client.submit("read it").unwrap();
        assert_eq!(
            lines,
            vec![
                "you: read it".to_string(),
                // The tool's own argument, not the JSON wrapped around it.
                "→ read_file a.txt".to_string(),
                "· policy: allow by builtin:write_workspace".to_string(),
                "← ok hello".to_string(),
                "it says hello".to_string(),
            ]
        );
    }

    #[test]
    fn a_question_met_while_resuming_carries_its_detail_too() {
        // The second question of a session is usually met while *resuming* from the answer to
        // the first: the turn continues, runs another tool, and stops again. That path has no
        // submit callback, so whoever asked for the detail found none — the panel said
        // "waiting for the details" about details the connection had already parsed. The
        // client assembles it from the events instead, so both paths are the same.
        let fake = Fake::with(vec![
            event(
                1,
                "tool.call",
                json!({"call_id": "c1", "tool": "shell", "args": {"command": "ls"}}),
            ),
            event(
                2,
                "approval.requested",
                json!({"request_id": "appr-1", "action_class": "out_of_bounds", "reason": "shell: out_of_bounds is Ask"}),
            ),
            response(2, json!({"turn_id": 1, "accepted": false})),
            // The resume: another call, and the next question about it.
            event(
                3,
                "tool.call",
                json!({"call_id": "c2", "tool": "write_file", "args": {"path": "game.html", "content": "<html>"}}),
            ),
            event(
                4,
                "approval.requested",
                json!({"request_id": "appr-2", "action_class": "out_of_bounds", "reason": "write_file: out_of_bounds is Ask"}),
            ),
            response(3, json!({"turn_id": 1, "accepted": true})),
        ]);
        let mut client = Client::new(fake, "s-1");
        client.submit("go").unwrap();
        let first = client.pending_decision().expect("the first question");
        assert_eq!(first.request_id, "appr-1");
        assert_eq!(first.tool, "shell", "named by the call it is about");
        assert!(first.args.contains("ls"));
        client
            .respond("appr-1", true, gen::ApprovalScope::Once, None)
            .unwrap();
        let second = client.pending_decision().expect("the second question");
        assert_eq!(second.request_id, "appr-2");
        assert_eq!(second.class, "out_of_bounds");
        assert!(second.reason.contains("write_file"), "{second:?}");
        assert_eq!(second.tool, "write_file", "{second:?}");
        assert!(second.args.contains("game.html"), "{second:?}");
    }

    #[test]
    fn an_approval_request_is_captured() {
        let fake = Fake::with(vec![
            event(
                1,
                "approval.requested",
                json!({"request_id": "appr-1", "action_class": "out_of_bounds", "reason": "curl: out_of_bounds is Ask"}),
            ),
            response(2, json!({"turn_id": 1, "accepted": true})),
        ]);
        let mut client = Client::new(fake, "s-1");
        let lines = client.submit("go").unwrap();
        assert!(lines[0].starts_with("! approval required (out_of_bounds)"));
        assert_eq!(client.pending_approval(), Some("appr-1"));
    }

    #[test]
    fn a_command_invocation_and_an_unknown_one_are_both_visible() {
        let fake = Fake::with(vec![
            event(
                1,
                "skill.invoked",
                json!({"name": "review", "source": "/w/.loongagent/commands/review.md", "args": "HEAD~1", "tokens": 120}),
            ),
            event(
                2,
                "skill.invoked",
                json!({"name": "reveiw", "source": "<none>", "args": "", "tokens": 0, "error": "no skill named \"reveiw\"; /help lists what exists"}),
            ),
            response(3, json!({"turn_id": 1, "accepted": true})),
        ]);
        let mut client = Client::new(fake, "s-1");
        let lines = client.submit("/review HEAD~1").unwrap();
        assert!(lines[0].contains("/review HEAD~1"), "{}", lines[0]);
        assert!(lines[0].contains("120 tokens"), "{}", lines[0]);
        assert!(lines[1].starts_with("! /reveiw"), "{}", lines[1]);
    }

    #[test]
    fn a_hook_refusal_is_visible_and_marks_the_turn_unaccepted() {
        let fake = Fake::with(vec![
            event(
                1,
                "hook.fired",
                json!({"event": "user_prompt_submit", "hook_id": "gate", "kind": "shell", "verdict": "deny", "reason": "this repository is read-only today", "duration_ms": 3, "failed": false}),
            ),
            response(2, json!({"turn_id": 1, "accepted": false})),
        ]);
        let mut client = Client::new(fake, "s-1");
        let lines = client.submit("refactor everything").unwrap();
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].starts_with("! hook gate deny on user_prompt_submit"),
            "{}",
            lines[0]
        );
        assert!(lines[0].contains("read-only today"));
        assert!(!client.last_accepted());
    }

    #[test]
    fn a_hook_that_could_not_answer_says_so() {
        let fake = Fake::with(vec![
            event(
                1,
                "hook.fired",
                json!({"event": "pre_tool", "hook_id": "flaky", "kind": "http", "verdict": "deny", "reason": "request failed (on_error = deny)", "duration_ms": 1, "failed": true}),
            ),
            response(2, json!({"turn_id": 1, "accepted": true})),
        ]);
        let mut client = Client::new(fake, "s-1");
        let lines = client.submit("go").unwrap();
        assert!(lines[0].contains("could not answer"), "{}", lines[0]);
    }

    #[test]
    fn compaction_is_rendered_with_its_numbers() {
        let fake = Fake::with(vec![
            event(
                1,
                "compaction.applied",
                json!({"tokens_before": 90000, "tokens_after": 40000, "kept_tokens": 20000, "cleared_tool_outputs": 7, "dropped_messages": 2, "summary": "x"}),
            ),
            response(2, json!({"turn_id": 1, "accepted": true})),
        ]);
        let mut client = Client::new(fake, "s-1");
        let lines = client.submit("go").unwrap();
        assert!(lines[0].contains("90000 -> 40000 tokens"), "{}", lines[0]);
        assert!(
            lines[0].contains("cleared 7 tool output(s)"),
            "{}",
            lines[0]
        );
    }

    #[test]
    fn a_fold_is_found_by_its_place_in_the_log_not_by_its_distance_from_the_end() {
        // Three results: a short one, then two the transcript summarised. The short one
        // has no marker on screen, so it is not one of the ones a reader can walk to.
        let noisy = |tag: &str| {
            (1..=50)
                .map(|n| format!("{tag} {n}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let replay = || {
            response(
                1,
                json!({"events": [
                    bare(1, "tool.result", json!({"call_id": "c1", "ok": true, "output": noisy("older")})),
                    bare(2, "tool.result", json!({"call_id": "c2", "ok": true, "output": noisy("newest")})),
                    bare(3, "tool.result", json!({"call_id": "c3", "ok": true, "output": "short"})),
                ]}),
            )
        };
        let fake = Fake::with(vec![replay(), replay(), replay(), replay()]);
        let mut client = Client::new(fake, "s-1");
        // The two folded ones are what the transcript draws a marker for, in this order.
        let older = client.expand_tool_result(0).unwrap();
        assert!(older[0].contains("older 1"), "{:?}", older[0]);
        assert!(older.iter().any(|line| line.contains("older 50")));
        let newest = client.expand_tool_result(1).unwrap();
        assert!(newest[0].contains("newest 1"), "{:?}", newest[0]);
        // Past the end there is nothing to name, and `/expand` still means the last result
        // of all — the short one, which is what it says on the tin.
        assert!(client.expand_tool_result(2).unwrap().is_empty());
        assert_eq!(
            client.expand_last_tool_result().unwrap(),
            vec!["← ok short"]
        );
    }

    #[test]
    fn a_folds_name_survives_the_log_growing_underneath_it() {
        // The same two folds, and then a third the turn has just written. Counting from
        // the oldest still names the result that was asked for; counting from the newest
        // would have silently moved to the fold below it.
        let noisy = |tag: &str| {
            (1..=50)
                .map(|n| format!("{tag} {n}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let log = |third: bool| {
            let mut events = vec![
                bare(
                    1,
                    "tool.result",
                    json!({"call_id": "c1", "ok": true, "output": noisy("first")}),
                ),
                bare(
                    2,
                    "tool.result",
                    json!({"call_id": "c2", "ok": true, "output": noisy("second")}),
                ),
            ];
            if third {
                events.push(bare(
                    3,
                    "tool.result",
                    json!({"call_id": "c3", "ok": true, "output": noisy("third")}),
                ));
            }
            response(1, json!({ "events": events }))
        };
        let mut lines = vec![log(false)];
        lines.extend([log(true), log(true), log(true)]);
        let mut client = Client::new(Fake::with(lines), "s-1");
        assert!(client.expand_tool_result(0).unwrap()[0].contains("first 1"));
        // The name still means the same fold once another one has landed after it, and
        // the new fold is simply the next name.
        assert!(client.expand_tool_result(0).unwrap()[0].contains("first 1"));
        assert!(client.expand_tool_result(1).unwrap()[0].contains("second 1"));
        assert!(client.expand_tool_result(2).unwrap()[0].contains("third 1"));
    }

    #[test]
    fn a_marker_is_the_line_the_summary_ends_with() {
        assert!(is_fold_marker("  … 197 more line(s), 5.0 KiB · /expand"));
        assert!(!is_fold_marker("  … 197 more line(s), 5.0 KiB"));
        assert!(!is_fold_marker("← ok /expand"));
        assert!(folded_output("← ok a\n  … 2 more line(s), 8 B · /expand"));
        assert!(!folded_output("← ok a\nb"));
    }

    #[test]
    fn an_error_response_surfaces_as_an_error() {
        let fake = Fake::with(vec![
            json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32601, "message": "unknown method"}}),
        ]);
        let mut client = Client::new(fake, "s-1");
        let err = client.submit("x").unwrap_err();
        assert!(err.contains("-32601"), "got {err}");
        assert!(err.contains("unknown method"));
    }
}
