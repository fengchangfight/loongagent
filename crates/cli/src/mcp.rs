//! Purpose: The two `mcp` commands: which external tool servers would join a session, and the per-repository trust record.
//!
//! # Contract
//! - **Serves:** `loongagent mcp list` and `mcp trust`, the CLI-only half of MCP trust.
//! - **Design:** `docs/design/surfaces.md` §2; `docs/design/extension-model.md` §3 and §7.
//! - **Upstream:** `main.rs`'s `mcp` arms dispatch here.
//! - **Downstream:** `loong_mcp::McpSet` — `load`, `summary`, `trust_now` — and the manifest fields it prints.
//! - **Verify:** `cargo test -p loongagent` · `./scripts/check.sh`

use super::*;

/// Purpose: Print every configured server, its action class and overrides, and whether the manifest is trusted enough to run.
/// Serves: `loongagent mcp list`, the read before an `mcp trust` decision.
/// Design: `docs/design/extension-model.md` §3 and §7.
/// Up: `main.rs`'s `mcp` → `list` arm; no other caller.
/// Down: `McpSet::load`, `McpSet::summary` and the `servers`/`trust` fields.
/// Verify: `cargo test -p loongagent` (no dedicated test; the command is inspection only).
/// Show which external tools would join the session.
pub(crate) fn mcp_list(args: Args) -> Result<(), String> {
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
