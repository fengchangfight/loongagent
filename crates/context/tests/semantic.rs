//! Purpose: The semantic layer's limits: what a model may add, and everything it may not touch.
//!
//! Serves: the field-ownership rule in ADR-0017 §4 as a regression net: a model cannot rewrite a computed status, a citation, an episode boundary or the call budget.
//!
//! # Contract
//! - **Design:** `docs/context/pipeline.md` §6.2 and §6.6; `docs/context/status.md` §11.1; `docs/design/context-ledger.md` §4.
//! - **Upstream:** `loong_context::{Store, WindowBudget}` and the scripted `Summarizer` implementations below.
//! - **Downstream:** `crate::summary`'s conditional enrichment write, the window budget and the foreground yield.
//! - **Verify:** `cargo test -p loong-context --test semantic`.

use loong_context::summarizer::{Summarizer, SummarizerError, Synthesis};
use loong_context::worker::IdleForeground;
use loong_context::{Store, SyncOutcome, WindowBudget};
use loong_eventlog::SessionLog;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

static N: AtomicU64 = AtomicU64::new(0);

/// Purpose: A temporary session log removed when the test ends.
/// Serves: every test starting from a clean log and leaving nothing behind.
/// Design: `docs/context/pipeline.md` §6.6.
/// Up: The semantic tests.
/// Down: `TempLog::new`, `TempLog::path` and `TempLog::drop`.
/// Verify: `cargo test -p loong-context --test semantic`.
struct TempLog(PathBuf);

impl TempLog {
    /// Purpose: Create a fresh temp log path and remove any stale file.
    /// Contract: `semantic.rs` fixtures.
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "loong-context-sem-{}-{}.jsonl",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_file(&p);
        TempLog(p)
    }
    /// Purpose: The session log's path.
    /// Contract: `semantic.rs` fixtures.
    fn path(&self) -> &PathBuf {
        &self.0
    }
}

impl Drop for TempLog {
    /// Purpose: Remove the session log.
    /// Contract: `semantic.rs` fixtures.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Purpose: Append one task: a request, an edit, a failing test and a passing re-run.
/// Serves: a realistic episode the segmenter closes and the model can be asked about.
/// Design: `docs/context/pipeline.md` §6.3.
/// Up: The semantic tests.
/// Down: `loong_eventlog::SessionLog`; no other callee.
/// Verify: `cargo test -p loong-context --test semantic`.
fn append_task(path: &PathBuf, index: usize, file: &str) {
    if !path.exists() {
        std::fs::File::create(path).unwrap();
    }
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

/// Purpose: A summariser that answers from a script and counts its calls.
/// Serves: the model's spend being checkable rather than asserted.
/// Design: `docs/context/pipeline.md` §6.6.
/// Up: The semantic tests.
/// Down: The `Summarizer` impl below.
/// Verify: `cargo test -p loong-context --test semantic the_call_budget_is_respected`.
/// A summariser that answers from a script and counts its calls, so "how much did
/// the background cost?" is checkable rather than asserted.
struct Scripted {
    answer: Synthesis,
    calls: Arc<Mutex<u32>>,
    fail_after: Option<u32>,
}

impl Scripted {
    /// Purpose: Build a summariser that always says the same thing, plus its call counter.
    /// Contract: `semantic.rs` fixtures.
    fn saying(synthesis: Synthesis) -> (Self, Arc<Mutex<u32>>) {
        let calls = Arc::new(Mutex::new(0));
        (
            Self {
                answer: synthesis,
                calls: Arc::clone(&calls),
                fail_after: None,
            },
            calls,
        )
    }

    /// Purpose: Build a summariser that answers usefully once and then fails.
    /// Serves: the partial-failure case: one degraded episode must not stop the pass.
    /// Design: `docs/context/pipeline.md` §6.6.
    /// Up: `a_model_that_breaks_mid_pass_degrades_one_episode_and_keeps_going`.
    /// Down: The `Summarizer` impl below.
    /// Verify: `cargo test -p loong-context --test semantic a_model_that_breaks_mid_pass`.
    fn failing_after(n: u32) -> (Self, Arc<Mutex<u32>>) {
        let calls = Arc::new(Mutex::new(0));
        (
            Self {
                // The first call answers usefully; the model then "goes away", which
                // is the partial-failure case worth testing.
                answer: Synthesis {
                    rejected: vec!["先按调用序合并".into()],
                    ..Default::default()
                },
                calls: Arc::clone(&calls),
                fail_after: Some(n),
            },
            calls,
        )
    }
}

impl Summarizer for Scripted {
    /// Purpose: The provenance name `scripted`.
    /// Contract: `semantic.rs` test double.
    fn name(&self) -> String {
        "scripted".to_string()
    }

    /// Purpose: Count the call and answer, or fail after the configured number.
    /// Serves: exercising both the success and the "model went away" path deterministically.
    /// Design: `docs/context/pipeline.md` §6.6.
    /// Up: `Store::synthesize_window` through `Summarizer`.
    /// Down: Returns the configured `Synthesis` or `Unavailable`; no other callee.
    /// Verify: `cargo test -p loong-context --test semantic`.
    fn summarize(
        &mut self,
        _input: &loong_context::DigestInput,
    ) -> Result<Synthesis, SummarizerError> {
        let mut calls = self.calls.lock().unwrap();
        *calls += 1;
        if let Some(limit) = self.fail_after {
            if *calls > limit {
                return Err(SummarizerError::Unavailable("the model went away".into()));
            }
        }
        Ok(self.answer.clone())
    }
}

/// Purpose: Build and digest a store over the temp log.
/// Contract: `semantic.rs` fixtures.
fn digested(path: &PathBuf) -> Store {
    let mut store = Store::in_memory().unwrap();
    assert!(matches!(
        store.sync(4096).unwrap(),
        SyncOutcome::NeedsRebuild
    ));
    store.rebuild(path).unwrap();
    store.digest().unwrap();
    store
}

/// Purpose: Pin that a model may add `rejected`/`why`/`open_questions`/`conclusion` while status, span, citations, domain and lineage stay computed.
#[test]
fn a_models_answer_is_added_and_computed_fields_are_left_alone() {
    let tmp = TempLog::new();
    append_task(tmp.path(), 1, "src/kernel/turn.rs");
    let mut store = digested(tmp.path());
    let before = store.summaries_at(2, 10).unwrap().remove(0);

    let (mut model, calls) = Scripted::saying(Synthesis {
        rejected: vec!["先深拷贝再合并：测试仍失败".into()],
        why: vec!["层叠顺序不能依赖调用序".into()],
        open_questions: vec!["是否要加一个回归测试".into()],
        conclusion: Some("改为按层号排序后合并".into()),
    });
    let enriched = store.synthesize(&mut model, 4).unwrap();
    assert_eq!(enriched, 1);
    assert_eq!(*calls.lock().unwrap(), 1);

    let after = store.summaries_at(2, 10).unwrap().remove(0);
    // Added: the four fields the model is allowed to answer.
    assert!(
        after.content.contains("被否决：先深拷贝再合并"),
        "{}",
        after.content
    );
    assert!(after.content.contains("原因：层叠顺序不能依赖调用序"));
    assert!(after.content.contains("未决问题：是否要加一个回归测试"));
    assert!(after.content.contains("模型结论：改为按层号排序后合并"));
    // Untouched: everything the harness computed.
    assert_eq!(
        after.status, before.status,
        "status is not the model's to change"
    );
    assert_eq!(after.seq_from, before.seq_from);
    assert_eq!(after.seq_to, before.seq_to);
    assert_eq!(after.citations, before.citations, "citations survive");
    assert_eq!(after.domain, before.domain);
    assert_eq!(after.inputs, before.inputs, "lineage survives");
    assert!(
        after.producer.starts_with("digest+model:"),
        "{}",
        after.producer
    );
    // And the deterministic lines are all still present, verbatim.
    for line in before.content.lines().filter(|l| !l.is_empty()) {
        assert!(
            after.content.contains(line),
            "the deterministic line {line:?} was lost from {}",
            after.content
        );
    }
}

/// Purpose: Pin that a model's claim that a verified task is open cannot change the computed status, and the disagreement stays visible.
#[test]
fn a_model_that_claims_a_verified_task_is_open_changes_nothing() {
    // The measured failure mode: a 3B model called a committed, passing task
    // `open`. Nothing in the merge path can honour that, by construction.
    let tmp = TempLog::new();
    append_task(tmp.path(), 1, "src/kernel/turn.rs");
    let mut store = digested(tmp.path());
    let before = store.summaries_at(2, 10).unwrap().remove(0);
    assert_eq!(before.status, "done");

    let (mut model, _) = Scripted::saying(Synthesis {
        // A conclusion that contradicts the evidence.
        conclusion: Some("这个任务还没有完成".into()),
        ..Default::default()
    });
    store.synthesize(&mut model, 4).unwrap();

    let after = store.summaries_at(2, 10).unwrap().remove(0);
    assert_eq!(after.status, "done", "the computed status wins");
    assert!(
        after.content.contains("状态：done"),
        "and the evidence line is still there: {}",
        after.content
    );
    // The disagreement stays visible rather than being resolved in the model's
    // favour: a contradiction a reader can see is information.
    assert!(after.content.contains("模型结论：这个任务还没有完成"));
}

/// Purpose: Pin that an empty answer is recorded as asked, with the computed abandoned list intact, and is never asked again.
#[test]
fn an_empty_answer_is_recorded_as_asked_and_not_asked_again() {
    let tmp = TempLog::new();
    append_task(tmp.path(), 1, "src/kernel/turn.rs");
    let mut store = digested(tmp.path());

    let (mut model, calls) = Scripted::saying(Synthesis::default());
    assert_eq!(store.synthesize(&mut model, 4).unwrap(), 0);
    assert_eq!(*calls.lock().unwrap(), 1, "it was asked once");

    let after = store.summaries_at(2, 10).unwrap().remove(0);
    assert!(after.producer.starts_with("digest+model:"));
    // "Nothing was added" now means the *model's* fields are absent. `被否决` is
    // present either way, because it is computed from the attempt sequence rather
    // than asked for — that is the change this test exists alongside.
    assert!(
        !after.content.contains("原因："),
        "the model added no reason"
    );
    assert!(
        after.content.contains("被否决"),
        "but the computed abandonment is still there: {}",
        after.content
    );

    // A second pass does not re-ask: the producer marker says it was answered.
    assert_eq!(store.synthesize(&mut model, 4).unwrap(), 0);
    assert_eq!(*calls.lock().unwrap(), 1);
}

/// Purpose: Pin that one failed call degrades only its own episode, leaves every digest in place and does not stop the pass.
#[test]
fn a_model_that_breaks_mid_pass_degrades_one_episode_and_keeps_going() {
    let tmp = TempLog::new();
    for index in 1..5 {
        append_task(tmp.path(), index, "src/kernel/turn.rs");
    }
    let mut store = digested(tmp.path());
    let episodes = store.summaries_at(2, 10).unwrap();
    assert!(
        episodes.len() >= 3,
        "several episodes to walk: {}",
        episodes.len()
    );

    let (mut model, calls) = Scripted::failing_after(1);
    let enriched = store.synthesize(&mut model, 10).unwrap();
    assert_eq!(enriched, 1, "the first answer was usable");
    assert!(
        *calls.lock().unwrap() >= 2,
        "it kept going after the failure"
    );

    let after = store.summaries_at(2, 10).unwrap();
    let failed: Vec<_> = after
        .iter()
        .filter(|s| s.producer.contains("(failed:"))
        .collect();
    assert!(
        !failed.is_empty(),
        "the failure is recorded on the summary, not swallowed: {:?}",
        after.iter().map(|s| &s.producer).collect::<Vec<_>>()
    );
    // Everything still has a digest, which is the point of degrading rather than
    // failing the pass.
    for summary in &after {
        assert!(summary.content.starts_with("L2 ·"), "{}", summary.content);
    }
}

/// Purpose: Pin that one call is made per enriched episode, and a second pass asks nothing already answered.
#[test]
fn the_model_is_never_asked_twice_for_the_same_episode() {
    let tmp = TempLog::new();
    for index in 1..4 {
        append_task(tmp.path(), index, "src/kernel/turn.rs");
    }
    let mut store = digested(tmp.path());
    let (mut model, calls) = Scripted::saying(Synthesis {
        rejected: vec!["方案 A".into()],
        ..Default::default()
    });
    let first = store.synthesize(&mut model, 10).unwrap();
    assert!(first >= 1);
    let after_first = *calls.lock().unwrap();
    assert_eq!(after_first as u64, first, "one call per enriched episode");

    let second = store.synthesize(&mut model, 10).unwrap();
    assert_eq!(second, 0, "already answered");
    assert_eq!(*calls.lock().unwrap(), after_first, "no second call");
}

/// Purpose: Pin that the call budget is a ceiling: two episodes enriched means exactly two calls.
#[test]
fn the_call_budget_is_respected() {
    let tmp = TempLog::new();
    for index in 1..6 {
        append_task(tmp.path(), index, "src/kernel/turn.rs");
    }
    let mut store = digested(tmp.path());
    let (mut model, calls) = Scripted::saying(Synthesis {
        rejected: vec!["被否决的做法".into()],
        ..Default::default()
    });
    let enriched = store.synthesize(&mut model, 2).unwrap();
    assert_eq!(enriched, 2);
    assert_eq!(
        *calls.lock().unwrap(),
        2,
        "the budget is a ceiling, not advice"
    );
}

/// Purpose: Pin that a recorded failure is terminal for that model, so a down endpoint is not re-asked each window; a new model name is asked.
#[test]
fn a_failed_episode_is_not_asked_again_in_the_same_session() {
    // The worker runs in a loop for the session's whole life. If a recorded
    // failure did not stop it, an endpoint that is down would be re-asked once per
    // window forever — the steady state §4.5 requires would never arrive.
    let tmp = TempLog::new();
    for index in 1..4 {
        append_task(tmp.path(), index, "src/kernel/turn.rs");
    }
    let mut store = digested(tmp.path());
    // A summariser that always errors is what this test needs; `Scripted::saying`
    // answers an empty synthesis, which is a success, not a failure.
    /// Purpose: A summariser that always errors.
    /// Serves: proving that a permanent failure is recorded rather than retried forever.
    /// Design: `docs/context/pipeline.md` §6.6.
    /// Up: `a_failed_episode_is_not_asked_again_in_the_same_session`.
    /// Down: The `Summarizer` impl below.
    /// Verify: `cargo test -p loong-context --test semantic a_failed_episode_is_not_asked_again`.
    struct Broken;
    impl Summarizer for Broken {
        /// Purpose: The provenance name `broken`.
        /// Contract: `semantic.rs` test double.
        fn name(&self) -> String {
            "broken".to_string()
        }
        /// Purpose: Always return `Unavailable`.
        /// Contract: `semantic.rs` test double.
        fn summarize(
            &mut self,
            _input: &loong_context::DigestInput,
        ) -> Result<Synthesis, SummarizerError> {
            Err(SummarizerError::Unavailable("nothing is listening".into()))
        }
    }
    let mut broken = Broken;
    let first = store
        .synthesize_window(&mut broken, WindowBudget::calls(10), &IdleForeground)
        .unwrap();
    assert!(first.calls >= 2, "there are episodes to try: {first:?}");
    assert_eq!(
        first.failed, first.calls,
        "every call failed, and every failure is charged"
    );
    assert_eq!(store.degraded_synthesis().unwrap(), first.failed);
    assert_eq!(store.pending_synthesis().unwrap(), 0, "none left pending");
    let second = store
        .synthesize_window(&mut broken, WindowBudget::calls(10), &IdleForeground)
        .unwrap();
    assert_eq!(
        second.calls, 0,
        "the failed episodes are not re-asked: {} left to try",
        second.remaining
    );
    assert_eq!(
        store.degraded_synthesis().unwrap(),
        first.failed,
        "and not re-marked"
    );

    // The same store, a different model name, is a different question.
    let (mut model, calls) = Scripted::saying(Synthesis {
        rejected: vec!["方案 A".into()],
        ..Default::default()
    });
    let third = store
        .synthesize_window(&mut model, WindowBudget::calls(2), &IdleForeground)
        .unwrap();
    assert!(third.calls > 0, "a new summariser is asked");
    assert_eq!(*calls.lock().unwrap(), third.calls as u32);
}

/// Purpose: Pin that a window stops before a second call that would exceed its token budget, while always making the first.
#[test]
fn a_window_stops_at_its_token_budget() {
    // The second half of §4.6's budget: "N calls / M tokens". A window with room
    // for roughly one prompt must not make a second call.
    let tmp = TempLog::new();
    for index in 1..6 {
        append_task(tmp.path(), index, "src/kernel/turn.rs");
    }
    let mut store = digested(tmp.path());
    let (mut model, calls) = Scripted::saying(Synthesis {
        rejected: vec!["方案 A".into()],
        ..Default::default()
    });
    let outcome = store
        .synthesize_window(&mut model, WindowBudget::new(8, 1), &IdleForeground)
        .unwrap();
    assert_eq!(
        outcome.calls, 1,
        "the first call is always made so a tiny window still makes progress"
    );
    assert_eq!(*calls.lock().unwrap(), 1);
    assert!(
        outcome.remaining > 0,
        "and work is left for the next window"
    );
}

/// Purpose: Pin that one window spends at most its call budget and leaves the rest for the next window.
#[test]
fn one_window_enriches_at_most_its_call_budget() {
    let tmp = TempLog::new();
    for index in 1..7 {
        append_task(tmp.path(), index, "src/kernel/turn.rs");
    }
    let mut store = digested(tmp.path());
    let (mut model, calls) = Scripted::saying(Synthesis {
        rejected: vec!["方案 A".into()],
        ..Default::default()
    });
    let outcome = store
        .synthesize_window(&mut model, WindowBudget::new(3, 100_000), &IdleForeground)
        .unwrap();
    assert_eq!(outcome.calls, 3);
    assert_eq!(outcome.enriched, 3);
    assert_eq!(*calls.lock().unwrap(), 3);
    assert!(
        outcome.remaining > 0,
        "three of the episodes are still waiting after one window: {outcome:?}"
    );
}

/// Purpose: Pin that a turn starting mid-window stops it at the next call boundary, not after the whole budget.
#[test]
fn a_window_yields_at_the_next_call_when_the_foreground_starts() {
    // §4.6's "yield at a batch boundary" with the boundary drawn per call: a turn
    // that starts in the middle of a window takes effect at the next call, not after
    // the whole window has spent its budget.
    let tmp = TempLog::new();
    for index in 1..6 {
        append_task(tmp.path(), index, "src/kernel/turn.rs");
    }
    let mut store = digested(tmp.path());
    let (mut model, calls) = Scripted::saying(Synthesis {
        rejected: vec!["方案 A".into()],
        ..Default::default()
    });
    let checks = std::sync::atomic::AtomicUsize::new(0);
    // Quiet for the first check, busy from the second onwards.
    let foreground = || checks.fetch_add(1, Ordering::SeqCst) >= 1;

    let outcome = store
        .synthesize_window(&mut model, WindowBudget::calls(4), &foreground)
        .unwrap();
    assert_eq!(outcome.calls, 1, "it stopped at the first call boundary");
    assert_eq!(*calls.lock().unwrap(), 1, "and made exactly one call");
    assert!(
        outcome.remaining > 0,
        "the rest is left for the next idle window"
    );
}

/// Purpose: A model that takes long enough for the foreground to re-derive the episode it is answering about.
/// Serves: pinning the real race: the worker yields between windows, not inside one.
/// Design: `docs/context/pipeline.md` §6.6.
/// Up: `a_summary_replaced_while_the_model_answers_is_not_overwritten`.
/// Down: The `Summarizer` impl below; `Store::put_summary` on a second connection.
/// Verify: `cargo test -p loong-context --test semantic a_summary_replaced_while_the_model_answers_is_not_overwritten`.
/// A model that takes long enough for the foreground to re-derive the episode it is
/// answering about. This is the real race: the worker yields *between* windows, not
/// inside one, so a new turn can start while a request is in flight.
struct ForegroundRewrites {
    db: PathBuf,
    replacement: loong_context::Summary,
}

impl Summarizer for ForegroundRewrites {
    /// Purpose: The provenance name `racing`.
    /// Contract: `semantic.rs` test double.
    fn name(&self) -> String {
        "racing".to_string()
    }

    /// Purpose: Re-derive the open episode on another connection before answering.
    /// Serves: simulating the foreground replacing the row the model was asked about.
    /// Design: `docs/context/pipeline.md` §6.6.
    /// Up: `Store::synthesize_window` through `Summarizer`.
    /// Down: `Store::open` and `Store::put_summary`; returns an answer about the old digest.
    /// Verify: `cargo test -p loong-context --test semantic a_summary_replaced_while_the_model_answers_is_not_overwritten`.
    fn summarize(
        &mut self,
        _input: &loong_context::DigestInput,
    ) -> Result<Synthesis, SummarizerError> {
        // The "foreground": another connection re-derives the open episode, so the
        // row the model was asked about is gone by the time it answers.
        let mut foreground = Store::open(&self.db).unwrap();
        foreground.put_summary(&self.replacement).unwrap();
        Ok(Synthesis {
            why: vec!["模型基于旧 digest 说的话".into()],
            conclusion: Some("模型结论".into()),
            ..Default::default()
        })
    }
}

/// Purpose: Pin that a model's answer is dropped when the foreground re-derived the episode while it was in flight, never restoring the stale digest.
#[test]
fn a_summary_replaced_while_the_model_answers_is_not_overwritten() {
    // The conditional write. `put_summary` is a replace, so a naive enrichment would
    // restore the digest the foreground had just replaced — undoing new work and
    // resurrecting a stale episode boundary. The model's contribution must be
    // dropped instead, and the episode is enriched in a later window.
    let tmp = TempLog::new();
    append_task(tmp.path(), 1, "src/kernel/turn.rs");
    let db = tmp.path().with_extension("context.db");
    let _ = std::fs::remove_file(&db);

    let mut store = Store::open(&db).unwrap();
    store.rebuild(tmp.path()).unwrap();
    store.digest().unwrap();
    let before = store.summaries_at(2, 10).unwrap().remove(0);
    assert!(!before.content.contains("模型"), "{}", before.content);

    let mut replacement = before.clone();
    replacement.content = "L2 · 前台重新派生后的新内容".to_string();
    let mut model = ForegroundRewrites {
        db: db.clone(),
        replacement: replacement.clone(),
    };
    let outcome = store
        .synthesize_window(&mut model, WindowBudget::calls(1), &IdleForeground)
        .unwrap();

    assert_eq!(outcome.calls, 1, "the model was asked");
    assert_eq!(
        outcome.enriched, 0,
        "but its answer must not be recorded over a newer digest"
    );
    let stored = store
        .summary(&before.id)
        .unwrap()
        .expect("the row is there");
    assert_eq!(
        stored.content, replacement.content,
        "the foreground's derivation stands"
    );
    assert!(
        !stored.content.contains("模型基于旧 digest"),
        "the stale answer is nowhere in the store: {}",
        stored.content
    );
    let _ = std::fs::remove_file(&db);
}
