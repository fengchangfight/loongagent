//! Purpose: MCP: external tools behind the same `Tool` interface as the built-ins.
//!
//! # Contract
//! - **Serves:** the kernel's registry and the CLI's `mcp` command.
//! - **Design:** `docs/design/mcp-client.md` and `docs/design/security-trust.md` §1.
//! - **Upstream:** `crates/kernel/src/turn/build.rs` (`attach_mcp`) and `crates/cli/src/mcp.rs`.
//! - **Downstream:** `client` (JSON-RPC over stdio or HTTP) and `config` (manifest and trust gate).
//! - **Verify:** `cargo test -p loong-mcp` · `./scripts/check.sh`
//!
//! MCP (T-15): external tools behind the same interface as the built-ins.
//!
//! The point is not the protocol; it is that an integration becomes *just another
//! tool*. It goes through the registry, it is subject to the same PDP verdict, it
//! emits the same `tool.call` / `tool.result` events, and it inherits the same
//! confinement policy for anything it spawns. Nothing about the kernel changes
//! to accommodate an external server, which is what makes "integrate through
//! MCP" a boundary argument rather than a promise.
//!
//! Two things are declared rather than discovered: the transport (a command, or a
//! URL — repository content either way, so it sits behind the trust gate) and each
//! tool's **action class** — the kernel cannot know what an external tool does, and
//! guessing generously would be a hole in the boundary.
//!
//! `docs/design/mcp-client.md` owns the details: the two transports, pagination,
//! result flattening, the `tool_search` deferral rule, and the protocol parts the
//! client answers with `-32601` instead of pretending to serve.

pub mod client;
pub mod config;

pub use client::{
    defer_by_default, map_prompt_arguments, media_dir, McpCapabilities, McpClient, McpError,
    McpPromptArgument, McpPromptInfo, McpTool, McpToolInfo, PROTOCOL_VERSION,
};
pub use config::{ConfigError, Defer, McpServerSpec, McpSet, MCP_DIR, MCP_FILE, MCP_TRUST_FILE};
pub use loong_manifest::TrustStatus;
