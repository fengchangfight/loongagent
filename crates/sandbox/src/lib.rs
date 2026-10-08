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

pub mod landlock;
pub mod sandbox;

pub use landlock::WorkspacePolicy;
pub use sandbox::{Capabilities, SelfTest, SelfTestReport};

/// Severity order used by the monotonic rule: allow < ask < deny.
fn rank(v: Verdict) -> u8 {
    match v {
        Verdict::Allow => 0,
        Verdict::Ask => 1,
        Verdict::Deny => 2,
    }
}

/// The per-action-class autonomy ladder (ADR-0007 §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ladder {
    pub investigate: Verdict,
    pub write_workspace: Verdict,
    pub out_of_bounds: Verdict,
}

impl Default for Ladder {
    /// Built-in defaults: investigate free, workspace writes allowed, anything
    /// out of bounds explicitly asked. Junior-proof by default.
    fn default() -> Self {
        Self {
            investigate: Verdict::Allow,
            write_workspace: Verdict::Allow,
            out_of_bounds: Verdict::Ask,
        }
    }
}

impl Ladder {
    pub fn get(&self, class: ActionClass) -> Verdict {
        match class {
            ActionClass::Investigate => self.investigate,
            ActionClass::WriteWorkspace => self.write_workspace,
            ActionClass::OutOfBounds => self.out_of_bounds,
        }
    }

    fn set(&mut self, class: ActionClass, verdict: Verdict) {
        match class {
            ActionClass::Investigate => self.investigate = verdict,
            ActionClass::WriteWorkspace => self.write_workspace = verdict,
            ActionClass::OutOfBounds => self.out_of_bounds = verdict,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyError {
    /// The requested change would loosen a higher layer's decision.
    WouldWiden { class: ActionClass, from: Verdict, to: Verdict },
}

impl std::fmt::Display for PolicyError {
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
pub struct Ruling {
    pub verdict: Verdict,
    /// The rule that fired, e.g. `builtin:out_of_bounds` or `session:out_of_bounds`.
    pub rule: String,
    pub reason: String,
}

pub struct Pdp {
    effective: Ladder,
    rules: [String; 3],
}

impl Default for Pdp {
    fn default() -> Self {
        Self::new()
    }
}

impl Pdp {
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

    fn slot(class: ActionClass) -> usize {
        match class {
            ActionClass::Investigate => 0,
            ActionClass::WriteWorkspace => 1,
            ActionClass::OutOfBounds => 2,
        }
    }

    pub fn ladder(&self) -> Ladder {
        self.effective
    }

    /// Tighten one action class. Refuses to widen (ADR-0008 monotonicity).
    pub fn tighten(&mut self, class: ActionClass, verdict: Verdict) -> Result<(), PolicyError> {
        self.tighten_from(class, verdict, "session")
    }

    /// Tighten and record **who** narrowed it, so the rule that fired names its
    /// source (ADR-0008 §4 provenance, ADR-0007 "the rule that fired").
    pub fn tighten_from(
        &mut self,
        class: ActionClass,
        verdict: Verdict,
        source: &str,
    ) -> Result<(), PolicyError> {
        let from = self.effective.get(class);
        if rank(verdict) < rank(from) {
            return Err(PolicyError::WouldWiden { class, from, to: verdict });
        }
        self.effective.set(class, verdict);
        self.rules[Self::slot(class)] = format!("{source}:{}", class_name(class));
        Ok(())
    }

    /// Decide an action. `action` is a human/model-readable label (usually the
    /// tool name) used only in the reason, never in the decision.
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
    fn defaults_are_junior_proof() {
        let p = Pdp::new();
        assert_eq!(p.ladder().investigate, Verdict::Allow);
        assert_eq!(p.ladder().write_workspace, Verdict::Allow);
        assert_eq!(p.ladder().out_of_bounds, Verdict::Ask, "out of bounds always asks");
    }

    #[test]
    fn a_lower_layer_may_tighten() {
        let mut p = Pdp::new();
        p.tighten(ActionClass::OutOfBounds, Verdict::Deny).unwrap();
        let r = p.decide(ActionClass::OutOfBounds, "curl");
        assert_eq!(r.verdict, Verdict::Deny);
        assert_eq!(r.rule, "session:out_of_bounds", "the rule that fired is recorded");
        assert!(r.reason.contains("out_of_bounds"));
    }

    #[test]
    fn a_lower_layer_may_not_widen() {
        let mut p = Pdp::new();
        p.tighten(ActionClass::OutOfBounds, Verdict::Deny).unwrap();
        let err = p.tighten(ActionClass::OutOfBounds, Verdict::Allow).unwrap_err();
        assert_eq!(
            err,
            PolicyError::WouldWiden {
                class: ActionClass::OutOfBounds,
                from: Verdict::Deny,
                to: Verdict::Allow
            }
        );
        assert_eq!(p.decide(ActionClass::OutOfBounds, "x").verdict, Verdict::Deny);
    }

    #[test]
    fn classes_are_independent() {
        let mut p = Pdp::new();
        p.tighten(ActionClass::WriteWorkspace, Verdict::Ask).unwrap();
        assert_eq!(p.decide(ActionClass::WriteWorkspace, "write_file").verdict, Verdict::Ask);
        assert_eq!(p.decide(ActionClass::Investigate, "read_file").verdict, Verdict::Allow);
    }

    #[test]
    fn tightening_to_the_same_verdict_is_allowed_and_renames_the_rule() {
        let mut p = Pdp::new();
        p.tighten(ActionClass::Investigate, Verdict::Allow).unwrap();
        assert_eq!(p.decide(ActionClass::Investigate, "read").rule, "session:investigate");
    }
}
