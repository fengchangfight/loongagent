//! Module contract: stdio end-to-end tests for the `loong-reverse-mcp` binary.
//! Purpose: Prove the binary speaks MCP JSON-RPC over stdio and lists exactly the trimmed tool surface.
//! Serves: `docs/design/reverse.md` §3 (the MCP server pair).
//! Design: `docs/superpowers/specs/2026-10-24-loongagent-reverse-design.md` Task 3.
//! Upstream: `crates/reverse/mcp-server/src/main.rs`.
//! Downstream: nothing (a test target).
//! Verify: `cargo test -p loong-reverse-mcp`.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Purpose: A spawned server with its stdio ends, killed when dropped.
struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    sent: u64,
}

impl Server {
    /// Purpose: Spawn the binary with `--workspace` in a temp directory.
    fn spawn(workspace: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_loong-reverse-mcp"))
            .arg("--workspace")
            .arg(workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the binary spawns");
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
        Server {
            child,
            stdin,
            stdout,
            sent: 0,
        }
    }

    /// Purpose: Send one request and read until the matching id answers (the server may interleave).
    fn call(&mut self, method: &str, params: Value) -> Value {
        self.sent += 1;
        let id = self.sent;
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{line}").expect("the request writes");
        self.stdin.flush().expect("the request flushes");
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            assert!(
                Instant::now() < deadline,
                "no answer to {method} within 30s"
            );
            let mut line = String::new();
            let n = self
                .stdout
                .read_line(&mut line)
                .expect("a response line reads");
            assert!(n > 0, "the server closed the stream during {method}");
            let reply: Value = serde_json::from_str(&line).expect("a response is JSON-RPC");
            if reply["id"] == id {
                return reply;
            }
        }
    }
}

impl Drop for Server {
    /// Purpose: Never leave a server running past the test.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Purpose: The trimmed tool surface answers `initialize` and lists the tools the design names.
#[test]
fn the_stdio_handshake_lists_the_trimmed_surface() {
    let workspace = tempfile::tempdir().expect("a temp workspace");
    let mut server = Server::spawn(workspace.path());

    let hello = server.call(
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "stdio-test", "version": "0"},
        }),
    );
    assert!(
        hello["result"]["serverInfo"]["name"].is_string(),
        "initialize answers with a serverInfo: {hello}"
    );

    // The binary lists the whole trimmed surface from the start (`run` sets `all_tools`):
    // no `enable_toolset` round-trip is needed.
    let tools = server.call("tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .expect("tools/list returns an array")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();

    for required in [
        "core_target_triage",
        "native_function_pseudocode",
        "web_site_open",
        "android_package_show",
    ] {
        assert!(
            names.contains(&required),
            "the surface keeps {required}: {names:?}"
        );
    }
    // Task 1 keeps the GUI-navigation tools (cursor/bookmark/comment) even though
    // `docs/design/reverse.md` §1 lists them as cut — the vendored `pinned_surface`
    // registry test pins them in, and `scripts/check-reverse.sh` only forbids the
    // external bridges. Trimming to the design list is follow-up work, recorded in
    // the plan's progress ledger; this test asserts the surface as the gate does.
    for dropped in [
        "android_jeb_decompile",
        "native_rizin_run",
        "native_cutter_open",
    ] {
        assert!(
            !names.contains(&dropped),
            "the surface drops {dropped}: {names:?}"
        );
    }
}
