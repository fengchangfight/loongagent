//! Purpose: Serve the reverse study tab — the `.reverse/` tree, previews and page — inside the portal.
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
    /// The JSON file tree of `.reverse/`.
    Tree,
    /// The bytes of one file under `.reverse/`.
    File(PathBuf),
    /// A directory listing for the target picker; empty means the home directory.
    List(String),
    /// The chosen target: `GET` reads it, `POST` records it.
    Target,
    /// The workflow phase: `GET` reads whether study is finished, `POST` sets it.
    Phase,
    /// The study page (three panes + chat).
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
/// A program file and an APK both open through `core_target_open`; a website does not
/// (`TargetKind` in the engine has no URL variant), it opens through `web_site_open`.
/// So the kind must survive a page reload: re-opening the wrong tab would point the model
/// at the wrong toolset (reverse.md §4.1).
/// Serves: the three-tab target bar and the study prompt, which branches on it.
/// Design: `docs/design/reverse.md` §4.1.
/// Up: `read_target`/`record_target`, and the page's tab state.
/// Down: `Target`, `read_target`, `record_target`.
/// Verify: `cargo test -p loong-reverse-ui target_`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    /// An executable in ELF, PE or Mach-O form.
    Binary,
    /// An Android package: opened like a binary, then read with the `android_*` tools.
    Apk,
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
            TargetKind::Binary => "binary",
            TargetKind::Apk => "apk",
            TargetKind::Url => "url",
        }
    }

    /// Purpose: Read back what `id` wrote; unknown text is not guessed.
    /// Serves: `read_target`, which trusts the recorded word only if it parses.
    /// Design: `docs/design/reverse.md` §4.1.
    /// Up: `read_target`.
    /// Down: nothing; a match over the three words.
    /// Verify: `cargo test -p loong-reverse-ui`.
    pub fn parse(text: &str) -> Option<TargetKind> {
        match text.trim() {
            "binary" => Some(TargetKind::Binary),
            "apk" => Some(TargetKind::Apk),
            "url" => Some(TargetKind::Url),
            _ => None,
        }
    }

    /// Purpose: Guess the kind from the value alone, for targets recorded before kinds existed.
    ///
    /// `http(s)://` is a URL; a `.apk` suffix is an APK; everything else is a program file.
    /// A guess, so `read_target` prefers the recorded kind when there is one.
    /// Serves: `read_target`, for a `.reverse/target` written before kinds existed.
    /// Design: `docs/design/reverse.md` §4.1.
    /// Up: `read_target` only.
    /// Down: nothing; the value is inspected, not opened.
    /// Verify: `cargo test -p loong-reverse-ui`.
    pub fn infer(value: &str) -> TargetKind {
        if value.starts_with("http://") || value.starts_with("https://") {
            TargetKind::Url
        } else if value.to_ascii_lowercase().ends_with(".apk") {
            TargetKind::Apk
        } else {
            TargetKind::Binary
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

/// Purpose: Read the target chosen in the page last time, if any.
///
/// The kind lives in `.reverse/target-kind`; a target recorded before that file existed has
/// only `.reverse/target`, so its kind is inferred from the value (`TargetKind::infer`).
/// Serves: `GET /reverse/target`, which prefills the target bar.
/// Design: `docs/design/reverse.md` §4.1.
/// Up: `handle`.
/// Down: `std::fs::read_to_string` on `.reverse/target` and `.reverse/target-kind`.
/// Verify: `cargo test -p loong-reverse-ui target_`.
pub fn read_target(reverse_dir: &Path) -> Option<Target> {
    let text = std::fs::read_to_string(reverse_dir.join("target")).ok()?;
    let value = text.trim().to_string();
    if value.is_empty() {
        return None;
    }
    let recorded = std::fs::read_to_string(reverse_dir.join("target-kind"))
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
/// this filesystem; a program file or APK has to be an absolute path that exists, because the
/// engine opens files by absolute path only. Refusing here, in the page, shows the user why
/// instead of turning it into a failed model turn.
/// Serves: `POST /reverse/target`.
/// Design: `docs/design/reverse.md` §4.1.
/// Up: `handle`.
/// Down: `std::fs::metadata` and the `.reverse/target` + `.reverse/target-kind` writes.
/// Verify: `cargo test -p loong-reverse-ui target_`.
pub fn record_target(reverse_dir: &Path, kind: TargetKind, chosen: &str) -> Result<Target, String> {
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
        std::fs::create_dir_all(reverse_dir).map_err(|e| e.to_string())?;
        std::fs::write(reverse_dir.join("target"), format!("{chosen}\n"))
            .map_err(|e| format!("记录目标失败：{e}"))?;
        std::fs::write(reverse_dir.join("target-kind"), "url\n")
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
    std::fs::create_dir_all(reverse_dir).map_err(|e| e.to_string())?;
    std::fs::write(reverse_dir.join("target"), format!("{chosen}\n"))
        .map_err(|e| format!("记录目标失败：{e}"))?;
    std::fs::write(reverse_dir.join("target-kind"), format!("{}\n", kind.id()))
        .map_err(|e| format!("记录目标类型失败：{e}"))?;
    Ok(Target {
        kind,
        value: chosen.to_string(),
    })
}

/// Purpose: Whether the user has marked the study step finished — the gate on the rebuild step.
///
/// The user decides, not the file tree: the model can land a half-written
/// `.reverse/study/00-overview.md`, and lighting rebuild up from that would start a rebuild on
/// a study that is not done (reverse.md §4.1.1). The marker is `.reverse/phase` holding `done`;
/// missing file or `todo` means not finished.
/// Serves: the workflow strip, which enables rebuild only when this is true.
/// Design: `docs/design/reverse.md` §4.1.1.
/// Up: `handle` for `GET /reverse/phase`.
/// Down: `std::fs::read_to_string` on `.reverse/phase`.
/// Verify: `cargo test -p loong-reverse-ui phase_`.
pub fn read_phase(reverse_dir: &Path) -> bool {
    std::fs::read_to_string(reverse_dir.join("phase"))
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
/// Down: the `.reverse/phase` write.
/// Verify: `cargo test -p loong-reverse-ui phase_`.
pub fn write_phase(reverse_dir: &Path, done: bool) -> Result<bool, String> {
    std::fs::create_dir_all(reverse_dir).map_err(|e| e.to_string())?;
    let word = if done { "done\n" } else { "todo\n" };
    std::fs::write(reverse_dir.join("phase"), word).map_err(|e| format!("记录进度失败：{e}"))?;
    Ok(done)
}

/// Purpose: The JSON file tree under `root`, stable-sorted, directories before files.
/// Serves: the `/reverse/tree` answer.
/// Design: `docs/design/reverse.md` §4.
/// Up: `handle` only.
/// Down: `std::fs::read_dir` walked recursively.
/// Verify: `cargo test -p loong-reverse-ui tree_`.
pub fn tree_json(root: &Path) -> String {
    let mut entries: Vec<(String, bool)> = Vec::new();
    walk(root, root, &mut entries);
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
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

/// Purpose: The study page: three panes (tree, preview, chat) plus Study and Build.
/// Serves: the `/reverse` route.
/// Design: `docs/design/reverse.md` §4 (the study tab of the portal).
/// Up: `handle` only.
/// Down: the portal's `/ws` socket for the chat pane.
/// Verify: `cargo test -p loong-reverse-ui page_`.
pub fn page_html() -> &'static str {
    include_str!("page.html")
}

/// Purpose: The study surface as a portal router: owns `/reverse`, answers from `.reverse/`.
/// Serves: `loong_web::serve_with`, which the reverse session installs.
/// Design: `docs/design/reverse.md` §4.
/// Up: `crates/cli/src/reverse.rs`'s portal.
/// Down: `handle`.
/// Verify: `cargo test -p loong-reverse-ui`.
pub struct StudySurface {
    /// The session's `.reverse/` directory, resolved once at session start.
    root: PathBuf,
}

impl StudySurface {
    /// Purpose: Mount the study surface on the session's `.reverse/` directory.
    /// Serves: `crates/cli/src/reverse.rs`.
    /// Design: `docs/design/reverse.md` §4.
    /// Up: `run_portal`.
    /// Down: nothing.
    /// Verify: `cargo test -p loong-reverse-ui`.
    pub fn new(reverse_dir: PathBuf) -> Arc<dyn Router> {
        Arc::new(Self { root: reverse_dir })
    }
}

impl Router for StudySurface {
    /// Purpose: This surface owns `/reverse`.
    /// Contract: `crates/reverse/ui/src/lib.rs`.
    fn prefix(&self) -> &str {
        "/reverse"
    }
    /// Purpose: Answer the request from the `.reverse/` directory.
    /// Contract: `crates/reverse/ui/src/lib.rs`.
    fn answer(&self, head: &str, stream: &mut TcpStream) -> Result<(), String> {
        handle(&self.root, head, stream).map(|_| ())
    }
}

/// Purpose: Answer one reverse request on an accepted connection, or hand it back to the portal.
///
/// `head` is the request as read so far: the request line, the headers, and whatever of
/// the body arrived with them. `POST /reverse/target` carries a short body, so the rest is
/// read from the stream when `content-length` says more is coming.
/// Serves: the reverse session's accept loop.
/// Design: `docs/design/reverse.md` §4 and §4.1.
/// Up: `StudySurface::answer`.
/// Down: `route`, `tree_json`, `list_dir`, `record_target`, `read_target`, `page_html`.
/// Verify: `cargo test -p loong-reverse-ui`.
pub fn handle(root: &Path, head: &str, stream: &mut TcpStream) -> Result<bool, String> {
    let request_line = head.lines().next().unwrap_or_default();
    let method = request_line.split_whitespace().next().unwrap_or("GET");
    let path = request_line.split_whitespace().nth(1).unwrap_or("/reverse");
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
                tree_json(root).as_bytes(),
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
                match record_target(root, kind, &chosen) {
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
                let current = read_target(root)
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
                match write_phase(root, done) {
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
                let word = if read_phase(root) { "done\n" } else { "todo\n" };
                respond(
                    stream,
                    "200 OK",
                    "text/plain; charset=utf-8",
                    word.as_bytes(),
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
    /// Purpose: The tree lists directories before files, sorted, as JSON.
    fn tree_lists_the_reverse_dir_as_json() {
        let root = temp_reverse();
        std::fs::create_dir_all(root.join("study")).unwrap();
        std::fs::write(root.join("study").join("00-overview.md"), "# notes\n").unwrap();
        std::fs::write(root.join("target.db"), b"db").unwrap();
        let json = tree_json(&root);
        assert!(json.contains("\"path\":\"study\",\"dir\":true"), "{json}");
        assert!(
            json.contains("\"path\":\"study/00-overview.md\",\"dir\":false"),
            "{json}"
        );
        assert!(
            json.contains("\"path\":\"target.db\",\"dir\":false"),
            "{json}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    /// Purpose: The page carries the target tabs, the two workflow steps, and the three panes.
    fn page_has_the_target_tabs_the_workflow_and_three_panes() {
        let html = page_html();
        for needle in [
            "id=\"tree\"",
            "id=\"preview\"",
            "id=\"chat\"",
            "id=\"target\"",
            "id=\"picker\"",
            "id=\"workflow\"",
            // Three target kinds, because the first tool call differs per kind (§4.1).
            "data-kind=\"binary\"",
            "data-kind=\"apk\"",
            "data-kind=\"url\"",
            // Two steps, the second gated by the user's mark (§4.1.1).
            "id=\"btn-study\"",
            "id=\"btn-done\"",
            "id=\"btn-build\"",
            // The page speaks Chinese; the prompts it injects stay English.
            "选择目标",
            "开始研究",
            "标记研究完成",
            "web_site_open",
            "core_target_open",
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
        let recorded = record_target(&root, TargetKind::Binary, &bin.display().to_string())
            .expect("a file is a target");
        assert_eq!(recorded.value, bin.display().to_string());
        assert_eq!(recorded.kind, TargetKind::Binary);
        assert_eq!(
            read_target(&root),
            Some(Target {
                kind: TargetKind::Binary,
                value: bin.display().to_string()
            })
        );

        // A directory is a target too (the engine opens folders).
        let recorded = record_target(&root, TargetKind::Binary, &root.display().to_string())
            .expect("a folder is a target");
        assert_eq!(recorded.value, root.display().to_string());

        // A path that does not exist is refused, with a reason.
        let refused = record_target(&root, TargetKind::Binary, "/nonexistent/oops")
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

        // The file tabs refuse a URL: that entrance would fail at the first tool call.
        let refused = record_target(&root, TargetKind::Binary, "https://example.com")
            .expect_err("a URL is not a program file");
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
                kind: TargetKind::Apk,
                value: "/home/you/app.apk".to_string()
            }),
            "an .apk path with no recorded kind infers Apk"
        );
        std::fs::write(root.join("target"), "https://example.com\n").unwrap();
        assert_eq!(
            read_target(&root).map(|t| t.kind),
            Some(TargetKind::Url),
            "a URL with no recorded kind infers Url"
        );
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
            record_target(&root, TargetKind::Binary, "app.elf").expect_err("a relative path");
        assert!(refused.contains("绝对路径"), "{refused}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    /// Purpose: `ls`, `target`, `phase` and the tree are decided by the router, page routes stay the page.
    fn route_maps_the_data_endpoints() {
        let root = temp_reverse();
        assert_eq!(
            route(&root, "/reverse/ls?path=/tmp"),
            Route::List("/tmp".to_string())
        );
        assert_eq!(route(&root, "/reverse/ls"), Route::List(String::new()));
        assert_eq!(route(&root, "/reverse/target"), Route::Target);
        assert_eq!(route(&root, "/reverse/phase"), Route::Phase);
        assert!(matches!(route(&root, "/reverse/nope"), Route::NotFound(_)));
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
            let handled = handle(&root2, &request, &mut stream).unwrap();
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
    /// Purpose: The study surface mounts on the portal and answers `/reverse` over one listener.
    fn study_surface_serves_through_the_portal() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let root = temp_reverse();
        std::fs::write(root.join("note.md"), "a study note").unwrap();

        // Mount the surface on the real portal server, with an echo session behind the socket.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let router = StudySurface::new(root.clone());
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
