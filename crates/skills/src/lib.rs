//! Skills and commands — **one concept** (T-18, T-16 merged in 2026-10-05).
//!
//! A skill is a markdown file with frontmatter. Its *description* is what enters
//! context; its *body* loads only when it is invoked. That is ADR-0006's
//! progressive disclosure applied to instructions: a workspace can carry twenty
//! workflows and pay for twenty lines, not twenty pages.
//!
//! Two shapes are accepted, because "commands merged into skills" means the older
//! spelling still works:
//!
//! ```text
//! .loongagent/skills/<name>/SKILL.md
//! .loongagent/commands/<name>.md
//! ```
//!
//! Invocation is `/name arguments`, and the body is a template: `$ARGUMENTS` is
//! everything after the name, `$1`…`$9` are individual arguments. If a body uses
//! none of them, the arguments are appended rather than dropped — silently
//! discarding what the user typed would be the worst option.
//!
//! **On trust:** skills are content, not code; nothing here executes. But their
//! descriptions enter every session's context, so the catalogue is an injection
//! surface, and it is treated as one: the kernel records what was loaded
//! (`skills.loaded`) and every invocation names the file it came from
//! (`skill.invoked`). A surprising instruction is therefore traceable to a path in
//! the log rather than being an anonymous part of the prompt.

use loong_schema::generated::{ActionClass, Json};
use loong_tools::{ExecContext, Tool, ToolError};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const SKILLS_DIR: &str = "skills";
pub const COMMANDS_DIR: &str = "commands";
pub const SKILL_FILE: &str = "SKILL.md";

#[derive(Debug)]
pub enum SkillError {
    Io(String),
    Invalid(String),
}

impl std::fmt::Display for SkillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkillError::Io(m) => write!(f, "{m}"),
            SkillError::Invalid(m) => write!(f, "skill: {m}"),
        }
    }
}

impl std::error::Error for SkillError {}

/// Where a skill's body lives. MCP prompts are the third case: the description is
/// in hand, the body is fetched on invoke — progressive disclosure, over a wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// A markdown file in the workspace.
    File { source: PathBuf, body: String },
    /// Provided by the agent itself (`/help`).
    Builtin { body: String },
    /// A prompt on an MCP server, with the argument names it declared.
    Mcp {
        server: String,
        prompt: String,
        arguments: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub argument_hint: Option<String>,
    /// Recorded for a later phase (per-skill tool allowlists); not enforced yet.
    pub allowed_tools: Option<String>,
    pub origin: Origin,
}

impl Skill {
    /// The body, when it is already in hand.
    pub fn body(&self) -> Option<&str> {
        match &self.origin {
            Origin::File { body, .. } | Origin::Builtin { body } => Some(body),
            Origin::Mcp { .. } => None,
        }
    }

    /// What to show a human (and the log) as this skill's provenance.
    pub fn source_label(&self) -> String {
        match &self.origin {
            Origin::File { source, .. } => source.display().to_string(),
            Origin::Builtin { .. } => "<builtin>".to_string(),
            Origin::Mcp { server, prompt, .. } => format!("mcp:{server}/{prompt}"),
        }
    }

    /// Rough size of what an invocation will add. An un-fetched MCP prompt has no
    /// known size, and pretending otherwise would be a made-up number.
    pub fn tokens(&self) -> u64 {
        match &self.origin {
            Origin::File { body, .. } | Origin::Builtin { body } => estimate_tokens(body),
            Origin::Mcp { .. } => 0,
        }
    }

    /// What the model is shown: a name, a description and the invocation syntax.
    /// Never the body.
    pub fn catalogue_line(&self) -> String {
        let hint = match &self.argument_hint {
            Some(hint) => format!(" {hint}"),
            None => String::new(),
        };
        let origin = match &self.origin {
            Origin::Builtin { .. } => String::new(),
            Origin::File { source, .. } => format!("  [{}]", source.display()),
            Origin::Mcp { server, prompt, .. } => format!("  [mcp:{server}/{prompt}]"),
        };
        format!("/{}{} — {}{}", self.name, hint, self.description, origin)
    }
}

/// What an invocation resolved to. `prompt` is `None` for an MCP prompt: the
/// caller fetches it (it owns the server connection).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub name: String,
    pub args: String,
    pub source: String,
    pub prompt: Option<String>,
    pub tokens: u64,
}

#[derive(Debug, Clone, Default)]
pub struct SkillSet {
    pub workspace: PathBuf,
    pub skills: Vec<Skill>,
    /// Names config disabled by identity (ADR-0008's add-only escape hatch), kept
    /// so the session can say what it left out rather than silently dropping it.
    pub disabled: Vec<String>,
}

impl SkillSet {
    /// Discover every skill in the workspace. A malformed skill is an error, not a
    /// silently skipped file: a workflow that stopped working should say so.
    pub fn load(workspace: impl AsRef<Path>) -> Result<Self, SkillError> {
        Self::load_in(workspace, &[], &[])
    }

    /// Discover skills from the workspace **and** any extra roots a profile
    /// contributed (ADR-0008 add-only: layers union), minus entries a
    /// lower-authority layer disabled by identity.
    pub fn load_in(
        workspace: impl AsRef<Path>,
        extra_dirs: &[PathBuf],
        disabled: &[String],
    ) -> Result<Self, SkillError> {
        let workspace = workspace.as_ref().to_path_buf();
        let root = workspace.join(".loongagent");
        let mut skills: Vec<Skill> = Vec::new();

        // The workspace's own two spellings, then every extra root in the same two
        // shapes: a directory per skill, or flat markdown files.
        let mut roots: Vec<PathBuf> = vec![root.clone()];
        roots.extend(extra_dirs.iter().cloned());
        for root in roots {
            if !root.is_dir() {
                if root != workspace.join(".loongagent") {
                    return Err(SkillError::Io(format!(
                        "{} is not a directory (a profile's skill_dirs must exist)",
                        root.display()
                    )));
                }
                continue;
            }
            skills.extend(scan_root(&root)?);
        }

        // A disabled entry is removed before anything is built from the set, so the
        // catalogue and `/help` cannot advertise a skill that will not run.
        let mut disabled_hits = Vec::new();
        skills.retain(|skill| {
            if disabled.iter().any(|name| name == &skill.name) {
                disabled_hits.push(skill.name.clone());
                false
            } else {
                true
            }
        });

        let set = SkillSet {
            workspace,
            skills,
            disabled: disabled_hits,
        };
        set.finalize()
    }

    /// Add skills that come from somewhere else (MCP prompts), then rebuild the
    /// catalogue: `/help` must list them too.
    pub fn with_external(mut self, mut extra: Vec<Skill>) -> Result<Self, SkillError> {
        self.skills.append(&mut extra);
        self.finalize()
    }

    fn finalize(mut self) -> Result<Self, SkillError> {
        // `/help` is regenerated from the final set — including skills that arrived
        // after the first pass — so a stale help entry must not survive.
        self.skills
            .retain(|s| !matches!(s.origin, Origin::Builtin { .. }));
        // A duplicate name is a configuration bug, and picking one at random would
        // make `/name` depend on directory order.
        let mut seen: BTreeMap<String, String> = BTreeMap::new();
        for skill in &self.skills {
            if let Some(first) = seen.get(&skill.name) {
                return Err(SkillError::Invalid(format!(
                    "{:?} is defined twice: {first} and {}",
                    skill.name,
                    skill.source_label()
                )));
            }
            seen.insert(skill.name.clone(), skill.source_label());
        }
        let help = self.help_skill();
        self.skills.push(help);
        self.skills.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(self)
    }

    pub fn is_empty(&self) -> bool {
        self.skills
            .iter()
            .all(|s| matches!(s.origin, Origin::Builtin { .. }))
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.name == name)
    }

    pub fn names(&self) -> Vec<&str> {
        self.skills.iter().map(|s| s.name.as_str()).collect()
    }

    /// The description-only view that goes into context.
    pub fn catalogue(&self) -> String {
        self.skills
            .iter()
            .map(|s| s.catalogue_line())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// `/name args` → what to send, or `None` when no such skill exists. An MCP
    /// prompt comes back without its body: the caller owns the connection.
    pub fn invoke(&self, name: &str, args: &str) -> Option<Invocation> {
        let skill = self.get(name)?;
        Some(Invocation {
            name: skill.name.clone(),
            args: args.to_string(),
            source: skill.source_label(),
            prompt: skill.body().map(|body| substitute(body, args)),
            tokens: skill.tokens(),
        })
    }

    fn help_skill(&self) -> Skill {
        let mut body = String::from("The skills and commands available in this workspace are:\n\n");
        for skill in self.skills.iter() {
            body.push_str(&format!("- {}\n", skill.catalogue_line()));
        }
        // `/help` describes itself too: a listing that omits itself is a riddle.
        body.push_str("- /help — list the skills and commands available here\n");
        body.push_str(
            "\nAsk the user which one to run, or run one yourself when it clearly fits the \
             request. Arguments are passed after the name, e.g. `/code-review HEAD~1`.\n",
        );
        Skill {
            name: "help".to_string(),
            description: "list the skills and commands available here".to_string(),
            argument_hint: None,
            allowed_tools: None,
            origin: Origin::Builtin { body },
        }
    }
}

/// `/name the rest` → `("name", "the rest")`.
///
/// Deliberately strict about what counts as an invocation: `/etc/hosts is wrong`
/// is a sentence about a path, not a command, and treating it as one would make a
/// chat message fail for looking like a filename.
pub fn parse_invocation(text: &str) -> Option<(String, String)> {
    let rest = text.trim().strip_prefix('/')?;
    let (name, args) = match rest.split_once(char::is_whitespace) {
        Some((name, args)) => (name, args.trim()),
        None => (rest, ""),
    };
    let looks_like_a_command = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    looks_like_a_command.then(|| (name.to_string(), args.to_string()))
}

fn substitute(body: &str, args: &str) -> String {
    let parts: Vec<&str> = args.split_whitespace().collect();
    let mut used_placeholder = false;
    let mut out = String::with_capacity(body.len() + args.len());
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        if chars.peek() == Some(&'A') {
            let mut word = String::new();
            while let Some(&next) = chars.peek() {
                if next.is_ascii_uppercase() {
                    word.push(next);
                    chars.next();
                } else {
                    break;
                }
            }
            if word == "ARGUMENTS" {
                out.push_str(args);
                used_placeholder = true;
                continue;
            }
            out.push('$');
            out.push_str(&word);
            continue;
        }
        if let Some(&digit) = chars.peek() {
            if digit.is_ascii_digit() && digit != '0' {
                chars.next();
                let index = digit.to_digit(10).unwrap() as usize - 1;
                out.push_str(parts.get(index).copied().unwrap_or(""));
                used_placeholder = true;
                continue;
            }
        }
        out.push('$');
    }
    if !used_placeholder && !args.is_empty() {
        out.push_str("\n\nArguments: ");
        out.push_str(args);
    }
    out
}

/// Every skill file under one root, in both accepted spellings.
fn scan_root(root: &Path) -> Result<Vec<Skill>, SkillError> {
    let mut skills = Vec::new();
    // Both readings are natural, so both work: a root that holds a `skills/`
    // directory (the workspace layout) and a root that *is* a directory of skills
    // (what `skill_dirs = ["~/team-skills"]` reads like).
    for parent in [root.join(SKILLS_DIR), root.to_path_buf()] {
        if !parent.is_dir() {
            continue;
        }
        let mut entries: Vec<_> = std::fs::read_dir(&parent)
            .map_err(|e| SkillError::Io(format!("{}: {e}", parent.display())))?
            .filter_map(|e| e.ok())
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let file = dir.join(SKILL_FILE);
            if !file.is_file() {
                continue;
            }
            let fallback = dir.file_name().map(|n| n.to_string_lossy().to_string());
            skills.push(read_skill(&file, fallback.as_deref())?);
        }
    }
    // `<name>.md` — the older "commands" spelling of the same concept. Accepted at
    // a root directly as well, so an extra profile root can be flat.
    for dir in [root.join(COMMANDS_DIR), root.to_path_buf()] {
        if !dir.is_dir() {
            continue;
        }
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .map_err(|e| SkillError::Io(format!("{}: {e}", dir.display())))?
            .filter_map(|e| e.ok())
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let file = entry.path();
            if file.extension().map(|e| e != "md").unwrap_or(true) {
                continue;
            }
            let fallback = file.file_stem().map(|n| n.to_string_lossy().to_string());
            skills.push(read_skill(&file, fallback.as_deref())?);
        }
    }
    Ok(skills)
}

fn read_skill(path: &Path, fallback_name: Option<&str>) -> Result<Skill, SkillError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| SkillError::Io(format!("{}: {e}", path.display())))?;
    let (front, body) = split_frontmatter(&text).ok_or_else(|| {
        SkillError::Invalid(format!("{}: no `---` frontmatter block", path.display()))
    })?;
    let fields = parse_frontmatter(front)
        .map_err(|e| SkillError::Invalid(format!("{}: {e}", path.display())))?;

    let name = fields
        .get("name")
        .cloned()
        .or_else(|| fallback_name.map(|n| n.to_string()))
        .ok_or_else(|| {
            SkillError::Invalid(format!("{}: no name and no file name", path.display()))
        })?;
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(SkillError::Invalid(format!(
            "{}: {name:?} is not a usable name (letters, digits, - and _ only)",
            path.display()
        )));
    }
    let description = fields.get("description").cloned().ok_or_else(|| {
        SkillError::Invalid(format!(
            "{}: frontmatter needs a `description` — it is the only part that enters context \
             before invocation",
            path.display()
        ))
    })?;

    Ok(Skill {
        name,
        description,
        argument_hint: fields.get("argument-hint").cloned(),
        allowed_tools: fields.get("allowed-tools").cloned(),
        origin: Origin::File {
            source: path.to_path_buf(),
            body: body.to_string(),
        },
    })
}

fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = text.trim_start_matches(['\n', '\r']);
    let rest = rest.strip_prefix("---")?;
    let rest = rest.trim_start_matches(['\n', '\r']);
    let end = rest.find("\n---")?;
    let front = &rest[..end];
    let after = &rest[end + 4..];
    let body = after.trim_start_matches(['\n', '\r']);
    Some((front, body))
}

fn parse_frontmatter(front: &str) -> Result<BTreeMap<String, String>, String> {
    let mut fields = BTreeMap::new();
    for (index, line) in front.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| format!("line {}: {line:?} is not `key: value`", index + 1))?;
        let key = key.trim().to_lowercase();
        let value = value.trim().trim_matches(['"', '\'']).to_string();
        fields.insert(key, value);
    }
    Ok(fields)
}

pub fn estimate_tokens(text: &str) -> u64 {
    (text.chars().count() as u64 + 3) / 4
}

/// Fetching a prompt body from the server that owns it. The skill registry stays
/// ignorant of MCP; whoever owns the connections supplies this.
pub trait PromptFetcher: Send + Sync {
    fn fetch(
        &self,
        server: &str,
        prompt: &str,
        argument_names: &[String],
        args: &str,
    ) -> Result<String, String>;
}

/// The model-invoked path: it sees the catalogue, and asks for a body by name.
pub struct SkillTool {
    skills: std::sync::Arc<SkillSet>,
    fetcher: Option<std::sync::Arc<dyn PromptFetcher>>,
}

impl SkillTool {
    pub fn new(skills: std::sync::Arc<SkillSet>) -> Self {
        Self {
            skills,
            fetcher: None,
        }
    }

    pub fn with_fetcher(
        skills: std::sync::Arc<SkillSet>,
        fetcher: std::sync::Arc<dyn PromptFetcher>,
    ) -> Self {
        Self {
            skills,
            fetcher: Some(fetcher),
        }
    }
}

impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill"
    }
    fn description(&self) -> &str {
        "Load the full instructions of a skill or command from this workspace by name. The \
         catalogue of available names is in the system prompt; this returns the body."
    }
    fn parameters(&self) -> Json {
        json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "a name from the catalogue, without the leading /"}
            },
            "required": ["name"]
        })
    }
    /// Reading a local file: no side effects, nothing outside the workspace.
    fn action_class(&self, _args: &Json, _cwd: &Path) -> ActionClass {
        ActionClass::Investigate
    }
    fn execute(&self, args: &Json, _ctx: &ExecContext<'_>) -> Result<String, ToolError> {
        let name = args
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Refused("missing string argument \"name\"".into()))?;
        let Some(skill) = self.skills.get(name) else {
            return Err(ToolError::Refused(format!(
                "no skill named {name:?}; available: {}",
                self.skills.names().join(", ")
            )));
        };
        // An MCP prompt has no local body: fetch it from the server that owns it.
        let (body, source) = match &skill.origin {
            Origin::File { body, .. } | Origin::Builtin { body } => {
                (body.clone(), skill.source_label())
            }
            Origin::Mcp {
                server,
                prompt,
                arguments,
            } => {
                let Some(fetcher) = &self.fetcher else {
                    return Err(ToolError::Refused(format!(
                        "{name} is provided by MCP server {server:?}, which is not reachable in \
                         this session"
                    )));
                };
                let body = fetcher
                    .fetch(server, prompt, arguments, "")
                    .map_err(|e| ToolError::Failed(format!("fetching {server}/{prompt}: {e}")))?;
                (body, skill.source_label())
            }
        };
        Ok(format!("# Skill: {} (from {source})\n\n{body}", skill.name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    fn workspace() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "loong-skills-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(dir.join(".loongagent/skills")).unwrap();
        std::fs::create_dir_all(dir.join(".loongagent/commands")).unwrap();
        dir
    }

    fn skill_file(ws: &Path, name: &str, body: &str) -> PathBuf {
        let dir = ws.join(".loongagent/skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(SKILL_FILE);
        std::fs::write(&path, body).unwrap();
        path
    }

    fn command_file(ws: &Path, name: &str, body: &str) -> PathBuf {
        let path = ws.join(".loongagent/commands").join(format!("{name}.md"));
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn a_skill_is_discovered_with_its_description_and_body() {
        let ws = workspace();
        skill_file(
            &ws,
            "code-review",
            "---\nname: code-review\ndescription: Review a diff for real bugs\nargument-hint: <ref>\n---\n\nLook at $ARGUMENTS and report only real bugs.\n",
        );
        let set = SkillSet::load(&ws).unwrap();
        let skill = set.get("code-review").expect("discovered");
        assert_eq!(skill.description, "Review a diff for real bugs");
        assert_eq!(skill.argument_hint.as_deref(), Some("<ref>"));
        assert!(skill.body().unwrap().starts_with("Look at $ARGUMENTS"));
        assert!(matches!(skill.origin, Origin::File { .. }));
    }

    #[test]
    fn the_older_commands_shape_is_the_same_concept() {
        let ws = workspace();
        command_file(
            &ws,
            "ship",
            "---\ndescription: Ship the current branch\n---\n\nShip it.\n",
        );
        let set = SkillSet::load(&ws).unwrap();
        assert!(
            set.get("ship").is_some(),
            "names come from the file stem: {:?}",
            set.names()
        );
    }

    #[test]
    fn the_catalogue_carries_descriptions_and_never_bodies() {
        let ws = workspace();
        skill_file(
            &ws,
            "release",
            "---\ndescription: Cut a release\n---\n\nSECRET-BODY-DO-NOT-LEAK\n",
        );
        let set = SkillSet::load(&ws).unwrap();
        let catalogue = set.catalogue();
        assert!(
            catalogue.contains("/release — Cut a release"),
            "{catalogue}"
        );
        assert!(
            !catalogue.contains("SECRET-BODY"),
            "the body stays out of context: {catalogue}"
        );
        // The source is named, so a surprising description is traceable.
        assert!(catalogue.contains("skills/release"), "{catalogue}");
    }

    #[test]
    fn help_is_always_available_and_lists_what_exists() {
        let ws = workspace();
        skill_file(
            &ws,
            "review",
            "---\ndescription: Review something\n---\nbody\n",
        );
        let set = SkillSet::load(&ws).unwrap();
        let help = set.invoke("help", "").expect("/help works");
        assert_eq!(help.source, "<builtin>");
        let prompt = help.prompt.expect("a builtin body is in hand");
        assert!(prompt.contains("/review — Review something"), "{prompt}");
        assert!(prompt.contains("/help"), "{prompt}");
    }

    #[test]
    fn arguments_substitute_all_forms_and_are_never_dropped() {
        let ws = workspace();
        skill_file(&ws, "all", "---\ndescription: d\n---\nARGS=[$ARGUMENTS]\n");
        skill_file(
            &ws,
            "positional",
            "---\ndescription: d\n---\nfirst=$1 second=$2 third=$3\n",
        );
        skill_file(
            &ws,
            "none",
            "---\ndescription: d\n---\nNo placeholder here.\n",
        );
        let set = SkillSet::load(&ws).unwrap();

        let prompt = |name: &str, args: &str| set.invoke(name, args).unwrap().prompt.unwrap();
        assert!(prompt("all", "one two").contains("ARGS=[one two]"));
        assert!(prompt("positional", "a b").contains("first=a second=b third="));
        assert!(prompt("none", "a b").ends_with("Arguments: a b"));
        // With no arguments, nothing is appended.
        assert!(!prompt("none", "").contains("Arguments:"));
        // A lone `$` is not a placeholder and must survive.
        skill_file(
            &ws,
            "dollar",
            "---\ndescription: d\n---\ncost is 5$ today\n",
        );
        let set = SkillSet::load(&ws).unwrap();
        assert!(set
            .invoke("dollar", "")
            .unwrap()
            .prompt
            .unwrap()
            .contains("5$ today"));
    }

    #[test]
    fn an_unknown_name_is_none_not_a_guess() {
        let ws = workspace();
        let set = SkillSet::load(&ws).unwrap();
        assert!(set.invoke("nope", "").is_none());
    }

    #[test]
    fn invocation_syntax_is_parsed_strictly() {
        assert_eq!(
            parse_invocation("/review HEAD~1"),
            Some(("review".into(), "HEAD~1".into()))
        );
        assert_eq!(
            parse_invocation("/help"),
            Some(("help".into(), String::new()))
        );
        assert_eq!(
            parse_invocation("  /a-b_c  x  "),
            Some(("a-b_c".into(), "x".into()))
        );
        assert_eq!(parse_invocation("no slash"), None);
        assert_eq!(parse_invocation("/"), None);
        // A sentence about a path is a sentence, not a command.
        assert_eq!(parse_invocation("/etc/hosts is misconfigured"), None);
        assert_eq!(parse_invocation("/usr/bin/env has a bug"), None);
    }

    #[test]
    fn broken_skills_are_loud_and_specific() {
        let ws = workspace();
        skill_file(&ws, "no-front", "just a body\n");
        assert!(SkillSet::load(&ws)
            .unwrap_err()
            .to_string()
            .contains("frontmatter"));

        let ws = workspace();
        skill_file(&ws, "no-description", "---\nname: x\n---\nbody\n");
        let err = SkillSet::load(&ws).unwrap_err().to_string();
        assert!(err.contains("description"), "{err}");

        let ws = workspace();
        skill_file(
            &ws,
            "bad-name",
            "---\nname: has space\ndescription: d\n---\nbody\n",
        );
        assert!(SkillSet::load(&ws)
            .unwrap_err()
            .to_string()
            .contains("not a usable name"));

        let ws = workspace();
        skill_file(&ws, "dup", "---\nname: same\ndescription: one\n---\nbody\n");
        command_file(
            &ws,
            "other",
            "---\nname: same\ndescription: two\n---\nbody\n",
        );
        let err = SkillSet::load(&ws).unwrap_err().to_string();
        assert!(err.contains("defined twice"), "{err}");
    }

    #[test]
    fn a_quoted_or_spaced_value_survives_the_frontmatter() {
        let ws = workspace();
        skill_file(
            &ws,
            "quoted",
            "---\nname: \"quoted\"\ndescription: 'A description: with a colon'\n---\nbody\n",
        );
        let set = SkillSet::load(&ws).unwrap();
        let skill = set.get("quoted").unwrap();
        assert_eq!(skill.description, "A description: with a colon");
    }

    #[test]
    fn the_model_can_load_a_body_by_name_and_gets_refused_for_a_bad_one() {
        let ws = workspace();
        skill_file(
            &ws,
            "deploy",
            "---\ndescription: Deploy it\n---\nStep one: breathe.\n",
        );
        let skills = std::sync::Arc::new(SkillSet::load(&ws).unwrap());
        let tool = SkillTool::new(skills);
        let ctx = ExecContext::new(&ws, &loong_tools::NoConfinement);

        assert_eq!(tool.name(), "skill");
        assert_eq!(tool.action_class(&json!({}), &ws), ActionClass::Investigate);
        let output = tool.execute(&json!({"name": "deploy"}), &ctx).unwrap();
        assert!(output.contains("Step one: breathe."), "{output}");
        assert!(
            output.contains("skills/deploy"),
            "the source is named: {output}"
        );
        assert!(output.contains("/deploy"), "{output}");

        let err = tool
            .execute(&json!({"name": "absent"}), &ctx)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no skill named"), "{err}");
        assert!(
            err.contains("deploy"),
            "the refusal lists what exists: {err}"
        );
        assert!(matches!(
            tool.execute(&json!({}), &ctx),
            Err(ToolError::Refused(_))
        ));
    }

    #[test]
    fn an_mcp_prompt_is_a_skill_whose_body_is_fetched_on_invoke() {
        let ws = workspace();
        skill_file(
            &ws,
            "local",
            "---\ndescription: A local skill\n---\nlocal body\n",
        );
        let set = SkillSet::load(&ws)
            .unwrap()
            .with_external(vec![Skill {
                name: "mcp__policy__review".to_string(),
                description: "Review a diff (from the policy server)".to_string(),
                argument_hint: Some("<ref>".to_string()),
                allowed_tools: None,
                origin: Origin::Mcp {
                    server: "policy".to_string(),
                    prompt: "review".to_string(),
                    arguments: vec!["ref".to_string()],
                },
            }])
            .unwrap();

        // Description in context, body not — and the origin is named.
        let catalogue = set.catalogue();
        assert!(
            catalogue.contains("/mcp__policy__review <ref> — Review a diff"),
            "{catalogue}"
        );
        assert!(catalogue.contains("[mcp:policy/review]"), "{catalogue}");
        assert!(set.get("mcp__policy__review").unwrap().body().is_none());
        assert_eq!(
            set.get("mcp__policy__review").unwrap().tokens(),
            0,
            "no made-up size"
        );

        // Invoking yields no body but the coordinates to fetch it.
        let invocation = set.invoke("mcp__policy__review", "HEAD~1").unwrap();
        assert!(invocation.prompt.is_none());
        assert_eq!(invocation.source, "mcp:policy/review");
        match &set.get("mcp__policy__review").unwrap().origin {
            Origin::Mcp {
                server,
                prompt,
                arguments,
            } => {
                assert_eq!(server, "policy");
                assert_eq!(prompt, "review");
                assert_eq!(arguments, &vec!["ref".to_string()]);
            }
            other => panic!("expected an MCP origin, got {other:?}"),
        }

        // `/help` lists it, and it is listed for the human too.
        let help = set.invoke("help", "").unwrap().prompt.unwrap();
        assert!(help.contains("/mcp__policy__review"), "{help}");
        assert!(set.names().contains(&"mcp__policy__review"));
    }

    struct FakeFetcher;

    impl PromptFetcher for FakeFetcher {
        fn fetch(
            &self,
            server: &str,
            prompt: &str,
            argument_names: &[String],
            args: &str,
        ) -> Result<String, String> {
            Ok(format!(
                "fetched {server}/{prompt} with {argument_names:?} args={args:?}"
            ))
        }
    }

    #[test]
    fn the_tool_fetches_an_mcp_body_and_says_so_without_a_server() {
        let ws = workspace();
        let set = std::sync::Arc::new(
            SkillSet::load(&ws)
                .unwrap()
                .with_external(vec![Skill {
                    name: "mcp__policy__review".to_string(),
                    description: "Review a diff".to_string(),
                    argument_hint: None,
                    allowed_tools: None,
                    origin: Origin::Mcp {
                        server: "policy".to_string(),
                        prompt: "review".to_string(),
                        arguments: vec!["ref".to_string()],
                    },
                }])
                .unwrap(),
        );
        let ctx = ExecContext::new(&ws, &loong_tools::NoConfinement);

        let with_fetcher = SkillTool::with_fetcher(
            std::sync::Arc::clone(&set),
            std::sync::Arc::new(FakeFetcher),
        );
        let output = with_fetcher
            .execute(&json!({"name": "mcp__policy__review"}), &ctx)
            .unwrap();
        assert!(output.contains("fetched policy/review"), "{output}");
        assert!(output.contains("mcp:policy/review"), "{output}");

        // Without a connection there is no body to give, and pretending otherwise
        // would be worse than refusing.
        let without = SkillTool::new(set);
        let err = without
            .execute(&json!({"name": "mcp__policy__review"}), &ctx)
            .unwrap_err();
        assert!(err.to_string().contains("not reachable"), "{err}");
    }

    #[test]
    fn an_external_skill_cannot_shadow_a_local_one() {
        let ws = workspace();
        skill_file(&ws, "review", "---\ndescription: local\n---\nbody\n");
        let err = SkillSet::load(&ws)
            .unwrap()
            .with_external(vec![Skill {
                name: "review".to_string(),
                description: "from a server".to_string(),
                argument_hint: None,
                allowed_tools: None,
                origin: Origin::Mcp {
                    server: "s".to_string(),
                    prompt: "review".to_string(),
                    arguments: vec![],
                },
            }])
            .unwrap_err()
            .to_string();
        assert!(err.contains("defined twice"), "{err}");
    }

    #[test]
    fn extra_roots_are_unioned_and_disabled_names_are_reported() {
        let ws = workspace();
        skill_file(&ws, "local", "---\ndescription: Local\n---\nbody\n");
        let team = ws.join("team-skills");
        std::fs::create_dir_all(team.join("from-team")).unwrap();
        std::fs::write(
            team.join("from-team/SKILL.md"),
            "---\ndescription: From the team profile\n---\nteam body\n",
        )
        .unwrap();
        std::fs::write(
            team.join("flat.md"),
            "---\ndescription: A flat one\n---\nflat body\n",
        )
        .unwrap();

        let set = SkillSet::load_in(&ws, &[team.clone()], &["from-team".to_string()]).unwrap();
        assert!(set.names().contains(&"local"));
        assert!(set.names().contains(&"flat"), "{:?}", set.names());
        assert!(
            !set.names().contains(&"from-team"),
            "a disabled name is gone from the set: {:?}",
            set.names()
        );
        assert_eq!(set.disabled, vec!["from-team".to_string()]);
        assert!(
            !set.catalogue().contains("from-team"),
            "and from the catalogue, so /help cannot advertise it: {}",
            set.catalogue()
        );
    }

    #[test]
    fn a_profile_root_that_does_not_exist_is_loud() {
        let ws = workspace();
        let err = SkillSet::load_in(&ws, &[ws.join("nope")], &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("must exist"), "{err}");
    }

    #[test]
    fn an_empty_workspace_still_offers_help() {
        let ws = workspace();
        let set = SkillSet::load(&ws).unwrap();
        assert!(set.is_empty(), "only built-ins");
        assert_eq!(set.names(), vec!["help"]);
        assert!(set.catalogue().contains("/help"));
    }
}
