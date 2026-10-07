//! A stdio MCP client: JSON-RPC 2.0 over newline-delimited messages.
//!
//! Deliberately small. It does the handshake, lists tools, calls tools, and does
//! nothing else — no resources, no prompts, no sampling, and it does not serve
//! server-initiated requests (a server that needs the model gets an error, not a
//! silent hang).
//!
//! Two properties keep it from becoming a liability:
//!
//! * **Every read has a deadline.** A reader thread feeds a channel and each
//!   request waits with a timeout, so a wedged server fails the call instead of
//!   hanging the session.
//! * **The child dies with the client.** Dropping the client kills the process
//!   group, so a server cannot outlive the session that started it.

use crate::config::McpServerSpec;
use loong_protocol::{Notification, Request, Response};
use loong_schema::generated::Json;
use loong_tools::{ExecContext, Tool, ToolError};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The protocol revision this client speaks. The server's answer is not
/// second-guessed: MCP is designed to negotiate, not to match exactly.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Debug)]
pub struct McpError(pub String);

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for McpError {}

fn err(message: impl Into<String>) -> McpError {
    McpError(message.into())
}

#[derive(Debug, Clone, PartialEq)]
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: Json,
}

pub struct McpClient {
    name: String,
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
    timeout: Duration,
}

impl McpClient {
    /// Start the server and complete the handshake.
    pub fn start(spec: &McpServerSpec) -> Result<Self, McpError> {
        let mut command = Command::new(&spec.command);
        command
            .args(&spec.args)
            .envs(&spec.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn().map_err(|e| {
            err(format!("starting MCP server {:?} ({}): {e}", spec.name, spec.command))
        })?;
        let stdin = child.stdin.take().ok_or_else(|| err("no stdin on the server"))?;
        let stdout = child.stdout.take().ok_or_else(|| err("no stdout on the server"))?;

        // A reader thread turns "the server is thinking" into a deadline rather
        // than a hang.
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(line) if !line.trim().is_empty() => {
                        if sender.send(line).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        });

        let mut client = Self {
            name: spec.name.clone(),
            child,
            stdin,
            lines,
            next_id: 1,
            timeout: spec.timeout,
        };
        client.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "loongagent", "version": env!("CARGO_PKG_VERSION")},
            }),
        )?;
        client.notify("notifications/initialized", json!({}))?;
        Ok(client)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn list_tools(&mut self) -> Result<Vec<McpToolInfo>, McpError> {
        let result = self.request("tools/list", json!({}))?;
        let tools = result
            .get("tools")
            .and_then(|t| t.as_array())
            .ok_or_else(|| err("tools/list returned no tools array"))?;
        let mut out = Vec::new();
        for tool in tools {
            let name = tool
                .get("name")
                .and_then(|n| n.as_str())
                .ok_or_else(|| err("a tool has no name"))?
                .to_string();
            out.push(McpToolInfo {
                name,
                description: tool
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("(no description)")
                    .to_string(),
                input_schema: tool
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object"})),
            });
        }
        Ok(out)
    }

    /// Call a tool and flatten its content to text. `isError` becomes an error so
    /// the model sees a failed call rather than a successful one with bad news.
    pub fn call_tool(&mut self, tool: &str, args: &Json) -> Result<String, McpError> {
        let result = self.request("tools/call", json!({"name": tool, "arguments": args}))?;
        if result.get("isError").and_then(|v| v.as_bool()).unwrap_or(false) {
            return Err(err(format!("{tool} reported an error: {}", flatten(&result))));
        }
        let text = flatten(&result);
        if text.is_empty() {
            Ok(format!("{tool} returned no text content"))
        } else {
            Ok(text)
        }
    }

    fn request(&mut self, method: &str, params: Json) -> Result<Json, McpError> {
        let id = self.next_id;
        self.next_id += 1;
        let line = Request::new(id, method, params).to_line();
        self.send(&line)?;

        let deadline = Instant::now() + self.timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(err(format!(
                    "{method}: no answer from MCP server {:?} within {:?}",
                    self.name, self.timeout
                )));
            }
            let line = match self.lines.recv_timeout(remaining) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(err(format!(
                        "{method}: no answer from MCP server {:?} within {:?}",
                        self.name, self.timeout
                    )))
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(err(format!("MCP server {:?} closed the connection", self.name)))
                }
            };
            // Notifications and server-initiated requests are not ours to answer
            // in v1; ignore them rather than mistaking them for a reply.
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                if value.get("method").is_some() {
                    continue;
                }
                if value.get("id") != Some(&json!(id)) {
                    continue;
                }
            }
            let response = Response::parse(&line)
                .map_err(|e| err(format!("{method}: unparseable reply: {e}")))?;
            if let Some(error) = response.error {
                return Err(err(format!("{method}: server error {}: {}", error.code, error.message)));
            }
            return Ok(response.result.unwrap_or(Value::Null));
        }
    }

    fn notify(&mut self, method: &str, params: Json) -> Result<(), McpError> {
        let line = Notification::new(method, params).to_line();
        self.send(&line)
    }

    fn send(&mut self, line: &str) -> Result<(), McpError> {
        writeln!(self.stdin, "{line}")
            .and_then(|_| self.stdin.flush())
            .map_err(|e| err(format!("writing to MCP server {:?}: {e}", self.name)))
    }
}

/// A client is a live child process, so it prints as a handle rather than as a
/// value (and `unwrap_err()` in tests stays available).
impl std::fmt::Debug for McpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClient").field("name", &self.name).field("pid", &self.child.id()).finish()
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            extern "C" {
                fn syscall(num: std::ffi::c_long, ...) -> std::ffi::c_long;
            }
            const SYS_KILL: std::ffi::c_long = 62;
            let pid = self.child.id() as i64;
            unsafe {
                syscall(SYS_KILL, -pid, 9);
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// MCP content blocks → one string. Text passes through; anything else is
/// described rather than dropped, so the model knows something was there.
fn flatten(result: &Value) -> String {
    let Some(content) = result.get("content").and_then(|c| c.as_array()) else {
        return String::new();
    };
    content
        .iter()
        .map(|block| match block.get("type").and_then(|t| t.as_str()) {
            Some("text") => block.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string(),
            Some(other) => format!("[{other} content omitted]"),
            None => "[unrecognised content block]".to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// An MCP tool as a kernel tool. The name is namespaced so two servers (or a
/// server and a built-in) can never collide.
pub struct McpTool {
    name: String,
    server: String,
    tool: String,
    description: String,
    schema: Json,
    action_class: loong_schema::generated::ActionClass,
    client: Arc<Mutex<McpClient>>,
}

impl McpTool {
    pub fn new(
        server: &str,
        info: McpToolInfo,
        action_class: loong_schema::generated::ActionClass,
        client: Arc<Mutex<McpClient>>,
    ) -> Self {
        Self {
            name: format!("mcp__{server}__{}", info.name),
            server: server.to_string(),
            tool: info.name,
            description: info.description,
            schema: info.input_schema,
            action_class,
            client,
        }
    }

    pub fn server(&self) -> &str {
        &self.server
    }
}

impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn parameters(&self) -> Json {
        self.schema.clone()
    }
    /// Declared by configuration, never guessed (see `mcp::config`).
    fn action_class(&self, _args: &Json, _cwd: &std::path::Path) -> loong_schema::generated::ActionClass {
        self.action_class
    }
    fn execute(&self, args: &Json, _ctx: &ExecContext<'_>) -> Result<String, ToolError> {
        let mut client = self
            .client
            .lock()
            .map_err(|_| ToolError::Failed(format!("the {} client is poisoned", self.server)))?;
        client.call_tool(&self.tool, args).map_err(|e| ToolError::Failed(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::McpSet;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    /// The fake MCP server is a small Python script (see `tests/`): writing it in
    /// the target language would mean reimplementing the protocol in Rust just to
    /// test the client, and MCP servers are typically scripts anyway.
    fn fake_server() -> Option<PathBuf> {
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fake_mcp_server.py");
        let python = ["python3", "python"]
            .into_iter()
            .find(|p| {
                std::process::Command::new(p)
                    .arg("--version")
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false)
            })?;
        let _ = python;
        script.exists().then_some(script)
    }

    fn spec(script: &std::path::Path, mode: &str) -> McpServerSpec {
        McpServerSpec {
            name: "fake".to_string(),
            command: "python3".to_string(),
            args: vec![script.to_string_lossy().to_string(), mode.to_string()],
            env: Default::default(),
            action_class: loong_schema::generated::ActionClass::OutOfBounds,
            tools: Default::default(),
            timeout: Duration::from_secs(10),
            required: true,
        }
    }

    #[test]
    fn handshake_list_and_call_round_trip() {
        let Some(script) = fake_server() else {
            eprintln!("skipping: no python3 or no fake server fixture");
            return;
        };
        let mut client = McpClient::start(&spec(&script, "normal")).expect("the server starts");
        let tools = client.list_tools().unwrap();
        assert_eq!(tools.len(), 2, "{tools:?}");
        assert_eq!(tools[0].name, "ping");
        assert!(tools[0].description.contains("round-trip"));
        assert_eq!(tools[0].input_schema["type"], "object");

        let out = client.call_tool("ping", &json!({"message": "hello"})).unwrap();
        assert_eq!(out, "pong: hello");
    }

    #[test]
    fn a_tool_that_reports_an_error_is_an_error() {
        let Some(script) = fake_server() else { return };
        let mut client = McpClient::start(&spec(&script, "normal")).unwrap();
        let err = client.call_tool("fail", &json!({})).unwrap_err().to_string();
        assert!(err.contains("reported an error"), "{err}");
        assert!(err.contains("this tool always fails"), "{err}");
    }

    #[test]
    fn an_unknown_method_is_an_error_not_a_hang() {
        let Some(script) = fake_server() else { return };
        let mut client = McpClient::start(&spec(&script, "normal")).unwrap();
        let err = client.request("does/not/exist", json!({})).unwrap_err().to_string();
        assert!(err.contains("server error"), "{err}");
    }

    #[test]
    fn a_wedged_server_fails_the_call_on_the_deadline() {
        let Some(script) = fake_server() else { return };
        let mut server = spec(&script, "silent");
        server.timeout = Duration::from_millis(400);
        let started = Instant::now();
        let err = McpClient::start(&server).unwrap_err().to_string();
        assert!(err.contains("no answer"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_server_that_dies_is_reported() {
        let Some(script) = fake_server() else { return };
        let err = McpClient::start(&spec(&script, "die")).unwrap_err().to_string();
        assert!(err.contains("closed the connection") || err.contains("no answer"), "{err}");
    }

    #[test]
    fn a_missing_command_is_an_error() {
        let mut server = spec(std::path::Path::new("/nonexistent.py"), "normal");
        server.command = "definitely-not-a-program".to_string();
        let err = McpClient::start(&server).unwrap_err().to_string();
        assert!(err.contains("starting MCP server"), "{err}");
        assert!(err.contains("fake"), "{err}");
    }

    #[test]
    fn tools_are_namespaced_and_keep_their_declared_class() {
        let Some(script) = fake_server() else { return };
        let client = Arc::new(Mutex::new(McpClient::start(&spec(&script, "normal")).unwrap()));
        let info = McpToolInfo {
            name: "ping".to_string(),
            description: "round-trip".to_string(),
            input_schema: json!({"type": "object"}),
        };
        let tool = McpTool::new("fake", info.clone(), loong_schema::generated::ActionClass::Investigate, client.clone());
        assert_eq!(tool.name(), "mcp__fake__ping");
        assert_eq!(tool.server(), "fake");
        assert_eq!(
            tool.action_class(&json!({}), std::path::Path::new("/")),
            loong_schema::generated::ActionClass::Investigate
        );
        let ctx = ExecContext::new(std::path::Path::new("/tmp"), &loong_tools::NoConfinement);
        assert_eq!(tool.execute(&json!({"message": "hi"}), &ctx).unwrap(), "pong: hi");
    }

    #[test]
    fn a_trusted_set_produces_startable_specs() {
        let dir = std::env::temp_dir().join(format!(
            "loong-mcp-client-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(dir.join(crate::config::MCP_DIR)).unwrap();
        std::fs::write(
            dir.join(crate::config::MCP_DIR).join(crate::config::MCP_FILE),
            "[[server]]\nname = \"fake\"\ncommand = \"python3\"\nargs = [\"x.py\"]\naction_class = \"investigate\"\n",
        )
        .unwrap();
        let set = McpSet::load(&dir).unwrap();
        assert!(set.active().is_empty(), "inert before trust");
        set.trust_now().unwrap();
        let set = McpSet::load(&dir).unwrap();
        assert_eq!(set.active().len(), 1);
        assert_eq!(
            set.active()[0].class_for("anything"),
            loong_schema::generated::ActionClass::Investigate
        );
    }
}
