//! Put the commit this binary was built from into `--version`, so two copies of a
//! program can be told apart. Twice already, a stale `~/.cargo/bin` copy silently ran
//! code from hours earlier — once publishing with the wrong issuer, once refusing a
//! pack the new rules accept.
fn main() {
    let commit = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=LOONG_COMMIT={commit}");
    println!("cargo:rerun-if-changed=.git/HEAD");
}
