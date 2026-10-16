use super::*;

/// Show what would run, and whether it is trusted to.
pub(crate) fn hooks_list(args: Args) -> Result<(), String> {
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
pub(crate) fn hooks_trust(args: Args) -> Result<(), String> {
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
