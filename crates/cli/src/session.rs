//! Purpose: Sessions as files: the id, the path, the listing, the resolution, the fork and the branch delete.
//!
//! # Contract
//! - **Serves:** `tui`, `resume`, `sessions`, `fork` and `delete`, and `Args::finish_session`, which picks the session before any command runs.
//! - **Design:** `docs/design/surfaces.md` §2; `docs/design/sessions-lineage.md` §1, §3–§5.
//! - **Upstream:** `main.rs`'s session arms and `Args::finish_session` call in.
//! - **Downstream:** `loong_eventlog` (`replay_lenient`, `SessionLog`), `loong_schema`'s `SessionForked` and the filesystem under the client's one root.
//! - **Verify:** `cargo test -p loongagent` · `./scripts/check.sh`

use super::*;

/// Purpose: Which of the three ways this run got its session, so the interface can say so.
/// Serves: `session_intro` and `Args::finish_session`.
/// Design: `docs/design/sessions-lineage.md` §5.
/// Up: `Args::finish_session` sets it; `session_intro` reads it.
/// Down: no further call; it is a plain enum.
/// Verify: `cargo test -p loongagent`.
/// `~/.loongagent/projects/<project>/session.jsonl` (or under `$LOONGAGENT_HOME`).
///
/// Sessions are state, not configuration, so they live under the state dir and are
/// keyed by the project they belong to: resuming yesterday's work in a directory is
/// the default, and `--session` is only for choosing differently.
/// Which of the three ways this run got its session. The interface says which, because
/// "did I just continue yesterday's conversation or start a new one?" is not a question
/// anyone should have to infer from a file listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionSource {
    /// A fresh id (`--new`, or nothing to continue).
    Created,
    /// Someone named it (`--session`, `resume`).
    Named,
    /// The default: the newest session in this project.
    Continued,
}

/// Purpose: The first line of the interface: which session this is and, when continuing, how to start a fresh one.
/// Serves: `tui.rs`'s intro line.
/// Design: `docs/design/sessions-lineage.md` §5.
/// Up: `tui.rs`'s `full_screen`; the session tests.
/// Down: `format!`; no further call.
/// Verify: `cargo test -p loongagent`.
/// The first line of the interface, and the whole of what someone needs to know about
/// which session they are in.
pub(crate) fn session_intro(source: SessionSource, id: &str, events: u64) -> String {
    match source {
        SessionSource::Created => format!("new session {id}"),
        SessionSource::Named => format!("reopened session {id} ({events} events)"),
        SessionSource::Continued => format!(
            "continuing session {id} ({events} events) — `loongagent tui --new` starts a fresh one"
        ),
    }
}

/// Purpose: Where one project's sessions live, under the client's root.
/// Serves: `server.rs`'s factory and `session_path_for`.
/// Design: `docs/design/sessions-lineage.md` §1.
/// Up: `server.rs`'s `serve` (the factory closure) and `session_path_in`.
/// Down: `session_dir_in` and `project_slug`.
/// Verify: `cargo test -p loongagent`.
/// Where one project's sessions live. A project is a directory; its slug names the
/// folder, so two checkouts of the same tree do not share a conversation by accident.
pub(crate) fn session_dir(cwd: &Path) -> PathBuf {
    session_dir_in(&config_dir(), cwd)
}

/// Purpose: The same directory under an injected root, for tests and for `new_session_id_in`.
/// Contract: `session.rs` path layout; `session_dir`, `new_session_id_in`, `newest_session_in` and `resolve_session_in` call it.
/// The directory a project's sessions live in, under a given root. The root is a
/// parameter so the tests can use one of their own instead of mutating the environment
/// of every other test in the binary.
fn session_dir_in(root: &Path, cwd: &Path) -> PathBuf {
    root.join("projects").join(project_slug(cwd))
}

/// Purpose: The file's own name as the id, with the legacy `session.jsonl` answering to `default`.
/// Serves: `Args::session_id`, `resume`, `delete` and the listing.
/// Design: `docs/design/sessions-lineage.md` §1.
/// Up: `Args::session_id`, `fork`, `delete`, `session_summaries_in` and `resolve_session_in`.
/// Down: `Path::file_stem`; no further call.
/// Verify: `cargo test -p loongagent`.
/// A session **is** its file, and the file's name is its id: no index to keep in step,
/// and `ls` is a valid way to look. The first session ever written kept the name
/// `session.jsonl`; it answers to `default` rather than looking like an accident.
pub(crate) fn session_id_of(path: &Path) -> String {
    match path
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
    {
        Some(stem) if stem != "session" && !stem.is_empty() => stem,
        _ => "default".to_string(),
    }
}

/// Purpose: The file a given id names in this project.
/// Serves: `Args::finish_session` for `--new` and the default case, and `resume`.
/// Design: `docs/design/sessions-lineage.md` §1.
/// Up: `Args::finish_session` and `resume`; the session tests.
/// Down: `session_path_in`, `session_dir_in` and `config_dir`.
/// Verify: `cargo test -p loongagent`.
pub(crate) fn session_path_for(id: &str, cwd: &Path) -> PathBuf {
    session_path_in(&config_dir(), id, cwd)
}

/// Purpose: The same mapping under an injected root, with `default` keeping its legacy file name.
/// Contract: `session.rs` path layout; `session_path_for` and the session tests call it.
fn session_path_in(root: &Path, id: &str, cwd: &Path) -> PathBuf {
    if id == "default" {
        session_dir_in(root, cwd).join("session.jsonl")
    } else {
        session_dir_in(root, cwd).join(format!("{id}.jsonl"))
    }
}

/// Purpose: A fresh eight-character id that says itself out loud and does not collide.
/// Serves: `Args::finish_session` and `server.rs`'s factory.
/// Design: `docs/design/sessions-lineage.md` §1.
/// Up: `Args::finish_session` and `server.rs`'s factory closure.
/// Down: `new_session_id_in` and `new_session_id_in_dir`.
/// Verify: `cargo test -p loongagent`.
/// A new id: eight characters, from a clock and this process, in an alphabet with the
/// pairs people misread removed (`0`/`O`, `1`/`l`/`I`). Short enough to type back,
/// random enough that a prefix is not a guess.
pub(crate) fn new_session_id(cwd: &Path) -> String {
    new_session_id_in(&config_dir(), cwd)
}

/// Purpose: The same generator under an injected root, for tests.
/// Contract: `session.rs` id generation; `new_session_id` and the session tests call it.
fn new_session_id_in(root: &Path, cwd: &Path) -> String {
    new_session_id_in_dir(&session_dir_in(root, cwd))
}

/// Purpose: Generate an id in a named directory, which is how `fork` puts a child beside its parent.
/// Serves: `fork` and `new_session_id`.
/// Design: `docs/design/sessions-lineage.md` §1 and §4.
/// Up: `new_session_id_in` and `fork`; no other caller.
/// Down: the clock, the process id and the collision check on the directory.
/// Verify: `cargo test -p loongagent`.
/// The same, for a directory named directly: `fork` puts the child beside its parent,
/// which may be a project other than the one this command was run in.
fn new_session_id_in_dir(dir: &Path) -> String {
    const ALPHABET: &[u8] = b"23456789abcdefghjkmnpqrstuvwxyz";
    for attempt in 0..256u64 {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let mut hash = 0xcbf2_9ce4_8422_2325u64 ^ u64::from(std::process::id()) ^ attempt;
        for byte in nanos
            .to_le_bytes()
            .iter()
            .chain(std::process::id().to_le_bytes().iter())
        {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        let id: String = (0..8)
            .map(|i| ALPHABET[((hash >> (i * 5)) % ALPHABET.len() as u64) as usize] as char)
            .collect();
        if !dir.join(format!("{id}.jsonl")).exists() {
            return id;
        }
    }
    // Asking the clock harder is not a plan; this cannot collide with a base32 id.
    format!("p{}", std::process::id())
}

/// Purpose: What a session looks like from outside, read from its own log rather than an index.
/// Serves: the listing, the resolution and the branch delete.
/// Design: `docs/design/sessions-lineage.md` §1 and §3.
/// Up: `session_summaries_in` builds them; `list_sessions`, `match_session`, `branch_of` and `delete` read them.
/// Down: no further call; it is data.
/// Verify: `cargo test -p loongagent`.
/// What a session looks like from outside, read from the log itself — the log is the
/// record, so a list must not need an index that could disagree with it.
#[derive(Debug, Clone)]
struct SessionSummary {
    id: String,
    path: PathBuf,
    project: String,
    modified: SystemTime,
    events: u64,
    turns: u64,
    first_prompt: Option<String>,
    /// Where this session came from, if it is a child (ADR-0015).
    parent: Option<(String, u64)>,
}

/// Purpose: Read every session log in a directory into summaries, counting the child's own events only.
/// Serves: `session_summaries`, `all_session_summaries_in`, `newest_session_in` and `delete`.
/// Design: `docs/design/sessions-lineage.md` §1 and §3.
/// Up: `session_summaries`, `all_session_summaries_in`, `newest_session_in` and `delete`.
/// Down: `std::fs::read_dir`, the JSONL line parser, `session_id_of` and `SessionSummary`.
/// Verify: `cargo test -p loongagent`.
fn session_summaries_in(dir: &Path, project: &str) -> Vec<SessionSummary> {
    let mut sessions = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return sessions;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(UNIX_EPOCH);
        let mut events = 0u64;
        let mut turns = 0u64;
        let mut first_prompt = None;
        let mut parent = None;
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            events += 1;
            let kind = value
                .get("kind")
                .and_then(|k| k.as_str())
                .unwrap_or_default();
            // Lineage is recorded once, as the child's first event.
            if kind == gen::event::SESSION_FORKED && events == 1 {
                let data = value.get("data").cloned().unwrap_or_default();
                if let Some(id) = data.get("parent_session").and_then(|v| v.as_str()) {
                    parent = Some((
                        id.to_string(),
                        data.get("parent_seq").and_then(|v| v.as_u64()).unwrap_or(0),
                    ));
                }
                continue;
            }
            if kind != "message.appended" {
                continue;
            }
            let data = value.get("data").cloned().unwrap_or_default();
            if data.get("role").and_then(|r| r.as_str()) != Some("user") {
                continue;
            }
            turns += 1;
            if first_prompt.is_none() {
                let text = data
                    .get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or_default();
                let flattened = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if !flattened.is_empty() {
                    first_prompt = Some(flattened);
                }
            }
        }
        sessions.push(SessionSummary {
            id: session_id_of(&path),
            project: project.to_string(),
            modified,
            events,
            turns,
            first_prompt,
            parent,
            path,
        });
    }
    sessions.sort_by(|a, b| b.modified.cmp(&a.modified).then(a.id.cmp(&b.id)));
    sessions
}

/// Purpose: This project's sessions, newest first.
/// Contract: `session.rs` listing; `list_sessions` is the only caller.
/// This project's sessions, newest first.
fn session_summaries(cwd: &Path) -> Vec<SessionSummary> {
    session_summaries_in(&session_dir(cwd), &project_slug(cwd))
}

/// Purpose: Every project's sessions, newest first.
/// Contract: `session.rs` listing; `list_sessions --all` and `resume` reach it through resolution.
/// Every project's sessions, newest first. `--all` for the list, and how `resume` finds
/// a session that is not in the current directory.
fn all_session_summaries() -> Vec<SessionSummary> {
    all_session_summaries_in(&config_dir())
}

/// Purpose: The same walk under an injected root, one directory per project.
/// Serves: `all_session_summaries` and `resolve_session_in`'s cross-project search.
/// Design: `docs/design/sessions-lineage.md` §3.
/// Up: `all_session_summaries` and `resolve_session_in`; no other caller.
/// Down: `session_summaries_in` and `std::fs::read_dir`.
/// Verify: `cargo test -p loongagent`.
fn all_session_summaries_in(root: &Path) -> Vec<SessionSummary> {
    let mut sessions = Vec::new();
    let root = root.join("projects");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return sessions;
    };
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let slug = entry.file_name().to_string_lossy().to_string();
        sessions.extend(session_summaries_in(&entry.path(), &slug));
    }
    sessions.sort_by(|a, b| b.modified.cmp(&a.modified).then(a.id.cmp(&b.id)));
    sessions
}

/// Purpose: The session a bare surface would continue, or `None` when this project has none.
/// Serves: `Args::finish_session`, `resolve_session` for `latest`, and `fork` and `list_sessions`.
/// Design: `docs/design/sessions-lineage.md` §5.
/// Up: `Args::finish_session`, `resolve_session_in`, `fork` and `list_sessions`.
/// Down: `newest_session_in`.
/// Verify: `cargo test -p loongagent`.
pub(crate) fn newest_session(cwd: &Path) -> Option<PathBuf> {
    newest_session_in(&config_dir(), cwd)
}

/// Purpose: The newest session in one directory, under an injected root.
/// Contract: `session.rs` resolution; `newest_session` and `resolve_session_in` call it.
fn newest_session_in(root: &Path, cwd: &Path) -> Option<PathBuf> {
    session_summaries_in(&session_dir_in(root, cwd), &project_slug(cwd))
        .first()
        .map(|s| s.path.clone())
}

/// Purpose: Turn a path, an id or a prefix into a session file, naming the candidates when the answer is several.
/// Serves: `Args::finish_session`, `fork` and `delete`.
/// Design: `docs/design/sessions-lineage.md` §5.
/// Up: `Args::finish_session`, `fork` and `delete`; the session tests.
/// Down: `resolve_session_in`.
/// Verify: `cargo test -p loongagent`.
/// Turn what someone typed into a session file. A path stays a path; anything else is an
/// id — exact, an unambiguous prefix of one, or `latest`. The error names the candidates
/// rather than saying "not found" when the answer is "several".
pub(crate) fn resolve_session(spec: &str, cwd: &Path) -> Result<PathBuf, String> {
    resolve_session_in(&config_dir(), spec, cwd)
}

/// Purpose: The same resolution under an injected root, including the cross-project fallback.
/// Serves: `resolve_session` and the session tests.
/// Design: `docs/design/sessions-lineage.md` §3 and §5.
/// Up: `resolve_session` and the session tests; no other caller.
/// Down: `session_dir_in`, `newest_session_in`, `session_summaries_in`, `match_session`, `all_session_summaries_in` and `match_session_across_projects`.
/// Verify: `cargo test -p loongagent`.
fn resolve_session_in(root: &Path, spec: &str, cwd: &Path) -> Result<PathBuf, String> {
    if spec.contains('/') || spec.ends_with(".jsonl") {
        return Ok(PathBuf::from(spec));
    }
    let dir = session_dir_in(root, cwd);
    let wanted = spec.to_ascii_lowercase();
    if wanted == "latest" || wanted == "last" {
        return newest_session_in(root, cwd).ok_or_else(|| {
            format!(
                "no sessions in {} yet — start one with `loongagent tui`",
                dir.display()
            )
        });
    }
    let sessions = session_summaries_in(&dir, &project_slug(cwd));
    if let Some(path) = match_session(&sessions, &wanted)? {
        return Ok(path);
    }
    // Not here: it may be a session from another project, which `sessions --all` shows.
    // Resuming it is allowed; the workspace stays wherever this command was run.
    let elsewhere = all_session_summaries_in(root);
    match match_session_across_projects(&elsewhere, &wanted)? {
        Some(path) => Ok(path),
        None => Err(format!(
            "no session {spec:?} — `loongagent sessions` lists what exists{}; `loongagent tui` \
             starts a new one",
            match sessions.len() {
                0 => String::new(),
                _ => format!(
                    " (here: {})",
                    sessions
                        .iter()
                        .map(|s| s.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }
        )),
    }
}

/// Purpose: Resolve a session in another project, refusing to guess when the same legacy id exists twice.
/// Serves: `resolve_session_in`'s fallback.
/// Design: `docs/design/sessions-lineage.md` §3.
/// Up: `resolve_session_in`; no other caller.
/// Down: no further call; it compares summaries.
/// Verify: `cargo test -p loongagent`.
/// The same, but for sessions in *other* projects: here an exact id is not enough to
/// disambiguate, because every legacy session in every project is called `default`.
/// Choosing one silently would be worse than saying which ones exist.
fn match_session_across_projects(
    sessions: &[SessionSummary],
    wanted: &str,
) -> Result<Option<PathBuf>, String> {
    let exact: Vec<&SessionSummary> = sessions.iter().filter(|s| s.id == wanted).collect();
    if let [one] = exact.as_slice() {
        return Ok(Some(one.path.clone()));
    }
    let candidates: Vec<&SessionSummary> = if exact.is_empty() {
        sessions
            .iter()
            .filter(|s| s.id.starts_with(wanted))
            .collect()
    } else {
        exact
    };
    match candidates.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(one.path.clone())),
        many => Err(format!(
            "{wanted:?} matches {} sessions in other projects ({}); run it where it lives, or \
             pass the full path",
            many.len(),
            many.iter()
                .map(|s| format!("{} in {}", s.id, s.project))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Purpose: Exact id first, then an unambiguous prefix, and an error naming the candidates otherwise.
/// Serves: `resolve_session_in`.
/// Design: `docs/design/sessions-lineage.md` §5.
/// Up: `resolve_session_in`; no other caller.
/// Down: no further call; it compares summaries.
/// Verify: `cargo test -p loongagent`.
/// Exact id, then an unambiguous prefix. `Ok(None)` means "not here"; the error is only
/// for the case where the answer is "several".
fn match_session(sessions: &[SessionSummary], wanted: &str) -> Result<Option<PathBuf>, String> {
    if let Some(found) = sessions.iter().find(|s| s.id == wanted) {
        return Ok(Some(found.path.clone()));
    }
    let matching: Vec<&SessionSummary> = sessions
        .iter()
        .filter(|s| s.id.starts_with(wanted))
        .collect();
    match matching.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(one.path.clone())),
        many => Err(format!(
            "{wanted:?} matches {} sessions ({}); give more characters",
            many.len(),
            many.iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Purpose: How long ago, in the only units a person reads in a list.
/// Contract: `session.rs` listing; `list_sessions` and `delete` print it.
/// How long ago, in the only units a person reads in a list.
fn age_text(then: SystemTime) -> String {
    let secs = SystemTime::now()
        .duration_since(then)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match secs {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86399 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86400),
    }
}

/// Purpose: Shorten a string to a column budget without breaking a character.
/// Contract: `session.rs` listing; `list_sessions` is the only caller.
fn clip(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let head: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{head}…")
}

/// Purpose: Print this project's sessions or every project's, newest first, with the one a bare surface would pick.
/// Serves: `loongagent sessions [--all] [--limit N]`.
/// Design: `docs/design/sessions-lineage.md` §3 and §5.
/// Up: `main.rs`'s `sessions` arm; no other caller.
/// Down: `session_summaries`, `all_session_summaries`, `newest_session`, `age_text` and `clip`.
/// Verify: `cargo test -p loongagent`.
/// `loongagent sessions [--all] [--limit N] [--cwd DIR]`
pub(crate) fn list_sessions(rest: &[String]) -> Result<(), String> {
    let mut cwd = std::env::current_dir().map_err(|e| format!("current directory: {e}"))?;
    let mut all = false;
    let mut limit = 20usize;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--all" => all = true,
            "--limit" => {
                i += 1;
                limit = rest
                    .get(i)
                    .and_then(|value| value.parse().ok())
                    .ok_or("--limit needs a number")?;
            }
            "--cwd" => {
                i += 1;
                cwd = PathBuf::from(rest.get(i).ok_or("--cwd needs a path")?);
            }
            other => return Err(format!("unexpected {other:?}\n\n{USAGE}")),
        }
        i += 1;
    }
    let sessions = if all {
        all_session_summaries()
    } else {
        session_summaries(&cwd)
    };
    if sessions.is_empty() {
        println!(
            "no sessions {} yet — `loongagent tui` starts one",
            if all {
                "on this machine".to_string()
            } else {
                format!("in {}", project_slug(&cwd))
            }
        );
        return Ok(());
    }
    // The one a bare `loongagent tui` would pick up. Compared by path, not by id:
    // every legacy session is called `default`, and marking five rows is not a mark.
    let current = newest_session(&cwd);
    println!(
        "{} session{} (newest first){}",
        sessions.len(),
        if sessions.len() == 1 { "" } else { "s" },
        if all {
            ""
        } else {
            " — --all shows every project"
        }
    );
    if all {
        println!(
            "  {:<3} {:<9} {:<11} {:>5} {:>7}  {:<14} {:<20} {}",
            "", "id", "when", "turns", "events", "from", "project", "first prompt"
        );
    } else {
        println!(
            "  {:<3} {:<9} {:<11} {:>5} {:>7}  {:<14} {}",
            "", "id", "when", "turns", "events", "from", "first prompt"
        );
    }
    for session in sessions.iter().take(limit) {
        let mark = if Some(&session.path) == current.as_ref() {
            "*"
        } else {
            " "
        };
        let prompt = session
            .first_prompt
            .as_deref()
            .map(|text| clip(text, 46))
            .unwrap_or_else(|| "—".to_string());
        let from = session
            .parent
            .as_ref()
            .map(|(parent, at)| format!("{parent}@{at}"))
            .unwrap_or_else(|| "—".to_string());
        if all {
            println!(
                "  {mark:<3} {:<9} {:<11} {:>5} {:>7}  {:<14} {:<20} {}",
                session.id,
                age_text(session.modified),
                session.turns,
                session.events,
                clip(&from, 14),
                clip(&session.project, 20),
                prompt
            );
        } else {
            println!(
                "  {mark:<3} {:<9} {:<11} {:>5} {:>7}  {:<14} {}",
                session.id,
                age_text(session.modified),
                session.turns,
                session.events,
                clip(&from, 14),
                prompt
            );
        }
    }
    if sessions.len() > limit {
        println!("  … {} more (--limit N)", sessions.len() - limit);
    }
    println!();
    println!(
        "  * = what `loongagent tui` picks up here; `loongagent resume <id>` opens any of them"
    );
    Ok(())
}

/// Purpose: Create a child session that references its parent's prefix instead of copying it.
/// Serves: `loongagent fork [ID] [--at SEQ]`.
/// Design: `docs/design/sessions-lineage.md` §4.
/// Up: `main.rs`'s `fork` arm; no other caller.
/// Down: `resolve_session`, `newest_session`, `session_id_of`, `replay_lenient`, `new_session_id_in_dir` and `SessionLog::append`.
/// Verify: `cargo test -p loongagent`.
/// `loongagent fork [ID] [--at SEQ] [--cwd DIR]`
///
/// A child session (ADR-0015, level 2): a new log that **references** its parent's
/// prefix rather than copying it. The reference is the parent's id plus the sequence it
/// continues from, recorded as the child's first event; the child then lives beside its
/// parent, so resolving that id back to a file is a name, not a path that can go stale.
pub(crate) fn fork(rest: &[String]) -> Result<(), String> {
    let mut cwd = std::env::current_dir().map_err(|e| format!("current directory: {e}"))?;
    let mut spec: Option<String> = None;
    let mut at: Option<u64> = None;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--at" => {
                i += 1;
                at = Some(
                    rest.get(i)
                        .and_then(|value| value.parse().ok())
                        .ok_or("--at needs an event number (seq)")?,
                );
            }
            "--cwd" => {
                i += 1;
                cwd = PathBuf::from(rest.get(i).ok_or("--cwd needs a path")?);
            }
            other if !other.starts_with('-') && spec.is_none() => {
                spec = Some(other.to_string());
            }
            other => return Err(format!("unexpected {other:?}\n\n{USAGE}")),
        }
        i += 1;
    }

    let parent_path = match &spec {
        Some(spec) => resolve_session(spec, &cwd)?,
        None => newest_session(&cwd).ok_or_else(|| {
            format!(
                "no session to fork here — `loongagent tui --new` starts one, or name one with \
                 `loongagent fork <id>`"
            )
        })?,
    };
    let parent_id = session_id_of(&parent_path);
    let last = loong_eventlog::replay_lenient(&parent_path)
        .map_err(|e| format!("{}: {e}", parent_path.display()))?
        .events
        .last()
        .map(|event| event.seq)
        .unwrap_or(0);
    let at = at.unwrap_or(last);
    if at == 0 {
        return Err(format!(
            "session {parent_id} has no events yet, so there is nothing to continue from"
        ));
    }
    if at > last {
        return Err(format!(
            "session {parent_id} has {last} events; --at {at} is past its end"
        ));
    }

    let dir = parent_path
        .parent()
        .ok_or_else(|| format!("{} has no directory", parent_path.display()))?
        .to_path_buf();
    let id = new_session_id_in_dir(&dir);
    let child = dir.join(format!("{id}.jsonl"));
    let mut log = loong_eventlog::SessionLog::create(&child)
        .map_err(|e| format!("{}: {e}", child.display()))?;
    let forked = gen::SessionForked {
        origin: "fork".to_string(),
        parent_session: parent_id.clone(),
        parent_seq: at,
    };
    log.append(
        gen::event::SESSION_FORKED,
        serde_json::to_value(&forked).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("{}: {e}", child.display()))?;

    println!("forked {parent_id} at {at} -> {id}");
    println!("  the child holds only its own events; it references the parent's prefix");
    println!("  reopen:  loongagent resume {id}");
    Ok(())
}

/// Purpose: A named session plus every descendant, as indices into the listing.
/// Serves: `delete`, and the test that pins the cascade.
/// Design: `docs/design/sessions-lineage.md` §4.
/// Up: `delete` and the session tests; no other caller.
/// Down: no further call; it walks the summaries' parent edges.
/// Verify: `cargo test -p loongagent`.
/// ADR-0015's *branch*: the session someone named, plus every session that descends
/// from it. Pure arithmetic over what the logs say, so the cascade can be reasoned about
/// (and tested) without touching a file.
fn branch_of(sessions: &[SessionSummary], target: &str) -> Vec<usize> {
    let mut include: Vec<usize> = sessions
        .iter()
        .enumerate()
        .filter(|(_, session)| session.id == target)
        .map(|(index, _)| index)
        .collect();
    loop {
        let before = include.len();
        for (index, session) in sessions.iter().enumerate() {
            if include.contains(&index) {
                continue;
            }
            if let Some((parent, _)) = &session.parent {
                if include.iter().any(|kept| &sessions[*kept].id == parent) {
                    include.push(index);
                }
            }
        }
        if include.len() == before {
            // Children of children arrive one level per pass; when a pass adds nothing,
            // the branch is closed.
            break;
        }
    }
    include.sort_unstable();
    include
}

/// Purpose: Processes that hold a session open, found by their `--session` argument.
/// Serves: `delete`, so a delete cannot silently orphan a live writer.
/// Design: `docs/design/sessions-lineage.md` §1.
/// Up: `delete`; no other caller.
/// Down: `/proc/<pid>/cmdline`; no further call.
/// Verify: `cargo test -p loongagent`.
/// Processes that hold this session open, by looking for `--session <path>` in their
/// command line. Deleting a file another process is appending to does not stop that
/// process: it keeps writing to a name nobody can see any more, and the conversation is
/// lost without a word. On Linux — the only platform this runs on — this is cheap to
/// check, so it is checked.
fn writers_of(path: &Path) -> Vec<u32> {
    let mut writers = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return writers;
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if pid == std::process::id() {
            continue;
        }
        let Ok(raw) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let args: Vec<String> = raw
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| String::from_utf8_lossy(arg).to_string())
            .collect();
        let holds = args.iter().enumerate().any(|(index, arg)| {
            (arg == "--session"
                && args
                    .get(index + 1)
                    .map(|next| Path::new(next) == path)
                    .unwrap_or(false))
                // `loongagent server --stdio --session PATH` is the long-running one.
                || Path::new(arg) == path
        });
        if holds {
            writers.push(pid);
        }
    }
    writers
}

/// Purpose: Delete a session and its whole branch, after saying what goes and refusing while it is open.
/// Serves: `loongagent delete ID [--force]`.
/// Design: `docs/design/sessions-lineage.md` §4.
/// Up: `main.rs`'s `delete` arm; no other caller.
/// Down: `resolve_session`, `session_summaries_in`, `session_id_of`, `branch_of`, `writers_of` and `std::fs::remove_file`.
/// Verify: `cargo test -p loongagent`.
/// `loongagent delete ID [--force] [--cwd DIR]`
///
/// The unit is ADR-0015's **branch**: a session and its descendants go together, because
/// a child without its ancestors cannot run — deleting a parent alone would leave
/// sessions that refuse to open.
pub(crate) fn delete(rest: &[String]) -> Result<(), String> {
    let mut cwd = std::env::current_dir().map_err(|e| format!("current directory: {e}"))?;
    let mut spec: Option<String> = None;
    let mut force = false;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--force" | "--yes" | "-f" => force = true,
            "--cwd" => {
                i += 1;
                cwd = PathBuf::from(rest.get(i).ok_or("--cwd needs a path")?);
            }
            other if !other.starts_with('-') && spec.is_none() => spec = Some(other.to_string()),
            other => return Err(format!("unexpected {other:?}\n\n{USAGE}")),
        }
        i += 1;
    }
    let spec = spec.ok_or_else(|| format!("delete needs a session id\n\n{USAGE}"))?;

    let target = resolve_session(&spec, &cwd)?;
    // The branch lives beside the session that was named, wherever that is.
    let dir = target
        .parent()
        .ok_or_else(|| format!("{} has no directory", target.display()))?
        .to_path_buf();
    let sessions = session_summaries_in(&dir, &project_slug(&cwd));
    let target_id = session_id_of(&target);
    let branch = branch_of(&sessions, &target_id);
    if branch.is_empty() {
        return Err(format!(
            "{} is not one of the sessions in {}",
            target_id,
            dir.display()
        ));
    }

    // What it would remove, said before it removes anything.
    let doomed: Vec<&SessionSummary> = branch.iter().map(|index| &sessions[*index]).collect();
    println!(
        "{} session{} in this branch:",
        doomed.len(),
        if doomed.len() == 1 { "" } else { "s" }
    );
    for session in &doomed {
        let from = session
            .parent
            .as_ref()
            .map(|(parent, at)| format!("  from {parent}@{at}"))
            .unwrap_or_default();
        println!(
            "  {}  {}  {} turn(s){from}",
            session.id,
            age_text(session.modified),
            session.turns
        );
    }

    if doomed.len() > 1 && !force {
        return Err(format!(
            "deleting {target_id} also deletes {} descendant session(s), because a child \
             without its parent cannot run (ADR-0015). Pass --force to do that.",
            doomed.len() - 1
        ));
    }

    let mut open: Vec<(String, u32)> = Vec::new();
    for session in &doomed {
        for pid in writers_of(&session.path) {
            open.push((session.id.clone(), pid));
        }
    }
    if !open.is_empty() && !force {
        let listed: Vec<String> = open
            .iter()
            .map(|(id, pid)| format!("{id} (pid {pid})"))
            .collect();
        return Err(format!(
            "still open: {} — that process would keep writing to a deleted file. Close it, or \
             pass --force if you know it does not matter.",
            listed.join(", ")
        ));
    }
    for (id, pid) in &open {
        eprintln!("warning: {id} is open in pid {pid}; it will keep writing to a deleted file");
    }

    let mut removed = 0usize;
    let mut freed = 0u64;
    for session in &doomed {
        for path in [session.path.clone(), session.path.with_extension("sqlite")] {
            if let Ok(meta) = std::fs::metadata(&path) {
                freed += meta.len();
            }
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                        removed += 1;
                        println!(
                            "  removed {}",
                            path.file_name().unwrap_or_default().to_string_lossy()
                        );
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("{}: {e}", path.display())),
            }
        }
    }
    println!(
        "deleted {removed} session(s), {:.1} KiB",
        freed as f64 / 1024.0
    );
    Ok(())
}

/// Purpose: The friendly spelling of `tui --session …`, defaulting to the newest session.
/// Serves: `loongagent resume [ID]`.
/// Design: `docs/design/sessions-lineage.md` §5.
/// Up: `main.rs`'s `resume` arm; no other caller.
/// Down: `Args::parse` and `tui.rs`'s `full_screen`.
/// Verify: `cargo test -p loongagent`.
/// `loongagent resume [ID]` — the friendly spelling of `tui --session …`.
pub(crate) fn resume(rest: &[String]) -> Result<(), String> {
    let mut args = rest.to_vec();
    let positional = args
        .first()
        .filter(|first| !first.starts_with('-'))
        .cloned();
    if positional.is_some() {
        args.remove(0);
    }
    args.splice(
        0..0,
        [
            "--session".to_string(),
            positional.unwrap_or_else(|| "latest".to_string()),
        ],
    );
    full_screen(Args::parse(&args)?)
}

/// Purpose: The file a project's bare surface would open when nothing was named.
/// Serves: `Args::session_path`.
/// Design: `docs/design/sessions-lineage.md` §1.
/// Up: `Args::session_path`; no other caller.
/// Down: `config_dir` and `project_slug`.
/// Verify: `cargo test -p loongagent`.
pub(crate) fn default_session_path(cwd: &Path) -> PathBuf {
    config_dir()
        .join("projects")
        .join(project_slug(cwd))
        .join("session.jsonl")
}

/// Purpose: A readable directory name for a project path, with a hash so two paths cannot share a conversation.
/// Serves: every path in this file.
/// Design: `docs/design/sessions-lineage.md` §1.
/// Up: `session_dir_in`, `session_summaries`, `newest_session_in`, `resolve_session_in` and `delete`.
/// Down: `std::fs::canonicalize` and the slug hash; no further call.
/// Verify: `cargo test -p loongagent`.
/// A readable directory name for a project path: `/home/me/work/api` becomes
/// `home-me-work-api-3f9a1c22`. The hash is there because two different paths can
/// produce the same slug (`/a-b/c` and `/a/b-c`), and quietly sharing a session
/// between two projects would be a nasty surprise.
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
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{}-{:08x}", slug.trim_matches('-'), hash as u32)
}

#[cfg(test)]
mod session_tests {
    use super::*;

    /// Purpose: A root of the test's own, so `config_dir` never reads the developer's environment.
    /// Contract: `session.rs` tests; the session tests call it.
    /// A root of our own: `config_dir()` would read the developer's environment.
    fn temp_root(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("loong-sessions-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Purpose: Write a session log with the given user prompts, enough for the listing and resolution to read.
    /// Serves: the session tests that need a real log.
    /// Design: `docs/design/sessions-lineage.md` §1.
    /// Up: the session tests; no caller in production code.
    /// Down: `std::fs::write` and `serde_json::to_string`.
    /// Verify: `cargo test -p loongagent`.
    /// A session log with `turns` user turns, enough for the listing to read.
    fn write_session(path: &Path, prompts: &[&str]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut text = String::from(
            "{\"kind\":\"skills.loaded\",\"data\":{\"count\":1},\"seq\":1,\"time_ms\":1,\"id\":\"e1\"}\n",
        );
        let mut seq = 2;
        for prompt in prompts {
            text.push_str(&format!(
                "{{\"kind\":\"message.appended\",\"data\":{{\"role\":\"user\",\"text\":{}}},\"seq\":{seq},\"time_ms\":1,\"id\":\"e{seq}\"}}\n",
                serde_json::to_string(prompt).unwrap()
            ));
            seq += 1;
            text.push_str(&format!(
                "{{\"kind\":\"message.appended\",\"data\":{{\"role\":\"assistant\",\"text\":\"ok\"}},\"seq\":{seq},\"time_ms\":1,\"id\":\"e{seq}\"}}\n"
            ));
            seq += 1;
        }
        std::fs::write(path, text).unwrap();
    }

    /// Purpose: The intro names the session, and says `--new` when this run continued one.
    #[test]
    fn the_first_line_says_which_session_this_is_and_how_to_start_another() {
        // The confusion this exists for: `tui` continues the newest session, which is
        // worth saying rather than leaving to be inferred.
        let continued = session_intro(SessionSource::Continued, "36bxg8rd", 17);
        assert!(
            continued.starts_with("continuing session 36bxg8rd (17 events)"),
            "{continued}"
        );
        assert!(
            continued.contains("--new"),
            "and how to start a fresh one: {continued}"
        );

        let created = session_intro(SessionSource::Created, "aatt5pww", 0);
        assert_eq!(created, "new session aatt5pww");

        let named = session_intro(SessionSource::Named, "36bxg8rd", 17);
        assert!(named.starts_with("reopened session 36bxg8rd"), "{named}");
    }

    /// Purpose: Ids are eight readable characters and never repeat or collide with a taken name.
    #[test]
    fn a_new_id_is_short_and_says_itself_out_loud() {
        let root = temp_root("id");
        let cwd = Path::new("/tmp/project");
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..200 {
            let id = new_session_id_in(&root, cwd);
            assert_eq!(id.chars().count(), 8, "{id}");
            assert!(
                id.chars()
                    .all(|c| "23456789abcdefghjkmnpqrstuvwxyz".contains(c)),
                "{id} must avoid the characters people misread"
            );
            assert!(seen.insert(id), "the generator repeated itself");
        }
        // And it will not hand out a name that is already taken.
        let taken = session_path_in(&root, "abcdefgh", cwd);
        std::fs::create_dir_all(taken.parent().unwrap()).unwrap();
        std::fs::write(&taken, "").unwrap();
        for _ in 0..50 {
            assert_ne!(new_session_id_in(&root, cwd), "abcdefgh");
        }
    }

    /// Purpose: The legacy `session.jsonl` is still `default`, and every other id keeps its own file name.
    #[test]
    fn the_first_session_ever_kept_its_name_and_answers_to_default() {
        assert_eq!(session_id_of(Path::new("/x/session.jsonl")), "default");
        assert_eq!(session_id_of(Path::new("/x/k7m2xq3f.jsonl")), "k7m2xq3f");
        assert_eq!(
            session_path_for("default", Path::new("/tmp/p"))
                .file_name()
                .unwrap(),
            "session.jsonl"
        );
        assert_eq!(
            session_path_for("k7m2xq3f", Path::new("/tmp/p"))
                .file_name()
                .unwrap(),
            "k7m2xq3f.jsonl"
        );
    }

    /// Purpose: A listing reads turns, events and the first prompt from the log, ignoring non-logs.
    #[test]
    fn a_session_is_summarised_from_its_own_log() {
        let root = temp_root("summary");
        let cwd = Path::new("/tmp/project");
        let dir = session_dir_in(&root, cwd);
        write_session(&dir.join("aaaa1111.jsonl"), &["first question", "second"]);
        // The derived index and anything else in the directory are not sessions.
        std::fs::write(dir.join("aaaa1111.sqlite"), "not a log").unwrap();
        std::fs::write(dir.join("notes.txt"), "not a log").unwrap();

        let sessions = session_summaries_in(&dir, "project");
        assert_eq!(sessions.len(), 1);
        let one = &sessions[0];
        assert_eq!(one.id, "aaaa1111");
        assert_eq!(one.turns, 2);
        assert_eq!(one.events, 5, "one skills.loaded plus two exchanges");
        assert_eq!(one.first_prompt.as_deref(), Some("first question"));
    }

    /// Purpose: An exact id, an unambiguous prefix, `latest` and a bare path each resolve to the right file.
    #[test]
    fn resolve_takes_an_exact_id_a_prefix_and_latest() {
        let root = temp_root("resolve");
        let cwd = Path::new("/tmp/project");
        let dir = session_dir_in(&root, cwd);
        write_session(&dir.join("aaaa1111.jsonl"), &["older"]);
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_session(&dir.join("bbbb2222.jsonl"), &["newer"]);

        assert_eq!(
            resolve_session_in(&root, "aaaa1111", cwd)
                .unwrap()
                .file_name()
                .unwrap(),
            "aaaa1111.jsonl"
        );
        assert_eq!(
            resolve_session_in(&root, "bbbb", cwd)
                .unwrap()
                .file_name()
                .unwrap(),
            "bbbb2222.jsonl",
            "an unambiguous prefix is enough"
        );
        assert_eq!(
            resolve_session_in(&root, "latest", cwd)
                .unwrap()
                .file_name()
                .unwrap(),
            "bbbb2222.jsonl",
            "the newest by modification time"
        );
        assert_eq!(
            resolve_session_in(&root, "./elsewhere.jsonl", cwd).unwrap(),
            PathBuf::from("./elsewhere.jsonl"),
            "a path is a path, even when it does not exist yet"
        );
        assert_eq!(
            resolve_session_in(&root, "AAAA", cwd)
                .unwrap()
                .file_name()
                .unwrap(),
            "aaaa1111.jsonl",
            "ids are matched case-insensitively"
        );
    }

    /// Purpose: An ambiguous prefix names both candidates instead of choosing one.
    #[test]
    fn a_prefix_that_fits_two_sessions_says_which() {
        let root = temp_root("ambiguous");
        let cwd = Path::new("/tmp/project");
        let dir = session_dir_in(&root, cwd);
        write_session(&dir.join("k7m2xq3f.jsonl"), &["one"]);
        write_session(&dir.join("k7m2yyyy.jsonl"), &["two"]);
        let error = resolve_session_in(&root, "k7m2", cwd).unwrap_err();
        assert!(error.contains("matches 2"), "{error}");
        assert!(
            error.contains("k7m2xq3f") && error.contains("k7m2yyyy"),
            "{error}"
        );
    }

    /// Purpose: An unknown id points at `loongagent sessions`, whether the project has sessions or none.
    #[test]
    fn an_unknown_id_names_the_way_to_find_one() {
        let root = temp_root("unknown");
        let cwd = Path::new("/tmp/project");
        write_session(
            &session_dir_in(&root, cwd).join("aaaa1111.jsonl"),
            &["only"],
        );
        let error = resolve_session_in(&root, "zzzz", cwd).unwrap_err();
        assert!(error.contains("loongagent sessions"), "{error}");

        // Nothing at all, not even in another project.
        let empty = temp_root("unknown-empty");
        let error = resolve_session_in(&empty, "zzzz", cwd).unwrap_err();
        assert!(error.contains("loongagent sessions"), "{error}");
    }

    /// Purpose: A session listed by `sessions --all` can be resumed from another project.
    #[test]
    fn a_session_from_another_project_can_still_be_reopened() {
        let root = temp_root("elsewhere");
        let here = Path::new("/tmp/here");
        let there = Path::new("/tmp/there");
        write_session(
            &session_dir_in(&root, there).join("cccc3333.jsonl"),
            &["from there"],
        );
        assert_eq!(
            session_summaries_in(&session_dir_in(&root, here), "here").len(),
            0
        );
        assert_eq!(
            resolve_session_in(&root, "cccc3333", here)
                .unwrap()
                .file_name()
                .unwrap(),
            "cccc3333.jsonl",
            "`sessions --all` shows it, so `resume` must be able to open it"
        );
        assert_eq!(all_session_summaries_in(&root).len(), 1);
    }

    /// Purpose: A legacy `default` present in two other projects is refused, and one present once resolves.
    #[test]
    fn an_id_that_exists_in_two_other_projects_is_not_guessed() {
        // Every legacy session is called `default`, so this is the common case, not a
        // corner: silently opening one of them would open the wrong conversation.
        let root = temp_root("ambiguous-elsewhere");
        let here = Path::new("/tmp/here");
        for (there, prompt) in [("/tmp/one", "first"), ("/tmp/two", "second")] {
            write_session(
                &session_dir_in(&root, Path::new(there)).join("session.jsonl"),
                &[prompt],
            );
        }
        let error = resolve_session_in(&root, "default", here).unwrap_err();
        assert!(error.contains("other projects"), "{error}");
        assert!(
            error.contains("full path"),
            "and says what to do instead: {error}"
        );

        // With only one of them, it resolves — the useful half of the same rule.
        std::fs::remove_file(session_dir_in(&root, Path::new("/tmp/two")).join("session.jsonl"))
            .unwrap();
        assert_eq!(
            resolve_session_in(&root, "default", here)
                .unwrap()
                .file_name()
                .unwrap(),
            "session.jsonl"
        );
    }

    /// Purpose: A summary fixture with a fixed path and an optional parent edge.
    /// Contract: `session.rs` tests; the branch and cycle tests call it.
    fn summary(id: &str, parent: Option<(&str, u64)>) -> SessionSummary {
        SessionSummary {
            id: id.to_string(),
            path: PathBuf::from(format!("/tmp/{id}.jsonl")),
            project: "project".to_string(),
            modified: SystemTime::now(),
            events: 1,
            turns: 0,
            first_prompt: None,
            parent: parent.map(|(id, at)| (id.to_string(), at)),
        }
    }

    /// Purpose: Deleting a parent takes every descendant, a child takes its own subtree, and unrelated roots stay.
    #[test]
    fn deleting_a_session_takes_its_whole_branch() {
        let sessions = vec![
            summary("a", None),
            summary("b", Some(("a", 3))),
            summary("b2", Some(("a", 3))),
            summary("c", Some(("b", 9))),
            summary("d", None),
        ];
        let ids = |target: &str| -> Vec<String> {
            branch_of(&sessions, target)
                .iter()
                .map(|index| sessions[*index].id.clone())
                .collect()
        };
        assert_eq!(
            ids("a"),
            vec!["a", "b", "b2", "c"],
            "the parent and every descendant"
        );
        assert_eq!(ids("b"), vec!["b", "c"], "a child takes its own subtree");
        assert_eq!(ids("c"), vec!["c"], "a leaf takes only itself");
        assert_eq!(ids("d"), vec!["d"], "an unrelated root is not touched");
    }

    /// Purpose: A hand-edited cycle in the lineage still terminates the branch walk.
    #[test]
    fn a_cycle_in_the_lineage_does_not_spin_forever() {
        // Not reachable through `fork`, but the cascade must not be the thing that hangs
        // when a log has been hand-edited.
        let sessions = vec![summary("x", Some(("y", 1))), summary("y", Some(("x", 1)))];
        assert_eq!(branch_of(&sessions, "x").len(), 2);
    }

    /// Purpose: A forked child's only event is the lineage record, and the listing reads it without counting the parent's events.
    #[test]
    fn a_child_session_says_where_it_came_from() {
        let root = temp_root("forked-listing");
        let cwd = Path::new("/tmp/project");
        let dir = session_dir_in(&root, cwd);
        std::fs::create_dir_all(&dir).unwrap();
        // Exactly what `loongagent fork` writes: one lineage event, nothing copied.
        let mut child = loong_eventlog::SessionLog::create(dir.join("child123.jsonl")).unwrap();
        child
            .append(
                gen::event::SESSION_FORKED,
                serde_json::json!({
                    "origin": "fork",
                    "parent_session": "parent99",
                    "parent_seq": 7,
                }),
            )
            .unwrap();
        write_session(&dir.join("parent99.jsonl"), &["the parent's prompt"]);

        let sessions = session_summaries_in(&dir, "project");
        let listed = sessions.iter().find(|s| s.id == "child123").unwrap();
        assert_eq!(listed.parent, Some(("parent99".to_string(), 7)));
        assert_eq!(
            listed.events, 1,
            "the listing counts the child's own events only"
        );
        let parent = sessions.iter().find(|s| s.id == "parent99").unwrap();
        assert_eq!(parent.parent, None, "a root session came from nowhere");
    }

    /// Purpose: Clipping a long or multi-byte prompt keeps it within the column budget.
    #[test]
    fn a_long_prompt_is_clipped_without_breaking_a_character() {
        assert_eq!(clip("short", 10), "short");
        assert_eq!(clip("abcdefghij", 5), "abcd…");
        let clipped = clip("中文中文中文", 4);
        assert_eq!(clipped.chars().count(), 4);
        assert!(clipped.ends_with('…'));
    }

    /// Purpose: Ages print as just now, minutes, hours or days.
    #[test]
    fn ages_are_said_the_way_a_person_says_them() {
        let now = SystemTime::now();
        assert_eq!(age_text(now), "just now");
        assert_eq!(
            age_text(now - std::time::Duration::from_secs(120)),
            "2 min ago"
        );
        assert_eq!(
            age_text(now - std::time::Duration::from_secs(7200)),
            "2 h ago"
        );
        assert_eq!(
            age_text(now - std::time::Duration::from_secs(172800)),
            "2 d ago"
        );
    }
}
