use super::*;

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

/// Resolve a `[[models]]` entry to its concrete endpoint, model id and key reference.
///
/// The order is deliberate: what a person *wrote* beats what the catalog *assumes*,
/// and anything the shorthand was supposed to supply but nobody wrote is an error
/// that names both ways of saying it — a model silently pointed at the wrong
/// endpoint is the worst outcome available here.
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
