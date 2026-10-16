use super::*;

use crate::config_cmd::resolve_reference;

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

/// The machine's control-plane configuration: who this installation talks to, and
/// the key it pinned. Root-owned and delivered with the binary (image, MDM) — the
/// one value a user must not be able to change is the pin (ADR-0016 §3).
///
/// `/etc/loongagent/control.toml` on Unix; `%PROGRAMDATA%\loongagent\control.toml`
/// on Windows, because the Unix path would be read as drive-relative there and could
/// never be the image/MDM location it is meant to be.
fn system_control_path() -> PathBuf {
    loong_config::system_config_dir().join("control.toml")
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
pub(crate) fn merge_control(
    system: Option<ControlFile>,
    user: Option<ControlFile>,
) -> Option<ControlFile> {
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

pub(crate) fn load_control() -> Result<Option<ControlFile>, String> {
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
pub(crate) fn control_config(
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

pub(crate) fn first_line(text: &str) -> String {
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
pub(crate) fn sync_pack(args: &Args) -> Result<Option<PackState>, String> {
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
pub(crate) fn cached_pack_state(args: &Args) -> Result<Option<PackState>, String> {
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
pub(crate) fn setup(args: Args) -> Result<(), String> {
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
pub(crate) fn connect(
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
pub(crate) fn control(rest: &[String]) -> Result<(), String> {
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
pub(crate) fn profiles(args: Args) -> Result<(), String> {
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
pub(crate) fn use_profile(args: Args, rest: &[String]) -> Result<(), String> {
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
