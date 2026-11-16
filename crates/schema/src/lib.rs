//! Purpose: Own the single source of truth for every message on the wire, generated once for Rust and TypeScript.
//!
//! # Contract
//! - **Serves:** a single source of truth for every message on the wire, generated once for Rust and TypeScript.
//! - **Design:** `docs/design/events-storage.md` (ADR-0002, ADR-0013).
//! - **Upstream:** every crate that names a wire type, and `loong-protocol`.
//! - **Downstream:** `generated`, the module written by `scripts/gen-schema.py`.
//! - **Verify:** `cargo test -p loong-schema` · `./scripts/check.sh`
//!
//! Protocol schema: the single source of truth for every message on the wire.
//!
//! ADR-0002: JSON-RPC 2.0, schema-first and versioned.
//! ADR-0013: Rust types and the TypeScript client's types are both *generated*
//! from this schema (`scripts/gen-schema.py`), so the two sides cannot drift.

/// Generated from `protocol.toml` — the wire types, method names and event kinds.
pub mod generated;

/// Version of the protocol schema. Breaking schema changes bump this.
pub use generated::SCHEMA_VERSION;

/// How much autonomy a session has been granted, as one axis (ADR-0007 §5).
///
/// Purpose: Be the three stops a session can be in, and the one value every
/// surface, the kernel and the wire agree on.
/// Serves: `loong-kernel`'s dispatch, `loong-server`'s `permissions.set`,
/// `loong-tui`'s client and `loong-tui-app`'s screen.
/// Design: `docs/decisions/0022-god-mode-refuses-two-shapes.md`;
/// `docs/design/kernel-loop.md` §5.
/// Up: every crate that reads a `permissions.relaxed` event.
/// Down: nothing — a plain value.
/// Verify: `cargo test -p loong-schema`.
///
/// Three stops, not two: `Ask` is the default and the only one a session starts
/// in; `Yolo` turns every question into an allow but the irreversible one
/// (ADR-0020); `God` turns that one too, and refuses two shapes outright instead
/// of asking (ADR-0022).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PermissionMode {
    /// The default: a ruling that would ask parks for an answer.
    #[default]
    Ask,
    /// Every question becomes an allow but the irreversible one (ADR-0020).
    Yolo,
    /// That one too, minus the two shapes the gates refuse (ADR-0022).
    God,
}

impl PermissionMode {
    /// Purpose: The wire spelling, and the word the surfaces show.
    /// Serves: the protocol's `mode` field and both surfaces' marks.
    /// Design: ADR-0002 (the wire's one spelling).
    /// Up: `Display`.
    /// Down: `parse`, which reads it back.
    /// Verify: `cargo test -p loong-schema`.
    pub fn as_str(self) -> &'static str {
        match self {
            PermissionMode::Ask => "ask",
            PermissionMode::Yolo => "yolo",
            PermissionMode::God => "god",
        }
    }

    /// Purpose: Read the wire spelling back, `None` for a word this build does not know.
    /// Serves: `FromStr`, and a client that has to accept a word from a newer build.
    /// Design: ADR-0002 (the wire's one spelling).
    /// Up: `as_str`, whose inverse this is.
    /// Down: `FromStr`.
    /// Verify: `cargo test -p loong-schema`.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "ask" => Some(PermissionMode::Ask),
            "yolo" => Some(PermissionMode::Yolo),
            "god" => Some(PermissionMode::God),
            _ => None,
        }
    }

    /// Purpose: Whether a ruling that would ask is allowed instead.
    /// Serves: the kernel's ask arm, and both surfaces' "no question" state.
    /// Design: `docs/decisions/0022-god-mode-refuses-two-shapes.md` §2.
    /// Up: `Kernel::dispatch`.
    /// Down: `Kernel::relaxes`.
    /// Verify: `cargo test -p loong-schema`.
    pub fn skips_prompts(self) -> bool {
        !matches!(self, PermissionMode::Ask)
    }
}

impl std::str::FromStr for PermissionMode {
    /// The wire word that failed to parse, quoted.
    ///
    /// Purpose: Report the unreadable word, so a client sees what it sent.
    /// Serves: `PermissionMode::from_str`.
    /// Design: ADR-0002 (the wire's one spelling).
    /// Up: `PermissionMode::parse`.
    /// Down: the error text a caller prints.
    /// Verify: `cargo test -p loong-schema`.
    type Err = String;

    /// Purpose: Parse the wire spelling, with the word in the message when it is wrong.
    /// Contract: `parse`, with an error rather than `None`.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text).ok_or_else(|| format!("unknown permission mode {text:?}"))
    }
}

impl std::fmt::Display for PermissionMode {
    /// Purpose: Print the wire spelling.
    /// Contract: `as_str`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    /// Purpose: Pins that the schema exposes a non-empty protocol version.
    fn schema_version_is_set() {
        assert!(!super::SCHEMA_VERSION.is_empty());
    }

    /// Purpose: The three modes round-trip through the wire spelling, and the spelling is the one the schema and the surfaces use.
    #[test]
    fn a_permission_mode_round_trips_through_its_name() {
        for (mode, name) in [
            (super::PermissionMode::Ask, "ask"),
            (super::PermissionMode::Yolo, "yolo"),
            (super::PermissionMode::God, "god"),
        ] {
            assert_eq!(mode.as_str(), name);
            assert_eq!(name.parse::<super::PermissionMode>().unwrap(), mode);
        }
        assert!("gdo".parse::<super::PermissionMode>().is_err());
        assert_eq!(super::PermissionMode::default(), super::PermissionMode::Ask);
        assert!(!super::PermissionMode::Ask.skips_prompts());
        assert!(super::PermissionMode::Yolo.skips_prompts());
        assert!(super::PermissionMode::God.skips_prompts());
    }

    /// Purpose: An event written before god mode existed still parses, and says yolo by its boolean.
    #[test]
    fn a_permissions_relaxed_event_without_a_mode_still_parses() {
        let old: crate::generated::PermissionsRelaxed = serde_json::from_value(serde_json::json!({
            "auto_approve": true,
            "reason": "the person asked for it with /yolo"
        }))
        .expect("an event from an older log parses: every new field is optional");
        assert_eq!(old.mode, None);
        assert!(old.auto_approve);
    }
}
