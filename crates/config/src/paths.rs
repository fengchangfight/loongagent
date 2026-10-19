//! Purpose: Where every configuration layer and the client's state live on this machine.
//!
//! # Contract
//! - **Serves:** `resolve_with`, the CLI's setup and the mode guard.
//! - **Design:** `docs/design/config-routing.md` §1 and §3.
//! - **Upstream:** `merge::resolve_with`, the CLI and `plugin`.
//! - **Downstream:** the platform environment variables and the returned paths.
//! - **Verify:** `cargo test -p loong-config` · `./scripts/check.sh`
//!
use super::*;

/// Purpose: Expand a leading `~/` against the user's home directory.
///
/// Serves: every `skill_dirs` entry a person writes with a tilde.
/// Design: `docs/design/config-routing.md` §1.
/// Up: `LayerValues::from_file` while resolving `skill_dirs`.
/// Down: `home_dir`.
/// Verify: `cargo test -p loong-config`.
pub(super) fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

/// The user's home directory, spelled the way each platform spells it.
///
/// Unix sets `HOME`. Windows sets `USERPROFILE` — `HOME` is only present when a
/// POSIX-ish shell (Git Bash, MSYS) exported it, and a process launched from a
/// shortcut or a service has none. Reading only `HOME` fails *quietly*: `~/…` stays
/// a literal directory name and the configuration root falls through to
/// [`std::env::temp_dir`], so a Windows user's settings and sessions would live in
/// `%TEMP%` and vanish. Both names are consulted; `HOME` wins when it is set, so a
/// Git Bash session keeps pointing where it always did.
/// Purpose: The user's home directory, spelled the way each platform spells it.
/// Serves: `expand_home` and every `~/.loongagent` path.
/// Design: `docs/design/config-routing.md` §1.
/// Up: `expand_home`, `loongagent_home`, the CLI and `plugin`.
/// Down: `non_empty` over `HOME` and `USERPROFILE`.
/// Verify: `cargo test -p loong-config`.
pub fn home_dir() -> Option<PathBuf> {
    non_empty("HOME")
        .or_else(|| non_empty("USERPROFILE"))
        .map(PathBuf::from)
}

/// Where a **machine-wide** configuration file lives.
///
/// `/etc/loongagent` on Unix is an administrator's directory. Its Windows analogue
/// is `%PROGRAMDATA%\loongagent`; without this the org layer would resolve to the
/// drive-relative `\etc\loongagent`, which is not a place anything writes.
/// Purpose: The machine-wide profile directory: `/etc/loongagent` or `%PROGRAMDATA%\loongagent`.
/// Serves: the unsigned org layer and the control-plane client file.
/// Design: `docs/design/config-routing.md` §1 and §5.
/// Up: `layer_paths` and the CLI's control path.
/// Down: the platform environment variables.
/// Verify: `cargo test -p loong-config`.
pub fn system_config_dir() -> PathBuf {
    #[cfg(windows)]
    {
        non_empty("PROGRAMDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:\\ProgramData"))
            .join("loongagent")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("/etc/loongagent")
    }
}

/// Where each layer's file lives on this machine.
///
/// The org layer is a file an administrator owns. The signed pack envelope
/// exists and is verified against a pinned key *before* it can occupy this layer
/// (ADR-0016); what this path is, is the **local** org file, a plain
/// `profile.toml` and the thing that must not be called tamper-resistant.
/// Purpose: The managed layer files this machine has, in resolution order.
/// Serves: `resolve`/`resolve_with` from the CLI.
/// Design: `docs/design/config-routing.md` §1.
/// Up: the CLI's config and control commands and `main`.
/// Down: `system_config_dir` and `loongagent_home`.
/// Verify: `cargo test -p loong-config`.
pub fn layer_paths(workspace: &Path, session: Option<&Path>) -> Vec<(Layer, PathBuf)> {
    let mut layers = Vec::new();
    layers.push((Layer::Org, system_config_dir().join("profile.toml")));
    layers.push((Layer::Project, workspace.join(".loongagent/profile.toml")));
    // One root for everything the local client keeps, so "where does loongagent put
    // things?" has a one-word answer. `LOONGAGENT_HOME` moves it for CI and scripts.
    layers.push((Layer::User, loongagent_home().join("profile.toml")));
    if let Some(session) = session {
        layers.push((Layer::Session, session.to_path_buf()));
    }
    layers
}

/// Where a **local** install keeps its own configuration.
///
/// A separate file, not a section of [`layer_paths`]' user file. Local mode is a
/// different way of deciding where prompts go, and the two must not be able to reach
/// each other: a local run that read the managed profile would inherit models nobody
/// chose, and — far worse — a managed run that read a local file could be switched out
/// of its firm's policy by a file that has nothing to do with managed mode.
/// Purpose: Where a local install keeps `local.toml`.
/// Serves: the separate local path and the mode guard.
/// Design: `docs/design/config-routing.md` §3.
/// Up: `local_layer_paths`, `mode_is_legitimate` and the CLI's local command.
/// Down: `loongagent_home`.
/// Verify: `cargo test -p loong-config`.
pub fn local_profile_path() -> PathBuf {
    loongagent_home().join("local.toml")
}

/// The layers a **local** run resolves.
///
/// Deliberately not [`layer_paths`]: no org file, and the user's own `local.toml`
/// instead of the managed `profile.toml`. The workspace layer stays, because a
/// repository may still *narrow* what is allowed (ADR-0008 §2) — it cannot name a
/// model, an endpoint or a key, so it cannot put anything into this list that a person
/// did not choose.
/// Purpose: The layers a local run resolves: project, `local.toml`, session; no org and no pack.
/// Serves: `--local`, so managed policy cannot leak in.
/// Design: `docs/design/config-routing.md` §3.
/// Up: the CLI's `layers(local)`.
/// Down: `local_profile_path`.
/// Verify: `cargo test -p loong-config`.
pub fn local_layer_paths(workspace: &Path, session: Option<&Path>) -> Vec<(Layer, PathBuf)> {
    let mut layers = Vec::new();
    layers.push((Layer::Project, workspace.join(".loongagent/profile.toml")));
    layers.push((Layer::User, local_profile_path()));
    if let Some(session) = session {
        layers.push((Layer::Session, session.to_path_buf()));
    }
    layers
}

/// Where the local client keeps its files: `$LOONGAGENT_HOME`, else `~/.loongagent`.
///
/// Files directly in it are **configuration** (`profile.toml`, `control.toml`, the
/// `env` file holding secrets, a copy of the client token); its subdirectories are
/// **state** (`projects/`, `control/`). The control *plane*, being a service, keeps
/// its own files where a service's belong: `/etc/loongagent-control` for what an
/// administrator authors, `/var/lib/loongagent-control` for what it writes.
/// Purpose: Where the local client keeps its files: `$LOONGAGENT_HOME`, else `~/.loongagent`.
/// Serves: the user layer, `local.toml`, the control cache and plugins.
/// Design: `docs/design/config-routing.md` §1.
/// Up: `layer_paths`, `local_profile_path`, the CLI and `plugin`.
/// Down: `non_empty` and `home_dir`.
/// Verify: `cargo test -p loong-config`.
pub fn loongagent_home() -> PathBuf {
    non_empty("LOONGAGENT_HOME")
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join(".loongagent")))
        .unwrap_or_else(std::env::temp_dir)
}

/// Purpose: A non-empty environment variable, or `None`.
/// Contract: `crates/config/src/paths.rs`.
fn non_empty(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}
