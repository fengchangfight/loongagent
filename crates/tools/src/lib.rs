//! Tools behind one uniform, MCP-shaped interface — built-ins included
//! (ADR-0004), so permissions, hooks and cost metering have exactly one
//! chokepoint.
//!
//! A tool declares the **action class** of a given call (ADR-0007 §4: autonomy is
//! allocated per action, not per tool), and the kernel asks the PDP before
//! executing. Enforcement happens in two other places too: a write tool refuses
//! to leave the workspace, and every subprocess a tool spawns is confined by the
//! policy the kernel hands in through [`ExecContext`].

pub mod search;

use loong_schema::generated::{ActionClass, Json};
use serde_json::json;
use search::{Glob, Grep};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolError {
    /// The tool refused the call (bad arguments, or outside its remit).
    Refused(String),
    Failed(String),
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolError::Refused(m) => write!(f, "refused: {m}"),
            ToolError::Failed(m) => write!(f, "failed: {m}"),
        }
    }
}

impl std::error::Error for ToolError {}

/// How a tool confines the processes it spawns (ADR-0007: enforcement is the
/// PEP's job and the policy comes from the kernel, not from the tool).
///
/// The implementation runs **in the child, after fork and before exec**, so an
/// implementation must avoid allocation on its success path.
pub trait Confinement: Send + Sync {
    fn confine(&self, command: &mut Command);
}

/// The policy for platforms with nothing to enforce with. The kernel only uses
/// this when `sandbox self-test` reported that no mechanism exists — and it says
/// so loudly at startup.
pub struct NoConfinement;

impl Confinement for NoConfinement {
    fn confine(&self, _command: &mut Command) {}
}

/// What a tool is given to run with: where it runs, and how its children are
/// confined.
pub struct ExecContext<'a> {
    pub cwd: &'a Path,
    pub confinement: &'a dyn Confinement,
}

impl<'a> ExecContext<'a> {
    pub fn new(cwd: &'a Path, confinement: &'a dyn Confinement) -> Self {
        Self { cwd, confinement }
    }
}

/// One tool. `action_class` is computed from the arguments, which is what makes
/// "investigate free, write gated" expressible.
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Json;
    fn action_class(&self, args: &Json, cwd: &Path) -> ActionClass;
    fn execute(&self, args: &Json, ctx: &ExecContext<'_>) -> Result<String, ToolError>;
}

#[derive(Default)]
pub struct Registry {
    tools: BTreeMap<String, Box<dyn Tool>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The registry a fresh session starts with.
    pub fn with_builtins() -> Self {
        let mut r = Self::new();
        r.register(ReadFile);
        r.register(WriteFile);
        r.register(Edit);
        r.register(Grep);
        r.register(Glob);
        r.register(Shell::default());
        r
    }

    pub fn register(&mut self, tool: impl Tool + 'static) {
        self.tools.insert(tool.name().to_string(), Box::new(tool));
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|t| t.as_ref())
    }

    pub fn names(&self) -> Vec<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }

    /// `(name, description, parameters)` — deliberately type-free so a model
    /// adapter can shape it however its wire protocol wants.
    pub fn specs(&self) -> Vec<(String, String, Json)> {
        self.tools
            .values()
            .map(|t| (t.name().to_string(), t.description().to_string(), t.parameters()))
            .collect()
    }
}

/// Resolve `target` against `cwd` and decide whether it stays inside the
/// workspace. Lexical only — no filesystem access, so it also works for paths
/// that do not exist yet (a write target).
pub fn resolve(cwd: &Path, target: &str) -> PathBuf {
    let p = Path::new(target);
    let joined = if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) };
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The action class of **reading** `target`. A read inside the workspace is an
/// investigate action — no side effects — which is what makes "investigate free,
/// write gated" true for reads rather than nominal.
pub fn classify_read_path(cwd: &Path, target: &str) -> ActionClass {
    match classify_path(cwd, target) {
        ActionClass::OutOfBounds => ActionClass::OutOfBounds,
        _ => ActionClass::Investigate,
    }
}

/// The action class of touching `target` (ADR-0007 §4: the *action*, not the
/// tool, decides the autonomy).
pub fn classify_path(cwd: &Path, target: &str) -> ActionClass {
    let resolved = resolve(cwd, target);
    let root = resolve(cwd, ".");
    if resolved.starts_with(&root) {
        ActionClass::WriteWorkspace
    } else {
        ActionClass::OutOfBounds
    }
}

pub(crate) fn arg_str(args: &Json, key: &str) -> Result<String, ToolError> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| ToolError::Refused(format!("missing string argument {key:?}")))
}

// ---------------------------------------------------------------- read/write

pub struct ReadFile;

impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }
    fn description(&self) -> &str {
        "Read a UTF-8 text file. Reads inside the workspace are investigate actions."
    }
    fn parameters(&self) -> Json {
        json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]})
    }
    fn action_class(&self, args: &Json, cwd: &Path) -> ActionClass {
        match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => classify_read_path(cwd, p),
            None => ActionClass::OutOfBounds,
        }
    }
    fn execute(&self, args: &Json, ctx: &ExecContext<'_>) -> Result<String, ToolError> {
        let path = resolve(ctx.cwd, &arg_str(args, "path")?);
        std::fs::read_to_string(&path)
            .map_err(|e| ToolError::Failed(format!("{}: {e}", path.display())))
    }
}

pub struct WriteFile;

impl Tool for WriteFile {
    fn name(&self) -> &str {
        "write_file"
    }
    fn description(&self) -> &str {
        "Write a UTF-8 text file inside the workspace."
    }
    fn parameters(&self) -> Json {
        json!({
            "type": "object",
            "properties": {"path": {"type": "string"}, "content": {"type": "string"}},
            "required": ["path", "content"]
        })
    }
    fn action_class(&self, args: &Json, cwd: &Path) -> ActionClass {
        match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => classify_path(cwd, p),
            None => ActionClass::OutOfBounds,
        }
    }
    fn execute(&self, args: &Json, ctx: &ExecContext<'_>) -> Result<String, ToolError> {
        let path = resolve(ctx.cwd, &arg_str(args, "path")?);
        if classify_path(ctx.cwd, &path.to_string_lossy()) != ActionClass::WriteWorkspace {
            return Err(ToolError::Refused(format!(
                "{} is outside the workspace — this tool only writes inside it",
                path.display()
            )));
        }
        let content = arg_str(args, "content")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| ToolError::Failed(e.to_string()))?;
        }
        std::fs::write(&path, &content).map_err(|e| ToolError::Failed(e.to_string()))?;
        Ok(format!("wrote {} bytes to {}", content.len(), path.display()))
    }
}

/// Replace an exact string in a file — the tool that makes an agent edit rather
/// than rewrite.
///
/// Its refusals are the feature: a missing or ambiguous `old_string` must not be
/// guessed at, because a wrong guess silently corrupts a file. So it fails with
/// the counts and the line numbers and lets the model look again.
pub struct Edit;

impl Tool for Edit {
    fn name(&self) -> &str {
        "edit"
    }
    fn description(&self) -> &str {
        "Replace an exact string in a file inside the workspace. Fails unless          `old_string` occurs exactly once, so pass more context (or set          `replace_all`) rather than guessing. Returns a summary, not the file."
    }
    fn parameters(&self) -> Json {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "old_string": {"type": "string", "description": "exact text to replace"},
                "new_string": {"type": "string", "description": "replacement text"},
                "replace_all": {"type": "boolean", "description": "replace every occurrence"}
            },
            "required": ["path", "old_string", "new_string"]
        })
    }
    fn action_class(&self, args: &Json, cwd: &Path) -> ActionClass {
        match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => classify_path(cwd, p),
            None => ActionClass::OutOfBounds,
        }
    }
    fn execute(&self, args: &Json, ctx: &ExecContext<'_>) -> Result<String, ToolError> {
        let path = resolve(ctx.cwd, &arg_str(args, "path")?);
        if classify_path(ctx.cwd, &path.to_string_lossy()) != ActionClass::WriteWorkspace {
            return Err(ToolError::Refused(format!(
                "{} is outside the workspace — this tool only edits inside it",
                path.display()
            )));
        }
        let old = arg_str(args, "old_string")?;
        let new = arg_str(args, "new_string")?;
        if old.is_empty() {
            return Err(ToolError::Refused("old_string is empty".into()));
        }
        if old == new {
            return Err(ToolError::Refused(
                "old_string and new_string are identical — there is nothing to do".into(),
            ));
        }
        let replace_all = args.get("replace_all").and_then(|v| v.as_bool()).unwrap_or(false);

        let original = std::fs::read_to_string(&path).map_err(|e| {
            ToolError::Failed(format!("reading {}: {e}", path.display()))
        })?;
        let occurrences = original.matches(&old).count();
        if occurrences == 0 {
            return Err(ToolError::Refused(format!(
                "old_string was not found in {} — read the file and copy the text exactly                  (whitespace and line endings included)",
                path.display()
            )));
        }
        if occurrences > 1 && !replace_all {
            let lines: Vec<usize> = line_numbers(&original, &old);
            return Err(ToolError::Refused(format!(
                "old_string occurs {occurrences} times in {} (lines {}); add surrounding context                  to make it unique, or set replace_all",
                path.display(),
                lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join(", ")
            )));
        }

        let replaced = if replace_all {
            original.replace(&old, &new)
        } else {
            original.replacen(&old, &new, 1)
        };
        let lines_before = original.lines().count();
        let lines_after = replaced.lines().count();
        std::fs::write(&path, &replaced).map_err(|e| {
            ToolError::Failed(format!("writing {}: {e}", path.display()))
        })?;

        let at = line_numbers(&original, &old);
        Ok(format!(
            "edited {}: replaced {} of {} occurrence(s){}; {} lines -> {} lines",
            path.display(),
            if replace_all { occurrences } else { 1 },
            occurrences,
            if at.is_empty() {
                String::new()
            } else {
                format!(" at line {}", at.iter().map(|l| l.to_string()).collect::<Vec<_>>().join(", "))
            },
            lines_before,
            lines_after
        ))
    }
}

/// 1-based line numbers where `needle` starts.
fn line_numbers(haystack: &str, needle: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut offset = 0;
    while let Some(found) = haystack[offset..].find(needle) {
        let absolute = offset + found;
        out.push(haystack[..absolute].matches('\n').count() + 1);
        offset = absolute + needle.len().max(1);
    }
    out
}

// -------------------------------------------------------------------- shell

/// Run a command in the workspace.
///
/// The command string is classified before it runs, because a shell tool has no
/// single action class: `ls` is an investigate action and `curl` is out of
/// bounds, even though both are the same tool. The classifier is a *heuristic
/// for allocating autonomy*, never a security boundary — the boundary is the
/// PDP plus the confinement the kernel hands in.
pub struct Shell {
    pub timeout: Duration,
    pub output_limit: usize,
}

impl Default for Shell {
    fn default() -> Self {
        Self { timeout: Duration::from_secs(120), output_limit: 64 * 1024 }
    }
}

/// Commands that are read-only by intent.
const READ_ONLY: &[&str] = &[
    "ls", "cat", "head", "tail", "grep", "rg", "find", "wc", "pwd", "which", "env", "echo",
    "stat", "file", "du", "df", "date", "whoami", "git status", "git diff", "git log", "git show",
    "git branch", "git remote", "git describe", "git rev-parse", "cargo metadata", "cargo tree",
];

/// Commands that reach the network, touch devices, escalate privilege, or are
/// destructive on sight.
const OUT_OF_BOUNDS: &[&str] = &[
    "curl", "wget", "ssh ", "scp ", "sftp ", "nc ", "ncat", "telnet", "rsync ", "git push",
    "git fetch", "git clone", "npm install", "npm i ", "npm publish", "pnpm add", "pnpm install",
    "yarn add", "pip install", "pip3 install", "cargo install", "cargo publish", "docker pull",
    "docker run", "apt install", "apt-get install", "sudo", "doas ", "mkfs", "dd if=", "shutdown",
    "reboot", "poweroff", "chmod -r 777 /", "chown -r ", ":(){", "> /dev/sd", "> /dev/null 2>&1;",
];

/// Verbs that mutate the filesystem.
const MUTATING: &[&str] = &[
    "rm ", "rmdir", "mv ", "cp ", "mkdir", "touch", "tee ", "chmod", "chown", "ln ", "dd ",
    "truncate", "sed -i", "gzip", "tar ", "unzip", "patch ", "install ",
];

/// Classify a shell command (ADR-0007 §4). Unknown commands are treated as
/// workspace-mutating, which is the conservative default.
pub fn classify_command(command: &str, cwd: &Path) -> ActionClass {
    let trimmed = command.trim();
    let lower = trimmed.to_lowercase();
    if trimmed.is_empty() {
        return ActionClass::Investigate;
    }
    if OUT_OF_BOUNDS.iter().any(|p| lower.contains(p)) {
        return ActionClass::OutOfBounds;
    }

    // A redirection target outside the workspace is out of bounds; inside it is
    // an ordinary workspace write.
    for target in redirections(trimmed) {
        if true {
            if classify_path(cwd, &target) == ActionClass::OutOfBounds {
                return ActionClass::OutOfBounds;
            }
        }
    }

    let mutating = MUTATING.iter().any(|p| lower.contains(p));
    if mutating {
        // A mutating verb aimed at an absolute path outside the workspace.
        let touches_outside = trimmed
            .split_whitespace()
            .map(|t| t.trim_matches(|c: char| "\"'`;()".contains(c)))
            .filter(|t| t.starts_with('/'))
            .any(|t| classify_path(cwd, t) == ActionClass::OutOfBounds);
        return if touches_outside {
            ActionClass::OutOfBounds
        } else {
            ActionClass::WriteWorkspace
        };
    }

    // Read-only only when nothing is being written and every segment is a
    // known read-only command.
    if !trimmed.contains('>') {
        let segments: Vec<&str> = trimmed
            .split(['|', '&', ';'])
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        if !segments.is_empty()
            && segments.iter().all(|s| {
                let s = s.to_lowercase();
                READ_ONLY.iter().any(|r| s == *r || s.starts_with(&format!("{r} ")))
            })
        {
            return ActionClass::Investigate;
        }
    }

    ActionClass::WriteWorkspace
}

/// The `>`/`>>` targets of a command line (naive but adequate: quotes are
/// stripped, and the classifier never decides whether something is *allowed* —
/// only how much autonomy it needs).
fn redirections(command: &str) -> Vec<String> {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let mut targets = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        if !token.starts_with('>') {
            continue;
        }
        // `> file`, `>>file`, or a value on the next token (`> file` split apart).
        let attached = token.trim_start_matches('>');
        let path = if attached.is_empty() {
            tokens.get(i + 1).copied().unwrap_or("")
        } else {
            attached
        };
        let path = path.trim_matches(|c: char| "\"'`;".contains(c));
        if !path.is_empty() && !path.starts_with('&') {
            targets.push(path.to_string());
        }
    }
    targets
}

impl Tool for Shell {
    fn name(&self) -> &str {
        "shell"
    }
    fn description(&self) -> &str {
        "Run a shell command in the workspace and return its bounded output. \
         Read-only commands are investigate actions; anything reaching the network, \
         leaving the workspace, or destructive is out of bounds."
    }
    fn parameters(&self) -> Json {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string"},
                "description": {"type": "string", "description": "why this command is needed"}
            },
            "required": ["command"]
        })
    }
    fn action_class(&self, args: &Json, cwd: &Path) -> ActionClass {
        match args.get("command").and_then(|v| v.as_str()) {
            Some(command) => classify_command(command, cwd),
            None => ActionClass::OutOfBounds,
        }
    }
    fn execute(&self, args: &Json, ctx: &ExecContext<'_>) -> Result<String, ToolError> {
        let command = arg_str(args, "command")?;
        let sh_args = vec!["-c".to_string(), command.clone()];
        // The PEP is applied to the child, never to us: the restriction is
        // irreversible and must not leak into the server.
        let confine = |cmd: &mut Command| ctx.confinement.confine(cmd);
        let spec = loong_proc::RunSpec::new("sh", &sh_args, ctx.cwd, self.timeout)
            .output_limit(self.output_limit)
            .confine(&confine);

        let out = loong_proc::run(&spec).map_err(|e| ToolError::Failed(e.to_string()))?;
        if out.timed_out {
            return Err(ToolError::Failed(format!(
                "timed out after {:?}: {command}",
                self.timeout
            )));
        }

        let mut report = out.describe_exit();
        if !out.stdout.is_empty() {
            report.push_str("\n--- stdout ---\n");
            report.push_str(&out.stdout);
        }
        if !out.stderr.is_empty() {
            report.push_str("\n--- stderr ---\n");
            report.push_str(&out.stderr);
        }
        if out.stdout_truncated || out.stderr_truncated {
            report.push_str(&format!(
                "\n[output truncated at {} bytes per stream]",
                self.output_limit
            ));
        }
        if out.success() {
            Ok(report)
        } else {
            Err(ToolError::Failed(report))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    static N: AtomicU64 = AtomicU64::new(0);
    fn tempdir() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "loong-tools-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    fn ctx<'a>(cwd: &'a Path, confinement: &'a dyn Confinement) -> ExecContext<'a> {
        ExecContext::new(cwd, confinement)
    }

    #[test]
    fn writes_inside_the_workspace_are_allowed_and_outside_are_refused() {
        let cwd = tempdir();
        let c = NoConfinement;
        let w = WriteFile;
        let ok = w.execute(&json!({"path": "sub/a.txt", "content": "hi"}), &ctx(&cwd, &c)).unwrap();
        assert!(ok.contains("2 bytes"));
        assert_eq!(std::fs::read_to_string(cwd.join("sub/a.txt")).unwrap(), "hi");

        let outside =
            w.execute(&json!({"path": "/tmp/loong-escape.txt", "content": "x"}), &ctx(&cwd, &c));
        assert!(matches!(outside, Err(ToolError::Refused(_))), "a write outside is refused");
    }

    #[test]
    fn action_class_follows_the_path_not_the_tool() {
        let cwd = Path::new("/work");
        assert_eq!(classify_path(cwd, "src/main.rs"), ActionClass::WriteWorkspace);
        assert_eq!(classify_path(cwd, "/work/src/x"), ActionClass::WriteWorkspace);
        assert_eq!(classify_path(cwd, "../etc/passwd"), ActionClass::OutOfBounds);
        assert_eq!(classify_path(cwd, "/etc/passwd"), ActionClass::OutOfBounds);
        let r = ReadFile;
        assert_eq!(
            r.action_class(&json!({"path": "README.md"}), cwd),
            ActionClass::Investigate,
            "reading inside the workspace has no side effects"
        );
        assert_eq!(r.action_class(&json!({"path": "/etc/shadow"}), cwd), ActionClass::OutOfBounds);
        assert_eq!(r.action_class(&json!({}), cwd), ActionClass::OutOfBounds);
    }

    #[test]
    fn missing_arguments_are_refused_not_panicked() {
        let cwd = tempdir();
        let c = NoConfinement;
        assert!(matches!(
            ReadFile.execute(&json!({}), &ctx(&cwd, &c)),
            Err(ToolError::Refused(_))
        ));
        assert!(matches!(
            WriteFile.execute(&json!({"path": "a"}), &ctx(&cwd, &c)),
            Err(ToolError::Refused(_))
        ));
        assert!(matches!(Shell::default().execute(&json!({}), &ctx(&cwd, &c)), Err(ToolError::Refused(_))));
    }

    #[test]
    fn edit_replaces_a_unique_string_and_says_where() {
        let cwd = tempdir();
        std::fs::write(cwd.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let c = NoConfinement;
        let out = Edit
            .execute(
                &json!({"path": "a.txt", "old_string": "two", "new_string": "TWO"}),
                &ctx(&cwd, &c),
            )
            .unwrap();
        assert!(out.contains("replaced 1 of 1"), "{out}");
        assert!(out.contains("line 2"), "{out}");
        assert_eq!(std::fs::read_to_string(cwd.join("a.txt")).unwrap(), "one\nTWO\nthree\n");
    }

    #[test]
    fn edit_refuses_a_missing_string_and_leaves_the_file_alone() {
        let cwd = tempdir();
        std::fs::write(cwd.join("a.txt"), "content\n").unwrap();
        let c = NoConfinement;
        let err = Edit
            .execute(
                &json!({"path": "a.txt", "old_string": "absent", "new_string": "x"}),
                &ctx(&cwd, &c),
            )
            .unwrap_err();
        assert!(err.to_string().contains("was not found"), "{err}");
        assert_eq!(std::fs::read_to_string(cwd.join("a.txt")).unwrap(), "content\n");
    }

    #[test]
    fn edit_refuses_an_ambiguous_string_and_names_the_lines() {
        let cwd = tempdir();
        std::fs::write(cwd.join("a.txt"), "dup\nmid\ndup\n").unwrap();
        let c = NoConfinement;
        let err = Edit
            .execute(&json!({"path": "a.txt", "old_string": "dup", "new_string": "x"}), &ctx(&cwd, &c))
            .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("occurs 2 times"), "{message}");
        assert!(message.contains("lines 1, 3"), "{message}");
        assert_eq!(std::fs::read_to_string(cwd.join("a.txt")).unwrap(), "dup\nmid\ndup\n");
    }

    #[test]
    fn edit_with_replace_all_touches_every_occurrence() {
        let cwd = tempdir();
        std::fs::write(cwd.join("a.txt"), "dup\nmid\ndup\n").unwrap();
        let c = NoConfinement;
        let out = Edit
            .execute(
                &json!({"path": "a.txt", "old_string": "dup", "new_string": "x", "replace_all": true}),
                &ctx(&cwd, &c),
            )
            .unwrap();
        assert!(out.contains("replaced 2 of 2"), "{out}");
        assert_eq!(std::fs::read_to_string(cwd.join("a.txt")).unwrap(), "x\nmid\nx\n");
    }

    #[test]
    fn edit_refuses_outside_the_workspace_and_pointless_calls() {
        let cwd = tempdir();
        let c = NoConfinement;
        assert!(matches!(
            Edit.execute(
                &json!({"path": "/etc/hosts", "old_string": "a", "new_string": "b"}),
                &ctx(&cwd, &c)
            ),
            Err(ToolError::Refused(_))
        ));
        std::fs::write(cwd.join("a.txt"), "same\n").unwrap();
        assert!(matches!(
            Edit.execute(&json!({"path": "a.txt", "old_string": "same", "new_string": "same"}), &ctx(&cwd, &c)),
            Err(ToolError::Refused(_))
        ));
        assert!(matches!(Edit.execute(&json!({}), &ctx(&cwd, &c)), Err(ToolError::Refused(_))));
    }

    #[test]
    fn edit_handles_multiline_replacements() {
        let cwd = tempdir();
        std::fs::write(cwd.join("a.rs"), "fn a() {\n    1\n}\n").unwrap();
        let c = NoConfinement;
        Edit.execute(
            &json!({"path": "a.rs", "old_string": "fn a() {\n    1\n}", "new_string": "fn a() -> i32 {\n    1\n}"}),
            &ctx(&cwd, &c),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(cwd.join("a.rs")).unwrap(), "fn a() -> i32 {\n    1\n}\n");
    }

    #[test]
    fn registry_exposes_specs_for_the_model() {
        let r = Registry::with_builtins();
        assert_eq!(r.names(), vec!["edit", "glob", "grep", "read_file", "shell", "write_file"]);
        let specs = r.specs();
        assert_eq!(specs.len(), 6);
        assert!(specs.iter().any(|(n, d, _)| n == "read_file" && !d.is_empty()));
        assert!(specs.iter().all(|(_, d, p)| !d.is_empty() && p["type"] == "object"));
    }

    // ------------------------------------------------------- the classifier

    #[test]
    fn one_tool_gets_three_action_classes() {
        let cwd = Path::new("/work");
        // read-only → investigate (no prompt)
        assert_eq!(classify_command("ls -la", cwd), ActionClass::Investigate);
        assert_eq!(classify_command("cat src/main.rs | grep fn", cwd), ActionClass::Investigate);
        assert_eq!(classify_command("git status --short", cwd), ActionClass::Investigate);
        // writes inside the workspace → workspace write
        assert_eq!(classify_command("echo hi > inside.txt", cwd), ActionClass::WriteWorkspace);
        assert_eq!(classify_command("cargo test --workspace", cwd), ActionClass::WriteWorkspace);
        assert_eq!(classify_command("rm -rf target/", cwd), ActionClass::WriteWorkspace);
        // out of bounds: leaving the workspace, the network, privilege, destructiveness
        assert_eq!(classify_command("echo hi > /etc/passwd", cwd), ActionClass::OutOfBounds);
        assert_eq!(classify_command("rm -rf /", cwd), ActionClass::OutOfBounds);
        assert_eq!(classify_command("curl https://example.com", cwd), ActionClass::OutOfBounds);
        assert_eq!(classify_command("git push origin main", cwd), ActionClass::OutOfBounds);
        assert_eq!(classify_command("sudo rm -rf /var", cwd), ActionClass::OutOfBounds);
        assert_eq!(classify_command("cp secrets.txt /elsewhere/", cwd), ActionClass::OutOfBounds);
        // an unknown command is conservatively a workspace write
        assert_eq!(classify_command("./deploy.sh", cwd), ActionClass::WriteWorkspace);
        assert_eq!(classify_command("", cwd), ActionClass::Investigate);
    }

    // ---------------------------------------------------------- the shell

    #[test]
    fn a_read_only_command_runs_and_reports_its_output() {
        let cwd = tempdir();
        std::fs::write(cwd.join("a.txt"), "hello from a file").unwrap();
        let c = NoConfinement;
        let shell = Shell { timeout: Duration::from_secs(10), output_limit: 4096 };
        let out = shell.execute(&json!({"command": "cat a.txt"}), &ctx(&cwd, &c)).unwrap();
        assert!(out.contains("exit 0"), "{out}");
        assert!(out.contains("hello from a file"), "{out}");
        assert!(out.contains("--- stdout ---"), "{out}");
    }

    #[test]
    fn a_failing_command_reports_the_exit_code_and_stderr() {
        let cwd = tempdir();
        let c = NoConfinement;
        let shell = Shell { timeout: Duration::from_secs(10), output_limit: 4096 };
        let err = shell
            .execute(&json!({"command": "echo boom >&2; exit 3"}), &ctx(&cwd, &c))
            .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("exit 3"), "{message}");
        assert!(message.contains("boom"), "{message}");
    }

    #[test]
    fn output_is_bounded_per_stream() {
        let cwd = tempdir();
        let c = NoConfinement;
        let shell = Shell { timeout: Duration::from_secs(20), output_limit: 2048 };
        let out = shell
            .execute(&json!({"command": "yes aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa | head -n 500"}), &ctx(&cwd, &c))
            .unwrap();
        assert!(out.contains("output truncated at 2048 bytes"), "{out}");
        assert!(out.len() < 6000, "the report stays small: {} bytes", out.len());
    }

    #[test]
    fn a_hung_command_is_killed_by_the_timeout() {
        let cwd = tempdir();
        let c = NoConfinement;
        let shell = Shell { timeout: Duration::from_millis(300), output_limit: 4096 };
        let started = Instant::now();
        let err = shell.execute(&json!({"command": "sleep 30"}), &ctx(&cwd, &c)).unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(10), "the timeout actually fires");
    }

    #[test]
    fn a_command_runs_in_the_workspace() {
        let cwd = tempdir();
        let c = NoConfinement;
        let shell = Shell { timeout: Duration::from_secs(10), output_limit: 4096 };
        let out = shell.execute(&json!({"command": "pwd"}), &ctx(&cwd, &c)).unwrap();
        let canonical = std::fs::canonicalize(&cwd).unwrap();
        assert!(out.contains(canonical.to_string_lossy().as_ref()), "{out}");
    }
}
