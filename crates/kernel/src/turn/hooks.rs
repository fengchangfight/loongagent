use super::*;

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
    /// The hook set's status, for the startup log and `hooks list`.
    pub fn hooks_note(&self) -> &str {
        &self.hooks_note
    }

    pub fn hooks(&self) -> &HookRunner {
        &self.hooks
    }
}
