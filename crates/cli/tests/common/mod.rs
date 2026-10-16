//! Shared scaffolding for the integration tests that drive the real binary.
//!
//! A file here is not an integration-test target of its own — that is the point:
//! each test binary declares `mod common;` and picks up the helpers it needs.

use std::process::Command;

/// A command that cannot see the developer's configuration. Without this a control
/// plane configured on this machine supplies the endpoints under test — and spends
/// real tokens while doing it.
pub fn isolated_command() -> Command {
    let base = std::env::temp_dir().join(format!("loongagent-testenv-{}", std::process::id()));
    let mut command = Command::new(env!("CARGO_BIN_EXE_loongagent"));
    command
        .env("LOONGAGENT_HOME", base.join("loongagent"))
        .env("HOME", base.join("home"));
    command
}
