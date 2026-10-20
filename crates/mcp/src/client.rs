//! Purpose: A stdio MCP client: JSON-RPC 2.0 with a deadline on every read and a child that dies with the client.
//!
//! # Contract
//! - **Serves:** the kernel's registry through `McpTool`, and prompt fetching through its `PromptFetcher`.
//! - **Design:** `docs/design/extension-model.md` §3 and `docs/design/security-trust.md` §1.
//! - **Upstream:** `crates/kernel/src/turn/build.rs` (`attach_mcp`).
//! - **Downstream:** `loong_protocol` for framing, `loong_tools` for the `Tool` seam, and the child process.
//! - **Verify:** `cargo test -p loong-mcp` · `./scripts/check.sh`
//!
//! A stdio MCP client: JSON-RPC 2.0 over newline-delimited messages.
//!
//! Deliberately small. It does the handshake, lists and fetches prompts, lists and
//! calls tools, and nothing else — no resources, no sampling — and it does not serve
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
/// Purpose: A client failure with the message the operator sees.
/// Serves: every `McpClient` operation and `McpTool::execute`.
/// Design: `docs/design/extension-model.md` §3.
/// Up: `McpClient` methods construct it.
/// Down: `ToolError::Failed`.
/// Verify: `cargo test -p loong-mcp`.
pub struct McpError(pub String);

impl std::fmt::Display for McpError {
    /// Purpose: Show the failure message.
    /// Contract: `crates/mcp/src/client.rs`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for McpError {}

/// Purpose: Wrap a message in `McpError`.
/// Contract: `crates/mcp/src/client.rs`.
fn err(message: impl Into<String>) -> McpError {
    McpError(message.into())
}

/// What the server said it can do. We ask only for what it advertised: a
/// `prompts/list` sent to a server without prompts is an error the operator did
/// not ask for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// Purpose: What the server advertised during the handshake.
/// Serves: `list_prompts`, which must not ask a server that has no prompts.
/// Design: `docs/design/extension-model.md` §3.
/// Up: `McpClient::start` sets it; `McpClient::capabilities` returns it.
/// Down: the `prompts` check in `list_prompts`.
/// Verify: `cargo test -p loong-mcp`.
pub struct McpCapabilities {
    pub tools: bool,
    pub prompts: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Purpose: One argument a prompt declared.
/// Serves: `McpPromptInfo` and the kernel's argument mapping.
/// Design: `docs/design/extension-model.md` §2.
/// Up: `list_prompts` parses it.
/// Down: the kernel's `attach_mcp`, which maps `/name args` onto it.
/// Verify: `cargo test -p loong-mcp`.
pub struct McpPromptArgument {
    pub name: String,
    pub description: String,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Purpose: One prompt the server offers.
/// Serves: the kernel's MCP prompt catalogue.
/// Design: `docs/design/extension-model.md` §2 and §3.
/// Up: `list_prompts`.
/// Down: the kernel's `attach_mcp`.
/// Verify: `cargo test -p loong-mcp`.
pub struct McpPromptInfo {
    pub name: String,
    pub description: String,
    pub arguments: Vec<McpPromptArgument>,
}

#[derive(Debug, Clone, PartialEq)]
/// Purpose: One tool the server offers, with its input schema.
/// Serves: `McpTool::new`.
/// Design: `docs/design/extension-model.md` §3.
/// Up: `list_tools`.
/// Down: `McpTool::new`.
/// Verify: `cargo test -p loong-mcp`.
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: Json,
}

/// Purpose: A live stdio server connection.
/// Serves: `McpTool` and the kernel's prompt fetcher.
/// Design: `docs/design/extension-model.md` §3 and `docs/design/security-trust.md` §1.
/// Up: `McpClient::start` in `attach_mcp`.
/// Down: `request`, `notify`, `send` and `Drop`.
/// Verify: `cargo test -p loong-mcp`.
pub struct McpClient {
    name: String,
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
    timeout: Duration,
    capabilities: McpCapabilities,
}

impl McpClient {
    /// Start the server and complete the handshake.
    /// Purpose: Spawn the server, complete the handshake and record its capabilities.
    /// Serves: `attach_mcp` in the kernel build and the client's own tests.
    /// Design: `docs/design/extension-model.md` §3.
    /// Up: `crates/kernel/src/turn/build.rs`.
    /// Down: `request`, `notify` and the reader thread.
    /// Verify: `cargo test -p loong-mcp`.
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
        // Windows: an MCP server is a tree of processes like any other child (npx
        // spawns node, a wrapper spawns the real server), so it leads its own group for
        // `taskkill /T` in `Drop` — and so a console control event aimed at the agent
        // does not reach it.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            command.creation_flags(CREATE_NEW_PROCESS_GROUP);
        }
        let mut child = command.spawn().map_err(|e| {
            err(format!(
                "starting MCP server {:?} ({}): {e}",
                spec.name, spec.command
            ))
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| err("no stdin on the server"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| err("no stdout on the server"))?;

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
            capabilities: McpCapabilities::default(),
        };
        let handshake = client.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "loongagent", "version": env!("CARGO_PKG_VERSION")},
            }),
        )?;
        let advertised = handshake.get("capabilities");
        client.capabilities = McpCapabilities {
            tools: advertised.and_then(|c| c.get("tools")).is_some(),
            prompts: advertised.and_then(|c| c.get("prompts")).is_some(),
        };
        client.notify("notifications/initialized", json!({}))?;
        Ok(client)
    }

    /// Purpose: The configured server name.
    /// Serves: `KernelMcp` in `crates/kernel/src/turn/build.rs`, which matches a server by name.
    /// Design: `docs/design/extension-model.md` §3.
    /// Up: `KernelMcp` in `crates/kernel/src/turn/build.rs` matches a server by name.
    /// Down: the `name` field.
    /// Verify: `cargo test -p loong-mcp`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Purpose: What the server advertised at handshake.
    /// Serves: the client's tests; the kernel does not branch on it.
    /// Design: `docs/design/extension-model.md` §3.
    /// Up: tests only.
    /// Down: the `capabilities` field.
    /// Verify: `cargo test -p loong-mcp`.
    pub fn capabilities(&self) -> McpCapabilities {
        self.capabilities
    }

    /// The prompts the server offers. Empty when it never advertised any — that is
    /// an answer, not a failure.
    /// Purpose: The prompts the server offers; empty when it advertised none.
    /// Serves: `attach_mcp` in the kernel build.
    /// Design: `docs/design/extension-model.md` §2 and §3.
    /// Up: `crates/kernel/src/turn/build.rs`.
    /// Down: `request("prompts/list")`.
    /// Verify: `cargo test -p loong-mcp`.
    pub fn list_prompts(&mut self) -> Result<Vec<McpPromptInfo>, McpError> {
        if !self.capabilities.prompts {
            return Ok(Vec::new());
        }
        let result = self.request("prompts/list", json!({}))?;
        let prompts = result
            .get("prompts")
            .and_then(|p| p.as_array())
            .ok_or_else(|| err("prompts/list returned no prompts array"))?;
        let mut out = Vec::new();
        for prompt in prompts {
            let name = prompt
                .get("name")
                .and_then(|n| n.as_str())
                .ok_or_else(|| err("a prompt has no name"))?
                .to_string();
            let arguments = prompt
                .get("arguments")
                .and_then(|a| a.as_array())
                .map(|list| {
                    list.iter()
                        .filter_map(|arg| {
                            let name = arg.get("name")?.as_str()?.to_string();
                            Some(McpPromptArgument {
                                name,
                                description: arg
                                    .get("description")
                                    .and_then(|d| d.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                required: arg
                                    .get("required")
                                    .and_then(|r| r.as_bool())
                                    .unwrap_or(false),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            out.push(McpPromptInfo {
                name,
                description: prompt
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("(no description)")
                    .to_string(),
                arguments,
            });
        }
        Ok(out)
    }

    /// Fetch a prompt's messages as text. A prompt is instructions the server
    /// author wrote, so the same rule as any other tool output applies: it is
    /// content, and which server it came from is recorded.
    /// Purpose: Fetch a prompt's messages as one text body.
    /// Serves: `KernelMcp` through `PromptFetcher`.
    /// Design: `docs/design/extension-model.md` §2.
    /// Up: the kernel's `PromptFetcher` impl.
    /// Down: `request("prompts/get")`.
    /// Verify: `cargo test -p loong-mcp`.
    pub fn get_prompt(
        &mut self,
        name: &str,
        arguments: &serde_json::Map<String, Value>,
    ) -> Result<String, McpError> {
        let result = self.request(
            "prompts/get",
            json!({"name": name, "arguments": Value::Object(arguments.clone())}),
        )?;
        let messages = result
            .get("messages")
            .and_then(|m| m.as_array())
            .ok_or_else(|| err(format!("prompt {name:?} returned no messages")))?;
        let mut parts: Vec<(String, String)> = Vec::new();
        for message in messages {
            let role = message
                .get("role")
                .and_then(|r| r.as_str())
                .unwrap_or("user")
                .to_string();
            let text = match message.get("content") {
                Some(Value::Array(blocks)) => blocks
                    .iter()
                    .filter_map(|block| match block.get("type").and_then(|t| t.as_str()) {
                        Some("text") => block.get("text").and_then(|t| t.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                Some(Value::Object(_)) => message["content"]
                    .get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string(),
                _ => String::new(),
            };
            parts.push((role, text));
        }
        // A single user message *is* the command's prompt; anything richer keeps
        // its speakers visible rather than silently merging them.
        if parts.len() == 1 && parts[0].0 == "user" {
            return Ok(parts.remove(0).1);
        }
        Ok(parts
            .into_iter()
            .map(|(role, text)| format!("[{role}] {text}"))
            .collect::<Vec<_>>()
            .join("\n\n"))
    }

    /// Purpose: The tools the server exposes.
    /// Serves: `attach_mcp` in the kernel build.
    /// Design: `docs/design/extension-model.md` §3.
    /// Up: `crates/kernel/src/turn/build.rs`.
    /// Down: `request("tools/list")` and `McpToolInfo`.
    /// Verify: `cargo test -p loong-mcp`.
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
    /// Purpose: Call a tool and flatten its content, turning `isError` into an error.
    /// Serves: `McpTool::execute`.
    /// Design: `docs/design/extension-model.md` §3.
    /// Up: `McpTool::execute`.
    /// Down: `request("tools/call")` and `flatten`.
    /// Verify: `cargo test -p loong-mcp`.
    pub fn call_tool(&mut self, tool: &str, args: &Json) -> Result<String, McpError> {
        let result = self.request("tools/call", json!({"name": tool, "arguments": args}))?;
        if result
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return Err(err(format!(
                "{tool} reported an error: {}",
                flatten(&result)
            )));
        }
        let text = flatten(&result);
        if text.is_empty() {
            Ok(format!("{tool} returned no text content"))
        } else {
            Ok(text)
        }
    }

    /// Purpose: Send one JSON-RPC request and wait for its reply within the deadline.
    /// Serves: every client operation.
    /// Design: `docs/design/extension-model.md` §3.
    /// Up: `start`, `list_prompts`, `get_prompt`, `list_tools` and `call_tool`.
    /// Down: `send`, `Response::parse` and the reader channel.
    /// Verify: `cargo test -p loong-mcp`.
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
                    return Err(err(format!(
                        "MCP server {:?} closed the connection",
                        self.name
                    )))
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
                return Err(err(format!(
                    "{method}: server error {}: {}",
                    error.code, error.message
                )));
            }
            return Ok(response.result.unwrap_or(Value::Null));
        }
    }

    /// Purpose: Send a JSON-RPC notification, which expects no reply.
    /// Contract: `crates/mcp/src/client.rs`.
    fn notify(&mut self, method: &str, params: Json) -> Result<(), McpError> {
        let line = Notification::new(method, params).to_line();
        self.send(&line)
    }

    /// Purpose: Write one newline-delimited message and flush the pipe.
    /// Contract: `crates/mcp/src/client.rs`.
    fn send(&mut self, line: &str) -> Result<(), McpError> {
        writeln!(self.stdin, "{line}")
            .and_then(|_| self.stdin.flush())
            .map_err(|e| err(format!("writing to MCP server {:?}: {e}", self.name)))
    }
}

/// A client is a live child process, so it prints as a handle rather than as a
/// value (and `unwrap_err()` in tests stays available).
impl std::fmt::Debug for McpClient {
    /// Purpose: Print the client as a handle (name and pid), not as a value.
    /// Contract: `crates/mcp/src/client.rs`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClient")
            .field("name", &self.name)
            .field("pid", &self.child.id())
            .finish()
    }
}

impl Drop for McpClient {
    /// Purpose: Kill the server's whole process group and reap the child.
    /// Serves: session shutdown, so a server cannot outlive the session that started it.
    /// Design: `docs/design/extension-model.md` §3.
    /// Up: Rust calls it when the last `Arc<Mutex<McpClient>>` drops.
    /// Down: `kill(-pid)` on Unix and `taskkill /T` on Windows.
    /// Verify: `cargo test -p loong-mcp`.
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            extern "C" {
                /// Purpose: The libc `syscall` declaration used to signal the whole process group.
                fn syscall(num: std::ffi::c_long, ...) -> std::ffi::c_long;
            }
            const SYS_KILL: std::ffi::c_long = 62;
            let pid = self.child.id() as i64;
            unsafe {
                syscall(SYS_KILL, -pid, 9);
            }
        }
        // Windows has no kill(-pgid): `taskkill /T` follows the parent links instead, so
        // the `npx`/`node` pair behind a server does not outlive the session.
        #[cfg(windows)]
        {
            let pid = self.child.id().to_string();
            let _ = std::process::Command::new("taskkill")
                .args(["/T", "/F", "/PID", pid.as_str()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Map a command's arguments onto the names a prompt declared: positionally first
/// (`/review HEAD~1` → `ref=HEAD~1`), then `name=value` tokens override.
/// Purpose: Map command arguments onto the names a prompt declared.
/// Serves: the kernel's `attach_mcp`.
/// Design: `docs/design/extension-model.md` §2.
/// Up: `crates/kernel/src/turn/build.rs`.
/// Down: the returned JSON argument map.
/// Verify: `cargo test -p loong-mcp`.
pub fn map_prompt_arguments(names: &[String], args: &str) -> serde_json::Map<String, Value> {
    let mut map = serde_json::Map::new();
    let mut positional = Vec::new();
    for token in args.split_whitespace() {
        match token.split_once('=') {
            Some((key, value)) if names.iter().any(|n| n == key) => {
                map.insert(key.to_string(), Value::String(value.to_string()));
            }
            _ => positional.push(token.to_string()),
        }
    }
    for (index, value) in positional.into_iter().enumerate() {
        if let Some(name) = names.get(index) {
            map.entry(name.clone()).or_insert(Value::String(value));
        }
    }
    map
}

/// MCP content blocks → one string. Text passes through; anything else is
/// described rather than dropped, so the model knows something was there.
/// Purpose: Flatten MCP content blocks to text, describing non-text blocks rather than dropping them.
/// Serves: `call_tool`.
/// Design: `docs/design/extension-model.md` §3.
/// Up: `call_tool`.
/// Down: the model-facing tool result.
/// Verify: `cargo test -p loong-mcp`.
fn flatten(result: &Value) -> String {
    let Some(content) = result.get("content").and_then(|c| c.as_array()) else {
        return String::new();
    };
    content
        .iter()
        .map(|block| match block.get("type").and_then(|t| t.as_str()) {
            Some("text") => block
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
            Some(other) => format!("[{other} content omitted]"),
            None => "[unrecognised content block]".to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// An MCP tool as a kernel tool. The name is namespaced so two servers (or a
/// server and a built-in) can never collide.
/// Purpose: An MCP tool exposed through the kernel's `Tool` interface.
/// Serves: the kernel's registry, so an external tool passes the same PDP as a built-in.
/// Design: `docs/design/extension-model.md` §3.
/// Up: `McpTool::new` in `attach_mcp`.
/// Down: `Tool::execute` and `Tool::action_class`.
/// Verify: `cargo test -p loong-mcp`.
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
    /// Purpose: Build the tool with its namespaced name and declared class.
    /// Serves: `attach_mcp`.
    /// Design: `docs/design/extension-model.md` §3.
    /// Up: `crates/kernel/src/turn/build.rs`.
    /// Down: the `name`, `tool` and `client` fields.
    /// Verify: `cargo test -p loong-mcp`.
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

    /// Purpose: The server this tool came from.
    /// Serves: the crate's tests; the kernel does not read it.
    /// Design: `docs/design/extension-model.md` §3.
    /// Up: tests only.
    /// Down: the `server` field.
    /// Verify: `cargo test -p loong-mcp`.
    pub fn server(&self) -> &str {
        &self.server
    }
}

impl Tool for McpTool {
    /// Purpose: The namespaced name the registry files the tool under.
    /// Contract: `crates/mcp/src/client.rs`.
    fn name(&self) -> &str {
        &self.name
    }
    /// Purpose: The description the model reads.
    /// Contract: `crates/mcp/src/client.rs`.
    fn description(&self) -> &str {
        &self.description
    }
    /// Purpose: The input schema the server declared.
    /// Contract: `crates/mcp/src/client.rs`.
    fn parameters(&self) -> Json {
        self.schema.clone()
    }
    /// Declared by configuration, never guessed (see `mcp::config`).
    /// Purpose: The class from `mcp.toml`, never a guess from the arguments.
    /// Contract: `crates/mcp/src/client.rs`.
    fn action_class(
        &self,
        _args: &Json,
        _cwd: &std::path::Path,
    ) -> loong_schema::generated::ActionClass {
        self.action_class
    }
    /// Purpose: Call the tool through the locked connection.
    /// Contract: `crates/mcp/src/client.rs`.
    fn execute(&self, args: &Json, _ctx: &ExecContext<'_>) -> Result<String, ToolError> {
        let mut client = self
            .client
            .lock()
            .map_err(|_| ToolError::Failed(format!("the {} client is poisoned", self.server)))?;
        client
            .call_tool(&self.tool, args)
            .map_err(|e| ToolError::Failed(e.to_string()))
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
    /// Purpose: Locate the Python fixture and an interpreter that can run it.
    /// Serves: every integration-style test in this module.
    /// Design: `docs/design/extension-model.md` §3.
    /// Up: those tests.
    /// Down: `crates/mcp/tests/fake_mcp_server.py`.
    /// Verify: `cargo test -p loong-mcp`.
    fn fake_server() -> Option<PathBuf> {
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fake_mcp_server.py");
        let python = ["python3", "python"].into_iter().find(|p| {
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

    /// Purpose: A server spec pointing at the fixture in one mode.
    /// Contract: `crates/mcp/src/client.rs`.
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
    /// Purpose: Pin the handshake, tool listing and tool call round trip.
    fn handshake_list_and_call_round_trip() {
        let Some(script) = fake_server() else {
            eprintln!("skipping: no python3 or no fake server fixture");
            return;
        };
        let mut client = McpClient::start(&spec(&script, "normal")).expect("the server starts");
        let tools = client.list_tools().unwrap();
        assert_eq!(
            tools.len(),
            4,
            "the fixture exposes ping, fail and two verdict tools: {tools:?}"
        );
        assert_eq!(tools[0].name, "ping");
        assert!(tools[0].description.contains("round-trip"));
        assert_eq!(tools[0].input_schema["type"], "object");

        let out = client
            .call_tool("ping", &json!({"message": "hello"}))
            .unwrap();
        assert_eq!(out, "pong: hello");
    }

    #[test]
    /// Purpose: Pin prompt listing, argument metadata and both prompt-body shapes.
    fn prompts_are_listed_and_fetched() {
        let Some(script) = fake_server() else { return };
        let mut client = McpClient::start(&spec(&script, "normal")).unwrap();
        assert!(
            client.capabilities().prompts,
            "the fixture advertises prompts"
        );
        let prompts = client.list_prompts().unwrap();
        assert_eq!(prompts.len(), 3, "{prompts:?}");
        assert_eq!(prompts[0].name, "review");
        assert_eq!(prompts[0].arguments.len(), 1);
        assert_eq!(prompts[0].arguments[0].name, "ref");
        assert!(prompts[0].arguments[0].required);

        let mut arguments = serde_json::Map::new();
        arguments.insert("ref".to_string(), json!("HEAD~1"));
        let text = client.get_prompt("review", &arguments).unwrap();
        assert_eq!(
            text,
            "Review HEAD~1 and report ONLY real bugs, each with file:line."
        );

        // A multi-message prompt keeps its speakers.
        let text = client
            .get_prompt("release", &serde_json::Map::new())
            .unwrap();
        assert!(
            text.contains("[user] Walk the release checklist."),
            "{text}"
        );
        assert!(text.contains("[assistant] Understood."), "{text}");
    }

    #[test]
    /// Purpose: Pin positional and `name=value` prompt argument mapping, including an undeclared key.
    fn prompt_arguments_map_positionally_and_by_name() {
        let names = vec!["ref".to_string(), "focus".to_string()];
        let mapped = map_prompt_arguments(&names, "HEAD~1 security");
        assert_eq!(mapped["ref"], "HEAD~1");
        assert_eq!(mapped["focus"], "security");

        let by_name = map_prompt_arguments(&names, "focus=perf HEAD~2");
        assert_eq!(
            by_name["ref"], "HEAD~2",
            "the positional token fills the free slot"
        );
        assert_eq!(by_name["focus"], "perf");

        // An undeclared `key=value` is a positional argument, not a silent drop.
        let undeclared = map_prompt_arguments(&names, "a=b");
        assert_eq!(undeclared["ref"], "a=b");
        assert!(map_prompt_arguments(&[], "anything").is_empty());
    }

    #[test]
    /// Purpose: Pin that a server without the prompts capability is never asked.
    fn a_server_without_the_prompts_capability_is_not_asked() {
        let Some(script) = fake_server() else { return };
        let mut client = McpClient::start(&spec(&script, "no-prompts")).unwrap();
        assert!(!client.capabilities().prompts);
        assert_eq!(
            client.list_prompts().unwrap(),
            Vec::new(),
            "no prompts, no error"
        );
    }

    #[test]
    /// Purpose: Pin that an `isError` tool result becomes an error carrying the message.
    fn a_tool_that_reports_an_error_is_an_error() {
        let Some(script) = fake_server() else { return };
        let mut client = McpClient::start(&spec(&script, "normal")).unwrap();
        let err = client
            .call_tool("fail", &json!({}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("reported an error"), "{err}");
        assert!(err.contains("this tool always fails"), "{err}");
    }

    #[test]
    /// Purpose: Pin that an unknown method is a server error, not a hang.
    fn an_unknown_method_is_an_error_not_a_hang() {
        let Some(script) = fake_server() else { return };
        let mut client = McpClient::start(&spec(&script, "normal")).unwrap();
        let err = client
            .request("does/not/exist", json!({}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("server error"), "{err}");
    }

    #[test]
    /// Purpose: Pin that a silent server fails the call on the deadline.
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
    /// Purpose: Pin that a server which exits is reported as a closed connection.
    fn a_server_that_dies_is_reported() {
        let Some(script) = fake_server() else { return };
        let err = McpClient::start(&spec(&script, "die"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("closed the connection") || err.contains("no answer"),
            "{err}"
        );
    }

    #[test]
    /// Purpose: Pin that a missing command is an error naming the server.
    fn a_missing_command_is_an_error() {
        let mut server = spec(std::path::Path::new("/nonexistent.py"), "normal");
        server.command = "definitely-not-a-program".to_string();
        let err = McpClient::start(&server).unwrap_err().to_string();
        assert!(err.contains("starting MCP server"), "{err}");
        assert!(err.contains("fake"), "{err}");
    }

    #[test]
    /// Purpose: Pin namespacing, the declared action class and the execute path.
    fn tools_are_namespaced_and_keep_their_declared_class() {
        let Some(script) = fake_server() else { return };
        let client = Arc::new(Mutex::new(
            McpClient::start(&spec(&script, "normal")).unwrap(),
        ));
        let info = McpToolInfo {
            name: "ping".to_string(),
            description: "round-trip".to_string(),
            input_schema: json!({"type": "object"}),
        };
        let tool = McpTool::new(
            "fake",
            info.clone(),
            loong_schema::generated::ActionClass::Investigate,
            client.clone(),
        );
        assert_eq!(tool.name(), "mcp__fake__ping");
        assert_eq!(tool.server(), "fake");
        assert_eq!(
            tool.action_class(&json!({}), std::path::Path::new("/")),
            loong_schema::generated::ActionClass::Investigate
        );
        let ctx = ExecContext::new(std::path::Path::new("/tmp"), &loong_tools::NoConfinement);
        assert_eq!(
            tool.execute(&json!({"message": "hi"}), &ctx).unwrap(),
            "pong: hi"
        );
    }

    #[test]
    /// Purpose: Pin that a trusted set yields startable specs with their declared class.
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
