use super::*;

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
