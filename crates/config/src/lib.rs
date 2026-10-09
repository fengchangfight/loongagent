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
        "model" | "base_url" | "profile" | "models" | "router.default" | "router.rules" => {
            Kind::FreeOverride
        }
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
    Model(ModelSpec),
    Rules(Vec<RouterRule>),
}

impl Value {
    pub fn render(&self) -> String {
        match self {
            Value::Text(t) => t.clone(),
            Value::Number(n) => n.to_string(),
            Value::Verdict(v) => format!("{v:?}").to_lowercase(),
            Value::List(items) => items.join(", "),
            Value::Model(spec) => format!("{} → {} ({})", spec.name, spec.model, spec.base_url),
            Value::Rules(rules) => rules
                .iter()
                .map(|rule| format!("{} → {}", rule.condition.render(), rule.model))
                .collect::<Vec<_>>()
                .join("; "),
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

/// A model a profile offers. Engineers pick one of these by name (ADR-0005/0007: a
/// profile names models, not endpoints), and the router chooses between them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSpec {
    pub name: String,
    /// The adapter to build it with; only `openai-compat` exists today.
    pub kind: String,
    pub base_url: String,
    pub model: String,
    /// A *reference* (`env:NAME`, `file:PATH#KEY`, `keychain:…`), never a secret.
    pub api_key: Option<String>,
    pub budget_tokens: Option<u64>,
}

/// The facts a routing rule may read. Deliberately small and inspectable: a model
/// choice that cannot be explained from the session is a model choice nobody can
/// debug.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Facts {
    pub context_tokens: u64,
    pub step: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fact {
    ContextTokens,
    Step,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Gt,
    Ge,
    Lt,
    Le,
    Eq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Condition {
    pub fact: Fact,
    pub op: Op,
    pub value: u64,
}

impl Condition {
    pub fn matches(&self, facts: &Facts) -> bool {
        let actual = match self.fact {
            Fact::ContextTokens => facts.context_tokens,
            Fact::Step => facts.step,
        };
        match self.op {
            Op::Gt => actual > self.value,
            Op::Ge => actual >= self.value,
            Op::Lt => actual < self.value,
            Op::Le => actual <= self.value,
            Op::Eq => actual == self.value,
        }
    }

    pub fn render(&self) -> String {
        let fact = match self.fact {
            Fact::ContextTokens => "context_tokens",
            Fact::Step => "step",
        };
        let op = match self.op {
            Op::Gt => ">",
            Op::Ge => ">=",
            Op::Lt => "<",
            Op::Le => "<=",
            Op::Eq => "==",
        };
        format!("{fact} {op} {}", self.value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouterRule {
    pub condition: Condition,
    pub model: String,
}

/// Ordered, first-match-wins. The rule that fired is recorded with the choice, so
/// "why this model?" has an answer (ADR-0008 §4 applied to routing).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Router {
    pub default: Option<String>,
    pub rules: Vec<RouterRule>,
}

impl Router {
    /// `(model, Some(rule))` when a rule fired, `(default, None)` otherwise.
    pub fn select(&self, facts: &Facts) -> (Option<&str>, Option<&RouterRule>) {
        for rule in &self.rules {
            if rule.condition.matches(facts) {
                return (Some(rule.model.as_str()), Some(rule));
            }
        }
        (self.default.as_deref(), None)
    }
}

/// The effective configuration, after merging every layer.
#[derive(Debug, Clone, Default)]
pub struct Profile {
    /// Every model the profile offers, in declaration order.
    pub models: Vec<ModelSpec>,
    pub router: Router,
    /// Convenience mirrors of the model a turn starts with, so a caller that only
    /// needs "the" model does not have to consult the router.
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
    /// The model a turn starts with: the router's default, else the first declared.
    pub fn default_model(&self) -> Option<&ModelSpec> {
        match self.router.default.as_deref() {
            Some(name) => self.models.iter().find(|m| m.name == name),
            None => self.models.first(),
        }
    }

    pub fn permission(&self, class: ActionClass) -> Option<Verdict> {
        self.permissions
            .iter()
            .find(|(c, _)| *c == class)
            .map(|(_, v)| *v)
    }

    /// The verdict **and** the layer that set it, for the PDP's rule string.
    pub fn permission_with_source(&self, class: ActionClass) -> Option<(Verdict, Layer)> {
        let key = format!(
            "permissions.{}",
            match class {
                ActionClass::Investigate => "investigate",
                ActionClass::WriteWorkspace => "write_workspace",
                ActionClass::OutOfBounds => "out_of_bounds",
            }
        );
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
    #[serde(default)]
    models: Vec<RawModel>,
    router: Option<RawRouter>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawModel {
    name: String,
    kind: Option<String>,
    base_url: String,
    model: String,
    api_key: Option<String>,
    budget_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRouter {
    default: Option<String>,
    #[serde(default)]
    rules: Vec<RawRule>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    when: String,
    #[serde(rename = "use")]
    model: String,
}

/// `context_tokens > 150000` → a condition. Unknown facts are errors that list the
/// ones that exist, because a rule that never fires is worse than no rule.
pub fn parse_condition(text: &str) -> Result<Condition, String> {
    // Explicit, because `split_once` on a set of characters hides which operator it
    // found — and `>=` must not be read as `>`.
    let operators: [(&str, Op); 5] = [
        (">=", Op::Ge),
        ("<=", Op::Le),
        ("==", Op::Eq),
        (">", Op::Gt),
        ("<", Op::Lt),
    ];
    for (symbol, op) in operators {
        if let Some((fact, value)) = text.split_once(symbol) {
            let fact = match fact.trim() {
                "context_tokens" => Fact::ContextTokens,
                "step" => Fact::Step,
                other => {
                    return Err(format!(
                        "unknown fact {other:?} — the facts a rule may read are context_tokens and step"
                    ))
                }
            };
            let value = value.trim().parse::<u64>().map_err(|_| {
                format!(
                    "{text:?} needs a whole number to compare against, got {:?}",
                    value.trim()
                )
            })?;
            return Ok(Condition { fact, op, value });
        }
    }
    Err(format!(
        "{text:?} needs a comparison, e.g. `context_tokens > 150000`"
    ))
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
        let raw: RawProfile = toml::from_str(&text)
            .map_err(|e| ConfigError::Parse(format!("{}: {e}", path.display())))?;
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
            check_secret_reference(&source, "api_key", &api_key)?;
            values.push(("api_key".to_string(), Value::Text(api_key)));
        }

        // A profile may offer several models; the router picks between them.
        let mut named: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for model in &raw.models {
            if layer == Layer::Project {
                return Err(ConfigError::Invalid(format!(
                    "{source}: the project layer may not define models — a repository must not \
                     choose where prompts go (ADR-0008)"
                )));
            }
            if !named.insert(model.name.clone()) {
                return Err(ConfigError::Invalid(format!(
                    "{source}: model {:?} is defined twice in this file",
                    model.name
                )));
            }
            let kind = model
                .kind
                .clone()
                .unwrap_or_else(|| "openai-compat".to_string());
            if !ADAPTER_KINDS.contains(&kind.as_str()) {
                return Err(ConfigError::Invalid(format!(
                    "{source}: model {:?} asks for kind {kind:?}; the adapters that exist are {}",
                    model.name,
                    ADAPTER_KINDS.join(", ")
                )));
            }
            check_endpoint_reference(&source, "base_url", &model.base_url)?;
            if let Some(key) = &model.api_key {
                check_secret_reference(&source, &format!("models.{}.api_key", model.name), key)?;
            }
            values.push((
                format!("models.{}", model.name),
                Value::Model(ModelSpec {
                    name: model.name.clone(),
                    kind,
                    base_url: model.base_url.clone(),
                    model: model.model.clone(),
                    api_key: model.api_key.clone(),
                    budget_tokens: model.budget_tokens,
                }),
            ));
        }
        if let Some(router) = &raw.router {
            if let Some(default) = &router.default {
                values.push(("router.default".to_string(), Value::Text(default.clone())));
            }
            if !router.rules.is_empty() {
                let mut rules = Vec::new();
                for rule in &router.rules {
                    let condition = parse_condition(&rule.when)
                        .map_err(|e| ConfigError::Invalid(format!("{source}: rule: {e}")))?;
                    rules.push(RouterRule {
                        condition,
                        model: rule.model.clone(),
                    });
                }
                values.push(("router.rules".to_string(), Value::Rules(rules)));
            }
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
            values.push((
                "skill_dirs".to_string(),
                Value::Text(resolved.display().to_string()),
            ));
        }
        for name in raw.disable_skills {
            values.push(("disable_skills".to_string(), Value::Text(name)));
        }

        for (key, _) in &values {
            let base = key.split('.').next().unwrap_or(key);
            // `permissions.*` and `models.*` take their kind from their table; the
            // router's keys are declared in full (`router.default`, `router.rules`).
            let known = kind_of(key).is_some()
                || (matches!(base, "permissions" | "models") && kind_of(base).is_some());
            if !known {
                return Err(ConfigError::Invalid(format!(
                    "{source}: unknown key {base:?}"
                )));
            }
        }
        Ok(Self {
            layer,
            source,
            values,
        })
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
                (
                    "permissions.investigate".to_string(),
                    Value::Verdict(Verdict::Allow),
                ),
                (
                    "permissions.write_workspace".to_string(),
                    Value::Verdict(Verdict::Allow),
                ),
                (
                    "permissions.out_of_bounds".to_string(),
                    Value::Verdict(Verdict::Ask),
                ),
            ],
        }
    }
}

const ADAPTER_KINDS: &[&str] = &["openai-compat"];

fn check_secret_reference(source: &str, key: &str, value: &str) -> Result<(), ConfigError> {
    let known = ["env:", "file:", "keychain:", "gateway:"];
    if known.iter().any(|prefix| value.starts_with(prefix)) {
        return Ok(());
    }
    Err(ConfigError::Invalid(format!(
        "{source}: `{key}` must be a reference (env:NAME, file:PATH#KEY, keychain:…, gateway:…) — \
         secrets never live in config (ADR-0008 §5)"
    )))
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
    let mut profile = merge(&present);
    finish(&mut profile)?;
    Ok(profile)
}

/// Rules that only make sense once every layer has been merged: the shorthand form,
/// and a router that may only name models that exist.
fn finish(profile: &mut Profile) -> Result<(), ConfigError> {
    // `model`/`base_url`/`api_key` on their own are the one-model shorthand, which
    // is what every profile written before `[[models]]` existed looks like.
    if profile.models.is_empty() {
        if let (Some(base_url), Some(model)) = (profile.base_url.clone(), profile.model.clone()) {
            profile.models.push(ModelSpec {
                name: "default".to_string(),
                kind: "openai-compat".to_string(),
                base_url,
                model,
                api_key: profile.api_key.clone(),
                budget_tokens: profile.budget_tokens,
            });
        }
    }
    if profile.router.default.is_none() {
        profile.router.default = profile.models.first().map(|m| m.name.clone());
    }

    let known: Vec<String> = profile.models.iter().map(|m| m.name.clone()).collect();
    let mut referenced: Vec<&str> = profile
        .router
        .rules
        .iter()
        .map(|r| r.model.as_str())
        .collect();
    if let Some(default) = profile.router.default.as_deref() {
        referenced.push(default);
    }
    for name in referenced {
        if !known.iter().any(|k| k == name) {
            return Err(ConfigError::Invalid(format!(
                "the router names a model {name:?} that no layer defines (defined: {})",
                if known.is_empty() {
                    "none".to_string()
                } else {
                    known.join(", ")
                }
            )));
        }
    }

    // Keep the convenience mirrors pointed at the model a turn starts with.
    if let Some(first) = profile.default_model().cloned() {
        profile.model = Some(first.model.clone());
        profile.base_url = Some(first.base_url.clone());
        profile.api_key = first.api_key.clone();
    }
    Ok(())
}

fn merge(layers: &[LayerValues]) -> Profile {
    let mut profile = Profile::default();
    // Key → the winning row so far, plus what lost.
    let mut winners: BTreeMap<String, Provenance> = BTreeMap::new();

    for layer in layers {
        for (key, value) in &layer.values {
            let base = key.split('.').next().unwrap_or(key).to_string();
            // The full key first: `router.default` is a key of its own, while
            // `permissions.*` and `models.*` take their kind from the table.
            let kind = kind_of(key)
                .or_else(|| kind_of(&base))
                .expect("checked when loading");
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
                Kind::FreeOverride if key.starts_with("models.") => {
                    // A model definition is free-override (the most local layer decides
                    // which endpoint *this user* talks to), but its budget is not: an
                    // organisation's ceiling survives a local redefinition.
                    match (winners.get_mut(key), value) {
                        (Some(current), Value::Model(new)) => {
                            if let Value::Model(old) = &current.value {
                                let mut merged = new.clone();
                                merged.budget_tokens = match (old.budget_tokens, new.budget_tokens)
                                {
                                    (Some(a), Some(b)) => Some(a.min(b)),
                                    (Some(a), None) => Some(a),
                                    (None, b) => b,
                                };
                                let old_value = current.value.clone();
                                let old_layer = current.layer;
                                current.value = Value::Model(merged);
                                current.layer = layer.layer;
                                current.source = layer.source.clone();
                                current.overridden.push((old_layer, old_value));
                            }
                        }
                        (None, _) => {
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
                        _ => {}
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
            (key, Value::Model(spec)) if key.starts_with("models.") => {
                profile.models.push(spec.clone())
            }
            ("router.default", Value::Text(name)) => profile.router.default = Some(name.clone()),
            ("router.rules", Value::Rules(rules)) => profile.router.rules = rules.clone(),
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
    profile.models.sort_by(|a, b| a.name.cmp(&b.name));
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
        assert_eq!(
            row.overridden.len(),
            1,
            "the widening attempt is on the record"
        );

        // Narrowing the baseline is allowed.
        let strict = write(&ws, "s.toml", "[permissions]\nout_of_bounds = \"deny\"\n");
        let profile = resolve(&[(Layer::User, strict)], &ws).unwrap();
        assert_eq!(
            profile.permission(ActionClass::OutOfBounds),
            Some(Verdict::Deny)
        );
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
        assert_eq!(
            profile.permission(ActionClass::WriteWorkspace),
            Some(Verdict::Ask)
        );

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
        let profile = resolve(&[(Layer::Org, org), (Layer::User, user)], &ws).unwrap();

        assert_eq!(
            profile.budget_tokens,
            Some(100_000),
            "the org's ceiling holds"
        );
        assert_eq!(profile.max_steps, Some(20));
        assert_eq!(
            profile.permission(ActionClass::Investigate),
            Some(Verdict::Deny),
            "a lower layer cannot loosen a deny"
        );
        assert_eq!(
            profile.permission(ActionClass::WriteWorkspace),
            Some(Verdict::Ask)
        );
        let row = profile.explain("budget_tokens").unwrap();
        assert_eq!(
            row.layer,
            Layer::Org,
            "the strictest value is the one in force"
        );
        assert_eq!(
            row.overridden.len(),
            1,
            "and the widening attempt is recorded"
        );
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
            &[
                (Layer::Org, org),
                (Layer::User, user),
                (Layer::Session, session),
            ],
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
            let err = resolve(&[(Layer::Project, project)], &ws)
                .unwrap_err()
                .to_string();
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
        let err = resolve(&[(Layer::Project, outside)], &ws)
            .unwrap_err()
            .to_string();
        assert!(err.contains("must stay inside the workspace"), "{err}");
    }

    #[test]
    fn secrets_never_live_in_config() {
        let ws = workspace();
        let literal = write(&ws, "u.toml", "api_key = \"sk-live-abcdef\"\n");
        let err = resolve(&[(Layer::User, literal)], &ws)
            .unwrap_err()
            .to_string();
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
        assert_eq!(
            profile.disable_skills,
            vec!["org-loud".to_string(), "other".to_string()]
        );
        let row = profile.explain("skill_dirs").unwrap();
        assert_eq!(row.kind, Kind::AddOnly);
        assert_eq!(
            row.layer,
            Layer::Project,
            "the first layer that set it owns the row"
        );
    }

    #[test]
    fn unknown_keys_and_bad_values_are_loud() {
        let ws = workspace();
        // A typo must be named, not ignored: a guard that silently does not exist
        // is the failure this rule exists to prevent.
        let unknown = write(&ws, "a.toml", "budget_token = 100\n");
        let err = resolve(&[(Layer::User, unknown)], &ws)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("unknown") && err.contains("budget_token"),
            "{err}"
        );

        let bad_verdict = write(&ws, "b.toml", "[permissions]\ninvestigate = \"maybe\"\n");
        let err = resolve(&[(Layer::User, bad_verdict)], &ws)
            .unwrap_err()
            .to_string();
        assert!(err.contains("allow | ask | deny"), "{err}");

        let bad_class = write(&ws, "c.toml", "[permissions]\nwhatever = \"allow\"\n");
        let err = resolve(&[(Layer::User, bad_class)], &ws)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown action class"), "{err}");

        let bad_url = write(&ws, "d.toml", "base_url = \"ftp://x/y\"\n");
        let err = resolve(&[(Layer::User, bad_url)], &ws)
            .unwrap_err()
            .to_string();
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
        assert!(profile
            .explain_all()
            .iter()
            .all(|row| row.layer == Layer::Builtin));
        assert_eq!(
            profile.permission(ActionClass::OutOfBounds),
            Some(Verdict::Ask),
            "the built-in default, not an invented one"
        );
    }

    #[test]
    fn several_models_and_a_router_can_be_declared() {
        let ws = workspace();
        let user = write(
            &ws,
            "u.toml",
            "[[models]]\n\
             name = \"fast\"\n\
             base_url = \"https://api.deepseek.com\"\n\
             model = \"deepseek-chat\"\n\
             api_key = \"env:DEEPSEEK_API_KEY\"\n\
             budget_tokens = 128000\n\n\
             [[models]]\n\
             name = \"long\"\n\
             base_url = \"https://api.moonshot.cn/v1\"\n\
             model = \"kimi-k2\"\n\
             budget_tokens = 512000\n\n\
             [router]\n\
             default = \"fast\"\n\
             [[router.rules]]\n\
             when = \"context_tokens > 150000\"\n\
             use = \"long\"\n",
        );
        let profile = resolve(&[(Layer::User, user)], &ws).unwrap();
        assert_eq!(profile.models.len(), 2);
        assert_eq!(profile.default_model().unwrap().name, "fast");
        assert_eq!(
            profile.models[0].kind, "openai-compat",
            "the default adapter"
        );
        assert_eq!(profile.models[1].budget_tokens, Some(512_000));

        let (chosen, rule) = profile.router.select(&Facts {
            context_tokens: 10,
            step: 1,
        });
        assert_eq!(chosen, Some("fast"));
        assert!(rule.is_none(), "the default, not a rule");
        let (chosen, rule) = profile.router.select(&Facts {
            context_tokens: 200_000,
            step: 4,
        });
        assert_eq!(chosen, Some("long"));
        assert_eq!(rule.unwrap().condition.render(), "context_tokens > 150000");

        // Every model answers "why is this the value?"
        assert_eq!(profile.explain("models.fast").unwrap().layer, Layer::User);
        assert!(profile.explain("router.rules").is_some());
    }

    #[test]
    fn the_shorthand_still_means_one_model() {
        let ws = workspace();
        let user = write(
            &ws,
            "u.toml",
            "base_url = \"https://api.deepseek.com\"\nmodel = \"deepseek-chat\"\n",
        );
        let profile = resolve(&[(Layer::User, user)], &ws).unwrap();
        assert_eq!(profile.models.len(), 1, "one model, synthesized");
        assert_eq!(profile.models[0].name, "default");
        assert_eq!(profile.router.default.as_deref(), Some("default"));
        assert_eq!(
            profile.base_url.as_deref(),
            Some("https://api.deepseek.com")
        );
    }

    #[test]
    fn a_repository_may_not_define_a_model() {
        let ws = workspace();
        let project = write(
            &ws,
            "p.toml",
            "[[models]]\nname = \"sneaky\"\nbase_url = \"http://elsewhere.invalid\"\nmodel = \"m\"\n",
        );
        let err = resolve(&[(Layer::Project, project)], &ws)
            .unwrap_err()
            .to_string();
        assert!(err.contains("may not define models"), "{err}");
    }

    #[test]
    fn a_project_may_route_among_models_it_did_not_define() {
        let ws = workspace();
        let user = write(
            &ws,
            "u.toml",
            "[[models]]\nname = \"fast\"\nbase_url = \"https://a.invalid\"\nmodel = \"m\"\n\n\
             [[models]]\nname = \"cheap\"\nbase_url = \"https://b.invalid\"\nmodel = \"m\"\n",
        );
        let project = write(
            &ws,
            "p.toml",
            "[router]\ndefault = \"cheap\"\n[[router.rules]]\nwhen = \"step > 3\"\nuse = \"cheap\"\n",
        );
        let profile = resolve(&[(Layer::User, user), (Layer::Project, project)], &ws).unwrap();
        assert_eq!(
            profile.router.default.as_deref(),
            Some("cheap"),
            "the project may shape routing within what exists"
        );
    }

    #[test]
    fn a_model_budget_only_narrows_across_layers() {
        let ws = workspace();
        let org = write(
            &ws,
            "org.toml",
            "[[models]]\nname = \"m\"\nbase_url = \"https://a.invalid\"\nmodel = \"x\"\nbudget_tokens = 100000\n",
        );
        // The user redefines the same model with a larger budget, and a different endpoint.
        let user = write(
            &ws,
            "u.toml",
            "[[models]]\nname = \"m\"\nbase_url = \"https://b.invalid\"\nmodel = \"y\"\nbudget_tokens = 500000\n",
        );
        let profile = resolve(&[(Layer::Org, org), (Layer::User, user)], &ws).unwrap();
        let model = &profile.models[0];
        assert_eq!(
            model.base_url, "https://b.invalid",
            "the local endpoint wins"
        );
        assert_eq!(model.model, "y");
        assert_eq!(
            model.budget_tokens,
            Some(100_000),
            "but the org's ceiling does not"
        );
    }

    #[test]
    fn a_router_that_names_a_missing_model_is_an_error() {
        let ws = workspace();
        let user = write(
            &ws,
            "u.toml",
            "[[models]]\nname = \"fast\"\nbase_url = \"https://a.invalid\"\nmodel = \"m\"\n\n\
             [router]\ndefault = \"fast\"\n[[router.rules]]\nwhen = \"step > 1\"\nuse = \"ghost\"\n",
        );
        let err = resolve(&[(Layer::User, user)], &ws)
            .unwrap_err()
            .to_string();
        assert!(err.contains("ghost") && err.contains("fast"), "{err}");
    }

    #[test]
    fn conditions_are_checked_where_they_are_written() {
        let ws = workspace();
        for (when, expected) in [
            ("token_count > 10", "unknown fact"),
            ("context_tokens 10", "needs a comparison"),
            ("context_tokens > many", "whole number"),
        ] {
            let user = write(
                &ws,
                "u.toml",
                &format!(
                    "base_url = \"https://a.invalid\"\nmodel = \"m\"\n[router]\n[[router.rules]]\nwhen = \"{when}\"\nuse = \"default\"\n"
                ),
            );
            let err = resolve(&[(Layer::User, user)], &ws)
                .unwrap_err()
                .to_string();
            assert!(err.contains(expected), "{when}: {err}");
        }
    }

    #[test]
    fn an_unknown_adapter_kind_is_refused_with_the_list() {
        let ws = workspace();
        let user = write(
            &ws,
            "u.toml",
            "[[models]]\nname = \"m\"\nkind = \"telepathy\"\nbase_url = \"https://a.invalid\"\nmodel = \"x\"\n",
        );
        let err = resolve(&[(Layer::User, user)], &ws)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("telepathy") && err.contains("openai-compat"),
            "{err}"
        );
    }

    #[test]
    fn conditions_evaluate_the_facts_they_name() {
        let condition = parse_condition("context_tokens >= 1000").unwrap();
        assert!(!condition.matches(&Facts {
            context_tokens: 999,
            step: 0
        }));
        assert!(condition.matches(&Facts {
            context_tokens: 1000,
            step: 0
        }));
        let step = parse_condition("step == 3").unwrap();
        assert!(step.matches(&Facts {
            context_tokens: 0,
            step: 3
        }));
        assert!(!step.matches(&Facts {
            context_tokens: 0,
            step: 4
        }));
        let less = parse_condition("step < 2").unwrap();
        assert!(less.matches(&Facts {
            context_tokens: 0,
            step: 1
        }));
    }

    #[test]
    fn the_router_takes_the_first_matching_rule() {
        let router = Router {
            default: Some("fallback".to_string()),
            rules: vec![
                RouterRule {
                    condition: parse_condition("context_tokens > 10").unwrap(),
                    model: "big".to_string(),
                },
                RouterRule {
                    condition: parse_condition("context_tokens > 10").unwrap(),
                    model: "never".to_string(),
                },
            ],
        };
        let (model, rule) = router.select(&Facts {
            context_tokens: 11,
            step: 0,
        });
        assert_eq!(model, Some("big"));
        assert_eq!(rule.unwrap().model, "big");
        let (model, rule) = router.select(&Facts {
            context_tokens: 1,
            step: 0,
        });
        assert_eq!(model, Some("fallback"));
        assert!(rule.is_none());
    }

    #[test]
    fn every_known_key_declares_a_kind() {
        for key in [
            "model",
            "base_url",
            "profile",
            "budget_tokens",
            "keep_newest_tokens",
            "max_steps",
            "compact_at_percent",
            "permissions",
            "skill_dirs",
            "disable_skills",
            "api_key",
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
        assert_eq!(
            layers,
            vec![Layer::Org, Layer::Project, Layer::User, Layer::Session]
        );
        assert_eq!(paths[1].1, ws.join(".loongagent/profile.toml"));
        assert_eq!(paths[3].1, PathBuf::from("/tmp/session.toml"));
    }
}
