//! Put the commit this binary was built from into `--version`, so two copies of a
//! program can be told apart — and say when the tree it came from was dirty, because
//! "am I running what I just wrote?" is the question this answers. A stale copy has
//! quietly run old rules here four times: the wrong issuer, a missing subcommand, a
//! refusal the new code accepts, and a long-running process holding all three.
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
    let dirty = git(&["status", "--porcelain"]).map(|out| !out.is_empty()).unwrap_or(false);
    println!(
        "cargo:rustc-env=LOONG_COMMIT={commit}{}",
        if dirty { "+dirty" } else { "" }
    );
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
}
