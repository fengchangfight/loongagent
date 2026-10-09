//! `loongagent-control` — the control plane, and the tools a **profile
//! administrator** uses to publish to it.
//!
//! Deliberately small (ADR-0012 §1): this binary distributes signed profile packs
//! and nothing else. It is not a session bus, a document server or a job scheduler,
//! and it holds no conversation content.
//!
//! The two roles are separated by token, not by goodwill:
//!
//! - **profile administrator**: writes a draft and publishes it with the admin token;
//! - **every install**: reads with the client token, and verifies what it reads
//!   against the public key pinned in its own configuration.
//!
//! **The control plane holds the signing key** (owner decision, 2026-10-09): it signs
//! the drafts an administrator publishes, so the private key never has to be copied
//! to a workstation. The trade is named in ADR-0016 §2 — a compromised control plane
//! can mint packs. `sign` remains for an air-gapped administrator who has the key
//! somewhere else; the envelope is identical, so installs cannot tell them apart.

use loong_control::{bind, ControlPlane, DEFAULT_TIMEOUT};
use loong_pack::{
    generate_keypair, key_id, public_of, Envelope, PROFILES_ARTIFACT, PROFILES_SCHEMA,
};
use std::path::{Path, PathBuf};

/// Walk up from `path` looking for a git work tree (a `.git` file or directory).
///
/// The signing key is the one file in this system that must never be committed: a
/// key in a repository is an off-site ability to mint packs every install accepts.
/// Administrators run `keygen` wherever they happen to be, so the tool checks.
fn git_work_tree(path: &Path) -> Option<PathBuf> {
    let mut dir = std::fs::canonicalize(path).ok()?;
    loop {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// `Some(stdout)` when the git command succeeded. A missing git or a repository that
/// is not one is not an error: it means "cannot tell", and we say less rather than
/// cry wolf.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).to_string())
}

fn is_tracked(repo: &Path, name: &str) -> bool {
    git(repo, &["ls-files", "--error-unmatch", name]).is_some()
}

fn is_ignored(repo: &Path, name: &str) -> bool {
    git(repo, &["check-ignore", "-q", name]).is_some()
}

/// Names that hold a secret. Deliberately about *this* system's files: an
/// `.env.example` is documentation, not a secret.
fn looks_like_a_secret(name: &str) -> bool {
    if name.ends_with(".example") || name.ends_with(".template") {
        return false;
    }
    matches!(
        name,
        "control-key.pkcs8" | "client.token" | "admin.token" | "credentials.toml" | ".env" | "env"
    ) || name.ends_with(".pkcs8")
        || name.ends_with(".key")
        || name.ends_with(".pem")
        || name.starts_with(".env.")
}

/// Refuse to *create* private material inside a work tree unless told twice.
///
/// `names` is what this command is about to write, so the forced path can name the
/// `.gitignore` lines before the files exist.
fn guard_private_material(
    dir: &Path,
    what: &str,
    names: &[&str],
    force: bool,
) -> Result<(), String> {
    let Some(repo) = git_work_tree(dir) else {
        return Ok(());
    };
    if !force {
        return Err(format!(
            "refusing to write {what} inside the git work tree at {}\n\
             \ta signing key in a repository is an off-site ability to mint packs every install \
             accepts.\n\
             \tuse --out /etc/loongagent-control (the layout the runbook describes), or pass \
             --force if this repository exists to hold secrets and ignores them.",
            repo.display()
        ));
    }
    eprintln!(
        "warning: writing {what} inside the git work tree at {} because --force was given",
        repo.display()
    );
    for name in names {
        if !is_ignored(&repo, name) {
            eprintln!("  {name} is not ignored by git — add this line to .gitignore:  {name}");
        }
    }
    Ok(())
}

/// Publishing is where a leak in the *repository holding the draft* becomes visible:
/// the draft itself can only carry references, but the repository may also hold the
/// machine's `.env` next to it.
fn guard_pack_repository(pack: &Path, force: bool) -> Result<(), String> {
    let dir = pack.parent().unwrap_or(Path::new("."));
    let Some(repo) = git_work_tree(dir) else {
        return Ok(());
    };
    let mut places = vec![repo.clone()];
    let pack_dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    if pack_dir != repo {
        places.push(pack_dir);
    }
    let mut tracked = Vec::new();
    let mut unignored = Vec::new();
    for place in places {
        let Ok(entries) = std::fs::read_dir(&place) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !looks_like_a_secret(&name) {
                continue;
            }
            if is_tracked(&repo, &name) {
                tracked.push(place.join(&name).display().to_string());
            } else if !is_ignored(&repo, &name) {
                unignored.push(place.join(&name).display().to_string());
            }
        }
    }
    if !tracked.is_empty() && !force {
        return Err(format!(
            "a secret is already tracked by git:\n\t{}\n\
             \tfix it before publishing:  git rm --cached <that file>, add it to .gitignore, and \
             rotate the secret.\n\
             \t(--force publishes anyway; this check is about the repository, not the pack.)",
            tracked.join("\n\t")
        ));
    }
    if !unignored.is_empty() {
        eprintln!(
            "warning: not ignored by git (one `git add -A` from a leak): {}",
            unignored.join(", ")
        );
    }
    Ok(())
}

fn main() {
    if let Err(message) = run() {
        eprintln!("loongagent-control: {message}");
        std::process::exit(1);
    }
}

fn usage() -> String {
    "\
loongagent-control — distribute signed profile packs (ADR-0008 §6, ADR-0016)

USAGE
  loongagent-control keygen [--out DIR] [--force]
      Make an Ed25519 signing keypair. Writes control-key.pkcs8 (mode 0600) and
      prints the public key to pin in every install. The private key stays with the
      control plane: install it there, and administrators publish drafts.

  loongagent-control serve --dir DIR --key control-key.pkcs8 --issuer NAME
                           --client-token-env NAME --admin-token-env NAME
                           [--public-key HEX] [--bind IP] [--port N]
      Serve the published pack **and hold the signing key**, so an administrator
      publishes a draft and the control plane signs it (owner decision, 2026-10-09).
      The private key never has to be copied to an administrator's workstation.
      Reads need the client token; publishing needs the admin token.

  loongagent-control publish --url URL --pack profiles.toml --token-env NAME
                             [--version V] [--issuer NAME] [--scope SCOPE]
      Send a draft to the control plane, which validates and signs it. This is the
      normal way to publish.

  loongagent-control publish --url URL --file profile-pack.json --token-env NAME
      Send an envelope that was signed elsewhere. For an air-gapped administrator;
      the envelope is the same, so installs cannot tell the difference.

  loongagent-control public-key --key control-key.pkcs8
      Print the public key (and its id) for a signing key: the value installs pin.
      The keygen output is easy to lose; this is how you get it back.

  loongagent-control show --file profile-pack.json [--public-key HEX]
  loongagent-control show --url URL --token-env NAME [--public-key HEX]
      Decode an envelope — from a file, or from what a control plane is serving —
      show who signed it, and verify it if a key is given.

  loongagent-control sign --pack FILE --key FILE --issuer NAME [--version V]
                          [--scope SCOPE] [-o profile-pack.json]
      Sign a pack **without** a control plane, for an air-gapped administrator.
      The normal path is `publish --pack`, which the control plane signs itself.

  loongagent-control health --url URL
      Is it up, and does it have a pack?

TOKENS come from the environment (--*-token-env) or a file (--*-token-file), never
from the command line: arguments are visible to every process on the machine.

GIT: keygen refuses to write a signing key inside a git work tree, and publish
refuses to run while a secret is *tracked* in the repository holding the draft. A
signing key in a repository is an off-site ability to mint packs; `--force` overrides
either check and says what to add to .gitignore.

A PACK is TOML with `default_profile` and one or more `[[profiles]]`, each with a
`description`, `[[profiles.models]]` and an optional `[profiles.router]`. Engineers
see the name and the description; the endpoints stay here.
"
    .to_string()
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("");
    let rest = &args[args.len().min(1)..];
    match command {
        "keygen" => keygen(rest),
        "sign" => sign(rest),
        "show" => show(rest),
        "public-key" => public_key(rest),
        "serve" => serve(rest),
        "publish" => publish(rest),
        "health" => health(rest),
        "-V" | "--version" => {
            println!(
                "loongagent-control {} (pack schema {})",
                env!("CARGO_PKG_VERSION"),
                loong_pack::PROFILES_SCHEMA
            );
            Ok(())
        }
        "-h" | "--help" | "help" | "" => {
            println!("{}", usage());
            Ok(())
        }
        other => Err(format!("unknown command {other:?}\n\n{}", usage())),
    }
}

fn flag(args: &[String], name: &str) -> Result<String, String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
        .ok_or_else(|| format!("{name} is required"))
}

fn optional(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn read_token(args: &[String], env_flag: &str, file_flag: &str) -> Result<String, String> {
    if let Some(name) = optional(args, env_flag) {
        return std::env::var(&name)
            .map_err(|_| format!("{env_flag} {name}: not set in this environment"));
    }
    if let Some(path) = optional(args, &file_flag) {
        return std::fs::read_to_string(&path)
            .map(|text| text.trim().to_string())
            .map_err(|e| format!("reading {path}: {e}"));
    }
    Err(format!(
        "{env_flag} NAME (or {file_flag} PATH) is required — a token on the command line \
         would be visible to every process on this machine"
    ))
}

fn keygen(args: &[String]) -> Result<(), String> {
    let dir = optional(args, "--out").unwrap_or_else(|| ".".to_string());
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    guard_private_material(
        &dir,
        "the signing key",
        &["control-key.pkcs8"],
        args.iter().any(|a| a == "--force"),
    )?;
    let key_path = dir.join("control-key.pkcs8");
    if key_path.exists() {
        return Err(format!(
            "{} already exists — move it aside first; overwriting a signing key would \
             invalidate every install that pinned it",
            key_path.display()
        ));
    }
    let (private, public) = generate_keypair().map_err(|e| e.to_string())?;
    std::fs::write(&key_path, &private)
        .map_err(|e| format!("writing {}: {e}", key_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("setting permissions on {}: {e}", key_path.display()))?;
    }
    println!("wrote {} (mode 0600)", key_path.display());
    println!();
    println!("public key (pin this in every install):");
    println!("  {}", loong_pack::hex(&public));
    println!("key id: {}", key_id(&public));
    println!();
    println!("pin it with, on each machine:");
    println!(
        "  loongagent setup --url https://<control-plane> --public-key {}",
        loong_pack::hex(&public)
    );
    Ok(())
}

fn sign(args: &[String]) -> Result<(), String> {
    let pack_path = flag(args, "--pack")?;
    let key_path = flag(args, "--key")?;
    let issuer = flag(args, "--issuer")?;
    let version = optional(args, "--version").unwrap_or_else(|| {
        // A default version that sorts sensibly and says when it was made.
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        format!("{seconds}")
    });
    let scope = optional(args, "--scope").unwrap_or_else(|| "org".to_string());
    let out = optional(args, "-o").unwrap_or_else(|| "profile-pack.json".to_string());

    let payload =
        std::fs::read_to_string(&pack_path).map_err(|e| format!("reading {pack_path}: {e}"))?;
    // Refuse to sign something the local resolver would reject anyway: the
    // administrator finds out now, not in every install's error log.
    let parsed = loong_config::profiles_from_payload(&payload)
        .map_err(|e| format!("{pack_path} is not a usable profile pack: {e}"))?;
    let key = std::fs::read(&key_path).map_err(|e| format!("reading {key_path}: {e}"))?;

    let issued_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let envelope = Envelope::sign(
        &key,
        PROFILES_ARTIFACT,
        PROFILES_SCHEMA,
        &version,
        &issuer,
        &scope,
        issued_at_ms,
        &payload,
    )
    .map_err(|e| e.to_string())?;

    // Prove it verifies before writing it out.
    let public = public_of(&key).map_err(|e| e.to_string())?;
    envelope
        .verify(&public, &[PROFILES_SCHEMA])
        .map_err(|e| format!("the pack we just signed does not verify: {e}"))?;

    std::fs::write(&out, envelope.to_json().map_err(|e| e.to_string())?)
        .map_err(|e| format!("writing {out}: {e}"))?;
    println!("signed {out}");
    println!("  artifact: {}", envelope.artifact);
    println!("  version:  {}", envelope.version);
    println!("  issuer:   {}", envelope.issuer);
    println!("  scope:    {}", envelope.scope);
    println!("  key id:   {}", envelope.key_id);
    println!("  profiles: {}", parsed.profiles.len());
    for profile in &parsed.profiles {
        println!(
            "    - {} — {}",
            profile.name,
            first_line(&profile.description)
        );
    }
    println!();
    println!("publish it with:");
    println!("  loongagent-control publish --url https://<control-plane> --file {out} --token-env ADMIN_TOKEN");
    Ok(())
}

/// The pinning value for an existing signing key. Not a secret, and the one thing an
/// administrator needs after the keygen output has scrolled away.
fn public_key(args: &[String]) -> Result<(), String> {
    let key_path = flag(args, "--key")?;
    let key = std::fs::read(&key_path).map_err(|e| format!("reading {key_path}: {e}"))?;
    let public = public_of(&key).map_err(|e| e.to_string())?;
    println!("{}", loong_pack::hex(&public));
    eprintln!("key id: {}", key_id(&public));
    Ok(())
}

fn show(args: &[String]) -> Result<(), String> {
    let text = match (optional(args, "--file"), optional(args, "--url")) {
        (Some(path), _) => {
            std::fs::read_to_string(&path).map_err(|e| format!("reading {path}: {e}"))?
        }
        (None, Some(url)) => {
            let token = read_token(args, "--token-env", "--token-file")?;
            let response = loong_http::get_json(
                &format!("{}/v1/pack/profiles", url.trim_end_matches('/')),
                Some(&token),
                DEFAULT_TIMEOUT,
            )
            .map_err(|e| e.0)?;
            println!("from {url}:");
            response.body
        }
        _ => return Err("--file PATH (or --url URL) is required".to_string()),
    };
    let envelope = Envelope::from_json(&text).map_err(|e| e.to_string())?;
    println!("artifact:  {}", envelope.artifact);
    println!("schema:    {}", envelope.payload_schema);
    println!("version:   {}", envelope.version);
    println!("issuer:    {}", envelope.issuer);
    println!("scope:     {}", envelope.scope);
    println!("key id:    {}", envelope.key_id);
    println!("issued:    {}", envelope.issued_at_ms);
    if let Some(public) = optional(args, "--public-key") {
        let public = loong_pack::unhex(&public).ok_or("--public-key must be hex")?;
        match envelope.verify(&public, &[PROFILES_SCHEMA]) {
            Ok(()) => println!("signature: verified under the key you gave"),
            Err(e) => {
                println!("signature: NOT VERIFIED — {e}");
                return Err("refusing to describe a pack that does not verify".to_string());
            }
        }
    } else {
        println!("signature: not checked (pass --public-key to check it)");
    }
    println!();
    match loong_config::profiles_from_payload(&envelope.payload) {
        Ok(pack) => {
            println!(
                "default profile: {}",
                pack.default_profile.unwrap_or_else(|| "(the first)".into())
            );
            for profile in &pack.profiles {
                println!("  {} — {}", profile.name, first_line(&profile.description));
                for model in &profile.models {
                    println!(
                        "      model {} → {} ({})",
                        model.name, model.model, model.base_url
                    );
                }
                println!(
                    "      router default: {}",
                    profile.router.default.clone().unwrap_or_default()
                );
                for rule in &profile.router.rules {
                    println!("      when {} use {}", rule.condition.render(), rule.model);
                }
            }
        }
        Err(e) => println!("payload is not a usable profile pack: {e}"),
    }
    Ok(())
}

fn serve(args: &[String]) -> Result<(), String> {
    let dir = flag(args, "--dir")?;
    let issuer = flag(args, "--issuer")?;
    let key_path = flag(args, "--key")?;
    let private = std::fs::read(&key_path).map_err(|e| format!("reading {key_path}: {e}"))?;
    let derived = public_of(&private).map_err(|e| e.to_string())?;
    // `--public-key` is optional but checked when given: serving under one key and
    // signing with another would publish packs no install accepts.
    let public = match optional(args, "--public-key") {
        Some(hex) => {
            let pinned = loong_pack::unhex(&hex).ok_or("--public-key must be hex")?;
            if pinned != derived {
                return Err(format!(
                    "--public-key pins {} but {key_path} is key {} — refusing to serve packs \
                     nobody could verify",
                    key_id(&pinned),
                    key_id(&derived)
                ));
            }
            pinned
        }
        None => derived,
    };
    let client_token = read_token(args, "--client-token-env", "--client-token-file")?;
    let admin_token = read_token(args, "--admin-token-env", "--admin-token-file")?;
    let bind_ip = optional(args, "--bind").unwrap_or_else(|| "127.0.0.1".to_string());
    let port: u16 = optional(args, "--port")
        .unwrap_or_else(|| "8738".to_string())
        .parse()
        .map_err(|_| "--port must be a number".to_string())?;

    let plane = ControlPlane::new(&dir, client_token, admin_token, public.clone(), &issuer)
        .with_signing_key(private)
        .map_err(|e| e.to_string())?;
    let listener = bind(&bind_ip, port).map_err(|e| format!("binding {bind_ip}:{port}: {e}"))?;
    println!(
        "loongagent-control serving {}",
        listener
            .local_addr()
            .map(|a| a.to_string())
            .unwrap_or_default()
    );
    println!("  directory: {}", dir);
    println!("  issuer:    {issuer}");
    println!("  key id:    {}", key_id(&public));
    println!("  public key: {}", loong_pack::hex(&public));
    println!(
        "  pack:      {}",
        if plane.has_pack() {
            "published"
        } else {
            "nothing published yet"
        }
    );
    println!();
    println!("clients read  GET  /v1/pack/profiles   (client token)");
    println!("admins publish POST /v1/admin/publish   (admin token)");
    println!("health        GET  /v1/health           (no token)");
    plane.serve(listener).map_err(|e| format!("serving: {e}"))
}

fn publish(args: &[String]) -> Result<(), String> {
    let url = flag(args, "--url")?;
    let token = read_token(args, "--token-env", "--token-file")?;
    let body = match (optional(args, "--pack"), optional(args, "--file")) {
        (Some(pack), None) => {
            // A draft: the control plane validates and signs it. This is the normal
            // path, and it is why the signing key never leaves the server.
            guard_pack_repository(Path::new(&pack), args.iter().any(|a| a == "--force"))?;
            let payload =
                std::fs::read_to_string(&pack).map_err(|e| format!("reading {pack}: {e}"))?;
            // Validate locally too, so the administrator hears about a broken pack
            // before a round trip.
            loong_config::profiles_from_payload(&payload)
                .map_err(|e| format!("{pack} is not a usable profile pack: {e}"))?;
            let version = optional(args, "--version").unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs().to_string())
                    .unwrap_or_else(|_| "1".to_string())
            });
            // No `--issuer` means "the identity this control plane was started with",
            // which is the honest answer: that is who is signing.
            let mut request = serde_json::json!({
                "payload": payload,
                "version": version,
                "scope": optional(args, "--scope").unwrap_or_else(|| "org".to_string()),
            });
            if let Some(issuer) = optional(args, "--issuer") {
                request["issuer"] = serde_json::json!(issuer);
            }
            request.to_string()
        }
        (None, Some(file)) => {
            std::fs::read_to_string(&file).map_err(|e| format!("reading {file}: {e}"))?
        }
        _ => {
            return Err(
                "--pack FILE (a draft) or --file FILE (a signed envelope) is required".to_string(),
            )
        }
    };
    let response = loong_http::post_json(
        &format!("{}/v1/admin/publish", url.trim_end_matches('/')),
        &body,
        Some(&token),
        DEFAULT_TIMEOUT,
    )
    .map_err(|e| e.0)?;
    if !(200..300).contains(&response.status) {
        return Err(format!(
            "the control plane refused it (HTTP {}): {}",
            response.status, response.body
        ));
    }
    let value: serde_json::Value = serde_json::from_str(&response.body).unwrap_or_default();
    println!(
        "published {} {} (key {})",
        value.get("issuer").and_then(|v| v.as_str()).unwrap_or("?"),
        value.get("version").and_then(|v| v.as_str()).unwrap_or("?"),
        value.get("key_id").and_then(|v| v.as_str()).unwrap_or("?")
    );
    if let Some(profiles) = value.get("profiles").and_then(|v| v.as_array()) {
        let names: Vec<&str> = profiles.iter().filter_map(|p| p.as_str()).collect();
        println!("  profiles: {}", names.join(", "));
    }
    Ok(())
}

fn health(args: &[String]) -> Result<(), String> {
    let url = flag(args, "--url")?;
    let response = loong_http::get_json(
        &format!("{}/v1/health", url.trim_end_matches('/')),
        None,
        DEFAULT_TIMEOUT,
    )
    .map_err(|e| e.0)?;
    let value: serde_json::Value =
        serde_json::from_str(&response.body).map_err(|e| format!("health is not JSON: {e}"))?;
    println!(
        "{}",
        serde_json::to_string_pretty(&value).unwrap_or(response.body)
    );
    if value.get("can_sign").and_then(|v| v.as_bool()) != Some(true) {
        eprintln!(
            "warning: this control plane has no signing key, so it cannot accept drafts — start \
             it with --key"
        );
    }
    if value.get("has_pack").and_then(|v| v.as_bool()) != Some(true) {
        return Err(
            "the control plane is up but has no profiles published — publish a pack before \
             pointing installs at it"
                .to_string(),
        );
    }
    Ok(())
}

fn first_line(text: &str) -> String {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

#[allow(dead_code)]
fn pack_of(path: &Path) -> Result<Envelope, String> {
    Envelope::from_json(&std::fs::read_to_string(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}
