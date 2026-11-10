//! Purpose: `loongagent reverse`: a study session over the reverse-engineering tools.
//!
//! # Contract
//! - **Serves:** the `reverse` arm of `main.rs`: claim a run directory under `.reverse/`, write
//!   and self-trust its manifest, run the browser session, and clean the manifest pair up on exit.
//! - **Design:** `docs/design/reverse.md` §3 and §4.
//! - **Upstream:** `main.rs`'s `reverse` arm parses the flags and calls `run`.
//! - **Downstream:** `loong_mcp::McpSet` for the name-collision check and `trust_reverse_now`,
//!   `crate::session::{new_session_id, session_file}` for the project's conversation,
//!   `loong_eventlog::SessionLog` as the one-writer probe, `crate::web::make_session` and
//!   `loong_web::{bind, serve_with}`, and `loong_reverse_ui::StudySurface` for the study tab
//!   mounted in the portal.
//! - **Verify:** `cargo test -p loongagent reverse_` · `./scripts/check.sh`

use std::path::{Path, PathBuf};

use loong_mcp::{ManifestOrigin, McpSet, MCP_FILE, MCP_TRUST_FILE, REVERSE_DIR};

/// Purpose: The server's own name in the session manifest: it namespaces every reverse tool.
/// Contract: `crates/cli/src/reverse.rs`.
const REVERSE_SERVER: &str = "loong-reverse";

/// Purpose: The directory holding the study notes, one subdirectory per project.
/// Contract: `crates/cli/src/reverse.rs`; design `docs/design/reverse.md` §3.
const STUDY_DIR: &str = "study";

/// Purpose: The directory holding the rebuilt project, one subdirectory per project.
/// Contract: `crates/cli/src/reverse.rs`; design `docs/design/reverse.md` §3.
const BUILD_DIR: &str = "build";

/// Purpose: The directory holding each run's own bookkeeping: session id and manifest pair.
/// Contract: `crates/cli/src/reverse.rs`; design `docs/design/reverse.md` §3.
const RUNS_DIR: &str = "runs";

/// Purpose: The prefix of a run's name; the number follows, zero-padded to three digits.
/// Contract: `crates/cli/src/reverse.rs`.
const RUN_PREFIX: &str = "run-";

/// Purpose: The file recording which session a run's notes come from, and which conversation
/// the project opens next — one name, defined by the surface that also writes it.
/// Contract: `crates/cli/src/reverse.rs`; design `docs/design/reverse.md` §3.
const SESSION_FILE: &str = loong_reverse_ui::SESSION_FILE;

/// Purpose: What the target turned out to be.
/// Serves: `run`, which tells the operator what was opened.
/// Design: `docs/design/reverse.md` §4.1.
/// Up: `run` only.
/// Down: nothing; a display enum.
/// Verify: `cargo test -p loongagent reverse_`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TargetKind {
    /// An existing regular file on disk.
    File,
    /// An existing folder: the engine opens app bundles and folders too.
    Folder,
}

/// Purpose: One reverse run's directories: the name it claimed, the project it studies and the
/// places it writes.
/// Serves: `run`, `cleanup`, `run_portal` and the tests that pin the layout.
/// Design: `docs/design/reverse.md` §3.
/// Up: `scaffold` builds it; `run` carries it to the surface and to the exit path.
/// Down: no further call; it is data.
/// Verify: `cargo test -p loongagent reverse_scaffold`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunDirs {
    /// The claimed name, `run-001`, `run-002`, …: this process's identifier.
    pub name: String,
    /// `.reverse/` itself: the root the study tab lists.
    pub root: PathBuf,
    /// The project this run studies: the name of the three directories below.
    pub project: String,
    /// `.reverse/study/<project>/`: what the model writes its notes into.
    pub study: PathBuf,
    /// `.reverse/build/<project>/`: where the rebuilt project lands.
    pub build: PathBuf,
    /// `.reverse/projects/<project>/`: target, kind and the study-finished mark, which belong
    /// to the project and outlive this process.
    pub project_state: PathBuf,
    /// `.reverse/runs/<run>/`: session id and the manifest pair, which belong to this process.
    pub state: PathBuf,
}

/// Purpose: Check the target before anything binds: it must exist and be a file or folder.
///
/// A URL is refused, not silently accepted: the engine opens local paths only
/// (`core_target_open` demands an absolute path and `Target::load` stats it), so a URL
/// would become a failed model turn instead of a study (reverse.md §4.1).
/// Serves: `run`'s fail-fast step when a target is passed on the command line.
/// Design: `docs/design/reverse.md` §4.1.
/// Up: `run` only.
/// Down: `std::fs::metadata`.
/// Verify: `cargo test -p loongagent reverse_`.
pub(crate) fn validate_target(target: &str) -> Result<TargetKind, String> {
    if target.starts_with("http://") || target.starts_with("https://") {
        return Err(format!(
            "{target}: a URL is not a target yet — the engine opens local files and folders \
             only; download the site or point at a file instead (reverse.md §4.1)"
        ));
    }
    let path = Path::new(target);
    if !path.is_absolute() {
        return Err(format!(
            "{target}: pass an absolute path (the engine opens absolute paths)"
        ));
    }
    let meta =
        std::fs::metadata(path).map_err(|e| format!("{target}: cannot open the target ({e})"))?;
    if meta.is_dir() {
        return Ok(TargetKind::Folder);
    }
    if !meta.is_file() {
        return Err(format!("{target}: neither a file nor a folder"));
    }
    Ok(TargetKind::File)
}

/// Purpose: Claim the next free run name by creating its directory, so two processes never share one.
/// Serves: `scaffold` only.
/// Design: `docs/design/reverse.md` §3.
/// Up: `scaffold`.
/// Down: `std::fs::create_dir`, which is the atomic claim: `AlreadyExists` means someone else has it.
/// Verify: `cargo test -p loongagent reverse_scaffold`.
fn claim_run(runs: &Path) -> Result<String, String> {
    std::fs::create_dir_all(runs).map_err(|e| format!("{}: {e}", runs.display()))?;
    for number in 1..=9999u32 {
        let name = format!("{RUN_PREFIX}{number:03}");
        match std::fs::create_dir(runs.join(&name)) {
            Ok(()) => return Ok(name),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("{}: {e}", runs.join(&name).display())),
        }
    }
    Err(format!(
        "{}: no free run number left — move the old runs aside",
        runs.display()
    ))
}

/// Purpose: The conversation this project talks in: the log a `--session`/`--new` named, else
/// the log it recorded, else a fresh one it records now — so a restart reopens the history the
/// project already has.
///
/// One project, one conversation to open: the id lives in `projects/<name>/session`
/// (`docs/design/reverse.md` §5), beside the target and the phase, so every later start —
/// and every later run — replays that log. It is the one to open, not the only one that
/// exists: every id this binding lands on also goes on `projects/<name>/sessions`, the list
/// the page's picker offers, so a project can hold the several conversations a study grows
/// into (`docs/design/reverse-conversation.md`). Naming a session on the command line is
/// another way that choice changes: the named log becomes the project's, and the next start
/// continues it.
/// A log another process already holds is the one case that cannot be shared: this start then
/// opens a conversation of its own and says so, instead of failing to open the page
/// (`docs/design/reverse.md` §11).
/// Serves: `run`, before the scaffold names the run's session.
/// Design: `docs/design/reverse.md` §5.
/// Up: `run` only.
/// Down: `bind_session_in`, `crate::config_dir`.
/// Verify: `cargo test -p loongagent reverse_session`.
fn bind_session(
    root: &Path,
    project: &str,
    cwd: &Path,
    named: Option<&Path>,
) -> Result<PathBuf, String> {
    bind_session_in(&crate::config_dir(), root, project, cwd, named)
}

/// Purpose: The same binding under an injected client root, which is what the test drives.
/// Serves: `bind_session`; the reverse session tests.
/// Design: `docs/design/reverse.md` §5; `docs/design/reverse-conversation.md`
/// 「A project's sessions」.
/// Up: `bind_session` and `cargo test -p loongagent reverse_session`.
/// Down: `crate::session::{session_id_of, reverse_drawer_name, drawer_dir_in,
/// new_session_id_in_drawer, session_file_in_drawer}`, `loong_reverse_ui::project_state`,
/// `record_session`/`fresh_log`, and `loong_eventlog::SessionLog::open` as the one-writer probe.
/// Verify: `cargo test -p loongagent reverse_session`.
fn bind_session_in(
    home: &Path,
    root: &Path,
    project: &str,
    cwd: &Path,
    named: Option<&Path>,
) -> Result<PathBuf, String> {
    if let Some(named) = named {
        // The person said which conversation to use; the project follows it from now on.
        record_session(root, project, named)?;
        return Ok(named.to_path_buf());
    }
    // Lookups and new logs happen in this project's own drawer, never the workspace's: the
    // picker shows what `session.list` answers, and that answer is the drawer.
    let drawer =
        crate::session::drawer_dir_in(home, &crate::session::reverse_drawer_name(cwd, project));
    let record = loong_reverse_ui::project_state(root, project).join(SESSION_FILE);
    if let Ok(text) = std::fs::read_to_string(&record) {
        let id = text.trim();
        let log = crate::session::session_file_in_drawer(&drawer, id);
        if !id.is_empty() && log.is_file() {
            // Opening the log is the probe: it takes the one-writer lock (events-storage.md
            // §3), so a log this process can open is a log nobody else is writing.
            if loong_eventlog::SessionLog::open(&log).is_ok() {
                return Ok(log);
            }
            // The other process owns the conversation *and* the record: stealing it would
            // hide a history that is still being written. This run gets a log of its own.
            eprintln!(
                "[reverse] project {project}'s session is being written by another process — \
                 this run talks in a session of its own (history will not join up)."
            );
            return fresh_log(&drawer);
        }
    }
    let log = fresh_log(&drawer)?;
    record_session(root, project, &log)?;
    Ok(log)
}

/// Purpose: Write `projects/<project>/session`, the one line the next start reads.
/// Serves: `bind_session_in`.
/// Design: `docs/design/reverse.md` §5.
/// Up: `bind_session_in` only.
/// Down: `crate::session::session_id_of` and `loong_reverse_ui::project_state`.
/// Verify: `cargo test -p loongagent reverse_session`.
fn record_session(root: &Path, project: &str, log: &Path) -> Result<(), String> {
    let state = loong_reverse_ui::project_state(root, project);
    let record = state.join(SESSION_FILE);
    if let Some(parent) = record.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let id = crate::session::session_id_of(log);
    std::fs::write(&record, format!("{id}\n")).map_err(|e| format!("{}: {e}", record.display()))
}

/// Purpose: Move the conversations this project recorded *before* it had a drawer of its own
/// out of the workspace's session directory and into that drawer.
///
/// Until drawers existed a reverse project bound its logs in the workspace directory, which is
/// also the TUI's. Those ids are still on record in `projects/<project>/sessions`; moving the
/// files keeps yesterday's conversation in the picker instead of making it look deleted. The
/// move is a rename, so a process still writing one keeps writing it — it just stops finding it
/// where it was. Done once per start and idempotent: a file already in the drawer is left alone.
/// Serves: `run`.
/// Design: `docs/design/reverse-conversation.md` 「A project's sessions」;
/// `docs/design/sessions-lineage.md` §1.
/// Up: `run`, before the session is bound.
/// Down: `migrate_sessions_in`.
/// Verify: `cargo test -p loongagent reverse_drawer`.
fn migrate_sessions(root: &Path, project: &str, cwd: &Path, drawer: &str) -> Result<usize, String> {
    migrate_sessions_in(&crate::config_dir(), root, project, cwd, drawer)
}

/// Purpose: The same migration under an injected client root, which is what the test drives.
/// Serves: `migrate_sessions`.
/// Design: `docs/design/reverse-conversation.md` 「A project's sessions」.
/// Up: `migrate_sessions` and the reverse session tests.
/// Down: `crate::session::{drawer_dir_in, session_file_in, session_file_in_drawer}` and
/// `loong_reverse_ui::{project_state, valid_session_id, LEGACY_SESSIONS_FILE}`.
/// Verify: `cargo test -p loongagent reverse_drawer`.
fn migrate_sessions_in(
    home: &Path,
    root: &Path,
    project: &str,
    cwd: &Path,
    drawer: &str,
) -> Result<usize, String> {
    let state = loong_reverse_ui::project_state(root, project);
    let Ok(list) = std::fs::read_to_string(state.join(loong_reverse_ui::LEGACY_SESSIONS_FILE))
    else {
        return Ok(0);
    };
    let dest = crate::session::drawer_dir_in(home, drawer);
    let mut moved = 0;
    for line in list.lines() {
        let id = line.trim();
        if !loong_reverse_ui::valid_session_id(id) {
            continue;
        }
        let from = crate::session::session_file_in(home, cwd, id);
        let to = crate::session::session_file_in_drawer(&dest, id);
        if from == to || !from.is_file() || to.exists() {
            continue;
        }
        std::fs::create_dir_all(&dest).map_err(|e| format!("{}: {e}", dest.display()))?;
        std::fs::rename(&from, &to)
            .map_err(|e| format!("{} → {}: {e}", from.display(), to.display()))?;
        moved += 1;
    }
    if moved > 0 {
        eprintln!(
            "[reverse] moved {moved} earlier conversation(s) of {project} into {} — from here \
             on this project's conversations stay out of the workspace's own list",
            dest.display()
        );
    }
    Ok(moved)
}

/// Purpose: A brand-new conversation log this start may write, inside a drawer, with its
/// directory made.
/// Serves: `bind_session_in`.
/// Design: `docs/design/reverse.md` §5.
/// Up: `bind_session_in` only.
/// Down: `crate::session::{new_session_id_in_drawer, session_file_in_drawer}`.
/// Verify: `cargo test -p loongagent reverse_session`.
fn fresh_log(drawer: &Path) -> Result<PathBuf, String> {
    let id = crate::session::new_session_id_in_drawer(drawer);
    let log = crate::session::session_file_in_drawer(drawer, &id);
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    Ok(log)
}

/// Purpose: Which project this start works on: the recorded one, else the newest that exists,/// else a fresh `project-001`.
///
/// Recorded first, so a restart lands on the project the user left (`docs/design/reverse.md`
/// §3). The newest is the fallback for a `.reverse/` written before `project` existed, and
/// `project-001` for one that is empty. Creating it here rather than in the page means the
/// target bar, the tree and the prompts always have a project to name.
/// Serves: `scaffold` only.
/// Design: `docs/design/reverse.md` §3.
/// Up: `scaffold`.
/// Down: `loong_reverse_ui::{current_project, list_projects, project_state, create_project}`.
/// Verify: `cargo test -p loongagent reverse_scaffold`.
fn pick_project(root: &Path) -> Result<String, String> {
    if let Some(name) = loong_reverse_ui::current_project(root) {
        return Ok(name);
    }
    let mut newest: Option<(std::time::SystemTime, String)> = None;
    for name in loong_reverse_ui::list_projects(root) {
        let changed = std::fs::metadata(loong_reverse_ui::project_state(root, &name))
            .and_then(|meta| meta.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        if newest
            .as_ref()
            .map(|(newest, _)| changed > *newest)
            .unwrap_or(true)
        {
            newest = Some((changed, name));
        }
    }
    if let Some((_, name)) = newest {
        loong_reverse_ui::select_project(root, &name)?;
        return Ok(name);
    }
    let name = loong_reverse_ui::DEFAULT_PROJECT.to_string();
    loong_reverse_ui::create_project(root, &name)?;
    Ok(name)
}

/// Purpose: Build `.reverse/`, claim a run, and write that run's manifest and trust record.
/// Serves: `run`'s startup, and the tests that pin the scaffold's shape.
/// Design: `docs/design/reverse.md` §3.
/// Up: `run` only.
/// Down: `claim_run`, `McpSet::load` for the collision check and `trust_reverse_now` for the record.
/// Verify: `cargo test -p loongagent reverse_scaffold`.
pub(crate) fn scaffold(cwd: &Path, session: &str) -> Result<RunDirs, String> {
    let root = cwd.join(REVERSE_DIR);
    std::fs::create_dir_all(root.join(STUDY_DIR))
        .map_err(|e| format!("{}: {e}", root.display()))?;
    std::fs::create_dir_all(root.join(BUILD_DIR))
        .map_err(|e| format!("{}: {e}", root.display()))?;
    std::fs::write(root.join(".gitignore"), "*\n")
        .map_err(|e| format!("{}: {e}", root.display()))?;

    // The old layout wrote the pair straight into `.reverse/`; a project that ran that
    // build still has it there, where a *plain* session would no longer look but a
    // `kill -9` from that build may have left it trusted. Removing it while we are
    // here keeps the promise "no reverse tools in a normal session" true for old
    // directories too.
    let _ = std::fs::remove_file(root.join(MCP_FILE));
    let _ = std::fs::remove_file(root.join(MCP_TRUST_FILE));

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

    let project = pick_project(&root)?;
    let project_state = loong_reverse_ui::project_state(&root, &project);
    let name = claim_run(&root.join(RUNS_DIR))?;
    let state = root.join(RUNS_DIR).join(&name);
    let study = root.join(STUDY_DIR).join(&project);
    let evidence = study.join(loong_reverse_ui::EVIDENCE_DIR);
    let build = root.join(BUILD_DIR).join(&project);
    for dir in [&study, &evidence, &build, &project_state] {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    // Which conversation wrote these notes: the session log lives in `~/.loongagent/`
    // (sessions-lineage.md §5), so the run records the id rather than copying a log.
    std::fs::write(state.join(SESSION_FILE), format!("{session}\n"))
        .map_err(|e| format!("{}: {e}", state.join(SESSION_FILE).display()))?;

    // Older layouts put the target at `.reverse/target` (before runs) and then inside each
    // `runs/<run>/`. Adopt the newest one into this project so an upgrade empties nothing;
    // moving it also leaves nothing behind that another project would inherit.
    if !project_state.join("target").is_file() {
        for legacy in legacy_state_dirs(&root) {
            if !legacy.join("target").is_file() {
                continue;
            }
            for file in ["target", "target-kind"] {
                let _ = std::fs::rename(legacy.join(file), project_state.join(file));
            }
            break;
        }
    }

    // `--allow-run` moves the consent question to this session's approval card. The
    // server refuses every tool that runs code or changes a device with
    // `consent_required` unless it was started this way, and here it has no inbox of
    // its own to ask through — so without the flag the person's "allow once" only
    // releases the host's policy, and the call is refused anyway (reverse.md §3).
    let manifest = format!(
        "# Written and self-trusted by `loongagent reverse`; do not edit by hand —\n\
         # the session rewrites it on every start and deletes it on exit.\n\
         [[server]]\n\
         name = \"{REVERSE_SERVER}\"\n\
         command = \"loong-reverse-mcp\"\n\
         args = [\"--allow-run\"]\n"
    );
    let manifest_path = state.join(MCP_FILE);
    std::fs::write(&manifest_path, manifest)
        .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    McpSet::load_from(cwd, Some(&state))
        .map_err(|e| e.to_string())?
        .trust_reverse_now()
        .map_err(|e| e.to_string())?;

    Ok(RunDirs {
        name,
        root,
        project,
        study,
        build,
        project_state,
        state,
    })
}

/// Purpose: Where a target may have been recorded by an older build, newest first: `.reverse/`
/// itself, then each `runs/<run>/`.
/// Serves: `scaffold`'s one-time adoption of a pre-project target.
/// Design: `docs/design/reverse.md` §3.
/// Up: `scaffold` only.
/// Down: `std::fs::read_dir` on `runs/`.
/// Verify: `cargo test -p loongagent reverse_scaffold`.
fn legacy_state_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![root.to_path_buf()];
    if let Ok(read) = std::fs::read_dir(root.join(RUNS_DIR)) {
        let mut runs: Vec<PathBuf> = read.flatten().map(|entry| entry.path()).collect();
        runs.sort();
        runs.reverse();
        dirs.extend(runs);
    }
    dirs
}

/// Purpose: Remove one run's manifest pair on exit; its notes, target, phase and session id stay.
/// Serves: `cleanup` and the SIGINT handler.
/// Design: `docs/design/reverse.md` §3.
/// Up: `cleanup` and `ctrlc_handler` only.
/// Down: `std::fs::remove_file`.
/// Verify: `cargo test -p loongagent reverse_cleanup`.
fn cleanup_state(state: &Path) {
    let _ = std::fs::remove_file(state.join(MCP_FILE));
    let _ = std::fs::remove_file(state.join(MCP_TRUST_FILE));
}

/// Purpose: Remove the run's manifest pair on exit; the study notes and any database stay behind.
/// Serves: `run`'s exit path (normal and Ctrl+C).
/// Design: `docs/design/reverse.md` §3.
/// Up: `run` only.
/// Down: `cleanup_state`.
/// Verify: `cargo test -p loongagent reverse_cleanup`.
pub(crate) fn cleanup(run: &RunDirs) {
    cleanup_state(&run.state);
}

/// Purpose: Run the study session: scaffold, serve the portal with the study tab,
/// then clean the manifest pair up (on any exit, including Ctrl+C).
///
/// The target is chosen in the page (reverse.md §4.1), so the command takes none:
/// `loongagent reverse` is enough. A target passed anyway is checked here and recorded
/// as the prefill, which is the whole of its effect.
/// Serves: the `reverse` arm of `main.rs`.
/// Design: `docs/design/reverse.md` §4.1.
/// Up: `main.rs`'s `reverse` arm only.
/// Down: `validate_target`, `pick_project`, `bind_session`, `scaffold`, `cleanup`,
/// `run_portal`, `crate::web::make_session`, and `Args::{session_was_named, session_path,
/// session_id}` for the conversation this project keeps (reverse.md §5).
/// Verify: `cargo test -p loongagent reverse_` covers the parts a unit test can reach.
pub(crate) fn run(target: Option<&str>, args: &mut crate::Args) -> Result<(), String> {
    // A target given on the command line is checked before anything binds or
    // scaffolds: a typo in the path must not leave a half-made `.reverse/` behind.
    let kind = match target {
        Some(target) => Some(validate_target(target)?),
        None => None,
    };
    // The project owns the conversation: bind it before the scaffold, so the run records
    // the same id and the next start reopens the same log (reverse.md §5). `--session` and
    // `--new` are the person naming it, and naming it is what changes it.
    let root = args.cwd.join(REVERSE_DIR);
    let project = pick_project(&root)?;
    // A reverse *project* is its own store of conversations, not the workspace: the
    // workspace's session directory is shared with the TUI and every other surface, so a
    // picker reading it shows dozens of conversations this study never had
    // (sessions-lineage.md §1). The drawer is named from the project, and the server child
    // this run spawns works in it — new, open and list all.
    let drawer = crate::session::reverse_drawer_name(&args.cwd, &project);
    args.session_drawer = Some(drawer.clone());
    migrate_sessions(&root, &project, &args.cwd, &drawer)?;
    // `session_was_named` is the difference between "the person chose this log" and "the
    // parser picked the newest one for me": only the first is a choice to record.
    let named = match args.session_was_named() {
        true => Some(args.session_path()),
        false => None,
    };
    args.session = Some(bind_session(&root, &project, &args.cwd, named.as_deref())?);
    // Whether this start reopens a log it found or mints one it will write: the page's first
    // paint depends on it, and so does the line below (`sessions-lineage.md` §5).
    let reopened = args.session_path().is_file();
    args.session_source = if reopened {
        crate::session::SessionSource::Continued
    } else {
        crate::session::SessionSource::Created
    };
    let run = scaffold(&args.cwd, &args.session_id())?;
    if let Some(target) = target {
        // Prefill only: the page reads it, the model still opens the target itself.
        std::fs::write(run.project_state.join("target"), format!("{target}\n"))
            .map_err(|e| format!("recording the target: {e}"))?;
    }
    // This session's own manifest pair, for the server child it is about to start.
    // Two reverse processes in one project therefore read different files.
    args.reverse_dir = Some(run.state.clone());

    // Ctrl+C still cleans up: the handler removes the manifest pair before the
    // process dies, so a killed session never leaves a live pair behind.
    install_ctrlc_cleanup(run.state.clone())?;

    match (target, kind) {
        (Some(target), Some(kind)) => eprintln!(
            "[reverse] target prefill: {target} ({})",
            match kind {
                TargetKind::File => "file",
                TargetKind::Folder => "folder",
            }
        ),
        _ => eprintln!("[reverse] no target yet — pick one at the top of the study tab."),
    }
    // A fresh session is either recorded (the next start reopens it) or — when another
    // process still holds the recorded log — this run's own, with the record left alone
    // (reverse-conversation.md 「A project's sessions」). Claiming "remembered" in both
    // cases is a lie the person acts on: they close the page and lose the conversation.
    let recorded_next = std::fs::read_to_string(run.project_state.join(SESSION_FILE))
        .map(|text| text.trim() == args.session_id())
        .unwrap_or(false);
    eprintln!(
        "[reverse] session {} — {}",
        args.session_id(),
        match (reopened, recorded_next) {
            (true, _) => "reopening it; the page replays its history",
            (false, true) => "new; it is remembered in projects/<project>/session for next time",
            (false, false) =>
                "new, but NOT remembered: the project still points at the session \
                               another process is writing, so the next start will not reopen it",
        }
    );
    eprintln!(
        "[reverse] project {} · run {} — notes in {}",
        run.project,
        run.name,
        run.study.display()
    );
    eprintln!("[reverse] rebuild output lands in {}", run.build.display());
    eprintln!("[reverse] this run's manifest pair is removed when the session ends; notes stay.");

    // The session rides the same portal as `loongagent web`, plus the reverse study
    // routes (`/reverse`): one listener, one token, the chat socket is the portal's.
    let result = run_portal(args, &run);
    cleanup(&run);
    result
}

/// Purpose: Serve the portal with the reverse study surface mounted until the process ends.
/// Serves: `run` only.
/// Design: `docs/design/reverse.md` §4.
/// Up: `run`.
/// Down: `crate::web::make_session`, `loong_web::{bind, random_token, serve_with}`,
/// `loong_reverse_ui::StudySurface`, `loong_tui::interrupt` for the chat's stop, and
/// `crate::browser::open_in_browser` for the printed URL.
/// Verify: `cargo test -p loongagent reverse_` covers scaffold/cleanup; the study surface's
/// routes are covered by `cargo test -p loong-reverse-ui`.
fn run_portal(args: &crate::Args, run: &RunDirs) -> Result<(), String> {
    let portal = crate::web::make_session(args)?;
    let (listener, addr) = loong_web::bind(&args.bind, args.port).map_err(|e| e.to_string())?;
    let token = loong_web::random_token().map_err(|e| e.to_string())?;
    // Stopping a turn is a SIGINT to the server child — the same signal Ctrl+C in the TUI
    // raises (`surfaces.md` §7). It carries no request, so a busy child still hears it:
    // the chat socket's own line would have to wait behind the turn it wants to stop.
    let server = portal.server_pid;
    let interrupt: std::sync::Arc<dyn Fn() + Send + Sync> =
        std::sync::Arc::new(move || loong_tui::interrupt(server));
    // The tree lists all of `.reverse/` (older runs stay reachable); the target and the
    // phase are this run's own, so two processes never read each other's.
    let surface =
        loong_reverse_ui::StudySurface::new(run.root.clone(), run.state.clone(), interrupt);

    let url = format!("http://{addr}/reverse#token={token}");
    println!("loongagent reverse — open this URL:");
    println!();
    println!("  {url}");
    println!();
    println!("The study tab sits beside the portal's chat tab; the token rides the URL fragment.");
    println!();
    println!("Both live on one listener: the portal's own page and socket are at / and /ws,");
    println!("and the study surface answers under /reverse.");
    // The URL is printed first, so a machine without an opener still shows the way in.
    crate::browser::open_in_browser(&url);

    // One listener, one token, the chat socket is the portal's; the study surface
    // answers beside it. `serve_with` blocks for the life of the process.
    loong_web::serve_with(portal.session, listener, token, Some(surface));
    Ok(())
}

/// Purpose: The directory the signal handler cleans; set once by `install_ctrlc_cleanup`.
/// Contract: `crates/cli/src/reverse.rs`.
#[cfg(unix)]
static CTRL_C_STATE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Purpose: The SIGINT handler: remove the manifest pair, then die with the shell's Ctrl+C code.
/// Contract: `crates/cli/src/reverse.rs`.
#[cfg(unix)]
extern "C" fn ctrlc_handler(_sig: i32) {
    if let Some(state) = CTRL_C_STATE.get() {
        cleanup_state(state);
    }
    // SAFETY: `_exit` is async-signal-safe; 130 is the conventional SIGINT status.
    unsafe { libc::_exit(130) }
}

/// Purpose: Wire the SIGINT handler so Ctrl+C still removes the manifest pair.
/// Serves: `run`, which would otherwise leave this run's `mcp.toml` behind on Ctrl+C.
/// Design: `docs/design/reverse.md` §3 (the manifest pair's lifetime is the session's).
/// Up: `run` only.
/// Down: `libc::signal`, `ctrlc_handler`.
/// Verify: `cargo test -p loongagent reverse_`.
#[cfg(unix)]
fn install_ctrlc_cleanup(state: PathBuf) -> Result<(), String> {
    CTRL_C_STATE
        .set(state)
        .map_err(|_| "the cleanup directory was set twice".to_string())?;
    // SAFETY: the handler only calls `cleanup_state` (file deletions) and `_exit`.
    // The pointer is a constant function item; no state is shared mutably.
    let handler: extern "C" fn(i32) = ctrlc_handler;
    unsafe {
        if libc::signal(libc::SIGINT, handler as libc::sighandler_t) == libc::SIG_ERR {
            return Err("installing the SIGINT handler failed".to_string());
        }
    }
    Ok(())
}

/// Purpose: On non-unix targets there is no signal wiring yet; the normal-exit cleanup still runs.
/// Contract: `crates/cli/src/reverse.rs`.
#[cfg(not(unix))]
fn install_ctrlc_cleanup(_state: PathBuf) -> Result<(), String> {
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
    /// Purpose: A project records the conversation to open and reopens that same log on the
    /// next start, inside the drawer of its own rather than the workspace's directory; a log
    /// that never got written is replaced rather than resumed forever.
    fn reverse_session_is_one_per_project() {
        let cwd = workspace();
        let home = workspace();
        let root = cwd.join(REVERSE_DIR);
        let record = loong_reverse_ui::project_state(&root, "project-001").join(SESSION_FILE);
        let drawer = crate::session::drawer_dir_in(
            &home,
            &crate::session::reverse_drawer_name(&cwd, "project-001"),
        );

        let first =
            bind_session_in(&home, &root, "project-001", &cwd, None).expect("bind a session");
        assert_eq!(
            first,
            crate::session::session_file_in_drawer(&drawer, &record_id(&record)),
            "the id it recorded is the log it will use, in this project's drawer"
        );
        assert!(
            first.starts_with(&drawer),
            "the log lives in the project's own drawer: {}",
            first.display()
        );
        // The workspace's directory is the TUI's, and this project's conversations are not in
        // it: that is the whole point of the drawer (sessions-lineage.md §1).
        assert_ne!(
            first.parent(),
            crate::session::session_file_in(&home, &cwd, "any").parent(),
            "a reverse project does not write into the workspace's session directory"
        );
        assert!(
            !first.exists(),
            "binding names the log; the server child is what writes it"
        );

        // A log the last session did write is the one this start reopens.
        std::fs::write(&first, "").expect("an empty session log");
        let again = bind_session_in(&home, &root, "project-001", &cwd, None).expect("bind again");
        assert_eq!(again, first, "a restart reopens the same conversation");

        // A recorded id whose log is gone is no conversation: the project gets a new one.
        std::fs::remove_file(&first).expect("the log goes missing");
        let third =
            bind_session_in(&home, &root, "project-001", &cwd, None).expect("bind a third time");
        assert_ne!(third, first, "no log, no history to reopen");
        assert_eq!(
            third,
            crate::session::session_file_in_drawer(&drawer, &record_id(&record)),
            "the new id is the one now recorded"
        );

        // A second project keeps its own conversation, in a drawer of its own.
        let other = bind_session_in(&home, &root, "project-002", &cwd, None)
            .expect("bind a second project");
        assert_ne!(other, third, "two projects do not share one conversation");
        assert_ne!(
            other.parent(),
            third.parent(),
            "two projects in one workspace do not share one drawer"
        );
        std::fs::remove_dir_all(&cwd).ok();
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    /// Purpose: A reverse project's conversations live in a drawer named after the project, so
    /// the workspace's TUI conversations are not in the list the page shows, and two projects
    /// in one workspace do not share a list.
    fn reverse_drawer_is_its_own_directory() {
        let cwd = workspace();
        let elsewhere = workspace();
        let home = workspace();
        let baidu = crate::session::reverse_drawer_name(&cwd, "baidu");
        let github = crate::session::reverse_drawer_name(&cwd, "github");
        assert!(
            baidu.starts_with("reverse-baidu-"),
            "the name says this is a reverse project and which one: {baidu}"
        );
        assert_ne!(baidu, github, "two projects do not share a drawer");
        assert_ne!(
            baidu,
            crate::session::reverse_drawer_name(&elsewhere, "baidu"),
            "the same project name in two workspaces is two projects"
        );
        // A name with nothing ASCII in it still gets a drawer of its own rather than the same
        // `reverse-project-` for every such project.
        let chinese = crate::session::reverse_drawer_name(&cwd, "百度分析");
        assert!(chinese.starts_with("reverse-project-"), "{chinese}");
        assert_ne!(chinese, crate::session::reverse_drawer_name(&cwd, "github"));

        // The TUI's conversation, in the workspace's directory, is nowhere in the drawer.
        let tui = crate::session::session_file_in(&home, &cwd, "tuionly");
        std::fs::create_dir_all(tui.parent().expect("the workspace drawer")).expect("mkdir");
        std::fs::write(&tui, "").expect("a TUI log");
        let drawer = crate::session::drawer_dir_in(&home, &baidu);
        assert_eq!(
            crate::session::session_infos_in_drawer(&drawer, 500).len(),
            0,
            "a new project lists nothing, not the workspace's conversations"
        );
        std::fs::create_dir_all(&drawer).expect("the project's drawer");
        std::fs::write(crate::session::session_file_in_drawer(&drawer, "mine"), "").expect("a log");
        let listed = crate::session::session_infos_in_drawer(&drawer, 500);
        assert_eq!(
            listed
                .iter()
                .map(|info| info.id.as_str())
                .collect::<Vec<_>>(),
            vec!["mine"],
            "the drawer lists exactly its own"
        );
        // And `session.open` inside the drawer cannot reach into the workspace either.
        assert!(
            crate::session::resolve_session_in_drawer(&drawer, "tuionly").is_err(),
            "a project cannot resume a conversation it does not show"
        );
        std::fs::remove_dir_all(&cwd).ok();
        std::fs::remove_dir_all(&elsewhere).ok();
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    /// Purpose: A conversation a project bound before drawers existed moves into its drawer,
    /// so that conversation stays in the page's list instead of looking deleted.
    fn reverse_migrates_earlier_conversations_into_the_drawer() {
        let cwd = workspace();
        let home = workspace();
        let root = cwd.join(REVERSE_DIR);
        let state = loong_reverse_ui::project_state(&root, "project-001");
        std::fs::create_dir_all(&state).expect("the project state");
        let old = crate::session::session_file_in(&home, &cwd, "oldone");
        std::fs::create_dir_all(old.parent().expect("the workspace drawer")).expect("mkdir");
        std::fs::write(&old, "").expect("an earlier log");
        std::fs::write(
            state.join(loong_reverse_ui::LEGACY_SESSIONS_FILE),
            "oldone\n../../etc/passwd\n",
        )
        .expect("the old list");

        let name = crate::session::reverse_drawer_name(&cwd, "project-001");
        let moved =
            migrate_sessions_in(&home, &root, "project-001", &cwd, &name).expect("the migration");
        assert_eq!(moved, 1, "the real id moves; a path is not an id");
        assert!(!old.exists(), "it left the workspace's session directory");
        assert!(
            crate::session::session_file_in_drawer(
                &crate::session::drawer_dir_in(&home, &name),
                "oldone"
            )
            .is_file(),
            "and is this project's conversation now"
        );
        assert_eq!(
            migrate_sessions_in(&home, &root, "project-001", &cwd, &name).expect("again"),
            0,
            "a second start moves nothing"
        );
        std::fs::remove_dir_all(&cwd).ok();
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    /// Purpose: Naming a session on the command line (`--session`, `--new`) is how a project's
    /// conversation changes: the named log becomes the project's, and the next start continues
    /// it — that is what makes "a restart shows yesterday's history" true after a rename.
    fn reverse_session_follows_a_named_one() {
        let cwd = workspace();
        let home = workspace();
        let root = cwd.join(REVERSE_DIR);
        let record = loong_reverse_ui::project_state(&root, "project-001").join(SESSION_FILE);

        let first =
            bind_session_in(&home, &root, "project-001", &cwd, None).expect("bind a session");
        std::fs::write(&first, "").expect("an empty session log");

        // The person names another conversation: the project adopts it. Naming one of the
        // project's own drawer is the case the page's picker produces.
        let drawer = crate::session::drawer_dir_in(
            &home,
            &crate::session::reverse_drawer_name(&cwd, "project-001"),
        );
        std::fs::create_dir_all(&drawer).expect("the project's drawer");
        let named = crate::session::session_file_in_drawer(&drawer, "namedone");
        std::fs::write(&named, "").expect("the named log");
        let chosen = bind_session_in(&home, &root, "project-001", &cwd, Some(&named))
            .expect("bind the named one");
        assert_eq!(chosen, named, "the named log is the one this start uses");
        assert_eq!(record_id(&record), "namedone", "and the one it records");

        // The next start — with nothing named — continues it, not the one before.
        let next = bind_session_in(&home, &root, "project-001", &cwd, None).expect("bind again");
        assert_eq!(next, named, "a restart continues the named conversation");
        assert_ne!(next, first, "the old one is no longer the project's");

        std::fs::remove_dir_all(&cwd).ok();
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    /// Purpose: A conversation another process is writing is left to that process: this start
    /// opens its own and leaves the record alone (reverse.md §11, one writer per log).
    fn reverse_session_steps_aside_when_the_log_is_held() {
        let cwd = workspace();
        let home = workspace();
        let root = cwd.join(REVERSE_DIR);
        let record = loong_reverse_ui::project_state(&root, "project-001").join(SESSION_FILE);

        let first =
            bind_session_in(&home, &root, "project-001", &cwd, None).expect("bind a session");
        std::fs::write(&first, "").expect("an empty session log");
        let held = loong_eventlog::SessionLog::open(&first).expect("the other process holds it");

        let second =
            bind_session_in(&home, &root, "project-001", &cwd, None).expect("bind while held");
        assert_ne!(second, first, "the held log is not this start's");
        assert_eq!(
            record_id(&record),
            crate::session::session_id_of(&first),
            "the project keeps the conversation the other process owns"
        );
        drop(held);
        std::fs::remove_dir_all(&cwd).ok();
        std::fs::remove_dir_all(&home).ok();
    }

    /// Purpose: The id a project recorded, read back the way `bind_session` writes it.
    /// Contract: `crates/cli/src/reverse.rs` test fixture.
    fn record_id(record: &Path) -> String {
        std::fs::read_to_string(record)
            .expect("a recorded id")
            .trim()
            .to_string()
    }

    #[test]
    /// Purpose: Scaffold makes the git-ignored layout for the first project and claims run-001.
    fn reverse_scaffold_creates_gitignored_dirs_and_manifest() {
        let cwd = workspace();
        let run = scaffold(&cwd, "abcd1234").expect("scaffold");
        assert_eq!(run.name, "run-001", "the first run of a session is run-001");
        assert_eq!(
            run.project, "project-001",
            "an empty directory gets one named project, not none"
        );
        assert!(run.study.is_dir(), "the project's notes directory exists");
        assert!(
            run.study.join("evidence").is_dir(),
            "intermediate products have a home from the start"
        );
        assert!(run.build.is_dir(), "the project's rebuild directory exists");
        assert_eq!(
            run.study,
            run.root.join("study").join("project-001"),
            "notes live under study/<project>, named after the project"
        );
        assert_eq!(
            std::fs::read_to_string(run.root.join("project")).unwrap(),
            "project-001\n",
            "the next start reads which project this was"
        );
        assert!(
            run.project_state.is_dir(),
            "the project's own state directory exists"
        );
        assert_eq!(
            std::fs::read_to_string(run.root.join(".gitignore")).unwrap(),
            "*\n",
            "the whole directory stays out of git"
        );
        assert_eq!(
            std::fs::read_to_string(run.state.join(SESSION_FILE)).unwrap(),
            "abcd1234\n",
            "the run records which conversation wrote it"
        );
        let manifest = std::fs::read_to_string(run.state.join(MCP_FILE)).unwrap();
        assert!(
            manifest.contains("name = \"loong-reverse\""),
            "the manifest names the session's server: {manifest}"
        );
        // The server is told the host asks: without it every run-code / change-device
        // tool answers `consent_required` and no answer in the page can release it.
        assert!(
            manifest.contains("args = [\"--allow-run\"]"),
            "the server defers consent to this session's approval card: {manifest}"
        );
        assert!(
            run.state.join(MCP_TRUST_FILE).is_file(),
            "the session trusts its own manifest"
        );
        // And the loader agrees when it is told about the run: the pair is trusted, the server active.
        let set = McpSet::load_from(&cwd, Some(&run.state)).unwrap();
        assert_eq!(set.active().len(), 1);
        assert_eq!(set.active()[0].origin, ManifestOrigin::Reverse);
        // A plain session reads none of it: the isolation is structural, not temporal.
        assert!(
            McpSet::load(&cwd).unwrap().servers.is_empty(),
            "a normal session never picks up a reverse manifest"
        );
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    /// Purpose: Cleanup removes the run's manifest pair but keeps that run's study notes.
    fn reverse_cleanup_removes_manifest_but_keeps_study() {
        let cwd = workspace();
        let run = scaffold(&cwd, "abcd1234").expect("scaffold");
        std::fs::write(run.study.join("00-overview.md"), "# notes\n").unwrap();
        cleanup(&run);
        assert!(!run.state.join(MCP_FILE).exists(), "the manifest is gone");
        assert!(
            !run.state.join(MCP_TRUST_FILE).exists(),
            "the trust record is gone"
        );
        assert!(
            run.study.join("00-overview.md").is_file(),
            "the study notes stay"
        );
        assert!(
            run.state.join(SESSION_FILE).is_file(),
            "the run's own bookkeeping stays"
        );
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    /// Purpose: A second reverse process claims the next run in the same project, and the two
    /// manifest pairs never share a directory; the notes directory is the project's, on purpose.
    fn reverse_second_run_does_not_touch_the_first() {
        let cwd = workspace();
        let first = scaffold(&cwd, "aaaa1111").expect("first run");
        std::fs::write(first.study.join("00-overview.md"), "# first\n").unwrap();
        let second = scaffold(&cwd, "bbbb2222").expect("second run");
        assert_eq!(second.name, "run-002", "the second process gets a new run");
        assert_eq!(
            first.project, second.project,
            "a restart resumes the project it left"
        );
        assert_eq!(
            first.study, second.study,
            "one project, one notes directory (reverse.md §3)"
        );
        assert_ne!(
            first.state, second.state,
            "their manifest pairs never share a directory"
        );
        // Each run's pair is its own: clearing the first leaves the second trusted.
        cleanup(&first);
        assert!(!first.state.join(MCP_FILE).exists());
        assert!(
            second.state.join(MCP_FILE).is_file(),
            "the surviving run keeps its manifest"
        );
        let set = McpSet::load_from(&cwd, Some(&second.state)).unwrap();
        assert_eq!(
            set.active().len(),
            1,
            "and the survivor still runs its tools"
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
        let err = scaffold(&cwd, "abcd1234").unwrap_err();
        assert!(err.contains("loong-reverse"), "{err}");
        assert!(err.contains("rename"), "{err}");
        assert_eq!(
            std::fs::read_dir(cwd.join(REVERSE_DIR).join(RUNS_DIR))
                .map(|dir| dir.count())
                .unwrap_or(0),
            0,
            "no run directory was claimed for a refused session"
        );
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    /// Purpose: A pair left behind by a killed run in another run directory is never read.
    fn reverse_never_reads_another_runs_leftover_manifest() {
        let cwd = workspace();
        let stale = scaffold(&cwd, "aaaa1111").expect("the killed run");
        // kill -9: the pair stays behind, still well-formed and trusted.
        std::fs::write(stale.state.join(MCP_FILE), "# stale\n").unwrap();
        // The next run gets its own directory and reads only that one.
        let fresh = scaffold(&cwd, "bbbb2222").expect("the new run");
        assert_eq!(fresh.name, "run-002");
        let set = McpSet::load_from(&cwd, Some(&fresh.state)).unwrap();
        assert_eq!(
            set.reverse_trust_failure(),
            None,
            "the new run's own pair is trusted"
        );
        assert_eq!(set.active().len(), 1);
        // The leftover is inert for everyone: no session is ever told about that directory.
        assert!(McpSet::load(&cwd).unwrap().servers.is_empty());
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    /// Purpose: A target recorded by an old layout moves into the project, so the upgrade empties
    /// nothing — and it stays the project's target rather than one run's.
    fn reverse_carries_the_old_layouts_target_into_the_project() {
        let cwd = workspace();
        let root = cwd.join(REVERSE_DIR);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("target"), "/bin/true\n").unwrap();
        std::fs::write(root.join("target-kind"), "binary\n").unwrap();
        let first = scaffold(&cwd, "aaaa1111").expect("scaffold");
        assert_eq!(
            std::fs::read_to_string(first.project_state.join("target")).unwrap(),
            "/bin/true\n",
            "the target bar still has something to show"
        );
        assert_eq!(
            std::fs::read_to_string(first.project_state.join("target-kind")).unwrap(),
            "binary\n",
            "and the kind travels with it"
        );
        assert!(!root.join("target").exists(), "the old file is gone");
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    /// Purpose: A target left inside an older build's `runs/<run>/` is adopted by the project, so
    /// a workspace that already moved to the run layout survives this change too.
    fn reverse_carries_a_runs_target_into_the_project() {
        let cwd = workspace();
        let old = cwd.join(REVERSE_DIR).join("runs").join("run-001");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("target"), "/bin/false\n").unwrap();
        std::fs::write(old.join("target-kind"), "binary\n").unwrap();
        let fresh = scaffold(&cwd, "bbbb2222").expect("scaffold");
        assert_eq!(
            std::fs::read_to_string(fresh.project_state.join("target")).unwrap(),
            "/bin/false\n",
            "the newest run's target becomes the project's"
        );
        assert!(!old.join("target").exists(), "and is not left behind");
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    /// Purpose: A project created in the page is the one the next start resumes.
    fn reverse_resumes_the_recorded_project() {
        let cwd = workspace();
        let run = scaffold(&cwd, "aaaa1111").expect("scaffold");
        loong_reverse_ui::create_project(&run.root, "我的手电筒").expect("a project from the rail");
        assert!(
            run.root.join("study").join("我的手电筒").is_dir(),
            "with its notes directory"
        );
        let next = scaffold(&cwd, "bbbb2222").expect("restart");
        assert_eq!(
            next.project, "我的手电筒",
            "the restart lands on the same project"
        );
        assert_eq!(
            next.study,
            run.root.join("study").join("我的手电筒"),
            "and writes the same notes directory"
        );
        assert_eq!(next.name, "run-002", "as a new run of that project");
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    /// Purpose: The old layout's pair in `.reverse/` itself is removed, so a normal session stays clean.
    fn reverse_removes_the_old_layouts_manifest_pair() {
        let cwd = workspace();
        let root = cwd.join(REVERSE_DIR);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join(MCP_FILE),
            "[[server]]\nname = \"loong-reverse\"\ncommand = \"x\"\n",
        )
        .unwrap();
        std::fs::write(root.join(MCP_TRUST_FILE), "{}").unwrap();
        let run = scaffold(&cwd, "abcd1234").expect("scaffold");
        assert!(!root.join(MCP_FILE).exists(), "the legacy manifest is gone");
        assert!(
            !root.join(MCP_TRUST_FILE).exists(),
            "the legacy trust record is gone"
        );
        assert!(
            run.state.join(MCP_FILE).is_file(),
            "the run's own pair exists"
        );
        std::fs::remove_dir_all(&cwd).ok();
    }
}
