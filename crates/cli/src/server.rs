//! Purpose: The `server --stdio` command: resolve the layers, build the kernel and run the request loop.
//!
//! # Contract
//! - **Serves:** `loongagent server`, and every surface that spawns it as a child.
//! - **Design:** `docs/design/surfaces.md` §1 and §2.
//! - **Upstream:** `main.rs`'s `server` arm; `tui.rs`'s `ChildTransport` starts it as `server --stdio`.
//! - **Downstream:** `loong_server::Server`, `loong_kernel` for the turn and the log, `loong_hooks`/`loong_mcp`/`loong_skills`/`loong_plugin` for what joins a session, and `loong_sandbox` for the boundary check.
//! - **Verify:** `cargo test -p loongagent --test vertical_slice` · `./scripts/check.sh`

use super::*;

use crate::config_cmd::resolve_reference;
use crate::plugin::{installed_plugins, load_catalogue};
use crate::session::session_dir;

// ---------------------------------------------------------------- the server

/// Purpose: Build the configured kernel, record the pack provenance, and run the stdio loop with a factory for new sessions.
/// Serves: `loongagent server --stdio`, the one process that owns a running session.
/// Design: `docs/design/surfaces.md` §1 and §2.
/// Up: `main.rs`'s `server` arm; no other caller.
/// Down: `Args::settings_with_pack`, `installed_plugins`, `load_catalogue`, `build_models`, `build_kernel`, `Kernel::{record_control_synced, record_profile_selected}` and `Server::run_stdio`.
/// Verify: `cargo test -p loongagent --test vertical_slice`.
pub(crate) fn serve(args: Args) -> Result<(), String> {
    args.ensure_parent()?;
    // The control descriptor must be checked before anything opens a file. If the number
    // is not an open descriptor, the session log can be handed that very number a moment
    // later, and the reader thread would then adopt (and close) the log instead of a
    // channel. A surface always passes a real one; a person running the server by hand
    // can still mistype it.
    #[cfg(unix)]
    if let Some(fd) = args.control_fd {
        // Safety: `F_GETFD` only reads a flag; it has no memory contract.
        if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
            return Err(format!("--control-fd {fd}: not an open descriptor"));
        }
    }
    // `--debug-model`: point the provider's raw-exchange dump at a sidecar beside this
    // session's log. Set before any model is built, and only when the environment does
    // not already name a file — an explicit `LOONGAGENT_DEBUG_MODEL` wins, so a person
    // can send several sessions to one place without a flag.
    if args.debug_model && std::env::var_os("LOONGAGENT_DEBUG_MODEL").is_none() {
        let path = args.session_path().with_extension("models.jsonl");
        eprintln!("[debug] raw model exchanges → {}", path.display());
        std::env::set_var("LOONGAGENT_DEBUG_MODEL", path);
    }
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
    // Installed plugins are extra skill roots with a name and a provenance (ADR-0018),
    // and the catalogue below is loaded through the same function a session uses, so
    // "what this install has" cannot disagree with "what this session gets".
    let plugins = installed_plugins(&args.cwd)?;
    if !plugins.is_empty() {
        let described: Vec<String> = plugins
            .plugins
            .iter()
            .map(|p| {
                let bootstrap = match p.manifest.bootstrap.is_empty() {
                    true => String::new(),
                    false => format!(" [always on: {}]", p.manifest.bootstrap.join(", ")),
                };
                format!(
                    "{} {} ({}){}",
                    p.name(),
                    p.manifest.version.as_deref().unwrap_or("-"),
                    p.source_label(),
                    bootstrap
                )
            })
            .collect();
        eprintln!("[plugin] {}", described.join("; "));
        if !plugins.shadowed.is_empty() {
            eprintln!(
                "[plugin] shadowed by a project install of the same name: {}",
                plugins.shadowed.join(", ")
            );
        }
    }
    match load_catalogue(&args.cwd, &settings, &plugins) {
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
    // Nothing bounds the context when no budget is set anywhere and no model declares a
    // window: `compact_if_needed` returns immediately, and a long session ends at the
    // provider's context limit with nothing said beforehand. Say it now.
    let bounded = loong_kernel::effective_budget(args.budget, &settings).is_some()
        || models
            .iter()
            .any(|(_, model)| model.context_window().is_some());
    if !bounded {
        eprintln!(
            "[context] no budget_tokens and no max_context_size anywhere — compaction is \
             off, and a long session will end at the provider's context limit. Set one of \
             them (or --budget N) to have context cleared instead."
        );
    }
    let session_path = args.session_path();
    let mut kernel = build_kernel(&args, &settings, &session_path)?;
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
    // `session.new` needs a kernel for a session that does not exist yet, and only this
    // function knows how to make one: the same args, the same resolved layers, a fresh id
    // and a fresh log beside the current one.
    let factory_args = args.clone();
    let factory_settings = settings.clone();
    let factory: loong_server::KernelFactory = Box::new(move || {
        let id = new_session_id(&factory_args.cwd);
        let path = session_dir(&factory_args.cwd).join(format!("{id}.jsonl"));
        build_kernel(&factory_args, &factory_settings, &path).map(|kernel| (id, kernel))
    });
    // The surface that spawned us may have opened a private control channel, which is how
    // `/yolo` reaches a turn that owns the connection. The slot is created here because
    // the kernel is built here; the server re-installs it on `session.new`.
    #[cfg(unix)]
    let permissions = args
        .control_fd
        .map(|_| std::sync::Arc::new(std::sync::Mutex::new(None)));
    #[cfg(unix)]
    if let Some(slot) = &permissions {
        kernel.use_permissions_requester(std::sync::Arc::clone(slot));
    }
    let server = Server::new(kernel, args.session_id()).with_factory(factory);
    #[cfg(unix)]
    let server = match (&permissions, args.control_fd) {
        (Some(slot), Some(fd)) => server.with_permissions(std::sync::Arc::clone(slot), fd),
        _ => server,
    };
    server.run_stdio().map_err(|e| e.to_string())
}

/// Purpose: Turn the flags and the resolved profile into the one kernel configuration every session build uses.
/// Serves: `build_kernel`, including a later `session.new` built through the factory.
/// Design: `docs/design/surfaces.md` §2; `docs/design/config-routing.md` §2.
/// Up: `build_kernel`; no other caller.
/// Down: `KernelConfig::new`, `steps_or_unlimited` and `context_engine_config`.
/// Verify: `cargo test -p loongagent --test vertical_slice`.
/// The kernel settings this command line and configuration describe.
///
/// Out here rather than inline so that a *second* session built later gets the same ones.
/// A new session that quietly ran under a different policy from the one on screen would be
/// a worse bug than having no new session at all.
fn kernel_config(args: &Args, settings: &loong_config::Profile, log: &Path) -> KernelConfig {
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
        config.max_steps = steps_or_unlimited(steps);
    }
    // The flag is the caller's own request, and it outranks the resolved layers here —
    // then the kernel takes the strictest of the two, so a ceiling in a layer still holds.
    if let Some(steps) = args.max_steps {
        config.max_steps = steps_or_unlimited(steps);
    }
    config.profile = settings
        .explain("profile")
        .map(|row| row.value.render())
        .unwrap_or_else(|| "default".to_string());
    config.settings = settings.clone();
    config.context_engine = context_engine_config(settings, log);
    config
}

/// Purpose: Build the layered-memory configuration when the profile asks for it, and `None` otherwise.
/// Serves: `kernel_config`; the default is the rollback path with no key set.
/// Design: `docs/design/context-ledger.md` §4 and §7.
/// Up: `kernel_config`; no other caller.
/// Down: `Profile::context_engine`, `ContextEngineConfig::new` and the summariser defaults.
/// Verify: `cargo test -p loongagent`.
/// The layered memory, when the resolved configuration asks for it (ADR-0017).
///
/// `None` is the default and the rollback path: no key was set, so the session
/// behaves exactly as it did before the feature existed. The sidecar database lives
/// next to the session's own log, so deleting a session deletes its derived memory
/// with it and nothing else has to be cleaned up.
fn context_engine_config(
    settings: &loong_config::Profile,
    log: &Path,
) -> Option<loong_kernel::ContextEngineConfig> {
    let engine = settings.context_engine()?;
    let mut config =
        loong_kernel::ContextEngineConfig::new(log.with_extension("context.db"), log.to_path_buf());
    config.worker.max_events = 256;
    // `record` mode is deterministic-only: a complete memory with no model and no
    // network. `semantic` adds the local summariser if policy allowed one.
    if engine.mode == "semantic" && engine.summarizer != "off" {
        let base_url = engine
            .summarizer_base_url
            .clone()
            .unwrap_or_else(|| DEFAULT_SUMMARIZER_URL.to_string());
        let model = engine
            .summarizer_model
            .clone()
            .unwrap_or_else(|| DEFAULT_SUMMARIZER_MODEL.to_string());
        config.summarizer = Some(loong_kernel::context_engine::SummarizerChoice {
            kind: engine.summarizer.clone(),
            base_url,
            model,
            // The idle window's budget (ADR-0017 §4.6). This used to be the number
            // of calls a step boundary made, which put the model on the foreground's
            // critical path; now it is what one background window may spend before
            // sleeping. The resolved configuration wins, and its default is the
            // design note's.
            window_calls: engine
                .summarizer_window_calls
                .unwrap_or(loong_kernel::context_engine::DEFAULT_WINDOW_CALLS),
            window_tokens: engine
                .summarizer_window_tokens
                .unwrap_or(loong_kernel::context_engine::DEFAULT_WINDOW_TOKENS),
        });
    }
    Some(config)
}

/// Purpose: Whether an endpoint is remote enough to need a key; loopback does not.
/// Serves: `build_models`, which warns about a missing key only where a 401 is real.
/// Design: `docs/design/config-routing.md` §8.
/// Up: `build_models`; the module tests call it directly.
/// Down: the small URL split in this function; no further call.
/// Verify: `cargo test -p loongagent` (the `endpoint_key_tests` module).
/// Whether an endpoint needs a credential.
///
/// A loopback address does not: the server is on this machine and has nobody to
/// authenticate against. Asking for a key there is a false alarm, and a warning
/// that fires on a correct configuration trains people to ignore warnings.
fn endpoint_authenticates(base_url: &str) -> bool {
    // Enough of a URL parse for this question: the host is what follows the scheme
    // and precedes the first `/`, `:` or `?`. Deliberately not a dependency on the
    // HTTP crate just for a warning.
    let rest = base_url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(base_url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = if let Some(rest) = authority.strip_prefix('[') {
        // An IPv6 literal: `[::1]:11434`.
        rest.split(']').next().unwrap_or(rest).to_string()
    } else {
        authority.split(':').next().unwrap_or(authority).to_string()
    };
    let host = host.to_ascii_lowercase();
    !(host == "localhost" || host == "::1" || host.starts_with("127.") || host == "0.0.0.0")
    // `host.docker.internal` and friends are remote from the agent's point of view,
    // so only genuine loopback is exempt.
}

/// Where the local summariser lives when nothing says otherwise. The owner's
/// machine runs Ollama here.
pub(crate) const DEFAULT_SUMMARIZER_URL: &str = "http://127.0.0.1:11434";

/// The model the acceptance gate picked.
///
/// Measured on 2026-10-14 (`cargo test -p loong-context --test faithfulness --
/// --ignored`), on the ten-case gate: `qwen2.5:7b-instruct` explained the adopted
/// approach correctly on 7/7 cases with a cause, invented nothing and missed
/// nothing. `qwen2.5:3b` invented rejected approaches; `qwen3:0.6b` invented
/// nothing but could not explain either.
///
/// `rejected` is *not* part of what this model is asked for — it is computed from
/// the attempt sequence (`Store::digest`), because asking a model to copy a list it
/// can read was measured to fail both ways.
pub(crate) const DEFAULT_SUMMARIZER_MODEL: &str = "qwen2.5:7b-instruct";

/// Purpose: Open the log at a path or create it, with the models, catalogue and memory a session needs.
/// Serves: `serve` for the first session and the `KernelFactory` for every later one.
/// Design: `docs/design/surfaces.md` §1 and §2.
/// Up: `serve` and the factory closure it installs; no other caller.
/// Down: `build_models`, `installed_plugins`, `kernel_config`, `Kernel::{open_with_models, create_with_models}` and `Plugin` bootstrap accessors.
/// Verify: `cargo test -p loongagent --test vertical_slice`.
/// Open the log at `path` if it is there, and start one if it is not.
///
/// A session **is** its file, so this is also how a session comes into being.
fn build_kernel(
    args: &Args,
    settings: &loong_config::Profile,
    path: &Path,
) -> Result<Kernel, String> {
    // The directory is the session's too. `serve` makes its own, but a session started
    // later gets a path nobody has created yet, and creating the log before its folder is
    // how `session.new` failed the first time it was tried.
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let models = build_models(args, settings)?;
    // The installed plugins join the catalogue the same way a profile's `skill_dirs`
    // does, and a plugin that asked for one seats its body in the system prompt
    // (ADR-0018). Both are wired here, so the live kernel and a later `session.new`
    // are built by one code path and cannot disagree.
    let plugins = installed_plugins(&args.cwd)?;
    let mut config = kernel_config(args, settings, path);
    config.settings.skill_dirs.extend(plugins.skill_dirs());
    config.bootstrap_skills = plugins.bootstrap_skills();
    config.bootstrap_notes = plugins.bootstrap_notes();
    if path.exists() {
        Kernel::open_with_models(path, config, models)
    } else {
        Kernel::create_with_models(path, config, models)
    }
    .map_err(|e| e.to_string())
}

/// Purpose: Build every model the profile offers, or the one adapter a `--model` flag names.
/// Serves: `serve` and `build_kernel`.
/// Design: `docs/design/config-routing.md` §8.
/// Up: `serve` and `build_kernel`; no other caller.
/// Down: `build_model`, `resolve_reference`, `endpoint_authenticates` and `OpenAiCompat::new`.
/// Verify: `cargo test -p loongagent --test vertical_slice`.
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
                // Only warn when a key is actually needed. A loopback endpoint does
                // not authenticate — an Ollama or llama.cpp server on this machine
                // answers a keyless request with 200, measured — so the old
                // unconditional warning cried wolf on every local setup and sent
                // people looking for a key that does not exist.
                if key.is_none() && endpoint_authenticates(&base_url) {
                    eprintln!(
                        "[config] model {:?} has no usable api_key — the provider will answer 401",
                        spec.name
                    );
                }
                let mut adapter = OpenAiCompat::new(base_url, spec.model.clone(), key);
                if let Some(tokens) = spec.max_context_size {
                    adapter = adapter.with_context_window(tokens);
                }
                Box::new(adapter) as Box<dyn Model>
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

/// Purpose: Build one named adapter: a provider double, the real OpenAI-compatible client, or the unconfigured stand-in.
/// Serves: `build_models` for the `--model` case.
/// Design: `docs/design/config-routing.md` §8.
/// Up: `build_models`; no other caller.
/// Down: `resolve_reference`, `OpenAiCompat::new`, `ScriptedModel::new` and the `echo`/`unconfigured` stand-ins.
/// Verify: `cargo test -p loongagent --test vertical_slice`.
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
        "demo-escape" => {
            // The demo has to name something genuinely *outside* the workspace on every
            // platform. A hard-coded `/tmp/...` is not absolute on Windows — it resolves
            // against the current drive and lands inside the workspace — so the demo would
            // demonstrate nothing there.
            let outside = std::env::temp_dir().join("loongagent-demo-escape.txt");
            Ok(Box::new(ScriptedModel::new(vec![
                CompletionResponse {
                    text: None,
                    tool_calls: vec![ToolCallRequest {
                        call_id: "c1".to_string(),
                        tool: "write_file".to_string(),
                        args: json!({"path": outside, "content": "should never be written\n"}),
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
            ])))
        }
        other => Err(format!(
            "unknown --model {other:?} (echo | demo-write | demo-escape | openai-compat)"
        )),
    }
}

/// Purpose: The model a first run gets when nothing is configured: every call answers with the command that fixes it.
/// Serves: `build_models` and `build_model`; the TUI shows the error inside a working session.
/// Design: `docs/design/surfaces.md` §2.
/// Up: `build_models` and `build_model` construct it.
/// Down: the `Model` impl's `complete`, which returns a transport error.
/// Verify: `cargo test -p loongagent`.
/// The model a first run gets: it answers every call with the one thing the user
/// needs to know. Refusing to start would be worse — the TUI shows this as an
/// error line inside a working session, next to the command that fixes it.
struct UnconfiguredModel;

impl Model for UnconfiguredModel {
    /// Purpose: Answer every completion with the one error that says no model is configured.
    /// Contract: `server.rs` `Model` impl for the unconfigured stand-in.
    fn complete(&mut self, _req: &CompletionRequest) -> Result<CompletionResponse, ModelError> {
        Err(ModelError::Transport(
            "no model is configured yet — run `loongagent setup` to pick one, then start again              (or pass --model echo / demo-write / demo-escape to exercise the loop without a model)"
                .to_string(),
        ))
    }
}

/// Purpose: The plainest provider double: repeats the last user message, with no keys and no network.
/// Serves: `build_model`'s `echo` case and the end-to-end test.
/// Design: `docs/design/surfaces.md` §2 (MODELS, the provider doubles).
/// Up: `build_model` constructs it.
/// Down: the `Model` impl's `complete`.
/// Verify: `cargo test -p loongagent --test vertical_slice`.
/// The plainest possible provider double: repeats the last user message. Used by
/// `run` and by the end-to-end test; needs no keys and no network.
struct EchoModel;

impl Model for EchoModel {
    /// Purpose: Repeat the last user message as `echo: <text>` and report trivial usage.
    /// Serves: the `echo` double and the client test that drives a real server.
    /// Design: `docs/design/surfaces.md` §2.
    /// Up: the kernel's turn loop, through the `Model` trait.
    /// Down: `ChatMessage` matching and `CompletionResponse`; no further call.
    /// Verify: `cargo test -p loongagent --test vertical_slice`.
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

#[cfg(test)]
mod endpoint_key_tests {
    use super::endpoint_authenticates;

    /// Purpose: A loopback endpoint is never warned about a missing key, whatever form the URL takes.
    #[test]
    fn a_loopback_endpoint_does_not_need_a_key() {
        // The bug this exists for: the warning fired for `ollama/qwen2.5:7b-instruct`
        // and sent the owner looking for a key that does not exist. A local server
        // answers a keyless request with 200 — measured against Ollama 0.40.3.
        for url in [
            "http://127.0.0.1:11434/v1",
            "http://localhost:11434/v1",
            "http://127.0.0.1:8000",
            "http://[::1]:11434/v1",
            "http://0.0.0.0:11434/v1",
            // No scheme, a port, and a path: still the same host.
            "127.0.0.1:8080/v1",
        ] {
            assert!(
                !endpoint_authenticates(url),
                "{url} is on this machine and needs no key"
            );
        }
    }

    /// Purpose: A remote or unreadable endpoint keeps the missing-key warning.
    #[test]
    fn a_remote_endpoint_still_wants_a_key() {
        // The warning has to keep working where it is true, or it has been deleted
        // rather than fixed. If the URL cannot be read at all, assume it is remote:
        // a real 401 is worth one line of noise.
        for url in [
            "https://api.openai.com/v1",
            "https://gateway.internal.corp/v1",
            "http://10.0.0.5:8080/v1",
            "http://host.docker.internal:11434/v1",
            "not a url",
        ] {
            assert!(
                endpoint_authenticates(url),
                "{url} is not loopback, so a keyless request may 401"
            );
        }
    }
}
