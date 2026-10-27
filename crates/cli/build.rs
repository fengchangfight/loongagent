//! Put the commit this binary was built from into `--version`, so two copies of a
//! program can be told apart — and say when the tree it came from was dirty, because
//! "am I running what I just wrote?" is the question this answers. A stale copy has
//! quietly run old rules here four times: the wrong issuer, a missing subcommand, a
//! refusal the new code accepts, and a long-running process holding all three.
//!
//! It also gives the Windows executable its icon. That is a target fact, not a host
//! one: a Linux build skips it entirely, while a Windows build (native or cross)
//! compiles `assets/loongagent.rc` with the resource compiler its toolchain carries.
fn main() {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let commit = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let dirty = git(&["status", "--porcelain"])
        .map(|out| !out.is_empty())
        .unwrap_or(false);
    println!(
        "cargo:rustc-env=LOONG_COMMIT={commit}{}",
        if dirty { "+dirty" } else { "" }
    );
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");

    // The icon belongs to the Windows PE resource table. `CARGO_CFG_TARGET_OS` is the
    // target's OS even when the build script itself runs on Linux, which is what makes
    // the cross build work; the host check would silently skip the icon there.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/loongagent.rc");
        println!("cargo:rerun-if-changed=assets/loongagent.ico");
        embed_resource::compile(
            "assets/loongagent.rc",
            embed_resource::ParamsIncludeDirs(&["assets"]),
        )
        .manifest_optional()
        .expect("the Windows icon resource compiles");
    }
}
