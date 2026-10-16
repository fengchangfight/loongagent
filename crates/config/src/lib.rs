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
//! **What is not here:** signing. This crate reads the org layer's *local* file,
//! and that file has no signature. What brings org policy is the pack envelope,
//! and it is signed and verified against a pinned key before it can occupy the
//! `control` layer (ADR-0016); neither is the file this module reads.

pub mod catalog;

mod layer;
mod merge;
mod paths;
mod profile;
mod secret;
#[cfg(test)]
mod tests;

use loong_schema::generated::{ActionClass, Verdict};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub use layer::{kind_of, profiles_from_payload, ConfigError, Kind, Layer, Provenance, Value};
#[cfg(test)]
pub(crate) use merge::mode_is_legitimate;
pub use merge::{resolve, resolve_with};
pub use paths::{
    home_dir, layer_paths, local_layer_paths, local_profile_path, loongagent_home,
    system_config_dir,
};
pub use profile::{
    pack_layer, parse_condition, Condition, ContextEngineSettings, ControlProvenance, Fact, Facts,
    ModelSpec, NamedProfile, Op, ProfilePack, Router, RouterRule,
};
pub(crate) use secret::check_secret_reference;

/// One layer's raw settings, already validated against the key table.
struct LayerValues {
    layer: Layer,
    source: String,
    values: Vec<(String, Value)>,
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
