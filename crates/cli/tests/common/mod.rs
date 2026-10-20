//! Purpose: Shared scaffolding for the integration tests that drive the real binary.
//!
//! # Contract
//! - **Serves:** every integration test that declares `mod common` and picks up the helper.
//! - **Design:** `docs/design/surfaces.md` §2 and §7.
//! - **Upstream:** `vertical_slice.rs`, `web_surface.rs` and `tui_surface.rs` declare `mod common;`.
//! - **Downstream:** `CARGO_BIN_EXE_loongagent` and the two environment variables that isolate a run.
//! - **Verify:** `cargo test -p loongagent --tests`
//!
//! Shared scaffolding for the integration tests that drive the real binary.
//!
//! A file here is not an integration-test target of its own — that is the point:
//! each test binary declares `mod common;` and picks up the helpers it needs.

use std::process::Command;

/// Purpose: A command for the built binary that cannot see the developer's configuration or spend real tokens.
/// Serves: `vertical_slice.rs`, `web_surface.rs` and `tui_surface.rs`.
/// Design: `docs/design/surfaces.md` §2.
/// Up: the integration tests that declare `mod common`; no other caller.
/// Down: `std::process::Command` and the two environment variables it sets.
/// Verify: `cargo test -p loongagent --tests`.
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
