//! Purpose: The non-Linux enforcement point: there is none, and this module says so instead of pretending.
//!
//! # Contract
//! - **Serves:** `Capabilities`, the probe and the kernel on platforms without Landlock.
//! - **Design:** `docs/design/tools-permissions.md` §4 (ADR-0007).
//! - **Upstream:** the kernel's build step and the sandbox probe, through the same module path as Linux.
//! - **Downstream:** `LandlockError::Unsupported` from every operation.
//! - **Verify:** `cargo test -p loong-sandbox` · `./scripts/check.sh`
//!
//! The non-Linux PEP: there isn't one, and this module says so instead of
//! pretending.
//!
//! Landlock is a Linux kernel interface. Windows and the BSDs have their own
//! primitives (AppContainer/Job Objects/ACLs, seatbelt, capsicum), and none of
//! them is wired up yet. Rather than let the workspace fail to *build* on those
//! platforms, this stub keeps the same shape as [`crate::landlock`] and answers
//! `Unsupported` for every operation.
//!
//! The consequence is deliberate and visible, not silent: the kernel builds with
//! [`crate::sandbox::Capabilities::strongest`] = `"none"`, its confinement note
//! reads `unconfined: …`, `loongagent sandbox self-test` exits non-zero, and the
//! server prints `[sandbox] WARNING: unconfined` before the first tool call. Tools
//! still run — the PDP still decides — but the kernel is *not* the boundary here,
//! and every surface that can say so does.
//!
//! Implementing a real PEP for Windows is tracked in
//! `docs/runbooks/windows-build.md`; until then this file is the honest answer.

use std::path::Path;

/// Mirrors the Linux error type so callers need no `cfg` of their own.
#[derive(Debug)]
/// Purpose: Mirrors the Linux error type so callers need no `cfg` of their own.
/// Serves: `WorkspacePolicy` and `confine_workspace` on platforms without Landlock.
/// Design: `docs/design/tools-permissions.md` §4.
/// Up: `WorkspacePolicy::new`/`apply` and `confine_workspace`.
/// Down: its `Display` impl.
/// Verify: `cargo test -p loong-sandbox`.
pub enum LandlockError {
    /// This platform has no Landlock at all.
    Unsupported,
    /// Kept for signature parity with the Linux implementation.
    Failed(String),
}

impl std::fmt::Display for LandlockError {
    /// Purpose: Explain that the OS boundary is absent, not silently accept it.
    /// Contract: `crates/sandbox/src/landlock_unsupported.rs`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LandlockError::Unsupported => write!(
                f,
                "landlock is a Linux mechanism and the OS boundary on this platform \
                 is not implemented yet (the policy still decides; nothing enforces it)"
            ),
            LandlockError::Failed(what) => write!(f, "{what}"),
        }
    }
}

impl std::error::Error for LandlockError {}

/// Always `None`: there is no Landlock ABI to report off Linux.
/// Purpose: Always `None`: there is no Landlock ABI off Linux.
/// Serves: `Capabilities::detect` and the self-test's "unavailable" answer.
/// Design: `docs/design/tools-permissions.md` §4.
/// Up: `Capabilities::detect` and tests.
/// Down: no callee.
/// Verify: `cargo test -p loong-sandbox`.
pub fn abi_version() -> Option<u32> {
    None
}

/// A workspace policy. Constructing one always fails here, which is what makes
/// the kernel fall back to [`crate::sandbox`]'s "unconfined" answer.
#[derive(Clone)]
/// Purpose: A workspace policy that cannot be constructed, making the unconfined fallback explicit.
/// Serves: the kernel's build on Windows and the BSDs.
/// Design: `docs/design/tools-permissions.md` §4.
/// Up: the kernel's build step.
/// Down: `new` and `apply`.
/// Verify: `cargo test -p loong-sandbox`.
pub struct WorkspacePolicy;

impl WorkspacePolicy {
    /// Purpose: Refuse to prepare a policy on a platform with no mechanism.
    /// Serves: the kernel's fallback to `NoConfinement`.
    /// Design: `docs/design/tools-permissions.md` §4.
    /// Up: the kernel's build step and tests.
    /// Down: `LandlockError::Unsupported`.
    /// Verify: `cargo test -p loong-sandbox`.
    pub fn new(_workspace: &Path) -> Result<Self, LandlockError> {
        Err(LandlockError::Unsupported)
    }

    /// Purpose: Refuse to apply a policy that was never constructed.
    /// Serves: signature parity with the Linux implementation.
    /// Design: `docs/design/tools-permissions.md` §4.
    /// Up: no in-tree caller; `WorkspacePolicy::new` already fails first.
    /// Down: `LandlockError::Unsupported`.
    /// Verify: `cargo test -p loong-sandbox`.
    pub fn apply(&self) -> Result<(), LandlockError> {
        Err(LandlockError::Unsupported)
    }
}

/// Confine the calling process — impossible here, and reported as such so the
/// `sandbox probe` child exits with the "could not confine" code rather than
/// claiming success.
/// Purpose: Tell the probe that confinement is impossible here instead of claiming success.
/// Serves: `sandbox::probe_main`'s "could not confine" exit code.
/// Design: `docs/design/tools-permissions.md` §4.
/// Up: `sandbox::probe_main` and tests.
/// Down: `LandlockError::Unsupported`.
/// Verify: `cargo test -p loong-sandbox`.
pub fn confine_workspace(_workspace: &Path) -> Result<(), LandlockError> {
    Err(LandlockError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Purpose: Pin that every operation answers unsupported with an honest message.
    fn a_platform_without_landlock_says_so_rather_than_guessing() {
        assert_eq!(abi_version(), None);
        assert!(WorkspacePolicy::new(Path::new(".")).is_err());
        assert!(confine_workspace(Path::new(".")).is_err());
        assert!(LandlockError::Unsupported
            .to_string()
            .contains("not implemented yet"));
    }
}
