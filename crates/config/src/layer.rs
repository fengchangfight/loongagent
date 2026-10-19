//! Purpose: The layer order and the four merge kinds: every key declares how a lower layer may change it.
//!
//! # Contract
//! - **Serves:** config resolution and `config explain`.
//! - **Design:** `docs/design/config-routing.md` §1–§2 (ADR-0008).
//! - **Upstream:** `merge`, `paths` and the control plane's payload parser.
//! - **Downstream:** the typed `Profile` values and the named layer order.
//! - **Verify:** `cargo test -p loong-config` · `./scripts/check.sh`
//!
use super::*;
use crate::check_secret_reference;
use crate::paths::expand_home;
use crate::profile::finish_model;
use crate::secret::{check_endpoint_reference, check_pack_secret};
use serde::Deserialize;

/// Lowest to highest precedence. The order *is* the rule: later beats earlier for
/// free-override, and can never loosen earlier for narrow-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
/// Purpose: Lowest to highest precedence; the order is itself the merge rule.
/// Serves: merge order, provenance and `config show`'s layer names.
/// Design: `docs/design/config-routing.md` §1.
/// Up: `paths` builds the file list; `resolve_with` sorts the layers it was given.
/// Down: the per-kind merge rules and `Provenance::layer`.
/// Verify: `cargo test -p loong-config`.
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
    /// Purpose: The layer's name in an error, a rule string or `config show`.
    /// Serves: provenance output and the control-plane lock's error message.
    /// Design: `docs/design/config-routing.md` §1.
    /// Up: `merge`'s lock error and the CLI's config command.
    /// Down: the enum variants.
    /// Verify: `cargo test -p loong-config`.
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
/// Purpose: How a key merges: free-override, narrow-only, add-only or reference.
/// Serves: the one rule that stops a lower layer widening a higher one.
/// Design: `docs/design/config-routing.md` §2.
/// Up: `kind_of`; the merge's match and the control lock.
/// Down: `merge`'s per-kind handling.
/// Verify: `cargo test -p loong-config`.
pub enum Kind {
    FreeOverride,
    NarrowOnly,
    AddOnly,
    Reference,
}

impl Kind {
    /// Purpose: The kind's name in an error, an explanation or a test assertion.
    /// Serves: `config explain` and the lock error.
    /// Design: `docs/design/config-routing.md` §2.
    /// Up: the CLI's config command and kernel tests.
    /// Down: the enum variants.
    /// Verify: `cargo test -p loong-config`.
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
/// Purpose: The declared kind of a known key, or `None` for a typo.
/// Serves: load-time validation and the merge rule; an unknown key is an error.
/// Design: `docs/design/config-routing.md` §2.
/// Up: `LayerValues::from_file`, `merge` and the CLI's config command.
/// Down: no callee; the match table is the declaration.
/// Verify: `cargo test -p loong-config`.
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
/// Purpose: A resolved value, carrying the variant the merge compares rather than a rendered string.
/// Serves: provenance rows and the typed `Profile` mirrors.
/// Design: `docs/design/config-routing.md` §2.
/// Up: layer loading, pack values and the merge.
/// Down: `Value::render` and the merge's per-kind handling.
/// Verify: `cargo test -p loong-config`.
pub enum Value {
    Text(String),
    Number(u64),
    Verdict(Verdict),
    List(Vec<String>),
    Model(ModelSpec),
    Rules(Vec<RouterRule>),
}

impl Value {
    /// Purpose: The value as `config show` and an error print it.
    /// Serves: the surfaces that print provenance and the add-only union's key.
    /// Design: `docs/design/config-routing.md` §2.
    /// Up: the CLI's config, web and server surfaces; `merge` for the add-only key.
    /// Down: `Condition::render` for a rule list.
    /// Verify: `cargo test -p loong-config`.
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
/// Purpose: One contributing key: the winning value, its layer and source, and the losers.
/// Serves: `config explain` and the PDP's rule string.
/// Design: `docs/design/config-routing.md` §2.
/// Up: `merge`; read through `Profile::explain`.
/// Down: the surfaces that print it and `permission_with_source`.
/// Verify: `cargo test -p loong-config`.
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
/// Purpose: Why configuration could not be read, parsed or accepted.
/// Serves: every loader that must name the bad file and the rule it broke.
/// Design: `docs/design/config-routing.md` §1–§3.
/// Up: the loaders and the merge; the CLI surfaces it.
/// Down: its `Display` impl and every `Result` in this crate.
/// Verify: `cargo test -p loong-config`.
pub enum ConfigError {
    Io(String),
    Parse(String),
    Invalid(String),
}

impl std::fmt::Display for ConfigError {
    /// Purpose: Render the error with the file prefix a person needs.
    /// Contract: `crates/config/src/layer.rs`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io(m) => write!(f, "{m}"),
            ConfigError::Parse(m) => write!(f, "profile: {m}"),
            ConfigError::Invalid(m) => write!(f, "profile: {m}"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Read a profile pack payload (the TOML inside a signed envelope).
///
/// Kept in this crate because the payload *is* configuration: the control plane and
/// the local agent must agree on what it means, and the merge rules apply to it.
/// Purpose: Parse a pack payload into named profiles, validating adapters, models and routing.
/// Serves: the control plane and the client, which must agree on what a pack means.
/// Design: `docs/design/config-routing.md` §4 (ADR-0016).
/// Up: `loong-control`'s publish, fetch and health paths.
/// Down: `finish_model`, `parse_condition` and `check_pack_secret`.
/// Verify: `cargo test -p loong-config`.
pub fn profiles_from_payload(payload: &str) -> Result<ProfilePack, ConfigError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    /// Purpose: The top of a pack payload: its default profile name and its profiles.
    /// Serves: `profiles_from_payload`'s `deny_unknown_fields` parse.
    /// Design: `docs/design/config-routing.md` §4.
    /// Up: `profiles_from_payload` through `toml::from_str`.
    /// Down: `RawProfileEntry`.
    /// Verify: `cargo test -p loong-config`.
    struct RawPayload {
        default_profile: Option<String>,
        #[serde(default)]
        profiles: Vec<RawProfileEntry>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    /// Purpose: One profile inside a pack payload.
    /// Serves: pack parsing and the per-profile validation.
    /// Design: `docs/design/config-routing.md` §4.
    /// Up: `RawPayload`.
    /// Down: `RawModel` and `RawRouter`.
    /// Verify: `cargo test -p loong-config`.
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

/// `deny_unknown_fields` is load-bearing: a mistyped key in a policy file that is
/// silently ignored is a guard that silently does not exist.
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
/// Purpose: One profile file's fields, before any layer merge.
/// Serves: `LayerValues::from_file`; `deny_unknown_fields` makes a typo loud.
/// Design: `docs/design/config-routing.md` §1–§3.
/// Up: `LayerValues::from_file`.
/// Down: `RawModel`, `RawRouter`, `RawControl`, `RawContextEngine` and `RawSummarizer`.
/// Verify: `cargo test -p loong-config`.
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
/// Purpose: The `[context_engine]` table of one profile file.
/// Serves: the opt-in layered memory and its narrow-only switches.
/// Design: `docs/design/config-routing.md` §2; `docs/design/context-ledger.md`.
/// Up: `RawProfile`.
/// Down: `LayerValues::from_file`'s enabled, mode and budget rows.
/// Verify: `cargo test -p loong-config`.
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
/// Purpose: The `[summarizer]` table: kind, endpoint and idle-window budget.
/// Serves: the local summariser's policy.
/// Design: `docs/design/config-routing.md` §2.
/// Up: `RawProfile`.
/// Down: `LayerValues::from_file`.
/// Verify: `cargo test -p loong-config`.
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
/// Purpose: The user's side of the control plane: which named profile to run.
/// Serves: `control.profile` resolution.
/// Design: `docs/design/config-routing.md` §4.
/// Up: `RawProfile`.
/// Down: `LayerValues::from_file`.
/// Verify: `cargo test -p loong-config`.
struct RawControl {
    profile: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
/// Purpose: One `[[models]]` entry before catalog expansion.
/// Serves: profile and pack model declarations.
/// Design: `docs/design/config-routing.md` §4.
/// Up: `RawProfile` and `RawProfileEntry`.
/// Down: `finish_model`.
/// Verify: `cargo test -p loong-config`.
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
/// Purpose: The router table of a profile file or a pack payload.
/// Serves: ordered first-match routing.
/// Design: `docs/design/config-routing.md` §7.
/// Up: `RawProfile` and `RawProfileEntry`.
/// Down: `RawRule` and `parse_condition`.
/// Verify: `cargo test -p loong-config`.
struct RawRouter {
    default: Option<String>,
    #[serde(default)]
    rules: Vec<RawRule>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
/// Purpose: One routing rule as written: a `when` expression and the model to use.
/// Serves: `parse_condition`'s input.
/// Design: `docs/design/config-routing.md` §7.
/// Up: `RawRouter`.
/// Down: `parse_condition`.
/// Verify: `cargo test -p loong-config`.
struct RawRule {
    when: String,
    #[serde(rename = "use")]
    model: String,
}

impl LayerValues {
    /// Purpose: Read one layer file and validate every key and value against its declared kind.
    /// Serves: `resolve_with`; this is where the project-layer refusals live.
    /// Design: `docs/design/config-routing.md` §2–§3.
    /// Up: `merge::resolve_with`.
    /// Down: `finish_model`, `check_secret_reference`, `parse_condition`, `parse_class`, `parse_verdict` and `expand_home`.
    /// Verify: `cargo test -p loong-config`.
    pub(super) fn from_file(
        layer: Layer,
        path: &Path,
        workspace: Option<&Path>,
    ) -> Result<Self, ConfigError> {
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
    /// Purpose: The built-in ladder as the lowest layer and the ceiling of permissiveness.
    /// Serves: `resolve_with` and the PDP's allow/ask/deny defaults.
    /// Design: `docs/design/tools-permissions.md` §3; `docs/design/config-routing.md` §1.
    /// Up: `merge::resolve_with`.
    /// Down: the three `Layer::Builtin` permission rows.
    /// Verify: `cargo test -p loong-config`.
    pub(super) fn builtin_defaults() -> Self {
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

/// Purpose: The English action-class key to its schema value.
/// Serves: `permissions.*` from a file and the merged verdict rows.
/// Design: `docs/design/tools-permissions.md` §3.
/// Up: `LayerValues::from_file` and `merge`'s typed view.
/// Down: `ActionClass`.
/// Verify: `cargo test -p loong-config`.
pub(super) fn parse_class(text: &str) -> Option<ActionClass> {
    match text {
        "investigate" => Some(ActionClass::Investigate),
        "write_workspace" => Some(ActionClass::WriteWorkspace),
        "out_of_bounds" => Some(ActionClass::OutOfBounds),
        _ => None,
    }
}

/// Purpose: The allow/ask/deny text to its schema value.
/// Serves: `permissions.*` validation.
/// Design: `docs/design/tools-permissions.md` §3.
/// Up: `LayerValues::from_file`.
/// Down: `Verdict`.
/// Verify: `cargo test -p loong-config`.
fn parse_verdict(text: &str) -> Option<Verdict> {
    match text {
        "allow" => Some(Verdict::Allow),
        "ask" => Some(Verdict::Ask),
        "deny" => Some(Verdict::Deny),
        _ => None,
    }
}
