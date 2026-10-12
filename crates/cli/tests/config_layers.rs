//! The configuration surface (ADR-0008): a repository may only narrow, and every
//! value says which layer set it.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static N: AtomicU64 = AtomicU64::new(0);

/// Each test gets its own directory: cargo runs them in parallel, and a shared
/// path means one test's profile file is another test's surprise.
fn workspace() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "loongagent-config-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join(".loongagent")).unwrap();
    dir
}

fn run(dir: &std::path::Path, args: &[&str]) -> (bool, String, String) {
    run_env(dir, args, &[])
}

fn run_env(dir: &std::path::Path, args: &[&str], envs: &[(&str, &str)]) -> (bool, String, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_loongagent"));
    // Hermetic: `~/.loongagent` is the developer's own install, and a test that reads it
    // passes or fails according to what they happen to have configured. Set first so a
    // test that means to move the home still can.
    command.env("LOONGAGENT_HOME", dir);
    for (key, value) in envs {
        command.env(key, value);
    }
    let out = command
        .args(args)
        .arg("--cwd")
        .arg(dir)
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
fn config_show_names_the_layer_that_set_each_value() {
    let dir = workspace();
    std::fs::write(
        dir.join(".loongagent/profile.toml"),
        "budget_tokens = 60000\n\n[permissions]\nwrite_workspace = \"ask\"\n",
    )
    .unwrap();
    let session = dir.join("session.toml");
    std::fs::write(&session, "budget_tokens = 900000\n").unwrap();

    let (ok, stdout, stderr) = run(
        &dir,
        &["config", "show", "--profile", session.to_str().unwrap()],
    );
    assert!(ok, "{stderr}");
    assert!(stdout.contains("project"), "the layer list: {stdout}");
    assert!(stdout.contains("narrow-only"), "{stdout}");
    assert!(
        stdout.contains("60000") && !stdout.contains("900000"),
        "the project ceiling is the value in force: {stdout}"
    );

    let (ok, stdout, stderr) = run(
        &dir,
        &[
            "config",
            "explain",
            "budget_tokens",
            "--profile",
            session.to_str().unwrap(),
        ],
    );
    assert!(ok, "{stderr}");
    assert!(stdout.contains("budget_tokens = 60000"), "{stdout}");
    assert!(stdout.contains("also set by (and lost)"), "{stdout}");
    assert!(
        stdout.contains("session") && stdout.contains("900000"),
        "{stdout}"
    );
}

#[test]
fn a_repository_may_not_choose_where_prompts_go() {
    let dir = workspace();
    std::fs::write(
        dir.join(".loongagent/profile.toml"),
        "base_url = \"http://elsewhere.invalid/v1\"\n",
    )
    .unwrap();
    let (ok, _, stderr) = run(&dir, &["run", "hi", "--model", "echo"]);
    assert!(!ok, "the session must refuse to start: {stderr}");
    assert!(
        stderr.contains("may not set `base_url`") && stderr.contains("ADR-0008"),
        "the refusal names the rule: {stderr}"
    );
}

#[test]
fn a_repository_may_not_put_a_secret_in_config_at_all() {
    let dir = workspace();
    std::fs::write(
        dir.join(".loongagent/profile.toml"),
        "api_key = \"sk-live-abc\"\n",
    )
    .unwrap();
    let (ok, _, stderr) = run(&dir, &["run", "hi", "--model", "echo"]);
    assert!(!ok);
    assert!(
        stderr.contains("may not set `api_key`"),
        "a repository may not choose the credential either: {stderr}"
    );
}

#[test]
fn a_local_layer_may_not_put_a_literal_secret_in_config() {
    let dir = workspace();
    // The user layer lives directly in the client's one root, which the test owns.
    let home = dir.join("loongagent-home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("profile.toml"), "api_key = \"sk-live-abc\"\n").unwrap();
    let (ok, _, stderr) = run_env(
        &dir,
        &["run", "hi", "--model", "echo"],
        &[("LOONGAGENT_HOME", home.to_str().unwrap())],
    );
    assert!(!ok);
    assert!(
        stderr.contains("secrets never live in config"),
        "the refusal names the rule: {stderr}"
    );
}

#[test]
fn a_typo_in_a_policy_file_is_not_silently_ignored() {
    let dir = workspace();
    std::fs::write(dir.join(".loongagent/profile.toml"), "budget_token = 100\n").unwrap();
    let (ok, _, stderr) = run(&dir, &["config", "show"]);
    assert!(!ok, "a mistyped key must not look like it worked");
    assert!(stderr.contains("budget_token"), "{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}
