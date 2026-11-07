//! Purpose: Read `mcp.toml` and keep its servers inert until a human trusts the manifest.
//!
//! # Contract
//! - **Serves:** the kernel's `attach_mcp` and the CLI's `mcp` command.
//! - **Design:** `docs/design/mcp-client.md` §1 and `docs/design/security-trust.md` §1.
//! - **Upstream:** `crates/kernel/src/turn/build.rs` and `crates/cli/src/mcp.rs`.
//! - **Downstream:** `loong_manifest` for the hash and trust check, `loong_schema::ActionClass`.
//! - **Verify:** `cargo test -p loong-mcp` · `./scripts/check.sh`
//!
//! MCP server configuration, behind the same trust gate as hooks.
//!
//! A server entry names exactly one transport: a command to run, or a URL to call.
//! Both make `.loongagent/mcp.toml` into something that reaches outside the
//! session, so it stays inert until its exact bytes are trusted
//! (`loongagent mcp trust`).
//!
//! **Action classes are declared, not guessed.** The kernel cannot know what an
//! external tool does, so the operator says: `action_class` per server, with
//! optional per-tool overrides, defaulting to the cautious `out_of_bounds` (which
//! asks every time). A server that never asked would be a hole in the boundary.
//!
//! **A URL is an egress path,** so it is narrower than a command: `https://`
//! always, plain `http://` only to this machine (loopback), and no redirects —
//! following one would let a trusted local endpoint hand the session to a host
//! the manifest never named.

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

/// When a server's tools are advertised through `tool_search` instead of being
/// pasted into every request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// Purpose: The `defer` policy: whether a server's tools enter the request directly.
/// Serves: `attach_mcp`, which registers the tools either directly or deferred.
/// Design: `docs/design/mcp-client.md` §6.
/// Up: `McpSet::load` parses it from `defer = "…"`.
/// Down: `attach_mcp` in `crates/kernel/src/turn/build.rs`.
/// Verify: `cargo test -p loong-mcp`.
pub enum Defer {
    /// Defer only when the server is expensive: many tools or a large schema.
    #[default]
    Auto,
    /// Always defer, however small the server is.
    Always,
    /// Never defer: the tools are always in the request.
    Never,
}

impl Defer {
    /// Purpose: Whether this policy defers a server with the given measurements.
    /// Serves: `attach_mcp`.
    /// Design: `docs/design/mcp-client.md` §6.
    /// Up: `crates/kernel/src/turn/build.rs`.
    /// Down: the returned verdict.
    /// Verify: `cargo test -p loong-mcp`.
    pub fn applies(self, expensive: bool) -> bool {
        match self {
            Self::Auto => expensive,
            Self::Always => true,
            Self::Never => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Purpose: One server entry: its transport, action classes, deadline and defer policy.
/// Serves: `McpSet`, `McpClient::start` and `McpTool`.
/// Design: `docs/design/mcp-client.md` §1.
/// Up: `McpSet::load` parses it from `mcp.toml`.
/// Down: `McpClient::start`, which copies its fields, and `class_for`, which `attach_mcp` calls.
/// Verify: `cargo test -p loong-mcp`.
pub struct McpServerSpec {
    pub name: String,
    /// The stdio transport's program; empty when `url` names the transport instead.
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// The HTTP transport's endpoint; `None` for a stdio server.
    pub url: Option<String>,
    /// Extra request headers for the HTTP transport, with `${VAR}` already expanded.
    pub headers: BTreeMap<String, String>,
    /// The class for tools this server exposes, unless overridden per tool.
    pub action_class: ActionClass,
    pub tools: BTreeMap<String, ActionClass>,
    pub timeout: Duration,
    /// `false` turns "this server is unavailable" into a note instead of a
    /// refusal to start the session.
    pub required: bool,
    pub defer: Defer,
}

impl McpServerSpec {
    /// Purpose: The action class for one tool: its override, else the server default.
    /// Serves: the kernel's `attach_mcp` when it registers a tool.
    /// Design: `docs/design/mcp-client.md` §1.
    /// Up: `crates/kernel/src/turn/build.rs`.
    /// Down: the `tools` map and `action_class` field.
    /// Verify: `cargo test -p loong-mcp`.
    pub fn class_for(&self, tool: &str) -> ActionClass {
        self.tools.get(tool).copied().unwrap_or(self.action_class)
    }

    /// Purpose: A one-word transport name for `mcp list` and startup notes.
    /// Serves: `crates/cli/src/mcp.rs` and the kernel's startup note.
    /// Design: `docs/design/mcp-client.md` §1.
    /// Up: `crates/cli/src/mcp.rs`.
    /// Down: the `url` and `command` fields.
    /// Verify: `cargo test -p loong-mcp`.
    pub fn transport(&self) -> &'static str {
        if self.url.is_some() {
            "http"
        } else {
            "stdio"
        }
    }

    /// Purpose: The endpoint or command line, for display only.
    /// Serves: `crates/cli/src/mcp.rs`, which lists what would run.
    /// Design: `docs/design/mcp-client.md` §1.
    /// Up: `crates/cli/src/mcp.rs`.
    /// Down: the `url`, `command` and `args` fields.
    /// Verify: `cargo test -p loong-mcp`.
    pub fn endpoint(&self) -> String {
        match &self.url {
            Some(url) => url.clone(),
            None => std::iter::once(self.command.clone())
                .chain(self.args.iter().cloned())
                .collect::<Vec<_>>()
                .join(" "),
        }
    }
}

#[derive(Debug)]
/// Purpose: Why `mcp.toml` could not be read, parsed or accepted.
/// Serves: `McpSet::load` and the CLI's error output.
/// Design: `docs/design/mcp-client.md` §1.
/// Up: `McpSet::load` constructs it.
/// Down: its `Display` impl.
/// Verify: `cargo test -p loong-mcp`.
pub enum ConfigError {
    Io(String),
    Parse(String),
    Invalid(String),
}

impl std::fmt::Display for ConfigError {
    /// Purpose: Prefix parse and validation errors with `mcp.toml`.
    /// Contract: `crates/mcp/src/config.rs`.
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
/// Purpose: The parsed manifest plus its trust verdict.
/// Serves: the kernel's `attach_mcp` and the CLI's `mcp` command.
/// Design: `docs/design/mcp-client.md` §1.
/// Up: `McpSet::load`.
/// Down: `active`, `trust_now` and `summary`.
/// Verify: `cargo test -p loong-mcp`.
pub struct McpSet {
    pub workspace: PathBuf,
    pub dir: PathBuf,
    pub servers: Vec<McpServerSpec>,
    pub trust: loong_manifest::TrustStatus,
}

impl McpSet {
    /// Purpose: Parse the manifest and check its trust record, with no manifest meaning no servers.
    /// Serves: `crates/kernel/src/turn/build.rs`, `crates/cli/src/mcp.rs` and `crates/cli/src/server.rs`.
    /// Design: `docs/design/mcp-client.md` §1.
    /// Up: those three callers.
    /// Down: `loong_manifest::check`, `parse_class` and `expand`.
    /// Verify: `cargo test -p loong-mcp`.
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
            let where_ = raw
                .name
                .clone()
                .unwrap_or_else(|| format!("#{}", index + 1));
            let name = raw
                .name
                .clone()
                .ok_or_else(|| ConfigError::Invalid(format!("{where_}: a server needs a name")))?;
            if !seen.insert(name.clone()) {
                return Err(ConfigError::Invalid(format!(
                    "{name}: duplicate server name"
                )));
            }
            if name.contains("__") || name.contains(' ') {
                return Err(ConfigError::Invalid(format!(
                    "{name}: a server name may not contain \"__\" or spaces (it namespaces tool names)"
                )));
            }
            let command = raw
                .command
                .clone()
                .filter(|c| !c.trim().is_empty())
                .unwrap_or_default();
            let url = raw
                .url
                .clone()
                .filter(|u| !u.trim().is_empty())
                .map(|url| check_url(&name, &url))
                .transpose()?;
            match (command.is_empty(), url.is_some()) {
                (true, false) => {
                    return Err(ConfigError::Invalid(format!(
                        "{name}: a server needs a command or a url"
                    )))
                }
                (false, true) => {
                    return Err(ConfigError::Invalid(format!(
                        "{name}: a server has a command and a url; name one transport"
                    )))
                }
                _ => {}
            }
            let headers = match (&url, &raw.headers) {
                (Some(_), Some(headers)) => expand_headers(&name, headers)?,
                (Some(_), None) => BTreeMap::new(),
                (None, Some(_)) => {
                    return Err(ConfigError::Invalid(format!(
                        "{name}: headers belong to a url transport; a command server has env"
                    )))
                }
                (None, None) => BTreeMap::new(),
            };

            let action_class = parse_class(
                &name,
                raw.action_class.as_deref().unwrap_or("out_of_bounds"),
            )?;
            let mut tools = BTreeMap::new();
            for (tool, class) in &raw.tools {
                tools.insert(tool.clone(), parse_class(&name, class)?);
            }
            let timeout = Duration::from_millis(
                raw.timeout_ms
                    .unwrap_or(DEFAULT_TIMEOUT_MS)
                    .min(MAX_TIMEOUT_MS),
            );

            servers.push(McpServerSpec {
                name,
                command,
                args: raw.args.clone().unwrap_or_default(),
                env: raw.env.clone().unwrap_or_default(),
                url,
                headers,
                action_class,
                tools,
                timeout,
                required: raw.required.unwrap_or(true),
                defer: parse_defer(&where_, raw.defer.as_deref())?,
            });
        }

        let sources = sources_of(&dir, &manifest, &servers);
        let trust =
            loong_manifest::check(&dir.join(MCP_TRUST_FILE), &sources).map_err(ConfigError::Io)?;
        Ok(Self {
            workspace,
            dir,
            servers,
            trust,
        })
    }

    /// The servers that may actually run: none until the bytes are trusted.
    /// Purpose: The servers that may run: none until the bytes are trusted.
    /// Serves: the kernel's `attach_mcp`.
    /// Design: `docs/design/mcp-client.md` §1.
    /// Up: `crates/kernel/src/turn/build.rs`.
    /// Down: the `servers` field, gated by `trust`.
    /// Verify: `cargo test -p loong-mcp`.
    pub fn active(&self) -> &[McpServerSpec] {
        match self.trust {
            loong_manifest::TrustStatus::Trusted => &self.servers,
            _ => &[],
        }
    }

    /// Purpose: Record the current manifest bytes as trusted.
    /// Serves: `loongagent mcp trust`.
    /// Design: `docs/design/mcp-client.md` §1.
    /// Up: `crates/cli/src/mcp.rs` and tests.
    /// Down: `loong_manifest::record`.
    /// Verify: `cargo test -p loong-mcp`.
    pub fn trust_now(&self) -> Result<PathBuf, ConfigError> {
        let sources = sources_of(&self.dir, &self.dir.join(MCP_FILE), &self.servers);
        let path = self.dir.join(MCP_TRUST_FILE);
        loong_manifest::record(&path, &sources).map_err(ConfigError::Io)?;
        Ok(path)
    }

    /// Purpose: A one-line trust summary for startup logs and `mcp list`.
    /// Serves: `crates/cli/src/mcp.rs`, `crates/cli/src/server.rs` and the kernel's `attach_mcp` startup note.
    /// Design: `docs/design/mcp-client.md` §1.
    /// Up: `crates/cli/src/mcp.rs`, `crates/cli/src/server.rs` and `crates/kernel/src/turn/build.rs`.
    /// Down: `trust` and `loong_manifest::inert_message`.
    /// Verify: `cargo test -p loong-mcp`.
    pub fn summary(&self) -> String {
        match &self.trust {
            loong_manifest::TrustStatus::Absent => "no MCP servers".to_string(),
            loong_manifest::TrustStatus::Trusted => {
                format!("{} MCP server(s), trusted", self.servers.len())
            }
            loong_manifest::TrustStatus::Untrusted { changed } => loong_manifest::inert_message(
                &loong_manifest::Sources::new(),
                changed,
                "run `loongagent mcp trust`",
            ),
        }
    }
}

/// Purpose: The one file whose bytes this trust record covers: `mcp.toml`.
/// Contract: `crates/mcp/src/config.rs`.
fn sources_of(dir: &Path, manifest: &Path, servers: &[McpServerSpec]) -> loong_manifest::Sources {
    let _ = (dir, servers);
    vec![(MCP_FILE.to_string(), manifest.to_path_buf())]
}

/// Purpose: Accept `https://`, or plain `http://` for this machine only.
/// Serves: `McpSet::load`, so a token never crosses a wire in clear text.
/// Design: `docs/design/mcp-client.md` §1.
/// Up: `McpSet::load`.
/// Down: the `url` field of one server entry.
/// Verify: `cargo test -p loong-mcp config::tests::the_url_rule_is_https_or_loopback_http`.
fn check_url(server: &str, url: &str) -> Result<String, ConfigError> {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("https://") {
        return Ok(url.to_string());
    }
    if let Some(rest) = lower.strip_prefix("http://") {
        let host = rest
            .split(['/', '?', '#'])
            .next()
            .unwrap_or("")
            .rsplit('@')
            .next()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("");
        if matches!(host, "127.0.0.1" | "::1" | "[::1]" | "localhost") {
            return Ok(url.to_string());
        }
        return Err(ConfigError::Invalid(format!(
            "{server}: {url} is plain http to a remote host; use https (plain http is allowed only to loopback)"
        )));
    }
    Err(ConfigError::Invalid(format!(
        "{server}: {url} needs https:// (or http:// to loopback)"
    )))
}

/// Purpose: Expand `${VAR}` in header values from the environment, naming a missing one.
/// Serves: `McpSet::load`, so a secret reaches a header without sitting in `mcp.toml`.
/// Design: `docs/design/mcp-client.md` §1.
/// Up: `McpSet::load` (via the HTTP server entry's `headers` table).
/// Down: `std::env::var` for each `${VAR}` placeholder.
/// Verify: `cargo test -p loong-mcp config::tests::headers_expand_the_environment_and_fail_loudly`.
fn expand_headers(
    server: &str,
    raw: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, ConfigError> {
    let mut out = BTreeMap::new();
    for (name, value) in raw {
        if name.contains(['\r', '\n', ':']) {
            return Err(ConfigError::Invalid(format!(
                "{server}: {name:?} is not a header name"
            )));
        }
        let mut expanded = String::new();
        let mut rest = value.as_str();
        while let Some(start) = rest.find("${") {
            expanded.push_str(&rest[..start]);
            let tail = &rest[start + 2..];
            let end = tail.find('}').ok_or_else(|| {
                ConfigError::Invalid(format!(
                    "{server}: {name}: unclosed ${{ in the header value"
                ))
            })?;
            let var = &tail[..end];
            let found = std::env::var(var).map_err(|_| {
                ConfigError::Invalid(format!(
                    "{server}: {name} uses ${{{var}}}, which is not set in the environment"
                ))
            })?;
            // The value is secret by assumption, so the error names the variable and
            // never the value it was supposed to hold.
            expanded.push_str(&found);
            rest = &tail[end + 1..];
        }
        expanded.push_str(rest);
        if expanded.contains(['\r', '\n']) {
            return Err(ConfigError::Invalid(format!(
                "{server}: {name}: the expanded header value contains a newline"
            )));
        }
        out.insert(name.clone(), expanded);
    }
    Ok(out)
}

/// Purpose: Parse an action-class name, naming the accepted set on failure.
/// Contract: `crates/mcp/src/config.rs`.
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

/// Purpose: Parse a `defer` value, naming the accepted set on failure.
/// Contract: `crates/mcp/src/config.rs`.
fn parse_defer(server: &str, text: Option<&str>) -> Result<Defer, ConfigError> {
    match text {
        None | Some("auto") => Ok(Defer::Auto),
        Some("always") => Ok(Defer::Always),
        Some("never") => Ok(Defer::Never),
        Some(other) => Err(ConfigError::Invalid(format!(
            "{server}: unknown defer {other:?} (auto | always | never)"
        ))),
    }
}

#[derive(Deserialize)]
/// Purpose: The TOML shape: a table of `server` entries.
/// Serves: `McpSet::load`.
/// Design: `docs/design/mcp-client.md` §1.
/// Up: `toml::from_str` in `load`.
/// Down: `RawServer`.
/// Verify: `cargo test -p loong-mcp`.
struct RawFile {
    #[serde(default)]
    server: Vec<RawServer>,
}

#[derive(Deserialize, Default)]
/// Purpose: One `[[server]]` table before validation.
/// Serves: `RawFile` and `McpSet::load`.
/// Design: `docs/design/mcp-client.md` §1.
/// Up: `RawFile`.
/// Down: `McpSet::load`'s validation.
/// Verify: `cargo test -p loong-mcp`.
struct RawServer {
    name: Option<String>,
    command: Option<String>,
    args: Option<Vec<String>>,
    env: Option<BTreeMap<String, String>>,
    url: Option<String>,
    headers: Option<BTreeMap<String, String>>,
    action_class: Option<String>,
    #[serde(default)]
    tools: BTreeMap<String, String>,
    timeout_ms: Option<u64>,
    required: Option<bool>,
    defer: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    static N: AtomicU64 = AtomicU64::new(0);

    /// Purpose: Serialise the tests that set a process-wide environment variable.
    /// Contract: `crates/mcp/src/config.rs`.
    static ENV: Mutex<()> = Mutex::new(());

    /// Purpose: A fresh temporary workspace carrying a `.loongagent` directory.
    /// Contract: `crates/mcp/src/config.rs`.
    fn workspace() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "loong-mcp-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(dir.join(MCP_DIR)).unwrap();
        dir
    }

    /// Purpose: Write an `mcp.toml` body into a test workspace.
    /// Contract: `crates/mcp/src/config.rs`.
    fn write(ws: &Path, body: &str) {
        std::fs::write(ws.join(MCP_DIR).join(MCP_FILE), body).unwrap();
    }

    #[test]
    /// Purpose: Pin that no manifest means no servers and the "no MCP servers" summary.
    fn no_manifest_means_no_servers() {
        let ws = workspace();
        let set = McpSet::load(&ws).unwrap();
        assert!(set.servers.is_empty());
        assert!(set.active().is_empty());
        assert_eq!(set.summary(), "no MCP servers");
    }

    #[test]
    /// Purpose: Pin that a configured server is inert until trusted and runs after `trust_now`.
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
        assert_eq!(set.servers[0].transport(), "stdio");
        assert!(set.active().is_empty(), "untrusted servers never start");
        assert!(set.summary().contains("INERT"));

        set.trust_now().unwrap();
        let trusted = McpSet::load(&ws).unwrap();
        assert_eq!(trusted.active().len(), 1);
        assert!(trusted.summary().contains("trusted"));
    }

    #[test]
    /// Purpose: Pin that an omitted action class defaults to `out_of_bounds`, a server is required, and the timeout is capped.
    fn the_default_action_class_is_the_cautious_one() {
        let ws = workspace();
        write(&ws, "[[server]]\nname = \"fs\"\ncommand = \"node\"\n");
        let set = McpSet::load(&ws).unwrap();
        assert_eq!(set.servers[0].action_class, ActionClass::OutOfBounds);
        assert_eq!(
            set.servers[0].class_for("anything"),
            ActionClass::OutOfBounds
        );
        assert!(
            set.servers[0].required,
            "a configured server is required by default"
        );
        assert_eq!(
            set.servers[0].timeout,
            Duration::from_millis(DEFAULT_TIMEOUT_MS)
        );
        assert_eq!(set.servers[0].defer, Defer::Auto);
    }

    #[test]
    /// Purpose: Pin that a per-tool override beats the server default.
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
    /// Purpose: Pin the loud, specific errors for a broken manifest.
    fn broken_manifests_are_loud_and_specific() {
        let ws = workspace();

        write(&ws, "[[server]]\ncommand = \"node\"\n");
        assert!(McpSet::load(&ws)
            .unwrap_err()
            .to_string()
            .contains("needs a name"));

        write(&ws, "[[server]]\nname = \"a b\"\ncommand = \"node\"\n");
        assert!(McpSet::load(&ws)
            .unwrap_err()
            .to_string()
            .contains("namespace"));

        write(&ws, "[[server]]\nname = \"a\"\n");
        assert!(McpSet::load(&ws)
            .unwrap_err()
            .to_string()
            .contains("needs a command or a url"));

        write(
            &ws,
            "[[server]]\nname = \"a\"\ncommand = \"node\"\nurl = \"https://example.com/mcp\"\n",
        );
        assert!(McpSet::load(&ws)
            .unwrap_err()
            .to_string()
            .contains("name one transport"));

        write(
            &ws,
            "[[server]]\nname = \"a\"\ncommand = \"node\"\naction_class = \"whatever\"\n",
        );
        assert!(McpSet::load(&ws)
            .unwrap_err()
            .to_string()
            .contains("unknown action_class"));

        write(
            &ws,
            "[[server]]\nname = \"a\"\ncommand = \"node\"\ndefer = \"sometimes\"\n",
        );
        assert!(McpSet::load(&ws)
            .unwrap_err()
            .to_string()
            .contains("unknown defer"));

        write(
            &ws,
            "[[server]]\nname = \"a\"\ncommand = \"node\"\n\n[[server]]\nname = \"a\"\ncommand = \"node\"\n",
        );
        assert!(McpSet::load(&ws)
            .unwrap_err()
            .to_string()
            .contains("duplicate"));

        write(&ws, "not toml {{{");
        assert!(matches!(
            McpSet::load(&ws).unwrap_err(),
            ConfigError::Parse(_)
        ));
    }

    #[test]
    /// Purpose: Pin that editing the manifest re-arms the trust question.
    fn editing_the_manifest_re_arms_the_question() {
        let ws = workspace();
        write(&ws, "[[server]]\nname = \"a\"\ncommand = \"node\"\n");
        McpSet::load(&ws).unwrap().trust_now().unwrap();
        assert_eq!(
            McpSet::load(&ws).unwrap().trust,
            loong_manifest::TrustStatus::Trusted
        );

        write(
            &ws,
            "[[server]]\nname = \"a\"\ncommand = \"node\"\nargs = [\"--evil\"]\n",
        );
        match McpSet::load(&ws).unwrap().trust {
            loong_manifest::TrustStatus::Untrusted { changed } => {
                assert_eq!(changed, vec![MCP_FILE.to_string()])
            }
            other => panic!("expected untrusted, got {other:?}"),
        }
    }

    #[test]
    /// Purpose: Pin that a remote plain-http URL is refused and loopback is allowed.
    fn the_url_rule_is_https_or_loopback_http() {
        let ws = workspace();
        write(
            &ws,
            "[[server]]\nname = \"remote\"\nurl = \"https://example.com/mcp\"\n",
        );
        let set = McpSet::load(&ws).unwrap();
        assert_eq!(set.servers[0].transport(), "http");
        assert_eq!(
            set.servers[0].url.as_deref(),
            Some("https://example.com/mcp")
        );
        assert!(set.servers[0].command.is_empty());

        write(
            &ws,
            "[[server]]\nname = \"local\"\nurl = \"http://127.0.0.1:8931/mcp\"\n",
        );
        assert!(McpSet::load(&ws).is_ok(), "loopback http is allowed");
        write(
            &ws,
            "[[server]]\nname = \"local\"\nurl = \"http://localhost:8931/mcp\"\n",
        );
        assert!(McpSet::load(&ws).is_ok(), "localhost is loopback");

        write(
            &ws,
            "[[server]]\nname = \"bad\"\nurl = \"http://example.com/mcp\"\n",
        );
        let err = McpSet::load(&ws).unwrap_err().to_string();
        assert!(err.contains("loopback"), "{err}");

        write(&ws, "[[server]]\nname = \"bad\"\nurl = \"ftp://x/mcp\"\n");
        let err = McpSet::load(&ws).unwrap_err().to_string();
        assert!(err.contains("https"), "{err}");
    }

    #[test]
    /// Purpose: Pin that `${VAR}` is expanded, a missing variable is named, and headers need a url.
    fn headers_expand_the_environment_and_fail_loudly() {
        let _guard = ENV.lock().unwrap();
        std::env::set_var("LOONG_MCP_TEST_TOKEN", "s3cret");
        let ws = workspace();
        write(
            &ws,
            "[[server]]\nname = \"remote\"\nurl = \"https://example.com/mcp\"\n\
             [server.headers]\nAuthorization = \"Bearer ${LOONG_MCP_TEST_TOKEN}\"\n",
        );
        let set = McpSet::load(&ws).unwrap();
        assert_eq!(
            set.servers[0]
                .headers
                .get("Authorization")
                .map(String::as_str),
            Some("Bearer s3cret")
        );

        write(
            &ws,
            "[[server]]\nname = \"remote\"\nurl = \"https://example.com/mcp\"\n\
             [server.headers]\nAuthorization = \"Bearer ${LOONG_MCP_TEST_MISSING}\"\n",
        );
        let err = McpSet::load(&ws).unwrap_err().to_string();
        assert!(err.contains("LOONG_MCP_TEST_MISSING"), "{err}");
        assert!(!err.contains("s3cret"), "the value is never printed: {err}");

        write(
            &ws,
            "[[server]]\nname = \"local\"\ncommand = \"node\"\n\
             [server.headers]\nAuthorization = \"Bearer x\"\n",
        );
        let err = McpSet::load(&ws).unwrap_err().to_string();
        assert!(err.contains("url transport"), "{err}");
        std::env::remove_var("LOONG_MCP_TEST_TOKEN");
    }

    #[test]
    /// Purpose: Pin the three `defer` policies against the same measurement.
    fn the_defer_policy_is_auto_by_default() {
        assert!(!Defer::Auto.applies(false));
        assert!(Defer::Auto.applies(true));
        assert!(Defer::Always.applies(false));
        assert!(!Defer::Never.applies(true));

        let ws = workspace();
        write(
            &ws,
            "[[server]]\nname = \"a\"\nurl = \"https://example.com/mcp\"\ndefer = \"always\"\n",
        );
        assert_eq!(McpSet::load(&ws).unwrap().servers[0].defer, Defer::Always);
    }
}
