//! > Vendored from Craft Anything (Apache-2.0/MIT, © Craft Anything contributors), trimmed:
//! > upstream commit `fc93bd0` (2026-11-05). Owned by `docs/design/reverse.md`.
//!
//! The decompiler-engine hook the native tools consult — and, in this build, always decline.
//!
//! Purpose: keep the `native_function_pseudocode` call path single-shaped while no external
//! decompiler ships, so the built-in lifter answers every request.
//! Serves: `docs/design/reverse.md` (native toolset: pseudo-code from the built-in lifter).
//! Design: `docs/design/reverse.md`;
//! `docs/superpowers/specs/2026-10-24-loongagent-reverse-design.md` (no Ghidra, no Deep mode).
//! Upstream: `crate::tool::Ctx`, `loong_reverse_evidence`.
//! Downstream: `crate::tools::native_function`, `crate::tools::session_tools`.
//! Verify: `cargo test -p loong-reverse-engine`.
//!
//! Upstream, this module preferred a Ghidra result when Deep mode was on, and fell back to the
//! built-in lifter otherwise. The reverse surface copies no decompiler bridge, so the hook returns
//! `Err(None)` ("no engine") every time: no limitation is added to the evidence and the built-in
//! lifter's answer stands on its own.

use crate::tool::{Ctx, ToolOutput};
use loong_reverse_evidence::ToolError;
use serde_json::Value;

/// Whose address space an address is in.
///
/// Purpose: name the address space the caller resolved the address in, without an engine to ask.
/// Serves: `docs/design/reverse.md`.
/// Design: `docs/design/reverse.md`.
/// Up: `crate::tools::native_function`.
/// Down: [`pseudo_code`].
/// Verify: `cargo test -p loong-reverse-engine`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum At {
    /// An address in the loaded image, which the built-in analysis placed.
    Ours(u64),
    /// An address from an external engine's own address space (never produced in this build).
    Engine(u64),
}

/// A per-target language override a caller may record (`core_target_open` accepts one).
///
/// Purpose: keep the parameter accepted so the tool schema stays stable, even though no engine
/// consumes it here.
/// Serves: `docs/design/reverse.md`.
/// Design: `docs/design/reverse.md`.
/// Up: `crate::tools::session_tools`.
/// Down: nothing.
/// Verify: `cargo test -p loong-reverse-engine`.
pub struct LanguageOverride(pub String);

/// Try the external decompiler.
///
/// Purpose: always answer "no engine", so the caller uses the built-in lifter.
/// Serves: `docs/design/reverse.md`.
/// Design: `docs/design/reverse.md`.
/// Up: `crate::tools::native_function`.
/// Down: nothing.
/// Verify: `cargo test -p loong-reverse-engine`.
///
/// `Err(None)` means "no engine is in play" (upstream's Clean-room case), `Err(Some(why))` would
/// say why an engine that is switched on can't be used. This build only ever returns `Err(None)`.
pub fn pseudo_code(
    _ctx: &mut Ctx<'_>,
    _at: At,
    _our_base: u64,
    _params: Value,
) -> Result<Result<ToolOutput, ToolError>, Option<String>> {
    Err(None)
}

#[cfg(test)]
mod tests {
    /// Purpose: the hook declines, so the built-in lifter answers.
    #[test]
    fn no_engine_is_ever_used() {
        assert_eq!(
            crate::tools::deep::At::Ours(0x401000),
            crate::tools::deep::At::Ours(0x401000)
        );
    }
}
