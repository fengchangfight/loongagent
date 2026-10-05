//! loongagent — one binary, many faces (ADR-0009).
//!
//! Subcommands arrive with the vertical slice; this is the entry point so the
//! workspace builds and the layering can be checked from day one.

fn main() {
    println!("loongagent {} (schema {})", env!("CARGO_PKG_VERSION"), loong_schema::SCHEMA_VERSION);
}
