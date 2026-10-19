//! Purpose: The enforcement probe: detect which OS mechanism exists and prove it denies an outside write.
//!
//! # Contract
//! - **Serves:** `sandbox self-test`, the server's startup report and the kernel's confinement choice.
//! - **Design:** `docs/design/tools-permissions.md` §4 (ADR-0007).
//! - **Upstream:** the CLI's sandbox command, the server's startup and the CLI's `probe` subcommand.
//! - **Downstream:** `landlock` and the probe child's exit-code contract.
//! - **Verify:** `cargo test -p loong-sandbox` · `./scripts/check.sh`
//!
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
/// Purpose: What this platform offers for confinement: Landlock ABI, bubblewrap and a note.
/// Serves: the server's startup line and the self-test's verdict.
/// Design: `docs/design/tools-permissions.md` §4.
/// Up: `Capabilities::detect`; held by `SelfTestReport`.
/// Down: `strongest`, `which` and `landlock::abi_version`.
/// Verify: `cargo test -p loong-sandbox`.
pub struct Capabilities {
    pub os: &'static str,
    /// Landlock ABI version, when the kernel has it (>= 1).
    pub landlock_abi: Option<u32>,
    /// `bwrap` on PATH, if any: the fallback for kernels without Landlock.
    pub bubblewrap: Option<PathBuf>,
    pub note: String,
}

impl Capabilities {
    /// Purpose: Detect the strongest confinement mechanism this machine actually has.
    /// Serves: `self_test` and `loongagent sandbox self-test`.
    /// Design: `docs/design/tools-permissions.md` §4.
    /// Up: `self_test`; the CLI reads the report.
    /// Down: `landlock::abi_version` and `which`.
    /// Verify: `cargo test -p loong-sandbox`.
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

    /// Purpose: Name the strongest mechanism, or `"none"`.
    /// Serves: the self-test's early exit, the startup warning and `interpret`.
    /// Design: `docs/design/tools-permissions.md` §4.
    /// Up: `self_test`, `interpret`, the CLI and tests.
    /// Down: the `landlock_abi` and `bubblewrap` fields.
    /// Verify: `cargo test -p loong-sandbox`.
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

/// Purpose: Find an executable on `PATH`.
/// Contract: `crates/sandbox/src/sandbox.rs`.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Purpose: The three honest outcomes of a confinement probe.
/// Serves: `SelfTestReport` and the server's warning.
/// Design: `docs/design/tools-permissions.md` §4.
/// Up: `self_test` and `interpret`.
/// Down: `SelfTestReport::ok` and the CLI's match.
/// Verify: `cargo test -p loong-sandbox`.
pub enum SelfTest {
    /// The probe could not write outside the workspace and could write inside.
    Enforced { mechanism: String, detail: String },
    /// No mechanism on this platform — the caller must say so out loud.
    Unavailable { detail: String },
    /// A mechanism exists but did not actually deny anything.
    Broken { detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Purpose: What the probe found: the capabilities and the outcome.
/// Serves: `sandbox self-test` and the server's startup report.
/// Design: `docs/design/tools-permissions.md` §4.
/// Up: `self_test`.
/// Down: `ok` and the CLI's printers.
/// Verify: `cargo test -p loong-sandbox`.
pub struct SelfTestReport {
    pub capabilities: Capabilities,
    pub result: SelfTest,
}

impl SelfTestReport {
    /// Purpose: Whether the probe actually demonstrated enforcement.
    /// Serves: a caller that must not treat unavailable or broken as present.
    /// Design: `docs/design/tools-permissions.md` §4.
    /// Up: its own unit test; the CLI matches on `result` directly.
    /// Down: `SelfTest`.
    /// Verify: `cargo test -p loong-sandbox`.
    pub fn ok(&self) -> bool {
        matches!(self.result, SelfTest::Enforced { .. })
    }
}

/// Run the self-test: a confined child attempts one allowed write and one
/// forbidden write, and only the expected outcomes count as success.
/// Purpose: Run the probe child and turn its exit code into an honest report.
/// Serves: `sandbox self-test`, before the first tool call.
/// Design: `docs/design/tools-permissions.md` §4.
/// Up: the CLI's sandbox command and the server's startup.
/// Down: `Capabilities::detect`, `outside_candidate`, `interpret` and the probe child.
/// Verify: `cargo test -p loong-sandbox`.
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
/// Purpose: A probe target that is definitely outside the workspace.
/// Serves: `self_test`; a workspace at the temp root must not make an inside write look like an escape.
/// Design: `docs/design/tools-permissions.md` §4.
/// Up: `self_test` and the probe-target test.
/// Down: `is_outside`.
/// Verify: `cargo test -p loong-sandbox`.
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
/// Purpose: Lexical containment on canonicalised ancestors, so `/tmpfoo` is not inside `/tmp`.
/// Serves: `outside_candidate` and the probe target's safety.
/// Design: `docs/design/tools-permissions.md` §4.
/// Up: `outside_candidate` and tests.
/// Down: `std::fs::canonicalize`.
/// Verify: `cargo test -p loong-sandbox`.
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
/// Purpose: Turn the probe's exit code into enforced, unavailable or broken.
/// Serves: `self_test`; it keeps "could not confine" distinct from "confined but leaked".
/// Design: `docs/design/tools-permissions.md` §4.
/// Up: `self_test` and the exit-code test.
/// Down: `Capabilities::strongest`.
/// Verify: `cargo test -p loong-sandbox`.
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
/// Purpose: The child side of the self-test: confine this process, then prove both directions.
/// Serves: `loongagent sandbox probe`.
/// Design: `docs/design/tools-permissions.md` §4.
/// Up: the CLI's `sandbox probe` subcommand.
/// Down: `landlock::confine_workspace` and the two probe environment variables.
/// Verify: `cargo test -p loong-sandbox`.
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

    /// Purpose: Build a capability set for an exit-code test without detecting anything.
    /// Contract: `crates/sandbox/src/sandbox.rs`.
    fn caps(abi: Option<u32>, bwrap: bool) -> Capabilities {
        Capabilities {
            os: "linux",
            landlock_abi: abi,
            bubblewrap: bwrap.then(|| PathBuf::from("/usr/bin/bwrap")),
            note: String::new(),
        }
    }

    #[test]
    /// Purpose: Pin the mechanism ranking: landlock, then bubblewrap, then none.
    fn landlock_wins_when_both_are_present() {
        assert_eq!(caps(Some(8), true).strongest(), "landlock");
        assert_eq!(caps(Some(1), false).strongest(), "landlock");
        assert_eq!(caps(None, true).strongest(), "bubblewrap");
        assert_eq!(caps(None, false).strongest(), "none");
    }

    #[test]
    /// Purpose: Pin that detection reports a mechanism consistent with its own note and ABI.
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
    /// Purpose: Pin every probe exit code to its verdict, including a crash as broken.
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
    /// Purpose: Pin the outside-target choice, including the `/tmpfoo` prefix trap.
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
    /// Purpose: Pin that unavailable and enforced are different answers.
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
