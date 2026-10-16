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
//! | **narrow-only** | `budget_tokens`, `keep_newest_tokens`, `max_steps`, `compact_at_percent`, `max_context_size`, `reserved_context_size`, `[permissions]` | strictest wins; a lower layer may only narrow |
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

pub mod catalog;

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
    /// A verified pack from the control plane (ADR-0008 §6, ADR-0016). Ranked just
    /// above the local org file — a managed install's policy is the authoritative
    /// delivery, and a stale local file must not shadow it — and *locked* against the
    /// layers below: an engineer picks a profile, not an endpoint (T-07).
    Control,
    Project,
    User,
    Session,
}

impl Layer {
    pub fn as_str(self) -> &'static str {
        match self {
            Layer::Builtin => "builtin",
            Layer::Org => "org",
            Layer::Control => "control plane",
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
        // Which mode this install runs in. Free-override like `model`: the most local
        // layer that says wins, so a `--local` session layer beats the user's profile.
        "mode" => Kind::FreeOverride,
        // Which mode to run in, and which local model to use. Endpoint-shaped keys
        // (`summarizer.base_url`, `summarizer.model`) are free-override here and
        // refused at the project layer in `from_file`.
        "context_engine.mode"
        | "context_engine"
        | "summarizer.kind"
        | "summarizer.base_url"
        | "summarizer.model" => Kind::FreeOverride,
        // The one profile-related key a user owns: which named bundle to run.
        "control.profile" => Kind::FreeOverride,
        "budget_tokens"
        | "keep_newest_tokens"
        | "max_steps"
        | "compact_at_percent"
        | "max_context_size"
        | "reserved_context_size"
        // 0/1 switches: `0` (off) is stricter, so a firm can force either facility
        // off and a lower layer cannot loosen it.
        | "context_engine.enabled"
        | "summarizer.enabled"
        | "context_engine.episode_events"
        | "context_engine.l4_max_tokens"
        // One idle window's budget for the background summariser. Smaller is
        // stricter: a firm may cap the background's spend, nobody may raise it.
        | "summarizer.window_calls"
        | "summarizer.window_tokens"
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
    /// How many tokens this model can hold in one request — prompt and answer together.
    ///
    /// The physical ceiling, next to `budget_tokens` which is the one a person chooses.
    /// `None` means nobody said, and the kernel then has no window to compact against.
    pub max_context_size: Option<u64>,
    /// The catalog entry this model was written as (`provider = "deepseek/…"`), when
    /// it was written that way.
    ///
    /// Kept after expansion because the *provenance* outlives the shorthand: it is
    /// what lets the CLI fall back to a key the user already exported under a
    /// conventional name, and what a surface shows instead of an endpoint.
    pub provider: Option<String>,
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

/// A named bundle of models plus the routing between them (T-07). This is what the
/// control plane publishes and what an engineer picks by name; the description is
/// what they are shown instead of a model list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedProfile {
    pub name: String,
    pub description: String,
    pub models: Vec<ModelSpec>,
    pub router: Router,
}

/// Read a profile pack payload (the TOML inside a signed envelope).
///
/// Kept in this crate because the payload *is* configuration: the control plane and
/// the local agent must agree on what it means, and the merge rules apply to it.
pub fn profiles_from_payload(payload: &str) -> Result<ProfilePack, ConfigError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct RawPayload {
        default_profile: Option<String>,
        #[serde(default)]
        profiles: Vec<RawProfileEntry>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct RawProfileEntry {
        name: String,
        description: Option<String>,
        #[serde(default)]
        models: Vec<RawModel>,
        router: Option<RawRouter>,
    }
    let raw: RawPayload = toml::from_str(payload)
        .map_err(|e| ConfigError::Invalid(format!("the pack payload is not valid TOML: {e}")))?;
    if raw.profiles.is_empty() {
        return Err(ConfigError::Invalid(
            "the pack defines no profiles — a profile administrator must publish at least one"
                .to_string(),
        ));
    }
    let mut profiles = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    for entry in raw.profiles {
        let mut models = Vec::new();
        for model in entry.models {
            let kind = model
                .kind
                .clone()
                .unwrap_or_else(|| "openai-compat".to_string());
            if !ADAPTER_KINDS.contains(&kind.as_str()) {
                return Err(ConfigError::Invalid(format!(
                    "profile {:?}: model {:?} asks for kind {kind:?}; the adapters that exist are {}",
                    entry.name,
                    model.name,
                    ADAPTER_KINDS.join(", ")
                )));
            }
            if let Some(key) = &model.api_key {
                check_pack_secret(
                    &format!("models.{}.api_key", model.name),
                    key,
                    &mut warnings,
                )?;
            }
            let (base_url, model_id, api_key, provider) = finish_model(
                &format!("profile {:?}", entry.name),
                &model.name,
                model.provider,
                model.base_url,
                model.model,
                model.api_key,
            )
            .map_err(ConfigError::Invalid)?;
            models.push(ModelSpec {
                name: model.name,
                kind,
                base_url,
                model: model_id,
                api_key,
                budget_tokens: model.budget_tokens,
                max_context_size: model.max_context_size,
                provider,
            });
        }
        if models.is_empty() {
            return Err(ConfigError::Invalid(format!(
                "profile {:?} defines no models",
                entry.name
            )));
        }
        let mut router = Router::default();
        if let Some(raw_router) = entry.router {
            router.default = raw_router.default;
            for rule in raw_router.rules {
                let condition = parse_condition(&rule.when)
                    .map_err(|e| ConfigError::Invalid(format!("profile {:?}: {e}", entry.name)))?;
                router.rules.push(RouterRule {
                    condition,
                    model: rule.model,
                });
            }
        }
        if router.default.is_none() {
            router.default = models.first().map(|m| m.name.clone());
        }
        let known: Vec<&str> = models.iter().map(|m| m.name.as_str()).collect();
        let mut referenced: Vec<&str> = router.rules.iter().map(|r| r.model.as_str()).collect();
        if let Some(default) = router.default.as_deref() {
            referenced.push(default);
        }
        for name in referenced {
            if !known.contains(&name) {
                return Err(ConfigError::Invalid(format!(
                    "profile {:?}: the router names {name:?}, which it does not define ({})",
                    entry.name,
                    known.join(", ")
                )));
            }
        }
        profiles.push(NamedProfile {
            name: entry.name,
            description: entry.description.unwrap_or_default(),
            models,
            router,
        });
    }
    Ok(ProfilePack {
        default_profile: raw.default_profile,
        profiles,
        warnings,
    })
}

/// A pack's contents: the bundles, and which one runs when a user has not chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfilePack {
    pub default_profile: Option<String>,
    pub profiles: Vec<NamedProfile>,
    /// What an operator should know before serving this pack. Today that is one thing:
    /// the pack carries a credential in the clear, which is a deliberate and temporary
    /// relaxation of ADR-0008 §5 (see its amendment).
    pub warnings: Vec<String>,
}

impl ProfilePack {
    /// The bundle named by the user's choice, else the pack's default, else the
    /// first — and if the user named something that is not there, say so rather than
    /// silently running a different profile.
    pub fn pick(&self, chosen: Option<&str>) -> Result<&NamedProfile, ConfigError> {
        if let Some(name) = chosen {
            return self
                .profiles
                .iter()
                .find(|p| p.name == name)
                .ok_or_else(|| {
                    ConfigError::Invalid(format!(
                        "profile {name:?} is not in the pack (the control plane offers: {})",
                        self.profiles
                            .iter()
                            .map(|p| p.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                });
        }
        if let Some(default) = self.default_profile.as_deref() {
            if let Some(profile) = self.profiles.iter().find(|p| p.name == default) {
                return Ok(profile);
            }
        }
        self.profiles
            .first()
            .ok_or_else(|| ConfigError::Invalid("the pack defines no profiles".to_string()))
    }
}

/// A pack, turned into the control layer's values.
///
/// `source` is what the provenance rows will show — the issuer and version — so
/// `config show` answers "and who said so?" with the pack, not with a file path.
pub fn pack_layer(profile: &NamedProfile) -> Vec<(String, Value)> {
    let mut values = Vec::new();
    for model in &profile.models {
        values.push((
            format!("models.{}", model.name),
            Value::Model(model.clone()),
        ));
    }
    if let Some(default) = &profile.router.default {
        values.push(("router.default".to_string(), Value::Text(default.clone())));
    }
    if !profile.router.rules.is_empty() {
        values.push((
            "router.rules".to_string(),
            Value::Rules(profile.router.rules.clone()),
        ));
    }
    // Not `control.profile`: that key is the *user's* choice, and the pack must not
    // decide it for them. The resolved name is recorded as provenance instead.
    values
}

/// The pack a session is running on, for provenance and for the TUI header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlProvenance {
    pub issuer: String,
    pub version: String,
    pub key_id: String,
    pub profile: String,
}

/// The effective configuration, after merging every layer.
#[derive(Debug, Clone, Default)]
pub struct Profile {
    /// Which pack this configuration came from, when it came from one.
    pub control: Option<ControlProvenance>,
    /// `local` when this install runs without a control plane; `None` means managed.
    ///
    /// This lives in configuration rather than in a flag because it has to *survive*:
    /// `loongagent resume` re-enters the interface with no memory of how the session
    /// began, and a session started with `--local` that silently rejoined a control
    /// plane on resume would send prompts somewhere its owner never chose. A key in
    /// the user's own profile is the durable, inspectable place for that decision —
    /// and it is why `--local` and `mode = "local"` mean the same thing.
    pub mode: Option<String>,
    /// The named profile this install runs (the user's one profile-related choice).
    pub control_profile: Option<String>,
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
    /// How many tokens this model can hold in one request. The shorthand form
    /// (`model` + `base_url`) passes it on to the one model it builds.
    pub max_context_size: Option<u64>,
    /// Tokens held back for the model's answer: compaction runs when the room left in the
    /// window falls below it. `0` or unset means the kernel's own default.
    pub reserved_context_size: Option<u64>,
    /// The layered memory (ADR-0017). `None` means nobody has said, which reads as
    /// **off**: the mechanism is opt-in, and the old compaction path is the default.
    /// A firm setting it to `false` wins over a user setting it to `true`
    /// (narrow-only), which is what makes "off" enforceable rather than advisory.
    pub context_engine_enabled: Option<bool>,
    /// `record` (deterministic summaries only) or `semantic` (adds a local model).
    pub context_engine_mode: Option<String>,
    pub context_engine_episode_events: Option<usize>,
    pub context_engine_l4_tokens: Option<u64>,
    /// Whether a local summariser may run at all — narrow-only, like the engine
    /// itself. Separate from `context_engine_enabled` because `record` mode is
    /// useful with no model, and a firm may allow one and not the other.
    pub summarizer_enabled: Option<bool>,
    pub summarizer_kind: Option<String>,
    pub summarizer_base_url: Option<String>,
    pub summarizer_model: Option<String>,
    /// The background enrichment window (ADR-0017 §4.6): at most this many model
    /// calls / this many tokens per idle window. Narrow-only and **minimum wins**,
    /// because a smaller window is the stricter one — a firm can cap what the
    /// background may spend and no lower layer can raise it.
    pub summarizer_window_calls: Option<usize>,
    pub summarizer_window_tokens: Option<u64>,
    /// Resolved per action class. A `Vec` rather than a map because the order of
    /// action classes is not meaningful — inventing one to satisfy a `BTreeMap`
    /// would be a lie about the domain.
    pub permissions: Vec<(ActionClass, Verdict)>,
    pub skill_dirs: Vec<PathBuf>,
    pub disable_skills: Vec<String>,
    provenance: BTreeMap<String, Provenance>,
}

/// The resolved layered-memory settings, ready for a caller to build a kernel
/// config from. Plain values on purpose: `loong-config` decides *policy*, and the
/// kernel decides how to run — a settings struct is the seam between them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextEngineSettings {
    /// `record` or `semantic`.
    pub mode: String,
    pub episode_events: Option<usize>,
    pub l4_max_tokens: Option<u64>,
    /// `ollama` or `off`.
    pub summarizer: String,
    pub summarizer_base_url: Option<String>,
    pub summarizer_model: Option<String>,
    /// One idle window's budget, `None` meaning "use the shipped default".
    pub summarizer_window_calls: Option<usize>,
    pub summarizer_window_tokens: Option<u64>,
}

impl Profile {
    /// The layered memory's settings, or `None` when it is off or nobody has said.
    ///
    /// `None` is the meaningful default: ADR-0017 makes the mechanism opt-in, and
    /// "not mentioned anywhere" must not read as "on" — a session that started
    /// summarising because a key was absent would be exactly the hidden behaviour
    /// the ledger exists to prevent.
    ///
    /// The summariser is only carried through when **both** switches allow it: the
    /// engine on, mode `semantic`, and `summarizer.enabled` not `false`. A model
    /// that cannot be reached is a runtime question; a model that is not permitted
    /// is a policy one, and this function only answers the second.
    pub fn context_engine(&self) -> Option<ContextEngineSettings> {
        if self.context_engine_enabled != Some(true) {
            return None;
        }
        let mode = self
            .context_engine_mode
            .clone()
            .unwrap_or_else(|| "record".to_string());
        let summarizer_allowed = self.summarizer_enabled != Some(false);
        let summarizer = if mode == "semantic" && summarizer_allowed {
            self.summarizer_kind
                .clone()
                .unwrap_or_else(|| "ollama".to_string())
        } else {
            "off".to_string()
        };
        Some(ContextEngineSettings {
            mode,
            episode_events: self.context_engine_episode_events,
            l4_max_tokens: self.context_engine_l4_tokens,
            summarizer,
            summarizer_base_url: self.summarizer_base_url.clone(),
            summarizer_model: self.summarizer_model.clone(),
            summarizer_window_calls: self.summarizer_window_calls,
            summarizer_window_tokens: self.summarizer_window_tokens,
        })
    }

    /// Whether this install runs without a control plane.
    ///
    /// One predicate, in one place, because "are we local?" decides whether a whole
    /// class of network traffic is allowed to happen — and a second, subtly different
    /// spelling of that question is how a `--local` session quietly reaches the
    /// control plane anyway.
    pub fn is_local(&self) -> bool {
        self.mode.as_deref() == Some("local")
    }

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
    /// `local` or `managed`. See [`Profile::mode`].
    mode: Option<String>,
    model: Option<String>,
    base_url: Option<String>,
    api_key: Option<String>,
    budget_tokens: Option<u64>,
    keep_newest_tokens: Option<u64>,
    max_steps: Option<u64>,
    compact_at_percent: Option<u64>,
    max_context_size: Option<u64>,
    reserved_context_size: Option<u64>,
    #[serde(default)]
    skill_dirs: Vec<String>,
    #[serde(default)]
    disable_skills: Vec<String>,
    #[serde(default)]
    permissions: BTreeMap<String, String>,
    #[serde(default)]
    models: Vec<RawModel>,
    router: Option<RawRouter>,
    control: Option<RawControl>,
    context_engine: Option<RawContextEngine>,
    summarizer: Option<RawSummarizer>,
}

/// `[context_engine]` (ADR-0017). Every field is optional: a layer says only what
/// it wants to change.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawContextEngine {
    /// A **narrow-only switch**, stored as 0/1: the organisation may force the
    /// layered memory off and a lower layer may not turn it back on. Turning it on
    /// costs a background model; whether that is allowed is a policy question, and
    /// policy belongs to the higher layer.
    enabled: Option<bool>,
    /// `record` (deterministic only) or `semantic` (adds a local model).
    mode: Option<String>,
    episode_events: Option<u64>,
    l4_max_tokens: Option<u64>,
}

/// `[summarizer]`: which local model, if any.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSummarizer {
    /// Narrow-only, like `context_engine.enabled`: a user may not switch on a
    /// facility the firm switched off.
    enabled: Option<bool>,
    /// `ollama` or `off`.
    kind: Option<String>,
    /// The endpoint and the model. Both are **endpoint keys**: a repository must
    /// not choose where prompts go (ADR-0008), so the project layer is refused.
    base_url: Option<String>,
    model: Option<String>,
    /// One idle window's budget (ADR-0017 §4.6). Narrow-only: a higher layer may
    /// cap the background's model calls or tokens; a lower one may not raise it.
    window_calls: Option<u64>,
    window_tokens: Option<u64>,
}

/// The user's side of the control plane: which named profile to run. The address and
/// the pinned key are machine configuration, not layered config — they are what
/// `loongagent setup` writes.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawControl {
    profile: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawModel {
    name: String,
    kind: Option<String>,
    /// `provider = "deepseek/deepseek-flash"` — the one-line form.
    ///
    /// When present it supplies `base_url`, `model` and an `api_key` reference, so a
    /// local profile can name a model without naming an endpoint. An explicit
    /// `base_url` or `model` still wins: the shorthand is a default, not a cage.
    provider: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    api_key: Option<String>,
    budget_tokens: Option<u64>,
    /// See [`ModelSpec::max_context_size`]. A pack may declare it, and so may a profile.
    max_context_size: Option<u64>,
}

/// Resolve a `[[models]]` entry to its concrete endpoint, model id and key reference.
///
/// The order is deliberate: what a person *wrote* beats what the catalog *assumes*,
/// and anything the shorthand was supposed to supply but nobody wrote is an error
/// that names both ways of saying it — a model silently pointed at the wrong
/// endpoint is the worst outcome available here.
fn finish_model(
    where_: &str,
    name: &str,
    provider: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    api_key: Option<String>,
) -> Result<(String, String, Option<String>, Option<String>), String> {
    let expansion = match provider.as_deref() {
        Some(spec) => Some(catalog::expand(spec).map_err(|e| format!("{where_}: {e}"))?),
        None => None,
    };
    let base_url = match (base_url, &expansion) {
        (Some(written), _) => written,
        (None, Some(expanded)) => expanded.base_url.clone(),
        (None, None) => {
            return Err(format!(
                "{where_}: model {name:?} needs a `base_url`, or a `provider` to supply one \
                 (e.g. provider = \"deepseek/deepseek-flash\")"
            ))
        }
    };
    let model = match (model, &expansion) {
        (Some(written), _) => written,
        (None, Some(expanded)) => expanded.model.clone(),
        (None, None) => {
            return Err(format!(
                "{where_}: model {name:?} needs a `model` id, or a `provider` to supply one \
                 (e.g. provider = \"deepseek/deepseek-flash\")"
            ))
        }
    };
    // A key the user wrote always wins; otherwise the shorthand's reference stands.
    let api_key = api_key.or_else(|| expansion.as_ref().and_then(|e| e.api_key.clone()));
    let provider = expansion.map(|e| e.provider.id.to_string()).or(provider);
    Ok((base_url, model, api_key, provider))
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
        if let Some(mode) = raw.mode.clone() {
            // Same threat as `model`/`base_url`: a repository that could set
            // `mode = "local"` would switch a managed install out of its firm's policy
            // with a file its owner never read. The project layer may not say it.
            if layer == Layer::Project {
                return Err(ConfigError::Invalid(format!(
                    "{source}: the project layer may not set `mode` — a repository must not \
                     switch an install out of managed mode (ADR-0008)"
                )));
            }
            // A mode nobody implements would silently read as "managed", which is the
            // one direction this key must never fail in.
            if mode != "local" && mode != "managed" {
                return Err(ConfigError::Invalid(format!(
                    "{source}: `mode` is {mode:?}; the modes that exist are \"local\" (no \
                     control plane) and \"managed\" (the default)"
                )));
            }
            values.push(("mode".to_string(), Value::Text(mode)));
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
            let (base_url, model_id, api_key, provider) = finish_model(
                &source,
                &model.name,
                model.provider.clone(),
                model.base_url.clone(),
                model.model.clone(),
                model.api_key.clone(),
            )
            .map_err(ConfigError::Invalid)?;
            check_endpoint_reference(&source, "base_url", &base_url)?;
            if let Some(key) = &api_key {
                check_secret_reference(&source, &format!("models.{}.api_key", model.name), key)?;
            }
            values.push((
                format!("models.{}", model.name),
                Value::Model(ModelSpec {
                    name: model.name.clone(),
                    kind,
                    base_url,
                    model: model_id,
                    api_key,
                    budget_tokens: model.budget_tokens,
                    max_context_size: model.max_context_size,
                    provider,
                }),
            ));
        }
        if let Some(control) = &raw.control {
            if let Some(name) = &control.profile {
                values.push(("control.profile".to_string(), Value::Text(name.clone())));
            }
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
            ("max_context_size", raw.max_context_size),
            ("reserved_context_size", raw.reserved_context_size),
        ] {
            if let Some(value) = value {
                values.push((key.to_string(), Value::Number(value)));
            }
        }
        // The context engine (ADR-0017). The two `enabled` flags travel as 0/1
        // narrow-only numbers rather than booleans, so they ride the existing
        // strictest-wins rule: `0` (off) is stricter than `1`, which is exactly the
        // direction policy needs — a firm can switch the layered memory off and no
        // layer below can switch it back on.
        if let Some(engine) = &raw.context_engine {
            if let Some(enabled) = engine.enabled {
                values.push((
                    "context_engine.enabled".to_string(),
                    Value::Number(u64::from(enabled)),
                ));
            }
            if let Some(mode) = &engine.mode {
                if mode != "record" && mode != "semantic" {
                    return Err(ConfigError::Invalid(format!(
                        "{source}: context_engine.mode is {mode:?}; the modes that exist are \
                         \"record\" (deterministic summaries only) and \"semantic\" (adds a \
                         local model)"
                    )));
                }
                values.push(("context_engine.mode".to_string(), Value::Text(mode.clone())));
            }
            for (key, value) in [
                ("context_engine.episode_events", engine.episode_events),
                ("context_engine.l4_max_tokens", engine.l4_max_tokens),
            ] {
                if let Some(value) = value {
                    values.push((key.to_string(), Value::Number(value)));
                }
            }
        }
        if let Some(summarizer) = &raw.summarizer {
            if let Some(enabled) = summarizer.enabled {
                values.push((
                    "summarizer.enabled".to_string(),
                    Value::Number(u64::from(enabled)),
                ));
            }
            if let Some(kind) = &summarizer.kind {
                if kind != "ollama" && kind != "off" {
                    return Err(ConfigError::Invalid(format!(
                        "{source}: summarizer.kind is {kind:?}; the kinds that exist are \
                         \"ollama\" and \"off\""
                    )));
                }
                values.push(("summarizer.kind".to_string(), Value::Text(kind.clone())));
            }
            // Endpoint keys: the project layer may not say them. A repository that
            // could point the summariser somewhere would be choosing where a
            // session's history is sent — the exfiltration vector ADR-0008 names,
            // and worse here than for the model, because a summary is the whole
            // session in condensed form.
            for (key, value) in [
                ("summarizer.base_url", summarizer.base_url.clone()),
                ("summarizer.model", summarizer.model.clone()),
            ] {
                let Some(value) = value else { continue };
                if layer == Layer::Project {
                    return Err(ConfigError::Invalid(format!(
                        "{source}: the project layer may not set `{key}` — a repository must \
                         not choose where a session's memory is summarised (ADR-0008)"
                    )));
                }
                check_endpoint_reference(&source, key, &value)?;
                values.push((key.to_string(), Value::Text(value)));
            }
            // Budgets, not endpoints: these say how much background work may happen,
            // which is a policy the narrow-only rule governs. Minimum wins.
            if let Some(calls) = summarizer.window_calls {
                values.push(("summarizer.window_calls".to_string(), Value::Number(calls)));
            }
            if let Some(tokens) = summarizer.window_tokens {
                values.push((
                    "summarizer.window_tokens".to_string(),
                    Value::Number(tokens),
                ));
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
    let reference = || {
        format!(
            "{source}: `{key}` must be a reference — `env:NAME`, `file:PATH#KEY`, `keychain:…`, \
             `gateway:…` — because secrets never live in configuration (ADR-0008 §5)"
        )
    };
    // A prefix is not enough. Writing the secret *after* the prefix
    // (`env:sk-4f06…`) satisfies "starts with env:" and puts the key in the pack, the
    // client cache, and — since a draft is meant to be committed — in git. So the name
    // itself is checked: an environment variable name, conventionally upper case.
    if let Some(name) = value.strip_prefix("env:") {
        return if is_secret_name(name) {
            Ok(())
        } else {
            Err(ConfigError::Invalid(format!(
                "{} — the part after `env:` must be the *name* of a variable (e.g. \
                 `env:DEEPSEEK_API_KEY`), not the secret itself: {name:?} is not a variable name",
                reference()
            )))
        };
    }
    if let Some(rest) = value.strip_prefix("file:") {
        // `file:PATH` (the file is the secret) or `file:PATH#NAME`.
        return match rest.split_once('#') {
            Some((path, name)) if !path.is_empty() && is_secret_name(name) => Ok(()),
            Some((_, name)) => Err(ConfigError::Invalid(format!(
                "{} — the part after `#` names a key inside the file (e.g. \
                 `file:credentials.toml#DEEPSEEK_API_KEY`), not the secret: {name:?}",
                reference()
            ))),
            None if !rest.is_empty() => Ok(()),
            None => Err(ConfigError::Invalid(reference())),
        };
    }
    for prefix in ["keychain:", "gateway:"] {
        if let Some(rest) = value.strip_prefix(prefix) {
            return if !rest.is_empty()
                && rest
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_./:-".contains(c))
            {
                Ok(())
            } else {
                Err(ConfigError::Invalid(reference()))
            };
        }
    }
    Err(ConfigError::Invalid(reference()))
}

/// An environment variable name, and nothing that looks like a value: letters, digits
/// and underscores, starting with a letter, conventionally upper case.
///
/// Deliberately strict. `sk-4f06…` fails on the hyphen and the case; a pasted
/// alphanumeric secret fails because a long unbroken run of characters with no
/// underscore is not how anybody names a variable.
fn is_secret_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 {
        return false;
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_uppercase() && first != '_' {
        return false;
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    {
        return false;
    }
    // `DEEPSEEK_API_KEY` has words separated by underscores; a secret does not.
    !name.split('_').any(|word| word.len() >= 20)
}

/// A secret in a **pack**: a reference must be well formed, and a bare literal is
/// accepted — visibly. The pack is the distribution channel, and until identity and the
/// model gateway exist (ADR-0012 §4/§5) a firm may choose to ship the credential with
/// it. The warning is the price of that choice, repeated at publish, at load, and in
/// `GET /v1/health`.
///
/// A *malformed* reference is still refused: `env:sk-4f06…` is not a name, and
/// accepting it would silently look up a variable called `sk-4f06…`.
fn check_pack_secret(
    key: &str,
    value: &str,
    warnings: &mut Vec<String>,
) -> Result<(), ConfigError> {
    let known = ["env:", "file:", "keychain:", "gateway:"];
    if known.iter().any(|prefix| value.starts_with(prefix)) {
        return check_secret_reference("the pack", key, value);
    }
    warnings.push(format!(
        "`{key}` carries the secret itself: it travels inside the signed pack, sits in \
         plaintext in every install's cache, and is copied by any backup of either. \
         References (`env:NAME`, `file:PATH#KEY`) are what ADR-0008 §5 asks for; this is \
         the recorded, temporary exception."
    ));
    Ok(())
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
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

/// The user's home directory, spelled the way each platform spells it.
///
/// Unix sets `HOME`. Windows sets `USERPROFILE` — `HOME` is only present when a
/// POSIX-ish shell (Git Bash, MSYS) exported it, and a process launched from a
/// shortcut or a service has none. Reading only `HOME` fails *quietly*: `~/…` stays
/// a literal directory name and the configuration root falls through to
/// [`std::env::temp_dir`], so a Windows user's settings and sessions would live in
/// `%TEMP%` and vanish. Both names are consulted; `HOME` wins when it is set, so a
/// Git Bash session keeps pointing where it always did.
pub fn home_dir() -> Option<PathBuf> {
    non_empty("HOME")
        .or_else(|| non_empty("USERPROFILE"))
        .map(PathBuf::from)
}

/// Where a **machine-wide** configuration file lives.
///
/// `/etc/loongagent` on Unix is an administrator's directory. Its Windows analogue
/// is `%PROGRAMDATA%\loongagent`; without this the org layer would resolve to the
/// drive-relative `\etc\loongagent`, which is not a place anything writes.
pub fn system_config_dir() -> PathBuf {
    #[cfg(windows)]
    {
        non_empty("PROGRAMDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:\\ProgramData"))
            .join("loongagent")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("/etc/loongagent")
    }
}

/// Where each layer's file lives on this machine.
///
/// The org layer is a file an administrator owns. The signed pack envelope
/// exists and is verified against a pinned key *before* it can occupy this layer
/// (ADR-0016); what this path is, is the **local** org file, a plain
/// `profile.toml` and the thing that must not be called tamper-resistant.
pub fn layer_paths(workspace: &Path, session: Option<&Path>) -> Vec<(Layer, PathBuf)> {
    let mut layers = Vec::new();
    layers.push((Layer::Org, system_config_dir().join("profile.toml")));
    layers.push((Layer::Project, workspace.join(".loongagent/profile.toml")));
    // One root for everything the local client keeps, so "where does loongagent put
    // things?" has a one-word answer. `LOONGAGENT_HOME` moves it for CI and scripts.
    layers.push((Layer::User, loongagent_home().join("profile.toml")));
    if let Some(session) = session {
        layers.push((Layer::Session, session.to_path_buf()));
    }
    layers
}

/// Where a **local** install keeps its own configuration.
///
/// A separate file, not a section of [`layer_paths`]' user file. Local mode is a
/// different way of deciding where prompts go, and the two must not be able to reach
/// each other: a local run that read the managed profile would inherit models nobody
/// chose, and — far worse — a managed run that read a local file could be switched out
/// of its firm's policy by a file that has nothing to do with managed mode.
pub fn local_profile_path() -> PathBuf {
    loongagent_home().join("local.toml")
}

/// The layers a **local** run resolves.
///
/// Deliberately not [`layer_paths`]: no org file, and the user's own `local.toml`
/// instead of the managed `profile.toml`. The workspace layer stays, because a
/// repository may still *narrow* what is allowed (ADR-0008 §2) — it cannot name a
/// model, an endpoint or a key, so it cannot put anything into this list that a person
/// did not choose.
pub fn local_layer_paths(workspace: &Path, session: Option<&Path>) -> Vec<(Layer, PathBuf)> {
    let mut layers = Vec::new();
    layers.push((Layer::Project, workspace.join(".loongagent/profile.toml")));
    layers.push((Layer::User, local_profile_path()));
    if let Some(session) = session {
        layers.push((Layer::Session, session.to_path_buf()));
    }
    layers
}

/// Where the local client keeps its files: `$LOONGAGENT_HOME`, else `~/.loongagent`.
///
/// Files directly in it are **configuration** (`profile.toml`, `control.toml`, the
/// `env` file holding secrets, a copy of the client token); its subdirectories are
/// **state** (`projects/`, `control/`). The control *plane*, being a service, keeps
/// its own files where a service's belong: `/etc/loongagent-control` for what an
/// administrator authors, `/var/lib/loongagent-control` for what it writes.
pub fn loongagent_home() -> PathBuf {
    non_empty("LOONGAGENT_HOME")
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join(".loongagent")))
        .unwrap_or_else(std::env::temp_dir)
}

fn non_empty(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

/// Merge the layers that exist, lowest precedence first.
pub fn resolve(layers: &[(Layer, PathBuf)], workspace: &Path) -> Result<Profile, ConfigError> {
    resolve_with(layers, workspace, None)
}

/// As [`resolve`], plus an optional pack from the control plane.
///
/// The pack arrives as values rather than as a path because it was *verified* before
/// it got here: nothing that reaches this function may be an unverified file a user
/// could edit (ADR-0008 §6).
pub fn resolve_with(
    layers: &[(Layer, PathBuf)],
    workspace: &Path,
    pack: Option<(String, Vec<(String, Value)>)>,
) -> Result<Profile, ConfigError> {
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
    if let Some((source, values)) = pack {
        sources.insert(Layer::Control, source.clone());
        present.push(LayerValues {
            layer: Layer::Control,
            source,
            values,
        });
    }
    present.sort_by_key(|l| l.layer);
    let mut profile = merge(&present)?;
    finish(&mut profile)?;
    // `mode` decides whether the control plane is consulted at all, so where it is
    // written matters more than what it says. It belongs in `local.toml` — the file a
    // local install owns — or in a session layer somebody named on the command line.
    // Honoured from the *managed* user file it becomes a way to switch a whole install
    // out of its firm's policy by editing a file that has nothing to do with local
    // mode, which is exactly what happened the first time this key existed.
    if let Some(row) = profile.explain("mode") {
        if !mode_is_legitimate(row.layer, &row.source) {
            return Err(ConfigError::Invalid(format!(
                "{}: `mode` belongs in {} — the managed profile must not be able to \
                 switch an install into local mode (run `loongagent tui --local` \
                 instead, which reads that file and nothing else)",
                row.source,
                local_profile_path().display()
            )));
        }
    }
    Ok(profile)
}

/// Whether a `mode` value from this layer and file is allowed to mean anything.
///
/// Split out so the rule can be stated — and tested — without a test having to write to
/// the real user's home directory.
fn mode_is_legitimate(layer: Layer, source: &str) -> bool {
    // A session layer is a file somebody named on the command line, which is as
    // deliberate as it gets. Everything else is a file that was read because of where
    // it sits, and `mode` is too consequential to be decided by geography.
    layer == Layer::Session || Path::new(source) == local_profile_path()
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
                max_context_size: profile.max_context_size,
                provider: None,
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

fn merge(layers: &[LayerValues]) -> Result<Profile, ConfigError> {
    let mut profile = Profile::default();
    // Key → the winning row so far, plus what lost.
    let mut winners: BTreeMap<String, Provenance> = BTreeMap::new();

    for layer in layers {
        for (key, value) in &layer.values {
            let base = key.split('.').next().unwrap_or(key).to_string();
            // Locked: once the control plane has set a key, a lower layer may narrow
            // it but never rewrite it. This is what makes "engineers pick a profile,
            // not a vendor" a property rather than advice (T-07, ADR-0016 §5).
            if let Some(current) = winners.get(key) {
                if current.layer == Layer::Control && layer.layer > Layer::Control {
                    let kind = kind_of(key).or_else(|| kind_of(&base));
                    if !matches!(kind, Some(Kind::NarrowOnly) | Some(Kind::AddOnly)) {
                        return Err(ConfigError::Invalid(format!(
                            "{}: `{key}` is set by the control plane ({}) and cannot be changed \
                             by the {} layer — pick a different profile instead",
                            layer.source,
                            current.source,
                            layer.layer.as_str()
                        )));
                    }
                }
            }
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
                            let (keep_new, lose_old) = stricter_for(key, &current.value, value);
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
                                // The window a model holds is a fact about the model, not a
                                // choice: a local redefinition may declare a *smaller* one
                                // (which compacts earlier and is therefore safe) and must
                                // not be able to lose or widen the one an organisation
                                // declared — losing it would turn compaction off.
                                merged.max_context_size =
                                    match (old.max_context_size, new.max_context_size) {
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
            ("mode", Value::Text(v)) => profile.mode = Some(v.clone()),
            ("base_url", Value::Text(v)) => profile.base_url = Some(v.clone()),
            ("api_key", Value::Text(v)) => profile.api_key = Some(v.clone()),
            ("budget_tokens", Value::Number(n)) => profile.budget_tokens = Some(*n),
            ("keep_newest_tokens", Value::Number(n)) => profile.keep_newest_tokens = Some(*n),
            ("max_steps", Value::Number(n)) => profile.max_steps = Some(*n as usize),
            ("compact_at_percent", Value::Number(n)) => profile.compact_at_percent = Some(*n),
            ("max_context_size", Value::Number(n)) => profile.max_context_size = Some(*n),
            ("reserved_context_size", Value::Number(n)) => profile.reserved_context_size = Some(*n),
            // The context engine (ADR-0017). Kept as the raw resolved facts; the
            // typed view a caller wants is `Profile::context_engine()`.
            ("context_engine.enabled", Value::Number(n)) => {
                profile.context_engine_enabled = Some(*n != 0)
            }
            ("context_engine.mode", Value::Text(v)) => {
                profile.context_engine_mode = Some(v.clone())
            }
            ("context_engine.episode_events", Value::Number(n)) => {
                profile.context_engine_episode_events = Some(*n as usize)
            }
            ("context_engine.l4_max_tokens", Value::Number(n)) => {
                profile.context_engine_l4_tokens = Some(*n)
            }
            ("summarizer.enabled", Value::Number(n)) => profile.summarizer_enabled = Some(*n != 0),
            ("summarizer.kind", Value::Text(v)) => profile.summarizer_kind = Some(v.clone()),
            ("summarizer.base_url", Value::Text(v)) => {
                profile.summarizer_base_url = Some(v.clone())
            }
            ("summarizer.model", Value::Text(v)) => profile.summarizer_model = Some(v.clone()),
            ("summarizer.window_calls", Value::Number(n)) => {
                profile.summarizer_window_calls = Some(*n as usize)
            }
            ("summarizer.window_tokens", Value::Number(n)) => {
                profile.summarizer_window_tokens = Some(*n)
            }
            // Models are collected after this loop, in declaration order — see below.
            (key, Value::Model(_)) if key.starts_with("models.") => {}
            ("router.default", Value::Text(name)) => profile.router.default = Some(name.clone()),
            ("router.rules", Value::Rules(rules)) => profile.router.rules = rules.clone(),
            ("control.profile", Value::Text(name)) => profile.control_profile = Some(name.clone()),
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
    // **Declaration order**, not alphabetical.
    //
    // The loop above iterates a map keyed by `models.<name>`, so collecting models there
    // handed them back sorted by name — which throws away the one thing the order means.
    // A profile's `[[models]]` is written cheapest-first, most capable-last; that order
    // *is* the tier ladder, it is what the router's default and the status bar's digits
    // refer to, and sorting it would silently renumber every hotkey somebody had learned.
    //
    // The most local layer that declares models sets the order — that is the file the
    // person actually wrote — and a model that only a lower layer declares is appended
    // after it, so nothing becomes unreachable by being out of order.
    let mut order: Vec<String> = Vec::new();
    for layer in layers.iter().rev() {
        for (key, value) in &layer.values {
            if let Some(name) = key.strip_prefix("models.") {
                if matches!(value, Value::Model(_)) && !order.iter().any(|seen| seen == name) {
                    order.push(name.to_string());
                }
            }
        }
    }
    for name in &order {
        if let Some(row) = winners.get(&format!("models.{name}")) {
            if let Value::Model(spec) = &row.value {
                profile.models.push(spec.clone());
            }
        }
    }

    profile.provenance = winners;
    Ok(profile)
}
/// Which of two narrow-only values is stricter. Returns
/// `(new_is_stricter, old_should_be_recorded_as_overridden)`.
/// Which of two narrow-only values is stricter.
///
/// Most caps are strictest when *smallest* — a smaller budget, fewer steps. A **reserve**
/// is the other way round: keeping more room for the answer is the conservative choice, so
/// a lower layer may raise it but never shrink it. Getting that backwards would let a
/// project file quietly leave the model less room to answer than the organisation allowed.
fn stricter_for(key: &str, current: &Value, candidate: &Value) -> (bool, bool) {
    if key == "reserved_context_size" {
        if let (Value::Number(a), Value::Number(b)) = (current, candidate) {
            return (b > a, b < a);
        }
    }
    stricter(current, candidate)
}

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
    fn the_layered_memory_is_off_unless_someone_says_so() {
        let ws = workspace();
        // Absent everywhere: off. A mechanism that switched itself on because a key
        // was missing would be the hidden behaviour ADR-0006 exists to prevent.
        let profile = resolve(&[], &ws).unwrap();
        assert_eq!(profile.context_engine(), None);

        // A user may switch it on.
        let user = write(
            &ws,
            "u.toml",
            "[context_engine]\nenabled = true\nmode = \"semantic\"\n",
        );
        let profile = resolve(&[(Layer::User, user)], &ws).unwrap();
        let settings = profile.context_engine().expect("on");
        assert_eq!(settings.mode, "semantic");
        // With no summariser named, `semantic` means the default local one.
        assert_eq!(settings.summarizer, "ollama");
    }

    #[test]
    fn a_firm_can_force_the_layered_memory_off_and_a_user_cannot_loosen_it() {
        // The whole reason the switch is narrow-only: this is a policy question
        // (background work spends a model) and policy belongs to the higher layer.
        let ws = workspace();
        let org = write(&ws, "org.toml", "[context_engine]\nenabled = false\n");
        let user = write(&ws, "u.toml", "[context_engine]\nenabled = true\n");
        let profile = resolve(&[(Layer::User, user), (Layer::Org, org)], &ws).unwrap();
        assert_eq!(
            profile.context_engine(),
            None,
            "the org's off must survive a user's on"
        );
        assert_eq!(profile.context_engine_enabled, Some(false));

        // The reverse order of intent: a user turning it off is also honoured.
        let ws2 = workspace();
        let user_off = write(&ws2, "u.toml", "[context_engine]\nenabled = false\n");
        let profile = resolve(&[(Layer::User, user_off)], &ws2).unwrap();
        assert_eq!(profile.context_engine(), None);
    }

    #[test]
    fn record_mode_is_useful_without_a_model_and_semantic_needs_one() {
        let ws = workspace();
        let record = write(
            &ws,
            "r.toml",
            "[context_engine]\nenabled = true\nmode = \"record\"\n",
        );
        let profile = resolve(&[(Layer::User, record)], &ws).unwrap();
        assert_eq!(profile.context_engine().unwrap().summarizer, "off");

        // `semantic` with the summariser switched off by policy is not `semantic`:
        // the settings say what may run, not what was asked for.
        let ws2 = workspace();
        let semantic_no_model = write(
            &ws2,
            "s.toml",
            "[context_engine]\nenabled = true\nmode = \"semantic\"\n             [summarizer]\nenabled = false\n",
        );
        let profile = resolve(&[(Layer::User, semantic_no_model)], &ws2).unwrap();
        let settings = profile.context_engine().unwrap();
        assert_eq!(settings.mode, "semantic");
        assert_eq!(
            settings.summarizer, "off",
            "policy forbids the model even though the mode asks for it"
        );
    }

    #[test]
    fn the_enrichment_window_is_narrow_only_and_smaller_wins() {
        // The window is the background's spend per idle turn of the crank. A firm
        // may cap it; a lower layer must not be able to raise it (ADR-0008 §2).
        let ws = workspace();
        let org = write(
            &ws,
            "org.toml",
            "[context_engine]\nenabled = true\nmode = \"semantic\"\n\n[summarizer]\nwindow_calls = 2\nwindow_tokens = 1500\n",
        );
        let user = write(
            &ws,
            "u.toml",
            "[summarizer]\nwindow_calls = 8\nwindow_tokens = 9000\n",
        );
        let profile = resolve(&[(Layer::User, user), (Layer::Org, org)], &ws).unwrap();
        let settings = profile.context_engine().unwrap();
        assert_eq!(
            settings.summarizer_window_calls,
            Some(2),
            "the org's smaller call budget survives the user's larger one"
        );
        assert_eq!(
            settings.summarizer_window_tokens,
            Some(1500),
            "and so does the token budget"
        );

        // A single layer simply sets it.
        let ws2 = workspace();
        let only = write(
            &ws2,
            "u.toml",
            "[context_engine]\nenabled = true\n\n[summarizer]\nwindow_calls = 6\n",
        );
        let profile = resolve(&[(Layer::User, only)], &ws2).unwrap();
        assert_eq!(
            profile.context_engine().unwrap().summarizer_window_calls,
            Some(6)
        );
    }

    #[test]
    fn a_repository_may_not_choose_where_memory_is_summarised() {
        // The same threat as `base_url`: a repository that could point the
        // summariser somewhere would be choosing where a session's history goes —
        // and a summary is the whole session in condensed form.
        let ws = workspace();
        let project = write(
            &ws,
            "p.toml",
            "[context_engine]\nenabled = true\n\n[summarizer]\nbase_url = \"http://evil/v1\"\n",
        );
        match resolve(&[(Layer::Project, project)], &ws) {
            Err(ConfigError::Invalid(message)) => {
                assert!(
                    message.contains("summarizer.base_url"),
                    "the refusal must name the key: {message}"
                );
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_mode_or_summariser_kind_is_refused_by_name() {
        let ws = workspace();
        let bad_mode = write(&ws, "b.toml", "[context_engine]\nmode = \"clever\"\n");
        match resolve(&[(Layer::User, bad_mode)], &ws) {
            Err(ConfigError::Invalid(m)) => assert!(m.contains("record"), "{m}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        let ws2 = workspace();
        let bad_kind = write(&ws2, "b.toml", "[summarizer]\nkind = \"gpt\"\n");
        match resolve(&[(Layer::User, bad_kind)], &ws2) {
            Err(ConfigError::Invalid(m)) => assert!(m.contains("ollama"), "{m}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
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
    fn a_reserve_is_stricter_when_it_is_bigger() {
        // Every other narrow-only number is strictest when smallest. A reserve for the
        // model's answer is the other way round: keeping more room is the conservative
        // choice. So a later layer may *raise* it and may not shrink it — backwards, a
        // project file could quietly leave the model less room to answer than the
        // organisation allowed, which is the failure mode this table exists to prevent.
        let ws = workspace();
        let org = write(&ws, "org.toml", "reserved_context_size = 50000\n");
        let user = write(&ws, "user.toml", "reserved_context_size = 5000\n");
        let profile = resolve(&[(Layer::Org, org), (Layer::User, user)], &ws).unwrap();
        assert_eq!(
            profile.reserved_context_size,
            Some(50_000),
            "shrinking the reserve is not narrowing"
        );
        assert_eq!(
            profile.explain("reserved_context_size").unwrap().layer,
            Layer::Org
        );

        let ws = workspace();
        let org = write(&ws, "org.toml", "reserved_context_size = 5000\n");
        let user = write(&ws, "user.toml", "reserved_context_size = 50000\n");
        let profile = resolve(&[(Layer::Org, org), (Layer::User, user)], &ws).unwrap();
        assert_eq!(
            profile.reserved_context_size,
            Some(50_000),
            "raising it is narrowing, and it wins"
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
    fn a_provider_shorthand_supplies_the_endpoint_the_model_and_the_key_reference() {
        let ws = workspace();
        let user = write(
            &ws,
            "u.toml",
            "[[models]]\n\
             name = \"cheap\"\n\
             provider = \"deepseek/deepseek-flash\"\n\n\
             [[models]]\n\
             name = \"strong\"\n\
             provider = \"moonshot/kimi-k3\"\n\
             base_url = \"https://example.invalid/v1\"\n",
        );
        let profile = resolve(&[(Layer::User, user)], &ws).unwrap();

        let cheap = &profile.models[0];
        assert_eq!(cheap.base_url, "https://api.deepseek.com");
        assert_eq!(cheap.model, "deepseek-flash");
        assert_eq!(
            cheap.api_key.as_deref(),
            Some("env:LOONG_DEEPSEEK_KEY"),
            "the shorthand yields a reference, so ADR-0008 §5 still holds"
        );
        assert_eq!(cheap.provider.as_deref(), Some("deepseek"));
        assert_eq!(
            cheap.kind, "openai-compat",
            "the shorthand implies the adapter"
        );

        // What a person wrote beats what the catalog assumes.
        let strong = &profile.models[1];
        assert_eq!(
            strong.base_url, "https://example.invalid/v1",
            "an explicit base_url is an override, not ignored"
        );
        assert_eq!(
            strong.model, "kimi-k3",
            "the model id still came from the shorthand"
        );
        assert_eq!(strong.api_key.as_deref(), Some("env:LOONG_MOONSHOT_KEY"));
    }

    #[test]
    fn a_model_with_neither_an_endpoint_nor_a_provider_names_both_ways_to_fix_it() {
        let ws = workspace();
        let user = write(&ws, "u.toml", "[[models]]\nname = \"x\"\nmodel = \"m\"\n");
        let err = resolve(&[(Layer::User, user)], &ws)
            .unwrap_err()
            .to_string();
        assert!(err.contains("base_url"), "{err}");
        assert!(
            err.contains("provider"),
            "the error must also name the one-line form: {err}"
        );

        let ws = workspace();
        let user = write(
            &ws,
            "u.toml",
            "[[models]]\nname = \"x\"\nbase_url = \"http://h/v1\"\n",
        );
        let err = resolve(&[(Layer::User, user)], &ws)
            .unwrap_err()
            .to_string();
        assert!(err.contains("`model` id"), "{err}");
        assert!(err.contains("provider"), "{err}");
    }

    #[test]
    fn a_provider_nobody_knows_is_refused_rather_than_silently_endpointless() {
        let ws = workspace();
        let user = write(
            &ws,
            "u.toml",
            "[[models]]\nname = \"x\"\nprovider = \"typo/m\"\n",
        );
        let err = resolve(&[(Layer::User, user)], &ws)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown provider"), "{err}");
        assert!(
            err.contains("deepseek"),
            "and lists what it does know: {err}"
        );
    }

    #[test]
    fn local_mode_is_a_setting_that_lives_in_the_local_file() {
        // The rule, stated directly rather than by writing to somebody's home directory:
        // `mode` means something in the local file and in a file named on the command
        // line, and nowhere else.
        assert!(
            mode_is_legitimate(Layer::User, &local_profile_path().display().to_string()),
            "the local file is where this key belongs"
        );
        assert!(
            mode_is_legitimate(Layer::Session, "/tmp/whatever-the-user-typed.toml"),
            "a session layer is a file somebody pointed at on purpose"
        );
        for forbidden in [
            "/home/me/.loongagent/profile.toml",
            "/etc/loongagent/profile.toml",
        ] {
            assert!(
                !mode_is_legitimate(Layer::User, forbidden),
                "{forbidden} is read because of where it sits, not because it was chosen"
            );
        }

        // Absent means managed, and that is the whole of it — `mode` in a file that is
        // read because of where it sits is refused whatever it says, because the
        // location is the problem, not the value.
        let ws = workspace();
        let bare = write(&ws, "u.toml", "budget_tokens = 1000\n");
        assert!(!resolve(&[(Layer::User, bare)], &ws).unwrap().is_local());
    }

    #[test]
    fn the_managed_profile_may_not_declare_local_mode() {
        // The bug this rule exists for: `mode = "local"` written into the *managed* user
        // file made `loongagent tui` — with no flag at all — resolve as a local install
        // and stop consulting the control plane. A key that can switch an install out of
        // its firm's policy does not belong in a file that is about something else.
        let ws = workspace();
        let managed = write(&ws, "profile.toml", "mode = \"local\"\n");
        let err = resolve(&[(Layer::User, managed)], &ws)
            .unwrap_err()
            .to_string();
        assert!(err.contains("mode"), "{err}");
        assert!(
            err.contains("local.toml"),
            "and says where it does belong: {err}"
        );
        assert!(
            err.contains("--local"),
            "and how to get what they wanted: {err}"
        );
    }

    #[test]
    fn a_local_run_never_reads_the_managed_profile_or_the_org_file() {
        // The isolation, asserted on the layer list itself: if these paths are not in
        // it, no amount of configuration in them can reach a local run.
        let ws = workspace();
        let layers = local_layer_paths(&ws, None);
        let paths: Vec<String> = layers
            .iter()
            .map(|(_, path)| path.display().to_string())
            .collect();

        assert!(
            paths.iter().any(|p| p.ends_with("local.toml")),
            "it reads its own file: {paths:?}"
        );
        assert!(
            !paths.contains(&loongagent_home().join("profile.toml").display().to_string()),
            "and not the managed one: {paths:?}"
        );
        assert!(
            !paths.iter().any(|p| p.starts_with("/etc/")),
            "and not the machine-wide org file: {paths:?}"
        );
        assert!(
            !layers.iter().any(|(layer, _)| *layer == Layer::Org),
            "which is the same statement in terms of layers"
        );
    }

    #[test]
    fn models_come_back_in_the_order_they_were_declared() {
        // The order *is* the tier ladder: cheapest first, most capable last, and the
        // digits the status bar counts by. The merge collects through a map keyed
        // `models.<name>`, so this is the property that a map iteration would destroy —
        // and it was destroyed, by an alphabetical sort, until a real profile showed it.
        //
        // Deliberately written in an order that is *not* alphabetical in either
        // direction, so a sort in either direction fails the test.
        let ws = workspace();
        let user = write(
            &ws,
            "u.toml",
            "[[models]]\nname = \"zulu\"\nprovider = \"ollama/one\"\n\n\
             [[models]]\nname = \"alpha\"\nprovider = \"deepseek/two\"\n\n\
             [[models]]\nname = \"mike\"\nprovider = \"moonshot/three\"\n\n\
             [router]\ndefault = \"zulu\"\n",
        );
        let profile = resolve(&[(Layer::User, user)], &ws).unwrap();
        let names: Vec<&str> = profile.models.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["zulu", "alpha", "mike"],
            "declaration order, which is the ladder"
        );

        // And the ladder is what the router's default names, so the two agree.
        assert_eq!(profile.default_model().unwrap().name, "zulu");
    }

    #[test]
    fn a_model_only_a_lower_layer_declares_is_appended_rather_than_lost() {
        // The order comes from the most local layer that declares any models, but the
        // *set* is a union: an organisation may add a model this user never mentioned,
        // and it has to remain reachable.
        let ws = workspace();
        let org = write(
            &ws,
            "org.toml",
            "[[models]]\nname = \"org-only\"\nprovider = \"deepseek/org\"\n",
        );
        let user = write(
            &ws,
            "u.toml",
            "[[models]]\nname = \"mine\"\nprovider = \"ollama/mine\"\n",
        );
        let profile = resolve(&[(Layer::Org, org), (Layer::User, user)], &ws).unwrap();
        let names: Vec<&str> = profile.models.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["mine", "org-only"],
            "the person's own order leads, and the org's extra model follows"
        );
    }

    #[test]
    fn a_repository_may_not_switch_an_install_out_of_managed_mode() {
        let ws = workspace();
        let project = write(&ws, ".loongagent/profile.toml", "mode = \"local\"\n");
        let err = resolve(&[(Layer::Project, project)], &ws)
            .unwrap_err()
            .to_string();
        assert!(err.contains("mode"), "{err}");
        assert!(
            err.contains("managed"),
            "and says what it would have escaped: {err}"
        );
    }

    #[test]
    fn a_mode_nobody_implements_is_refused_rather_than_read_as_managed() {
        let ws = workspace();
        let user = write(&ws, "u.toml", "mode = \"offline\"\n");
        let err = resolve(&[(Layer::User, user)], &ws)
            .unwrap_err()
            .to_string();
        assert!(err.contains("offline"), "{err}");
        assert!(err.contains("\"local\""), "{err}");
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

    const EXAMPLE_PACK: &str = "\
default_profile = \"default\"\n\
\n\
[[profiles]]\n\
name = \"default\"\n\
description = \"Balanced: cheap for the loop, escalating past 150k tokens.\"\n\
  [[profiles.models]]\n\
  name = \"fast\"\n\
  base_url = \"https://api.deepseek.com\"\n\
  model = \"an-id-from-the-docs\"\n\
  budget_tokens = 128000\n\
  max_context_size = 131072\n\
  [[profiles.models]]\n\
  name = \"frontier\"\n\
  base_url = \"https://api.example.invalid/v1\"\n\
  model = \"big\"\n\
  [profiles.router]\n\
  default = \"fast\"\n\
  [[profiles.router.rules]]\n\
  when = \"context_tokens > 150000\"\n\
  use = \"frontier\"\n\
\n\
[[profiles]]\n\
name = \"local-only\"\n\
description = \"Air-gapped: nothing leaves the machine.\"\n\
  [[profiles.models]]\n\
  name = \"local\"\n\
  base_url = \"http://127.0.0.1:8000/v1\"\n\
  model = \"whatever-you-run\"\n";

    #[test]
    fn a_pack_payload_becomes_named_profiles_with_descriptions() {
        let pack = profiles_from_payload(EXAMPLE_PACK).unwrap();
        assert_eq!(pack.default_profile.as_deref(), Some("default"));
        assert_eq!(pack.profiles.len(), 2);
        assert_eq!(pack.profiles[0].name, "default");
        assert!(pack.profiles[0].description.contains("Balanced"));
        assert_eq!(pack.profiles[0].models.len(), 2);
        // The window a model holds belongs in the pack: it is a fact about the endpoint
        // the pack names, and the engineer never sees the pack (T-07).
        assert_eq!(pack.profiles[0].models[0].budget_tokens, Some(128_000));
        assert_eq!(pack.profiles[0].models[0].max_context_size, Some(131_072));
        assert_eq!(pack.profiles[1].name, "local-only");

        // Choosing is by name, and a name that is not there is an error that lists
        // what is — never a silent fallback to another profile.
        assert_eq!(pack.pick(Some("local-only")).unwrap().name, "local-only");
        let err = pack.pick(Some("supersmart")).unwrap_err().to_string();
        assert!(
            err.contains("supersmart") && err.contains("local-only"),
            "{err}"
        );
        assert_eq!(
            pack.pick(None).unwrap().name,
            "default",
            "the pack's default"
        );
    }

    #[test]
    fn a_literal_in_a_config_file_is_still_refused() {
        // The pack may carry a secret for now; a file a repository or a dotfiles
        // directory can commit may not. That is the line the amendment keeps.
        let ws = workspace();
        let user = write(
            &ws,
            "u.toml",
            "[[models]]\nname = \"m\"\nbase_url = \"https://a.invalid\"\nmodel = \"i\"\napi_key = \"sk-live-abc\"\n",
        );
        let err = resolve(&[(Layer::User, user)], &ws)
            .unwrap_err()
            .to_string();
        assert!(err.contains("must be a reference"), "{err}");
        assert!(err.contains("ADR-0008"), "{err}");
    }

    #[test]
    fn a_secret_pasted_after_the_prefix_is_refused() {
        // The mistake this exists for: `env:` followed by the key itself passes a
        // prefix check, and then the key is inside the signed pack — and inside a
        // committed draft. Every value below is made up — no real key is written
        // into a test, because a test is a file that gets committed.
        for (value, why) in [
            (
                "env:sk-0123456789abcdef0123456789abcdef",
                "a pasted key after env:",
            ),
            (
                "env:sk0123456789abcdef0123456789abcdef",
                "alphanumeric but lowercase",
            ),
            ("env:deepseekApiKey", "not the upper-case convention"),
            ("env:", "empty"),
            (
                "file:credentials.toml#sk-live-abc",
                "a pasted key as the table name",
            ),
            ("keychain:", "empty"),
        ] {
            let payload = format!(
                "[[profiles]]\nname = \"x\"\n  [[profiles.models]]\n  name = \"m\"\n  \
                 base_url = \"https://a.invalid\"\n  model = \"i\"\n  api_key = \"{value}\"\n"
            );
            let error = profiles_from_payload(&payload).unwrap_err().to_string();
            assert!(
                error.contains("reference")
                    || error.contains("variable name")
                    || error.contains("secret"),
                "{why}: {error}"
            );
        }

        // And the shapes that are references still work.
        for value in [
            "env:DEEPSEEK_API_KEY",
            "env:LOONGAGENT_CONTROL_TOKEN",
            "file:/etc/loongagent/client.token",
            "file:credentials.toml#DEEPSEEK_API_KEY",
            "keychain:loongagent/deepseek",
            "gateway:acme/profiles",
        ] {
            let payload = format!(
                "[[profiles]]\nname = \"x\"\n  [[profiles.models]]\n  name = \"m\"\n  \
                 base_url = \"https://a.invalid\"\n  model = \"i\"\n  api_key = \"{value}\"\n"
            );
            assert!(
                profiles_from_payload(&payload).is_ok(),
                "{value} must be accepted"
            );
        }
    }

    #[test]
    fn a_broken_pack_says_what_is_wrong() {
        let err = profiles_from_payload("[[profiles]]\nname = \"x\"\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("no models"), "{err}");
        let err = profiles_from_payload("default_profile = \"d\"\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("no profiles"), "{err}");
        let err = profiles_from_payload(
            "[[profiles]]\nname = \"x\"\n  [[profiles.models]]\n  name = \"m\"\n  base_url = \"https://a.invalid\"\n  model = \"i\"\n  [profiles.router]\n  default = \"ghost\"\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("ghost"), "{err}");
        // A literal in a pack is accepted *and warned about*: the distribution channel
        // may carry it until identity and the gateway exist (ADR-0008 §5 amendment).
        let pack = profiles_from_payload(
            "[[profiles]]\nname = \"x\"\n  [[profiles.models]]\n  name = \"m\"\n  base_url = \"https://a.invalid\"\n  model = \"i\"\n  api_key = \"sk-literal\"\n",
        )
        .unwrap();
        assert_eq!(pack.warnings.len(), 1, "{:?}", pack.warnings);
        assert!(
            pack.warnings[0].contains("carries the secret itself"),
            "{:?}",
            pack.warnings[0]
        );
        assert!(
            pack.warnings[0].contains("ADR-0008"),
            "and names the rule it bends"
        );

        // A well-formed reference warns about nothing.
        let clean = profiles_from_payload(
            "[[profiles]]\nname = \"x\"\n  [[profiles.models]]\n  name = \"m\"\n  base_url = \"https://a.invalid\"\n  model = \"i\"\n  api_key = \"env:DEEPSEEK_API_KEY\"\n",
        )
        .unwrap();
        assert!(clean.warnings.is_empty(), "{:?}", clean.warnings);
    }

    #[test]
    fn the_pack_is_the_control_layer_and_it_outranks_a_local_org_file() {
        let ws = workspace();
        // A stale local org file naming a different endpoint for the same model.
        let org = write(
            &ws,
            "org.toml",
            "[[models]]\nname = \"fast\"\nbase_url = \"http://stale.invalid\"\nmodel = \"old\"\n",
        );
        let profile = resolve_with(
            &[(Layer::Org, org)],
            &ws,
            Some((
                "control plane: acme 1".to_string(),
                pack_layer(
                    &profiles_from_payload(EXAMPLE_PACK)
                        .unwrap()
                        .pick(None)
                        .unwrap()
                        .clone(),
                ),
            )),
        )
        .unwrap();
        let model = &profile.models[0];
        assert_eq!(
            model.base_url, "https://api.deepseek.com",
            "the verified pack wins"
        );
        let provenance = profile.explain("models.fast").unwrap();
        assert_eq!(provenance.layer, Layer::Control);
        assert!(
            provenance.source.contains("acme"),
            "and the row names the pack"
        );
    }

    #[test]
    fn a_user_or_project_may_not_rewrite_what_the_control_plane_set() {
        let ws = workspace();
        let pack = || {
            Some((
                "control plane: acme 1".to_string(),
                pack_layer(
                    &profiles_from_payload(EXAMPLE_PACK)
                        .unwrap()
                        .pick(None)
                        .unwrap()
                        .clone(),
                ),
            ))
        };

        // The endpoint of a control-plane model: refused, and the message says why.
        let user = write(
            &ws,
            "u.toml",
            "[[models]]\nname = \"fast\"\nbase_url = \"http://mine.invalid\"\nmodel = \"mine\"\n",
        );
        let err = resolve_with(&[(Layer::User, user)], &ws, pack())
            .unwrap_err()
            .to_string();
        assert!(err.contains("control plane"), "{err}");
        assert!(err.contains("cannot be changed"), "{err}");
        assert!(err.contains("pick a different profile"), "{err}");

        // The routing table too: the profile carries it.
        let project = write(&ws, "p.toml", "[router]\ndefault = \"frontier\"\n");
        assert!(resolve(&[(Layer::Project, project)], &ws).is_err());

        // But narrowing is still allowed — an org ceiling stays a ceiling, and a
        // local layer may only make things stricter.
        let narrowing = write(&ws, "n.toml", "budget_tokens = 32000\n");
        let narrowed = resolve_with(&[(Layer::User, narrowing)], &ws, pack()).unwrap();
        assert_eq!(narrowed.budget_tokens, Some(32_000));

        // And the user's own choice of profile is theirs.
        let choice = write(&ws, "c.toml", "control.profile = \"local-only\"\n");
        let chosen = resolve_with(&[(Layer::User, choice)], &ws, pack()).unwrap();
        assert_eq!(chosen.control_profile.as_deref(), Some("local-only"));
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
        // The user layer lives under one root, and `LOONGAGENT_HOME` moves it.
        std::env::set_var("LOONGAGENT_HOME", "/tmp/loong-home-test");
        let layers = layer_paths(Path::new("/work/repo"), None);
        let user = layers
            .iter()
            .find(|(layer, _)| *layer == Layer::User)
            .map(|(_, path)| path.clone())
            .unwrap();
        assert_eq!(user, PathBuf::from("/tmp/loong-home-test/profile.toml"));
        std::env::remove_var("LOONGAGENT_HOME");
    }

    #[test]
    fn layer_paths_used_to_be_where_they_claim_to_be() {
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
