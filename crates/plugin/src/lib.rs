//! Purpose: Plugins: install a reviewed checkout's skills by name, and never its hooks, servers or code.
//!
//! # Contract
//! - **Serves:** the session's skill catalogue (ADR-0018) and the CLI's `plugin add/list/update/remove` and `skills list`.
//! - **Design:** `docs/design/extension-model.md` §4 and §7; `docs/design/security-trust.md` §2.
//! - **Upstream:** `crates/cli/src/plugin.rs` (every `plugin` command) and `crates/cli/src/server.rs`, whose kernel build reaches `Installed::skill_dirs` through `installed_plugins`.
//! - **Downstream:** `loong_skills::SkillSet::load_in` for validation and the catalogue, `loong_config` for the two plugin roots, and `git` as a child process.
//! - **Verify:** `cargo test -p loong-plugin` · `./scripts/check.sh`
//!
//! Plugins: a bundle of skills you install **by name**, instead of hand-copying
//! markdown into every workspace.
//!
//! The ecosystem this borrows from (Claude Code's plugin marketplaces) bundles
//! four things: skills, hooks, subagents and MCP servers. loongagent installs
//! **only the skills**, and that is a decision rather than an unfinished edge:
//!
//! * skills are content, and the skills loader already treats their descriptions
//!   as an injection surface with an audit trail (`skills.loaded`, `skill.invoked`
//!   naming the file they came from);
//! * hooks and MCP servers *execute*. Both sit behind an explicit per-repository
//!   trust gate (`loongagent hooks trust`, `loongagent mcp trust`) precisely so
//!   that installing content can never start running someone's code. A plugin
//!   install must not be a way around that, so it installs no hooks and no
//!   servers. A plugin that ships them is still visible — the checkout is a
//!   directory you can read — but nothing in it runs;
//! * the one thing a plugin genuinely needs from the hook layer — "load this
//!   instruction at the start of every session" — is served natively by
//!   [`Manifest::bootstrap`], which the kernel resolves through the ordinary
//!   skills path.
//!
//! An installed plugin is a directory under a plugin root with a manifest inside
//! it:
//!
//! ```text
//! ~/.loongagent/plugins/superpowers/            # a git checkout
//! ~/.loongagent/plugins/superpowers/.loongagent-plugin.toml
//! <workspace>/.loongagent/plugins/<name>/       # the same, per project
//! ```
//!
//! Two roots, the same shape as configuration layers: a **project** plugin beats
//! a **user** one of the same name, and the one it hid is reported rather than
//! silently forgotten. A directory without a manifest is not a plugin: cloning a
//! repository into the plugin root is not an install, and `plugin add` is the act
//! that puts skills into every session's context. That is the whole trust story
//! here, and it is deliberately the same one `.loongagent/skills` already has.

use loong_skills::{Origin, SkillSet};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The manifest this crate writes and reads. Namespaced with a leading dot so it
/// cannot collide with a `plugin.toml` a repository ships for some other tool.
pub const MANIFEST_FILE: &str = ".loongagent-plugin.toml";
/// The directory under a configuration root that holds installed plugins.
pub const PLUGINS_DIR: &str = "plugins";
/// The Claude Code convention: a repository's skills live under `skills/`.
pub const SKILLS_DIR: &str = "skills";

/// Purpose: Why a plugin could not be read, validated or fetched.
/// Serves: `install`, `update`, `read_manifest`, `resolve_spec` and the CLI's `plugin` command.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `read_manifest`, `install`, `update` and `put_back` construct it; the source resolvers raise `Invalid`.
/// Down: its `Display` impl and `crates/cli/src/plugin.rs`, which turns it into a `String`.
/// Verify: `cargo test -p loong-plugin`.
#[derive(Debug)]
pub enum PluginError {
    Io(String),
    Invalid(String),
    /// Talking to the world: `git`, or a directory used as a source.
    Fetch(String),
}

impl std::fmt::Display for PluginError {
    /// Purpose: Render a plugin failure, marking validation errors with a `plugin:` prefix.
    /// Contract: the `Display` half of `PluginError`; proven by `cargo test -p loong-plugin`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PluginError::Io(m) => write!(f, "{m}"),
            PluginError::Invalid(m) => write!(f, "plugin: {m}"),
            PluginError::Fetch(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for PluginError {}

/// Purpose: This crate's short result type: a `PluginError` on every failure path.
/// Serves: every fallible function in the file.
/// Design: `docs/design/extension-model.md` §4.
/// Up: no code constructs it by name; it appears as the written return type below.
/// Down: `std::result::Result`.
/// Verify: `cargo test -p loong-plugin`.
type Result<T> = std::result::Result<T, PluginError>;

/// Purpose: The two install layers: user-wide and project-local.
/// Serves: `Roots`, `InstallOptions`, `find_dir` and the CLI's `--user`/`--project` flags.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `InstallOptions::new`, `find_dir` and `crates/cli/src/plugin.rs` build it.
/// Down: `Roots::root` and `as_str`.
/// Verify: `cargo test -p loong-plugin`.
/// Where a plugin is installed: the two layers, most local last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// `~/.loongagent/plugins` (or `$LOONGAGENT_HOME/plugins`): every project.
    User,
    /// `<workspace>/.loongagent/plugins`: this project only.
    Project,
}

impl Scope {
    /// Purpose: The lower-case word the CLI prints for a scope.
    /// Serves: `find_dir`'s ambiguity message and `crates/cli/src/plugin.rs`.
    /// Design: `docs/design/extension-model.md` §4.
    /// Up: `Plugin::scope` and `find_dir`.
    /// Down: no call; it returns a static label.
    /// Verify: `cargo test -p loong-plugin`.
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::User => "user",
            Scope::Project => "project",
        }
    }

    /// Purpose: Both scopes, least local first, so a project install replaces a user one.
    /// Serves: `discover` and `find_dir`.
    /// Design: `docs/design/extension-model.md` §4.
    /// Up: `discover` and `find_dir`.
    /// Down: the returned array, whose entries reach `Roots::root`.
    /// Verify: `cargo test -p loong-plugin`.
    /// Least local first, so a later scope replaces an earlier one of the same name.
    pub fn all() -> [Scope; 2] {
        [Scope::User, Scope::Project]
    }
}

/// Purpose: The user and project plugin roots, injected rather than read at each use.
/// Serves: `discover`, `install`, `remove`, `update` and the CLI's plugin commands.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `Roots::for_workspace` in `crates/cli/src/plugin.rs`, and the test helper `roots`.
/// Down: `root` and every filesystem scan in this file.
/// Verify: `cargo test -p loong-plugin`.
/// The two directories plugins are installed under.
///
/// Explicit rather than read from the environment at each use: a test (or a
/// scratch install) can point both at temporary directories and never touch the
/// person's own `~/.loongagent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roots {
    pub user: PathBuf,
    pub project: PathBuf,
}

impl Roots {
    /// Purpose: The real roots for a workspace: the user plugin directory and `<cwd>/.loongagent/plugins`.
    /// Serves: `crates/cli/src/plugin.rs` and `InstallOptions::new`.
    /// Design: `docs/design/extension-model.md` §4.
    /// Up: `crates/cli/src/plugin.rs` and `InstallOptions::new`.
    /// Down: `loong_config::loongagent_home`.
    /// Verify: `cargo test -p loong-plugin`.
    pub fn for_workspace(cwd: &Path) -> Self {
        Self {
            user: loong_config::loongagent_home().join(PLUGINS_DIR),
            project: cwd.join(".loongagent").join(PLUGINS_DIR),
        }
    }

    /// Purpose: The directory a given scope installs under.
    /// Serves: `discover`, `install`, `find_dir` and `look_in`.
    /// Design: `docs/design/extension-model.md` §4.
    /// Up: `discover`, `install`, `find_dir` and `look_in`.
    /// Down: the `user`/`project` fields; no further call.
    /// Verify: `cargo test -p loong-plugin`.
    pub fn root(&self, scope: Scope) -> &Path {
        match scope {
            Scope::User => &self.user,
            Scope::Project => &self.project,
        }
    }
}

/// Purpose: One catalogue entry: the name, its source, its skills directory and its bootstrap list.
/// Serves: `catalog`, `resolve_spec` and `install`.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `catalog` builds the static entries; `crates/cli/src/plugin.rs` prints them for `plugin list`.
/// Down: `resolve_spec` and `install` read the fields.
/// Verify: `cargo test -p loong-plugin`.
/// A plugin the binary already knows by name, so `plugin add superpowers` needs
/// no URL. This is a *catalogue*, not a marketplace: one entry per plugin that
/// has been looked at, and nothing is fetched until a person asks for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Known {
    pub name: &'static str,
    /// What the name resolves to: `owner/repo` on GitHub, or any git URL.
    pub source: &'static str,
    /// One line, shown instead of a URL.
    pub what: &'static str,
    /// Where this plugin's skills live inside the checkout.
    pub skills: &'static str,
    /// Skills whose *bodies* belong in the system prompt — see [`Manifest::bootstrap`].
    pub bootstrap: &'static [&'static str],
    /// Harness adaptation for the bootstrap text, for a plugin written for another
    /// harness that names tools or conventions this one does not have.
    pub notes: &'static str,
}

/// Purpose: The reviewed names `plugin add <name>` accepts without a URL.
/// Serves: `resolve_spec`, `install`, `crates/cli/src/plugin.rs` and the tests.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `resolve_spec`, `install`, `crates/cli/src/plugin.rs` and the tests.
/// Down: the returned `Known` entries.
/// Verify: `cargo test -p loong-plugin`.
/// The plugins `plugin add <name>` knows without a URL.
pub fn catalog() -> &'static [Known] {
    &[Known {
        name: "superpowers",
        source: "obra/superpowers",
        what: "the Superpowers skill library: brainstorming, TDD, systematic debugging, \
               writing-plans, code review, verification",
        skills: SKILLS_DIR,
        // This is the half of Superpowers that its SessionStart hook performs on the
        // harnesses that have one: "check for a skill before responding" only holds if
        // the instruction is *in context* from the first turn, rather than waiting
        // behind a description the model may skip.
        bootstrap: &["using-superpowers"],
        notes: "Superpowers writes its skill names as `superpowers:<name>`; here that is the \
                skill `<name>` from the catalogue, loaded with the `skill` tool or invoked by \
                the person as `/<name>`. There is no todo tool: keep a checklist in your reply \
                instead.",
    }]
}

/// Purpose: One installed plugin: its manifest and the checkout directory it lives in.
/// Serves: `discover`, `install`, `update`, `read_manifest` and `crates/cli/src/plugin.rs`.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `read_manifest` and `install` construct it; `discover` collects it.
/// Down: `name`, `manifest_path`, `skills_dir`, `scope` and `source_label`.
/// Verify: `cargo test -p loong-plugin`.
/// One installed plugin: the manifest this crate wrote, plus where it lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plugin {
    pub manifest: Manifest,
    /// The installed directory (the checkout itself), not the manifest file.
    pub root: PathBuf,
}

impl Plugin {
    /// Purpose: The installed name, which is also its directory and its skill namespace.
    /// Serves: `discover`, `find_dir`, `look_in` and `crates/cli/src/plugin.rs`.
    /// Design: `docs/design/extension-model.md` §4.
    /// Up: `discover`, `find_dir`, `look_in`, `put_back` and `crates/cli/src/plugin.rs`.
    /// Down: `Manifest::name`; no further call.
    /// Verify: `cargo test -p loong-plugin`.
    pub fn name(&self) -> &str {
        &self.manifest.name
    }

    /// Purpose: The manifest file inside the checkout.
    /// Serves: `write_manifest` and the tests.
    /// Design: `docs/design/extension-model.md` §4.
    /// Up: `write_manifest` and the tests call it; `read_manifest` and `install` join the file name directly.
    /// Down: `MANIFEST_FILE` joined to the root; no further call.
    /// Verify: `cargo test -p loong-plugin`.
    pub fn manifest_path(&self) -> PathBuf {
        self.root.join(MANIFEST_FILE)
    }

    /// Purpose: The directory inside the checkout that the skills loader is pointed at.
    /// Serves: `Installed::skill_dirs`, `update`, `crates/cli/src/plugin.rs` and the kernel build.
    /// Design: `docs/design/extension-model.md` §4.
    /// Up: `Installed::skill_dirs`, `update` and `crates/cli/src/plugin.rs`.
    /// Down: a join only; `read_manifest` already proved the path stays inside the checkout.
    /// Verify: `cargo test -p loong-plugin`.
    /// The directory the skills loader is pointed at.
    ///
    /// Already checked to be inside the checkout by [`read_manifest`], which is the
    /// only way a [`Plugin`] comes into being.
    pub fn skills_dir(&self) -> PathBuf {
        self.root.join(&self.manifest.skills)
    }

    /// Purpose: Which root this plugin was found under, as `plugin list` prints it.
    /// Serves: `crates/cli/src/plugin.rs` and the shadow test.
    /// Design: `docs/design/extension-model.md` §4.
    /// Up: `crates/cli/src/plugin.rs` and `discovery_reads_installs_and_reports_shadowing`.
    /// Down: `Scope::as_str`.
    /// Verify: `cargo test -p loong-plugin`.
    pub fn scope(&self, roots: &Roots) -> &'static str {
        if self.root.starts_with(&roots.project) {
            Scope::Project.as_str()
        } else {
            Scope::User.as_str()
        }
    }

    /// Purpose: The provenance line `plugin list` shows: source, requested ref and short commit.
    /// Serves: `crates/cli/src/plugin.rs`.
    /// Design: `docs/design/extension-model.md` §4.
    /// Up: `crates/cli/src/plugin.rs` only.
    /// Down: the `Manifest` fields.
    /// Verify: `cargo test -p loong-plugin`.
    /// What `plugin list` shows as where this came from.
    pub fn source_label(&self) -> String {
        let mut label = self.manifest.source.clone();
        if let Some(git_ref) = &self.manifest.git_ref {
            label.push('@');
            label.push_str(git_ref);
        }
        if let Some(commit) = &self.manifest.commit {
            let short: String = commit.chars().take(8).collect();
            label.push_str(&format!(" ({short})"));
        }
        label
    }
}

/// Purpose: The manifest written into a checkout: name, source, commit, skills directory, bootstrap and notes.
/// Serves: `read_manifest`, `write_manifest`, `install` and every `Plugin` consumer.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `read_manifest` parses it; `install` and `update` write it.
/// Down: `Plugin`, `install`, `update` and `crates/cli/src/plugin.rs` read its fields.
/// Verify: `cargo test -p loong-plugin`.
/// The manifest on disk. Every field is written by an install; a person may edit
/// it — that is why it is TOML inside the plugin directory — and `plugin update`
/// rewrites only what it owns.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Manifest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `owner/repo` or a full git URL — what `plugin update` fetches from.
    pub source: String,
    /// The branch or tag that was asked for, when one was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    /// The commit that was checked out, so "which version is installed?" has an answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Where the skills live inside the checkout, relative to it.
    #[serde(default = "default_skills")]
    pub skills: String,
    /// Skills whose bodies are loaded into the system prompt of every session.
    ///
    /// This is the native replacement for the `SessionStart` hook this class of
    /// plugin ships on harnesses that have one: the same instruction, carried by the
    /// loader that already exists, with no third-party script executing.
    #[serde(default)]
    pub bootstrap: Vec<String>,
    /// Extra lines injected with the bootstrap text (harness adaptation).
    #[serde(default)]
    pub notes: Vec<String>,
    pub installed_at_ms: u64,
    /// Set when the source was a plain directory, which is not updatable in place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_from: Option<String>,
}

/// Purpose: Serde's default for `skills`: `.`, a flat checkout.
/// Contract: read by `#[serde(default = "default_skills")]` on `Manifest::skills`.
fn default_skills() -> String {
    ".".to_string()
}

impl Manifest {
    /// Purpose: Render the manifest as TOML with the header a person editing it needs.
    /// Contract: used by `install` and `write_manifest`; a render failure becomes `PluginError::Invalid`.
    fn to_toml(&self) -> Result<String> {
        let body = toml::to_string(self)
            .map_err(|e| PluginError::Invalid(format!("cannot render the manifest: {e}")))?;
        Ok(format!(
            "# Written by `loongagent plugin add`. Meant to be read, and safe to edit:\n\
             # `plugin update` rewrites source/commit/installed_at_ms and leaves the rest.\n\
             {body}"
        ))
    }
}

/// Purpose: Every plugin installed for one workspace and user, plus the names a project install shadowed.
/// Serves: `discover`, `crates/cli/src/plugin.rs` and the kernel build through `skill_dirs`.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `discover` returns it; `crates/cli/src/plugin.rs` holds it while building a session.
/// Down: `skill_dirs`, `bootstrap_skills`, `bootstrap_notes`, `get` and `is_empty`.
/// Verify: `cargo test -p loong-plugin`.
/// Every plugin installed for this workspace and this user.
#[derive(Debug, Clone, Default)]
pub struct Installed {
    pub plugins: Vec<Plugin>,
    /// Names installed at the user layer that a project plugin of the same name
    /// replaced. Reported rather than dropped: "which one am I actually running?"
    /// is a question a silent shadow makes unanswerable.
    pub shadowed: Vec<String>,
}

impl Installed {
    /// Purpose: The skill roots to hand the loader, one per installed plugin.
    /// Serves: `crates/cli/src/server.rs` and `crates/cli/src/plugin.rs`.
    /// Design: `docs/design/extension-model.md` §4.
    /// Up: `crates/cli/src/server.rs`, `crates/cli/src/plugin.rs` and `discovery_reads_installs_and_reports_shadowing`.
    /// Down: `Plugin::skills_dir`.
    /// Verify: `cargo test -p loong-plugin`.
    /// The skill roots to hand the loader, in name order.
    pub fn skill_dirs(&self) -> Vec<PathBuf> {
        self.plugins.iter().map(Plugin::skills_dir).collect()
    }

    /// Purpose: The always-on skill names, de-duplicated in plugin order.
    /// Serves: `crates/cli/src/server.rs`'s kernel build.
    /// Design: `docs/design/extension-model.md` §4.
    /// Up: `crates/cli/src/server.rs` and `discovery_reads_installs_and_reports_shadowing`.
    /// Down: the `Manifest::bootstrap` lists.
    /// Verify: `cargo test -p loong-plugin`.
    /// The skills whose bodies belong in the system prompt, plugin order.
    pub fn bootstrap_skills(&self) -> Vec<String> {
        let mut names = Vec::new();
        for plugin in &self.plugins {
            for name in &plugin.manifest.bootstrap {
                if !names.contains(name) {
                    names.push(name.clone());
                }
            }
        }
        names
    }

    /// Purpose: The harness-adaptation lines that travel with the bootstrap bodies.
    /// Serves: `crates/cli/src/server.rs`'s kernel build.
    /// Design: `docs/design/extension-model.md` §4.
    /// Up: `crates/cli/src/server.rs` and `discovery_reads_installs_and_reports_shadowing`.
    /// Down: the `Manifest::notes` lists.
    /// Verify: `cargo test -p loong-plugin`.
    /// The adaptation lines that travel with those bodies.
    pub fn bootstrap_notes(&self) -> Vec<String> {
        let mut notes = Vec::new();
        for plugin in &self.plugins {
            if plugin.manifest.bootstrap.is_empty() {
                continue;
            }
            for note in &plugin.manifest.notes {
                if !note.is_empty() && !notes.contains(note) {
                    notes.push(note.clone());
                }
            }
        }
        notes
    }

    /// Purpose: One installed plugin by name, when present.
    /// Serves: no production caller; tests look one plugin up by name.
    /// Design: `docs/design/extension-model.md` §4.
    /// Up: no production caller; the tests call it.
    /// Down: the `plugins` vector; no further call.
    /// Verify: `cargo test -p loong-plugin`.
    pub fn get(&self, name: &str) -> Option<&Plugin> {
        self.plugins.iter().find(|p| p.name() == name)
    }

    /// Purpose: Whether anything is installed, for messages and fast paths.
    /// Serves: `crates/cli/src/plugin.rs` and `not_installed`.
    /// Design: `docs/design/extension-model.md` §4.
    /// Up: `crates/cli/src/plugin.rs` and `crates/cli/src/server.rs` both ask it;
    /// `not_installed` does too.
    /// Down: the `plugins` vector; no further call.
    /// Verify: `cargo test -p loong-plugin`.
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }
}

/// Purpose: Read every installed plugin under both roots and report a project shadow.
/// Serves: `crates/cli/src/plugin.rs` and the kernel build.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `crates/cli/src/plugin.rs` and the tests.
/// Down: `installed_dirs` and `read_manifest`; `not_installed` calls back into it.
/// Verify: `cargo test -p loong-plugin`.
/// Read every installed plugin under both roots. A manifest that cannot be read is
/// an error, not a silent skip: skills that are not there while the person believes
/// they are is the failure mode worth being loud about.
pub fn discover(roots: &Roots) -> Result<Installed> {
    let mut by_name: BTreeMap<String, Plugin> = BTreeMap::new();
    let mut shadowed = Vec::new();
    for scope in Scope::all() {
        for dir in installed_dirs(roots.root(scope))? {
            let plugin = read_manifest(&dir)?;
            let name = plugin.name().to_string();
            if by_name.insert(name.clone(), plugin).is_some() {
                shadowed.push(name);
            }
        }
    }
    Ok(Installed {
        plugins: by_name.into_values().collect(),
        shadowed,
    })
}

/// Purpose: The directories under a root that carry a manifest, in name order.
/// Serves: `discover` and `look_in`.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `discover` and `look_in`.
/// Down: `MANIFEST_FILE` for the filter; no further call.
/// Verify: `cargo test -p loong-plugin`.
/// The directories under a plugin root that carry a manifest, in name order.
fn installed_dirs(root: &Path) -> Result<Vec<PathBuf>> {
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
        .map_err(|e| PluginError::Io(format!("{}: {e}", root.display())))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.join(MANIFEST_FILE).is_file())
        .collect();
    dirs.sort();
    Ok(dirs)
}

/// Purpose: Read one manifest and refuse a skills directory that leaves the checkout.
/// Serves: `discover`, `install`, `update` and `look_in`.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `discover`, `install`, `update`, `look_in` and the tests.
/// Down: `resolve_inside`, the TOML parse and the filesystem read.
/// Verify: `cargo test -p loong-plugin`.
/// Read one installed plugin's manifest.
pub fn read_manifest(dir: &Path) -> Result<Plugin> {
    let path = dir.join(MANIFEST_FILE);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| PluginError::Io(format!("{}: {e}", path.display())))?;
    let manifest: Manifest = toml::from_str(&text)
        .map_err(|e| PluginError::Invalid(format!("{}: {e}", path.display())))?;
    let plugin = Plugin {
        manifest,
        root: dir.to_path_buf(),
    };
    // An install that pointed the loader at nothing would contribute no skills while
    // `plugin list` claimed otherwise.
    let skills = resolve_inside(&plugin.root, &plugin.manifest.skills)?;
    if !skills.is_dir() {
        return Err(PluginError::Invalid(format!(
            "{}: skills = {:?}, but {} is not a directory — reinstall it, or fix the manifest",
            path.display(),
            plugin.manifest.skills,
            skills.display()
        )));
    }
    Ok(plugin)
}

/// Purpose: Join a manifest's skills path and refuse anything that escapes the checkout.
/// Contract: `read_manifest` and `install` are the two callers; proven by `cargo test -p loong-plugin`.
/// The directory a manifest names, refused unless it lands inside the checkout.
///
/// `Path::join` hands back an absolute path unchanged and `..` walks up, so a manifest
/// saying either of those points the skills loader at a directory this plugin does not
/// own — and the manifest is a file a person is invited to edit. No install produces
/// such a path (the three sources are `--skills`, the catalogue, and what the checkout
/// shows), so it is refused where the manifest is read, not trusted and acted on later.
fn resolve_inside(root: &Path, relative: &str) -> Result<PathBuf> {
    let escapes = Path::new(relative)
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir));
    if escapes {
        return Err(PluginError::Invalid(format!(
            "skills = {relative:?} does not name a directory inside the checkout — it has to be \
             a relative path under the plugin's own directory (`.`, `skills`, `content/skills`)"
        )));
    }
    Ok(root.join(relative))
}

/// Purpose: What a spec string turned out to be: a repository or a plain directory.
/// Serves: `resolve_spec`, `install` and the tests.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `resolve_spec` returns it; `install` matches on it.
/// Down: `name` and `git_ref`.
/// Verify: `cargo test -p loong-plugin`.
/// What a spec string turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A git repository, local or remote.
    Git {
        url: String,
        name: String,
        git_ref: Option<String>,
    },
    /// A plain directory: copied, and not updatable in place.
    Dir { path: PathBuf, name: String },
}

impl Source {
    /// Purpose: The directory name the source installs under.
    /// Serves: `install` and the tests.
    /// Design: `docs/design/extension-model.md` §4.
    /// Up: `install` and `every_spec_shape_resolves_the_way_a_person_would_expect`.
    /// Down: the variant's `name` field; no further call.
    /// Verify: `cargo test -p loong-plugin`.
    pub fn name(&self) -> &str {
        match self {
            Source::Git { name, .. } | Source::Dir { name, .. } => name,
        }
    }

    /// Purpose: The branch or tag asked for, when the source is a repository.
    /// Serves: `install`.
    /// Design: `docs/design/extension-model.md` §4.
    /// Up: `install`.
    /// Down: the `Git` variant; no further call.
    /// Verify: `cargo test -p loong-plugin`.
    pub fn git_ref(&self) -> Option<&str> {
        match self {
            Source::Git { git_ref, .. } => git_ref.as_deref(),
            Source::Dir { .. } => None,
        }
    }
}

/// Purpose: Turn a `plugin add` argument into a git source or a directory.
/// Serves: `install` and the tests.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `install` and the tests.
/// Down: `catalog`, `split_ref`, `existing_dir` and `git_url`.
/// Verify: `cargo test -p loong-plugin`.
/// What `plugin add <spec>` accepts, and the one place that grammar lives.
///
/// * `superpowers` — a name from [`catalog`], so the common case needs no URL;
/// * `obra/superpowers`, `github.com/obra/superpowers`, `https://…`, `git@…`,
///   `ssh://…` — a repository, cloned shallowly;
/// * `/path/to/skills`, `./relative`, `~/.dotfiles/skills` — a directory;
/// * any of the above with `@branch-or-tag` appended.
pub fn resolve_spec(spec: &str, cwd: &Path) -> Result<Source> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err(PluginError::Invalid(
            "which plugin? try `plugin add superpowers`, `plugin add owner/repo`, or the path \
             of a directory holding skills"
                .to_string(),
        ));
    }
    let (base, git_ref) = split_ref(spec);

    // A known name means the catalogue, always: `plugin add superpowers` must not
    // depend on what happens to be in this directory. Spell a local directory
    // `./superpowers` to mean the directory.
    if let Some(known) = catalog().iter().find(|k| k.name == base) {
        return Ok(Source::Git {
            url: git_url(known.source),
            name: known.name.to_string(),
            git_ref,
        });
    }

    // A directory wins over the URL heuristics: `./plugin` and `~/.dotfiles/skills`
    // are paths even though they contain a slash.
    if let Some(path) = existing_dir(spec, cwd).or_else(|| existing_dir(base, cwd)) {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "plugin".to_string());
        return Ok(Source::Dir { path, name });
    }

    // Anything that already names a scheme, a host, or the `owner/repo` shorthand is
    // a repository; one function turns every one of those into a URL `git` accepts.
    let url = git_url(base);
    if url != base || base.contains("://") || base.starts_with("git@") {
        return Ok(Source::Git {
            name: repo_name(&url),
            url,
            git_ref,
        });
    }

    Err(PluginError::Invalid(format!(
        "cannot tell what {spec:?} is: not one of the known plugins ({}), not `owner/repo`, \
         not a URL, and no such directory",
        catalog()
            .iter()
            .map(|k| k.name)
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

/// Purpose: Expand `owner/repo`, a host-qualified path or a scheme into a URL git accepts.
/// Contract: called by `resolve_spec`; an unchanged answer means "not a repository".
/// The URL `git` should be given for what a person typed.
///
/// `obra/superpowers` is GitHub shorthand; `gitlab.example.com/team/skills` names its
/// own host; anything with a scheme is already a URL. Returns the input unchanged when
/// it is none of those, so the caller can use "did this change?" as the test.
fn git_url(spec: &str) -> String {
    if spec.contains("://") || spec.starts_with("git@") {
        return spec.to_string();
    }
    let segments: Vec<&str> = spec.split('/').filter(|s| !s.is_empty()).collect();
    match segments.len() {
        2 if !segments[0].contains('.') => format!("https://github.com/{spec}"),
        n if n >= 3 && segments[0].contains('.') => format!("https://{spec}"),
        _ => spec.to_string(),
    }
}

/// Purpose: The canonical directory a spec names, when one exists.
/// Serves: `resolve_spec`.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `resolve_spec`.
/// Down: `expand_home` and `Path::canonicalize`.
/// Verify: `cargo test -p loong-plugin`.
/// The directory a spec names, when it names one that exists.
fn existing_dir(spec: &str, cwd: &Path) -> Option<PathBuf> {
    if spec.is_empty() || spec.contains("://") {
        return None;
    }
    let candidate = expand_home(spec);
    let candidate = if candidate.is_absolute() {
        candidate
    } else {
        cwd.join(candidate)
    };
    let resolved = candidate.canonicalize().ok()?;
    resolved.is_dir().then_some(resolved)
}

/// Purpose: Split a trailing `@ref` off a spec without mangling `git@host:path`.
/// Serves: `resolve_spec`.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `resolve_spec`.
/// Down: no call; it returns the pair.
/// Verify: `cargo test -p loong-plugin`.
/// `owner/repo@v1.2` → (`owner/repo`, Some("v1.2")). The `@` in `git@host:path` and
/// in `https://user@host/…` is not a ref separator: after the first, the rest of the
/// URL still contains a `/`, and a scp-like `user@host:path` never has one at all.
fn split_ref(spec: &str) -> (&str, Option<String>) {
    let scp_like = match spec.split_once('@') {
        Some((user, rest)) => !user.is_empty() && !user.contains('/') && rest.contains(':'),
        None => false,
    };
    if scp_like {
        return (spec, None);
    }
    match spec.rsplit_once('@') {
        Some((base, reference))
            if !base.is_empty()
                && !reference.is_empty()
                && !reference.contains('/')
                && !reference.contains(':') =>
        {
            (base, Some(reference.to_string()))
        }
        _ => (spec, None),
    }
}

/// Purpose: The last path segment minus `.git`: the directory a clone is called.
/// Contract: called by `resolve_spec` when it builds a `Source::Git`.
/// The last path segment, minus `.git`: what a clone of this URL is called.
fn repo_name(url: &str) -> String {
    let trimmed = url.trim_end_matches('/').trim_end_matches(".git");
    trimmed
        .rsplit(['/', ':'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("plugin")
        .to_string()
}

/// Purpose: Expand a leading `~/` using the configured home directory.
/// Contract: called by `existing_dir`.
fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = loong_config::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

/// Purpose: How one `plugin add` should behave: roots, scope, name, ref, skills, bootstrap and force.
/// Serves: `install` and `crates/cli/src/plugin.rs`.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `InstallOptions::new` and `crates/cli/src/plugin.rs`.
/// Down: `install` reads every field.
/// Verify: `cargo test -p loong-plugin`.
/// How an install should behave.
#[derive(Debug, Clone)]
pub struct InstallOptions {
    /// The workspace the command was run in: where a relative spec is resolved.
    pub cwd: PathBuf,
    /// The two plugin directories. Overridable so a test or a scratch install
    /// never touches a real `~/.loongagent`.
    pub roots: Roots,
    /// Override the name (the directory under the plugin root).
    pub name: Option<String>,
    /// Override the branch or tag in the spec.
    pub git_ref: Option<String>,
    pub scope: Scope,
    /// Where the skills live inside the checkout; detected when `None`.
    pub skills: Option<String>,
    /// Bootstrap skills; the catalogue's answer when `None`.
    pub bootstrap: Option<Vec<String>>,
    /// Replace an existing install of the same name.
    pub force: bool,
}

impl InstallOptions {
    /// Purpose: The defaults for one install: real roots, user scope, no overrides and no force.
    /// Serves: `crates/cli/src/plugin.rs` and the test helper `options`.
    /// Design: `docs/design/extension-model.md` §4.
    /// Up: `crates/cli/src/plugin.rs` and the tests.
    /// Down: `Roots::for_workspace`.
    /// Verify: `cargo test -p loong-plugin`.
    pub fn new(cwd: impl AsRef<Path>) -> Self {
        Self {
            cwd: cwd.as_ref().to_path_buf(),
            roots: Roots::for_workspace(cwd.as_ref()),
            name: None,
            git_ref: None,
            scope: Scope::User,
            skills: None,
            bootstrap: None,
            force: false,
        }
    }
}

/// Purpose: Fetch, validate, manifest and atomically place one plugin.
/// Serves: `crates/cli/src/plugin.rs`.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `crates/cli/src/plugin.rs` and the tests.
/// Down: `resolve_spec`, `catalog`, `read_manifest`, `skill_names`, `clone_into`, `copy_tree` and `read_tool_manifest`.
/// Verify: `cargo test -p loong-plugin`.
/// Fetch a plugin and write its manifest. Returns the installed plugin, whose
/// skills have already been parsed once: an install that would break every session
/// fails here, not in the next session.
pub fn install(spec: &str, opts: &InstallOptions) -> Result<Plugin> {
    let source = resolve_spec(spec, &opts.cwd)?;
    // The catalogue's answer applies to anything that turns out to be a known
    // plugin, however it was named: `plugin add obra/superpowers` and
    // `plugin add /checkouts/superpowers` both get the bootstrap.
    let known = catalog().iter().find(|k| k.name == source.name()).copied();
    let name = opts
        .name
        .clone()
        .or_else(|| known.map(|k| k.name.to_string()))
        .unwrap_or_else(|| source.name().to_string());
    if !is_usable_name(&name) {
        return Err(PluginError::Invalid(format!(
            "{name:?} is not a usable name (letters, digits, - and _ only); pass --name"
        )));
    }
    let git_ref = opts
        .git_ref
        .clone()
        .or_else(|| source.git_ref().map(|r| r.to_string()));

    let root = opts.roots.root(opts.scope).to_path_buf();
    std::fs::create_dir_all(&root)
        .map_err(|e| PluginError::Io(format!("{}: {e}", root.display())))?;
    let dest = root.join(&name);
    if dest.exists() {
        if !opts.force {
            return Err(PluginError::Invalid(format!(
                "{} is already installed at {} — `plugin update {name}` to refresh it, or \
                 `plugin add --force` to replace it",
                name,
                dest.display()
            )));
        }
        std::fs::remove_dir_all(&dest)
            .map_err(|e| PluginError::Io(format!("{}: {e}", dest.display())))?;
    }

    // Fetch into a staging directory and rename: a clone that fails halfway leaves
    // no half-installed plugin for the next session to trip over.
    let staging = root.join(format!(".{name}.staging-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    let fetched = match &source {
        Source::Git { url, .. } => clone_into(url, git_ref.as_deref(), &staging),
        Source::Dir { path, .. } => {
            if path.join(".git").is_dir() {
                clone_into(&path.to_string_lossy(), None, &staging)
            } else {
                copy_tree(path, &staging)
            }
        }
    };
    if let Err(e) = fetched {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }

    let commit = if staging.join(".git").is_dir() {
        git_in(&staging, &["rev-parse", "HEAD"]).ok()
    } else {
        None
    };
    let skills = match &opts.skills {
        Some(explicit) => explicit.clone(),
        // What the catalogue says first (a known plugin may keep its skills
        // somewhere other than `skills/`), then what the checkout shows.
        None => match known
            .map(|k| k.skills)
            .filter(|d| holds_skills(&staging.join(d)))
        {
            Some(from_catalog) => from_catalog.to_string(),
            None => match detect_skills(&staging) {
                Ok(found) => found,
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&staging);
                    return Err(e);
                }
            },
        },
    };
    let bootstrap = opts
        .bootstrap
        .clone()
        .or_else(|| known.map(|k| k.bootstrap.iter().map(|s| s.to_string()).collect()))
        .unwrap_or_default();
    let notes: Vec<String> = known
        .filter(|k| !bootstrap.is_empty() && !k.notes.is_empty())
        .map(|k| vec![k.notes.to_string()])
        .unwrap_or_default();
    let tool_manifest = read_tool_manifest(&staging);
    // The plugin's own skills are parsed before it is committed to disk, and every
    // skill it declares as always-on has to be one of them. Otherwise the install
    // would succeed and the *next session* would refuse to open — the worst place to
    // discover a manifest that only half makes sense.
    let names = match resolve_inside(&staging, &skills).and_then(|root| skill_names(&root)) {
        Ok(names) => names,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
    };
    if let Some(missing) = bootstrap.iter().find(|b| !names.contains(b)) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(PluginError::Invalid(format!(
            "{missing:?} is declared as a bootstrap skill, but this plugin has no such skill \
             (it has: {}) — `--bootstrap <name>` names a different one, `--skills <dir>` points \
             at the skills if they live somewhere else",
            names.join(", ")
        )));
    }
    let manifest = Manifest {
        name: name.clone(),
        version: tool_manifest.as_ref().and_then(|m| m.version.clone()),
        description: tool_manifest.as_ref().and_then(|m| m.description.clone()),
        source: match &source {
            Source::Git { url, .. } => url.clone(),
            Source::Dir { path, .. } => path.to_string_lossy().to_string(),
        },
        git_ref,
        commit,
        skills,
        bootstrap,
        notes,
        installed_at_ms: now_ms(),
        installed_from: match &source {
            Source::Git { .. } => None,
            Source::Dir { path, .. } => Some(path.to_string_lossy().to_string()),
        },
    };
    let manifest_path = staging.join(MANIFEST_FILE);
    std::fs::write(&manifest_path, manifest.to_toml()?)
        .map_err(|e| PluginError::Io(format!("{}: {e}", manifest_path.display())))?;
    std::fs::rename(&staging, &dest).map_err(|e| {
        PluginError::Io(format!(
            "the plugin is fetched but could not be moved into {}: {e}",
            dest.display()
        ))
    })?;
    read_manifest(&dest)
}

/// Purpose: Parse a skills root the way a session will and return the names it yields.
/// Serves: `install`, `update` and the tests.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `install`, `update` and the tests.
/// Down: `loong_skills::SkillSet::load_in`.
/// Verify: `cargo test -p loong-plugin`.
/// Which skills a directory holds, parsed the way a session will parse them. The
/// names are returned so an install can say what it added.
///
/// The directory is handed to the loader as an *extra root*, which is exactly how a
/// session reaches it (`SkillSet::load_in`), so an install that validates here
/// cannot succeed while the session fails.
pub fn skill_names(skills_root: &Path) -> Result<Vec<String>> {
    let set = SkillSet::load_in(skills_root, &[skills_root.to_path_buf()], &[])
        .map_err(|e| PluginError::Invalid(e.to_string()))?;
    Ok(set
        .skills
        .iter()
        .filter(|s| matches!(s.origin, Origin::File { .. }))
        .map(|s| s.name.clone())
        .collect())
}

/// Purpose: Find the skills directory in a fresh checkout, or refuse with a way forward.
/// Serves: `install`.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `install`.
/// Down: `holds_skills`.
/// Verify: `cargo test -p loong-plugin`.
/// The skills directory inside a fresh checkout, or a refusal that says what to do
/// about it. The Claude Code layout (`skills/<name>/SKILL.md`) is the convention; a
/// directory of skill directories at the root is the fallback.
fn detect_skills(checkout: &Path) -> Result<String> {
    if holds_skills(&checkout.join(SKILLS_DIR)) {
        return Ok(SKILLS_DIR.to_string());
    }
    if holds_skills(checkout) {
        return Ok(".".to_string());
    }
    Err(PluginError::Invalid(format!(
        "{} holds no skills: no `{SKILLS_DIR}/<name>/SKILL.md` and no `<name>/SKILL.md` at the \
         root — pass `--skills <dir>` if they live somewhere else",
        checkout.display()
    )))
}

/// Purpose: A cheap structural probe for "does this look like a skills root?".
/// Serves: `detect_skills` and `install`.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `detect_skills` and `install`.
/// Down: the filesystem only; whether the skills parse is `skill_names`' job.
/// Verify: `cargo test -p loong-plugin`.
/// A cheap structural probe: does this look like a skills root at all? Used only to
/// *find* the root; whether the skills parse is decided by [`skill_names`].
fn holds_skills(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && path.join("SKILL.md").is_file() {
            return true;
        }
        // A flat markdown file counts only when it opens a frontmatter block:
        // otherwise every README would make a directory look like a plugin.
        if path.extension().and_then(|e| e.to_str()) == Some("md") {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if text.trim_start_matches('\u{feff}').starts_with("---") {
                    return true;
                }
            }
        }
    }
    false
}

/// Purpose: The `version` and `description` a Claude Code style plugin.json offers, for display only.
/// Serves: `read_tool_manifest` and `install`.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `read_tool_manifest` builds it; `install` copies the fields into `Manifest`.
/// Down: no further call in this crate.
/// Verify: `cargo test -p loong-plugin`.
/// The plugin's own `version`/`description`, when it ships the manifest shape the
/// Claude Code ecosystem uses. Read for display only: the installed name is the one
/// the person typed, so a repository cannot rename itself into somebody's directory.
struct ToolManifest {
    version: Option<String>,
    description: Option<String>,
}

/// Purpose: Read the first plugin.json a checkout ships, for its version and description.
/// Serves: `install`.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `install`.
/// Down: `serde_json` only.
/// Verify: `cargo test -p loong-plugin`.
fn read_tool_manifest(checkout: &Path) -> Option<ToolManifest> {
    for candidate in [
        ".claude-plugin/plugin.json",
        ".codex-plugin/plugin.json",
        ".muse-plugin/plugin.json",
    ] {
        let path = checkout.join(candidate);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let field = |key: &str| {
            value
                .get(key)
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        };
        return Some(ToolManifest {
            version: field("version"),
            description: field("description"),
        });
    }
    None
}

/// Purpose: Locate one install by name and scope, refusing an ambiguous name.
/// Serves: `remove` and `update`.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `remove` and `update`.
/// Down: `look_in` and `not_installed`.
/// Verify: `cargo test -p loong-plugin`.
/// Find an install by name. The directory name is searched first, so a plugin whose
/// manifest has been broken can still be found — and removed, which is the only way
/// out of that state.
/// Find one installed plugin by name.
///
/// `scope` is `Some(..)` when the person said which copy they meant (`--project` /
/// `--user`), and `None` when they only said the name. A name in one scope is that
/// one; a name in both is a refusal that names both and asks, because a project
/// install shadows a user install (ADR-0018 §1) and quietly acting on the shadow is
/// how the hidden copy becomes impossible to reach.
fn find_dir(roots: &Roots, name: &str, scope: Option<Scope>) -> Result<PathBuf> {
    if let Some(scope) = scope {
        return look_in(&roots.root(scope), name)?.ok_or_else(|| not_installed(roots, name));
    }
    // Most local first, so a project install is the one that is actually running.
    let mut found: Vec<(Scope, PathBuf)> = Vec::new();
    for scope in Scope::all().into_iter().rev() {
        if let Some(dir) = look_in(&roots.root(scope), name)? {
            found.push((scope, dir));
        }
    }
    match found.len() {
        0 => Err(not_installed(roots, name)),
        1 => Ok(found.remove(0).1),
        _ => Err(PluginError::Invalid(format!(
            "{name} is installed twice — {} — so say which one is meant: `--project` for this \
             workspace, `--user` for the one that follows you",
            found
                .iter()
                .map(|(scope, dir)| format!("{}: {}", scope.as_str(), dir.display()))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// Purpose: One root's answer for a name: the directory, or a manifest that names it.
/// Serves: `find_dir`.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `find_dir`.
/// Down: `installed_dirs` and `read_manifest`.
/// Verify: `cargo test -p loong-plugin`.
/// One root's answer for a name: the directory called `name`, or failing that a
/// directory whose manifest says `name` (an install may have been renamed).
fn look_in(root: &Path, name: &str) -> Result<Option<PathBuf>> {
    let dir = root.join(name);
    if dir.join(MANIFEST_FILE).is_file() {
        return Ok(Some(dir));
    }
    for dir in installed_dirs(root)? {
        if let Ok(plugin) = read_manifest(&dir) {
            if plugin.name() == name {
                return Ok(Some(dir));
            }
        }
    }
    Ok(None)
}

/// Purpose: The "no plugin named X" error, listing what is installed.
/// Serves: `find_dir`.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `find_dir`.
/// Down: `discover`.
/// Verify: `cargo test -p loong-plugin`.
fn not_installed(roots: &Roots, name: &str) -> PluginError {
    // A root that cannot be read is its own answer, and a better one than "no plugin
    // named X" — the same thing the scan this replaced would have said.
    let installed = match discover(roots) {
        Ok(installed) => installed,
        Err(e) => return e,
    };
    PluginError::Invalid(format!(
        "no plugin named {name:?} is installed{}",
        if installed.is_empty() {
            String::new()
        } else {
            format!(
                " (installed: {})",
                installed
                    .plugins
                    .iter()
                    .map(|p| p.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    ))
}

/// Purpose: Delete an installed plugin's checkout.
/// Serves: `crates/cli/src/plugin.rs`.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `crates/cli/src/plugin.rs` and the tests.
/// Down: `find_dir` and `std::fs::remove_dir_all`.
/// Verify: `cargo test -p loong-plugin`.
/// Remove an installed plugin. Returns the directory that was deleted.
pub fn remove(roots: &Roots, name: &str, scope: Option<Scope>) -> Result<PathBuf> {
    let dir = find_dir(roots, name, scope)?;
    std::fs::remove_dir_all(&dir)
        .map_err(|e| PluginError::Io(format!("{}: {e}", dir.display())))?;
    Ok(dir)
}

/// Purpose: Fetch the latest commit in place, rolling back when the checkout no longer loads.
/// Serves: `crates/cli/src/plugin.rs`.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `crates/cli/src/plugin.rs` and the tests.
/// Down: `find_dir`, `git_in`, `skill_names`, `put_back` and `write_manifest`.
/// Verify: `cargo test -p loong-plugin`.
/// Fetch the latest commit for an installed plugin, in place.
pub fn update(roots: &Roots, name: &str, scope: Option<Scope>) -> Result<Plugin> {
    let dir = find_dir(roots, name, scope)?;
    let plugin = read_manifest(&dir)?;
    if !plugin.root.join(".git").is_dir() {
        return Err(PluginError::Invalid(format!(
            "{name} was installed by copying the directory {} — there is nothing to fetch; \
             `plugin add --force` re-copies it",
            plugin.root.display()
        )));
    }
    // `origin` is what the clone recorded. For a plugin cloned from a local path
    // that is the local path, and updating from it is exactly right.
    let reference = plugin
        .manifest
        .git_ref
        .clone()
        .unwrap_or_else(|| "HEAD".to_string());
    git_in(
        &plugin.root,
        &[
            "-c",
            "http.version=HTTP/1.1",
            "fetch",
            "--depth",
            "1",
            "origin",
            &reference,
        ],
    )?;

    // Everything needed to undo the fetch, taken while the checkout is still the one
    // that loaded.
    let previous = plugin.clone();
    git_in(&plugin.root, &["reset", "--hard", "FETCH_HEAD"])?;

    // The fetched commit's skills are parsed *before* the manifest is rewritten. A
    // checkout that does not load is put back on the commit that did, rather than left
    // for the next session start to trip over — the same failure `plugin add` rolls
    // back, and a session that will not open is the worst place to learn about it.
    if let Err(cause) = skill_names(&plugin.skills_dir()) {
        return Err(put_back(&previous, cause));
    }

    let mut updated = plugin.clone();
    let commit = git_in(&plugin.root, &["rev-parse", "HEAD"]).ok();
    if commit != updated.manifest.commit {
        updated.manifest.commit = commit;
        updated.manifest.installed_at_ms = now_ms();
    }
    write_manifest(&updated)?;
    read_manifest(&updated.root)
}

/// Purpose: Restore the last loading commit and manifest after a bad update, and explain the result.
/// Serves: `update`.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `update`.
/// Down: `git_in` and `write_manifest`.
/// Verify: `cargo test -p loong-plugin`.
/// Put a checkout back the way it was, after a fetch brought a commit that does not
/// load, and return the error that says so.
///
/// The manifest is rewritten from the copy taken *before* the fetch rather than left as
/// the checkout now has it. A repository may commit a `.loongagent-plugin.toml` of its
/// own and `reset --hard` restores tracked files, so anything less than an explicit
/// rewrite would let a failed update seat whatever that file says.
fn put_back(previous: &Plugin, cause: PluginError) -> PluginError {
    let name = previous.name();
    let Some(commit) = previous.manifest.commit.clone() else {
        return PluginError::Invalid(format!(
            "{cause}\n({name} is on a commit that does not load, and the one it was on was never \
             recorded — `plugin remove {name}` and add it again)"
        ));
    };
    let short: String = commit.chars().take(8).collect();
    match git_in(&previous.root, &["reset", "--hard", &commit]) {
        Err(back) => PluginError::Invalid(format!(
            "{cause}\n(and rolling it back failed too: {back}; {name} is on a commit that does \
             not load — `plugin remove {name}` and add it again)"
        )),
        Ok(_) => match write_manifest(previous) {
            Ok(()) => PluginError::Invalid(format!(
                "{cause}\n(nothing changed: the update was rolled back, and {name} is on {short} \
                 again)"
            )),
            Err(e) => PluginError::Invalid(format!(
                "{cause}\n(the update was rolled back to {short}, but its manifest could not be \
                 rewritten: {e} — `plugin add --force` puts it right)"
            )),
        },
    }
}

/// Purpose: Write a plugin's manifest beside its checkout.
/// Contract: called by `update` and `put_back`; a write failure becomes `PluginError::Io`.
/// Write an installed plugin's manifest beside its checkout.
fn write_manifest(plugin: &Plugin) -> Result<()> {
    let path = plugin.manifest_path();
    std::fs::write(&path, plugin.manifest.to_toml()?)
        .map_err(|e| PluginError::Io(format!("{}: {e}", path.display())))
}

/// Purpose: Clone a repository shallowly into a staging directory, with one retry.
/// Serves: `install`.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `install`.
/// Down: `run_git`.
/// Verify: `cargo test -p loong-plugin`.
/// `git clone --depth 1`.
///
/// `http.version=HTTP/1.1` is not decoration: git negotiates HTTP/2 by default and
/// on some networks and proxies the request then never completes, which looks like a
/// hung install rather than a failed one. Measured here against github.com: plain
/// `git clone` hung past 120 s, the same clone over HTTP/1.1 finished in seconds.
///
/// One retry, because the failure this actually sees is a dropped TLS handshake
/// ("the TLS connection was non-properly terminated"), and making somebody retype a
/// one-word command because a socket closed is not a good use of their attention. The
/// retry starts from an empty staging directory, so half a clone cannot be mistaken
/// for one that worked.
fn clone_into(url: &str, git_ref: Option<&str>, dest: &Path) -> Result<()> {
    let mut args: Vec<String> = vec![
        "-c".into(),
        "http.version=HTTP/1.1".into(),
        "clone".into(),
        "--depth".into(),
        "1".into(),
    ];
    if let Some(reference) = git_ref {
        args.push("--branch".into());
        args.push(reference.to_string());
    }
    args.push(url.to_string());
    args.push(dest.to_string_lossy().to_string());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut last = None;
    for attempt in 0..2 {
        if attempt > 0 {
            let _ = std::fs::remove_dir_all(dest);
        }
        match run_git(&refs) {
            Ok(_) => return Ok(()),
            Err(e) => last = Some(e),
        }
    }
    Err(PluginError::Fetch(format!(
        "could not fetch {url}: {}",
        last.expect("both attempts ran")
    )))
}

/// Purpose: Run `git -C <dir>` with argument-only commands, never a shell string.
/// Contract: called by `update` and `put_back`; `install` reaches it through `clone_into`.
/// `git` in a checkout. The operations this system needs — clone, fetch, reset,
/// rev-parse — take arguments rather than a shell string, so none of them needs a
/// shell (and nothing a plugin contains can smuggle a command in).
fn git_in(dir: &Path, args: &[&str]) -> Result<String> {
    let mut all = vec!["-C".to_string(), dir.to_string_lossy().to_string()];
    all.extend(args.iter().map(|a| a.to_string()));
    let refs: Vec<&str> = all.iter().map(String::as_str).collect();
    run_git(&refs)
}

/// Purpose: Run `git` and turn a failure into an actionable `PluginError::Fetch`.
/// Serves: `git_in` and `clone_into`.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `git_in` and `clone_into`.
/// Down: `std::process::Command`; no further call.
/// Verify: `cargo test -p loong-plugin`.
fn run_git(args: &[&str]) -> Result<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .output()
        .map_err(|e| {
            PluginError::Fetch(format!(
                "cannot run git ({e}) — installing a plugin from a repository needs git on PATH; \
                 a local directory works without it (`plugin add ./path/to/plugin`)"
            ))
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(PluginError::Fetch(format!(
            "git {} failed: {}",
            args.join(" "),
            stderr.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Purpose: Copy a plain directory into staging, skipping a nested `.git`.
/// Serves: `install`.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `install`.
/// Down: itself, recursively, and the filesystem.
/// Verify: `cargo test -p loong-plugin`.
/// Copy a plain directory, so a plugin that is just files on a disk installs the
/// same way one from a repository does. A nested `.git` is skipped: the copy is not
/// a checkout, and `update` should say so rather than fetch half a history.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to).map_err(|e| PluginError::Io(format!("{}: {e}", to.display())))?;
    for entry in std::fs::read_dir(from)
        .map_err(|e| PluginError::Io(format!("{}: {e}", from.display())))?
        .flatten()
    {
        let source = entry.path();
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let dest = to.join(&name);
        if source.is_dir() {
            copy_tree(&source, &dest)?;
        } else {
            std::fs::copy(&source, &dest)
                .map_err(|e| PluginError::Io(format!("{}: {e}", dest.display())))?;
        }
    }
    Ok(())
}

/// Purpose: Whether a name is safe as a directory, an argument and a `/name`.
/// Contract: called by `install` before anything is fetched.
/// A name that can be a directory, an argument, and a `/name`.
fn is_usable_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Purpose: The wall clock in milliseconds since the Unix epoch, for `installed_at_ms`.
/// Contract: called by `install` and `update`.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Purpose: Format a millisecond timestamp as a UTC `YYYY-MM-DD` date.
/// Serves: `crates/cli/src/plugin.rs` and the tests.
/// Design: `docs/design/extension-model.md` §4.
/// Up: `crates/cli/src/plugin.rs` and `the_dates_are_the_dates`.
/// Down: no call; pure arithmetic.
/// Verify: `cargo test -p loong-plugin`.
/// `YYYY-MM-DD` in UTC. Ten lines instead of a date library: the only thing this is
/// for is saying how old an install is.
pub fn utc_date(ms: u64) -> String {
    let days = (ms / 86_400_000) as i64;
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    /// Purpose: A unique temporary directory for one test, cleared first.
    /// Contract: test helper for every test in this module.
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "loong-plugin-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Purpose: Both plugin roots under temporary directories.
    /// Contract: test helper, so no test touches a real `~/.loongagent`.
    /// Both roots under temporary directories, so no test can touch a real install.
    fn roots(tag: &str) -> Roots {
        Roots {
            user: temp_dir(&format!("{tag}-user")),
            project: temp_dir(&format!("{tag}-project")),
        }
    }

    /// Purpose: Install options pointed at a test's temporary roots.
    /// Contract: test helper for the install and update tests.
    /// Options pointed at those roots. Specifications in tests are absolute paths
    /// or catalogue names, so `cwd` only has to be somewhere real.
    fn options(roots: &Roots) -> InstallOptions {
        InstallOptions {
            roots: roots.clone(),
            ..InstallOptions::new(&roots.project)
        }
    }

    /// Purpose: Lay out a Claude-Code-style `skills/<name>/SKILL.md` checkout.
    /// Contract: test fixture for the install and discovery tests.
    /// A checkout laid out the way a Claude Code plugin is: `skills/<name>/SKILL.md`.
    fn checkout(dir: &Path, names: &[&str]) {
        for name in names {
            let skill = dir.join("skills").join(name);
            std::fs::create_dir_all(&skill).unwrap();
            std::fs::write(
                skill.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: does {name}\n---\n\nBody of {name}.\n"),
            )
            .unwrap();
        }
    }

    /// Purpose: Write one skill directory into an existing checkout.
    /// Contract: test fixture used by the update tests.
    fn skill(dir: &Path, name: &str) {
        let skill = dir.join("skills").join(name);
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            format!("---\ndescription: does {name}\n---\n\nBody of {name}.\n"),
        )
        .unwrap();
    }

    /// Purpose: Run `git` in a test fixture and fail loudly on a non-zero exit.
    /// Serves: the two git-backed install and update tests.
    /// Design: `docs/design/extension-model.md` §4.
    /// Up: `a_git_install_records_the_commit_and_updates_in_place` and `an_update_that_would_break_the_plugin_goes_back_to_the_commit_that_worked`.
    /// Down: `std::process::Command`.
    /// Verify: `cargo test -p loong-plugin`.
    fn git(args: &[&str], cwd: &Path) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Purpose: Pin that `superpowers` resolves to the catalogue's source, skills directory and bootstrap.
    /// Verify: `cargo test -p loong-plugin`.
    #[test]
    fn a_catalog_name_is_a_one_word_install() {
        let known = catalog().iter().find(|k| k.name == "superpowers").unwrap();
        assert_eq!(known.source, "obra/superpowers");
        assert_eq!(known.skills, "skills");
        assert_eq!(known.bootstrap, &["using-superpowers"]);

        let cwd = temp_dir("spec");
        match resolve_spec("superpowers", &cwd).unwrap() {
            Source::Git { url, name, .. } => {
                assert_eq!(url, "https://github.com/obra/superpowers");
                assert_eq!(name, "superpowers");
            }
            other => panic!("expected a git source, got {other:?}"),
        }
    }

    /// Purpose: Pin every accepted spec shape: name, shorthand, URL, scp, directory and trailing `@ref`.
    /// Verify: `cargo test -p loong-plugin`.
    #[test]
    fn every_spec_shape_resolves_the_way_a_person_would_expect() {
        let cwd = temp_dir("specs");
        let named = |spec: &str| match resolve_spec(spec, &cwd).unwrap() {
            Source::Git { url, name, git_ref } => (url, name, git_ref),
            other => panic!("{spec}: expected a git source, got {other:?}"),
        };
        assert_eq!(
            named("obra/superpowers"),
            (
                "https://github.com/obra/superpowers".into(),
                "superpowers".into(),
                None
            )
        );
        assert_eq!(
            named("https://github.com/obra/superpowers.git"),
            (
                "https://github.com/obra/superpowers.git".into(),
                "superpowers".into(),
                None
            )
        );
        assert_eq!(
            named("github.com/obra/superpowers"),
            (
                "https://github.com/obra/superpowers".into(),
                "superpowers".into(),
                None
            )
        );
        assert_eq!(
            named("git@github.com:obra/superpowers.git"),
            (
                "git@github.com:obra/superpowers.git".into(),
                "superpowers".into(),
                None
            )
        );
        // A branch or a tag rides along, including on a catalogue name.
        assert_eq!(
            named("obra/superpowers@v6.4.2"),
            (
                "https://github.com/obra/superpowers".into(),
                "superpowers".into(),
                Some("v6.4.2".into())
            )
        );
        assert_eq!(named("superpowers@main").2, Some("main".into()));

        // A directory is a directory, spelled either way.
        let local = cwd.join("my-skills");
        std::fs::create_dir_all(&local).unwrap();
        assert_eq!(
            resolve_spec("my-skills", &cwd).unwrap(),
            Source::Dir {
                path: local.canonicalize().unwrap(),
                name: "my-skills".into()
            }
        );
        assert_eq!(
            resolve_spec(local.to_str().unwrap(), &cwd).unwrap(),
            Source::Dir {
                path: local.canonicalize().unwrap(),
                name: "my-skills".into()
            }
        );
        // And a path beats the heuristics even when it contains an `@`.
        let odd = cwd.join("we@rd");
        std::fs::create_dir_all(&odd).unwrap();
        assert!(matches!(
            resolve_spec("we@rd", &cwd).unwrap(),
            Source::Dir { .. }
        ));
    }

    /// Purpose: Pin that an unknown spec names the catalogue and `owner/repo` as the ways forward.
    #[test]
    fn an_unrecognisable_spec_says_what_would_be_recognised() {
        let cwd = temp_dir("bad-spec");
        let err = resolve_spec("nonsense", &cwd).unwrap_err().to_string();
        assert!(err.contains("superpowers"), "{err}");
        assert!(err.contains("owner/repo"), "{err}");
        assert!(resolve_spec("", &cwd).is_err());
    }

    /// Purpose: Pin that an install writes a readable manifest and parses the skills it installed.
    /// Verify: `cargo test -p loong-plugin`.
    #[test]
    fn install_writes_a_manifest_and_parses_the_skills_it_installed() {
        let roots = roots("install");
        let source = temp_dir("install-src");
        checkout(&source, &["alpha", "beta"]);

        let mut opts = options(&roots);
        opts.name = Some("mine".to_string());
        opts.skills = Some("skills".to_string());
        opts.bootstrap = Some(vec!["alpha".to_string()]);
        let plugin = install(source.to_str().unwrap(), &opts).unwrap();

        assert_eq!(plugin.name(), "mine");
        assert_eq!(plugin.root, roots.user.join("mine"));
        assert_eq!(plugin.skills_dir(), plugin.root.join("skills"));
        assert_eq!(plugin.manifest.bootstrap, vec!["alpha".to_string()]);
        assert!(plugin.manifest_path().is_file());
        assert_eq!(
            plugin.manifest.installed_from.as_deref(),
            Some(source.canonicalize().unwrap().to_str().unwrap())
        );
        let mut names = skill_names(&plugin.skills_dir()).unwrap();
        names.sort();
        assert_eq!(names, vec!["alpha".to_string(), "beta".to_string()]);
        // The manifest says what it is, in a file a person can read.
        let text = std::fs::read_to_string(plugin.manifest_path()).unwrap();
        assert!(
            text.starts_with("# Written by `loongagent plugin add`"),
            "{text}"
        );
        assert!(text.contains("name = \"mine\""), "{text}");
    }

    /// Purpose: Pin `skills/` detection and the flat-checkout fallback.
    /// Verify: `cargo test -p loong-plugin`.
    #[test]
    fn the_skills_root_is_detected_in_both_shapes() {
        let roots = roots("detect");
        let claude = temp_dir("detect-claude");
        checkout(&claude, &["one"]);
        std::fs::write(claude.join("README.md"), "not a skill").unwrap();
        let opts = options(&roots);
        let plugin = install(claude.to_str().unwrap(), &opts).unwrap();
        assert_eq!(plugin.manifest.skills, "skills");
        assert_eq!(skill_names(&plugin.skills_dir()).unwrap(), vec!["one"]);

        let flat = temp_dir("detect-flat");
        std::fs::create_dir_all(flat.join("only")).unwrap();
        std::fs::write(
            flat.join("only/SKILL.md"),
            "---\ndescription: only\n---\nbody\n",
        )
        .unwrap();
        let plugin = install(flat.to_str().unwrap(), &opts).unwrap();
        assert_eq!(plugin.manifest.skills, ".");
    }

    /// Purpose: Pin that a skills-less directory is refused and leaves nothing half-installed.
    /// Verify: `cargo test -p loong-plugin`.
    #[test]
    fn a_directory_without_skills_is_refused_with_a_way_forward() {
        let roots = roots("noskills");
        let empty = temp_dir("noskills-src");
        std::fs::write(empty.join("README.md"), "hello").unwrap();
        let opts = options(&roots);
        let err = install(empty.to_str().unwrap(), &opts)
            .unwrap_err()
            .to_string();
        assert!(err.contains("holds no skills"), "{err}");
        assert!(err.contains("--skills"), "{err}");
        // Nothing half-installed is left behind.
        assert_eq!(std::fs::read_dir(&roots.user).unwrap().count(), 0);
    }

    /// Purpose: Pin that one unparseable skill refuses the whole install before it is committed.
    /// Verify: `cargo test -p loong-plugin`.
    #[test]
    fn a_broken_skill_refuses_the_whole_install() {
        let roots = roots("broken");
        let source = temp_dir("broken-src");
        checkout(&source, &["fine"]);
        let bad = source.join("skills/bad");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("SKILL.md"), "no frontmatter here\n").unwrap();

        let err = install(source.to_str().unwrap(), &options(&roots))
            .unwrap_err()
            .to_string();
        assert!(err.contains("frontmatter"), "{err}");
        assert_eq!(std::fs::read_dir(&roots.user).unwrap().count(), 0);
        assert!(
            !roots.user.join("broken-src").exists(),
            "a plugin that would break every session is not installed"
        );
    }

    /// Purpose: Pin that a bootstrap skill the checkout does not have refuses the install.
    /// Verify: `cargo test -p loong-plugin`.
    #[test]
    fn a_declared_bootstrap_skill_the_plugin_does_not_have_is_refused() {
        let roots = roots("bootstrap-missing");
        let source = temp_dir("bootstrap-missing-src");
        checkout(&source, &["alpha"]);
        let mut opts = options(&roots);
        // What `plugin add /path/to/superpowers` does when the checkout is not
        // actually Superpowers: the catalogue's declaration must not be taken on faith.
        opts.name = Some("superpowers".to_string());
        opts.bootstrap = Some(vec!["using-superpowers".to_string()]);

        let err = install(source.to_str().unwrap(), &opts)
            .unwrap_err()
            .to_string();
        assert!(err.contains("declared as a bootstrap skill"), "{err}");
        assert!(err.contains("it has: alpha"), "{err}");
        assert!(
            !roots.user.join("superpowers").exists(),
            "a plugin whose always-on skill does not exist is not installed"
        );
    }

    /// Purpose: Pin the already-installed refusal and the `--force` override.
    /// Verify: `cargo test -p loong-plugin`.
    #[test]
    fn installing_twice_is_refused_until_forced() {
        let roots = roots("twice");
        let source = temp_dir("twice-src");
        checkout(&source, &["one"]);
        let mut opts = options(&roots);
        opts.name = Some("mine".to_string());
        install(source.to_str().unwrap(), &opts).unwrap();

        let err = install(source.to_str().unwrap(), &opts)
            .unwrap_err()
            .to_string();
        assert!(err.contains("already installed"), "{err}");

        opts.force = true;
        assert!(install(source.to_str().unwrap(), &opts).is_ok());
    }

    /// Purpose: Pin discovery order, the catalogue bootstrap and the project shadow report.
    /// Verify: `cargo test -p loong-plugin`.
    #[test]
    fn discovery_reads_installs_and_reports_shadowing() {
        let roots = roots("discover");
        // A checkout of a known plugin lives in a directory named after it, which is
        // how `plugin add /path/to/superpowers` finds the catalogue's answer.
        let source = temp_dir("discover-src").join("superpowers");
        std::fs::create_dir_all(&source).unwrap();
        checkout(&source, &["alpha", "using-superpowers"]);

        let opts = options(&roots);
        install(source.to_str().unwrap(), &opts).unwrap();

        let installed = discover(&roots).unwrap();
        assert_eq!(installed.plugins.len(), 1);
        assert_eq!(installed.plugins[0].name(), "superpowers");
        assert_eq!(
            installed.skill_dirs(),
            vec![installed.plugins[0].skills_dir()]
        );
        assert_eq!(installed.plugins[0].scope(&roots), "user");
        // The catalogue's bootstrap answer applied, and its note travelled with it.
        assert_eq!(
            installed.bootstrap_skills(),
            vec!["using-superpowers".to_string()]
        );
        let notes = installed.bootstrap_notes();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("superpowers:<name>"), "{}", notes[0]);
        assert!(installed.get("superpowers").is_some());
        assert!(installed.get("nope").is_none());

        // A project install of the same name wins, and says so rather than
        // leaving two answers in the catalogue.
        let mut project = opts.clone();
        project.scope = Scope::Project;
        install(source.to_str().unwrap(), &project).unwrap();
        let installed = discover(&roots).unwrap();
        assert_eq!(installed.plugins.len(), 1);
        assert_eq!(installed.shadowed, vec!["superpowers".to_string()]);
        assert_eq!(installed.plugins[0].scope(&roots), "project");
    }

    /// Purpose: Pin the ambiguous-name refusal and that each scope stays reachable.
    /// Verify: `cargo test -p loong-plugin`.
    #[test]
    fn a_name_installed_in_both_scopes_has_to_say_which_one() {
        let roots = roots("scopes");
        let source = temp_dir("scopes-src");
        checkout(&source, &["alpha"]);

        let mut user = options(&roots);
        user.scope = Scope::User;
        user.name = Some("both".to_string());
        let user_dir = install(source.to_str().unwrap(), &user).unwrap().root;

        let mut project = options(&roots);
        project.scope = Scope::Project;
        project.name = Some("both".to_string());
        let project_dir = install(source.to_str().unwrap(), &project).unwrap().root;
        assert_ne!(user_dir, project_dir);

        // Unqualified the name is ambiguous, and the project copy shadows the user one.
        // Picking one silently is exactly what ADR-0018 §1 refuses to do — and until now
        // the hidden copy could not be reached at all.
        let err = remove(&roots, "both", None).unwrap_err().to_string();
        assert!(err.contains("--project"), "{err}");
        assert!(err.contains("--user"), "{err}");
        assert!(
            user_dir.exists() && project_dir.exists(),
            "an ambiguous name removes nothing"
        );

        // `update` reaches the same resolver, so it refuses the same way before fetching.
        let err = update(&roots, "both", None).unwrap_err().to_string();
        assert!(err.contains("--user"), "{err}");

        // Qualified, each copy is reachable — including the one the shadow hides.
        assert_eq!(remove(&roots, "both", Some(Scope::User)).unwrap(), user_dir);
        assert!(project_dir.exists(), "the shadow survived");
        assert_eq!(
            remove(&roots, "both", Some(Scope::Project)).unwrap(),
            project_dir
        );

        // A name in one scope only still needs no flag.
        let mut solo = options(&roots);
        solo.scope = Scope::User;
        solo.name = Some("solo".to_string());
        install(source.to_str().unwrap(), &solo).unwrap();
        assert!(remove(&roots, "solo", None).is_ok());
    }

    /// Purpose: Pin that a plain checkout without a manifest is not a plugin.
    #[test]
    fn a_directory_without_a_manifest_is_not_a_plugin() {
        let roots = roots("nomanifest");
        std::fs::create_dir_all(roots.user.join("just-a-checkout")).unwrap();
        assert!(discover(&roots).unwrap().is_empty());
    }

    /// Purpose: Pin that a missing skills directory and a bad manifest both fail loudly and stay removable.
    /// Verify: `cargo test -p loong-plugin`.
    #[test]
    fn a_manifest_pointing_at_nothing_is_loud() {
        let roots = roots("missing");
        let dir = roots.user.join("broken");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(MANIFEST_FILE),
            "name = \"broken\"\nsource = \"obra/superpowers\"\nskills = \"skills\"\n\
             installed_at_ms = 0\n",
        )
        .unwrap();
        let err = discover(&roots).unwrap_err().to_string();
        assert!(err.contains("not a directory"), "{err}");

        // A manifest that is not TOML is loud too, and names the file.
        std::fs::write(dir.join(MANIFEST_FILE), "this is not toml").unwrap();
        let err = discover(&roots).unwrap_err().to_string();
        assert!(err.contains(MANIFEST_FILE), "{err}");

        // …and it can still be removed, which is the way out of that state.
        assert_eq!(remove(&roots, "broken", None).unwrap(), dir);
        assert!(discover(&roots).unwrap().is_empty());
    }

    /// Purpose: Pin that absolute and `..` skills paths are refused while the shapes an install writes still read.
    /// Verify: `cargo test -p loong-plugin`.
    #[test]
    fn a_manifest_pointing_outside_the_checkout_is_refused() {
        let roots = roots("outside");
        let dir = roots.user.join("stray");
        std::fs::create_dir_all(&dir).unwrap();
        // `Path::join` hands an absolute path back unchanged and `..` walks up, so a
        // manifest saying either of these points the skills loader at a directory the
        // plugin does not own. `/` is deliberately paired with `/etc`: the first is
        // *refused because it escapes*, not because it happens to be missing.
        for skills in ["/", "/etc", "../outside", "skills/../../outside"] {
            std::fs::write(
                dir.join(MANIFEST_FILE),
                format!(
                    "name = \"stray\"\nsource = \"obra/superpowers\"\nskills = {skills:?}\n\
                     installed_at_ms = 0\n"
                ),
            )
            .unwrap();
            let err = discover(&roots).unwrap_err().to_string();
            assert!(
                err.contains("inside the checkout"),
                "skills = {skills:?} was not refused for escaping: {err}"
            );
        }

        // The shapes an install actually writes still read.
        for skills in [".", "skills", "content/skills"] {
            std::fs::create_dir_all(dir.join(skills)).unwrap();
            std::fs::write(
                dir.join(MANIFEST_FILE),
                format!(
                    "name = \"stray\"\nsource = \"obra/superpowers\"\nskills = {skills:?}\n\
                     installed_at_ms = 0\n"
                ),
            )
            .unwrap();
            assert_eq!(read_manifest(&dir).unwrap().manifest.skills, skills);
        }
    }

    /// Purpose: Pin that `--skills` cannot point outside the checkout at install time.
    /// Verify: `cargo test -p loong-plugin`.
    #[test]
    fn skills_outside_the_checkout_are_refused_at_install() {
        let roots = roots("skills-outside");
        let source = temp_dir("skills-outside-src");
        checkout(&source, &["alpha"]);
        // A directory that really does hold a parseable skill, so the refusal is about
        // *where it is* rather than about it being empty.
        let elsewhere = temp_dir("skills-outside-elsewhere");
        skill(&elsewhere, "beta");

        let mut opts = options(&roots);
        opts.skills = Some(elsewhere.to_string_lossy().to_string());
        let err = install(source.to_str().unwrap(), &opts)
            .unwrap_err()
            .to_string();
        assert!(err.contains("inside the checkout"), "{err}");
        assert!(
            discover(&roots).unwrap().is_empty(),
            "a refused install leaves nothing behind"
        );
    }

    /// Purpose: Pin remove's deletion and update's refusal on a copied directory.
    /// Verify: `cargo test -p loong-plugin`.
    #[test]
    fn remove_deletes_the_checkout_and_update_says_why_it_cannot() {
        let roots = roots("remove");
        let source = temp_dir("remove-src");
        checkout(&source, &["alpha"]);
        let mut opts = options(&roots);
        opts.name = Some("gone".to_string());
        install(source.to_str().unwrap(), &opts).unwrap();
        assert!(discover(&roots).unwrap().get("gone").is_some());

        let err = update(&roots, "gone", None).unwrap_err().to_string();
        assert!(err.contains("nothing to fetch"), "{err}");

        let root = remove(&roots, "gone", None).unwrap();
        assert!(!root.exists());
        assert!(discover(&roots).unwrap().is_empty());
        let err = remove(&roots, "gone", None).unwrap_err().to_string();
        assert!(err.contains("no plugin named"), "{err}");
    }

    /// Purpose: Pin the recorded commit and an in-place update that gains a skill.
    /// Verify: `cargo test -p loong-plugin`.
    #[test]
    fn a_git_install_records_the_commit_and_updates_in_place() {
        let roots = roots("git");
        let source = temp_dir("git-src");
        git(&["init", "--quiet", "-b", "main"], &source);
        checkout(&source, &["alpha"]);
        git(&["add", "-A"], &source);
        git(&["commit", "-m", "first"], &source);

        let mut opts = options(&roots);
        opts.name = Some("from-git".to_string());
        let plugin = install(&source.to_string_lossy(), &opts).unwrap();
        let first = plugin
            .manifest
            .commit
            .clone()
            .expect("a commit was recorded");
        assert_eq!(first.len(), 40);
        assert!(plugin.manifest.installed_from.is_some());

        // A second commit upstream is what `plugin update` is for.
        skill(&source, "beta");
        git(&["add", "-A"], &source);
        git(&["commit", "-m", "second"], &source);

        let updated = update(&roots, "from-git", None).unwrap();
        assert_ne!(updated.manifest.commit.as_deref(), Some(first.as_str()));
        let mut names = skill_names(&updated.skills_dir()).unwrap();
        names.sort();
        assert_eq!(names, vec!["alpha".to_string(), "beta".to_string()]);
    }

    /// Purpose: Pin the rollback to the last loading commit and manifest.
    /// Verify: `cargo test -p loong-plugin`.
    #[test]
    fn an_update_that_would_break_the_plugin_goes_back_to_the_commit_that_worked() {
        let roots = roots("update-back");
        let source = temp_dir("update-back-src");
        git(&["init", "--quiet", "-b", "main"], &source);
        checkout(&source, &["alpha"]);
        git(&["add", "-A"], &source);
        git(&["commit", "-m", "first"], &source);

        let mut opts = options(&roots);
        opts.name = Some("rolling".to_string());
        let installed = install(&source.to_string_lossy(), &opts).unwrap();
        let good = installed
            .manifest
            .commit
            .clone()
            .expect("a commit was recorded");

        // Upstream now ships a skill that cannot parse, and a manifest of its own —
        // `git reset --hard` restores tracked files, so a failed update must not leave
        // either of them in place.
        std::fs::write(
            source.join("skills/alpha/SKILL.md"),
            "no frontmatter here\n",
        )
        .unwrap();
        std::fs::write(
            source.join(MANIFEST_FILE),
            "name = \"hijacked\"\nsource = \"https://example.invalid\"\nskills = \".\"\n\
             bootstrap = [\"alpha\"]\ninstalled_at_ms = 0\n",
        )
        .unwrap();
        git(&["add", "-A"], &source);
        git(&["commit", "-m", "breaks"], &source);

        let err = update(&roots, "rolling", None).unwrap_err().to_string();
        assert!(err.contains("rolled back"), "{err}");

        // The install on disk is still the one that worked: the manifest read before
        // the fetch, and the commit that loaded.
        let after = discover(&roots).unwrap();
        assert!(after.shadowed.is_empty(), "{:?}", after.shadowed);
        let plugin = after.get("rolling").expect("still installed");
        assert_eq!(
            plugin.manifest.name, "rolling",
            "not the checkout's manifest"
        );
        assert!(
            plugin.manifest.bootstrap.is_empty(),
            "no hijacked bootstrap"
        );
        assert_eq!(plugin.manifest.commit.as_deref(), Some(good.as_str()));
        assert_eq!(
            skill_names(&plugin.skills_dir()).unwrap(),
            vec!["alpha".to_string()]
        );
    }

    /// Purpose: Pin the UTC date arithmetic across the epoch and recent dates.
    #[test]
    fn the_dates_are_the_dates() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(1_700_000_000_000), "2023-11-14");
        assert_eq!(utc_date(1_760_000_000_000), "2025-10-09");
    }
}
