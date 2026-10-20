//! Purpose: Lifecycle hooks: the deterministic veto channel around the loop, allow / ask / deny only (ADR-0007 §7).
//!
//! # Contract
//! - **Serves:** the kernel's turn, at the six implemented lifecycle events.
//! - **Design:** `docs/design/extension-model.md` §1 and `docs/design/security-trust.md` §1–§3.
//! - **Upstream:** `crates/kernel/src/turn/build.rs` and `crates/cli/src/hooks.rs`.
//! - **Downstream:** `config` (manifest and trust gate) and `run` (verdict contract and aggregation).
//! - **Verify:** `cargo test -p loong-hooks` · `./scripts/check.sh`
//!
//! Lifecycle hooks: the deterministic enforcement layer (ADR-0007 §7).
//!
//! A hook answers one question at a declared moment — allow, ask, or deny — and
//! nothing else. It is not a place for behaviour: it cannot rewrite a tool call,
//! edit a file, or speak to the model. That restraint is the point. Hooks make the
//! *decisions* around the loop reproducible and administrable, while the loop
//! itself stays small enough to audit.
//!
//! All three kinds run: `shell` (a fingerprinted script), `http` (an endpoint,
//! off until the manifest opts in) and `mcp` (a tool on a trusted server). The
//! verdict contract is the same for each.
//!
//! See [`config`] for the manifest and the trust gate, and [`run`] for the
//! verdict contract and monotonic aggregation.

pub mod config;
pub mod run;

pub use config::{
    ConfigError, HookEvent, HookKind, HookSet, HookSpec, OnError, TrustStatus, HOOKS_DIR,
    HOOKS_FILE, TRUST_FILE,
};
pub use run::{payload, HookOutcome, HookReport, HookRunner, McpCaller};
