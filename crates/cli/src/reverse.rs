//! Purpose: `loongagent reverse`: a study session over the reverse-engineering tools.
//!
//! # Contract
//! - **Serves:** the `reverse` arm of `main.rs`: scaffold `.reverse/`, write and self-trust its
//!   manifest, run the browser session, and clean the manifest pair up on exit.
//! - **Design:** `docs/design/reverse.md` §3 and §4.
//! - **Upstream:** `main.rs`'s `reverse` arm parses the flags and calls `run`.
//! - **Downstream:** `loong_mcp::McpSet` for the name-collision check and `trust_reverse_now`,
//!   `ChildTransport` and `loong_web` for the session itself.
//! - **Verify:** `cargo test -p loongagent reverse_` · `./scripts/check.sh`

use std::path::{Path, PathBuf};

use loong_mcp::{ManifestOrigin, McpSet, MCP_FILE, MCP_TRUST_FILE, REVERSE_DIR};

/// Purpose: The server's own name in the session manifest: it namespaces every reverse tool.
/// Contract: `crates/cli/src/reverse.rs`.
const REVERSE_SERVER: &str = "loong-reverse";

/// Purpose: What the positional target turned out to be.
/// Serves: `run`, which tells the operator what was opened.
/// Design: `docs/design/reverse.md` §4.
/// Up: `run` only.
/// Down: nothing; a display enum.
/// Verify: `cargo test -p loongagent reverse_`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TargetKind {
    /// An existing regular file on disk.
    File,
    /// An `http://` / `https://` URL.
    Url,
}

/// Purpose: Check the target before anything binds: a file must exist and be a file.
/// Serves: `run`'s fail-fast step.
/// Design: `docs/design/reverse.md` §4.
/// Up: `run` only.
/// Down: `std::fs::metadata`.
/// Verify: `cargo test -p loongagent reverse_rejects_missing_target`.
pub(crate) fn validate_target(target: &str) -> Result<TargetKind, String> {
    if target.starts_with("http://") || target.starts_with("https://") {
        return Ok(TargetKind::Url);
    }
    let path = Path::new(target);
    let meta = std::fs::metadata(path).map_err(|e| {
        format!("{target}: cannot open the target ({e}); pass a file or an http(s) URL")
    })?;
    if !meta.is_file() {
        return Err(format!(
            "{target}: not a regular file; pass a file or an http(s) URL"
        ));
    }
    Ok(TargetKind::File)
}

/// Purpose: Build `.reverse/`, write the session's manifest, and record its trust.
/// Serves: `run`'s startup, and the tests that pin the scaffold's shape.
/// Design: `docs/design/reverse.md` §3.
/// Up: `run` only.
/// Down: `McpSet::load` for the collision check and `trust_reverse_now` for the record.
/// Verify: `cargo test -p loongagent reverse_scaffold`.
pub(crate) fn scaffold(cwd: &Path) -> Result<PathBuf, String> {
    let dir = cwd.join(REVERSE_DIR);
    std::fs::create_dir_all(dir.join("study")).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::write(dir.join(".gitignore"), "*\n").map_err(|e| format!("{}: {e}", dir.display()))?;

    // A reverse server name colliding with the workspace manifest is a loud error
    // at load time; say it here, before anything binds, with the fix.
    let set = McpSet::load(cwd).map_err(|e| e.to_string())?;
    if set
        .servers
        .iter()
        .any(|s| s.origin == ManifestOrigin::Workspace && s.name == REVERSE_SERVER)
    {
        return Err(format!(
            ".loongagent/mcp.toml already names a server {REVERSE_SERVER}; rename one \
             (the reverse session needs the name for its tool namespace)"
        ));
    }

    let manifest = format!(
        "# Written and self-trusted by `loongagent reverse`; do not edit by hand —\n\
         # the session rewrites it on every start and deletes it on exit.\n\
         [[server]]\n\
         name = \"{REVERSE_SERVER}\"\n\
         command = \"loong-reverse-mcp\"\n"
    );
    std::fs::write(dir.join(MCP_FILE), manifest)
        .map_err(|e| format!("{}: {e}", dir.join(MCP_FILE).display()))?;
    McpSet::load(cwd)
        .map_err(|e| e.to_string())?
        .trust_reverse_now()
        .map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Purpose: Remove the session's manifest pair on exit; the study notes and any
/// database stay behind.
/// Serves: `run`'s exit path (normal and Ctrl+C).
/// Design: `docs/design/reverse.md` §3.
/// Up: `run` only.
/// Down: `std::fs::remove_file`.
/// Verify: `cargo test -p loongagent reverse_cleanup`.
pub(crate) fn cleanup(cwd: &Path) {
    let dir = cwd.join(REVERSE_DIR);
    let _ = std::fs::remove_file(dir.join(MCP_FILE));
    let _ = std::fs::remove_file(dir.join(MCP_TRUST_FILE));
}

/// Purpose: Run the study session: validate the target, scaffold, serve the browser
/// surface until it ends, then clean the manifest pair up (on any exit, including Ctrl+C).
/// Serves: the `reverse` arm of `main.rs`.
/// Design: `docs/design/reverse.md` §4.
/// Up: `main.rs`'s `reverse` arm only.
/// Down: `validate_target`, `scaffold`, `cleanup`, `ChildTransport`, `loong_web`.
/// Verify: `cargo test -p loongagent reverse_` covers the parts a unit test can reach.
pub(crate) fn run(target: &str, args: &crate::Args) -> Result<(), String> {
    // The target is checked before anything binds or scaffolds: a typo in the path
    // must not leave a half-made `.reverse/` behind.
    let kind = validate_target(target)?;
    scaffold(&args.cwd)?;

    // Ctrl+C still cleans up: the handler removes the manifest pair before the
    // process dies, so a killed session never leaves a stale trust record behind.
    install_ctrlc_cleanup(args.cwd.clone())?;

    eprintln!(
        "[reverse] target: {target} ({})",
        match kind {
            TargetKind::File => "file",
            TargetKind::Url => "url",
        }
    );
    eprintln!(
        "[reverse] session files live in {}/",
        args.cwd.join(REVERSE_DIR).display()
    );
    eprintln!("[reverse] the manifest pair is removed when the session ends; study notes stay.");

    // The session itself is the browser surface: the reverse tools ride in through
    // the self-trusted `.reverse/mcp.toml` the scaffold just wrote.
    let result = crate::web::web(args.clone());
    cleanup(&args.cwd);
    result
}

// Ctrl+C during the session runs the handler above, which removes the manifest pair.
// On a normal end the same cleanup runs after the server returns.
#[cfg(unix)]
fn install_ctrlc_cleanup(cwd: PathBuf) -> Result<(), String> {
    // SAFETY: the handler only calls async-signal-safe work through a pipe-less path:
    // it sets no state in this process beyond the files it removes, and removal is a
    // plain syscall. The handler runs once per signal.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = ctrlc_handler as usize;
        libc::sigemptyset(&mut action.sa_mask);
        if libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut()) != 0 {
            return Err("cannot install the Ctrl+C handler".to_string());
        }
    }
    CTRL_C_CWD
        .set(cwd)
        .map_err(|_| "the Ctrl+C handler was already installed".to_string())
}

/// Purpose: The directory the signal handler cleans; set once by `install_ctrlc_cleanup`.
/// Contract: `crates/cli/src/reverse.rs`.
#[cfg(unix)]
static CTRL_C_CWD: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Purpose: The SIGINT handler: remove the manifest pair, then die with the shell's Ctrl+C code.
/// Contract: `crates/cli/src/reverse.rs`.
#[cfg(unix)]
extern "C" fn ctrlc_handler(_sig: i32) {
    if let Some(cwd) = CTRL_C_CWD.get() {
        cleanup(cwd);
    }
    // SAFETY: `_exit` is async-signal-safe; 130 is the conventional SIGINT status.
    unsafe { libc::_exit(130) }
}

/// Purpose: On non-unix targets there is no signal wiring yet; the normal-exit cleanup still runs.
/// Contract: `crates/cli/src/reverse.rs`.
#[cfg(not(unix))]
fn install_ctrlc_cleanup(_cwd: PathBuf) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Purpose: A fresh workspace directory for one test.
    /// Contract: `crates/cli/src/reverse.rs` test fixtures.
    fn workspace() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "loongagent-reverse-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("a test workspace");
        dir
    }

    #[test]
    /// Purpose: Scaffold makes the git-ignored `.reverse/` pair: manifest, trust record, study dir.
    fn reverse_scaffold_creates_gitignored_dirs_and_manifest() {
        let cwd = workspace();
        let dir = scaffold(&cwd).expect("scaffold");
        assert!(dir.join("study").is_dir(), "the study directory exists");
        assert_eq!(
            std::fs::read_to_string(dir.join(".gitignore")).unwrap(),
            "*\n",
            "the whole directory stays out of git"
        );
        let manifest = std::fs::read_to_string(dir.join(MCP_FILE)).unwrap();
        assert!(
            manifest.contains("name = \"loong-reverse\""),
            "the manifest names the session's server: {manifest}"
        );
        assert!(
            dir.join(MCP_TRUST_FILE).is_file(),
            "the session trusts its own manifest"
        );
        // And the loader agrees: the pair is trusted, the server active.
        let set = McpSet::load(&cwd).unwrap();
        assert_eq!(set.active().len(), 1);
        assert_eq!(set.active()[0].origin, ManifestOrigin::Reverse);
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    /// Purpose: Cleanup removes the manifest pair but keeps the study notes.
    fn reverse_cleanup_removes_manifest_but_keeps_study() {
        let cwd = workspace();
        let dir = scaffold(&cwd).expect("scaffold");
        std::fs::write(dir.join("study").join("00-overview.md"), "# notes\n").unwrap();
        cleanup(&cwd);
        assert!(!dir.join(MCP_FILE).exists(), "the manifest is gone");
        assert!(
            !dir.join(MCP_TRUST_FILE).exists(),
            "the trust record is gone"
        );
        assert!(
            dir.join("study").join("00-overview.md").is_file(),
            "the study notes stay"
        );
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    /// Purpose: A missing target is refused before anything binds.
    fn reverse_rejects_missing_target_before_binding() {
        let cwd = workspace();
        let missing = cwd.join("no-such-binary.exe");
        let err = validate_target(missing.to_str().unwrap()).unwrap_err();
        assert!(err.contains("no-such-binary.exe"), "{err}");
        assert!(
            !cwd.join(REVERSE_DIR).exists(),
            "nothing was scaffolded for a bad target"
        );
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    /// Purpose: A workspace manifest that already names `loong-reverse` blocks the session.
    fn reverse_refuses_name_collision_with_workspace_manifest() {
        let cwd = workspace();
        let mcp_dir = cwd.join(loong_mcp::MCP_DIR);
        std::fs::create_dir_all(&mcp_dir).unwrap();
        std::fs::write(
            mcp_dir.join(MCP_FILE),
            "[[server]]\nname = \"loong-reverse\"\ncommand = \"node\"\n",
        )
        .unwrap();
        let err = scaffold(&cwd).unwrap_err();
        assert!(err.contains("loong-reverse"), "{err}");
        assert!(err.contains("rename"), "{err}");
        assert!(
            !cwd.join(REVERSE_DIR).join(MCP_FILE).exists(),
            "no session manifest was written over the collision"
        );
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    /// Purpose: A leftover manifest from a killed run is rewritten without complaint.
    fn reverse_overwrites_stale_manifest_from_killed_run() {
        let cwd = workspace();
        let dir = scaffold(&cwd).expect("first scaffold");
        // A kill -9 left the pair behind: stale trust record, stale manifest.
        std::fs::write(dir.join(MCP_FILE), "# stale\n").unwrap();
        let dir = scaffold(&cwd).expect("the new run overwrites the leftover");
        let manifest = std::fs::read_to_string(dir.join(MCP_FILE)).unwrap();
        assert!(
            manifest.contains("name = \"loong-reverse\""),
            "the manifest is fresh: {manifest}"
        );
        let set = McpSet::load(&cwd).unwrap();
        assert_eq!(
            set.reverse_trust_failure(),
            None,
            "the fresh pair is trusted again"
        );
        std::fs::remove_dir_all(&cwd).ok();
    }
}
