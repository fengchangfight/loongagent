//! Purpose: The sandbox probe end to end: enforcement is demonstrated, and an absent or broken boundary is reported.
//!
//! # Contract
//! - **Serves:** ADR-0007's pre-flight proof before the first tool call.
//! - **Design:** `docs/design/surfaces.md` §2; `docs/design/tools-permissions.md` §4 and §6.
//! - **Upstream:** runs `loongagent sandbox self-test` and `probe`; nothing imports this file.
//! - **Downstream:** `loong_sandbox`'s self-test and probe, through the real binary.
//! - **Verify:** `cargo test -p loongagent --test sandbox_self_test`
//!
//! ADR-0007 requires `sandbox self-test` before the first tool call, and it
//! requires the answer to be *demonstrated*, not assumed: the confined child
//! must fail to write outside the workspace and succeed inside it.

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static N: AtomicU64 = AtomicU64::new(0);

/// Purpose: The supported platform's self-test demonstrates a denied write and exits zero.
#[test]
fn the_os_boundary_is_proven_not_assumed() {
    let dir = std::env::temp_dir().join(format!(
        "loongagent-sandbox-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_loongagent"))
        .arg("sandbox")
        .arg("self-test")
        .arg("--cwd")
        .arg(&dir)
        .output()
        .expect("the binary runs");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    // Linux with Landlock is the supported v1 target (macOS uses seatbelt).
    // If this fails on another platform, the platform needs its own PEP — the
    // message says exactly that rather than pretending the boundary exists.
    assert_eq!(
        out.status.code(),
        Some(0),
        "the sandbox self-test did not prove enforcement.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("enforced"), "stdout: {stdout}");
    assert!(stdout.contains("landlock"), "stdout: {stdout}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Purpose: When the workspace is the temp directory, the probe still picks a target genuinely outside it.
/// Regression: when the workspace *is* the temp directory, the probe target must
/// not be picked inside it — that made an allowed write look like an escape.
#[test]
fn a_workspace_at_the_temp_root_is_still_probed_honestly() {
    let dir = std::env::temp_dir();
    let out = Command::new(env!("CARGO_BIN_EXE_loongagent"))
        .arg("sandbox")
        .arg("self-test")
        .arg("--cwd")
        .arg(&dir)
        .output()
        .expect("the binary runs");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        !stderr.contains("OUTSIDE the workspace succeeded"),
        "the probe target was chosen inside the workspace:\n{stdout}\n{stderr}"
    );
    // On this platform an outside candidate exists (/var/tmp), so it must prove enforcement.
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("enforced"), "stdout: {stdout}");
}

/// Purpose: An unconfigured probe reports a required contract and never claims success.
#[test]
fn a_broken_boundary_would_be_reported_not_hidden() {
    // The probe refuses to run without its contract, and that is an
    // `Unavailable`-class answer (exit 2 from the probe), never a success.
    let out = Command::new(env!("CARGO_BIN_EXE_loongagent"))
        .arg("sandbox")
        .arg("probe")
        .output()
        .expect("the binary runs");
    assert_ne!(
        out.status.code(),
        Some(0),
        "an unconfigured probe must not claim success"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("required"), "stderr: {stderr}");
}
