//! Tools behind one uniform, MCP-shaped interface — built-ins included
//! (ADR-0004), so permissions, hooks and cost metering have exactly one
//! chokepoint.
//!
//! A tool declares the **action class** of a given call (ADR-0007 §4: autonomy is
//! allocated per action, not per tool), and the kernel asks the PDP before
//! executing. Enforcement lives here too, as a second line of defence: a write
//! tool refuses to leave the workspace even if it were somehow allowed through.

use loong_schema::generated::{ActionClass, Json};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

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

/// One tool. `action_class` is computed from the arguments, which is what makes
/// "investigate free, write gated" expressible.
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Json;
    fn action_class(&self, args: &Json, cwd: &Path) -> ActionClass;
    fn execute(&self, args: &Json, cwd: &Path) -> Result<String, ToolError>;
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

fn arg_str(args: &Json, key: &str) -> Result<String, ToolError> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| ToolError::Refused(format!("missing string argument {key:?}")))
}

pub struct ReadFile;

impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }
    fn description(&self) -> &str {
        "Read a UTF-8 text file. Reading is an investigate action inside the workspace."
    }
    fn parameters(&self) -> Json {
        json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]})
    }
    fn action_class(&self, args: &Json, cwd: &Path) -> ActionClass {
        match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => classify_path(cwd, p),
            None => ActionClass::OutOfBounds,
        }
    }
    fn execute(&self, args: &Json, cwd: &Path) -> Result<String, ToolError> {
        let path = resolve(cwd, &arg_str(args, "path")?);
        std::fs::read_to_string(&path).map_err(|e| ToolError::Failed(format!("{}: {e}", path.display())))
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
    fn execute(&self, args: &Json, cwd: &Path) -> Result<String, ToolError> {
        let path = resolve(cwd, &arg_str(args, "path")?);
        if classify_path(cwd, &path.to_string_lossy()) != ActionClass::WriteWorkspace {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

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

    #[test]
    fn writes_inside_the_workspace_are_allowed_and_outside_are_refused() {
        let cwd = tempdir();
        let w = WriteFile;
        let ok = w.execute(&json!({"path": "sub/a.txt", "content": "hi"}), &cwd).unwrap();
        assert!(ok.contains("2 bytes"));
        assert_eq!(std::fs::read_to_string(cwd.join("sub/a.txt")).unwrap(), "hi");

        let outside = w.execute(&json!({"path": "/tmp/loong-escape.txt", "content": "x"}), &cwd);
        assert!(matches!(outside, Err(ToolError::Refused(_))), "a write outside is refused");
    }

    #[test]
    fn action_class_follows_the_path_not_the_tool() {
        let cwd = Path::new("/work");
        assert_eq!(classify_path(cwd, "src/main.rs"), ActionClass::WriteWorkspace);
        assert_eq!(classify_path(cwd, "/work/src/x"), ActionClass::WriteWorkspace);
        assert_eq!(classify_path(cwd, "../etc/passwd"), ActionClass::OutOfBounds);
        assert_eq!(classify_path(cwd, "/etc/passwd"), ActionClass::OutOfBounds);
        // reads are investigate *inside* the workspace, out-of-bounds outside it
        let r = ReadFile;
        assert_eq!(r.action_class(&json!({"path": "README.md"}), cwd), ActionClass::WriteWorkspace);
        assert_eq!(r.action_class(&json!({"path": "/etc/shadow"}), cwd), ActionClass::OutOfBounds);
        assert_eq!(r.action_class(&json!({}), cwd), ActionClass::OutOfBounds);
    }

    #[test]
    fn missing_arguments_are_refused_not_panicked() {
        let cwd = tempdir();
        assert!(matches!(ReadFile.execute(&json!({}), &cwd), Err(ToolError::Refused(_))));
        assert!(matches!(WriteFile.execute(&json!({"path": "a"}), &cwd), Err(ToolError::Refused(_))));
    }

    #[test]
    fn registry_exposes_specs_for_the_model() {
        let r = Registry::with_builtins();
        assert_eq!(r.names(), vec!["read_file", "write_file"]);
        let specs = r.specs();
        assert_eq!(specs.len(), 2);
        assert!(specs.iter().any(|(n, d, _)| n == "read_file" && !d.is_empty()));
    }
}
