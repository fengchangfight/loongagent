use super::*;

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
