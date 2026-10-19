//! Purpose: Sign and verify the one envelope every distributed artifact travels in.
//!
//! # Contract
//! - **Serves:** a pack can occupy the `control` layer only after its signature verifies under the pinned key (security-trust.md §1, config-routing.md §4).
//! - **Design:** `docs/design/security-trust.md` (ADR-0016); `docs/design/config-routing.md` §4 owns the layer.
//! - **Upstream:** `loong-control` verifies a fetched pack; the control plane signs it.
//! - **Downstream:** `ring` Ed25519, `Envelope::signing_bytes` and the pinned `key_id`.
//! - **Verify:** `cargo test -p loong-pack` · `./scripts/check.sh`
//!
//! The one signed envelope every distributed artifact travels in.
//!
//! ADR-0008 §6: profiles, policy and context packs share **one** envelope, and the
//! local install verifies the signature *before* the pack can occupy the org layer.
//! ADR-0012 §1 goes further and requires the sign/verify path to live in a crate
//! both binaries depend on: the control plane and the local agent are deployed
//! independently, so a duplicated verifier would eventually "reject valid packs or
//! accept invalid ones". That is why this crate exists rather than a function in
//! either side.
//!
//! Two rules the code enforces rather than documents:
//!
//! - the signature covers **everything except the signature**: artifact, schema,
//!   version, issuer, scope, issue time, key id and payload. A pack cannot be
//!   replayed as a different artifact or as a different version.
//! - verification takes the public key the caller **pinned**, and checks the
//!   envelope's `key_id` against it. A pack signed by some other key is refused
//!   even if that key is otherwise valid.

use ring::digest;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};
use serde::{Deserialize, Serialize};

/// The artifact id for model profiles (T-07). Policy and context packs will use
/// their own ids in the same envelope.
pub const PROFILES_ARTIFACT: &str = "loongagent.profiles";
/// The payload schema id for profiles; readers refuse what they do not understand.
pub const PROFILES_SCHEMA: &str = "loongagent.profiles/1";

#[derive(Debug)]
/// Purpose: Why a pack is malformed, signed by the wrong key, or unreadable to this build.
///
/// Serves: verification refuses a pack before it can become policy (config-routing.md §4).
/// Design: `docs/design/security-trust.md` (ADR-0016).
/// Up: `Envelope::verify`, `sign`, `to_json` and `from_json`.
/// Down: the envelope fields and the pinned key id.
/// Verify: `cargo test -p loong-pack`.
pub enum PackError {
    /// The envelope is malformed, or a field failed its own check.
    Malformed(String),
    /// The signature does not verify under the pinned key.
    BadSignature,
    /// Signed by a key other than the one this install pinned.
    WrongKey {
        expected: String,
        found: String,
    },
    /// A payload this reader does not understand. Refusing is the point.
    UnsupportedSchema {
        found: String,
        supported: Vec<String>,
    },
    Crypto(String),
    Json(String),
}

impl std::fmt::Display for PackError {
    /// Purpose: Render each verification failure as the sentence a user or the control plane reads.
    ///
    /// Serves: an error that says which check failed, not just that verification did.
    /// Design: `docs/design/security-trust.md` (ADR-0016).
    /// Up: `Display` for `PackError`.
    /// Down: the variants of `PackError`.
    /// Verify: `cargo test -p loong-pack`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackError::Malformed(m) => write!(f, "malformed envelope: {m}"),
            PackError::BadSignature => write!(
                f,
                "the signature does not verify under the pinned key — the pack was altered, \
                 or it was signed by a different key"
            ),
            PackError::WrongKey { expected, found } => write!(
                f,
                "the pack is signed by key {found}, but this install pinned {expected}"
            ),
            PackError::UnsupportedSchema { found, supported } => write!(
                f,
                "the pack uses payload schema {found:?}; this build understands {}",
                supported.join(", ")
            ),
            PackError::Crypto(m) => write!(f, "cryptography: {m}"),
            PackError::Json(m) => write!(f, "json: {m}"),
        }
    }
}

impl std::error::Error for PackError {}

/// The envelope, as it travels (JSON on the wire) and as it is cached on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Purpose: The signed artifact envelope: what it is, who signed it, the payload and the signature.
///
/// Serves: every distributed artifact travels in one verifiable shape (security-trust.md §1).
/// Design: `docs/design/security-trust.md` (ADR-0016); `docs/design/config-routing.md` §4.
/// Up: `loong-control` fetches and verifies one; the control plane signs one.
/// Down: `signing_bytes`, `sign` and `verify`.
/// Verify: `cargo test -p loong-pack`.
pub struct Envelope {
    /// What this is, e.g. `loongagent.profiles`.
    pub artifact: String,
    /// How to read the payload. A reader refuses schemas it does not know.
    pub payload_schema: String,
    /// Semantic version, chosen by the signing administrator.
    pub version: String,
    /// Who signed it. Shown to users, and recorded in the session log.
    pub issuer: String,
    /// Who it is for (`org`, `team:platform`).
    pub scope: String,
    pub issued_at_ms: u64,
    /// Truncated SHA-256 of the signing key, so rotation is possible.
    pub key_id: String,
    /// The artifact itself — TOML text for profiles.
    pub payload: String,
    /// Ed25519 over [`Envelope::signing_bytes`], hex.
    pub signature: String,
}

impl Envelope {
    /// The bytes the signature covers: canonical JSON of every field except
    /// `signature`, in a fixed order, with no insignificant whitespace.
    ///
    /// Built by hand rather than by serializing the struct: `serde_json`'s map
    /// ordering is stable here, but a field added later must not silently change
    /// what older signatures covered, and an explicit list makes that visible.
    /// Purpose: The canonical bytes the signature covers: every field except the signature.
    ///
    /// Serves: a field added later cannot silently change what older signatures covered (config-routing.md §4).
    /// Design: `docs/design/security-trust.md` (ADR-0016).
    /// Up: `sign` and `verify`.
    /// Down: the explicit JSON field list.
    /// Verify: `cargo test -p loong-pack`.
    pub fn signing_bytes(&self) -> Vec<u8> {
        let canonical = serde_json::json!({
            "artifact": self.artifact,
            "payload_schema": self.payload_schema,
            "version": self.version,
            "issuer": self.issuer,
            "scope": self.scope,
            "issued_at_ms": self.issued_at_ms,
            "key_id": self.key_id,
            "payload": self.payload,
        });
        canonical.to_string().into_bytes()
    }

    /// Sign a payload with an Ed25519 private key in PKCS#8 form.
    /// Purpose: Sign a payload with an Ed25519 private key in PKCS#8 form.
    ///
    /// Serves: the control plane minting a pack the client can pin (config-routing.md §4).
    /// Design: `docs/design/security-trust.md` (ADR-0016).
    /// Up: the control plane's publish path and the pack tests.
    /// Down: `Ed25519KeyPair`, `signing_bytes` and `key_id`.
    /// Verify: `cargo test -p loong-pack`.
    pub fn sign(
        key_pkcs8: &[u8],
        artifact: &str,
        payload_schema: &str,
        version: &str,
        issuer: &str,
        scope: &str,
        issued_at_ms: u64,
        payload: &str,
    ) -> Result<Self, PackError> {
        let pair = Ed25519KeyPair::from_pkcs8(key_pkcs8)
            .map_err(|e| PackError::Crypto(format!("reading the private key: {e}")))?;
        let key_id = key_id(pair.public_key().as_ref());
        let mut envelope = Envelope {
            artifact: artifact.to_string(),
            payload_schema: payload_schema.to_string(),
            version: version.to_string(),
            issuer: issuer.to_string(),
            scope: scope.to_string(),
            issued_at_ms,
            key_id,
            payload: payload.to_string(),
            signature: String::new(),
        };
        let signature = pair.sign(&envelope.signing_bytes());
        envelope.signature = hex(signature.as_ref());
        Ok(envelope)
    }

    /// Verify against the **pinned** public key.
    ///
    /// `expected_schema` is what this build understands; a pack for a schema it does
    /// not know is refused before the signature is even checked, because "valid
    /// signature, unknown content" must never be treated as usable.
    /// Purpose: Verify the envelope against the caller's pinned public key and expected schema.
    ///
    /// Serves: a valid signature under some other key, or an unknown payload, is refused (config-routing.md §4).
    /// Design: `docs/design/security-trust.md` (ADR-0016).
    /// Up: `loong-control` and the cache load.
    /// Down: `key_id`, `signing_bytes` and `UnparsedPublicKey::verify`.
    /// Verify: `cargo test -p loong-pack`.
    pub fn verify(&self, public_key: &[u8], expected_schema: &[&str]) -> Result<(), PackError> {
        if self.signature.is_empty() {
            return Err(PackError::Malformed("no signature".to_string()));
        }
        if !expected_schema.contains(&self.payload_schema.as_str()) {
            return Err(PackError::UnsupportedSchema {
                found: self.payload_schema.clone(),
                supported: expected_schema.iter().map(|s| s.to_string()).collect(),
            });
        }
        let found = key_id(public_key);
        if found != self.key_id {
            return Err(PackError::WrongKey {
                expected: found,
                found: self.key_id.clone(),
            });
        }
        let signature = unhex(&self.signature)
            .ok_or_else(|| PackError::Malformed("signature is not hex".into()))?;
        let key = UnparsedPublicKey::new(&ED25519, public_key);
        key.verify(&self.signing_bytes(), &signature)
            .map_err(|_| PackError::BadSignature)
    }

    /// Purpose: Serialize the envelope for the wire or the cache.
    ///
    /// Serves: one JSON shape for transport and disk.
    /// Design: `docs/design/security-trust.md` (ADR-0016).
    /// Up: the control-plane client and the cache writer.
    /// Down: `serde_json::to_string`.
    /// Verify: `cargo test -p loong-pack`.
    pub fn to_json(&self) -> Result<String, PackError> {
        serde_json::to_string(self).map_err(|e| PackError::Json(e.to_string()))
    }

    /// Purpose: Parse a cached or fetched envelope and reject an empty artifact or version.
    ///
    /// Serves: a malformed envelope is refused before verification (config-routing.md §4).
    /// Design: `docs/design/security-trust.md` (ADR-0016).
    /// Up: the control-plane client when it reads the cache.
    /// Down: `serde_json::from_str` and the required-field check.
    /// Verify: `cargo test -p loong-pack`.
    pub fn from_json(text: &str) -> Result<Self, PackError> {
        let envelope: Envelope =
            serde_json::from_str(text).map_err(|e| PackError::Json(e.to_string()))?;
        if envelope.artifact.is_empty() || envelope.version.is_empty() {
            return Err(PackError::Malformed(
                "artifact and version are required".to_string(),
            ));
        }
        Ok(envelope)
    }
}

/// A short, stable fingerprint of a public key: the first eight bytes of its
/// SHA-256, hex. Short enough to read aloud, long enough that a collision is not a
/// practical concern for pinning.
/// Purpose: A short fingerprint of a public key: the first eight bytes of its SHA-256, hex.
///
/// Serves: key rotation and pinning without reading the whole key (config-routing.md §4).
/// Design: `docs/design/security-trust.md` (ADR-0016).
/// Up: `sign`, `verify` and the control plane's key output.
/// Down: `ring::digest` and `hex`.
/// Verify: `cargo test -p loong-pack`.
pub fn key_id(public_key: &[u8]) -> String {
    let digest = digest::digest(&digest::SHA256, public_key);
    hex(&digest.as_ref()[..8])
}

/// Generate an Ed25519 keypair as PKCS#8. The caller writes the private half with
/// mode 0600 and never sends it anywhere.
/// Purpose: Generate an Ed25519 keypair as PKCS#8 and a public key.
///
/// Serves: the control plane's `keygen` command (control-plane.md).
/// Design: `docs/design/security-trust.md` (ADR-0016).
/// Up: the control-plane binary and the pack tests.
/// Down: `Ed25519KeyPair::generate_pkcs8` and the system random source.
/// Verify: `cargo test -p loong-pack`.
pub fn generate_keypair() -> Result<(Vec<u8>, Vec<u8>), PackError> {
    let random = SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&random)
        .map_err(|e| PackError::Crypto(format!("generating a key: {e}")))?;
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
        .map_err(|e| PackError::Crypto(format!("reading the key we just made: {e}")))?;
    Ok((pkcs8.as_ref().to_vec(), pair.public_key().as_ref().to_vec()))
}

/// The public half of a PKCS#8 private key, so `sign` and `keygen` agree on what
/// the pinned key will be.
/// Purpose: The public half of a PKCS#8 private key.
///
/// Serves: `sign` and `keygen` agree on which key a client pins (config-routing.md §4).
/// Design: `docs/design/security-trust.md` (ADR-0016).
/// Up: the control-plane key output and the pack tests.
/// Down: `Ed25519KeyPair::from_pkcs8`.
/// Verify: `cargo test -p loong-pack`.
pub fn public_of(key_pkcs8: &[u8]) -> Result<Vec<u8>, PackError> {
    let pair = Ed25519KeyPair::from_pkcs8(key_pkcs8)
        .map_err(|e| PackError::Crypto(format!("reading the private key: {e}")))?;
    Ok(pair.public_key().as_ref().to_vec())
}

/// Purpose: Render bytes as lowercase hex.
///
/// Serves: the signature and key id text forms (config-routing.md §4).
/// Design: `docs/design/security-trust.md` (ADR-0016).
/// Up: `sign`, `key_id` and the hex test.
/// Down: `format!` per byte.
/// Verify: `cargo test -p loong-pack`.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Purpose: Parse lowercase hex into bytes, returning `None` on an odd length or bad digit.
///
/// Serves: a non-hex signature is refused before it is verified (config-routing.md §4).
/// Design: `docs/design/security-trust.md` (ADR-0016).
/// Up: `verify` and the hex test.
/// Down: `u8::from_str_radix`.
/// Verify: `cargo test -p loong-pack`.
pub fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Purpose: Generate a keypair and sign a sample payload for a test.
    ///
    /// Serves: the pack tests share one signing fixture.
    /// Design: `docs/design/security-trust.md` (ADR-0016).
    /// Up: every signing test in this module.
    /// Down: `generate_keypair` and `Envelope::sign`.
    /// Verify: `cargo test -p loong-pack`.
    fn signed(version: &str, payload: &str) -> (Vec<u8>, Envelope) {
        let (private, public) = generate_keypair().unwrap();
        let envelope = Envelope::sign(
            &private,
            PROFILES_ARTIFACT,
            PROFILES_SCHEMA,
            version,
            "acme-platform",
            "org",
            1_791_500_000_000,
            payload,
        )
        .unwrap();
        (public, envelope)
    }

    /// Purpose: Pins that a signed pack verifies under its own key and survives JSON.
    #[test]
    fn a_signed_pack_verifies_under_its_own_key() {
        let (public, envelope) = signed("2026.10.09-1", "default_profile = \"default\"\n");
        envelope.verify(&public, &[PROFILES_SCHEMA]).unwrap();
        assert_eq!(envelope.key_id, key_id(&public));
        assert_eq!(envelope.issuer, "acme-platform");

        // It survives the trip to disk and back as JSON.
        let json = envelope.to_json().unwrap();
        let back = Envelope::from_json(&json).unwrap();
        assert_eq!(back, envelope);
        back.verify(&public, &[PROFILES_SCHEMA]).unwrap();
    }

    /// Purpose: Pins that changing any field the signature covers makes verification fail.
    #[test]
    fn any_field_the_signature_covers_can_not_be_changed() {
        let (public, envelope) = signed("v1", "payload");
        // The interesting cases: a changed payload, a replayed version, a pack
        // presented as another artifact or for another scope.
        for mutate in [
            (|e: &mut Envelope| e.payload = "other".into()) as fn(&mut Envelope),
            |e: &mut Envelope| e.version = "v2".into(),
            |e: &mut Envelope| e.artifact = "loongagent.policy".into(),
            |e: &mut Envelope| e.scope = "team:other".into(),
            |e: &mut Envelope| e.issuer = "somebody-else".into(),
            |e: &mut Envelope| e.key_id = "0000000000000000".into(),
            |e: &mut Envelope| e.issued_at_ms += 1,
        ] {
            let mut tampered = envelope.clone();
            mutate(&mut tampered);
            assert!(
                tampered.verify(&public, &[PROFILES_SCHEMA]).is_err(),
                "a changed field must not verify"
            );
        }
    }

    /// Purpose: Pins that a valid pack signed by another key is refused.
    #[test]
    fn a_pack_signed_by_another_key_is_refused_even_though_it_is_valid() {
        let (_, theirs) = signed("v1", "their payload");
        let (ours, _) = signed("v1", "our payload");
        match theirs.verify(&ours, &[PROFILES_SCHEMA]) {
            Err(PackError::WrongKey { expected, found }) => {
                assert_eq!(expected, key_id(&ours));
                assert_eq!(found, theirs.key_id);
            }
            other => panic!("expected WrongKey, got {other:?}"),
        }
    }

    /// Purpose: Pins that a tampered payload and a garbage signature fail the signature check.
    #[test]
    fn a_signature_from_a_tampered_payload_does_not_verify() {
        // Same key_id, so this is the signature check itself, not the key check.
        let (public, envelope) = signed("v1", "payload");
        let mut forgery = envelope.clone();
        forgery.payload = "payload ".to_string(); // one space
        assert!(matches!(
            forgery.verify(&public, &[PROFILES_SCHEMA]),
            Err(PackError::BadSignature)
        ));
        let mut garbage = envelope.clone();
        garbage.signature = "ff".repeat(64);
        assert!(matches!(
            garbage.verify(&public, &[PROFILES_SCHEMA]),
            Err(PackError::BadSignature)
        ));
    }

    /// Purpose: Pins that an unknown payload schema is refused before the signature check.
    #[test]
    fn an_unknown_payload_schema_is_refused_before_anything_else() {
        let (private, _) = generate_keypair().unwrap();
        let envelope = Envelope::sign(
            &private,
            "loongagent.policy",
            "loongagent.policy/9",
            "v1",
            "acme",
            "org",
            0,
            "whatever",
        )
        .unwrap();
        let public = public_of(&private).unwrap();
        match envelope.verify(&public, &[PROFILES_SCHEMA]) {
            Err(PackError::UnsupportedSchema { found, supported }) => {
                assert_eq!(found, "loongagent.policy/9");
                assert_eq!(supported, vec![PROFILES_SCHEMA.to_string()]);
            }
            other => panic!("expected UnsupportedSchema, got {other:?}"),
        }
    }

    /// Purpose: Pins that a pack with an empty signature is not verifiable.
    #[test]
    fn an_unsigned_pack_is_not_verifiable() {
        let (public, mut envelope) = signed("v1", "payload");
        envelope.signature = String::new();
        assert!(matches!(
            envelope.verify(&public, &[PROFILES_SCHEMA]),
            Err(PackError::Malformed(_))
        ));
    }

    /// Purpose: Pins that hex round-trips and refuses odd-length or non-hex input.
    #[test]
    fn hex_round_trips_and_refuses_nonsense() {
        assert_eq!(
            unhex(&hex(&[0x00, 0x0f, 0xff])).unwrap(),
            vec![0x00, 0x0f, 0xff]
        );
        assert!(unhex("abc").is_none(), "odd length");
        assert!(unhex("zz").is_none());
    }
}
