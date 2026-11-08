//! Purpose: The analyzers that turn a parsed target into findings, call graphs and values.
//! Serves: `docs/design/reverse.md` (the reverse-engineering surface loongagent exposes).
//! Design: `docs/design/reverse.md`; the vendoring procedure is
//! `docs/superpowers/specs/2026-10-24-loongagent-reverse-design.md`.
//! Upstream: the crates below it in this workspace.
//! Downstream: `loong-reverse-engine` and `loong-reverse-mcp`, and through them the agent.
//! Verify: `scripts/check-reverse.sh`.
//!
//! > Vendored from Craft Anything (Apache-2.0/MIT, © Craft Anything contributors),
//! > upstream commit `fc93bd0` (2026-11-05). Owned by `docs/design/reverse.md`; the vendoring
//! > procedure lives in `docs/superpowers/specs/2026-10-24-loongagent-reverse-design.md`.
//! > Verified by `scripts/check-reverse.sh`.

//!
//! Analyzers that turn parsed targets into evidence for loongagent reverse.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

pub mod function_match;
pub mod java_decompile;
pub mod js_app;
pub mod js_compare;
pub mod js_inventory;
pub mod js_ledger;
pub mod js_projection;
pub mod js_source;
pub mod js_trace;
pub mod native;
pub mod standin;
pub mod web_har;
pub mod web_scripts;
