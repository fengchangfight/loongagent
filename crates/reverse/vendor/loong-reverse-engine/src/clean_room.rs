//! > Vendored from Craft Anything (Apache-2.0/MIT, © Craft Anything contributors), trimmed:
//! > upstream commit `fc93bd0` (2026-11-05). Owned by `docs/design/reverse.md`.
//!
//! The one place that answers "may this tool read inside the target?" — now always yes.
//!
//! Purpose: keep the call sites (evidence environment, availability, reports) single-shaped while
//! the reverse surface runs with the target readable, as `docs/design/reverse.md` decides.
//! Serves: `docs/design/reverse.md` (no Clean room, no Deep-mode toggle).
//! Design: `docs/design/reverse.md`;
//! `docs/superpowers/specs/2026-10-24-loongagent-reverse-design.md` (no Clean room).
//! Upstream: `crate::Availability`, `loong_reverse_evidence::{ErrorCode, ToolError}`.
//! Downstream: `crate::lib` (availability), `crate::registry`, `crate::tools::{investigation,
//! browser, report}`.
//! Verify: `cargo test -p loong-reverse-engine`.
//!
//! Upstream kept a `TOOLS` allow-list of the tools that run without reading a target, and refused
//! the rest in Clean room. Reverse engineering exists to read the target, so the distinction is
//! gone: [`allows`] is always true, [`availability`] never refuses, and a refusal error is only
//! ever produced by an explicit caller (tests keep the shape checkable).

use crate::Availability;
use loong_reverse_evidence::{ErrorCode, ToolError};

/// Whether `id` may read inside a target here: always.
///
/// Purpose: name the decision in one place, so the call sites stay unchanged and greppable.
/// Serves: `docs/design/reverse.md`.
/// Design: `docs/design/reverse.md`.
/// Up: `crate::registry`, `crate::lib`.
/// Down: nothing.
/// Verify: `cargo test -p loong-reverse-engine`.
pub fn allows(_id: &str) -> bool {
    true
}

const REASON: &str = "This looks inside the original (its code, files or page source).";
const REMEDIATION: &str =
    "Reverse engineering needs to read the target; no switch turns that off in this build.";

/// Why `id` can't run in this mode: never, because there is no mode that refuses.
///
/// Purpose: keep the availability shape (`Option<Availability>`) the registry expects.
/// Serves: `docs/design/reverse.md`.
/// Design: `docs/design/reverse.md`.
/// Up: `crate::lib`.
/// Down: [`allows`].
/// Verify: `cargo test -p loong-reverse-engine`.
pub(crate) fn availability(id: &str, clean_room: bool) -> Option<Availability> {
    (clean_room && !allows(id)).then(|| Availability::no(REASON, REMEDIATION))
}

/// The error shape a refusal would have; no shipped call site produces one.
///
/// Purpose: keep `details.reason = "clean_room"` recognisable for clients that look for it.
/// Serves: `docs/design/reverse.md`.
/// Design: `docs/design/reverse.md`.
/// Up: tests only.
/// Down: [`is_refusal`].
/// Verify: `cargo test -p loong-reverse-engine`.
pub(crate) fn refusal(id: &str) -> ToolError {
    ToolError::new(ErrorCode::FeatureMissing, REASON)
        .with_remediation(REMEDIATION)
        .with_details(serde_json::json!({"reason": "clean_room", "tool": id}))
}

/// How a report names the mode its findings were made in.
///
/// Purpose: one wording for reports and evidence environments.
/// Serves: `docs/design/reverse.md`.
/// Design: `docs/design/reverse.md`.
/// Up: `crate::tools::investigation_tools`, `crate::tools::report_tools`.
/// Down: nothing.
/// Verify: `cargo test -p loong-reverse-engine`.
pub fn mode_words(clean_room: bool) -> &'static str {
    if clean_room {
        "Clean room (nothing inside the original was read)"
    } else {
        "Deep (the original's code and files could be read)"
    }
}

/// Whether an error is the Clean-room refusal (no shipped tool produces one now).
///
/// Purpose: let a client recognise the legacy refusal shape without changing.
/// Serves: `docs/design/reverse.md`.
/// Design: `docs/design/reverse.md`.
/// Up: tests only.
/// Down: nothing.
/// Verify: `cargo test -p loong-reverse-engine`.
pub fn is_refusal(e: &ToolError) -> bool {
    e.details
        .as_ref()
        .and_then(|d| d.get("reason"))
        .and_then(serde_json::Value::as_str)
        == Some("clean_room")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Purpose: nothing is ever refused, whatever the settings say.
    #[test]
    fn the_surface_has_no_clean_room() {
        let mut s = crate::testing::session();
        assert!(!s.clean_room());
        s.settings.deep_mode = false;
        assert!(!s.clean_room(), "the setting can't narrow the surface");
        for id in [
            "core_target_open",
            "native_function_pseudocode",
            "web_page_show",
        ] {
            assert!(allows(id));
            assert!(availability(id, false).is_none());
        }
    }

    /// Purpose: the legacy refusal shape is still recognisable.
    #[test]
    fn the_refusal_shape_is_recognisable() {
        assert!(is_refusal(&refusal("native_functions_list")));
        assert_eq!(
            mode_words(false),
            "Deep (the original's code and files could be read)"
        );
    }
}
