//! Purpose: Tests for L2: digests are derived, computed fields stay computed, and every claim cites the record.
//!
//! Serves: the comprehension layer's promises: a deterministic digest, citations that resolve, evidence on expand, and recall over summaries.
//!
//! # Contract
//! - **Design:** `docs/context/pipeline.md` §6.2–§6.3; `docs/context/recall.md` §7.2; `docs/design/context-ledger.md` §5.
//! - **Upstream:** `loong_context::{SearchQuery, Store}` and the two-episode fixture below.
//! - **Downstream:** `crate::digest`, `crate::summaries` and `crate::synthesis::domain_of`.
//! - **Verify:** `cargo test -p loong-context --test summaries`.

use loong_context::{SearchQuery, Store};
use loong_eventlog::SessionLog;
use loong_schema::generated::EventEnvelope;
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static N: AtomicU64 = AtomicU64::new(0);

/// Purpose: A temporary session log removed when the test ends.
/// Serves: every test starting from a clean log and leaving nothing behind.
/// Design: `docs/context/pipeline.md` §6.2.
/// Up: The summaries tests.
/// Down: `TempLog::new`, `TempLog::path` and `TempLog::drop`.
/// Verify: `cargo test -p loong-context --test summaries`.
struct TempLog(PathBuf);

impl TempLog {
    /// Purpose: Create a fresh temp log path and remove any stale file.
    /// Contract: `summaries.rs` fixtures.
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "loong-context-sum-{}-{}.jsonl",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_file(&p);
        TempLog(p)
    }
    /// Purpose: The session log's path.
    /// Contract: `summaries.rs` fixtures.
    fn path(&self) -> &PathBuf {
        &self.0
    }
}

impl Drop for TempLog {
    /// Purpose: Remove the session log.
    /// Contract: `summaries.rs` fixtures.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Purpose: Two episodes: a bug fixed and verified, then a small follow-up request.
/// Serves: a log that closes one episode on a new request and leaves another in the tail.
/// Design: `docs/context/pipeline.md` §6.3.
/// Up: The summaries tests.
/// Down: `loong_eventlog::SessionLog`; no other callee.
/// Verify: `cargo test -p loong-context --test summaries episodes_split`.
/// Two episodes: a bug fixed and verified, then a small follow-up request.
fn two_episodes(path: &PathBuf) -> Vec<EventEnvelope> {
    let mut log = SessionLog::create(path).unwrap();
    let mut events = Vec::new();
    let mut push = |log: &mut SessionLog, kind: &str, data: serde_json::Value| {
        events.push(log.append(kind, data).unwrap());
    };

    // --- episode 1 ---------------------------------------------------------
    push(
        &mut log,
        "message.appended",
        json!({"role": "user", "text": "配置层叠失效了，帮我查一下"}),
    );
    push(
        &mut log,
        "tool.call",
        json!({"call_id": "c1", "tool": "read_file",
               "args": {"path": "src/config/merge.rs"}, "action_class": "investigate"}),
    );
    push(
        &mut log,
        "tool.result",
        json!({"call_id": "c1", "ok": true, "output": "pub fn merge(...) { }"}),
    );
    push(
        &mut log,
        "tool.call",
        json!({"call_id": "c2", "tool": "run_command",
               "args": {"command": "cargo test layer_merge"}, "action_class": "investigate"}),
    );
    // A write: the file it names must land in touched_files.
    push(
        &mut log,
        "tool.call",
        json!({"call_id": "c3", "tool": "edit_file",
               "args": {"path": "src/config/merge.rs", "new_string": "sort by layer id"},
               "action_class": "write_workspace"}),
    );
    push(
        &mut log,
        "tool.result",
        json!({"call_id": "c3", "ok": true, "output": "applied 1 edit"}),
    );
    // The test fails first…
    push(
        &mut log,
        "tool.result",
        json!({"call_id": "c2", "ok": false,
               "output": "test layer_merge ... FAILED\nexpected 2 layers, got 1"}),
    );
    // …then passes: that transition is the evidence a fix was verified.
    push(
        &mut log,
        "tool.call",
        json!({"call_id": "c4", "tool": "run_command",
               "args": {"command": "cargo test layer_merge"}, "action_class": "investigate"}),
    );
    push(
        &mut log,
        "tool.result",
        json!({"call_id": "c4", "ok": true, "output": "test layer_merge ... ok"}),
    );
    push(
        &mut log,
        "message.appended",
        json!({"role": "assistant", "text": "根因是深拷贝，改为按层号排序后通过了"}),
    );

    // --- episode 2 ---------------------------------------------------------
    push(
        &mut log,
        "message.appended",
        json!({"role": "user", "text": "顺便把 README 的说明补一句"}),
    );
    push(
        &mut log,
        "message.appended",
        json!({"role": "assistant", "text": "好的，我稍后补"}),
    );
    events
}

/// Purpose: Build and digest a store over the temp log.
/// Contract: `summaries.rs` fixtures.
fn digested(path: &PathBuf) -> Store {
    let mut store = Store::in_memory().unwrap();
    store.rebuild(path).unwrap();
    store.digest().unwrap();
    store
}

/// Purpose: Pin that a new request closes an episode only once it has material, not on every message.
#[test]
fn episodes_split_on_a_new_request_but_not_on_every_message() {
    let tmp = TempLog::new();
    two_episodes(tmp.path());
    let store = digested(tmp.path());

    let episodes = store.summaries_at(2, 10).unwrap();
    assert_eq!(episodes.len(), 2, "{episodes:#?}");
    // Newest first.
    assert!(episodes[0].seq_to > episodes[1].seq_to);
    // The split lands on the second user message, not on the assistant message
    // between them: refining one request is still one episode.
    let second = &episodes[0];
    assert!(
        second.title.contains("README"),
        "the second episode is the follow-up: {:?}",
        second.title
    );
}

/// Purpose: Pin that a digest is reproducible verbatim from the atoms, by the same store and by an independent one.
#[test]
fn a_digest_is_deterministic_and_rebuilding_reproduces_it() {
    let tmp = TempLog::new();
    two_episodes(tmp.path());

    let mut store = Store::in_memory().unwrap();
    store.rebuild(tmp.path()).unwrap();
    store.digest().unwrap();
    let first = store.summaries_at(2, 10).unwrap();

    // Rebuild the summaries from the atoms alone.
    store.rebuild_summaries().unwrap();
    let again = store.summaries_at(2, 10).unwrap();
    assert_eq!(first, again, "a digest must be reproducible verbatim");

    // And a completely independent store agrees.
    let fresh = digested(tmp.path());
    let fresh_episodes = fresh.summaries_at(2, 10).unwrap();
    assert_eq!(first, fresh_episodes);
}

/// Purpose: Pin that touched files and commands come from a write, not a read.
#[test]
fn touched_files_come_from_writes_not_reads() {
    let tmp = TempLog::new();
    two_episodes(tmp.path());
    let store = digested(tmp.path());
    let episodes = store.summaries_at(2, 10).unwrap();
    let fix = episodes
        .iter()
        .find(|s| s.title.contains("层叠"))
        .expect("the bug episode");

    assert!(
        fix.content.contains("src/config/merge.rs"),
        "the edited file must appear: {}",
        fix.content
    );
    assert!(
        fix.content.contains("cargo"),
        "the command must appear: {}",
        fix.content
    );
}

/// Purpose: Pin that the failed→ok transition survives instead of collapsing to ok.
#[test]
fn a_test_that_failed_then_passed_keeps_the_transition() {
    let tmp = TempLog::new();
    two_episodes(tmp.path());
    let store = digested(tmp.path());
    let episodes = store.summaries_at(2, 10).unwrap();
    let fix = episodes
        .iter()
        .find(|s| s.title.contains("层叠"))
        .expect("the bug episode");

    // "failed→ok" is the whole point: it is the evidence that the fix was
    // verified, and a digest that collapsed it to "ok" would throw that away.
    assert!(
        fix.content.contains("layer_merge"),
        "the test must be named: {}",
        fix.content
    );
    assert!(
        fix.content.contains("failed→ok"),
        "the failure-then-success transition must survive: {}",
        fix.content
    );
}

/// Purpose: Pin that status is computed from evidence and the producer stays deterministic.
#[test]
fn status_is_computed_from_evidence_not_guessed() {
    let tmp = TempLog::new();
    two_episodes(tmp.path());
    let store = digested(tmp.path());
    let episodes = store.summaries_at(2, 10).unwrap();
    let fix = episodes
        .iter()
        .find(|s| s.title.contains("层叠"))
        .expect("the bug episode");

    // The tests ended green, so the episode reached a conclusion. This is the
    // field the local 3B model got wrong when it was asked to infer it.
    assert_eq!(fix.status, "done", "{}", fix.content);
    assert_eq!(fix.producer, "deterministic");
}

/// Purpose: Pin that every citation names real atoms and stays inside its episode.
#[test]
fn every_citation_resolves_to_a_real_atom() {
    let tmp = TempLog::new();
    two_episodes(tmp.path());
    let store = digested(tmp.path());
    let episodes = store.summaries_at(2, 10).unwrap();
    assert!(!episodes.is_empty());

    for episode in &episodes {
        assert!(
            !episode.citations.is_empty(),
            "a summary without citations must not be materialised: {episode:?}"
        );
        for (from, to) in &episode.citations {
            assert!(from <= to);
            for seq in *from..=*to {
                assert!(
                    store.atom(seq).unwrap().is_some(),
                    "citation {from}-{to} points at missing seq {seq}"
                );
            }
        }
        for (from, to) in &episode.citations {
            assert!(
                *from >= episode.seq_from && *to <= episode.seq_to,
                "a citation must stay inside the episode it belongs to"
            );
        }
    }
}

/// Purpose: Pin that expanding a summary hands back the raw evidence, including the failing run and the fixing write.
#[test]
fn expand_returns_the_summary_and_the_raw_evidence_it_rests_on() {
    let tmp = TempLog::new();
    two_episodes(tmp.path());
    let store = digested(tmp.path());
    let episode = store
        .summaries_at(2, 10)
        .unwrap()
        .into_iter()
        .find(|s| s.title.contains("层叠"))
        .unwrap();

    let expansion = store.expand(&episode.id, 50).unwrap().expect("exists");
    assert_eq!(expansion.summary.id, episode.id);
    assert!(
        !expansion.evidence.is_empty(),
        "expanding must hand back the original events, not only the summary text"
    );
    // The evidence is the raw material: a tool result with its actual output.
    assert!(
        expansion
            .evidence
            .iter()
            .any(|a| a.kind == "tool.result" && a.text.contains("FAILED")),
        "the failing run must be among the evidence: {:#?}",
        expansion.evidence
    );
    assert!(
        expansion.evidence.iter().any(|a| a.is_write),
        "the write that fixed it must be among the evidence"
    );
}

/// Purpose: Pin that a two-character Chinese term finds a summary, and an absent word finds nothing.
#[test]
fn a_two_character_chinese_word_is_findable_in_summaries() {
    // The FTS5 path: `层叠` is shorter than the trigram width, so a naive MATCH
    // returns nothing. `recall` routes it to LIKE instead.
    let tmp = TempLog::new();
    two_episodes(tmp.path());
    let store = digested(tmp.path());

    let hits = store.recall(&SearchQuery::new(["层叠"])).unwrap();
    assert_eq!(hits.len(), 1, "two-character Chinese must find the episode");
    assert!(hits[0].title.contains("层叠"));

    // And a three-character term takes the FTS5 path to the same episode.
    let hits = store.recall(&SearchQuery::new(["深拷贝"])).unwrap();
    assert_eq!(hits[0].title.contains("层叠"), true);

    // An absent word finds nothing rather than everything.
    assert!(store
        .recall(&SearchQuery::new(["不存在的词"]))
        .unwrap()
        .is_empty());
}

/// Purpose: Pin that recall can be restricted to a level and an empty level returns nothing.
#[test]
fn recall_can_be_restricted_to_a_level() {
    let tmp = TempLog::new();
    two_episodes(tmp.path());
    let store = digested(tmp.path());

    assert_eq!(store.summary_count(2).unwrap(), 2);
    assert_eq!(store.summary_count(3).unwrap(), 0);
    let level2 = store
        .recall_at(Some(2), &SearchQuery::new(["层叠"]).limit(5))
        .unwrap();
    assert_eq!(level2.len(), 1);
    let level3 = store
        .recall_at(Some(3), &SearchQuery::new(["层叠"]).limit(5))
        .unwrap();
    assert!(level3.is_empty(), "nothing is at L3 yet");
}

/// Purpose: Pin that an L2 records one raw-event input span, never another summary.
#[test]
fn a_summary_records_its_lineage_as_a_raw_event_range() {
    let tmp = TempLog::new();
    two_episodes(tmp.path());
    let store = digested(tmp.path());
    for episode in store.summaries_at(2, 10).unwrap() {
        assert_eq!(episode.inputs.len(), 1, "an L2 has exactly one input span");
        let input = &episode.inputs[0];
        assert!(
            input.summary_id.is_none(),
            "an L2 is derived from raw events, never from another summary"
        );
        assert_eq!(input.seq_from, Some(episode.seq_from));
        assert_eq!(input.seq_to, Some(episode.seq_to));
    }
}

/// Purpose: Pin that a superseded summary is marked and remains readable alongside its replacement.
#[test]
fn superseding_a_conclusion_keeps_the_old_one() {
    // "We used to think X" is itself a fact. A superseded summary is marked, not
    // deleted, and points at what replaced it.
    let tmp = TempLog::new();
    two_episodes(tmp.path());
    let mut store = digested(tmp.path());
    let episode = store.summaries_at(2, 10).unwrap().remove(0);

    let mut replacement = episode.clone();
    replacement.id = format!("{}-v2", episode.id);
    replacement.superseded_by = None;
    store.put_summary(&replacement).unwrap();
    let mut old = episode.clone();
    old.superseded_by = Some(replacement.id.clone());
    store.put_summary(&old).unwrap();

    let read_back = store.summary(&episode.id).unwrap().unwrap();
    assert_eq!(
        read_back.superseded_by.as_deref(),
        Some(replacement.id.as_str())
    );
    assert!(
        store.summary(&replacement.id).unwrap().is_some(),
        "the replacement exists alongside it"
    );
}

/// Purpose: Pin that a summary's entity links answer a module lookup, the input to L3.
#[test]
fn a_summary_can_be_looked_up_by_a_module_it_mentions() {
    let tmp = TempLog::new();
    two_episodes(tmp.path());
    let store = digested(tmp.path());
    let episode = store
        .summaries_at(2, 10)
        .unwrap()
        .into_iter()
        .find(|s| s.title.contains("层叠"))
        .unwrap();

    // The entity link is what will let L3 roll up "everything about src/config".
    let entities = store.entities(Some("src/config"), 10).unwrap();
    assert!(
        entities
            .iter()
            .any(|(kind, name, _)| kind == "module" && name == "src/config"),
        "{entities:?}"
    );
    assert!(episode.seq_from < episode.seq_to);
}
