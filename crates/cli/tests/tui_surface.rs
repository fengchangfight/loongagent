//! The two terminal surfaces: a full-screen one that needs a TTY, and a line-mode
//! one that does not. Both must say what they are instead of failing obscurely.

use std::process::{Command, Stdio};

#[test]
fn the_full_screen_surface_refuses_a_pipe_and_points_at_the_line_mode() {
    // The test harness has no TTY on stdout, which is exactly the case a user hits
    // when piping output: the failure must name the way out.
    //
    // Its own session and cwd: a shared one means one test's leftovers become
    // another test's mystery.
    let dir = std::env::temp_dir().join(format!("loongagent-tui-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_loongagent"))
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
    assert!(!out.status.success(), "a full-screen UI must not pretend to run in a pipe");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no terminal on stdout"), "stderr: {stderr}");
    assert!(stderr.contains("loongagent repl"), "the way out is named: {stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_line_mode_surface_works_under_a_pipe() {
    let dir = std::env::temp_dir().join(format!("loongagent-repl-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_loongagent"))
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
    assert!(stdout.contains("echo: hello from a pipe"), "stdout: {stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}
