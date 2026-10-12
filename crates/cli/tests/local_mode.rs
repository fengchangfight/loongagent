//! `loongagent tui --local` (ADR-0009's surface, ADR-0008's layers): a mode that
//! removes the control plane rather than merely ignoring it.
//!
//! The tests here are black-box on purpose. "Does local mode skip the control plane?"
//! is a question about *observable behaviour* — whether a network call happens and
//! whether a prompt appears — not about which function ran. Pointing the control
//! plane at a closed port makes the answer falsifiable: if the control plane is
//! consulted, the run fails; if it is not, the run succeeds.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static N: AtomicU64 = AtomicU64::new(0);

fn workspace() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "loongagent-local-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("ws")).unwrap();
    dir
}

/// A home whose control plane is a closed port: reachable-looking, never reachable.
/// Port 1 is reserved and nothing listens there, so a sync cannot accidentally pass.
fn home_with_a_dead_control_plane() -> std::path::PathBuf {
    let dir = workspace();
    std::fs::write(
        dir.join("control.toml"),
        "url = \"http://127.0.0.1:1/\"\n\
         public_key = \"0000000000000000000000000000000000000000000000000000000000000000\"\n",
    )
    .unwrap();
    dir
}

fn run(home: &std::path::Path, args: &[&str]) -> (bool, String, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_loongagent"));
    command.env("LOONGAGENT_HOME", home);
    // The workspace of *this* repository has a `.env`, and a developer's shell may have
    // any of these exported. Either would let a test pass — or fail — for a reason that
    // has nothing to do with the code, and the names the local first run looks for are
    // exactly the ones most likely to be set.
    for provider in loong_config::catalog::PROVIDERS {
        command.env_remove(provider.key_env);
        for alias in provider.aliases {
            command.env_remove(alias);
        }
    }
    let out = command
        .args(args)
        .arg("--cwd")
        .arg(home.join("ws"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("the binary runs");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn the_control_plane_is_really_consulted_without_the_flag() {
    // The control half of the experiment. Without this, the `--local` test below
    // would pass just as well against a build that ignored the control plane always.
    let home = home_with_a_dead_control_plane();
    let (ok, _stdout, stderr) = run(&home, &["config", "show"]);
    assert!(
        !ok,
        "a dead control plane must fail a managed run, or the flag means nothing"
    );
    assert!(stderr.contains("unreachable"), "and say why: {stderr}");
}

#[test]
fn local_mode_never_touches_the_control_plane() {
    let home = home_with_a_dead_control_plane();
    std::fs::write(
        home.join("profile.toml"),
        "[[models]]\nname = \"cheap\"\nprovider = \"deepseek/deepseek-flash\"\n\
         [router]\ndefault = \"cheap\"\n",
    )
    .unwrap();

    let (ok, stdout, stderr) = run(&home, &["config", "show", "--local"]);
    assert!(
        ok,
        "the same dead control plane must not matter in local mode: {stderr}"
    );
    assert!(
        !stderr.contains("unreachable"),
        "nothing may have tried to reach it: {stderr}"
    );
    assert!(
        stdout.contains("mode") && stdout.contains("--local (this run)"),
        "the run says out loud which mode it is in: {stdout}"
    );
}

#[test]
fn local_mode_may_be_declared_in_the_profile_so_a_resume_keeps_it() {
    // `loongagent resume` re-enters with no memory of the original flag, so the
    // setting has to be able to live in a file.
    let home = home_with_a_dead_control_plane();
    std::fs::write(
        home.join("profile.toml"),
        "mode = \"local\"\n\n[[models]]\nname = \"cheap\"\n\
         provider = \"deepseek/deepseek-flash\"\n[router]\ndefault = \"cheap\"\n",
    )
    .unwrap();

    let (ok, stdout, stderr) = run(&home, &["config", "show"]);
    assert!(ok, "the profile alone is enough: {stderr}");
    assert!(
        stdout.contains("local"),
        "and the table attributes it to the file that said so: {stdout}"
    );
}

#[test]
fn a_provider_shorthand_reaches_the_command_line_without_an_endpoint() {
    let home = workspace();
    std::fs::write(
        home.join("profile.toml"),
        "mode = \"local\"\n\n[[models]]\nname = \"cheap\"\n\
         provider = \"deepseek/deepseek-flash\"\n\n[[models]]\nname = \"strong\"\n\
         provider = \"moonshot/kimi-k3\"\n\n[router]\ndefault = \"cheap\"\n",
    )
    .unwrap();

    let (ok, stdout, stderr) = run(&home, &["config", "show"]);
    assert!(ok, "{stderr}");
    assert!(
        stdout.contains("https://api.deepseek.com"),
        "the catalog supplied the endpoint: {stdout}"
    );
    assert!(
        stdout.contains("https://api.moonshot.cn/v1"),
        "and did so per model, not once: {stdout}"
    );
    assert!(
        stdout.contains("deepseek-flash") && stdout.contains("kimi-k3"),
        "the model ids came from the shorthand too: {stdout}"
    );
}

#[test]
fn a_local_first_run_with_no_key_refuses_cleanly_and_writes_nothing() {
    // No key anywhere, and no terminal to ask on. The failure must be the *terminal*,
    // said plainly — not a hang, and not half a configuration left on disk for the
    // next run to trip over.
    let home = workspace();
    let (ok, _stdout, stderr) = run(&home, &["tui", "--local"]);
    assert!(!ok, "a pipe is not a terminal to ask on");
    assert!(
        stderr.contains("terminal"),
        "and the reason is the terminal: {stderr}"
    );
    assert!(
        !home.join("profile.toml").exists(),
        "nothing is written when nothing was configured"
    );
    assert!(
        !home.join("env").exists(),
        "and no secret file is created for a key that was never given"
    );
}

#[test]
fn a_repository_may_not_declare_the_install_local() {
    // The same threat as a repo choosing its own endpoint: `mode = "local"` in a
    // checked-out file would switch a managed install out of its firm's policy.
    let home = workspace();
    std::fs::create_dir_all(home.join("ws/.loongagent")).unwrap();
    std::fs::write(
        home.join("ws/.loongagent/profile.toml"),
        "mode = \"local\"\n",
    )
    .unwrap();

    let (ok, _stdout, stderr) = run(&home, &["config", "show", "--local"]);
    assert!(!ok, "the project layer must not be able to say this");
    assert!(stderr.contains("managed"), "{stderr}");
}
