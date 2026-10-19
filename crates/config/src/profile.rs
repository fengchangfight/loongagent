//! Purpose: Profiles, routing rules and the resolved settings a kernel is built from.
//!
//! # Contract
//! - **Serves:** the CLI's profile picker and the kernel's per-step model router.
//! - **Design:** `docs/design/config-routing.md` §4 and §7 (ADR-0005, ADR-0016).
//! - **Upstream:** the layer loader and the control plane's pack payload parser.
//! - **Downstream:** `layer`'s values and `catalog::expand` for the shorthand.
//! - **Verify:** `cargo test -p loong-config` · `./scripts/check.sh`
//!
use super::*;

/// A model a profile offers. Engineers pick one of these by name (ADR-0005/0007: a
/// profile names models, not endpoints), and the router chooses between them.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Purpose: One model a profile offers, with its endpoint, id and key reference.
/// Serves: the profile picker, the router and the kernel's provider builder.
/// Design: `docs/design/config-routing.md` §4.
/// Up: `profiles_from_payload` and `LayerValues::from_file` through `finish_model`.
/// Down: `Profile::models`, `pack_layer` and the provider built from it.
/// Verify: `cargo test -p loong-config`.
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
/// Purpose: The two session facts a routing rule may read.
/// Serves: `Router::select` on every step.
/// Design: `docs/design/config-routing.md` §7.
/// Up: the kernel computes them from the ledger's token total and the step number.
/// Down: `Condition::matches`.
/// Verify: `cargo test -p loong-config`.
pub struct Facts {
    pub context_tokens: u64,
    pub step: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Purpose: Which session fact a routing condition reads.
/// Serves: `parse_condition`, `Condition::matches` and `Condition::render`.
/// Design: `docs/design/config-routing.md` §7.
/// Up: `parse_condition` from a profile's `when` text.
/// Down: `Condition::matches` and `Condition::render`.
/// Verify: `cargo test -p loong-config`.
pub enum Fact {
    ContextTokens,
    Step,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Purpose: The comparison a routing condition applies.
/// Serves: `parse_condition`, `Condition::matches` and `Condition::render`.
/// Design: `docs/design/config-routing.md` §7.
/// Up: `parse_condition`.
/// Down: `Condition::matches` and `Condition::render`.
/// Verify: `cargo test -p loong-config`.
pub enum Op {
    Gt,
    Ge,
    Lt,
    Le,
    Eq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Purpose: One parsed routing condition: fact, comparison and threshold.
/// Serves: `RouterRule` and the explanation `render` gives.
/// Design: `docs/design/config-routing.md` §7.
/// Up: `parse_condition` and `profiles_from_payload`.
/// Down: `Condition::matches` and `Condition::render`.
/// Verify: `cargo test -p loong-config`.
pub struct Condition {
    pub fact: Fact,
    pub op: Op,
    pub value: u64,
}

impl Condition {
    /// Purpose: Whether the session's facts satisfy this condition.
    /// Serves: first-match routing.
    /// Design: `docs/design/config-routing.md` §7.
    /// Up: `Router::select`.
    /// Down: the `Fact` and `Op` variants.
    /// Verify: `cargo test -p loong-config`.
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

    /// Purpose: The condition as the text a person wrote, for a log or `config show`.
    /// Serves: `Value::render` and the `model.selected` rule.
    /// Design: `docs/design/config-routing.md` §7.
    /// Up: `Value::render` in `config`, and the turn loop when it records a routing rule.
    /// Down: the `Fact` and `Op` renderings.
    /// Verify: `cargo test -p loong-config`.
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
/// Purpose: One rule: if its condition holds, route to this model.
/// Serves: ordered first-match routing.
/// Design: `docs/design/config-routing.md` §7.
/// Up: the profile and pack parsers, through `parse_condition`.
/// Down: `Router::select` and `Value::render`.
/// Verify: `cargo test -p loong-config`.
pub struct RouterRule {
    pub condition: Condition,
    pub model: String,
}

/// Ordered, first-match-wins. The rule that fired is recorded with the choice, so
/// "why this model?" has an answer (ADR-0008 §4 applied to routing).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
/// Purpose: Ordered, first-match routing between a profile's models, with a default.
/// Serves: the kernel's per-step model choice.
/// Design: `docs/design/config-routing.md` §7.
/// Up: the layer loader and the pack payload parser; `Profile::router`.
/// Down: `Router::select` and `merge::finish`'s validation.
/// Verify: `cargo test -p loong-config`.
pub struct Router {
    pub default: Option<String>,
    pub rules: Vec<RouterRule>,
}

impl Router {
    /// `(model, Some(rule))` when a rule fired, `(default, None)` otherwise.
    /// Purpose: The model the first matching rule names, else the default, with the rule that fired.
    /// Serves: the kernel's step-time routing and the `model.selected` record.
    /// Design: `docs/design/config-routing.md` §7.
    /// Up: the turn loop's model choice (`crates/kernel/src/turn/loop.rs`).
    /// Down: `Condition::matches`.
    /// Verify: `cargo test -p loong-config`.
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
/// Purpose: A named bundle of models plus the router between them.
/// Serves: a person picking a profile by name instead of a model list.
/// Design: `docs/design/config-routing.md` §4.
/// Up: `profiles_from_payload`; held by `ProfilePack`.
/// Down: `ProfilePack::pick` and `pack_layer`.
/// Verify: `cargo test -p loong-config`.
pub struct NamedProfile {
    pub name: String,
    pub description: String,
    pub models: Vec<ModelSpec>,
    pub router: Router,
}

/// A pack's contents: the bundles, and which one runs when a user has not chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Purpose: A pack's contents: the named profiles, its default, and operator warnings.
/// Serves: the control plane's signed payload and the client's profile choice.
/// Design: `docs/design/config-routing.md` §4.
/// Up: `profiles_from_payload`, after the envelope verified.
/// Down: `ProfilePack::pick`, `pack_layer` and the control plane's health output.
/// Verify: `cargo test -p loong-config`.
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
    /// Purpose: The profile the user chose, else the pack's default, else the first.
    /// Serves: setup and the CLI's pack path, where an unknown chosen name must be refused.
    /// Design: `docs/design/config-routing.md` §4.
    /// Up: `crates/cli/src/control.rs` after fetching a pack.
    /// Down: the profiles scan and `ConfigError::Invalid`.
    /// Verify: `cargo test -p loong-config`.
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
/// Purpose: The chosen profile as the control layer's models, default and rules.
/// Serves: the locked control layer that carries a verified pack's policy.
/// Design: `docs/design/config-routing.md` §4.
/// Up: the CLI's `settings_with_pack`.
/// Down: `Value::Model`, `Value::Text` and `Value::Rules`.
/// Verify: `cargo test -p loong-config`.
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
/// Purpose: Which pack and profile a session is running, for provenance and the surface header.
/// Serves: `config show`, the TUI header and the kernel's profile record.
/// Design: `docs/design/config-routing.md` §4.
/// Up: the CLI when it verifies a pack; `Profile::control`.
/// Down: the surfaces that print it.
/// Verify: `cargo test -p loong-config`.
pub struct ControlProvenance {
    pub issuer: String,
    pub version: String,
    pub key_id: String,
    pub profile: String,
}

/// The resolved layered-memory settings, ready for a caller to build a kernel
/// config from. Plain values on purpose: `loong-config` decides *policy*, and the
/// kernel decides how to run — a settings struct is the seam between them.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Purpose: The resolved layered-memory settings, ready for a kernel config.
/// Serves: the seam where config decides policy and the kernel decides how to run.
/// Design: `docs/design/config-routing.md` §2; `docs/design/context-ledger.md`.
/// Up: `Profile::context_engine`; the server when it builds the kernel config.
/// Down: the kernel's context engine and summariser choice.
/// Verify: `cargo test -p loong-config`.
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
    /// Purpose: The layered-memory settings, or `None` when the mechanism is off or unmentioned.
    /// Serves: the server's kernel build, and the rule that "not mentioned" never reads as "on".
    /// Design: `docs/design/config-routing.md` §2; `docs/design/context-ledger.md`.
    /// Up: `crates/cli/src/server.rs` when it builds the context engine.
    /// Down: the typed `ContextEngineSettings` fields.
    /// Verify: `cargo test -p loong-config`.
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
    /// Purpose: Whether this install runs without a control plane.
    /// Serves: every caller that must not reach the control plane, and the mode guard.
    /// Design: `docs/design/config-routing.md` §3.
    /// Up: the CLI's layer choice and `config show`.
    /// Down: `Profile::mode`.
    /// Verify: `cargo test -p loong-config`.
    pub fn is_local(&self) -> bool {
        self.mode.as_deref() == Some("local")
    }

    /// The model a turn starts with: the router's default, else the first declared.
    /// Purpose: The model a turn starts with: the router's default, else the first declared.
    /// Serves: the CLI's initial model and the kernel's convenience mirror.
    /// Design: `docs/design/config-routing.md` §7.
    /// Up: `crates/cli/src/local.rs` and `merge::finish`, which points the mirrors at it.
    /// Down: `router.default` and `models`.
    /// Verify: `cargo test -p loong-config`.
    pub fn default_model(&self) -> Option<&ModelSpec> {
        match self.router.default.as_deref() {
            Some(name) => self.models.iter().find(|m| m.name == name),
            None => self.models.first(),
        }
    }

    /// Purpose: The resolved verdict for one action class.
    /// Serves: readers that need only the verdict, not the layer that set it.
    /// Design: `docs/design/tools-permissions.md` §3.
    /// Up: its own unit tests; production callers use `permission_with_source`.
    /// Down: `self.permissions`.
    /// Verify: `cargo test -p loong-config`.
    pub fn permission(&self, class: ActionClass) -> Option<Verdict> {
        self.permissions
            .iter()
            .find(|(c, _)| *c == class)
            .map(|(_, v)| *v)
    }

    /// The verdict **and** the layer that set it, for the PDP's rule string.
    /// Purpose: The verdict and the layer that set it, for the PDP's rule string.
    /// Serves: the kernel's build step, which tightens the ladder from profile values.
    /// Design: `docs/design/tools-permissions.md` §3; `docs/design/config-routing.md` §2.
    /// Up: `crates/kernel/src/turn/build.rs`.
    /// Down: `Profile::explain` and `Value::Verdict`.
    /// Verify: `cargo test -p loong-config`.
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
    /// Purpose: The provenance row for one key: what won, from where, and what lost.
    /// Serves: `config show`/`config explain` and the mode guard.
    /// Design: `docs/design/config-routing.md` §2.
    /// Up: `permission_with_source`, `merge::resolve_with` and the CLI's config command.
    /// Down: the private `provenance` map.
    /// Verify: `cargo test -p loong-config`.
    pub fn explain(&self, key: &str) -> Option<&Provenance> {
        self.provenance.get(key)
    }

    /// Every key that has a value, ready to print.
    /// Purpose: Every key that has a value, in key order.
    /// Serves: `config show`, the web and server surfaces, and the startup log.
    /// Design: `docs/design/config-routing.md` §2.
    /// Up: `crates/cli/src/config_cmd.rs`, `web.rs` and `server.rs`.
    /// Down: `provenance.values`.
    /// Verify: `cargo test -p loong-config`.
    pub fn explain_all(&self) -> Vec<&Provenance> {
        self.provenance.values().collect()
    }

    /// Purpose: Whether any key was resolved at all.
    /// Serves: callers that must distinguish "nothing configured" from "defaults applied".
    /// Design: `docs/design/config-routing.md` §2.
    /// Up: no in-tree caller today; the method is the only spelling of the distinction.
    /// Down: `provenance`.
    /// Verify: `cargo test -p loong-config`.
    pub fn is_empty(&self) -> bool {
        self.provenance.is_empty()
    }
}

/// Resolve a `[[models]]` entry to its concrete endpoint, model id and key reference.
///
/// The order is deliberate: what a person *wrote* beats what the catalog *assumes*,
/// and anything the shorthand was supposed to supply but nobody wrote is an error
/// that names both ways of saying it — a model silently pointed at the wrong
/// endpoint is the worst outcome available here.
/// Purpose: Resolve a model entry to its endpoint, id, key reference and catalog provider.
/// Serves: both a profile file and a pack payload, so the shorthand means one thing.
/// Design: `docs/design/config-routing.md` §4 and §8.
/// Up: `LayerValues::from_file` and `profiles_from_payload`.
/// Down: `catalog::expand`.
/// Verify: `cargo test -p loong-config`.
pub(super) fn finish_model(
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

/// `context_tokens > 150000` → a condition. Unknown facts are errors that list the
/// ones that exist, because a rule that never fires is worse than no rule.
/// Purpose: Parse `fact op number` into a routing condition.
/// Serves: profile files and pack payloads; a rule that never fires is an error.
/// Design: `docs/design/config-routing.md` §7.
/// Up: `LayerValues::from_file`, `profiles_from_payload` and kernel tests.
/// Down: `Fact`, `Op` and `Condition`.
/// Verify: `cargo test -p loong-config`.
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
