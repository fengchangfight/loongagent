//! Purpose: `loongagent sandbox self-test`: prove the OS boundary instead of assuming it.
//!
//! # Contract
//! - **Serves:** the `sandbox self-test` command, the first-run proof ADR-0007 asks for.
//! - **Design:** `docs/design/surfaces.md` §2; `docs/design/tools-permissions.md` §4.
//! - **Upstream:** `main.rs`'s `sandbox` → `self-test` arm. The `probe` arm bypasses this file and calls `loong_sandbox::sandbox::probe_main` directly.
//! - **Downstream:** `loong_sandbox::sandbox::self_test`, which runs the confined child; the exit codes are this file's verdict.
//! - **Verify:** `cargo test -p loongagent --test sandbox_self_test` · `./scripts/check.sh`

use super::*;

/// Purpose: Print the strongest available mechanism and the demonstrated result, failing loudly when nothing denies.
/// Serves: `loongagent sandbox self-test`, the first-run proof ADR-0007 asks for.
/// Design: `docs/design/tools-permissions.md` §4.
/// Up: `main.rs`'s `sandbox` → `self-test` arm; no other caller.
/// Down: `loong_sandbox::sandbox::self_test` and `std::process::exit`.
/// Verify: `cargo test -p loongagent --test sandbox_self_test`.
/// ADR-0007: prove the boundary before the first tool call. "Available" is not
/// good enough — the probe must demonstrate that a forbidden write is denied.
pub(crate) fn sandbox_self_test(args: Args) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let report = loong_sandbox::sandbox::self_test(&exe, &args.cwd);
    println!("os:        {}", report.capabilities.os);
    println!(
        "mechanism: {} — {}",
        report.capabilities.strongest(),
        report.capabilities.note
    );
    match &report.result {
        loong_sandbox::SelfTest::Enforced { mechanism, detail } => {
            println!("result:    enforced by {mechanism}");
            println!("           {detail}");
            Ok(())
        }
        loong_sandbox::SelfTest::Unavailable { detail } => {
            eprintln!("result:    UNAVAILABLE — {detail}");
            eprintln!("           tool execution is not confined on this platform");
            std::process::exit(1)
        }
        loong_sandbox::SelfTest::Broken { detail } => {
            eprintln!("result:    BROKEN — {detail}");
            std::process::exit(2)
        }
    }
}
