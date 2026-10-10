//! The fixed kernel (ADR-0004): the model+tool loop, the context ledger that is
//! the only write path to the model (ADR-0006), and the wiring to the PDP
//! (ADR-0007) and the tool registry.
//!
//! Everything here is deliberately *not* pluggable: behavior cannot hide in a
//! replaced core, which is what makes the event stream trustworthy.

pub mod ledger;
pub mod turn;

pub use ledger::{estimate_tokens, Category, Entry, EvictionReport, ExplainRow, Ledger, Lifetime};
pub use turn::{
    steps_or_unlimited, Kernel, KernelConfig, KernelError, PendingCall, TurnOutcome,
    DEFAULT_MAX_STEPS,
};
