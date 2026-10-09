//! The web surface, end to end: the real binary, a real WebSocket client, and a
//! model behind it. If this passes, a browser is the only part left untested — and
//! a browser speaks the same protocol this client does.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tungstenite::client::IntoClientRequest;
use tungstenite::Message;

/// A command that cannot see the developer's configuration. Without this a control
/// plane configured on this machine supplies the endpoints under test — and spends
/// real tokens while doing it.
fn isolated_command() -> Command {
    let base = std::env::temp_dir().join(format!("loongagent-testenv-{}", std::process::id()));
    let mut command = Command::new(env!("CARGO_BIN_EXE_loongagent"));
    command
        .env("LOONGAGENT_HOME", base.join("loongagent"))
        .env("HOME", base.join("home"));
    command
}

static N: AtomicU64 = AtomicU64::new(0);

fn workspace() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "loongagent-web-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A model that calls `read_file` once and then answers.
fn fake_model() -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            // One request per connection: read the headers, then the body.
            let mut raw = Vec::new();
            let mut buffer = [0u8; 4096];
            let mut wanted = usize::MAX;
            loop {
                let Ok(read) = stream.read(&mut buffer) else {
                    break;
                };
                if read == 0 {
                    break;
                }
                raw.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&raw).to_string();
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    if wanted == usize::MAX {
                        wanted = head
                            .lines()
                            .find_map(|l| l.strip_prefix("Content-Length: "))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                    }
                    if body.len() >= wanted {
                        let asked_for_a_tool = body.contains("\"role\":\"tool\"");
                        let reply = if asked_for_a_tool {
                            json!({"choices": [{"message": {"role": "assistant", "content": "the file says hi"}}],
                                   "usage": {"prompt_tokens": 20, "completion_tokens": 5}})
                        } else {
                            json!({"choices": [{"message": {"role": "assistant", "content": null,
                                   "tool_calls": [{"id": "c1", "type": "function",
                                     "function": {"name": "read_file", "arguments": "{\"path\":\"note.txt\"}"}}]},
                                   "finish_reason": "tool_calls"}],
                                   "usage": {"prompt_tokens": 20, "completion_tokens": 4}})
                        };
                        let body = reply.to_string();
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(head.as_bytes());
                        let _ = stream.write_all(body.as_bytes());
                        let _ = stream.flush();
                        break;
                    }
                }
            }
        }
    });
    (format!("http://127.0.0.1:{port}/v1"), handle)
}

struct Web {
    child: Child,
    port: u16,
    token: String,
    _dir: std::path::PathBuf,
}

impl Web {
    fn start(dir: &std::path::Path, base_url: &str) -> Self {
        let mut child = isolated_command()
            .arg("web")
            .arg("--session")
            .arg(dir.join("s.jsonl"))
            .arg("--cwd")
            .arg(dir)
            .arg("--model")
            .arg("openai-compat")
            .arg("--bind")
            .arg("127.0.0.1")
            .arg("--port")
            .arg("0")
            .env("LOONGAGENT_BASE_URL", base_url)
            .env("LOONGAGENT_MODEL", "fake")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the binary runs");

        // The banner carries the URL, which carries the token.
        let stdout = child.stdout.take().unwrap();
        let mut reader = BufReader::new(stdout);
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut port = 0;
        let mut token = String::new();
        while Instant::now() < deadline {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            if let Some(rest) = line.trim().strip_prefix("http://127.0.0.1:") {
                let (p, fragment) = rest.split_once('/').expect("host:port/");
                port = p.parse().expect("a port");
                token = fragment
                    .split("token=")
                    .nth(1)
                    .expect("a token in the fragment")
                    .to_string();
                break;
            }
        }
        assert!(
            port > 0 && !token.is_empty(),
            "the banner names the URL and token"
        );
        // Keep draining stdout so the child never blocks on a full pipe.
        std::thread::spawn(move || {
            let mut sink = String::new();
            let _ = reader.read_to_string(&mut sink);
        });
        Web {
            child,
            port,
            token,
            _dir: dir.to_path_buf(),
        }
    }

    fn connect(&self) -> tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>> {
        let request = format!("ws://127.0.0.1:{}/ws?token={}", self.port, self.token)
            .into_client_request()
            .unwrap();
        tungstenite::connect(request)
            .expect("the token is accepted")
            .0
    }
}

impl Drop for Web {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Send one request and collect the events plus the response.
fn exchange(
    socket: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>,
    id: u64,
    method: &str,
    params: Value,
) -> (Vec<Value>, Value) {
    let (events, methods, response) = exchange_verbose(socket, id, method, params);
    let _ = methods;
    (events, response)
}

/// The same, but also reporting every notification method seen — which is how a
/// test can tell a streamed answer from one delivered in a single piece.
fn exchange_verbose(
    socket: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>,
    id: u64,
    method: &str,
    params: Value,
) -> (Vec<Value>, Vec<String>, Value) {
    socket
        .send(Message::text(
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string(),
        ))
        .unwrap();
    let mut events = Vec::new();
    let mut methods = Vec::new();
    loop {
        let message = socket.read().expect("a reply");
        let text = message.into_text().unwrap().to_string();
        let value: Value = serde_json::from_str(&text).expect("JSON on the wire");
        if value.get("method").is_some() {
            let method = value["method"].as_str().unwrap_or("").to_string();
            methods.push(method.clone());
            if method == "event" {
                events.push(value["params"].clone());
            }
            continue;
        }
        assert_eq!(value["id"], json!(id), "the response matches the request");
        return (events, methods, value);
    }
}

#[test]
fn a_browser_can_drive_a_session_with_a_model_behind_it() {
    let (base_url, _model) = fake_model();
    let dir = workspace();
    std::fs::write(dir.join("note.txt"), "hi from the workspace\n").unwrap();
    let web = Web::start(&dir, &base_url);

    let mut socket = web.connect();
    let (events, response) = exchange(
        &mut socket,
        1,
        "session.attach",
        json!({"cwd": "", "client": "web"}),
    );
    assert!(response["error"].is_null(), "{response}");
    let session_id = response["result"]["session_id"]
        .as_str()
        .unwrap()
        .to_string();
    let kinds: Vec<&str> = events.iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert!(
        kinds.contains(&"skills.loaded"),
        "the catalogue streams on attach: {kinds:?}"
    );

    let (events, methods, response) = exchange_verbose(
        &mut socket,
        2,
        "prompt.submit",
        json!({"session_id": session_id, "text": "read note.txt"}),
    );
    assert_eq!(response["result"]["accepted"], true, "{response}");
    // The browser sees the answer *and* the fact that it was written: a delta
    // notification reaches it before the response, while the log keeps only the
    // finished message (asserted on the server side).
    assert!(
        methods.contains(&"model.delta".to_string()),
        "deltas reach the browser: {methods:?}"
    );
    let first_delta = methods.iter().position(|m| m == "model.delta").unwrap();
    assert_eq!(first_delta, 0, "before anything else: {methods:?}");

    let kinds: Vec<&str> = events.iter().map(|e| e["kind"].as_str().unwrap()).collect();
    for expected in [
        "message.appended",
        "tool.call",
        "policy.decided",
        "tool.result",
        "cost.recorded",
    ] {
        assert!(
            kinds.contains(&expected),
            "{expected} must reach the browser: {kinds:?}"
        );
    }
    let call = events.iter().find(|e| e["kind"] == "tool.call").unwrap();
    assert_eq!(call["data"]["tool"], "read_file");
    let result = events.iter().find(|e| e["kind"] == "tool.result").unwrap();
    assert_eq!(result["data"]["ok"], true, "{:?}", result["data"]["output"]);
    let answer = events
        .iter()
        .filter(|e| e["kind"] == "message.appended" && e["data"]["role"] == "assistant")
        .next_back()
        .unwrap();
    assert_eq!(answer["data"]["text"], "the file says hi");

    // The page itself is served on the same port.
    let mut stream = TcpStream::connect(("127.0.0.1", web.port)).unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut page = String::new();
    stream.read_to_string(&mut page).unwrap();
    assert!(
        page.contains("<!DOCTYPE html>"),
        "a page, not a protocol error"
    );
    assert!(page.contains("loongagent"));

    let _ = std::fs::remove_dir_all(&dir);
}
