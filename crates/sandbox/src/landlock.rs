//! Purpose: Landlock, the Linux enforcement point that grants writes only under the workspace.
//!
//! # Contract
//! - **Serves:** `sandbox self-test` and the kernel's `LandlockConfinement`.
//! - **Design:** `docs/design/tools-permissions.md` §4–§5 (ADR-0007).
//! - **Upstream:** the kernel's build step through `WorkspacePolicy`; the probe through `confine_workspace`.
//! - **Downstream:** the raw Landlock syscalls and the masks this module installs.
//! - **Verify:** `cargo test -p loong-sandbox` · `./scripts/check.sh`
//!
//! Landlock confinement — the Linux PEP (ADR-0007: the decision is in the
//! kernel, the enforcement is at the OS).
//!
//! Landlock is the right primitive here: it needs **no privileges**, it is
//! kernel-native, and it is **inherited by child processes** — so confining the
//! current process confines every subprocess a tool spawns.
//!
//! The rules this module installs: read and execute everywhere, **write only
//! under the workspace root**. Everything else in the handled set is denied by
//! omission, which is what makes the boundary structural rather than advisory.

use std::ffi::c_long;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;

// --- syscalls (generic numbering; identical on x86_64 and aarch64) ----------
const SYS_LANDLOCK_CREATE_RULESET: c_long = 444;
const SYS_LANDLOCK_ADD_RULE: c_long = 445;
const SYS_LANDLOCK_RESTRICT_SELF: c_long = 446;
const SYS_CLOSE: c_long = 3;
/// `openat` is the one arch-dependent number we need; the two arches we target
/// are covered, and anywhere else the policy reports `Unsupported` rather than
/// guessing.
#[cfg(target_arch = "x86_64")]
const SYS_OPENAT: c_long = 257;
#[cfg(target_arch = "aarch64")]
const SYS_OPENAT: c_long = 56;
const AT_FDCWD: i32 = -100;
const PR_SET_NO_NEW_PRIVS: std::ffi::c_int = 38;

const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1;
const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;

extern "C" {
    /// Purpose: Declare the libc `syscall` entry point used for every Landlock operation.
    fn syscall(num: c_long, ...) -> c_long;
    /// Purpose: Declare `prctl`, used to set `NO_NEW_PRIVS` before restricting the process.
    fn prctl(option: std::ffi::c_int, ...) -> std::ffi::c_int;
}

// --- rights (Documentation/userspace-api/landlock.rst) ---------------------
const EXECUTE: u64 = 1 << 0;
const WRITE_FILE: u64 = 1 << 1;
const READ_FILE: u64 = 1 << 2;
const READ_DIR: u64 = 1 << 3;
const REMOVE_DIR: u64 = 1 << 4;
const REMOVE_FILE: u64 = 1 << 5;
const MAKE_CHAR: u64 = 1 << 6;
const MAKE_DIR: u64 = 1 << 7;
const MAKE_REG: u64 = 1 << 8;
const MAKE_SOCK: u64 = 1 << 9;
const MAKE_FIFO: u64 = 1 << 10;
const MAKE_BLOCK: u64 = 1 << 11;
const MAKE_SYM: u64 = 1 << 12;
const REFER: u64 = 1 << 13; // ABI >= 2
const TRUNCATE: u64 = 1 << 14; // ABI >= 3
const IOCTL_DEV: u64 = 1 << 15; // ABI >= 5

// O_PATH and O_CLOEXEC, spelled out because we take no libc dependency.
const O_PATH: i32 = 0o10000000;
const O_CLOEXEC: i32 = 0o2000000;

#[repr(C)]
/// Purpose: The `landlock_create_ruleset` argument: the filesystem access to mediate.
/// Serves: `WorkspacePolicy::apply` and `confine_workspace`.
/// Design: `docs/design/tools-permissions.md` §4.
/// Up: those two functions.
/// Down: `SYS_LANDLOCK_CREATE_RULESET`.
/// Verify: `cargo test -p loong-sandbox`.
struct RulesetAttr {
    handled_access_fs: u64,
}

#[repr(C)]
/// Purpose: The `landlock_add_rule` argument: a parent file descriptor and the rights granted beneath it.
/// Serves: `add_path_rule_raw` and `add_path_rule`.
/// Design: `docs/design/tools-permissions.md` §4.
/// Up: `add_path_rule_raw` and `add_path_rule`.
/// Down: `SYS_LANDLOCK_ADD_RULE`.
/// Verify: `cargo test -p loong-sandbox`.
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

#[derive(Debug)]
/// Purpose: Why a Landlock operation could not be applied.
/// Serves: `WorkspacePolicy` and `confine_workspace`, and the kernel's unconfined fallback.
/// Design: `docs/design/tools-permissions.md` §4.
/// Up: `WorkspacePolicy::new`/`apply`, `confine_workspace` and the kernel's build.
/// Down: its `Display` impl and `errno`.
/// Verify: `cargo test -p loong-sandbox`.
pub enum LandlockError {
    /// The kernel has no Landlock (or it is disabled).
    Unsupported,
    /// A syscall failed; the payload is `name: errno`.
    Failed(String),
}

impl std::fmt::Display for LandlockError {
    /// Purpose: Render the reason a Landlock operation failed.
    /// Contract: `crates/sandbox/src/landlock.rs`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LandlockError::Unsupported => write!(f, "landlock is not available on this kernel"),
            LandlockError::Failed(what) => write!(f, "{what}"),
        }
    }
}

impl std::error::Error for LandlockError {}

/// Purpose: The last OS error as a named Landlock failure.
/// Contract: `crates/sandbox/src/landlock.rs`.
fn errno(what: &str) -> LandlockError {
    let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    LandlockError::Failed(format!("{what} failed (errno {code})"))
}

/// The Landlock ABI version, or `None` when the kernel does not support it.
/// Purpose: The Landlock ABI version, or `None` when the kernel has none.
/// Serves: `Capabilities::detect`, `WorkspacePolicy` and the probe.
/// Design: `docs/design/tools-permissions.md` §4.
/// Up: `Capabilities::detect`, `WorkspacePolicy::new`/`apply` and `confine_workspace`.
/// Down: `SYS_LANDLOCK_CREATE_RULESET`.
/// Verify: `cargo test -p loong-sandbox`.
pub fn abi_version() -> Option<u32> {
    let r = unsafe {
        syscall(
            SYS_LANDLOCK_CREATE_RULESET,
            std::ptr::null::<RulesetAttr>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if r < 0 {
        None
    } else {
        Some(r as u32)
    }
}

/// The rights we ask the kernel to *mediate*. Anything in this mask that no
/// rule grants is denied; rights outside it keep their normal behaviour.
/// Purpose: The filesystem rights Landlock is asked to mediate for this ABI.
/// Serves: `apply` and `confine_workspace`; anything handled and ungranted is denied.
/// Design: `docs/design/tools-permissions.md` §4.
/// Up: `WorkspacePolicy::apply` and `confine_workspace`, and the ABI test.
/// Down: the right constants gated by ABI version.
/// Verify: `cargo test -p loong-sandbox`.
pub fn handled_mask(abi: u32) -> u64 {
    let mut m = EXECUTE
        | WRITE_FILE
        | READ_FILE
        | READ_DIR
        | REMOVE_DIR
        | REMOVE_FILE
        | MAKE_CHAR
        | MAKE_DIR
        | MAKE_REG
        | MAKE_SOCK
        | MAKE_FIFO
        | MAKE_BLOCK
        | MAKE_SYM;
    if abi >= 2 {
        m |= REFER;
    }
    if abi >= 3 {
        m |= TRUNCATE;
    }
    if abi >= 5 {
        m |= IOCTL_DEV;
    }
    m
}

/// What is permitted under a read-only path (everything else is denied).
/// Purpose: What a read-only path grants: read and execute, plus device ioctl from ABI 5.
/// Serves: the `/` rule and the workspace rule's base.
/// Design: `docs/design/tools-permissions.md` §4.
/// Up: `WorkspacePolicy::apply`, `confine_workspace` and `read_write_mask`.
/// Down: the right constants.
/// Verify: `cargo test -p loong-sandbox`.
pub fn read_only_mask(abi: u32) -> u64 {
    let mut m = EXECUTE | READ_FILE | READ_DIR;
    if abi >= 5 {
        // Device ioctls are how ordinary programs talk to their terminal; the
        // boundary we care about is file writes, not tty control.
        m |= IOCTL_DEV;
    }
    m
}

/// What is permitted under the workspace root.
/// Purpose: What the workspace root grants: the read-only set plus writes, removal and creation.
/// Serves: the one writable path in the policy.
/// Design: `docs/design/tools-permissions.md` §4.
/// Up: `WorkspacePolicy::apply` and `confine_workspace`.
/// Down: `read_only_mask`.
/// Verify: `cargo test -p loong-sandbox`.
pub fn read_write_mask(abi: u32) -> u64 {
    let mut m = read_only_mask(abi)
        | WRITE_FILE
        | REMOVE_DIR
        | REMOVE_FILE
        | MAKE_DIR
        | MAKE_REG
        | MAKE_SOCK
        | MAKE_FIFO
        | MAKE_SYM;
    if abi >= 2 {
        m |= REFER; // renames and links *within* the workspace
    }
    if abi >= 3 {
        m |= TRUNCATE;
    }
    m
}

/// A workspace policy **prepared in the parent** so a forked child can apply it
/// without allocating (only async-signal-safe work may happen between `fork` and
/// `exec`, and `malloc` after a fork in a threaded process can deadlock).
#[derive(Clone)]
/// Purpose: A workspace policy prepared in the parent so a forked child can apply it without allocating.
/// Serves: the kernel's `LandlockConfinement`.
/// Design: `docs/design/tools-permissions.md` §4–§5.
/// Up: the kernel's build step.
/// Down: `new`, `apply` and `add_path_rule_raw`.
/// Verify: `cargo test -p loong-sandbox`.
pub struct WorkspacePolicy {
    root: std::ffi::CString,
}

impl WorkspacePolicy {
    /// Purpose: Prepare the workspace policy, or say why this kernel cannot.
    /// Serves: the kernel's choice between Landlock and `NoConfinement`.
    /// Design: `docs/design/tools-permissions.md` §4.
    /// Up: the kernel's build step and tests.
    /// Down: `abi_version` and the workspace directory check.
    /// Verify: `cargo test -p loong-sandbox`.
    pub fn new(workspace: &Path) -> Result<Self, LandlockError> {
        if abi_version().is_none() {
            return Err(LandlockError::Unsupported);
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            return Err(LandlockError::Failed(
                "no openat syscall number for this architecture".to_string(),
            ));
        }
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        {
            if !workspace.is_dir() {
                return Err(LandlockError::Failed(format!(
                    "workspace {} is not a directory",
                    workspace.display()
                )));
            }
            use std::os::unix::ffi::OsStrExt;
            let root = std::ffi::CString::new(workspace.as_os_str().as_bytes())
                .map_err(|_| LandlockError::Failed("workspace path contains a NUL".to_string()))?;
            Ok(Self { root })
        }
    }

    /// Apply the policy to the calling process. Intended for `pre_exec`: the
    /// success path performs no allocation.
    /// Purpose: Apply the policy to the calling process, for use in `pre_exec`.
    /// Serves: the kernel's confinement callback before the child execs.
    /// Design: `docs/design/tools-permissions.md` §4–§5.
    /// Up: the kernel's `LandlockConfinement`; no other caller.
    /// Down: `handled_mask`, the two masks, `add_path_rule_raw` and `prctl`.
    /// Verify: `cargo test -p loong-sandbox`.
    pub fn apply(&self) -> Result<(), LandlockError> {
        let abi = abi_version().ok_or(LandlockError::Unsupported)?;
        let attr = RulesetAttr {
            handled_access_fs: handled_mask(abi),
        };
        let ruleset = unsafe {
            syscall(
                SYS_LANDLOCK_CREATE_RULESET,
                &attr as *const RulesetAttr,
                std::mem::size_of::<RulesetAttr>(),
                0u32,
            )
        };
        if ruleset < 0 {
            return Err(errno("landlock_create_ruleset"));
        }
        let result = (|| {
            add_path_rule_raw(ruleset as i32, c"/", read_only_mask(abi))?;
            add_path_rule_raw(ruleset as i32, &self.root, read_write_mask(abi))?;
            if unsafe { prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
                return Err(errno("prctl(PR_SET_NO_NEW_PRIVS)"));
            }
            if unsafe { syscall(SYS_LANDLOCK_RESTRICT_SELF, ruleset, 0u32) } != 0 {
                return Err(errno("landlock_restrict_self"));
            }
            Ok(())
        })();
        unsafe {
            syscall(SYS_CLOSE, ruleset as i32);
        }
        result
    }
}

/// Add a path-beneath rule using raw syscalls only.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
/// Purpose: Add one path-beneath rule using raw syscalls only.
/// Serves: `WorkspacePolicy::apply`; the success path must not allocate between fork and exec.
/// Design: `docs/design/tools-permissions.md` §4.
/// Up: `WorkspacePolicy::apply`.
/// Down: `SYS_OPENAT`, `SYS_LANDLOCK_ADD_RULE` and `errno`.
/// Verify: `cargo test -p loong-sandbox`.
fn add_path_rule_raw(
    ruleset_fd: i32,
    path: &std::ffi::CStr,
    allowed: u64,
) -> Result<(), LandlockError> {
    let fd = unsafe {
        syscall(
            SYS_OPENAT,
            AT_FDCWD,
            path.as_ptr(),
            O_PATH | O_CLOEXEC,
            0u32,
        )
    };
    if fd < 0 {
        return Err(errno("openat"));
    }
    let attr = PathBeneathAttr {
        allowed_access: allowed,
        parent_fd: fd as i32,
    };
    let result = unsafe {
        syscall(
            SYS_LANDLOCK_ADD_RULE,
            ruleset_fd,
            LANDLOCK_RULE_PATH_BENEATH,
            &attr as *const PathBeneathAttr,
            0u32,
        )
    };
    unsafe {
        syscall(SYS_CLOSE, fd as i32);
    }
    if result != 0 {
        return Err(errno("landlock_add_rule"));
    }
    Ok(())
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
/// Purpose: The unsupported-architecture stub: report that no `openat` number is known.
/// Contract: `crates/sandbox/src/landlock.rs`.
fn add_path_rule_raw(
    _ruleset_fd: i32,
    _path: &std::ffi::CStr,
    _allowed: u64,
) -> Result<(), LandlockError> {
    Err(LandlockError::Failed(
        "no openat syscall number for this architecture".to_string(),
    ))
}

/// Confine **this process** (and, by inheritance, every child it spawns) to
/// read/execute everywhere and write only under `workspace`.
///
/// Irreversible by design: Landlock restrictions cannot be lifted once applied,
/// which is exactly why the self-test runs it in a child process.
/// Purpose: Confine this process, and by inheritance every child, to write only under the workspace.
/// Serves: the `sandbox probe` child; the restriction is irreversible by design.
/// Design: `docs/design/tools-permissions.md` §4.
/// Up: `sandbox::probe_main`.
/// Down: `abi_version`, `handled_mask`, the two masks, `add_path_rule` and `prctl`.
/// Verify: `cargo test -p loong-sandbox`.
pub fn confine_workspace(workspace: &Path) -> Result<(), LandlockError> {
    let abi = abi_version().ok_or(LandlockError::Unsupported)?;
    if !workspace.is_dir() {
        return Err(LandlockError::Failed(format!(
            "workspace {} is not a directory",
            workspace.display()
        )));
    }

    let attr = RulesetAttr {
        handled_access_fs: handled_mask(abi),
    };
    let ruleset = unsafe {
        syscall(
            SYS_LANDLOCK_CREATE_RULESET,
            &attr as *const RulesetAttr,
            std::mem::size_of::<RulesetAttr>(),
            0u32,
        )
    };
    if ruleset < 0 {
        return Err(errno("landlock_create_ruleset"));
    }

    let result = (|| {
        add_path_rule(ruleset as i32, Path::new("/"), read_only_mask(abi))?;
        add_path_rule(ruleset as i32, workspace, read_write_mask(abi))?;
        if unsafe { prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
            return Err(errno("prctl(PR_SET_NO_NEW_PRIVS)"));
        }
        if unsafe { syscall(SYS_LANDLOCK_RESTRICT_SELF, ruleset, 0u32) } != 0 {
            return Err(errno("landlock_restrict_self"));
        }
        Ok(())
    })();

    unsafe {
        syscall(SYS_CLOSE, ruleset as i32);
    }
    result
}

/// Purpose: Add one path rule through the standard library's file handle.
/// Serves: `confine_workspace`.
/// Design: `docs/design/tools-permissions.md` §4.
/// Up: `confine_workspace`.
/// Down: `SYS_LANDLOCK_ADD_RULE` and `errno`.
/// Verify: `cargo test -p loong-sandbox`.
fn add_path_rule(ruleset_fd: i32, path: &Path, allowed: u64) -> Result<(), LandlockError> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(O_PATH | O_CLOEXEC)
        .open(path)
        .map_err(|e| LandlockError::Failed(format!("opening {}: {e}", path.display())))?;
    let attr = PathBeneathAttr {
        allowed_access: allowed,
        parent_fd: file.as_raw_fd(),
    };
    let r = unsafe {
        syscall(
            SYS_LANDLOCK_ADD_RULE,
            ruleset_fd,
            LANDLOCK_RULE_PATH_BENEATH,
            &attr as *const PathBeneathAttr,
            0u32,
        )
    };
    if r != 0 {
        return Err(errno("landlock_add_rule"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Purpose: Pin that a reported ABI is at least 1 and an absent one is allowed.
    fn the_abi_probe_is_well_behaved() {
        match abi_version() {
            Some(v) => assert!(v >= 1, "a reported ABI is at least 1, got {v}"),
            None => { /* older kernels are fine; the self-test reports it */ }
        }
    }

    #[test]
    /// Purpose: Pin the ABI-gated right constants and their monotonic growth.
    fn newer_abis_widen_the_handled_set_but_never_narrow_it() {
        let a1 = handled_mask(1);
        let a2 = handled_mask(2);
        let a3 = handled_mask(3);
        let a5 = handled_mask(5);
        assert_eq!(a1 & REFER, 0, "REFER arrives with ABI 2");
        assert_eq!(a1 & TRUNCATE, 0, "TRUNCATE arrives with ABI 3");
        assert_eq!(a1 & IOCTL_DEV, 0, "IOCTL_DEV arrives with ABI 5");
        assert_eq!(a2 & REFER, REFER);
        assert_eq!(a3 & TRUNCATE, TRUNCATE);
        assert_eq!(a5 & IOCTL_DEV, IOCTL_DEV);
        assert_eq!(a1, a2 & !REFER);
        assert_eq!(a2, a3 & !TRUNCATE);
        assert_eq!(a3, a5 & !IOCTL_DEV);
    }

    #[test]
    /// Purpose: Pin the read-only and workspace-rights masks.
    fn the_workspace_may_write_and_the_rest_of_the_filesystem_may_not() {
        let ro = read_only_mask(8);
        let rw = read_write_mask(8);
        assert_eq!(ro & WRITE_FILE, 0, "read-only paths cannot be written");
        assert_eq!(ro & REMOVE_FILE, 0, "…nor have files removed");
        assert_eq!(ro & MAKE_REG, 0, "…nor have files created");
        assert_eq!(rw & WRITE_FILE, WRITE_FILE);
        assert_eq!(rw & REMOVE_FILE, REMOVE_FILE);
        assert_eq!(
            rw & TRUNCATE,
            TRUNCATE,
            "shell redirection must work inside the workspace"
        );
        assert_eq!(rw & REFER, REFER, "renames inside the workspace must work");
        assert_eq!(
            ro & READ_FILE,
            READ_FILE,
            "reading stays allowed everywhere"
        );
        assert_eq!(rw & EXECUTE, EXECUTE);
    }

    #[test]
    /// Purpose: Pin that a policy can be prepared and cloned in the parent.
    fn a_policy_is_prepared_in_the_parent() {
        let dir = std::env::temp_dir();
        match WorkspacePolicy::new(&dir) {
            Ok(policy) => {
                // Cloning must be allocation-safe in the parent (it is), and the
                // clone must be usable in the child.
                let _clone = policy.clone();
            }
            Err(LandlockError::Unsupported) => { /* older kernels: reported elsewhere */ }
            Err(e) => panic!(
                "unexpected failure preparing a policy for {}: {e}",
                dir.display()
            ),
        }
    }

    #[test]
    /// Purpose: Pin that a policy for a missing workspace is refused, not guessed.
    fn a_policy_for_a_missing_directory_is_refused() {
        assert!(matches!(
            WorkspacePolicy::new(Path::new("/definitely/not/here")),
            Err(LandlockError::Failed(_)) | Err(LandlockError::Unsupported)
        ));
    }

    #[test]
    /// Purpose: Pin that confining a missing workspace is refused, not guessed.
    fn confining_a_missing_directory_is_refused() {
        assert!(matches!(
            confine_workspace(Path::new("/definitely/not/here")),
            Err(LandlockError::Failed(_)) | Err(LandlockError::Unsupported)
        ));
    }
}
