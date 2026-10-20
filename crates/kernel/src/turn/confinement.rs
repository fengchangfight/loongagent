//! Purpose: The PEP for spawned processes: install the workspace policy in the forked child, and expose the PDP and the confinement the kernel chose.
//!
//! Serves: a tool child that cannot write outside the workspace even when the action
//! heuristic was wrong (ADR-0007 §4).
//!
//! # Contract
//! - **Design:** `docs/design/tools-permissions.md` §4–§5.
//! - **Upstream:** `Kernel::execute` builds `ExecContext` from the kernel; the turn
//!   tests inspect the resulting note.
//! - **Downstream:** `loong_sandbox::landlock::WorkspacePolicy`, `Command::pre_exec`
//!   and `loong_tools::Confinement`.
//! - **Verify:** `cargo test -p loong-kernel turn::tests::the_pep_stops_an_escape_the_classifier_cannot_see` · `./scripts/check.sh`.

use super::*;

impl Confinement for LandlockConfinement {
    /// Purpose: Install the workspace policy in a forked child just before exec.
    ///
    /// Serves: a confinement that is irreversible and inherited by the child, never the server.
    /// Design: `docs/design/tools-permissions.md` §4–§5.
    /// Up: `Kernel::execute` builds `ExecContext` with this `Confinement`; `loong_proc` applies it.
    /// Down: `WorkspacePolicy::apply` through `Command::pre_exec`.
    /// Verify: `cargo test -p loong-kernel turn::tests::the_pep_stops_an_escape_the_classifier_cannot_see`.
    fn confine(&self, command: &mut Command) {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let policy = self.policy.clone();
            // SAFETY: `apply` is documented to do no allocation on its success
            // path, which is what makes it safe between fork and exec.
            unsafe {
                command.pre_exec(move || {
                    policy.apply().map_err(|e| {
                        std::io::Error::new(std::io::ErrorKind::PermissionDenied, e.to_string())
                    })
                });
            }
        }
        #[cfg(not(unix))]
        let _ = command;
    }
}
impl Kernel {
    /// Purpose: The policy decision point, for a caller that needs to narrow it.
    ///
    /// Serves: a session or a test tightening the ladder before a decision.
    /// Design: `docs/design/tools-permissions.md` §3.
    /// Up: the turn tests only; no production caller (`Kernel::build` tightens through the config path).
    /// Down: `Pdp`.
    /// Verify: `cargo test -p loong-kernel turn::tests::a_tightened_policy_denies_without_asking`.
    pub fn pdp_mut(&mut self) -> &mut Pdp {
        &mut self.pdp
    }

    /// Purpose: What confinement is actually in force, for diagnostics.
    ///
    /// Serves: an operator seeing `unconfined:` instead of assuming a sandbox exists.
    /// Design: `docs/design/tools-permissions.md` §4.
    /// Up: the turn tests only; no production caller (the startup log reports the sandbox self-test).
    /// Down: `Kernel.confinement_note`.
    /// Verify: `cargo test -p loong-kernel turn::tests::the_pep_stops_an_escape_the_classifier_cannot_see`.
    /// What confinement is in force for spawned processes.
    pub fn confinement_note(&self) -> &str {
        &self.confinement_note
    }

    /// Purpose: Replace the confinement applied to every process a tool spawns.
    ///
    /// Serves: a caller or test swapping the PEP without rebuilding the kernel.
    /// Design: `docs/design/tools-permissions.md` §4.
    /// Up: no in-tree caller.
    /// Down: `Kernel.confinement`; `Kernel::execute` reads it on every call.
    /// Verify: `cargo test -p loong-kernel turn::tests`.
    /// Replace the confinement policy applied to every process a tool spawns.
    pub fn set_confinement(&mut self, confinement: Box<dyn Confinement>) {
        self.confinement = confinement;
    }
}
