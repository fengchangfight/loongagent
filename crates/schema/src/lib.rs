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

#[cfg(test)]
mod tests {
    #[test]
    /// Purpose: Pins that the schema exposes a non-empty protocol version.
    fn schema_version_is_set() {
        assert!(!super::SCHEMA_VERSION.is_empty());
    }
}
