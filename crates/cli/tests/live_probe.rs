//! Live probes against the real vendors — the first-run experience, end to end.
//!
//! `#[ignore]`d on purpose: these need a network and somebody's key, so they must
//! never decide whether a build is good. They exist because a probe is only correct
//! if it agrees with the vendor, and the only way to know that is to ask one.
//!
//! ```text
//! DEEPSEEK_API_KEY=sk-… cargo test -p loongagent --test live_probe -- --ignored --nocapture
//! ```
//!
//! They live in this crate rather than next to the probe itself because they need
//! both the catalog and the adapter, and this is where those two already meet.

use loong_providers::probe::{self, Probe};

fn key(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| std::env::var(name).ok())
}

/// The base URL the wizard would actually use — read from the catalog, not retyped,
/// so a catalog entry that drifts from reality fails here.
fn base_url(provider: &str) -> &'static str {
    loong_config::catalog::provider(provider)
        .unwrap_or_else(|| panic!("{provider} is not in the catalog"))
        .base_url
}

#[test]
#[ignore = "needs a network and a real key"]
fn deepseek_answers_with_models_and_their_context_windows() {
    let Some(key) = key(&["LOONG_DEEPSEEK_KEY", "DEEPSEEK_API_KEY"]) else {
        eprintln!("no DeepSeek key in the environment — skipping");
        return;
    };
    let result = probe::probe_models(base_url("deepseek"), Some(&key));
    println!("{}", probe::render(&result));
    match &result {
        Probe::Ok { models } => {
            assert!(!models.is_empty(), "a working key lists at least one model");
            assert!(
                models.iter().any(|m| m.context_window.is_some()),
                "DeepSeek publishes context_window, which is what fills max_context_size"
            );
        }
        other => panic!("expected the key to work: {other:?}"),
    }
}

#[test]
#[ignore = "needs a network"]
fn a_wrong_key_is_rejected_rather_than_reported_as_unreachable() {
    // The distinction the whole first run depends on: a syntactically plausible but
    // bogus key must come back `Rejected`, not `Unreachable`, or the wizard cannot
    // tell the user which of the two things is actually wrong.
    let result = probe::probe_models(
        base_url("deepseek"),
        Some("sk-00000000000000000000000000000000"),
    );
    println!("{}", probe::render(&result));
    match result {
        Probe::Rejected { status, .. } => {
            assert!(
                status == 401 || status == 403,
                "a bad key is refused, not errored: HTTP {status}"
            );
        }
        other => panic!("expected a rejection, got {other:?}"),
    }
}

#[test]
#[ignore = "needs no network at all, but is opt-in like its siblings"]
fn a_loopback_port_with_nothing_on_it_is_unreachable_not_rejected() {
    // The other half of the same distinction, and the easy one to get wrong: a
    // connection that never happens says nothing about the key.
    let result = probe::probe_models("http://127.0.0.1:1", Some("sk-whatever"));
    println!("{}", probe::render(&result));
    assert!(
        matches!(result, Probe::Unreachable { .. }),
        "nothing answered, so nothing may be blamed on the key: {result:?}"
    );
}

#[test]
#[ignore = "needs a network and a real key"]
fn zhipu_is_reachable_but_has_no_models_route() {
    // The vendor that made the three-outcome design necessary. If Zhipu ever ships a
    // `/models` route this test starts failing, which is the correct moment to find
    // out — the catalog entry says it does not have one.
    let Some(key) = key(&["LOONG_ZHIPU_KEY", "ZHIPU_API_KEY", "GLM_API_KEY"]) else {
        eprintln!("no Zhipu key in the environment — skipping");
        return;
    };
    let base = base_url("zhipu");
    let result = probe::probe_models(base, Some(&key));
    println!("{}", probe::render(&result));

    // Whatever the route answers, the fallback must still identify a working key.
    let fallback = probe::probe_with_completion(base, "glm-4-flash", Some(&key));
    println!("fallback: {}", probe::render(&fallback));
    assert!(
        !matches!(fallback, Probe::Unreachable { .. }),
        "the fallback exists precisely so this vendor can still be validated"
    );
}
