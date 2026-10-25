//! Purpose: Discover the instruction files a session should read, and hand the kernel filled blocks.
//!
//! # Contract
//! - **Serves:** `crates/cli/src/server.rs`'s `build_kernel`, once per session.
//! - **Design:** `docs/design/instructions.md` §1, §2 and §4.
//! - **Upstream:** `build_kernel`, which passes `--cwd` and the user's home.
//! - **Downstream:** `loong_kernel::InstructionBlock` and `KernelConfig.instructions`.
//! - **Verify:** `cargo test -p loongagent instructions` · `./scripts/check.sh`.
//!
use loong_kernel::{InstructionBlock, InstructionScope};
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The hard cap for one instruction file.
pub(crate) const MAX_FILE_BYTES: usize = 32 * 1024;

/// The hard cap across every instruction file in one session.
pub(crate) const MAX_TOTAL_BYTES: usize = 64 * 1024;

/// Purpose: What one discovery pass produced: the blocks, what it could not read, and a summary.
///
/// Serves: `build_kernel` setting `KernelConfig.instructions` and printing the startup line.
/// Design: `docs/design/instructions.md` §4.
/// Up: `load` builds it.
/// Down: `build_kernel` consumes it.
/// Verify: `cargo test -p loongagent instructions::tests`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoadedInstructions {
    /// The files that loaded, least specific first.
    pub blocks: Vec<InstructionBlock>,
    /// One line per file that was found but could not be used.
    pub warnings: Vec<String>,
    /// `N files, M bytes`, for the startup line.
    pub summary: String,
}

/// Purpose: Read the user, project and local instruction files in order.
///
/// Serves: a session that starts with the workspace's instructions already in context.
/// Design: `docs/design/instructions.md` §1.
/// Up: `crates/cli/src/server.rs`'s `build_kernel`.
/// Down: `candidates`, `project_dirs` and `read_capped`.
/// Verify: `cargo test -p loongagent instructions::tests`.
pub(crate) fn load(workspace: &Path, home: &Path) -> LoadedInstructions {
    let workspace = canonical(workspace);
    let project_root = git_root(&workspace).unwrap_or_else(|| workspace.clone());
    let mut blocks = Vec::new();
    let mut warnings = Vec::new();
    let mut seen = HashSet::new();
    let mut total = 0usize;
    for (scope, path) in candidates(&workspace, home) {
        let resolved = canonical(&path);
        if !seen.insert(resolved.clone()) {
            continue;
        }
        // A repository must not use a symlink to make the agent read a file
        // outside its own tree. The user's own file is the exception: it lives
        // outside the workspace by design.
        if scope != InstructionScope::User {
            let boundary = if scope == InstructionScope::Project {
                &project_root
            } else {
                &workspace
            };
            if !resolved.starts_with(boundary) {
                warnings.push(format!(
                    "{}: points outside {}",
                    path.display(),
                    boundary.display()
                ));
                continue;
            }
        }
        let remaining = MAX_TOTAL_BYTES.saturating_sub(total);
        if remaining == 0 {
            warnings.push(format!(
                "{}: skipped, the 64 KiB total cap is full",
                path.display()
            ));
            continue;
        }
        match read_capped(&path, MAX_FILE_BYTES.min(remaining)) {
            Ok(Some(text)) => {
                if text.trim().is_empty() {
                    continue;
                }
                total += text.len();
                blocks.push(InstructionBlock {
                    scope,
                    source: path.display().to_string(),
                    text,
                });
            }
            Ok(None) => {}
            Err(error) => warnings.push(format!("{}: {error}", path.display())),
        }
    }
    let count = blocks.len();
    LoadedInstructions {
        summary: format!(
            "{} file{}, {} bytes",
            count,
            if count == 1 { "" } else { "s" },
            total
        ),
        blocks,
        warnings,
    }
}

/// Purpose: The candidate files in load order, before deduplication or reading.
///
/// Contract: `instructions.rs` loader.
fn candidates(workspace: &Path, home: &Path) -> Vec<(InstructionScope, PathBuf)> {
    let mut out = vec![(InstructionScope::User, home.join("AGENTS.md"))];
    for dir in project_dirs(workspace) {
        out.push((InstructionScope::Project, dir.join("AGENTS.md")));
    }
    out.push((InstructionScope::Local, workspace.join("AGENTS.local.md")));
    out
}

/// Purpose: Every directory from the git root down to the workspace, or just the workspace.
///
/// Serves: `candidates`, which joins each directory with `AGENTS.md`.
/// Design: `docs/design/instructions.md` §1.
/// Up: `candidates`.
/// Down: `git_root`.
/// Verify: `cargo test -p loongagent instructions::tests`.
fn project_dirs(workspace: &Path) -> Vec<PathBuf> {
    let Some(root) = git_root(workspace) else {
        return vec![workspace.to_path_buf()];
    };
    let mut dirs = Vec::new();
    let mut current = Some(workspace);
    while let Some(dir) = current {
        dirs.push(dir.to_path_buf());
        if dir == root {
            break;
        }
        current = dir.parent();
    }
    dirs.reverse();
    dirs
}

/// Purpose: The nearest ancestor that is a git root, marked by a `.git` directory or file.
///
/// Contract: `instructions.rs` loader.
fn git_root(workspace: &Path) -> Option<PathBuf> {
    workspace
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

/// Purpose: A path with symlinks resolved when possible, and unchanged when it does not exist.
///
/// Contract: `instructions.rs` de-duplication.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Purpose: Read up to `limit` bytes of one file, cutting on a character boundary.
///
/// Serves: `load`, which needs a regular, decodable, capped body.
/// Design: `docs/design/instructions.md` §4.
/// Up: `load`.
/// Down: `std::fs::metadata`, `File::open` and `String::from_utf8`.
/// Verify: `cargo test -p loongagent instructions::tests`.
fn read_capped(path: &Path, limit: usize) -> Result<Option<String>, String> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if !metadata.is_file() {
        // Directories, FIFOs, sockets and devices are not instruction files;
        // opening a FIFO would block the session before its first turn.
        return Err("not a regular file".to_string());
    }
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let mut bytes = Vec::new();
    file.take(limit as u64 + 4)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    let truncated = bytes.len() > limit;
    let head = &bytes[..limit.min(bytes.len())];
    let text = match std::str::from_utf8(head) {
        Ok(text) => text.to_string(),
        Err(error) if error.error_len().is_none() && truncated => {
            // The cap landed inside a character: keep the complete prefix.
            String::from_utf8_lossy(&head[..error.valid_up_to()]).into_owned()
        }
        Err(_) => return Err("not valid UTF-8".to_string()),
    };
    if truncated {
        Ok(Some(format!(
            "{text}\n\n[loongagent: truncated at {limit} bytes]"
        )))
    } else {
        Ok(Some(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    /// Purpose: A temporary tree with helpers for writing the files a test needs.
    ///
    /// Serves: every loader test that needs a workspace and a home on disk.
    /// Design: `docs/design/instructions.md` §1.
    /// Up: the loader tests.
    /// Down: `Tree::path`, `Tree::write` and `Tree::mkdir`.
    /// Verify: `cargo test -p loongagent instructions::tests`.
    struct Tree {
        root: PathBuf,
    }

    impl Tree {
        /// Purpose: Create a fresh tree.
        ///
        /// Contract: `instructions.rs` test fixture.
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "loong-instructions-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir_all(&root).unwrap();
            Tree { root }
        }

        /// Purpose: A path inside the tree.
        ///
        /// Contract: `instructions.rs` test fixture.
        fn path(&self, rel: &str) -> PathBuf {
            self.root.join(rel)
        }

        /// Purpose: Write a file inside the tree, creating its parents.
        ///
        /// Contract: `instructions.rs` test fixture.
        fn write(&self, rel: &str, text: &str) {
            let path = self.path(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }

        /// Purpose: Create a directory inside the tree.
        ///
        /// Contract: `instructions.rs` test fixture.
        fn mkdir(&self, rel: &str) {
            std::fs::create_dir_all(self.path(rel)).unwrap();
        }
    }

    impl Drop for Tree {
        /// Purpose: Remove the tree when the test ends.
        ///
        /// Contract: `instructions.rs` test fixture.
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// Purpose: User, project and local files load least-specific first.
    #[test]
    fn user_project_and_local_load_in_order() {
        let tree = Tree::new();
        tree.write("home/AGENTS.md", "USER");
        tree.mkdir("repo/.git");
        tree.write("repo/AGENTS.md", "ROOT");
        tree.write("repo/sub/AGENTS.md", "SUB");
        tree.write("repo/sub/AGENTS.local.md", "LOCAL");

        let loaded = load(&tree.path("repo/sub"), &tree.path("home"));
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        let seen: Vec<(InstructionScope, &str)> = loaded
            .blocks
            .iter()
            .map(|block| (block.scope, block.text.as_str()))
            .collect();
        assert_eq!(
            seen,
            vec![
                (InstructionScope::User, "USER"),
                (InstructionScope::Project, "ROOT"),
                (InstructionScope::Project, "SUB"),
                (InstructionScope::Local, "LOCAL"),
            ]
        );
    }

    /// Purpose: A file above the git root is outside the project and is not read.
    #[test]
    fn a_file_above_the_git_root_is_not_read() {
        let tree = Tree::new();
        tree.write("AGENTS.md", "OUTSIDE");
        tree.mkdir("repo/.git");
        tree.write("repo/AGENTS.md", "ROOT");

        let loaded = load(&tree.path("repo"), &tree.path("home"));
        assert_eq!(loaded.blocks.len(), 1);
        assert_eq!(loaded.blocks[0].text, "ROOT");
    }

    /// Purpose: With no `.git`, only the working directory is a project root.
    #[test]
    fn no_git_reads_only_the_working_directory() {
        let tree = Tree::new();
        tree.write("AGENTS.md", "OUTSIDE");
        tree.write("work/AGENTS.md", "INSIDE");

        let loaded = load(&tree.path("work"), &tree.path("home"));
        assert_eq!(loaded.blocks.len(), 1);
        assert_eq!(loaded.blocks[0].text, "INSIDE");
    }

    /// Purpose: A worktree's `.git` file marks the root just as a directory does.
    #[test]
    fn git_file_marks_the_root() {
        let tree = Tree::new();
        tree.write("repo/.git", "gitdir: elsewhere");
        tree.write("repo/AGENTS.md", "ROOT");
        tree.write("repo/sub/AGENTS.md", "SUB");

        let loaded = load(&tree.path("repo/sub"), &tree.path("home"));
        let texts: Vec<&str> = loaded
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect();
        assert_eq!(texts, vec!["ROOT", "SUB"]);
    }

    /// Purpose: A path discovered as both user and project is read once.
    #[test]
    fn duplicate_path_is_read_once() {
        let tree = Tree::new();
        tree.mkdir("repo/.git");
        tree.write("repo/AGENTS.md", "ONCE");

        let loaded = load(&tree.path("repo"), &tree.path("repo"));
        assert_eq!(loaded.blocks.len(), 1);
        assert_eq!(loaded.blocks[0].text, "ONCE");
    }

    /// Purpose: A file over the cap is cut at a character boundary and says it was cut.
    #[test]
    fn truncates_at_the_file_cap() {
        let tree = Tree::new();
        let body = format!("{}😀tail", "a".repeat(MAX_FILE_BYTES - 2));
        tree.write("home/AGENTS.md", &body);

        let loaded = load(&tree.path("work"), &tree.path("home"));
        assert_eq!(loaded.blocks.len(), 1);
        let text = &loaded.blocks[0].text;
        assert!(
            text.ends_with("[loongagent: truncated at 32768 bytes]"),
            "{text}"
        );
        assert!(
            !text.contains('\u{FFFD}'),
            "the cut never produces a replacement character: {text}"
        );
    }

    /// Purpose: A file that is not UTF-8 is skipped with a warning, not read lossily.
    #[test]
    fn skips_a_non_utf8_file() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.path("home")).unwrap();
        std::fs::write(tree.path("home/AGENTS.md"), [0xff, 0xfe]).unwrap();

        let loaded = load(&tree.path("work"), &tree.path("home"));
        assert!(loaded.blocks.is_empty());
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].contains("AGENTS.md"));
    }

    /// Purpose: An empty or whitespace-only file contributes no block.
    #[test]
    fn skips_an_empty_file() {
        let tree = Tree::new();
        tree.write("home/AGENTS.md", "   \n");

        let loaded = load(&tree.path("work"), &tree.path("home"));
        assert!(loaded.blocks.is_empty());
        assert!(loaded.warnings.is_empty());
    }

    /// Purpose: Once the total cap is spent, a later file is skipped with a warning.
    #[test]
    fn stops_at_the_total_cap() {
        let tree = Tree::new();
        tree.write("home/AGENTS.md", &"u".repeat(MAX_FILE_BYTES));
        tree.mkdir("repo/.git");
        tree.write("repo/AGENTS.md", &"r".repeat(MAX_FILE_BYTES));
        tree.write("repo/sub/AGENTS.md", "x");

        let loaded = load(&tree.path("repo/sub"), &tree.path("home"));
        assert_eq!(loaded.blocks.len(), 2);
        assert!(
            loaded
                .warnings
                .iter()
                .any(|warning| warning.contains("sub/AGENTS.md")),
            "{:?}",
            loaded.warnings
        );
    }

    /// Purpose: A directory named like an instruction file is a warning, not a crash.
    #[test]
    fn a_directory_named_agents_md_warns() {
        let tree = Tree::new();
        tree.mkdir("home/AGENTS.md");

        let loaded = load(&tree.path("work"), &tree.path("home"));
        assert!(loaded.blocks.is_empty());
        assert_eq!(loaded.warnings.len(), 1);
    }

    /// Purpose: A project symlink that leaves the git root is skipped instead of reading an outside file.
    #[cfg(unix)]
    #[test]
    fn a_project_symlink_outside_the_git_root_is_not_read() {
        use std::os::unix::fs::symlink;

        let tree = Tree::new();
        tree.mkdir("repo/.git");
        tree.write("outside/secret.md", "SECRET");
        symlink(tree.path("outside/secret.md"), tree.path("repo/AGENTS.md")).unwrap();

        let loaded = load(&tree.path("repo"), &tree.path("home"));
        assert!(loaded.blocks.is_empty());
        assert!(
            loaded
                .warnings
                .iter()
                .any(|warning| warning.contains("points outside")),
            "{:?}",
            loaded.warnings
        );
    }

    /// Purpose: A FIFO named AGENTS.md is rejected by metadata, so session build cannot block opening it.
    #[cfg(unix)]
    #[test]
    fn a_fifo_is_not_opened() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let tree = Tree::new();
        tree.mkdir("home");
        let path = tree.path("home/AGENTS.md");
        let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
        // Safety: `mkfifo` only creates a directory entry at a valid C path.
        let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
        assert_eq!(rc, 0, "mkfifo {}", path.display());

        let loaded = load(&tree.path("work"), &tree.path("home"));
        assert!(loaded.blocks.is_empty());
        assert_eq!(loaded.warnings.len(), 1);
    }
}
