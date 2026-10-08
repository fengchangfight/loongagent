//! Configuration and profile layering (ADR-0008).
//!
//! The whole point of this crate is one sentence from the decision record:
//! **no lower layer may ever loosen a higher one.** A precedence chain where
//! "last writer wins" would let a repository quietly widen an organisation's hard
//! caps, so the merge behaviour follows the **key's declared kind**, not the
//! value's shape:
//!
//! | Kind | Keys | Rule |
//! |---|---|---|
//! | **free-override** | `model`, `base_url`, `profile` | the most local layer that sets it wins |
//! | **narrow-only** | `budget_tokens`, `keep_newest_tokens`, `max_steps`, `compact_at_percent`, `[permissions]` | strictest wins; a lower layer may only narrow |
//! | **add-only** | `skill_dirs`, `disable_skills` | union across layers (a project layer may *disable* an entry by identity — content is advisory, not enforcement) |
//! | **reference** | `api_key` | names a source (`env:NAME`); a literal secret is refused outright |
//!
//! Two extra rules come from the threat model rather than the table, and both are
//! about the same thing — a repository must not be able to choose where prompts go
//! or what instructions the agent reads from outside its own tree:
//!
//! * the **project layer may not set** `model`, `base_url` or `api_key` at all;
//! * a **project layer's `skill_dirs` must stay inside the workspace**.
//!
//! Every resolved key answers "why is this the value?" — the winning layer and the
//! file it came from ([`Profile::explain`], `loongagent config explain`). That is
//! the same provenance principle as the context ledger and the PDP's "rule that
//! fired".
//!
//! **What is not here:** signed org packs (ADR-0008 §6). The org layer resolves
//! like any other file today; the signature check that makes it tamper-resistant
//! is not implemented, so nothing should describe it as enforced.

use loong_schema::generated::{ActionClass, Verdict};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Lowest to highest precedence. The order *is* the rule: later beats earlier for
/// free-override, and can never loosen earlier for narrow-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    Builtin,
    Org,
    Project,
    User,
    Session,
}

impl Layer {
    pub fn as_str(self) -> &'static str {
        match self {
            Layer::Builtin => "builtin",
            Layer::Org => "org",
            Layer::Project => "project",
            Layer::User => "user",
            Layer::Session => "session",
        }
    }
}

/// How a key merges (ADR-0008 §2). A property of the key, never of the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    FreeOverride,
    NarrowOnly,
    AddOnly,
    Reference,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::FreeOverride => "free-override",
            Kind::NarrowOnly => "narrow-only",
            Kind::AddOnly => "add-only",
            Kind::Reference => "reference",
        }
    }
}

/// The kind of each known key. An unknown key is an error, not a silent no-op:
/// a typo in a policy file must not look like it worked.
pub fn kind_of(key: &str) -> Option<Kind> {
    Some(match key {
        "model" | "base_url" | "profile" => Kind::FreeOverride,
        "budget_tokens" | "keep_newest_tokens" | "max_steps" | "compact_at_percent"
        | "permissions" => Kind::NarrowOnly,
        "skill_dirs" | "disable_skills" => Kind::AddOnly,
        "api_key" => Kind::Reference,
        _ => return None,
    })
}

/// A value that survives resolution, with where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Text(String),
    Number(u64),
    Verdict(Verdict),
    List(Vec<String>),
}

impl Value {
    pub fn render(&self) -> String {
        match self {
            Value::Text(t) => t.clone(),
            Value::Number(n) => n.to_string(),
            Value::Verdict(v) => format!("{v:?}").to_lowercase(),
            Value::List(items) => items.join(", "),
        }
    }
}

/// "Why is this the value?" — one row per contributing key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    pub key: String,
    pub kind: Kind,
    pub value: Value,
    pub layer: Layer,
    /// The artifact it came from (`<defaults>` for built-in values).
    pub source: String,
    /// Layers that also set it, and lost — the narrowing story, in order.
    pub overridden: Vec<(Layer, Value)>,
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
            ConfigError::Parse(m) => write!(f, "profile: {m}"),
            ConfigError::Invalid(m) => write!(f, "profile: {m}"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// The effective configuration, after merging every layer.
#[derive(Debug, Clone, Default)]
pub struct Profile {
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub budget_tokens: Option<u64>,
    pub keep_newest_tokens: Option<u64>,
    pub max_steps: Option<usize>,
    pub compact_at_percent: Option<u64>,
    /// Resolved per action class. A `Vec` rather than a map because the order of
    /// action classes is not meaningful — inventing one to satisfy a `BTreeMap`
    /// would be a lie about the domain.
    pub permissions: Vec<(ActionClass, Verdict)>,
    pub skill_dirs: Vec<PathBuf>,
    pub disable_skills: Vec<String>,
    provenance: BTreeMap<String, Provenance>,
}

impl Profile {
    pub fn permission(&self, class: ActionClass) -> Option<Verdict> {
        self.permissions.iter().find(|(c, _)| *c == class).map(|(_, v)| *v)
    }

    /// The verdict **and** the layer that set it, for the PDP's rule string.
    pub fn permission_with_source(&self, class: ActionClass) -> Option<(Verdict, Layer)> {
        let key = format!("permissions.{}", match class {
            ActionClass::Investigate => "investigate",
            ActionClass::WriteWorkspace => "write_workspace",
            ActionClass::OutOfBounds => "out_of_bounds",
        });
        let row = self.explain(&key)?;
        match &row.value {
            Value::Verdict(v) => Some((*v, row.layer)),
            _ => None,
        }
    }

    /// The answer to "why is this the value?" for one key.
    pub fn explain(&self, key: &str) -> Option<&Provenance> {
        self.provenance.get(key)
    }

    /// Every key that has a value, ready to print.
    pub fn explain_all(&self) -> Vec<&Provenance> {
        self.provenance.values().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.provenance.is_empty()
    }
}

/// `deny_unknown_fields` is load-bearing: a mistyped key in a policy file that is
/// silently ignored is a guard that silently does not exist.
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawProfile {
    profile: Option<String>,
    model: Option<String>,
    base_url: Option<String>,
    api_key: Option<String>,
    budget_tokens: Option<u64>,
    keep_newest_tokens: Option<u64>,
    max_steps: Option<u64>,
    compact_at_percent: Option<u64>,
    #[serde(default)]
    skill_dirs: Vec<String>,
    #[serde(default)]
    disable_skills: Vec<String>,
    #[serde(default)]
    permissions: BTreeMap<String, String>,
}

/// One layer's raw settings, already validated against the key table.
struct LayerValues {
    layer: Layer,
    source: String,
    values: Vec<(String, Value)>,
}

impl LayerValues {
    fn from_file(layer: Layer, path: &Path, workspace: Option<&Path>) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::Io(format!("{}: {e}", path.display())))?;
        let raw: RawProfile =
            toml::from_str(&text).map_err(|e| ConfigError::Parse(format!("{}: {e}", path.display())))?;
        let source = path.display().to_string();
        let mut values = Vec::new();

        // free-override: model and endpoints. A repository may not choose where
        // prompts go — that is the exfiltration vector, not a preference. The rule
        // covers references too: `env:…` in a repo layer reaches for a variable the
        // repo does not own.
        for (key, value) in [
            ("model", raw.model.clone()),
            ("base_url", raw.base_url.clone()),
            ("api_key", raw.api_key.clone()),
        ] {
            let Some(value) = value else { continue };
            if layer == Layer::Project {
                return Err(ConfigError::Invalid(format!(
                    "{source}: the project layer may not set `{key}` — a repository must not \
                     choose where prompts go (ADR-0008)"
                )));
            }
            check_endpoint_reference(&source, key, &value)?;
            values.push((key.to_string(), Value::Text(value)));
        }
        if let Some(profile) = raw.profile {
            values.push(("profile".to_string(), Value::Text(profile)));
        }

        // reference: a secret is never a literal.
        if let Some(api_key) = raw.api_key.filter(|_| layer != Layer::Project) {
            if !api_key.starts_with("env:")
                && !api_key.starts_with("keychain:")
                && !api_key.starts_with("gateway:")
            {
                return Err(ConfigError::Invalid(format!(
                    "{source}: `api_key` must be a reference (env:NAME, keychain:…, gateway:…) — \
                     secrets never live in config (ADR-0008 §5)"
                )));
            }
            values.push(("api_key".to_string(), Value::Text(api_key)));
        }

        // narrow-only numbers
        for (key, value) in [
            ("budget_tokens", raw.budget_tokens),
            ("keep_newest_tokens", raw.keep_newest_tokens),
            ("max_steps", raw.max_steps),
            ("compact_at_percent", raw.compact_at_percent),
        ] {
            if let Some(value) = value {
                values.push((key.to_string(), Value::Number(value)));
            }
        }
        for (key, verdict) in &raw.permissions {
            let class = parse_class(key).ok_or_else(|| {
                ConfigError::Invalid(format!(
                    "{source}: unknown action class {key:?} \
                     (investigate | write_workspace | out_of_bounds)"
                ))
            })?;
            let verdict = parse_verdict(verdict).ok_or_else(|| {
                ConfigError::Invalid(format!(
                    "{source}: permissions.{key} must be allow | ask | deny, got {verdict:?}"
                ))
            })?;
            values.push((format!("permissions.{key}"), Value::Verdict(verdict)));
            let _ = class;
        }

        // add-only: skill directories and identity-based disables
        for dir in raw.skill_dirs {
            // A relative directory is relative to the workspace: that is the only
            // reading under which "inside the workspace" is checkable at all.
            let expanded = expand_home(&dir);
            let resolved = if expanded.is_absolute() {
                expanded
            } else {
                workspace.unwrap_or(Path::new(".")).join(expanded)
            };
            if layer == Layer::Project {
                // A repository may add skills from its own tree, not from the disk
                // around it.
                let inside = workspace
                    .map(|ws| resolved.starts_with(ws))
                    .unwrap_or(false);
                if !inside {
                    return Err(ConfigError::Invalid(format!(
                        "{source}: the project layer's skill_dirs must stay inside the workspace \
                         (got {})",
                        resolved.display()
                    )));
                }
            }
            values.push(("skill_dirs".to_string(), Value::Text(resolved.display().to_string())));
        }
        for name in raw.disable_skills {
            values.push(("disable_skills".to_string(), Value::Text(name)));
        }

        for (key, _) in &values {
            let base = key.split('.').next().unwrap_or(key);
            if kind_of(base).is_none() {
                return Err(ConfigError::Invalid(format!("{source}: unknown key {base:?}")));
            }
        }
        Ok(Self { layer, source, values })
    }

    /// The built-in ladder is a *layer*, and it is the ceiling of permissiveness:
    /// without it, a profile could widen `out_of_bounds` from ask to allow. With it,
    /// such an attempt resolves to the stricter built-in value and is recorded as
    /// overridden, which is what ADR-0008 §2 means by narrow-only.
    fn builtin_defaults() -> Self {
        Self {
            layer: Layer::Builtin,
            source: "<defaults>".to_string(),
            values: vec![
                ("permissions.investigate".to_string(), Value::Verdict(Verdict::Allow)),
                ("permissions.write_workspace".to_string(), Value::Verdict(Verdict::Allow)),
                ("permissions.out_of_bounds".to_string(), Value::Verdict(Verdict::Ask)),
            ],
        }
    }
}

fn check_endpoint_reference(source: &str, key: &str, value: &str) -> Result<(), ConfigError> {
    if key == "base_url" {
        let ok = value.starts_with("http://")
            || value.starts_with("https://")
            || value.starts_with("env:")
            || value.starts_with("gateway:");
        if !ok {
            return Err(ConfigError::Invalid(format!(
                "{source}: base_url must be an http(s) URL or a reference \
                 (env:NAME, gateway:…), got {value:?}"
            )));
        }
    }
    Ok(())
}

fn parse_class(text: &str) -> Option<ActionClass> {
    match text {
        "investigate" => Some(ActionClass::Investigate),
        "write_workspace" => Some(ActionClass::WriteWorkspace),
        "out_of_bounds" => Some(ActionClass::OutOfBounds),
        _ => None,
    }
}

fn parse_verdict(text: &str) -> Option<Verdict> {
    match text {
        "allow" => Some(Verdict::Allow),
        "ask" => Some(Verdict::Ask),
        "deny" => Some(Verdict::Deny),
        _ => None,
    }
}

fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

/// Where each layer's file lives on this machine.
///
/// The org layer is a file an administrator owns. ADR-0008 §6 delivers it as a
/// **signed pack**; that is not implemented, so this path is a placeholder for it
/// and nothing here should be called tamper-resistant.
pub fn layer_paths(workspace: &Path, session: Option<&Path>) -> Vec<(Layer, PathBuf)> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".config")));
    let mut layers = Vec::new();
    layers.push((Layer::Org, PathBuf::from("/etc/loongagent/profile.toml")));
    layers.push((Layer::Project, workspace.join(".loongagent/profile.toml")));
    if let Some(xdg) = xdg {
        layers.push((Layer::User, xdg.join("loongagent/profile.toml")));
    }
    if let Some(session) = session {
        layers.push((Layer::Session, session.to_path_buf()));
    }
    layers
}

/// Merge the layers that exist, lowest precedence first.
pub fn resolve(layers: &[(Layer, PathBuf)], workspace: &Path) -> Result<Profile, ConfigError> {
    let mut present: Vec<LayerValues> = vec![LayerValues::builtin_defaults()];
    let mut sources = std::collections::BTreeMap::new();
    for (layer, path) in layers {
        if !path.exists() {
            continue;
        }
        let values = LayerValues::from_file(*layer, path, Some(workspace))?;
        sources.insert(*layer, path.display().to_string());
        present.push(values);
    }
    present.sort_by_key(|l| l.layer);
    Ok(merge(&present))
}

fn merge(layers: &[LayerValues]) -> Profile {
    let mut profile = Profile::default();
    // Key → the winning row so far, plus what lost.
    let mut winners: BTreeMap<String, Provenance> = BTreeMap::new();

    for layer in layers {
        for (key, value) in &layer.values {
            let base = key.split('.').next().unwrap_or(key).to_string();
            let kind = kind_of(&base).expect("checked when loading");
            match kind {
                Kind::NarrowOnly => {
                    // Strictest wins: numbers by minimum, verdicts by rank.
                    match winners.get_mut(key) {
                        None => {
                            winners.insert(
                                key.clone(),
                                Provenance {
                                    key: key.clone(),
                                    kind,
                                    value: value.clone(),
                                    layer: layer.layer,
                                    source: layer.source.clone(),
                                    overridden: Vec::new(),
                                },
                            );
                        }
                        Some(current) => {
                            let (keep_new, lose_old) = stricter(&current.value, value);
                            if keep_new {
                                let old = current.value.clone();
                                current.value = value.clone();
                                current.layer = layer.layer;
                                current.source = layer.source.clone();
                                current.overridden.push((layer.layer, old));
                            } else if lose_old {
                                current.overridden.push((layer.layer, value.clone()));
                            }
                        }
                    }
                }
                Kind::FreeOverride | Kind::Reference => {
                    // The most local layer that sets it wins (session > user >
                    // project > org > defaults).
                    match winners.get_mut(key) {
                        None => {
                            winners.insert(
                                key.clone(),
                                Provenance {
                                    key: key.clone(),
                                    kind,
                                    value: value.clone(),
                                    layer: layer.layer,
                                    source: layer.source.clone(),
                                    overridden: Vec::new(),
                                },
                            );
                        }
                        Some(current) => {
                            let old = current.value.clone();
                            let old_layer = current.layer;
                            current.value = value.clone();
                            current.layer = layer.layer;
                            current.source = layer.source.clone();
                            current.overridden.push((old_layer, old));
                        }
                    }
                }
                Kind::AddOnly => match winners.get_mut(key) {
                    None => {
                        winners.insert(
                            key.clone(),
                            Provenance {
                                key: key.clone(),
                                kind,
                                value: Value::List(vec![value.render()]),
                                layer: layer.layer,
                                source: layer.source.clone(),
                                overridden: Vec::new(),
                            },
                        );
                    }
                    Some(current) => {
                        if let Value::List(items) = &mut current.value {
                            let rendered = value.render();
                            if !items.contains(&rendered) {
                                items.push(rendered);
                            }
                        }
                    }
                },
            }
        }
    }

    // The typed view, derived from the winners so the two cannot disagree.
    for (key, row) in &winners {
        match (key.as_str(), &row.value) {
            ("model", Value::Text(v)) => profile.model = Some(v.clone()),
            ("base_url", Value::Text(v)) => profile.base_url = Some(v.clone()),
            ("api_key", Value::Text(v)) => profile.api_key = Some(v.clone()),
            ("budget_tokens", Value::Number(n)) => profile.budget_tokens = Some(*n),
            ("keep_newest_tokens", Value::Number(n)) => profile.keep_newest_tokens = Some(*n),
            ("max_steps", Value::Number(n)) => profile.max_steps = Some(*n as usize),
            ("compact_at_percent", Value::Number(n)) => profile.compact_at_percent = Some(*n),
            ("skill_dirs", Value::List(items)) => {
                profile.skill_dirs = items.iter().map(PathBuf::from).collect()
            }
            ("disable_skills", Value::List(items)) => profile.disable_skills = items.clone(),
            (other, Value::Verdict(verdict)) => {
                if let Some(class) = other.strip_prefix("permissions.").and_then(parse_class) {
                    profile.permissions.push((class, *verdict));
                }
            }
            _ => {}
        }
    }
    profile.provenance = winners;
    profile
}

/// Which of two narrow-only values is stricter. Returns
/// `(new_is_stricter, old_should_be_recorded_as_overridden)`.
fn stricter(current: &Value, candidate: &Value) -> (bool, bool) {
    match (current, candidate) {
        (Value::Number(a), Value::Number(b)) => {
            if b < a {
                (true, false)
            } else if b > a {
                (false, true)
            } else {
                (false, false)
            }
        }
        (Value::Verdict(a), Value::Verdict(b)) => {
            let rank = |v: &Verdict| match v {
                Verdict::Allow => 0,
                Verdict::Ask => 1,
                Verdict::Deny => 2,
            };
            if rank(b) > rank(a) {
                (true, false)
            } else if rank(b) < rank(a) {
                (false, true)
            } else {
                (false, false)
            }
        }
        _ => (false, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    fn workspace() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "loong-config-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(dir.join(".loongagent")).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn the_builtin_ladder_is_the_ceiling_of_permissiveness() {
        let ws = workspace();
        // A profile that tries to make out-of-bounds actions free.
        let user = write(&ws, "u.toml", "[permissions]\nout_of_bounds = \"allow\"\n");
        let profile = resolve(&[(Layer::User, user)], &ws).unwrap();
        assert_eq!(
            profile.permission(ActionClass::OutOfBounds),
            Some(Verdict::Ask),
            "the built-in baseline holds; a profile may only narrow it"
        );
        let row = profile.explain("permissions.out_of_bounds").unwrap();
        assert_eq!(row.layer, Layer::Builtin);
        assert_eq!(row.overridden.len(), 1, "the widening attempt is on the record");

        // Narrowing the baseline is allowed.
        let strict = write(&ws, "s.toml", "[permissions]\nout_of_bounds = \"deny\"\n");
        let profile = resolve(&[(Layer::User, strict)], &ws).unwrap();
        assert_eq!(profile.permission(ActionClass::OutOfBounds), Some(Verdict::Deny));
        assert_eq!(
            profile.explain("permissions.out_of_bounds").unwrap().layer,
            Layer::User
        );
    }

    #[test]
    fn a_single_layer_resolves_with_provenance() {
        let ws = workspace();
        let user = write(
            &ws,
            "user.toml",
            "model = \"qwen\"\nbudget_tokens = 50000\n\n[permissions]\ninvestigate = \"allow\"\nwrite_workspace = \"ask\"\n",
        );
        let profile = resolve(&[(Layer::User, user)], &ws).unwrap();
        assert_eq!(profile.model.as_deref(), Some("qwen"));
        assert_eq!(profile.budget_tokens, Some(50_000));
        assert_eq!(profile.permission(ActionClass::WriteWorkspace), Some(Verdict::Ask));

        let row = profile.explain("model").unwrap();
        assert_eq!(row.layer, Layer::User);
        assert_eq!(row.kind, Kind::FreeOverride);
        assert!(row.source.ends_with("user.toml"));
    }

    #[test]
    fn narrow_only_keys_only_narrow() {
        let ws = workspace();
        let org = write(
            &ws,
            "org.toml",
            "budget_tokens = 100000\nmax_steps = 20\n\n[permissions]\ninvestigate = \"deny\"\nwrite_workspace = \"ask\"\n",
        );
        // A later layer tries to widen everything.
        let user = write(
            &ws,
            "user.toml",
            "budget_tokens = 500000\nmax_steps = 50\n\n[permissions]\ninvestigate = \"allow\"\nwrite_workspace = \"allow\"\n",
        );
        let profile =
            resolve(&[(Layer::Org, org), (Layer::User, user)], &ws).unwrap();

        assert_eq!(profile.budget_tokens, Some(100_000), "the org's ceiling holds");
        assert_eq!(profile.max_steps, Some(20));
        assert_eq!(
            profile.permission(ActionClass::Investigate),
            Some(Verdict::Deny),
            "a lower layer cannot loosen a deny"
        );
        assert_eq!(profile.permission(ActionClass::WriteWorkspace), Some(Verdict::Ask));
        let row = profile.explain("budget_tokens").unwrap();
        assert_eq!(row.layer, Layer::Org, "the strictest value is the one in force");
        assert_eq!(row.overridden.len(), 1, "and the widening attempt is recorded");
    }

    #[test]
    fn narrowing_is_allowed_and_recorded() {
        let ws = workspace();
        let org = write(&ws, "org.toml", "budget_tokens = 100000\n");
        let user = write(&ws, "user.toml", "budget_tokens = 20000\n");
        let profile = resolve(&[(Layer::Org, org), (Layer::User, user)], &ws).unwrap();
        assert_eq!(profile.budget_tokens, Some(20_000));
        assert_eq!(profile.explain("budget_tokens").unwrap().layer, Layer::User);
    }

    #[test]
    fn free_override_takes_the_most_local_layer() {
        let ws = workspace();
        let org = write(&ws, "org.toml", "model = \"org-model\"\n");
        let user = write(&ws, "user.toml", "model = \"my-model\"\n");
        let session = write(&ws, "session.toml", "model = \"this-run\"\n");
        let profile = resolve(
            &[(Layer::Org, org), (Layer::User, user), (Layer::Session, session)],
            &ws,
        )
        .unwrap();
        assert_eq!(profile.model.as_deref(), Some("this-run"));
        assert_eq!(profile.explain("model").unwrap().overridden.len(), 2);
    }

    #[test]
    fn a_repository_may_not_choose_where_prompts_go() {
        let ws = workspace();
        for key in ["model", "base_url", "api_key"] {
            let project = write(
                &ws,
                "project.toml",
                &match key {
                    "api_key" => "api_key = \"env:KEY\"\n".to_string(),
                    "base_url" => "base_url = \"http://elsewhere.invalid/v1\"\n".to_string(),
                    _ => format!("{key} = \"x\"\n"),
                },
            );
            let err = resolve(&[(Layer::Project, project)], &ws).unwrap_err().to_string();
            assert!(
                err.contains("may not set") || err.contains("must be a reference"),
                "{key}: {err}"
            );
        }
    }

    #[test]
    fn a_project_skill_dir_must_stay_inside_the_workspace() {
        let ws = workspace();
        let inside = write(&ws, "p1.toml", "skill_dirs = [\"./team-skills\"]\n");
        // Inside is fine…
        assert!(resolve(&[(Layer::Project, inside)], &ws).is_ok());

        // …but reaching out of the tree is not.
        let outside = write(&ws, "p2.toml", "skill_dirs = [\"/etc/skills\"]\n");
        let err = resolve(&[(Layer::Project, outside)], &ws).unwrap_err().to_string();
        assert!(err.contains("must stay inside the workspace"), "{err}");
    }

    #[test]
    fn secrets_never_live_in_config() {
        let ws = workspace();
        let literal = write(&ws, "u.toml", "api_key = \"sk-live-abcdef\"\n");
        let err = resolve(&[(Layer::User, literal)], &ws).unwrap_err().to_string();
        assert!(err.contains("secrets never live in config"), "{err}");

        let reference = write(&ws, "u2.toml", "api_key = \"env:LOONG_KEY\"\n");
        let profile = resolve(&[(Layer::User, reference)], &ws).unwrap();
        assert_eq!(profile.api_key.as_deref(), Some("env:LOONG_KEY"));
    }

    #[test]
    fn add_only_keys_union_across_layers() {
        let ws = workspace();
        std::fs::create_dir_all(ws.join("team-skills")).unwrap();
        let project = write(
            &ws,
            "p.toml",
            "skill_dirs = [\"./team-skills\"]\ndisable_skills = [\"org-loud\"]\n",
        );
        let user = write(
            &ws,
            "u.toml",
            "skill_dirs = [\"/home/someone/skills\"]\ndisable_skills = [\"other\"]\n",
        );
        let profile = resolve(&[(Layer::Project, project), (Layer::User, user)], &ws).unwrap();
        assert_eq!(profile.skill_dirs.len(), 2, "{:?}", profile.skill_dirs);
        // Union in precedence order (project before user), not alphabetically: the
        // order is how a reader sees which layer asked for what.
        assert_eq!(profile.disable_skills, vec!["org-loud".to_string(), "other".to_string()]);
        let row = profile.explain("skill_dirs").unwrap();
        assert_eq!(row.kind, Kind::AddOnly);
        assert_eq!(row.layer, Layer::Project, "the first layer that set it owns the row");
    }

    #[test]
    fn unknown_keys_and_bad_values_are_loud() {
        let ws = workspace();
        // A typo must be named, not ignored: a guard that silently does not exist
        // is the failure this rule exists to prevent.
        let unknown = write(&ws, "a.toml", "budget_token = 100\n");
        let err = resolve(&[(Layer::User, unknown)], &ws).unwrap_err().to_string();
        assert!(err.contains("unknown") && err.contains("budget_token"), "{err}");

        let bad_verdict = write(&ws, "b.toml", "[permissions]\ninvestigate = \"maybe\"\n");
        let err = resolve(&[(Layer::User, bad_verdict)], &ws).unwrap_err().to_string();
        assert!(err.contains("allow | ask | deny"), "{err}");

        let bad_class = write(&ws, "c.toml", "[permissions]\nwhatever = \"allow\"\n");
        let err = resolve(&[(Layer::User, bad_class)], &ws).unwrap_err().to_string();
        assert!(err.contains("unknown action class"), "{err}");

        let bad_url = write(&ws, "d.toml", "base_url = \"ftp://x/y\"\n");
        let err = resolve(&[(Layer::User, bad_url)], &ws).unwrap_err().to_string();
        assert!(err.contains("base_url must be"), "{err}");

        let bad_toml = write(&ws, "e.toml", "not toml {{{\n");
        assert!(matches!(
            resolve(&[(Layer::User, bad_toml)], &ws).unwrap_err(),
            ConfigError::Parse(_)
        ));
    }

    #[test]
    fn absent_layers_are_skipped_not_invented() {
        let ws = workspace();
        let missing = ws.join("nope.toml");
        let profile = resolve(&[(Layer::User, missing)], &ws).unwrap();
        // Nothing was invented from a file that is not there…
        assert!(profile.model.is_none());
        assert!(profile.budget_tokens.is_none());
        assert!(profile.skill_dirs.is_empty());
        // …but the built-in ladder is always the baseline of the permission chain.
        assert_eq!(profile.explain_all().len(), 3);
        assert!(profile.explain_all().iter().all(|row| row.layer == Layer::Builtin));
        assert_eq!(
            profile.permission(ActionClass::OutOfBounds),
            Some(Verdict::Ask),
            "the built-in default, not an invented one"
        );
    }

    #[test]
    fn every_known_key_declares_a_kind() {
        for key in [
            "model", "base_url", "profile", "budget_tokens", "keep_newest_tokens", "max_steps",
            "compact_at_percent", "permissions", "skill_dirs", "disable_skills", "api_key",
        ] {
            assert!(kind_of(key).is_some(), "{key} has no kind");
        }
        assert!(kind_of("not_a_key").is_none());
    }

    #[test]
    fn the_layer_order_is_the_rule() {
        assert!(Layer::Builtin < Layer::Org);
        assert!(Layer::Org < Layer::Project);
        assert!(Layer::Project < Layer::User);
        assert!(Layer::User < Layer::Session);
    }

    #[test]
    fn layer_paths_are_where_they_claim_to_be() {
        let ws = workspace();
        let paths = layer_paths(&ws, Some(Path::new("/tmp/session.toml")));
        let layers: Vec<Layer> = paths.iter().map(|(l, _)| *l).collect();
        assert_eq!(layers, vec![Layer::Org, Layer::Project, Layer::User, Layer::Session]);
        assert_eq!(paths[1].1, ws.join(".loongagent/profile.toml"));
        assert_eq!(paths[3].1, PathBuf::from("/tmp/session.toml"));
    }
}
