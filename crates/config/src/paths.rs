use super::*;

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
pub fn loongagent_home() -> PathBuf {
    non_empty("LOONGAGENT_HOME")
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join(".loongagent")))
        .unwrap_or_else(std::env::temp_dir)
}

fn non_empty(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}
