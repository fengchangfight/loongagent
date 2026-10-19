//! Purpose: The two read-only search tools: `grep` for contents and `glob` for paths.
//!
//! # Contract
//! - **Serves:** the default investigate surface, with bounded output.
//! - **Design:** `docs/design/tools-permissions.md` §2 and §6.
//! - **Upstream:** `Registry::with_builtins` and the kernel's dispatch.
//! - **Downstream:** `walk`, `glob_options` and `classify_read_path`.
//! - **Verify:** `cargo test -p loong-tools` · `./scripts/check.sh`
//!
//! Searching: `grep` (contents) and `glob` (paths).
//!
//! Both are **read-only by construction**, so they are investigate actions and
//! free under the default ladder — which is the whole point of splitting autonomy
//! per action. They are also the two tools a coding agent uses most, so they bound
//! their own output: an agent that reads 40 MB of matches has spent its context on
//! noise.
//!
//! The skip list is deliberately small and explicit; `.gitignore` awareness is a
//! later refinement, and pretending to honour it before it is implemented would be
//! worse than saying so.

use crate::{arg_str, classify_read_path, resolve, ExecContext, Tool, ToolError};
use loong_schema::generated::{ActionClass, Json};
use regex::RegexBuilder;
use serde_json::json;
use std::path::{Path, PathBuf};

/// Directories never walked: version control, build output, dependency caches.
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", ".venv", "__pycache__"];
/// Files bigger than this are not searched (a regex over a 200 MB log is a hang).
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// Usable-output budgets, both overridable per call.
const DEFAULT_MAX_RESULTS: usize = 100;
const MAX_RESULTS_CEILING: usize = 2000;
const MAX_LINE_CHARS: usize = 400;

/// Path globbing has to be path-aware: with the crate's default options `*`
/// happily crosses `/`, so `src/*.rs` would match `src/deep/mod.rs`.
/// Purpose: Path-aware glob options, so `*` cannot cross `/`.
///
/// Serves: `Grep`'s name filter and `Glob`'s path match.
/// Design: `docs/design/tools-permissions.md` §6.
/// Up: `Grep::execute` and `Glob::execute`.
/// Down: `glob::MatchOptions`.
/// Verify: `cargo test -p loong-tools`.
pub(crate) fn glob_options() -> glob::MatchOptions {
    glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: false,
    }
}

/// Purpose: The per-call result cap, validated and held under the ceiling.
/// Contract: `crates/tools/src/search.rs`.
fn max_results(args: &Json) -> Result<usize, ToolError> {
    match args.get("max_results").and_then(|v| v.as_u64()) {
        Some(n) if n > 0 => Ok((n as usize).min(MAX_RESULTS_CEILING)),
        Some(_) => Err(ToolError::Refused("max_results must be positive".into())),
        None => Ok(DEFAULT_MAX_RESULTS),
    }
}

/// Walk `root`, yielding files in a deterministic order and never entering the
/// skip list.
/// Purpose: Walk a root in deterministic order, never entering the skip list.
/// Serves: `Grep::execute` and `Glob::execute`.
/// Design: `docs/design/tools-permissions.md` §6.
/// Up: `Grep::execute` and `Glob::execute`.
/// Down: `SKIP_DIRS` and `std::fs::read_dir`.
/// Verify: `cargo test -p loong-tools`.
fn walk(root: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(root)?.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            if SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            walk(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

/// Purpose: Whether a file starts with a NUL byte, or cannot be read.
/// Contract: `crates/tools/src/search.rs`.
fn looks_binary(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return true;
    };
    let mut head = [0u8; 4096];
    match file.read(&mut head) {
        Ok(n) => head[..n].contains(&0),
        Err(_) => true,
    }
}

/// Purpose: A path as the search root sees it, for a report a model can use.
/// Contract: `crates/tools/src/search.rs`.
fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

// ------------------------------------------------------------------- grep

/// Purpose: Search file contents with a regular expression and bounded output.
/// Serves: the default investigate surface for content search.
/// Design: `docs/design/tools-permissions.md` §2 and §6.
/// Up: `Registry::with_builtins` and the kernel's dispatch.
/// Down: `walk`, `max_results` and `classify_read_path`.
/// Verify: `cargo test -p loong-tools`.
pub struct Grep;

impl Tool for Grep {
    /// Purpose: Register this tool as `grep`.
    /// Contract: `crates/tools/src/search.rs`.
    fn name(&self) -> &str {
        "grep"
    }
    /// Purpose: Tell the model the syntax, the result shape and the bounds.
    /// Contract: `crates/tools/src/search.rs`.
    fn description(&self) -> &str {
        "Search file contents with a regular expression (Rust regex syntax). Returns \
         `path:line: text` matches, newest files first class citizens: bounded by \
         `max_results`. Skips .git, build output and binary files."
    }
    /// Purpose: Declare the pattern, path, glob, case and result-cap arguments.
    ///
    /// Serves: the model's view of what a grep call accepts.
    /// Design: `docs/design/tools-permissions.md` §6.
    /// Up: `Registry::specs` when the kernel builds the model's tool list.
    /// Down: `serde_json::json!`.
    /// Verify: `cargo test -p loong-tools`.
    fn parameters(&self) -> Json {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "regular expression"},
                "path": {"type": "string", "description": "file or directory, defaults to the workspace"},
                "glob": {"type": "string", "description": "only search files whose name matches, e.g. *.rs"},
                "ignore_case": {"type": "boolean"},
                "max_results": {"type": "integer"}
            },
            "required": ["pattern"]
        })
    }
    /// Purpose: Classify the search from its path, defaulting to investigate.
    /// Contract: `crates/tools/src/search.rs`.
    fn action_class(&self, args: &Json, cwd: &Path) -> ActionClass {
        match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => classify_read_path(cwd, p),
            None => ActionClass::Investigate,
        }
    }
    /// Purpose: Search the resolved root, skipping build output and binaries, and stop at the cap.
    /// Serves: the content-search answer, with bounded context cost.
    /// Design: `docs/design/tools-permissions.md` §6.
    /// Up: `Kernel::execute` through the registry.
    /// Down: `walk`, `max_results`, `looks_binary`, `relative`.
    /// Verify: `cargo test -p loong-tools`.
    fn execute(&self, args: &Json, ctx: &ExecContext<'_>) -> Result<String, ToolError> {
        let pattern = arg_str(args, "pattern")?;
        let matcher = RegexBuilder::new(&pattern)
            .case_insensitive(
                args.get("ignore_case")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            )
            .build()
            .map_err(|e| ToolError::Refused(format!("bad regular expression: {e}")))?;

        let root = resolve(
            ctx.cwd,
            args.get("path").and_then(|v| v.as_str()).unwrap_or("."),
        );
        let filter = match args.get("glob").and_then(|v| v.as_str()) {
            Some(g) => Some(
                glob::Pattern::new(g)
                    .map_err(|e| ToolError::Refused(format!("bad glob {g:?}: {e}")))?,
            ),
            None => None,
        };
        let limit = max_results(args)?;

        let mut files = Vec::new();
        if root.is_file() {
            files.push(root.clone());
        } else if root.is_dir() {
            walk(&root, &mut files).map_err(|e| ToolError::Failed(e.to_string()))?;
        } else {
            return Err(ToolError::Refused(format!(
                "{} does not exist",
                root.display()
            )));
        }

        let display_root = if root.is_dir() {
            root.clone()
        } else {
            ctx.cwd.to_path_buf()
        };
        let mut hits = Vec::new();
        let mut searched = 0usize;
        let mut skipped_binary = 0usize;
        for file in files {
            if let Some(pattern) = &filter {
                let name = file
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let relative_name = relative(&display_root, &file);
                if !pattern.matches_with(&name, glob_options())
                    && !pattern.matches_with(&relative_name, glob_options())
                {
                    continue;
                }
            }
            let size = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
            if size > MAX_FILE_BYTES || looks_binary(&file) {
                if looks_binary(&file) {
                    skipped_binary += 1;
                }
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            searched += 1;
            for (index, line) in text.lines().enumerate() {
                if matcher.is_match(line) {
                    let mut shown: String = line.trim_end().chars().take(MAX_LINE_CHARS).collect();
                    if line.trim_end().chars().count() > MAX_LINE_CHARS {
                        shown.push('…');
                    }
                    hits.push(format!(
                        "{}:{}: {}",
                        relative(&display_root, &file),
                        index + 1,
                        shown
                    ));
                    if hits.len() >= limit {
                        break;
                    }
                }
            }
            if hits.len() >= limit {
                break;
            }
        }

        if hits.is_empty() {
            return Ok(format!(
                "no matches for {pattern:?} ({} file(s) searched{})",
                searched,
                if skipped_binary > 0 {
                    format!(", {skipped_binary} binary file(s) skipped")
                } else {
                    String::new()
                }
            ));
        }
        let mut report = format!("{} match(es) in {} file(s) searched", hits.len(), searched);
        if hits.len() >= limit {
            report.push_str(&format!(" — stopped at max_results={limit}"));
        }
        report.push('\n');
        report.push_str(&hits.join("\n"));
        Ok(report)
    }
}

// ------------------------------------------------------------------- glob

/// Purpose: List files whose path matches a glob pattern.
/// Serves: the default investigate surface for path search.
/// Design: `docs/design/tools-permissions.md` §2 and §6.
/// Up: `Registry::with_builtins` and the kernel's dispatch.
/// Down: `walk`, `max_results`, `glob_options` and `classify_read_path`.
/// Verify: `cargo test -p loong-tools`.
pub struct Glob;

impl Tool for Glob {
    /// Purpose: Register this tool as `glob`.
    /// Contract: `crates/tools/src/search.rs`.
    fn name(&self) -> &str {
        "glob"
    }
    /// Purpose: Tell the model the `**` depth syntax and the relative result shape.
    /// Contract: `crates/tools/src/search.rs`.
    fn description(&self) -> &str {
        "List files whose path matches a glob pattern (** for any depth), e.g. \
         `**/*.rs` or `src/**/mod.rs`. Returns paths relative to the search root."
    }
    /// Purpose: Declare the pattern, search root and result-cap arguments.
    /// Contract: `crates/tools/src/search.rs`.
    fn parameters(&self) -> Json {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string"},
                "path": {"type": "string", "description": "search root, defaults to the workspace"},
                "max_results": {"type": "integer"}
            },
            "required": ["pattern"]
        })
    }
    /// Purpose: Classify the listing from its path, defaulting to investigate.
    /// Contract: `crates/tools/src/search.rs`.
    fn action_class(&self, args: &Json, cwd: &Path) -> ActionClass {
        match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => classify_read_path(cwd, p),
            None => ActionClass::Investigate,
        }
    }
    /// Purpose: Walk the root, match paths with path-aware globbing and cap the list.
    /// Serves: the path-search answer, with a sorted and bounded result.
    /// Design: `docs/design/tools-permissions.md` §6.
    /// Up: `Kernel::execute` through the registry.
    /// Down: `walk`, `max_results`, `glob_options` and `relative`.
    /// Verify: `cargo test -p loong-tools`.
    fn execute(&self, args: &Json, ctx: &ExecContext<'_>) -> Result<String, ToolError> {
        let pattern_text = arg_str(args, "pattern")?;
        let pattern = glob::Pattern::new(&pattern_text)
            .map_err(|e| ToolError::Refused(format!("bad glob {pattern_text:?}: {e}")))?;
        let root = resolve(
            ctx.cwd,
            args.get("path").and_then(|v| v.as_str()).unwrap_or("."),
        );
        if !root.is_dir() {
            return Err(ToolError::Refused(format!(
                "{} is not a directory",
                root.display()
            )));
        }
        let limit = max_results(args)?;

        let mut files = Vec::new();
        walk(&root, &mut files).map_err(|e| ToolError::Failed(e.to_string()))?;
        let mut matches: Vec<String> = files
            .iter()
            .map(|f| relative(&root, f))
            // Path-aware matching only: plain `matches` would let `*` cross `/`,
            // so `src/*.rs` would wrongly include `src/deep/mod.rs`.
            .filter(|rel| pattern.matches_with(rel, glob_options()))
            .collect();
        matches.sort();
        let total = matches.len();
        matches.truncate(limit);
        if matches.is_empty() {
            return Ok(format!(
                "no files match {pattern_text:?} under {}",
                root.display()
            ));
        }
        let mut report = format!("{total} file(s) match {pattern_text:?}");
        if total > limit {
            report.push_str(&format!(" — showing the first {limit}"));
        }
        report.push('\n');
        report.push_str(&matches.join("\n"));
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NoConfinement;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    /// Purpose: Build a temporary tree with matches, a binary, and skipped directories.
    /// Serves: the search tests' fixture.
    /// Design: `docs/design/tools-permissions.md` §6.
    /// Up: every test in this file.
    /// Down: `std::fs` under the temp directory.
    /// Verify: `cargo test -p loong-tools`.
    fn workspace() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "loong-search-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(dir.join("src/deep")).unwrap();
        std::fs::write(
            dir.join("src/main.rs"),
            "fn main() {\n    let needle = 1;\n}\n",
        )
        .unwrap();
        std::fs::write(dir.join("src/deep/mod.rs"), "// needle appears here too\n").unwrap();
        std::fs::write(dir.join("README.md"), "# needle in a haystack\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "NEEDLE uppercase\n").unwrap();
        std::fs::write(dir.join("blob.bin"), [0u8, 159, 146, 150]).unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("target/junk.rs"), "needle in build output\n").unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".git/config"), "needle in git\n").unwrap();
        dir
    }

    /// Purpose: Build an `ExecContext` for a test.
    /// Contract: `crates/tools/src/search.rs`.
    fn ctx<'a>(cwd: &'a Path, c: &'a dyn crate::Confinement) -> ExecContext<'a> {
        ExecContext::new(cwd, c)
    }

    #[test]
    /// Purpose: Pin that grep reports path, line and text.
    fn grep_finds_matches_with_file_and_line() {
        let ws = workspace();
        let c = NoConfinement;
        let out = Grep
            .execute(&json!({"pattern": "needle"}), &ctx(&ws, &c))
            .unwrap();
        assert!(out.contains("src/main.rs:2:"), "{out}");
        assert!(out.contains("src/deep/mod.rs:1:"), "{out}");
        assert!(out.contains("README.md:1:"), "{out}");
    }

    #[test]
    /// Purpose: Pin the skip list and the binary-file skip.
    fn grep_skips_build_output_version_control_and_binaries() {
        let ws = workspace();
        let c = NoConfinement;
        let out = Grep
            .execute(&json!({"pattern": "needle"}), &ctx(&ws, &c))
            .unwrap();
        assert!(
            !out.contains("target/junk.rs"),
            "target/ is not searched: {out}"
        );
        assert!(!out.contains(".git/config"), ".git is not searched: {out}");
        assert!(!out.contains("blob.bin"), "binary files are skipped: {out}");
    }

    #[test]
    /// Purpose: Pin the case-sensitivity default and the name glob filter.
    fn grep_honours_case_and_glob_filters() {
        let ws = workspace();
        let c = NoConfinement;
        let sensitive = Grep
            .execute(&json!({"pattern": "needle"}), &ctx(&ws, &c))
            .unwrap();
        assert!(!sensitive.contains("notes.txt"), "case matters by default");

        let insensitive = Grep
            .execute(
                &json!({"pattern": "needle", "ignore_case": true}),
                &ctx(&ws, &c),
            )
            .unwrap();
        assert!(insensitive.contains("notes.txt"), "{insensitive}");

        let only_rust = Grep
            .execute(&json!({"pattern": "needle", "glob": "*.rs"}), &ctx(&ws, &c))
            .unwrap();
        assert!(only_rust.contains("src/main.rs"));
        assert!(!only_rust.contains("README.md"), "{only_rust}");
    }

    #[test]
    /// Purpose: Pin that grep stops at `max_results` and says so.
    fn grep_bounds_its_output() {
        let ws = workspace();
        let c = NoConfinement;
        let out = Grep
            .execute(
                &json!({"pattern": "needle", "max_results": 1}),
                &ctx(&ws, &c),
            )
            .unwrap();
        assert!(out.contains("stopped at max_results=1"), "{out}");
        assert_eq!(
            out.lines().count(),
            2,
            "a header plus exactly one match: {out}"
        );
    }

    #[test]
    /// Purpose: Pin that no matches is an answer, not an error.
    fn grep_reports_no_matches_without_failing() {
        let ws = workspace();
        let c = NoConfinement;
        let out = Grep
            .execute(&json!({"pattern": "nothing-here"}), &ctx(&ws, &c))
            .unwrap();
        assert!(out.starts_with("no matches"), "{out}");
    }

    #[test]
    /// Purpose: Pin grep's refusals for a bad regex, a missing root and a missing pattern.
    fn grep_refuses_a_bad_expression_and_a_missing_path() {
        let ws = workspace();
        let c = NoConfinement;
        assert!(matches!(
            Grep.execute(&json!({"pattern": "([unclosed"}), &ctx(&ws, &c)),
            Err(ToolError::Refused(_))
        ));
        assert!(matches!(
            Grep.execute(&json!({"pattern": "x", "path": "nope"}), &ctx(&ws, &c)),
            Err(ToolError::Refused(_))
        ));
        assert!(matches!(
            Grep.execute(&json!({}), &ctx(&ws, &c)),
            Err(ToolError::Refused(_))
        ));
    }

    #[test]
    /// Purpose: Pin grep's default class and its outside-path class.
    fn grep_is_an_investigate_action_but_follows_the_path() {
        let cwd = Path::new("/work");
        assert_eq!(
            Grep.action_class(&json!({"pattern": "x"}), cwd),
            ActionClass::Investigate
        );
        assert_eq!(
            Grep.action_class(&json!({"pattern": "x", "path": "/etc"}), cwd),
            ActionClass::OutOfBounds
        );
    }

    #[test]
    /// Purpose: Pin depth matching, top-level matching and that `*` does not cross directories.
    fn glob_matches_at_depth_and_filters() {
        let ws = workspace();
        let c = NoConfinement;
        let rust = Glob
            .execute(&json!({"pattern": "**/*.rs"}), &ctx(&ws, &c))
            .unwrap();
        assert!(rust.contains("src/main.rs"), "{rust}");
        assert!(rust.contains("src/deep/mod.rs"), "{rust}");
        assert!(!rust.contains("README.md"), "{rust}");
        assert!(
            !rust.contains("target/junk.rs"),
            "build output is not listed: {rust}"
        );

        let top = Glob
            .execute(&json!({"pattern": "*.txt"}), &ctx(&ws, &c))
            .unwrap();
        assert!(top.contains("notes.txt"), "{top}");
        assert!(!top.contains("src/deep/mod.rs"), "{top}");

        let single = Glob
            .execute(&json!({"pattern": "src/*.rs"}), &ctx(&ws, &c))
            .unwrap();
        assert!(single.contains("src/main.rs"), "{single}");
        assert!(
            !single.contains("src/deep/mod.rs"),
            "`*` does not cross directories: {single}"
        );
    }

    #[test]
    /// Purpose: Pin glob's cap, its empty answer and its refusals.
    fn glob_bounds_and_reports_emptiness() {
        let ws = workspace();
        let c = NoConfinement;
        let capped = Glob
            .execute(
                &json!({"pattern": "**/*.rs", "max_results": 1}),
                &ctx(&ws, &c),
            )
            .unwrap();
        assert!(capped.contains("showing the first 1"), "{capped}");

        let none = Glob
            .execute(&json!({"pattern": "**/*.py"}), &ctx(&ws, &c))
            .unwrap();
        assert!(none.starts_with("no files match"), "{none}");

        assert!(matches!(
            Glob.execute(&json!({"pattern": "["}), &ctx(&ws, &c)),
            Err(ToolError::Refused(_))
        ));
        assert!(matches!(
            Glob.execute(
                &json!({"pattern": "**/*.rs", "path": "src/main.rs"}),
                &ctx(&ws, &c)
            ),
            Err(ToolError::Refused(_))
        ));
    }
}
