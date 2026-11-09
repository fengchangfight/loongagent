//! `loong-reverse-mcp`: the reverse-engineering MCP stdio server loongagent launches.
//!
//! Purpose: Start `loong_reverse_mcp_lib::serve_stdio` over a `loong_reverse_engine::Engine`,
//! with no pre-opened target: the session's first `core_target_open` chooses it.
//! Serves: the `.reverse/mcp.toml` entry the reverse session writes (`docs/design/reverse.md` §3).
//! Design: `docs/superpowers/specs/2026-10-24-loongagent-reverse-design.md` Task 3.
//! Upstream: `loong-reverse-engine`, `loong-reverse-mcp-lib`, `loong-reverse-platform`.
//! Downstream: loongagent's MCP client (`crates/kernel/src/turn/build.rs` `attach_mcp`).
//! Verify: `cargo test -p loong-reverse-mcp-bin` and `scripts/check-reverse.sh`.

use std::path::PathBuf;
use std::sync::Arc;

use loong_reverse_engine::{Engine, Session, Settings};
use loong_reverse_evidence::ToolError;
use loong_reverse_mcp_lib::{serve_stdio, ServerOptions};

/// Purpose: Parse argv, build the engine, and serve JSON-RPC on stdio until the client hangs up.
/// Serves: the process entry point.
/// Design: `docs/superpowers/specs/2026-10-24-loongagent-reverse-design.md` Task 3 Step 3.
/// Up: the OS launcher (loongagent's MCP client spawns this binary).
/// Down: `run` and `usage`.
/// Verify: `cargo test -p loong-reverse-mcp-bin`.
fn main() -> std::process::ExitCode {
    let mut workspace: Option<PathBuf> = None;
    let mut opts = ServerOptions::from_env();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--workspace" => match args.next() {
                Some(dir) => workspace = Some(PathBuf::from(dir)),
                None => return usage("--workspace needs a directory"),
            },
            "--allow-run" => opts.allow_run = true,
            "--all-tools" => opts.all_tools = true,
            "-h" | "--help" => {
                println!(
                    "loong-reverse-mcp — reverse-engineering MCP server (stdio).\n\n\
                     Usage: loong-reverse-mcp [--workspace DIR] [--allow-run] [--all-tools]\n\n\
                     --workspace DIR  the session workspace; the target is opened later\n\
                     \x20                  by the first core_target_open call\n\
                     --allow-run      treat run-code / change-device tools as consented\n\
                     --all-tools      list every tool instead of Core + enabled toolsets"
                );
                return std::process::ExitCode::SUCCESS;
            }
            other => return usage(&format!("unknown argument: {other}")),
        }
    }
    match run(workspace.as_deref(), opts) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("loong-reverse-mcp: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Purpose: Build the engine with the trimmed surface and serve stdio until disconnect.
/// Serves: `main`.
/// Design: `docs/superpowers/specs/2026-10-24-loongagent-reverse-design.md` Task 3 Step 3.
/// Up: `main` only.
/// Down: `Settings::load`, `Engine::shared`, `serve_stdio`.
/// Verify: `cargo test -p loong-reverse-mcp-bin` (the stdio handshake test).
fn run(workspace: Option<&std::path::Path>, mut opts: ServerOptions) -> Result<(), ToolError> {
    loong_reverse_platform::sync::start_watchdog(
        loong_reverse_engine::settings::config_dir().join("logs"),
    );
    let _ = workspace; // The session's anchor; the target arrives with core_target_open.
                       // This binary exists to serve loongagent's MCP client, not an interactive one: the
                       // client knows the surface from `docs/design/reverse.md`, so list it whole instead of
                       // making every session spend a turn on `enable_toolset`.
    opts.all_tools = true;
    let session = Session::new(Settings::load());
    let engine: Arc<Engine> = Engine::shared(session, Vec::new());
    serve_stdio(engine, opts).map_err(|e| ToolError::io("stdio", e))
}

/// Purpose: Report a usage error on stderr and exit 2.
/// Contract: `crates/reverse/mcp-server/src/main.rs`.
fn usage(msg: &str) -> std::process::ExitCode {
    eprintln!("loong-reverse-mcp: error: {msg}");
    std::process::ExitCode::from(2)
}
