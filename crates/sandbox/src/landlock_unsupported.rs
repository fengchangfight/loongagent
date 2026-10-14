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
pub enum LandlockError {
    /// This platform has no Landlock at all.
    Unsupported,
    /// Kept for signature parity with the Linux implementation.
    Failed(String),
}

impl std::fmt::Display for LandlockError {
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
pub fn abi_version() -> Option<u32> {
    None
}

/// A workspace policy. Constructing one always fails here, which is what makes
/// the kernel fall back to [`crate::sandbox`]'s "unconfined" answer.
#[derive(Clone)]
pub struct WorkspacePolicy;

impl WorkspacePolicy {
    pub fn new(_workspace: &Path) -> Result<Self, LandlockError> {
        Err(LandlockError::Unsupported)
    }

    pub fn apply(&self) -> Result<(), LandlockError> {
        Err(LandlockError::Unsupported)
    }
}

/// Confine the calling process — impossible here, and reported as such so the
/// `sandbox probe` child exits with the "could not confine" code rather than
/// claiming success.
pub fn confine_workspace(_workspace: &Path) -> Result<(), LandlockError> {
    Err(LandlockError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_platform_without_landlock_says_so_rather_than_guessing() {
        assert_eq!(abi_version(), None);
        assert!(WorkspacePolicy::new(Path::new(".")).is_err());
        assert!(confine_workspace(Path::new(".")).is_err());
        assert!(LandlockError::Unsupported
            .to_string()
            .contains("not implemented yet"));
    }
}
