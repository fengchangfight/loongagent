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
    fn syscall(num: c_long, ...) -> c_long;
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
struct RulesetAttr {
    handled_access_fs: u64,
}

#[repr(C)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

#[derive(Debug)]
pub enum LandlockError {
    /// The kernel has no Landlock (or it is disabled).
    Unsupported,
    /// A syscall failed; the payload is `name: errno`.
    Failed(String),
}

impl std::fmt::Display for LandlockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LandlockError::Unsupported => write!(f, "landlock is not available on this kernel"),
            LandlockError::Failed(what) => write!(f, "{what}"),
        }
    }
}

impl std::error::Error for LandlockError {}

fn errno(what: &str) -> LandlockError {
    let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    LandlockError::Failed(format!("{what} failed (errno {code})"))
}

/// The Landlock ABI version, or `None` when the kernel does not support it.
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
pub struct WorkspacePolicy {
    root: std::ffi::CString,
}

impl WorkspacePolicy {
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
    fn the_abi_probe_is_well_behaved() {
        match abi_version() {
            Some(v) => assert!(v >= 1, "a reported ABI is at least 1, got {v}"),
            None => { /* older kernels are fine; the self-test reports it */ }
        }
    }

    #[test]
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
    fn a_policy_for_a_missing_directory_is_refused() {
        assert!(matches!(
            WorkspacePolicy::new(Path::new("/definitely/not/here")),
            Err(LandlockError::Failed(_)) | Err(LandlockError::Unsupported)
        ));
    }

    #[test]
    fn confining_a_missing_directory_is_refused() {
        assert!(matches!(
            confine_workspace(Path::new("/definitely/not/here")),
            Err(LandlockError::Failed(_)) | Err(LandlockError::Unsupported)
        ));
    }
}
