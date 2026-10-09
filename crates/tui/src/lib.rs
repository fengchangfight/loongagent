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
    pending_approval: Option<String>,
    /// Whether the last submitted prompt was accepted (a hook can refuse one).
    last_accepted: bool,
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
            let detail = [
                "command", "path", "pattern", "query", "url", "name", "prompt",
            ]
            .iter()
            .find_map(|key| args.get(*key).and_then(|value| value.as_str()))
            .map(|text| text.to_string())
            .unwrap_or_else(|| truncate(&args.to_string(), 60));
            Some(format!("→ {tool} {}", truncate(&detail, 80)))
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

/// A tool result, bounded. A chatty command must not turn the transcript into a wall of
/// text, so a long one is summarised with the count and the way to see the rest.
fn collapse_result(ok: bool, output: &str) -> String {
    const MAX_ROWS: usize = 3;
    const MAX_COLUMNS: usize = 100;
    let verdict = if ok { "ok" } else { "failed" };
    let lines: Vec<&str> = output.lines().collect();
    let shown: Vec<String> = lines
        .iter()
        .take(MAX_ROWS)
        .map(|line| truncate(line, MAX_COLUMNS))
        .collect();
    let cut = lines.len() > MAX_ROWS
        || lines
            .iter()
            .take(MAX_ROWS)
            .zip(shown.iter())
            .any(|(raw, clipped)| raw.chars().count() > clipped.chars().count());
    let mut text = format!("← {verdict} {}", shown.first().cloned().unwrap_or_default());
    for line in shown.iter().skip(1) {
        text.push('\n');
        text.push_str("  ");
        text.push_str(line);
    }
    if cut {
        let hidden = lines.len().saturating_sub(MAX_ROWS);
        text.push('\n');
        text.push_str(&format!(
            "  … {hidden} more line(s), {} · /expand",
            human_bytes(output.len())
        ));
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

impl<T: Transport> Client<T> {
    pub fn new(transport: T, session_id: impl Into<String>) -> Self {
        Self {
            transport,
            session_id: session_id.into(),
            next_id: 1,
            pending_approval: None,
            last_accepted: true,
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn pending_approval(&self) -> Option<&str> {
        self.pending_approval.as_deref()
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
        let params = gen::SessionOnlyParams {
            session_id: self.session_id.clone(),
        };
        let (_, result) = self.call(
            gen::method::SESSION_REPLAY,
            serde_json::to_value(params).unwrap(),
        )?;
        let replay: gen::SessionReplayResult =
            serde_json::from_value(result).map_err(|e| format!("session.replay: {e}"))?;
        let last = replay.events.iter().rev().find(|event| {
            event.get("kind").and_then(|k| k.as_str()) == Some(gen::event::TOOL_RESULT)
        });
        Ok(match last.and_then(render_event_full) {
            Some(text) => text.split('\n').map(|line| line.to_string()).collect(),
            None => Vec::new(),
        })
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
        self.pending_approval = None;
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
                        == Some(gen::event::APPROVAL_REQUESTED)
                    {
                        self.pending_approval = notification
                            .params
                            .get("data")
                            .and_then(|data| data.get("request_id"))
                            .and_then(|id| id.as_str())
                            .map(|id| id.to_string());
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
            4,
            "bounded, whatever the command printed: {rows:?}"
        );
        assert!(rows[0].starts_with("← ok line 1 of output"), "{rows:?}");
        assert!(rows[3].contains("197 more line(s)"), "{rows:?}");
        assert!(
            rows[3].contains("/expand"),
            "and says how to see them: {rows:?}"
        );
        assert!(rows[3].contains("KiB"), "{rows:?}");

        // `/expand` is the other half: the log has everything, so nothing is lost.
        let expanded = render_event_full(&event).unwrap();
        assert!(
            expanded.contains("line 200 of output"),
            "{}",
            &expanded[..80]
        );
        assert!(expanded.lines().count() > 100);

        // A single long line is cut, and says so.
        let one = json!({
            "kind": gen::event::TOOL_RESULT,
            "data": {"ok": true, "output": "x".repeat(500)},
        });
        let collapsed = render_event(&one).unwrap();
        assert!(collapsed.lines().count() <= 4, "{collapsed}");
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
