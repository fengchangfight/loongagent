//! Purpose: An MCP client over stdio or Streamable HTTP: JSON-RPC 2.0, deadlines, and a child that dies with the client.
//!
//! # Contract
//! - **Serves:** the kernel's registry through `McpTool`, and prompt fetching through its `PromptFetcher`.
//! - **Design:** `docs/design/mcp-client.md` §2–§6.
//! - **Upstream:** `crates/kernel/src/turn/build.rs` (`attach_mcp`).
//! - **Downstream:** `loong_protocol` for framing, `loong_tools` for the `Tool` seam, `loong_http` for the HTTP transport, and the server itself.
//! - **Verify:** `cargo test -p loong-mcp` · `./scripts/check.sh`
//!
//! Deliberately small, and honest about it. It does the handshake, lists and
//! fetches prompts, lists and calls tools — and nothing else. Resources, sampling,
//! roots and elicitation are not implemented, and a server that asks for them gets
//! JSON-RPC `-32601`, so it learns the answer instead of waiting for one.
//!
//! Four properties keep it from becoming a liability:
//!
//! * **Every reply has a deadline.** A wedged server fails the call, not the session.
//! * **The child dies with the client.** Dropping the client kills the process
//!   group, so a server cannot outlive the session that started it.
//! * **Every list is followed to its last page.** A server with more tools than one
//!   page loses none of them, which is the failure that looks like health.
//! * **A non-text block is described, not dropped.** An image becomes a file and a
//!   path, so a text-only model knows exactly what it could not see.

use crate::config::McpServerSpec;
use loong_protocol::{Notification, Request, Response};
use loong_schema::generated::Json;
use loong_tools::{ExecContext, Tool, ToolError};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The protocol revision this client speaks. The server's answer is not
/// second-guessed: MCP is designed to negotiate, not to match exactly.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Pages one list may take before the client gives up.
pub const MAX_PAGES: usize = 100;

/// Items one list may hold before the client gives up.
pub const MAX_ITEMS: usize = 10_000;

/// Above this many tools a server is deferred by default.
pub const DEFER_TOOL_THRESHOLD: usize = 10;

/// Above this much input schema a server is deferred by default.
pub const DEFER_SCHEMA_BYTES: usize = 8 * 1024;

/// Above this size a media block is described instead of written to disk.
pub const MAX_MEDIA_BYTES: usize = 5 * 1024 * 1024;

/// The JSON-RPC error code that means "this method is not implemented here".
const METHOD_NOT_FOUND: i64 = -32601;

#[derive(Debug)]
/// Purpose: A client failure with the message the operator sees.
/// Serves: every `McpClient` operation and `McpTool::execute`.
/// Design: `docs/design/mcp-client.md` §2.
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

/// Where an oversized media block's bytes are written: one directory, named after
/// the server and the session, so two sessions never collide.
/// Purpose: The directory that receives content blocks too large to inline.
/// Serves: `McpClient` when it flattens an image or a binary resource.
/// Design: `docs/design/mcp-client.md` §7.
/// Up: `McpClient::write_media`.
/// Down: the platform's temp directory.
/// Verify: `cargo test -p loong-mcp`.
pub fn media_dir() -> PathBuf {
    std::env::temp_dir().join("loongagent-mcp")
}

/// Whether a server's tools should be advertised through `tool_search` rather
/// than pasted into every request: many tools, or a large schema.
/// Purpose: Whether a server is expensive enough to defer by default.
/// Serves: `attach_mcp` in the kernel build.
/// Design: `docs/design/mcp-client.md` §6.
/// Up: `crates/kernel/src/turn/build.rs`.
/// Down: `DEFER_TOOL_THRESHOLD`, `DEFER_SCHEMA_BYTES` and the tool schemas.
/// Verify: `cargo test -p loong-mcp`.
pub fn defer_by_default(tools: &[McpToolInfo]) -> bool {
    tools.len() > DEFER_TOOL_THRESHOLD
        || tools
            .iter()
            .map(|tool| tool.input_schema.to_string().len())
            .sum::<usize>()
            > DEFER_SCHEMA_BYTES
}

/// What the server said it can do. We ask only for what it advertised: a
/// `prompts/list` sent to a server without prompts is an error the operator did
/// not ask for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// Purpose: What the server advertised during the handshake.
/// Serves: `list_prompts`, which must not ask a server that has no prompts.
/// Design: `docs/design/mcp-client.md` §2.
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
/// Design: `docs/design/mcp-client.md` §5.
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
/// Design: `docs/design/mcp-client.md` §5.
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
/// Serves: `McpTool::new` and `defer_by_default`.
/// Design: `docs/design/mcp-client.md` §4.
/// Up: `list_tools`.
/// Down: `McpTool::new`.
/// Verify: `cargo test -p loong-mcp`.
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: Json,
}

/// Purpose: The two ways the client reaches a server: a child process, or an endpoint.
/// Serves: `McpClient`'s `send` and `next_message`, which branch on it.
/// Design: `docs/design/mcp-client.md` §2.
/// Up: `McpClient::start` builds it.
/// Down: the stdio child and pipes, or the HTTP URL, headers and reply queue.
/// Verify: `cargo test -p loong-mcp`.
enum Transport {
    /// Newline-delimited JSON on the child's stdin, with its stdout on a channel.
    Stdio {
        child: Child,
        stdin: ChildStdin,
        lines: Receiver<String>,
    },
    /// One POST per message. Replies are read whole and queued, because an HTTP
    /// answer may carry more than the message that was asked for.
    Http {
        url: String,
        headers: Vec<(String, String)>,
        session: Option<String>,
        buffer: VecDeque<String>,
    },
}

/// Purpose: A live MCP server connection.
/// Serves: `McpTool` and the kernel's prompt fetcher.
/// Design: `docs/design/mcp-client.md` §2 and §8.
/// Up: `McpClient::start` in `attach_mcp`.
/// Down: `request`, `send`, `next_message` and `Drop`.
/// Verify: `cargo test -p loong-mcp`.
pub struct McpClient {
    name: String,
    transport: Transport,
    next_id: u64,
    timeout: Duration,
    capabilities: McpCapabilities,
    /// Set by a `list_changed` notification, cleared by `take_dirty`.
    dirty: bool,
    /// Counts media files, so two blocks in one session never overwrite.
    media_written: u64,
}

impl McpClient {
    /// Start the server and complete the handshake.
    /// Purpose: Open the transport, complete the handshake and record the server's capabilities.
    /// Serves: `attach_mcp` in the kernel build and the client's own tests.
    /// Design: `docs/design/mcp-client.md` §2.
    /// Up: `crates/kernel/src/turn/build.rs`.
    /// Down: `spawn_stdio`, `request`, `notify`.
    /// Verify: `cargo test -p loong-mcp`.
    pub fn start(spec: &McpServerSpec) -> Result<Self, McpError> {
        let transport = match &spec.url {
            Some(url) => Transport::Http {
                url: url.clone(),
                headers: spec
                    .headers
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect(),
                session: None,
                buffer: VecDeque::new(),
            },
            None => start_stdio(spec)?,
        };
        let mut client = Self {
            name: spec.name.clone(),
            transport,
            next_id: 1,
            timeout: spec.timeout,
            capabilities: McpCapabilities::default(),
            dirty: false,
            media_written: 0,
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
    /// Design: `docs/design/mcp-client.md` §1.
    /// Up: `KernelMcp` in `crates/kernel/src/turn/build.rs` matches a server by name.
    /// Down: the `name` field.
    /// Verify: `cargo test -p loong-mcp`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Purpose: What the server advertised at handshake.
    /// Serves: the client's tests; the kernel does not branch on it.
    /// Design: `docs/design/mcp-client.md` §2.
    /// Up: tests only.
    /// Down: the `capabilities` field.
    /// Verify: `cargo test -p loong-mcp`.
    pub fn capabilities(&self) -> McpCapabilities {
        self.capabilities
    }

    /// Purpose: Whether the server has announced a change the client has not yet applied.
    /// Serves: `Kernel::begin_turn`, which re-lists a dirty server before a turn starts.
    /// Design: `docs/design/mcp-client.md` §6.
    /// Up: `crates/kernel/src/turn/loop.rs`.
    /// Down: the `dirty` field.
    /// Verify: `cargo test -p loong-mcp`.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Purpose: Report and clear the dirty flag, so one change causes one re-list.
    /// Serves: `Kernel::begin_turn`.
    /// Design: `docs/design/mcp-client.md` §6.
    /// Up: `crates/kernel/src/turn/loop.rs`.
    /// Down: the `dirty` field.
    /// Verify: `cargo test -p loong-mcp`.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// The prompts the server offers. Empty when it never advertised any — that is
    /// an answer, not a failure.
    /// Purpose: The prompts the server offers; empty when it advertised none.
    /// Serves: `attach_mcp` in the kernel build.
    /// Design: `docs/design/mcp-client.md` §5.
    /// Up: `crates/kernel/src/turn/build.rs`.
    /// Down: `collect("prompts/list")`.
    /// Verify: `cargo test -p loong-mcp`.
    pub fn list_prompts(&mut self) -> Result<Vec<McpPromptInfo>, McpError> {
        if !self.capabilities.prompts {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for prompt in self.collect("prompts/list", "prompts")? {
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
    /// Design: `docs/design/mcp-client.md` §5.
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

    /// Purpose: The tools the server exposes, every page of them.
    /// Serves: `attach_mcp` in the kernel build.
    /// Design: `docs/design/mcp-client.md` §3 and §4.
    /// Up: `crates/kernel/src/turn/build.rs`.
    /// Down: `collect("tools/list")` and `McpToolInfo`.
    /// Verify: `cargo test -p loong-mcp`.
    pub fn list_tools(&mut self) -> Result<Vec<McpToolInfo>, McpError> {
        let mut out = Vec::new();
        for tool in self.collect("tools/list", "tools")? {
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
    /// Design: `docs/design/mcp-client.md` §4.
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
                self.flatten(&result)
            )));
        }
        let text = self.flatten(&result);
        if text.is_empty() {
            Ok(format!("{tool} returned no text content"))
        } else {
            Ok(text)
        }
    }

    /// Every page of one list. A cursor is followed until the server stops sending
    /// one; a repeated cursor or an implausible count is an error rather than a
    /// spin.
    /// Purpose: Collect every page of `tools/list` or `prompts/list`.
    /// Serves: `list_tools` and `list_prompts`.
    /// Design: `docs/design/mcp-client.md` §3.
    /// Up: `list_tools` and `list_prompts`.
    /// Down: `request` with a `cursor` parameter.
    /// Verify: `cargo test -p loong-mcp`.
    fn collect(&mut self, method: &str, key: &str) -> Result<Vec<Value>, McpError> {
        let mut items: Vec<Value> = Vec::new();
        let mut cursor: Option<String> = None;
        let mut seen: Vec<String> = Vec::new();
        for _ in 0..MAX_PAGES {
            let params = match &cursor {
                Some(cursor) => json!({"cursor": cursor}),
                None => json!({}),
            };
            let result = self.request(method, params)?;
            let page = result
                .get(key)
                .and_then(|v| v.as_array())
                .ok_or_else(|| err(format!("{method} returned no {key} array")))?;
            items.extend(page.iter().cloned());
            if items.len() > MAX_ITEMS {
                return Err(err(format!(
                    "{method}: more than {MAX_ITEMS} items; the server is not paginating sensibly"
                )));
            }
            match result.get("nextCursor").and_then(|c| c.as_str()) {
                Some(next) if !next.is_empty() => {
                    if seen.iter().any(|prev| prev == next) {
                        return Err(err(format!(
                            "{method}: the server repeated cursor {next:?}, so the list never ends"
                        )));
                    }
                    seen.push(next.to_string());
                    cursor = Some(next.to_string());
                }
                _ => return Ok(items),
            }
        }
        Err(err(format!(
            "{method}: more than {MAX_PAGES} pages; giving up"
        )))
    }

    /// Purpose: Send one JSON-RPC request and wait for its reply within the deadline.
    /// Serves: every client operation.
    /// Design: `docs/design/mcp-client.md` §2.
    /// Up: `start`, `collect`, `get_prompt` and `call_tool`.
    /// Down: `send`, `next_message`, `answer_unsupported` and `observe`.
    /// Verify: `cargo test -p loong-mcp`.
    fn request(&mut self, method: &str, params: Json) -> Result<Json, McpError> {
        let id = self.next_id;
        self.next_id += 1;
        let line = Request::new(id, method, params).to_line();
        self.send(&line)?;

        let deadline = Instant::now() + self.timeout;
        loop {
            let line = self.next_message(method, deadline)?;
            let value: Value = match serde_json::from_str(&line) {
                Ok(value) => value,
                Err(_) => continue,
            };
            // A message with a `method` is not our reply: it is a notification or a
            // request the server is making of us.
            if let Some(server_method) = value.get("method").and_then(|m| m.as_str()) {
                let server_method = server_method.to_string();
                match value.get("id") {
                    Some(reply_to) => {
                        self.answer_unsupported(reply_to.clone(), &server_method)?;
                    }
                    None => self.observe(&server_method),
                }
                continue;
            }
            if value.get("id") != Some(&json!(id)) {
                continue;
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

    /// Purpose: Record what a notification means to this client.
    /// Contract: `crates/mcp/src/client.rs`.
    fn observe(&mut self, method: &str) {
        if method.ends_with("list_changed") {
            self.dirty = true;
        }
        // `progress` and `cancelled` change nothing this client tracks, and an
        // unknown notification is not an error in JSON-RPC.
    }

    /// Answer a server-initiated request with `-32601`, so the server learns the
    /// client does not serve it instead of waiting for a reply that never comes.
    /// Purpose: Tell a server that a method it asked for is not implemented here.
    /// Serves: `request`, for `sampling/createMessage`, `roots/list`, `elicitation/create` and anything unknown.
    /// Design: `docs/design/mcp-client.md` §2.
    /// Up: `request`.
    /// Down: `send` with a JSON-RPC error.
    /// Verify: `cargo test -p loong-mcp`.
    fn answer_unsupported(&mut self, id: Value, method: &str) -> Result<(), McpError> {
        let reply = json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": METHOD_NOT_FOUND,
                "message": format!("method not found: {method}"),
            },
        });
        self.send(&reply.to_string())
    }

    /// Purpose: Send a JSON-RPC notification, which expects no reply.
    /// Contract: `crates/mcp/src/client.rs`.
    fn notify(&mut self, method: &str, params: Json) -> Result<(), McpError> {
        let line = Notification::new(method, params).to_line();
        self.send(&line)
    }

    /// Purpose: Put one message on the wire, and for HTTP read and queue the reply.
    /// Serves: `request`, `notify` and `answer_unsupported`.
    /// Design: `docs/design/mcp-client.md` §2.
    /// Up: those three.
    /// Down: the child's stdin, or `post_json_reply`.
    /// Verify: `cargo test -p loong-mcp`.
    fn send(&mut self, line: &str) -> Result<(), McpError> {
        let timeout = self.timeout;
        let name = self.name.clone();
        match &mut self.transport {
            Transport::Stdio { stdin, .. } => writeln!(stdin, "{line}")
                .and_then(|_| stdin.flush())
                .map_err(|e| err(format!("writing to MCP server {name:?}: {e}"))),
            Transport::Http {
                url,
                headers,
                session,
                buffer,
            } => {
                let mut request_headers = headers.clone();
                request_headers.push((
                    "MCP-Protocol-Version".to_string(),
                    PROTOCOL_VERSION.to_string(),
                ));
                if let Some(session) = session.as_ref() {
                    request_headers.push(("Mcp-Session-Id".to_string(), session.clone()));
                }
                let reply = loong_http::post_json_reply(url, line, &request_headers, timeout)
                    .map_err(|e| err(format!("MCP server {name:?} at {url}: {e}")))?;
                if !(200..300).contains(&reply.status) {
                    let excerpt: String = reply.body.trim().chars().take(300).collect();
                    return Err(err(format!(
                        "MCP server {name:?} at {url}: HTTP {}: {excerpt}",
                        reply.status
                    )));
                }
                if let Some(returned) = reply.header("mcp-session-id") {
                    *session = Some(returned.to_string());
                }
                if reply.body.trim().is_empty() {
                    return Ok(());
                }
                let streaming = reply
                    .header("content-type")
                    .map(|t| t.to_ascii_lowercase().contains("text/event-stream"))
                    .unwrap_or(false)
                    || reply.body.trim_start().starts_with("data:");
                if streaming {
                    for message in sse_messages(&reply.body) {
                        buffer.push_back(message);
                    }
                } else {
                    buffer.push_back(reply.body.clone());
                }
                Ok(())
            }
        }
    }

    /// Purpose: The next message from the server, within the deadline.
    /// Serves: `request`.
    /// Design: `docs/design/mcp-client.md` §2.
    /// Up: `request`.
    /// Down: the reader channel, or the queue an HTTP POST filled.
    /// Verify: `cargo test -p loong-mcp`.
    fn next_message(&mut self, method: &str, deadline: Instant) -> Result<String, McpError> {
        match &mut self.transport {
            Transport::Http { buffer, .. } => buffer.pop_front().ok_or_else(|| {
                err(format!(
                    "{method}: MCP server {:?} answered nothing",
                    self.name
                ))
            }),
            Transport::Stdio { lines, .. } => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(err(format!(
                        "{method}: no answer from MCP server {:?} within {:?}",
                        self.name, self.timeout
                    )));
                }
                match lines.recv_timeout(remaining) {
                    Ok(line) => Ok(line),
                    Err(RecvTimeoutError::Timeout) => Err(err(format!(
                        "{method}: no answer from MCP server {:?} within {:?}",
                        self.name, self.timeout
                    ))),
                    Err(RecvTimeoutError::Disconnected) => Err(err(format!(
                        "MCP server {:?} closed the connection",
                        self.name
                    ))),
                }
            }
        }
    }

    /// MCP content blocks → one string. Text passes through; anything else is
    /// described rather than dropped, so the model knows something was there — and
    /// binary content becomes a file it can name.
    /// Purpose: Flatten a tool result's content to text, describing non-text blocks.
    /// Serves: `call_tool`.
    /// Design: `docs/design/mcp-client.md` §4.
    /// Up: `call_tool`.
    /// Down: `render_block` and the model-facing tool result.
    /// Verify: `cargo test -p loong-mcp`.
    fn flatten(&mut self, result: &Value) -> String {
        let mut parts: Vec<String> = match result.get("content").and_then(|c| c.as_array()) {
            Some(blocks) => blocks.iter().map(|b| self.render_block(b)).collect(),
            None => Vec::new(),
        };
        // A server may send the same answer twice: once as text for humans and once
        // as structured data. Keeping both means a model that only reads text loses
        // nothing.
        if let Some(structured) = result.get("structuredContent") {
            if !structured.is_null() {
                let pretty = serde_json::to_string_pretty(structured)
                    .unwrap_or_else(|_| structured.to_string());
                parts.push(format!("structured content:\n{pretty}"));
            }
        }
        parts
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Purpose: Describe one content block, writing binary payloads to a file.
    /// Serves: `flatten`, so a text-only model learns what it could not see.
    /// Design: `docs/design/mcp-client.md` §4.
    /// Up: `flatten`.
    /// Down: `store_payload` for binary blocks.
    /// Verify: `cargo test -p loong-mcp`.
    fn render_block(&mut self, block: &Value) -> String {
        match block.get("type").and_then(|t| t.as_str()) {
            Some("text") => block
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
            Some(kind @ ("image" | "audio")) => self.store_payload(block, kind, "data"),
            Some("resource") => {
                let resource = block.get("resource").cloned().unwrap_or(Value::Null);
                let uri = resource
                    .get("uri")
                    .and_then(|u| u.as_str())
                    .unwrap_or("resource")
                    .to_string();
                match resource.get("text").and_then(|t| t.as_str()) {
                    Some(text) => format!("[{uri}]\n{text}"),
                    None => {
                        let described = self.store_payload(&resource, "resource", "blob");
                        format!("[{uri}] {described}")
                    }
                }
            }
            Some("resource_link") => format!(
                "[resource link] {} ({})",
                block.get("uri").and_then(|u| u.as_str()).unwrap_or("?"),
                block
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("unnamed")
            ),
            Some(other) => format!("[{other} content omitted]"),
            None => "[unrecognised content block]".to_string(),
        }
    }

    /// Purpose: Decode a base64 payload and write it to a file, or explain why not.
    /// Serves: `render_block`, for `image`, `audio` and binary `resource` blocks.
    /// Design: `docs/design/mcp-client.md` §4 and §7.
    /// Up: `render_block`.
    /// Down: `decode_base64`, `write_media` and `MAX_MEDIA_BYTES`.
    /// Verify: `cargo test -p loong-mcp`.
    fn store_payload(&mut self, holder: &Value, kind: &str, field: &str) -> String {
        let mime = holder
            .get("mimeType")
            .and_then(|m| m.as_str())
            .unwrap_or("application/octet-stream")
            .to_string();
        let Some(text) = holder.get(field).and_then(|d| d.as_str()) else {
            return format!("[{kind} {mime}: no {field} payload]");
        };
        let bytes = match decode_base64(text) {
            Some(bytes) => bytes,
            None => {
                return format!(
                    "[{kind} {mime}: {} of undecodable base64]",
                    human_size(text.len())
                )
            }
        };
        let size = human_size(bytes.len());
        if bytes.len() > MAX_MEDIA_BYTES {
            return format!(
                "[{kind} {mime}: {size}, above the {MAX_MEDIA_BYTES}-byte limit, not written]"
            );
        }
        match self.write_media(&mime, &bytes) {
            Some(path) => format!("[{kind} {mime} → {} ({size})]", path.display()),
            None => format!("[{kind} {mime}: {size}, could not be written]"),
        }
    }

    /// Purpose: Write a payload under the media directory and return its path.
    /// Serves: `store_payload`.
    /// Design: `docs/design/mcp-client.md` §7.
    /// Up: `store_payload`.
    /// Down: `media_dir`, `extension` and the session counter.
    /// Verify: `cargo test -p loong-mcp`.
    fn write_media(&mut self, mime: &str, bytes: &[u8]) -> Option<PathBuf> {
        let dir = media_dir();
        std::fs::create_dir_all(&dir).ok()?;
        self.media_written += 1;
        let safe: String = self
            .name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let path = dir.join(format!(
            "{safe}-{}-{}.{}",
            std::process::id(),
            self.media_written,
            extension(mime)
        ));
        std::fs::write(&path, bytes).ok()?;
        Some(path)
    }
}

/// A client is a live child process, so it prints as a handle rather than as a
/// value (and `unwrap_err()` in tests stays available).
impl std::fmt::Debug for McpClient {
    /// Purpose: Print the client as a handle (name and pid or URL), not as a value.
    /// Contract: `crates/mcp/src/client.rs`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut out = f.debug_struct("McpClient");
        out.field("name", &self.name);
        match &self.transport {
            Transport::Stdio { child, .. } => out.field("pid", &child.id()),
            Transport::Http { url, .. } => out.field("url", url),
        };
        out.finish()
    }
}

impl Drop for McpClient {
    /// Purpose: Kill a stdio server's whole process group and reap the child.
    /// Serves: session shutdown, so a server cannot outlive the session that started it.
    /// Design: `docs/design/mcp-client.md` §8.
    /// Up: Rust calls it when the last `Arc<Mutex<McpClient>>` drops.
    /// Down: `kill(-pid)` on Unix and `taskkill /T` on Windows.
    /// Verify: `cargo test -p loong-mcp`.
    fn drop(&mut self) {
        let Transport::Stdio { child, .. } = &mut self.transport else {
            // An HTTP session has nothing local to kill; it expires on the server.
            return;
        };
        #[cfg(unix)]
        {
            extern "C" {
                /// Purpose: The libc `syscall` declaration used to signal the whole process group.
                fn syscall(num: std::ffi::c_long, ...) -> std::ffi::c_long;
            }
            const SYS_KILL: std::ffi::c_long = 62;
            let pid = child.id() as i64;
            unsafe {
                syscall(SYS_KILL, -pid, 9);
            }
        }
        // Windows has no kill(-pgid): `taskkill /T` follows the parent links instead, so
        // the `npx`/`node` pair behind a server does not outlive the session.
        #[cfg(windows)]
        {
            let pid = child.id().to_string();
            let _ = std::process::Command::new("taskkill")
                .args(["/T", "/F", "/PID", pid.as_str()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Purpose: Spawn a stdio server in its own process group.
/// Serves: `start_stdio`.
/// Design: `docs/design/mcp-client.md` §8.
/// Up: `start_stdio`.
/// Down: the manifest's command, args and env.
/// Verify: `cargo test -p loong-mcp`.
fn spawn_stdio(spec: &McpServerSpec) -> Result<Child, McpError> {
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
    command.spawn().map_err(|e| {
        err(format!(
            "starting MCP server {:?} ({}): {e}",
            spec.name, spec.command
        ))
    })
}

/// Purpose: Spawn the child, take its pipes and attach a reader thread.
/// Serves: `McpClient::start`, for the command transport.
/// Design: `docs/design/mcp-client.md` §2.
/// Up: `McpClient::start`.
/// Down: `spawn_stdio` and the reader thread that fills the channel.
/// Verify: `cargo test -p loong-mcp`.
fn start_stdio(spec: &McpServerSpec) -> Result<Transport, McpError> {
    let mut child = spawn_stdio(spec)?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| err("no stdin on the server"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| err("no stdout on the server"))?;
    // A reader thread turns "the server is thinking" into a deadline rather than a
    // hang. Its stderr is dropped: a server that logs is not the session's log, and
    // a full pipe would wedge it.
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
    Ok(Transport::Stdio {
        child,
        stdin,
        lines,
    })
}

/// Purpose: Pull the JSON-RPC messages out of an SSE body.
/// Serves: `McpClient::send`, for the HTTP transport.
/// Design: `docs/design/mcp-client.md` §2.
/// Up: `send`.
/// Down: the reply body's `data:` lines.
/// Verify: `cargo test -p loong-mcp`.
fn sse_messages(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut pending = String::new();
    for line in body.lines() {
        if let Some(data) = line.strip_prefix("data:") {
            pending.push_str(data.trim_start());
        } else if line.trim().is_empty() && !pending.is_empty() {
            out.push(std::mem::take(&mut pending));
        }
    }
    if !pending.is_empty() {
        out.push(pending);
    }
    out
}

/// Purpose: A byte count a person can read.
/// Contract: `crates/mcp/src/client.rs`.
fn human_size(bytes: usize) -> String {
    const KIB: f64 = 1024.0;
    let bytes = bytes as f64;
    if bytes >= KIB * KIB {
        format!("{:.1} MiB", bytes / (KIB * KIB))
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes / KIB)
    } else {
        format!("{bytes:.0} B")
    }
}

/// Purpose: A file extension for a media type, so a written block opens by name.
/// Serves: `write_media`.
/// Design: `docs/design/mcp-client.md` §7.
/// Up: `write_media`.
/// Down: the media type from the block.
/// Verify: `cargo test -p loong-mcp`.
fn extension(mime: &str) -> &'static str {
    match mime.split(';').next().unwrap_or("").trim() {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/mpeg" => "mp3",
        "audio/ogg" => "ogg",
        "application/pdf" => "pdf",
        _ => "bin",
    }
}

/// Purpose: Decode standard base64, ignoring whitespace and stopping at padding.
/// Serves: `store_payload`, so the client needs no extra dependency.
/// Design: `docs/design/mcp-client.md` §4.
/// Up: `store_payload`.
/// Down: the base64 text from the block.
/// Verify: `cargo test -p loong-mcp`.
fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut accumulator: u32 = 0;
    let mut bits: u32 = 0;
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b'\n' | b'\r' | b' ' | b'\t' => continue,
            _ => return None,
        } as u32;
        accumulator = (accumulator << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((accumulator >> bits) as u8);
        }
    }
    Some(out)
}

/// Map a command's arguments onto the names a prompt declared: positionally first
/// (`/review HEAD~1` → `ref=HEAD~1`), then `name=value` tokens override.
/// Purpose: Map command arguments onto the names a prompt declared.
/// Serves: the kernel's `attach_mcp`.
/// Design: `docs/design/mcp-client.md` §5.
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

/// An MCP tool as a kernel tool. The name is namespaced so two servers (or a
/// server and a built-in) can never collide.
/// Purpose: An MCP tool exposed through the kernel's `Tool` interface.
/// Serves: the kernel's registry, so an external tool passes the same PDP as a built-in.
/// Design: `docs/design/mcp-client.md` §4.
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
    /// Design: `docs/design/mcp-client.md` §4.
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
    /// Design: `docs/design/mcp-client.md` §4.
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
    use crate::config::Defer;
    use crate::config::McpSet;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    /// The fake MCP server is a small Python script (see `tests/`): writing it in
    /// the target language would mean reimplementing the protocol in Rust just to
    /// test the client, and MCP servers are typically scripts anyway.
    /// Purpose: Locate the Python fixture and an interpreter that can run it.
    /// Serves: every integration-style test in this module.
    /// Design: `docs/design/mcp-client.md` §2.
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

    /// Purpose: A server spec pointing the fixture at one mode.
    /// Serves: every client test.
    /// Design: `docs/design/mcp-client.md` §1.
    /// Up: those tests.
    /// Down: `crates/mcp/tests/fake_mcp_server.py`.
    /// Verify: `cargo test -p loong-mcp`.
    fn spec(script: &std::path::Path, mode: &str) -> McpServerSpec {
        McpServerSpec {
            name: "fake".to_string(),
            command: "python3".to_string(),
            args: vec![script.to_string_lossy().to_string(), mode.to_string()],
            env: Default::default(),
            url: None,
            headers: Default::default(),
            action_class: loong_schema::generated::ActionClass::OutOfBounds,
            tools: Default::default(),
            timeout: Duration::from_secs(10),
            required: true,
            defer: Defer::Auto,
            origin: crate::config::ManifestOrigin::Workspace,
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
            5,
            "the fixture exposes ping, fail, two verdict tools and screenshot: {tools:?}"
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

    #[test]
    /// Purpose: Pin that every page of `tools/list` is followed, so no tool is silently lost.
    fn every_page_of_a_tool_list_is_read() {
        let Some(script) = fake_server() else { return };
        let mut client = McpClient::start(&spec(&script, "paginated")).unwrap();
        let tools = client.list_tools().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["paged_a", "paged_b", "paged_c"],
            "three pages, one tool each: {names:?}"
        );

        // Prompts paginate the same way.
        let prompts = client.list_prompts().unwrap();
        let names: Vec<&str> = prompts.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["paged_review", "paged_release"], "{names:?}");
    }

    #[test]
    /// Purpose: Pin that a server-initiated request is answered with -32601 instead of being ignored.
    fn a_server_initiated_request_gets_method_not_found() {
        let Some(script) = fake_server() else { return };
        let mut client = McpClient::start(&spec(&script, "ask-back")).unwrap();
        // The fixture sends `sampling/createMessage` with id 900 before answering the
        // next request, and reports what came back.
        let out = client.call_tool("ping", &json!({"message": "hi"})).unwrap();
        assert!(
            out.contains("answered -32601"),
            "the server learnt the method is unsupported: {out}"
        );
    }

    #[test]
    /// Purpose: Pin that a `list_changed` notification makes the server dirty once.
    fn a_list_change_notification_marks_the_server_dirty() {
        let Some(script) = fake_server() else { return };
        let mut client = McpClient::start(&spec(&script, "notify")).unwrap();
        assert!(!client.is_dirty(), "nothing announced yet");
        // The fixture announces a change before answering the next request.
        client.call_tool("ping", &json!({"message": "hi"})).unwrap();
        assert!(client.take_dirty(), "the notification was noticed");
        assert!(!client.take_dirty(), "and reported only once");
    }

    #[test]
    /// Purpose: Pin that an image block becomes a file plus a description, and structured content is kept.
    fn media_blocks_become_files_and_structured_content_is_kept() {
        let Some(script) = fake_server() else { return };
        let mut client = McpClient::start(&spec(&script, "media")).unwrap();
        let out = client.call_tool("screenshot", &json!({})).unwrap();
        assert!(out.contains("image image/png →"), "{out}");
        let path = out
            .split('→')
            .nth(1)
            .and_then(|rest| rest.trim().split(' ').next())
            .expect("a path in the description");
        let written = std::fs::read(PathBuf::from(path.trim())).expect("the file exists");
        assert_eq!(written, b"hello", "the base64 payload was decoded");
        assert!(out.contains("structured content:"), "{out}");
        assert!(out.contains("\"width\": 2"), "{out}");
    }

    #[test]
    /// Purpose: Pin the deferral threshold: many tools or a large schema defers a server.
    fn deferral_is_decided_by_size() {
        let small = McpToolInfo {
            name: "a".to_string(),
            description: String::new(),
            input_schema: json!({"type": "object"}),
        };
        assert!(!defer_by_default(&[small.clone()]));
        let many: Vec<McpToolInfo> = (0..DEFER_TOOL_THRESHOLD + 1)
            .map(|i| McpToolInfo {
                name: format!("t{i}"),
                description: String::new(),
                input_schema: json!({"type": "object"}),
            })
            .collect();
        assert!(defer_by_default(&many));
        let fat = McpToolInfo {
            name: "fat".to_string(),
            description: String::new(),
            input_schema: json!({"type": "object", "description": "x".repeat(DEFER_SCHEMA_BYTES)}),
        };
        assert!(defer_by_default(&[fat]));
    }

    /// Purpose: Serve JSON-RPC over loopback HTTP for the transport test: it answers each POST
    /// with a fixed JSON-RPC reply and records the requests it saw.
    /// Serves: `the_http_transport_speaks_streamable_http`.
    /// Design: `docs/design/mcp-client.md` §2.
    /// Up: that test.
    /// Down: a loopback `TcpListener` and the recorded requests.
    /// Verify: `cargo test -p loong-mcp`.
    fn spawn_http_server() -> (String, Arc<Mutex<Vec<String>>>, std::sync::mpsc::Sender<()>) {
        use std::io::Read;
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (stop, stopped) = std::sync::mpsc::channel::<()>();
        let log = seen.clone();
        std::thread::spawn(move || loop {
            if stopped.try_recv().is_ok() {
                return;
            }
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut raw = Vec::new();
            let mut buffer = [0u8; 1024];
            // Read the head, then exactly Content-Length bytes of body.
            while !raw.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => raw.extend_from_slice(&buffer[..n]),
                }
            }
            let split = raw
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|at| at + 4)
                .unwrap_or(raw.len());
            let head = String::from_utf8_lossy(&raw[..split]).to_string();
            let length = head
                .lines()
                .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
                .and_then(|l| l.split_once(':'))
                .and_then(|(_, v)| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            while raw.len() < split + length {
                match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => raw.extend_from_slice(&buffer[..n]),
                }
            }
            let body = raw[split..].to_vec();
            let body = String::from_utf8_lossy(&body).to_string();
            log.lock().unwrap().push(format!("{head}\n{body}"));
            let request: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let method = request
                .get("method")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .to_string();
            let id = request.get("id").cloned().unwrap_or(Value::Null);
            let (status, payload, extra) = match method.as_str() {
                "initialize" => (
                    "200 OK",
                    json!({"jsonrpc":"2.0","id":id,"result":{
                        "protocolVersion": PROTOCOL_VERSION,
                        "capabilities": {"tools": {}},
                    }}),
                    "Mcp-Session-Id: sess-1\r\n",
                ),
                "tools/list" => (
                    "200 OK",
                    json!({"jsonrpc":"2.0","id":id,"result":{"tools":[
                        {"name":"ping","description":"round-trip","inputSchema":{"type":"object"}}
                    ]}}),
                    "",
                ),
                "tools/call" => (
                    "200 OK",
                    json!({"jsonrpc":"2.0","id":id,"result":{"content":[
                        {"type":"text","text":"pong over http"}
                    ]}}),
                    "",
                ),
                "notifications/initialized" => ("202 Accepted", Value::Null, ""),
                _ => (
                    "200 OK",
                    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"nope"}}),
                    "",
                ),
            };
            if status == "202 Accepted" {
                let reply = format!(
                    "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.write_all(reply.as_bytes());
            } else {
                // The reply arrives as an SSE stream, which is what a real
                // Streamable HTTP server may send.
                let body = format!("event: message\ndata: {payload}\n\n");
                let reply = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes());
            }
            let _ = stream.flush();
        });
        (url, seen, stop)
    }

    #[test]
    /// Purpose: Pin the HTTP transport: handshake, tool list, tool call, session id and SSE parsing.
    fn the_http_transport_speaks_streamable_http() {
        let (url, seen, stop) = spawn_http_server();
        let mut server = spec(std::path::Path::new("unused"), "normal");
        server.command.clear();
        server.url = Some(url);
        let mut client = McpClient::start(&server).expect("the HTTP server answers");
        assert!(client.capabilities().tools, "tools were advertised");
        let tools = client.list_tools().unwrap();
        assert_eq!(tools.len(), 1);
        let out = client.call_tool("ping", &json!({"message": "hi"})).unwrap();
        assert_eq!(out, "pong over http");

        let requests = seen.lock().unwrap().clone();
        let initialize = &requests[0];
        assert!(
            initialize.to_ascii_lowercase().contains("accept:"),
            "{initialize}"
        );
        assert!(
            requests.iter().any(|r| r.contains("MCP-Protocol-Version:")),
            "the negotiated version is sent: {requests:#?}"
        );
        assert!(
            requests
                .iter()
                .any(|r| r.contains("Mcp-Session-Id: sess-1")),
            "the session id is sent back: {requests:#?}"
        );
        assert!(
            !requests.iter().any(|r| r.contains("HTTP/1.1 202")),
            "no request line was mistaken for a header"
        );
        let _ = stop.send(());
    }
}
