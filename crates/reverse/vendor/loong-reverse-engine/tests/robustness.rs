//! Robustness of the vendored tools against bad targets and missing externals.
//!
//! Purpose: Pin that a bad target (a directory, random bytes, a fake APK) or a missing
//! browser becomes a structured tool error, never a panic or a hang.
//! Serves: the review focus of `docs/superpowers/plans/2026-10-24-loongagent-reverse.md` Task 4.
//! Design: `docs/design/reverse.md` §3 (the MCP server pair surfaces these errors as-is).
//! Upstream: `crates/reverse/vendor/loong-reverse-engine/` tool entry points.
//! Downstream: nothing (a test target).
//! Verify: `cargo test -p loong-reverse-engine --test robustness`.

use loong_reverse_engine::evidence::ToolError;
use loong_reverse_engine::{CallOptions, Caller, Engine, ToolOutput};
use serde_json::json;

/// Purpose: An engine call against a fresh isolated session.
/// Contract: `crates/reverse/vendor/loong-reverse-engine/tests/robustness.rs`.
fn call(id: &str, params: serde_json::Value) -> Result<ToolOutput, ToolError> {
    let engine = Engine::new(loong_reverse_engine::testing::session());
    engine.call(id, params, CallOptions::new(Caller::Mcp))
}

#[test]
/// Purpose: `core_target_open` accepts a folder as a directory-kind target (it is the
/// "open an app bundle" path), and answers with evidence, never a panic.
fn open_directory_as_target_opens_a_directory_kind() {
    let dir = loong_reverse_engine::testing::temp_dir();
    let out =
        call("core_target_open", json!({"path": dir})).expect("a folder opens, it does not panic");
    // Opening answers with the target record itself; the kind says what it became.
    let value = match out {
        ToolOutput::Value(v) => v,
        ToolOutput::Evidence(ev) => serde_json::to_value(&*ev).expect("evidence serialises"),
    };
    assert_eq!(
        value["kind"].as_str().unwrap_or_default(),
        "directory",
        "a folder opens as a directory target: {value}"
    );
}

#[test]
/// Purpose: `core_target_triage` on random bytes reports an unknown kind, never a panic.
fn triage_random_bytes_returns_unknown_kind() {
    let path = loong_reverse_engine::testing::temp_dir().join("noise.bin");
    let mut bytes = Vec::with_capacity(1024);
    // A tiny deterministic PRNG: the bytes only need to not be any known magic.
    let mut x: u32 = 0x2545_F491;
    for _ in 0..1024 {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        bytes.push((x >> 24) as u8);
    }
    std::fs::write(&path, &bytes).expect("the fixture writes");

    let out = call("core_target_triage", json!({"path": path}))
        .expect("triage answers, it does not panic");
    let ev = match out {
        ToolOutput::Evidence(ev) => ev,
        ToolOutput::Value(v) => panic!("triage must answer with evidence, got {v}"),
    };
    assert_eq!(
        ev.subject.kind, "unknown",
        "random bytes triage as unknown, not a guessed format: {}",
        ev.summary
    );
}

#[test]
/// Purpose: `web_site_open` on an unreachable host answers with an error quickly, not a hang.
fn web_site_open_unreachable_host_returns_error() {
    let started = std::time::Instant::now();
    let result = call("web_site_open", json!({"url": "http://127.0.0.1:1/"}));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(60),
        "an unreachable host must not hang the session"
    );
    match result {
        Err(_) => {} // The shape we want: a structured error.
        Ok(out) => panic!("an unreachable host must not open: {out:?}"),
    }
}

#[test]
/// Purpose: `android_package_show` on a zip named `.apk` without `classes.dex` answers with
/// an error, not a confident empty report: a real apk always carries a dex, so its absence
/// means the file is not one.
fn android_package_show_on_zip_without_dex_returns_error() {
    let dir = loong_reverse_engine::testing::temp_dir();
    let apk = dir.join("fake.apk");
    let zip = dir.join("fake.zip");
    // A minimal empty zip archive (end-of-central-directory record only).
    let empty_zip: [u8; 22] = [
        0x50, 0x4B, 0x05, 0x06, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    std::fs::write(&zip, empty_zip).expect("the fixture writes");
    std::fs::copy(&zip, &apk).expect("the .apk copy");

    let result = call("android_package_show", json!({"path": apk}));
    match result {
        Err(err) => {
            let text = format!("{} {:?}", err.message, err.remediation);
            assert!(
                text.contains("classes.dex"),
                "the error says what is missing: {text}"
            );
        }
        Ok(ToolOutput::Evidence(ev)) => panic!(
            "a zip without classes.dex is not an apk; got evidence: {}",
            ev.summary
        ),
        Ok(ToolOutput::Value(v)) => panic!("a zip without classes.dex is not an apk; got {v}"),
    }
}
