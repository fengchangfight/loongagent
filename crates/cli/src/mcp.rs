//! Purpose: The two `mcp` commands: which external tool servers would join a session, and the per-repository trust record.
//!
//! # Contract
//! - **Serves:** `loongagent mcp list` and `mcp trust`, the CLI-only half of MCP trust.
//! - **Design:** `docs/design/surfaces.md` §2; `docs/design/extension-model.md` §3 and §7.
//! - **Upstream:** `main.rs`'s `mcp` arms dispatch here.
//! - **Downstream:** `loong_mcp::McpSet` — `load`, `summary`, `trust_now` — and the manifest fields it prints.
//! - **Verify:** `cargo test -p loongagent` · `./scripts/check.sh`

use super::*;

/// Purpose: Print every configured server, its transport, action class, overrides and defer policy, and whether the manifest is trusted enough to run.
/// Serves: `loongagent mcp list`, the read before an `mcp trust` decision.
/// Design: `docs/design/mcp-client.md` §1 and `docs/design/extension-model.md` §3 and §7.
/// Up: `main.rs`'s `mcp` → `list` arm; no other caller.
/// Down: `McpSet::load`, `McpSet::summary` and `McpServerSpec::{transport, endpoint}`.
/// Verify: `cargo test -p loongagent` (no dedicated test; the command is inspection only).
/// Show which external tools would join the session.
pub(crate) fn mcp_list(args: Args) -> Result<(), String> {
    let set = loong_mcp::McpSet::load(&args.cwd).map_err(|e| e.to_string())?;
    println!("manifest: {}", set.dir.join(loong_mcp::MCP_FILE).display());
    let reverse_manifest = args.cwd.join(".reverse").join(loong_mcp::MCP_FILE);
    if reverse_manifest.exists() {
        println!("manifest: {}", reverse_manifest.display());
    }
    println!("status:   {}", set.summary());
    for spec in &set.servers {
        println!();
        println!(
            "  {} — [{}] {}",
            spec.name,
            spec.transport(),
            spec.endpoint()
        );
        println!("    origin:       {}", spec.origin.label());
        if spec.url.is_some() && !spec.headers.is_empty() {
            // Names only: a header value is a secret (an `Authorization`), so it is
            // never printed. `${VAR}` was expanded at load, so the value is real.
            let names: Vec<&str> = spec.headers.keys().map(String::as_str).collect();
            println!("    headers:      {}", names.join(", "));
        }
        println!("    action class: {:?}", spec.action_class);
        for (tool, class) in &spec.tools {
            println!("    override:     {tool} -> {class:?}");
        }
        println!("    timeout:      {}ms", spec.timeout.as_millis());
        println!("    required:     {}", spec.required);
        println!("    defer:        {:?}", spec.defer);
    }
    if let loong_mcp::TrustStatus::Untrusted { .. } = &set.trust {
        println!();
        println!(
            "INERT — the manifest at {} is not trusted (changed or never trusted).",
            set.dir.join(loong_mcp::MCP_FILE).display()
        );
        println!("A server entry names a command to run or a URL to call, so read it first, then:");
        println!("  loongagent mcp trust --cwd {}", args.cwd.display());
    }
    if let Some(failure) = set.reverse_trust_failure() {
        println!();
        println!("{failure}");
        println!("  loongagent mcp trust --cwd {}", args.cwd.display());
    }
    Ok(())
}

/// Purpose: Record a human trust decision over the current `mcp.toml` bytes and say where it was recorded.
/// Serves: `loongagent mcp trust`; editing the manifest re-arms the question.
/// Design: `docs/design/extension-model.md` §7.
/// Up: `main.rs`'s `mcp` → `trust` arm; no other caller.
/// Down: `McpSet::load` and `McpSet::trust_now`.
/// Verify: `cargo test -p loongagent` (no dedicated test; trust is a disk write on an explicit command).
pub(crate) fn mcp_trust(args: Args) -> Result<(), String> {
    let set = loong_mcp::McpSet::load(&args.cwd).map_err(|e| e.to_string())?;
    if set.servers.is_empty() {
        return Err("no mcp.toml in this workspace — nothing to trust".to_string());
    }
    let workspace_count = set
        .servers
        .iter()
        .filter(|s| s.origin == loong_mcp::ManifestOrigin::Workspace)
        .count();
    let reverse_count = set.servers.len() - workspace_count;
    if workspace_count == 0 && reverse_count > 0 {
        // No `.loongagent/mcp.toml`: only the reverse session's own pair exists,
        // and that pair belongs to the reverse binary — the session writes and
        // trusts it itself. A human answering this question for it would be
        // answering blind.
        return Err(format!(
            "no .loongagent/mcp.toml — the {reverse_count} server(s) in .reverse/mcp.toml belong to the reverse session, which trusts its own manifest when it starts one"
        ));
    }
    let path = set.trust_now().map_err(|e| e.to_string())?;
    println!(
        "trusted {} server(s) in {}",
        workspace_count,
        args.cwd.display()
    );
    println!("recorded in {}", path.display());
    if set.reverse_trust_failure().is_some() && reverse_count > 0 {
        let reverse_path = set.trust_reverse_now().map_err(|e| e.to_string())?;
        println!(
            "trusted {} reverse server(s); recorded in {}",
            reverse_count,
            reverse_path.display()
        );
    }
    println!("Editing either mcp.toml re-arms its question on the next session.");
    Ok(())
}
