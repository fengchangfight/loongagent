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
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::Receiver;

const USAGE: &str = "\
loongagent — a firm-internal coding agent

USAGE:
  loongagent server --stdio [--session PATH] [--cwd DIR] [--model NAME]
  loongagent setup          [--profile PATH]                                write a profile
  loongagent tui            [--session PATH] [--cwd DIR] [--model NAME]   full-screen
  loongagent repl           [--session PATH] [--cwd DIR] [--model NAME]   line mode (any pipe)
  loongagent web            [--session PATH] [--cwd DIR] [--model NAME] [--port N] [--bind IP]
  loongagent run \"PROMPT\"   [--session PATH] [--cwd DIR] [--model NAME] [--budget N] [--keep-newest N]
  loongagent --version

MODELS (provider doubles, for a first run without keys):
  echo         answers \"echo: <your prompt>\"
  demo-write   calls write_file inside the workspace, then finishes
  demo-escape  tries to write outside the workspace — the PDP will ask
  openai-compat  a real endpoint: set LOONGAGENT_BASE_URL (http://…), and
                 LOONGAGENT_MODEL / LOONGAGENT_API_KEY as needed

  loongagent sandbox self-test [--cwd DIR]   prove the OS boundary actually denies
  loongagent config show       [--cwd DIR] [--profile FILE]   every setting, and which layer set it
  loongagent config explain KEY [--cwd DIR] [--profile FILE]   why that value won
  loongagent skills list       [--cwd DIR]   show the skills and commands available here
  loongagent hooks list        [--cwd DIR]   show the effective hook set and its trust
  loongagent hooks trust       [--cwd DIR]   trust the current hook bytes (per repository)
  loongagent mcp list          [--cwd DIR]   show the configured MCP servers and their trust
  loongagent mcp trust         [--cwd DIR]   trust the current mcp.toml (per repository)
";

fn main() {
    if let Err(err) = run() {
        eprintln!("loongagent: {err}");
        std::process::exit(1);
    }
}

#[derive(Debug, Clone)]
struct Args {
    /// Where the session lives. `None` means "the usual place for this project",
    /// so nobody has to pass `--session` to start working.
    session: Option<PathBuf>,
    cwd: PathBuf,
    /// `None` means "not given": the layered configuration decides (ADR-0008).
    /// Flags are the **session layer**, the most local one.
    model: Option<String>,
    /// A profile file used as the session layer.
    profile: Option<PathBuf>,
    /// The web surface's bind address and port (loopback by default: a browser on
    /// this machine, not the network).
    bind: String,
    port: u16,
    stdio: bool,
    /// Context budget in tokens; 0 means unbounded.
    budget: Option<u64>,
    /// Newest context left verbatim when compaction runs.
    keep_newest: Option<u64>,
    positional: Vec<String>,
}

impl Args {
    fn parse(rest: &[String]) -> Result<Self, String> {
        let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
        let mut args = Args {
            session: None,
            cwd: cwd.clone(),
            model: None,
            profile: None,
            bind: "127.0.0.1".to_string(),
            port: 8737,
            stdio: false,
            budget: None,
            keep_newest: None,
            positional: Vec::new(),
        };
        let mut i = 0;
        while i < rest.len() {
            match rest[i].as_str() {
                "--session" => {
                    i += 1;
                    args.session =
                        Some(PathBuf::from(rest.get(i).ok_or("--session needs a path")?));
                }
                "--cwd" => {
                    i += 1;
                    args.cwd = PathBuf::from(rest.get(i).ok_or("--cwd needs a path")?);
                }
                "--model" => {
                    i += 1;
                    args.model = Some(rest.get(i).ok_or("--model needs a name")?.clone());
                }
                "--profile" => {
                    i += 1;
                    args.profile =
                        Some(PathBuf::from(rest.get(i).ok_or("--profile needs a path")?));
                }
                "--stdio" => args.stdio = true,
                "--bind" => {
                    i += 1;
                    args.bind = rest.get(i).ok_or("--bind needs an address")?.clone();
                }
                "--port" => {
                    i += 1;
                    args.port = rest
                        .get(i)
                        .ok_or("--port needs a number")?
                        .parse()
                        .map_err(|_| "--port must be a number".to_string())?;
                }
                "--budget" => {
                    i += 1;
                    args.budget = Some(
                        rest.get(i)
                            .ok_or("--budget needs a token count")?
                            .parse()
                            .map_err(|_| "--budget must be a number".to_string())?,
                    );
                }
                "--keep-newest" => {
                    i += 1;
                    args.keep_newest = Some(
                        rest.get(i)
                            .ok_or("--keep-newest needs a token count")?
                            .parse()
                            .map_err(|_| "--keep-newest must be a number".to_string())?,
                    );
                }
                other if other.starts_with("--") => {
                    return Err(format!("unknown flag {other:?}\n\n{USAGE}"))
                }
                other => args.positional.push(other.to_string()),
            }
            i += 1;
        }
        Ok(args)
    }

    /// The layered configuration for this run (ADR-0008): built-in defaults, then
    /// whatever the org / project / user files say, then `--profile` as the session
    /// layer. Flags are applied on top of the resolved values, and only ever
    /// narrow (the kernel enforces the narrow-only arithmetic).
    fn settings(&self) -> Result<loong_config::Profile, String> {
        let layers = loong_config::layer_paths(&self.cwd, self.profile.as_deref());
        loong_config::resolve(&layers, &self.cwd).map_err(|e| e.to_string())
    }

    /// The session file: explicit if asked, otherwise the per-project default.
    fn session_path(&self) -> PathBuf {
        self.session
            .clone()
            .unwrap_or_else(|| default_session_path(&self.cwd))
    }

    /// Where a relative credential path is looked up: beside the profile in use.
    fn profile_base(&self) -> PathBuf {
        self.profile
            .as_ref()
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .unwrap_or_else(config_dir)
    }

    fn session_id(&self) -> String {
        self.session_path()
            .file_stem()
            .map(|s| format!("s-{}", s.to_string_lossy()))
            .unwrap_or_else(|| "s-local".to_string())
    }

    fn ensure_parent(&self) -> Result<(), String> {
        if let Some(dir) = self.session_path().parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        Ok(())
    }
}

fn non_empty_env(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

/// What the wizard collected, as data: rendering and writing are separate, so the
/// answers can be tested without a terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SetupPlan {
    models: Vec<SetupModel>,
    budget_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SetupModel {
    name: String,
    base_url: String,
    model: String,
    /// `Some(key)` when the key was pasted (it goes to the credentials file),
    /// `None` when the profile should reference the environment.
    key: Option<String>,
    env_name: String,
    budget_tokens: u64,
}

/// The providers the wizard knows. Only the base URL is pre-filled: model ids change
/// every few months, and a wizard that hard-codes today's names is a wizard that goes
/// stale silently. The user copies the id from their provider's docs.
const PROVIDERS: &[(&str, &str, &str, &str)] = &[
    (
        "1",
        "DeepSeek",
        "https://api.deepseek.com",
        "DEEPSEEK_API_KEY",
    ),
    (
        "2",
        "Moonshot / Kimi",
        "https://api.moonshot.cn/v1",
        "MOONSHOT_API_KEY",
    ),
    ("3", "OpenAI", "https://api.openai.com/v1", "OPENAI_API_KEY"),
    (
        "4",
        "a local server (vLLM, LM Studio, Ollama)",
        "http://127.0.0.1:8000/v1",
        "LOONGAGENT_API_KEY",
    ),
    ("5", "something else", "", "LOONGAGENT_API_KEY"),
];

fn ask(prompt: &str, default: Option<&str>, lines: &mut dyn Iterator<Item = String>) -> String {
    print!("{prompt}");
    match default {
        Some(default) if !default.is_empty() => print!(" [{default}]"),
        _ => {}
    }
    print!(": ");
    let _ = std::io::stdout().flush();
    let answer = lines.next().unwrap_or_default();
    let answer = answer.trim().to_string();
    if answer.is_empty() {
        default.unwrap_or("").to_string()
    } else {
        answer
    }
}

/// Collect the answers. Split from input and from writing so it can be tested.
fn plan_from(lines: &mut dyn Iterator<Item = String>) -> Result<SetupPlan, String> {
    println!("Which provider?");
    for (key, label, base, _) in PROVIDERS {
        println!(
            "  {key}) {label}{}",
            if base.is_empty() {
                String::new()
            } else {
                format!("  ({base})")
            }
        );
    }
    let choice = ask("choice", Some("1"), lines);
    let (_, label, base_url, env_name) = PROVIDERS
        .iter()
        .find(|(key, ..)| *key == choice)
        .ok_or_else(|| format!("{choice:?} is not one of the choices"))?;

    let base_url = ask("base_url", Some(base_url), lines);
    if base_url.is_empty() {
        return Err("a base_url is required".to_string());
    }
    println!("The model id comes from your provider's docs (e.g. their chat-completions model).");
    let model = ask("model id", None, lines);
    if model.is_empty() {
        return Err("a model id is required".to_string());
    }
    let name = ask("a short name for it", Some(model.as_str()), lines);

    let budget = ask(
        "context budget in tokens (0 = no limit)",
        Some("200000"),
        lines,
    )
    .parse::<u64>()
    .map_err(|_| "the budget must be a whole number".to_string())?;
    // Asked last, so nothing else follows the secret on screen.
    print!("api_key (paste it, or press Enter to reference ${env_name} instead): ");
    let _ = std::io::stdout().flush();
    let pasted = lines.next().unwrap_or_default().trim().to_string();
    let key = if pasted.is_empty() {
        None
    } else {
        Some(pasted)
    };
    if key.is_some() {
        println!(
            "  → the key goes to the credentials file (mode 0600), and the profile references it"
        );
    } else {
        println!("  → the profile will reference env:{env_name}; export it in your shell");
    }

    let first_base_url = base_url.clone();
    let mut models = vec![SetupModel {
        name,
        base_url,
        model,
        key,
        env_name: env_name.to_string(),
        budget_tokens: budget,
    }];

    // A second model is what makes the router useful, so offer it rather than
    // document it.
    loop {
        let more = ask("add another model for routing? (y/N)", Some("n"), lines);
        if !more.eq_ignore_ascii_case("y") {
            break;
        }
        let base_url = ask("base_url", Some(first_base_url.as_str()), lines);
        let model = ask("model id", None, lines);
        if model.is_empty() {
            return Err("a model id is required".to_string());
        }
        let name = ask("a short name for it", Some(model.as_str()), lines);
        print!("api_key (paste it, or Enter for env:LOONGAGENT_API_KEY): ");
        let _ = std::io::stdout().flush();
        let pasted = lines.next().unwrap_or_default().trim().to_string();
        models.push(SetupModel {
            name,
            base_url,
            model,
            key: if pasted.is_empty() {
                None
            } else {
                Some(pasted)
            },
            env_name: "LOONGAGENT_API_KEY".to_string(),
            budget_tokens: budget,
        });
        println!("  (routing rules are written by hand; see the file's comments)");
    }

    println!("Configured {} from {label}.", models[0].name);
    Ok(SetupPlan {
        models,
        budget_tokens: budget,
    })
}

fn render_profile(plan: &SetupPlan) -> String {
    let mut out = String::from(
        "# Written by `loongagent setup`. Committed files may not contain secrets: keys are\n\
         # references (env:NAME or file:PATH#KEY), never literals (ADR-0008 §5).\n\
         profile = \"personal\"\n",
    );
    for model in &plan.models {
        let key = match &model.key {
            Some(_) => format!("file:credentials.toml#{}", model.env_name),
            None => format!("env:{}", model.env_name),
        };
        out.push_str(&format!(
            "\n[[models]]\nname = \"{name}\"\nkind = \"openai-compat\"\n\
             base_url = \"{base}\"\nmodel = \"{id}\"\napi_key = \"{key}\"\n\
             budget_tokens = {budget}\n",
            name = model.name,
            base = model.base_url,
            id = model.model,
            budget = model.budget_tokens,
        ));
    }
    // A commented router: the shape is useful to see, and switching models silently
    // is exactly what this project refuses to do.
    out.push_str(&format!(
        "\n[router]\ndefault = \"{}\"\n\n\
         # Rules are first-match-wins, and the one that fired is recorded in the session:\n\
         # [[router.rules]]\n\
         # when = \"context_tokens > 150000\"\n\
         # use = \"a-long-context-model\"\n",
        plan.models[0].name
    ));
    out
}

fn render_credentials(plan: &SetupPlan) -> Option<String> {
    let keys: Vec<&SetupModel> = plan.models.iter().filter(|m| m.key.is_some()).collect();
    if keys.is_empty() {
        return None;
    }
    let mut out = String::from(
        "# Written by `loongagent setup`, mode 0600. Not configuration: this is the one\n\
         # file that holds secrets, which is why the profile above only references it.\n\
         [keys]\n",
    );
    for model in keys {
        out.push_str(&format!(
            "{} = {:?}\n",
            model.env_name,
            model.key.clone().unwrap_or_default()
        ));
    }
    Some(out)
}

fn setup(args: Args) -> Result<(), String> {
    let dir = config_dir();
    let profile_path = args
        .profile
        .clone()
        .unwrap_or_else(|| dir.join("profile.toml"));
    let credentials_path = profile_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("credentials.toml");

    println!("loongagent setup");
    println!("  profile:     {}", profile_path.display());
    println!("  credentials: {}", credentials_path.display());
    println!();
    let mut lines = std::io::stdin().lock().lines().map_while(Result::ok);
    let plan = plan_from(&mut lines)?;

    if let Some(parent) = profile_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    if profile_path.exists() {
        let backup = profile_path.with_extension("toml.bak");
        std::fs::rename(&profile_path, &backup)
            .map_err(|e| format!("backing up {}: {e}", profile_path.display()))?;
        println!("kept the previous profile as {}", backup.display());
    }
    std::fs::write(&profile_path, render_profile(&plan))
        .map_err(|e| format!("writing {}: {e}", profile_path.display()))?;

    if let Some(text) = render_credentials(&plan) {
        std::fs::write(&credentials_path, text)
            .map_err(|e| format!("writing {}: {e}", credentials_path.display()))?;
        // 0600 before anything else can read it.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&credentials_path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| {
                    format!("setting permissions on {}: {e}", credentials_path.display())
                })?;
        }
    }

    println!();
    println!("wrote {}", profile_path.display());
    if render_credentials(&plan).is_some() {
        println!("wrote {} (mode 0600)", credentials_path.display());
    }
    for model in plan.models.iter().filter(|m| m.key.is_none()) {
        println!(
            "export {}='…'   # the profile references it",
            model.env_name
        );
    }
    println!();
    println!("next:  loongagent tui          (in the project you want to work on)");
    println!("       loongagent config show  (which layer set what)");
    Ok(())
}

/// Read one secret out of a credentials file: `[keys]` NAME = "…".
///
/// `toml::from_str`, not `str::parse`: the `FromStr` implementation parses a *single
/// value*, so a document with a `[keys]` table fails with "unexpected content".
fn read_credential(path: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let parsed: toml::Value = toml::from_str(&text)
        .map_err(|e| eprintln!("[config] {} is not valid TOML: {e}", path.display()))
        .ok()?;
    parsed
        .get("keys")
        .and_then(|keys| keys.get(key))
        .and_then(|value| value.as_str())
        .map(|secret| secret.to_string())
}

fn config_dir() -> PathBuf {
    non_empty_env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| non_empty_env("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(std::env::temp_dir)
        .join("loongagent")
}

/// `$XDG_STATE_HOME/loongagent/projects/<project>/session.jsonl`.
///
/// Sessions are state, not configuration, so they live under the state dir and are
/// keyed by the project they belong to: resuming yesterday's work in a directory is
/// the default, and `--session` is only for choosing differently.
fn default_session_path(cwd: &Path) -> PathBuf {
    // An XDG variable that is set but empty means "use the default" (the spec says
    // so, and a relative path here would scatter sessions around the project).
    let base = non_empty_env("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| non_empty_env("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("loongagent")
        .join("projects")
        .join(project_slug(cwd))
        .join("session.jsonl")
}

/// A readable directory name for a project path: `/home/me/work/api` becomes
/// `home-me-work-api-3f9a1c22`. The hash is there because two different paths can
/// produce the same slug (`/a-b/c` and `/a/b-c`), and quietly sharing a session
/// between two projects would be a nasty surprise.
fn project_slug(cwd: &Path) -> String {
    let path = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let text = path.to_string_lossy();
    let mut slug: String = text
        .trim_start_matches('/')
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{}-{:08x}", slug.trim_matches('-'), hash as u32)
}

fn run() -> Result<(), String> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let (command, rest) = argv
        .split_first()
        .map(|(c, r)| (c.as_str(), r))
        .unwrap_or(("", &[]));
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
        "config" => match rest.first().map(String::as_str) {
            Some("show") => config_show(Args::parse(&rest[1..])?),
            Some("explain") => config_explain(Args::parse(&rest[1..])?),
            _ => Err(format!("config show | config explain <key>\n\n{USAGE}")),
        },
        "skills" => match rest.first().map(String::as_str) {
            Some("list") => skills_list(Args::parse(&rest[1..])?),
            _ => Err(format!("skills list\n\n{USAGE}")),
        },
        "hooks" => match rest.first().map(String::as_str) {
            Some("list") => hooks_list(Args::parse(&rest[1..])?),
            Some("trust") => hooks_trust(Args::parse(&rest[1..])?),
            _ => Err(format!("hooks list | hooks trust\n\n{USAGE}")),
        },
        "mcp" => match rest.first().map(String::as_str) {
            Some("list") => mcp_list(Args::parse(&rest[1..])?),
            Some("trust") => mcp_trust(Args::parse(&rest[1..])?),
            _ => Err(format!("mcp list | mcp trust\n\n{USAGE}")),
        },
        "setup" => setup(Args::parse(rest)?),
        "tui" => full_screen(Args::parse(rest)?),
        "repl" => repl(Args::parse(rest)?),
        "web" => web(Args::parse(rest)?),
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
    let settings = args.settings()?;
    for row in settings.explain_all() {
        eprintln!(
            "[config] {:<28} {:<9} {:<14} {}",
            row.key,
            row.value.render(),
            format!("({})", row.kind.as_str()),
            row.source
        );
    }
    match loong_hooks::HookSet::load(&args.cwd) {
        Ok(set) => eprintln!("[hooks] {}", set.summary()),
        Err(e) => return Err(format!("hooks.toml is broken: {e}")),
    }
    match loong_mcp::McpSet::load(&args.cwd) {
        Ok(set) => eprintln!("[mcp]   {}", set.summary()),
        Err(e) => return Err(format!("mcp.toml is broken: {e}")),
    }
    match loong_skills::SkillSet::load(&args.cwd) {
        Ok(set) => eprintln!(
            "[skills] workspace: {} (MCP prompts join per session)",
            set.names().join(", ")
        ),
        Err(e) => return Err(format!("a skill is broken: {e}")),
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
                return Err(format!(
                    "refusing to start: the sandbox self-test is broken — {detail}"
                ));
            }
        }
    }
    let models = build_models(&args, &settings)?;
    let mut config = KernelConfig::new(&args.cwd);
    // `--budget 0` means "unbounded", which is the widest: the kernel takes the
    // strictest of this and the configuration, so a ceiling still holds.
    config.budget_tokens = match args.budget {
        Some(0) => None,
        Some(n) => Some(n),
        None => settings.budget_tokens,
    };
    config.keep_newest_tokens = args.keep_newest.unwrap_or(20_000);
    if let Some(percent) = settings.compact_at_percent {
        config.compact_at_percent = percent;
    }
    if let Some(steps) = settings.max_steps {
        config.max_steps = steps;
    }
    config.profile = settings
        .explain("profile")
        .map(|row| row.value.render())
        .unwrap_or_else(|| "default".to_string());
    config.settings = settings;
    let session_path = args.session_path();
    let kernel = if session_path.exists() {
        Kernel::open_with_models(&session_path, config, models)
    } else {
        Kernel::create_with_models(&session_path, config, models)
    }
    .map_err(|e| e.to_string())?;
    Server::new(kernel, args.session_id())
        .run_stdio()
        .map_err(|e| e.to_string())
}

/// Which provider to build: the session's choice if it made one, else the
/// configuration's (T-07: engineers pick a profile, not a model), else the `echo`
/// double so a fresh checkout runs without keys.
/// Every model the profile offers, by name — the set the router chooses from. A
/// `--model` on the command line overrides the whole set with one adapter, which is
/// what a developer wants when trying the loop with a provider double.
fn build_models(
    args: &Args,
    settings: &loong_config::Profile,
) -> Result<Vec<(String, Box<dyn Model>)>, String> {
    let base = args.profile_base();
    if let Some(name) = args.model.clone() {
        return Ok(vec![(
            "default".to_string(),
            build_model(args, settings, Some(&name))?,
        )]);
    }
    if settings.models.is_empty() {
        return Ok(vec![("default".to_string(), Box::new(UnconfiguredModel))]);
    }
    let mut built = Vec::new();
    for spec in &settings.models {
        let model = match spec.kind.as_str() {
            "openai-compat" => {
                let base_url = resolve_reference(Some(&spec.base_url), &base)
                    .or_else(|| std::env::var("LOONGAGENT_BASE_URL").ok())
                    .ok_or_else(|| {
                        format!("model {:?}: base_url could not be resolved", spec.name)
                    })?;
                let key = resolve_reference(spec.api_key.as_deref(), &base)
                    .or_else(|| std::env::var("LOONGAGENT_API_KEY").ok());
                if key.is_none() {
                    eprintln!(
                        "[config] model {:?} has no usable api_key — the provider will answer 401",
                        spec.name
                    );
                }
                Box::new(OpenAiCompat::new(base_url, spec.model.clone(), key)) as Box<dyn Model>
            }
            other => {
                return Err(format!(
                    "model {:?} asks for adapter {other:?}, which this build does not have",
                    spec.name
                ))
            }
        };
        built.push((spec.name.clone(), model));
    }
    Ok(built)
}

fn build_model(
    args: &Args,
    settings: &loong_config::Profile,
    forced: Option<&str>,
) -> Result<Box<dyn Model>, String> {
    let base_dir = args.profile_base();
    let kind = match forced {
        Some(name) => name.to_string(),
        None => args
            .model
            .clone()
            .or_else(|| {
                settings
                    .base_url
                    .as_ref()
                    .map(|_| "openai-compat".to_string())
            })
            .unwrap_or_else(|| "unconfigured".to_string()),
    };
    match kind.as_str() {
        "echo" => Ok(Box::new(EchoModel)),
        "unconfigured" => Ok(Box::new(UnconfiguredModel)),
        "openai-compat" => {
            let base = resolve_reference(settings.base_url.as_deref(), &base_dir)
                .or_else(|| std::env::var("LOONGAGENT_BASE_URL").ok())
                .ok_or_else(|| {
                    "openai-compat needs a base_url: set it in a profile (base_url = \"http://…\" \
                     or \"env:NAME\") or LOONGAGENT_BASE_URL"
                        .to_string()
                })?;
            let model = settings
                .model
                .clone()
                .or_else(|| std::env::var("LOONGAGENT_MODEL").ok())
                .unwrap_or_else(|| "local".to_string());
            let key = match resolve_reference(settings.api_key.as_deref(), &base_dir) {
                Some(key) => Some(key),
                None => std::env::var("LOONGAGENT_API_KEY").ok(),
            };
            Ok(Box::new(OpenAiCompat::new(base, model, key)))
        }
        "demo-write" => Ok(Box::new(ScriptedModel::new(vec![
            CompletionResponse {
                text: None,
                tool_calls: vec![ToolCallRequest {
                    call_id: "c1".to_string(),
                    tool: "write_file".to_string(),
                    args: json!({"path": "demo.txt", "content": "written by the loongagent vertical slice\n"}),
                }],
                usage: Usage {
                    tokens_in: 20,
                    tokens_out: 8,
                },
            },
            CompletionResponse {
                text: Some("wrote demo.txt".to_string()),
                tool_calls: Vec::new(),
                usage: Usage {
                    tokens_in: 30,
                    tokens_out: 4,
                },
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
                usage: Usage {
                    tokens_in: 20,
                    tokens_out: 8,
                },
            },
            CompletionResponse {
                text: Some("understood".to_string()),
                tool_calls: Vec::new(),
                usage: Usage {
                    tokens_in: 10,
                    tokens_out: 2,
                },
            },
        ]))),
        other => Err(format!(
            "unknown --model {other:?} (echo | demo-write | demo-escape | openai-compat)"
        )),
    }
}

/// Resolve a configuration reference (ADR-0008 §5: config carries a reference, not
/// a secret). A bare value is passed through only for non-secret keys.
fn resolve_reference(value: Option<&str>, base: &Path) -> Option<String> {
    let value = value?;
    if let Some(name) = value.strip_prefix("env:") {
        return std::env::var(name).ok();
    }
    if let Some(rest) = value.strip_prefix("file:") {
        let (path, key) = match rest.split_once('#') {
            Some((path, key)) => (path, key),
            None => {
                eprintln!("[config] {value} needs the form file:PATH#KEY");
                return None;
            }
        };
        let path = if let Some(relative) = path.strip_prefix("~/") {
            match std::env::var("HOME") {
                Ok(home) => PathBuf::from(home).join(relative),
                Err(_) => return None,
            }
        } else if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            // Relative to the profile in use, so a profile and its credentials file
            // move together — and `--profile elsewhere/profile.toml` finds its own.
            base.join(path)
        };
        return read_credential(&path, key).or_else(|| {
            eprintln!("[config] {} has no usable keys.{key}", path.display());
            None
        });
    }
    if value.starts_with("keychain:") || value.starts_with("gateway:") {
        // ADR-0008 §5 says credentials live in the OS store or the control plane.
        // Neither is wired, and pretending to read them would be worse than saying so.
        eprintln!(
            "[config] {value} is a credential reference, and no credential store is wired yet \
             (ADR-0008 §5) — falling back to the environment"
        );
        return None;
    }
    Some(value.to_string())
}

/// The model a first run gets: it answers every call with the one thing the user
/// needs to know. Refusing to start would be worse — the TUI shows this as an
/// error line inside a working session, next to the command that fixes it.
struct UnconfiguredModel;

impl Model for UnconfiguredModel {
    fn complete(&mut self, _req: &CompletionRequest) -> Result<CompletionResponse, ModelError> {
        Err(ModelError::Transport(
            "no model is configured yet — run `loongagent setup` to pick one, then start again              (or pass --model echo / demo-write / demo-escape to exercise the loop without a model)"
                .to_string(),
        ))
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
            usage: Usage {
                tokens_in: 3,
                tokens_out: 3,
            },
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
    /// `capture_notes` pipes the server's stderr instead of inheriting it: a
    /// full-screen UI owns the terminal, so diagnostics must be drawn, not dumped.
    fn spawn(args: &Args, capture_notes: bool) -> Result<(Self, Option<Receiver<String>>), String> {
        // Same binary by default; overridable so a *test* binary can point at the
        // built server (an integration test's current_exe is the test harness).
        let exe = match std::env::var_os("LOONGAGENT_SERVER_BIN") {
            Some(path) => PathBuf::from(path),
            None => std::env::current_exe().map_err(|e| e.to_string())?,
        };
        let mut command = Command::new(exe);
        // The subcommand comes first: flags before it would be read as the command.
        command.arg("server");
        // The session layer travels with the child: a profile file the caller named,
        // and the flags they typed. Everything else the child resolves itself from
        // the same layers, so the two cannot disagree about the configuration.
        if let Some(profile) = &args.profile {
            command.arg("--profile").arg(profile);
        }
        if let Some(model) = &args.model {
            command.arg("--model").arg(model);
        }
        if let Some(budget) = args.budget {
            command.arg("--budget").arg(budget.to_string());
        }
        if let Some(keep) = args.keep_newest {
            command.arg("--keep-newest").arg(keep.to_string());
        }
        let mut child = command
            .arg("--stdio")
            .arg("--session")
            .arg(args.session_path())
            .arg("--cwd")
            .arg(&args.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(if capture_notes {
                Stdio::piped()
            } else {
                Stdio::inherit()
            })
            .spawn()
            .map_err(|e| format!("spawning the local server: {e}"))?;
        let stdin = child.stdin.take().ok_or("no stdin on the server child")?;
        let stdout = BufReader::new(child.stdout.take().ok_or("no stdout on the server child")?);
        let notes = if capture_notes {
            child.stderr.take().map(|stderr| {
                let (sender, receiver) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    use std::io::BufRead;
                    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                        if sender.send(line).is_err() {
                            break;
                        }
                    }
                });
                receiver
            })
        } else {
            None
        };
        Ok((
            Self {
                child,
                stdin,
                stdout,
            },
            notes,
        ))
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

/// The full-screen surface: the client here, the screen in `loong-tui-app`.
fn full_screen(args: Args) -> Result<(), String> {
    args.ensure_parent()?;
    let (transport, notes) = ChildTransport::spawn(&args, true)?;
    let mut client = Client::new(transport, args.session_id());
    let attached = match client.attach(&args.cwd.to_string_lossy(), "tui") {
        Ok(attached) => attached,
        Err(e) => {
            // The child's own explanation is the useful one (a corrupt log, a bad
            // profile); without this the user sees only "the connection closed".
            if let Some(notes) = &notes {
                let mut tail: Vec<String> = Vec::new();
                while let Ok(line) = notes.try_recv() {
                    tail.push(line);
                }
                for line in tail.iter().rev().take(5).rev() {
                    eprintln!("[server] {line}");
                }
            }
            return Err(e);
        }
    };
    // What a first run needs to see, inside the UI rather than on a stderr that the
    // alternate screen would overwrite: where this session lives, and whether a model
    // is configured at all.
    // The client resolves the same layers the server does, so it can tell the user
    // whether a model exists without asking the server about it.
    let settings = args.settings()?;
    let mut intro = vec![format!("session: {}", args.session_path().display())];
    if attached.existing_events > 0 {
        intro.push(format!(
            "resuming {} events from that session",
            attached.existing_events
        ));
    }
    if args.model.is_none() && settings.base_url.is_none() {
        intro.push(
            "no model configured — run `loongagent setup`, or pass --model demo-write to try the \
             loop without one"
                .to_string(),
        );
    }
    loong_tui_app::run(
        client,
        attached.session_id,
        attached.schema_version,
        notes,
        intro,
    )
}

/// The browser surface: the same JSON-RPC lines, carried over a WebSocket.
struct WebSession {
    transport: ChildTransport,
    cwd: String,
}

impl loong_web::Session for WebSession {
    fn exchange(
        &mut self,
        line: &str,
        sink: &mut dyn FnMut(String) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut request: serde_json::Value =
            serde_json::from_str(line).map_err(|e| format!("not JSON-RPC: {e}"))?;
        // The browser cannot know the server's working directory, so the surface
        // fills it in. Everything else is passed through untouched.
        if request.get("method").and_then(|m| m.as_str())
            == Some(loong_schema::generated::method::SESSION_ATTACH)
        {
            if let Some(params) = request.get_mut("params") {
                let empty = params
                    .get("cwd")
                    .and_then(|c| c.as_str())
                    .map(|c| c.is_empty())
                    .unwrap_or(true);
                if empty {
                    params["cwd"] = serde_json::json!(self.cwd);
                }
            }
        }
        let id = request.get("id").cloned();
        self.transport.send(&request.to_string())?;

        loop {
            let Some(line) = self.transport.recv_line()? else {
                return Err("the server closed the connection".to_string());
            };
            if line.trim().is_empty() {
                continue;
            }
            let parsed: serde_json::Value =
                serde_json::from_str(&line).unwrap_or(serde_json::Value::Null);
            let is_response = parsed.get("id").is_some() && parsed.get("method").is_none();
            let mine = id.is_some() && parsed.get("id") == id.as_ref();
            // Straight through: a delta written by the model reaches the browser now,
            // not when the turn ends.
            sink(line)?;
            if is_response && mine {
                return Ok(());
            }
        }
    }
}

fn web(args: Args) -> Result<(), String> {
    args.ensure_parent()?;
    let settings = args.settings()?;
    let (transport, notes) = ChildTransport::spawn(&args, true)?;
    if let Some(notes) = notes {
        // The server's own diagnostics go to this terminal, where the operator is.
        std::thread::spawn(move || {
            while let Ok(line) = notes.recv() {
                eprintln!("[server] {line}");
            }
        });
    }
    let session = std::sync::Arc::new(std::sync::Mutex::new(WebSession {
        transport,
        cwd: args.cwd.display().to_string(),
    }));
    let token = loong_web::random_token()?;
    let (listener, addr) = loong_web::bind(&args.bind, args.port)?;

    for row in settings.explain_all() {
        eprintln!(
            "[config] {:<28} {:<9} {}",
            row.key,
            row.value.render(),
            row.source
        );
    }
    println!("loongagent web — open this URL:");
    println!();
    println!("  http://{addr}/#token={token}");
    println!();
    println!("The token is in the URL fragment: browsers never send it to a server, and");
    println!("another page cannot open the socket without it. Bound to {addr}.");
    loong_web::serve(session, listener, token);
    Ok(())
}

/// Print a turn's deltas as they arrive, and return the rendered lines that are
/// not already on screen. A streamed answer would otherwise appear twice: once as
/// it was written and once when the turn finished.
fn print_streaming(client: &mut Client<ChildTransport>, text: &str) -> Result<Vec<String>, String> {
    let mut streamed = String::new();
    let lines = client.submit_with(text, &mut |method, params| {
        if method != loong_schema::generated::notification::MODEL_DELTA {
            return;
        }
        if let Some(piece) = params.get("text").and_then(|t| t.as_str()) {
            streamed.push_str(piece);
            print!("{piece}");
            let _ = std::io::stdout().flush();
        }
    })?;
    if streamed.is_empty() {
        return Ok(lines);
    }
    if !streamed.ends_with('\n') {
        println!();
    }
    Ok(lines.into_iter().filter(|line| line != &streamed).collect())
}

fn repl(args: Args) -> Result<(), String> {
    args.ensure_parent()?;
    let (transport, _) = ChildTransport::spawn(&args, false)?;
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
            None => print_streaming(&mut client, &line),
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

/// Show which external tools would join the session.
fn mcp_list(args: Args) -> Result<(), String> {
    let set = loong_mcp::McpSet::load(&args.cwd).map_err(|e| e.to_string())?;
    println!("manifest: {}", set.dir.join(loong_mcp::MCP_FILE).display());
    println!("status:   {}", set.summary());
    for spec in &set.servers {
        println!();
        println!("  {} — {}", spec.name, spec.command);
        if !spec.args.is_empty() {
            println!("    args:        {}", spec.args.join(" "));
        }
        println!("    action class: {:?}", spec.action_class);
        for (tool, class) in &spec.tools {
            println!("    override:     {tool} -> {class:?}");
        }
        println!("    timeout:      {}ms", spec.timeout.as_millis());
        println!("    required:     {}", spec.required);
    }
    if let loong_mcp::TrustStatus::Untrusted { changed } = &set.trust {
        println!();
        println!(
            "INERT until trusted — new or changed: {}",
            changed.join(", ")
        );
        println!("A server entry names a command to run, so read it first, then:");
        println!("  loongagent mcp trust --cwd {}", args.cwd.display());
    }
    Ok(())
}

fn mcp_trust(args: Args) -> Result<(), String> {
    let set = loong_mcp::McpSet::load(&args.cwd).map_err(|e| e.to_string())?;
    if set.servers.is_empty() {
        return Err("no mcp.toml in this workspace — nothing to trust".to_string());
    }
    let path = set.trust_now().map_err(|e| e.to_string())?;
    println!(
        "trusted {} server(s) in {}",
        set.servers.len(),
        args.cwd.display()
    );
    println!("recorded in {}", path.display());
    println!("Editing mcp.toml re-arms the question on the next session.");
    Ok(())
}

/// Every key answers "why is this the value?" (ADR-0008 §4).
fn config_show(args: Args) -> Result<(), String> {
    let settings = args.settings()?;
    for (layer, path) in loong_config::layer_paths(&args.cwd, args.profile.as_deref()) {
        let mark = if path.exists() { "  " } else { "· " };
        println!("{mark}{:<8} {}", layer.as_str(), path.display());
    }
    println!();
    println!("{:<30} {:<16} {:<8} {}", "key", "value", "kind", "set by");
    for row in settings.explain_all() {
        println!(
            "{:<30} {:<16} {:<8} {}",
            row.key,
            row.value.render(),
            row.kind.as_str(),
            row.source
        );
    }
    println!();
    println!("(· = no such file; built-in defaults come from <defaults>)");
    Ok(())
}

/// The same question for one key, including what lost and why.
fn config_explain(args: Args) -> Result<(), String> {
    let key = args
        .positional
        .first()
        .cloned()
        .ok_or_else(|| format!("config explain needs a key\n\n{USAGE}"))?;
    let settings = args.settings()?;
    let base = key.split('.').next().unwrap_or(&key);
    let kind = loong_config::kind_of(base).ok_or_else(|| format!("unknown key {base:?}"))?;
    match settings.explain(&key) {
        Some(row) => {
            println!("{key} = {}", row.value.render());
            println!("  kind:   {} ({})", kind.as_str(), explain_kind(kind));
            println!("  layer:  {}", row.layer.as_str());
            println!("  source: {}", row.source);
            if row.overridden.is_empty() {
                println!("  other layers did not set it");
            } else {
                println!("  also set by (and lost):");
                for (layer, value) in &row.overridden {
                    println!("    {:<8} {}", layer.as_str(), value.render());
                }
            }
        }
        None => {
            println!("{key} is not set by any layer");
            println!("  kind:   {} ({})", kind.as_str(), explain_kind(kind));
        }
    }
    Ok(())
}

fn explain_kind(kind: loong_config::Kind) -> &'static str {
    match kind {
        loong_config::Kind::FreeOverride => "the most local layer that sets it wins",
        loong_config::Kind::NarrowOnly => "strictest wins; a lower layer may only narrow",
        loong_config::Kind::AddOnly => "union across layers",
        loong_config::Kind::Reference => "names a source; secrets never live in config",
    }
}

/// Show what a `/name` in this workspace resolves to.
fn skills_list(args: Args) -> Result<(), String> {
    // Honour the layered configuration: what this prints must be what a session
    // would actually get, or it is worse than not printing it.
    let settings = args.settings()?;
    let set =
        loong_skills::SkillSet::load_in(&args.cwd, &settings.skill_dirs, &settings.disable_skills)
            .map_err(|e| e.to_string())?;
    for name in &set.disabled {
        println!("disabled by configuration: /{name}");
    }
    println!("workspace skills and commands (an MCP server's prompts join the");
    println!("catalogue inside a session, namespaced as mcp__<server>__<prompt>):");
    println!();
    for line in set.catalogue().lines() {
        println!("  {line}");
    }
    println!();
    for skill in &set.skills {
        println!(
            "  /{:<22} {:>5} tokens  {}",
            skill.name,
            skill.tokens(),
            skill.source_label()
        );
    }
    Ok(())
}

/// Show what would run, and whether it is trusted to.
fn hooks_list(args: Args) -> Result<(), String> {
    let set = loong_hooks::HookSet::load(&args.cwd).map_err(|e| e.to_string())?;
    println!("workspace: {}", args.cwd.display());
    println!(
        "manifest:  {}",
        set.hooks_dir.join(loong_hooks::HOOKS_FILE).display()
    );
    println!("status:    {}", set.summary());
    if set.specs.is_empty() {
        return Ok(());
    }
    for spec in &set.specs {
        let target = spec.kind.target();
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
            println!(
                "INERT until trusted — new or changed: {}",
                changed.join(", ")
            );
            println!("These hooks ship inside this repository, so they are content from it.");
            println!(
                "Run `loongagent hooks trust --cwd {}` once you have read them.",
                args.cwd.display()
            );
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
    println!(
        "trusted {} file(s) in {}",
        fingerprints.len(),
        args.cwd.display()
    );
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
    println!(
        "mechanism: {} — {}",
        report.capabilities.strongest(),
        report.capabilities.note
    );
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
    let (transport, _) = ChildTransport::spawn(&args, false)?;
    let mut client = Client::new(transport, args.session_id());
    client.attach(&args.cwd.to_string_lossy(), "headless")?;
    // Headless output stays in order: the turn's events, then the answer. Streaming
    // would print the answer before the tool calls that produced it, which is
    // exactly wrong for a log someone reads afterwards. `repl`, `tui` and `web`
    // stream, because a human is watching.
    for line in client.submit(prompt)? {
        println!("{line}");
    }
    if !client.last_accepted() {
        eprintln!("the turn was refused; the reason is in the event above");
        std::process::exit(3);
    }
    if let Some(request_id) = client.pending_approval().map(|s| s.to_string()) {
        eprintln!("approval required for {request_id}; rerun with `tui` to answer");
        std::process::exit(3);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answers(list: &[&str]) -> std::vec::IntoIter<String> {
        list.iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn the_wizard_turns_answers_into_a_profile_and_a_credentials_file() {
        // choice, base_url, model id, name, budget, api_key, "another model?"
        let mut lines = answers(&["1", "", "some-model", "small", "64000", "sk-secret", "n"]);
        let plan = plan_from(&mut lines).unwrap();
        assert_eq!(plan.models.len(), 1);
        assert_eq!(
            plan.models[0].base_url, "https://api.deepseek.com",
            "the provider default"
        );
        assert_eq!(plan.models[0].name, "small");
        assert_eq!(plan.models[0].budget_tokens, 64000);

        let profile = render_profile(&plan);
        assert!(profile.contains("name = \"small\""));
        assert!(profile.contains("model = \"some-model\""));
        assert!(profile.contains("api_key = \"file:credentials.toml#DEEPSEEK_API_KEY\""));
        assert!(profile.contains("budget_tokens = 64000"));
        assert!(
            !profile.contains("sk-secret"),
            "a secret never lands in the profile"
        );
        assert!(profile.contains("default = \"small\""));

        let credentials = render_credentials(&plan).unwrap();
        assert!(credentials.contains("DEEPSEEK_API_KEY = \"sk-secret\""));
    }

    #[test]
    fn a_key_that_is_not_pasted_becomes_an_environment_reference() {
        let mut lines = answers(&["3", "", "gpt-x", "", "1000", "", "n"]);
        let plan = plan_from(&mut lines).unwrap();
        assert!(plan.models[0].key.is_none());
        assert!(render_profile(&plan).contains("api_key = \"env:OPENAI_API_KEY\""));
        assert!(render_credentials(&plan).is_none(), "no secret, no file");
    }

    #[test]
    fn a_second_model_becomes_a_second_entry_for_the_router() {
        let mut lines = answers(&[
            "4", "", "local-a", "a", "8000", "", "y", "", "local-b", "b", "", "n",
        ]);
        let plan = plan_from(&mut lines).unwrap();
        assert_eq!(plan.models.len(), 2);
        assert_eq!(plan.models[1].model, "local-b");
        assert_eq!(
            plan.models[1].base_url, plan.models[0].base_url,
            "the first base_url is offered"
        );
        assert_eq!(render_profile(&plan).matches("[[models]]").count(), 2);
    }

    #[test]
    fn a_credentials_file_is_read_by_the_reference_it_is_named_for() {
        let dir = std::env::temp_dir().join(format!("loong-creds-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("credentials.toml");
        std::fs::write(
            &path,
            "# a comment, then a table\n[keys]\nDEEPSEEK_API_KEY = \"sk-secret\"\n\
             MOONSHOT_API_KEY = \"ms-secret\"\n",
        )
        .unwrap();
        assert_eq!(
            read_credential(&path, "DEEPSEEK_API_KEY").as_deref(),
            Some("sk-secret")
        );
        assert_eq!(
            read_credential(&path, "MOONSHOT_API_KEY").as_deref(),
            Some("ms-secret")
        );
        assert_eq!(
            read_credential(&path, "NOT_THERE"),
            None,
            "a missing key is not a panic"
        );
        std::fs::write(&path, "this is not toml at all").unwrap();
        assert_eq!(read_credential(&path, "ANY"), None, "nor is nonsense");
        assert_eq!(read_credential(&dir.join("absent.toml"), "ANY"), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bad_answers_are_refused_with_a_reason() {
        let mut lines = answers(&["1", "", "m", "n", "lots", "", "n"]);
        assert!(plan_from(&mut lines).unwrap_err().contains("whole number"));
        let mut lines = answers(&["9"]);
        assert!(plan_from(&mut lines)
            .unwrap_err()
            .contains("not one of the choices"));
        let mut lines = answers(&["1", "", "", "n", "1", "", "n"]);
        assert!(plan_from(&mut lines).unwrap_err().contains("model id"));
    }
}
