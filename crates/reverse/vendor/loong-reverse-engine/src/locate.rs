//! > Vendored from Craft Anything (Apache-2.0/MIT, © Craft Anything contributors),
//! > upstream commit `fc93bd0` (2026-11-05). Owned by `docs/design/reverse.md`.
//!
//! Finding an external program on this computer, and naming the ones we look for.
//!
//! Purpose: answer "where is `chromium`?" for the few external programs the reverse surface still
//! uses (a browser for the web tools), honouring the user's path override before the system.
//! Serves: `docs/design/reverse.md` (web/js toolsheet needs a local browser).
//! Design: `docs/design/reverse.md`; vendoring in
//! `docs/superpowers/specs/2026-10-24-loongagent-reverse-design.md`.
//! Upstream: `loong-reverse-evidence` (`ToolError`).
//! Downstream: `crate::settings`, `crate::tools::web_common`, `crate::lib` (availability).
//! Verify: `scripts/check-reverse.sh`.
//!
//! Upstream kept a whole `loong-reverse-engines` crate listing every supported engine, its install
//! recipes and its bridges (Ghidra, JADX, adb, Frida, rizin…). The reverse surface copies none of
//! those bridges, so only the lookup, the display names and the override table remain.

use std::path::{Path, PathBuf};

/// A settings object that can name its own path for an external program.
///
/// Purpose: decouple the lookup from the settings type, so this module never depends on it.
/// Serves: `crate::settings::ToolPaths`.
/// Design: `docs/design/reverse.md`.
/// Up: `crate::settings::Settings::engine_path`.
/// Down: [`locate`], [`locate_configured`].
/// Verify: `cargo test -p loong-reverse-engine`.
pub trait ToolPathLookup {
    /// The path the user set for `id`, when they set one.
    fn override_for(&self, id: &str) -> Option<PathBuf>;
}

/// The external programs the reverse surface looks for, with the name a person sees.
///
/// Only `chromium` is required by a shipped tool (the web/js toolsheet starts a browser); the rest
/// are looked up for the settings page, so a user's own install is honoured if a future tool needs
/// one. Order is the settings page's order.
///
/// Purpose: one table behind [`display_name`] and [`path_setting_ids`].
/// Serves: `docs/design/reverse.md`.
/// Design: `docs/design/reverse.md`.
/// Up: [`locate`], [`path_setting_ids`].
/// Down: nothing.
/// Verify: `cargo test -p loong-reverse-engine`.
pub const PROGRAMS: &[(&str, &str)] = &[
    ("chromium", "Chromium (a Chrome-family browser)"),
    ("adb", "Android Debug Bridge"),
    ("jadx", "JADX"),
    ("apktool", "Apktool"),
    ("java", "Java runtime"),
    ("objdump", "GNU objdump"),
];

/// The command a lookup tries on `PATH` for an id, when it differs from the id.
///
/// Purpose: let `chromium` match the real binaries (`chromium`, `google-chrome`, …).
/// Serves: `docs/design/reverse.md`.
/// Design: `docs/design/reverse.md`.
/// Up: [`locate`].
/// Down: nothing.
/// Verify: `cargo test -p loong-reverse-engine`.
fn commands(id: &str) -> &'static [&'static str] {
    match id {
        "chromium" => &[
            "chromium",
            "chromium-browser",
            "google-chrome",
            "google-chrome-stable",
            "microsoft-edge",
            "brave-browser",
        ],
        "java" => &["java"],
        "adb" => &["adb"],
        "jadx" => &["jadx"],
        "apktool" => &["apktool"],
        "objdump" => &["objdump"],
        _ => &[],
    }
}

/// The name a person sees for an external program id.
///
/// Purpose: one wording for availability messages and the settings page.
/// Serves: `docs/design/reverse.md`.
/// Design: `docs/design/reverse.md`.
/// Up: `crate::lib` (availability), `crate::settings`.
/// Down: [`PROGRAMS`].
/// Verify: `cargo test -p loong-reverse-engine`.
pub fn display_name(id: &str) -> String {
    PROGRAMS
        .iter()
        .find(|(k, _)| *k == id)
        .map(|(_, name)| (*name).to_string())
        .unwrap_or_else(|| id.to_string())
}

/// Every id that has a path setting, in settings-page order.
///
/// Purpose: keep the settings page's override list and the table above in step.
/// Serves: `docs/design/reverse.md`.
/// Design: `docs/design/reverse.md`.
/// Up: `crate::settings` (and its test).
/// Down: [`PROGRAMS`].
/// Verify: `cargo test -p loong-reverse-engine`.
pub fn path_setting_ids() -> impl Iterator<Item = &'static str> {
    PROGRAMS.iter().map(|(id, _)| *id)
}

/// Whether `id` names a program we know.
///
/// Purpose: let a spec's `Requires::Engine(id)` be checked at test time.
/// Serves: `docs/design/reverse.md`.
/// Design: `docs/design/reverse.md`.
/// Up: `crate::lib` (test).
/// Down: [`PROGRAMS`].
/// Verify: `cargo test -p loong-reverse-engine`.
pub fn is_known(id: &str) -> bool {
    PROGRAMS.iter().any(|(k, _)| *k == id)
}

/// Where a program is on this computer: the user's override, the managed store, then `PATH`.
///
/// Purpose: one lookup behind `Settings::engine_path` and `Requires::Engine`.
/// Serves: `docs/design/reverse.md`.
/// Design: `docs/design/reverse.md`.
/// Up: `crate::settings::Settings::engine_path`.
/// Down: [`locate_configured`].
/// Verify: `cargo test -p loong-reverse-engine`.
pub fn locate(id: &str, tools: &impl ToolPathLookup, store: &Path) -> Option<PathBuf> {
    if let Some(p) = tools.override_for(id).filter(|p| p.is_file()) {
        return Some(p);
    }
    if let Some(p) = managed(store, id) {
        return Some(p);
    }
    on_path(id)
}

/// Like [`locate`], but only where the settings say: no `PATH`, no standard install folders.
///
/// Purpose: keep tests off the developer's machine (a test session sets `only_configured`).
/// Serves: `docs/design/reverse.md`.
/// Design: `docs/design/reverse.md`.
/// Up: `crate::settings::Settings::engine_path`.
/// Down: nothing.
/// Verify: `cargo test -p loong-reverse-engine`.
pub fn locate_configured(id: &str, tools: &impl ToolPathLookup, store: &Path) -> Option<PathBuf> {
    tools
        .override_for(id)
        .filter(|p| p.is_file())
        .or_else(|| managed(store, id))
}

/// A program the user installed into the managed engine store, if any.
///
/// Purpose: check the store before searching `PATH`.
/// Serves: `docs/design/reverse.md`.
/// Design: `docs/design/reverse.md`.
/// Up: [`locate`], [`locate_configured`].
/// Down: nothing.
/// Verify: `cargo test -p loong-reverse-engine`.
fn managed(store: &Path, id: &str) -> Option<PathBuf> {
    commands(id)
        .iter()
        .map(|c| store.join(id).join(c))
        .find(|p| p.is_file())
}

/// The first of `id`'s commands found on `PATH`.
///
/// Purpose: find a program the user already has without any setting.
/// Serves: `docs/design/reverse.md`.
/// Design: `docs/design/reverse.md`.
/// Up: [`locate`].
/// Down: nothing.
/// Verify: `cargo test -p loong-reverse-engine`.
fn on_path(id: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let names = commands(id);
    std::env::split_paths(&path)
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Purpose: every table id has a display name different from its id, and is known.
    #[test]
    fn table_is_consistent() {
        for (id, _) in PROGRAMS {
            assert!(is_known(id));
            assert_ne!(display_name(id), *id);
        }
        assert_eq!(path_setting_ids().count(), PROGRAMS.len());
        assert!(!is_known("ghidra"));
    }

    /// Purpose: an override wins, and a configured-only lookup ignores `PATH`.
    #[test]
    fn override_beats_path() {
        struct Only(PathBuf);
        impl ToolPathLookup for Only {
            fn override_for(&self, id: &str) -> Option<PathBuf> {
                (id == "chromium").then(|| self.0.clone())
            }
        }
        let dir = std::env::temp_dir().join(format!("loong-reverse-locate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("chromium");
        std::fs::write(&fake, b"").unwrap();
        let store = dir.join("store");
        assert_eq!(
            locate("chromium", &Only(fake.clone()), &store).as_deref(),
            Some(fake.as_path())
        );
        assert_eq!(locate_configured("jadx", &Only(fake), &store), None);
        std::fs::remove_dir_all(&dir).ok();
    }
}
