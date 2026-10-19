//! Purpose: MCP: external tools behind the same `Tool` interface as the built-ins.
//!
//! # Contract
//! - **Serves:** the kernel's registry and the CLI's `mcp` command.
//! - **Design:** `docs/design/extension-model.md` §3 and `docs/design/security-trust.md` §1.
//! - **Upstream:** `crates/kernel/src/turn/build.rs` (`attach_mcp`) and `crates/cli/src/mcp.rs`.
//! - **Downstream:** `client` (stdio JSON-RPC) and `config` (manifest and trust gate).
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
//! Two things are declared rather than discovered: the server command (and it is
//! repository content, so it sits behind the trust gate) and each tool's
//! **action class** — the kernel cannot know what an external tool does, and
//! guessing generously would be a hole in the boundary.

pub mod client;
pub mod config;

pub use client::{
    map_prompt_arguments, McpCapabilities, McpClient, McpError, McpPromptArgument, McpPromptInfo,
    McpTool, McpToolInfo, PROTOCOL_VERSION,
};
pub use config::{ConfigError, McpServerSpec, McpSet, MCP_DIR, MCP_FILE, MCP_TRUST_FILE};
pub use loong_manifest::TrustStatus;
