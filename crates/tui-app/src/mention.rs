//! Purpose: The `@` file reference: parse the token, rank the workspace's files, and let the picker insert a path.
//!
//! # Contract
//! - **Serves:** `App::refresh_picker` and `App::accept_picker`, which draw and finish the picker, and the picker tests.
//! - **Design:** `docs/design/surfaces.md` §3 and §6.
//! - **Upstream:** `App` in `crates/tui-app/src/lib.rs` builds a `FileIndex` and a `Picker` from the prompt text and caret.
//! - **Downstream:** `git ls-files` (or a bounded walk) for the file list, and the ranking functions in this file.
//! - **Verify:** `cargo test -p loong-tui-app` · `./scripts/check.sh`
//!
//! `@` file references in the prompt (the picker behind `@`).
//!
//! # What `@` does here, and what it deliberately does not
//!
//! Choosing a file inserts its **path as text** into the prompt: `@src/main.rs`.
//! It does not read the file and paste its contents. Two reasons, and the second
//! is the one that decides it:
//!
//! * The model already has a `read_file` tool, and a path is what it needs to call
//!   it. Pasting contents would duplicate a file into the transcript that the model
//!   can fetch, edit, and cite.
//! * **Context is the scarce resource this whole project is organised around.** A
//!   completion that silently attached a file would spend tokens the person never
//!   chose to spend, and nothing in the ledger would explain where they went. A path
//!   is a reference; contents are a budget decision.
//!
//! Whether the *agent* should also expand `@path` into a read is a kernel question,
//! not a keybinding one, and it is not decided here — this module only makes the
//! path easy to type.
//!
//! # Why `git ls-files` rather than walking the tree
//!
//! A workspace is full of files nobody wants offered: `target/`, `node_modules/`,
//! and whatever else the project already decided to ignore. Git knows all of that,
//! because the project already told it — so the picker asks git instead of
//! reimplementing an ignore grammar. It also lists untracked-but-not-ignored files,
//! which is exactly the set a person is about to `@` (a file they just created).
//! With no git available, a bounded walk with the same exclusions is the fallback.

use std::path::{Path, PathBuf};

/// The character that starts a reference.
pub const TRIGGER: char = '@';

/// How many matches the picker offers at once. A list longer than the space it is
/// drawn in is a list nobody reads.
pub const MAX_MATCHES: usize = 10;

/// Directories never offered even without git's help. Kept short on purpose: git
/// is the real answer, and a long hand-written list is one that drifts.
const ALWAYS_IGNORED: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    "dist",
    "build",
];

/// Purpose: A stable, read-once list of the workspace's files, with the root it was read from.
/// Serves: `Picker::refresh`, `App::refresh_picker` and the picker tests.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `FileIndex::read` and `FileIndex::from_files`; `App::refresh_picker`; the tests.
/// Down: no call; `matches` reads it.
/// Verify: `cargo test -p loong-tui-app`.
/// The files a workspace offers, in a stable order.
#[derive(Debug, Clone, Default)]
pub struct FileIndex {
    root: PathBuf,
    files: Vec<String>,
}

impl FileIndex {
    /// Purpose: Read the workspace's files, preferring git and falling back to a bounded walk.
    /// Serves: `App::refresh_picker`.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::refresh_picker` and the tests.
    /// Down: `git_files` and `walk_files`.
    /// Verify: `cargo test -p loong-tui-app`.
    /// Read the workspace's files. Prefers git; falls back to a bounded walk.
    pub fn read(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let files = git_files(&root).unwrap_or_else(|| walk_files(&root));
        Self { root, files }
    }

    /// Purpose: An index over a fixed list, so a test need not depend on the repository it runs in.
    /// Serves: the picker tests.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: the picker tests.
    /// Down: no call; it stores the list.
    /// Verify: `cargo test -p loong-tui-app`.
    /// An index over a fixed list, for tests and for a caller that already has the
    /// files. Fixing the list is what keeps a test from depending on the repository
    /// it happens to run in.
    pub fn from_files(files: Vec<String>) -> Self {
        Self {
            root: PathBuf::from("."),
            files,
        }
    }

    /// Purpose: The directory this index was read from.
    /// Serves: no caller today; the picker matches relative paths and never joins them back to the root.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: no caller; `FileIndex::read` is the only writer.
    /// Down: no call; it borrows `root`.
    /// Verify: `cargo test -p loong-tui-app`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Purpose: How many files the index holds.
    /// Serves: no caller today; the picker asks `matches` for the rows it needs.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: no caller; `FileIndex::read` and `FileIndex::from_files` are the only writers.
    /// Down: no call; it reads `files`.
    /// Verify: `cargo test -p loong-tui-app`.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Purpose: Whether the index holds no files at all.
    /// Serves: no caller today; the picker asks `matches` for the rows it needs.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: no caller; `FileIndex::read` and `FileIndex::from_files` are the only writers.
    /// Down: no call; it reads `files`.
    /// Verify: `cargo test -p loong-tui-app`.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Purpose: Every file in the index, in the order it was read.
    /// Serves: no caller today; `matches` is the only reader of the list.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: no caller; `FileIndex::read` and `FileIndex::from_files` are the only writers.
    /// Down: no call; it borrows `files`.
    /// Verify: `cargo test -p loong-tui-app`.
    pub fn files(&self) -> &[String] {
        &self.files
    }

    /// Purpose: Files whose path matches the query, best first, capped at the picker's limit.
    /// Serves: `Picker::refresh` and the picker tests.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `Picker::refresh`.
    /// Down: `rank`.
    /// Verify: `cargo test -p loong-tui-app`.
    /// Files whose path matches `query`, best first.
    ///
    /// Ranked by *where* the query matches rather than by a fuzzy score: the
    /// basename counts for more than a path segment, and an earlier position counts
    /// for more than a later one. That is enough to put `src/main.rs` above
    /// `src/vendor/main_helper.rs` for `main`, which is the decision that matters;
    /// a scoring function nobody can predict would be worse than one they can.
    pub fn matches(&self, query: &str, limit: usize) -> Vec<&str> {
        let query = query.to_ascii_lowercase();
        let mut scored: Vec<(u32, &str)> = Vec::new();
        for file in &self.files {
            if let Some(rank) = rank(file, &query) {
                scored.push((rank, file.as_str()));
            }
        }
        scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.len().cmp(&b.1.len())));
        scored
            .into_iter()
            .take(limit)
            .map(|(_, file)| file)
            .collect()
    }
}

/// Purpose: Score one file against one query: a basename prefix beats a basename hit, which beats a path hit, which beats a subsequence.
/// Serves: `FileIndex::matches`.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `FileIndex::matches`.
/// Down: `subsequence_rank`.
/// Verify: `cargo test -p loong-tui-app`.
/// A smaller rank is a better match. `None` means "does not match".
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

/// Purpose: Score an in-order subsequence match by the spread of its positions, so `skv` finds `src/kernel/view.rs`.
/// Serves: `rank`.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `rank`.
/// Down: no call; it walks the lowercased path.
/// Verify: `cargo test -p loong-tui-app`.
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
    let spread = matched
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .sum::<u32>();
    Some(spread)
}

/// Purpose: The `@`-token the caret is inside, as a byte range and the text typed after the trigger.
/// Serves: `Picker::refresh`.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `Picker::refresh` and the tests.
/// Down: `char_to_byte`.
/// Verify: `cargo test -p loong-tui-app`.
/// The `@`-token the caret is inside, if any.
///
/// Returns the token's **byte range** in `input` and the text typed after the `@`.
/// A token runs from the trigger to the caret, and since a path may contain spaces
/// it may be quoted (`@"my file.txt"`). Whitespace ends it otherwise.
pub fn token_at(input: &str, cursor_chars: usize) -> Option<(std::ops::Range<usize>, String)> {
    let caret = char_to_byte(input, cursor_chars);
    let before = &input[..caret];
    let at = before.rfind(TRIGGER)?;
    let after_trigger = &before[at + TRIGGER.len_utf8()..];

    // A quoted reference is closed or still open; either way the caret is inside it.
    if let Some(rest) = after_trigger.strip_prefix('"') {
        if rest.contains('"') {
            return None;
        }
        return Some((at..caret, rest.to_string()));
    }
    if after_trigger.contains(char::is_whitespace) {
        return None;
    }
    // `someone@example.com` is not a reference: the trigger must start a word.
    if at > 0 {
        let previous = before[..at].chars().next_back();
        if previous.is_some_and(|c| !c.is_whitespace()) {
            return None;
        }
    }
    Some((at..caret, after_trigger.to_string()))
}

/// Purpose: Replace a token's byte range with `@path`, quoting a path that contains spaces.
/// Serves: `App::accept_picker`.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `App::accept_picker`.
/// Down: `TRIGGER` for the rendered token.
/// Verify: `cargo test -p loong-tui-app`.
/// Replace a token's byte range with `@path`, quoting a path that needs it.
pub fn apply(input: &mut String, range: std::ops::Range<usize>, path: &str) {
    let needs_quotes = path.chars().any(char::is_whitespace);
    let rendered = if needs_quotes {
        format!("{TRIGGER}\"{path}\" ")
    } else {
        format!("{TRIGGER}{path} ")
    };
    input.replace_range(range, &rendered);
}

/// Purpose: The byte offset of a character index in the prompt, for the token parser.
/// Contract: called by `token_at`.
fn char_to_byte(input: &str, char_index: usize) -> usize {
    input
        .char_indices()
        .nth(char_index)
        .map(|(byte, _)| byte)
        .unwrap_or(input.len())
}

/// Purpose: The workspace's files from `git ls-files`, or `None` when git or the repository is unavailable.
/// Serves: `FileIndex::read`.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `FileIndex::read`.
/// Down: the `git` child process.
/// Verify: `cargo test -p loong-tui-app`.
/// The workspace's files according to git: tracked, plus untracked-and-not-ignored.
/// `None` when git is unavailable or this is not a repository, so the caller falls
/// back rather than showing an empty list.
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
        .map(|name| name.to_string())
        .collect();
    if files.is_empty() {
        // A repository with no files yet is a workspace with files; the walk is the
        // better answer than "nothing to offer".
        return None;
    }
    files.sort();
    Some(files)
}

/// Purpose: A bounded walk of the tree with the always-ignored directories skipped, for a workspace that is not a git repository.
/// Serves: `FileIndex::read`'s fallback.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `FileIndex::read`.
/// Down: `std::fs::read_dir` and `ALWAYS_IGNORED`.
/// Verify: `cargo test -p loong-tui-app`.
/// A bounded walk, for a workspace that is not a git repository.
fn walk_files(root: &Path) -> Vec<String> {
    /// Deep enough for a real project, shallow enough that a stray symlink cannot
    /// spend the session walking `/`.
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
    out
}

/// Purpose: The live picker: the token range being replaced, the query, the matches and the highlighted row.
/// Serves: `App::picker`, `App::draw_picker`, `App::accept_picker` and the tests.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `Picker::refresh` builds it; `App::refresh_picker` stores it.
/// Down: no call; `draw_picker` and `accept_picker` read it.
/// Verify: `cargo test -p loong-tui-app`.
/// The live state of the picker: which files match, and which one is highlighted.
#[derive(Debug, Clone)]
pub struct Picker {
    /// Byte range of the `@`-token being replaced.
    pub range: std::ops::Range<usize>,
    pub query: String,
    pub matches: Vec<String>,
    pub selected: usize,
}

impl Picker {
    /// Purpose: Build the picker for the current text and caret, or `None` when the caret left the reference.
    /// Serves: `App::refresh_picker` and the picker tests.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::refresh_picker` and the tests.
    /// Down: `token_at` and `FileIndex::matches`.
    /// Verify: `cargo test -p loong-tui-app`.
    /// Refresh against the current input and a workspace index. `None` when the
    /// caret is not inside a reference — which is also how the picker closes, so
    /// there is no second piece of state to keep in step with the text.
    pub fn refresh(
        input: &str,
        cursor_chars: usize,
        files: &FileIndex,
        previous: Option<&Picker>,
    ) -> Option<Self> {
        let (range, query) = token_at(input, cursor_chars)?;
        let matches: Vec<String> = files
            .matches(&query, MAX_MATCHES)
            .into_iter()
            .map(|file| file.to_string())
            .collect();
        // Keep the highlight where it was while the list is unchanged, so typing a
        // character that narrows nothing does not move the selection under the hand.
        let selected = previous
            .filter(|old| old.query == query && old.matches == matches)
            .map(|old| old.selected.min(matches.len().saturating_sub(1)))
            .unwrap_or(0);
        Some(Self {
            range,
            query,
            matches,
            selected,
        })
    }

    /// Purpose: Whether the picker has no matches at all.
    /// Serves: no caller today; `draw_picker` and `accept_picker` check `Picker.matches` directly.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: no caller; `Picker::refresh` is the only writer.
    /// Down: no call; it reads `matches`.
    /// Verify: `cargo test -p loong-tui-app`.
    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }

    /// Purpose: The highlighted file, or nothing in an empty list.
    /// Serves: `App::accept_picker`.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::accept_picker`.
    /// Down: no call; it indexes `matches`.
    /// Verify: `cargo test -p loong-tui-app`.
    pub fn selected_path(&self) -> Option<&str> {
        self.matches.get(self.selected).map(String::as_str)
    }

    /// Purpose: Move the highlight by `delta`, wrapping, and never leave the list.
    /// Serves: `App::handle_key`'s Up, Down, Tab and Shift-Tab picker arms.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::handle_key`.
    /// Down: no call; it changes `selected`.
    /// Verify: `cargo test -p loong-tui-app`.
    /// Move the highlight, wrapping. Never leaves the list.
    pub fn move_by(&mut self, delta: isize) {
        if self.matches.is_empty() {
            return;
        }
        let count = self.matches.len() as isize;
        let next = (self.selected as isize + delta).rem_euclid(count);
        self.selected = next as usize;
    }
}
