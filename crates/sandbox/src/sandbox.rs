//! The enforcement layer (ADR-0007): detect what the OS offers, **prove** it
//! works, and confine subprocesses with it.
//!
//! Two rules from the decision record are honoured here:
//!
//! * The PDP decides, the PEP enforces — this module is only the PEP.
//! * `sandbox self-test` runs **before** the first tool call, and it does not
//!   report "available": it runs a confined child that tries to write outside
//!   the workspace and checks that the write is refused. A mechanism that cannot
//!   demonstrate denial is reported as broken, not as present.

use crate::landlock;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Env var carrying the workspace root into the probe child.
pub const PROBE_WORKSPACE: &str = "LOONG_SANDBOX_WORKSPACE";
/// Env var carrying a path that must NOT be writable inside the probe.
pub const PROBE_OUTSIDE: &str = "LOONG_SANDBOX_OUTSIDE";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    pub os: &'static str,
    /// Landlock ABI version, when the kernel has it (>= 1).
    pub landlock_abi: Option<u32>,
    /// `bwrap` on PATH, if any: the fallback for kernels without Landlock.
    pub bubblewrap: Option<PathBuf>,
    pub note: String,
}

impl Capabilities {
    pub fn detect() -> Self {
        let landlock_abi = landlock::abi_version();
        let bubblewrap = which("bwrap");
        let note = match (landlock_abi, &bubblewrap) {
            (Some(v), _) => format!("landlock ABI {v} (unprivileged, inherited by children)"),
            (None, Some(_)) => {
                "no landlock; bubblewrap present (needs user namespaces)".to_string()
            }
            (None, None) => "no OS confinement available on this platform".to_string(),
        };
        Self {
            os: std::env::consts::OS,
            landlock_abi,
            bubblewrap,
            note,
        }
    }

    pub fn strongest(&self) -> &'static str {
        if self.landlock_abi.is_some() {
            "landlock"
        } else if self.bubblewrap.is_some() {
            "bubblewrap"
        } else {
            "none"
        }
    }
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelfTest {
    /// The probe could not write outside the workspace and could write inside.
    Enforced { mechanism: String, detail: String },
    /// No mechanism on this platform — the caller must say so out loud.
    Unavailable { detail: String },
    /// A mechanism exists but did not actually deny anything.
    Broken { detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfTestReport {
    pub capabilities: Capabilities,
    pub result: SelfTest,
}

impl SelfTestReport {
    pub fn ok(&self) -> bool {
        matches!(self.result, SelfTest::Enforced { .. })
    }
}

/// Run the self-test: a confined child attempts one allowed write and one
/// forbidden write, and only the expected outcomes count as success.
pub fn self_test(probe_exe: &Path, workspace: &Path) -> SelfTestReport {
    let capabilities = Capabilities::detect();
    if capabilities.strongest() == "none" {
        return SelfTestReport {
            capabilities,
            result: SelfTest::Unavailable {
                detail: "neither landlock nor bubblewrap is available".to_string(),
            },
        };
    }

    // The probe path must be *genuinely* outside the workspace. When the
    // workspace is the temp directory itself (or an ancestor of it), the obvious
    // candidate is inside it and a successful write there would look like an
    // escape. Pick the first candidate that is really outside.
    let Some(outside) = outside_candidate(workspace) else {
        return SelfTestReport {
            capabilities,
            result: SelfTest::Unavailable {
                detail: format!(
                    "no path outside {} is available to probe with (the workspace is the whole filesystem)",
                    workspace.display()
                ),
            },
        };
    };
    let _ = std::fs::remove_file(&outside);
    let child = Command::new(probe_exe)
        .arg("sandbox")
        .arg("probe")
        .env(PROBE_WORKSPACE, workspace)
        .env(PROBE_OUTSIDE, &outside)
        .output();

    match child {
        Err(e) => SelfTestReport {
            capabilities,
            result: SelfTest::Broken {
                detail: format!("could not run the probe: {e}"),
            },
        },
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let result = interpret(&capabilities, out.status.code(), &stderr, workspace);
            SelfTestReport {
                capabilities,
                result,
            }
        }
    }
}

/// A probe target that is definitely not inside `workspace`.
pub fn outside_candidate(workspace: &Path) -> Option<PathBuf> {
    let name = format!("loong-sandbox-probe-{}", std::process::id());
    let mut candidates = vec![
        std::env::temp_dir().join(&name),
        PathBuf::from("/var/tmp").join(&name),
    ];
    if let Some(parent) = workspace.parent() {
        candidates.push(parent.join(&name));
    }
    candidates.into_iter().find(|c| is_outside(workspace, c))
}

/// Lexical containment on canonicalised ancestors: `/tmpfoo` is not inside
/// `/tmp`, and a path whose parent does not exist yet still answers correctly.
pub fn is_outside(workspace: &Path, candidate: &Path) -> bool {
    let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let ws = canonical(workspace);
    let parent = candidate.parent().map(canonical).unwrap_or_default();
    let full = match candidate.file_name() {
        Some(name) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => candidate.to_path_buf(),
    };
    !full.starts_with(&ws)
}

/// The exit-code contract, isolated so it can be tested without a child.
pub fn interpret(
    capabilities: &Capabilities,
    code: Option<i32>,
    stderr: &str,
    workspace: &Path,
) -> SelfTest {
    match code {
        Some(0) => SelfTest::Enforced {
            mechanism: capabilities.strongest().to_string(),
            detail: format!(
                "a write outside {} was denied and a write inside it was allowed",
                workspace.display()
            ),
        },
        Some(2) => SelfTest::Unavailable {
            detail: format!("the probe could not apply confinement: {stderr}"),
        },
        Some(3) => SelfTest::Broken {
            detail: format!(
                "a write OUTSIDE the workspace succeeded, so the boundary is not enforced: {stderr}"
            ),
        },
        Some(4) => SelfTest::Broken {
            detail: format!(
                "even the allowed write inside the workspace failed, so confinement is too strict: {stderr}"
            ),
        },
        other => SelfTest::Broken {
            detail: format!("the probe exited with {other:?}: {stderr}"),
        },
    }
}

/// The child side of the self-test: confine this process, then prove both
/// directions. Exit codes: 0 enforced, 2 could not confine, 3 escaped,
/// 4 over-restricted.
pub fn probe_main() -> i32 {
    let (Some(workspace), Some(outside)) = (
        std::env::var_os(PROBE_WORKSPACE),
        std::env::var_os(PROBE_OUTSIDE),
    ) else {
        eprintln!("probe: {PROBE_WORKSPACE} and {PROBE_OUTSIDE} are required");
        return 2;
    };
    let workspace = PathBuf::from(workspace);
    let outside = PathBuf::from(outside);

    if let Err(e) = landlock::confine_workspace(&workspace) {
        eprintln!("probe: {e}");
        return 2;
    }

    let inside = workspace.join(".loongagent-sandbox-probe");
    if let Err(e) = std::fs::write(&inside, b"ok") {
        eprintln!("probe: the allowed write inside the workspace failed: {e}");
        return 4;
    }
    let _ = std::fs::remove_file(&inside);

    match std::fs::write(&outside, b"should not be possible") {
        Ok(_) => {
            let _ = std::fs::remove_file(&outside);
            eprintln!("probe: a write outside the workspace SUCCEEDED");
            3
        }
        Err(e) => {
            println!("denied as required: {e}");
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(abi: Option<u32>, bwrap: bool) -> Capabilities {
        Capabilities {
            os: "linux",
            landlock_abi: abi,
            bubblewrap: bwrap.then(|| PathBuf::from("/usr/bin/bwrap")),
            note: String::new(),
        }
    }

    #[test]
    fn landlock_wins_when_both_are_present() {
        assert_eq!(caps(Some(8), true).strongest(), "landlock");
        assert_eq!(caps(Some(1), false).strongest(), "landlock");
        assert_eq!(caps(None, true).strongest(), "bubblewrap");
        assert_eq!(caps(None, false).strongest(), "none");
    }

    #[test]
    fn detection_reports_a_consistent_mechanism() {
        let detected = Capabilities::detect();
        assert!(!detected.note.is_empty());
        if detected.landlock_abi.is_some() {
            assert_eq!(detected.strongest(), "landlock");
        }
        if let Some(abi) = detected.landlock_abi {
            assert!(abi >= 1);
        }
    }

    #[test]
    fn the_exit_codes_mean_what_the_self_test_says() {
        let c = caps(Some(8), false);
        let ws = Path::new("/work");
        assert!(matches!(
            interpret(&c, Some(0), "", ws),
            SelfTest::Enforced { .. }
        ));
        assert!(matches!(
            interpret(&c, Some(2), "no", ws),
            SelfTest::Unavailable { .. }
        ));
        match interpret(&c, Some(3), "escaped", ws) {
            SelfTest::Broken { detail } => assert!(detail.contains("OUTSIDE"), "{detail}"),
            other => panic!("expected Broken, got {other:?}"),
        }
        match interpret(&c, Some(4), "too strict", ws) {
            SelfTest::Broken { detail } => assert!(detail.contains("too strict"), "{detail}"),
            other => panic!("expected Broken, got {other:?}"),
        }
        assert!(matches!(
            interpret(&c, None, "killed", ws),
            SelfTest::Broken { .. }
        ));
    }

    #[test]
    fn the_probe_target_is_chosen_outside_the_workspace() {
        let temp = std::env::temp_dir();
        // A workspace inside the temp directory: its own temp sibling is outside.
        let nested = temp.join("loongagent-ws-test");
        assert!(is_outside(&nested, &temp.join("loong-sandbox-probe-1")));
        // A workspace that *is* the temp directory: the candidate inside it is
        // not acceptable, and a real alternative must be found.
        assert!(!is_outside(&temp, &temp.join("loong-sandbox-probe-1")));
        let chosen = outside_candidate(&temp).expect("somewhere outside the temp dir exists");
        assert!(
            is_outside(&temp, &chosen),
            "{} should be outside {}",
            chosen.display(),
            temp.display()
        );
        // Prefix traps: `/tmpfoo` is a different directory from `/tmp`.
        assert!(is_outside(Path::new("/tmp"), Path::new("/tmpfoo/x")));
        assert!(!is_outside(Path::new("/tmp"), Path::new("/tmp/x")));
        assert!(is_outside(Path::new("/tmp"), Path::new("/etc/x")));
    }

    #[test]
    fn a_platform_without_a_mechanism_reports_unavailable_not_enforced() {
        let report = SelfTestReport {
            capabilities: caps(None, false),
            result: SelfTest::Unavailable {
                detail: "none".into(),
            },
        };
        assert!(!report.ok(), "unavailable must never count as enforced");
        assert!(SelfTestReport {
            capabilities: caps(Some(8), false),
            result: SelfTest::Enforced {
                mechanism: "landlock".into(),
                detail: String::new()
            },
        }
        .ok());
    }
}
