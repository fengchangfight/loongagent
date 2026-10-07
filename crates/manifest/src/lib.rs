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
pub type Sources = Vec<(String, PathBuf)>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustStatus {
    /// Nothing to trust: the manifest does not exist.
    Absent,
    Trusted,
    /// New, changed, or never recorded.
    Untrusted { changed: Vec<String> },
}

/// FNV-1a over the bytes. Stable, dependency-free, and explicitly not
/// cryptographic — see the module docs.
pub fn fingerprint(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

pub fn hash_file(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(format!("{:016x}", fingerprint(&bytes)))
}

/// Which of `sources` are covered by the trust record.
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
    Ok(if changed.is_empty() { TrustStatus::Trusted } else { TrustStatus::Untrusted { changed } })
}

/// Record the current bytes as trusted. The explicit, per-repository opt-in.
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
pub fn inert_message(sources: &Sources, changed: &[String], how_to_trust: &str) -> String {
    let _ = sources;
    format!(
        "INERT — untrusted: {} ({how_to_trust})",
        changed.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    fn dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "loong-manifest-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

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

    #[test]
    fn nothing_to_trust_is_absent_not_trusted() {
        let d = dir();
        assert_eq!(check(&d.join("x.trusted"), &Sources::new()).unwrap(), TrustStatus::Absent);
    }

    #[test]
    fn a_manifest_is_untrusted_until_recorded() {
        let d = dir();
        let trust = d.join("m.trusted");
        let sources = sources(&d, &[("m.toml", "a = 1"), ("s.sh", "exit 0")]);
        assert!(matches!(check(&trust, &sources).unwrap(), TrustStatus::Untrusted { .. }));

        record(&trust, &sources).unwrap();
        assert_eq!(check(&trust, &sources).unwrap(), TrustStatus::Trusted);
    }

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

    #[test]
    fn a_missing_source_file_is_an_error_not_a_silent_pass() {
        let d = dir();
        let trust = d.join("m.trusted");
        let sources = vec![("gone.sh".to_string(), d.join("gone.sh"))];
        assert!(check(&trust, &sources).is_err());
    }

    #[test]
    fn the_hash_is_stable_and_order_sensitive() {
        assert_eq!(fingerprint(b"abc"), fingerprint(b"abc"));
        assert_ne!(fingerprint(b"abc"), fingerprint(b"acb"));
        assert_eq!(fingerprint(b""), 0xcbf2_9ce4_8422_2325);
    }
}
