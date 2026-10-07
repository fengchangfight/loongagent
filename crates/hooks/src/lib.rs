//! Lifecycle hooks: the deterministic enforcement layer (ADR-0007 §7).
//!
//! A hook answers one question at a declared moment — allow, ask, or deny — and
//! nothing else. It is not a place for behaviour: it cannot rewrite a tool call,
//! edit a file, or speak to the model. That restraint is the point. Hooks make the
//! *decisions* around the loop reproducible and administrable, while the loop
//! itself stays small enough to audit.
//!
//! What runs in v1: `shell` (a fingerprinted script) and `http` (an endpoint,
//! off until the manifest opts in). The `mcp` kind needs the MCP client (T-15)
//! and is refused with that reason rather than accepted and ignored.
//!
//! See [`config`] for the manifest and the trust gate, and [`run`] for the
//! verdict contract and monotonic aggregation.

pub mod config;
pub mod run;

pub use config::{
    ConfigError, HookEvent, HookKind, HookSet, HookSpec, OnError, TrustStatus, HOOKS_DIR,
    HOOKS_FILE, TRUST_FILE,
};
pub use run::{payload, HookOutcome, HookReport, HookRunner};
