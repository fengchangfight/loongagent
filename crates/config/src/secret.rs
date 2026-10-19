//! Purpose: Check that a secret is a reference and an endpoint is an http(s) URL or a reference.
//!
//! # Contract
//! - **Serves:** layer loading and pack verification; keeps secrets out of configuration.
//! - **Design:** `docs/design/config-routing.md` §2, §3 and §9 (ADR-0008 §5).
//! - **Upstream:** `LayerValues::from_file` and `profiles_from_payload`.
//! - **Downstream:** `ConfigError` at the first bad value.
//! - **Verify:** `cargo test -p loong-config` · `./scripts/check.sh`
//!
use super::*;

/// Purpose: Refuse a literal secret and accept only a well-formed reference.
/// Serves: every local file and every pack reference; a key pasted after `env:` is refused.
/// Design: `docs/design/config-routing.md` §2 (ADR-0008 §5).
/// Up: `LayerValues::from_file` and `profiles_from_payload` through `check_pack_secret`.
/// Down: `is_secret_name`.
/// Verify: `cargo test -p loong-config`.
pub(super) fn check_secret_reference(
    source: &str,
    key: &str,
    value: &str,
) -> Result<(), ConfigError> {
    let reference = || {
        format!(
            "{source}: `{key}` must be a reference — `env:NAME`, `file:PATH#KEY`, `keychain:…`, \
             `gateway:…` — because secrets never live in configuration (ADR-0008 §5)"
        )
    };
    // A prefix is not enough. Writing the secret *after* the prefix
    // (`env:sk-4f06…`) satisfies "starts with env:" and puts the key in the pack, the
    // client cache, and — since a draft is meant to be committed — in git. So the name
    // itself is checked: an environment variable name, conventionally upper case.
    if let Some(name) = value.strip_prefix("env:") {
        return if is_secret_name(name) {
            Ok(())
        } else {
            Err(ConfigError::Invalid(format!(
                "{} — the part after `env:` must be the *name* of a variable (e.g. \
                 `env:DEEPSEEK_API_KEY`), not the secret itself: {name:?} is not a variable name",
                reference()
            )))
        };
    }
    if let Some(rest) = value.strip_prefix("file:") {
        // `file:PATH` (the file is the secret) or `file:PATH#NAME`.
        return match rest.split_once('#') {
            Some((path, name)) if !path.is_empty() && is_secret_name(name) => Ok(()),
            Some((_, name)) => Err(ConfigError::Invalid(format!(
                "{} — the part after `#` names a key inside the file (e.g. \
                 `file:credentials.toml#DEEPSEEK_API_KEY`), not the secret: {name:?}",
                reference()
            ))),
            None if !rest.is_empty() => Ok(()),
            None => Err(ConfigError::Invalid(reference())),
        };
    }
    for prefix in ["keychain:", "gateway:"] {
        if let Some(rest) = value.strip_prefix(prefix) {
            return if !rest.is_empty()
                && rest
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_./:-".contains(c))
            {
                Ok(())
            } else {
                Err(ConfigError::Invalid(reference()))
            };
        }
    }
    Err(ConfigError::Invalid(reference()))
}

/// An environment variable name, and nothing that looks like a value: letters, digits
/// and underscores, starting with a letter, conventionally upper case.
///
/// Deliberately strict. `sk-4f06…` fails on the hyphen and the case; a pasted
/// alphanumeric secret fails because a long unbroken run of characters with no
/// underscore is not how anybody names a variable.
/// Purpose: Whether a string is an environment-variable name, not a value.
/// Serves: `check_secret_reference`'s strictness against a pasted key.
/// Design: `docs/design/config-routing.md` §2.
/// Up: `check_secret_reference`.
/// Down: no callee; the character rules are the decision.
/// Verify: `cargo test -p loong-config`.
fn is_secret_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 {
        return false;
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_uppercase() && first != '_' {
        return false;
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    {
        return false;
    }
    // `DEEPSEEK_API_KEY` has words separated by underscores; a secret does not.
    !name.split('_').any(|word| word.len() >= 20)
}

/// A secret in a **pack**: a reference must be well formed, and a bare literal is
/// accepted — visibly. The pack is the distribution channel, and until identity and the
/// model gateway exist (ADR-0012 §4/§5) a firm may choose to ship the credential with
/// it. The warning is the price of that choice, repeated at publish, at load, and in
/// `GET /v1/health`.
///
/// A *malformed* reference is still refused: `env:sk-4f06…` is not a name, and
/// accepting it would silently look up a variable called `sk-4f06…`.
/// Purpose: Validate a pack's key reference, accepting a literal only with a visible warning.
/// Serves: `profiles_from_payload`; the pack is the one recorded exception.
/// Design: `docs/design/config-routing.md` §9.
/// Up: `profiles_from_payload`.
/// Down: `check_secret_reference` and the warnings list.
/// Verify: `cargo test -p loong-config`.
pub(super) fn check_pack_secret(
    key: &str,
    value: &str,
    warnings: &mut Vec<String>,
) -> Result<(), ConfigError> {
    let known = ["env:", "file:", "keychain:", "gateway:"];
    if known.iter().any(|prefix| value.starts_with(prefix)) {
        return check_secret_reference("the pack", key, value);
    }
    warnings.push(format!(
        "`{key}` carries the secret itself: it travels inside the signed pack, sits in \
         plaintext in every install's cache, and is copied by any backup of either. \
         References (`env:NAME`, `file:PATH#KEY`) are what ADR-0008 §5 asks for; this is \
         the recorded, temporary exception."
    ));
    Ok(())
}

/// Purpose: Require a `base_url` to be an http(s) URL or a reference, not a bare host.
/// Serves: every endpoint key; the project layer may not choose one.
/// Design: `docs/design/config-routing.md` §3.
/// Up: `LayerValues::from_file`.
/// Down: no callee; the prefix check is the decision.
/// Verify: `cargo test -p loong-config`.
pub(super) fn check_endpoint_reference(
    source: &str,
    key: &str,
    value: &str,
) -> Result<(), ConfigError> {
    if key == "base_url" {
        let ok = value.starts_with("http://")
            || value.starts_with("https://")
            || value.starts_with("env:")
            || value.starts_with("gateway:");
        if !ok {
            return Err(ConfigError::Invalid(format!(
                "{source}: base_url must be an http(s) URL or a reference \
                 (env:NAME, gateway:…), got {value:?}"
            )));
        }
    }
    Ok(())
}
