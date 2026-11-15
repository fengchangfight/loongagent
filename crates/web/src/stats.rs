//! Purpose: The numbers behind the token statistics page: every session log's
//! `cost.recorded`, summed by model, by workspace and by session.
//!
//! # Contract
//! - **Serves:** `crate::app`'s `stats.usage` — the banner's 统计 view.
//! - **Design:** `docs/design/web-stats.md` §2–§3.
//! - **Upstream:** `crate::app::dispatch_surface`.
//! - **Downstream:** the session logs under `$LOONGAGENT_HOME/projects/<slug>/`, and
//!   `sessions::session_dir` for the slug rule this crate already mirrors.
//! - **Verify:** `cargo test -p loong-web` (`usage_`).
//!
//! The log is the record (web-stats.md §2): this module reads the `cost.recorded` events
//! the kernel already wrote and adds them up. It never recomputes a price and never asks a
//! session child — a child answers a turn, and the bill outlives the turn.
//!
//! One pass per log, no index and no cache: a resumed session writes more, and a number
//! kept from the last look would be a number that lies. The price of that choice is a wait
//! on the first open of a large history, which web-stats.md §2 states as the trade.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::sessions;

/// The label a call that recorded no model gets in the model table, so the call is in a row
/// rather than quietly dropped (web-stats.md §1).
const UNNAMED_MODEL: &str = "未记录";

/// Purpose: One bucket's counters — what a set of `cost.recorded` events added up to.
/// Serves: `usage`, as the total and as every row.
/// Design: `docs/design/web-stats.md` §2 — the fields are `CostRecorded`'s, summed.
/// Up: `usage`.
/// Down: nothing; the JSON it prints is the wire.
/// Verify: `cargo test -p loong-web` (`usage_`).
#[derive(Default, Clone)]
struct Spend {
    /// model calls recorded, one per `cost.recorded`
    calls: u64,
    /// prompt tokens the provider reported
    tokens_in: u64,
    /// generated tokens, reasoning included
    tokens_out: u64,
    /// prompt tokens billed as cache hits
    cache_hit: u64,
    /// prompt tokens billed as cache misses
    cache_miss: u64,
    /// calls whose provider reported a cache split, so a 0% rate is not read as "no cache"
    cache_reported_calls: u64,
    /// the recorded bill, in micro-USD
    usd_micros: u64,
    /// calls a local model answered: ¥0 because it is free
    local_free_calls: u64,
    /// calls with no price at all (`price_basis: unknown`): ¥0 because nobody knows
    unpriced_calls: u64,
}

impl Spend {
    /// Purpose: Add one `cost.recorded` event's `data` to this bucket.
    /// Serves: `usage`, for the total and for each cut of it.
    /// Design: `docs/design/web-stats.md` §2.
    /// Contract: an event missing a field counts the field as 0 — a half-written line is
    /// one lost counter, never a lost session; `price_basis` decides free from unknown.
    fn add(&mut self, data: &Value) {
        let num = |name: &str| data.get(name).and_then(Value::as_u64).unwrap_or(0);
        self.calls += 1;
        self.tokens_in += num("tokens_in");
        self.tokens_out += num("tokens_out");
        self.cache_hit += num("cache_hit_tokens");
        self.cache_miss += num("cache_miss_tokens");
        self.usd_micros += num("usd_micros");
        if data
            .get("cache_reported")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            self.cache_reported_calls += 1;
        }
        match data.get("price_basis").and_then(Value::as_str) {
            Some("local-free") => self.local_free_calls += 1,
            Some("unknown") => self.unpriced_calls += 1,
            _ => {}
        }
    }

    /// Purpose: Fold another bucket into this one.
    /// Serves: `usage`, adding a session's spend to its model, its workspace and the total.
    /// Design: `docs/design/web-stats.md` §2.
    /// Contract: every counter adds; nothing here divides.
    fn merge(&mut self, other: &Spend) {
        self.calls += other.calls;
        self.tokens_in += other.tokens_in;
        self.tokens_out += other.tokens_out;
        self.cache_hit += other.cache_hit;
        self.cache_miss += other.cache_miss;
        self.cache_reported_calls += other.cache_reported_calls;
        self.usd_micros += other.usd_micros;
        self.local_free_calls += other.local_free_calls;
        self.unpriced_calls += other.unpriced_calls;
    }

    /// Purpose: Whether this bucket ever recorded a call.
    /// Serves: `usage`, dropping the sessions and workspaces that cost nothing.
    /// Design: `docs/design/web-stats.md` §2.
    /// Contract: a bucket with no calls stays out of the answer.
    fn is_empty(&self) -> bool {
        self.calls == 0
    }

    /// Purpose: This bucket's counters as they travel to the page.
    /// Serves: `usage`.
    /// Design: `docs/design/web-stats.md` §2 — the page prints these and takes the cache
    /// rate from `cache_hit` over `cache_hit + cache_miss`.
    /// Contract: the same nine keys on every row, so the table needs one shape.
    fn json(&self) -> Value {
        json!({
            "calls": self.calls,
            "tokens_in": self.tokens_in,
            "tokens_out": self.tokens_out,
            "cache_hit": self.cache_hit,
            "cache_miss": self.cache_miss,
            "cache_reported_calls": self.cache_reported_calls,
            "usd_micros": self.usd_micros,
            "local_free_calls": self.local_free_calls,
            "unpriced_calls": self.unpriced_calls,
        })
    }

    /// Purpose: The sort key this bucket holds: bill first, tokens second.
    /// Serves: `usage`, ordering every table the way the page draws it.
    /// Design: `docs/design/web-stats.md` §1 — "what is spending" is at the top.
    /// Contract: local models all share ¥0, so tokens break their tie instead of the order
    /// coming out of the directory listing.
    fn weight(&self) -> (u64, u64) {
        (self.usd_micros, self.tokens_in + self.tokens_out)
    }
}

/// Purpose: What one slug directory added up to, while `usage` is still reading it.
/// Serves: `usage`.
/// Design: `docs/design/web-stats.md` §3 — the workspace row and the `cwd` that names it.
/// Up: `usage`.
/// Down: `Spend`.
/// Verify: `cargo test -p loong-web` (`usage_`).
#[derive(Default)]
struct Project {
    /// the sum of its sessions' spend
    spend: Spend,
    /// how many of its sessions recorded a call
    sessions: u64,
    /// the directory its sessions ran in, when a `session.start` said so
    cwd: Option<String>,
}

/// Purpose: One session's spend, as the session table draws it.
/// Serves: `usage`.
/// Design: `docs/design/web-stats.md` §1.
/// Up: `usage`.
/// Down: `Spend` and the log it was read from.
/// Verify: `cargo test -p loong-web` (`usage_`).
struct Session {
    /// the slug of the directory its log sits in
    slug: String,
    /// the session id, as the rail names it
    id: String,
    /// the first thing the human said, for recognition; empty when nothing was said
    title: String,
    /// what its `cost.recorded` events add up to
    spend: Spend,
    /// the log's last event time, so a row can say when
    last_ms: u64,
}

/// Purpose: The whole answer to `stats.usage`: totals, three cuts, and the range they cover.
/// Serves: `crate::app::dispatch_surface`'s `stats.usage` arm.
/// Design: `docs/design/web-stats.md` §1–§3.
/// Up: `dispatch_surface`.
/// Down: `Spend`, `Project`, `Session`, and the logs under `<root>/projects/`.
/// Verify: `cargo test -p loong-web` (`usage_`).
pub fn usage(root: &Path, workspaces: &[PathBuf]) -> Value {
    let names = registered(root, workspaces);
    let mut totals = Spend::default();
    let mut models: BTreeMap<String, Spend> = BTreeMap::new();
    let mut projects: BTreeMap<String, Project> = BTreeMap::new();
    let mut sessions: Vec<Session> = Vec::new();
    let mut since = u64::MAX;
    let mut until = 0u64;

    for (slug, dir) in project_dirs(root) {
        let mut project = Project::default();
        for (id, path) in log_files(&dir) {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let mut spend = Spend::default();
            let mut per_model: BTreeMap<String, Spend> = BTreeMap::new();
            let mut title = String::new();
            let mut last_ms = 0u64;
            for line in text.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(value) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                let kind = value.get("kind").and_then(Value::as_str);
                let data = value.get("data").cloned().unwrap_or_default();
                let ms = value.get("time_ms").and_then(Value::as_u64);
                if let Some(ms) = ms {
                    last_ms = last_ms.max(ms);
                }
                match kind {
                    Some("cost.recorded") => {
                        spend.add(&data);
                        // One row per profile slot that answered (web-stats.md §1): a
                        // session that switched models is counted under each, because the
                        // bill is per call and not per session.
                        per_model.entry(model_name(&data)).or_default().add(&data);
                        if let Some(ms) = ms {
                            since = since.min(ms);
                            until = until.max(ms);
                        }
                    }
                    // Where the session ran: a slug is a fingerprint, and a workspace no
                    // seat has opened still has a readable name (web-stats.md §3).
                    Some("session.start") if project.cwd.is_none() => {
                        project.cwd = data
                            .get("cwd")
                            .and_then(Value::as_str)
                            .filter(|c| !c.is_empty())
                            .and_then(|c| Path::new(c).file_name())
                            .map(|n| n.to_string_lossy().to_string());
                    }
                    Some("message.appended")
                        if title.is_empty()
                            && data.get("role").and_then(Value::as_str) == Some("user") =>
                    {
                        title = first_line(data.get("text").and_then(Value::as_str).unwrap_or(""));
                    }
                    _ => {}
                }
            }
            if spend.is_empty() {
                // A session with no model call costs nothing and would be a zero row.
                continue;
            }
            for (model, spend) in per_model {
                models.entry(model).or_default().merge(&spend);
            }
            project.spend.merge(&spend);
            project.sessions += 1;
            totals.merge(&spend);
            sessions.push(Session {
                slug: slug.clone(),
                id,
                title,
                spend,
                last_ms,
            });
        }
        projects.insert(slug, project);
    }

    let label = |slug: &str, cwd: &Option<String>| {
        names
            .get(slug)
            .cloned()
            .or_else(|| cwd.clone())
            .unwrap_or_else(|| trimmed(slug))
    };
    sessions.sort_by(|a, b| b.spend.weight().cmp(&a.spend.weight()));
    let mut workspaces: Vec<Value> = projects
        .iter()
        .filter(|(_, project)| !project.spend.is_empty())
        .map(|(slug, project)| {
            let mut row = project.spend.json();
            let object = row.as_object_mut().expect("a Spend is an object");
            object.insert("workspace".into(), json!(label(slug, &project.cwd)));
            object.insert("slug".into(), json!(slug));
            object.insert("dir".into(), json!(project.cwd));
            object.insert("sessions".into(), json!(project.sessions));
            row
        })
        .collect();
    workspaces.sort_by(|a, b| weight_of(b).cmp(&weight_of(a)));
    let mut models: Vec<Value> = models
        .into_iter()
        .map(|(name, spend)| {
            let mut row = spend.json();
            row.as_object_mut()
                .expect("a Spend is an object")
                .insert("model".into(), json!(name));
            row
        })
        .collect();
    models.sort_by(|a, b| weight_of(b).cmp(&weight_of(a)));
    let sessions: Vec<Value> = sessions
        .into_iter()
        .map(|session| {
            let cwd = projects.get(&session.slug).and_then(|p| p.cwd.clone());
            let mut row = session.spend.json();
            let object = row.as_object_mut().expect("a Spend is an object");
            object.insert("workspace".into(), json!(label(&session.slug, &cwd)));
            object.insert("session_id".into(), json!(session.id));
            object.insert("title".into(), json!(session.title));
            object.insert("last_ms".into(), json!(session.last_ms));
            row
        })
        .collect();

    let projects_count = workspaces.len();
    let sessions_count = sessions.len();
    json!({
        "totals": totals.json(),
        "models": models,
        "workspaces": workspaces,
        "sessions": sessions,
        "covered": {
            // A page that says "all time" has to print the time it actually covered, and
            // count what it found rather than every slug directory that ever existed.
            "since_ms": if since == u64::MAX { 0 } else { since },
            "until_ms": until,
            "projects": projects_count,
            "sessions": sessions_count,
        },
    })
}

/// Purpose: The name of the profile slot that answered one call, for the model table.
/// Serves: `usage`.
/// Design: `docs/design/web-stats.md` §1 — the slot the model bar names.
/// Contract: `model` first, `profile` second, and a fixed label when neither was written,
/// so a call is never dropped from the model table for want of a name.
fn model_name(data: &Value) -> String {
    let name = data
        .get("model")
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty())
        .or_else(|| {
            data.get("profile")
                .and_then(Value::as_str)
                .filter(|m| !m.is_empty())
        });
    name.unwrap_or(UNNAMED_MODEL).to_string()
}

/// Purpose: The workspace name for each slug a seat has, so a registered place keeps its name.
/// Serves: `usage`.
/// Design: `docs/design/web-stats.md` §3 — registered first, the log's own `cwd` second.
/// Up: `usage`.
/// Down: `sessions::session_dir` for the slug rule.
/// Verify: `cargo test -p loong-web` (`usage_names_a_registered_workspace`).
fn registered(root: &Path, workspaces: &[PathBuf]) -> BTreeMap<String, String> {
    let mut names = BTreeMap::new();
    for workspace in workspaces {
        let dir = sessions::session_dir(root, workspace);
        if let Some(slug) = dir.file_name().and_then(|n| n.to_str()) {
            names.insert(slug.to_string(), label_of(workspace));
        }
    }
    names
}

/// Purpose: What a directory is called on the page: its last component.
/// Serves: `registered`, and through it every workspace row.
/// Design: `docs/design/web-stats.md` §3.
/// Contract: a path with no last component (`/`) keeps its text, so no row is nameless.
fn label_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}

/// Purpose: Every slug directory under the client's root, in a stable order.
/// Serves: `usage`.
/// Design: `docs/design/web-stats.md` §2 — the whole home, not only the open seats.
/// Up: `usage`.
/// Down: `<root>/projects/`.
/// Verify: `cargo test -p loong-web` (`usage_`).
fn project_dirs(root: &Path) -> Vec<(String, PathBuf)> {
    let mut dirs = Vec::new();
    let Ok(entries) = std::fs::read_dir(root.join("projects")) else {
        return dirs;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if let Some(slug) = path.file_name().and_then(|n| n.to_str()) {
            dirs.push((slug.to_string(), path));
        }
    }
    dirs.sort();
    dirs
}

/// Purpose: The session logs inside one slug directory, `history.jsonl` excluded as the rail does.
/// Serves: `usage`.
/// Design: `docs/design/sessions-lineage.md` §1 — a session is its file.
/// Up: `usage`.
/// Down: the files in `<root>/projects/<slug>/`.
/// Verify: `cargo test -p loong-web` (`usage_`).
fn log_files(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut files = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return files;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let stem = path.file_stem().map(|s| s.to_string_lossy().to_string());
        let id = match stem.as_deref() {
            Some("history") => continue,
            Some("session") | None | Some("") => "default".to_string(),
            Some(other) => other.to_string(),
        };
        files.push((id, path));
    }
    files.sort();
    files
}

/// Purpose: The slug without its fingerprint hash, for a workspace nothing else could name.
/// Serves: `usage`.
/// Design: `docs/design/web-stats.md` §3 — the last resort, so a row is never blank.
/// Contract: `-a1b2c3d4` is dropped only when the tail looks like the hash `project_slug` writes.
fn trimmed(slug: &str) -> String {
    match slug.rsplit_once('-') {
        Some((head, tail)) if !head.is_empty() && is_hash(tail) => head.to_string(),
        _ => slug.to_string(),
    }
}

/// Purpose: Whether a slug's tail is the eight hex digits `project_slug` appends.
/// Serves: `trimmed`.
/// Design: `docs/design/sessions-lineage.md` §1.
/// Contract: eight ASCII hex digits, nothing else.
fn is_hash(tail: &str) -> bool {
    tail.len() == 8 && tail.chars().all(|c| c.is_ascii_hexdigit())
}

/// Purpose: A title's first line, cut short, so a session row has one line to print.
/// Serves: `usage`, for the session table's title.
/// Design: `docs/design/web-stats.md` §1.
/// Contract: whitespace collapses to single spaces and the result is at most 48 characters;
/// the cut is on a character boundary, so a multi-byte prompt is never split.
fn first_line(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(48).collect()
}

/// Purpose: The sort key a finished row carries, read back from the JSON it printed.
/// Serves: `usage`, ordering the model and workspace tables.
/// Design: `docs/design/web-stats.md` §1.
/// Contract: a row always carries both counters, so the key is never missing.
fn weight_of(row: &Value) -> (u64, u64) {
    let num = |name: &str| row.get(name).and_then(Value::as_u64).unwrap_or(0);
    (num("usd_micros"), num("tokens_in") + num("tokens_out"))
}

#[cfg(test)]
mod tests {
    //! Purpose: The statistics page's numbers: the sums, the three cuts, and the workspace
    //! naming rule of web-stats.md §3.
    //!
    //! # Contract
    //! - **Serves:** `cargo test -p loong-web`.
    //! - **Design:** `docs/design/web-stats.md` §1–§3.
    //! - **Upstream:** `scripts/check.sh`.
    //! - **Downstream:** `crate::stats::usage` and the log layout it reads.
    //! - **Verify:** `cargo test -p loong-web`.

    use super::*;

    /// Purpose: A throwaway client root with a slug directory to plant logs in.
    /// Serves: every test in this module.
    /// Design: `docs/design/sessions-lineage.md` §1.
    /// Up: the tests below.
    /// Down: the filesystem under the temp dir.
    /// Verify: `cargo test -p loong-web` (`usage_`).
    struct Fixture {
        /// the client root the logs live under
        root: PathBuf,
        /// the slug directory inside it
        dir: PathBuf,
    }

    impl Fixture {
        /// Purpose: Make the root and one slug directory named `slug`.
        /// Serves: every test in this module.
        /// Design: `docs/design/sessions-lineage.md` §1.
        /// Up: the tests' bodies.
        /// Down: `Fixture::plant`.
        /// Verify: `cargo test -p loong-web` (`usage_`).
        fn new(slug: &str) -> Self {
            // Unique per test and per call: the tests run in parallel threads.
            let base = std::env::temp_dir().join(format!(
                "loong-stats-{}-{:?}-{}",
                std::process::id(),
                std::thread::current().id(),
                slug
            ));
            let _ = std::fs::remove_dir_all(&base);
            let fixture = Fixture {
                root: base.join("home"),
                dir: base.join("home").join("projects").join(slug),
            };
            std::fs::create_dir_all(&fixture.dir).unwrap();
            fixture
        }

        /// Purpose: Write one session log with a title and one model call per `(model, usd)`.
        /// Serves: every test in this module.
        /// Design: `docs/design/web-stats.md` §2 — the `CostRecorded` fields it sums.
        /// Up: the tests' bodies.
        /// Down: the `<dir>/<id>.jsonl` file.
        /// Verify: `cargo test -p loong-web` (`usage_`).
        fn plant(&self, id: &str, title: &str, calls: &[(&str, u64, u64)]) {
            let mut lines = vec![json!({
                "kind": "message.appended", "seq": 1, "time_ms": 1_000, "id": "e1",
                "data": {"role": "user", "text": title}
            })];
            for (i, (model, tokens_in, usd)) in calls.iter().enumerate() {
                lines.push(json!({
                    "kind": "cost.recorded", "seq": 2 + i, "time_ms": 2_000 + i as u64,
                    "id": format!("e{}", 2 + i),
                    "data": {
                        "profile": "default", "model": model,
                        "tokens_in": tokens_in, "tokens_out": 10,
                        "cache_hit_tokens": 100, "cache_miss_tokens": 300,
                        "cache_reported": true, "usd_micros": usd,
                        "price_basis": "list-peak",
                    }
                }));
            }
            let text = lines
                .iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            std::fs::write(self.dir.join(format!("{id}.jsonl")), text + "\n").unwrap();
        }

        /// Purpose: Write a log whose only event records where the session ran.
        /// Serves: `usage_reads_an_unregistered_directory_from_its_log`.
        /// Design: `docs/design/web-stats.md` §3.
        /// Up: that test.
        /// Down: the `<dir>/<id>.jsonl` file.
        /// Verify: `cargo test -p loong-web` (`usage_reads_an_unregistered_directory_from_its_log`).
        fn plant_start(&self, id: &str, cwd: &str) {
            let line = json!({
                "kind": "session.start", "seq": 1, "time_ms": 500, "id": "e1",
                "data": {"cwd": cwd, "model": "claude"}
            });
            std::fs::write(
                self.dir.join(format!("{id}.jsonl")),
                line.to_string() + "\n",
            )
            .unwrap();
        }
    }

    impl Drop for Fixture {
        /// Purpose: Delete the temporary client root this fixture made.
        /// Serves: every test in this module.
        /// Design: `docs/design/web-stats.md` §2.
        /// Up: the tests' scope.
        /// Down: the temp directory.
        /// Verify: `cargo test -p loong-web`.
        fn drop(&mut self) {
            if let Some(base) = self.root.parent() {
                let _ = std::fs::remove_dir_all(base);
            }
        }
    }

    /// Purpose: Two sessions and two models add up in the total and split across the cuts.
    /// Serves: `usage` — the whole answer in one assertion.
    /// Design: `docs/design/web-stats.md` §1–§2.
    /// Up: the page's headline row and its three tables.
    /// Down: `Fixture`.
    /// Verify: `cargo test -p loong-web` (`usage_sums_by_model_workspace_and_session`).
    #[test]
    fn usage_sums_by_model_workspace_and_session() {
        let fixture = Fixture::new("proj-0000abcd");
        fixture.plant("s-1", "the older session", &[("opus", 1_000, 500)]);
        fixture.plant(
            "s-2",
            "the newer session",
            &[("opus", 2_000, 100), ("haiku", 3_000, 1)],
        );
        let answer = usage(&fixture.root, &[]);

        let totals = &answer["totals"];
        assert_eq!(totals["calls"], 3);
        assert_eq!(totals["tokens_in"], 6_000);
        assert_eq!(totals["tokens_out"], 30);
        assert_eq!(totals["cache_hit"], 300);
        assert_eq!(totals["cache_reported_calls"], 3);
        assert_eq!(totals["usd_micros"], 601);

        // Costliest first, and one row per slot rather than one per session.
        let models = answer["models"].as_array().unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0]["model"], "opus");
        assert_eq!(models[0]["calls"], 2);
        assert_eq!(models[1]["model"], "haiku");
        assert_eq!(models[1]["usd_micros"], 1);

        // The directory is unregistered and no log said where it ran, so the slug names it
        // with its fingerprint trimmed (web-stats.md §3).
        let workspaces = answer["workspaces"].as_array().unwrap();
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0]["workspace"], "proj");
        assert_eq!(workspaces[0]["sessions"], 2);
        assert_eq!(workspaces[0]["usd_micros"], 601);

        // Spendiest session first, with the title the row is recognised by.
        let sessions = answer["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0]["session_id"], "s-1");
        assert_eq!(sessions[0]["title"], "the older session");
        assert_eq!(sessions[0]["usd_micros"], 500);
        assert_eq!(sessions[1]["session_id"], "s-2");

        // "All time" is only honest with the range printed next to it.
        assert_eq!(answer["covered"]["since_ms"], 2_000);
        assert_eq!(answer["covered"]["until_ms"], 2_001);
    }

    /// Purpose: A registered workspace keeps its own name, ahead of anything the log said.
    /// Serves: `usage` and `registered`.
    /// Design: `docs/design/web-stats.md` §3.
    /// Up: the workspace table.
    /// Down: `Fixture`, `sessions::session_dir`.
    /// Verify: `cargo test -p loong-web` (`usage_names_a_registered_workspace`).
    #[test]
    fn usage_names_a_registered_workspace() {
        // The slug is a function of the workspace path alone, so it can be computed before
        // the root the fixture plants under exists (sessions-lineage.md §1).
        let placeholder = Fixture::new("placeholder-0000abcd");
        let workspace = placeholder.root.parent().unwrap().join("the-workspace");
        let slug = sessions::session_dir(&placeholder.root, &workspace)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let fixture = Fixture::new(&slug);
        fixture.plant("s-1", "hello", &[("opus", 10, 5)]);
        fixture.plant_start("s-2", "/elsewhere/another-place");

        let answer = usage(&fixture.root, &[workspace]);
        let workspaces = answer["workspaces"].as_array().unwrap();
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0]["workspace"], "the-workspace");
        assert_eq!(workspaces[0]["dir"], "another-place");
        assert_eq!(answer["sessions"][0]["workspace"], "the-workspace");
    }

    /// Purpose: A directory no seat has opened is named from the `cwd` its log recorded.
    /// Serves: `usage`.
    /// Design: `docs/design/web-stats.md` §3 — the whole home is counted, not the open seats.
    /// Up: the workspace table.
    /// Down: `Fixture`.
    /// Verify: `cargo test -p loong-web` (`usage_reads_an_unregistered_directory_from_its_log`).
    #[test]
    fn usage_reads_an_unregistered_directory_from_its_log() {
        let fixture = Fixture::new("other-0000abcd");
        fixture.plant("s-1", "hello", &[("opus", 10, 5)]);
        fixture.plant_start("s-2", "/home/someone/yesterday");

        let answer = usage(&fixture.root, &[]);
        assert_eq!(answer["workspaces"][0]["workspace"], "yesterday");
        assert_eq!(answer["workspaces"][0]["dir"], "yesterday");
    }

    /// Purpose: A log with no model call is not a zero row, and `history.jsonl` is not a session.
    /// Serves: `usage`.
    /// Design: `docs/design/web-stats.md` §2.
    /// Up: the three tables.
    /// Down: `Fixture`.
    /// Verify: `cargo test -p loong-web` (`usage_skips_what_costs_nothing`).
    #[test]
    fn usage_skips_what_costs_nothing() {
        let fixture = Fixture::new("proj-0000abcd");
        fixture.plant_start("s-quiet", "/home/someone/elsewhere");
        std::fs::write(
            fixture.dir.join("history.jsonl"),
            "{\"kind\":\"message.appended\"}\n",
        )
        .unwrap();

        let answer = usage(&fixture.root, &[]);
        assert_eq!(answer["totals"]["calls"], 0);
        assert!(answer["sessions"].as_array().unwrap().is_empty());
        assert!(answer["workspaces"].as_array().unwrap().is_empty());
        assert_eq!(answer["covered"]["since_ms"], 0);
    }

    /// Purpose: A local model and an unknown price are two different zeros.
    /// Serves: `usage`.
    /// Design: `docs/design/web-stats.md` §4 — `price_basis` decides free from unknown.
    /// Up: the footnote under the totals.
    /// Down: `Fixture`.
    /// Verify: `cargo test -p loong-web` (`usage_counts_free_and_unpriced_apart`).
    #[test]
    fn usage_counts_free_and_unpriced_apart() {
        let fixture = Fixture::new("proj-0000abcd");
        let line = json!({
            "kind": "cost.recorded", "seq": 1, "time_ms": 1_000, "id": "e1",
            "data": {"profile": "local", "model": "qwen", "tokens_in": 5, "tokens_out": 5,
                     "usd_micros": 0, "price_basis": "local-free"}
        });
        std::fs::write(fixture.dir.join("s-1.jsonl"), line.to_string() + "\n").unwrap();

        let answer = usage(&fixture.root, &[]);
        assert_eq!(answer["totals"]["local_free_calls"], 1);
        assert_eq!(answer["totals"]["unpriced_calls"], 0);
        // A call with no cache split reported must not read as a 0% hit rate.
        assert_eq!(answer["totals"]["cache_reported_calls"], 0);
    }
}
