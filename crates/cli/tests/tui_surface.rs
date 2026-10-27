//! Purpose: The two terminal surfaces: the full-screen one refuses a pipe and names line mode, and line mode works under one.
//!
//! # Contract
//! - **Serves:** the first-run experience of both terminal surfaces.
//! - **Design:** `docs/design/surfaces.md` §2 and §3.
//! - **Upstream:** runs the built binary; nothing imports this file.
//! - **Downstream:** `common::isolated_command`, and the `tui` and `repl` commands.
//! - **Verify:** `cargo test -p loongagent --test tui_surface`
//!
//! The two terminal surfaces: a full-screen one that needs a TTY, and a line-mode
//! one that does not. Both must say what they are instead of failing obscurely.

use std::path::{Path, PathBuf};
use std::process::Stdio;

mod common;

/// Purpose: Every session log under an isolated home, so a test can count sessions without knowing the project slug.
/// Serves: `a_bare_tui_starts_a_fresh_session`.
/// Design: `docs/design/sessions-lineage.md` §1.
/// Up: the test below; no production caller.
/// Down: `std::fs::read_dir`.
/// Verify: `cargo test -p loongagent --test tui_surface`.
fn session_logs(home: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(projects) = std::fs::read_dir(home.join("projects")) else {
        return found;
    };
    for project in projects.flatten() {
        let Ok(entries) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Purpose: A piped full-screen run fails with the reason and names `loongagent repl` as the way out.
#[test]
fn the_full_screen_surface_refuses_a_pipe_and_points_at_the_line_mode() {
    // The test harness has no TTY on stdout, which is exactly the case a user hits
    // when piping output: the failure must name the way out.
    //
    // Its own session and cwd: a shared one means one test's leftovers become
    // another test's mystery.
    let dir = std::env::temp_dir().join(format!("loongagent-tui-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = common::isolated_command()
        .arg("tui")
        .arg("--session")
        .arg(dir.join("s.jsonl"))
        .arg("--cwd")
        .arg(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("the binary runs");
    assert!(
        !out.status.success(),
        "a full-screen UI must not pretend to run in a pipe"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("stdout 不是终端"), "stderr: {stderr}");
    assert!(
        stderr.contains("loongagent repl"),
        "the way out is named: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Purpose: Line mode accepts a prompt on a pipe, prints the banner and the answer, and exits when the pipe closes.
#[test]
fn the_line_mode_surface_works_under_a_pipe() {
    let dir = std::env::temp_dir().join(format!("loongagent-repl-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut child = common::isolated_command()
        .arg("repl")
        .arg("--session")
        .arg(dir.join("s.jsonl"))
        .arg("--cwd")
        .arg(&dir)
        .arg("--model")
        .arg("echo")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the binary runs");

    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(stdin, "hello from a pipe").unwrap();
        stdin.flush().unwrap();
        // Closing stdin ends the loop; the banner and the answer must be there.
    }
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("loongagent — session"), "stdout: {stdout}");
    assert!(
        stdout.contains("echo: hello from a pipe"),
        "stdout: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Purpose: A bare `tui` starts a fresh session instead of continuing this project's newest one.
#[test]
fn a_bare_tui_starts_a_fresh_session() {
    let base = std::env::temp_dir().join(format!("loongagent-tui-fresh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let project = base.join("project");
    let home = base.join("home");
    let user = base.join("user");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&user).unwrap();

    let command = || {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_loongagent"));
        command
            .env("LOONGAGENT_HOME", &home)
            .env("HOME", &user)
            .current_dir(&project);
        command
    };

    // Seed exactly one project session, with the echo model so no key is needed.
    let seeded = command()
        .arg("run")
        .arg("seed")
        .arg("--model")
        .arg("echo")
        .arg("--cwd")
        .arg(&project)
        .stdin(Stdio::null())
        .output()
        .expect("the binary runs");
    assert!(
        seeded.status.success(),
        "the seed run failed: {}",
        String::from_utf8_lossy(&seeded.stderr)
    );
    let before = session_logs(&home);
    assert_eq!(before.len(), 1, "one seeded session: {before:?}");
    let seeded_id = before[0].file_stem().unwrap().to_string_lossy().to_string();

    // No `--session`: with a TTY the screen would open; in a pipe it fails after
    // naming the session it chose, which is what this test reads.
    let out = command()
        .arg("tui")
        .arg("--model")
        .arg("echo")
        .arg("--cwd")
        .arg(&project)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("the binary runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let chosen = stdout
        .lines()
        .find_map(|line| line.strip_prefix("session "))
        .expect("the tui names the session it used")
        .trim()
        .to_string();
    assert_ne!(
        chosen, seeded_id,
        "a bare tui must start fresh rather than continue {seeded_id}"
    );
    let after = session_logs(&home);
    assert_eq!(after.len(), 2, "the new session is a second log: {after:?}");
    let _ = std::fs::remove_dir_all(&base);
}
