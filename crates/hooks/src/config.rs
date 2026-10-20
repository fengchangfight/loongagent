//! Purpose: Parse `hooks.toml` and keep its hooks inert until a human trusts the exact bytes.
//!
//! # Contract
//! - **Serves:** the kernel's build step and the `loongagent hooks` command.
//! - **Design:** `docs/design/extension-model.md` §1 and `docs/design/security-trust.md` §1.
//! - **Upstream:** `crates/kernel/src/turn/build.rs` and `crates/cli/src/hooks.rs`.
//! - **Downstream:** `loong_manifest` for the hash and trust check; `run` consumes the specs.
//! - **Verify:** `cargo test -p loong-hooks` · `./scripts/check.sh`
//!
//! Hook configuration and the trust gate.
//!
//! Hooks that ship inside a repository are **content from that repository**, so
//! they stay inert until a human trusts the exact bytes. The trust file records a
//! change-detection hash per source file — it is deliberately *not* a signature,
//! and saying so matters: its job is "this changed since you looked", and the
//! security property comes from the human re-answering, not from the hash.

use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The directory a workspace keeps its agent configuration in.
pub const HOOKS_DIR: &str = ".loongagent";
/// The hook manifest inside that directory.
pub const HOOKS_FILE: &str = "hooks.toml";
/// The trust record inside that directory.
pub const TRUST_FILE: &str = "hooks.trusted";

const DEFAULT_TIMEOUT_MS: u64 = 5_000;
const MAX_TIMEOUT_MS: u64 = 60_000;

/// The moments a hook can answer for (ADR-0007 §7's event set, restricted to the
/// ones this kernel actually has).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
/// Purpose: The six lifecycle moments a hook can answer for.
/// Serves: `HookSet::load`, `HookRunner::fire` and the kernel's hook calls.
/// Design: `docs/design/extension-model.md` §1.
/// Up: `HookEvent::parse` from the manifest.
/// Down: `wire` and the kernel's `fire` calls.
/// Verify: `cargo test -p loong-hooks`.
pub enum HookEvent {
    SessionStart,
    UserPromptSubmit,
    PreTool,
    PostTool,
    PermissionRequested,
    PermissionDecided,
}

impl HookEvent {
    pub const ALL: [HookEvent; 6] = [
        HookEvent::SessionStart,
        HookEvent::UserPromptSubmit,
        HookEvent::PreTool,
        HookEvent::PostTool,
        HookEvent::PermissionRequested,
        HookEvent::PermissionDecided,
    ];

    /// Purpose: The event's snake_case name on the wire and in the payload.
    /// Serves: `HookEvent::parse` and `payload`.
    /// Design: `docs/design/extension-model.md` §1.
    /// Up: `parse`, `HookSet::load`'s error listing, `payload`, the CLI's `hooks list` and the kernel's `hook.fired`.
    /// Down: the manifest spelling and the payload's `event` field.
    /// Verify: `cargo test -p loong-hooks`.
    pub fn wire(self) -> &'static str {
        match self {
            HookEvent::SessionStart => "session_start",
            HookEvent::UserPromptSubmit => "user_prompt_submit",
            HookEvent::PreTool => "pre_tool",
            HookEvent::PostTool => "post_tool",
            HookEvent::PermissionRequested => "permission_requested",
            HookEvent::PermissionDecided => "permission_decided",
        }
    }

    /// Purpose: Parse a manifest event name, rejecting an unknown one.
    /// Serves: `HookSet::load`.
    /// Design: `docs/design/extension-model.md` §1.
    /// Up: `HookSet::load`.
    /// Down: `HookEvent::ALL` and `wire`.
    /// Verify: `cargo test -p loong-hooks`.
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.wire() == text)
    }
}

/// What a hook that could not answer means. **Fail closed by default**: a guard
/// that silently evaporates is worse than one that stops work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Purpose: What a hook that could not answer means; fail closed by default.
/// Serves: `HookSet::load`, which parses it, and `run`'s fallback.
/// Design: `docs/design/extension-model.md` §1.
/// Up: `HookSet::load`.
/// Down: `HookRunner::fallback`.
/// Verify: `cargo test -p loong-hooks`.
pub enum OnError {
    Deny,
    Allow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Purpose: How a hook answers: a script, an HTTP endpoint, or an MCP tool.
/// Serves: `HookSet::load` and `HookRunner::run_one`.
/// Design: `docs/design/extension-model.md` §1.
/// Up: `HookSet::load` constructs it.
/// Down: `label`, `target` and `HookRunner::run_one`.
/// Verify: `cargo test -p loong-hooks`.
pub enum HookKind {
    /// A script file (fingerprinted, executed with `sh`).
    Shell { script: PathBuf },
    /// An endpoint returning a verdict. Off unless the manifest opts in.
    Http { url: String },
    /// A tool on an MCP server (which has its own trust gate in `mcp.toml`),
    /// returning the same verdict JSON as an HTTP hook.
    Mcp { server: String, tool: String },
}

impl HookKind {
    /// Purpose: The kind's short name for default ids and `HookOutcome`.
    /// Serves: `HookSet::load` and `run::outcome`.
    /// Design: `docs/design/extension-model.md` §1.
    /// Up: `HookSet::load`'s default id and `outcome`.
    /// Down: the strings `"shell"`, `"http"` and `"mcp"`.
    /// Verify: `cargo test -p loong-hooks`.
    pub fn label(&self) -> &'static str {
        match self {
            HookKind::Shell { .. } => "shell",
            HookKind::Http { .. } => "http",
            HookKind::Mcp { .. } => "mcp",
        }
    }

    /// What to show a human in `hooks list`.
    /// Purpose: What to show a human in `hooks list`.
    /// Serves: `crates/cli/src/hooks.rs`.
    /// Design: `docs/design/extension-model.md` §1.
    /// Up: the CLI's `hooks list`.
    /// Down: the script path, URL or `server/tool` pair.
    /// Verify: `cargo test -p loong-hooks`.
    pub fn target(&self) -> String {
        match self {
            HookKind::Shell { script } => script.display().to_string(),
            HookKind::Http { url } => url.clone(),
            HookKind::Mcp { server, tool } => format!("{server}/{tool}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Purpose: One parsed hook: id, event, kind, failure policy and deadline.
/// Serves: `HookSet` and `HookRunner`.
/// Design: `docs/design/extension-model.md` §1.
/// Up: `HookSet::load`.
/// Down: `HookRunner`.
/// Verify: `cargo test -p loong-hooks`.
pub struct HookSpec {
    pub id: String,
    pub event: HookEvent,
    pub kind: HookKind,
    pub on_error: OnError,
    pub timeout: Duration,
}

#[derive(Debug)]
/// Purpose: Why `hooks.toml` could not be read, parsed or accepted.
/// Serves: `HookSet::load` and the CLI's error output.
/// Design: `docs/design/extension-model.md` §1.
/// Up: `HookSet::load`.
/// Down: its `Display` impl and the kernel's error.
/// Verify: `cargo test -p loong-hooks`.
pub enum ConfigError {
    Io(String),
    Parse(String),
    Invalid(String),
}

impl std::fmt::Display for ConfigError {
    /// Purpose: Prefix parse and validation errors with `hooks.toml`.
    /// Contract: `crates/hooks/src/config.rs`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io(m) => write!(f, "{m}"),
            ConfigError::Parse(m) => write!(f, "hooks.toml: {m}"),
            ConfigError::Invalid(m) => write!(f, "hooks.toml: {m}"),
        }
    }
}

impl std::error::Error for ConfigError {}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Purpose: The trust verdict for the manifest and every script it names.
/// Serves: `HookSet::active` and `summary`.
/// Design: `docs/design/extension-model.md` §1 and `docs/design/security-trust.md` §1.
/// Up: `HookSet::read_trust`.
/// Down: `active` and `summary`.
/// Verify: `cargo test -p loong-hooks`.
pub enum TrustStatus {
    /// No manifest: the session simply has no hooks.
    NoHooks,
    Trusted,
    /// The manifest or one of its scripts is new or changed.
    Untrusted {
        changed: Vec<String>,
    },
}

#[derive(Debug, Clone)]
/// Purpose: The parsed manifest plus its trust verdict.
/// Serves: the kernel's build step and the CLI.
/// Design: `docs/design/extension-model.md` §1 and `docs/design/security-trust.md` §1.
/// Up: `HookSet::load`.
/// Down: `active`, `fingerprints` and `summary`.
/// Verify: `cargo test -p loong-hooks`.
pub struct HookSet {
    pub workspace: PathBuf,
    pub hooks_dir: PathBuf,
    pub specs: Vec<HookSpec>,
    pub trust: TrustStatus,
}

impl HookSet {
    /// Load `<workspace>/.loongagent/hooks.toml`. An absent manifest is not an
    /// error; a malformed one is (silently ignoring a broken guard is the worst
    /// outcome).
    /// Purpose: Parse the manifest and check its trust record, with no manifest meaning no hooks.
    /// Serves: `crates/kernel/src/turn/build.rs`, `crates/cli/src/hooks.rs` and `crates/cli/src/server.rs`.
    /// Design: `docs/design/extension-model.md` §1 and `docs/design/security-trust.md` §1.
    /// Up: those three callers.
    /// Down: `read_trust` and `sources_of`.
    /// Verify: `cargo test -p loong-hooks`.
    pub fn load(workspace: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let workspace = workspace.as_ref().to_path_buf();
        let hooks_dir = workspace.join(HOOKS_DIR);
        let manifest = hooks_dir.join(HOOKS_FILE);
        if !manifest.exists() {
            return Ok(Self {
                workspace,
                hooks_dir,
                specs: Vec::new(),
                trust: TrustStatus::NoHooks,
            });
        }
        let text = std::fs::read_to_string(&manifest)
            .map_err(|e| ConfigError::Io(format!("{}: {e}", manifest.display())))?;
        let file: RawFile = toml::from_str(&text).map_err(|e| ConfigError::Parse(e.to_string()))?;

        let mut specs = Vec::new();
        for (index, raw) in file.hook.iter().enumerate() {
            let where_ = raw.id.clone().unwrap_or_else(|| format!("#{}", index + 1));
            let event = HookEvent::parse(&raw.event).ok_or_else(|| {
                ConfigError::Invalid(format!(
                    "{where_}: unknown event {:?} (one of {})",
                    raw.event,
                    HookEvent::ALL.map(|e| e.wire()).join(", ")
                ))
            })?;
            let on_error = match raw.on_error.as_deref() {
                None | Some("deny") => OnError::Deny,
                Some("allow") => OnError::Allow,
                Some(other) => {
                    return Err(ConfigError::Invalid(format!(
                        "{where_}: on_error must be \"deny\" or \"allow\", got {other:?}"
                    )))
                }
            };
            let timeout = Duration::from_millis(
                raw.timeout_ms
                    .unwrap_or(DEFAULT_TIMEOUT_MS)
                    .min(MAX_TIMEOUT_MS),
            );

            let kind = match raw.kind.as_str() {
                "shell" => {
                    let script = raw.script.clone().ok_or_else(|| {
                        ConfigError::Invalid(format!("{where_}: a shell hook needs `script`"))
                    })?;
                    let path = hooks_dir.join(&script);
                    if !path.is_file() {
                        return Err(ConfigError::Invalid(format!(
                            "{where_}: script {} does not exist",
                            path.display()
                        )));
                    }
                    HookKind::Shell { script: path }
                }
                "http" => {
                    if !file.allow_http_hooks {
                        return Err(ConfigError::Invalid(format!(
                            "{where_}: http hooks are off by default — set `allow_http_hooks = true` \
                             at the top of hooks.toml to opt in (an HTTP hook is the one thing that \
                             leaves the machine)"
                        )));
                    }
                    let url = raw.url.clone().ok_or_else(|| {
                        ConfigError::Invalid(format!("{where_}: an http hook needs `url`"))
                    })?;
                    if !url.starts_with("http://") {
                        return Err(ConfigError::Invalid(format!(
                            "{where_}: {url} is not an http:// URL"
                        )));
                    }
                    HookKind::Http { url }
                }
                "mcp" => {
                    let server = raw.server.clone().ok_or_else(|| {
                        ConfigError::Invalid(format!(
                            "{where_}: an mcp hook needs `server` (a name from mcp.toml)"
                        ))
                    })?;
                    let tool = raw.tool.clone().ok_or_else(|| {
                        ConfigError::Invalid(format!("{where_}: an mcp hook needs `tool`"))
                    })?;
                    HookKind::Mcp { server, tool }
                }
                other => {
                    return Err(ConfigError::Invalid(format!(
                        "{where_}: unknown kind {other:?} (shell | http | mcp)"
                    )))
                }
            };

            specs.push(HookSpec {
                id: raw
                    .id
                    .clone()
                    .unwrap_or_else(|| format!("{}-{}", kind.label(), index + 1)),
                event,
                kind,
                on_error,
                timeout,
            });
        }

        let trust = Self::read_trust(&hooks_dir, &specs, &manifest)?;
        Ok(Self {
            workspace,
            hooks_dir,
            specs,
            trust,
        })
    }

    /// Every file whose bytes must be trusted, with a change-detection hash.
    /// Purpose: Every file whose bytes must be trusted, with its change-detection hash.
    /// Serves: `loongagent hooks trust`, which prints them.
    /// Design: `docs/design/extension-model.md` §1 and `docs/design/security-trust.md` §1.
    /// Up: `crates/cli/src/hooks.rs`.
    /// Down: `sources_of` and `loong_manifest::hash_file`.
    /// Verify: `cargo test -p loong-hooks`.
    pub fn fingerprints(&self) -> Result<Vec<(String, String)>, ConfigError> {
        let sources = sources_of(
            &self.hooks_dir,
            &self.specs,
            &self.hooks_dir.join(HOOKS_FILE),
        )?;
        sources
            .iter()
            .map(|(name, path)| {
                loong_manifest::hash_file(path)
                    .map(|hash| (name.clone(), hash))
                    .map_err(ConfigError::Io)
            })
            .collect()
    }

    /// Purpose: Turn the trust record into `TrustStatus`, counting an absent record as changed.
    /// Serves: `HookSet::load`.
    /// Design: `docs/design/extension-model.md` §1 and `docs/design/security-trust.md` §1.
    /// Up: `HookSet::load`.
    /// Down: `loong_manifest::check`.
    /// Verify: `cargo test -p loong-hooks`.
    fn read_trust(
        hooks_dir: &Path,
        specs: &[HookSpec],
        manifest: &Path,
    ) -> Result<TrustStatus, ConfigError> {
        let sources = sources_of(hooks_dir, specs, manifest)?;
        match loong_manifest::check(&hooks_dir.join(TRUST_FILE), &sources)
            .map_err(ConfigError::Io)?
        {
            loong_manifest::TrustStatus::Trusted => Ok(TrustStatus::Trusted),
            // Nothing recorded yet counts as changed, not as consent.
            loong_manifest::TrustStatus::Absent | loong_manifest::TrustStatus::Untrusted { .. } => {
                let changed = match loong_manifest::check(&hooks_dir.join(TRUST_FILE), &sources)
                    .map_err(ConfigError::Io)?
                {
                    loong_manifest::TrustStatus::Untrusted { changed } => changed,
                    _ => sources.iter().map(|(n, _)| n.clone()).collect(),
                };
                Ok(TrustStatus::Untrusted { changed })
            }
        }
    }

    /// Record the current bytes as trusted — the explicit, per-repository opt-in.
    /// Purpose: Record the current bytes as trusted, the explicit per-repository opt-in.
    /// Serves: `loongagent hooks trust` and the crate's tests.
    /// Design: `docs/design/extension-model.md` §1 and `docs/design/security-trust.md` §1.
    /// Up: `crates/cli/src/hooks.rs`; `crates/server/src/lib.rs` calls it only from a test.
    /// Down: `loong_manifest::record`.
    /// Verify: `cargo test -p loong-hooks`.
    pub fn trust_now(&self) -> Result<PathBuf, ConfigError> {
        let sources = sources_of(
            &self.hooks_dir,
            &self.specs,
            &self.hooks_dir.join(HOOKS_FILE),
        )?;
        let path = self.hooks_dir.join(TRUST_FILE);
        loong_manifest::record(&path, &sources).map_err(ConfigError::Io)?;
        Ok(path)
    }

    /// The hooks that may actually run: none until the bytes are trusted.
    /// Purpose: The hooks that may run: none until the bytes are trusted.
    /// Serves: the kernel's build step when it builds the runner.
    /// Design: `docs/design/extension-model.md` §1.
    /// Up: `crates/kernel/src/turn/build.rs`.
    /// Down: the `specs` field, gated by `trust`.
    /// Verify: `cargo test -p loong-hooks`.
    pub fn active(&self) -> &[HookSpec] {
        match self.trust {
            TrustStatus::Trusted => &self.specs,
            TrustStatus::NoHooks | TrustStatus::Untrusted { .. } => &[],
        }
    }

    /// A one-line summary for startup logs and `hooks list`.
    /// Purpose: A one-line trust summary for startup logs and `hooks list`.
    /// Serves: `crates/cli/src/hooks.rs` and the kernel's startup note.
    /// Design: `docs/design/extension-model.md` §1.
    /// Up: `crates/cli/src/hooks.rs` and `crates/kernel/src/turn/build.rs`.
    /// Down: `trust` and the spec count.
    /// Verify: `cargo test -p loong-hooks`.
    pub fn summary(&self) -> String {
        match &self.trust {
            TrustStatus::NoHooks => "no hooks".to_string(),
            TrustStatus::Trusted => format!("{} hook(s), trusted", self.specs.len()),
            TrustStatus::Untrusted { changed } => format!(
                "{} hook(s) INERT — untrusted: {} (run `loongagent hooks trust`)",
                self.specs.len(),
                changed.join(", ")
            ),
        }
    }
}

/// The files a hook set's behaviour depends on: the manifest, then every script
/// it names (deduplicated: one script used by two hooks is trusted once).
/// Purpose: The files whose bytes a hook set depends on: the manifest and every shell script.
/// Serves: `fingerprints`, `read_trust` and `trust_now`.
/// Design: `docs/design/extension-model.md` §1.
/// Up: those three methods.
/// Down: `HookKind::Shell` and `loong_manifest::Sources`.
/// Verify: `cargo test -p loong-hooks`.
fn sources_of(
    hooks_dir: &Path,
    specs: &[HookSpec],
    manifest: &Path,
) -> Result<loong_manifest::Sources, ConfigError> {
    let mut sources: loong_manifest::Sources =
        vec![(HOOKS_FILE.to_string(), manifest.to_path_buf())];
    for spec in specs {
        if let HookKind::Shell { script } = &spec.kind {
            let name = script
                .strip_prefix(hooks_dir)
                .unwrap_or(script)
                .to_string_lossy()
                .to_string();
            sources.push((name, script.clone()));
        }
    }
    sources.sort();
    sources.dedup_by(|a, b| a.0 == b.0);
    Ok(sources)
}

/// Re-exported so callers keep one name for "the change detector".
pub use loong_manifest::fingerprint;

#[derive(Deserialize)]
/// Purpose: The TOML shape: the HTTP opt-in flag and the hook entries.
/// Serves: `HookSet::load`.
/// Design: `docs/design/extension-model.md` §1.
/// Up: `toml::from_str` in `load`.
/// Down: `RawHook`.
/// Verify: `cargo test -p loong-hooks`.
struct RawFile {
    #[serde(default)]
    allow_http_hooks: bool,
    #[serde(default)]
    hook: Vec<RawHook>,
}

#[derive(Deserialize)]
/// Purpose: One `[[hook]]` table before validation.
/// Serves: `RawFile` and `HookSet::load`.
/// Design: `docs/design/extension-model.md` §1.
/// Up: `RawFile`.
/// Down: `HookSet::load`'s validation.
/// Verify: `cargo test -p loong-hooks`.
struct RawHook {
    id: Option<String>,
    event: String,
    kind: String,
    script: Option<String>,
    url: Option<String>,
    server: Option<String>,
    tool: Option<String>,
    on_error: Option<String>,
    timeout_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    /// Purpose: A fresh temporary workspace carrying a `.loongagent` directory.
    /// Serves: every test in this module.
    /// Design: `docs/design/extension-model.md` §1.
    /// Up: the hooks config tests only.
    /// Down: `std::env::temp_dir` and `HOOKS_DIR`.
    /// Verify: `cargo test -p loong-hooks`.
    pub fn workspace() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "loong-hooks-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(dir.join(HOOKS_DIR)).unwrap();
        dir
    }

    /// Purpose: Write a `hooks.toml` body into a test workspace.
    /// Contract: `crates/hooks/src/config.rs`.
    fn write_manifest(ws: &Path, body: &str) {
        std::fs::write(ws.join(HOOKS_DIR).join(HOOKS_FILE), body).unwrap();
    }

    /// Purpose: Write a hook script into a test workspace and return its path.
    /// Contract: `crates/hooks/src/config.rs`.
    fn write_script(ws: &Path, name: &str, body: &str) -> PathBuf {
        let path = ws.join(HOOKS_DIR).join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    /// Purpose: Pin that no manifest means no hooks and the "no hooks" summary.
    fn no_manifest_means_no_hooks() {
        let ws = workspace();
        let set = HookSet::load(&ws).unwrap();
        assert_eq!(set.trust, TrustStatus::NoHooks);
        assert!(set.active().is_empty());
        assert_eq!(set.summary(), "no hooks");
    }

    #[test]
    /// Purpose: Pin shell parsing, the fail-closed default, and inertness until trusted.
    fn a_shell_hook_parses_and_is_inert_until_trusted() {
        let ws = workspace();
        write_script(&ws, "check.sh", "exit 0\n");
        write_manifest(
            &ws,
            "[[hook]]\nid = \"guard\"\nevent = \"pre_tool\"\nkind = \"shell\"\nscript = \"check.sh\"\ntimeout_ms = 250\n",
        );
        let set = HookSet::load(&ws).unwrap();
        assert_eq!(set.specs.len(), 1);
        assert_eq!(set.specs[0].id, "guard");
        assert_eq!(set.specs[0].event, HookEvent::PreTool);
        assert_eq!(set.specs[0].timeout, Duration::from_millis(250));
        assert_eq!(
            set.specs[0].on_error,
            OnError::Deny,
            "fail closed by default"
        );
        assert!(matches!(set.trust, TrustStatus::Untrusted { .. }));
        assert!(set.active().is_empty(), "untrusted hooks never run");
        assert!(set.summary().contains("INERT"));

        set.trust_now().unwrap();
        let trusted = HookSet::load(&ws).unwrap();
        assert_eq!(trusted.trust, TrustStatus::Trusted);
        assert_eq!(trusted.active().len(), 1);
    }

    #[test]
    /// Purpose: Pin that trust is bound to content, so a changed script or manifest re-arms it.
    fn trusting_is_bound_to_content_not_to_a_path() {
        let ws = workspace();
        let script = write_script(&ws, "check.sh", "exit 0\n");
        write_manifest(
            &ws,
            "[[hook]]\nevent = \"pre_tool\"\nkind = \"shell\"\nscript = \"check.sh\"\n",
        );
        HookSet::load(&ws).unwrap().trust_now().unwrap();
        assert_eq!(HookSet::load(&ws).unwrap().trust, TrustStatus::Trusted);

        // The script changes: the old trust record must not cover it.
        std::fs::write(&script, "exit 2\n").unwrap();
        match HookSet::load(&ws).unwrap().trust {
            TrustStatus::Untrusted { changed } => assert_eq!(changed, vec!["check.sh".to_string()]),
            other => panic!("expected untrusted, got {other:?}"),
        }

        // So does the manifest.
        std::fs::write(&script, "exit 0\n").unwrap();
        HookSet::load(&ws).unwrap().trust_now().unwrap();
        write_manifest(
            &ws,
            "[[hook]]\nevent = \"pre_tool\"\nkind = \"shell\"\nscript = \"check.sh\"\n# edited\n",
        );
        assert!(matches!(
            HookSet::load(&ws).unwrap().trust,
            TrustStatus::Untrusted { .. }
        ));
    }

    #[test]
    /// Purpose: Pin that HTTP hooks are refused unless the manifest opts in.
    fn http_hooks_are_off_unless_the_manifest_opts_in() {
        let ws = workspace();
        write_manifest(
            &ws,
            "[[hook]]\nevent = \"pre_tool\"\nkind = \"http\"\nurl = \"http://127.0.0.1:9/x\"\n",
        );
        let err = HookSet::load(&ws).unwrap_err().to_string();
        assert!(err.contains("off by default"), "{err}");

        write_manifest(
            &ws,
            "allow_http_hooks = true\n\n[[hook]]\nevent = \"pre_tool\"\nkind = \"http\"\nurl = \"http://127.0.0.1:9/x\"\n",
        );
        let set = HookSet::load(&ws).unwrap();
        assert_eq!(set.specs.len(), 1);
        assert!(matches!(set.specs[0].kind, HookKind::Http { .. }));
    }

    #[test]
    /// Purpose: Pin the loud, specific errors for a broken manifest.
    fn broken_manifests_are_loud() {
        let ws = workspace();
        write_script(&ws, "ok.sh", "exit 0\n");

        write_manifest(
            &ws,
            "[[hook]]\nevent = \"pre_tool\"\nkind = \"shell\"\nscript = \"missing.sh\"\n",
        );
        assert!(HookSet::load(&ws)
            .unwrap_err()
            .to_string()
            .contains("does not exist"));

        write_manifest(
            &ws,
            "[[hook]]\nevent = \"whenever\"\nkind = \"shell\"\nscript = \"ok.sh\"\n",
        );
        assert!(HookSet::load(&ws)
            .unwrap_err()
            .to_string()
            .contains("unknown event"));

        write_manifest(&ws, "[[hook]]\nevent = \"pre_tool\"\nkind = \"mcp\"\n");
        assert!(HookSet::load(&ws)
            .unwrap_err()
            .to_string()
            .contains("needs `server`"));

        write_manifest(
            &ws,
            "[[hook]]\nevent = \"pre_tool\"\nkind = \"mcp\"\nserver = \"policy\"\n",
        );
        assert!(HookSet::load(&ws)
            .unwrap_err()
            .to_string()
            .contains("needs `tool`"));

        write_manifest(
            &ws,
            "[[hook]]\nevent = \"pre_tool\"\nkind = \"mcp\"\nserver = \"policy\"\ntool = \"decide\"\n",
        );
        let set = HookSet::load(&ws).unwrap();
        assert_eq!(set.specs[0].kind.label(), "mcp");
        assert_eq!(set.specs[0].kind.target(), "policy/decide");

        write_manifest(&ws, "[[hook]]\nevent = \"pre_tool\"\nkind = \"shell\"\nscript = \"ok.sh\"\non_error = \"maybe\"\n");
        assert!(HookSet::load(&ws)
            .unwrap_err()
            .to_string()
            .contains("on_error"));

        write_manifest(&ws, "this is not toml {{{");
        assert!(matches!(
            HookSet::load(&ws).unwrap_err(),
            ConfigError::Parse(_)
        ));
    }

    #[test]
    /// Purpose: Pin that fingerprints cover the manifest and every named script.
    fn fingerprints_cover_the_manifest_and_every_script() {
        let ws = workspace();
        write_script(&ws, "a.sh", "exit 0\n");
        write_script(&ws, "b.sh", "exit 0\n");
        write_manifest(
            &ws,
            "[[hook]]\nevent = \"pre_tool\"\nkind = \"shell\"\nscript = \"a.sh\"\n\n\
             [[hook]]\nevent = \"post_tool\"\nkind = \"shell\"\nscript = \"b.sh\"\n",
        );
        let names: Vec<String> = HookSet::load(&ws)
            .unwrap()
            .fingerprints()
            .unwrap()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(names, vec!["a.sh", "b.sh", "hooks.toml"]);
    }

    #[test]
    /// Purpose: Pin that one script used by two hooks is fingerprinted once and still trusts.
    fn one_script_used_by_two_hooks_is_fingerprinted_once() {
        let ws = workspace();
        write_script(&ws, "shared.sh", "exit 0\n");
        write_manifest(
            &ws,
            "[[hook]]\nid = \"a\"\nevent = \"pre_tool\"\nkind = \"shell\"\nscript = \"shared.sh\"\n\n\
             [[hook]]\nid = \"b\"\nevent = \"post_tool\"\nkind = \"shell\"\nscript = \"shared.sh\"\n",
        );
        let set = HookSet::load(&ws).unwrap();
        let names: Vec<String> = set
            .fingerprints()
            .unwrap()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(names, vec!["hooks.toml", "shared.sh"]);

        set.trust_now().unwrap();
        assert_eq!(HookSet::load(&ws).unwrap().trust, TrustStatus::Trusted);
        match &HookSet::load(&ws).unwrap().trust {
            TrustStatus::Trusted => {}
            other => panic!("duplicate entries must not leave it looking untrusted: {other:?}"),
        }
    }

    #[test]
    /// Purpose: Pin that the fingerprint is stable and order sensitive.
    fn the_hash_is_stable_and_order_sensitive() {
        assert_eq!(fingerprint(b"abc"), fingerprint(b"abc"));
        assert_ne!(fingerprint(b"abc"), fingerprint(b"acb"));
        assert_eq!(fingerprint(b""), 0xcbf2_9ce4_8422_2325);
    }
}
