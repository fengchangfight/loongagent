//! The local agent server (ADR-0001): sessions live here, clients attach.
//!
//! Transport is newline-delimited JSON-RPC over stdio (a unix socket is the same
//! loop over a different stream). The server owns the session: it asks the
//! kernel to run a turn, then **streams every event the turn produced** as an
//! `event` notification before answering the request. A thin client therefore
//! needs no agent logic to render what happened (ADR-0009) — and no client can
//! see a decision that was not also written to the log.

use loong_kernel::{Kernel, KernelError};
use loong_protocol::{codes, Notification, Request, Response};
use loong_schema::generated as gen;
use serde_json::Value;
use std::io::{BufRead, Write};

#[derive(Debug)]
pub enum ServerError {
    Io(std::io::Error),
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServerError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for ServerError {}

impl From<std::io::Error> for ServerError {
    fn from(e: std::io::Error) -> Self {
        ServerError::Io(e)
    }
}

pub struct Server {
    kernel: Kernel,
    session_id: String,
    /// Highest sequence already streamed to the client.
    last_sent: u64,
    turns: u64,
}

impl Server {
    pub fn new(kernel: Kernel, session_id: impl Into<String>) -> Self {
        Self { kernel, session_id: session_id.into(), last_sent: 0, turns: 0 }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Handle one inbound line and return the outbound lines, in order:
    /// event notifications first, then exactly one response for a request.
    /// Pure enough to test without any transport at all.
    pub fn handle_line(&mut self, line: &str) -> Vec<String> {
        match Request::parse(line) {
            Ok(request) => self.dispatch(request),
            Err(response) => vec![response.to_line()],
        }
    }

    fn dispatch(&mut self, request: Request) -> Vec<String> {
        let id = request.id.clone().unwrap_or(Value::Null);
        match request.method.as_str() {
            gen::method::SESSION_ATTACH => {
                let params: gen::SessionAttachParams = match request.params_as() {
                    Ok(p) => p,
                    Err(e) => return vec![e.to_line()],
                };
                let _ = &params.client;
                let result = gen::SessionAttachResult {
                    session_id: self.session_id.clone(),
                    schema_version: gen::SCHEMA_VERSION.to_string(),
                    existing_events: self.kernel.last_seq(),
                };
                self.respond(id, result, "session.attach")
            }
            gen::method::PROMPT_SUBMIT => {
                let params: gen::PromptSubmitParams = match request.params_as() {
                    Ok(p) => p,
                    Err(e) => return vec![e.to_line()],
                };
                if let Some(bad) = self.wrong_session(&params.session_id) {
                    return vec![bad];
                }
                self.turns += 1;
                let result = gen::PromptSubmitResult { turn_id: self.turns, accepted: true };
                match self.kernel.submit(&params.text) {
                    Ok(_) => self.respond(id, result, "prompt.submit"),
                    Err(e) => self.fail(id, e),
                }
            }
            gen::method::APPROVAL_RESPOND => {
                let params: gen::ApprovalRespondParams = match request.params_as() {
                    Ok(p) => p,
                    Err(e) => return vec![e.to_line()],
                };
                if let Some(bad) = self.wrong_session(&params.session_id) {
                    return vec![bad];
                }
                let result = gen::PromptSubmitResult { turn_id: self.turns, accepted: params.granted };
                match self.kernel.respond_to_approval(
                    &params.request_id,
                    params.granted,
                    params.scope,
                    params.reason.as_deref(),
                ) {
                    Ok(_) => self.respond(id, result, "approval.respond"),
                    Err(e) => self.fail(id, e),
                }
            }
            other => vec![Response::err(
                id,
                codes::METHOD_NOT_FOUND,
                format!("unknown method {other:?}"),
            )
            .to_line()],
        }
    }

    /// `<T>`: stream the events this call produced, then the response.
    fn respond<T: serde::Serialize>(&mut self, id: Value, result: T, _method: &str) -> Vec<String> {
        let mut lines = self.drain_events();
        match serde_json::to_value(&result) {
            Ok(value) => lines.push(Response::ok(id, value).to_line()),
            Err(e) => lines.push(Response::err(id, codes::INTERNAL_ERROR, e.to_string()).to_line()),
        }
        lines
    }

    fn fail(&mut self, id: Value, error: KernelError) -> Vec<String> {
        let mut lines = self.drain_events();
        let code = match error {
            KernelError::OutOfOrder(_) => codes::INVALID_PARAMS,
            _ => codes::INTERNAL_ERROR,
        };
        lines.push(Response::err(id, code, error.to_string()).to_line());
        lines
    }

    fn wrong_session(&self, session_id: &str) -> Option<String> {
        (session_id != self.session_id).then(|| {
            Response::err(
                Value::Null,
                codes::INVALID_PARAMS,
                format!("unknown session {session_id:?} (this server owns {:?})", self.session_id),
            )
            .to_line()
        })
    }

    /// Every event appended since the client last heard from us.
    fn drain_events(&mut self) -> Vec<String> {
        let Ok(events) = self.kernel.events_since(self.last_sent) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(events.len());
        for event in events {
            self.last_sent = self.last_sent.max(event.seq);
            match serde_json::to_value(&event) {
                Ok(value) => out.push(Notification::event(&value).to_line()),
                Err(_) => continue,
            }
        }
        out
    }

    /// Serve stdio until EOF (the transport the TUI spawns).
    pub fn run_stdio(mut self) -> Result<(), ServerError> {
        let stdin = std::io::stdin();
        let mut stdout = std::io::stdout();
        for line in stdin.lock().lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            for out in self.handle_line(&line) {
                writeln!(stdout, "{out}")?;
            }
            stdout.flush()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loong_kernel::KernelConfig;
    use loong_providers::{CompletionResponse, ScriptedModel, ToolCallRequest, Usage};
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "loong-server-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Temp(dir)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scripted(calls: Vec<ToolCallRequest>, text: Option<&str>) -> ScriptedModel {
        ScriptedModel::new(vec![
            CompletionResponse {
                text: None,
                tool_calls: calls,
                usage: Usage { tokens_in: 11, tokens_out: 4 },
            },
            CompletionResponse {
                text: text.map(|t| t.to_string()),
                tool_calls: vec![],
                usage: Usage { tokens_in: 5, tokens_out: 2 },
            },
        ])
    }

    fn attach_line() -> String {
        Request::new(1, gen::method::SESSION_ATTACH, json!({"cwd": "/tmp", "client": "tui"})).to_line()
    }

    fn lines_of(server: &mut Server, line: &str) -> Vec<Value> {
        server
            .handle_line(line)
            .into_iter()
            .map(|l| serde_json::from_str(&l).expect("every outbound line is JSON"))
            .collect()
    }

    #[test]
    fn attach_answers_with_the_session_and_schema_version() {
        let t = Temp::new();
        let model = scripted(vec![], Some("hi"));
        let kernel =
            Kernel::create(t.0.join("s.jsonl"), KernelConfig::new(&t.0), Box::new(model)).unwrap();
        let mut server = Server::new(kernel, "s-local");
        let out = lines_of(&mut server, &attach_line());
        assert_eq!(out.len(), 1, "attach streams no events");
        assert_eq!(out[0]["result"]["session_id"], "s-local");
        assert_eq!(out[0]["result"]["schema_version"], gen::SCHEMA_VERSION);
        assert_eq!(out[0]["result"]["existing_events"], 0);
    }

    #[test]
    fn a_prompt_streams_events_then_answers() {
        let t = Temp::new();
        std::fs::write(t.0.join("a.txt"), "hello").unwrap();
        let model = scripted(
            vec![ToolCallRequest { call_id: "c1".into(), tool: "read_file".into(), args: json!({"path": "a.txt"}) }],
            Some("it says hello"),
        );
        let kernel =
            Kernel::create(t.0.join("s.jsonl"), KernelConfig::new(&t.0), Box::new(model)).unwrap();
        let mut server = Server::new(kernel, "s-local");
        let out = lines_of(
            &mut server,
            &Request::new(2, gen::method::PROMPT_SUBMIT, json!({"session_id": "s-local", "text": "read it"})).to_line(),
        );
        let events: Vec<&Value> = out.iter().filter(|v| v["method"] == "event").collect();
        let responses: Vec<&Value> = out.iter().filter(|v| v.get("result").is_some()).collect();
        assert_eq!(responses.len(), 1, "exactly one response");
        assert_eq!(responses[0]["result"]["accepted"], true);
        let kinds: Vec<&str> = events.iter().map(|e| e["params"]["kind"].as_str().unwrap()).collect();
        assert_eq!(
            kinds,
            vec![
                "message.appended",
                "cost.recorded",
                "message.appended",
                "tool.call",
                "policy.decided",
                "tool.result",
                "cost.recorded",
                "message.appended",
            ]
        );
        assert_eq!(out.last().unwrap()["result"]["turn_id"], 1, "responses come last");
    }

    #[test]
    fn an_out_of_bounds_write_asks_over_the_wire_and_resumes() {
        let t = Temp::new();
        let escape = t.0.parent().unwrap().join("loong-server-escape.txt");
        let _ = std::fs::remove_file(&escape);
        let model = scripted(
            vec![ToolCallRequest {
                call_id: "c1".into(),
                tool: "write_file".into(),
                args: json!({"path": escape.to_string_lossy(), "content": "x"}),
            }],
            Some("understood"),
        );
        let kernel =
            Kernel::create(t.0.join("s.jsonl"), KernelConfig::new(&t.0), Box::new(model)).unwrap();
        let mut server = Server::new(kernel, "s-local");
        let out = lines_of(
            &mut server,
            &Request::new(2, gen::method::PROMPT_SUBMIT, json!({"session_id": "s-local", "text": "escape"})).to_line(),
        );
        let asked = out
            .iter()
            .find(|v| v["method"] == "event" && v["params"]["kind"] == "approval.requested")
            .expect("the client is told an approval is pending");
        let request_id = asked["params"]["data"]["request_id"].as_str().unwrap().to_string();
        assert_eq!(asked["params"]["data"]["action_class"], "out_of_bounds");
        assert!(!escape.exists(), "nothing happens before the answer");

        let out = lines_of(
            &mut server,
            &Request::new(3, gen::method::APPROVAL_RESPOND, json!({
                "session_id": "s-local",
                "request_id": request_id,
                "granted": false,
                "scope": "once",
                "reason": "no"
            }))
            .to_line(),
        );
        let kinds: Vec<&str> = out
            .iter()
            .filter(|v| v["method"] == "event")
            .map(|e| e["params"]["kind"].as_str().unwrap())
            .collect();
        assert_eq!(kinds[0], "approval.decided");
        assert!(kinds.contains(&"tool.result"));
        assert_eq!(out.last().unwrap()["result"]["accepted"], false);
        assert!(!escape.exists());
    }

    #[test]
    fn protocol_errors_are_answered_not_ignored() {
        let t = Temp::new();
        let kernel = Kernel::create(
            t.0.join("s.jsonl"),
            KernelConfig::new(&t.0),
            Box::new(ScriptedModel::saying("hi")),
        )
        .unwrap();
        let mut server = Server::new(kernel, "s-local");
        assert_eq!(lines_of(&mut server, "{oops")[0]["error"]["code"], codes::PARSE_ERROR as i64);
        assert_eq!(
            lines_of(&mut server, &Request::new(9, "nope.method", json!({})).to_line())[0]["error"]["code"],
            codes::METHOD_NOT_FOUND as i64
        );
        let wrong = lines_of(
            &mut server,
            &Request::new(10, gen::method::PROMPT_SUBMIT, json!({"session_id": "other", "text": "x"})).to_line(),
        );
        assert_eq!(wrong[0]["error"]["code"], codes::INVALID_PARAMS as i64);
    }
}
