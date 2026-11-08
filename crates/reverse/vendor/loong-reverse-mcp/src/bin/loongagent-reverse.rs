//! `loongagent-reverse`: the reverse-engineering MCP server binary.
//!
//! Purpose: start `loong_reverse_mcp::serve_stdio` over a `loong_reverse_engine::Engine` built
//! from the user's settings, with the vendored reverse toolset.
//! Serves: `docs/design/reverse.md` (the reverse surface loongagent exposes).
//! Design: `docs/superpowers/specs/2026-10-24-loongagent-reverse-design.md` Task 1 Step 5.
//! Upstream: `loong-reverse-engine`, `loong-reverse-mcp`, `loong-reverse-platform`.
//! Downstream: loongagent's MCP client (Task 4) and any MCP host that spawns this binary.
//! Verify: `scripts/check-reverse.sh` (hello + tools/list over stdio).
//!
//! Vendored from craft-anything's `apps/loong-reverse-cli` serve path, trimmed to serving only.

use std::path::{Path, PathBuf};

use loong_reverse_engine::project::{Project, Purpose};
use loong_reverse_engine::{Engine, Session, Settings};
use loong_reverse_evidence::ToolError;
use loong_reverse_mcp::{ServerOptions, serve_stdio};

fn main() -> std::process::ExitCode {
    let mut project: Option<PathBuf> = None;
    let mut opts = ServerOptions::from_env();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--allow-run" => opts.allow_run = true,
            "--all-tools" => opts.all_tools = true,
            "--project" => match args.next() {
                Some(p) => project = Some(PathBuf::from(p)),
                None => return usage("--project needs a directory"),
            },
            "-h" | "--help" => {
                println!(
                    "loongagent-reverse — reverse-engineering MCP server (stdio).\n\n\
                     Usage: loongagent-reverse [--allow-run] [--all-tools] [--project DIR]\n\n\
                     --allow-run    treat run-code / change-device tools as consented\n\
                     --all-tools    list every tool instead of Core + enabled toolsets\n\
                     --project DIR  open the project in DIR (created when missing)"
                );
                return std::process::ExitCode::SUCCESS;
            }
            other => return usage(&format!("unknown argument: {other}")),
        }
    }
    match run(project.as_deref(), opts) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("loongagent-reverse: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Purpose: build the engine and serve stdio until the client disconnects.
/// Contract: loads the user's settings; `--project` opens or creates the project in DIR.
fn run(project: Option<&Path>, opts: ServerOptions) -> Result<(), ToolError> {
    loong_reverse_platform::sync::start_watchdog(
        loong_reverse_engine::settings::config_dir().join("logs"),
    );
    let mut session = Session::new(Settings::load());
    if let Some(dir) = project {
        session.open_project(open_or_create_project(dir)?)?;
    }
    let engine = Engine::shared(session, Vec::new());
    serve_stdio(engine, opts).map_err(|e| ToolError::io("stdio", e))
}

/// Purpose: open `dir`'s project, creating one when `project.toml` is absent.
/// Contract: `dir` must be a directory the process can write; never touches other paths.
fn open_or_create_project(dir: &Path) -> Result<Project, ToolError> {
    let dir = std::path::absolute(dir).map_err(|e| ToolError::io(dir.display(), e))?;
    if dir.join("project.toml").is_file() {
        return Project::open(&dir);
    }
    let name = dir
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".into());
    Project::create(&dir, &name, Purpose::default())
}

/// Purpose: report a usage error on stderr and exit 2.
/// Contract: never returns.
fn usage(msg: &str) -> std::process::ExitCode {
    eprintln!("loongagent-reverse: error: {msg}");
    std::process::ExitCode::from(2)
}
