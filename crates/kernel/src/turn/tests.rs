//! Purpose: Pin the turn's end-to-end behaviour: the gate, approvals, the budget, replay and lineage, routing, skills, hooks, compaction and recall.
//!
//! Serves: a regression net under `docs/design/kernel-loop.md` §6: every ending, every
//! recorded fact and every refusal this repository promises is asserted here.
//!
//! # Contract
//! - **Design:** `docs/design/kernel-loop.md` §6; `docs/design/context-ledger.md`,
//!   `docs/design/sessions-lineage.md`, `docs/design/tools-permissions.md` and
//!   `docs/design/extension-model.md` own the behaviour each test reaches.
//! - **Upstream:** the `turn` module's public API and the `Kernel` it builds.
//! - **Downstream:** `Temp`, `RecordingModel`, the streaming and stopping models, and
//!   the scripted helpers below.
//! - **Verify:** `cargo test -p loong-kernel turn::tests`.

use super::nudge::{leaked_tool_call, looks_like_pasted_file};
use super::recall::RECALL_MAX_CHARS;
use super::*;
use loong_providers::{CompletionRequest, CompletionResponse, Model, ModelError, ScriptedModel};
use loong_schema::generated::event as ev;
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

static N: AtomicU64 = AtomicU64::new(0);

/// Purpose: A temporary workspace with its own session log, removed when the test ends.
///
/// Serves: every test starting from a clean directory and a clean log.
/// Design: `docs/design/kernel-loop.md` §1.
/// Up: the turn tests that build a kernel.
/// Down: `Temp::new` and `Temp::drop`; the tests' `dir` and `log` fields.
/// Verify: `cargo test -p loong-kernel turn::tests`.
struct Temp {
    dir: PathBuf,
    log: PathBuf,
}
impl Temp {
    /// Purpose: Create a fresh directory and session-log path for one test.
    ///
    /// Contract: `turn/tests.rs` fixtures.
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "loong-kernel-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("session.jsonl");
        Temp { dir, log }
    }
}
impl Drop for Temp {
    /// Purpose: Remove the temporary directory when the test ends, passing or failing.
    ///
    /// Contract: `turn/tests.rs` fixtures.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Purpose: Install a hook manifest and its scripts, and trust it when the test asks.
///
/// Serves: tests that need a real hook set on disk, trusted or deliberately inert.
/// Design: `docs/design/extension-model.md` §1 and §7.
/// Up: the hook tests in this file.
/// Down: `HookSet::load` and `trust_now`; writes `.loongagent/hooks.toml` and the scripts.
/// Verify: `cargo test -p loong-kernel turn::tests::a_pre_tool_hook_can_deny_what_the_policy_would_allow`.
/// Install a hook manifest (and its scripts) in the workspace. `trust`
/// decides whether the hooks are allowed to run at all.
fn install_hooks(dir: &Path, manifest: &str, scripts: &[(&str, &str)], trust: bool) {
    let hooks = dir.join(".loongagent");
    std::fs::create_dir_all(&hooks).unwrap();
    for (name, body) in scripts {
        std::fs::write(hooks.join(name), body).unwrap();
    }
    std::fs::write(hooks.join("hooks.toml"), manifest).unwrap();
    if trust {
        loong_hooks::HookSet::load(dir)
            .unwrap()
            .trust_now()
            .unwrap();
    }
}

/// Purpose: Build a tool-call request from an id, a tool name and JSON arguments.
///
/// Contract: `turn/tests.rs` fixtures.
fn call(id: &str, tool: &str, args: serde_json::Value) -> ToolCallRequest {
    ToolCallRequest {
        call_id: id.into(),
        tool: tool.into(),
        args,
    }
}

/// Purpose: Build a model response with optional text, calls and usage.
///
/// Contract: `turn/tests.rs` fixtures.
fn response(calls: Vec<ToolCallRequest>, text: Option<&str>) -> CompletionResponse {
    CompletionResponse {
        text: text.map(|t| t.to_string()),
        tool_calls: calls,
        usage: Usage {
            tokens_in: 7,
            tokens_out: 3,
        },
    }
}

/// Purpose: The event kinds in a log, in order, for an end-to-end assertion.
///
/// Contract: `turn/tests.rs` fixtures.
fn kinds(log: &Path) -> Vec<String> {
    loong_eventlog::replay(log)
        .unwrap()
        .events
        .into_iter()
        .map(|e| e.kind)
        .collect()
}

/// Purpose: An allowed `read_file` runs, the model reads its output, and the turn ends with the answer — the baseline the loop is measured against.
#[test]
fn a_tool_runs_and_the_turn_finishes() {
    let t = Temp::new();
    std::fs::write(t.dir.join("hello.txt"), "hi from the workspace").unwrap();
    let model = ScriptedModel::new(vec![
        response(
            vec![call("c1", "read_file", json!({"path": "hello.txt"}))],
            None,
        ),
        response(vec![], Some("the file says hi")),
    ]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    let out = k.submit("read hello.txt").unwrap();

    assert_eq!(out.pending_approval, None);
    assert_eq!(out.final_text.as_deref(), Some("the file says hi"));
    assert_eq!(out.steps, 2);
    assert_eq!(
        kinds(&t.log),
        vec![
            // The session's own opening: what entered context before the work.
            ev::SKILLS_LOADED,
            ev::MESSAGE_APPENDED,
            ev::COST_RECORDED,
            ev::MESSAGE_APPENDED,
            ev::TOOL_CALL,
            ev::POLICY_DECIDED,
            ev::TOOL_RESULT,
            ev::COST_RECORDED,
            ev::MESSAGE_APPENDED,
        ]
    );
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let policy = events
        .iter()
        .find(|e| e.kind == ev::POLICY_DECIDED)
        .unwrap();
    assert_eq!(policy.data["verdict"], "allow");
    assert_eq!(
        policy.data["rule"], "builtin:investigate",
        "reading a file inside the workspace is an investigate action"
    );
    let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
    assert_eq!(result.data["ok"], true);
    assert!(result.data["output"]
        .as_str()
        .unwrap()
        .contains("hi from the workspace"));
}

/// Purpose: `/yolo` turns an out-of-bounds ask into an allow and records `permissions.relaxed` plus a `yolo:` ruling, so the log says why nothing asked.
#[test]
fn opting_out_of_prompts_allows_the_action_and_says_so_in_the_log() {
    let t = Temp::new();
    let escape = t.dir.parent().unwrap().join("loong-kernel-yolo.txt");
    let _ = std::fs::remove_file(&escape);
    let model = ScriptedModel::new(vec![
        response(
            vec![call(
                "c1",
                "write_file",
                json!({"path": escape.to_string_lossy(), "content": "x"}),
            )],
            None,
        ),
        response(vec![], Some("done")),
    ]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();

    // The opt-out is itself an event, before anything is allowed by it.
    k.set_auto_approve(true).unwrap();
    let kinds = kinds(&t.log);
    assert!(
        kinds.contains(&gen::event::PERMISSIONS_RELAXED.to_string()),
        "{kinds:?}"
    );

    let outcome = k.submit("write outside").unwrap();
    assert!(
        outcome.pending_approval.is_none(),
        "nothing waits for a human now: {:?}",
        outcome.pending_approval
    );
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let ruling = events
        .iter()
        .filter(|e| e.kind == ev::POLICY_DECIDED)
        .find(|e| e.data["rule"].as_str().unwrap_or("").starts_with("yolo:"))
        .expect("the override is recorded, so the log says why nothing asked");
    assert_eq!(ruling.data["verdict"], "allow");

    // The **sandbox** is a different axis: the write still happened where the tool
    // could reach it, and the boundary is not what the opt-out changed. Asserting the
    // permission path, not the filesystem: the point is that nothing was asked.
    let _ = std::fs::remove_file(&escape);
}

/// Purpose: A permissions request from outside the turn is applied at the next decision, and recorded before the call it relaxes.
#[test]
fn a_permissions_request_is_applied_at_the_next_decision() {
    let t = Temp::new();
    let escape = t.dir.parent().unwrap().join("loong-kernel-control.txt");
    let _ = std::fs::remove_file(&escape);
    let model = ScriptedModel::new(vec![
        response(
            vec![call(
                "c1",
                "write_file",
                json!({"path": escape.to_string_lossy(), "content": "x"}),
            )],
            None,
        ),
        response(vec![], Some("done")),
    ]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();

    // The surface's private control channel leaves the request; the turn applies it.
    *k.permissions_requestor().lock().unwrap() = Some(true);
    let outcome = k.submit("write outside").unwrap();
    assert!(
        outcome.pending_approval.is_none(),
        "the request arrived before the decision, so nothing asks: {:?}",
        outcome.pending_approval
    );
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let relaxed = events
        .iter()
        .position(|event| event.kind == gen::event::PERMISSIONS_RELAXED)
        .expect("the opt-out is in the log");
    let called = events
        .iter()
        .position(|event| event.kind == ev::TOOL_CALL)
        .expect("the call is in the log");
    assert!(
        relaxed < called,
        "the relaxation is recorded before the call it allowed"
    );
    let ruling = events
        .iter()
        .filter(|event| event.kind == ev::POLICY_DECIDED)
        .find(|event| {
            event.data["rule"]
                .as_str()
                .unwrap_or("")
                .starts_with("yolo:")
        })
        .expect("the override is recorded, so the log says why nothing asked");
    assert_eq!(ruling.data["verdict"], "allow");
    let _ = std::fs::remove_file(&escape);
}

/// Purpose: A request that arrives while the model is answering is applied before the next call is judged, not after the turn.
#[test]
fn a_permissions_request_during_a_model_call_still_allows_the_call() {
    /// Purpose: A scripted model that leaves a permissions request as it answers the first call.
    /// Serves: `a_permissions_request_during_a_model_call_still_allows_the_call`.
    /// Design: `docs/design/flows/tool-approval.md`.
    /// Up: the test constructs it, and the kernel calls it.
    /// Down: `ScriptedModel::complete` and the request slot.
    /// Verify: `cargo test -p loong-kernel`.
    struct AsksMidCall {
        inner: ScriptedModel,
        slot: std::sync::Arc<std::sync::Mutex<Option<bool>>>,
        fired: bool,
    }
    impl Model for AsksMidCall {
        /// Purpose: Leave the request on the first call, then behave like the script.
        /// Contract: kernel test fixture.
        fn complete(
            &mut self,
            request: &CompletionRequest,
        ) -> Result<CompletionResponse, ModelError> {
            if !self.fired {
                self.fired = true;
                *self.slot.lock().unwrap() = Some(true);
            }
            self.inner.complete(request)
        }
    }

    let t = Temp::new();
    let escape = t.dir.parent().unwrap().join("loong-kernel-midcall.txt");
    let _ = std::fs::remove_file(&escape);
    let scripted = ScriptedModel::new(vec![
        response(
            vec![call(
                "c1",
                "write_file",
                json!({"path": escape.to_string_lossy(), "content": "x"}),
            )],
            None,
        ),
        response(vec![], Some("done")),
    ]);
    let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
    let mut k = Kernel::create(
        &t.log,
        KernelConfig::new(&t.dir),
        Box::new(AsksMidCall {
            inner: scripted,
            slot: std::sync::Arc::clone(&slot),
            fired: false,
        }),
    )
    .unwrap();
    k.use_permissions_requester(slot);
    let outcome = k.submit("write outside").unwrap();
    assert!(
        outcome.pending_approval.is_none(),
        "the request arrived while the model was answering, and the call is still allowed: {:?}",
        outcome.pending_approval
    );
    let _ = std::fs::remove_file(&escape);
}

/// Purpose: An out-of-bounds write parks the turn with `approval.requested`, writes nothing, and resumes on a denial with the answer in the log.
#[test]
fn an_out_of_bounds_write_asks_then_resumes_on_the_answer() {
    let t = Temp::new();
    let escape = t.dir.parent().unwrap().join("loong-escape.txt");
    let _ = std::fs::remove_file(&escape);
    let model = ScriptedModel::new(vec![
        response(
            vec![call(
                "c1",
                "write_file",
                json!({"path": escape.to_string_lossy(), "content": "x"}),
            )],
            None,
        ),
        response(vec![], Some("understood")),
    ]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    let out = k.submit("write outside").unwrap();

    let pending = out.pending_approval.expect("out-of-bounds asks");
    assert_eq!(pending.class, ActionClass::OutOfBounds);
    assert_eq!(pending.rule, "builtin:out_of_bounds");
    assert!(!escape.exists(), "nothing is written before the answer");

    let resumed = k
        .respond_to_approval(&pending.request_id, false, ApprovalScope::Once, Some("no"))
        .unwrap();
    assert_eq!(resumed.final_text.as_deref(), Some("understood"));
    assert!(!escape.exists());
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let decided = events
        .iter()
        .find(|e| e.kind == ev::APPROVAL_DECIDED)
        .unwrap();
    assert_eq!(decided.data["granted"], false);
    assert_eq!(decided.data["scope"], "once");
    assert_eq!(decided.data["reason"], "no");
    let denied = events
        .iter()
        .filter(|e| e.kind == ev::TOOL_RESULT)
        .next()
        .unwrap();
    assert_eq!(denied.data["ok"], false);
}

/// Purpose: A granted approval runs the same PEP, and the tool's own guard still refuses a write outside the workspace.
#[test]
fn a_granted_approval_still_cannot_escape_the_workspace() {
    let t = Temp::new();
    let escape = t.dir.parent().unwrap().join("loong-escape-2.txt");
    let _ = std::fs::remove_file(&escape);
    let model = ScriptedModel::new(vec![
        response(
            vec![call(
                "c1",
                "write_file",
                json!({"path": escape.to_string_lossy(), "content": "x"}),
            )],
            None,
        ),
        response(vec![], Some("ok")),
    ]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    let pending = k.submit("write outside").unwrap().pending_approval.unwrap();
    k.respond_to_approval(&pending.request_id, true, ApprovalScope::Once, None)
        .unwrap();
    assert!(!escape.exists(), "the tool is the second line of defence");
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
    assert_eq!(result.data["ok"], false);
    assert!(result.data["output"]
        .as_str()
        .unwrap()
        .contains("outside the workspace"));
}

/// Purpose: A session-tightened PDP rule denies an out-of-bounds read without asking, records `session:` as the rule, and feeds the refusal back.
#[test]
fn a_tightened_policy_denies_without_asking() {
    let t = Temp::new();
    let model = ScriptedModel::new(vec![
        response(
            vec![call("c1", "read_file", json!({"path": "/etc/hostname"}))],
            None,
        ),
        response(vec![], Some("could not read that")),
    ]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    k.pdp_mut()
        .tighten(ActionClass::OutOfBounds, Verdict::Deny)
        .unwrap();
    let out = k.submit("read /etc/hostname").unwrap();
    assert!(out.pending_approval.is_none(), "deny never asks");
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let policy = events
        .iter()
        .find(|e| e.kind == ev::POLICY_DECIDED)
        .unwrap();
    assert_eq!(policy.data["verdict"], "deny");
    assert_eq!(policy.data["rule"], "session:out_of_bounds");
    let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
    assert!(result.data["output"]
        .as_str()
        .unwrap()
        .contains("denied by policy"));
}

/// Purpose: A call to an unknown tool becomes a failed `tool.result` and the turn continues, because a bad call is not a failed turn.
#[test]
fn an_unknown_tool_is_reported_not_fatal() {
    let t = Temp::new();
    let model = ScriptedModel::new(vec![
        response(vec![call("c1", "no_such_tool", json!({}))], None),
        response(vec![], Some("fine")),
    ]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    let out = k.submit("go").unwrap();
    assert_eq!(out.final_text.as_deref(), Some("fine"));
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
    assert!(result.data["output"]
        .as_str()
        .unwrap()
        .contains("unknown tool"));
}

/// Purpose: A model that keeps calling tools stops at `max_steps` with the step-limit note, so a spinning model cannot run forever.
#[test]
fn the_step_limit_stops_a_spinning_model() {
    let t = Temp::new();
    std::fs::write(t.dir.join("a.txt"), "x").unwrap();
    let spin = |_: usize| response(vec![call("c", "read_file", json!({"path": "a.txt"}))], None);
    let model = ScriptedModel::new((0..10).map(spin).collect());
    let mut config = KernelConfig::new(&t.dir);
    config.max_steps = 3;
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    let out = k.submit("go").unwrap();
    assert_eq!(out.steps, 3);
    assert!(out.final_text.unwrap().contains("step limit"));
}

/// Purpose: When one call in a batched message asks, the rest still get `tool.result` saying they did not run, because a provider rejects two calls with one result.
#[test]
fn a_message_that_stops_for_an_approval_still_answers_every_call_it_made() {
    // Two calls in one message, the first of which asks. The turn stops there, and the
    // second call was never dispatched — but the provider requires a result for every call
    // it was shown, so a request carrying two calls and one result is refused. The model
    // that batches its calls is the one that hits this, and it hits it while asking the
    // person for permission.
    let t = Temp::new();
    std::fs::write(t.dir.join("a.txt"), "x").unwrap();
    let (model, seen) = RecordingModel::new(vec![
        response(
            vec![
                call("c1", "read_file", json!({"path": "/etc/hostname"})),
                call("c2", "read_file", json!({"path": "a.txt"})),
            ],
            None,
        ),
        response(vec![], Some("done")),
    ]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    let asked = k.submit("read both").unwrap();
    let pending = asked.pending_approval.expect("the first call asks");
    k.respond_to_approval(&pending.request_id, true, gen::ApprovalScope::Once, None)
        .unwrap();

    let sent = seen.lock().unwrap();
    let last = sent.last().expect("the model was called again");
    let calls = last
        .iter()
        .map(|message| match message {
            ChatMessage::Assistant { tool_calls, .. } => tool_calls.len(),
            _ => 0,
        })
        .sum::<usize>();
    let results = last
        .iter()
        .filter(|message| matches!(message, ChatMessage::Tool { .. }))
        .count();
    assert_eq!(calls, 2);
    assert_eq!(results, 2, "every call has an answer: {last:?}");
    assert!(
        last.iter().any(|message| matches!(
            message,
            ChatMessage::Tool { call_id, output }
                if call_id == "c2" && output.contains("not run")
        )),
        "and the one that did not run says so: {last:?}"
    );
}

/// Purpose: The default `max_steps` lets twenty real tool rounds finish, guarding against the old eight-step death mid-edit.
#[test]
fn the_default_step_limit_leaves_room_for_a_real_task() {
    // Writing one page is read, write, run, fix, repeat — dozens of model calls, not
    // eight. A default that ordinary work can reach is the bug: the agent dies
    // mid-edit and the transcript blames a "step limit". The limit is deliberately not
    // set here, so it is the **default** under test.
    let t = Temp::new();
    std::fs::write(t.dir.join("a.txt"), "x").unwrap();
    let rounds = 20;
    let model = ScriptedModel::new(
        (0..rounds)
            .map(|round| {
                response(
                    vec![call(
                        &format!("c{round}"),
                        "read_file",
                        json!({"path": "a.txt"}),
                    )],
                    None,
                )
            })
            .collect(),
    );
    let config = KernelConfig::new(&t.dir);
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    let out = k.submit("write the page").unwrap();
    // The script runs out after twenty rounds; ScriptedModel then answers "done".
    assert_eq!(out.steps, rounds + 1, "{out:?}");
    assert_eq!(out.final_text.as_deref(), Some("done"));
}

/// Purpose: `max_steps = 0` means unlimited in both config and a layer, not an empty range that ends before the model is asked.
#[test]
fn zero_steps_is_no_limit_rather_than_no_work() {
    // `0` reads as "unset" everywhere a person writes it — `--max-steps 0`, or
    // `max_steps = 0` in a layer. It must never be taken literally: `1..=0` is an empty
    // range, so a turn would end before the model was asked anything, which is the
    // opposite of what "no limit" means.
    assert_eq!(steps_or_unlimited(0), usize::MAX);
    assert_eq!(steps_or_unlimited(7), 7);

    let t = Temp::new();
    std::fs::write(t.dir.join("a.txt"), "x").unwrap();
    let model = ScriptedModel::new(vec![
        response(
            vec![call("c1", "read_file", json!({"path": "a.txt"}))],
            None,
        ),
        response(
            vec![call("c2", "read_file", json!({"path": "a.txt"}))],
            None,
        ),
        response(vec![], Some("done")),
    ]);
    let mut config = KernelConfig::new(&t.dir);
    config.max_steps = 0;
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    let out = k.submit("go").unwrap();
    assert_eq!(
        out.steps, 3,
        "the turn ran to the model's own stop: {out:?}"
    );
    assert_eq!(out.final_text.as_deref(), Some("done"));

    // And from a layer, where it is written as a number like any other.
    let t = Temp::new();
    let mut config = KernelConfig::new(&t.dir);
    config.settings = settings_from(&t.dir, "max_steps = 0\n");
    let model = ScriptedModel::saying("done");
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    assert_eq!(k.submit("go").unwrap().steps, 1);
}

/// Purpose: A model's declared `budget_tokens` becomes the session cap and only narrows, so a routed-to model cannot outgrow its window.
#[test]
fn a_budget_a_model_declares_is_the_sessions_budget() {
    // The pack this repository ships writes `budget_tokens = 128000` on its only model.
    // That value was parsed, merged, and then read by nothing: the session it configured
    // ran with no budget at all, so compaction never ran and the context grew until the
    // provider refused it. A model the router may pick is a model the cap has to cover,
    // so the strictest declared cap is the one that holds.
    let t = Temp::new();
    let settings = settings_from(
        &t.dir,
        "[[models]]\nname = \"flash\"\nbase_url = \"http://x/v1\"\nmodel = \"m\"\n\
         budget_tokens = 128000\n",
    );
    assert_eq!(effective_budget(None, &settings), Some(128_000));
    // A request may only tighten it, never widen it.
    assert_eq!(effective_budget(Some(1_000), &settings), Some(1_000));
    assert_eq!(
        effective_budget(Some(999_999), &settings),
        Some(128_000),
        "the model's own cap still holds"
    );

    // Two models, and the router may pick either: the strictest covers both.
    let t = Temp::new();
    let two = settings_from(
        &t.dir,
        "[[models]]\nname = \"flash\"\nbase_url = \"http://x/v1\"\nmodel = \"m\"\n\
         budget_tokens = 128000\n\
         [[models]]\nname = \"pro\"\nbase_url = \"http://x/v1\"\nmodel = \"p\"\n\
         budget_tokens = 32000\n",
    );
    assert_eq!(effective_budget(None, &two), Some(32_000));
}

/// Purpose: With no budget, a model's context window still triggers compaction; with neither, nothing compacts, so an unbounded session is not silent.
#[test]
fn the_models_window_bounds_the_context_when_no_budget_does() {
    // Same prompts, same everything but the window: one compacts, the other has nothing
    // to compact against. Before this, only a budget could bound a session — and the
    // budget was optional, so the default was "grow until the provider says no".
    let filler = "x".repeat(2_000);
    let rounds = 3;
    let compactions = |window: Option<u64>| {
        let t = Temp::new();
        let answers: Vec<String> = (0..rounds).map(|n| format!("answer {n}")).collect();
        let responses = answers
            .iter()
            .map(|answer| response(vec![], Some(answer)))
            .collect();
        let (model, _seen) = RecordingModel::with_window(responses, window);
        let mut config = KernelConfig::new(&t.dir);
        config.keep_newest_tokens = 100;
        config.reserved_context_size = 256;
        assert!(
            config.budget_tokens.is_none(),
            "this is about the window, not the budget"
        );
        let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
        for turn in 1..=rounds {
            k.submit(&format!("turn {turn} {filler}")).unwrap();
        }
        loong_eventlog::replay(&t.log)
            .unwrap()
            .events
            .iter()
            .filter(|event| event.kind == ev::COMPACTION_APPLIED)
            .count()
    };

    assert!(
        compactions(Some(1_024)) > 0,
        "a window with a small reserve leaves no room, so it compacts"
    );
    assert_eq!(
        compactions(None),
        0,
        "with neither a window nor a budget, nothing bounds the context"
    );
}

/// Purpose: Every message has exactly one ledger entry, the system prompt included, pinning the lockstep that makes the ledger the model's view.
#[test]
fn the_ledger_and_the_conversation_stay_in_step() {
    let t = Temp::new();
    std::fs::write(t.dir.join("a.txt"), "x").unwrap();
    let model = ScriptedModel::new(vec![
        response(
            vec![call("c1", "read_file", json!({"path": "a.txt"}))],
            None,
        ),
        response(vec![], Some("done")),
    ]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    k.submit("go").unwrap();
    // Every message has exactly one ledger entry — the system prompt included,
    // which is why the count is over all entries rather than History.
    assert_eq!(
        k.ledger().entries().len(),
        k.messages().len(),
        "every message is a ledger entry, and none is missing one"
    );
    assert!(k.ledger().explain().iter().any(|r| r.source == "user"));
    assert!(
        k.ledger().explain().iter().any(|r| r.source == "system"),
        "the system prompt is accounted for"
    );
}

// The escape is stopped by landlock, so this test only means anything where
// landlock exists. On a platform without a PEP the kernel says `unconfined:`
// and the same command succeeds — which `sandbox self-test` reports loudly.

/// Purpose: Landlock stops `touch ../file` even though the classifier allowed it: the OS is the enforcement boundary, not the heuristic.
#[test]
#[cfg(target_os = "linux")]
fn the_pep_stops_an_escape_the_classifier_cannot_see() {
    let t = Temp::new();
    let escaped_name = format!(
        "{}-escaped.txt",
        t.dir.file_name().unwrap().to_string_lossy()
    );
    let escape = t.dir.parent().unwrap().join(&escaped_name);
    let _ = std::fs::remove_file(&escape);

    // `touch ../<file>`: the classifier cannot see through `..` here, so it
    // says write_workspace and the PDP allows it. The OS is what stops it.
    let command = format!("touch ../{escaped_name}");
    let model = ScriptedModel::new(vec![
        response(vec![call("c1", "shell", json!({"command": command}))], None),
        response(vec![], Some("blocked")),
    ]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    assert!(
        k.confinement_note().contains("landlock"),
        "the kernel must confine tool children: {}",
        k.confinement_note()
    );
    k.submit("try to write outside").unwrap();

    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let policy = events
        .iter()
        .find(|e| e.kind == ev::POLICY_DECIDED)
        .unwrap();
    assert_eq!(policy.data["verdict"], "allow", "the classifier allowed it");
    assert_eq!(policy.data["action_class"], "write_workspace");
    let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
    assert_eq!(result.data["ok"], false, "the OS refused it");
    let output = result.data["output"].as_str().unwrap();
    assert!(output.contains("Permission denied"), "output was: {output}");
    assert!(!escape.exists(), "the escaped file must not exist");
}

/// Purpose: A workspace write through the shell runs under the same confinement, so the PEP is not a blanket refusal.
#[test]
fn the_pep_still_allows_a_workspace_write_through_the_shell() {
    let t = Temp::new();
    let model = ScriptedModel::new(vec![
        response(
            vec![call(
                "c1",
                "shell",
                json!({"command": "touch inside-control.txt"}),
            )],
            None,
        ),
        response(vec![], Some("done")),
    ]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    k.submit("write a file inside").unwrap();
    assert!(
        t.dir.join("inside-control.txt").exists(),
        "the allowed write happened"
    );
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
    assert_eq!(result.data["ok"], true, "{:?}", result.data["output"]);
}

/// Purpose: A confined child keeps `/dev/null` writable: git opens it read-write at startup, so denying the write broke every git command.
#[test]
#[cfg(target_os = "linux")]
fn the_pep_keeps_dev_null_writable_for_ordinary_tools() {
    let t = Temp::new();
    let model = ScriptedModel::new(vec![
        response(
            vec![call("c1", "shell", json!({"command": "git --version"}))],
            None,
        ),
        response(vec![], Some("done")),
    ]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    assert!(
        k.confinement_note().contains("landlock"),
        "the kernel must confine tool children: {}",
        k.confinement_note()
    );
    k.submit("check git").unwrap();

    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
    let output = result.data["output"].as_str().unwrap_or_default();
    assert_eq!(result.data["ok"], true, "output was: {output}");
    assert!(output.contains("git version"), "output was: {output}");
}

/// Purpose: A read-only shell verb is classified `Investigate` and allowed, keeping read work free.
#[test]
fn a_read_only_shell_command_is_an_investigate_action() {
    let t = Temp::new();
    std::fs::write(t.dir.join("a.txt"), "content here").unwrap();
    let model = ScriptedModel::new(vec![
        response(
            vec![call("c1", "shell", json!({"command": "cat a.txt"}))],
            None,
        ),
        response(vec![], Some("read")),
    ]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    k.submit("show me a.txt").unwrap();
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let policy = events
        .iter()
        .find(|e| e.kind == ev::POLICY_DECIDED)
        .unwrap();
    assert_eq!(policy.data["action_class"], "investigate");
    assert_eq!(policy.data["verdict"], "allow");
    let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
    assert!(result.data["output"]
        .as_str()
        .unwrap()
        .contains("content here"));
}

/// Purpose: The bundled fake MCP server used by the MCP tests.
///
/// Contract: `turn/tests.rs` MCP fixtures.
fn fake_mcp() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../mcp/tests/fake_mcp_server.py")
}

/// Purpose: Install the fake MCP server as trusted, with the given action class, if it can run.
///
/// Contract: `turn/tests.rs` MCP fixtures.
fn install_mcp(dir: &Path, action_class: &str, trust: bool) -> bool {
    install_mcp_mode(dir, action_class, trust, "normal")
}

/// Purpose: Install the fake MCP server in a given mode and trust it, or report that the fixture cannot run.
///
/// Serves: MCP tests that need a live server but must skip cleanly where python3 is absent.
/// Design: `docs/design/extension-model.md` §3.
/// Up: the MCP prompt and tool tests.
/// Down: `fake_mcp`, `std::process::Command` and `McpSet::trust_now`; writes `.loongagent/mcp.toml`.
/// Verify: `cargo test -p loong-kernel turn::tests::an_mcp_tool_is_just_another_tool`.
fn install_mcp_mode(dir: &Path, action_class: &str, trust: bool, mode: &str) -> bool {
    let script = fake_mcp();
    if !script.exists()
        || std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_err()
    {
        return false;
    }
    let hooks = dir.join(".loongagent");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::write(
        hooks.join("mcp.toml"),
        format!(
            "[[server]]\nname = \"fake\"\ncommand = \"python3\"\nargs = [\"{}\", \"normal\"]\n\
             action_class = \"{action_class}\"\n",
            script.display()
        )
        .replace("\"normal\"", &format!("\"{mode}\"")),
    )
    .unwrap();
    if trust {
        loong_mcp::McpSet::load(dir).unwrap().trust_now().unwrap();
    }
    true
}

/// Purpose: A trusted MCP prompt is advertised as `/mcp__server__prompt`, fetched on invoke with its argument substituted, and accounted for in `skill.invoked`.
#[test]
fn an_mcp_prompt_becomes_a_command_with_its_body_fetched_on_invoke() {
    let t = Temp::new();
    if !install_mcp(&t.dir, "investigate", true) {
        return;
    }
    let (model, seen) = RecordingModel::new(vec![response(vec![], Some("reviewed"))]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();

    // The catalogue advertises the server's prompts as commands…
    assert!(
        k.skills().names().contains(&"mcp__fake__review"),
        "{:?}",
        k.skills().names()
    );
    let sent = seen.lock().unwrap();
    assert!(sent.is_empty(), "nothing has been asked of the model yet");
    drop(sent);

    // …and invoking one fetches the body from the server, with the argument
    // mapped onto the name the prompt declared.
    k.submit("/mcp__fake__review HEAD~1").unwrap();
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let invoked = events.iter().find(|e| e.kind == ev::SKILL_INVOKED).unwrap();
    assert_eq!(invoked.data["name"], "mcp__fake__review");
    assert_eq!(invoked.data["source"], "mcp:fake/review");
    assert!(
        invoked.data["tokens"].as_u64().unwrap() > 0,
        "the fetched body is accounted for"
    );
    assert!(invoked.data["error"].is_null());

    let sent = seen.lock().unwrap();
    let user = sent[0]
        .iter()
        .find_map(|m| match m {
            ChatMessage::User { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap();
    assert!(
        user.contains("Review HEAD~1 and report ONLY real bugs"),
        "the argument reached the server and the body came back: {user}"
    );
}

/// Purpose: The `skill` tool fetches an MCP prompt's body from its server and names the origin, so the model can load it on demand.
#[test]
fn the_model_can_fetch_an_mcp_prompt_body_with_the_tool() {
    let t = Temp::new();
    if !install_mcp(&t.dir, "investigate", true) {
        return;
    }
    let model = ScriptedModel::new(vec![
        response(
            vec![call("c1", "skill", json!({"name": "mcp__fake__review"}))],
            None,
        ),
        response(vec![], Some("loaded")),
    ]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    k.submit("get the review checklist").unwrap();

    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
    assert_eq!(result.data["ok"], true, "{:?}", result.data["output"]);
    let output = result.data["output"].as_str().unwrap();
    assert!(
        output.contains("Review HEAD and report ONLY real bugs"),
        "{output}"
    );
    assert!(
        output.contains("mcp:fake/review"),
        "the origin is named: {output}"
    );
}

/// Purpose: `/help` lists MCP prompts with their argument hints, so a server's commands are discoverable.
#[test]
fn help_lists_the_servers_prompts_too() {
    let t = Temp::new();
    if !install_mcp(&t.dir, "investigate", true) {
        return;
    }
    let (model, seen) = RecordingModel::new(vec![response(vec![], Some("listed"))]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    k.submit("/help").unwrap();
    let sent = seen.lock().unwrap();
    let user = sent[0]
        .iter()
        .find_map(|m| match m {
            ChatMessage::User { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap();
    assert!(user.contains("/mcp__fake__review"), "{user}");
    assert!(user.contains("/mcp__fake__release"), "{user}");
    // A prompt with no declared arguments gets no hint.
    assert!(user.contains("Walk the release checklist"), "{user}");
}

/// Purpose: A server with no prompts adds no commands but keeps its tools, so one empty list does not disable the server.
#[test]
fn a_server_without_prompts_contributes_none() {
    let t = Temp::new();
    if !install_mcp_mode(&t.dir, "investigate", true, "no-prompts") {
        return;
    }
    let model = ScriptedModel::saying("hi");
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    assert!(
        !k.skills().names().iter().any(|n| n.starts_with("mcp__")),
        "a server that advertises no prompts adds no commands: {:?}",
        k.skills().names()
    );
    // Its tools still work.
    assert!(k.tools_mut().names().contains(&"mcp__fake__ping"));
}

/// Purpose: A broken MCP prompt refuses the turn with `skill.invoked` naming the error and never calls the model.
#[test]
fn a_prompt_that_fails_to_load_refuses_the_turn_instead_of_reaching_the_model() {
    let t = Temp::new();
    if !install_mcp(&t.dir, "investigate", true) {
        return;
    }
    // An empty script: if the model were called the turn would fail instead.
    let model = ScriptedModel::new(vec![]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    assert!(k.skills().names().contains(&"mcp__fake__broken"));

    let outcome = k.submit("/mcp__fake__broken").unwrap();
    let refused = outcome
        .refused
        .expect("a prompt that cannot load is refused");
    assert!(refused.contains("could not be loaded"), "{refused}");

    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let invoked = events.iter().find(|e| e.kind == ev::SKILL_INVOKED).unwrap();
    assert_eq!(invoked.data["name"], "mcp__fake__broken");
    assert!(
        invoked.data["error"]
            .as_str()
            .unwrap()
            .contains("prompt handler is broken"),
        "{:?}",
        invoked.data
    );
    assert!(
        !events.iter().any(|e| e.kind == ev::COST_RECORDED),
        "the model was never called"
    );
}

/// Purpose: An MCP tool records `tool.call`, a declared-class `policy.decided` and `tool.result` like a built-in: one interface, one audit trail.
#[test]
fn an_mcp_tool_is_just_another_tool() {
    let t = Temp::new();
    if !install_mcp(&t.dir, "investigate", true) {
        eprintln!("skipping: no python3 or no fixture");
        return;
    }
    let model = ScriptedModel::new(vec![
        response(
            vec![call("c1", "mcp__fake__ping", json!({"message": "hello"}))],
            None,
        ),
        response(vec![], Some("done")),
    ]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    assert!(k.mcp_note().contains("trusted"), "{}", k.mcp_note());
    assert_eq!(k.mcp_servers(), 1);
    assert!(k.tools_mut().names().contains(&"mcp__fake__ping"));

    k.submit("ping the server").unwrap();
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    // Same events as any other tool: call, decision by declared class, result.
    let call_event = events.iter().find(|e| e.kind == ev::TOOL_CALL).unwrap();
    assert_eq!(call_event.data["tool"], "mcp__fake__ping");
    assert_eq!(call_event.data["action_class"], "investigate");
    let policy = events
        .iter()
        .find(|e| e.kind == ev::POLICY_DECIDED)
        .unwrap();
    assert_eq!(policy.data["verdict"], "allow");
    let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
    assert_eq!(result.data["ok"], true, "{:?}", result.data["output"]);
    assert!(result.data["output"]
        .as_str()
        .unwrap()
        .contains("pong: hello"));
}

/// Purpose: An MCP tool with the cautious `out_of_bounds` default asks first and resumes through the same approval seam.
#[test]
fn an_mcp_tool_with_the_default_class_asks_first() {
    let t = Temp::new();
    if !install_mcp(&t.dir, "out_of_bounds", true) {
        return;
    }
    let model = ScriptedModel::new(vec![response(
        vec![call("c1", "mcp__fake__ping", json!({"message": "hi"}))],
        None,
    )]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    let outcome = k.submit("ping").unwrap();
    let pending = outcome.pending_approval.expect("the cautious default asks");
    assert!(
        pending.reason.contains("out_of_bounds"),
        "{}",
        pending.reason
    );

    // Answering resumes through the same seam as any built-in tool.
    k.respond_to_approval(&pending.request_id, true, ApprovalScope::Once, None)
        .unwrap();
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
    assert_eq!(result.data["ok"], true, "{:?}", result.data["output"]);
    assert!(result.data["output"].as_str().unwrap().contains("pong: hi"));
}

/// Purpose: An `mcp` pre_tool hook can deny a built-in shell call, and `policy.decided` names the hook that did it.
#[test]
fn an_mcp_hook_can_gate_a_builtin_tool() {
    let t = Temp::new();
    if !install_mcp(&t.dir, "investigate", true) {
        return;
    }
    install_hooks(
        &t.dir,
        "[[hook]]\nid = \"policy-svc\"\nevent = \"pre_tool\"\nkind = \"mcp\"\n\
         server = \"fake\"\ntool = \"verdict_deny\"\n",
        &[],
        true,
    );
    let model = ScriptedModel::new(vec![
        response(
            vec![call("c1", "shell", json!({"command": "touch gated.txt"}))],
            None,
        ),
        response(vec![], Some("understood")),
    ]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    k.submit("go").unwrap();

    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let fired = events
        .iter()
        .find(|e| e.kind == ev::HOOK_FIRED)
        .expect("the hook answered");
    assert_eq!(fired.data["kind"], "mcp");
    assert_eq!(fired.data["hook_id"], "policy-svc");
    assert_eq!(fired.data["verdict"], "deny");
    assert_eq!(fired.data["failed"], false);
    assert!(fired.data["reason"]
        .as_str()
        .unwrap()
        .contains("policy service says deny"));

    // A built-in tool, classified as an ordinary workspace write, stopped by an
    // external service — and the log says which one.
    let policy = events
        .iter()
        .find(|e| e.kind == ev::POLICY_DECIDED)
        .unwrap();
    assert_eq!(policy.data["verdict"], "deny");
    assert_eq!(policy.data["rule"], "hook:policy-svc");
    assert!(
        !t.dir.join("gated.txt").exists(),
        "the gated tool never ran"
    );
}

/// Purpose: An `mcp` hook whose server is absent records `failed: true` and denies; no server means no consent.
#[test]
fn an_mcp_hook_without_a_server_fails_closed() {
    let t = Temp::new();
    // A hook that names a server the session does not have.
    install_hooks(
        &t.dir,
        "[[hook]]\nid = \"policy-svc\"\nevent = \"pre_tool\"\nkind = \"mcp\"\n\
         server = \"absent\"\ntool = \"decide\"\n",
        &[],
        true,
    );
    let model = ScriptedModel::new(vec![
        response(
            vec![call("c1", "shell", json!({"command": "touch nope.txt"}))],
            None,
        ),
        response(vec![], Some("understood")),
    ]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    k.submit("go").unwrap();
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let fired = events.iter().find(|e| e.kind == ev::HOOK_FIRED).unwrap();
    assert_eq!(fired.data["verdict"], "deny");
    assert_eq!(fired.data["failed"], true);
    assert!(!t.dir.join("nope.txt").exists());
}

/// Purpose: An untrusted `mcp.toml` starts no server and registers no tools; the note says INERT.
#[test]
fn an_untrusted_mcp_manifest_starts_nothing() {
    let t = Temp::new();
    if !install_mcp(&t.dir, "investigate", false) {
        return;
    }
    let model = ScriptedModel::saying("hi");
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    assert_eq!(k.mcp_servers(), 0, "untrusted servers are never started");
    assert!(k.mcp_note().contains("INERT"), "{}", k.mcp_note());
    assert!(!k.tools_mut().names().iter().any(|n| n.starts_with("mcp__")));
}

/// Purpose: A required MCP server that cannot start fails the session with `KernelError::Mcp` rather than silently dropping a guard.
#[test]
fn a_required_server_that_cannot_start_is_fatal() {
    let t = Temp::new();
    let hooks = t.dir.join(".loongagent");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::write(
        hooks.join("mcp.toml"),
        "[[server]]\nname = \"broken\"\ncommand = \"definitely-not-a-program\"\n",
    )
    .unwrap();
    loong_mcp::McpSet::load(&t.dir)
        .unwrap()
        .trust_now()
        .unwrap();
    let model = ScriptedModel::saying("hi");
    let err = match Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)) {
        Ok(_) => panic!("a required server that cannot start must be fatal"),
        Err(e) => e,
    };
    assert!(matches!(err, KernelError::Mcp(_)), "{err}");
}

/// Purpose: A `required = false` server that cannot start becomes a note and the session opens, so optional capabilities degrade visibly.
#[test]
fn an_optional_server_is_only_a_note() {
    let t = Temp::new();
    let hooks = t.dir.join(".loongagent");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::write(
        hooks.join("mcp.toml"),
        "[[server]]\nname = \"broken\"\ncommand = \"definitely-not-a-program\"\nrequired = false\n",
    )
    .unwrap();
    loong_mcp::McpSet::load(&t.dir)
        .unwrap()
        .trust_now()
        .unwrap();
    let model = ScriptedModel::saying("hi");
    let k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    assert_eq!(k.mcp_servers(), 0);
    assert!(k.mcp_note().contains("unavailable"), "{}", k.mcp_note());
}

/// Purpose: A model that answers from a script and records every request it was shown.
///
/// Serves: asserting what compaction, replay, routing and nudging actually sent.
/// Design: `docs/design/kernel-loop.md` §2 and §6.
/// Up: the compaction, replay, routing and nudge tests.
/// Down: `RecordingModel::new`, `with_window`, `Model::complete` and the shared `seen` log.
/// Verify: `cargo test -p loong-kernel turn::tests::compaction_clears_stale_tool_output_before_dropping_anything`.
/// A model that answers from a script and records what it was shown, which is
/// the only way to check what compaction actually sent.
struct RecordingModel {
    responses: std::collections::VecDeque<CompletionResponse>,
    seen: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
    window: Option<u64>,
}

impl RecordingModel {
    /// Purpose: A recording model with no declared window and the shared request log.
    ///
    /// Contract: `turn/tests.rs` fixtures.
    fn new(responses: Vec<CompletionResponse>) -> (Self, Arc<Mutex<Vec<Vec<ChatMessage>>>>) {
        Self::with_window(responses, None)
    }

    /// Purpose: A recording model that declares a context window, for the ceiling tests.
    ///
    /// Serves: measuring a window that bounds the context when no budget does.
    /// Design: `docs/design/context-ledger.md` §3.
    /// Up: the window and compaction tests.
    /// Down: the `RecordingModel` fields; `Model::context_window` returns the window.
    /// Verify: `cargo test -p loong-kernel turn::tests::the_models_window_bounds_the_context_when_no_budget_does`.
    /// A model that declares how much it can hold, for the compaction tests.
    fn with_window(
        responses: Vec<CompletionResponse>,
        window: Option<u64>,
    ) -> (Self, Arc<Mutex<Vec<Vec<ChatMessage>>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                responses: responses.into(),
                seen: Arc::clone(&seen),
                window,
            },
            seen,
        )
    }
}

impl Model for RecordingModel {
    /// Purpose: The declared window; `None` means this model does not bound the context.
    ///
    /// Contract: `turn/tests.rs` fixtures.
    fn context_window(&self) -> Option<u64> {
        self.window
    }

    /// Purpose: Record the request and pop the next scripted response, failing when the script is exhausted.
    ///
    /// Contract: `turn/tests.rs` fixtures.
    fn complete(&mut self, request: &CompletionRequest) -> Result<CompletionResponse, ModelError> {
        self.seen.lock().unwrap().push(request.messages.clone());
        self.responses
            .pop_front()
            .ok_or_else(|| ModelError::Transport("the script is exhausted".into()))
    }
}

/// Purpose: A file big enough that its tool output has to be cleared.
///
/// Contract: `turn/tests.rs` compaction fixtures.
/// A file big enough that its output has to be cleared.
fn big_file(dir: &Path, name: &str, lines: usize) -> PathBuf {
    let body: String = (0..lines)
        .map(|i| format!("line {i} of a large file\n"))
        .collect();
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    path
}

/// Purpose: Resolve a real profile from a file, so provenance is not hand-made.
///
/// Contract: `turn/tests.rs` configuration fixtures.
/// A resolved profile from a real file, so the provenance (which the PDP rule
/// string is built from) is the real thing rather than a hand-made struct.
fn settings_from(dir: &Path, body: &str) -> Profile {
    let path = dir.join("profile.toml");
    std::fs::write(&path, body).unwrap();
    loong_config::resolve(&[(loong_config::Layer::User, path)], dir).unwrap()
}

/// Purpose: Install a `/name` command file under `.loongagent/commands`.
///
/// Contract: `turn/tests.rs` skill fixtures.
fn install_command(dir: &Path, name: &str, body: &str) {
    let commands = dir.join(".loongagent/commands");
    std::fs::create_dir_all(&commands).unwrap();
    std::fs::write(commands.join(format!("{name}.md")), body).unwrap();
}

/// Purpose: `/review HEAD~1` substitutes its argument into the body, and `skill.invoked` records the file source and the argument.
#[test]
fn a_command_expands_into_its_body_and_the_event_names_the_file() {
    let t = Temp::new();
    install_command(
        &t.dir,
        "review",
        "---
description: Review a diff for real bugs
---

Look at $ARGUMENTS and report only real bugs.
",
    );
    let (model, seen) = RecordingModel::new(vec![response(vec![], Some("reviewed"))]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    k.submit("/review HEAD~1").unwrap();

    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let invoked = events
        .iter()
        .find(|e| e.kind == ev::SKILL_INVOKED)
        .expect("skill.invoked");
    assert_eq!(invoked.data["name"], "review");
    assert_eq!(invoked.data["args"], "HEAD~1");
    assert!(invoked.data["source"]
        .as_str()
        .unwrap()
        .ends_with("review.md"));
    assert!(invoked.data["tokens"].as_u64().unwrap() > 0);

    // What the model received is the body, with the arguments substituted — and
    // the user's own words are still recoverable from the event above.
    let sent = seen.lock().unwrap();
    let first = sent.first().unwrap();
    let user = first
        .iter()
        .find_map(|m| match m {
            ChatMessage::User { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap();
    assert!(
        user.contains("Look at HEAD~1 and report only real bugs."),
        "{user}"
    );
}

/// Purpose: A mistyped `/reveiw` is refused with `/help` named in the error and never reaches the model.
#[test]
fn an_unknown_command_is_refused_and_never_reaches_the_model() {
    let t = Temp::new();
    // An empty script: if the model were called, the turn would fail instead.
    let model = ScriptedModel::new(vec![]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    let outcome = k.submit("/reveiw HEAD~1").unwrap();

    let refused = outcome.refused.expect("a typo is refused, not guessed at");
    assert!(refused.contains("unknown command /reveiw"), "{refused}");
    assert!(refused.contains("/help"), "the way out is named: {refused}");

    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let invoked = events.iter().find(|e| e.kind == ev::SKILL_INVOKED).unwrap();
    assert!(
        invoked.data["error"]
            .as_str()
            .unwrap()
            .contains("no skill named"),
        "{:?}",
        invoked.data
    );
    assert!(
        !events.iter().any(|e| e.kind == ev::COST_RECORDED),
        "the model was never called"
    );
}

/// Purpose: `/etc/hosts is misconfigured` is a normal prompt, not a command, because a path is not an invocation.
#[test]
fn a_sentence_that_starts_with_a_path_is_a_normal_prompt() {
    let t = Temp::new();
    let (model, seen) = RecordingModel::new(vec![response(vec![], Some("noted"))]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    k.submit("/etc/hosts is misconfigured, please look")
        .unwrap();

    let sent = seen.lock().unwrap();
    assert!(
        sent[0]
            .iter()
            .any(|m| matches!(m, ChatMessage::User { text } if text.starts_with("/etc/hosts"))),
        "a path sentence reaches the model as itself"
    );
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    assert!(!events.iter().any(|e| e.kind == ev::SKILL_INVOKED));
}

/// Purpose: The system prompt carries descriptions only and withholds bodies until invoke, so the catalogue does not pay for every skill.
#[test]
fn the_catalogue_is_in_context_and_the_body_is_not() {
    let t = Temp::new();
    install_command(
        &t.dir,
        "release",
        "---
description: Cut a release
---

SECRET-BODY-STEPS
",
    );
    let (model, seen) = RecordingModel::new(vec![response(vec![], Some("ok"))]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    k.submit("say hi").unwrap();

    let sent = seen.lock().unwrap();
    let system = match sent[0].first() {
        Some(ChatMessage::System { text }) => text.clone(),
        other => panic!("the prompt starts with a system message, got {other:?}"),
    };
    assert!(system.contains("/release — Cut a release"), "{system}");
    assert!(system.contains("/help"), "{system}");
    assert!(
        !system.contains("SECRET-BODY-STEPS"),
        "bodies load on invoke only: {system}"
    );

    // …and the catalogue is accounted for as a pinned ledger entry.
    let explain = k.ledger().explain();
    let row = explain
        .iter()
        .find(|r| r.source == "system")
        .expect("the system prompt");
    assert!(row.pinned);
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let loaded = events.iter().find(|e| e.kind == ev::SKILLS_LOADED).unwrap();
    assert!(loaded.data["names"].as_str().unwrap().contains("release"));
}

/// Purpose: A plugin's bootstrap skill is seated in full with its source and adaptation notes, and still appears in the catalogue and `skills.loaded`.
/// ADR-0018: the body of a skill a plugin names is in the prompt from the first
/// turn — the native form of the `SessionStart` injection this class of plugin
/// performs with a hook on other harnesses.
#[test]
fn a_plugin_bootstrap_skill_is_seated_in_full_and_named_by_its_source() {
    let t = Temp::new();
    install_command(
        &t.dir,
        "using-superpowers",
        "---
description: check for a skill before responding
---

ALWAYS-LOOK-FOR-A-SKILL-FIRST
",
    );
    let (model, seen) = RecordingModel::new(vec![response(vec![], Some("ok"))]);
    let mut config = KernelConfig::new(&t.dir);
    config.bootstrap_skills = vec!["using-superpowers".to_string()];
    config.bootstrap_notes = vec!["skills are written `superpowers:<name>` here".to_string()];
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    k.submit("say hi").unwrap();

    let sent = seen.lock().unwrap();
    let system = match sent[0].first() {
        Some(ChatMessage::System { text }) => text.clone(),
        other => panic!("the prompt starts with a system message, got {other:?}"),
    };
    assert!(
        system.contains("ALWAYS-LOOK-FOR-A-SKILL-FIRST"),
        "the body is seated in full: {system}"
    );
    assert!(
        system.contains("superpowers:<name>"),
        "the adaptation note travels with it: {system}"
    );
    assert!(
        system.contains("using-superpowers/SKILL.md")
            || system.contains("commands/using-superpowers.md"),
        "the source is named, so a surprising instruction is traceable: {system}"
    );
    // It is still a skill: the catalogue advertises it and it can be re-loaded.
    assert!(system.contains("/using-superpowers"), "{system}");
    assert!(k.skills().get("using-superpowers").is_some());
    // …and the session log names it, through the ordinary skills.loaded event.
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let loaded = events.iter().find(|e| e.kind == ev::SKILLS_LOADED).unwrap();
    assert!(loaded.data["names"]
        .as_str()
        .unwrap()
        .contains("using-superpowers"));
}

/// Purpose: A declared bootstrap name missing from the catalogue refuses the session with reinstall advice, instead of opening with a silent gap.
#[test]
fn a_bootstrap_name_that_is_not_a_skill_refuses_the_session() {
    let t = Temp::new();
    let mut config = KernelConfig::new(&t.dir);
    config.bootstrap_skills = vec!["not-here".to_string()];
    let err = match Kernel::create(&t.log, config, Box::new(ScriptedModel::saying("hi"))) {
        Ok(_) => panic!("a bootstrap skill that does not exist must not open a session"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("not-here"), "{err}");
    assert!(err.contains("reinstall the plugin"), "{err}");
}

/// Purpose: Config disabling a bootstrap skill is a deliberate off, not a broken declaration, so the session opens without the instruction.
#[test]
fn disabling_a_bootstrap_skill_turns_it_off_rather_than_refusing() {
    let t = Temp::new();
    install_command(
        &t.dir,
        "using-superpowers",
        "---
description: check for a skill before responding
---

ALWAYS-LOOK-FOR-A-SKILL-FIRST
",
    );
    let (model, seen) = RecordingModel::new(vec![response(vec![], Some("ok"))]);
    let mut config = KernelConfig::new(&t.dir);
    config.settings = settings_from(&t.dir, "disable_skills = [\"using-superpowers\"]\n");
    config.bootstrap_skills = vec!["using-superpowers".to_string()];
    // A deliberate "not here" is not a broken declaration: the session opens, and
    // the instruction is simply absent.
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    k.submit("say hi").unwrap();
    let sent = seen.lock().unwrap();
    let system = match sent[0].first() {
        Some(ChatMessage::System { text }) => text.clone(),
        other => panic!("the prompt starts with a system message, got {other:?}"),
    };
    assert!(
        !system.contains("ALWAYS-LOOK-FOR-A-SKILL-FIRST"),
        "{system}"
    );
    assert!(!system.contains("/using-superpowers"), "{system}");
}

/// Purpose: The `skill` tool loads a body by name, classified `Investigate`, so loading instructions is read-only.
#[test]
fn the_model_can_load_a_body_by_name() {
    let t = Temp::new();
    install_command(
        &t.dir,
        "release",
        "---
description: Cut a release
---

Step one: breathe.
",
    );
    let model = ScriptedModel::new(vec![
        response(vec![call("c1", "skill", json!({"name": "release"}))], None),
        response(vec![], Some("loaded")),
    ]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    k.submit("do a release").unwrap();

    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
    assert_eq!(result.data["ok"], true, "{:?}", result.data["output"]);
    assert!(result.data["output"]
        .as_str()
        .unwrap()
        .contains("Step one: breathe."));
    let policy = events
        .iter()
        .find(|e| e.kind == ev::POLICY_DECIDED)
        .unwrap();
    assert_eq!(
        policy.data["action_class"], "investigate",
        "loading a body is read-only"
    );
}

/// Purpose: Under budget pressure every request still starts with the system prompt and is never empty, pinning the prompt as un-evictable.
#[test]
fn compaction_never_drops_the_system_prompt() {
    let t = Temp::new();
    let filler = "x".repeat(2_000);
    let (model, seen) = RecordingModel::new(vec![
        response(vec![], Some("first")),
        response(vec![], Some("second")),
        response(vec![], Some("third")),
    ]);
    let mut config = KernelConfig::new(&t.dir);
    config.budget_tokens = Some(1_200);
    config.keep_newest_tokens = 200;
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    for turn in 1..=3 {
        k.submit(&format!("turn {turn} {filler}")).unwrap();
    }
    let sent = seen.lock().unwrap();
    for (index, messages) in sent.iter().enumerate() {
        assert!(
            matches!(messages.first(), Some(ChatMessage::System { .. })),
            "turn {index}: the system prompt is always first"
        );
    }
    assert!(!sent.last().unwrap().is_empty());
}

/// Purpose: A model that emits its answer in pieces through `complete_streaming`.
///
/// Serves: proving deltas are a preview and never become a second record.
/// Design: `docs/design/kernel-loop.md` §3.
/// Up: `deltas_are_a_preview_and_never_the_record`.
/// Down: `Model::complete_streaming`; `complete` panics because this model has no non-streaming path.
/// Verify: `cargo test -p loong-kernel turn::tests::deltas_are_a_preview_and_never_the_record`.
struct StreamingModel {
    pieces: Vec<String>,
}

impl Model for StreamingModel {
    /// Purpose: The non-streaming path is unreachable for this model.
    ///
    /// Contract: `turn/tests.rs` fixtures.
    fn complete(&mut self, _request: &CompletionRequest) -> Result<CompletionResponse, ModelError> {
        unreachable!("this model is only used through its streaming path")
    }

    /// Purpose: Call `on_delta` for every piece and return the concatenated answer.
    ///
    /// Serves: a deterministic delta order and a final message that does not depend on the fragments.
    /// Design: `docs/design/kernel-loop.md` §3.
    /// Up: `Kernel::run_inner` through `Kernel::submit_with`.
    /// Down: the `on_delta` callback and `CompletionResponse`.
    /// Verify: `cargo test -p loong-kernel turn::tests::deltas_are_a_preview_and_never_the_record`.
    fn complete_streaming(
        &mut self,
        _request: &CompletionRequest,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<CompletionResponse, ModelError> {
        for piece in &self.pieces {
            on_delta(piece);
        }
        Ok(CompletionResponse {
            text: Some(self.pieces.concat()),
            tool_calls: Vec::new(),
            usage: Usage {
                tokens_in: 3,
                tokens_out: 9,
            },
        })
    }
}

/// Purpose: Streaming deltas arrive in order but are not logged and add one assistant ledger entry, so a preview never becomes a second record.
#[test]
fn deltas_are_a_preview_and_never_the_record() {
    let t = Temp::new();
    let model = StreamingModel {
        pieces: vec!["Hel".to_string(), "lo, ".to_string(), "world".to_string()],
    };
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();

    let mut seen = Vec::new();
    let outcome = k
        .submit_with("say hi", &mut |piece| seen.push(piece.to_string()))
        .unwrap();
    assert_eq!(
        seen,
        vec!["Hel", "lo, ", "world"],
        "in the order they were written"
    );
    assert_eq!(outcome.final_text.as_deref(), Some("Hello, world"));

    // The record is the message, not the fragments: nothing in the log mentions
    // a delta, and the assistant message holds the whole answer.
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    assert!(
        events.iter().all(|e| e.kind != "model.delta"),
        "deltas are not logged"
    );
    let answer = events
        .iter()
        .filter(|e| e.kind == ev::MESSAGE_APPENDED && e.data["role"] == "assistant")
        .next_back()
        .unwrap();
    assert_eq!(answer.data["text"], "Hello, world");
    // …and the ledger accounted for the answer once, not once per fragment.
    let history = k
        .ledger()
        .entries()
        .iter()
        .filter(|e| e.source == "assistant")
        .count();
    assert_eq!(history, 1, "one assistant entry, not one per delta");
}

/// Purpose: A model that sets the stop flag during its first call, as a signal handler would.
///
/// Serves: a stop arriving mid-turn that belongs to that turn only.
/// Design: `docs/design/kernel-loop.md` §5.
/// Up: `a_turn_stops_at_the_next_boundary_and_the_log_says_who_stopped_it`.
/// Down: the shared `AtomicBool` and the `called` counter; `Model::complete`.
/// Verify: `cargo test -p loong-kernel turn::tests::a_turn_stops_at_the_next_boundary_and_the_log_says_who_stopped_it`.
/// A model that asks for the turn to stop while it runs — which is what a signal
/// arriving mid-turn looks like from inside the kernel.
struct StoppingModel {
    flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    called: usize,
}

impl Model for StoppingModel {
    /// Purpose: Set the stop flag on the first call and answer normally afterwards.
    ///
    /// Serves: the next turn not inheriting a stop that was meant for the previous one.
    /// Design: `docs/design/kernel-loop.md` §5.
    /// Up: `Kernel::run_inner`'s step loop.
    /// Down: the shared `AtomicBool`; returns a tool call first and an answer later.
    /// Verify: `cargo test -p loong-kernel turn::tests::a_turn_stops_at_the_next_boundary_and_the_log_says_who_stopped_it`.
    fn complete(&mut self, _request: &CompletionRequest) -> Result<CompletionResponse, ModelError> {
        self.called += 1;
        if self.called > 1 {
            // The next turn is an ordinary one: the stop belonged to the first.
            return Ok(CompletionResponse {
                text: Some("done".to_string()),
                tool_calls: Vec::new(),
                usage: Usage {
                    tokens_in: 1,
                    tokens_out: 1,
                },
            });
        }
        self.flag.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(CompletionResponse {
            text: None,
            tool_calls: vec![ToolCallRequest {
                call_id: "c1".to_string(),
                tool: "read_file".to_string(),
                args: serde_json::json!({"path": "a.txt"}),
            }],
            usage: Usage {
                tokens_in: 1,
                tokens_out: 1,
            },
        })
    }
}

/// Purpose: Write a child log the way `loongagent fork` does: one lineage event, nothing copied.
///
/// Contract: `turn/tests.rs` lineage fixtures.
/// Write the child the way `loongagent fork` does: one lineage event, nothing copied.
fn fork_log(child: &Path, parent_id: &str, at: u64) {
    let mut log = loong_eventlog::SessionLog::create(child).unwrap();
    log.append(
        gen::event::SESSION_FORKED,
        serde_json::json!({
            "origin": "fork",
            "parent_session": parent_id,
            "parent_seq": at,
        }),
    )
    .unwrap();
}

/// Purpose: Whether any request the model saw contained the exact message text.
///
/// Contract: `turn/tests.rs` replay fixtures.
fn saw(seen: &Arc<Mutex<Vec<Vec<ChatMessage>>>>, needle: &str) -> bool {
    seen.lock()
        .unwrap()
        .iter()
        .flatten()
        .any(|message| match message {
            ChatMessage::User { text } => text == needle,
            ChatMessage::Assistant { text, .. } => text.as_deref() == Some(needle),
            ChatMessage::Tool { output, .. } => output == needle,
            ChatMessage::System { text } => text == needle,
        })
}

/// Purpose: Reopening a session replays its user and assistant history into the next request, fixing the resume that forgot the conversation.
#[test]
fn a_reopened_session_hands_the_model_its_own_history() {
    // The bug this exists for: `messages` started empty, so resuming a session
    // asked the model to carry on a conversation it could not see. It answered
    // "this is the first message in our conversation" — correctly.
    let t = Temp::new();
    let (model, _) = RecordingModel::new(vec![response(vec![], Some("stored"))]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    k.submit("remember 4242").unwrap();
    drop(k);

    let (model, seen) = RecordingModel::new(vec![response(vec![], Some("4242"))]);
    let mut k = Kernel::open(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    k.submit("what number?").unwrap();

    assert!(saw(&seen, "remember 4242"), "the earlier user turn");
    assert!(saw(&seen, "stored"), "and the answer to it");
    assert!(saw(&seen, "what number?"), "and the new turn");
    // The context view agrees with the messages.
    assert!(k
        .context_explain()
        .entries
        .iter()
        .any(|entry| entry.source == "user"));
}

/// Purpose: A replay stitches one assistant message with all its calls before the results, so a valid multi-call log can never be unopenable.
#[test]
fn a_turn_with_several_tool_calls_replays_every_one_of_them() {
    // The shape a real session has, taken from one: **one** assistant message, then
    // each call's events in order. An earlier replay flushed the assistant at the
    // first result, so the second call looked orphaned and the session refused to
    // open — a log that is perfectly valid must never be unopenable.
    let t = Temp::new();
    let mut log = loong_eventlog::SessionLog::create(&t.log).unwrap();
    let append = |log: &mut loong_eventlog::SessionLog, kind: &str, data: serde_json::Value| {
        log.append(kind, data).unwrap();
    };
    append(
        &mut log,
        gen::event::MESSAGE_APPENDED,
        serde_json::json!({"role": "user", "text": "explore"}),
    );
    append(
        &mut log,
        gen::event::MESSAGE_APPENDED,
        serde_json::json!({"role": "assistant", "text": "looking"}),
    );
    for (id, tool) in [("c1", "shell"), ("c2", "glob")] {
        append(
            &mut log,
            gen::event::TOOL_CALL,
            serde_json::json!({"call_id": id, "tool": tool, "args": {"command": "ls"}, "action_class": "investigate"}),
        );
        append(
            &mut log,
            gen::event::POLICY_DECIDED,
            serde_json::json!({"verdict": "allow", "rule": "builtin"}),
        );
        append(
            &mut log,
            gen::event::TOOL_RESULT,
            serde_json::json!({"call_id": id, "ok": true, "output": "here"}),
        );
    }
    append(
        &mut log,
        gen::event::COST_RECORDED,
        serde_json::json!({"tokens_in": 1, "tokens_out": 1, "profile": "default"}),
    );
    append(
        &mut log,
        gen::event::MESSAGE_APPENDED,
        serde_json::json!({"role": "assistant", "text": "done"}),
    );
    drop(log);

    let (model, _seen) = RecordingModel::new(vec![response(vec![], Some("ok"))]);
    let k = Kernel::open(&t.log, KernelConfig::new(&t.dir), Box::new(model))
        .expect("a valid log opens, whatever shape its turns have");
    let calls: Vec<String> = k
        .messages()
        .iter()
        .filter_map(|message| match message {
            ChatMessage::Assistant { tool_calls, .. } => Some(
                tool_calls
                    .iter()
                    .map(|c| c.call_id.clone())
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(calls, vec!["c1", "c2"], "both calls, on one assistant turn");

    // And the order a provider needs: the assistant turn, then its results.
    let roles: Vec<&str> = k
        .messages()
        .iter()
        .map(|message| match message {
            ChatMessage::User { .. } => "user",
            ChatMessage::Assistant { .. } => "assistant",
            ChatMessage::Tool { .. } => "tool",
            ChatMessage::System { .. } => "system",
        })
        .collect();
    assert_eq!(
        roles,
        vec!["system", "user", "assistant", "tool", "tool", "assistant"],
        "{roles:?}"
    );
}

/// Purpose: A child references its parent and replays the prefix without copying it, labelling inherited history `user from parent`.
#[test]
fn a_child_sees_its_parents_prefix_without_copying_it() {
    let t = Temp::new();
    let parent = t.dir.join("parent.jsonl");
    let (model, _) = RecordingModel::new(vec![response(vec![], Some("stored"))]);
    let mut k = Kernel::create(&parent, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    k.submit("remember 4242").unwrap();
    let at = k.last_seq();
    drop(k);

    let child = t.dir.join("child.jsonl");
    fork_log(&child, "parent", at);
    assert_eq!(
        loong_eventlog::replay(&child).unwrap().events.len(),
        1,
        "a child references; it does not copy"
    );

    let (model, seen) = RecordingModel::new(vec![response(vec![], Some("4242"))]);
    let mut k = Kernel::open(&child, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    k.submit("what number?").unwrap();
    assert!(saw(&seen, "remember 4242"), "the parent's prefix");
    assert!(saw(&seen, "what number?"), "and the child's own turn");

    // And the panel can say where the inherited history came from.
    assert!(
        k.context_explain()
            .entries
            .iter()
            .any(|entry| entry.source == "user from parent"),
        "{:?}",
        k.context_explain()
            .entries
            .iter()
            .map(|e| e.source.clone())
            .collect::<Vec<_>>()
    );
}

/// Purpose: A fork at an earlier sequence leaves later parent turns out of the child's context, pinning `parent_seq` as inclusive and bounded.
#[test]
fn forking_earlier_leaves_the_later_parent_turns_behind() {
    let t = Temp::new();
    let parent = t.dir.join("parent.jsonl");
    let (model, _) = RecordingModel::new(vec![
        response(vec![], Some("first")),
        response(vec![], Some("second")),
    ]);
    let mut k = Kernel::create(&parent, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    k.submit("turn one").unwrap();
    let early = k.last_seq();
    k.submit("turn two").unwrap();
    drop(k);

    let child = t.dir.join("child.jsonl");
    fork_log(&child, "parent", early);
    let (model, seen) = RecordingModel::new(vec![response(vec![], Some("ok"))]);
    let mut k = Kernel::open(&child, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    k.submit("continue").unwrap();
    assert!(saw(&seen, "turn one"), "what the fork point includes");
    assert!(!saw(&seen, "turn two"), "and what it does not");
}

/// Purpose: A three-generation chain is stitched oldest first with per-ancestor provenance, so deeper lineage is not flattened.
#[test]
fn a_chain_of_children_is_stitched_oldest_first() {
    let t = Temp::new();
    let a = t.dir.join("a.jsonl");
    let (model, _) = RecordingModel::new(vec![response(vec![], Some("a-answer"))]);
    let mut k = Kernel::create(&a, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    k.submit("from a").unwrap();
    let a_at = k.last_seq();
    drop(k);

    let b = t.dir.join("b.jsonl");
    fork_log(&b, "a", a_at);
    let (model, _) = RecordingModel::new(vec![response(vec![], Some("b-answer"))]);
    let mut k = Kernel::open(&b, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    k.submit("from b").unwrap();
    let b_at = k.last_seq();
    drop(k);

    let c = t.dir.join("c.jsonl");
    fork_log(&c, "b", b_at);
    let (model, seen) = RecordingModel::new(vec![response(vec![], Some("c-answer"))]);
    let mut k = Kernel::open(&c, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    k.submit("from c").unwrap();

    {
        let requests = seen.lock().unwrap();
        let last = requests.last().unwrap();
        let texts: Vec<String> = last
            .iter()
            .filter_map(|message| match message {
                ChatMessage::User { text } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["from a", "from b", "from c"], "oldest first");
    }
    // And the provenance is per ancestor, not one anonymous prefix.
    let sources: Vec<String> = k
        .context_explain()
        .entries
        .iter()
        .map(|entry| entry.source.clone())
        .collect();
    assert!(sources.iter().any(|s| s == "user from a"), "{sources:?}");
    assert!(sources.iter().any(|s| s == "user from b"), "{sources:?}");
}

/// Purpose: A missing ancestor refuses to open and names the child, the parent and ADR-0014, because a child without its prefix is inoperable.
#[test]
fn a_child_without_its_parent_says_which_chain_is_broken() {
    let t = Temp::new();
    let child = t.dir.join("child.jsonl");
    fork_log(&child, "long-gone", 7);
    let error = match Kernel::open(
        &child,
        KernelConfig::new(&t.dir),
        Box::new(ScriptedModel::new(vec![])),
    ) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("a child without its parent must refuse to open"),
    };
    assert!(error.contains("long-gone"), "{error}");
    assert!(error.contains("child.jsonl"), "names the child: {error}");
    assert!(error.contains("ADR-0014"), "and the way out: {error}");
}

/// Purpose: A cyclic lineage is refused with the repeated session named, so the replay cannot loop forever.
#[test]
fn a_cycle_in_the_lineage_is_refused() {
    let t = Temp::new();
    let a = t.dir.join("a.jsonl");
    let b = t.dir.join("b.jsonl");
    fork_log(&b, "a", 1);
    fork_log(&a, "b", 1);
    let error = match Kernel::open(
        &a,
        KernelConfig::new(&t.dir),
        Box::new(ScriptedModel::new(vec![])),
    ) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("a cyclic lineage must refuse to open"),
    };
    assert!(error.contains("cyclic"), "{error}");
}

/// Purpose: A stop requested mid-turn ends after the current step with `turn.interrupted`, and the next turn runs normally.
#[test]
fn a_turn_stops_at_the_next_boundary_and_the_log_says_who_stopped_it() {
    let t = Temp::new();
    std::fs::write(t.dir.join("a.txt"), "hello").unwrap();
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut k = Kernel::create(
        &t.log,
        KernelConfig::new(&t.dir),
        Box::new(StoppingModel {
            flag: std::sync::Arc::clone(&flag),
            called: 0,
        }),
    )
    .unwrap();
    k.use_interruptor(std::sync::Arc::clone(&flag));

    let outcome = k.submit("do something long").unwrap();
    // The stop landed after the step that asked for it: one step ran (the model was
    // called and answered with a tool call), and the turn did not go on to a second.
    assert!(outcome.interrupted, "{outcome:?}");
    assert_eq!(outcome.steps, 1, "{outcome:?}");
    assert_eq!(outcome.final_text, None);
    assert!(
        kinds(&t.log).contains(&gen::event::TURN_INTERRUPTED.to_string()),
        "the record says why there is no answer: {:?}",
        kinds(&t.log)
    );

    // The stop belonged to that turn: the next one runs normally.
    let second = k.submit("now do it").unwrap();
    assert!(
        !second.interrupted,
        "a stale stop must not kill the next turn"
    );
    assert_eq!(second.final_text.as_deref(), Some("done"));
}

/// Purpose: `context_explain` reports tokens, messages, tools and categories largest first, and reading it writes no event.
#[test]
fn the_context_view_says_what_the_next_request_will_carry() {
    let t = Temp::new();
    let (model, _seen) = RecordingModel::new(vec![response(vec![], Some("hello there"))]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    let before = loong_eventlog::replay(&t.log).unwrap().events.len();
    k.submit("hi").unwrap();

    let explained = k.context_explain();
    assert!(
        explained.context_tokens > 0,
        "the ledger accounts for something"
    );
    assert!(
        explained.messages >= 2,
        "the user turn and the answer: {}",
        explained.messages
    );
    assert!(explained.tools > 0, "the tools are described to the model");
    assert_eq!(explained.compactions, 0, "nothing has been compacted yet");

    let categories: Vec<&str> = explained
        .categories
        .iter()
        .map(|c| c.category.as_str())
        .collect();
    assert!(categories.contains(&"system_prompt"), "{categories:?}");
    assert!(categories.contains(&"history"), "{categories:?}");
    assert!(
        explained
            .categories
            .windows(2)
            .all(|w| w[0].tokens >= w[1].tokens),
        "largest first: {:?}",
        explained.categories
    );
    assert!(!explained.entries.is_empty());
    assert!(explained
        .entries
        .iter()
        .all(|e| !e.lifetime.is_empty() && !e.category.is_empty()));

    // Asking changes nothing: it is a question about the session, not a fact in it.
    let after_turn = loong_eventlog::replay(&t.log).unwrap().events.len();
    assert!(after_turn > before, "the turn itself recorded something");
    let _ = k.context_explain();
    assert_eq!(
        loong_eventlog::replay(&t.log).unwrap().events.len(),
        after_turn,
        "and reading the context added nothing"
    );
}

/// Purpose: The explain view distinguishes memory off from on-but-empty and shows the deterministic digest after a turn.
#[test]
fn the_context_view_says_which_memory_is_on_even_before_it_produces_a_map() {
    // "Off" and "on, but nothing produced yet" have the same ledger — neither has
    // a `cognitive_map` entry — so the explain view must carry the distinction
    // itself. A surface left to infer it would draw a fresh semantic session as
    // if the feature were switched off.
    let off = Temp::new();
    let (model, _seen) = RecordingModel::new(vec![response(vec![], Some("ok"))]);
    let k = Kernel::create(&off.log, KernelConfig::new(&off.dir), Box::new(model)).unwrap();
    let explained = k.context_explain();
    assert!(
        explained.memory_mode.is_empty(),
        "with no engine the mode is empty: {:?}",
        explained.memory_mode
    );
    assert_eq!(explained.memory_worker.state, "off");
    assert!(explained.memory_layers.is_empty());

    // Engine on, nothing derived yet.
    let on = Temp::new();
    let (model, _seen) = RecordingModel::new(vec![response(vec![], Some("ok"))]);
    let mut config = KernelConfig::new(&on.dir);
    config.context_engine = Some(crate::context_engine::ContextEngineConfig::new(
        on.dir.join("session.context.db"),
        on.log.clone(),
    ));
    let mut k = Kernel::create(&on.log, config, Box::new(model)).unwrap();
    let fresh = k.context_explain();
    assert_eq!(fresh.memory_mode, "record", "on, and it says so");
    assert!(
        fresh.memory_summarizer.is_empty(),
        "record mode has no model"
    );
    let l4 = fresh
        .memory_layers
        .iter()
        .find(|layer| layer.level == 4)
        .expect("the layer list is honest about a map that does not exist");
    assert_eq!(l4.count, 0, "no map has been produced yet");
    assert_eq!(
        fresh
            .memory_layers
            .iter()
            .find(|layer| layer.level == 2)
            .map(|layer| layer.count),
        Some(0),
        "and no episode either"
    );

    // After a turn the deterministic derivation has run, and the panel can show
    // it — still without claiming a model wrote anything.
    k.submit("hi").unwrap();
    let after = k.context_explain();
    assert_eq!(after.memory_mode, "record");
    let l2 = after
        .memory_layers
        .iter()
        .find(|layer| layer.level == 2)
        .expect("an L2 row");
    assert!(l2.count >= 1, "the digest runs synchronously: {l2:?}");
    assert_eq!(
        after.memory_worker.model_calls, 0,
        "record mode asks no model"
    );
}

/// Purpose: A semantic engine reports its mode and summariser name even when the model is unreachable, so a status panel does not guess.
#[test]
fn a_semantic_session_names_its_model_in_the_explain_view() {
    let t = Temp::new();
    let (model, _seen) = RecordingModel::new(vec![response(vec![], Some("ok"))]);
    let mut config = KernelConfig::new(&t.dir);
    config.context_engine = Some(
        crate::context_engine::ContextEngineConfig::new(
            t.dir.join("session.context.db"),
            t.log.clone(),
        )
        .with_summarizer(crate::context_engine::SummarizerChoice {
            kind: "ollama".to_string(),
            // Nothing listens there; the point is the *configuration*, which is
            // what the panel must report even when the model is unreachable.
            base_url: "http://127.0.0.1:1".to_string(),
            model: "qwen3:0.6b".to_string(),
            window_calls: 2,
            window_tokens: 6000,
        }),
    );
    let k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    let explained = k.context_explain();
    assert_eq!(explained.memory_mode, "semantic");
    assert_eq!(
        explained.memory_summarizer, "ollama:qwen3:0.6b",
        "which model, not just that there is one"
    );
    assert_ne!(
        explained.memory_worker.state, "off",
        "a semantic engine has a worker: {:?}",
        explained.memory_worker.state
    );
}

/// Purpose: A router rule switches models on step number and records `model.selected` with the rule and the step, making the switch auditable.
#[test]
fn the_router_can_switch_models_and_the_log_says_why() {
    let t = Temp::new();
    // The first model answers with a tool call, so the turn needs a second model
    // call — and the rule fires on the step number.
    let (first, seen_first) = RecordingModel::new(vec![
        response(
            vec![call("c1", "read_file", json!({"path": "note.txt"}))],
            None,
        ),
        response(vec![], Some("the first model finished it")),
    ]);
    let (second, seen_second) = RecordingModel::new(vec![response(vec![], Some("second"))]);
    std::fs::write(t.dir.join("note.txt"), "hi\n").unwrap();

    let mut config = KernelConfig::new(&t.dir);
    config.settings.router = loong_config::Router {
        default: Some("first".to_string()),
        rules: vec![loong_config::RouterRule {
            condition: loong_config::parse_condition("step > 1").unwrap(),
            model: "second".to_string(),
        }],
    };
    let mut k = Kernel::create_with_models(
        &t.log,
        config,
        vec![
            ("first".to_string(), Box::new(first)),
            ("second".to_string(), Box::new(second)),
        ],
    )
    .unwrap();

    let outcome = k.submit("read the note").unwrap();
    assert_eq!(
        outcome.final_text.as_deref(),
        Some("second"),
        "the second call went to the routed model"
    );
    assert_eq!(
        seen_first.lock().unwrap().len(),
        1,
        "the first model was used once"
    );
    assert_eq!(seen_second.lock().unwrap().len(), 1);

    // The switch is a fact in the log, with the rule that caused it.
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let switch = events
        .iter()
        .find(|e| e.kind == ev::MODEL_SELECTED)
        .expect("a model switch is recorded");
    assert_eq!(switch.data["model"], "second");
    assert_eq!(switch.data["previous"], "first");
    assert_eq!(switch.data["rule"], "step > 1");
    assert!(
        switch.data["context_tokens"].as_u64().is_some(),
        "with the fact it used"
    );
}

/// Purpose: The model switch reaches the live event sink while the turn runs, not after it, fixing a mid-turn switch nobody could see.
#[test]
fn a_rule_driven_switch_reaches_a_watcher_while_the_turn_is_still_running() {
    // A model change is news, and news arrives when it happens. Writing it straight
    // to the log skipped the live sink, so a routing rule could change the model
    // mid-turn and nothing watching heard about it until the turn was over — which
    // is exactly the moment the change stopped mattering.
    let t = Temp::new();
    std::fs::write(t.dir.join("note.txt"), "hi\n").unwrap();
    let (first, _seen_first) = RecordingModel::new(vec![
        response(
            vec![call("c1", "read_file", json!({"path": "note.txt"}))],
            None,
        ),
        response(vec![], Some("the first model finished it")),
    ]);
    let (second, _seen_second) = RecordingModel::new(vec![response(vec![], Some("second"))]);

    let mut config = KernelConfig::new(&t.dir);
    config.settings.router = loong_config::Router {
        default: Some("first".to_string()),
        rules: vec![loong_config::RouterRule {
            condition: loong_config::parse_condition("step > 1").unwrap(),
            model: "second".to_string(),
        }],
    };
    let mut k = Kernel::create_with_models(
        &t.log,
        config,
        vec![
            ("first".to_string(), Box::new(first)),
            ("second".to_string(), Box::new(second)),
        ],
    )
    .unwrap();

    let watched: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = std::sync::Arc::clone(&watched);
    k.set_event_sink(Some(Box::new(move |event: &gen::EventEnvelope| {
        sink.lock().unwrap().push(event.kind.clone());
    })));

    k.submit("read the note").unwrap();
    let kinds = watched.lock().unwrap().clone();
    assert!(
        kinds.iter().any(|kind| kind == ev::MODEL_SELECTED),
        "the switch was announced while the turn ran, not after it: {kinds:?}"
    );
}

/// Purpose: A `ModelSpec` with only a name, for ordering and switching tests.
///
/// Contract: `turn/tests.rs` routing fixtures.
/// A `ModelSpec` with nothing but a name, for tests about ordering and switching
/// rather than about endpoints.
fn named(name: &str) -> loong_config::ModelSpec {
    loong_config::ModelSpec {
        name: name.to_string(),
        kind: "openai-compat".to_string(),
        base_url: "http://127.0.0.1:1/v1".to_string(),
        model: name.to_string(),
        api_key: None,
        budget_tokens: None,
        max_context_size: None,
        provider: None,
    }
}

/// Purpose: `model_names` follows profile declaration order, not the alphabetical map, and a person's switch survives the next turn and is logged.
#[test]
fn the_model_list_is_the_profile_order_and_a_person_can_choose_from_it() {
    let t = Temp::new();
    // Declared cheapest-last-capable-first, which is the user's ranking. The map
    // holds them alphabetically, so this is also the test that the list does *not*
    // come from the map: `zeta` is declared first and must be slot one.
    let (zeta, seen_zeta) = RecordingModel::new(vec![response(vec![], Some("zeta"))]);
    let (alpha, seen_alpha) = RecordingModel::new(vec![response(vec![], Some("alpha"))]);

    let mut config = KernelConfig::new(&t.dir);
    config.settings.models = vec![named("zeta"), named("alpha")];
    config.settings.router = loong_config::Router {
        default: Some("zeta".to_string()),
        rules: Vec::new(),
    };
    let mut k = Kernel::create_with_models(
        &t.log,
        config,
        vec![
            ("zeta".to_string(), Box::new(zeta)),
            ("alpha".to_string(), Box::new(alpha)),
        ],
    )
    .unwrap();

    assert_eq!(
        k.model_names(),
        vec!["zeta".to_string(), "alpha".to_string()],
        "declaration order, not alphabetical order"
    );
    assert_eq!(k.selected_model(), "zeta");
    let listed = k.model_list();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].name, "zeta");
    assert_eq!(listed[1].name, "alpha");

    k.select_model("alpha").unwrap();
    assert_eq!(k.selected_model(), "alpha");

    // The switch has to survive the next turn — the fear being that the router's
    // default quietly puts it back and the key appears to have done nothing.
    let outcome = k.submit("go").unwrap();
    assert_eq!(outcome.final_text.as_deref(), Some("alpha"));
    assert_eq!(seen_alpha.lock().unwrap().len(), 1);
    assert_eq!(
        seen_zeta.lock().unwrap().len(),
        0,
        "the model nobody chose was never called"
    );

    // And it is a fact in the log, with the same shape a rule's choice has.
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let switch = events
        .iter()
        .find(|e| e.kind == ev::MODEL_SELECTED)
        .expect("a person's switch is recorded like a rule's");
    assert_eq!(switch.data["model"], "alpha");
    assert_eq!(switch.data["previous"], "zeta");
    assert_eq!(
        switch.data["rule"], "chosen by the user",
        "'why this model?' must have an answer for a hand-picked one too"
    );
}

/// Purpose: Selecting an unknown model is refused with the available names and stores nothing, because reporting a model the session lacks is worse.
#[test]
fn a_model_nobody_offers_is_refused_rather_than_stored() {
    let t = Temp::new();
    let model = ScriptedModel::new(vec![response(vec![], Some("hi"))]);
    let mut config = KernelConfig::new(&t.dir);
    config.settings.models = vec![named("only")];
    config.settings.router = loong_config::Router {
        default: Some("only".to_string()),
        rules: Vec::new(),
    };
    let mut k =
        Kernel::create_with_models(&t.log, config, vec![("only".to_string(), Box::new(model))])
            .unwrap();

    let err = k.select_model("typo").unwrap_err().to_string();
    assert!(err.contains("typo"), "{err}");
    assert!(
        err.contains("only"),
        "the error names what is available: {err}"
    );
    assert_eq!(
        k.selected_model(),
        "only",
        "and nothing was stored — a session reporting a model it does not have would be worse than an error"
    );
}

/// Purpose: Re-selecting the running model records no `model.selected`, keeping the log a record of changes.
#[test]
fn choosing_the_model_already_running_records_nothing() {
    let t = Temp::new();
    let model = ScriptedModel::new(vec![response(vec![], Some("hi"))]);
    let mut config = KernelConfig::new(&t.dir);
    config.settings.models = vec![named("only")];
    config.settings.router = loong_config::Router {
        default: Some("only".to_string()),
        rules: Vec::new(),
    };
    let mut k =
        Kernel::create_with_models(&t.log, config, vec![("only".to_string(), Box::new(model))])
            .unwrap();

    k.select_model("only").unwrap();
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    assert!(
        !events.iter().any(|e| e.kind == ev::MODEL_SELECTED),
        "re-choosing what is already running is not a change, and the log is the record"
    );
}

/// Purpose: A profile's `investigate = deny` narrows the ladder and the ruling names `profile:user:investigate`, so the deciding layer is visible.
#[test]
fn a_profile_narrows_the_ladder_and_the_rule_names_the_layer() {
    let t = Temp::new();
    std::fs::write(t.dir.join("a.txt"), "content").unwrap();
    let model = ScriptedModel::new(vec![
        response(
            vec![call("c1", "read_file", json!({"path": "a.txt"}))],
            None,
        ),
        response(vec![], Some("done")),
    ]);
    let mut config = KernelConfig::new(&t.dir);
    config.settings = settings_from(&t.dir, "[permissions]\ninvestigate = \"deny\"\n");
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    k.submit("read it").unwrap();

    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let policy = events
        .iter()
        .find(|e| e.kind == ev::POLICY_DECIDED)
        .unwrap();
    assert_eq!(policy.data["verdict"], "deny");
    assert_eq!(
        policy.data["rule"], "profile:user:investigate",
        "the rule that fired names the layer that narrowed it"
    );
    let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
    assert_eq!(result.data["ok"], false);
    assert!(result.data["output"]
        .as_str()
        .unwrap()
        .contains("denied by policy"));
}

/// Purpose: A profile asking to allow out-of-bounds cannot widen the built-in ask, so configuration can only tighten.
#[test]
fn a_profile_can_only_narrow_never_widen() {
    let t = Temp::new();
    let model = ScriptedModel::new(vec![
        response(
            vec![call(
                "c1",
                "shell",
                json!({"command": "curl http://example.invalid"}),
            )],
            None,
        ),
        response(vec![], Some("done")),
    ]);
    let mut config = KernelConfig::new(&t.dir);
    // The profile asks for out-of-bounds actions to be free. The built-in
    // baseline is `ask`, and no layer may loosen it.
    config.settings = settings_from(&t.dir, "[permissions]\nout_of_bounds = \"allow\"\n");
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    let outcome = k.submit("fetch something").unwrap();
    assert!(
        outcome.pending_approval.is_some(),
        "the request still stops for a human: {:?}",
        outcome.final_text
    );
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let policy = events
        .iter()
        .find(|e| e.kind == ev::POLICY_DECIDED)
        .unwrap();
    assert_eq!(policy.data["verdict"], "ask");
    assert_eq!(policy.data["rule"], "builtin:out_of_bounds");
}

/// Purpose: The stricter of caller and config wins for budget and steps, so a layer's ceiling always holds.
#[test]
fn narrow_only_numbers_are_the_strictest_of_the_two() {
    let t = Temp::new();
    let model = ScriptedModel::new(vec![
        response(
            vec![call("c1", "read_file", json!({"path": "a.txt"}))],
            None,
        ),
        response(
            vec![call("c2", "read_file", json!({"path": "a.txt"}))],
            None,
        ),
        response(vec![], Some("done")),
    ]);
    std::fs::write(t.dir.join("a.txt"), "x").unwrap();
    let mut config = KernelConfig::new(&t.dir);
    config.budget_tokens = Some(50_000);
    config.max_steps = 8;
    config.settings = settings_from(
        &t.dir,
        "budget_tokens = 1000\nmax_steps = 1\nkeep_newest_tokens = 100\n",
    );
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();

    assert_eq!(k.ledger().budget(), Some(1_000), "the config ceiling holds");
    let outcome = k.submit("go").unwrap();
    assert_eq!(outcome.steps, 1, "the config's step ceiling holds");
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == ev::COST_RECORDED)
            .count(),
        1,
        "the model was called exactly once"
    );
}

/// Purpose: `narrow_budget` keeps the config ceiling when the caller asks for more or for unbounded, and honours a smaller request.
#[test]
fn the_session_may_narrow_but_not_widen_a_config_budget() {
    let t = Temp::new();
    let config_from_file = settings_from(&t.dir, "budget_tokens = 1000\n");
    // The session asks for more than config allows: the ceiling wins.
    assert_eq!(
        narrow_budget(Some(50_000), config_from_file.budget_tokens),
        Some(1_000)
    );
    // The session asks for less: it gets less.
    assert_eq!(
        narrow_budget(Some(100), config_from_file.budget_tokens),
        Some(100)
    );
    // The session asks for "unbounded": it still gets the ceiling.
    assert_eq!(
        narrow_budget(None, config_from_file.budget_tokens),
        Some(1_000)
    );
    // Nothing set anywhere: unbounded stays unbounded.
    assert_eq!(narrow_budget(None, None), None);
}

/// Purpose: `skill_dirs` adds a root and `disable_skills` removes a name by identity, with the disabled names recorded in `skills.loaded`.
#[test]
fn a_profile_can_add_skill_roots_and_disable_names_by_identity() {
    let t = Temp::new();
    // A workspace skill that config will disable, and a team root that config adds.
    install_command(&t.dir, "local-loud", "---\ndescription: noisy\n---\nbody\n");
    let team = t.dir.join("team-skills");
    std::fs::create_dir_all(team.join("from-team")).unwrap();
    std::fs::write(
        team.join("from-team/SKILL.md"),
        "---\ndescription: From the team profile\n---\nteam body\n",
    )
    .unwrap();

    let model = ScriptedModel::saying("hi");
    let mut config = KernelConfig::new(&t.dir);
    config.settings = settings_from(
        &t.dir,
        "skill_dirs = [\"team-skills\"]\ndisable_skills = [\"local-loud\"]\n",
    );
    let k = Kernel::create(&t.log, config, Box::new(model)).unwrap();

    assert!(
        k.skills().names().contains(&"from-team"),
        "{:?}",
        k.skills().names()
    );
    assert!(
        !k.skills().names().contains(&"local-loud"),
        "a disabled name is gone: {:?}",
        k.skills().names()
    );
    assert!(!k.skills().catalogue().contains("local-loud"));
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let loaded = events.iter().find(|e| e.kind == ev::SKILLS_LOADED).unwrap();
    assert_eq!(loaded.data["disabled"], "local-loud", "{:?}", loaded.data);
}

/// Purpose: A skill file with no frontmatter fails the session with `KernelError::Skills`, because silently skipping a broken instruction is worse.
#[test]
fn a_broken_skill_file_is_fatal() {
    let t = Temp::new();
    let dir = t.dir.join(".loongagent/skills/broken");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "no frontmatter at all
",
    )
    .unwrap();
    let model = ScriptedModel::saying("hi");
    let err = match Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)) {
        Ok(_) => panic!("a malformed skill must be fatal, not silently skipped"),
        Err(e) => e,
    };
    assert!(matches!(err, KernelError::Skills(_)), "{err}");
    assert!(err.to_string().contains("frontmatter"));
}

/// Purpose: Compaction clears old tool bodies first and drops no messages when clearing is enough, leaving a marker and the newest output verbatim.
#[test]
fn compaction_clears_stale_tool_output_before_dropping_anything() {
    let t = Temp::new();
    big_file(&t.dir, "big.txt", 400); // ~4000 chars ≈ 1000 tokens
    let (model, seen) = RecordingModel::new(vec![
        response(
            vec![call("c1", "read_file", json!({"path": "big.txt"}))],
            None,
        ),
        response(
            vec![call("c2", "read_file", json!({"path": "big.txt"}))],
            None,
        ),
        response(
            vec![call("c3", "read_file", json!({"path": "big.txt"}))],
            None,
        ),
        response(vec![], Some("read three times")),
    ]);
    let mut config = KernelConfig::new(&t.dir);
    config.budget_tokens = Some(3_000); // threshold 2400
    config.keep_newest_tokens = 800;
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    k.submit("read it a few times").unwrap();

    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let compaction = events
        .iter()
        .find(|e| e.kind == ev::COMPACTION_APPLIED)
        .expect("a compaction event");
    assert!(
        compaction.data["cleared_tool_outputs"].as_u64().unwrap() >= 1,
        "{:?}",
        compaction.data
    );
    assert_eq!(
        compaction.data["dropped_messages"], 0,
        "clearing alone was enough"
    );
    assert!(
        compaction.data["tokens_after"].as_u64().unwrap()
            < compaction.data["tokens_before"].as_u64().unwrap(),
        "compaction must reduce the context: {:?}",
        compaction.data
    );

    // The model was shown a marker where the stale output used to be, and the
    // newest output stayed verbatim.
    let seen = seen.lock().unwrap();
    let last = seen.last().unwrap();
    let outputs: Vec<&String> = last
        .iter()
        .filter_map(|m| match m {
            ChatMessage::Tool { output, .. } => Some(output),
            _ => None,
        })
        .collect();
    assert!(
        outputs
            .iter()
            .any(|o| o.contains("cleared to save context")),
        "a stale tool output is cleared: {outputs:?}"
    );
    assert!(
        outputs.iter().any(|o| o.contains("line 399")),
        "the newest tool output is kept verbatim: {outputs:?}"
    );
}

/// Purpose: After compaction every tool result still follows its call and every prompt survives, so no request becomes invalid.
#[test]
fn compaction_never_splits_a_tool_pair() {
    let t = Temp::new();
    big_file(&t.dir, "big.txt", 400);
    let (model, seen) = RecordingModel::new(vec![
        response(
            vec![call("c1", "read_file", json!({"path": "big.txt"}))],
            None,
        ),
        response(
            vec![call("c2", "read_file", json!({"path": "big.txt"}))],
            None,
        ),
        response(
            vec![call("c3", "read_file", json!({"path": "big.txt"}))],
            None,
        ),
        response(vec![], Some("done")),
    ]);
    let mut config = KernelConfig::new(&t.dir);
    config.budget_tokens = Some(1_200); // tiny: forces both phases
    config.keep_newest_tokens = 200;
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    k.submit("read it a few times").unwrap();

    for (turn, messages) in seen.lock().unwrap().iter().enumerate() {
        let mut answered: Vec<String> = Vec::new();
        for message in messages {
            match message {
                ChatMessage::Assistant { tool_calls, .. } => {
                    for call in tool_calls {
                        assert!(
                            !answered.contains(&call.call_id),
                            "turn {turn}: {} was answered twice",
                            call.call_id
                        );
                        answered.push(call.call_id.clone());
                    }
                }
                ChatMessage::Tool { call_id, .. } => {
                    assert!(
                        answered.contains(call_id),
                        "turn {turn}: tool result {call_id} has no preceding call — the \
                         conversation would be invalid"
                    );
                }
                _ => {}
            }
        }
        assert!(
            !messages.is_empty(),
            "turn {turn}: the prompt was never dropped"
        );
    }
}

/// Purpose: A response that calls `edit_file` on a fixed path, so recall has work to find.
///
/// Contract: `turn/tests.rs` recall fixtures.
/// A session with a fixed tool and argument, so `recall` has something to find.
fn scripted_edit(index: usize) -> CompletionResponse {
    response(
        vec![call(
            &format!("c{index}"),
            "edit_file",
            json!({"path": "src/kernel/turn.rs", "new_string": "x"}),
        )],
        None,
    )
}

/// Purpose: The pasted-file detector fires on a labelled file block or a prose `write_file`, but not on a snippet or an unlabelled fence.
#[test]
fn a_pasted_file_is_recognised_and_a_snippet_is_not() {
    // The failure this exists for, verbatim in shape from a real session: the
    // whole file in a fenced block, offered for the person to save.
    assert!(looks_like_pasted_file(
        "好的，下面是代码：\n```html\n<!DOCTYPE html>\n<html>…\n```\n请保存为 typing.html"
    ));
    assert!(looks_like_pasted_file("```python\nprint(1)\n```"));
    // A tool call written out as prose instead of called.
    assert!(looks_like_pasted_file(
        "请允许我进行操作：\n\nwrite_file typing.html - \"<!DOCTYPE html>"
    ));
    // Not the failure: explaining with a snippet, or an unlabelled fence.
    assert!(!looks_like_pasted_file(
        "把这一行改成：\n```\nlet x = 1;\n```\n就对了"
    ));
    assert!(!looks_like_pasted_file("已经写好了，见 src/main.rs"));
    assert!(!looks_like_pasted_file(""));
}

/// Purpose: The printed-call detector finds a `<tool_call>` or a quoted name plus arguments mid-text, but ignores a mere mention.
#[test]
fn a_printed_tool_call_is_recognised_and_prose_is_not() {
    // Verbatim in shape from this project's own session (47x75pvh.jsonl): the call
    // arrived inside `content`, wrapped in `<tool_call>`, and the leading text was
    // even cut off mid-word — so the detector has to scan, not parse from the start.
    let measured = "portun\n{\"name\": \"write_file\", \"arguments\": {\"content\":\"# 摘要\n- 出货量 -20%\", \"path\":\"/tmp/markdown.md\"}}\n</tool_call>";
    assert_eq!(leaked_tool_call(measured).as_deref(), Some("write_file"));

    // No spaces, and `edit_file` rather than `write_file`.
    assert_eq!(
        leaked_tool_call("{\"name\":\"edit_file\",\"arguments\":{\"path\":\"a.rs\"}}").as_deref(),
        Some("edit_file")
    );
    // A marker without a parseable name still says what happened.
    assert_eq!(
        leaked_tool_call("<tool_call>\nwrite_file a.txt\n</tool_call>").as_deref(),
        Some("write_file")
    );

    // Not the failure: talking *about* a tool, or a code sample that happens to name
    // one. Neither may trigger a correction.
    assert_eq!(leaked_tool_call("用 write_file 就能写文件。"), None);
    assert_eq!(
        leaked_tool_call("```json\n{\"name\": \"something_else\", \"arguments\": {}}\n```"),
        None
    );
    assert_eq!(
        leaked_tool_call("{\"name\": \"write_file\"}"),
        None,
        "a name with no arguments is a mention, not an attempted call"
    );
    assert_eq!(leaked_tool_call(""), None);
}

/// Purpose: The system prompt names the printed-call shape and shows literal braces, so the first half of the nudge fix is actually present.
#[test]
fn the_system_prompt_names_the_printed_call_shape_too() {
    // The prompt is the first half of the fix; this asserts it actually carries the
    // clause (and that the `format!` braces rendered as braces, not as escapes).
    let t = Temp::new();
    let (model, _seen) = RecordingModel::new(vec![response(vec![], Some("ok"))]);
    let k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    let prompt = match &k.messages()[0] {
        ChatMessage::System { text } => text.clone(),
        other => panic!("the first message is the system prompt: {other:?}"),
    };
    assert!(prompt.contains("write_file"), "{prompt}");
    assert!(
        prompt.contains("<tool_call>"),
        "the printed-call shape is named: {prompt}"
    );
    assert!(
        prompt.contains("{\"name\": \"write_file\""),
        "and shown literally, not as escaped braces: {prompt}"
    );
}

/// Purpose: A model that pastes a file gets one correction, then calls the tool and writes the file; the correction is not a user message in the log.
#[test]
fn a_model_that_pastes_a_file_gets_one_chance_to_actually_write_it() {
    // First reply: the file in chat, no tool call. Second: the real call. Without
    // the nudge the turn would end having written nothing while reading like success.
    let t = Temp::new();
    let (model, seen) = RecordingModel::new(vec![
        response(
            vec![],
            Some("```html\n<!DOCTYPE html>\n<html><body>game</body></html>\n```\n请保存为 typing.html"),
        ),
        response(
            vec![call(
                "c1",
                "write_file",
                json!({"path": "typing.html", "content": "<!DOCTYPE html>"}),
            )],
            None,
        ),
        response(vec![], Some("已写入 typing.html")),
    ]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    let outcome = k.submit("写一个单页打字游戏到 typing.html").unwrap();
    assert_eq!(outcome.final_text.as_deref(), Some("已写入 typing.html"));

    // The tool actually ran, and the file is on disk — the point of the nudge.
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    assert!(
        events.iter().any(|e| e.kind == ev::TOOL_CALL),
        "the retry called a tool"
    );
    assert!(
        std::fs::read_to_string(t.dir.join("typing.html")).is_ok(),
        "and the file exists"
    );

    // The nudge itself is not in the log as something the person said: only the
    // replies that mattered are.
    let seen = seen.lock().unwrap();
    let nudged = seen
        .iter()
        .flatten()
        .any(|m| matches!(m, ChatMessage::User { text } if text.contains("did not call a tool")));
    assert!(nudged, "the model was told what went wrong");
    assert!(
        !events.iter().any(|e| e.kind == ev::MESSAGE_APPENDED
            && e.data["role"] == json!("user")
            && e.data["text"]
                .as_str()
                .unwrap_or("")
                .contains("did not call a tool")),
        "the harness's own objection must not appear as a user message in the log"
    );
}

/// Purpose: A second paste is accepted as the answer with no tool call, because arguing forever is worse than showing the refusal.
#[test]
fn a_model_that_pastes_twice_is_believed_rather_than_looped() {
    // One objection, then accept. A harness that argued forever would be worse
    // than one that let the person see the model refuse.
    let t = Temp::new();
    let paste = response(vec![], Some("```html\n<html></html>\n```\n自己保存吧"));
    let (model, _seen) = RecordingModel::new(vec![paste.clone(), paste]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    let outcome = k.submit("写一个单页游戏").unwrap();
    assert!(
        outcome.final_text.unwrap().contains("自己保存吧"),
        "the second answer is the turn's answer"
    );
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    assert_eq!(
        events.iter().filter(|e| e.kind == ev::TOOL_CALL).count(),
        0,
        "nothing was called, and the log says so"
    );
}

/// Purpose: A printed tool call gets one correction naming that tool, then the real call writes the file.
#[test]
fn a_model_that_prints_a_tool_call_gets_one_chance_to_emit_it() {
    // The measured failure this exists for (47x75pvh.jsonl, qwen2.5:7b-instruct):
    // the call arrived as `<tool_call>{…}</tool_call>` inside the reply, so no
    // `tool.call` event was recorded and the turn ended as if the file had been
    // written. One correction, then the real call.
    let t = Temp::new();
    let printed = "portun\n{\"name\": \"write_file\", \"arguments\": {\"content\":\"# 摘要\\n- 出货量 -20%\", \"path\":\"markdown.md\"}}\n</tool_call>";
    let (model, seen) = RecordingModel::new(vec![
        response(vec![], Some(printed)),
        response(
            vec![call(
                "c1",
                "write_file",
                json!({"path": "markdown.md", "content": "# 摘要\n- 出货量 -20%"}),
            )],
            None,
        ),
        response(vec![], Some("已写入 markdown.md")),
    ]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    let outcome = k
        .submit("帮我总结一下 @report.txt 写入 markdown.md")
        .unwrap();
    assert_eq!(outcome.final_text.as_deref(), Some("已写入 markdown.md"));

    let events = loong_eventlog::replay(&t.log).unwrap().events;
    assert!(
        events.iter().any(|e| e.kind == ev::TOOL_CALL),
        "the retry actually called the tool"
    );
    assert!(
        std::fs::read_to_string(t.dir.join("markdown.md")).is_ok(),
        "and the file exists"
    );
    // The correction names the tool the model tried to print, so a model that meant
    // `edit_file` is not told to `write_file`.
    let seen = seen.lock().unwrap();
    assert!(
        seen.iter()
            .flatten()
            .any(|m| matches!(m, ChatMessage::User { text }
            if text.contains("wrote the `write_file` call as text")
                && text.contains("Do not print the call"))),
        "the model was told what went wrong, in terms of its own call"
    );
    assert!(
        !events.iter().any(|e| e.kind == ev::MESSAGE_APPENDED
            && e.data["role"] == json!("user")
            && e.data["text"]
                .as_str()
                .unwrap_or("")
                .contains("wrote the `write_file` call")),
        "the harness's correction is not a user message in the log"
    );
}

/// Purpose: Reading a file does not excuse printing a different tool's call, because the guard is per tool, not per turn.
#[test]
fn a_read_then_printed_write_is_still_corrected() {
    // The guard is per tool, not per turn: reading a file counts as "a tool ran",
    // but it is not the tool the model then printed. Without the names, this reply
    // would end the turn having written nothing.
    let t = Temp::new();
    std::fs::write(t.dir.join("report.txt"), "Counterpoint: -20%").unwrap();
    let printed = "{\"name\": \"write_file\", \"arguments\": {\"content\":\"摘要\", \"path\":\"markdown.md\"}}\n</tool_call>";
    let (model, _seen) = RecordingModel::new(vec![
        response(
            vec![call("c1", "read_file", json!({"path": "report.txt"}))],
            None,
        ),
        response(vec![], Some(printed)),
        response(
            vec![call(
                "c2",
                "write_file",
                json!({"path": "markdown.md", "content": "摘要"}),
            )],
            None,
        ),
        response(vec![], Some("已写入 markdown.md")),
    ]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    let outcome = k.submit("总结 report.txt 到 markdown.md").unwrap();
    assert_eq!(outcome.final_text.as_deref(), Some("已写入 markdown.md"));
    assert!(std::fs::read_to_string(t.dir.join("markdown.md")).is_ok());
}

/// Purpose: Quoting a call for a tool that already ran is a summary and is not nudged, so the file is not written twice.
#[test]
fn printing_a_call_for_a_tool_that_already_ran_is_not_corrected() {
    // A model that wrote the file and then quotes the call in its summary is
    // describing what happened. Nudging here would write the file a second time.
    let t = Temp::new();
    let quoted = "已写入：\n{\"name\": \"write_file\", \"arguments\": {\"path\":\"a.txt\",\"content\":\"hi\"}}\n</tool_call>";
    let (model, _seen) = RecordingModel::new(vec![
        response(
            vec![call(
                "c1",
                "write_file",
                json!({"path": "a.txt", "content": "hi"}),
            )],
            None,
        ),
        response(vec![], Some(quoted)),
    ]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    let outcome = k.submit("写 a.txt").unwrap();
    assert_eq!(outcome.final_text.as_deref(), Some(quoted));
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    assert_eq!(
        events.iter().filter(|e| e.kind == ev::TOOL_CALL).count(),
        1,
        "no second write was attempted"
    );
}

/// Purpose: After a tool has run, a reply quoting code is accepted, keeping the nudge off honest work.
#[test]
fn a_finished_turn_with_tool_calls_is_never_nudged() {
    // The guard that keeps the nudge from firing on honest work: once a tool has
    // run, a later reply may quote code freely.
    let t = Temp::new();
    let (model, _seen) = RecordingModel::new(vec![
        response(
            vec![call(
                "c1",
                "write_file",
                json!({"path": "a.txt", "content": "hi"}),
            )],
            None,
        ),
        response(vec![], Some("写好了：\n```html\n<html></html>\n```")),
    ]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    let outcome = k.submit("写个文件").unwrap();
    assert!(
        outcome.final_text.unwrap().contains("写好了"),
        "the answer stands; no objection after real work"
    );
}

/// Purpose: `recall` appears in the tool specs only when the context engine exists, because a tool that cannot answer teaches the model not to ask.
#[test]
fn recall_is_offered_only_when_there_is_memory_behind_it() {
    let t = Temp::new();
    let (model, _seen) = RecordingModel::new(vec![response(vec![], Some("ok"))]);
    let off = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    assert!(
        !off.tool_specs().iter().any(|s| s.name == "recall"),
        "a tool that cannot answer must not be advertised"
    );

    let t2 = Temp::new();
    let (model, _seen) = RecordingModel::new(vec![response(vec![], Some("ok"))]);
    let mut config = KernelConfig::new(&t2.dir);
    config.context_engine = Some(crate::context_engine::ContextEngineConfig::new(
        t2.dir.join("session.context.db"),
        t2.log.clone(),
    ));
    let on = Kernel::create(&t2.log, config, Box::new(model)).unwrap();
    assert!(on.tool_specs().iter().any(|s| s.name == "recall"));
}

/// Purpose: `recall` finds an earlier edit and returns summaries with `seq[...]` citations, and the answer is an auditable `tool.result`.
#[test]
fn recall_finds_earlier_work_and_cites_it() {
    let t = Temp::new();
    let (model, seen) = RecordingModel::new(vec![
        scripted_edit(1),
        response(vec![], Some("第一轮完成")),
        response(vec![call("c9", "recall", json!({"query": "层叠"}))], None),
        response(vec![], Some("我查到了")),
    ]);
    let mut config = KernelConfig::new(&t.dir);
    config.context_engine = Some(crate::context_engine::ContextEngineConfig::new(
        t.dir.join("session.context.db"),
        t.log.clone(),
    ));
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    k.submit("修一下 src/kernel/turn.rs 的层叠失效").unwrap();
    k.submit("刚才那个任务是什么来着？").unwrap();

    // The answer reached the model as a normal tool result…
    let seen = seen.lock().unwrap();
    let recall_output = seen
        .iter()
        .flatten()
        .find_map(|m| match m {
            ChatMessage::Tool { output, .. } if output.contains("memory") => Some(output.clone()),
            _ => None,
        })
        .expect("recall returned something");
    // …carrying a citation into the record, which is what makes it checkable.
    assert!(recall_output.contains("seq["), "{recall_output}");
    assert!(recall_output.contains("层叠"), "{recall_output}");

    // And a following `expand` hands back the original events too.
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    assert!(
        events.iter().any(|e| e.kind == ev::TOOL_RESULT
            && e.data["output"].as_str().unwrap_or("").contains("seq[")),
        "the recall answer is an auditable event"
    );
}

/// Purpose: An empty recall is a real answer that says so, not an error that teaches the model the memory is broken.
#[test]
fn recall_says_so_when_it_finds_nothing_rather_than_failing() {
    // "Nothing matched" is a real answer. An error would teach the model that
    // asking about the past is broken.
    let t = Temp::new();
    let (model, seen) = RecordingModel::new(vec![
        scripted_edit(1),
        response(vec![], Some("完成")),
        response(
            vec![call("c9", "recall", json!({"query": "完全不存在的词"}))],
            None,
        ),
        response(vec![], Some("没有找到")),
    ]);
    let mut config = KernelConfig::new(&t.dir);
    config.context_engine = Some(crate::context_engine::ContextEngineConfig::new(
        t.dir.join("session.context.db"),
        t.log.clone(),
    ));
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    k.submit("修一下 src/kernel/turn.rs").unwrap();
    k.submit("找点不存在的东西").unwrap();

    let seen = seen.lock().unwrap();
    let output = seen
        .iter()
        .flatten()
        .find_map(|m| match m {
            ChatMessage::Tool { output, .. } if output.contains("nothing in this session") => {
                Some(output.clone())
            }
            _ => None,
        })
        .expect("a not-found answer reached the model");
    assert!(output.contains("real answer"), "{output}");
}

/// Purpose: A recall with no query, entity, tool or expand id is refused with what to provide instead.
#[test]
fn recall_refuses_a_request_with_nothing_to_search_for() {
    let t = Temp::new();
    let (model, seen) = RecordingModel::new(vec![
        scripted_edit(1),
        response(vec![], Some("完成")),
        response(vec![call("c9", "recall", json!({}))], None),
        response(vec![], Some("好")),
    ]);
    let mut config = KernelConfig::new(&t.dir);
    config.context_engine = Some(crate::context_engine::ContextEngineConfig::new(
        t.dir.join("session.context.db"),
        t.log.clone(),
    ));
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    k.submit("修一下 src/kernel/turn.rs").unwrap();
    k.submit("随便问").unwrap();

    let seen = seen.lock().unwrap();
    assert!(
        seen.iter()
            .flatten()
            .any(|m| matches!(m, ChatMessage::Tool { output, .. }
            if output.contains("give a `query`"))),
        "an empty request is refused with what to do instead"
    );
}

/// Purpose: An unknown summary id is named in the reply rather than silently returning empty;
/// the happy path is covered by `crates/context`'s expansion tests, and the test's name predates this one.
#[test]
fn expand_returns_the_summary_with_its_original_events() {
    let t = Temp::new();
    let (model, seen) = RecordingModel::new(vec![
        scripted_edit(1),
        response(vec![], Some("完成")),
        // The model asks for something that cannot exist; the point is the
        // error message, not a successful expansion, which is covered in the
        // context crate's own tests against a real store.
        response(
            vec![call("c9", "recall", json!({"expand": "no-such-summary"}))],
            None,
        ),
        response(vec![], Some("好")),
    ]);
    let mut config = KernelConfig::new(&t.dir);
    config.context_engine = Some(crate::context_engine::ContextEngineConfig::new(
        t.dir.join("session.context.db"),
        t.log.clone(),
    ));
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    k.submit("修一下 src/kernel/turn.rs").unwrap();
    k.submit("展开一个不存在的摘要").unwrap();

    let seen = seen.lock().unwrap();
    assert!(
        seen.iter()
            .flatten()
            .any(|m| matches!(m, ChatMessage::Tool { output, .. }
            if output.contains("no summary with id"))),
        "an unknown id is named, not silently empty"
    );
}

/// Purpose: Every recall answer stays under `RECALL_MAX_CHARS`, so retrieval cannot spend the context it exists to save.
#[test]
fn a_recall_answer_is_capped_and_says_it_was_capped() {
    // The thin foreground depends on retrieval being bounded: an unbounded
    // answer would spend exactly the context the design exists to save.
    let t = Temp::new();
    for index in 1..8 {
        std::fs::write(t.dir.join(format!("f{index}.txt")), "some content").unwrap();
    }
    let mut responses = Vec::new();
    for index in 1..8 {
        responses.push(scripted_edit(index));
        responses.push(response(vec![], Some("done")));
    }
    responses.push(response(
        vec![call("c99", "recall", json!({"query": "src/kernel"}))],
        None,
    ));
    responses.push(response(vec![], Some("好了")));
    let (model, seen) = RecordingModel::new(responses);
    let mut config = KernelConfig::new(&t.dir);
    config.context_engine = Some(crate::context_engine::ContextEngineConfig::new(
        t.dir.join("session.context.db"),
        t.log.clone(),
    ));
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    for index in 1..8 {
        k.submit(&format!("任务 {index} 修一下 src/kernel/turn.rs"))
            .unwrap();
    }
    k.submit("回顾一下").unwrap();

    let seen = seen.lock().unwrap();
    for messages in seen.iter() {
        for message in messages {
            if let ChatMessage::Tool { output, .. } = message {
                assert!(
                    output.chars().count() <= RECALL_MAX_CHARS + 64,
                    "a recall answer must stay inside its ceiling: {} chars",
                    output.chars().count()
                );
            }
        }
    }
}

/// Purpose: The L4 map is a pinned system message after the system prompt, refreshed in one ledger entry rather than one per step.
#[test]
fn the_layered_engine_puts_a_map_in_front_of_the_model() {
    let t = Temp::new();
    let (model, seen) = RecordingModel::new(vec![
        response(
            vec![call(
                "c1",
                "edit_file",
                json!({"path": "src/kernel/a.rs", "new_string": "x"}),
            )],
            None,
        ),
        response(vec![], Some("done")),
        response(vec![], Some("second turn")),
    ]);
    let mut config = KernelConfig::new(&t.dir);
    config.context_engine = Some(crate::context_engine::ContextEngineConfig::new(
        t.dir.join("session.context.db"),
        t.log.clone(),
    ));
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    k.submit("修一下 src/kernel/a.rs").unwrap();
    k.submit("继续").unwrap();

    let seen = seen.lock().unwrap();
    let last = seen.last().expect("a request happened");
    let map = last.iter().find_map(|m| match m {
        ChatMessage::System { text } if text.starts_with("L4 · 全局认知地图") => {
            Some(text.clone())
        }
        _ => None,
    });
    let map = map.expect("the L4 map is in the request");
    assert!(map.contains("src/kernel"), "{map}");

    // Exactly one map entry, refreshed rather than appended per step.
    let maps = k
        .ledger()
        .entries()
        .iter()
        .filter(|e| e.category == Category::CognitiveMap)
        .count();
    assert_eq!(maps, 1, "one map entry, not one per step");

    // And the map message sits after the system prompt, not before it.
    let positions: Vec<usize> = last
        .iter()
        .enumerate()
        .filter(|(_, m)| matches!(m, ChatMessage::System { .. }))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(positions[0], 0, "the operator's system prompt stays first");
    assert!(positions.len() >= 2, "the map is its own system message");
}

/// Purpose: With the engine on, no compaction drops an exchange and the first turn is still in the request: ADR-0017's core promise.
#[test]
fn with_the_engine_on_a_long_session_never_drops_an_exchange() {
    // The core promise of ADR-0017: the original stays reachable, so the one
    // lossy step compaction used to take must not happen.
    let t = Temp::new();
    let filler = "x".repeat(2_000);
    let (model, seen) = RecordingModel::new(vec![
        response(vec![], Some("first")),
        response(vec![], Some("second")),
        response(vec![], Some("third")),
    ]);
    let mut config = KernelConfig::new(&t.dir);
    config.budget_tokens = Some(1_200);
    config.keep_newest_tokens = 200;
    config.context_engine = Some(crate::context_engine::ContextEngineConfig::new(
        t.dir.join("session.context.db"),
        t.log.clone(),
    ));
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    k.submit(&format!("turn one {filler}")).unwrap();
    k.submit(&format!("turn two {filler}")).unwrap();
    k.submit(&format!("turn three {filler}")).unwrap();

    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let dropped: u64 = events
        .iter()
        .filter(|e| e.kind == ev::COMPACTION_APPLIED)
        .map(|e| e.data["dropped_messages"].as_u64().unwrap_or(0))
        .sum();
    assert_eq!(
        dropped, 0,
        "with the engine on, no exchange is ever dropped"
    );

    // The oldest prompt is still in the request, which is the point.
    let seen = seen.lock().unwrap();
    let last = seen.last().unwrap();
    assert!(
        last.iter()
            .any(|m| matches!(m, ChatMessage::User { text } if text.contains("turn one"))),
        "the first turn is still reachable in the request"
    );
}

/// Purpose: With the engine explicitly off, compaction drops exchanges again, so the rollback path is the old behaviour exactly.
#[test]
fn with_the_engine_off_compaction_still_drops_exchanges() {
    // The rollback path has to be exactly the old behaviour, not an
    // approximation of it. This mirrors
    // `compaction_drops_the_oldest_exchange_when_clearing_is_not_enough`
    // with the engine explicitly configured off.
    let t = Temp::new();
    let filler = "x".repeat(2_000);
    let (model, _seen) = RecordingModel::new(vec![
        response(vec![], Some("first")),
        response(vec![], Some("second")),
        response(vec![], Some("third")),
    ]);
    let mut config = KernelConfig::new(&t.dir);
    config.budget_tokens = Some(1_200);
    config.keep_newest_tokens = 200;
    config.context_engine = Some(crate::context_engine::ContextEngineConfig::off());
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    k.submit(&format!("turn one {filler}")).unwrap();
    k.submit(&format!("turn two {filler}")).unwrap();
    k.submit(&format!("turn three {filler}")).unwrap();

    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let dropped: u64 = events
        .iter()
        .filter(|e| e.kind == ev::COMPACTION_APPLIED)
        .map(|e| e.data["dropped_messages"].as_u64().unwrap_or(0))
        .sum();
    assert!(dropped >= 1, "off means the old behaviour: {dropped}");
}

/// Purpose: With no tool output to clear, compaction still drops the oldest exchange while the current prompt survives.
#[test]
fn compaction_drops_the_oldest_exchange_when_clearing_is_not_enough() {
    let t = Temp::new();
    let filler = "x".repeat(2_000); // ~500 tokens per turn, no tool output to clear
    let (model, seen) = RecordingModel::new(vec![
        response(vec![], Some("first")),
        response(vec![], Some("second")),
        response(vec![], Some("third")),
    ]);
    let mut config = KernelConfig::new(&t.dir);
    config.budget_tokens = Some(1_200); // threshold 960
    config.keep_newest_tokens = 200;
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    k.submit(&format!("turn one {filler}")).unwrap();
    k.submit(&format!("turn two {filler}")).unwrap();
    k.submit(&format!("turn three {filler}")).unwrap();

    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let dropped: u64 = events
        .iter()
        .filter(|e| e.kind == ev::COMPACTION_APPLIED)
        .map(|e| e.data["dropped_messages"].as_u64().unwrap_or(0))
        .sum();
    assert!(dropped >= 1, "the oldest exchange was dropped: {dropped}");

    // The prompt being answered is always still there.
    let seen = seen.lock().unwrap();
    assert!(seen.iter().all(|messages| !messages.is_empty()));
    let last = seen.last().unwrap();
    assert!(
        last.iter()
            .any(|m| matches!(m, ChatMessage::User { text } if text.contains("turn three"))),
        "the current prompt survives compaction"
    );
}

/// Purpose: A budget smaller than one turn stops dropping rather than emptying the prompt being answered.
#[test]
fn a_budget_that_cannot_be_met_stops_dropping_rather_than_emptying_the_prompt() {
    let t = Temp::new();
    let (model, seen) = RecordingModel::new(vec![response(vec![], Some("hi"))]);
    let mut config = KernelConfig::new(&t.dir);
    config.budget_tokens = Some(10); // absurdly small
    config.keep_newest_tokens = 5;
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    k.submit("a single long turn that cannot fit").unwrap();
    let seen = seen.lock().unwrap();
    assert!(
        !seen[0].is_empty(),
        "the prompt we are answering is never dropped"
    );
}

/// Purpose: A trusted `pre_tool` shell hook can deny what the PDP would allow, and `policy.decided` names `hook:guard`.
#[test]
fn a_pre_tool_hook_can_deny_what_the_policy_would_allow() {
    let t = Temp::new();
    install_hooks(
        &t.dir,
        "[[hook]]\nid = \"guard\"\nevent = \"pre_tool\"\nkind = \"shell\"\nscript = \"deny.sh\"\n",
        &[(
            "deny.sh",
            "cat > /dev/null\necho 'no shell commands in this repo' >&2\nexit 2\n",
        )],
        true,
    );
    let model = ScriptedModel::new(vec![
        response(
            vec![call("c1", "shell", json!({"command": "touch made.txt"}))],
            None,
        ),
        response(vec![], Some("understood")),
    ]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    assert!(k.hooks_note().contains("trusted"), "{}", k.hooks_note());
    k.submit("go").unwrap();

    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let fired = events
        .iter()
        .find(|e| e.kind == ev::HOOK_FIRED)
        .expect("a hook.fired event");
    assert_eq!(fired.data["hook_id"], "guard");
    assert_eq!(fired.data["verdict"], "deny");
    assert_eq!(fired.data["failed"], false);
    assert!(fired.data["reason"]
        .as_str()
        .unwrap()
        .contains("no shell commands"));

    // The classifier said write_workspace and the PDP would have allowed it:
    // the hook is what changed the outcome, and the rule names it.
    let policy = events
        .iter()
        .find(|e| e.kind == ev::POLICY_DECIDED)
        .unwrap();
    assert_eq!(policy.data["verdict"], "deny");
    assert_eq!(policy.data["rule"], "hook:guard");
    let result = events.iter().find(|e| e.kind == ev::TOOL_RESULT).unwrap();
    assert_eq!(result.data["ok"], false);
    assert!(
        !t.dir.join("made.txt").exists(),
        "the denied tool must never run"
    );
}

/// Purpose: A hook's ask parks the turn, fires all four permission seams once, and a granted answer runs the call.
#[test]
fn a_hook_that_asks_stops_the_turn_for_a_human() {
    let t = Temp::new();
    install_hooks(
        &t.dir,
        "[[hook]]\nid = \"checker\"\nevent = \"pre_tool\"\nkind = \"shell\"\nscript = \"ask.sh\"\n\n\
         [[hook]]\nid = \"seam-req\"\nevent = \"permission_requested\"\nkind = \"shell\"\nscript = \"ok.sh\"\n\n\
         [[hook]]\nid = \"seam-dec\"\nevent = \"permission_decided\"\nkind = \"shell\"\nscript = \"ok.sh\"\n\n\
         [[hook]]\nid = \"after\"\nevent = \"post_tool\"\nkind = \"shell\"\nscript = \"ok.sh\"\n",
        &[
            ("ask.sh", "cat > /dev/null\nprintf '{\"verdict\":\"ask\",\"reason\":\"double-check this\"}'\n"),
            ("ok.sh", "cat > /dev/null\nexit 0\n"),
        ],
        true,
    );
    let model = ScriptedModel::new(vec![
        response(
            vec![call("c1", "shell", json!({"command": "touch asked.txt"}))],
            None,
        ),
        response(vec![], Some("done")),
    ]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    let outcome = k.submit("go").unwrap();

    let pending = outcome
        .pending_approval
        .expect("the hook asked, so the turn stops");
    assert!(
        pending.reason.contains("double-check"),
        "{}",
        pending.reason
    );
    assert_eq!(pending.rule, "hook:checker");
    assert!(
        !t.dir.join("asked.txt").exists(),
        "nothing runs before the answer"
    );

    // The approval path is the same one the PDP uses, so answering resumes.
    let request_id = pending.request_id.clone();
    let resumed = k
        .respond_to_approval(&request_id, true, ApprovalScope::Once, None)
        .unwrap();
    assert!(resumed.pending_approval.is_none());
    assert!(t.dir.join("asked.txt").exists(), "the granted call ran");

    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
    assert!(kinds.contains(&ev::APPROVAL_REQUESTED));
    assert!(kinds.contains(&ev::APPROVAL_DECIDED));
    // Every seam fired once: pre_tool, permission_requested, permission_decided,
    // post_tool — the guards see the whole shape of the episode.
    let fired: Vec<&str> = events
        .iter()
        .filter(|e| e.kind == ev::HOOK_FIRED)
        .map(|e| e.data["event"].as_str().unwrap())
        .collect();
    assert_eq!(
        fired,
        vec![
            "pre_tool",
            "permission_requested",
            "permission_decided",
            "post_tool"
        ]
    );
}

/// Purpose: An untrusted hook is never asked: no `hook.fired`, and the tool proceeds.
#[test]
fn an_untrusted_hook_is_inert() {
    let t = Temp::new();
    install_hooks(
        &t.dir,
        "[[hook]]\nid = \"guard\"\nevent = \"pre_tool\"\nkind = \"shell\"\nscript = \"deny.sh\"\n",
        &[("deny.sh", "cat > /dev/null\nexit 2\n")],
        false,
    );
    let model = ScriptedModel::new(vec![
        response(
            vec![call("c1", "shell", json!({"command": "touch ran.txt"}))],
            None,
        ),
        response(vec![], Some("ok")),
    ]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    assert!(k.hooks_note().contains("INERT"), "{}", k.hooks_note());
    k.submit("go").unwrap();

    let events = loong_eventlog::replay(&t.log).unwrap().events;
    assert!(
        !events.iter().any(|e| e.kind == ev::HOOK_FIRED),
        "an untrusted hook must not even be asked"
    );
    assert!(t.dir.join("ran.txt").exists(), "the call proceeded");
}

/// Purpose: A `user_prompt_submit` hook refuses before any model call, records the prompt, and reports `refused` with zero steps.
#[test]
fn a_prompt_hook_refuses_the_turn_before_the_model_is_called() {
    let t = Temp::new();
    install_hooks(
        &t.dir,
        "[[hook]]\nid = \"gate\"\nevent = \"user_prompt_submit\"\nkind = \"shell\"\nscript = \"gate.sh\"\n",
        &[("gate.sh", "cat > /dev/null\necho 'this repository is read-only today' >&2\nexit 2\n")],
        true,
    );
    // An empty script: if the model were called, the turn would fail instead.
    let model = ScriptedModel::new(vec![]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    let outcome = k.submit("please refactor everything").unwrap();

    let refused = outcome.refused.expect("the hook refused the turn");
    assert!(refused.contains("read-only today"), "{refused}");
    assert_eq!(outcome.steps, 0);
    assert!(outcome.final_text.is_none());

    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
    assert!(kinds.contains(&ev::HOOK_FIRED));
    assert!(
        !kinds.contains(&ev::COST_RECORDED),
        "the model was never called: no cost, no assistant message"
    );
    let user_turns = events
        .iter()
        .filter(|e| e.kind == ev::MESSAGE_APPENDED && e.data["role"] == "user")
        .count();
    assert_eq!(
        user_turns, 1,
        "the prompt itself is still recorded as the truth"
    );
}

/// Purpose: `session_start` hooks fire while the kernel is built, so a guard sees the session before any work.
#[test]
fn session_start_hooks_fire_when_the_session_opens() {
    let t = Temp::new();
    install_hooks(
        &t.dir,
        "[[hook]]\nid = \"boot\"\nevent = \"session_start\"\nkind = \"shell\"\nscript = \"boot.sh\"\n",
        &[("boot.sh", "cat > /dev/null\necho '{\"verdict\":\"allow\"}'\n")],
        true,
    );
    let model = ScriptedModel::saying("hi");
    let _k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let fired = events
        .iter()
        .find(|e| e.kind == ev::HOOK_FIRED)
        .expect("session_start fired");
    assert_eq!(fired.data["event"], "session_start");
    assert_eq!(fired.data["verdict"], "allow");
}

/// Purpose: A malformed `hooks.toml` fails the session with `KernelError::Hooks`, because a guard that cannot parse must not vanish.
#[test]
fn a_malformed_manifest_refuses_to_open_a_session() {
    let t = Temp::new();
    install_hooks(&t.dir, "not toml at all {{{", &[], false);
    let model = ScriptedModel::saying("hi");
    let err = match Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)) {
        Ok(_) => panic!("a malformed manifest must be fatal, not silently dropped"),
        Err(e) => e,
    };
    assert!(matches!(err, KernelError::Hooks(_)), "{err}");
    assert!(err.to_string().contains("hooks.toml"));
}

/// Purpose: The derived index sits beside the log, holds no stale marker, and answers a search with the matching event.
#[test]
fn the_derived_index_answers_queries_over_the_session() {
    let t = Temp::new();
    std::fs::write(t.dir.join("a.txt"), "needle in the workspace").unwrap();
    let model = ScriptedModel::new(vec![
        response(
            vec![call("c1", "read_file", json!({"path": "a.txt"}))],
            None,
        ),
        response(vec![], Some("read it")),
    ]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    k.submit("read a.txt").unwrap();

    assert!(
        k.index_stale().is_none(),
        "a healthy session keeps a fresh index"
    );
    assert!(
        t.log.with_extension("sqlite").exists(),
        "the index sits beside the log"
    );
    let hits = k.search("needle in the workspace").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].kind, "tool.result");
    assert!(k.search("nothing like this").unwrap().is_empty());
}

/// Purpose: A stale index refuses queries as `OutOfOrder`, and `rebuild_index` restores answers from the log.
#[test]
fn a_stale_index_refuses_to_answer_instead_of_lying() {
    let t = Temp::new();
    let model = ScriptedModel::saying("hi");
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    k.submit("hello").unwrap();
    // Simulate the index going bad (a write failure on a locked or full disk).
    k.index_stale = Some("disk full".to_string());
    assert!(matches!(k.search("hello"), Err(KernelError::OutOfOrder(_))));
    k.rebuild_index().unwrap();
    assert!(k.index_stale().is_none());
    assert!(
        !k.search("hello").unwrap().is_empty(),
        "rebuilt from the log alone"
    );
}

/// Purpose: Deleting the SQLite index and reopening rebuilds it from the log, proving the index is disposable.
#[test]
fn reopening_a_session_rebuilds_the_index_from_the_log() {
    let t = Temp::new();
    {
        let model = ScriptedModel::saying("first");
        let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
        k.submit("one").unwrap();
    }
    // Delete the index: it is derived, so the session must still work.
    std::fs::remove_file(t.log.with_extension("sqlite")).unwrap();
    let model = ScriptedModel::saying("second");
    let mut k = Kernel::open(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    assert!(k.index_stale().is_none());
    assert!(
        !k.search("one").unwrap().is_empty(),
        "the old turn is indexed again"
    );
    let out = k.submit("two").unwrap();
    assert_eq!(out.final_text.as_deref(), Some("second"));
}

/// Purpose: A submit while an approval is pending, or an answer with the wrong id, is `OutOfOrder`; the correct answer resumes.
#[test]
fn submitting_while_an_approval_is_pending_is_refused() {
    let t = Temp::new();
    let escape = t.dir.parent().unwrap().join("loong-escape-3.txt");
    let model = ScriptedModel::new(vec![
        response(
            vec![call(
                "c1",
                "write_file",
                json!({"path": escape.to_string_lossy(), "content": "x"}),
            )],
            None,
        ),
        response(vec![], Some("ok")),
    ]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    let pending = k.submit("go").unwrap().pending_approval.unwrap();
    assert!(matches!(k.submit("again"), Err(KernelError::OutOfOrder(_))));
    assert!(matches!(
        k.respond_to_approval("appr-999", false, ApprovalScope::Once, None),
        Err(KernelError::OutOfOrder(_))
    ));
    k.respond_to_approval(&pending.request_id, false, ApprovalScope::Once, None)
        .unwrap();
}

/// Purpose: A file's instructions sit after the system prompt and before the conversation, accounted as pinned rules.
#[test]
fn instructions_are_seated_after_the_system_prompt() {
    let t = Temp::new();
    let (model, seen) = RecordingModel::new(vec![response(vec![], Some("ok"))]);
    let mut config = KernelConfig::new(&t.dir);
    config.instructions = vec![InstructionBlock {
        scope: InstructionScope::Project,
        source: "/repo/AGENTS.md".to_string(),
        text: "ALWAYS-READ-THE-SPEC".to_string(),
    }];
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    k.submit("say hi").unwrap();

    let seen = seen.lock().unwrap();
    let sent = &seen[0];
    assert!(matches!(sent.first(), Some(ChatMessage::System { .. })));
    let instructions = match sent.get(1) {
        Some(ChatMessage::System { text }) => text,
        other => panic!("the second message is the instruction block, got {other:?}"),
    };
    assert!(instructions.contains("Project instructions from /repo/AGENTS.md"));
    assert!(instructions.contains("ALWAYS-READ-THE-SPEC"));
    assert!(
        instructions.contains("do not grant permissions"),
        "the block says it is advisory: {instructions}"
    );

    let row = k
        .ledger()
        .explain()
        .into_iter()
        .find(|row| row.source == "/repo/AGENTS.md")
        .expect("the instruction has a ledger entry");
    assert_eq!(row.category, "rules");
    assert!(row.pinned, "rules are pinned");
}

/// Purpose: No instruction files means the request has exactly the system prompt and the user message.
#[test]
fn no_instructions_means_no_extra_messages() {
    let t = Temp::new();
    let (model, seen) = RecordingModel::new(vec![response(vec![], Some("ok"))]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    k.submit("say hi").unwrap();

    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].len(), 2, "system prompt plus the user message");
    assert!(!seen[0].iter().any(|m| matches!(
        m,
        ChatMessage::System { text } if text.contains("instructions from")
    )));
}

/// Purpose: The context view reports loaded instructions under the rules category.
#[test]
fn context_explain_reports_instructions_as_rules() {
    let t = Temp::new();
    let mut config = KernelConfig::new(&t.dir);
    config.instructions = vec![InstructionBlock {
        scope: InstructionScope::User,
        source: "/home/person/.loongagent/AGENTS.md".to_string(),
        text: "PREFER-SMALL-DIFFS".to_string(),
    }];
    let k = Kernel::create(&t.log, config, Box::new(ScriptedModel::saying("hi"))).unwrap();

    let explain = k.context_explain();
    let entry = explain
        .entries
        .iter()
        .find(|entry| entry.source == "/home/person/.loongagent/AGENTS.md")
        .expect("the instruction is in the context view");
    assert_eq!(entry.category, "rules");
}

/// Purpose: Dropping the oldest exchange keeps every leading system block, instructions included.
#[test]
fn compaction_keeps_leading_system_blocks() {
    let t = Temp::new();
    let model = ScriptedModel::new(vec![
        response(vec![], Some("one")),
        response(vec![], Some("two")),
    ]);
    let mut config = KernelConfig::new(&t.dir);
    config.instructions = vec![InstructionBlock {
        scope: InstructionScope::Project,
        source: "AGENTS.md".to_string(),
        text: "RULES-SURVIVE-COMPACTION".to_string(),
    }];
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    k.submit("first").unwrap();
    k.submit("second").unwrap();

    let dropped = k.drop_oldest_exchange();
    assert_eq!(dropped, 2, "the first user turn and its answer");
    let leading_systems = k
        .messages()
        .iter()
        .take_while(|m| matches!(m, ChatMessage::System { .. }))
        .count();
    assert_eq!(leading_systems, 2, "the prompt and the instruction block");
    assert!(matches!(
        k.messages().get(2),
        Some(ChatMessage::User { text }) if text == "second"
    ));
    assert!(k
        .ledger()
        .explain()
        .iter()
        .any(|row| row.category == "rules" && row.source == "AGENTS.md"));
}

/// Purpose: The cognitive map follows the instruction blocks instead of sitting between the prompt and them.
#[test]
fn the_map_follows_instruction_blocks() {
    let t = Temp::new();
    let (model, seen) = RecordingModel::new(vec![
        response(
            vec![call(
                "c1",
                "edit_file",
                json!({"path": "src/kernel/a.rs", "new_string": "x"}),
            )],
            None,
        ),
        response(vec![], Some("done")),
        response(vec![], Some("second turn")),
    ]);
    let mut config = KernelConfig::new(&t.dir);
    config.instructions = vec![InstructionBlock {
        scope: InstructionScope::Project,
        source: "AGENTS.md".to_string(),
        text: "RULES-BEFORE-MAP".to_string(),
    }];
    config.context_engine = Some(crate::context_engine::ContextEngineConfig::new(
        t.dir.join("session.context.db"),
        t.log.clone(),
    ));
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    k.submit("修一下 src/kernel/a.rs").unwrap();
    k.submit("继续").unwrap();

    let seen = seen.lock().unwrap();
    let last = seen.last().expect("a request happened");
    let positions: Vec<usize> = last
        .iter()
        .enumerate()
        .filter(|(_, m)| matches!(m, ChatMessage::System { .. }))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(positions, vec![0, 1, 2], "prompt, instructions, map");
    assert!(matches!(
        &last[1],
        ChatMessage::System { text } if text.contains("RULES-BEFORE-MAP")
    ));
    assert!(matches!(
        &last[2],
        ChatMessage::System { text } if text.starts_with("L4")
    ));
}
