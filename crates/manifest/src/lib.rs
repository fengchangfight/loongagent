//! Purpose: Gate repository manifests behind a human trust decision and a change-detection hash.
//!
//! # Contract
//! - **Serves:** hooks and MCP servers stay inert until a human trusts their exact bytes (security-trust.md §1, config-routing.md §6).
//! - **Design:** `docs/design/security-trust.md` (ADR-0007); `docs/design/config-routing.md` §6 owns the trust record.
//! - **Upstream:** `loong-hooks` and `loong-mcp` call `check` before a manifest becomes active.
//! - **Downstream:** FNV-1a over the file bytes and the `hooks.trusted` / `mcp.trusted` record.
//! - **Verify:** `cargo test -p loong-manifest` · `./scripts/check.sh`
//!
//! Manifest trust: a repository's configuration is **content from that
//! repository**, and some of it executes.
//!
//! Hooks and MCP servers both name commands to run, so both need the same gate:
//! stay inert until a human trusts the exact bytes, and re-arm the question when
//! any of those bytes change.
//!
//! The record is a **change-detection hash, not a signature**. That distinction is
//! load-bearing and is repeated in the user-facing output: its job is "this
//! changed since you looked", and the security property comes from the human
//! re-answering, not from the digest's collision resistance. Signatures arrive
//! with ADR-0008's pack signing.

use std::path::{Path, PathBuf};

/// `(name shown to the human, file whose bytes matter)`.
/// Purpose: The named files whose bytes a trust record covers, as `(name, path)` pairs.
///
/// Serves: one manifest file plus every script it names (config-routing.md §6).
/// Design: `docs/design/security-trust.md` (ADR-0007).
/// Up: `check`, `record` and `inert_message`.
/// Down: `PathBuf` for each file.
/// Verify: `cargo test -p loong-manifest`.
pub type Sources = Vec<(String, PathBuf)>;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Purpose: Whether a manifest is absent, trusted, or untrusted with the names that changed.
///
/// Serves: a caller that keeps a manifest inert until a human trusts the exact bytes (security-trust.md §1).
/// Design: `docs/design/security-trust.md` (ADR-0007).
/// Up: `check` and the CLI's trust commands.
/// Down: the recorded hash lines.
/// Verify: `cargo test -p loong-manifest`.
pub enum TrustStatus {
    /// Nothing to trust: the manifest does not exist.
    Absent,
    Trusted,
    /// New, changed, or never recorded.
    Untrusted {
        changed: Vec<String>,
    },
}

/// FNV-1a over the bytes. Stable, dependency-free, and explicitly not
/// cryptographic — see the module docs.
/// Purpose: Hash bytes with FNV-1a, a stable change detector that is explicitly not cryptographic.
///
/// Serves: a hash that only answers whether the bytes changed (config-routing.md §6).
/// Design: `docs/design/security-trust.md` (ADR-0007).
/// Up: `hash_file` and the fingerprint test.
/// Down: the FNV-1a constants.
/// Verify: `cargo test -p loong-manifest`.
pub fn fingerprint(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Purpose: Hash one file's bytes and format the digest as hex.
///
/// Serves: the value written to a `.trusted` record.
/// Design: `docs/design/security-trust.md` (ADR-0007).
/// Up: `check` and `record`.
/// Down: `std::fs::read` and `fingerprint`.
/// Verify: `cargo test -p loong-manifest`.
pub fn hash_file(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(format!("{:016x}", fingerprint(&bytes)))
}

/// Which of `sources` are covered by the trust record.
/// Purpose: Compare the sources' hashes with the trust record and report what changed.
///
/// Serves: the gate that keeps hooks and MCP servers inert (security-trust.md §1).
/// Design: `docs/design/security-trust.md` (ADR-0007); `docs/design/config-routing.md` §6.
/// Up: `loong-hooks` and `loong-mcp` before a manifest becomes active.
/// Down: `hash_file` and the `{hash} {name}` record lines.
/// Verify: `cargo test -p loong-manifest`.
pub fn check(trust_file: &Path, sources: &Sources) -> Result<TrustStatus, String> {
    if sources.is_empty() {
        return Ok(TrustStatus::Absent);
    }
    let recorded = std::fs::read_to_string(trust_file).unwrap_or_default();
    let recorded: Vec<&str> = recorded
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();

    let mut changed = Vec::new();
    for (name, path) in sources {
        let hash = hash_file(path)?;
        if !recorded.contains(&format!("{hash} {name}").as_str()) {
            changed.push(name.clone());
        }
    }
    Ok(if changed.is_empty() {
        TrustStatus::Trusted
    } else {
        TrustStatus::Untrusted { changed }
    })
}

/// Record the current bytes as trusted. The explicit, per-repository opt-in.
/// Purpose: Write the current bytes' hashes as the explicit trust record.
///
/// Serves: the human trust decision, recorded on disk (security-trust.md §1).
/// Design: `docs/design/security-trust.md` (ADR-0007).
/// Up: the `loong-hooks` and `loong-mcp` trust writers behind the CLI's `hooks trust` and `mcp trust` commands.
/// Down: `hash_file` and `std::fs::write`.
/// Verify: `cargo test -p loong-manifest`.
pub fn record(trust_file: &Path, sources: &Sources) -> Result<(), String> {
    let mut text = String::from(
        "# loongagent trust\n\
         # A change detector, not a signature: editing a listed file re-arms the question.\n",
    );
    for (name, path) in sources {
        text.push_str(&format!("{} {}\n", hash_file(path)?, name));
    }
    std::fs::write(trust_file, text).map_err(|e| format!("{}: {e}", trust_file.display()))
}

/// The line a surface prints when something is inert.
/// Purpose: The line a surface prints when a manifest is inert, naming the changed files.
///
/// Serves: the user sees why the hooks or MCP servers are not running (config-routing.md §6).
/// Design: `docs/design/security-trust.md` (ADR-0007).
/// Up: `loong-mcp`'s config when it reports an untrusted manifest to a surface.
/// Down: `changed` and the `how_to_trust` hint.
/// Verify: `cargo test -p loong-manifest`.
pub fn inert_message(sources: &Sources, changed: &[String], how_to_trust: &str) -> String {
    let _ = sources;
    format!("INERT — untrusted: {} ({how_to_trust})", changed.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    /// Purpose: Create a unique temporary directory for a test.
    ///
    /// Contract: `crates/manifest/src/lib.rs`.
    fn dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "loong-manifest-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Purpose: Write named files into a test directory and return their `Sources`.
    ///
    /// Contract: `crates/manifest/src/lib.rs`.
    fn sources(dir: &Path, files: &[(&str, &str)]) -> Sources {
        files
            .iter()
            .map(|(name, body)| {
                let path = dir.join(name);
                std::fs::write(&path, body).unwrap();
                (name.to_string(), path)
            })
            .collect()
    }

    /// Purpose: Pins that no sources means absent, not trusted.
    #[test]
    fn nothing_to_trust_is_absent_not_trusted() {
        let d = dir();
        assert_eq!(
            check(&d.join("x.trusted"), &Sources::new()).unwrap(),
            TrustStatus::Absent
        );
    }

    /// Purpose: Pins that a manifest is untrusted until recorded, then trusted.
    #[test]
    fn a_manifest_is_untrusted_until_recorded() {
        let d = dir();
        let trust = d.join("m.trusted");
        let sources = sources(&d, &[("m.toml", "a = 1"), ("s.sh", "exit 0")]);
        assert!(matches!(
            check(&trust, &sources).unwrap(),
            TrustStatus::Untrusted { .. }
        ));

        record(&trust, &sources).unwrap();
        assert_eq!(check(&trust, &sources).unwrap(), TrustStatus::Trusted);
    }

    /// Purpose: Pins that changing any listed byte re-arms the question.
    #[test]
    fn changing_any_listed_byte_re_arms_the_question() {
        let d = dir();
        let trust = d.join("m.trusted");
        let sources = sources(&d, &[("m.toml", "a = 1"), ("s.sh", "exit 0")]);
        record(&trust, &sources).unwrap();

        std::fs::write(d.join("s.sh"), "exit 2").unwrap();
        match check(&trust, &sources).unwrap() {
            TrustStatus::Untrusted { changed } => assert_eq!(changed, vec!["s.sh".to_string()]),
            other => panic!("expected untrusted, got {other:?}"),
        }
    }

    /// Purpose: Pins that adding a source invalidates the record for that source only.
    #[test]
    fn adding_a_source_invalidates_the_record_for_that_source_only() {
        let d = dir();
        let trust = d.join("m.trusted");
        let first = sources(&d, &[("m.toml", "a = 1")]);
        record(&trust, &first).unwrap();

        let both = sources(&d, &[("m.toml", "a = 1"), ("s.sh", "exit 0")]);
        match check(&trust, &both).unwrap() {
            TrustStatus::Untrusted { changed } => assert_eq!(changed, vec!["s.sh".to_string()]),
            other => panic!("expected untrusted, got {other:?}"),
        }
    }

    /// Purpose: Pins that a missing source file is an error, not a silent pass.
    #[test]
    fn a_missing_source_file_is_an_error_not_a_silent_pass() {
        let d = dir();
        let trust = d.join("m.trusted");
        let sources = vec![("gone.sh".to_string(), d.join("gone.sh"))];
        assert!(check(&trust, &sources).is_err());
    }

    /// Purpose: Pins that the hash is stable and order-sensitive.
    #[test]
    fn the_hash_is_stable_and_order_sensitive() {
        assert_eq!(fingerprint(b"abc"), fingerprint(b"abc"));
        assert_ne!(fingerprint(b"abc"), fingerprint(b"acb"));
        assert_eq!(fingerprint(b""), 0xcbf2_9ce4_8422_2325);
    }
}
