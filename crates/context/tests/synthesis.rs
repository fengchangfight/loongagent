//! Purpose: Tests for P4: the deterministic L3 domain map and the bounded L4 global map.
//!
//! Serves: maps that exist with no model, roll up by a deterministic rule, record their children as lineage and never grow past their ceiling.
//!
//! # Contract
//! - **Design:** `docs/context/pipeline.md` §6.4; `docs/context/recall.md` §7.1; `docs/design/context-ledger.md` §2.
//! - **Upstream:** `loong_context::{domain_of, Store, SyncOutcome}` and the task fixture below.
//! - **Downstream:** `Store::digest`, `Store::digest_domains` and `Store::global_map`; `crate::synthesis::{domain_of, global_map}`.
//! - **Verify:** `cargo test -p loong-context --test synthesis`.

use loong_context::{domain_of, Store, SyncOutcome};
use loong_eventlog::SessionLog;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static N: AtomicU64 = AtomicU64::new(0);

/// Purpose: A temporary session log removed when the test ends.
/// Serves: every test starting from a clean log and leaving nothing behind.
/// Design: `docs/context/pipeline.md` §6.4.
/// Up: The synthesis tests.
/// Down: `TempLog::new`, `TempLog::path` and `TempLog::drop`.
/// Verify: `cargo test -p loong-context --test synthesis`.
struct TempLog(PathBuf);

impl TempLog {
    /// Purpose: Create a fresh temp log path and remove any stale file.
    /// Contract: `synthesis.rs` fixtures.
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "loong-context-synth-{}-{}.jsonl",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_file(&p);
        TempLog(p)
    }
    /// Purpose: The session log's path.
    /// Contract: `synthesis.rs` fixtures.
    fn path(&self) -> &PathBuf {
        &self.0
    }
}

impl Drop for TempLog {
    /// Purpose: Remove the session log.
    /// Contract: `synthesis.rs` fixtures.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Purpose: Append one task that edits a file and verifies itself with a failing-then-passing test.
/// Serves: a closed episode whose domain membership is computable.
/// Design: `docs/context/pipeline.md` §6.3.
/// Up: `session`.
/// Down: `loong_eventlog::SessionLog`; no other callee.
/// Verify: `cargo test -p loong-context --test synthesis`.
/// Append a task that edits `file` and verifies itself with a test.
fn append_task(path: &PathBuf, index: usize, file: &str) {
    let mut log = SessionLog::open(path).unwrap();
    log.append(
        "message.appended",
        serde_json::json!({"role": "user", "text": format!("任务 {index}：{file} 的层叠失效了")}),
    )
    .unwrap();
    log.append(
        "tool.call",
        serde_json::json!({"call_id": format!("e{index}"), "tool": "edit_file",
            "args": {"path": file, "new_string": "x"}, "action_class": "write_workspace"}),
    )
    .unwrap();
    log.append(
        "tool.result",
        serde_json::json!({"call_id": format!("e{index}"), "ok": true, "output": "applied"}),
    )
    .unwrap();
    log.append(
        "tool.call",
        serde_json::json!({"call_id": format!("t{index}"), "tool": "run_command",
            "args": {"command": format!("cargo test fix_{index}")}, "action_class": "investigate"}),
    )
    .unwrap();
    log.append(
        "tool.result",
        serde_json::json!({"call_id": format!("t{index}"), "ok": false,
            "output": format!("test fix_{index} ... FAILED")}),
    )
    .unwrap();
    // The re-run matters beyond realism: a task needs enough events for the
    // segmenter to close it, and a fix that was verified is what a map is for.
    log.append(
        "tool.call",
        serde_json::json!({"call_id": format!("r{index}"), "tool": "run_command",
            "args": {"command": format!("cargo test fix_{index}")}, "action_class": "investigate"}),
    )
    .unwrap();
    log.append(
        "tool.result",
        serde_json::json!({"call_id": format!("r{index}"), "ok": true,
            "output": format!("test fix_{index} ... ok")}),
    )
    .unwrap();
}

/// Purpose: Create the log and append one task per file.
/// Contract: `synthesis.rs` fixtures.
fn session(path: &PathBuf, files: &[&str]) {
    if !path.exists() {
        std::fs::File::create(path).unwrap();
    }
    for (index, file) in files.iter().enumerate() {
        append_task(path, index, file);
    }
}

/// Purpose: Pin the deterministic two-segment domain rule, including `./` and `inbox`.
#[test]
fn a_domain_is_the_first_two_path_segments() {
    assert_eq!(domain_of("src/kernel/turn.rs"), "src/kernel");
    assert_eq!(domain_of("src/kernel"), "src/kernel");
    assert_eq!(domain_of("crates/context/src/store.rs"), "crates/context");
    assert_eq!(domain_of("./src/config/merge.rs"), "src/config");
    assert_eq!(domain_of("src"), "src");
    assert_eq!(domain_of(""), "inbox");
}

/// Purpose: Pin that three episodes of one domain produce one L3 map naming all of them.
#[test]
fn episodes_roll_up_into_a_domain_map() {
    let tmp = TempLog::new();
    session(
        tmp.path(),
        &[
            "src/kernel/turn.rs",
            "src/kernel/turn.rs",
            "src/kernel/ledger.rs",
        ],
    );
    let mut store = Store::in_memory().unwrap();
    store.rebuild(tmp.path()).unwrap();
    store.digest().unwrap();

    let episodes = store.summaries_at(2, 100).unwrap();
    assert_eq!(episodes.len(), 3);
    for episode in &episodes {
        assert_eq!(episode.domain.as_deref(), Some("src/kernel"));
    }

    let written = store.digest_domains().unwrap();
    assert_eq!(written, 1, "one domain earned a map");

    let domains = store.summaries_at(3, 10).unwrap();
    assert_eq!(domains.len(), 1, "{domains:#?}");
    assert_eq!(domains[0].scope, "domain:src/kernel");
    assert!(
        domains[0].content.contains("3 段"),
        "{}",
        domains[0].content
    );
    assert!(domains[0].content.contains("src/kernel"));
    // The map is an index: every episode is named in it.
    for episode in &episodes {
        assert!(
            domains[0].content.contains(&episode.id),
            "the map must name {}: {}",
            episode.id,
            domains[0].content
        );
    }
}

/// Purpose: Pin that a domain with fewer than three episodes does not get an invented map.
#[test]
fn a_domain_below_the_threshold_does_not_get_a_map() {
    // Two episodes is not a subsystem; inventing a map for it would be noise
    // dressed as structure.
    let tmp = TempLog::new();
    session(tmp.path(), &["src/kernel/turn.rs", "src/kernel/ledger.rs"]);
    let mut store = Store::in_memory().unwrap();
    store.rebuild(tmp.path()).unwrap();
    store.digest().unwrap();
    assert_eq!(store.digest_domains().unwrap(), 0);
    assert_eq!(store.summary_count(3).unwrap(), 0);
}

/// Purpose: Pin that an L3's lineage is its L2 children, not a raw range and not itself.
#[test]
fn a_domain_map_records_its_children_not_a_previous_version_of_itself() {
    let tmp = TempLog::new();
    session(
        tmp.path(),
        &["src/kernel/a.rs", "src/kernel/b.rs", "src/kernel/c.rs"],
    );
    let mut store = Store::in_memory().unwrap();
    store.rebuild(tmp.path()).unwrap();
    store.digest().unwrap();
    store.digest_domains().unwrap();

    let domain = store.summaries_at(3, 10).unwrap().remove(0);
    // Oldest first: a map lists its children in the order they happened, while
    // `summaries_at` returns newest first for the foreground's benefit.
    let mut episode_ids: Vec<String> = store
        .summaries_at(2, 100)
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    episode_ids.sort();
    let inputs: Vec<String> = domain
        .inputs
        .iter()
        .filter_map(|i| i.summary_id.clone())
        .collect();
    assert_eq!(inputs, episode_ids, "an L3 is derived from its L2 children");
    assert!(
        domain.inputs.iter().all(|i| i.seq_from.is_none()),
        "and never from a raw event range"
    );
    assert!(
        !inputs.contains(&domain.id),
        "an L3 must not list itself as its own input"
    );
}

/// Purpose: Pin that two modules roll up into two separate L3 scopes.
#[test]
fn two_domains_stay_separate() {
    let tmp = TempLog::new();
    session(
        tmp.path(),
        &[
            "src/kernel/a.rs",
            "src/kernel/b.rs",
            "src/kernel/c.rs",
            "src/config/a.rs",
            "src/config/b.rs",
            "src/config/c.rs",
        ],
    );
    let mut store = Store::in_memory().unwrap();
    store.rebuild(tmp.path()).unwrap();
    store.digest().unwrap();
    assert_eq!(store.digest_domains().unwrap(), 2);

    let mut scopes: Vec<String> = store
        .summaries_at(3, 10)
        .unwrap()
        .into_iter()
        .map(|s| s.scope)
        .collect();
    scopes.sort();
    assert_eq!(
        scopes,
        vec![
            "domain:src/config".to_string(),
            "domain:src/kernel".to_string()
        ]
    );
}

/// Purpose: Pin that L4 names the busiest domains and stays under its hard token ceiling.
#[test]
fn the_global_map_is_small_and_always_available() {
    let tmp = TempLog::new();
    let files: Vec<&str> = (0..12)
        .map(|i| match i % 3 {
            0 => "src/kernel/turn.rs",
            1 => "src/config/merge.rs",
            _ => "crates/context/store.rs",
        })
        .collect();
    session(tmp.path(), &files);

    let mut store = Store::in_memory().unwrap();
    store.rebuild(tmp.path()).unwrap();
    store.digest().unwrap();
    store.digest_domains().unwrap();

    let map = store.global_map().unwrap().expect("a map exists");
    assert!(map.content.starts_with("L4 · 全局认知地图"));
    assert!(map.content.contains("src/kernel"), "{}", map.content);
    assert!(map.content.contains("src/config"), "{}", map.content);
    assert!(map.content.contains("crates/context"), "{}", map.content);
    // The block that is always in the foreground has a hard ceiling.
    assert!(
        map.tokens <= loong_context::L4_MAX_TOKENS,
        "L4 is {} tokens: {}",
        map.tokens,
        map.content
    );
    assert_eq!(map.episodes, 12);
}

/// Purpose: Pin that a young session still gets an L4 map before any L3 exists.
#[test]
fn the_global_map_exists_before_any_domain_does() {
    // A young session still deserves a map: otherwise the foreground has nothing
    // to carry during exactly the period when a person is most likely to ask
    // "where were we?".
    let tmp = TempLog::new();
    session(tmp.path(), &["src/kernel/turn.rs", "src/config/merge.rs"]);
    let mut store = Store::in_memory().unwrap();
    store.rebuild(tmp.path()).unwrap();
    store.digest().unwrap();

    assert_eq!(store.summary_count(3).unwrap(), 0, "no domain map yet");
    let map = store.global_map().unwrap().expect("still a map");
    assert!(map.content.contains("情节"), "{}", map.content);
    assert!(map.tokens <= loong_context::L4_MAX_TOKENS);
}

/// Purpose: Pin the steady state: an unchanged session is not re-derived at L2 or L3.
#[test]
fn an_idle_synthesis_pass_writes_nothing() {
    // The steady-state rule, one level up: a worker looking at an unchanged
    // session must not rewrite the maps, or it can never idle.
    let tmp = TempLog::new();
    session(
        tmp.path(),
        &["src/kernel/a.rs", "src/kernel/b.rs", "src/kernel/c.rs"],
    );
    let mut store = Store::in_memory().unwrap();
    store.rebuild(tmp.path()).unwrap();
    store.digest().unwrap();
    assert_eq!(store.digest_domains().unwrap(), 1);
    assert_eq!(store.digest_domains().unwrap(), 0, "already current");
    assert_eq!(store.digest_pending(4).unwrap(), 0, "already current");
}

/// Purpose: Pin that incremental sync and digest keep L2/L3/L4 current as tasks are appended.
#[test]
fn the_worker_keeps_the_maps_current_as_the_session_grows() {
    let tmp = TempLog::new();
    session(tmp.path(), &["src/kernel/a.rs"]);
    let mut store = Store::in_memory().unwrap();
    store.rebuild(tmp.path()).unwrap();
    store.digest().unwrap();

    // Three more tasks in the same domain, a few at a time, as a worker would.
    for index in 1..4 {
        append_task(tmp.path(), index, "src/kernel/b.rs");
        assert!(matches!(
            store.sync(256).unwrap(),
            SyncOutcome::Synced { .. }
        ));
        store.digest_pending(4).unwrap();
        store.digest_domains().unwrap();
    }

    assert_eq!(store.summary_count(2).unwrap(), 4);
    assert_eq!(store.summary_count(3).unwrap(), 1);
    let map = store.global_map().unwrap().unwrap();
    assert!(map.content.contains("4 段"), "{}", map.content);
}
