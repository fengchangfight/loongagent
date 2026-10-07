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

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.wire() == text)
    }
}

/// What a hook that could not answer means. **Fail closed by default**: a guard
/// that silently evaporates is worse than one that stops work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnError {
    Deny,
    Allow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookKind {
    /// A script file (fingerprinted, executed with `sh`).
    Shell { script: PathBuf },
    /// An endpoint returning a verdict. Off unless the manifest opts in.
    Http { url: String },
}

impl HookKind {
    pub fn label(&self) -> &'static str {
        match self {
            HookKind::Shell { .. } => "shell",
            HookKind::Http { .. } => "http",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookSpec {
    pub id: String,
    pub event: HookEvent,
    pub kind: HookKind,
    pub on_error: OnError,
    pub timeout: Duration,
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
            ConfigError::Parse(m) => write!(f, "hooks.toml: {m}"),
            ConfigError::Invalid(m) => write!(f, "hooks.toml: {m}"),
        }
    }
}

impl std::error::Error for ConfigError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustStatus {
    /// No manifest: the session simply has no hooks.
    NoHooks,
    Trusted,
    /// The manifest or one of its scripts is new or changed.
    Untrusted { changed: Vec<String> },
}

#[derive(Debug, Clone)]
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
        let file: RawFile =
            toml::from_str(&text).map_err(|e| ConfigError::Parse(e.to_string()))?;

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
                raw.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS).min(MAX_TIMEOUT_MS),
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
                    return Err(ConfigError::Invalid(format!(
                        "{where_}: the mcp hook kind needs the MCP client (T-15), which is not \
                         built yet — use a shell or http hook for now"
                    )))
                }
                other => {
                    return Err(ConfigError::Invalid(format!(
                        "{where_}: unknown kind {other:?} (shell | http)"
                    )))
                }
            };

            specs.push(HookSpec {
                id: raw.id.clone().unwrap_or_else(|| format!("{}-{}", kind.label(), index + 1)),
                event,
                kind,
                on_error,
                timeout,
            });
        }

        let trust = Self::read_trust(&hooks_dir, &specs, &manifest)?;
        Ok(Self { workspace, hooks_dir, specs, trust })
    }

    /// Every file whose bytes must be trusted, with a change-detection hash.
    pub fn fingerprints(&self) -> Result<Vec<(String, String)>, ConfigError> {
        let mut out = Vec::new();
        let manifest = self.hooks_dir.join(HOOKS_FILE);
        if manifest.exists() {
            out.push((HOOKS_FILE.to_string(), hash_file(&manifest)?));
        }
        for spec in &self.specs {
            if let HookKind::Shell { script } = &spec.kind {
                let name = script
                    .strip_prefix(&self.hooks_dir)
                    .unwrap_or(script)
                    .to_string_lossy()
                    .to_string();
                out.push((name, hash_file(script)?));
            }
        }
        out.sort();
        out.dedup_by(|a, b| a.0 == b.0);
        Ok(out)
    }

    fn read_trust(
        hooks_dir: &Path,
        specs: &[HookSpec],
        manifest: &Path,
    ) -> Result<TrustStatus, ConfigError> {
        let recorded = std::fs::read_to_string(hooks_dir.join(TRUST_FILE)).unwrap_or_default();
        let recorded: Vec<&str> = recorded
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect();

        let mut expected = vec![(HOOKS_FILE.to_string(), hash_file(manifest)?)];
        for spec in specs {
            if let HookKind::Shell { script } = &spec.kind {
                let name = script
                    .strip_prefix(hooks_dir)
                    .unwrap_or(script)
                    .to_string_lossy()
                    .to_string();
                expected.push((name, hash_file(script)?));
            }
        }
        expected.sort();
        expected.dedup_by(|a, b| a.0 == b.0);

        let mut changed = Vec::new();
        for (name, hash) in &expected {
            let line = format!("{hash} {name}");
            if !recorded.contains(&line.as_str()) {
                changed.push(name.clone());
            }
        }
        Ok(if changed.is_empty() { TrustStatus::Trusted } else { TrustStatus::Untrusted { changed } })
    }

    /// Record the current bytes as trusted — the explicit, per-repository opt-in.
    pub fn trust_now(&self) -> Result<PathBuf, ConfigError> {
        let mut text = String::from(
            "# loongagent hook trust\n\
             # A change detector, not a signature: editing a listed file re-arms the question.\n",
        );
        for (name, hash) in self.fingerprints()? {
            text.push_str(&format!("{hash} {name}\n"));
        }
        let path = self.hooks_dir.join(TRUST_FILE);
        std::fs::write(&path, text).map_err(|e| ConfigError::Io(format!("{}: {e}", path.display())))?;
        Ok(path)
    }

    /// The hooks that may actually run: none until the bytes are trusted.
    pub fn active(&self) -> &[HookSpec] {
        match self.trust {
            TrustStatus::Trusted => &self.specs,
            TrustStatus::NoHooks | TrustStatus::Untrusted { .. } => &[],
        }
    }

    /// A one-line summary for startup logs and `hooks list`.
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

fn hash_file(path: &Path) -> Result<String, ConfigError> {
    let bytes =
        std::fs::read(path).map_err(|e| ConfigError::Io(format!("{}: {e}", path.display())))?;
    Ok(format!("{:016x}", fingerprint(&bytes)))
}

/// FNV-1a over the bytes. A change detector, **not** a cryptographic digest: it
/// answers "did this change since a human looked?", which is all the trust gate
/// needs. Signatures arrive with ADR-0008's pack signing.
pub fn fingerprint(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[derive(Deserialize)]
struct RawFile {
    #[serde(default)]
    allow_http_hooks: bool,
    #[serde(default)]
    hook: Vec<RawHook>,
}

#[derive(Deserialize)]
struct RawHook {
    id: Option<String>,
    event: String,
    kind: String,
    script: Option<String>,
    url: Option<String>,
    on_error: Option<String>,
    timeout_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    pub fn workspace() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "loong-hooks-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(dir.join(HOOKS_DIR)).unwrap();
        dir
    }

    fn write_manifest(ws: &Path, body: &str) {
        std::fs::write(ws.join(HOOKS_DIR).join(HOOKS_FILE), body).unwrap();
    }

    fn write_script(ws: &Path, name: &str, body: &str) -> PathBuf {
        let path = ws.join(HOOKS_DIR).join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn no_manifest_means_no_hooks() {
        let ws = workspace();
        let set = HookSet::load(&ws).unwrap();
        assert_eq!(set.trust, TrustStatus::NoHooks);
        assert!(set.active().is_empty());
        assert_eq!(set.summary(), "no hooks");
    }

    #[test]
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
        assert_eq!(set.specs[0].on_error, OnError::Deny, "fail closed by default");
        assert!(matches!(set.trust, TrustStatus::Untrusted { .. }));
        assert!(set.active().is_empty(), "untrusted hooks never run");
        assert!(set.summary().contains("INERT"));

        set.trust_now().unwrap();
        let trusted = HookSet::load(&ws).unwrap();
        assert_eq!(trusted.trust, TrustStatus::Trusted);
        assert_eq!(trusted.active().len(), 1);
    }

    #[test]
    fn trusting_is_bound_to_content_not_to_a_path() {
        let ws = workspace();
        let script = write_script(&ws, "check.sh", "exit 0\n");
        write_manifest(&ws, "[[hook]]\nevent = \"pre_tool\"\nkind = \"shell\"\nscript = \"check.sh\"\n");
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
        assert!(matches!(HookSet::load(&ws).unwrap().trust, TrustStatus::Untrusted { .. }));
    }

    #[test]
    fn http_hooks_are_off_unless_the_manifest_opts_in() {
        let ws = workspace();
        write_manifest(&ws, "[[hook]]\nevent = \"pre_tool\"\nkind = \"http\"\nurl = \"http://127.0.0.1:9/x\"\n");
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
    fn broken_manifests_are_loud() {
        let ws = workspace();
        write_script(&ws, "ok.sh", "exit 0\n");

        write_manifest(&ws, "[[hook]]\nevent = \"pre_tool\"\nkind = \"shell\"\nscript = \"missing.sh\"\n");
        assert!(HookSet::load(&ws).unwrap_err().to_string().contains("does not exist"));

        write_manifest(&ws, "[[hook]]\nevent = \"whenever\"\nkind = \"shell\"\nscript = \"ok.sh\"\n");
        assert!(HookSet::load(&ws).unwrap_err().to_string().contains("unknown event"));

        write_manifest(&ws, "[[hook]]\nevent = \"pre_tool\"\nkind = \"mcp\"\n");
        assert!(HookSet::load(&ws).unwrap_err().to_string().contains("MCP client"));

        write_manifest(&ws, "[[hook]]\nevent = \"pre_tool\"\nkind = \"shell\"\nscript = \"ok.sh\"\non_error = \"maybe\"\n");
        assert!(HookSet::load(&ws).unwrap_err().to_string().contains("on_error"));

        write_manifest(&ws, "this is not toml {{{");
        assert!(matches!(HookSet::load(&ws).unwrap_err(), ConfigError::Parse(_)));
    }

    #[test]
    fn fingerprints_cover_the_manifest_and_every_script() {
        let ws = workspace();
        write_script(&ws, "a.sh", "exit 0\n");
        write_script(&ws, "b.sh", "exit 0\n");
        write_manifest(
            &ws,
            "[[hook]]\nevent = \"pre_tool\"\nkind = \"shell\"\nscript = \"a.sh\"\n\n\
             [[hook]]\nevent = \"post_tool\"\nkind = \"shell\"\nscript = \"b.sh\"\n",
        );
        let names: Vec<String> =
            HookSet::load(&ws).unwrap().fingerprints().unwrap().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["a.sh", "b.sh", "hooks.toml"]);
    }

    #[test]
    fn one_script_used_by_two_hooks_is_fingerprinted_once() {
        let ws = workspace();
        write_script(&ws, "shared.sh", "exit 0\n");
        write_manifest(
            &ws,
            "[[hook]]\nid = \"a\"\nevent = \"pre_tool\"\nkind = \"shell\"\nscript = \"shared.sh\"\n\n\
             [[hook]]\nid = \"b\"\nevent = \"post_tool\"\nkind = \"shell\"\nscript = \"shared.sh\"\n",
        );
        let set = HookSet::load(&ws).unwrap();
        let names: Vec<String> =
            set.fingerprints().unwrap().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["hooks.toml", "shared.sh"]);

        set.trust_now().unwrap();
        assert_eq!(HookSet::load(&ws).unwrap().trust, TrustStatus::Trusted);
        match &HookSet::load(&ws).unwrap().trust {
            TrustStatus::Trusted => {}
            other => panic!("duplicate entries must not leave it looking untrusted: {other:?}"),
        }
    }

    #[test]
    fn the_hash_is_stable_and_order_sensitive() {
        assert_eq!(fingerprint(b"abc"), fingerprint(b"abc"));
        assert_ne!(fingerprint(b"abc"), fingerprint(b"acb"));
        assert_eq!(fingerprint(b""), 0xcbf2_9ce4_8422_2325);
    }
}
