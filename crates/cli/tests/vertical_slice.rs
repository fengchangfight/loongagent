//! End-to-end over the real binary: spawn `loongagent server --stdio`, speak
//! JSON-RPC to it, and check the three things the slice promises — the tool ran
//! inside the workspace, the policy boundary asked before stepping outside, and
//! the JSONL log on disk is the same truth the client was streamed.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

mod common;

static N: AtomicU64 = AtomicU64::new(0);

const ESCAPE_NAME: &str = "loongagent-demo-escape.txt";

/// The path `demo-escape` names, derived the same way the model derives it rather
/// than written as a literal `/tmp/...`: that literal is not absolute on Windows, so
/// the "escape" would land inside the workspace and the test would prove nothing.
fn escape_path() -> PathBuf {
    std::env::temp_dir().join(ESCAPE_NAME)
}

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "loongagent-e2e-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Temp(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    session_id: String,
}

impl Session {
    fn start(dir: &Path, model: &str) -> Self {
        let mut child = common::isolated_command()
            .arg("server")
            .arg("--stdio")
            .arg("--session")
            .arg(dir.join("session.jsonl"))
            .arg("--cwd")
            .arg(dir)
            .arg("--model")
            .arg(model)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("the server binary starts");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut session = Session {
            child,
            stdin,
            stdout,
            next_id: 1,
            session_id: String::new(),
        };
        let (_, result) = session.call(
            "session.attach",
            json!({"cwd": dir.to_string_lossy(), "client": "test"}),
        );
        session.session_id = result["session_id"].as_str().unwrap().to_string();
        assert_eq!(result["schema_version"], loong_schema::SCHEMA_VERSION);
        session
    }

    /// Send a request and read until its response, collecting the events.
    fn call(&mut self, method: &str, params: Value) -> (Vec<Value>, Value) {
        let id = self.next_id;
        self.next_id += 1;
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{line}").unwrap();
        self.stdin.flush().unwrap();

        let mut events = Vec::new();
        loop {
            let mut buf = String::new();
            if self.stdout.read_line(&mut buf).unwrap() == 0 {
                panic!("the server closed the connection");
            }
            let value: Value = serde_json::from_str(buf.trim()).expect("a JSON line");
            // Anything with a `method` is a notification: `event` lines are the log
            // streamed, `model.delta` is a transient preview. Neither is the answer.
            if value.get("method").is_some() {
                if value["method"] == "event" {
                    events.push(value["params"].clone());
                }
                continue;
            }
            assert_eq!(value["id"], json!(id), "responses match the request");
            if let Some(error) = value.get("error") {
                panic!("rpc error: {error}");
            }
            return (events, value["result"].clone());
        }
    }

    fn prompt(&mut self, text: &str) -> (Vec<Value>, Value) {
        let session_id = self.session_id.clone();
        self.call(
            "prompt.submit",
            json!({"session_id": session_id, "text": text}),
        )
    }

    fn respond(&mut self, request_id: &str, granted: bool) -> (Vec<Value>, Value) {
        let session_id = self.session_id.clone();
        self.call(
            "approval.respond",
            json!({"session_id": session_id, "request_id": request_id, "granted": granted, "scope": "once"}),
        )
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn kinds(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|e| e["kind"].as_str().unwrap().to_string())
        .collect()
}

fn logged_kinds(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("session.jsonl"))
        .unwrap()
        .lines()
        .map(|l| {
            serde_json::from_str::<Value>(l).unwrap()["kind"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect()
}

#[test]
fn a_tool_runs_inside_the_workspace_and_the_log_is_the_same_truth() {
    let t = Temp::new();
    let mut session = Session::start(t.path(), "demo-write");
    let (events, result) = session.prompt("write the demo file");

    assert_eq!(result["accepted"], true);
    assert_eq!(result["turn_id"], 1);
    let seen = kinds(&events);
    assert!(seen.contains(&"tool.call".to_string()), "got {seen:?}");
    assert!(seen.contains(&"policy.decided".to_string()));
    assert!(seen.contains(&"tool.result".to_string()));

    let policy = events
        .iter()
        .find(|e| e["kind"] == "policy.decided")
        .unwrap();
    assert_eq!(policy["data"]["verdict"], "allow");
    let tool_result = events.iter().find(|e| e["kind"] == "tool.result").unwrap();
    assert_eq!(tool_result["data"]["ok"], true);

    let written = std::fs::read_to_string(t.path().join("demo.txt")).unwrap();
    assert!(written.contains("vertical slice"));

    // What the client saw is what the log holds: the streamed events are exactly
    // the tail of the log (the session's earlier opening events were streamed to
    // whoever attached before this turn).
    let logged = logged_kinds(t.path());
    assert!(logged.len() >= seen.len());
    assert_eq!(
        &logged[logged.len() - seen.len()..],
        &seen[..],
        "stream == truth"
    );
    for line in std::fs::read_to_string(t.path().join("session.jsonl"))
        .unwrap()
        .lines()
    {
        let envelope: Value = serde_json::from_str(line).unwrap();
        assert!(
            envelope["ignorable"].is_boolean(),
            "every event carries the envelope"
        );
        assert!(envelope["seq"].is_u64());
    }
}

#[test]
fn an_out_of_bounds_write_asks_before_it_touches_anything() {
    let _ = std::fs::remove_file(escape_path());
    let t = Temp::new();
    let mut session = Session::start(t.path(), "demo-escape");
    let (events, _) = session.prompt("write outside the workspace");

    let asked = events
        .iter()
        .find(|e| e["kind"] == "approval.requested")
        .expect("the boundary asks");
    assert_eq!(asked["data"]["action_class"], "out_of_bounds");
    let request_id = asked["data"]["request_id"].as_str().unwrap().to_string();
    assert!(asked["data"]["reason"]
        .as_str()
        .unwrap()
        .contains("out_of_bounds"));
    assert!(
        !escape_path().exists(),
        "nothing is written before the answer"
    );

    let (events, result) = session.respond(&request_id, false);
    assert_eq!(result["accepted"], false);
    let decided = events
        .iter()
        .find(|e| e["kind"] == "approval.decided")
        .unwrap();
    assert_eq!(decided["data"]["granted"], false);
    assert_eq!(decided["data"]["scope"], "once");
    let tool_result = events.iter().find(|e| e["kind"] == "tool.result").unwrap();
    assert_eq!(tool_result["data"]["ok"], false);
    assert!(!escape_path().exists());

    let logged = logged_kinds(t.path());
    assert!(logged.contains(&"approval.requested".to_string()));
    assert!(logged.contains(&"approval.decided".to_string()));
}

#[test]
fn the_tui_client_drives_a_real_server() {
    let t = Temp::new();
    let out = common::isolated_command()
        .env("LOONGAGENT_SERVER_BIN", env!("CARGO_BIN_EXE_loongagent"))
        .arg("run")
        .arg("hello from the client")
        .arg("--session")
        .arg(t.path().join("session.jsonl"))
        .arg("--cwd")
        .arg(t.path())
        .arg("--model")
        .arg("echo")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("echo: hello from the client"),
        "got: {stdout}"
    );
    assert!(t.path().join("session.jsonl").exists());
}
