//! MCP server configuration, behind the same trust gate as hooks.
//!
//! A server entry names a command to run, which makes `.loongagent/mcp.toml`
//! repository content that executes — so it stays inert until its exact bytes are
//! trusted (`loongagent mcp trust`).
//!
//! **Action classes are declared, not guessed.** The kernel cannot know what an
//! external tool does, so the operator says: `action_class` per server, with
//! optional per-tool overrides, defaulting to the cautious `out_of_bounds` (which
//! asks every time). A server that never asked would be a hole in the boundary.

use loong_schema::generated::ActionClass;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const MCP_DIR: &str = ".loongagent";
pub const MCP_FILE: &str = "mcp.toml";
pub const MCP_TRUST_FILE: &str = "mcp.trusted";
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const MAX_TIMEOUT_MS: u64 = 600_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerSpec {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// The class for tools this server exposes, unless overridden per tool.
    pub action_class: ActionClass,
    pub tools: BTreeMap<String, ActionClass>,
    pub timeout: Duration,
    /// `false` turns "this server is unavailable" into a note instead of a
    /// refusal to start the session.
    pub required: bool,
}

impl McpServerSpec {
    pub fn class_for(&self, tool: &str) -> ActionClass {
        self.tools.get(tool).copied().unwrap_or(self.action_class)
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Io(String),
    Parse(String),
    Invalid(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io(m) => write!(f, "{m}"),
            ConfigError::Parse(m) => write!(f, "mcp.toml: {m}"),
            ConfigError::Invalid(m) => write!(f, "mcp.toml: {m}"),
        }
    }
}

impl std::error::Error for ConfigError {}

#[derive(Debug, Clone)]
pub struct McpSet {
    pub workspace: PathBuf,
    pub dir: PathBuf,
    pub servers: Vec<McpServerSpec>,
    pub trust: loong_manifest::TrustStatus,
}

impl McpSet {
    pub fn load(workspace: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let workspace = workspace.as_ref().to_path_buf();
        let dir = workspace.join(MCP_DIR);
        let manifest = dir.join(MCP_FILE);
        if !manifest.exists() {
            return Ok(Self {
                workspace,
                dir,
                servers: Vec::new(),
                trust: loong_manifest::TrustStatus::Absent,
            });
        }
        let text = std::fs::read_to_string(&manifest)
            .map_err(|e| ConfigError::Io(format!("{}: {e}", manifest.display())))?;
        let file: RawFile = toml::from_str(&text).map_err(|e| ConfigError::Parse(e.to_string()))?;

        let mut servers = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for (index, raw) in file.server.iter().enumerate() {
            let where_ = raw.name.clone().unwrap_or_else(|| format!("#{}", index + 1));
            let name = raw
                .name
                .clone()
                .ok_or_else(|| ConfigError::Invalid(format!("{where_}: a server needs a name")))?;
            if !seen.insert(name.clone()) {
                return Err(ConfigError::Invalid(format!("{name}: duplicate server name")));
            }
            if name.contains("__") || name.contains(' ') {
                return Err(ConfigError::Invalid(format!(
                    "{name}: a server name may not contain \"__\" or spaces (it namespaces tool names)"
                )));
            }
            let command = raw.command.clone().filter(|c| !c.trim().is_empty()).ok_or_else(|| {
                ConfigError::Invalid(format!("{name}: a server needs a command"))
            })?;

            let action_class = parse_class(&name, raw.action_class.as_deref().unwrap_or("out_of_bounds"))?;
            let mut tools = BTreeMap::new();
            for (tool, class) in &raw.tools {
                tools.insert(tool.clone(), parse_class(&name, class)?);
            }
            let timeout = Duration::from_millis(
                raw.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS).min(MAX_TIMEOUT_MS),
            );

            servers.push(McpServerSpec {
                name,
                command,
                args: raw.args.clone().unwrap_or_default(),
                env: raw.env.clone().unwrap_or_default(),
                action_class,
                tools,
                timeout,
                required: raw.required.unwrap_or(true),
            });
        }

        let sources = sources_of(&dir, &manifest, &servers);
        let trust = loong_manifest::check(&dir.join(MCP_TRUST_FILE), &sources)
            .map_err(ConfigError::Io)?;
        Ok(Self { workspace, dir, servers, trust })
    }

    /// The servers that may actually run: none until the bytes are trusted.
    pub fn active(&self) -> &[McpServerSpec] {
        match self.trust {
            loong_manifest::TrustStatus::Trusted => &self.servers,
            _ => &[],
        }
    }

    pub fn trust_now(&self) -> Result<PathBuf, ConfigError> {
        let sources = sources_of(&self.dir, &self.dir.join(MCP_FILE), &self.servers);
        let path = self.dir.join(MCP_TRUST_FILE);
        loong_manifest::record(&path, &sources).map_err(ConfigError::Io)?;
        Ok(path)
    }

    pub fn summary(&self) -> String {
        match &self.trust {
            loong_manifest::TrustStatus::Absent => "no MCP servers".to_string(),
            loong_manifest::TrustStatus::Trusted => {
                format!("{} MCP server(s), trusted", self.servers.len())
            }
            loong_manifest::TrustStatus::Untrusted { changed } => {
                loong_manifest::inert_message(&loong_manifest::Sources::new(), changed, "run `loongagent mcp trust`")
            }
        }
    }
}

fn sources_of(
    dir: &Path,
    manifest: &Path,
    servers: &[McpServerSpec],
) -> loong_manifest::Sources {
    let _ = (dir, servers);
    vec![(MCP_FILE.to_string(), manifest.to_path_buf())]
}

fn parse_class(server: &str, text: &str) -> Result<ActionClass, ConfigError> {
    match text {
        "investigate" => Ok(ActionClass::Investigate),
        "write_workspace" => Ok(ActionClass::WriteWorkspace),
        "out_of_bounds" => Ok(ActionClass::OutOfBounds),
        other => Err(ConfigError::Invalid(format!(
            "{server}: unknown action_class {other:?} \
             (investigate | write_workspace | out_of_bounds)"
        ))),
    }
}

#[derive(Deserialize)]
struct RawFile {
    #[serde(default)]
    server: Vec<RawServer>,
}

#[derive(Deserialize, Default)]
struct RawServer {
    name: Option<String>,
    command: Option<String>,
    args: Option<Vec<String>>,
    env: Option<BTreeMap<String, String>>,
    action_class: Option<String>,
    #[serde(default)]
    tools: BTreeMap<String, String>,
    timeout_ms: Option<u64>,
    required: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    fn workspace() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "loong-mcp-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(dir.join(MCP_DIR)).unwrap();
        dir
    }

    fn write(ws: &Path, body: &str) {
        std::fs::write(ws.join(MCP_DIR).join(MCP_FILE), body).unwrap();
    }

    #[test]
    fn no_manifest_means_no_servers() {
        let ws = workspace();
        let set = McpSet::load(&ws).unwrap();
        assert!(set.servers.is_empty());
        assert!(set.active().is_empty());
        assert_eq!(set.summary(), "no MCP servers");
    }

    #[test]
    fn a_server_is_inert_until_trusted() {
        let ws = workspace();
        write(
            &ws,
            "[[server]]\nname = \"fs\"\ncommand = \"node\"\nargs = [\"server.js\"]\n",
        );
        let set = McpSet::load(&ws).unwrap();
        assert_eq!(set.servers.len(), 1);
        assert_eq!(set.servers[0].command, "node");
        assert_eq!(set.servers[0].args, vec!["server.js".to_string()]);
        assert!(set.active().is_empty(), "untrusted servers never start");
        assert!(set.summary().contains("INERT"));

        set.trust_now().unwrap();
        let trusted = McpSet::load(&ws).unwrap();
        assert_eq!(trusted.active().len(), 1);
        assert!(trusted.summary().contains("trusted"));
    }

    #[test]
    fn the_default_action_class_is_the_cautious_one() {
        let ws = workspace();
        write(&ws, "[[server]]\nname = \"fs\"\ncommand = \"node\"\n");
        let set = McpSet::load(&ws).unwrap();
        assert_eq!(set.servers[0].action_class, ActionClass::OutOfBounds);
        assert_eq!(set.servers[0].class_for("anything"), ActionClass::OutOfBounds);
        assert!(set.servers[0].required, "a configured server is required by default");
        assert_eq!(set.servers[0].timeout, Duration::from_millis(DEFAULT_TIMEOUT_MS));
    }

    #[test]
    fn per_tool_overrides_beat_the_server_default() {
        let ws = workspace();
        write(
            &ws,
            "[[server]]\nname = \"fs\"\ncommand = \"node\"\naction_class = \"investigate\"\n\
             [server.tools]\nwrite_file = \"out_of_bounds\"\n",
        );
        let set = McpSet::load(&ws).unwrap();
        let spec = &set.servers[0];
        assert_eq!(spec.class_for("read_file"), ActionClass::Investigate);
        assert_eq!(spec.class_for("write_file"), ActionClass::OutOfBounds);
        assert_eq!(spec.class_for("unknown"), ActionClass::Investigate);
    }

    #[test]
    fn broken_manifests_are_loud_and_specific() {
        let ws = workspace();

        write(&ws, "[[server]]\ncommand = \"node\"\n");
        assert!(McpSet::load(&ws).unwrap_err().to_string().contains("needs a name"));

        write(&ws, "[[server]]\nname = \"a b\"\ncommand = \"node\"\n");
        assert!(McpSet::load(&ws).unwrap_err().to_string().contains("namespace"));

        write(&ws, "[[server]]\nname = \"a\"\n");
        assert!(McpSet::load(&ws).unwrap_err().to_string().contains("needs a command"));

        write(&ws, "[[server]]\nname = \"a\"\ncommand = \"node\"\naction_class = \"whatever\"\n");
        assert!(McpSet::load(&ws).unwrap_err().to_string().contains("unknown action_class"));

        write(
            &ws,
            "[[server]]\nname = \"a\"\ncommand = \"node\"\n\n[[server]]\nname = \"a\"\ncommand = \"node\"\n",
        );
        assert!(McpSet::load(&ws).unwrap_err().to_string().contains("duplicate"));

        write(&ws, "not toml {{{");
        assert!(matches!(McpSet::load(&ws).unwrap_err(), ConfigError::Parse(_)));
    }

    #[test]
    fn editing_the_manifest_re_arms_the_question() {
        let ws = workspace();
        write(&ws, "[[server]]\nname = \"a\"\ncommand = \"node\"\n");
        McpSet::load(&ws).unwrap().trust_now().unwrap();
        assert_eq!(McpSet::load(&ws).unwrap().trust, loong_manifest::TrustStatus::Trusted);

        write(&ws, "[[server]]\nname = \"a\"\ncommand = \"node\"\nargs = [\"--evil\"]\n");
        match McpSet::load(&ws).unwrap().trust {
            loong_manifest::TrustStatus::Untrusted { changed } => {
                assert_eq!(changed, vec![MCP_FILE.to_string()])
            }
            other => panic!("expected untrusted, got {other:?}"),
        }
    }
}
