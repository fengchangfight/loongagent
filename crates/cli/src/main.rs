//! loongagent — one binary, many faces (ADR-0009).
//!
//! ```text
//! loongagent server --stdio [--session PATH] [--cwd DIR] [--model NAME]
//! loongagent tui            [--session PATH] [--cwd DIR] [--model NAME]
//! loongagent run "PROMPT"   [--session PATH] [--cwd DIR] [--model NAME]
//! ```
//!
//! `server` owns the session; `tui` and `run` are thin clients that speak
//! JSON-RPC to it (the TUI spawns its own server child). The `demo-*` models are
//! deterministic provider doubles so the whole slice can be exercised without a
//! network or an API key; they disappear when a real adapter lands.

use loong_kernel::{Kernel, KernelConfig};
use loong_providers::{
    ChatMessage, CompletionRequest, CompletionResponse, Model, ModelError, OpenAiCompat,
    ScriptedModel, ToolCallRequest, Usage,
};
use loong_schema::generated::ApprovalScope;
use loong_server::Server;
use loong_tui::{Client, Transport};
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const USAGE: &str = "\
loongagent — a firm-internal coding agent

USAGE:
  loongagent server --stdio [--session PATH] [--cwd DIR] [--model NAME]
  loongagent tui            [--session PATH] [--cwd DIR] [--model NAME]
  loongagent run \"PROMPT\"   [--session PATH] [--cwd DIR] [--model NAME]
  loongagent --version

MODELS (provider doubles, for a first run without keys):
  echo         answers \"echo: <your prompt>\"
  demo-write   calls write_file inside the workspace, then finishes
  demo-escape  tries to write outside the workspace — the PDP will ask
  openai-compat  a real endpoint: set LOONGAGENT_BASE_URL (http://…), and
                 LOONGAGENT_MODEL / LOONGAGENT_API_KEY as needed

  loongagent sandbox self-test [--cwd DIR]   prove the OS boundary actually denies
  loongagent hooks list        [--cwd DIR]   show the effective hook set and its trust
  loongagent hooks trust       [--cwd DIR]   trust the current hook bytes (per repository)
";

fn main() {
    if let Err(err) = run() {
        eprintln!("loongagent: {err}");
        std::process::exit(1);
    }
}

#[derive(Debug, Clone)]
struct Args {
    session: PathBuf,
    cwd: PathBuf,
    model: String,
    stdio: bool,
    positional: Vec<String>,
}

impl Args {
    fn parse(rest: &[String]) -> Result<Self, String> {
        let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
        let mut args = Args {
            session: cwd.join(".loongagent").join("session.jsonl"),
            cwd: cwd.clone(),
            model: "echo".to_string(),
            stdio: false,
            positional: Vec::new(),
        };
        let mut i = 0;
        while i < rest.len() {
            match rest[i].as_str() {
                "--session" => {
                    i += 1;
                    args.session = PathBuf::from(rest.get(i).ok_or("--session needs a path")?);
                }
                "--cwd" => {
                    i += 1;
                    args.cwd = PathBuf::from(rest.get(i).ok_or("--cwd needs a path")?);
                }
                "--model" => {
                    i += 1;
                    args.model = rest.get(i).ok_or("--model needs a name")?.clone();
                }
                "--stdio" => args.stdio = true,
                other if other.starts_with("--") => {
                    return Err(format!("unknown flag {other:?}\n\n{USAGE}"))
                }
                other => args.positional.push(other.to_string()),
            }
            i += 1;
        }
        Ok(args)
    }

    fn session_id(&self) -> String {
        self.session
            .file_stem()
            .map(|s| format!("s-{}", s.to_string_lossy()))
            .unwrap_or_else(|| "s-local".to_string())
    }

    fn ensure_parent(&self) -> Result<(), String> {
        if let Some(dir) = self.session.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        Ok(())
    }
}

fn run() -> Result<(), String> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let (command, rest) =
        argv.split_first().map(|(c, r)| (c.as_str(), r)).unwrap_or(("", &[]));
    match command {
        "server" => {
            let args = Args::parse(rest)?;
            if !args.stdio {
                return Err("only `--stdio` is implemented for now".to_string());
            }
            serve(args)
        }
        "sandbox" => match rest.first().map(String::as_str) {
            Some("self-test") => sandbox_self_test(Args::parse(&rest[1..])?),
            Some("probe") => std::process::exit(loong_sandbox::sandbox::probe_main()),
            _ => Err(format!("sandbox self-test | probe\n\n{USAGE}")),
        },
        "hooks" => match rest.first().map(String::as_str) {
            Some("list") => hooks_list(Args::parse(&rest[1..])?),
            Some("trust") => hooks_trust(Args::parse(&rest[1..])?),
            _ => Err(format!("hooks list | hooks trust\n\n{USAGE}")),
        },
        "tui" => repl(Args::parse(rest)?),
        "run" => {
            let args = Args::parse(rest)?;
            let prompt = args
                .positional
                .first()
                .cloned()
                .ok_or_else(|| format!("run needs a prompt\n\n{USAGE}"))?;
            headless(args, &prompt)
        }
        "--version" | "version" => {
            println!(
                "loongagent {} (schema {})",
                env!("CARGO_PKG_VERSION"),
                loong_schema::SCHEMA_VERSION
            );
            Ok(())
        }
        _ => {
            println!("{USAGE}");
            Ok(())
        }
    }
}

// ---------------------------------------------------------------- the server

fn serve(args: Args) -> Result<(), String> {
    args.ensure_parent()?;
    // ADR-0007: the boundary is proven before the first tool call. A mechanism
    // that fails to deny is fatal; a platform without one is a loud degradation.
    match loong_hooks::HookSet::load(&args.cwd) {
        Ok(set) => eprintln!("[hooks] {}", set.summary()),
        Err(e) => return Err(format!("hooks.toml is broken: {e}")),
    }
    if let Ok(exe) = std::env::current_exe() {
        match loong_sandbox::sandbox::self_test(&exe, &args.cwd).result {
            loong_sandbox::SelfTest::Enforced { mechanism, .. } => {
                eprintln!("[sandbox] filesystem boundary enforced by {mechanism}");
            }
            loong_sandbox::SelfTest::Unavailable { detail } => {
                eprintln!("[sandbox] WARNING: unconfined — {detail}");
            }
            loong_sandbox::SelfTest::Broken { detail } => {
                return Err(format!("refusing to start: the sandbox self-test is broken — {detail}"));
            }
        }
    }
    let model = build_model(&args.model)?;
    let mut config = KernelConfig::new(&args.cwd);
    config.max_steps = 8;
    let kernel = if args.session.exists() {
        Kernel::open(&args.session, config, model)
    } else {
        Kernel::create(&args.session, config, model)
    }
    .map_err(|e| e.to_string())?;
    Server::new(kernel, args.session_id()).run_stdio().map_err(|e| e.to_string())
}

fn build_model(name: &str) -> Result<Box<dyn Model>, String> {
    match name {
        "echo" => Ok(Box::new(EchoModel)),
        "openai-compat" => Ok(Box::new(OpenAiCompat::from_env().map_err(|e| e.to_string())?)),
        "demo-write" => Ok(Box::new(ScriptedModel::new(vec![
            CompletionResponse {
                text: None,
                tool_calls: vec![ToolCallRequest {
                    call_id: "c1".to_string(),
                    tool: "write_file".to_string(),
                    args: json!({"path": "demo.txt", "content": "written by the loongagent vertical slice\n"}),
                }],
                usage: Usage { tokens_in: 20, tokens_out: 8 },
            },
            CompletionResponse {
                text: Some("wrote demo.txt".to_string()),
                tool_calls: Vec::new(),
                usage: Usage { tokens_in: 30, tokens_out: 4 },
            },
        ]))),
        "demo-escape" => Ok(Box::new(ScriptedModel::new(vec![
            CompletionResponse {
                text: None,
                tool_calls: vec![ToolCallRequest {
                    call_id: "c1".to_string(),
                    tool: "write_file".to_string(),
                    args: json!({"path": "/tmp/loongagent-demo-escape.txt", "content": "should never be written\n"}),
                }],
                usage: Usage { tokens_in: 20, tokens_out: 8 },
            },
            CompletionResponse {
                text: Some("understood".to_string()),
                tool_calls: Vec::new(),
                usage: Usage { tokens_in: 10, tokens_out: 2 },
            },
        ]))),
        other => Err(format!("unknown --model {other:?} (echo | demo-write | demo-escape)")),
    }
}

/// The plainest possible provider double: repeats the last user message. Used by
/// `run` and by the end-to-end test; needs no keys and no network.
struct EchoModel;

impl Model for EchoModel {
    fn complete(&mut self, req: &CompletionRequest) -> Result<CompletionResponse, ModelError> {
        let last_user = req
            .messages
            .iter()
            .rev()
            .find_map(|m| match m {
                ChatMessage::User { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        Ok(CompletionResponse {
            text: Some(format!("echo: {last_user}")),
            tool_calls: Vec::new(),
            usage: Usage { tokens_in: 3, tokens_out: 3 },
        })
    }
}

// ---------------------------------------------------------------- the client

/// The TUI's transport: it spawns the server as a child process and talks to it
/// over pipes. Same binary, different mode (ADR-0009).
struct ChildTransport {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl ChildTransport {
    fn spawn(args: &Args) -> Result<Self, String> {
        // Same binary by default; overridable so a *test* binary can point at the
        // built server (an integration test's current_exe is the test harness).
        let exe = match std::env::var_os("LOONGAGENT_SERVER_BIN") {
            Some(path) => PathBuf::from(path),
            None => std::env::current_exe().map_err(|e| e.to_string())?,
        };
        let mut child = Command::new(exe)
            .arg("server")
            .arg("--stdio")
            .arg("--session")
            .arg(&args.session)
            .arg("--cwd")
            .arg(&args.cwd)
            .arg("--model")
            .arg(&args.model)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("spawning the local server: {e}"))?;
        let stdin = child.stdin.take().ok_or("no stdin on the server child")?;
        let stdout = BufReader::new(child.stdout.take().ok_or("no stdout on the server child")?);
        Ok(Self { child, stdin, stdout })
    }
}

impl Transport for ChildTransport {
    fn send(&mut self, line: &str) -> Result<(), String> {
        writeln!(self.stdin, "{line}").map_err(|e| e.to_string())?;
        self.stdin.flush().map_err(|e| e.to_string())
    }

    fn recv_line(&mut self) -> Result<Option<String>, String> {
        let mut buf = String::new();
        match self.stdout.read_line(&mut buf) {
            Ok(0) => Ok(None),
            Ok(_) => Ok(Some(buf.trim_end().to_string())),
            Err(e) => Err(e.to_string()),
        }
    }
}

impl Drop for ChildTransport {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn repl(args: Args) -> Result<(), String> {
    args.ensure_parent()?;
    let transport = ChildTransport::spawn(&args)?;
    let mut client = Client::new(transport, args.session_id());
    let cwd = args.cwd.to_string_lossy().to_string();
    let attached = client.attach(&cwd, "tui")?;
    println!(
        "loongagent — session {} (schema {}, {} events). Ctrl-D to quit.",
        attached.session_id, attached.schema_version, attached.existing_events
    );

    let stdin = std::io::stdin();
    loop {
        let pending = client.pending_approval().map(|s| s.to_string());
        match &pending {
            Some(request_id) => print!("[a]pprove / [d]eny {request_id}: "),
            None => print!("> "),
        }
        std::io::stdout().flush().map_err(|e| e.to_string())?;

        let mut line = String::new();
        if stdin.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            println!();
            return Ok(());
        }
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        let result = match pending {
            Some(request_id) => {
                let granted = matches!(line.as_str(), "a" | "approve" | "y" | "yes");
                client.respond(&request_id, granted, ApprovalScope::Once, None)
            }
            None => client.submit(&line),
        };
        match result {
            Ok(lines) => {
                for l in lines {
                    println!("{l}");
                }
            }
            Err(e) => eprintln!("error: {e}"),
        }
    }
}

/// Show what would run, and whether it is trusted to.
fn hooks_list(args: Args) -> Result<(), String> {
    let set = loong_hooks::HookSet::load(&args.cwd).map_err(|e| e.to_string())?;
    println!("workspace: {}", args.cwd.display());
    println!("manifest:  {}", set.hooks_dir.join(loong_hooks::HOOKS_FILE).display());
    println!("status:    {}", set.summary());
    if set.specs.is_empty() {
        return Ok(());
    }
    for spec in &set.specs {
        let target = match &spec.kind {
            loong_hooks::HookKind::Shell { script } => script.display().to_string(),
            loong_hooks::HookKind::Http { url } => url.clone(),
        };
        println!(
            "  {:<20} {:<20} {:<6} on_error={:<5} timeout={}ms  {}",
            spec.id,
            spec.event.wire(),
            spec.kind.label(),
            match spec.on_error {
                loong_hooks::OnError::Deny => "deny",
                loong_hooks::OnError::Allow => "allow",
            },
            spec.timeout.as_millis(),
            target
        );
    }
    match &set.trust {
        loong_hooks::TrustStatus::Trusted => {}
        loong_hooks::TrustStatus::Untrusted { changed } => {
            println!();
            println!("INERT until trusted — new or changed: {}", changed.join(", "));
            println!("These hooks ship inside this repository, so they are content from it.");
            println!("Run `loongagent hooks trust --cwd {}` once you have read them.", args.cwd.display());
        }
        loong_hooks::TrustStatus::NoHooks => {}
    }
    Ok(())
}

/// The explicit, per-repository opt-in. Bound to bytes: editing a hook re-arms it.
fn hooks_trust(args: Args) -> Result<(), String> {
    let set = loong_hooks::HookSet::load(&args.cwd).map_err(|e| e.to_string())?;
    if set.specs.is_empty() {
        return Err("no hooks.toml in this workspace — nothing to trust".to_string());
    }
    let fingerprints = set.fingerprints().map_err(|e| e.to_string())?;
    let path = set.trust_now().map_err(|e| e.to_string())?;
    println!("trusted {} file(s) in {}", fingerprints.len(), args.cwd.display());
    for (name, hash) in &fingerprints {
        println!("  {hash} {name}");
    }
    println!("recorded in {}", path.display());
    println!("Editing any of these files re-arms the question on the next session.");
    Ok(())
}

/// ADR-0007: prove the boundary before the first tool call. "Available" is not
/// good enough — the probe must demonstrate that a forbidden write is denied.
fn sandbox_self_test(args: Args) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let report = loong_sandbox::sandbox::self_test(&exe, &args.cwd);
    println!("os:        {}", report.capabilities.os);
    println!("mechanism: {} — {}", report.capabilities.strongest(), report.capabilities.note);
    match &report.result {
        loong_sandbox::SelfTest::Enforced { mechanism, detail } => {
            println!("result:    enforced by {mechanism}");
            println!("           {detail}");
            Ok(())
        }
        loong_sandbox::SelfTest::Unavailable { detail } => {
            eprintln!("result:    UNAVAILABLE — {detail}");
            eprintln!("           tool execution is not confined on this platform");
            std::process::exit(1)
        }
        loong_sandbox::SelfTest::Broken { detail } => {
            eprintln!("result:    BROKEN — {detail}");
            std::process::exit(2)
        }
    }
}

fn headless(args: Args, prompt: &str) -> Result<(), String> {
    args.ensure_parent()?;
    let transport = ChildTransport::spawn(&args)?;
    let mut client = Client::new(transport, args.session_id());
    client.attach(&args.cwd.to_string_lossy(), "headless")?;
    for line in client.submit(prompt)? {
        println!("{line}");
    }
    if !client.last_accepted() {
        eprintln!("the turn was refused by a hook; see the hook.fired event above");
        std::process::exit(3);
    }
    if let Some(request_id) = client.pending_approval().map(|s| s.to_string()) {
        eprintln!("approval required for {request_id}; rerun with `tui` to answer");
        std::process::exit(3);
    }
    Ok(())
}
