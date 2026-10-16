use super::*;

/// Show what a `/name` in this workspace resolves to.
pub(crate) fn skills_list(args: Args) -> Result<(), String> {
    // Honour the layered configuration: what this prints must be what a session
    // would actually get, or it is worse than not printing it. Installed plugins are
    // part of that answer (ADR-0018) — they are extra skill roots, nothing more.
    let (settings, pack) = args.settings_with_pack()?;
    if let Some(state) = &pack {
        eprintln!(
            "[control] {} {} — profile: {}",
            state.provenance.issuer,
            state.provenance.version,
            state.summary()
        );
    }
    let plugins = installed_plugins(&args.cwd)?;
    let set = load_catalogue(&args.cwd, &settings, &plugins)?;
    for name in &set.disabled {
        println!("disabled by configuration: /{name}");
    }
    println!("workspace skills and commands (an MCP server's prompts join the");
    println!("catalogue inside a session, namespaced as mcp__<server>__<prompt>):");
    if !plugins.is_empty() {
        let names: Vec<&str> = plugins.plugins.iter().map(|p| p.name()).collect();
        println!(
            "plus {} installed plugin(s): {}",
            names.len(),
            names.join(", ")
        );
    }
    println!();
    for line in set.catalogue().lines() {
        println!("  {line}");
    }
    println!();
    for skill in &set.skills {
        println!(
            "  /{:<22} {:>5} tokens  {}",
            skill.name,
            skill.tokens(),
            skill.source_label()
        );
    }
    Ok(())
}

// -------------------------------------------------------------------- plugins

/// What the installed plugins contribute, or a loud error naming the manifest.
///
/// This runs on the way to every session (`build_kernel`), so a plugin manifest that
/// cannot be read stops the session instead of quietly contributing no skills. The
/// fix is one command, and the error says which one.
pub(crate) fn installed_plugins(cwd: &Path) -> Result<loong_plugin::Installed, String> {
    loong_plugin::discover(&loong_plugin::Roots::for_workspace(cwd)).map_err(|e| e.to_string())
}

/// Load the catalogue exactly as a session will: the workspace's own skills, the
/// configuration's extra roots, and the installed plugins' (ADR-0018).
pub(crate) fn load_catalogue(
    cwd: &Path,
    settings: &loong_config::Profile,
    plugins: &loong_plugin::Installed,
) -> Result<loong_skills::SkillSet, String> {
    let mut dirs = settings.skill_dirs.clone();
    dirs.extend(plugins.skill_dirs());
    loong_skills::SkillSet::load_in(cwd, &dirs, &settings.disable_skills).map_err(|e| e.to_string())
}

/// The skills one plugin contributes, in catalogue order.
fn plugin_skills<'a>(
    set: &'a loong_skills::SkillSet,
    plugin: &loong_plugin::Plugin,
) -> Vec<&'a loong_skills::Skill> {
    let dir = plugin.skills_dir();
    set.skills
        .iter()
        .filter(|skill| {
            matches!(skill.origin, loong_skills::Origin::File { .. })
                && Path::new(&skill.source_label()).starts_with(&dir)
        })
        .collect()
}

/// `plugin add <spec>`: fetch it, prove the catalogue still loads, and say what
/// entered every session's context.
pub(crate) fn plugin_add(args: Args) -> Result<(), String> {
    let spec = args.positional.first().cloned().ok_or_else(|| {
        format!("which plugin? `loongagent plugin add superpowers` is the one-word case\n\n{USAGE}")
    })?;
    let roots = loong_plugin::Roots::for_workspace(&args.cwd);
    let (settings, _pack) = args.settings_with_pack()?;
    // The catalogue has to be sound *before* the install, or "did this break it?" has
    // no answer and a rollback could take away something that was already broken.
    let before = installed_plugins(&args.cwd)?;
    load_catalogue(&args.cwd, &settings, &before).map_err(|e| {
        format!("the catalogue is already broken, so this install cannot be judged: {e}")
    })?;

    let mut opts = loong_plugin::InstallOptions::new(&args.cwd);
    opts.roots = roots.clone();
    opts.name = args.name.clone();
    opts.git_ref = args.git_ref.clone();
    opts.scope = plugin_scope(&args)?.unwrap_or(loong_plugin::Scope::User);
    opts.skills = args.skills.clone();
    opts.bootstrap = (!args.bootstrap.is_empty()).then(|| args.bootstrap.clone());
    opts.force = args.force;
    eprintln!("fetching {spec} …");
    let plugin = loong_plugin::install(&spec, &opts).map_err(|e| e.to_string())?;

    // What a session will actually see, checked now rather than discovered at the
    // next session start: an install that breaks the catalogue is rolled back.
    let after = installed_plugins(&args.cwd)?;
    let set = match load_catalogue(&args.cwd, &settings, &after) {
        Ok(set) => set,
        Err(e) => {
            // The scope it was just installed into, so a rollback cannot take away a
            // same-named plugin in the other one.
            let undone = loong_plugin::remove(&roots, plugin.name(), Some(opts.scope));
            return Err(match undone {
                Ok(_) => format!("{e}\n(nothing was installed: {spec} was rolled back)"),
                Err(undone) => format!(
                    "{e}\n(and rolling it back failed too: {undone}; remove {} by hand)",
                    plugin.root.display()
                ),
            });
        }
    };
    describe_plugin(&plugin, &set, &after);
    Ok(())
}

/// The summary `plugin add` and `plugin list` both owe a person: what was added, how
/// it was fetched, what it costs in every session, and how to undo it.
fn describe_plugin(
    plugin: &loong_plugin::Plugin,
    set: &loong_skills::SkillSet,
    all: &loong_plugin::Installed,
) {
    let skills = plugin_skills(set, plugin);
    let tokens: u64 = skills.iter().map(|s| s.tokens()).sum();
    let version = plugin
        .manifest
        .version
        .as_deref()
        .map(|v| format!(" {v}"))
        .unwrap_or_default();
    println!();
    println!("{}{} — {}", plugin.name(), version, plugin.source_label());
    if let Some(description) = &plugin.manifest.description {
        println!("  {description}");
    }
    println!("  installed: {}", plugin.root.display());
    println!(
        "  skills:    {} ({tokens} tokens when loaded)  {}",
        skills.len(),
        plugin.skills_dir().display()
    );
    for skill in &skills {
        let mark = if plugin.manifest.bootstrap.contains(&skill.name) {
            "  (always on)"
        } else {
            ""
        };
        println!("    /{:<24} {:>5} tokens{mark}", skill.name, skill.tokens());
    }
    if !plugin.manifest.bootstrap.is_empty() {
        println!(
            "  always on: {} — seated in full in every session's system prompt",
            plugin.manifest.bootstrap.join(", ")
        );
    }
    // The subset of a plugin this system refuses to install, said out loud rather than
    // discovered later by wondering why a hook never fired.
    if plugin.root.join("hooks").is_dir() {
        println!(
            "  note:      this plugin ships hooks/, which loongagent does not install or run — \
             a hook is a per-repository trust decision (`hooks trust`), never a side effect of \
             installing content"
        );
    }
    let catalogue_count = set
        .skills
        .iter()
        .filter(|s| !matches!(s.origin, loong_skills::Origin::Builtin { .. }))
        .count();
    println!(
        "  catalogue: {catalogue_count} entries, {} tokens in the system prompt",
        loong_skills::estimate_tokens(&set.catalogue())
    );
    if all.plugins.len() > 1 {
        println!("  installed plugins now: {}", all.plugins.len());
    }
    println!(
        "  undo:      `loongagent plugin remove {}`; switch one skill off with \
         disable_skills = [\"name\"]",
        plugin.name()
    );
}

/// `plugin list`: what is installed, from where, and what it puts in context.
pub(crate) fn plugin_list(args: Args) -> Result<(), String> {
    let roots = loong_plugin::Roots::for_workspace(&args.cwd);
    let installed = installed_plugins(&args.cwd)?;
    println!("user root:    {}", roots.user.display());
    println!("project root: {}", roots.project.display());
    if installed.is_empty() {
        println!();
        println!("no plugins installed. The known ones:");
        for known in loong_plugin::catalog() {
            println!("  {} — {}", known.name, known.what);
        }
        println!();
        println!("  install one with `loongagent plugin add <name>`; `plugin add owner/repo`,");
        println!("  a git URL, or a directory holding skills all work too.");
        return Ok(());
    }
    let (settings, _pack) = args.settings_with_pack()?;
    let set = load_catalogue(&args.cwd, &settings, &installed)?;
    for plugin in &installed.plugins {
        let skills = plugin_skills(&set, plugin);
        let tokens: u64 = skills.iter().map(|s| s.tokens()).sum();
        let version = plugin
            .manifest
            .version
            .as_deref()
            .map(|v| format!(" {v}"))
            .unwrap_or_default();
        println!();
        println!(
            "{}{}  [{}]  {}  {}",
            plugin.name(),
            version,
            plugin.scope(&roots),
            loong_plugin::utc_date(plugin.manifest.installed_at_ms),
            plugin.source_label()
        );
        println!(
            "  {} skills, {tokens} tokens when loaded  {}",
            skills.len(),
            plugin.skills_dir().display()
        );
        if !plugin.manifest.bootstrap.is_empty() {
            println!("  always on: {}", plugin.manifest.bootstrap.join(", "));
        }
        if plugin.root.join("hooks").is_dir() {
            println!("  ships hooks/ (not installed or run)");
        }
    }
    if !installed.shadowed.is_empty() {
        println!();
        println!(
            "shadowed by a project install of the same name: {}",
            installed.shadowed.join(", ")
        );
    }
    Ok(())
}

/// `plugin update <name>`: fetch the latest commit for what is already installed.
pub(crate) fn plugin_update(args: Args) -> Result<(), String> {
    let name = plugin_name(&args, "update")?;
    let roots = loong_plugin::Roots::for_workspace(&args.cwd);
    let plugin =
        loong_plugin::update(&roots, &name, plugin_scope(&args)?).map_err(|e| e.to_string())?;
    println!("updated {name} — {}", plugin.source_label());
    // `update` returns only once the fetched commit's skills have parsed and, if they
    // did not, has already put the checkout back on the commit that did. This second
    // load is the whole catalogue, which is where a collision with another source shows.
    let (settings, _pack) = args.settings_with_pack()?;
    let installed = installed_plugins(&args.cwd)?;
    let set = load_catalogue(&args.cwd, &settings, &installed)?;
    describe_plugin(&plugin, &set, &installed);
    Ok(())
}

/// `plugin remove <name>`: delete the checkout, and with it the skills.
pub(crate) fn plugin_remove(args: Args) -> Result<(), String> {
    let name = plugin_name(&args, "remove")?;
    let roots = loong_plugin::Roots::for_workspace(&args.cwd);
    let dir =
        loong_plugin::remove(&roots, &name, plugin_scope(&args)?).map_err(|e| e.to_string())?;
    println!("removed {name} ({})", dir.display());
    println!("its skills are gone from the next session's catalogue");
    Ok(())
}

/// Which copy of a plugin is meant, when both scopes hold the same name.
///
/// `None` is "the name alone": the one scope that has it when only one does, and a
/// refusal naming both when it does not. A project install shadows a user install
/// (ADR-0018 §1), and acting on the shadow without being asked is how the hidden copy
/// becomes impossible to reach — `plugin list` reports it, so there has to be a way to
/// name it.
fn plugin_scope(args: &Args) -> Result<Option<loong_plugin::Scope>, String> {
    match (args.project, args.user) {
        (true, true) => Err("--project and --user are opposites; pass one".to_string()),
        (true, false) => Ok(Some(loong_plugin::Scope::Project)),
        (false, true) => Ok(Some(loong_plugin::Scope::User)),
        (false, false) => Ok(None),
    }
}

fn plugin_name(args: &Args, verb: &str) -> Result<String, String> {
    args.positional
        .first()
        .cloned()
        .ok_or_else(|| format!("plugin {verb} needs a plugin name\n\n{USAGE}"))
}
