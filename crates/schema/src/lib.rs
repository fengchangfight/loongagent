//! Protocol schema: the single source of truth for every message on the wire.
//!
//! ADR-0002: JSON-RPC 2.0, schema-first and versioned.
//! ADR-0013: Rust types and the TypeScript client's types are both *generated*
//! from this schema (`scripts/gen-schema.py`), so the two sides cannot drift.

/// Version of the protocol schema. Breaking schema changes bump this.
pub const SCHEMA_VERSION: &str = "0.1.0";

#[cfg(test)]
mod tests {
    #[test]
    fn schema_version_is_set() {
        assert!(!super::SCHEMA_VERSION.is_empty());
    }
}
