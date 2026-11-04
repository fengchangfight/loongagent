//! Purpose: The prompt history: what was asked before, where the up/down walk is, and the one file that keeps it across runs.
//!
//! # Contract
//! - **Serves:** `App`'s key handler, its submit path and the prompt box's title.
//! - **Design:** `docs/design/surfaces.md` §3.
//! - **Upstream:** `surface::run` loads it from the path `crates/cli/src/session.rs` names; `App::begin_submit` records into it.
//! - **Downstream:** the project's `history.jsonl` and `serde_json`, one object per line.
//! - **Verify:** `cargo test -p loong-tui-app` · `./scripts/check.sh`

use std::path::Path;

/// Purpose: How many prompts the walk keeps. The file is small by design: history is a convenience, not a record — the session log is the record.
/// Serves: `History::record`, `History::load` and the cap test.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `History::record` and `History::load` read it; the tests assert the bound.
/// Down: no call; it is a number.
/// Verify: `cargo test -p loong-tui-app`.
pub const MAX_ENTRIES: usize = 200;

/// Purpose: What a down-arrow recall produced: an older prompt, or the draft the walk started from.
/// Serves: `App::recall_newer`, which puts the text and caret back into the prompt.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `History::newer` returns it; `App::recall_newer` matches it.
/// Down: no call; it is data.
/// Verify: `cargo test -p loong-tui-app`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recalled {
    /// A prompt from the history, with the caret at its end.
    Entry { text: String, cursor: usize },
    /// The line the person had typed before the first up-arrow, with its caret.
    Draft { text: String, cursor: usize },
}

/// Purpose: The prompts a project has asked, the place the arrows are at, and the draft to return to.
/// Serves: `App`'s prompt field, its key handler, its submit path and its title.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `surface::run` builds one through `load`; `App::new` starts empty.
/// Down: `std::fs` for the file and `serde_json` for the line.
/// Verify: `cargo test -p loong-tui-app`.
#[derive(Debug, Default, Clone)]
pub struct History {
    /// The prompts, oldest first.
    entries: Vec<String>,
    /// Where the walk is, newest is `entries.len() - 1`. `None` means the prompt is the
    /// person's own line, not a recalled one.
    at: Option<usize>,
    /// The line and caret that were in the prompt when the walk began.
    draft: Option<(String, usize)>,
}

impl History {
    /// Purpose: An empty history, as a surface with no file or a fresh test starts.
    /// Serves: `App::new` and the history tests.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::new` and the tests.
    /// Down: no call; it initialises the fields.
    /// Verify: `cargo test -p loong-tui-app`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Purpose: The prompts, oldest first, so a test and the title can read them without a second source.
    /// Serves: the history tests and `History::position`.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: the history tests; `History::position` reads the length.
    /// Down: no call; it borrows the field.
    /// Verify: `cargo test -p loong-tui-app`.
    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    /// Purpose: Whether the prompt currently shows a recalled line, which is what keeps the menus out of the arrows.
    /// Serves: `App::status`, the `@` picker, the command menu and the arrow keys.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::status`, `App::refresh_picker` and `App::handle_key`'s prompt arms.
    /// Down: no call; it reads the walk field.
    /// Verify: `cargo test -p loong-tui-app`.
    pub fn is_browsing(&self) -> bool {
        self.at.is_some()
    }

    /// Purpose: The walk's place as one-based "at of total", for the prompt title.
    /// Serves: `App::status`.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::status`.
    /// Down: `History::entries`'s length.
    /// Verify: `cargo test -p loong-tui-app`.
    pub fn position(&self) -> Option<(usize, usize)> {
        self.at.map(|at| (at + 1, self.entries.len()))
    }

    /// Purpose: Keep one submitted prompt, skip an empty or immediately repeated one, cap the list and end any walk.
    /// Serves: `App::begin_submit`, which saves the file when this returns true.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::begin_submit` and the history tests.
    /// Down: `stop`, `Vec::drain`; no file work, which belongs to the caller.
    /// Verify: `cargo test -p loong-tui-app`.
    pub fn record(&mut self, prompt: &str) -> bool {
        self.stop();
        let prompt = prompt.trim();
        if prompt.is_empty() || self.entries.last().map(String::as_str) == Some(prompt) {
            return false;
        }
        self.entries.push(prompt.to_string());
        let excess = self.entries.len().saturating_sub(MAX_ENTRIES);
        self.entries.drain(0..excess);
        true
    }

    /// Purpose: Step one prompt older, stashing the person's own line the first time, and return what to show.
    /// Serves: `App::recall_older` on the up-arrow.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::recall_older` and the history tests.
    /// Down: `stop` is not called here; the walk stays live until down passes the newest.
    /// Verify: `cargo test -p loong-tui-app`.
    pub fn older(&mut self, draft: &str, cursor: usize) -> Option<String> {
        let len = self.entries.len();
        if len == 0 {
            return None;
        }
        match self.at {
            None => {
                self.draft = Some((draft.to_string(), cursor));
                self.at = Some(len - 1);
            }
            // The oldest entry is the end of the walk: pressing up again stays there
            // rather than wrapping, so the reader cannot lose their place.
            Some(0) => {}
            Some(at) => self.at = Some(at - 1),
        }
        self.at.map(|at| self.entries[at].clone())
    }

    /// Purpose: Step one prompt newer, and past the newest return the draft; `None` when there is no walk.
    /// Serves: `App::recall_newer` on the down-arrow.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::recall_newer` and the history tests.
    /// Down: no call; it walks the fields.
    /// Verify: `cargo test -p loong-tui-app`.
    pub fn newer(&mut self) -> Option<Recalled> {
        let at = self.at?;
        if at + 1 < self.entries.len() {
            let text = self.entries[at + 1].clone();
            self.at = Some(at + 1);
            return Some(Recalled::Entry {
                cursor: text.chars().count(),
                text,
            });
        }
        self.at = None;
        self.draft
            .take()
            .map(|(text, cursor)| Recalled::Draft { text, cursor })
    }

    /// Purpose: End the walk and drop the draft, as submitting or leaving history does.
    /// Serves: `History::record`, so a new prompt is not appended inside a recalled position.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `History::record`.
    /// Down: no call; it clears the walk fields.
    /// Verify: `cargo test -p loong-tui-app`.
    pub fn stop(&mut self) {
        self.at = None;
        self.draft = None;
    }

    /// Purpose: Read the project's history file, skipping lines that are not a prompt object and keeping the newest entries.
    /// Serves: `App::load_history`, which the surface calls before the first frame.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::load_history` and the history tests.
    /// Down: `std::fs::read_to_string` and `serde_json`; a missing or unreadable file is an empty history.
    /// Verify: `cargo test -p loong-tui-app`.
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let mut entries = Vec::new();
        for line in text.lines() {
            // A line the store cannot read is skipped rather than ending the file: one
            // bad write should cost one prompt, not every prompt after it.
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if let Some(prompt) = value.get("prompt").and_then(|prompt| prompt.as_str()) {
                if !prompt.trim().is_empty() {
                    entries.push(prompt.to_string());
                }
            }
        }
        let excess = entries.len().saturating_sub(MAX_ENTRIES);
        entries.drain(0..excess);
        Self {
            entries,
            at: None,
            draft: None,
        }
    }

    /// Purpose: Write the whole list to the project's history file through a temporary file and a rename.
    /// Serves: `App::begin_submit` after `record` says something changed.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::begin_submit` and the history tests.
    /// Down: `std::fs::{create_dir_all, write, rename}` and `serde_json`.
    /// Verify: `cargo test -p loong-tui-app`.
    ///
    /// Best effort by design: history is a convenience, so a read-only home or a full
    /// disk must never refuse a prompt. The rename is what keeps a crash mid-write from
    /// leaving half a list behind.
    pub fn save(&self, path: &Path) {
        let Some(parent) = path.parent() else {
            return;
        };
        let _ = std::fs::create_dir_all(parent);
        let mut body = String::new();
        for entry in &self.entries {
            let Ok(line) = serde_json::to_string(&serde_json::json!({ "prompt": entry })) else {
                continue;
            };
            body.push_str(&line);
            body.push('\n');
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("history.jsonl");
        let temporary = path.with_file_name(format!("{name}.tmp-{}", std::process::id()));
        if std::fs::write(&temporary, body).is_ok() {
            let _ = std::fs::rename(&temporary, path);
        }
    }
}
