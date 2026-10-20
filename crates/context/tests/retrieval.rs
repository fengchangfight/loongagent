//! Purpose: End-to-end tests for L1 retrieval: pure materialisation, rebuildable state, citations and Chinese search.
//!
//! Serves: the retrieval layer's promises: the same log always derives the same state, a deleted store rebuilds, and two-character Chinese terms match.
//!
//! # Contract
//! - **Design:** `docs/context/data-model.md` §5.2–§5.4; `docs/context/recall.md` §7.2; `docs/design/context-ledger.md` §5.
//! - **Upstream:** `loong_context::{build, materialize, Store, SearchQuery}` and the session fixture below.
//! - **Downstream:** `Store::rebuild`, `Store::search`, `Store::verify` and `Store::atom`.
//! - **Verify:** `cargo test -p loong-context --test retrieval`.

use loong_context::{build, materialize, EntityKind, SearchQuery, Store, StoreError};
use loong_eventlog::SessionLog;
use loong_schema::generated::EventEnvelope;
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static N: AtomicU64 = AtomicU64::new(0);

/// Purpose: A throwaway log path; the JSONL file is the source of truth, so tests keep it.
/// Serves: each retrieval test owning a clean session log and cleaning it up.
/// Design: `docs/context/data-model.md` §5.2.
/// Up: The retrieval tests.
/// Down: `TempLog::new`, `TempLog::path` and `TempLog::drop`.
/// Verify: `cargo test -p loong-context --test retrieval`.
/// A throwaway log path. The file is the source of truth, so tests keep it.
struct TempLog(PathBuf);

impl TempLog {
    /// Purpose: Create a fresh temp log path and remove any stale file.
    /// Contract: `retrieval.rs` fixtures.
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "loong-context-{}-{}.jsonl",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_file(&p);
        TempLog(p)
    }
    /// Purpose: The session log's path.
    /// Contract: `retrieval.rs` fixtures.
    fn path(&self) -> &PathBuf {
        &self.0
    }
}

impl Drop for TempLog {
    /// Purpose: Remove the log and its sidecar database.
    /// Contract: `retrieval.rs` fixtures.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(self.0.with_extension("jsonl.context.db"));
    }
}

/// Purpose: A session shaped like the design note's example: report, reads, a failed attempt, a fix, green tests.
/// Serves: exercising every entity kind, both test outcomes and a skipped event in one realistic log.
/// Design: `docs/context/data-model.md` §5.3–§5.4.
/// Up: The retrieval tests.
/// Down: `loong_eventlog::SessionLog`; no other callee.
/// Verify: `cargo test -p loong-context --test retrieval`.
/// A session shaped like the one in the design note §6.5: a bug is reported,
/// files are read, a wrong fix is tried, a right fix lands, tests pass, commit.
fn config_layering_session(path: &PathBuf) -> Vec<EventEnvelope> {
    let mut log = SessionLog::create(path).unwrap();
    let mut events = Vec::new();
    let mut push = |log: &mut SessionLog, kind: &str, data: serde_json::Value| {
        let env = log.append(kind, data).unwrap();
        events.push(env);
    };
    push(
        &mut log,
        "message.appended",
        json!({"role": "user", "text": "配置层叠失效了，帮我查一下"}),
    );
    push(
        &mut log,
        "tool.call",
        json!({"call_id": "c1", "tool": "read_file", "args": {"path": "src/config/merge.rs"}, "action_class": "investigate"}),
    );
    push(
        &mut log,
        "tool.result",
        json!({"call_id": "c1", "ok": true, "output": "pub fn merge(layers: &[Layer]) { /* ... */ }"}),
    );
    push(
        &mut log,
        "tool.call",
        json!({"call_id": "c2", "tool": "run_command", "args": {"command": "cargo test layer_merge"}, "action_class": "investigate"}),
    );
    push(
        &mut log,
        "tool.result",
        json!({"call_id": "c2", "ok": false, "output": "test layer_merge ... FAILED\nexpected 2 layers, got 1\nerror[E0308]: mismatched types"}),
    );
    push(
        &mut log,
        "message.appended",
        json!({"role": "assistant", "text": "方案 A：先深拷贝再合并。已试，测试仍然失败。"}),
    );
    push(
        &mut log,
        "message.appended",
        json!({"role": "assistant", "text": "方案 B：改为按层号排序后合并。测试通过，已提交。"}),
    );
    push(
        &mut log,
        "tool.call",
        json!({"call_id": "c3", "tool": "run_command", "args": {"command": "cargo test layer_merge"}, "action_class": "investigate"}),
    );
    push(
        &mut log,
        "tool.result",
        json!({"call_id": "c3", "ok": true, "output": "test layer_merge ... ok\n1 passed"}),
    );
    // A kind with no searchable content: it must not create an atom.
    push(
        &mut log,
        "policy.decided",
        json!({"action_class": "investigate", "verdict": "allow", "rule": "read-only"}),
    );
    // A long output, to prove the extract is capped rather than copied.
    push(
        &mut log,
        "tool.result",
        json!({"call_id": "c4", "ok": true, "output": "x".repeat(4000)}),
    );
    events
}

/// Purpose: Pin that materialisation is pure and skips kinds that carry no searchable content.
#[test]
fn the_same_event_always_materialises_the_same_way() {
    let tmp = TempLog::new();
    let events = config_layering_session(tmp.path());
    let first: Vec<_> = events.iter().filter_map(materialize).collect();
    let second: Vec<_> = events.iter().filter_map(materialize).collect();
    assert_eq!(first, second, "materialisation must be a pure function");
    assert_eq!(
        first.len(),
        events.len() - 1,
        "policy.decided carries no content"
    );
}

/// Purpose: Pin that a long tool extract is capped and announces the cut.
#[test]
fn a_long_tool_output_is_capped_and_says_so() {
    let tmp = TempLog::new();
    let events = config_layering_session(tmp.path());
    let atom = events.iter().filter_map(materialize).last().unwrap();
    assert!(
        atom.text.ends_with("[truncated]"),
        "a cut extract must announce itself: {:?}",
        &atom.text[atom.text.len().saturating_sub(40)..]
    );
    assert!(atom.text.chars().count() < 600);
}

/// Purpose: Pin that paths, modules, commands, tests, tools and error codes are extracted deterministically.
#[test]
fn entities_are_extracted_from_paths_commands_tests_and_errors() {
    let tmp = TempLog::new();
    let events = config_layering_session(tmp.path());
    let atoms: Vec<_> = events.iter().filter_map(materialize).collect();

    let all: Vec<(EntityKind, String)> = atoms
        .iter()
        .flat_map(|a| a.entities.iter().map(|e| (e.kind, e.name.clone())))
        .collect();
    let has = |kind: EntityKind, name: &str| all.contains(&(kind, name.to_string()));

    assert!(has(EntityKind::Tool, "read_file"), "{all:?}");
    assert!(has(EntityKind::Tool, "run_command"), "{all:?}");
    assert!(has(EntityKind::Path, "src/config/merge.rs"), "{all:?}");
    assert!(
        has(EntityKind::Module, "src/config"),
        "the L3 rollup unit must exist: {all:?}"
    );
    assert!(has(EntityKind::Command, "cargo"), "{all:?}");
    assert!(has(EntityKind::Test, "layer_merge"), "{all:?}");
    assert!(has(EntityKind::Error, "E0308"), "{all:?}");
}

/// Purpose: Pin that two-character Chinese terms match, the case an FTS5-trigram-only search silently fails.
#[test]
fn a_two_character_chinese_word_is_findable() {
    // The case an FTS5-trigram-only implementation gets silently wrong: `修复`
    // is shorter than the trigram width, so a MATCH query returns nothing at all.
    let tmp = TempLog::new();
    config_layering_session(tmp.path());
    let store = build(tmp.path()).unwrap();

    let hits = store.search(&SearchQuery::new(["层叠"])).unwrap();
    assert!(!hits.is_empty(), "two-character Chinese must match");
    assert!(hits.iter().any(|h| h.preview.contains("层叠")));

    let hits = store.search(&SearchQuery::new(["配置"])).unwrap();
    assert!(!hits.is_empty(), "two-character Chinese must match");
}

/// Purpose: Pin implicit AND: every term must match, and an absent term excludes rather than zeroing the others.
#[test]
fn several_terms_narrow_rather_than_zero_out() {
    let tmp = TempLog::new();
    config_layering_session(tmp.path());
    let store = build(tmp.path()).unwrap();

    // Implicit AND, per term — one term never poisons the others.
    let both = store.search(&SearchQuery::new(["层叠", "配置"])).unwrap();
    assert!(!both.is_empty());

    let impossible = store
        .search(&SearchQuery::new(["层叠", "不存在的词"]))
        .unwrap();
    assert!(impossible.is_empty(), "a term that is absent must exclude");
}

/// Purpose: Pin newest-first ordering and that a hit carries its entities.
#[test]
fn results_are_newest_first_and_entities_come_back_with_them() {
    let tmp = TempLog::new();
    config_layering_session(tmp.path());
    let store = build(tmp.path()).unwrap();

    let hits = store
        .search(&SearchQuery::new(["layer_merge"]).limit(10))
        .unwrap();
    assert!(hits.len() >= 2);
    let seqs: Vec<u64> = hits.iter().map(|h| h.seq).collect();
    let mut sorted = seqs.clone();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    assert_eq!(seqs, sorted, "newest first");

    let failing = hits
        .iter()
        .find(|h| h.ok == Some(false))
        .expect("the failing test run is among the hits");
    assert!(failing
        .entities
        .iter()
        .any(|(k, n)| k == "test" && n == "layer_merge"));
}

/// Purpose: Pin that entity filters answer which commands and modules the session used.
#[test]
fn entity_filters_answer_which_commands_and_tests_this_session_used() {
    let tmp = TempLog::new();
    config_layering_session(tmp.path());
    let store = build(tmp.path()).unwrap();

    let by_command = store
        .search(&SearchQuery::default().entity("command:cargo").limit(20))
        .unwrap();
    assert_eq!(by_command.len(), 2, "two cargo commands ran");

    let by_module = store
        .search(&SearchQuery::default().entity("module:src/config").limit(20))
        .unwrap();
    assert!(!by_module.is_empty());

    let entities = store.entities(Some("layer_merge"), 10).unwrap();
    assert!(
        entities
            .iter()
            .any(|(kind, name, n)| kind == "test" && name == "layer_merge" && *n >= 2),
        "{entities:?}"
    );
}

/// Purpose: Pin that re-ingest adds nothing and an appended event is picked up without re-reading state.
#[test]
fn ingest_is_idempotent_and_incremental() {
    let tmp = TempLog::new();
    config_layering_session(tmp.path());

    let mut store = Store::in_memory().unwrap();
    let first = store.ingest(tmp.path()).unwrap();
    assert!(first > 0);
    let after_first = store.atom_count().unwrap();

    // Running it again must change nothing — the watermark holds.
    let again = store.ingest(tmp.path()).unwrap();
    assert_eq!(again, 0, "nothing new to ingest");
    assert_eq!(store.atom_count().unwrap(), after_first);

    // Appending to the log must be picked up without re-reading state.
    {
        let mut log = loong_eventlog::SessionLog::open(tmp.path()).unwrap();
        log.append(
            "message.appended",
            json!({"role": "user", "text": "再顺手加个测试"}),
        )
        .unwrap();
    }
    let delta = store.ingest(tmp.path()).unwrap();
    assert_eq!(delta, 1);
    assert_eq!(store.atom_count().unwrap(), after_first + 1);
}

/// Purpose: Pin that a rebuild from the log reproduces the same rows as the first build.
#[test]
fn a_deleted_store_rebuilds_to_the_same_rows() {
    let tmp = TempLog::new();
    config_layering_session(tmp.path());

    let mut store = Store::in_memory().unwrap();
    store.rebuild(tmp.path()).unwrap();
    let before = store.search(&SearchQuery::new(["层叠"]).limit(50)).unwrap();

    // Delete everything and rebuild from the log alone.
    store.rebuild(tmp.path()).unwrap();
    let after = store.search(&SearchQuery::new(["层叠"]).limit(50)).unwrap();
    assert_eq!(before, after, "a rebuild must reproduce the derived state");

    // And a second, independently built store agrees with the first.
    let fresh = build(tmp.path()).unwrap();
    assert_eq!(fresh.atom_count().unwrap(), store.atom_count().unwrap());
    let fresh_hits = fresh.search(&SearchQuery::new(["层叠"]).limit(50)).unwrap();
    assert_eq!(before, fresh_hits);
}

/// Purpose: Pin that a healthy store verifies clean, skipped events are auditable, and an empty store is healthy.
#[test]
fn verify_reports_a_healthy_store_and_survives_an_empty_one() {
    let tmp = TempLog::new();
    config_layering_session(tmp.path());
    let store = build(tmp.path()).unwrap();
    let report = store.verify().unwrap();
    assert!(report.is_healthy(), "{report:?}");
    assert_eq!(report.orphan_links, 0);
    assert_eq!(report.dangling_entity_links, 0);
    assert_eq!(report.missing_atoms, 0);
    assert_eq!(
        report.skipped, 1,
        "policy.decided is deliberately not recorded, and that is auditable"
    );
    assert_eq!(report.watermark, 11, "every event has been considered");
    assert_eq!(report.atoms, 10);

    let empty = Store::in_memory().unwrap();
    let report = empty.verify().unwrap();
    assert!(report.is_healthy());
    assert_eq!(report.atoms, 0);
    assert_eq!(report.watermark, 0);
}

/// Purpose: Pin that a missing atom behind the store's back is reported rather than hidden.
#[test]
fn a_store_missing_a_row_is_not_reported_as_healthy() {
    // Delete a row behind the store's back: `verify` must notice, because a
    // stale derived store that claims to be fine is worse than one that admits it.
    let tmp = TempLog::new();
    config_layering_session(tmp.path());
    let db = tmp.path().with_extension("jsonl.context.db");
    {
        let mut store = Store::open(&db).unwrap();
        store.rebuild(tmp.path()).unwrap();
        assert!(store.verify().unwrap().is_healthy());
    }
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        // The link rows first: the store declares foreign keys, and a test that
        // has to fight its own schema is testing the wrong thing.
        conn.execute("DELETE FROM atom_entity WHERE seq = 5", [])
            .unwrap();
        conn.execute("DELETE FROM atom WHERE seq = 5", []).unwrap();
    }
    let store = Store::open(&db).unwrap();
    let report = store.verify().unwrap();
    assert_eq!(report.missing_atoms, 1, "{report:?}");
    assert!(!report.is_healthy());
}

/// Purpose: Pin that one atom is returned in full with its citation and that a missing seq is None.
#[test]
fn one_atom_can_be_expanded_in_full() {
    let tmp = TempLog::new();
    let events = config_layering_session(tmp.path());
    let store = build(tmp.path()).unwrap();

    let target = events
        .iter()
        .filter_map(materialize)
        .find(|a| a.kind == "tool.result" && a.ok == Some(false))
        .unwrap();
    let hit = store.atom(target.seq).unwrap().expect("recorded");
    assert_eq!(hit.kind, "tool.result");
    assert!(hit.preview.contains("FAILED"));
    assert_eq!(hit.citation(), (target.seq, target.seq));
    assert!(
        store.atom(9_999).unwrap().is_none(),
        "a missing seq is None"
    );
}

/// Purpose: Pin that an unknown schema version refuses to open instead of being half-read.
#[test]
fn a_store_written_by_another_schema_is_refused_not_half_read() {
    let tmp = TempLog::new();
    config_layering_session(tmp.path());
    let db = tmp.path().with_extension("jsonl.context.db");
    {
        let store = Store::open(&db).unwrap();
        store.verify().unwrap();
    }
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', '99')",
            [],
        )
        .unwrap();
    }
    match Store::open(&db) {
        Err(StoreError::SchemaVersion { found: 99, .. }) => {}
        other => panic!("expected SchemaVersion, got {other:?}"),
    }
}
