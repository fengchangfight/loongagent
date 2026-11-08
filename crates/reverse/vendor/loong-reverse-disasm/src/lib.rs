//! Purpose: Instruction decoding and the function-level view over it (x86, ARM, wasm).
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
//! loongagent reverse's own native engine: decoding, functions, control flow, references, pseudo-C.
//!
//! [`Image`] wraps a parsed container and its bytes; [`Program::analyze`] discovers functions
//! (entry, symbols, exports, container function lists, call targets, code pointers, prologue
//! scan), splits them into basic blocks with jump-table recovery, and records cross-references,
//! string references and the call graph. [`pseudo`] lifts one function to structured pseudo-C
//! and [`dataflow`] builds a register def-use graph. Everything is bounded and cancellable, and
//! never panics on hostile input.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

pub mod arity;
pub mod dataflow;
pub mod decode;
pub mod image;
pub mod program;
pub mod pseudo;
/// eBPF / Solana sBPF decoding and static analysis (clean room, self-contained).
pub mod sbpf;

pub use decode::{ArmConsts, Cond, Flow, Insn, Kind, MemOp, Mode, Op, canonical_reg, decode};
pub use image::Image;
pub use program::{
    Block, CallSite, Callee, Function, JumpTable, Limits, ProgString, Program, SlotInfo, Stats,
    Term, Xref, XrefKind, slot_label,
};

/// Why the native engine stopped.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DisasmError {
    #[error("{0}")]
    Format(loong_reverse_formats::FormatError),
    #[error("not supported: {0}")]
    Unsupported(String),
    #[error("cancelled")]
    Cancelled,
}

/// Cancellation and progress for long analyses (the engine passes its cancel token through).
pub trait Control {
    fn cancelled(&self) -> bool {
        false
    }
    fn progress(&self, _fraction: f32, _message: &str) {}
}

/// A [`Control`] that never cancels and ignores progress.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoControl;

impl Control for NoControl {}
