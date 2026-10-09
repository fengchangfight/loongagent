//! The signing key is the one file in this system that must never be committed: a key
//! in a repository is an off-site ability to mint packs that every install accepts.
//! An administrator runs `keygen` wherever they happen to be, so these are the checks
//! that stand between a convenient directory and a leaked signing key.

use std::path::{Path, PathBuf};
use std::process::Command;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_loongagent-control")
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "loong-git-guard-{}-{}-{name}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn git(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run(args: &[&str]) -> (bool, String) {
    let output = Command::new(binary()).args(args).output().unwrap();
    (
        output.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

#[test]
fn keygen_refuses_to_write_a_signing_key_inside_a_repository() {
    let repo = temp("keygen-in-repo");
    if !git(&repo, &["init", "-q", "."]) {
        eprintln!("git is unavailable; skipping");
        return;
    }
    let (ok, output) = run(&["keygen", "--out", repo.to_str().unwrap()]);
    assert!(!ok, "it must refuse rather than warn: {output}");
    assert!(output.contains("git work tree"), "{output}");
    assert!(
        output.contains("--force"),
        "and it must say how to override: {output}"
    );
    assert!(
        output.contains("--out /etc/loongagent-control"),
        "and where to put it: {output}"
    );
    assert!(
        !repo.join("control-key.pkcs8").exists(),
        "the refusal must leave nothing behind"
    );

    // Twice told: it writes, and says what to ignore.
    let (ok, output) = run(&["keygen", "--out", repo.to_str().unwrap(), "--force"]);
    assert!(ok, "--force must work: {output}");
    assert!(repo.join("control-key.pkcs8").exists());
    assert!(output.contains("warning"), "{output}");
    assert!(output.contains(".gitignore"), "and name the fix: {output}");

    std::fs::remove_dir_all(&repo).ok();
}

#[test]
fn keygen_outside_a_repository_is_undisturbed() {
    let dir = temp("keygen-plain");
    let (ok, output) = run(&["keygen", "--out", dir.to_str().unwrap()]);
    assert!(ok, "{output}");
    assert!(dir.join("control-key.pkcs8").exists());
    assert!(
        !output.contains("git work tree"),
        "no repository, no lecture: {output}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn publishing_refuses_while_a_secret_is_tracked_beside_the_draft() {
    let repo = temp("publish-leak");
    if !git(&repo, &["init", "-q", "."]) {
        eprintln!("git is unavailable; skipping");
        return;
    }
    let pack = repo.join("profiles.toml");
    std::fs::write(
        &pack,
        "default_profile = \"default\"\n[[profiles]]\nname = \"default\"\ndescription = \"d\"\n  \
         [[profiles.models]]\n  name = \"m\"\n  base_url = \"https://a.invalid\"\n  model = \"i\"\n",
    )
    .unwrap();
    // The mistake this check exists for: a `.env` that got committed next to the draft.
    std::fs::write(repo.join(".env"), "DEEPSEEK_API_KEY=sk-not-a-real-key\n").unwrap();
    git(&repo, &["add", "-f", ".env"]);

    let (ok, output) = run(&[
        "publish",
        "--url",
        "http://127.0.0.1:9",
        "--pack",
        pack.to_str().unwrap(),
        "--token-file",
        "/dev/null",
    ]);
    assert!(!ok, "it must refuse: {output}");
    assert!(output.contains("already tracked by git"), "{output}");
    assert!(
        output.contains("git rm --cached"),
        "and give the fix: {output}"
    );

    std::fs::remove_dir_all(&repo).ok();
}

#[test]
fn an_unignored_secret_is_a_warning_not_a_wall() {
    let repo = temp("publish-warn");
    if !git(&repo, &["init", "-q", "."]) {
        eprintln!("git is unavailable; skipping");
        return;
    }
    let pack = repo.join("profiles.toml");
    std::fs::write(
        &pack,
        "default_profile = \"default\"\n[[profiles]]\nname = \"default\"\ndescription = \"d\"\n  \
         [[profiles.models]]\n  name = \"m\"\n  base_url = \"https://a.invalid\"\n  model = \"i\"\n",
    )
    .unwrap();
    // Untracked, but one `git add -A` from being tracked.
    std::fs::write(repo.join("client.token"), "ct-abcdef\n").unwrap();

    let (ok, output) = run(&[
        "publish",
        "--url",
        "http://127.0.0.1:9",
        "--pack",
        pack.to_str().unwrap(),
        "--token-file",
        "/dev/null",
    ]);
    assert!(!ok, "the publish itself still fails (nothing is listening)");
    assert!(
        output.contains("one `git add -A` from a leak"),
        "but the warning names the trap: {output}"
    );
    assert!(output.contains("client.token"), "{output}");

    std::fs::remove_dir_all(&repo).ok();
}

#[test]
fn a_draft_with_only_references_publishes_without_a_word_about_secrets() {
    let repo = temp("publish-clean");
    if !git(&repo, &["init", "-q", "."]) {
        eprintln!("git is unavailable; skipping");
        return;
    }
    let pack = repo.join("profiles.toml");
    std::fs::write(
        &pack,
        "default_profile = \"default\"\n[[profiles]]\nname = \"default\"\ndescription = \"d\"\n  \
         [[profiles.models]]\n  name = \"m\"\n  base_url = \"https://a.invalid\"\n  model = \"i\"\n  \
         api_key = \"env:DEEPSEEK_API_KEY\"\n",
    )
    .unwrap();
    std::fs::write(repo.join(".gitignore"), ".env\n").unwrap();
    std::fs::write(repo.join(".env"), "DEEPSEEK_API_KEY=sk-not-a-real-key\n").unwrap();

    let (_, output) = run(&[
        "publish",
        "--url",
        "http://127.0.0.1:9",
        "--pack",
        pack.to_str().unwrap(),
        "--token-file",
        "/dev/null",
    ]);
    assert!(
        !output.contains("git"),
        "an ignored secret and a clean draft: silence\n{output}"
    );
    std::fs::remove_dir_all(&repo).ok();
}
