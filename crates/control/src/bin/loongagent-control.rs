//! `loongagent-control` — the control plane, and the tools a **profile
//! administrator** uses to publish to it.
//!
//! Deliberately small (ADR-0012 §1): this binary distributes signed profile packs
//! and nothing else. It is not a session bus, a document server or a job scheduler,
//! and it holds no conversation content.
//!
//! The two roles are separated by token, not by goodwill:
//!
//! - **profile administrator**: holds the signing key, runs `keygen`/`sign`, and
//!   publishes with the admin token;
//! - **every install**: reads with the client token, and verifies what it reads
//!   against the public key pinned in its own configuration.
//!
//! The private key never reaches the server; only the public key does.

use loong_control::{bind, ControlPlane, DEFAULT_TIMEOUT};
use loong_pack::{
    generate_keypair, key_id, public_of, Envelope, PROFILES_ARTIFACT, PROFILES_SCHEMA,
};
use std::path::{Path, PathBuf};

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
  loongagent-control keygen [--out DIR]
      Make an Ed25519 signing keypair. Writes control-key.pkcs8 (mode 0600) and
      prints the public key to pin in every install. The private key never leaves
      this machine.

  loongagent-control sign --pack FILE --key FILE --issuer NAME [--version V]
                          [--scope SCOPE] [-o profile-pack.json]
      Sign a profile pack (the TOML a profile administrator writes) into the
      envelope every install verifies.

  loongagent-control show --file profile-pack.json [--public-key HEX]
      Decode an envelope, show who signed it, and verify it if a key is given.

  loongagent-control serve --dir DIR --public-key HEX [--key FILE] --issuer NAME
                           --client-token-env NAME --admin-token-env NAME
                           [--bind IP] [--port N]
      Serve the published pack. Reads need the client token; publishing needs the
      admin token.

  loongagent-control publish --url URL --file profile-pack.json --token-env NAME
      Send a signed pack to a running control plane.

  loongagent-control health --url URL
      Is it up, and does it have a pack?

TOKENS come from the environment (--*-token-env) or a file (--*-token-file), never
from the command line: arguments are visible to every process on the machine.

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
        "serve" => serve(rest),
        "publish" => publish(rest),
        "health" => health(rest),
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

fn show(args: &[String]) -> Result<(), String> {
    let path = flag(args, "--file")?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("reading {path}: {e}"))?;
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
    let public = match (optional(args, "--public-key"), optional(args, "--key")) {
        (Some(hex), _) => loong_pack::unhex(&hex).ok_or("--public-key must be hex")?,
        (None, Some(key_path)) => {
            let key = std::fs::read(&key_path).map_err(|e| format!("reading {key_path}: {e}"))?;
            public_of(&key).map_err(|e| e.to_string())?
        }
        _ => return Err("--public-key HEX (or --key FILE) is required".to_string()),
    };
    let client_token = read_token(args, "--client-token-env", "--client-token-file")?;
    let admin_token = read_token(args, "--admin-token-env", "--admin-token-file")?;
    let bind_ip = optional(args, "--bind").unwrap_or_else(|| "127.0.0.1".to_string());
    let port: u16 = optional(args, "--port")
        .unwrap_or_else(|| "8738".to_string())
        .parse()
        .map_err(|_| "--port must be a number".to_string())?;

    let plane = ControlPlane::new(&dir, client_token, admin_token, public.clone(), &issuer);
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
    let file = flag(args, "--file")?;
    let token = read_token(args, "--token-env", "--token-file")?;
    let body = std::fs::read_to_string(&file).map_err(|e| format!("reading {file}: {e}"))?;
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
    println!("published: {}", response.body);
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
