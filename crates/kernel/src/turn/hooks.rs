//! Purpose: Tighten a policy ruling with hook verdicts, and expose the hook set for status.
//!
//! Serves: hooks that can stop an action but never widen one, with the objecting hook
//! named in the ruling (ADR-0007 §7, ADR-0008).
//!
//! # Contract
//! - **Design:** `docs/design/kernel-loop.md` §3; `docs/design/tools-permissions.md`
//!   §3 owns the allow/ask/deny order.
//! - **Upstream:** `Kernel::dispatch` after the `pre_tool` hooks; the turn tests.
//! - **Downstream:** `HookRunner`, `HookReport`, `loong_sandbox::Ruling`.
//! - **Verify:** `cargo test -p loong-kernel turn::tests::a_pre_tool_hook_can_deny_what_the_policy_would_allow` · `./scripts/check.sh`.

use super::*;

/// Purpose: Let a hook's verdict replace the policy's only when it is stricter, naming the hook.
///
/// Serves: a guard that can stop an action but never widen one (ADR-0008).
/// Design: `docs/design/tools-permissions.md` §3.
/// Up: `Kernel::dispatch` after the `pre_tool` hooks; the turn tests.
/// Down: `HookReport::{tightening, verdict, reason}`, `loong_sandbox::Ruling` and `Verdict`.
/// Verify: `cargo test -p loong-kernel turn::tests::a_pre_tool_hook_can_deny_what_the_policy_would_allow`.
/// A hook verdict replaces the policy's only when it is stricter (hooks tighten,
/// never widen). The rule records which hook objected, so the log says why.
pub(super) fn tighten(
    ruling: &loong_sandbox::Ruling,
    report: &HookReport,
) -> Option<loong_sandbox::Ruling> {
    if !report.tightening() {
        return None;
    }
    let rank = |v: &Verdict| match v {
        Verdict::Allow => 0,
        Verdict::Ask => 1,
        Verdict::Deny => 2,
    };
    if rank(&report.verdict) <= rank(&ruling.verdict) {
        return None;
    }
    let ids: Vec<&str> = report
        .outcomes
        .iter()
        .filter(|o| rank(&o.verdict) == rank(&report.verdict))
        .map(|o| o.id.as_str())
        .collect();
    Some(loong_sandbox::Ruling {
        verdict: report.verdict,
        rule: format!("hook:{}", ids.join(",")),
        reason: report.reason(),
    })
}
impl Kernel {
    /// Purpose: The hook set's status note.
    ///
    /// Serves: a startup surface saying whether hooks are trusted, inert or malformed.
    /// Design: `docs/design/extension-model.md` §4.
    /// Up: the turn tests only; no production caller.
    /// Down: `Kernel.hooks_note`.
    /// Verify: `cargo test -p loong-kernel turn::tests::an_untrusted_hook_is_inert`.
    /// The hook set's status, for the startup log and `hooks list`.
    pub fn hooks_note(&self) -> &str {
        &self.hooks_note
    }

    /// Purpose: The hook runner this session holds.
    ///
    /// Serves: a caller inspecting which hooks are registered.
    /// Design: `docs/design/extension-model.md` §4.
    /// Up: no in-tree caller.
    /// Down: `Kernel.hooks`.
    /// Verify: `cargo test -p loong-kernel turn::tests`.
    pub fn hooks(&self) -> &HookRunner {
        &self.hooks
    }
}
