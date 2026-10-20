//! Purpose: The `loongagent` binary: parse the command line, resolve the one session and the one configuration, and dispatch.
//!
//! # Contract
//! - **Serves:** every command the binary offers; this file is the composition root that turns flags into one configured server (ADR-0009 §2).
//! - **Design:** `docs/design/surfaces.md` §2; `docs/design/sessions-lineage.md` §5; `docs/design/control-plane.md` §1.
//! - **Upstream:** `std::env::args`; the command modules are its callees and its submodules.
//! - **Downstream:** every command module (`local`, `tui`, `server`, `session`, `control`, `config_cmd`, `context`, `plugin`, `hooks`, `mcp`, `sandbox`, `repl`, `web`, `headless`) and `loong_config` for the layer choice.
//! - **Verify:** `cargo test -p loongagent` · `./scripts/check.sh`
//!
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

use loong_kernel::{steps_or_unlimited, Kernel, KernelConfig};
use loong_providers::{
    ChatMessage, CompletionRequest, CompletionResponse, Model, ModelError, OpenAiCompat,
    ScriptedModel, ToolCallRequest, Usage,
};
use loong_schema::generated as gen;
use loong_schema::generated::ApprovalScope;
use loong_server::Server;
use loong_tui::{Client, Transport};
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::Receiver;
use std::time::{SystemTime, UNIX_EPOCH};

mod config_cmd;
mod context;
mod control;
mod headless;
mod hooks;
mod local;
mod mcp;
mod plugin;
mod repl;
mod sandbox;
mod server;
mod session;
mod tui;
mod web;

use crate::config_cmd::{config_explain, config_show, load_env_files};
use crate::context::{context_rebuild, context_status, context_verify};
use crate::control::{control, first_line, profiles, setup, sync_pack, use_profile};
use crate::headless::headless;
use crate::hooks::{hooks_list, hooks_trust};
use crate::local::local;
use crate::mcp::{mcp_list, mcp_trust};
use crate::plugin::{plugin_add, plugin_list, plugin_remove, plugin_update, skills_list};
use crate::repl::repl;
use crate::sandbox::sandbox_self_test;
use crate::server::serve;
use crate::session::{
    default_session_path, delete, fork, list_sessions, new_session_id, newest_session,
    resolve_session, resume, session_id_of, session_path_for, SessionSource,
};
use crate::tui::full_screen;
use crate::web::web;

const USAGE: &str = "\
loongagent — a firm-internal coding agent

USAGE:
  loongagent server --stdio [--session PATH] [--cwd DIR] [--model NAME]
  loongagent local          [--cwd DIR]                                    what this install runs
  loongagent local add      [PROVIDER]                                     add models (a wizard)
  loongagent local delete   <slot|model|provider>                          remove them
  loongagent local edit                                                    open the file
  loongagent local semantic [status]                                       the layered memory (ADR-0017)
  loongagent local semantic on [ollama/MODEL] [--base-url URL]             turn it on, pick the summariser
  loongagent local semantic on --record                                    deterministic layers only
  loongagent local semantic off                                            turn it off
  loongagent setup          [--profile PATH]                                write a profile
  loongagent tui            [--session ID|PATH] [--cwd DIR] [--new] [--local]  full-screen
      with no --session it continues this project's newest session; --new starts another
      --local runs with no control plane at all: no pack, no sync, no setup form
      in a session:  alt+1..9 choose a model (or /1../9); alt+r asks the last
                     prompt again of the model chosen now (or /r)
  loongagent resume [ID]    [--cwd DIR]                                    reopen one (default: latest)
  loongagent sessions       [--all] [--limit N] [--cwd DIR]                list them
  loongagent fork [ID]      [--at SEQ] [--cwd DIR]                         a child session
  loongagent delete ID      [--force] [--cwd DIR]                          its whole branch
  loongagent repl           [--session PATH] [--cwd DIR] [--model NAME]   line mode (any pipe)
  loongagent web            [--session PATH] [--cwd DIR] [--model NAME] [--port N] [--bind IP]
  loongagent run \"PROMPT\"   [--session PATH] [--cwd DIR] [--model NAME] [--budget N] [--keep-newest N]
                            [--max-steps N]   a step is one model call; 0 means no limit
  --debug-model             append every raw model exchange to <session>.models.jsonl:
                            request, finish_reason, content, tool_calls (why a call did
                            not run is then a grep, not a guess)
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
  loongagent plugin add <spec> [--name N] [--ref BRANCH] [--project|--user] [--skills DIR]
                               [--bootstrap SKILL] [--force]
                               install a plugin's skills: a known name (`superpowers`),
                               `owner/repo`, a git URL, or a directory holding skills
  loongagent plugin list       [--cwd DIR]   what is installed, and what it adds to context
  loongagent plugin update     <name> [--project|--user]
                                              fetch the plugin's latest commit
  loongagent plugin remove     <name> [--project|--user]
                                              uninstall it (deletes the checkout)
                                              the flag names the copy when the same plugin
                                              is installed both for you and for this project
  loongagent hooks list        [--cwd DIR]   show the effective hook set and its trust
  loongagent hooks trust       [--cwd DIR]   trust the current hook bytes (per repository)
  loongagent mcp list          [--cwd DIR]   show the configured MCP servers and their trust
  loongagent mcp trust         [--cwd DIR]   trust the current mcp.toml (per repository)
";

/// Purpose: Run the command and turn one error into an exit code and one stderr line.
/// Contract: `main.rs` entry point; nothing calls it.
fn main() {
    if let Err(err) = run() {
        eprintln!("loongagent: {err}");
        std::process::exit(1);
    }
}

/// Purpose: Everything the command line said, before any command resolves it.
/// Serves: every command module, which receives one.
/// Design: `docs/design/surfaces.md` §2.
/// Up: `Args::parse` builds it; `run` hands it to the command modules.
/// Down: no further call; it is data.
/// Verify: `cargo test -p loongagent`.
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
    /// How this run's session was chosen, so the interface can say so instead of leaving
    /// someone to wonder whether they are continuing yesterday's conversation.
    session_source: SessionSource,
    cwd: PathBuf,
    /// `None` means "not given": the layered configuration decides (ADR-0008).
    /// Flags are the **session layer**, the most local one.
    model: Option<String>,
    /// A profile file used as the session layer.
    profile: Option<PathBuf>,
    /// `--local`: run with no control plane at all — no pack, no sync, no setup form.
    ///
    /// A session layer like `--model`: this run's answer to a question the profile
    /// also answers (`mode = "local"`). Either one turns it on.
    local: bool,
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
    /// Model calls one turn may take; 0 means no limit.
    max_steps: Option<usize>,
    /// `local semantic --base-url URL`: where the *summariser* lives. Separate from a
    /// model's `base_url` because it is a different endpoint and a different protocol
    /// (Ollama's native `/api/chat`, not the OpenAI shim).
    summarizer_base_url: Option<String>,
    /// `local semantic on --record`: deterministic layers only, no model at all.
    record: bool,
    /// `--debug-model`: append every raw model exchange to a sidecar next to the
    /// session log, so "did the server send `tool_calls` or prose?" is checkable.
    debug_model: bool,
    /// `plugin add --name N`: the name to install under, when it is not the one the
    /// source implies.
    name: Option<String>,
    /// `plugin add --ref R`: the branch or tag to fetch.
    git_ref: Option<String>,
    /// `plugin add --project`: install for this workspace only, instead of for the
    /// whole user (`~/.loongagent/plugins`).
    project: bool,
    /// `plugin update|remove --user`: the copy that follows this person, when the same
    /// name is installed in both scopes and the bare name would be ambiguous.
    user: bool,
    /// `plugin add --skills DIR`: where the skills live inside the checkout, when
    /// detection cannot tell.
    skills: Option<String>,
    /// `plugin add --bootstrap SKILL`: a skill whose body belongs in the system prompt
    /// of every session. Repeatable.
    bootstrap: Vec<String>,
    /// `plugin add --force`: replace an existing install of the same name.
    force: bool,
    positional: Vec<String>,
}

impl Args {
    /// Purpose: Parse the flags and positionals, load the env files first, and settle the session once the whole line is known.
    /// Serves: every command; `run` parses before dispatch.
    /// Design: `docs/design/surfaces.md` §2; `docs/design/config-routing.md` §5.
    /// Up: `run` and every command module that takes an `Args`; `resume` re-enters through it.
    /// Down: `load_env_files`, `config_dir` and `finish_session`.
    /// Verify: `cargo test -p loongagent`.
    fn parse(rest: &[String]) -> Result<Self, String> {
        let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
        let mut args = Args {
            session: None,
            session_spec: None,
            new_session: false,
            session_source: SessionSource::Created,
            cwd: cwd.clone(),
            model: None,
            profile: None,
            local: false,
            bind: "127.0.0.1".to_string(),
            control_url: None,
            control_key: None,
            control_token: None,
            control_system: false,
            port: 8737,
            stdio: false,
            budget: None,
            keep_newest: None,
            max_steps: None,
            summarizer_base_url: None,
            record: false,
            debug_model: false,
            name: None,
            git_ref: None,
            project: false,
            user: false,
            skills: None,
            bootstrap: Vec::new(),
            force: false,
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
                "--local" => args.local = true,
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
                // `--max-steps 0` is "no limit", the same shape as `--budget 0`: a step is
                // a model call, so this is the knob a long task turns, not a number anyone
                // should have to discover by hitting it.
                "--max-steps" => {
                    i += 1;
                    args.max_steps = Some(
                        rest.get(i)
                            .ok_or("--max-steps needs a count")?
                            .parse()
                            .map_err(|_| "--max-steps must be a number".to_string())?,
                    );
                }
                // `local semantic`: the summariser's own endpoint and the
                // deterministic-only switch. `--base-url` is not the model's: the
                // summariser speaks Ollama's native API, which has no `/v1`.
                "--base-url" => {
                    i += 1;
                    args.summarizer_base_url =
                        Some(rest.get(i).ok_or("--base-url needs an address")?.clone());
                }
                "--record" => args.record = true,
                "--debug-model" => args.debug_model = true,
                // `plugin add`: the name to install under, the branch or tag, the
                // scope, where the skills live, the skills to seat at session start,
                // and whether to replace what is there.
                "--name" => {
                    i += 1;
                    args.name = Some(rest.get(i).ok_or("--name needs a name")?.clone());
                }
                "--ref" => {
                    i += 1;
                    args.git_ref = Some(rest.get(i).ok_or("--ref needs a branch or tag")?.clone());
                }
                "--project" => args.project = true,
                "--user" => args.user = true,
                "--skills" => {
                    i += 1;
                    args.skills = Some(
                        rest.get(i)
                            .ok_or("--skills needs a path inside the checkout")?
                            .clone(),
                    );
                }
                "--bootstrap" => {
                    i += 1;
                    args.bootstrap
                        .push(rest.get(i).ok_or("--bootstrap needs a skill name")?.clone());
                }
                "--force" => args.force = true,
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

    /// Purpose: Work out which session this run is about, once, so every command and the interface agree.
    /// Serves: `Args::parse`, and so every command.
    /// Design: `docs/design/sessions-lineage.md` §5.
    /// Up: `Args::parse`; no other caller.
    /// Down: `new_session_id`, `session_path_for`, `resolve_session` and `newest_session`.
    /// Verify: `cargo test -p loongagent`.
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
            self.session_source = SessionSource::Created;
            Some(session_path_for(&id, &self.cwd))
        } else if let Some(spec) = self.session_spec.clone() {
            self.session_source = SessionSource::Named;
            Some(resolve_session(&spec, &self.cwd).map_err(|e| format!("--session {spec}: {e}"))?)
        } else if let Some(newest) = crate::newest_session(&self.cwd) {
            // The default, and it is worth saying out loud: this continues the project's
            // most recent session rather than starting one.
            self.session_source = SessionSource::Continued;
            Some(newest)
        } else {
            let id = new_session_id(&self.cwd);
            self.session_source = SessionSource::Created;
            Some(session_path_for(&id, &self.cwd))
        };
        Ok(())
    }

    /// Purpose: The one place local and managed runs diverge: which layer list this run reads.
    /// Serves: `is_local` and `settings_with_pack`.
    /// Design: `docs/design/config-routing.md` §1 and §3.
    /// Up: `is_local` and `settings_with_pack`; no other caller.
    /// Down: `loong_config::{local_layer_paths, layer_paths}`.
    /// Verify: `cargo test -p loongagent --test local_mode`.
    /// The layered configuration for this run (ADR-0008): built-in defaults, then
    /// whatever the org / project / user files say, then `--profile` as the session
    /// layer. Flags are applied on top of the resolved values, and only ever
    /// narrow (the kernel enforces the narrow-only arithmetic).
    /// Resolve the configuration with the control plane's pack in place, if there is
    /// one. The pack is the *verified* layer: nothing reaches the resolver that a user
    /// could have written by hand (ADR-0008 §6).
    /// Which layer list a run reads. **The one place the two modes diverge**, because
    /// everything downstream is resolution: a local run reads `local.toml` and never the
    /// managed profile or the org file, and a managed run reads the other set.
    ///
    /// Keeping this as one function is what makes "local mode is independent" checkable
    /// rather than a property that has to hold in several places at once.
    fn layers(&self, local: bool) -> Vec<(loong_config::Layer, PathBuf)> {
        if local {
            loong_config::local_layer_paths(&self.cwd, self.profile.as_deref())
        } else {
            loong_config::layer_paths(&self.cwd, self.profile.as_deref())
        }
    }

    /// Purpose: Whether this run is local, answered from the flag or from `local.toml` and nothing else.
    /// Serves: `settings_with_pack` and `tui.rs`'s `full_screen`, both before any control-plane work.
    /// Design: `docs/design/config-routing.md` §3.
    /// Up: `settings_with_pack` and `tui.rs`'s `full_screen`; no other caller.
    /// Down: `layers` and `loong_config::resolve`; no network, no pack.
    /// Verify: `cargo test -p loongagent --test local_mode`.
    /// Whether this run is local: the flag, or `local.toml`'s own answer.
    ///
    /// Reads the layer files and nothing else — no pack, no network — so it is safe
    /// to ask *before* anything control-plane-shaped has happened. That ordering is
    /// the whole point: a question asked after the sync is a question already
    /// answered wrongly.
    fn is_local(&self) -> Result<bool, String> {
        if self.local {
            return Ok(true);
        }
        // The *local* layers, because only they can say "local" — asking the managed
        // profile would be asking a document that is not allowed to answer.
        let resolved =
            loong_config::resolve(&self.layers(true), &self.cwd).map_err(|e| e.to_string())?;
        Ok(resolved.is_local())
    }

    /// Purpose: Resolve the layered configuration with the verified pack in place, or with no control plane at all in local mode.
    /// Serves: every command that reads settings, and `serve`/`full_screen`.
    /// Design: `docs/design/config-routing.md` §1–§4; `docs/design/control-plane.md` §4.
    /// Up: `serve`, `full_screen`, `web`, `local`, `config_show`, `config_explain`, `skills_list`, `plugin_*` and `session_needs_a_model` callers.
    /// Down: `is_local`, `layers`, `loong_config::resolve`, `resolve_with`, `sync_pack` and `PackState`.
    /// Verify: `cargo test -p loongagent --test local_mode`.
    fn settings_with_pack(&self) -> Result<(loong_config::Profile, Option<PackState>), String> {
        // Settled **before** anything control-plane-shaped can happen. This function is
        // the whole severance: `full_screen` and `serve` both get their configuration
        // through it, so nothing downstream has to remember the rule — and there is no
        // second path that could forget it.
        if self.is_local()? {
            let mut resolved =
                loong_config::resolve(&self.layers(true), &self.cwd).map_err(|e| e.to_string())?;
            // The flag is this run's session layer, so the resolved profile says out
            // loud what the run already decided. Set after resolution on purpose: it is
            // a fact about *this run*, not a line in anybody's file — which is exactly
            // the mistake the `mode` guard now prevents.
            if self.local {
                resolved.mode = Some("local".to_string());
            }
            return Ok((resolved, None));
        }

        let layers = self.layers(false);
        // Resolve once **without** the pack before reaching for the network. Two
        // reasons, and the second is the one that bit: it is a handful of file reads, so
        // a broken profile is reported as a broken profile — and a profile that is going
        // to be refused is refused before a request is made about it. Sync-first meant a
        // poisoned `profile.toml` surfaced as "the control plane is unreachable", which
        // sent the reader to the wrong machine entirely.
        loong_config::resolve(&layers, &self.cwd).map_err(|e| e.to_string())?;
        let pack = sync_pack(self)?;
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

    /// Purpose: The session file this run will use.
    /// Contract: `main.rs` accessor; every command module reads it through `Args`.
    /// The session file: explicit if asked, otherwise the per-project default.
    fn session_path(&self) -> PathBuf {
        self.session
            .clone()
            .unwrap_or_else(|| default_session_path(&self.cwd))
    }

    /// Purpose: Where a relative credential path is looked up: beside the profile in use.
    /// Contract: `main.rs` accessor; `server.rs`'s `build_model` and `build_models` are the callers.
    /// Where a relative credential path is looked up: beside the profile in use.
    fn profile_base(&self) -> PathBuf {
        self.profile
            .as_ref()
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .unwrap_or_else(config_dir)
    }

    /// Purpose: The id of the session file this run will use.
    /// Contract: `main.rs` accessor; `tui.rs`, `repl.rs`, `headless.rs` and `server.rs` are the callers.
    fn session_id(&self) -> String {
        session_id_of(&self.session_path())
    }

    /// Purpose: Create the session directory before anything writes the log into it.
    /// Contract: `main.rs` accessor; `serve`, `full_screen`, `repl`, `headless` and `web` call it.
    fn ensure_parent(&self) -> Result<(), String> {
        if let Some(dir) = self.session_path().parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        Ok(())
    }
}

/// Purpose: The machine's control-plane configuration: address, pinned key and a token reference.
/// Serves: `control.rs`, which merges and validates it, and `tui.rs`'s first-run form.
/// Design: `docs/design/control-plane.md` §1 and §4.
/// Up: `control.rs` reads and writes it; `tui.rs`'s `full_screen` builds one from the form.
/// Down: no further call; it is data.
/// Verify: `cargo test -p loongagent`.
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

/// Purpose: What a session needs to know about the verified pack it is about to run on, and where it came from.
/// Serves: `control.rs`'s `sync_pack`/`cached_pack_state` and every command that reports the pack.
/// Design: `docs/design/control-plane.md` §3 and §4.
/// Up: `control.rs` builds it; `serve`, `full_screen`, `web` and the config commands read it.
/// Down: `summary` and the `loong_config` types it carries.
/// Verify: `cargo test -p loongagent`.
/// What a session needs to know about the pack it is about to run on.
struct PackState {
    provenance: loong_config::ControlProvenance,
    profile: loong_config::NamedProfile,
    layer: Vec<(String, loong_config::Value)>,
    from_cache: bool,
    note: Option<String>,
}

impl PackState {
    /// Purpose: The one-line pack summary the terminal and the TUI intro both print.
    /// Contract: `main.rs` `PackState`; `control.rs`, `tui.rs` and `web.rs` call it.
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

/// Purpose: The client's one root, `$LOONGAGENT_HOME` or `~/.loongagent`.
/// Contract: `main.rs` path root; `Args::parse`, `profile_base`, `control.rs`, `local.rs`'s `configure_local` and `session.rs` call it.
/// The client's one root: `$LOONGAGENT_HOME`, else `~/.loongagent` (defined in the
/// config crate, because the user-layer path comes from the same rule).
fn config_dir() -> PathBuf {
    loong_config::loongagent_home()
}

/// Purpose: Dispatch the first word to its command module, or print the usage and the version.
/// Serves: `main`, and every command the binary offers.
/// Design: `docs/design/surfaces.md` §2 (the command table).
/// Up: `main`; no other caller.
/// Down: every command module's entry function, `Args::parse` and `USAGE`.
/// Verify: `cargo test -p loongagent --test local_mode`.
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
        "plugin" => match rest.first().map(String::as_str) {
            Some("add") => plugin_add(Args::parse(&rest[1..])?),
            Some("list") => plugin_list(Args::parse(&rest[1..])?),
            Some("update") => plugin_update(Args::parse(&rest[1..])?),
            Some("remove") => plugin_remove(Args::parse(&rest[1..])?),
            _ => Err(format!(
                "plugin add <name|owner/repo|URL|DIR> | plugin list | plugin update <name> | \
                 plugin remove <name>\n\n{USAGE}"
            )),
        },
        "context" => match rest.first().map(String::as_str) {
            Some("status") => context_status(Args::parse(&rest[1..])?),
            Some("verify") => context_verify(Args::parse(&rest[1..])?),
            Some("rebuild") => context_rebuild(Args::parse(&rest[1..])?),
            _ => Err(format!(
                "context status | context verify | context rebuild\n\n{USAGE}"
            )),
        },
        "setup" => setup(Args::parse(rest)?),
        "local" => local(Args::parse(rest)?),
        "control" => control(rest),
        "profiles" => profiles(Args::parse(rest)?),
        "use" => use_profile(Args::parse(rest)?, rest),
        "tui" => full_screen(Args::parse(rest)?),
        "resume" => resume(rest),
        "fork" => fork(rest),
        "delete" => delete(rest),
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
