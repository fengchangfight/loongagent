//! Purpose: Plugins end to end: one command puts skills in the catalogue, the always-on skill reaches the prompt, and a broken install leaves nothing.
//!
//! # Contract
//! - **Serves:** ADR-0018's install contract, observed through the real binary and a model.
//! - **Design:** `docs/design/surfaces.md` §2; `docs/design/extension-model.md` §4 and §7.
//! - **Upstream:** runs `CARGO_BIN_EXE_loongagent`; nothing imports this file.
//! - **Downstream:** `plugin add|list|remove`, `skills list`, and `run` against a recording HTTP model.
//! - **Verify:** `cargo test -p loongagent --test plugins`
//!
//! Plugins, end to end, against the real binary (ADR-0018).
//!
//! The claim being tested is not "a directory appeared under `plugins/`". It is that
//! one command puts a plugin's skills into the catalogue a session loads, that the
//! skill a plugin marks *always on* reaches the prompt the model is actually given,
//! and that an install which would break the catalogue leaves nothing behind. The
//! first two are only observable through a model, so these tests start one.

use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static N: AtomicU64 = AtomicU64::new(0);

/// Purpose: A fresh directory for a home, a workspace or a plugin checkout.
/// Contract: `plugins.rs` fixtures; every test calls it.
fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "loongagent-plugin-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Purpose: Write a skill file in the shape the plugin ecosystem uses.
/// Contract: `plugins.rs` fixtures; the install tests call it.
/// A skill file, in the shape the Claude Code plugin ecosystem uses.
fn skill(checkout: &Path, name: &str, body: &str) {
    let dir = checkout.join("skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\ndescription: does {name}\n---\n\n{body}\n"),
    )
    .unwrap();
}

/// Purpose: Write a workspace skill, so a plugin can collide with something real.
/// Contract: `plugins.rs` fixtures; the clash test calls it.
/// A workspace skill, so a plugin can collide with something real.
fn workspace_skill(ws: &Path, name: &str) {
    let dir = ws.join(".loongagent/skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\ndescription: the workspace's own {name}\n---\nbody\n"),
    )
    .unwrap();
}

/// Purpose: Run the binary against a test-owned home and workspace, with extra environment for the model.
/// Serves: every test in this file.
/// Design: `docs/design/extension-model.md` §7.
/// Up: `run` and the install tests; the recording-model test calls it directly.
/// Down: `std::process::Command` and the environment it sets.
/// Verify: `cargo test -p loongagent --test plugins`.
fn run_in(home: &Path, ws: &Path, args: &[&str], env: &[(&str, &str)]) -> (bool, String, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_loongagent"));
    // A home of its own: the developer's real `~/.loongagent` has plugins in it, and
    // an install that leaked into it would be a bug this test could not see.
    command.env("LOONGAGENT_HOME", home).env("HOME", home);
    for (key, value) in env {
        command.env(key, value);
    }
    let out = command
        .args(args)
        .arg("--cwd")
        .arg(ws)
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

/// Purpose: The same run with no extra environment.
/// Contract: `plugins.rs` fixtures; most tests call it.
fn run(home: &Path, ws: &Path, args: &[&str]) -> (bool, String, String) {
    run_in(home, ws, args, &[])
}

/// Purpose: A one-answer model that hands the test the request body it was sent.
/// Serves: the test that proves the always-on skill reaches the prompt.
/// Design: `docs/design/config-routing.md` §8.
/// Up: the install test; no other caller.
/// Down: `std::net::TcpListener` and a reader thread.
/// Verify: `cargo test -p loongagent --test plugins`.
/// A model that answers once, and hands the test the request body it was sent.
fn recording_model() -> (String, std::sync::mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut raw = Vec::new();
            let mut buffer = [0u8; 4096];
            let mut wanted = usize::MAX;
            loop {
                let Ok(read) = stream.read(&mut buffer) else {
                    break;
                };
                if read == 0 {
                    break;
                }
                raw.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&raw).to_string();
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    if wanted == usize::MAX {
                        wanted = head
                            .lines()
                            .find_map(|l| l.strip_prefix("Content-Length: "))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                    }
                    if body.len() >= wanted {
                        let _ = tx.send(body.to_string());
                        let reply = r#"{"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#;
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            reply.len()
                        );
                        let _ = stream.write_all(head.as_bytes());
                        let _ = stream.write_all(reply.as_bytes());
                        let _ = stream.flush();
                        break;
                    }
                }
            }
        }
    });
    (format!("http://127.0.0.1:{port}/v1"), rx)
}

/// Purpose: The system message a recorded request carried.
/// Contract: `plugins.rs` fixture; the install test calls it.
/// The system message the model was sent.
fn system_message(body: &str) -> String {
    let value: Value = serde_json::from_str(body).expect("the request is JSON");
    value["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|m| m["role"] == "system")
        .and_then(|m| m["content"].as_str())
        .expect("a system message")
        .to_string()
}

/// Purpose: One command installs a plugin, its skills join the catalogue, the always-on body reaches the model's prompt, and removing it takes the skills away.
#[test]
fn one_command_installs_a_plugin_and_its_always_on_skill_reaches_the_model() {
    let home = temp_dir("home");
    let ws = temp_dir("ws");
    // A checkout named after a known plugin, which is what makes the catalogue's
    // bootstrap answer apply: `plugin add /path/to/superpowers`.
    let source = temp_dir("src").join("superpowers");
    std::fs::create_dir_all(&source).unwrap();
    skill(&source, "alpha", "ALWAYS-CHECK-FOR-A-SKILL");
    skill(&source, "using-superpowers", "ALWAYS-CHECK-FOR-A-SKILL");

    let (ok, out, err) = run(&home, &ws, &["plugin", "add", source.to_str().unwrap()]);
    assert!(ok, "install failed:\n{out}\n{err}");
    assert!(out.contains("superpowers"), "{out}");
    assert!(out.contains("always on: using-superpowers"), "{out}");
    assert!(
        out.contains("/alpha"),
        "the skills it added are listed: {out}"
    );
    assert!(
        !out.contains("hooks/"),
        "this fixture ships no hooks, so nothing should say it does: {out}"
    );

    // The catalogue a session loads now holds them.
    let (ok, out, err) = run(&home, &ws, &["skills", "list"]);
    assert!(ok, "{err}");
    assert!(out.contains("/alpha — does alpha"), "{out}");
    assert!(out.contains("installed plugin(s): superpowers"), "{out}");

    // …and the prompt the model is *given* carries the always-on body, not just its
    // description. This is the half of Superpowers its SessionStart hook performs on
    // other harnesses.
    let (base_url, requests) = recording_model();
    let (ok, out, err) = run_in(
        &home,
        &ws,
        &["run", "hello", "--model", "openai-compat", "--local"],
        &[
            ("LOONGAGENT_BASE_URL", &base_url),
            ("LOONGAGENT_MODEL", "fake"),
            ("LOONGAGENT_API_KEY", "test"),
        ],
    );
    assert!(ok, "the run failed:\n{out}\n{err}");
    let request = requests
        .recv_timeout(Duration::from_secs(30))
        .expect("the model was called");
    let system = system_message(&request);
    assert!(
        system.contains("ALWAYS-CHECK-FOR-A-SKILL"),
        "the always-on body is in the prompt: {system}"
    );
    assert!(
        system.contains("# Skill: using-superpowers (from"),
        "and it names the file it came from: {system}"
    );
    assert!(
        system.contains("/alpha — does alpha"),
        "the catalogue is there too: {system}"
    );
    assert!(
        system.contains("superpowers:<name>"),
        "the adaptation note travelled with it: {system}"
    );

    // Removing the plugin takes its skills out of the next session.
    let (ok, out, err) = run(&home, &ws, &["plugin", "remove", "superpowers"]);
    assert!(ok, "{out}{err}");
    let (ok, out, _) = run(&home, &ws, &["skills", "list"]);
    assert!(ok);
    assert!(!out.contains("/alpha"), "{out}");
    assert!(
        out.contains("/help"),
        "the workspace's own skills remain: {out}"
    );
}

/// Purpose: An install that would make two skills share a name fails and leaves nothing behind.
#[test]
fn an_install_that_would_break_the_catalogue_is_rolled_back() {
    let home = temp_dir("home");
    let ws = temp_dir("ws");
    workspace_skill(&ws, "alpha");
    let source = temp_dir("src").join("clash");
    std::fs::create_dir_all(&source).unwrap();
    // The same name the workspace already defines: one of them has to lose, and a
    // session that cannot start is not an acceptable way to find out.
    skill(&source, "alpha", "a second alpha");

    let (ok, out, err) = run(&home, &ws, &["plugin", "add", source.to_str().unwrap()]);
    assert!(!ok, "a clashing install must fail:\n{out}\n{err}");
    let said = format!("{out}{err}");
    assert!(said.contains("defined twice"), "{said}");
    assert!(said.contains("rolled back"), "{said}");

    let (ok, out, _) = run(&home, &ws, &["plugin", "list"]);
    assert!(ok);
    assert!(
        out.contains("no plugins installed"),
        "nothing may be left installed: {out}"
    );
}

/// Purpose: A checkout whose skill has no frontmatter is refused and not installed.
#[test]
fn a_plugin_whose_skills_do_not_parse_is_not_installed() {
    let home = temp_dir("home");
    let ws = temp_dir("ws");
    let source = temp_dir("src").join("broken");
    std::fs::create_dir_all(source.join("skills/bad")).unwrap();
    std::fs::write(source.join("skills/bad/SKILL.md"), "no frontmatter\n").unwrap();

    let (ok, out, err) = run(&home, &ws, &["plugin", "add", source.to_str().unwrap()]);
    assert!(!ok, "{out}{err}");
    assert!(format!("{out}{err}").contains("frontmatter"), "{out}{err}");
    let (_, out, _) = run(&home, &ws, &["plugin", "list"]);
    assert!(out.contains("no plugins installed"), "{out}");
}

/// Purpose: An unknown plugin name names the known ones and the other accepted forms.
#[test]
fn an_unknown_name_says_which_names_are_known() {
    let home = temp_dir("home");
    let ws = temp_dir("ws");
    let (ok, out, err) = run(&home, &ws, &["plugin", "add", "not-a-plugin"]);
    assert!(!ok, "{out}{err}");
    let said = format!("{out}{err}");
    assert!(
        said.contains("superpowers"),
        "the known ones are named: {said}"
    );
    assert!(said.contains("owner/repo"), "{said}");
}

/// Purpose: A plugin whose skills disappear stops a session loudly, and `plugin remove` is still the way out.
#[test]
fn a_manifest_that_stops_making_sense_stops_a_session_and_can_be_removed() {
    let home = temp_dir("home");
    let ws = temp_dir("ws");
    let source = temp_dir("src").join("decays");
    std::fs::create_dir_all(&source).unwrap();
    skill(&source, "alpha", "body");
    let (ok, _, err) = run(&home, &ws, &["plugin", "add", source.to_str().unwrap()]);
    assert!(ok, "{err}");

    // Somebody deleted the checkout's skills directory out from under the manifest.
    let skills_dir = home.join("plugins/decays/skills");
    assert!(
        skills_dir.is_dir(),
        "the install landed under the test's home"
    );
    std::fs::remove_dir_all(&skills_dir).unwrap();

    let (ok, out, err) = run(&home, &ws, &["skills", "list"]);
    assert!(!ok, "a plugin that contributes nothing must not be silent");
    assert!(
        format!("{out}{err}").contains("not a directory"),
        "{out}{err}"
    );

    // The way out is one command, and it works even though the manifest is unusable.
    let (ok, out, err) = run(&home, &ws, &["plugin", "remove", "decays"]);
    assert!(ok, "{out}{err}");
    let (ok, _, err) = run(&home, &ws, &["skills", "list"]);
    assert!(ok, "{err}");
}

/// Purpose: When both scopes hold the same plugin, a bare name is refused and the two scope flags pick the copy.
#[test]
fn a_shadowed_name_says_which_copy_is_meant() {
    let home = temp_dir("shadow-home");
    let ws = temp_dir("shadow-ws");
    let source = temp_dir("shadow-src").join("twice");
    std::fs::create_dir_all(&source).unwrap();
    skill(&source, "alpha", "body");

    // The same name in both scopes: the project copy shadows the user one (ADR-0018 §1).
    let (ok, _, err) = run(&home, &ws, &["plugin", "add", source.to_str().unwrap()]);
    assert!(ok, "{err}");
    let (ok, _, err) = run(
        &home,
        &ws,
        &["plugin", "add", source.to_str().unwrap(), "--project"],
    );
    assert!(ok, "{err}");

    let user_dir = home.join("plugins/twice");
    let project_dir = ws.join(".loongagent/plugins/twice");
    assert!(
        user_dir.is_dir() && project_dir.is_dir(),
        "both are installed"
    );

    // Unqualified it refuses and names both. Before this, `remove` silently took the
    // shadowing copy, so the one `plugin list` reported as hidden could not be removed.
    let (ok, out, err) = run(&home, &ws, &["plugin", "remove", "twice"]);
    assert!(!ok, "an ambiguous name must not pick one:\n{out}{err}");
    assert!(err.contains("--project"), "{err}");
    assert!(err.contains("--user"), "{err}");
    assert!(
        user_dir.is_dir() && project_dir.is_dir(),
        "a refusal removes nothing"
    );

    // Qualified, it removes the copy that was asked for and only that one.
    let (ok, out, err) = run(&home, &ws, &["plugin", "remove", "twice", "--user"]);
    assert!(ok, "{out}{err}");
    assert!(!user_dir.exists(), "the user copy is gone");
    assert!(project_dir.is_dir(), "the project copy is untouched");

    // Opposites at once is not a choice.
    let (ok, _, err) = run(
        &home,
        &ws,
        &["plugin", "remove", "twice", "--project", "--user"],
    );
    assert!(!ok, "both scopes at once is not a choice: {err}");
    assert!(project_dir.is_dir());

    // With the shadow gone the bare name is unambiguous again.
    let (ok, out, err) = run(&home, &ws, &["plugin", "remove", "twice"]);
    assert!(ok, "{out}{err}");
    assert!(!project_dir.exists());
}
