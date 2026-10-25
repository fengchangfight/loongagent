//! Purpose: The fixed kernel (ADR-0004): the model+tool loop and the context ledger that is its only write path.
//!
//! Serves: a session a person can audit: every step is an event, every context block
//! is accounted for, and one PDP gates every registry tool call.
//!
//! # Contract
//! - **Design:** `docs/design/kernel-loop.md`; `docs/design/context-ledger.md` owns
//!   the ledger and `docs/design/tools-permissions.md` owns the gate.
//! - **Upstream:** `crates/cli/src/server.rs` builds a `Kernel`; `crates/server` runs
//!   it and `crates/tui-app` reaches it through that server.
//! - **Downstream:** `ledger`, `context_engine` and `turn`, re-exported here.
//! - **Verify:** `cargo test -p loong-kernel` · `./scripts/check.sh`.

pub mod context_engine;
pub mod ledger;
pub mod turn;

pub use context_engine::{ContextEngine, ContextEngineConfig};
pub use ledger::{estimate_tokens, Category, Entry, EvictionReport, ExplainRow, Ledger, Lifetime};
pub use turn::{
    effective_budget, steps_or_unlimited, InstructionBlock, InstructionScope, Kernel, KernelConfig,
    KernelError, PendingCall, TurnOutcome, DEFAULT_MAX_STEPS, DEFAULT_RESERVED_CONTEXT,
};
