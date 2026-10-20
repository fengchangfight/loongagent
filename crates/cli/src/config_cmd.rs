//! Purpose: The config inspection commands and the reference resolver: every key says which layer set it.
//!
//! # Contract
//! - **Serves:** `config show`, `config explain`, and the environment and credential loading every run does before it resolves anything.
//! - **Design:** `docs/design/surfaces.md` §2; `docs/design/config-routing.md` §1–§5.
//! - **Upstream:** `main.rs`'s `config` arms, and `Args::parse`, which loads the env files first.
//! - **Downstream:** `loong_config::{resolve, layer_paths, explain_all, Kind}` and the environment or credential files it reads.
//! - **Verify:** `cargo test -p loongagent --test config_layers` · `./scripts/check.sh`

use super::*;

/// Purpose: Resolve a `file:` target as absolute, `~/…`, or relative to the profile in use.
/// Contract: `config_cmd.rs` reference resolution; `resolve_reference` is the only caller.
/// Resolve a configuration reference (ADR-0008 §5: config carries a reference, not
/// a secret). A bare value is passed through only for non-secret keys.
/// Where a `file:` reference points: absolute, `~/…`, or relative to the profile in
/// use, so a profile and its credentials file move together.
fn credential_path(path: &str, base: &Path) -> Option<PathBuf> {
    if let Some(relative) = path.strip_prefix("~/") {
        return loong_config::home_dir().map(|home| home.join(relative));
    }
    if Path::new(path).is_absolute() {
        return Some(PathBuf::from(path));
    }
    Some(base.join(path))
}

/// Purpose: The two env files a run reads, in the order that makes the user win over the workspace.
/// Serves: `load_env_files` and `read_env_files`; a pack may name `env:NAME` and still work.
/// Design: `docs/design/config-routing.md` §5 (a secret is a reference).
/// Up: `load_env_files`; the `local.rs` tests also call it.
/// Down: `Path::join`; no file is read here.
/// Verify: `cargo test -p loongagent --test config_layers`.
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
pub(crate) fn env_file_paths(user_config_dir: &Path, workspace: &Path) -> Vec<PathBuf> {
    vec![user_config_dir.join("env"), workspace.join(".env")]
}

/// Purpose: Parse the env files into name/value pairs, first definition winning.
/// Serves: `load_env_files`, and the test that pins the precedence.
/// Design: `docs/design/config-routing.md` §5.
/// Up: `load_env_files` and the `local.rs` tests; no other caller.
/// Down: `std::fs::read_to_string` and the line parser in this function.
/// Verify: `cargo test -p loongagent --test config_layers`.
/// Parse the files into `(name, value)` pairs, first definition winning.
pub(crate) fn read_env_files(paths: &[PathBuf]) -> Vec<(String, String)> {
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

/// Purpose: Put the files' variables into this process and name the file each one came from.
/// Serves: `Args::parse`, so a pack's `env:NAME` resolves without an export in every shell.
/// Design: `docs/design/config-routing.md` §5.
/// Up: `Args::parse`; no other caller.
/// Down: `env_file_paths`, `read_env_files` and `std::env::set_var`.
/// Verify: `cargo test -p loongagent --test config_layers`.
/// Apply them to this process, naming the file each variable actually came from.
/// Names are printed, never values.
pub(crate) fn load_env_files(user_config_dir: &Path, workspace: &Path) {
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

/// Purpose: Read one named secret out of a credentials file, naming the keys that exist but never their values.
/// Serves: the `file:PATH#KEY` shape of `resolve_reference`.
/// Design: `docs/design/config-routing.md` §5.
/// Up: `resolve_reference`; the `local.rs` tests call it directly.
/// Down: `std::fs::read_to_string` and the per-line parser.
/// Verify: `cargo test -p loongagent --test config_layers`.
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
pub(crate) fn read_credential(path: &Path, key: &str) -> Option<String> {
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

/// Purpose: Turn `env:` and `file:` references into a value, and refuse to pretend a credential store is wired.
/// Serves: `control_config` (the token) and `server.rs` (the key and base URL).
/// Design: `docs/design/config-routing.md` §5.
/// Up: `control_config`, `server.rs`'s `build_model` and `build_models`, and `local.rs`'s `local_ping`.
/// Down: `credential_path`, `read_credential` and `std::env::var`.
/// Verify: `cargo test -p loongagent --test config_layers`.
pub(crate) fn resolve_reference(value: Option<&str>, base: &Path) -> Option<String> {
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

/// Purpose: Print the layer list and one row per key with its kind and the layer that set it.
/// Serves: `loongagent config show`, the answer to why a value is what it is.
/// Design: `docs/design/config-routing.md` §2 and §4.
/// Up: `main.rs`'s `config` → `show` arm; no other caller.
/// Down: `Args::settings_with_pack`, `loong_config::layer_paths` and `Profile::explain_all`.
/// Verify: `cargo test -p loongagent --test config_layers`.
/// Every key answers "why is this the value?" (ADR-0008 §4).
pub(crate) fn config_show(args: Args) -> Result<(), String> {
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
    // `--local` is a session-layer flag that exists in no file, so it has no row to
    // appear in. It still answers "may this run reach a control plane?", which is not
    // a thing to leave somebody to infer from the absence of a line.
    if settings.is_local() && settings.explain("mode").is_none() {
        println!(
            "{:<30} {:<16} {:<8} {}",
            "mode", "local", "free-override", "--local (this run)"
        );
    }
    // A budget may be declared on a model rather than on the session, and the strictest one
    // is what a run enforces (`effective_budget`). That is how the pack this repository
    // ships wrote `budget_tokens = 128000`: the session key was never set, so the table
    // showed nothing and the cap did nothing. Say who is setting it.
    let declared = settings
        .models
        .iter()
        .filter_map(|model| {
            model
                .budget_tokens
                .map(|budget| (model.name.clone(), budget))
        })
        .min_by_key(|(_, budget)| *budget);
    if let Some((name, tokens)) = declared {
        if settings
            .budget_tokens
            .map_or(true, |session| tokens < session)
        {
            println!(
                "{:<30} {:<16} {:<8} {}",
                "budget_tokens",
                tokens,
                loong_config::Kind::NarrowOnly.as_str(),
                format!("the cap on model {name}")
            );
        }
    }
    // A default no layer sets is still the value a run uses. The step limit is the one that
    // has bitten somebody — a turn stopped at 8 steps, and no command said where the 8 came
    // from — so it is listed even when nothing configured it.
    if settings.explain("max_steps").is_none() {
        println!(
            "{:<30} {:<16} {:<8} {}",
            "max_steps",
            loong_kernel::DEFAULT_MAX_STEPS,
            loong_config::Kind::NarrowOnly.as_str(),
            "<defaults>"
        );
    }
    println!();
    println!("(· = no such file; built-in defaults come from <defaults>)");
    println!("(a step is one model call; `--max-steps 0` means no limit)");
    Ok(())
}

/// Purpose: Answer the same question for one key, including the values that were set and lost.
/// Serves: `loongagent config explain KEY`.
/// Design: `docs/design/config-routing.md` §2.
/// Up: `main.rs`'s `config` → `explain` arm; no other caller.
/// Down: `Args::settings_with_pack`, `loong_config::kind_of` and `Profile::explain`.
/// Verify: `cargo test -p loongagent --test config_layers`.
/// The same question for one key, including what lost and why.
pub(crate) fn config_explain(args: Args) -> Result<(), String> {
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
            if key == "max_steps" {
                println!(
                    "  value:  {} (the built-in default; `--max-steps 0` means no limit)",
                    loong_kernel::DEFAULT_MAX_STEPS
                );
            }
        }
    }
    Ok(())
}

/// Purpose: The one-line meaning of each merge kind, for both config commands.
/// Contract: `config_cmd.rs` explanation text, called only by `config_explain`.
fn explain_kind(kind: loong_config::Kind) -> &'static str {
    match kind {
        loong_config::Kind::FreeOverride => "the most local layer that sets it wins",
        loong_config::Kind::NarrowOnly => "strictest wins; a lower layer may only narrow",
        loong_config::Kind::AddOnly => "union across layers",
        loong_config::Kind::Reference => "names a source; secrets never live in config",
    }
}
