//! L1 — atomic records, extracted deterministically from raw events.
//!
//! An [`Atom`] is *not* a short summary. It is a near-original materialisation of
//! one event, built to be searchable and citable: a stable id (the event's own
//! `seq`), the tool it came from, the extracted file/module/command/test/error
//! entities, and a length-capped text body. The full payload stays in the JSONL
//! log and in `loong-index`'s `events` table; this never copies it wholesale.
//!
//! Everything here is pure and deterministic: the same event always yields the
//! same atom and the same entities. No model, no clock, no randomness. That is
//! what makes "delete the database and rebuild it" a real answer rather than a
//! hopeful one, and there is a test for it.

use loong_schema::generated::EventEnvelope;
use std::collections::BTreeSet;

/// How much of an event's text body is kept in the searchable extract. The rest
/// is reachable through the citation, not through this table — see the disk
/// budget reasoning in `docs/06-context-engineering.md` §5.3.
pub const MAX_TEXT_CHARS: usize = 512;

/// What kind of thing an entity name refers to. `kind` is part of the identity,
/// so `test:merge` and `module:merge` never collide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntityKind {
    /// A file, e.g. `src/config/merge.rs`.
    Path,
    /// A directory, e.g. `src/config` — the L3 rollup unit.
    Module,
    /// A bare identifier, e.g. a function or type name seen in prose.
    Symbol,
    /// A tool the model called, e.g. `read_file`.
    Tool,
    /// The head of a command line, e.g. `cargo test`.
    Command,
    /// A test name, e.g. `layer_merge`.
    Test,
    /// A compiler or runtime error code, e.g. `E0308`.
    Error,
}

impl EntityKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EntityKind::Path => "path",
            EntityKind::Module => "module",
            EntityKind::Symbol => "symbol",
            EntityKind::Tool => "tool",
            EntityKind::Command => "command",
            EntityKind::Test => "test",
            EntityKind::Error => "error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Entity {
    pub kind: EntityKind,
    pub name: String,
}

/// One event, made searchable. `seq` is the event's sequence number — a stable
/// id that also *is* the citation, so no second identifier scheme is invented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Atom {
    pub seq: u64,
    pub id: String,
    pub time_ms: i64,
    pub kind: String,
    /// `user` | `assistant` | `tool`, when the event is a message.
    pub role: Option<String>,
    /// The tool, for tool calls and their results.
    pub tool: Option<String>,
    /// `Some(true/false)` only for `tool.result`.
    pub ok: Option<bool>,
    pub text: String,
    /// Stable content hash, for cheap duplicate detection.
    pub content_hash: u64,
    /// Whether this event changed a file. Part of the atom because it is a
    /// *deterministic classification* the digest depends on — deriving it again
    /// at read time would make a rebuild stop reproducing the summaries.
    pub is_write: bool,
    /// The tool-call id, so a result can be tied back to the call it answers:
    /// `is_write` lives on the call, success/failure on the result.
    pub call_id: Option<String>,
    /// For a write, what the attempt was. `None` for everything else, and for a
    /// write whose arguments carry no text (a delete, say).
    pub write_attempt: Option<String>,
    pub entities: Vec<Entity>,
}

impl Atom {
    /// Entities of one kind, for a deterministic filter (`entity = "src/config"`).
    pub fn names_of(&self, kind: EntityKind) -> Vec<&str> {
        self.entities
            .iter()
            .filter(|e| e.kind == kind)
            .map(|e| e.name.as_str())
            .collect()
    }
}

/// Turn one event into an atom. Returns `None` for kinds that carry no
/// searchable content at all — recording them would inflate the store without
/// ever answering a question.
pub fn materialize(env: &EventEnvelope) -> Option<Atom> {
    let data = &env.data;
    let mut role = None;
    let mut tool = None;
    let mut ok = None;
    let mut call_id = None;
    let mut is_write = false;
    let mut write_attempt: Option<String> = None;
    let text: String;

    match env.kind.as_str() {
        "message.appended" => {
            let r = str_field(data, "role").unwrap_or_default();
            role = Some(r.clone());
            let body = str_field(data, "text").unwrap_or_default();
            // The role is part of the text so that "user said X" is searchable
            // without a join, and so a bare keyword hit is self-describing.
            text = format!("[{r}] {body}");
        }
        "tool.call" => {
            tool = str_field(data, "tool");
            call_id = str_field(data, "call_id");
            is_write = tool
                .as_deref()
                .map(|t| classify_write(t, data.get("args")))
                .unwrap_or(false);
            if is_write {
                write_attempt = write_attempt_opt(data.get("args"));
            }
            text = join_nonempty(
                " ",
                &[
                    tool.clone().unwrap_or_default(),
                    call_id.clone().unwrap_or_default(),
                    compact_json(data.get("args")),
                ],
            );
        }
        "tool.result" => {
            ok = data.get("ok").and_then(|v| v.as_bool());
            call_id = str_field(data, "call_id");
            let output = str_field(data, "output").unwrap_or_default();
            text = join_nonempty(
                " ",
                &[
                    call_id.clone().unwrap_or_default(),
                    if ok == Some(true) { "ok" } else { "failed" }.to_string(),
                    output,
                ],
            );
        }
        "skill.invoked" => {
            text = join_nonempty(
                " ",
                &[
                    "skill".to_string(),
                    str_field(data, "name").unwrap_or_default(),
                    str_field(data, "source").unwrap_or_default(),
                    str_field(data, "args").unwrap_or_default(),
                ],
            );
        }
        "compaction.applied" | "memory.summary.updated" | "memory.superseded" => {
            text = compact_json(Some(data));
        }
        // Decided, but not worth a row on their own: they carry no content a
        // grep would ever look for. `approval.decided` and `policy.decided`
        // matter to a *human* reading the log, and the log has them.
        _ => return None,
    }

    let text = cap(&text, MAX_TEXT_CHARS);
    let entities = extract_entities(env, &text, tool.as_deref());

    Some(Atom {
        seq: env.seq,
        id: env.id.clone(),
        time_ms: env.time_ms,
        kind: env.kind.clone(),
        role,
        tool,
        ok,
        content_hash: fnv1a64(&text),
        is_write,
        call_id,
        write_attempt,
        text,
        entities,
    })
}

/// Tool names that change a file. A list rather than a convention, because tool
/// names are the one thing every harness spells differently.
const WRITE_TOOLS: &[&str] = &[
    "write_file",
    "edit_file",
    "str_replace",
    "apply_patch",
    "create_file",
    "write",
    "edit",
    "patch",
];

/// What a write attempt actually was, in a few words: the new text, or the patch,
/// truncated. This is the part a model needs to tell one attempt from another —
/// measured: an attempt described only as "changed merge.rs" leaves a 7B model with
/// nothing to name as rejected, and it abstains rather than inventing (which is the
/// good outcome; an empty `rejected` is a thin memory, not a wrong one).
fn write_attempt_opt(args: Option<&serde_json::Value>) -> Option<String> {
    let args = args?;
    for key in ["new_string", "content", "new_text", "patch"] {
        if let Some(value) = args.get(key).and_then(|v| v.as_str()) {
            let head: String = value.chars().take(120).collect();
            if !head.trim().is_empty() {
                return Some(head.replace('\n', " "));
            }
        }
    }
    // A rename or a delete moves no text, but the path change is the attempt.
    for key in ["from", "old_string"] {
        if let Some(value) = args.get(key).and_then(|v| v.as_str()) {
            return Some(format!(
                "{} → {}",
                value,
                args.get("to").and_then(|v| v.as_str()).unwrap_or("?")
            ));
        }
    }
    None
}

/// Whether a tool call changes a file: by name, or by carrying the arguments a
/// write carries. The second signal is what keeps this working for a tool whose
/// name we have never seen (`fs__put` with a `content` argument).
fn classify_write(tool: &str, args: Option<&serde_json::Value>) -> bool {
    let lowered = tool.to_ascii_lowercase();
    if WRITE_TOOLS.contains(&lowered.as_str()) {
        return true;
    }
    if ["write", "create", "apply", "patch", "edit", "str_replace"]
        .iter()
        .any(|prefix| lowered.starts_with(prefix))
    {
        return true;
    }
    match args {
        Some(args) => ["content", "new_string", "new_text", "patch"]
            .iter()
            .any(|key| args.get(key).is_some()),
        None => false,
    }
}

/// The entities an event refers to. Deterministic rules only — see the table in
/// `docs/06-context-engineering.md` §5.4. The module assignment (L3's rollup
/// unit) is deliberately computed here rather than asked of a model, so the
/// "dirty set" that drives incremental updates is exactly computable.
fn extract_entities(env: &EventEnvelope, text: &str, tool: Option<&str>) -> Vec<Entity> {
    let mut out = BTreeSet::new();

    if let Some(tool) = tool {
        if !tool.is_empty() {
            out.insert(Entity {
                kind: EntityKind::Tool,
                name: tool.to_string(),
            });
        }
    }

    // Explicit argument fields are the highest-signal source.
    if let Some(args) = env.data.get("args") {
        for key in ["path", "file", "file_path", "pattern", "filename"] {
            if let Some(value) = args.get(key).and_then(|v| v.as_str()) {
                insert_path_like(&mut out, value);
            }
        }
        if let Some(command) = args.get("command").and_then(|v| v.as_str()) {
            insert_command(&mut out, command);
            scan_text(&mut out, command);
        }
    }
    if let Some(command) = env.data.get("command").and_then(|v| v.as_str()) {
        insert_command(&mut out, command);
    }

    scan_text(&mut out, text);
    out.into_iter().collect()
}

/// Path-ish tokens and error codes in free text.
fn scan_text(out: &mut BTreeSet<Entity>, text: &str) {
    for raw in text.split(is_token_separator) {
        if raw.is_empty() {
            continue;
        }
        if let Some(code) = error_code(raw) {
            out.insert(Entity {
                kind: EntityKind::Error,
                name: code,
            });
            continue;
        }
        insert_path_like(out, raw);
    }
    // `test <name> ... ok` — the one place Rust's own test output is stable.
    for name in test_names(text) {
        out.insert(Entity {
            kind: EntityKind::Test,
            name,
        });
    }
}

fn insert_path_like(out: &mut BTreeSet<Entity>, raw: &str) {
    let token = raw.trim();
    if token.is_empty() || token.len() > 200 {
        return;
    }
    if let Some(code) = error_code(token) {
        out.insert(Entity {
            kind: EntityKind::Error,
            name: code,
        });
        return;
    }
    if !looks_like_path(token) {
        return;
    }
    let mut path = token.to_string();
    while path.ends_with('/') || path.ends_with('\\') {
        path.pop();
    }
    if path.is_empty() {
        return;
    }
    out.insert(Entity {
        kind: EntityKind::Path,
        name: path.clone(),
    });
    // The module is the path's first two segments after any leading `./`, so
    // `src/kernel/turn.rs` rolls up to `src/kernel` and L3 does not explode
    // into one map per file. Deeper grouping stays possible via the path entity.
    let cleaned = path.trim_start_matches("./");
    let segments: Vec<&str> = cleaned.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() >= 2 {
        out.insert(Entity {
            kind: EntityKind::Module,
            name: format!("{}/{}", segments[0], segments[1]),
        });
    } else if segments.len() == 1 && !has_known_extension(segments[0]) {
        out.insert(Entity {
            kind: EntityKind::Module,
            name: segments[0].to_string(),
        });
    }
}

/// A bare name that names a file we recognise (`turn.rs`), as opposed to a
/// directory-like token (`kernel`) that is worth a module entity on its own.
fn has_known_extension(token: &str) -> bool {
    let ext = token.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    KNOWN_EXTENSIONS.contains(&ext)
}

fn insert_command(out: &mut BTreeSet<Entity>, command: &str) {
    // The head of the command is what anybody greps for; the arguments are in
    // the text and in the raw event. Leading `VAR=value` assignments are
    // skipped, because `RUST_LOG=debug cargo test` is a `cargo` command.
    let head = command
        .split_whitespace()
        .find(|word| !(word.contains('=') && !word.contains('/')));
    if let Some(head) = head {
        out.insert(Entity {
            kind: EntityKind::Command,
            name: head.to_string(),
        });
    }
}

/// `E0308`, `error[E0308]`, `TS2345`, `ERR_MODULE_NOT_FOUND`.
fn error_code(token: &str) -> Option<String> {
    let t = token.trim_matches(|c: char| c == '[' || c == ']' || c == '(' || c == ')' || c == ',');
    let t = t.strip_prefix("error[").unwrap_or(t);
    let t = t.trim_end_matches(']');
    let core = t.strip_prefix("error").unwrap_or(t);
    if core.is_empty() || core.len() > 40 {
        return None;
    }
    let bytes = core.as_bytes();
    let alnum = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    if bytes.iter().all(|&b| alnum(b))
        && bytes.iter().any(|b| b.is_ascii_digit())
        && bytes[0].is_ascii_uppercase()
    {
        return Some(core.to_string());
    }
    None
}

const KNOWN_EXTENSIONS: &[&str] = &[
    "rs", "toml", "json", "jsonl", "md", "txt", "ts", "tsx", "js", "jsx", "py", "go", "java", "kt",
    "c", "h", "cc", "cpp", "hpp", "cs", "rb", "php", "sh", "bash", "zsh", "yml", "yaml", "sql",
    "html", "css", "scss", "proto", "lock", "cfg", "ini", "env", "gradle", "swift", "mm", "ex",
    "exs", "lua", "pl", "r", "scala", "clj", "hs", "ml", "vue", "svelte",
];

fn looks_like_path(token: &str) -> bool {
    if token.contains("://") || token.starts_with("--") {
        return false;
    }
    let ext = token.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    if token.contains('/') {
        let last = token.rsplit('/').next().unwrap_or("");
        // A version, a URL path fragment, or a bare `a/b` prose slash is not a
        // file. Require either a known extension or a dotted last segment.
        return !last.is_empty()
            && (KNOWN_EXTENSIONS.contains(&ext) || last.contains('.') || token.starts_with("./"));
    }
    // No separator: only a recognised file name, never a bare `foo.bar` prose.
    KNOWN_EXTENSIONS.contains(&ext)
}

fn test_names(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut tokens = text.split_whitespace().peekable();
    while let Some(token) = tokens.next() {
        if token != "test" {
            continue;
        }
        let Some(candidate) = tokens.peek() else {
            continue;
        };
        let name = candidate.trim_end_matches(|c: char| !(c.is_alphanumeric() || c == '_'));
        if !name.is_empty()
            && name.chars().any(|c| c.is_alphanumeric())
            && name.len() <= 120
            && !KNOWN_EXTENSIONS.contains(&name.rsplit_once('.').map(|(_, e)| e).unwrap_or(""))
        {
            out.push(name.to_string());
        }
    }
    out
}

fn is_token_separator(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '"' | '\''
                | '`'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '<'
                | '>'
                | '='
                | ','
                | ';'
                | ':'
                | '|'
                | '*'
                | '?'
                | '!'
                | '#'
                | '$'
                | '&'
                | '+'
                | '@'
                | '~'
                // Full-width CJK punctuation counts. Without it,
                // `任务 2：src/kernel/a.rs` tokenises as one "path" and yields a
                // module named `2：src/kernel` — a real bug this project hit.
                | '：'
                | '，'
                | '。'
                | '、'
                | '；'
                | '（'
                | '）'
                | '【'
                | '】'
                | '？'
                | '！'
                | '“'
                | '”'
                | '‘'
                | '’'
                | '《'
                | '》'
                | '…'
                | '—'
                | '·'
        )
}

fn str_field(data: &serde_json::Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn compact_json(value: Option<&serde_json::Value>) -> String {
    match value {
        Some(v) => serde_json::to_string(v).unwrap_or_default(),
        None => String::new(),
    }
}

fn join_nonempty(sep: &str, parts: &[String]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(sep)
}

/// Truncate on a character boundary and say so, so a reader (human or model)
/// never mistakes a cut-off extract for the whole output.
fn cap(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}…[truncated]")
}

/// FNV-1a, 64-bit. Chosen deliberately over a `sha2` dependency: the hash is
/// only used to spot duplicates, and it must be *stable across runs* so that a
/// rebuild is byte-reproducible — which is the property a test actually needs.
pub fn fnv1a64(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}
