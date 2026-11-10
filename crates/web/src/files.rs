//! Purpose: The workspace's file list behind the composer's `@` picker, ranked so the best match is first.
//!
//! # Contract
//! - **Serves:** the `/api/files` route, which the page's `@` picker calls on every keystroke.
//! - **Design:** `docs/design/web-composer.md` §2.
//! - **Upstream:** `handle_web`'s `/api/files` arm, with the seat's directory resolved.
//! - **Downstream:** `git ls-files` in the workspace, or the bounded walk when it is not a repository.
//! - **Verify:** `cargo test -p loong-web` (`files::tests`) · `./scripts/check.sh`
//!
//! The ranking is the TUI picker's rule (`crates/tui-app/src/mention.rs`), restated here:
//! ADR-0004's layering puts the terminal surface and the portal on separate branches,
//! with no crate below both, so the portal cannot call that one. The rule is the same on
//! purpose — the same keystrokes must produce the same first row in both surfaces — and
//! the tests below pin the decisions rather than the implementation.

use std::path::Path;

/// Directories no workspace wants offered, git repository or not.
const ALWAYS_IGNORED: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".loongagent",
];

/// The most rows an answer carries by default, so a page cannot ask for the whole tree.
const MAX_MATCHES: usize = 10;

/// Purpose: The ranked answer to one `@` query, as the JSON body the route returns.
/// Serves: `crate::files`'s route arm (`/api/files`).
/// Design: `docs/design/web-composer.md` §2.
/// Up: `handle_web`, with the seat's directory, the typed query and the asked limit.
/// Down: `read` and `matches`.
/// Verify: `cargo test -p loong-web` (`files::tests`).
/// `{"files": ["src/main.rs", ...]}` — best match first, capped at `limit`.
pub fn answer(root: &Path, query: &str, limit: usize) -> String {
    let files = read(root);
    let shown = matches(&files, query, limit.clamp(1, MAX_MATCHES));
    serde_json::json!({ "files": shown }).to_string()
}

/// Purpose: The workspace's files: git's list when it has one, a bounded walk otherwise.
/// Serves: `answer`.
/// Design: `docs/design/web-composer.md` §2.
/// Up: `answer`, once per request.
/// Down: `git_files` and `walk_files`.
/// Verify: `cargo test -p loong-web` (`files::tests`).
fn read(root: &Path) -> Vec<String> {
    git_files(root).unwrap_or_else(|| walk_files(root))
}

/// Purpose: The workspace's files according to git: tracked, plus untracked-and-not-ignored.
/// Serves: `read`.
/// Design: `docs/design/web-composer.md` §2.
/// Up: `read`.
/// Down: the `git` child process.
/// Verify: `cargo test -p loong-web` (`files::tests`).
/// `None` when git is unavailable, this is not a repository, or the repository is empty,
/// so the caller falls back to the walk instead of showing nothing.
fn git_files(root: &Path) -> Option<Vec<String>> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let mut files: Vec<String> = text
        .split('\0')
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect();
    if files.is_empty() {
        return None;
    }
    files.sort();
    Some(files)
}

/// Purpose: A bounded walk of the tree, for a workspace that is not a git repository.
/// Serves: `read`'s fallback.
/// Design: `docs/design/web-composer.md` §2.
/// Up: `read`.
/// Down: `std::fs::read_dir` and `ALWAYS_IGNORED`.
/// Verify: `cargo test -p loong-web` (`files::tests`).
fn walk_files(root: &Path) -> Vec<String> {
    /// Deep enough for a real project, shallow enough that a stray symlink cannot
    /// spend the request walking `/`.
    const MAX_DEPTH: usize = 8;
    const MAX_FILES: usize = 20_000;

    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > MAX_DEPTH || out.len() >= MAX_FILES {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if ALWAYS_IGNORED.contains(&name.as_str()) {
                continue;
            }
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                stack.push((path, depth + 1));
            } else if kind.is_file() {
                if let Ok(relative) = path.strip_prefix(root) {
                    out.push(relative.to_string_lossy().replace('\\', "/"));
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Purpose: Files whose path matches the query, best first, capped at `limit`.
/// Serves: `answer`, and the tests that pin the ordering.
/// Design: `docs/design/web-composer.md` §2.
/// Up: `answer`.
/// Down: `rank`.
/// Verify: `cargo test -p loong-web` (`files::tests`).
/// Ranked by *where* the query matches rather than by a fuzzy score: the basename
/// counts for more than a path segment, and an earlier position counts for more than
/// a later one. That is enough to put `src/main.rs` above `src/vendor/main_helper.rs`
/// for `main`, which is the decision that matters; a score nobody can predict would
/// be worse than one they can.
fn matches(files: &[String], query: &str, limit: usize) -> Vec<String> {
    let query = query.to_ascii_lowercase();
    let mut scored: Vec<(u32, &String)> = Vec::new();
    for file in files {
        if let Some(rank) = rank(file, &query) {
            scored.push((rank, file));
        }
    }
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.len().cmp(&b.1.len())));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, file)| file.clone())
        .collect()
}

/// Purpose: Score one file against one query; a smaller rank is a better match, `None` is no match.
/// Serves: `matches`.
/// Design: `docs/design/web-composer.md` §2.
/// Up: `matches`.
/// Down: `subsequence_rank`.
/// Verify: `cargo test -p loong-web` (`files::tests`).
fn rank(file: &str, query: &str) -> Option<u32> {
    if query.is_empty() {
        // Nothing typed yet: shallow paths first, because they are the ones a person
        // is most likely to want and they fit on one line.
        return Some((file.matches('/').count() as u32) * 100 + file.len() as u32);
    }
    let lowered = file.to_ascii_lowercase();
    let basename = lowered.rsplit('/').next().unwrap_or(&lowered);

    // A prefix on the file's own name is the strongest signal.
    if basename.starts_with(query) {
        return Some(basename.len() as u32);
    }
    if let Some(at) = basename.find(query) {
        return Some(100 + at as u32);
    }
    // Then anywhere in the path, earlier and shallower winning.
    if let Some(at) = lowered.find(query) {
        return Some(1_000 + at as u32 + (lowered.matches('/').count() as u32) * 10);
    }
    // Finally, a subsequence match, so `skv` finds `src/kernel/view.rs`. Ranked
    // last, and only when the characters appear in order — a picker that offered
    // everything would be a picker nobody trusts.
    subsequence_rank(&lowered, query).map(|spread| 10_000 + spread)
}

/// Purpose: Score an in-order subsequence match by the spread of its positions.
/// Serves: `rank`'s last resort.
/// Design: `docs/design/web-composer.md` §2.
/// Up: `rank`.
/// Down: no call; it walks the lowercased path.
/// Verify: `cargo test -p loong-web` (`files::tests`).
fn subsequence_rank(haystack: &str, needle: &str) -> Option<u32> {
    let mut haystack = haystack.chars().enumerate();
    // The haystack index of each matched character, so the spread is the distance
    // between *positions in the path* rather than between needle offsets.
    let mut matched: Vec<u32> = Vec::new();
    for want in needle.chars() {
        loop {
            let (at, c) = haystack.next()?;
            if c == want {
                matched.push(at as u32);
                break;
            }
        }
    }
    Some(matched.windows(2).map(|pair| pair[1] - pair[0]).sum())
}

#[cfg(test)]
/// Purpose: Pin the picker's ordering and the two file sources, so the page's first row is predictable.
/// Serves: `cargo test -p loong-web`.
/// Design: `docs/design/web-composer.md` §2.
/// Up: nothing calls it; the gate does.
/// Down: `answer`, `matches`, `rank` and `read`.
/// Verify: `cargo test -p loong-web`.
mod tests {
    use super::*;

    /// Purpose: A workspace that is not a repository is walked, and ignored directories stay out.
    /// Serves: `read`'s fallback.
    /// Design: `docs/design/web-composer.md` §2.
    /// Up: the gate.
    /// Down: `walk_files`.
    /// Verify: `cargo test -p loong-web`.
    #[test]
    fn the_walk_offers_files_and_skips_what_is_never_wanted() {
        let root = std::env::temp_dir().join(format!("loong-files-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("src/main.rs"), b"fn main() {}").unwrap();
        std::fs::write(root.join("target/ignored.rs"), b"x").unwrap();

        let files = walk_files(&root);
        std::fs::remove_dir_all(&root).unwrap();

        assert!(files.iter().any(|f| f == "src/main.rs"), "{files:?}");
        assert!(
            !files.iter().any(|f| f.starts_with("target/")),
            "an ignored directory was offered: {files:?}"
        );
    }

    /// Purpose: Pins that the best match is the one a person would name, not the alphabetically first.
    /// Serves: `matches`' ordering.
    /// Design: `docs/design/web-composer.md` §2.
    /// Up: the gate.
    /// Down: `matches` and `rank`.
    /// Verify: `cargo test -p loong-web`.
    #[test]
    fn a_basename_match_outranks_a_longer_path() {
        let files: Vec<String> = [
            "src/vendor/main_helper.rs",
            "src/main.rs",
            "docs/maintenance.md",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        assert_eq!(matches(&files, "main", 10)[0], "src/main.rs");
        assert_eq!(
            matches(&files, "helper", 10),
            vec!["src/vendor/main_helper.rs"]
        );
    }

    /// Purpose: Pins the subsequence rule that makes an abbreviation work, and that it ranks last.
    /// Serves: `rank`'s last resort.
    /// Design: `docs/design/web-composer.md` §2.
    /// Up: the gate.
    /// Down: `subsequence_rank`.
    /// Verify: `cargo test -p loong-web`.
    #[test]
    fn an_out_of_order_letter_run_finds_nothing() {
        let files: Vec<String> = ["src/kernel/view.rs"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        assert_eq!(matches(&files, "skv", 10)[0], "src/kernel/view.rs");
        assert!(matches(&files, "vks", 10).is_empty(), "order must matter");
    }

    /// Purpose: Pins the cap, so a page cannot ask for the whole tree in one answer.
    /// Serves: `answer`.
    /// Design: `docs/design/web-composer.md` §2.
    /// Up: the gate.
    /// Down: `answer`.
    /// Verify: `cargo test -p loong-web`.
    #[test]
    fn the_answer_is_capped_and_is_json() {
        let root = std::env::temp_dir().join(format!("loong-files-cap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for n in 0..40 {
            std::fs::write(root.join(format!("f{n}.txt")), b"x").unwrap();
        }

        let body = answer(&root, "", 500);
        std::fs::remove_dir_all(&root).unwrap();
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(value["files"].as_array().unwrap().len(), MAX_MATCHES);
    }
}
