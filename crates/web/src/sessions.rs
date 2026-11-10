//! Purpose: The session catalogue the browser shows: list, check, and delete a workspace's sessions.
//!
//! # Contract
//! - **Serves:** `crate::app`'s `session.list`, `session.open` and `session.delete`.
//! - **Design:** `docs/design/web-surface.md` §2.
//! - **Upstream:** `crate::app::dispatch_surface`.
//! - **Downstream:** `loong_config::loongagent_home`, the session logs themselves, and the filesystem.
//! - **Verify:** `cargo test -p loong-web`
//!
//! The surface-side half of "every workspace has N sessions". The layout this
//! reads is the one `crates/cli/src/session.rs` owns (sessions-lineage.md §1:
//! `$LOONGAGENT_HOME/projects/<slug>/<id>.jsonl`); this crate mirrors the three
//! pure path rules it needs — the slug, the directory, the id → file mapping —
//! so the browser never depends on the CLI binary's crate, and never parses a
//! log beyond the title lines a rail needs.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::app::Error;

/// Purpose: The directory one workspace's sessions live in, under the client's root.
/// Serves: `list`, `known`, `delete` and `log_path`.
/// Design: `docs/design/sessions-lineage.md` §1.
/// Up: the catalogue functions.
/// Down: `project_slug` and `loong_config::loongagent_home`.
/// Verify: `cargo test -p loong-web` (`layout_`).
pub fn session_dir(root: &Path, workspace: &Path) -> PathBuf {
    root.join("projects").join(project_slug(workspace))
}

/// Purpose: The log file one session id names in its workspace.
/// Serves: `delete` and the catalogue tests.
/// Design: `docs/design/sessions-lineage.md` §1 — a session is its file, and `default` keeps
/// the legacy name `session.jsonl`.
/// Up: `delete`, `known`, and the catalogue tests.
/// Down: the `<root>/projects/<slug>/<id>.jsonl` path.
/// Verify: `cargo test -p loong-web` (`layout_matches_the_cli_rule`).
/// Contract: mirrors `session_path_in` in `crates/cli/src/session.rs`.
pub fn log_path(root: &Path, workspace: &Path, id: &str) -> PathBuf {
    if id == "default" {
        session_dir(root, workspace).join("session.jsonl")
    } else {
        session_dir(root, workspace).join(format!("{id}.jsonl"))
    }
}

/// Purpose: The sessions of one workspace, newest first, as the page's rail renders them.
/// Serves: `crate::app`'s `session.list` answer.
/// Design: `docs/design/web-surface.md` §2.
/// Up: `dispatch_surface`.
/// Down: `summaries_in`.
/// Verify: `cargo test -p loong-web` (`list_`).
pub fn list(root: &Path, workspace: &Path) -> Vec<Value> {
    summaries_in(&session_dir(root, workspace))
        .iter()
        .map(|s| {
            json!({
                "id": s.id,
                "title": s.title,
                "turns": s.turns,
                "events": s.events,
                "modified": s
                    .modified
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                // The rail's "2m ago" label reads the conversation's clock, not the file's
                // mtime: opening a session touches the file, and a label that says "just
                // now" for a session nobody spoke in is the same bug as the reordering.
                "last_ms": s.last_ms,
            })
        })
        .collect()
}

/// Purpose: Fail when the workspace has no session of that id — the open guard.
/// Serves: `crate::app`'s `session.open`.
/// Design: `docs/design/web-surface.md` §2 — resume never guesses a path.
/// Up: `dispatch_surface`.
/// Down: `log_path`.
/// Verify: `cargo test -p loong-web` (`known_`).
pub fn known(root: &Path, workspace: &Path, session_id: &str) -> Result<(), Error> {
    if session_id.is_empty() {
        return Err(Error("session.open needs a session_id".to_string()));
    }
    if log_path(root, workspace, session_id).exists() {
        Ok(())
    } else {
        Err(Error(format!("no session {session_id} in this workspace")))
    }
}

/// Purpose: Remove a session's log and its derived index, both of them, or refuse with the reason.
/// Serves: `crate::app`'s `session.delete`.
/// Design: `docs/design/web-surface.md` §2 — the index is derived from the log
/// (`turn/build.rs` rebuilds it), so deleting the pair is complete.
/// Up: `dispatch_surface`.
/// Down: `log_path` and the filesystem.
/// Verify: `cargo test -p loong-web` (`delete_`).
pub fn delete(root: &Path, workspace: &Path, session_id: &str) -> Result<(), Error> {
    let log = log_path(root, workspace, session_id);
    if !log.exists() {
        return Err(Error(format!("no session {session_id} in this workspace")));
    }
    let index = log.with_extension("sqlite");
    std::fs::remove_file(&log)
        .map_err(|e| Error(format!("cannot delete {}: {e}", log.display())))?;
    if index.exists() {
        std::fs::remove_file(&index)
            .map_err(|e| Error(format!("cannot delete {}: {e}", index.display())))?;
    }
    Ok(())
}

/// Purpose: The client's root, `$LOONGAGENT_HOME` or `~/.loongagent`.
/// Serves: the catalogue's callers, so they never name the environment themselves.
/// Design: `docs/design/sessions-lineage.md` §1.
/// Up: `list`, `delete` and the tests, which pass the root down.
/// Down: `loong_config::loongagent_home`.
/// Verify: `cargo test -p loong-web` (`layout_matches_the_cli_rule`).
/// Contract: the same root `crates/cli/src/main.rs`'s `config_dir` answers with.
pub fn client_root() -> PathBuf {
    loong_config::loongagent_home()
}

/// Purpose: One session seen from outside: id, a title to show, counts, and when it last moved.
/// Serves: `list`.
/// Design: `docs/design/sessions-lineage.md` §1 and §3 — read from the log, not an index.
/// Up: `list`.
/// Down: `summaries_in` builds it.
/// Verify: `cargo test -p loong-web`.
pub struct Summary {
    /// the file's own name as the id
    pub id: String,
    /// the first prompt, whitespace-collapsed, or "New session"
    pub title: String,
    /// how many user turns the log holds
    pub turns: u64,
    /// how many events the log holds
    pub events: u64,
    /// the file's modification time
    pub modified: SystemTime,
    /// the largest `time_ms` any conversation event in the log carries — the session's own clock
    pub last_ms: u64,
    /// the smallest `time_ms` in the log — when the session began; a resume cannot move it
    pub created_ms: u64,
}

/// Purpose: Read every session log in a directory into summaries, newest first.
/// Serves: `list`.
/// Design: `docs/design/sessions-lineage.md` §1 and §3 — the log is the record.
/// Up: `list`.
/// Down: the filesystem and the JSONL lines; mirrors `session_summaries_in` in
/// `crates/cli/src/session.rs`.
/// Verify: `cargo test -p loong-web` (`list_`).
fn summaries_in(dir: &Path) -> Vec<Summary> {
    let mut summaries = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return summaries;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        if path.file_name().and_then(|n| n.to_str()) == Some("history.jsonl") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(UNIX_EPOCH);
        let id = match path.file_stem().map(|s| s.to_string_lossy().to_string()) {
            Some(stem) if stem != "session" && !stem.is_empty() => stem,
            _ => "default".to_string(),
        };
        let mut events = 0u64;
        let mut turns = 0u64;
        let mut last_ms = 0u64;
        let mut created_ms = u64::MAX;
        let mut first_prompt = None;
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            events += 1;
            let kind = value.get("kind").and_then(Value::as_str);
            if let Some(ms) = value.get("time_ms").and_then(Value::as_u64) {
                created_ms = created_ms.min(ms);
                // Session-start bookkeeping records that a session was opened, not that
                // it was used: a resume writes `skills.loaded` at the current time, and
                // counting it would lift the session to the front of the rail.
                if !is_session_start_event(kind) {
                    last_ms = last_ms.max(ms);
                }
            }
            if kind != Some("message.appended") {
                continue;
            }
            let data = value.get("data").cloned().unwrap_or_default();
            if data.get("role").and_then(Value::as_str) != Some("user") {
                continue;
            }
            turns += 1;
            if first_prompt.is_none() {
                let text = data.get("text").and_then(Value::as_str).unwrap_or_default();
                let flattened: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if !flattened.is_empty() {
                    first_prompt = Some(flattened);
                }
            }
        }
        if created_ms == u64::MAX {
            // A session created a moment ago has no event of its own yet, and a very old
            // log may predate `time_ms`: the file's own time is the only clock there is.
            // It is read only when the log carries no clock at all, so the mtime a resume
            // touches can never reorder a session that has one.
            created_ms = modified
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
        }
        // A log of nothing but session-start events still needs a clock to sort by.
        if last_ms == 0 {
            last_ms = created_ms;
        }
        let title = match first_prompt.as_deref() {
            Some(prompt) => {
                let title: String = prompt.chars().take(80).collect();
                if title.is_empty() {
                    "New session".to_string()
                } else {
                    title
                }
            }
            None => "New session".to_string(),
        };
        summaries.push(Summary {
            id,
            title,
            turns,
            events,
            modified,
            last_ms,
            created_ms,
        });
    }
    // Newest *started* first: the rail is a list of sessions, and a session belongs where
    // it was created. Creation time cannot move after the fact — not when the session is
    // merely opened, and not when it is talked in either — so the list the user learned
    // stays put. Ties (two sessions made in the same millisecond) fall back to the id so
    // the order is total.
    summaries.sort_by(|a, b| b.created_ms.cmp(&a.created_ms).then(a.id.cmp(&b.id)));
    summaries
}

/// Purpose: Whether an event records that a session was *started* rather than used —
/// its timestamp must not decide the rail order.
/// Serves: `summaries_in`, excluding these from `last_ms`.
/// Design: `docs/design/web-surface.md` §2 — opening a session must not reorder the rail.
/// Contract: `Kernel::build` writes `skills.loaded` on every open
/// (`crates/kernel/src/turn/build.rs`); `model.selected` and `profile.selected` record the
/// same start-up choice, so all three are bookkeeping, not activity.
fn is_session_start_event(kind: Option<&str>) -> bool {
    matches!(
        kind,
        Some("skills.loaded") | Some("model.selected") | Some("profile.selected")
    )
}

/// Purpose: A directory's fingerprint: its path turned into one filesystem-safe, collision-checked name.
/// Serves: `session_dir`.
/// Design: `docs/design/sessions-lineage.md` §1 — mirrors `project_slug` in
/// `crates/cli/src/session.rs`, so a session the TUI wrote is one the browser lists.
/// Up: `session_dir`, and through it `sessions::list`.
/// Down: the `projects/<slug>` directory name.
/// Verify: `cargo test -p loong-web` (`layout_matches_the_cli_rule`).
/// Contract: the same input gives the same slug in both crates; the test pins a known value.
fn project_slug(cwd: &Path) -> String {
    let path = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let text = path.to_string_lossy();
    let mut slug: String = text
        .trim_start_matches('/')
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{}-{:08x}", slug.trim_matches('-'), hash as u32)
}

/// Purpose: The directory picker's answer: the directory's subdirectories, with enough to render.
/// Serves: `crate::browse` — the `/api/browse` route.
/// Design: `docs/design/web-surface.md` §5.
/// Up: `crate::browse`.
/// Down: `resolve_dir` and the filesystem.
/// Verify: `cargo test -p loong-web` (`browse_`).
pub fn browse(asked: &str, home: &Path) -> Result<String, Error> {
    let dir = resolve_dir(asked, home)?;
    let mut dirs = Vec::new();
    let read = std::fs::read_dir(&dir)
        .map_err(|e| Error(format!("cannot read {}: {e}", dir.display())))?;
    for entry in read.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        dirs.push(json!({
            "name": name,
            "path": path,
        }));
    }
    dirs.sort_by(|a, b| {
        a.get("name")
            .and_then(Value::as_str)
            .cmp(&b.get("name").and_then(Value::as_str))
    });
    let parent = dir.parent().map(|p| p.to_path_buf());
    Ok(json!({
        "cwd": dir,
        "parent": parent,
        "dirs": dirs,
    })
    .to_string())
}

/// Purpose: The directory a `dir` query parameter names, checked against the picker's rules.
/// Serves: `crate::browse` — the `/api/browse` route the picker calls.
/// Design: `docs/design/web-surface.md` §5 — the picker lists directories only.
/// Up: `browse`, with the raw query string.
/// Down: `std::fs::canonicalize`, then `sessions::browse`.
/// Verify: `cargo test -p loong-web` (`browse_decodes_path`, `browse_refuses_a_file`).
/// Contract: defaults to the home directory; must exist and be a directory.
fn resolve_dir(asked: &str, home: &Path) -> Result<PathBuf, Error> {
    let query = asked.splitn(2, '?').nth(1).unwrap_or("");
    let raw = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("dir="))
        .map(percent_decode)
        .unwrap_or_default();
    let path = if raw.is_empty() {
        home.to_path_buf()
    } else {
        let p = PathBuf::from(&raw);
        if p.is_absolute() {
            p
        } else {
            home.join(p)
        }
    };
    let canonical =
        std::fs::canonicalize(&path).map_err(|_| Error(format!("no such directory: {raw}")))?;
    if !canonical.is_dir() {
        return Err(Error(format!("not a directory: {raw}")));
    }
    Ok(canonical)
}

/// Purpose: Decode the `%XX` and `+` of one query value.
/// Serves: `resolve_dir`.
/// Design: `docs/design/web-surface.md` §5 — the picker's query string.
/// Up: `resolve_dir`, on the `dir=` value.
/// Down: the decoded path string it canonicalises.
/// Verify: `cargo test -p loong-web` (`browse_decodes_path`).
/// Contract: invalid `%` sequences pass through unchanged; a directory path never needs them.
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = |b: u8| (b as char).to_digit(16);
                match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    //! Purpose: The catalogue's gates: the layout matches the CLI's, list order, the open
    //! guard, deletion of the pair, and the picker's directory rules.
    //!
    //! # Contract
    //! - **Serves:** `cargo test -p loong-web`.
    //! - **Design:** `docs/design/web-surface.md` §2 and §5.
    //! - **Upstream:** `scripts/check.sh`.
    //! - **Downstream:** `crate::sessions::{list, known, delete, browse, session_dir, log_path}`.
    //! - **Verify:** `cargo test -p loong-web`.

    use super::*;

    /// Purpose: A throwaway root and workspace with fake session logs in the real layout.
    ///
    /// Serves: every catalogue test.
    /// Design: `docs/design/sessions-lineage.md` §1.
    /// Up: the tests below.
    /// Down: the filesystem under the temp dir.
    /// Verify: `cargo test -p loong-web`.
    struct Fixture {
        /// the client root the logs live under
        root: PathBuf,
        /// the workspace the sessions belong to
        workspace: PathBuf,
    }

    impl Fixture {
        /// Purpose: Make root and workspace, and plant two session logs.
        /// Serves: every `list_*` test in this module.
        /// Design: `docs/design/web-surface.md` §2 — the rail's ordering tests.
        /// Up: those tests' bodies.
        /// Down: `Fixture::plant`, which writes the logs it checks.
        /// Verify: `cargo test -p loong-web` (`sessions::tests`).
        fn new() -> Self {
            // Unique per test and per call: the tests run in parallel threads.
            let base = std::env::temp_dir().join(format!(
                "loong-sessions-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&base);
            let fixture = Fixture {
                root: base.join("home"),
                workspace: base.join("workspace"),
            };
            std::fs::create_dir_all(&fixture.workspace).unwrap();
            // Distinct event times: the rail order is a function of the logs' contents.
            fixture.plant("s-old", "the older session", 1_000);
            fixture.plant("s-new", "the newer session", 2_000);
            fixture
        }

        /// Purpose: Write one minimal session log in the workspace's session directory.
        /// Contract: the first user message is the title the rail shows.
        fn plant(&self, id: &str, title: &str, time_ms: u64) {
            let log = log_path(&self.root, &self.workspace, id);
            std::fs::create_dir_all(log.parent().unwrap()).unwrap();
            let line = json!({
                "kind": "message.appended", "seq": 1, "time_ms": time_ms, "id": "e1",
                "data": {"role": "user", "text": title}
            });
            std::fs::write(&log, format!("{line}\n")).unwrap();
        }
    }

    impl Drop for Fixture {
        /// Purpose: Delete the temporary client root this fixture made.
        /// Serves: `Fixture::new` — one test's scratch tree is removed when the test ends.
        /// Design: `docs/design/web-surface.md` §2 (the tests' own cleanup).
        /// Up: the test holding the fixture.
        /// Down: nothing; the removal result is deliberately ignored.
        /// Verify: `cargo test -p loong-web` (`sessions::tests`).
        fn drop(&mut self) {
            let base = self
                .root
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| self.root.clone());
            std::fs::remove_dir_all(base).ok();
        }
    }

    /// Purpose: the layout rule this crate mirrors is the CLI's, pinned against one known slug.
    #[test]
    fn layout_matches_the_cli_rule() {
        let fixture = Fixture::new();
        // The planted logs are found through the mirrored path — if the slug rule ever
        // drifts from `crates/cli/src/session.rs`, this fixture goes silent.
        let dir = session_dir(&fixture.root, &fixture.workspace);
        assert!(dir.join("s-old.jsonl").exists(), "{dir:?}");
        assert!(log_path(&fixture.root, &fixture.workspace, "default").ends_with("session.jsonl"));
    }

    /// Purpose: the newest *created* session sits on top, even when an older one has
    /// been talked in more recently — the rail is ordered by when a session was started.
    #[test]
    fn list_orders_by_creation_not_last_chatter() {
        let fixture = Fixture::new();
        // A long conversation started first, last spoken in long after "s-fresh" began.
        let chatty = log_path(&fixture.root, &fixture.workspace, "s-chatty");
        std::fs::write(
            &chatty,
            format!(
                "{}\n{}\n",
                json!({"kind":"message.appended","seq":1,"time_ms":1_000,"id":"e1",
                       "data":{"role":"user","text":"an old conversation"}}),
                json!({"kind":"message.appended","seq":2,"time_ms":9_000,"id":"e2",
                       "data":{"role":"user","text":"still going"}}),
            ),
        )
        .unwrap();
        // Started later, then left alone.
        let fresh = log_path(&fixture.root, &fixture.workspace, "s-fresh");
        std::fs::write(
            &fresh,
            format!(
                "{}\n",
                json!({"kind":"message.appended","seq":1,"time_ms":5_000,"id":"e1",
                       "data":{"role":"user","text":"a newer conversation"}}),
            ),
        )
        .unwrap();
        let ids: Vec<String> = list(&fixture.root, &fixture.workspace)
            .iter()
            .filter_map(|e| e.get("id").and_then(Value::as_str).map(str::to_string))
            .filter(|id| id == "s-chatty" || id == "s-fresh")
            .collect();
        assert_eq!(
            ids,
            vec!["s-fresh", "s-chatty"],
            "the session created last is listed first"
        );
    }

    /// Purpose: a session created a moment ago is on top straight away, even before its
    /// child has written a first event — a brand-new log has no clock of its own yet.
    #[test]
    fn list_puts_a_brand_new_session_on_top() {
        let fixture = Fixture::new();
        let brand_new = log_path(&fixture.root, &fixture.workspace, "s-brand-new");
        std::fs::write(&brand_new, "").unwrap();
        let ids: Vec<String> = list(&fixture.root, &fixture.workspace)
            .iter()
            .filter_map(|e| e.get("id").and_then(Value::as_str).map(str::to_string))
            .collect();
        assert_eq!(
            ids.first().map(String::as_str),
            Some("s-brand-new"),
            "an empty new session must not sink to the bottom: {ids:?}"
        );
    }

    /// Purpose: both planted sessions are listed, newest first, with their titles and counts.
    #[test]
    fn list_shows_titles_newest_first() {
        let fixture = Fixture::new();
        let entries = list(&fixture.root, &fixture.workspace);
        let ids: Vec<&str> = entries
            .iter()
            .filter_map(|e| e.get("id").and_then(Value::as_str))
            .collect();
        assert_eq!(ids, vec!["s-new", "s-old"], "{ids:?}");
        let first = &entries[0];
        assert_eq!(
            first.get("title").and_then(Value::as_str),
            Some("the newer session")
        );
        assert_eq!(first.get("turns").and_then(Value::as_u64), Some(1));
    }

    /// Purpose: a session with no user turn yet is listed as "New session".
    #[test]
    fn list_names_an_empty_session() {
        let fixture = Fixture::new();
        let log = log_path(&fixture.root, &fixture.workspace, "s-empty");
        std::fs::write(
            &log,
            "{\"kind\":\"session.resumed\",\"seq\":1,\"time_ms\":1,\"id\":\"e1\"}\n",
        )
        .unwrap();
        let entries = list(&fixture.root, &fixture.workspace);
        let empty = entries
            .iter()
            .find(|e| e.get("id").and_then(Value::as_str) == Some("s-empty"))
            .expect("the empty session is listed");
        assert_eq!(
            empty.get("title").and_then(Value::as_str),
            Some("New session")
        );
    }

    /// Purpose: a planted session passes the open guard; a made-up id does not.
    #[test]
    fn known_guards_open() {
        let fixture = Fixture::new();
        assert!(known(&fixture.root, &fixture.workspace, "s-old").is_ok());
        assert!(known(&fixture.root, &fixture.workspace, "no-such").is_err());
        assert!(known(&fixture.root, &fixture.workspace, "").is_err());
    }

    /// Purpose: deleting removes the log and its derived index, and the pair only.
    #[test]
    fn delete_removes_log_and_index() {
        let fixture = Fixture::new();
        let log = log_path(&fixture.root, &fixture.workspace, "s-old");
        let index = log.with_extension("sqlite");
        std::fs::write(&index, b"index-bytes").unwrap();
        delete(&fixture.root, &fixture.workspace, "s-old").unwrap();
        assert!(!log.exists(), "the log is gone");
        assert!(!index.exists(), "the derived index is gone");
        assert!(known(&fixture.root, &fixture.workspace, "s-new").is_ok());
    }

    /// Purpose: deleting a session that does not exist says so, and touches nothing.
    #[test]
    fn delete_missing_refused() {
        let fixture = Fixture::new();
        let err = delete(&fixture.root, &fixture.workspace, "no-such")
            .expect_err("a missing session is refused");
        assert!(err.0.contains("no session"), "{}", err.0);
    }

    /// Purpose: the picker lists subdirectories and skips dotfiles and files.
    #[test]
    fn browse_lists_directories_only() {
        let fixture = Fixture::new();
        let root = fixture.workspace.join("browse");
        std::fs::create_dir_all(root.join("alpha")).unwrap();
        std::fs::create_dir_all(root.join(".hidden")).unwrap();
        std::fs::write(root.join("a-file.txt"), b"x").unwrap();
        let body = browse(
            &format!("/api/browse?dir={}", root.display()),
            &fixture.workspace,
        )
        .unwrap();
        let value: Value = serde_json::from_str(&body).unwrap();
        let names: Vec<&str> = value
            .get("dirs")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .filter_map(|d| d.get("name").and_then(Value::as_str))
            .collect();
        assert_eq!(names, vec!["alpha"], "{names:?}");
        assert!(value.get("parent").is_some(), "{body}");
    }

    /// Purpose: the picker defaults to the home directory when no `dir` is given.
    #[test]
    fn browse_defaults_to_home() {
        let fixture = Fixture::new();
        let body = browse("/api/browse", &fixture.workspace).unwrap();
        let value: Value = serde_json::from_str(&body).unwrap();
        let cwd = value.get("cwd").and_then(Value::as_str).unwrap();
        assert_eq!(
            cwd,
            std::fs::canonicalize(&fixture.workspace)
                .unwrap()
                .to_string_lossy()
        );
    }

    /// Purpose: a path that is not a directory is refused, not listed.
    #[test]
    fn browse_refuses_a_file() {
        let fixture = Fixture::new();
        let file = fixture.workspace.join("a-file");
        std::fs::write(&file, b"x").unwrap();
        let err = browse(
            &format!("/api/browse?dir={}", file.display()),
            &fixture.workspace,
        )
        .expect_err("a file is not a directory");
        assert!(err.0.contains("not a directory"), "{}", err.0);
    }

    /// Purpose: percent-encoded paths decode, including spaces and slashes.
    #[test]
    fn browse_decodes_path() {
        let fixture = Fixture::new();
        let sub = fixture.workspace.join("with space");
        std::fs::create_dir_all(&sub).unwrap();
        let raw = sub
            .to_string_lossy()
            .replace(' ', "%20")
            .replace('/', "%2F");
        let body = browse(&format!("/api/browse?dir={raw}"), &fixture.workspace).unwrap();
        let value: Value = serde_json::from_str(&body).unwrap();
        let cwd = value.get("cwd").and_then(Value::as_str).unwrap();
        assert_eq!(cwd, std::fs::canonicalize(&sub).unwrap().to_string_lossy());
    }

    /// Purpose: the rail order follows the session's own clock (the latest `time_ms`
    /// in its log), not the file's mtime, so merely opening a session never reshuffles it.
    #[test]
    fn list_orders_by_event_time_not_file_mtime() {
        let fixture = Fixture::new();
        // a-active has a LARGER time_ms (its conversation is more recent) but we will
        // give it an OLDER file mtime; a-quiet is the reverse. Ordering by mtime would
        // put a-quiet first; ordering by the log's own clock puts a-active first.
        let active = log_path(&fixture.root, &fixture.workspace, "a-active");
        let quiet = log_path(&fixture.root, &fixture.workspace, "a-quiet");
        let write = |path: &Path, ms: u64, title: &str| {
            let line = json!({
                "kind": "message.appended", "seq": 1, "time_ms": ms, "id": "e1",
                "data": {"role": "user", "text": title}
            });
            std::fs::write(path, format!("{line}\n")).unwrap();
        };
        write(&active, 9_000, "active conversation");
        write(&quiet, 1_000, "quiet conversation");
        // Make the file mtimes contradict the event times: quiet is the newer FILE.
        // (touch a-quiet last)
        std::fs::write(&quiet, std::fs::read_to_string(&quiet).unwrap()).unwrap();

        let entries = list(&fixture.root, &fixture.workspace);
        let ids: Vec<&str> = entries
            .iter()
            .filter_map(|e| e.get("id").and_then(Value::as_str))
            .filter(|id| id.starts_with("a-"))
            .collect();
        assert_eq!(
            ids,
            vec!["a-active", "a-quiet"],
            "the session whose log is newer leads, regardless of file mtime"
        );
    }

    /// Purpose: a session-start bookkeeping event — what a resume appends — must not
    /// decide the rail order. Opening a session builds a kernel, which writes
    /// `skills.loaded` with the current time; that alone must not lift a quiet session
    /// above one whose conversation is genuinely newer.
    #[test]
    fn list_ignores_session_start_events() {
        let fixture = Fixture::new();
        let busy = log_path(&fixture.root, &fixture.workspace, "b-busy");
        let opened = log_path(&fixture.root, &fixture.workspace, "b-opened");
        let message = |ms: u64, title: &str| {
            json!({
                "kind": "message.appended", "seq": 1, "time_ms": ms, "id": "e1",
                "data": {"role": "user", "text": title}
            })
        };
        let start = |ms: u64, seq: u64| json!({"kind": "skills.loaded", "seq": seq, "time_ms": ms, "id": "e2"});
        std::fs::write(&busy, format!("{}\n", message(5_000, "busy conversation"))).unwrap();
        // "opened" was clicked at 9_000: the resume wrote its skills.loaded line then,
        // though its conversation last moved at 1_000.
        std::fs::write(
            &opened,
            format!(
                "{}\n{}\n",
                message(1_000, "quiet conversation"),
                start(9_000, 2)
            ),
        )
        .unwrap();

        let ids: Vec<String> = list(&fixture.root, &fixture.workspace)
            .iter()
            .filter_map(|e| e.get("id").and_then(Value::as_str).map(str::to_string))
            .filter(|id| id.starts_with("b-"))
            .collect();
        assert_eq!(
            ids,
            vec!["b-busy", "b-opened"],
            "a resume's own event must not move a session to the front"
        );
    }

    /// Purpose: appending what a resume writes leaves the rail order untouched — the
    /// exact reported symptom, pinned.
    #[test]
    fn list_order_survives_a_resume_append() {
        let fixture = Fixture::new();
        let order = |root: &Path, workspace: &Path| -> Vec<String> {
            list(root, workspace)
                .iter()
                .filter_map(|e| e.get("id").and_then(Value::as_str).map(str::to_string))
                .collect()
        };
        let before = order(&fixture.root, &fixture.workspace);
        // Resuming "s-old" appends a fresh skills.loaded at the current time.
        let log = log_path(&fixture.root, &fixture.workspace, "s-old");
        let mut text = std::fs::read_to_string(&log).unwrap();
        text.push_str(
            "{\"kind\":\"skills.loaded\",\"seq\":2,\"time_ms\":99999999,\"id\":\"e2\"}\n",
        );
        std::fs::write(&log, text).unwrap();
        assert_eq!(
            before,
            order(&fixture.root, &fixture.workspace),
            "opening a session must not reshuffle the rail"
        );
    }

    /// Purpose: re-reading the logs (what merely opening a session does) never changes
    /// the rail order — the order is a pure function of the logs' contents.
    #[test]
    fn list_order_is_stable_across_rereads() {
        let fixture = Fixture::new();
        let first: Vec<String> = list(&fixture.root, &fixture.workspace)
            .iter()
            .filter_map(|e| e.get("id").and_then(Value::as_str).map(str::to_string))
            .collect();
        // Simulate opening a session: the file is read, not written.
        for id in ["s-old", "s-new"] {
            let _ = std::fs::read_to_string(log_path(&fixture.root, &fixture.workspace, id));
        }
        let second: Vec<String> = list(&fixture.root, &fixture.workspace)
            .iter()
            .filter_map(|e| e.get("id").and_then(Value::as_str).map(str::to_string))
            .collect();
        assert_eq!(
            first, second,
            "opening a session must not reshuffle the rail"
        );
    }
}
