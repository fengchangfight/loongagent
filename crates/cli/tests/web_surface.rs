//! Purpose: The web surface end to end: a real WebSocket client, the token, the event stream and the page.
//!
//! # Contract
//! - **Serves:** the localhost browser path ADR-0009 describes.
//! - **Design:** `docs/design/surfaces.md` §4 and §7.
//! - **Upstream:** runs `loongagent web`; nothing imports this file.
//! - **Downstream:** `common::isolated_command` and a fake OpenAI-compatible model behind the server.
//! - **Verify:** `cargo test -p loongagent --test web_surface`
//!
//! The web surface, end to end: the real binary, a real WebSocket client, and a
//! model behind it. If this passes, a browser is the only part left untested — and
//! a browser speaks the same protocol this client does.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tungstenite::client::IntoClientRequest;
use tungstenite::Message;

mod common;

static N: AtomicU64 = AtomicU64::new(0);

/// Purpose: A fresh workspace for the one test.
/// Contract: `web_surface.rs` fixtures; the test calls it.
fn workspace() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "loongagent-web-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Purpose: A model that calls `read_file` once and then answers, so the page sees a tool turn.
/// Serves: the one test in this file.
/// Design: `docs/design/config-routing.md` §8.
/// Up: the test; no other caller.
/// Down: `std::net::TcpListener` and a reader thread.
/// Verify: `cargo test -p loongagent --test web_surface`.
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

/// Purpose: One running `loongagent web` child, its port and the token from its banner.
/// Serves: the one test in this file.
/// Design: `docs/design/surfaces.md` §4.
/// Up: `Web::start` builds it; the test connects through it.
/// Down: the child process, its stdout banner and the `Drop` impl.
/// Verify: `cargo test -p loongagent --test web_surface`.
struct Web {
    child: Child,
    port: u16,
    token: String,
    _dir: std::path::PathBuf,
}

impl Web {
    /// Purpose: Spawn `loongagent web`, read the URL and token from the banner, and keep stdout drained.
    /// Serves: the one test in this file.
    /// Design: `docs/design/surfaces.md` §4.
    /// Up: the test; no other caller.
    /// Down: `common::isolated_command` and `std::io::BufReader`.
    /// Verify: `cargo test -p loongagent --test web_surface`.
    fn start(dir: &std::path::Path, base_url: &str) -> Self {
        Self::start_with(dir, base_url, Some("openai-compat"))
    }

    /// Purpose: The same child, optionally launched with no `--model`, so a test can stand in
    /// the shoes of an install that has never configured one.
    /// Serves: the model-panel test's empty case (web-surface.md §6).
    /// Design: `docs/design/web-surface.md` §6.
    /// Up: `start`, and the panel test directly.
    /// Down: `common::isolated_command` and the child's stdout banner.
    /// Verify: `cargo test -p loongagent --test web_surface`.
    fn start_with(dir: &std::path::Path, base_url: &str, model: Option<&str>) -> Self {
        let mut command = common::isolated_command();
        command
            .arg("web")
            .arg("--session")
            .arg(dir.join("s.jsonl"))
            .arg("--cwd")
            .arg(dir);
        if let Some(model) = model {
            command.arg("--model").arg(model);
        }
        command
            .arg("--bind")
            .arg("127.0.0.1")
            .arg("--port")
            .arg("0");
        command
            .env("LOONGAGENT_BASE_URL", base_url)
            .env("LOONGAGENT_MODEL", "fake")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        Self::from_spawned(command, dir)
    }

    /// Purpose: The same child on a local profile of its own: a home holding a `local.toml` with
    /// several `[[models]]`, no `--model` and no `LOONGAGENT_MODEL` — the shape a reorder needs,
    /// because the order is written back to that file.
    /// Serves: the reorder test (web-surface.md §4).
    /// Design: `docs/design/web-surface.md` §4 and `docs/design/config-routing.md` §3.
    /// Up: the reorder test; no other caller.
    /// Down: `common::isolated_command`, the banner on stdout, and the child's own home.
    /// Verify: `cargo test -p loongagent --test web_surface` (`reorder_`).
    fn start_local(dir: &std::path::Path, base_url: &str) -> Self {
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        // Three models on Ollama, which needs no key: nothing here reaches the network,
        // and the first block is the one a new session would start with.
        std::fs::write(
            home.join("local.toml"),
            "[[models]]\nname = \"alpha\"\nprovider = \"ollama/llama3\"\n\n\
             [[models]]\nname = \"beta\"\nprovider = \"ollama/qwen2.5:3b\"\n\n\
             [[models]]\nname = \"gamma\"\nprovider = \"ollama/mistral\"\n",
        )
        .unwrap();
        let mut command = common::isolated_command();
        command
            .arg("web")
            .arg("--session")
            .arg(dir.join("s.jsonl"))
            .arg("--cwd")
            .arg(dir)
            .arg("--bind")
            .arg("127.0.0.1")
            .arg("--port")
            .arg("0")
            .env("LOONGAGENT_HOME", &home)
            .env("HOME", dir.join("homeuser"))
            .env("LOONGAGENT_BASE_URL", base_url)
            .env_remove("LOONGAGENT_MODEL")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        Self::from_spawned(command, dir)
    }

    /// Purpose: Spawn the command and read the port and token out of its banner.
    /// Serves: the two constructors above.
    /// Design: `docs/design/surfaces.md` §4.
    /// Up: `start_with` and `start_local`.
    /// Down: the child process and its stdout.
    /// Verify: `cargo test -p loongagent --test web_surface`.
    /// Contract: the child's stdout is drained for the rest of the run, so it never blocks.
    fn from_spawned(mut command: std::process::Command, dir: &std::path::Path) -> Self {
        // A developer's shell may have any of these exported, and `model.add` treats a key
        // it finds in the environment as one already on file — which it is. A test that
        // wants "no key configured yet" has to say so, or it passes for a reason that has
        // nothing to do with the code (config-routing.md §5, and `local_mode.rs` does the
        // same).
        for provider in loong_config::catalog::PROVIDERS {
            command.env_remove(provider.key_env);
            for alias in provider.aliases {
                command.env_remove(alias);
            }
        }
        let mut child = command.spawn().expect("the binary runs");
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

    /// Purpose: Open the WebSocket with the banner's token.
    /// Contract: `web_surface.rs` fixture; the test is the only caller.
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
    /// Purpose: Kill and reap the web child when the test ends.
    /// Contract: `web_surface.rs` `Drop` impl for `Web`.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Purpose: Send one request and return the events and the response, dropping the method list.
/// Contract: `web_surface.rs` fixture; the attach call is the only caller.
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

/// Purpose: The same exchange, also reporting every notification method so a streamed answer is distinguishable.
/// Serves: the test that checks deltas arrive before the response.
/// Design: `docs/design/surfaces.md` §7.
/// Up: `exchange` and the test; no other caller.
/// Down: the WebSocket and `serde_json`.
/// Verify: `cargo test -p loongagent --test web_surface`.
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

/// Purpose: A browser attaches, submits a prompt, sees the tool turn's events and a delta before the answer, and is served the page.
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
    // The work reaches the browser as it is recorded, so the writing is no longer the
    // first thing it sees: the session's opening events and the turn's own arrive
    // first, in order, and the delta follows. What matters is that nothing but events
    // precedes it and that at least one did.
    assert!(
        methods[..first_delta]
            .iter()
            .all(|method| method == "event"),
        "only events precede the first delta: {methods:?}"
    );
    assert!(
        !methods[..first_delta].is_empty(),
        "and the work is visible before the answer: {methods:?}"
    );

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

/// Purpose: A profile with no model says so to the page, the panel lists the real catalogue, and a
/// keyless add for a provider that wants one is refused before any file is written.
/// Serves: the empty-state warning and the panel's two offline reads (web-surface.md §6).
/// Design: `docs/design/web-surface.md` §6.
/// Up: `cargo test -p loongagent --test web_surface`.
/// Down: `Web::start_with`, `exchange` and the surface's `model.*` arms.
/// Verify: `cargo test -p loongagent --test web_surface`.
#[test]
fn the_model_panel_warns_when_nothing_is_configured() {
    let dir = std::env::temp_dir().join(format!("loong-panel-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (base_url, _server) = fake_model();
    // No `--model`: this is an install that has never configured one, which is exactly the
    // person the warning exists for.
    let web = Web::start_with(&dir, &base_url, None);
    let mut socket = web.connect();

    let (_, configured) = exchange(&mut socket, 1, "model.configured", json!({}));
    assert_eq!(
        configured["result"]["models"],
        json!([]),
        "a profile with no model is reported empty, and that is what shows the banner: {configured}"
    );

    let (_, providers) = exchange(&mut socket, 2, "model.providers", json!({}));
    let list = providers["result"]["providers"]
        .as_array()
        .expect("the catalogue is an array");
    let deepseek = list
        .iter()
        .find(|p| p["id"] == "deepseek")
        .expect("deepseek is in the catalogue");
    assert_eq!(
        deepseek["needs_key"], true,
        "a hosted provider says it wants a key, so the panel can ask for one: {deepseek}"
    );
    assert!(
        list.iter().any(|p| p["needs_key"] == false),
        "a loopback provider says it wants none, so the panel does not ask: {list:?}"
    );

    let (_, refused) = exchange(
        &mut socket,
        3,
        "model.add",
        json!({"provider": "deepseek", "key": "", "models": []}),
    );
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("API key"),
        "a keyless add names what is missing instead of writing half a provider: {refused}"
    );

    let (_, unselected) = exchange(
        &mut socket,
        4,
        "model.add",
        json!({"provider": "deepseek", "key": "sk-test"}),
    );
    assert!(
        unselected["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("models"),
        "a page that sends no selection is refused, never read as \"tick nothing\": {unselected}"
    );

    let (_, unknown) = exchange(
        &mut socket,
        5,
        "model.add",
        json!({"provider": "no-such-provider", "key": "x", "models": []}),
    );
    assert!(
        unknown["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("no-such-provider"),
        "an unknown provider is named back: {unknown}"
    );

    // Listing a provider whose key was never stored says so instead of guessing at an
    // empty key: the panel has no rows to draw until there is a key to draw them with
    // (web-surface.md §6).
    let (_, keyless) = exchange(
        &mut socket,
        6,
        "model.probe",
        json!({"provider": "deepseek", "key": ""}),
    );
    assert!(
        keyless["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("key"),
        "probing without a key on file is refused in words that name the key: {keyless}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Purpose: The panel writes exactly the models that were ticked and can untick one later —
/// a provider with many models must not fill `local.toml` with all of them because a key was
/// pasted, and turning one off must not cost the key.
#[test]
fn the_panel_writes_only_the_ticked_models_and_can_untick_them() {
    let (base_url, _model) = fake_model();
    let dir = workspace();
    // Three models on Ollama, which needs no key: the write is the only thing under test,
    // so nothing here reaches the network (a loopback provider has no `/models` to ask).
    let web = Web::start_local(&dir, &base_url);
    let mut socket = web.connect();
    let profile = dir.join("home").join("local.toml");

    let (_, configured) = exchange(&mut socket, 1, "model.configured", json!({}));
    let rows = configured["result"]["models"]
        .as_array()
        .expect("configured models are an array");
    let names: Vec<&str> = rows.iter().filter_map(|m| m["name"].as_str()).collect();
    assert_eq!(
        names,
        vec!["alpha", "beta", "gamma"],
        "the panel opens on what is configured, because that is what can be turned off: {configured}"
    );
    assert_eq!(
        rows[0]["provider"], "ollama",
        "each row carries the provider it belongs to, so the page can write it back: {configured}"
    );

    // One ticked out of three: one block stays, two go. This is the write the panel makes
    // when a person unticks a model, and it is the whole reason the key must not be needed
    // again for it.
    let (_, written) = exchange(
        &mut socket,
        2,
        "model.add",
        json!({"provider": "ollama", "key": "",
               "models": [{"name": "beta", "context_window": 32768}]}),
    );
    assert_eq!(
        written["result"]["models"],
        json!([{"name": "beta", "context_window": 32768}]),
        "the answer is the whole selection, not a delta: {written}"
    );
    let text = std::fs::read_to_string(&profile).unwrap();
    assert!(
        text.contains("name = \"beta\""),
        "the ticked one stays: {text}"
    );
    assert!(
        !text.contains("name = \"alpha\""),
        "the unticked ones go: {text}"
    );
    assert!(!text.contains("name = \"gamma\""), "{text}");

    let (_, after) = exchange(&mut socket, 3, "model.configured", json!({}));
    assert_eq!(
        after["result"]["models"],
        json!([{"name": "beta", "provider": "ollama", "context_window": 32768}]),
        "the panel's list follows what it just wrote, so no unticked model lingers: {after}"
    );

    // Ticking nothing is a real answer: it turns the provider's models off and keeps the
    // provider, so the same panel can turn them back on later.
    let (_, cleared) = exchange(
        &mut socket,
        4,
        "model.add",
        json!({"provider": "ollama", "key": "", "models": []}),
    );
    assert_eq!(cleared["result"]["models"], json!([]), "{cleared}");
    let (_, none) = exchange(&mut socket, 5, "model.configured", json!({}));
    assert_eq!(
        none["result"]["models"],
        json!([]),
        "nothing is configured any more, which is what shows the banner again: {none}"
    );
    let text = std::fs::read_to_string(&profile).unwrap();
    assert!(
        !text.contains("ollama/"),
        "the blocks are gone, not merely hidden: {text}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Purpose: Ctrl+right is a reorder, not a switch: on a real local profile it moves the current
/// model one slot, keeps it current, and rewrites `local.toml` so the next session starts with
/// the new order — while a refresh of `model.list` still shows the arrangement.
#[test]
fn reorder_writes_the_new_order_to_the_local_profile() {
    let (base_url, _model) = fake_model();
    let dir = workspace();
    let web = Web::start_local(&dir, &base_url);
    let mut socket = web.connect();

    let (_, response) = exchange(
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

    let (_, listed) = exchange(
        &mut socket,
        2,
        "model.list",
        json!({"session_id": session_id}),
    );
    let names: Vec<&str> = listed["result"]["models"]
        .as_array()
        .expect("a model list")
        .iter()
        .map(|m| m["name"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        names,
        vec!["alpha", "beta", "gamma"],
        "the local profile's blocks, in file order: {listed}"
    );
    assert_eq!(listed["result"]["selected"], "alpha", "{listed}");

    let (_, moved) = exchange(
        &mut socket,
        3,
        "model.reorder",
        json!({"session_id": session_id, "delta": 1}),
    );
    assert!(moved["error"].is_null(), "{moved}");
    let after: Vec<&str> = moved["result"]["models"]
        .as_array()
        .expect("a model list")
        .iter()
        .map(|m| m["name"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        after,
        vec!["beta", "alpha", "gamma"],
        "one slot, not a switch: {moved}"
    );
    assert_eq!(
        moved["result"]["selected"], "alpha",
        "the model stayed current while its slot changed: {moved}"
    );
    assert_eq!(moved["result"]["persisted"], json!(true), "{moved}");

    let text = std::fs::read_to_string(dir.join("home").join("local.toml")).unwrap();
    let beta_at = text.find("name = \"beta\"").expect("beta is still there");
    let alpha_at = text.find("name = \"alpha\"").expect("alpha is still there");
    assert!(
        beta_at < alpha_at,
        "the file now leads with beta, which is what a new session starts on: {text}"
    );

    // A refresh from a child that read the old order must not snap the bar back.
    let (_, refreshed) = exchange(
        &mut socket,
        4,
        "model.list",
        json!({"session_id": session_id}),
    );
    let refreshed: Vec<&str> = refreshed["result"]["models"]
        .as_array()
        .expect("a model list")
        .iter()
        .map(|m| m["name"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        refreshed,
        vec!["beta", "alpha", "gamma"],
        "the arrangement outlives a fresh model.list"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
