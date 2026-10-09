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
use std::time::{SystemTime, UNIX_EPOCH};

const USAGE: &str = "\
loongagent — a firm-internal coding agent

USAGE:
  loongagent server --stdio [--session PATH] [--cwd DIR] [--model NAME]
  loongagent setup          [--profile PATH]                                write a profile
  loongagent tui            [--session ID|PATH] [--cwd DIR] [--new]        full-screen
  loongagent resume [ID]    [--cwd DIR]                                    reopen one (default: latest)
  loongagent sessions       [--all] [--limit N] [--cwd DIR]                list them
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
    /// Where the session lives, once it has been worked out. `None` means "the newest
    /// one in this project", so nobody has to pass `--session` to start working.
    session: Option<PathBuf>,
    /// What `--session` was given: a path, an id, or `latest`. Resolved once the whole
    /// command line is known (it needs `--cwd`).
    session_spec: Option<String>,
    /// `--new`: start a session rather than picking one up.
    new_session: bool,
    cwd: PathBuf,
    /// `None` means "not given": the layered configuration decides (ADR-0008).
    /// Flags are the **session layer**, the most local one.
    model: Option<String>,
    /// A profile file used as the session layer.
    profile: Option<PathBuf>,
    /// The web surface's bind address and port (loopback by default: a browser on
    /// this machine, not the network).
    bind: String,
    /// `setup`: the control plane's address, its pinned public key, and a token
    /// reference. All three can be answered interactively instead.
    control_url: Option<String>,
    control_key: Option<String>,
    control_token: Option<String>,
    /// `setup --system`: write the machine-wide file (needs root) instead of the
    /// user's own.
    control_system: bool,
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
            session_spec: None,
            new_session: false,
            cwd: cwd.clone(),
            model: None,
            profile: None,
            bind: "127.0.0.1".to_string(),
            control_url: None,
            control_key: None,
            control_token: None,
            control_system: false,
            port: 8737,
            stdio: false,
            budget: None,
            keep_newest: None,
            positional: Vec::new(),
        };
        // Before anything resolves a reference: a pack says `env:NAME`, and this is
        // what makes that work without an export in every shell.
        load_env_files(&config_dir(), &cwd);

        let mut i = 0;
        while i < rest.len() {
            match rest[i].as_str() {
                "--session" => {
                    i += 1;
                    args.session_spec = Some(
                        rest.get(i)
                            .ok_or("--session needs an id or a path")?
                            .clone(),
                    );
                }
                "--new" => args.new_session = true,
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
                // `setup`: the control plane. Flags exist so a machine can be
                // provisioned without a prompt; without them it asks.
                "--url" => {
                    i += 1;
                    args.control_url = Some(rest.get(i).ok_or("--url needs an address")?.clone());
                }
                "--public-key" => {
                    i += 1;
                    args.control_key = Some(rest.get(i).ok_or("--public-key needs hex")?.clone());
                }
                "--token" => {
                    i += 1;
                    args.control_token = Some(rest.get(i).ok_or("--token needs a value")?.clone());
                }
                "--system" => args.control_system = true,
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
        args.finish_session()?;
        Ok(args)
    }

    /// Work out which session this run is about — once, here, so every command and the
    /// interface agree. Three ways to say it: a path (`./x.jsonl`), an id (the file's
    /// own name, or an unambiguous prefix of it), or `latest`. Nothing said means the
    /// newest session in this project, and if there is none, a fresh id is minted now so
    /// that the answer is stable for the rest of the run.
    fn finish_session(&mut self) -> Result<(), String> {
        if self.new_session && self.session_spec.is_some() {
            return Err(
                "--new starts a session and --session picks one; pass only one of them".to_string(),
            );
        }
        self.session = if self.new_session {
            let id = new_session_id(&self.cwd);
            Some(session_path_for(&id, &self.cwd))
        } else if let Some(spec) = self.session_spec.clone() {
            Some(resolve_session(&spec, &self.cwd).map_err(|e| format!("--session {spec}: {e}"))?)
        } else {
            crate::newest_session(&self.cwd)
                .or_else(|| Some(session_path_for(&new_session_id(&self.cwd), &self.cwd)))
        };
        Ok(())
    }

    /// The layered configuration for this run (ADR-0008): built-in defaults, then
    /// whatever the org / project / user files say, then `--profile` as the session
    /// layer. Flags are applied on top of the resolved values, and only ever
    /// narrow (the kernel enforces the narrow-only arithmetic).
    /// Resolve the configuration with the control plane's pack in place, if there is
    /// one. The pack is the *verified* layer: nothing reaches the resolver that a user
    /// could have written by hand (ADR-0008 §6).
    fn settings_with_pack(&self) -> Result<(loong_config::Profile, Option<PackState>), String> {
        let pack = sync_pack(self)?;
        let layers = loong_config::layer_paths(&self.cwd, self.profile.as_deref());
        let resolved = match &pack {
            Some(state) => loong_config::resolve_with(
                &layers,
                &self.cwd,
                Some((
                    format!(
                        "control plane: {} {}",
                        state.provenance.issuer, state.provenance.version
                    ),
                    state.layer.clone(),
                )),
            ),
            None => loong_config::resolve(&layers, &self.cwd),
        }
        .map_err(|e| e.to_string())?;
        let mut resolved = resolved;
        if let Some(state) = &pack {
            resolved.control = Some(state.provenance.clone());
            resolved.control_profile = Some(state.profile.name.clone());
        }
        Ok((resolved, pack))
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
        session_id_of(&self.session_path())
    }

    fn ensure_parent(&self) -> Result<(), String> {
        if let Some(dir) = self.session_path().parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        Ok(())
    }
}

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

/// The client's side of the control plane: where it is, and the key this install
/// pinned. Deliberately *not* the layered profile config — this is machine
/// configuration, delivered with the binary (ADR-0016 §3).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ControlFile {
    url: String,
    /// Hex, the org's public key. The signature is what makes an org layer an org
    /// layer, so this is the one value that must not be learned from the wire.
    public_key: String,
    /// A reference (`env:NAME`, `file:PATH#KEY`), never a literal token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token: Option<String>,
}

/// The machine's control-plane configuration: who this installation talks to, and
/// the key it pinned. Root-owned and delivered with the binary (image, MDM) — the
/// one value a user must not be able to change is the pin (ADR-0016 §3).
fn system_control_path() -> PathBuf {
    PathBuf::from("/etc/loongagent/control.toml")
}

/// The user's own file. On an unmanaged install it is the whole configuration; on a
/// managed one only `token` may come from here, so each user can keep their own copy
/// of the read token (the machine file lives in a root-only directory).
fn user_control_path() -> PathBuf {
    config_dir().join("control.toml")
}

fn read_control_file(path: &Path) -> Result<Option<ControlFile>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let file: ControlFile =
        toml::from_str(&text).map_err(|e| format!("{} is not valid TOML: {e}", path.display()))?;
    Ok(Some(file))
}

/// Where the two files disagree, the machine file wins for the address and the key,
/// and the user file may only supply the token. A user who could re-pin the key would
/// make the pin advisory, and a user who could not supply a token would have to be
/// able to read a root-only file.
fn merge_control(system: Option<ControlFile>, user: Option<ControlFile>) -> Option<ControlFile> {
    match (system, user) {
        (Some(system), Some(user)) => Some(ControlFile {
            url: system.url,
            public_key: system.public_key,
            token: user.token.or(system.token),
        }),
        (Some(system), None) => Some(system),
        (None, Some(user)) => Some(user),
        (None, None) => None,
    }
}

fn load_control() -> Result<Option<ControlFile>, String> {
    let system = read_control_file(&system_control_path())?;
    let user = read_control_file(&user_control_path())?;
    Ok(merge_control(system, user))
}

/// Write the configuration. `system` needs root, and is what an image or a
/// configuration-management run uses; otherwise it goes to the user's own file.
fn save_control(file: &ControlFile, system: bool) -> Result<PathBuf, String> {
    let path = if system {
        system_control_path()
    } else {
        user_control_path()
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            format!(
                "{}: {e} (writing the machine-wide file needs root)",
                parent.display()
            )
        })?;
    }
    let text = toml::to_string(file).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| {
        format!(
            "{}: {e}{}",
            path.display(),
            if system {
                " (the machine-wide file needs root: try with sudo, or drop --system to write \
                 the user's own file)"
            } else {
                ""
            }
        )
    })?;
    Ok(path)
}

/// Where the verified pack is cached. State, not configuration.
fn pack_cache(config: &loong_control::ControlConfig) -> loong_control::Cache {
    // Subdirectories of the same root: the file that the client *writes* goes under
    // one, so a backup or a `rm -rf` has a single target.
    loong_control::Cache::new(config_dir(), &config.origin())
}

/// The control-plane client configuration, with the token reference resolved.
fn control_config(
    file: &ControlFile,
) -> Result<(loong_control::ControlConfig, Option<String>), String> {
    let public_key = loong_pack::unhex(&file.public_key)
        .ok_or_else(|| "the control plane's public key must be hex".to_string())?;
    if public_key.len() != 32 {
        return Err(format!(
            "the public key must be a 32-byte Ed25519 key in hex ({} bytes given)",
            public_key.len()
        ));
    }
    // The same rule the pack's `api_key` gets, for the same reason: a token is a
    // secret, and a secret in a configuration file is one `git add` from a leak
    // (this file lives in ~/.config, which dotfiles repositories do commit).
    if let Some(token) = &file.token {
        let known = ["env:", "file:", "keychain:", "gateway:"];
        if !known.iter().any(|prefix| token.starts_with(prefix)) {
            return Err(format!(
                "the control-plane token must be a reference (env:NAME, file:PATH), not a \
                 literal — use file:/path/to/client.token (ADR-0008 §5)"
            ));
        }
    }
    let token = resolve_reference(file.token.as_deref(), &config_dir());
    let config = loong_control::ControlConfig {
        base_url: file.url.clone(),
        public_key,
        token: file.token.clone(),
        profile: None,
    };
    config.check_transport().map_err(|e| e.to_string())?;
    Ok((config, token))
}

/// What a session needs to know about the pack it is about to run on.
struct PackState {
    provenance: loong_config::ControlProvenance,
    profile: loong_config::NamedProfile,
    layer: Vec<(String, loong_config::Value)>,
    from_cache: bool,
    note: Option<String>,
}

impl PackState {
    fn summary(&self) -> String {
        format!(
            "{} — {}{}",
            self.profile.name,
            first_line(&self.profile.description),
            if self.from_cache {
                " (cached pack)"
            } else {
                ""
            }
        )
    }
}

fn first_line(text: &str) -> String {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Fetch, verify and cache the pack (or fall back to the last verified one), then
/// pick the profile this install runs.
///
/// The layers are read twice: once to learn `control.profile`, once with the pack in
/// place. That keeps "the pack may not set the choice, the choice selects the pack"
/// from becoming a special case in the resolver, and both passes are cheap.
fn sync_pack(args: &Args) -> Result<Option<PackState>, String> {
    let Some(file) = load_control()? else {
        return Ok(None);
    };
    let (config, token) = control_config(&file)?;
    let cache = pack_cache(&config);

    // The program decides, once per start: reachable means refresh, unreachable means the
    // last verified pack. There is no switch to remember.
    let synced = loong_control::sync(
        &config,
        &cache,
        token.as_deref(),
        loong_control::CLIENT_SYNC_TIMEOUT,
    )
    .map_err(|e| match e {
        // The one case where the person must act: nothing cached *and* nothing reachable.
        loong_control::ControlError::Unreachable(detail) => format!(
            "no profile pack has been synced on this machine yet, and the control plane at {} is \
             unreachable ({detail}) — start it, or run `loongagent setup` once while it is \
             reachable",
            config.base_url
        ),
        other => other.to_string(),
    })?;

    let pack = loong_config::profiles_from_payload(&synced.envelope.payload)
        .map_err(|e| format!("the control plane's pack is not usable: {e}"))?;
    note_pack_warnings(&pack);
    let layers = loong_config::layer_paths(&args.cwd, args.profile.as_deref());
    let without_pack = loong_config::resolve(&layers, &args.cwd).map_err(|e| e.to_string())?;
    let chosen = without_pack.control_profile.clone();
    let profile = pack
        .pick(chosen.as_deref())
        .map_err(|e| e.to_string())?
        .clone();

    Ok(Some(PackState {
        provenance: loong_config::ControlProvenance {
            issuer: synced.envelope.issuer.clone(),
            version: synced.envelope.version.clone(),
            key_id: synced.envelope.key_id.clone(),
            profile: profile.name.clone(),
        },
        layer: loong_config::pack_layer(&profile),
        profile,
        from_cache: synced.from_cache,
        note: synced.note,
    }))
}

/// The pack this install last verified, without touching the network — what a client
/// surface shows before a session starts.
fn cached_pack_state(args: &Args) -> Result<Option<PackState>, String> {
    let Some(file) = load_control()? else {
        return Ok(None);
    };
    let (config, _token) = control_config(&file)?;
    let Some(envelope) = pack_cache(&config).load() else {
        return Ok(None);
    };
    envelope
        .verify(&config.public_key, &[loong_pack::PROFILES_SCHEMA])
        .map_err(|e| format!("the cached pack no longer verifies: {e}"))?;
    let pack = loong_config::profiles_from_payload(&envelope.payload).map_err(|e| e.to_string())?;
    note_pack_warnings(&pack);
    let layers = loong_config::layer_paths(&args.cwd, args.profile.as_deref());
    let without_pack = loong_config::resolve(&layers, &args.cwd).map_err(|e| e.to_string())?;
    let profile = pack
        .pick(without_pack.control_profile.as_deref())
        .map_err(|e| e.to_string())?
        .clone();
    Ok(Some(PackState {
        provenance: loong_config::ControlProvenance {
            issuer: envelope.issuer.clone(),
            version: envelope.version.clone(),
            key_id: envelope.key_id.clone(),
            profile: profile.name.clone(),
        },
        layer: loong_config::pack_layer(&profile),
        profile,
        from_cache: true,
        note: None,
    }))
}

/// Configure the control plane. This is the only thing a first run needs, and the
/// only thing it may configure: which control plane, and which key this install
/// trusts. Model endpoints and routing are not a user's to set (T-07, ADR-0016).
fn setup(args: Args) -> Result<(), String> {
    let existing = load_control()?;
    let target = if args.control_system {
        system_control_path()
    } else {
        user_control_path()
    };
    println!("loongagent setup — connect this install to your firm's control plane");
    println!("  configuration: {}", target.display());
    if let Some(file) = &existing {
        println!(
            "  currently:     {} (key {}…)",
            file.url,
            &file.public_key[..file.public_key.len().min(16)]
        );
    }
    println!();

    let mut lines = std::io::stdin().lock().lines().map_while(Result::ok);
    let url = match &args.control_url {
        Some(url) => url.clone(),
        None => {
            println!("The control plane is the service your profile administrator runs.");
            ask(
                "control-plane address (https://…)",
                existing.as_ref().map(|f| f.url.as_str()),
                &mut lines,
            )
        }
    };
    if url.trim().is_empty() {
        return Err("a control-plane address is required".to_string());
    }
    // Check the address before asking for anything secret: refusing plain HTTP to
    // another machine *after* accepting a token would be a strange order to be told.
    loong_control::ControlConfig {
        base_url: url.trim().to_string(),
        public_key: vec![0; 32],
        token: None,
        profile: None,
    }
    .check_transport()
    .map_err(|e| e.to_string())?;
    let public_key = match &args.control_key {
        Some(key) => key.clone(),
        None => {
            println!();
            println!("Paste the public key your profile administrator gave you (hex).");
            println!(
                "It is what makes the pack trustworthy; a pack signed by another key is refused."
            );
            ask(
                "public key",
                existing.as_ref().map(|f| f.public_key.as_str()),
                &mut lines,
            )
        }
    };
    let token = match &args.control_token {
        Some(token) => Some(token.clone()),
        None => {
            let answer = ask(
                "token reference (env:NAME, or Enter for none)",
                existing.as_ref().and_then(|f| f.token.as_deref()),
                &mut lines,
            );
            if answer.trim().is_empty() {
                None
            } else {
                Some(answer)
            }
        }
    };

    let file = ControlFile {
        url: url.trim().to_string(),
        public_key: public_key.trim().to_string(),
        token,
    };
    let (path, synced, pack) = connect(&file, args.control_system)?;
    println!("wrote {}", path.display());
    if !args.control_system && system_control_path().exists() {
        println!(
            "note: {} exists, so it decides the address and the pinned key; this file only \
             supplies the token",
            system_control_path().display()
        );
    }
    println!();
    println!(
        "verified pack: {} {} (key {}), {} profiles",
        synced.envelope.issuer,
        synced.envelope.version,
        synced.envelope.key_id,
        pack.profiles.len()
    );
    print_profiles(&pack, None);
    println!();
    println!("choose one with:  loongagent use <name>");
    println!("then start:       loongagent tui");
    Ok(())
}

/// Say what the pack is carrying, once, where a person will see it. The TUI shows the
/// server's notes, so this reaches the interface as well as the terminal.
fn note_pack_warnings(pack: &loong_config::ProfilePack) {
    for warning in &pack.warnings {
        eprintln!("[control] warning: {warning}");
    }
}

/// Prove a control-plane configuration before saving it: validate the address, fetch
/// and verify a pack, and only then write the file. Shared by `loongagent setup` and
/// the TUI's first-run form, so the two cannot drift apart.
fn connect(
    file: &ControlFile,
    system: bool,
) -> Result<(PathBuf, loong_control::Synced, loong_config::ProfilePack), String> {
    let (config, resolved_token) = control_config(file)?;
    let cache = pack_cache(&config);
    eprintln!("contacting {} …", file.url);
    let synced = loong_control::sync(
        &config,
        &cache,
        resolved_token.as_deref(),
        loong_control::DEFAULT_TIMEOUT,
    )
    .map_err(|e| e.to_string())?;
    let pack = loong_config::profiles_from_payload(&synced.envelope.payload)
        .map_err(|e| format!("the pack is not usable: {e}"))?;
    note_pack_warnings(&pack);
    let path = save_control(file, system)?;
    Ok((path, synced, pack))
}

fn print_profiles(pack: &loong_config::ProfilePack, selected: Option<&str>) {
    for profile in &pack.profiles {
        let marker = if Some(profile.name.as_str()) == selected {
            " (selected)"
        } else {
            ""
        };
        println!("  {}{}", profile.name, marker);
        if !profile.description.trim().is_empty() {
            println!("      {}", profile.description.trim());
        }
        let models: Vec<&str> = profile.models.iter().map(|m| m.name.as_str()).collect();
        println!("      models: {}", models.join(", "));
    }
}

/// `loongagent control status|sync` — what this install trusts, and what it has.
fn control(rest: &[String]) -> Result<(), String> {
    let sub = rest
        .iter()
        .find(|a| !a.starts_with("--") && **a != "control")
        .cloned()
        .unwrap_or_else(|| "status".to_string());
    let file = load_control()?.ok_or_else(|| {
        "no control plane is configured — run `loongagent setup` with the address your firm gave \
         you"
        .to_string()
    })?;
    match sub.as_str() {
        "status" => {
            let (config, token) = control_config(&file)?;
            let cache = pack_cache(&config);
            let system = system_control_path();
            let user = user_control_path();
            println!(
                "machine file:  {} {}",
                system.display(),
                if system.exists() {
                    "(present — it decides the address and key)"
                } else {
                    "(absent)"
                }
            );
            println!(
                "user file:     {} {}",
                user.display(),
                if user.exists() {
                    "(present)"
                } else {
                    "(absent)"
                }
            );
            println!("control plane: {}", file.url);
            println!(
                "pinned key:    {}…",
                &file.public_key[..file.public_key.len().min(16)]
            );
            println!(
                "token:         {}",
                file.token.as_deref().unwrap_or("(none)")
            );
            println!("cache:         {}", cache.path().display());
            match cache.load() {
                Some(envelope) => {
                    println!(
                        "cached pack:   {} {} (key {})",
                        envelope.issuer, envelope.version, envelope.key_id
                    );
                    println!("  cached pack verifies: yes");
                    if let Ok(pack) = loong_config::profiles_from_payload(&envelope.payload) {
                        note_pack_warnings(&pack);
                        print_profiles(&pack, None);
                    }
                }
                None => println!("cached pack:   none yet"),
            }
            println!();
            println!("checking {} …", file.url);
            match loong_control::sync(
                &config,
                &cache,
                token.as_deref(),
                loong_control::DEFAULT_TIMEOUT,
            ) {
                Ok(synced) => println!(
                    "control plane: reachable; serving {} {}{}",
                    synced.envelope.issuer,
                    synced.envelope.version,
                    if synced.from_cache {
                        " (from cache — unreachable)"
                    } else {
                        ""
                    }
                ),
                Err(e) => println!("control plane: {e}"),
            }
            Ok(())
        }
        "sync" => {
            let (config, token) = control_config(&file)?;
            let cache = pack_cache(&config);
            let synced = loong_control::sync(
                &config,
                &cache,
                token.as_deref(),
                loong_control::DEFAULT_TIMEOUT,
            )
            .map_err(|e| e.to_string())?;
            let pack = loong_config::profiles_from_payload(&synced.envelope.payload)
                .map_err(|e| format!("the pack is not usable: {e}"))?;
            note_pack_warnings(&pack);
            println!(
                "{} {} (key {}), {}{}",
                synced.envelope.issuer,
                synced.envelope.version,
                synced.envelope.key_id,
                if synced.from_cache {
                    "from cache"
                } else {
                    "verified"
                },
                synced
                    .note
                    .as_deref()
                    .map(|n| format!(" — {n}"))
                    .unwrap_or_default()
            );
            print_profiles(&pack, None);
            Ok(())
        }
        other => Err(format!(
            "unknown control subcommand {other:?} — try `loongagent control status` or `sync`"
        )),
    }
}

/// `loongagent profiles` — what the control plane offers, and what this install runs.
fn profiles(args: Args) -> Result<(), String> {
    let state = cached_pack_state(&args)?.ok_or_else(|| {
        "nothing is synced yet — run `loongagent setup` or `loongagent control sync`".to_string()
    })?;
    let file = load_control()?.expect("a pack implies a control plane");
    let (config, _) = control_config(&file)?;
    let envelope = pack_cache(&config)
        .load()
        .ok_or_else(|| "no cached pack".to_string())?;
    let pack = loong_config::profiles_from_payload(&envelope.payload).map_err(|e| e.to_string())?;
    note_pack_warnings(&pack);
    println!(
        "from {} {} (key {}), signed and verified",
        envelope.issuer, envelope.version, envelope.key_id
    );
    print_profiles(&pack, Some(&state.profile.name));
    println!();
    println!("switch with: loongagent use <name>");
    Ok(())
}

/// `loongagent use <name>` — the one profile-related choice a user owns.
fn use_profile(args: Args, rest: &[String]) -> Result<(), String> {
    // `rest` is everything after `use`, so the first non-flag word *is* the name.
    let name = rest
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .ok_or_else(|| {
            "usage: loongagent use <profile-name> (see `loongagent profiles`)".to_string()
        })?;

    // Refuse a name the pack does not offer, rather than storing a typo.
    let state = cached_pack_state(&args)?.ok_or_else(|| {
        "nothing is synced yet — run `loongagent setup` or `loongagent control sync`".to_string()
    })?;
    let file = load_control()?.expect("a pack implies a control plane");
    let (config, _) = control_config(&file)?;
    let envelope = pack_cache(&config).load().ok_or("no cached pack")?;
    let pack = loong_config::profiles_from_payload(&envelope.payload).map_err(|e| e.to_string())?;
    note_pack_warnings(&pack);
    let chosen = pack.pick(Some(&name)).map_err(|e| e.to_string())?.clone();
    let _ = state;

    let path = loong_config::layer_paths(&args.cwd, None)
        .into_iter()
        .find(|(layer, _)| *layer == loong_config::Layer::User)
        .map(|(_, path)| path)
        .ok_or_else(|| "no user-layer configuration path on this machine".to_string())?;
    let mut document: toml::Value = match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?,
        Err(_) => toml::Value::Table(toml::map::Map::new()),
    };
    let table = document
        .as_table_mut()
        .ok_or_else(|| format!("{} is not a TOML table", path.display()))?;
    let control = table
        .entry("control".to_string())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    let control = control
        .as_table_mut()
        .ok_or_else(|| "control is not a table in your profile".to_string())?;
    control.insert(
        "profile".to_string(),
        toml::Value::String(chosen.name.clone()),
    );

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(
        &path,
        toml::to_string(&document).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("{}: {e}", path.display()))?;
    println!("now running profile {} in {}", chosen.name, path.display());
    if !chosen.description.trim().is_empty() {
        println!("  {}", chosen.description.trim());
    }
    Ok(())
}

/// The client's one root: `$LOONGAGENT_HOME`, else `~/.loongagent` (defined in the
/// config crate, because the user-layer path comes from the same rule).
fn config_dir() -> PathBuf {
    loong_config::loongagent_home()
}

/// `~/.loongagent/projects/<project>/session.jsonl` (or under `$LOONGAGENT_HOME`).
///
/// Sessions are state, not configuration, so they live under the state dir and are
/// keyed by the project they belong to: resuming yesterday's work in a directory is
/// the default, and `--session` is only for choosing differently.
/// Where one project's sessions live. A project is a directory; its slug names the
/// folder, so two checkouts of the same tree do not share a conversation by accident.
fn session_dir(cwd: &Path) -> PathBuf {
    session_dir_in(&config_dir(), cwd)
}

/// The directory a project's sessions live in, under a given root. The root is a
/// parameter so the tests can use one of their own instead of mutating the environment
/// of every other test in the binary.
fn session_dir_in(root: &Path, cwd: &Path) -> PathBuf {
    root.join("projects").join(project_slug(cwd))
}

/// A session **is** its file, and the file's name is its id: no index to keep in step,
/// and `ls` is a valid way to look. The first session ever written kept the name
/// `session.jsonl`; it answers to `default` rather than looking like an accident.
fn session_id_of(path: &Path) -> String {
    match path
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
    {
        Some(stem) if stem != "session" && !stem.is_empty() => stem,
        _ => "default".to_string(),
    }
}

fn session_path_for(id: &str, cwd: &Path) -> PathBuf {
    session_path_in(&config_dir(), id, cwd)
}

fn session_path_in(root: &Path, id: &str, cwd: &Path) -> PathBuf {
    if id == "default" {
        session_dir_in(root, cwd).join("session.jsonl")
    } else {
        session_dir_in(root, cwd).join(format!("{id}.jsonl"))
    }
}

/// A new id: eight characters, from a clock and this process, in an alphabet with the
/// pairs people misread removed (`0`/`O`, `1`/`l`/`I`). Short enough to type back,
/// random enough that a prefix is not a guess.
fn new_session_id(cwd: &Path) -> String {
    new_session_id_in(&config_dir(), cwd)
}

fn new_session_id_in(root: &Path, cwd: &Path) -> String {
    const ALPHABET: &[u8] = b"23456789abcdefghjkmnpqrstuvwxyz";
    let dir = session_dir_in(root, cwd);
    for attempt in 0..256u64 {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let mut hash = 0xcbf2_9ce4_8422_2325u64 ^ u64::from(std::process::id()) ^ attempt;
        for byte in nanos
            .to_le_bytes()
            .iter()
            .chain(std::process::id().to_le_bytes().iter())
        {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        let id: String = (0..8)
            .map(|i| ALPHABET[((hash >> (i * 5)) % ALPHABET.len() as u64) as usize] as char)
            .collect();
        if !dir.join(format!("{id}.jsonl")).exists() {
            return id;
        }
    }
    // Asking the clock harder is not a plan; this cannot collide with a base32 id.
    format!("p{}", std::process::id())
}

/// What a session looks like from outside, read from the log itself — the log is the
/// record, so a list must not need an index that could disagree with it.
#[derive(Debug, Clone)]
struct SessionSummary {
    id: String,
    path: PathBuf,
    project: String,
    modified: SystemTime,
    events: u64,
    turns: u64,
    first_prompt: Option<String>,
}

fn session_summaries_in(dir: &Path, project: &str) -> Vec<SessionSummary> {
    let mut sessions = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return sessions;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(UNIX_EPOCH);
        let mut events = 0u64;
        let mut turns = 0u64;
        let mut first_prompt = None;
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            events += 1;
            if value.get("kind").and_then(|k| k.as_str()) != Some("message.appended") {
                continue;
            }
            let data = value.get("data").cloned().unwrap_or_default();
            if data.get("role").and_then(|r| r.as_str()) != Some("user") {
                continue;
            }
            turns += 1;
            if first_prompt.is_none() {
                let text = data
                    .get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or_default();
                let flattened = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if !flattened.is_empty() {
                    first_prompt = Some(flattened);
                }
            }
        }
        sessions.push(SessionSummary {
            id: session_id_of(&path),
            project: project.to_string(),
            modified,
            events,
            turns,
            first_prompt,
            path,
        });
    }
    sessions.sort_by(|a, b| b.modified.cmp(&a.modified).then(a.id.cmp(&b.id)));
    sessions
}

/// This project's sessions, newest first.
fn session_summaries(cwd: &Path) -> Vec<SessionSummary> {
    session_summaries_in(&session_dir(cwd), &project_slug(cwd))
}

/// Every project's sessions, newest first. `--all` for the list, and how `resume` finds
/// a session that is not in the current directory.
fn all_session_summaries() -> Vec<SessionSummary> {
    all_session_summaries_in(&config_dir())
}

fn all_session_summaries_in(root: &Path) -> Vec<SessionSummary> {
    let mut sessions = Vec::new();
    let root = root.join("projects");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return sessions;
    };
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let slug = entry.file_name().to_string_lossy().to_string();
        sessions.extend(session_summaries_in(&entry.path(), &slug));
    }
    sessions.sort_by(|a, b| b.modified.cmp(&a.modified).then(a.id.cmp(&b.id)));
    sessions
}

fn newest_session(cwd: &Path) -> Option<PathBuf> {
    newest_session_in(&config_dir(), cwd)
}

fn newest_session_in(root: &Path, cwd: &Path) -> Option<PathBuf> {
    session_summaries_in(&session_dir_in(root, cwd), &project_slug(cwd))
        .first()
        .map(|s| s.path.clone())
}

/// Turn what someone typed into a session file. A path stays a path; anything else is an
/// id — exact, an unambiguous prefix of one, or `latest`. The error names the candidates
/// rather than saying "not found" when the answer is "several".
fn resolve_session(spec: &str, cwd: &Path) -> Result<PathBuf, String> {
    resolve_session_in(&config_dir(), spec, cwd)
}

fn resolve_session_in(root: &Path, spec: &str, cwd: &Path) -> Result<PathBuf, String> {
    if spec.contains('/') || spec.ends_with(".jsonl") {
        return Ok(PathBuf::from(spec));
    }
    let dir = session_dir_in(root, cwd);
    let wanted = spec.to_ascii_lowercase();
    if wanted == "latest" || wanted == "last" {
        return newest_session_in(root, cwd).ok_or_else(|| {
            format!(
                "no sessions in {} yet — start one with `loongagent tui`",
                dir.display()
            )
        });
    }
    let sessions = session_summaries_in(&dir, &project_slug(cwd));
    if let Some(path) = match_session(&sessions, &wanted)? {
        return Ok(path);
    }
    // Not here: it may be a session from another project, which `sessions --all` shows.
    // Resuming it is allowed; the workspace stays wherever this command was run.
    let elsewhere = all_session_summaries_in(root);
    match match_session_across_projects(&elsewhere, &wanted)? {
        Some(path) => Ok(path),
        None => Err(format!(
            "no session {spec:?} — `loongagent sessions` lists what exists{}; `loongagent tui` \
             starts a new one",
            match sessions.len() {
                0 => String::new(),
                _ => format!(
                    " (here: {})",
                    sessions
                        .iter()
                        .map(|s| s.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }
        )),
    }
}

/// The same, but for sessions in *other* projects: here an exact id is not enough to
/// disambiguate, because every legacy session in every project is called `default`.
/// Choosing one silently would be worse than saying which ones exist.
fn match_session_across_projects(
    sessions: &[SessionSummary],
    wanted: &str,
) -> Result<Option<PathBuf>, String> {
    let exact: Vec<&SessionSummary> = sessions.iter().filter(|s| s.id == wanted).collect();
    if let [one] = exact.as_slice() {
        return Ok(Some(one.path.clone()));
    }
    let candidates: Vec<&SessionSummary> = if exact.is_empty() {
        sessions
            .iter()
            .filter(|s| s.id.starts_with(wanted))
            .collect()
    } else {
        exact
    };
    match candidates.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(one.path.clone())),
        many => Err(format!(
            "{wanted:?} matches {} sessions in other projects ({}); run it where it lives, or \
             pass the full path",
            many.len(),
            many.iter()
                .map(|s| format!("{} in {}", s.id, s.project))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Exact id, then an unambiguous prefix. `Ok(None)` means "not here"; the error is only
/// for the case where the answer is "several".
fn match_session(sessions: &[SessionSummary], wanted: &str) -> Result<Option<PathBuf>, String> {
    if let Some(found) = sessions.iter().find(|s| s.id == wanted) {
        return Ok(Some(found.path.clone()));
    }
    let matching: Vec<&SessionSummary> = sessions
        .iter()
        .filter(|s| s.id.starts_with(wanted))
        .collect();
    match matching.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(one.path.clone())),
        many => Err(format!(
            "{wanted:?} matches {} sessions ({}); give more characters",
            many.len(),
            many.iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// How long ago, in the only units a person reads in a list.
fn age_text(then: SystemTime) -> String {
    let secs = SystemTime::now()
        .duration_since(then)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match secs {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86399 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86400),
    }
}

fn clip(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let head: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{head}…")
}

/// `loongagent sessions [--all] [--limit N] [--cwd DIR]`
fn list_sessions(rest: &[String]) -> Result<(), String> {
    let mut cwd = std::env::current_dir().map_err(|e| format!("current directory: {e}"))?;
    let mut all = false;
    let mut limit = 20usize;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--all" => all = true,
            "--limit" => {
                i += 1;
                limit = rest
                    .get(i)
                    .and_then(|value| value.parse().ok())
                    .ok_or("--limit needs a number")?;
            }
            "--cwd" => {
                i += 1;
                cwd = PathBuf::from(rest.get(i).ok_or("--cwd needs a path")?);
            }
            other => return Err(format!("unexpected {other:?}\n\n{USAGE}")),
        }
        i += 1;
    }
    let sessions = if all {
        all_session_summaries()
    } else {
        session_summaries(&cwd)
    };
    if sessions.is_empty() {
        println!(
            "no sessions {} yet — `loongagent tui` starts one",
            if all {
                "on this machine".to_string()
            } else {
                format!("in {}", project_slug(&cwd))
            }
        );
        return Ok(());
    }
    // The one a bare `loongagent tui` would pick up. Compared by path, not by id:
    // every legacy session is called `default`, and marking five rows is not a mark.
    let current = newest_session(&cwd);
    println!(
        "{} session{} (newest first){}",
        sessions.len(),
        if sessions.len() == 1 { "" } else { "s" },
        if all {
            ""
        } else {
            " — --all shows every project"
        }
    );
    if all {
        println!(
            "  {:<3} {:<9} {:<11} {:>5} {:>7}  {:<20} {}",
            "", "id", "when", "turns", "events", "project", "first prompt"
        );
    } else {
        println!(
            "  {:<3} {:<9} {:<11} {:>5} {:>7}  {}",
            "", "id", "when", "turns", "events", "first prompt"
        );
    }
    for session in sessions.iter().take(limit) {
        let mark = if Some(&session.path) == current.as_ref() {
            "*"
        } else {
            " "
        };
        let prompt = session
            .first_prompt
            .as_deref()
            .map(|text| clip(text, 46))
            .unwrap_or_else(|| "—".to_string());
        if all {
            println!(
                "  {mark:<3} {:<9} {:<11} {:>5} {:>7}  {:<20} {}",
                session.id,
                age_text(session.modified),
                session.turns,
                session.events,
                clip(&session.project, 20),
                prompt
            );
        } else {
            println!(
                "  {mark:<3} {:<9} {:<11} {:>5} {:>7}  {}",
                session.id,
                age_text(session.modified),
                session.turns,
                session.events,
                prompt
            );
        }
    }
    if sessions.len() > limit {
        println!("  … {} more (--limit N)", sessions.len() - limit);
    }
    println!();
    println!(
        "  * = what `loongagent tui` picks up here; `loongagent resume <id>` opens any of them"
    );
    Ok(())
}

/// `loongagent resume [ID]` — the friendly spelling of `tui --session …`.
fn resume(rest: &[String]) -> Result<(), String> {
    let mut args = rest.to_vec();
    let positional = args
        .first()
        .filter(|first| !first.starts_with('-'))
        .cloned();
    if positional.is_some() {
        args.remove(0);
    }
    args.splice(
        0..0,
        [
            "--session".to_string(),
            positional.unwrap_or_else(|| "latest".to_string()),
        ],
    );
    full_screen(Args::parse(&args)?)
}

fn default_session_path(cwd: &Path) -> PathBuf {
    config_dir()
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
        "control" => control(rest),
        "profiles" => profiles(Args::parse(rest)?),
        "use" => use_profile(Args::parse(rest)?, rest),
        "tui" => full_screen(Args::parse(rest)?),
        "resume" => resume(rest),
        "sessions" => list_sessions(rest),
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
                "loongagent {} (schema {}, commit {})",
                env!("CARGO_PKG_VERSION"),
                loong_schema::SCHEMA_VERSION,
                env!("LOONG_COMMIT")
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
    let (settings, pack) = args.settings_with_pack()?;
    if let Some(state) = &pack {
        eprintln!(
            "[control] {} {} (key {}) — profile: {}",
            state.provenance.issuer,
            state.provenance.version,
            state.provenance.key_id,
            state.summary()
        );
        if let Some(note) = &state.note {
            eprintln!("[control] {note}");
        }
    }
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
    let mut kernel = if session_path.exists() {
        Kernel::open_with_models(&session_path, config, models)
    } else {
        Kernel::create_with_models(&session_path, config, models)
    }
    .map_err(|e| e.to_string())?;
    // Provenance first, then what it means: a session should be able to answer "what
    // policy was in force, and which profile did I run?" long after the fact.
    if let Some(state) = &pack {
        kernel
            .record_control_synced(&state.provenance, state.from_cache, state.note.as_deref())
            .map_err(|e| e.to_string())?;
        kernel
            .record_profile_selected(&state.profile)
            .map_err(|e| e.to_string())?;
    }
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
/// Where a `file:` reference points: absolute, `~/…`, or relative to the profile in
/// use, so a profile and its credentials file move together.
fn credential_path(path: &str, base: &Path) -> Option<PathBuf> {
    if let Some(relative) = path.strip_prefix("~/") {
        return std::env::var("HOME")
            .ok()
            .map(|home| PathBuf::from(home).join(relative));
    }
    if Path::new(path).is_absolute() {
        return Some(PathBuf::from(path));
    }
    Some(base.join(path))
}

/// Environment files the client reads, in order, so a pack can name a variable
/// (`api_key = "env:DEEPSEEK_API_KEY"`) and still work without the operator
/// exporting anything before every run.
///
/// Two rules, both deliberate:
///
/// - a variable that is **already set** is never overridden, so the shell — and
///   anything a configuration-management run exported — always wins;
/// - the user's own file wins over the workspace's, so a checked-out repository
///   cannot replace a credential the user chose.
fn env_file_paths(user_config_dir: &Path, workspace: &Path) -> Vec<PathBuf> {
    vec![user_config_dir.join("env"), workspace.join(".env")]
}

/// Parse the files into `(name, value)` pairs, first definition winning.
fn read_env_files(paths: &[PathBuf]) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = Vec::new();
    for path in paths {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
                continue;
            }
            let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
            let split = line.char_indices().find(|(_, c)| *c == '=' || *c == ':');
            let Some((index, _)) = split else { continue };
            let name = line[..index].trim().to_string();
            let value = line[index + 1..]
                .trim()
                .trim_matches('"')
                .trim_matches('\'');
            if name.is_empty() || value.is_empty() {
                continue;
            }
            if found.iter().any(|(existing, _)| *existing == name) {
                continue;
            }
            found.push((name, value.to_string()));
        }
    }
    found
}

/// Apply them to this process, naming the file each variable actually came from.
/// Names are printed, never values.
fn load_env_files(user_config_dir: &Path, workspace: &Path) {
    let mut by_file: Vec<(String, Vec<String>)> = Vec::new();
    for path in env_file_paths(user_config_dir, workspace) {
        let pairs = read_env_files(std::slice::from_ref(&path));
        let mut taken = Vec::new();
        for (name, value) in pairs {
            if std::env::var_os(&name).is_none() {
                std::env::set_var(&name, value);
                taken.push(name);
            }
        }
        if !taken.is_empty() {
            by_file.push((path.display().to_string(), taken));
        }
    }
    for (file, names) in by_file {
        eprintln!("[config] environment from {file}: {}", names.join(", "));
    }
}

/// Read one named secret out of a file.
///
/// Line-based and forgiving about the separator, because the file is something an
/// operator already has rather than something we dictate:
///
/// ```text
/// NAME=value          NAME: value          export NAME=value
/// [keys]              # a comment          NAME = "quoted value"
/// ```
///
/// A missing key names the keys that *are* there and never their values — an error
/// message is not the place to print a secret.
fn read_credential(path: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| eprintln!("[config] cannot read {}: {e}", path.display()))
        .ok()?;
    let mut names = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        // The first `=` or `:` only, so a value may contain either character.
        let split = line.char_indices().find(|(_, c)| *c == '=' || *c == ':');
        let Some((index, _)) = split else { continue };
        let name = line[..index].trim();
        let value = line[index + 1..]
            .trim()
            .trim_matches('"')
            .trim_matches('\'');
        names.push(name.to_string());
        if name == key {
            if value.is_empty() {
                eprintln!("[config] {} has {key} but it is empty", path.display());
                return None;
            }
            return Some(value.to_string());
        }
    }
    eprintln!(
        "[config] {} has no {key}; it defines: {}",
        path.display(),
        if names.is_empty() {
            "(nothing)".to_string()
        } else {
            names.join(", ")
        }
    );
    None
}

fn resolve_reference(value: Option<&str>, base: &Path) -> Option<String> {
    let value = value?;
    if let Some(name) = value.strip_prefix("env:") {
        return std::env::var(name).ok();
    }
    if let Some(rest) = value.strip_prefix("file:") {
        // Two shapes, because both are useful and the control plane writes one of them:
        //   file:PATH          the file *is* the secret (what `--client-token-file` writes)
        //   file:PATH#KEY      `[keys] KEY = "…"` inside a shared credentials file
        return match rest.split_once('#') {
            Some((path, key)) => {
                let path = credential_path(path, base)?;
                read_credential(&path, key).or_else(|| {
                    eprintln!("[config] {} has no usable keys.{key}", path.display());
                    None
                })
            }
            None => {
                let path = credential_path(rest, base)?;
                match std::fs::read_to_string(&path) {
                    Ok(text) if !text.trim().is_empty() => Some(text.trim().to_string()),
                    Ok(_) => {
                        eprintln!("[config] {} is empty", path.display());
                        None
                    }
                    Err(e) => {
                        eprintln!("[config] cannot read {}: {e}", path.display());
                        None
                    }
                }
            }
        };
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
    // First run: ask here, in the terminal UI, rather than sending the user off to
    // discover a command. Doing it before the session starts is also why nothing has
    // to be reconfigured mid-session.
    if load_control()?.is_none() && args.model.is_none() {
        // In a pipe the form cannot be drawn: refuse first, in the same words the TUI
        // would use, rather than asking for three fields that cannot be seen.
        loong_tui_app::full_screen_needs_a_terminal()?;
        if let Some((url, public_key, token)) = loong_tui_app::setup_form()? {
            let file = ControlFile {
                url,
                public_key,
                token: if token.trim().is_empty() {
                    None
                } else {
                    Some(token)
                },
            };
            match connect(&file, false) {
                Ok((path, synced, pack)) => eprintln!(
                    "wrote {} — verified {} {} ({} profiles)",
                    path.display(),
                    synced.envelope.issuer,
                    synced.envelope.version,
                    pack.profiles.len()
                ),
                Err(e) => return Err(format!("not connected: {e}")),
            }
        }
    }

    // What a first run needs to see, inside the UI rather than on a stderr that the
    // alternate screen would overwrite: where this session lives, and whether a model
    // is configured at all.
    // The client resolves the same layers the server does, so it can tell the user
    // whether a model exists without asking the server about it.
    let (_settings, pack) = args.settings_with_pack()?;
    if let Some(state) = &pack {
        eprintln!(
            "[control] {} {} — profile: {}",
            state.provenance.issuer,
            state.provenance.version,
            state.summary()
        );
    }
    let session_id = args.session_id();
    let resume_hint = format!("loongagent resume {session_id}");
    let mut intro = vec![format!("session {session_id} — reopen with: {resume_hint}")];
    if attached.existing_events > 0 {
        intro.push(format!(
            "resuming {} events from that session",
            attached.existing_events
        ));
    }
    match cached_pack_state(&args) {
        Ok(Some(state)) => intro.push(format!("profile: {}", state.summary())),
        Ok(None) if args.model.is_none() => intro.push(
            "not connected to a control plane — run `loongagent setup` with the address your \
             firm gave you (or pass --model demo-write to try the loop with no model)"
                .to_string(),
        ),
        Ok(None) => {}
        Err(e) => intro.push(format!("control plane: {e}")),
    }
    let outcome = loong_tui_app::run(
        client,
        attached.session_id,
        attached.schema_version,
        notes,
        intro,
    );
    // Leaving the interface — by `/exit`, ctrl-c, or an error — says where the session
    // went, so the way out is not a dead end.
    println!("session {session_id}");
    println!("reopen:  {resume_hint}");
    outcome
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
    let (settings, pack) = args.settings_with_pack()?;
    if let Some(state) = &pack {
        eprintln!(
            "[control] {} {} — profile: {}",
            state.provenance.issuer,
            state.provenance.version,
            state.summary()
        );
    }
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
    // Show what the session already contains, for the same reason the TUI does: a
    // reopened conversation should look reopened.
    for line in client.replay().unwrap_or_default() {
        println!("{line}");
    }

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
    let (settings, pack) = args.settings_with_pack()?;
    if let Some(state) = &pack {
        eprintln!(
            "[control] {} {} — profile: {}",
            state.provenance.issuer,
            state.provenance.version,
            state.summary()
        );
    }
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
    let (settings, pack) = args.settings_with_pack()?;
    if let Some(state) = &pack {
        eprintln!(
            "[control] {} {} — profile: {}",
            state.provenance.issuer,
            state.provenance.version,
            state.summary()
        );
    }
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
    let (settings, pack) = args.settings_with_pack()?;
    if let Some(state) = &pack {
        eprintln!(
            "[control] {} {} — profile: {}",
            state.provenance.issuer,
            state.provenance.version,
            state.summary()
        );
    }
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
    fn the_control_file_round_trips_and_its_key_is_checked() {
        let file = ControlFile {
            url: "https://control.acme.internal".to_string(),
            public_key: "ab".repeat(32),
            token: Some("env:LOONGAGENT_CONTROL_TOKEN".to_string()),
        };
        let text = toml::to_string(&file).unwrap();
        let back: ControlFile = toml::from_str(&text).unwrap();
        assert_eq!(back.url, file.url);
        assert_eq!(back.public_key, file.public_key);
        assert_eq!(back.token, file.token);
        let (config, _) = control_config(&back).unwrap();
        assert_eq!(config.base_url, "https://control.acme.internal");
        assert_eq!(config.public_key.len(), 32);

        // A key that is not a 32-byte hex Ed25519 key is refused before anything is
        // sent anywhere.
        let short = ControlFile {
            public_key: "abcd".to_string(),
            ..back.clone()
        };
        assert!(control_config(&short).unwrap_err().contains("32-byte"));
        let not_hex = ControlFile {
            public_key: "zz".repeat(32),
            ..back.clone()
        };
        assert!(control_config(&not_hex).unwrap_err().contains("hex"));

        // And the token is never a literal: only a reference resolves.
        let literal = ControlFile {
            token: Some("a-real-secret".to_string()),
            ..back
        };
        assert_eq!(
            resolve_reference(literal.token.as_deref(), Path::new("/tmp")),
            Some("a-real-secret".to_string())
        );
    }

    #[test]
    fn a_user_file_may_supply_a_token_but_not_a_different_pin() {
        let machine = ControlFile {
            url: "https://control.acme.internal".to_string(),
            public_key: "ab".repeat(32),
            token: None,
        };
        let mine = ControlFile {
            url: "https://control.evil.invalid".to_string(),
            public_key: "cd".repeat(32),
            token: Some("file:/home/me/.loongagent/client.token".to_string()),
        };
        let merged = merge_control(Some(machine.clone()), Some(mine.clone())).unwrap();
        assert_eq!(
            merged.url, machine.url,
            "the machine decides who we talk to"
        );
        assert_eq!(merged.public_key, machine.public_key, "and what we trust");
        assert_eq!(
            merged.token, mine.token,
            "the user supplies their own token"
        );

        // An unmanaged install is the user's file, whole.
        assert_eq!(merge_control(None, Some(mine.clone())).unwrap(), mine);
        // A managed one with no user file is the machine's, whole.
        assert_eq!(merge_control(Some(machine.clone()), None).unwrap(), machine);
        assert!(merge_control(None, None).is_none());
    }

    #[test]
    fn environment_files_are_read_with_the_user_winning_and_the_shell_above_both() {
        let dir = std::env::temp_dir().join(format!("loong-env-{}", std::process::id()));
        let user = dir.join("cfg");
        let workspace = dir.join("ws");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            &user.join("env"),
            "# the user's own file\nexport DEEPSEEK_API_KEY=\"sk-user\"\nSHARED: from-user\n",
        )
        .unwrap();
        std::fs::write(
            &workspace.join(".env"),
            "DEEPSEEK_API_KEY=sk-workspace\nSHARED=from-workspace\nONLY_HERE=yes\n",
        )
        .unwrap();

        let pairs = read_env_files(&env_file_paths(&user, &workspace));
        assert_eq!(
            pairs
                .iter()
                .find(|(n, _)| n == "DEEPSEEK_API_KEY")
                .unwrap()
                .1,
            "sk-user",
            "the user's file wins over a checked-out repository"
        );
        assert_eq!(
            pairs.iter().find(|(n, _)| n == "SHARED").unwrap().1,
            "from-user"
        );
        assert_eq!(
            pairs.iter().find(|(n, _)| n == "ONLY_HERE").unwrap().1,
            "yes",
            "and the workspace can still supply what the user did not"
        );

        // A missing file is not an error, and neither is an empty one.
        let none = read_env_files(&[dir.join("absent"), dir.join("also-absent")]);
        assert!(none.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_literal_control_plane_token_is_refused() {
        let literal = ControlFile {
            url: "https://control.acme.internal".to_string(),
            public_key: "ab".repeat(32),
            token: Some("ct-abcdef123456".to_string()),
        };
        let error = control_config(&literal).unwrap_err();
        assert!(error.contains("reference"), "{error}");
        assert!(error.contains("ADR-0008"), "{error}");
        // The two shapes that are not literals still work.
        for reference in [
            "env:LOONGAGENT_CONTROL_TOKEN",
            "file:/etc/loongagent/client.token",
        ] {
            let file = ControlFile {
                token: Some(reference.to_string()),
                ..literal.clone()
            };
            assert!(
                control_config(&file).is_ok(),
                "{reference} must be accepted"
            );
        }
    }

    #[test]
    fn a_control_plane_address_that_would_leak_a_token_is_refused() {
        let file = ControlFile {
            url: "http://control.acme.internal".to_string(),
            public_key: "ab".repeat(32),
            token: Some("env:X".to_string()),
        };
        let error = control_config(&file).unwrap_err();
        assert!(error.contains("TLS"), "{error}");
    }

    #[test]
    fn a_file_reference_reads_either_a_table_or_a_bare_secret() {
        let dir = std::env::temp_dir().join(format!("loong-secret-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // What `loongagent-control --client-token-file` writes: the file is the token.
        let bare = dir.join("client.token");
        std::fs::write(&bare, "ct-secret\n").unwrap();
        assert_eq!(
            resolve_reference(Some(&format!("file:{}", bare.display())), &dir),
            Some("ct-secret".to_string()),
            "trailing whitespace is not part of a token"
        );
        // An empty file is a mistake, not an empty token.
        let empty = dir.join("empty.token");
        std::fs::write(&empty, "   \n").unwrap();
        assert_eq!(
            resolve_reference(Some(&format!("file:{}", empty.display())), &dir),
            None
        );
        // And the table form still works, with the key named.
        let table = dir.join("credentials.toml");
        std::fs::write(&table, "[keys]\nCONTROL_TOKEN = \"from-table\"\n").unwrap();
        assert_eq!(
            resolve_reference(
                Some(&format!("file:{}#CONTROL_TOKEN", table.display())),
                &dir
            ),
            Some("from-table".to_string())
        );
        // A relative path is read beside the profile in use.
        assert_eq!(
            resolve_reference(Some("file:client.token"), &dir),
            Some("ct-secret".to_string())
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_secret_file_is_read_in_the_shapes_operators_actually_write() {
        let dir = std::env::temp_dir().join(format!("loong-creds-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("secrets");

        // `KEY: value` — a `.env` written by hand.
        std::fs::write(&path, "# comment\nDEEPSEEK_API_KEY: sk-colon\nOTHER: x\n").unwrap();
        assert_eq!(
            read_credential(&path, "DEEPSEEK_API_KEY").as_deref(),
            Some("sk-colon")
        );

        // `KEY=value`, quoted, with `export`, and a value containing `=`.
        std::fs::write(
            &path,
            "export DEEPSEEK_API_KEY=\"sk-equals=\"\nEMPTY=\n#NAME=ignored\n",
        )
        .unwrap();
        assert_eq!(
            read_credential(&path, "DEEPSEEK_API_KEY").as_deref(),
            Some("sk-equals=")
        );
        assert_eq!(
            read_credential(&path, "EMPTY"),
            None,
            "an empty value is not a secret"
        );
        assert_eq!(
            read_credential(&path, "NAME"),
            None,
            "a comment is not a definition"
        );

        // A TOML credentials file still works, table header and quotes included.
        std::fs::write(&path, "[keys]\nDEEPSEEK_API_KEY = \"sk-toml\"\n").unwrap();
        assert_eq!(
            read_credential(&path, "DEEPSEEK_API_KEY").as_deref(),
            Some("sk-toml")
        );

        assert_eq!(read_credential(&path, "ABSENT"), None);
        assert_eq!(read_credential(&dir.join("absent"), "ANY"), None);
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;

    /// A root of our own: `config_dir()` would read the developer's environment.
    fn temp_root(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("loong-sessions-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A session log with `turns` user turns, enough for the listing to read.
    fn write_session(path: &Path, prompts: &[&str]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut text = String::from(
            "{\"kind\":\"skills.loaded\",\"data\":{\"count\":1},\"seq\":1,\"time_ms\":1,\"id\":\"e1\"}\n",
        );
        let mut seq = 2;
        for prompt in prompts {
            text.push_str(&format!(
                "{{\"kind\":\"message.appended\",\"data\":{{\"role\":\"user\",\"text\":{}}},\"seq\":{seq},\"time_ms\":1,\"id\":\"e{seq}\"}}\n",
                serde_json::to_string(prompt).unwrap()
            ));
            seq += 1;
            text.push_str(&format!(
                "{{\"kind\":\"message.appended\",\"data\":{{\"role\":\"assistant\",\"text\":\"ok\"}},\"seq\":{seq},\"time_ms\":1,\"id\":\"e{seq}\"}}\n"
            ));
            seq += 1;
        }
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn a_new_id_is_short_and_says_itself_out_loud() {
        let root = temp_root("id");
        let cwd = Path::new("/tmp/project");
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..200 {
            let id = new_session_id_in(&root, cwd);
            assert_eq!(id.chars().count(), 8, "{id}");
            assert!(
                id.chars()
                    .all(|c| "23456789abcdefghjkmnpqrstuvwxyz".contains(c)),
                "{id} must avoid the characters people misread"
            );
            assert!(seen.insert(id), "the generator repeated itself");
        }
        // And it will not hand out a name that is already taken.
        let taken = session_path_in(&root, "abcdefgh", cwd);
        std::fs::create_dir_all(taken.parent().unwrap()).unwrap();
        std::fs::write(&taken, "").unwrap();
        for _ in 0..50 {
            assert_ne!(new_session_id_in(&root, cwd), "abcdefgh");
        }
    }

    #[test]
    fn the_first_session_ever_kept_its_name_and_answers_to_default() {
        assert_eq!(session_id_of(Path::new("/x/session.jsonl")), "default");
        assert_eq!(session_id_of(Path::new("/x/k7m2xq3f.jsonl")), "k7m2xq3f");
        assert_eq!(
            session_path_for("default", Path::new("/tmp/p"))
                .file_name()
                .unwrap(),
            "session.jsonl"
        );
        assert_eq!(
            session_path_for("k7m2xq3f", Path::new("/tmp/p"))
                .file_name()
                .unwrap(),
            "k7m2xq3f.jsonl"
        );
    }

    #[test]
    fn a_session_is_summarised_from_its_own_log() {
        let root = temp_root("summary");
        let cwd = Path::new("/tmp/project");
        let dir = session_dir_in(&root, cwd);
        write_session(&dir.join("aaaa1111.jsonl"), &["first question", "second"]);
        // The derived index and anything else in the directory are not sessions.
        std::fs::write(dir.join("aaaa1111.sqlite"), "not a log").unwrap();
        std::fs::write(dir.join("notes.txt"), "not a log").unwrap();

        let sessions = session_summaries_in(&dir, "project");
        assert_eq!(sessions.len(), 1);
        let one = &sessions[0];
        assert_eq!(one.id, "aaaa1111");
        assert_eq!(one.turns, 2);
        assert_eq!(one.events, 5, "one skills.loaded plus two exchanges");
        assert_eq!(one.first_prompt.as_deref(), Some("first question"));
    }

    #[test]
    fn resolve_takes_an_exact_id_a_prefix_and_latest() {
        let root = temp_root("resolve");
        let cwd = Path::new("/tmp/project");
        let dir = session_dir_in(&root, cwd);
        write_session(&dir.join("aaaa1111.jsonl"), &["older"]);
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_session(&dir.join("bbbb2222.jsonl"), &["newer"]);

        assert_eq!(
            resolve_session_in(&root, "aaaa1111", cwd)
                .unwrap()
                .file_name()
                .unwrap(),
            "aaaa1111.jsonl"
        );
        assert_eq!(
            resolve_session_in(&root, "bbbb", cwd)
                .unwrap()
                .file_name()
                .unwrap(),
            "bbbb2222.jsonl",
            "an unambiguous prefix is enough"
        );
        assert_eq!(
            resolve_session_in(&root, "latest", cwd)
                .unwrap()
                .file_name()
                .unwrap(),
            "bbbb2222.jsonl",
            "the newest by modification time"
        );
        assert_eq!(
            resolve_session_in(&root, "./elsewhere.jsonl", cwd).unwrap(),
            PathBuf::from("./elsewhere.jsonl"),
            "a path is a path, even when it does not exist yet"
        );
        assert_eq!(
            resolve_session_in(&root, "AAAA", cwd)
                .unwrap()
                .file_name()
                .unwrap(),
            "aaaa1111.jsonl",
            "ids are matched case-insensitively"
        );
    }

    #[test]
    fn a_prefix_that_fits_two_sessions_says_which() {
        let root = temp_root("ambiguous");
        let cwd = Path::new("/tmp/project");
        let dir = session_dir_in(&root, cwd);
        write_session(&dir.join("k7m2xq3f.jsonl"), &["one"]);
        write_session(&dir.join("k7m2yyyy.jsonl"), &["two"]);
        let error = resolve_session_in(&root, "k7m2", cwd).unwrap_err();
        assert!(error.contains("matches 2"), "{error}");
        assert!(
            error.contains("k7m2xq3f") && error.contains("k7m2yyyy"),
            "{error}"
        );
    }

    #[test]
    fn an_unknown_id_names_the_way_to_find_one() {
        let root = temp_root("unknown");
        let cwd = Path::new("/tmp/project");
        write_session(
            &session_dir_in(&root, cwd).join("aaaa1111.jsonl"),
            &["only"],
        );
        let error = resolve_session_in(&root, "zzzz", cwd).unwrap_err();
        assert!(error.contains("loongagent sessions"), "{error}");

        // Nothing at all, not even in another project.
        let empty = temp_root("unknown-empty");
        let error = resolve_session_in(&empty, "zzzz", cwd).unwrap_err();
        assert!(error.contains("loongagent sessions"), "{error}");
    }

    #[test]
    fn a_session_from_another_project_can_still_be_reopened() {
        let root = temp_root("elsewhere");
        let here = Path::new("/tmp/here");
        let there = Path::new("/tmp/there");
        write_session(
            &session_dir_in(&root, there).join("cccc3333.jsonl"),
            &["from there"],
        );
        assert_eq!(
            session_summaries_in(&session_dir_in(&root, here), "here").len(),
            0
        );
        assert_eq!(
            resolve_session_in(&root, "cccc3333", here)
                .unwrap()
                .file_name()
                .unwrap(),
            "cccc3333.jsonl",
            "`sessions --all` shows it, so `resume` must be able to open it"
        );
        assert_eq!(all_session_summaries_in(&root).len(), 1);
    }

    #[test]
    fn an_id_that_exists_in_two_other_projects_is_not_guessed() {
        // Every legacy session is called `default`, so this is the common case, not a
        // corner: silently opening one of them would open the wrong conversation.
        let root = temp_root("ambiguous-elsewhere");
        let here = Path::new("/tmp/here");
        for (there, prompt) in [("/tmp/one", "first"), ("/tmp/two", "second")] {
            write_session(
                &session_dir_in(&root, Path::new(there)).join("session.jsonl"),
                &[prompt],
            );
        }
        let error = resolve_session_in(&root, "default", here).unwrap_err();
        assert!(error.contains("other projects"), "{error}");
        assert!(
            error.contains("full path"),
            "and says what to do instead: {error}"
        );

        // With only one of them, it resolves — the useful half of the same rule.
        std::fs::remove_file(session_dir_in(&root, Path::new("/tmp/two")).join("session.jsonl"))
            .unwrap();
        assert_eq!(
            resolve_session_in(&root, "default", here)
                .unwrap()
                .file_name()
                .unwrap(),
            "session.jsonl"
        );
    }

    #[test]
    fn a_long_prompt_is_clipped_without_breaking_a_character() {
        assert_eq!(clip("short", 10), "short");
        assert_eq!(clip("abcdefghij", 5), "abcd…");
        let clipped = clip("中文中文中文", 4);
        assert_eq!(clipped.chars().count(), 4);
        assert!(clipped.ends_with('…'));
    }

    #[test]
    fn ages_are_said_the_way_a_person_says_them() {
        let now = SystemTime::now();
        assert_eq!(age_text(now), "just now");
        assert_eq!(
            age_text(now - std::time::Duration::from_secs(120)),
            "2 min ago"
        );
        assert_eq!(
            age_text(now - std::time::Duration::from_secs(7200)),
            "2 h ago"
        );
        assert_eq!(
            age_text(now - std::time::Duration::from_secs(172800)),
            "2 d ago"
        );
    }
}
