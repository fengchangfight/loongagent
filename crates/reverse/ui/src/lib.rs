//! Purpose: Serve the reverse study tab — the project list, the notes, the files and the page.
//!
//! # Contract
//! - **Serves:** the study tab of the `reverse` command's portal (`docs/design/reverse.md` §4).
//! - **Design:** `docs/design/reverse.md` §4 and `docs/design/surfaces.md` §4.
//! - **Upstream:** `crates/cli/src/reverse.rs` mounts `StudySurface` through `serve_with`.
//! - **Downstream:** `loong_web::Router`; the chat socket itself stays the portal's.
//! - **Verify:** `cargo test -p loong-reverse-ui` · `./scripts/check.sh`
//!
//! One portal, several surfaces (surfaces.md §4): the portal owns the listener, the token
//! and the chat socket; this crate owns one URL prefix, `/reverse`, and the files under it.

use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use loong_web::Router;

/// Purpose: Where a `GET` to the reverse routes lands.
/// Serves: `handle`, which answers from it.
/// Design: `docs/design/reverse.md` §4.
/// Up: `handle` only.
/// Down: the response body and content type.
/// Verify: `cargo test -p loong-reverse-ui`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// A 404 with a plain-text reason.
    NotFound(String),
    /// The JSON file tree of the current project: its `study/` and `build/` only.
    Tree,
    /// The bytes of one file under `.reverse/`.
    File(PathBuf),
    /// A directory listing for the target picker; empty means the home directory.
    List(String),
    /// The chosen target: `GET` reads it, `POST` records it.
    Target,
    /// The workflow phase: `GET` reads whether study is finished, `POST` sets it.
    Phase,
    /// The projects under `.reverse/projects/`: `GET /reverse/projects` lists them,
    /// `POST /reverse/project` creates, selects or deletes one.
    Projects,
    /// Move the current project's notes into `.trash/` so a redo starts from nothing.
    Redo,
    /// The conversation this project opens next: `POST /reverse/session` records the id a
    /// person picked in the chat header, which is what the next start reads.
    Session,
    /// Which run this session is and which project it studies: the two directories the
    /// prompts name.
    Run,
    /// The study page (project rail, target, workflow, panes and chat).
    Page,
    /// Not a reverse route at all: the portal handles it.
    Pass,
}

/// Purpose: One entry the target picker shows.
/// Serves: the JSON answer of `GET /reverse/ls`.
/// Design: `docs/design/reverse.md` §4.1.
/// Up: `list_dir`, which builds them.
/// Down: the page, which renders and clicks them.
/// Verify: `cargo test -p loong-reverse-ui ls_`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The last path component, as shown.
    pub name: String,
    /// The absolute path the page will send back.
    pub path: String,
    /// A directory (clickable to descend) or a file (clickable to choose).
    pub dir: bool,
}

/// Purpose: The answer of `GET /reverse/ls`: where we are and what is here.
/// Serves: the target picker.
/// Design: `docs/design/reverse.md` §4.1.
/// Up: `list_dir` builds it, the page renders it.
/// Down: nothing.
/// Verify: `cargo test -p loong-reverse-ui ls_`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    /// The directory being listed, absolute.
    pub path: String,
    /// One level up, or `None` at the filesystem root.
    pub parent: Option<String>,
    /// Directories first, then files, each sorted by name.
    pub entries: Vec<Entry>,
}

/// Purpose: What kind of target the user picked — it decides the *first* tool call, not just a label.
///
/// Only two kinds exist because only two entrances do: `core_target_open` takes an absolute
/// path, `web_site_open` takes a URL (the engine's `TargetKind` has no URL variant). What a
/// local file *is* — ELF, PE, Mach-O, APK — is settled after opening by `core_target_triage`,
/// which sniffs the bytes and names the toolsets; a tab that guessed got `.aab`, `.xapk`,
/// `.dex` and repacked zips wrong. The kind must survive a page reload: re-opening on the
/// wrong tab would point the model at the wrong tool (reverse.md §4.1).
/// Serves: the two-tab target bar and the study prompt, which branches on it.
/// Design: `docs/design/reverse.md` §4.1.
/// Up: `read_target`/`record_target`, and the page's tab state.
/// Down: `Target`, `read_target`, `record_target`.
/// Verify: `cargo test -p loong-reverse-ui target_`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    /// A file or folder on this machine: ELF/PE/Mach-O, an APK, a jar, whatever opens.
    Local,
    /// A website, opened in a session-private browser by `web_site_open`.
    Url,
}

impl TargetKind {
    /// Purpose: The word written on the first line of `.reverse/target-kind` and the page's tabs.
    /// Serves: `record_target`, which writes it, and the page, which draws one tab per word.
    /// Design: `docs/design/reverse.md` §4.1.
    /// Up: `record_target` and the page's tab strip.
    /// Down: nothing; a constant per variant.
    /// Verify: `cargo test -p loong-reverse-ui`.
    pub fn id(self) -> &'static str {
        match self {
            TargetKind::Local => "local",
            TargetKind::Url => "url",
        }
    }

    /// Purpose: Read back what `id` wrote; unknown text is not guessed.
    ///
    /// `binary` and `apk` are the words older builds wrote, when the page had a tab per file
    /// format; both are the local entrance, so they read back as `Local`.
    /// Serves: `read_target`, which trusts the recorded word only if it parses.
    /// Design: `docs/design/reverse.md` §4.1.
    /// Up: `read_target`.
    /// Down: nothing; a match over the four words.
    /// Verify: `cargo test -p loong-reverse-ui`.
    pub fn parse(text: &str) -> Option<TargetKind> {
        match text.trim() {
            "local" | "binary" | "apk" => Some(TargetKind::Local),
            "url" => Some(TargetKind::Url),
            _ => None,
        }
    }

    /// Purpose: Guess the kind from the value alone, for targets recorded before kinds existed.
    ///
    /// `http(s)://` is a URL; anything else is a local path. An APK is not special here: the
    /// triage after opening says what it is, and the suffix often is not `.apk`.
    /// A guess, so `read_target` prefers the recorded kind when there is one.
    /// Serves: `read_target`, for a `target` file written before kinds existed.
    /// Design: `docs/design/reverse.md` §4.1.
    /// Up: `read_target` only.
    /// Down: nothing; the value is inspected, not opened.
    /// Verify: `cargo test -p loong-reverse-ui`.
    pub fn infer(value: &str) -> TargetKind {
        if value.starts_with("http://") || value.starts_with("https://") {
            TargetKind::Url
        } else {
            TargetKind::Local
        }
    }
}

/// Purpose: The target the user chose: a kind and the absolute path or URL it applies to.
/// Serves: the target bar's "已选定" line and the study prompt.
/// Design: `docs/design/reverse.md` §4.1.
/// Up: `read_target` returns it, `record_target` builds it.
/// Down: the page, as the two lines of `GET /reverse/target`.
/// Verify: `cargo test -p loong-reverse-ui target_`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Which of the three tabs this belongs to.
    pub kind: TargetKind,
    /// An absolute path for `Binary`/`Apk`, an `http(s)://` URL for `Url`.
    pub value: String,
}

/// Purpose: The `GET /reverse/target` and `POST /reverse/target` body — kind on line one, value on line two.
///
/// Two lines rather than JSON: neither a path nor a URL can contain a newline, so reading it
/// is one `split_once`, and this crate needs no serde (reverse.md §4.1).
/// Contract: `crates/reverse/ui/src/lib.rs`.
fn target_body(target: &Target) -> String {
    format!("{}\n{}\n", target.kind.id(), target.value)
}

/// Purpose: Parse the two-line target body; the value may be empty (nothing chosen yet).
/// Contract: `crates/reverse/ui/src/lib.rs`.
fn parse_target_body(body: &str) -> (Option<TargetKind>, String) {
    match body.split_once('\n') {
        Some((kind, value)) => (TargetKind::parse(kind), value.trim().to_string()),
        None => (TargetKind::parse(body), String::new()),
    }
}

/// Purpose: List one directory for the target picker — names and kinds only, never contents.
///
/// Serves: the "选择…" browser that replaces the browser's `fakepath` (reverse.md §4.1).
/// Design: `docs/design/reverse.md` §4.1.
/// Up: `handle` for `GET /reverse/ls`.
/// Down: `std::fs::read_dir`.
/// Verify: `cargo test -p loong-reverse-ui ls_`.
pub fn list_dir(dir: &Path) -> Result<Listing, String> {
    let meta = std::fs::metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !meta.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    let mut entries: Vec<Entry> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            // Hidden files are still listed: `.reverse/` itself starts with a dot.
            // Sockets, pipes and the like are dropped: they are not pickable targets.
            let file_type = entry.file_type().ok()?;
            if !file_type.is_dir() && !file_type.is_file() {
                return None;
            }
            Some(Entry {
                name: entry.file_name().to_string_lossy().to_string(),
                path: entry.path().display().to_string(),
                dir: file_type.is_dir(),
            })
        })
        .collect();
    entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.cmp(&b.name)));
    let parent = dir
        .parent()
        .map(|p| p.display().to_string())
        .filter(|p| p != &dir.display().to_string());
    Ok(Listing {
        path: dir.display().to_string(),
        parent,
        entries,
    })
}

/// Purpose: The JSON answer of `GET /reverse/ls` for the target picker.
/// Serves: `handle`.
/// Design: `docs/design/reverse.md` §4.1.
/// Up: `handle` only.
/// Down: `escape_json` per field.
/// Verify: `cargo test -p loong-reverse-ui ls_`.
fn listing_json(listing: &Listing) -> String {
    let entries: Vec<String> = listing
        .entries
        .iter()
        .map(|entry| {
            format!(
                "{{\"name\":\"{}\",\"path\":\"{}\",\"dir\":{}}}",
                escape_json(&entry.name),
                escape_json(&entry.path),
                entry.dir
            )
        })
        .collect();
    format!(
        "{{\"path\":\"{}\",\"parent\":{},\"entries\":[{}]}}",
        escape_json(&listing.path),
        match &listing.parent {
            Some(parent) => format!("\"{}\"", escape_json(parent)),
            None => "null".to_string(),
        },
        entries.join(",")
    )
}

/// Purpose: Read the request body, from what arrived with the head plus the rest of the stream.
/// Serves: `POST /reverse/target`.
/// Design: `docs/design/reverse.md` §4.1.
/// Up: `handle`.
/// Down: `TcpStream::read`.
/// Verify: `cargo test -p loong-reverse-ui target_`.
fn read_body(head: &str, stream: &mut TcpStream) -> Result<String, String> {
    use std::io::Read;
    let (headers, already) = head.split_once("\r\n\r\n").unwrap_or((head, ""));
    let declared: usize = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.trim().eq_ignore_ascii_case("content-length") {
                value.trim().parse().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    let mut body = already.as_bytes().to_vec();
    while body.len() < declared {
        let mut chunk = [0u8; 1024];
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(e) => return Err(format!("reading the request body: {e}")),
        }
    }
    body.truncate(declared.max(already.len()));
    Ok(String::from_utf8_lossy(&body).to_string())
}

/// Purpose: The content type a `Route::File` answer carries, from the suffix.
/// Contract: `crates/reverse/ui/src/lib.rs`.
fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "md" | "markdown" => "text/markdown; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "smali" | "kt" | "java" | "rs" | "c" | "h" | "cpp" | "py" | "txt" | "xml" => {
            "text/plain; charset=utf-8"
        }
        "sqlite" | "db" => "application/octet-stream",
        _ => "application/octet-stream",
    }
}

/// Purpose: True when `path` stays inside `root` after resolving `..` lexically.
/// Serves: `route`, which must not let `../` escape `.reverse/`.
/// Design: `docs/design/reverse.md` §4 (the tree is the session's own files).
/// Up: `route` only.
/// Down: nothing.
/// Verify: `cargo test -p loong-reverse-ui route_`.
fn inside(root: &Path, path: &Path) -> bool {
    let mut depth: i64 = 0;
    for part in path.components() {
        match part {
            std::path::Component::Normal(_) => depth += 1,
            std::path::Component::ParentDir => depth -= 1,
            std::path::Component::CurDir => {}
            _ => return false,
        }
        if depth < 0 {
            return false;
        }
    }
    let _ = root;
    true
}

/// Purpose: Turn a request path into a `Route` against the session's `.reverse/` directory.
/// Serves: the reverse session's HTTP layer.
/// Design: `docs/design/reverse.md` §4.
/// Up: `crates/cli/src/reverse.rs`'s accept loop (once it installs this).
/// Down: `inside`, `content_type`.
/// Verify: `cargo test -p loong-reverse-ui route_`.
pub fn route(root: &Path, path: &str) -> Route {
    let bare = path.split('?').next().unwrap_or("");
    let query = path.split_once('?').map(|(_, q)| q).unwrap_or("");
    if bare == "/reverse" || bare == "/reverse/" {
        return Route::Page;
    }
    if let Some(rest) = bare.strip_prefix("/reverse/") {
        if rest == "tree" {
            return Route::Tree;
        }
        if rest == "target" {
            return Route::Target;
        }
        if rest == "phase" {
            return Route::Phase;
        }
        if rest == "run" {
            return Route::Run;
        }
        // `projects` lists, `project` changes: one route, three verbs, two names (reverse-ui.md §Routes).
        if rest == "projects" || rest == "project" {
            return Route::Projects;
        }
        if rest == "study/redo" {
            return Route::Redo;
        }
        if rest == "session" {
            return Route::Session;
        }
        if rest == "ls" {
            let wanted = query
                .split('&')
                .find_map(|pair| pair.strip_prefix("path="))
                .map(percent_decode)
                .unwrap_or_default();
            return Route::List(wanted);
        }
        if let Some(file) = rest.strip_prefix("file/") {
            if file.is_empty() || !inside(root, Path::new(file)) {
                return Route::NotFound("that path stays inside .reverse/".to_string());
            }
            return Route::File(PathBuf::from(file));
        }
        return Route::NotFound(format!("no reverse route for {bare}"));
    }
    Route::Pass
}

/// Purpose: Decode `%XX` escapes and `+` from a query parameter.
/// Serves: `route`, which reads `?path=` from `GET /reverse/tree`.
/// Design: `docs/design/reverse.md` §4.
/// Up: `query_param` only.
/// Down: `u8::from_str_radix` and `String::from_utf8_lossy`.
/// Verify: `cargo test -p loong-reverse-ui`.
fn percent_decode(text: &str) -> String {
    let bytes = text.replace('+', " ").into_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Purpose: The absolute path of the reverse session's `.reverse/` directory.
/// Contract: `crates/reverse/ui/src/lib.rs`.
pub const REVERSE_DIR: &str = ".reverse";

/// Purpose: The directory holding one state directory per project.
/// Serves: `project_state`, `list_projects`, the CLI's scaffolding and the page's rail.
/// Design: `docs/design/reverse.md` §3.
/// Up: `crates/cli/src/reverse.rs` creates it; `project_state` reads under it.
/// Down: nothing.
/// Verify: `cargo test -p loong-reverse-ui project_`.
pub const PROJECTS_DIR: &str = "projects";

/// Purpose: The file naming the current project, one line, at the top of `.reverse/`.
/// Serves: `current_project`, so a restart resumes the project the user was on.
/// Design: `docs/design/reverse.md` §3.
/// Up: `create_project` and `select_project` write it; `current_project` reads it.
/// Down: nothing.
/// Verify: `cargo test -p loong-reverse-ui project_`.
pub const PROJECT_FILE: &str = "project";

/// Purpose: Where a study's intermediate products go, inside its notes directory.
///
/// Unpacked archives, saved scripts, captures and traces are study material, so they live
/// under `study/<project>/` and not in `build/<project>/`, which is reserved for the rebuild
/// (`docs/design/reverse.md` §3). Created up front because the capture tools insist on a
/// parent that already exists. A file under it is *not* a note: `studied` skips the whole
/// directory, so a study that produced only evidence still counts as not written up.
/// Serves: `create_project`, `redo_study`, `studied` and the page's prompts.
/// Design: `docs/design/reverse.md` §3.
/// Up: `create_project` and `redo_study` create it; `studied` skips it.
/// Down: nothing; the model writes into it with its own tools.
/// Verify: `cargo test -p loong-reverse-ui evidence_`.
pub const EVIDENCE_DIR: &str = "evidence";

/// Purpose: Where a redone study's notes are moved instead of being deleted.
/// Serves: `redo_study`.
/// Design: `docs/design/reverse.md` §3.
/// Up: `redo_study` writes under it.
/// Down: nothing; it is never read back by the program.
/// Verify: `cargo test -p loong-reverse-ui redo_`.
pub const TRASH_DIR: &str = ".trash";

/// Purpose: The project the first `loongagent reverse` in an empty directory creates.
/// Serves: `crates/cli/src/reverse.rs`, which has nothing to resume yet.
/// Design: `docs/design/reverse.md` §3.
/// Up: the CLI's scaffolding.
/// Down: nothing.
/// Verify: `cargo test -p loong-reverse-ui project_`.
pub const DEFAULT_PROJECT: &str = "project-001";

/// Purpose: Refuse a project name that cannot safely be a directory name under `.reverse/`.
///
/// A name is a directory name and nothing else: Chinese and spaces are fine, but `/`, `\`,
/// `..` and a leading dot are not, because each of them can turn `study/<name>` into somewhere
/// else (`docs/design/reverse.md` §3). The page shows the reason, so a typo is a sentence, not
/// a mystery.
/// Serves: `create_project`, `select_project`, `redo_study` and the CLI's scaffolding.
/// Design: `docs/design/reverse.md` §3.
/// Up: `handle` (through the three functions above) and `crates/cli/src/reverse.rs`.
/// Down: nothing; the string is inspected, never touched on disk.
/// Verify: `cargo test -p loong-reverse-ui project_name_`.
pub fn valid_project_name(name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("项目名不能为空".to_string());
    }
    if name.chars().count() > 64 {
        return Err("项目名最多 64 个字符".to_string());
    }
    if name.starts_with('.') {
        return Err("项目名不能以点开头：.reverse/ 里点开头的东西是留给程序的".to_string());
    }
    if name.contains('/') || name.contains('\\') {
        return Err("项目名不能包含 / 或 \\：它就是目录名".to_string());
    }
    if name.contains("..") {
        return Err("项目名不能包含 ..".to_string());
    }
    if name.chars().any(char::is_control) {
        return Err("项目名不能包含控制字符".to_string());
    }
    Ok(())
}

/// Purpose: One project's state directory: `.reverse/projects/<name>`.
/// Serves: `read_target`, `write_phase` and the two writers, all of which take a directory.
/// Design: `docs/design/reverse.md` §3.
/// Up: `handle`, `create_project`, `redo_study`.
/// Down: nothing; it only joins path components.
/// Verify: `cargo test -p loong-reverse-ui project_`.
pub fn project_state(root: &Path, name: &str) -> PathBuf {
    root.join(PROJECTS_DIR).join(name)
}

/// Purpose: The file inside a project's state that names the conversation to open next.
/// Serves: `record_session`, `read_recorded_session`, and the CLI's startup binding, which
/// reads the same file — one definition, so the writer and the reader cannot disagree.
/// Design: `docs/design/reverse.md` §5.
/// Up: the page's picker writes it through `POST /reverse/session`.
/// Down: nothing.
/// Verify: `cargo test -p loong-reverse-ui session_`.
pub const SESSION_FILE: &str = "session";

/// Purpose: The file that listed the conversations a project had used, before drawers existed.
///
/// Kept only to be read: a project that bound conversations in the workspace's session
/// directory still lists them here, and the CLI moves those logs into the project's own
/// drawer on the next start (`crates/cli/src/reverse.rs`). Nothing writes it any more — the
/// drawer *is* the list (sessions-lineage.md §1).
/// Serves: `crates/cli/src/reverse.rs`'s `migrate_sessions_in`, through nothing else.
/// Design: `docs/design/reverse-conversation.md` 「A project's sessions」.
/// Up: that migration only.
/// Down: nothing.
/// Verify: `cargo test -p loongagent reverse_drawer`.
pub const LEGACY_SESSIONS_FILE: &str = "sessions";

/// Purpose: Whether a string is a session id and not a path dressed up as one.
///
/// Session ids are the log files' own names, so a caller that has to look one up must not be
/// able to reach outside the session directory with it. Shared by every writer here and by
/// the CLI's opener (`crates/cli/src/server.rs`), so the gate is one gate.
/// Serves: `record_session`.
/// Design: `docs/design/reverse-conversation.md` 「The picker」.
/// Up: `record_session`.
/// Down: nothing.
/// Verify: `cargo test -p loong-reverse-ui session_`.
pub fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && !id.contains('/')
        && !id.contains('\\')
        && !id.contains("..")
        && !id.ends_with(".jsonl")
        && id.chars().all(|c| !c.is_whitespace() && !c.is_control())
}

/// Purpose: Record which conversation this project opens next, after a person picked one in
/// the page's chat header.
///
/// The surface writes this because the page is the only place that knows a person chose a
/// different conversation; the CLI reads it at the next start (reverse.md §5). Nothing is
/// checked against the session directory here — whether that log exists is the kernel
/// opener's business, and it refuses an id it cannot open before this record is written.
/// Serves: the `POST` half of `Route::Session`.
/// Design: `docs/design/reverse-conversation.md` 「The picker」.
/// Up: `handle`'s `Route::Session` arm only.
/// Down: `std::fs`.
/// Verify: `cargo test -p loong-reverse-ui session_`.
fn record_session(pstate: &Path, id: &str) -> Result<(), String> {
    if !valid_session_id(id) {
        return Err(format!("{id:?} 不是会话 id"));
    }
    std::fs::create_dir_all(pstate).map_err(|e| format!("{}: {e}", pstate.display()))?;
    let path = pstate.join(SESSION_FILE);
    std::fs::write(&path, format!("{id}\n")).map_err(|e| format!("{}: {e}", path.display()))
}

/// Purpose: Note in this run's state which conversation its notes now come from, after a
/// person switched mid-run.
///
/// `runs/<run>/session` is written when the run is scaffolded
/// (`crates/cli/src/reverse.rs`); a switch inside the page changes the conversation that
/// writes the notes, and leaving the old id there would make the run claim a conversation it
/// stopped using. The run's state directory is this process's own, so this is a note, not a
/// projection of the project.
/// Serves: the `POST` half of `Route::Session`.
/// Design: `docs/design/reverse.md` §3; `docs/design/reverse-conversation.md` 「The picker」.
/// Up: `handle`'s `Route::Session` arm.
/// Down: `std::fs`.
/// Verify: `cargo test -p loong-reverse-ui session_`.
fn record_run_session(state: &Path, id: &str) -> Result<(), String> {
    let path = state.join(SESSION_FILE);
    if !state.is_dir() {
        // An older layout, or a run directory that was never made: nothing to correct.
        return Ok(());
    }
    std::fs::write(&path, format!("{id}\n")).map_err(|e| format!("{}: {e}", path.display()))
}

/// Purpose: The conversation this project recorded, or empty when it recorded none.
/// Serves: the `GET` half of `Route::Session`.
/// Design: `docs/design/reverse-conversation.md` 「The picker」.
/// Up: `handle`'s `Route::Session` arm only.
/// Down: `std::fs::read_to_string`.
/// Verify: `cargo test -p loong-reverse-ui session_`.
fn read_recorded_session(pstate: &Path) -> Option<String> {
    let text = std::fs::read_to_string(pstate.join(SESSION_FILE)).ok()?;
    let id = text.trim();
    (!id.is_empty()).then(|| id.to_string())
}

/// Purpose: One project's notes directory: `.reverse/study/<name>`.
/// Contract: `crates/reverse/ui/src/lib.rs`.
fn project_study(root: &Path, name: &str) -> PathBuf {
    root.join("study").join(name)
}

/// Purpose: One project's evidence directory: `.reverse/study/<name>/evidence`.
/// Contract: `crates/reverse/ui/src/lib.rs`.
fn project_evidence(root: &Path, name: &str) -> PathBuf {
    project_study(root, name).join(EVIDENCE_DIR)
}

/// Purpose: Every project that exists, sorted by name.
///
/// Only directories: a stray file under `projects/` is not a project.
/// Serves: `projects_json`, so the rail lists exactly what can be switched to.
/// Design: `docs/design/reverse.md` §3.
/// Up: `projects_json`.
/// Down: `std::fs::read_dir` on `projects/`.
/// Verify: `cargo test -p loong-reverse-ui project_`.
pub fn list_projects(root: &Path) -> Vec<String> {
    let Ok(read) = std::fs::read_dir(root.join(PROJECTS_DIR)) else {
        return Vec::new();
    };
    let mut names: Vec<String> = read
        .flatten()
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| valid_project_name(name).is_ok())
        .collect();
    names.sort();
    names
}

/// Purpose: The project the page and the prompts work on, or `None` when there is none.
///
/// `.reverse/project` names it; the name only counts if its directory still exists, so a
/// deleted project is not silently reopened.
/// Serves: `handle`, `projects_json`, `run_json` and the CLI.
/// Design: `docs/design/reverse.md` §3.
/// Up: `handle`, `run_json`, `crates/cli/src/reverse.rs`.
/// Down: `std::fs::read_to_string` on `project`, then `project_state`'s `is_dir`.
/// Verify: `cargo test -p loong-reverse-ui project_`.
pub fn current_project(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join(PROJECT_FILE)).ok()?;
    let name = text.trim().to_string();
    valid_project_name(&name).ok()?;
    project_state(root, &name).is_dir().then_some(name)
}

/// Purpose: Whether a project has ever been studied — a file under `study/<name>/`, evidence out.
///
/// The test is "a note exists", not `phase`: `phase` answers "did the user declare the study
/// finished", which is a different question, and a project can hold notes without one
/// (`docs/design/reverse.md` §3). `evidence/` does not count: a study that only unpacked an
/// APK or captured a page has written nothing down yet, so the page must not greet it with
/// the redo question.
/// Serves: `projects_json` (the rail's dot) and `run_json` (the page's redo question).
/// Design: `docs/design/reverse.md` §3.
/// Up: `projects_json`, `run_json`, `redo_study`.
/// Down: `std::fs::read_dir`, walked with an explicit stack.
/// Verify: `cargo test -p loong-reverse-ui evidence_` · `cargo test -p loong-reverse-ui redo_`.
pub fn studied(root: &Path, name: &str) -> bool {
    let mut stack = vec![project_study(root, name)];
    while let Some(dir) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_file() {
                return true;
            }
            if kind.is_dir() && entry.file_name() != EVIDENCE_DIR {
                stack.push(entry.path());
            }
        }
    }
    false
}

/// Purpose: Record which project the page works on, after checking it exists.
/// Serves: `POST /reverse/project` with `select:<name>`.
/// Design: `docs/design/reverse.md` §3.
/// Up: `handle`.
/// Down: `write_current` and `project_state`.
/// Verify: `cargo test -p loong-reverse-ui project_`.
pub fn select_project(root: &Path, name: &str) -> Result<(), String> {
    valid_project_name(name)?;
    if !project_state(root, name).is_dir() {
        return Err(format!("没有 {name} 这个项目"));
    }
    write_current(root, name)
}

/// Purpose: Start an empty project and make it current — the page then reloads to a clean one.
///
/// Creates the project's directories up front (`projects/`, `study/`, `study/.../evidence`,
/// `build/`), so the tree shows the project's two roots immediately, the first note has
/// somewhere to land, and a capture tool is handed a parent that already exists
/// (`create_absent_dir` refuses a missing one).
/// Serves: `POST /reverse/project` with `create:<name>` and the CLI's first start.
/// Design: `docs/design/reverse.md` §3.
/// Up: `handle`, `crates/cli/src/reverse.rs`.
/// Down: `std::fs::create_dir_all` and `write_current`.
/// Verify: `cargo test -p loong-reverse-ui project_`.
pub fn create_project(root: &Path, name: &str) -> Result<(), String> {
    valid_project_name(name)?;
    let state = project_state(root, name);
    if state.is_dir() {
        return Err(format!("项目 {name} 已经存在"));
    }
    for dir in [
        state,
        project_study(root, name),
        project_evidence(root, name),
        root.join("build").join(name),
    ] {
        std::fs::create_dir_all(&dir).map_err(|e| format!("建项目 {name} 失败：{e}"))?;
    }
    write_current(root, name)
}

/// Purpose: Write `.reverse/project`, the one line the next start reads.
/// Contract: `crates/reverse/ui/src/lib.rs`.
fn write_current(root: &Path, name: &str) -> Result<(), String> {
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    std::fs::write(root.join(PROJECT_FILE), format!("{name}\n"))
        .map_err(|e| format!("记录当前项目失败：{e}"))
}

/// Purpose: Delete a project for good — every directory it owns, plus its leftovers in `.trash/`.
///
/// Not a rename: the user asked for the project gone from the filesystem, so the three
/// directories are removed and the process's free space comes back (`docs/design/reverse.md`
/// §3). The trash entries named `<project>-<stamp>` go too: they are this project's earlier
/// notes, and a delete that left them behind would keep both the disk and the name alive.
/// A delete of the current project hands the session to the next one still standing, so the
/// page reloads onto a project instead of an empty rail; when none is left the `project` file
/// goes, and the page says there is no project.
/// Serves: `POST /reverse/project` with `delete:<name>`.
/// Design: `docs/design/reverse.md` §3.
/// Up: `handle`.
/// Down: `remove_path`, `remove_trash`, `list_projects`, `write_current`.
/// Verify: `cargo test -p loong-reverse-ui delete_`.
pub fn delete_project(root: &Path, name: &str) -> Result<(), String> {
    valid_project_name(name)?;
    if !project_state(root, name).is_dir() && !project_study(root, name).is_dir() {
        return Err(format!("没有 {name} 这个项目"));
    }
    // Read before deleting: once the directory is gone `current_project` answers `None` for
    // every project, so a check afterwards could not tell which one was current.
    let was_current = current_project(root).as_deref() == Some(name);
    for dir in [
        project_state(root, name),
        project_study(root, name),
        root.join("build").join(name),
    ] {
        remove_path(&dir)?;
    }
    remove_trash(root, name)?;
    if was_current {
        return match list_projects(root).first() {
            Some(next) => write_current(root, next),
            None => std::fs::remove_file(root.join(PROJECT_FILE))
                .map_err(|e| format!("清掉当前项目记录失败：{e}")),
        };
    }
    Ok(())
}

/// Purpose: Remove a file or a whole directory, and say nothing when it was never there.
///
/// A symlink is unlinked, never followed: a project directory that is really a link must not
/// take its target with it.
/// Serves: `delete_project`, which deletes the three directories and the project's trash.
/// Design: `docs/design/reverse.md` §3.
/// Up: `delete_project`, `remove_trash`.
/// Down: `std::fs::symlink_metadata`, `std::fs::remove_dir_all`, `std::fs::remove_file`.
/// Verify: `cargo test -p loong-reverse-ui delete_`.
fn remove_path(path: &Path) -> Result<(), String> {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    let done = if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    done.map_err(|e| format!("删 {} 失败：{e}", path.display()))
}

/// Purpose: Remove every `.trash/<name>-<stamp>` directory, and leave other projects' alone.
///
/// The stamp is the only thing that may follow the name, so deleting project `run` does not
/// touch the trash of project `run-001` — `001-<stamp>` is not a stamp and never matches.
/// Serves: `delete_project`, so a delete leaves no old notes of that project behind.
/// Design: `docs/design/reverse.md` §3.
/// Up: `delete_project`.
/// Down: `std::fs::read_dir` on `.trash/`, then `remove_path`.
/// Verify: `cargo test -p loong-reverse-ui delete_`.
fn remove_trash(root: &Path, name: &str) -> Result<(), String> {
    let trash = root.join(TRASH_DIR);
    let Ok(read) = std::fs::read_dir(&trash) else {
        return Ok(());
    };
    let prefix = format!("{name}-");
    for entry in read.flatten() {
        let leaf = entry.file_name().to_string_lossy().to_string();
        let is_ours = leaf
            .strip_prefix(&prefix)
            .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()));
        if is_ours {
            remove_path(&entry.path())?;
        }
    }
    Ok(())
}

/// Purpose: The JSON answer of `GET /reverse/projects`: the current project and the whole rail.
///
/// `studied` is what lets the rail show which projects already hold notes, so the page can
/// warn before a redo and the user can see what a switch would bring back.
/// Serves: `Route::Projects`.
/// Design: `docs/design/reverse-ui.md` §"The project rail".
/// Up: `handle` only.
/// Down: `list_projects`, `current_project`, `studied`, `escape_json`.
/// Verify: `cargo test -p loong-reverse-ui project_`.
pub fn projects_json(root: &Path) -> String {
    let current = current_project(root).unwrap_or_default();
    let items: Vec<String> = list_projects(root)
        .iter()
        .map(|name| {
            format!(
                "{{\"name\":\"{}\",\"studied\":{}}}",
                escape_json(name),
                studied(root, name)
            )
        })
        .collect();
    format!(
        "{{\"current\":\"{}\",\"projects\":[{}]}}",
        escape_json(&current),
        items.join(",")
    )
}

/// Purpose: Move a project's notes into `.trash/` and clear its study-finished mark.
///
/// A rename, not a delete: the notes stay on disk under
/// `.trash/<project>-<stamp>/<project>/`, so one wrong click costs nothing
/// (`docs/design/reverse.md` §3). Evidence goes with them — a redo starts from nothing, and
/// the old capture is not evidence for the new study. The empty `study/<project>/` and its
/// `evidence/` are recreated, so the tree still shows where the new material will land.
/// Serves: `POST /reverse/study/redo`, which the page calls after the user confirms.
/// Design: `docs/design/reverse.md` §3.
/// Up: `handle`.
/// Down: `std::fs::rename`, `std::fs::create_dir_all`, `write_phase`.
/// Verify: `cargo test -p loong-reverse-ui redo_`.
pub fn redo_study(root: &Path, name: &str) -> Result<PathBuf, String> {
    valid_project_name(name)?;
    write_phase(&project_state(root, name), false)?;
    let study = project_study(root, name);
    let evidence = project_evidence(root, name);
    if !studied(root, name) {
        std::fs::create_dir_all(&evidence).map_err(|e| format!("建 {name} 的笔记目录失败：{e}"))?;
        return Ok(study);
    }
    let dest = root.join(TRASH_DIR).join(format!("{name}-{}", stamp()));
    std::fs::create_dir_all(&dest).map_err(|e| format!("建 .trash 失败：{e}"))?;
    let moved = dest.join(name);
    std::fs::rename(&study, &moved).map_err(|e| format!("挪走旧笔记失败：{e}"))?;
    std::fs::create_dir_all(&evidence).map_err(|e| format!("重建笔记目录失败：{e}"))?;
    Ok(moved)
}

/// Purpose: A timestamp for a trash directory name, in whole seconds since the epoch.
/// Contract: `crates/reverse/ui/src/lib.rs`.
fn stamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

/// Purpose: Read the target chosen in the page last time, if any.
///
/// The kind lives in `target-kind` beside it, inside `projects/<project>/` — the target
/// belongs to the project, so switching projects switches targets (`docs/design/reverse.md`
/// §3). A target recorded before that file existed has only `target`, so its kind is
/// inferred from the value (`TargetKind::infer`).
/// Serves: `GET /reverse/target`, which prefills the target bar.
/// Design: `docs/design/reverse.md` §4.1.
/// Up: `handle`.
/// Down: `std::fs::read_to_string` on `target` and `target-kind` in the project's directory.
/// Verify: `cargo test -p loong-reverse-ui target_`.
pub fn read_target(state: &Path) -> Option<Target> {
    let text = std::fs::read_to_string(state.join("target")).ok()?;
    let value = text.trim().to_string();
    if value.is_empty() {
        return None;
    }
    let recorded = std::fs::read_to_string(state.join("target-kind"))
        .ok()
        .and_then(|kind| TargetKind::parse(&kind));
    Some(Target {
        kind: recorded.unwrap_or_else(|| TargetKind::infer(&value)),
        value,
    })
}

/// Purpose: Validate a chosen target and record it so the page can prefill it next time.
///
/// Validation is per kind (reverse.md §4.1): a URL only has to be `http(s)://` — it is not on
/// this filesystem; a local target has to be an absolute path that exists, because the engine
/// opens files by absolute path only. Refusing here, in the page, shows the user why instead
/// of turning it into a failed model turn.
/// Serves: `POST /reverse/target`.
/// Design: `docs/design/reverse.md` §4.1.
/// Up: `handle`.
/// Down: `std::fs::metadata` and the `target` + `target-kind` writes in the project's directory.
/// Verify: `cargo test -p loong-reverse-ui target_`.
pub fn record_target(state: &Path, kind: TargetKind, chosen: &str) -> Result<Target, String> {
    let chosen = chosen.trim();
    if chosen.is_empty() {
        return Err("请先填写目标".to_string());
    }
    if kind == TargetKind::Url {
        if !(chosen.starts_with("http://") || chosen.starts_with("https://")) {
            return Err(format!(
                "{chosen} 不是网址：网站目标要以 http:// 或 https:// 开头"
            ));
        }
        std::fs::create_dir_all(state).map_err(|e| e.to_string())?;
        std::fs::write(state.join("target"), format!("{chosen}\n"))
            .map_err(|e| format!("记录目标失败：{e}"))?;
        std::fs::write(state.join("target-kind"), "url\n")
            .map_err(|e| format!("记录目标类型失败：{e}"))?;
        return Ok(Target {
            kind,
            value: chosen.to_string(),
        });
    }
    if chosen.starts_with("http://") || chosen.starts_with("https://") {
        return Err(format!(
            "{chosen} 是网址：请切到「网站」标签，网站要用 web_site_open 打开，不走文件入口"
        ));
    }
    let path = Path::new(chosen);
    if !path.is_absolute() {
        return Err(format!(
            "{chosen} 不是绝对路径：请用「浏览…」选，或粘贴完整路径"
        ));
    }
    let meta = std::fs::metadata(path).map_err(|e| format!("{chosen}：{e}"))?;
    if !meta.is_file() && !meta.is_dir() {
        return Err(format!("{chosen} 既不是文件也不是目录"));
    }
    std::fs::create_dir_all(state).map_err(|e| e.to_string())?;
    std::fs::write(state.join("target"), format!("{chosen}\n"))
        .map_err(|e| format!("记录目标失败：{e}"))?;
    std::fs::write(state.join("target-kind"), format!("{}\n", kind.id()))
        .map_err(|e| format!("记录目标类型失败：{e}"))?;
    Ok(Target {
        kind,
        value: chosen.to_string(),
    })
}

/// Purpose: Whether the user has marked the study step finished — the gate on the rebuild step.
///
/// The user decides, not the file tree: the model can land a half-written
/// `.reverse/study/<project>/00-overview.md`, and lighting rebuild up from that would start a
/// rebuild on a study that is not done (reverse.md §4.1.1). The marker is `phase` holding
/// `done` inside `projects/<project>/`; missing file or `todo` means not finished.
/// Serves: the workflow strip, which enables rebuild only when this is true.
/// Design: `docs/design/reverse.md` §4.1.1.
/// Up: `handle` for `GET /reverse/phase`.
/// Down: `std::fs::read_to_string` on the project's `phase`.
/// Verify: `cargo test -p loong-reverse-ui phase_`.
pub fn read_phase(state: &Path) -> bool {
    std::fs::read_to_string(state.join("phase"))
        .map(|text| text.trim() == "done")
        .unwrap_or(false)
}

/// Purpose: Record whether the study step is finished, so a page reload keeps the same gate.
///
/// Not browser localStorage: another tab, another browser or a restarted page must agree on
/// which step the workflow is at (reverse.md §4.1.1).
/// Serves: `POST /reverse/phase`.
/// Design: `docs/design/reverse.md` §4.1.1.
/// Up: `handle`.
/// Down: the `phase` write in the project's directory.
/// Verify: `cargo test -p loong-reverse-ui phase_`.
pub fn write_phase(state: &Path, done: bool) -> Result<bool, String> {
    std::fs::create_dir_all(state).map_err(|e| e.to_string())?;
    let word = if done { "done\n" } else { "todo\n" };
    std::fs::write(state.join("phase"), word).map_err(|e| format!("记录进度失败：{e}"))?;
    Ok(done)
}

/// Purpose: The JSON file tree of the current project — only its `study/` and `build/`,
/// sorted by path so a directory sits directly above its contents.
///
/// One project's files, not all of `.reverse/`: notes of other projects, `runs/` bookkeeping
/// (`phase`, `session`, manifests) and the trash are not study results and would bury the
/// three notes the page is about (`docs/design/reverse.md` §3). The two roots are emitted even
/// when empty, so the page can show where the next note lands. Paths stay relative to
/// `.reverse/`, the same base `/reverse/file/<path>` takes.
/// Serves: the `/reverse/tree` answer.
/// Design: `docs/design/reverse.md` §3 and `docs/design/reverse-ui.md` §"Routes the page calls".
/// Up: `handle` only.
/// Down: `walk`, `std::fs::read_dir`.
/// Verify: `cargo test -p loong-reverse-ui tree_`.
pub fn tree_json(root: &Path, project: Option<&str>) -> String {
    let mut entries: Vec<(String, bool)> = Vec::new();
    if let Some(name) = project {
        for kind in ["study", "build"] {
            let dir = root.join(kind).join(name);
            if !dir.is_dir() {
                continue;
            }
            entries.push((format!("{kind}/{name}"), true));
            walk(root, &dir, &mut entries);
        }
    }
    // Path order, not directories-first: a parent is a prefix of its children, so this keeps
    // each branch together in one block instead of listing every directory before every file.
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = String::from("[");
    for (i, (name, is_dir)) in entries.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"path\":\"{}\",\"dir\":{}}}",
            escape_json(name),
            is_dir
        ));
    }
    out.push(']');
    out
}

/// Purpose: Recursively collect relative paths under `root` into `out`.
/// Serves: `tree_json`, which needs every file and folder of `.reverse/` in one pass.
/// Design: `docs/design/reverse.md` §4.
/// Up: `tree_json`, and itself for each subdirectory.
/// Down: `std::fs::read_dir` only.
/// Verify: `cargo test -p loong-reverse-ui`.
fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, bool)>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        let is_dir = path.is_dir();
        out.push((rel.clone(), is_dir));
        if is_dir {
            walk(root, &path, out);
        }
    }
}

/// Purpose: Escape a string for embedding in JSON.
/// Contract: `crates/reverse/ui/src/lib.rs`.
fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

/// Purpose: The study page: project rail, target, workflow, tree and chat.
/// Serves: the `/reverse` route.
/// Design: `docs/design/reverse.md` §4 (the study tab of the portal).
/// Up: `handle` only.
/// Down: the portal's `/ws` socket for the chat pane.
/// Verify: `cargo test -p loong-reverse-ui page_`.
pub fn page_html() -> &'static str {
    include_str!("page.html")
}

/// Purpose: The chat's stop route: `POST` here asks the session to stop the turn it is running.
/// Contract: `crates/reverse/ui/src/lib.rs`; design `docs/design/reverse.md` §5.
const INTERRUPT_ROUTE: &str = "/reverse/interrupt";

/// Purpose: The study surface as a portal router: owns `/reverse`, answers from `.reverse/`.
/// Serves: `loong_web::serve_with`, which the reverse session installs.
/// Design: `docs/design/reverse.md` §4.
/// Up: `crates/cli/src/reverse.rs`'s portal.
/// Down: `handle`.
/// Verify: `cargo test -p loong-reverse-ui`.
pub struct StudySurface {
    /// The session's `.reverse/` directory: the projects, the tree and the files under it.
    root: PathBuf,
    /// This session's own run directory, `.reverse/runs/<run>`: only the run name, which
    /// belongs to this process. The target and the phase belong to the project and are read
    /// from `projects/<current>/` instead (`docs/design/reverse.md` §3).
    state: PathBuf,
    /// The chat's stop: a signal to the session's server child, handed in by the process that
    /// owns it, because the page cannot reach it any other way while a turn is streaming.
    interrupt: Arc<dyn Fn() + Send + Sync>,
}

impl StudySurface {
    /// Purpose: Mount the study surface on the session's `.reverse/` directory, its own run,
    /// and the stop that belongs to the chat beside it.
    /// Serves: `crates/cli/src/reverse.rs`.
    /// Design: `docs/design/reverse.md` §3 and §4.
    /// Up: `run_portal`.
    /// Down: nothing.
    /// Verify: `cargo test -p loong-reverse-ui`.
    pub fn new(
        reverse_dir: PathBuf,
        run_state: PathBuf,
        interrupt: Arc<dyn Fn() + Send + Sync>,
    ) -> Arc<dyn Router> {
        Arc::new(Self {
            root: reverse_dir,
            state: run_state,
            interrupt,
        })
    }
}

impl Router for StudySurface {
    /// Purpose: This surface owns `/reverse`.
    /// Contract: `crates/reverse/ui/src/lib.rs`.
    fn prefix(&self) -> &str {
        "/reverse"
    }
    /// Purpose: Answer the request from the `.reverse/` directory, or — for the chat's stop —
    /// by raising the signal the study tree knows nothing about.
    /// Contract: `crates/reverse/ui/src/lib.rs`.
    fn answer(&self, head: &str, stream: &mut TcpStream) -> Result<(), String> {
        if target_of(head) == INTERRUPT_ROUTE {
            return answer_interrupt(head, stream, self.interrupt.as_ref());
        }
        handle(&self.root, &self.state, head, stream).map(|_| ())
    }
}

/// Purpose: The path a request line names, taken as written (no query handling needed: the
/// token rides the URL fragment, which browsers never send).
/// Contract: `crates/reverse/ui/src/lib.rs`.
fn target_of(head: &str) -> &str {
    head.split_whitespace().nth(1).unwrap_or("/reverse")
}

/// Purpose: Answer `POST /reverse/interrupt`: ask the session to stop the turn it is running.
///
/// The stop is the surface's own, not `handle`'s: it is a signal to the server child, and the
/// page has no other way to reach that child while the turn it wants to stop is streaming on
/// the socket (`docs/design/reverse.md` §5).
/// Serves: `StudySurface::answer`.
/// Design: `docs/design/reverse.md` §5.
/// Up: `StudySurface::answer` only.
/// Down: the `interrupt` hook the process installed, `respond`.
/// Verify: `cargo test -p loong-reverse-ui interrupt_`.
fn answer_interrupt(
    head: &str,
    stream: &mut TcpStream,
    interrupt: &(dyn Fn() + Send + Sync),
) -> Result<(), String> {
    let method = head.split_whitespace().next().unwrap_or("GET");
    if !method.eq_ignore_ascii_case("POST") {
        return respond(
            stream,
            "405 Method Not Allowed",
            "text/plain; charset=utf-8",
            "只要 POST：打断是动作，不是可缓存的一张表".as_bytes(),
        );
    }
    interrupt();
    respond(
        stream,
        "200 OK",
        "application/json; charset=utf-8",
        br#"{"interrupted":true}"#,
    )
}

/// Purpose: Answer one reverse request on an accepted connection, or hand it back to the portal.
///
/// `head` is the request as read so far: the request line, the headers, and whatever of
/// the body arrived with them. `POST /reverse/target` carries a short body, so the rest is
/// read from the stream when `content-length` says more is coming.
/// Serves: the reverse session's accept loop.
/// Design: `docs/design/reverse.md` §4 and §4.1.
/// Up: `StudySurface::answer`.
/// Down: `route`, `tree_json`, `list_dir`, `record_target`, `read_target`, `run_json`,
/// `projects_json`, `create_project`, `select_project`, `redo_study`, `page_html`.
/// Verify: `cargo test -p loong-reverse-ui`.
pub fn handle(
    root: &Path,
    state: &Path,
    head: &str,
    stream: &mut TcpStream,
) -> Result<bool, String> {
    let request_line = head.lines().next().unwrap_or_default();
    let method = request_line.split_whitespace().next().unwrap_or("GET");
    let path = request_line.split_whitespace().nth(1).unwrap_or("/reverse");
    // The current project is read per request, not remembered: `POST /reverse/project` changes
    // it, and the answer to every route below depends on which one it is (reverse.md §3).
    let project = current_project(root);
    match route(root, path) {
        Route::Pass => Ok(false),
        Route::NotFound(reason) => {
            respond(
                stream,
                "404 Not Found",
                "text/plain; charset=utf-8",
                reason.as_bytes(),
            )?;
            Ok(true)
        }
        Route::Tree => {
            respond(
                stream,
                "200 OK",
                "application/json; charset=utf-8",
                tree_json(root, project.as_deref()).as_bytes(),
            )?;
            Ok(true)
        }
        Route::File(rel) => {
            let full = root.join(&rel);
            match std::fs::read(&full) {
                Ok(bytes) => {
                    respond(stream, "200 OK", content_type(&rel), &bytes)?;
                    Ok(true)
                }
                Err(e) => {
                    respond(
                        stream,
                        "404 Not Found",
                        "text/plain; charset=utf-8",
                        format!("{}: {e}", rel.display()).as_bytes(),
                    )?;
                    Ok(true)
                }
            }
        }
        Route::List(wanted) => {
            let dir = if wanted.is_empty() {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| root.to_path_buf())
            } else {
                PathBuf::from(wanted)
            };
            match list_dir(&dir) {
                Ok(listing) => {
                    respond(
                        stream,
                        "200 OK",
                        "application/json; charset=utf-8",
                        listing_json(&listing).as_bytes(),
                    )?;
                }
                Err(reason) => {
                    respond(
                        stream,
                        "400 Bad Request",
                        "text/plain; charset=utf-8",
                        reason.as_bytes(),
                    )?;
                }
            }
            Ok(true)
        }
        Route::Target => {
            if method.eq_ignore_ascii_case("POST") {
                let body = read_body(head, stream)?;
                let (kind, chosen) = parse_target_body(&body);
                let Some(kind) = kind else {
                    respond(
                        stream,
                        "400 Bad Request",
                        "text/plain; charset=utf-8",
                        "目标类型只能是 binary、apk 或 url".as_bytes(),
                    )?;
                    return Ok(true);
                };
                let Some(pstate) = project_state_or_refuse(root, stream)? else {
                    return Ok(true);
                };
                match record_target(&pstate, kind, &chosen) {
                    Ok(target) => {
                        respond(
                            stream,
                            "200 OK",
                            "text/plain; charset=utf-8",
                            target_body(&target).as_bytes(),
                        )?;
                    }
                    Err(reason) => {
                        respond(
                            stream,
                            "400 Bad Request",
                            "text/plain; charset=utf-8",
                            reason.as_bytes(),
                        )?;
                    }
                }
            } else {
                let current = project
                    .as_deref()
                    .and_then(|name| read_target(&project_state(root, name)))
                    .map(|t| target_body(&t))
                    .unwrap_or_default();
                respond(
                    stream,
                    "200 OK",
                    "text/plain; charset=utf-8",
                    current.as_bytes(),
                )?;
            }
            Ok(true)
        }
        Route::Phase => {
            if method.eq_ignore_ascii_case("POST") {
                let body = read_body(head, stream)?.trim().to_string();
                let done = matches!(body.as_str(), "done" | "true" | "1");
                let Some(pstate) = project_state_or_refuse(root, stream)? else {
                    return Ok(true);
                };
                match write_phase(&pstate, done) {
                    Ok(done) => {
                        let word = if done { "done\n" } else { "todo\n" };
                        respond(
                            stream,
                            "200 OK",
                            "text/plain; charset=utf-8",
                            word.as_bytes(),
                        )?;
                    }
                    Err(reason) => {
                        respond(
                            stream,
                            "400 Bad Request",
                            "text/plain; charset=utf-8",
                            reason.as_bytes(),
                        )?;
                    }
                }
            } else {
                let finished = project
                    .as_deref()
                    .map(|name| read_phase(&project_state(root, name)))
                    .unwrap_or(false);
                let word = if finished { "done\n" } else { "todo\n" };
                respond(
                    stream,
                    "200 OK",
                    "text/plain; charset=utf-8",
                    word.as_bytes(),
                )?;
            }
            Ok(true)
        }
        Route::Run => {
            respond(
                stream,
                "200 OK",
                "application/json; charset=utf-8",
                run_json(root, state).as_bytes(),
            )?;
            Ok(true)
        }
        Route::Projects => {
            if method.eq_ignore_ascii_case("POST") {
                let body = read_body(head, stream)?;
                let (verb, name) = body.split_once(':').unwrap_or((&body, ""));
                let result = match verb.trim() {
                    "create" => create_project(root, name.trim()),
                    "select" => select_project(root, name.trim()),
                    "delete" => delete_project(root, name.trim()),
                    other => Err(format!(
                        "未知操作 {other}：只有 create:<项目名>、select:<项目名> 和 delete:<项目名>"
                    )),
                };
                match result {
                    Ok(()) => {
                        let now = current_project(root).unwrap_or_default();
                        respond(
                            stream,
                            "200 OK",
                            "text/plain; charset=utf-8",
                            format!("{now}\n").as_bytes(),
                        )?;
                    }
                    Err(reason) => {
                        respond(
                            stream,
                            "400 Bad Request",
                            "text/plain; charset=utf-8",
                            reason.as_bytes(),
                        )?;
                    }
                }
            } else {
                respond(
                    stream,
                    "200 OK",
                    "application/json; charset=utf-8",
                    projects_json(root).as_bytes(),
                )?;
            }
            Ok(true)
        }
        Route::Redo => {
            let Some(name) = project else {
                respond(
                    stream,
                    "409 Conflict",
                    "text/plain; charset=utf-8",
                    "还没有项目：请在左侧新建一个项目".as_bytes(),
                )?;
                return Ok(true);
            };
            match redo_study(root, &name) {
                Ok(moved) => {
                    respond(
                        stream,
                        "200 OK",
                        "text/plain; charset=utf-8",
                        format!("旧的笔记已挪到 {}\n", moved.display()).as_bytes(),
                    )?;
                }
                Err(reason) => {
                    respond(
                        stream,
                        "400 Bad Request",
                        "text/plain; charset=utf-8",
                        reason.as_bytes(),
                    )?;
                }
            }
            Ok(true)
        }
        Route::Session => {
            // Both halves name the conversation the page is on, so the record and the page
            // cannot drift: the page asks what is recorded, then says what it switched to.
            if method.eq_ignore_ascii_case("POST") {
                let id = read_body(head, stream)?.trim().to_string();
                let Some(pstate) = project_state_or_refuse(root, stream)? else {
                    return Ok(true);
                };
                match record_session(&pstate, &id).and_then(|()| record_run_session(state, &id)) {
                    Ok(()) => respond(
                        stream,
                        "200 OK",
                        "text/plain; charset=utf-8",
                        format!("{id}\n").as_bytes(),
                    )?,
                    Err(reason) => respond(
                        stream,
                        "400 Bad Request",
                        "text/plain; charset=utf-8",
                        reason.as_bytes(),
                    )?,
                }
            } else {
                let recorded = project
                    .as_deref()
                    .and_then(|name| read_recorded_session(&project_state(root, name)))
                    .unwrap_or_default();
                respond(
                    stream,
                    "200 OK",
                    "text/plain; charset=utf-8",
                    format!("{recorded}\n").as_bytes(),
                )?;
            }
            Ok(true)
        }
        Route::Page => {
            respond(
                stream,
                "200 OK",
                "text/html; charset=utf-8",
                page_html().as_bytes(),
            )?;
            Ok(true)
        }
    }
}

/// Purpose: The current project's state directory, or a 409 to the page when there is none.
///
/// Reached only if `.reverse/project` names a project whose directory is gone: the CLI always
/// creates one at startup. Answering 409 rather than writing into a stub keeps the mistake
/// visible instead of quietly scattering state.
/// Serves: the `Target` and `Phase` arms of `handle`.
/// Design: `docs/design/reverse.md` §3.
/// Up: `handle` only.
/// Down: `current_project`, `project_state`, `respond`.
/// Verify: `cargo test -p loong-reverse-ui`.
fn project_state_or_refuse(root: &Path, stream: &mut TcpStream) -> Result<Option<PathBuf>, String> {
    match current_project(root) {
        Some(name) => Ok(Some(project_state(root, &name))),
        None => {
            respond(
                stream,
                "409 Conflict",
                "text/plain; charset=utf-8",
                "还没有项目：请在左侧新建一个项目".as_bytes(),
            )?;
            Ok(None)
        }
    }
}

/// Purpose: The JSON answer of `GET /reverse/run`: this session's run name, the current project
/// and where its files go.
///
/// The page cannot guess the run name — a project accumulates `run-001`, `run-002`, … and only
/// the running session knows which one it is (reverse.md §3) — and it must not guess the
/// project either, since `POST /reverse/project` can change it. `studied` is what the page asks
/// about before pressing 「开始研究」 a second time.
/// Serves: `Route::Run`, which the page fetches at load and again before it injects a prompt.
/// Design: `docs/design/reverse.md` §3 and `docs/design/reverse-ui.md` §"Routes the page calls".
/// Up: `handle` only.
/// Down: `std::path::Path::file_name`, `current_project`, `studied`.
/// Verify: `cargo test -p loong-reverse-ui run_`.
pub fn run_json(root: &Path, state: &Path) -> String {
    let run = state
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let project = current_project(root).unwrap_or_default();
    let has_notes = !project.is_empty() && studied(root, &project);
    // Notes and rebuild output are named after the project, not after the run: the user asked
    // for `study/<project>/` so the folder says which project it belongs to.
    format!(
        "{{\"name\":\"{}\",\"project\":\"{}\",\"study\":\"study/{}\",\"build\":\"build/{}\",\"studied\":{}}}",
        escape_json(&run),
        escape_json(&project),
        escape_json(&project),
        escape_json(&project),
        has_notes
    )
}

/// Purpose: Write one HTTP response onto the accepted connection.
/// Contract: `crates/reverse/ui/src/lib.rs`.
fn respond(stream: &mut TcpStream, status: &str, kind: &str, body: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let head = format!(
        "HTTP/1.1 {status}\r\ncontent-length: {}\r\ncontent-type: {kind}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.write_all(body))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Purpose: A fresh temporary `.reverse/` for one test.
    /// Contract: `crates/reverse/ui/src/lib.rs` test fixtures.
    fn temp_reverse() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "loong-reverse-ui-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("a test .reverse/");
        dir
    }

    #[test]
    /// Purpose: The route table maps the four paths and passes everything else to the portal.
    fn route_maps_paths_and_passes_the_rest() {
        let root = temp_reverse();
        assert_eq!(route(&root, "/reverse"), Route::Page);
        assert_eq!(route(&root, "/reverse/tree"), Route::Tree);
        assert_eq!(
            route(&root, "/reverse/file/study/00-overview.md"),
            Route::File(PathBuf::from("study/00-overview.md"))
        );
        assert_eq!(
            route(&root, "/ws"),
            Route::Pass,
            "the socket is the portal's"
        );
        assert_eq!(
            route(&root, "/"),
            Route::Pass,
            "the chat page is the portal's"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    /// Purpose: A `..` in the file path never escapes `.reverse/`.
    fn route_refuses_paths_outside_the_reverse_dir() {
        let root = temp_reverse();
        for bad in ["/reverse/file/../secret", "/reverse/file/../../etc/passwd"] {
            match route(&root, bad) {
                Route::NotFound(_) => {}
                other => panic!("{bad} must not be served: {other:?}"),
            }
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    /// Purpose: The tree is the current project's two roots, before files, and nothing else.
    fn tree_lists_the_current_project_only() {
        let root = temp_reverse();
        create_project(&root, "alpha").unwrap();
        std::fs::create_dir_all(root.join("build").join("alpha").join("src")).unwrap();
        std::fs::write(
            root.join("study").join("alpha").join("00-overview.md"),
            "# notes\n",
        )
        .unwrap();
        create_project(&root, "beta").unwrap();
        std::fs::write(
            root.join("study").join("beta").join("00-overview.md"),
            "# other\n",
        )
        .unwrap();
        let json = tree_json(&root, Some("alpha"));
        for needle in [
            "\"path\":\"study/alpha\",\"dir\":true",
            "\"path\":\"study/alpha/00-overview.md\",\"dir\":false",
            "\"path\":\"build/alpha/src\",\"dir\":true",
        ] {
            assert!(json.contains(needle), "the tree carries {needle}: {json}");
        }
        assert!(!json.contains("beta"), "another project stays out: {json}");
        assert!(!json.contains("runs"), "the bookkeeping stays out: {json}");
        assert_eq!(tree_json(&root, None), "[]", "no project, no tree");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    /// Purpose: The page carries the project rail, the target tabs, the two workflow steps and
    /// the panes, and opens a note in an overlay instead of a preview column.
    fn page_has_the_rail_the_target_tabs_and_the_workflow() {
        let html = page_html();
        for needle in [
            "id=\"projects\"",
            "id=\"tree\"",
            "id=\"preview-modal\"",
            "id=\"chat\"",
            "favicon.png",
            "id=\"target\"",
            "id=\"picker\"",
            "id=\"workflow\"",
            // Two target kinds, because only two entrances exist; an APK is a local target (§4.1).
            "data-kind=\"local\"",
            "data-kind=\"url\"",
            // Two steps, the second gated by the user's mark (§4.1.1).
            "id=\"btn-study\"",
            "id=\"btn-done\"",
            "id=\"btn-build\"",
            // The header's model picker lists the session's models and can switch (§4.2).
            "id=\"model\"",
            "model.list",
            "model.select",
            // The page speaks Chinese; the prompts it injects stay English.
            "选择目标",
            "开始研究",
            "标记研究完成",
            "web_site_open",
            "core_target_open",
            // Deleting a project is destructive, so the page asks for the name first (§"The
            // project rail") — the verb and the guard both live in the page.
            "delete:",
            "prow",
        ] {
            assert!(html.contains(needle), "the page carries {needle}");
        }
        // The chat pane rides the portal's socket: the same /ws the chat page uses.
        assert!(
            html.contains("/ws"),
            "the chat pane opens the portal socket"
        );
        // And every data route goes through the token header (§4.2).
        assert!(
            html.contains("x-loong-token"),
            "the page presents the token on its fetches"
        );
    }

    #[test]
    /// Purpose: The page shows what a running turn is doing, and locks the buttons while it runs:
    /// a click greys the button, a spinner turns, deltas stream, tool calls and their results
    /// appear on one line each, and an approval parks the turn until the person answers.
    fn page_shows_progress_and_locks_the_buttons_while_a_turn_runs() {
        let html = page_html();
        // The progress line, its spinner, and the streaming caret.
        for needle in [
            "id=\"progress\"",
            "id=\"progress-text\"",
            "id=\"progress-spin\"",
            "@keyframes spin",
            "prefers-reduced-motion",
            "live::after",
            // The three streams that make up the progress: model text, tool calls, results.
            "model.delta",
            "tool.call",
            "tool.result",
            // A parked turn must be answerable here, or it looks like a hang.
            "approval.requested",
            "approval.respond",
            // And answered once: the pair of buttons turns into the decision itself,
            // so a card that is done never looks pressable again — not even when the
            // same request was answered in the chat tab or the TUI.
            "approval.decided",
            "已允许（只这一次）",
            "已拒绝",
            "settleApproval",
            "button:disabled",
            // What the user sees while it runs.
            "正在研究…",
            "正在调用 ",
            "停在这里等你批准",
        ] {
            assert!(html.contains(needle), "the page carries {needle}");
        }
        // The gate closes on `working`, not on the step alone: both buttons go grey.
        assert!(
            html.contains("btnStudy.disabled = blocked") && html.contains("btnBuild.disabled ="),
            "the buttons are disabled while a turn runs"
        );
        assert!(
            html.contains("btnStudy.textContent ="),
            "the study button says what it is doing"
        );
    }

    #[test]
    /// Purpose: The conversation section is the project's own: it replays the session it
    /// attaches to, shows the usage row under the input, and puts one button there that sends
    /// when idle and stops when a turn runs.
    fn page_replays_the_session_and_offers_send_or_stop() {
        let html = page_html();
        for needle in [
            // A refresh or a restart shows the same history: the session is replayed from its
            // first event, and only then does the page say it is connected.
            "session.attach",
            "session.replay",
            "重放了 ",
            // Usage, the same reading as the TUI's bottom bar.
            "id=\"metrics\"",
            "cost.recorded",
            "缓存命中 ",
            "次",
            // One button in two roles: send when idle, stop while a turn runs.
            "id=\"btn-send\"",
            "/reverse/interrupt",
            "btnSend.textContent = busy ? \"停止\" : \"发送\"",
            "正在停下来…",
            // A project holds several conversations, and the header lists them: pick one to
            // continue it, or start a fresh one, both without leaving the page.
            "id=\"session-picker\"",
            "id=\"btn-new-session\"",
            "session.open",
            "session.list",
            "＋ 新会话",
            "/reverse/session",
            // The picker is this project's own list: the server it talks to works in the
            // project's drawer, so the workspace's TUI conversations cannot appear in it —
            // and the page needs no filter of its own to keep them out.
            "这个项目的会话",
        ] {
            assert!(html.contains(needle), "the page carries {needle}");
        }
        // A page that reached for the workspace's directory would bring those conversations
        // back; the drawer is the only list this page may show.
        assert!(
            !html.contains("/reverse/sessions"),
            "the page lists the project's drawer, not a workspace-wide index"
        );
        // A replay lays rows out but must not drive the progress line or the buttons.
        assert!(
            html.contains("renderEvent(envelope.kind, envelope.data || {}, true)"),
            "the replay is rendered quietly"
        );
        // Switching is refused while a turn runs, rather than let through and faked: the
        // streamed events would land on a session the page is no longer showing.
        assert!(
            html.contains("sessionPicker.disabled = busy || switching"),
            "the picker is locked while a turn runs or a switch is in flight"
        );
    }

    #[test]
    /// Purpose: The directory browser lists a real directory: names, kinds, and a way up.
    fn ls_lists_a_directory_without_reading_files() {
        let root = temp_reverse();
        let dir = root.join("tree");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("app.elf"), "not read by ls").unwrap();

        let answer = list_dir(&dir).expect("a directory lists");
        assert_eq!(answer.path, dir.display().to_string());
        assert_eq!(
            answer.parent.as_deref(),
            Some(root.display().to_string().as_str()),
            "the parent is one level up"
        );
        let names: Vec<(&str, bool)> = answer
            .entries
            .iter()
            .map(|e| (e.name.as_str(), e.dir))
            .collect();
        assert_eq!(
            names,
            vec![("sub", true), ("app.elf", false)],
            "directories first, then files, each sorted"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    /// Purpose: `ls` reports a missing path and a file path as errors, not as empty listings.
    fn ls_refuses_what_it_cannot_list() {
        let root = temp_reverse();
        std::fs::write(root.join("a.bin"), b"x").unwrap();
        assert!(
            list_dir(&root.join("a.bin")).is_err(),
            "a file is not a directory"
        );
        assert!(
            list_dir(&root.join("nope")).is_err(),
            "a missing path is an error"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    /// Purpose: A chosen target is validated per kind, recorded, and read back with its kind.
    fn target_is_recorded_validated_and_read_back() {
        let root = temp_reverse();
        let bin = root.join("app.elf");
        std::fs::write(&bin, b"\x7fELF").unwrap();

        // Nothing chosen yet.
        assert_eq!(read_target(&root), None);

        // A real file is accepted and remembered, with its kind.
        let recorded = record_target(&root, TargetKind::Local, &bin.display().to_string())
            .expect("a file is a target");
        assert_eq!(recorded.value, bin.display().to_string());
        assert_eq!(recorded.kind, TargetKind::Local);
        assert_eq!(
            read_target(&root),
            Some(Target {
                kind: TargetKind::Local,
                value: bin.display().to_string()
            })
        );

        // A directory is a target too (the engine opens folders).
        let recorded = record_target(&root, TargetKind::Local, &root.display().to_string())
            .expect("a folder is a target");
        assert_eq!(recorded.value, root.display().to_string());

        // A path that does not exist is refused, with a reason.
        let refused = record_target(&root, TargetKind::Local, "/nonexistent/oops")
            .expect_err("a missing path");
        assert!(refused.contains("/nonexistent/oops"), "{refused}");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    /// Purpose: A website target is accepted as a URL — it opens through `web_site_open`, not the file entrance.
    fn target_accepts_a_url_only_in_the_url_tab() {
        let root = temp_reverse();

        // The URL tab takes a URL and remembers that it was the URL tab.
        let recorded = record_target(&root, TargetKind::Url, "https://example.com/a?b=1")
            .expect("a URL is a website target");
        assert_eq!(recorded.kind, TargetKind::Url);
        assert_eq!(
            read_target(&root),
            Some(Target {
                kind: TargetKind::Url,
                value: "https://example.com/a?b=1".to_string()
            })
        );

        // The 本地目标 tab refuses a URL: that entrance would fail at the first tool call.
        let refused = record_target(&root, TargetKind::Local, "https://example.com")
            .expect_err("a URL is not a local target");
        assert!(refused.contains("网站"), "{refused}");

        // The URL tab refuses a bare path: `web_site_open` needs a scheme.
        let refused =
            record_target(&root, TargetKind::Url, "/bin/true").expect_err("a path is not a URL");
        assert!(refused.contains("http"), "{refused}");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    /// Purpose: A target recorded before kinds existed still reads back, with its kind inferred.
    fn target_infers_the_kind_of_a_legacy_target_file() {
        let root = temp_reverse();
        std::fs::write(root.join("target"), "/home/you/app.apk\n").unwrap();
        assert_eq!(
            read_target(&root),
            Some(Target {
                kind: TargetKind::Local,
                value: "/home/you/app.apk".to_string()
            }),
            "an .apk path with no recorded kind is a local target; triage names the real kind"
        );
        std::fs::write(root.join("target"), "https://example.com\n").unwrap();
        assert_eq!(
            read_target(&root).map(|t| t.kind),
            Some(TargetKind::Url),
            "a URL with no recorded kind infers Url"
        );
        // The words older builds wrote on line one of `target-kind` are all the local entrance.
        for legacy in ["binary", "apk"] {
            std::fs::write(root.join("target-kind"), legacy).unwrap();
            assert_eq!(
                read_target(&root).map(|t| t.kind),
                Some(TargetKind::Local),
                "`{legacy}` was a tab of its own; both open through core_target_open"
            );
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    /// Purpose: The study-done marker is what unlocks rebuild, and it round-trips through the file.
    fn phase_marks_study_done_and_undoes_it() {
        let root = temp_reverse();
        assert!(!read_phase(&root), "a fresh session has not finished study");
        assert!(write_phase(&root, true).unwrap());
        assert!(read_phase(&root), "the marker survives a page reload");
        write_phase(&root, false).unwrap();
        assert!(
            !read_phase(&root),
            "starting the study again clears the marker"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    /// Purpose: A relative path is refused before it becomes a failed model turn.
    fn target_refuses_a_relative_path() {
        let root = temp_reverse();
        let refused =
            record_target(&root, TargetKind::Local, "app.elf").expect_err("a relative path");
        assert!(refused.contains("绝对路径"), "{refused}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    /// Purpose: `ls`, `target`, `phase` and the tree are decided by the router, page routes stay the page.
    fn route_maps_the_data_endpoints() {
        let root = temp_reverse();
        assert_eq!(route(&root, "/reverse/projects"), Route::Projects);
        assert_eq!(route(&root, "/reverse/study/redo"), Route::Redo);
        assert_eq!(
            route(&root, "/reverse/ls?path=/tmp"),
            Route::List("/tmp".to_string())
        );
        assert_eq!(route(&root, "/reverse/ls"), Route::List(String::new()));
        assert_eq!(route(&root, "/reverse/target"), Route::Target);
        assert_eq!(route(&root, "/reverse/phase"), Route::Phase);
        assert_eq!(route(&root, "/reverse/run"), Route::Run);
        assert_eq!(route(&root, "/reverse/session"), Route::Session);
        assert!(matches!(route(&root, "/reverse/nope"), Route::NotFound(_)));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    /// Purpose: The session record round-trips, and a path dressed up as an id is refused.
    fn session_record_reads_back_what_the_page_wrote() {
        let root = temp_reverse();
        create_project(&root, "project-001").unwrap();
        let state = project_state(&root, "project-001");
        assert_eq!(
            read_recorded_session(&state),
            None,
            "a fresh project records none"
        );
        record_session(&state, "20261019-120000-1").unwrap();
        assert_eq!(
            read_recorded_session(&state).as_deref(),
            Some("20261019-120000-1"),
            "the next start reads back the id the page recorded"
        );
        let refused = record_session(&state, "../../etc/passwd").expect_err("a path, not an id");
        assert!(refused.contains("不是会话 id"), "{refused}");
        assert_eq!(
            read_recorded_session(&state).as_deref(),
            Some("20261019-120000-1"),
            "a refused write leaves the record alone"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    /// Purpose: The id gate refuses paths and accepts ids, so a picker cannot read outside
    /// the session directory.
    fn session_ids_are_ids_not_paths() {
        assert!(valid_session_id("20261019-120000-1"));
        assert!(valid_session_id("latest"));
        for bad in ["", "a/b", "a\\b", "..", "x/../y", "x.jsonl", "a b", "a\tb"] {
            assert!(!valid_session_id(bad), "{bad:?} must be refused");
        }
    }

    #[test]
    /// Purpose: The run route names this process's run and the project whose folders the prompts use.
    fn run_answers_the_run_and_the_project() {
        let root = temp_reverse();
        let state = root.join("runs").join("run-002");
        std::fs::create_dir_all(&state).unwrap();
        create_project(&root, "my app").unwrap();
        assert_eq!(
            run_json(&root, &state),
            concat!(
                "{\"name\":\"run-002\",\"project\":\"my app\",",
                "\"study\":\"study/my app\",\"build\":\"build/my app\",\"studied\":false}"
            ),
            "the page gets the run, the project and that project's two folders"
        );
        std::fs::write(
            root.join("study").join("my app").join("00-overview.md"),
            "x",
        )
        .unwrap();
        assert!(
            run_json(&root, &state).contains("\"studied\":true"),
            "a note on disk is what makes the page ask before redoing"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    /// Purpose: The target and the phase belong to the project, so switching projects switches both.
    fn target_and_phase_belong_to_the_project() {
        let root = temp_reverse();
        let here = std::env::temp_dir().to_string_lossy().to_string();
        create_project(&root, "alpha").unwrap();
        let alpha = project_state(&root, "alpha");
        record_target(&alpha, TargetKind::Local, &here).unwrap();
        write_phase(&alpha, true).unwrap();

        create_project(&root, "beta").unwrap();
        let beta = project_state(&root, "beta");
        assert_eq!(read_target(&beta), None, "a new project inherits no target");
        assert!(!read_phase(&beta), "and no finished mark");

        select_project(&root, "alpha").unwrap();
        assert_eq!(current_project(&root).as_deref(), Some("alpha"));
        assert_eq!(
            read_target(&alpha).map(|t| t.value),
            Some(here),
            "the project that chose a target reads it back"
        );
        assert!(read_phase(&alpha), "and keeps its own finished mark");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    /// Purpose: A project name is a directory name: dots, slashes, `..` and long names are refused.
    fn project_name_must_stay_inside_the_projects_dir() {
        for good in ["project-001", "我的项目", "my app", "a_b-c.1"] {
            assert!(valid_project_name(good).is_ok(), "{good} is a fine name");
        }
        for bad in [
            "",
            "   ",
            ".hidden",
            "a/b",
            "a\\b",
            "..",
            "a..b",
            "tab\there",
        ] {
            assert!(valid_project_name(bad).is_err(), "{bad:?} must be refused");
        }
        let long = "x".repeat(65);
        assert!(
            valid_project_name(&long).is_err(),
            "65 characters is too long"
        );
    }

    #[test]
    /// Purpose: Creating a project makes its three directories, records it, and refuses a duplicate;
    /// selecting one refuses a name that does not exist.
    fn create_and_select_project() {
        let root = temp_reverse();
        assert_eq!(
            current_project(&root),
            None,
            "an empty .reverse/ has no project"
        );
        create_project(&root, "alpha").unwrap();
        assert_eq!(current_project(&root).as_deref(), Some("alpha"));
        for dir in [
            project_state(&root, "alpha"),
            root.join("study").join("alpha"),
            root.join("build").join("alpha"),
        ] {
            assert!(dir.is_dir(), "{} exists", dir.display());
        }
        assert!(create_project(&root, "alpha").is_err(), "no silent reuse");
        assert!(
            select_project(&root, "nope").is_err(),
            "selecting a missing project is refused"
        );
        create_project(&root, "beta").unwrap();
        assert_eq!(list_projects(&root), vec!["alpha", "beta"]);
        select_project(&root, "alpha").unwrap();
        assert_eq!(current_project(&root).as_deref(), Some("alpha"));
        std::fs::write(root.join("study").join("alpha").join("00-overview.md"), "x").unwrap();
        let json = projects_json(&root);
        assert!(
            json.contains("{\"current\":\"alpha\",\"projects\":["),
            "the rail names the current project: {json}"
        );
        assert!(
            json.contains("{\"name\":\"alpha\",\"studied\":true}"),
            "and marks the project that holds notes: {json}"
        );
        assert!(
            json.contains("{\"name\":\"beta\",\"studied\":false}"),
            "and the empty one: {json}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    /// Purpose: Evidence alone is not a study: a project that only unpacked or captured
    /// something is still "not written up", and a redo takes its evidence along.
    fn evidence_does_not_count_as_a_note() {
        let root = temp_reverse();
        create_project(&root, "alpha").unwrap();
        let evidence = root.join("study").join("alpha").join(EVIDENCE_DIR);
        assert!(
            evidence.is_dir(),
            "a new project already has somewhere to put raw material"
        );
        std::fs::write(evidence.join("dex_strings.txt"), "com.example\n").unwrap();
        assert!(
            !studied(&root, "alpha"),
            "raw material is not a note, so the page must not offer a redo"
        );

        std::fs::write(
            root.join("study").join("alpha").join("00-overview.md"),
            "# it\n",
        )
        .unwrap();
        assert!(studied(&root, "alpha"), "a note makes it studied");

        let moved = redo_study(&root, "alpha").unwrap();
        assert!(
            moved.join("00-overview.md").is_file()
                && moved.join(EVIDENCE_DIR).join("dex_strings.txt").is_file(),
            "notes and evidence move to the trash together"
        );
        assert!(
            evidence.is_dir(),
            "and an empty evidence directory is recreated for the new study"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    /// Purpose: Redoing a study moves the old notes into the trash instead of deleting them, and
    /// clears the finished mark; a project with no notes is left alone.
    fn redo_moves_the_old_notes_to_the_trash() {
        let root = temp_reverse();
        create_project(&root, "alpha").unwrap();
        write_phase(&project_state(&root, "alpha"), true).unwrap();
        let notes = root.join("study").join("alpha").join("00-overview.md");
        std::fs::write(&notes, "# first try\n").unwrap();
        assert!(studied(&root, "alpha"));

        let moved = redo_study(&root, "alpha").unwrap();
        assert!(
            moved.starts_with(root.join(TRASH_DIR)),
            "{}",
            moved.display()
        );
        assert!(
            moved.join("00-overview.md").is_file(),
            "the notes are still there"
        );
        assert!(
            !studied(&root, "alpha"),
            "and the study directory is empty again"
        );
        assert!(
            root.join("study").join("alpha").is_dir(),
            "the directory itself is kept, so the tree still shows where notes land"
        );
        assert!(
            !read_phase(&project_state(&root, "alpha")),
            "a redo means the study is unsettled again"
        );

        let again = redo_study(&root, "alpha").unwrap();
        assert_eq!(
            again,
            root.join("study").join("alpha"),
            "nothing to move, so nothing is created in the trash"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    /// Purpose: Deleting a project takes its notes, its raw material and its rebuild off the
    /// disk, carries its own trash entries with it, and hands the session to what is left.
    fn delete_takes_the_project_off_the_disk() {
        let root = temp_reverse();
        create_project(&root, "alpha").unwrap();
        create_project(&root, "beta").unwrap();
        std::fs::write(
            root.join("study").join("alpha").join("00-overview.md"),
            "# alpha\n",
        )
        .unwrap();
        std::fs::write(
            root.join("study")
                .join("alpha")
                .join(EVIDENCE_DIR)
                .join("dex_strings.txt"),
            "com.example\n",
        )
        .unwrap();
        std::fs::write(
            root.join("build").join("alpha").join("main.rs"),
            "fn main() {}\n",
        )
        .unwrap();
        let own_trash = root.join(TRASH_DIR).join("alpha-1700000000");
        std::fs::create_dir_all(own_trash.join("alpha")).unwrap();
        // A lookalike project's trash: `alpha-2-<stamp>` is not alpha's, and must survive.
        let others_trash = root.join(TRASH_DIR).join("alpha-2-1700000000");
        std::fs::create_dir_all(&others_trash).unwrap();

        delete_project(&root, "alpha").unwrap();

        assert!(
            !root.join("study").join("alpha").exists(),
            "the notes and their evidence are gone"
        );
        assert!(
            !root.join("build").join("alpha").exists(),
            "the rebuild is gone"
        );
        assert!(
            !project_state(&root, "alpha").exists(),
            "the project's state directory is gone"
        );
        assert!(!own_trash.exists(), "its old notes in the trash go too");
        assert!(
            others_trash.is_dir(),
            "another project's trash is untouched, even with a name that starts the same"
        );
        assert!(
            project_state(&root, "beta").is_dir(),
            "the other project is untouched"
        );
        assert_eq!(
            current_project(&root).as_deref(),
            Some("beta"),
            "deleting the current project hands the session to the next one"
        );

        delete_project(&root, "beta").unwrap();
        assert_eq!(
            current_project(&root),
            None,
            "the last project leaving means no current project"
        );
        assert!(
            !root.join(PROJECT_FILE).is_file(),
            "and the record of it is removed, not left pointing at nothing"
        );
        assert!(
            delete_project(&root, "alpha").is_err(),
            "deleting a project that is not there is an error, not a silent success"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    /// Purpose: A real HTTP exchange over a socket answers with the routed body.
    fn handle_answers_over_a_socket() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let root = temp_reverse();
        std::fs::create_dir_all(root.join("study")).unwrap();
        std::fs::write(root.join("study").join("n.md"), "hello study").unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let root2 = root.clone();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            // Read the request line.
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let handled = handle(&root2, &root2, &request, &mut stream).unwrap();
            assert!(handled, "a reverse route is handled");
        });

        let mut client = TcpStream::connect(addr).unwrap();
        client
            .write_all(b"GET /reverse/file/study/n.md HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        let mut body = String::new();
        client.read_to_string(&mut body).unwrap();
        assert!(body.starts_with("HTTP/1.1 200 OK"), "{body}");
        assert!(body.ends_with("hello study"), "{body}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    /// Purpose: The chat's stop: a `POST` raises the hook the process installed, a `GET` is
    /// refused and raises nothing.
    fn interrupt_route_raises_the_hook_only_for_post() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let root = temp_reverse();
        let stopped = Arc::new(AtomicUsize::new(0));

        // One connection per request, so the hook and the counter are fresh per exchange.
        let exchange = |method: &str| -> String {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let counter = stopped.clone();
            let router = StudySurface::new(
                root.clone(),
                root.join("runs").join("run-001"),
                Arc::new(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                }),
            );
            std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap();
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                router.answer(&request, &mut stream).unwrap();
            });
            let mut client = TcpStream::connect(addr).unwrap();
            client
                .write_all(
                    format!("{method} /reverse/interrupt HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
                        .as_bytes(),
                )
                .unwrap();
            let mut body = String::new();
            client.read_to_string(&mut body).unwrap();
            body
        };

        let posted = exchange("POST");
        assert!(posted.starts_with("HTTP/1.1 200 OK"), "{posted}");
        assert!(posted.contains(r#""interrupted":true"#), "{posted}");
        assert_eq!(stopped.load(Ordering::SeqCst), 1, "the stop was raised");

        let got = exchange("GET");
        assert!(got.starts_with("HTTP/1.1 405"), "{got}");
        assert_eq!(
            stopped.load(Ordering::SeqCst),
            1,
            "a GET raises nothing more"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    /// Purpose: The project routes work over a socket: the rail answers, `create:` records a
    /// project, a second `create:` of the same name is refused, and a redo moves the notes.
    fn project_routes_answer_over_a_socket() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let root = temp_reverse();

        // One connection per request: the handler answers one and returns.
        let exchange = |request: String| -> String {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let root = root.clone();
            std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap();
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                handle(&root, &root, &head, &mut stream).unwrap();
            });
            let mut client = TcpStream::connect(addr).unwrap();
            client.write_all(request.as_bytes()).unwrap();
            let mut body = String::new();
            client.read_to_string(&mut body).unwrap();
            body
        };

        let rail = exchange("GET /reverse/projects HTTP/1.1\r\nHost: x\r\n\r\n".to_string());
        assert!(rail.starts_with("HTTP/1.1 200 OK"), "{rail}");
        assert!(
            rail.ends_with("{\"current\":\"\",\"projects\":[]}"),
            "{rail}"
        );

        let made = exchange(
            "POST /reverse/project HTTP/1.1\r\nHost: x\r\nContent-Length: 12\r\n\r\ncreate:alpha"
                .to_string(),
        );
        assert!(made.starts_with("HTTP/1.1 200 OK"), "{made}");
        assert!(
            made.ends_with("alpha\n"),
            "the answer names the current project: {made}"
        );

        let twice = exchange(
            "POST /reverse/project HTTP/1.1\r\nHost: x\r\nContent-Length: 12\r\n\r\ncreate:alpha"
                .to_string(),
        );
        assert!(twice.starts_with("HTTP/1.1 400"), "{twice}");

        std::fs::write(
            root.join("study").join("alpha").join("00-overview.md"),
            "# notes\n",
        )
        .unwrap();
        let redone = exchange("POST /reverse/study/redo HTTP/1.1\r\nHost: x\r\n\r\n".to_string());
        assert!(redone.starts_with("HTTP/1.1 200 OK"), "{redone}");
        assert!(redone.contains(".trash/alpha-"), "{redone}");
        assert!(!studied(&root, "alpha"), "the notes were moved, not kept");

        // The delete verb goes through the same path, and takes the redo's trash with it.
        let gone = exchange(
            "POST /reverse/project HTTP/1.1\r\nHost: x\r\nContent-Length: 12\r\n\r\ndelete:alpha"
                .to_string(),
        );
        assert!(gone.starts_with("HTTP/1.1 200 OK"), "{gone}");
        assert!(
            !root.join("study").join("alpha").exists(),
            "the socket route deletes for real"
        );
        let rail = exchange("GET /reverse/projects HTTP/1.1\r\nHost: x\r\n\r\n".to_string());
        assert!(
            rail.ends_with("{\"current\":\"\",\"projects\":[]}"),
            "and the rail is empty again: {rail}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    /// Purpose: The study surface mounts on the portal and answers `/reverse` over one listener.
    fn study_surface_serves_through_the_portal() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let root = temp_reverse();
        std::fs::write(root.join("note.md"), "a study note").unwrap();

        // Mount the surface on the real portal server, with an echo session behind the socket.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let router = StudySurface::new(
            root.clone(),
            root.join("runs").join("run-001"),
            std::sync::Arc::new(|| {}),
        );
        let session = std::sync::Arc::new(std::sync::Mutex::new(loong_web::Echo(vec![])));
        std::thread::spawn(move || {
            loong_web::serve_with(session, listener, "t".to_string(), Some(router));
        });

        // The surface answers its prefix.
        let mut client = TcpStream::connect(addr).unwrap();
        client
            .write_all(
                b"GET /reverse/file/note.md HTTP/1.1\r\nHost: x\r\nx-loong-token: t\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        let mut body = String::new();
        client.read_to_string(&mut body).unwrap();
        assert!(body.ends_with("a study note"), "{body}");

        // The portal still answers `/` with its own page.
        let mut client = TcpStream::connect(addr).unwrap();
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut body = String::new();
        client.read_to_string(&mut body).unwrap();
        assert!(body.contains("loongagent"), "the portal page: {body}");
        std::fs::remove_dir_all(&root).ok();
    }
}
