//! Purpose: The policy decision point: allow, ask or deny plus the rule that fired, and never a widening.
//!
//! # Contract
//! - **Serves:** the kernel's tool dispatch and approval path.
//! - **Design:** `docs/design/tools-permissions.md` §3–§4 (ADR-0007).
//! - **Upstream:** `Kernel::build` and `Kernel::dispatch`.
//! - **Downstream:** `Ladder`, `Ruling` and the platform's `landlock` enforcement point.
//! - **Verify:** `cargo test -p loong-sandbox` · `./scripts/check.sh`
//!
//! Policy decision point (ADR-0007): the kernel decides, the OS enforces.
//!
//! What lives here is the **PDP** — the part that returns
//! `allow | ask | deny` **plus the rule that fired**, over the autonomy ladder
//! (investigate / write-in-workspace / out-of-bounds). Enforcement (PEPs: the OS
//! sandbox, the tool runner, network egress) is deliberately *not* here, so a
//! decision bug cannot by itself grant access.
//!
//! **Monotonicity (ADR-0008).** A lower layer may only tighten a higher one.
//! [`Pdp::tighten`] refuses any change that would widen the effective verdict, so
//! a session override can never turn an org `deny` back into an `allow`.

use loong_schema::generated::{ActionClass, Verdict};

// The enforcement layer is chosen by the platform, not by the caller: landlock is
// the Linux PEP, everywhere else gets an honest "no mechanism" that fails closed
// upward (the kernel reports `unconfined:` and the startup log says so out loud).
// Nothing above this line may assume landlock exists — that is what made the
// workspace Linux-only.
#[cfg(target_os = "linux")]
pub mod landlock;
#[cfg(not(target_os = "linux"))]
#[path = "landlock_unsupported.rs"]
pub mod landlock;
pub mod sandbox;

pub use landlock::WorkspacePolicy;
pub use sandbox::{Capabilities, SelfTest, SelfTestReport};

/// Severity order used by the monotonic rule: allow < ask < deny.
/// Purpose: Severity order allow < ask < deny, used by the monotonic rule.
/// Contract: `crates/sandbox/src/lib.rs`.
fn rank(v: Verdict) -> u8 {
    match v {
        Verdict::Allow => 0,
        Verdict::Ask => 1,
        Verdict::Deny => 2,
    }
}

/// The per-action-class autonomy ladder (ADR-0007 §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Purpose: The per-action-class autonomy ladder.
/// Serves: `Pdp`'s effective values and the kernel's three-way decision.
/// Design: `docs/design/tools-permissions.md` §3.
/// Up: `Pdp::new` holds one; `Pdp::ladder` hands it out.
/// Down: `Pdp`'s `get` and `set`.
/// Verify: `cargo test -p loong-sandbox`.
pub struct Ladder {
    pub investigate: Verdict,
    pub write_workspace: Verdict,
    pub out_of_bounds: Verdict,
}

impl Default for Ladder {
    /// Built-in defaults: investigate free, workspace writes allowed, anything
    /// out of bounds explicitly asked. Junior-proof by default.
    /// Purpose: The junior-proof defaults: investigate and workspace writes allowed, out of bounds asked.
    /// Contract: `crates/sandbox/src/lib.rs`.
    fn default() -> Self {
        Self {
            investigate: Verdict::Allow,
            write_workspace: Verdict::Allow,
            out_of_bounds: Verdict::Ask,
        }
    }
}

impl Ladder {
    /// Purpose: The verdict in force for one action class.
    /// Serves: `Pdp`'s decision and tightening methods.
    /// Design: `docs/design/tools-permissions.md` §3.
    /// Up: `Pdp::tighten_from` and `Pdp::decide`; no caller outside this module.
    /// Down: the three ladder fields.
    /// Verify: `cargo test -p loong-sandbox`.
    pub fn get(&self, class: ActionClass) -> Verdict {
        match class {
            ActionClass::Investigate => self.investigate,
            ActionClass::WriteWorkspace => self.write_workspace,
            ActionClass::OutOfBounds => self.out_of_bounds,
        }
    }

    /// Purpose: Replace one action class's verdict.
    /// Contract: `crates/sandbox/src/lib.rs`.
    fn set(&mut self, class: ActionClass, verdict: Verdict) {
        match class {
            ActionClass::Investigate => self.investigate = verdict,
            ActionClass::WriteWorkspace => self.write_workspace = verdict,
            ActionClass::OutOfBounds => self.out_of_bounds = verdict,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Purpose: Why a tightening was refused.
/// Serves: `Pdp::tighten`/`tighten_from` and the kernel's build step.
/// Design: `docs/design/tools-permissions.md` §3 (ADR-0008).
/// Up: `Pdp::tighten_from`; the kernel's build.
/// Down: its `Display` impl.
/// Verify: `cargo test -p loong-sandbox`.
pub enum PolicyError {
    /// The requested change would loosen a higher layer's decision.
    WouldWiden {
        class: ActionClass,
        from: Verdict,
        to: Verdict,
    },
}

impl std::fmt::Display for PolicyError {
    /// Purpose: Render the refusal as the class and the verdicts it would change.
    /// Contract: `crates/sandbox/src/lib.rs`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PolicyError::WouldWiden { class, from, to } => write!(
                f,
                "{class:?}: refusing to widen {from:?} -> {to:?} (lower layers may only tighten)"
            ),
        }
    }
}

impl std::error::Error for PolicyError {}

/// A verdict with its provenance: which rule decided it, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Purpose: A verdict with the rule that decided it and a human-readable reason.
/// Serves: `policy.decided` and the hook tightening path.
/// Design: `docs/design/tools-permissions.md` §3.
/// Up: `Pdp::decide`; the kernel's hook path.
/// Down: the kernel's recorded event and the hook's stricter ruling.
/// Verify: `cargo test -p loong-sandbox`.
pub struct Ruling {
    pub verdict: Verdict,
    /// The rule that fired, e.g. `builtin:out_of_bounds` or `session:out_of_bounds`.
    pub rule: String,
    pub reason: String,
}

/// Purpose: The three-slot decision point: an effective ladder and one rule string per slot.
/// Serves: every tool call's allow/ask/deny decision.
/// Design: `docs/design/tools-permissions.md` §3.
/// Up: the kernel's build step and dispatch.
/// Down: `Ladder`, `tighten_from` and `decide`.
/// Verify: `cargo test -p loong-sandbox`.
pub struct Pdp {
    effective: Ladder,
    rules: [String; 3],
}

impl Default for Pdp {
    /// Purpose: The default decision point, with the built-in ladder.
    /// Contract: `crates/sandbox/src/lib.rs`.
    fn default() -> Self {
        Self::new()
    }
}

impl Pdp {
    /// Purpose: A decision point with the built-in ladder and `builtin:` rules.
    ///
    /// Serves: the kernel's build step, before any profile value narrows it.
    /// Design: `docs/design/tools-permissions.md` §3.
    /// Up: `Kernel::build` and `Pdp::default`.
    /// Down: `Ladder::default` and the three rule slots.
    /// Verify: `cargo test -p loong-sandbox`.
    pub fn new() -> Self {
        Self {
            effective: Ladder::default(),
            rules: [
                "builtin:investigate".into(),
                "builtin:write_workspace".into(),
                "builtin:out_of_bounds".into(),
            ],
        }
    }

    /// Purpose: The rule slot for an action class.
    /// Contract: `crates/sandbox/src/lib.rs`.
    fn slot(class: ActionClass) -> usize {
        match class {
            ActionClass::Investigate => 0,
            ActionClass::WriteWorkspace => 1,
            ActionClass::OutOfBounds => 2,
        }
    }

    /// Purpose: The ladder currently in force.
    ///
    /// Serves: callers that need the whole ladder rather than one class's verdict.
    /// Design: `docs/design/tools-permissions.md` §3.
    /// Up: its own unit test; the kernel asks `decide` or `tighten_from` instead.
    /// Down: the `effective` field.
    /// Verify: `cargo test -p loong-sandbox`.
    pub fn ladder(&self) -> Ladder {
        self.effective
    }

    /// Tighten one action class. Refuses to widen (ADR-0008 monotonicity).
    /// Purpose: Tighten one action class, refusing any change that would widen.
    /// Serves: callers that narrow a class without naming a provenance layer.
    /// Design: `docs/design/tools-permissions.md` §3.
    /// Up: its own unit tests and kernel tests; no production caller (the kernel narrows through `tighten_from`).
    /// Down: `tighten_from` with the source `session`.
    /// Verify: `cargo test -p loong-sandbox`.
    pub fn tighten(&mut self, class: ActionClass, verdict: Verdict) -> Result<(), PolicyError> {
        self.tighten_from(class, verdict, "session")
    }

    /// Tighten and record **who** narrowed it, so the rule that fired names its
    /// source (ADR-0008 §4 provenance, ADR-0007 "the rule that fired").
    /// Purpose: Tighten one class and record which source narrowed it.
    /// Serves: the kernel's build step, which applies a profile's values with their provenance.
    /// Design: `docs/design/tools-permissions.md` §3.
    /// Up: `Kernel::build` and `Pdp::tighten`.
    /// Down: `Ladder::get`/`set`, `rank` and `class_name`.
    /// Verify: `cargo test -p loong-sandbox`.
    pub fn tighten_from(
        &mut self,
        class: ActionClass,
        verdict: Verdict,
        source: &str,
    ) -> Result<(), PolicyError> {
        let from = self.effective.get(class);
        if rank(verdict) < rank(from) {
            return Err(PolicyError::WouldWiden {
                class,
                from,
                to: verdict,
            });
        }
        self.effective.set(class, verdict);
        self.rules[Self::slot(class)] = format!("{source}:{}", class_name(class));
        Ok(())
    }

    /// Decide an action. `action` is a human/model-readable label (usually the
    /// tool name) used only in the reason, never in the decision.
    /// Purpose: Decide an action class, returning the verdict with the rule and reason.
    /// Serves: the kernel's dispatch, before the tool runs.
    /// Design: `docs/design/tools-permissions.md` §3.
    /// Up: `Kernel::dispatch`.
    /// Down: `Ladder::get` and `class_name`.
    /// Verify: `cargo test -p loong-sandbox`.
    pub fn decide(&self, class: ActionClass, action: &str) -> Ruling {
        let verdict = self.effective.get(class);
        let rule = self.rules[Self::slot(class)].clone();
        Ruling {
            verdict,
            reason: format!("{action}: {} is {verdict:?} by {rule}", class_name(class)),
            rule,
        }
    }
}

/// Purpose: The action class as it appears in a rule string.
/// Contract: `crates/sandbox/src/lib.rs`.
fn class_name(class: ActionClass) -> &'static str {
    match class {
        ActionClass::Investigate => "investigate",
        ActionClass::WriteWorkspace => "write_workspace",
        ActionClass::OutOfBounds => "out_of_bounds",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Purpose: Pin the built-in ladder: investigate and workspace writes allow, out of bounds ask.
    fn defaults_are_junior_proof() {
        let p = Pdp::new();
        assert_eq!(p.ladder().investigate, Verdict::Allow);
        assert_eq!(p.ladder().write_workspace, Verdict::Allow);
        assert_eq!(
            p.ladder().out_of_bounds,
            Verdict::Ask,
            "out of bounds always asks"
        );
    }

    #[test]
    /// Purpose: Pin that tightening narrows and records the source in the rule.
    fn a_lower_layer_may_tighten() {
        let mut p = Pdp::new();
        p.tighten(ActionClass::OutOfBounds, Verdict::Deny).unwrap();
        let r = p.decide(ActionClass::OutOfBounds, "curl");
        assert_eq!(r.verdict, Verdict::Deny);
        assert_eq!(
            r.rule, "session:out_of_bounds",
            "the rule that fired is recorded"
        );
        assert!(r.reason.contains("out_of_bounds"));
    }

    #[test]
    /// Purpose: Pin that a widening attempt is refused and the stricter verdict stands.
    fn a_lower_layer_may_not_widen() {
        let mut p = Pdp::new();
        p.tighten(ActionClass::OutOfBounds, Verdict::Deny).unwrap();
        let err = p
            .tighten(ActionClass::OutOfBounds, Verdict::Allow)
            .unwrap_err();
        assert_eq!(
            err,
            PolicyError::WouldWiden {
                class: ActionClass::OutOfBounds,
                from: Verdict::Deny,
                to: Verdict::Allow
            }
        );
        assert_eq!(
            p.decide(ActionClass::OutOfBounds, "x").verdict,
            Verdict::Deny
        );
    }

    #[test]
    /// Purpose: Pin that tightening one action class leaves the others alone.
    fn classes_are_independent() {
        let mut p = Pdp::new();
        p.tighten(ActionClass::WriteWorkspace, Verdict::Ask)
            .unwrap();
        assert_eq!(
            p.decide(ActionClass::WriteWorkspace, "write_file").verdict,
            Verdict::Ask
        );
        assert_eq!(
            p.decide(ActionClass::Investigate, "read_file").verdict,
            Verdict::Allow
        );
    }

    #[test]
    /// Purpose: Pin that an equal verdict is allowed and still renames the rule.
    fn tightening_to_the_same_verdict_is_allowed_and_renames_the_rule() {
        let mut p = Pdp::new();
        p.tighten(ActionClass::Investigate, Verdict::Allow).unwrap();
        assert_eq!(
            p.decide(ActionClass::Investigate, "read").rule,
            "session:investigate"
        );
    }
}
