# Turn budget and stall guard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One step budget per turn (approval resumes included), a guard that stops a model repeating itself, and a prompt prefix that survives an approval resume.

**Architecture:** Four changes inside the kernel: `Kernel` carries `steps_taken`, `map_seated` and the stall state; `run_inner` reads them instead of a per-run counter and seats the map once per turn; `record_tool_result` feeds the failure counter; `seat_map` becomes idempotent. The default step budget drops from 100 to 50.

**Tech Stack:** Rust 2021, `cargo test -p loong-kernel`, the `RecordingModel`/`ScriptedModel` fixtures, `loong-eventlog` replay.

**Spec:** `docs/superpowers/specs/2026-11-03-turn-budget-and-stall-guard-design.md`

## Global Constraints

- **Exact values:** `STALL_REPEAT_CALLS = 3`, `STALL_FAILURES = 4`, `STALL_SCRATCH_WRITES = 5`, `DEFAULT_MAX_STEPS = 50`.
- **No schema change, no new config key.** The stall note is an assistant `message.appended`; the step-limit note keeps its shape.
- **Contracts:** every new `fn`/`struct` carries `Purpose:` plus `Serves`, `Design`, `Up`, `Down`, `Verify`; every touched file updates its module contract if a new caller or callee appears.
- **Gates:** `cargo test -p loong-kernel` green per task; `./scripts/check.sh` green before the final commit.
- **Language:** code comments in English; commit messages `fix:`/`feat:`/`test:`.
- **Docs first:** the spec above exists; `docs/design/kernel-loop.md`, `flows/turn-lifecycle.md`, `flows/context-compaction.md` and `context-ledger.md` are updated in Task 5.

## Review Focus

- The approval path: a resumed turn must not reset `steps_taken`, the failure counter, the repeat window or `map_seated`.
- A new turn must reset all of them, including after a `TurnOutcome.refused`.
- The “not run” results after a parked approval must not count as failures.
- `max_steps = 0` still means unlimited, and the arithmetic must not overflow.
- `seat_map` must write nothing when the text is unchanged, while still updating after a clearing pass.
- A turn that makes ordinary progress must emit exactly the messages it emitted before.

## File Structure

| File | Responsibility in this change |
|---|---|
| `crates/kernel/src/turn.rs` | the new `Kernel` fields, the `RecentCall` type, `DEFAULT_MAX_STEPS = 50`, `record_tool_result` feeding the failure counter |
| `crates/kernel/src/turn/loop.rs` | per-turn loop range, stall check, `map_seated` gate, reset in `submit_with` |
| `crates/kernel/src/turn/compaction.rs` | `seat_map` idempotence |
| `crates/kernel/src/turn/build.rs` | initialise the new fields |
| `crates/kernel/src/turn/tests.rs` | the six tests in the spec's verification table |
| `docs/design/kernel-loop.md` | §2, §4, §5 and §6 |
| `docs/design/flows/turn-lifecycle.md` | the step sequence and the ending list |
| `docs/design/flows/context-compaction.md` | `refresh_memory` / `seat_map` wording |
| `docs/design/context-ledger.md` | the writer table names `seat_map` |
| `README.md` | the test count the gate compares |

---

### Task 1: Pin the per-turn budget across an approval resume

**Files:**
- Modify: `crates/kernel/src/turn.rs` (field, reset, `DEFAULT_MAX_STEPS` later in Task 4)
- Modify: `crates/kernel/src/turn/loop.rs` (`submit_with`, `run_inner`)
- Modify: `crates/kernel/src/turn/build.rs` (field initialiser)
- Test: `crates/kernel/src/turn/tests.rs`

**Interfaces:**
- Produces: `Kernel.steps_taken: usize` — completions taken in the current turn.
- Produces: `Kernel::run_inner` loops `steps_taken+1 ..= max_steps`.

- [ ] **Step 1: Write the failing test**

```rust
/// Purpose: One step budget covers a turn across an approval resume, so a parked call cannot double it.
#[test]
fn the_step_budget_survives_an_approval_resume() {
    let t = Temp::new();
    std::fs::write(t.dir.join("a.txt"), "x").unwrap();
    let (model, seen) = RecordingModel::new(vec![
        response(vec![call("c1", "read_file", json!({"path": "/etc/hostname"}))], None),
        response(vec![call("c2", "read_file", json!({"path": "a.txt"}))], None),
        response(vec![call("c3", "read_file", json!({"path": "a.txt"}))], None),
        response(vec![], Some("must not run")),
    ]);
    let mut config = KernelConfig::new(&t.dir);
    config.max_steps = 3;
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    let asked = k.submit("go").unwrap();
    let pending = asked.pending_approval.expect("the first call asks");
    let out = k
        .respond_to_approval(&pending.request_id, true, gen::ApprovalScope::Once, None)
        .unwrap();
    assert_eq!(out.steps, 3, "{out:?}");
    assert!(out.final_text.unwrap().contains("step limit"));
    assert_eq!(seen.lock().unwrap().len(), 3, "the fourth response was never asked for");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p loong-kernel turn::tests::the_step_budget_survives_an_approval_resume`
Expected: FAIL — the resume restarts at 1 and the “must not run” answer ends the turn.

- [ ] **Step 3: Implement the turn counter**

Add `steps_taken: usize` to `Kernel`, initialise it to 0 in `build.rs`, and set it to 0 in `submit_with` after the pending check. In `run_inner` replace the range with:

```rust
let start = self.steps_taken;
let remaining = self.config.max_steps.saturating_sub(start);
for offset in 1..=remaining {
    let step = start + offset;
    self.steps_taken = step;
    // ... existing body, using `step`
}
```

Keep every `TurnOutcome.steps` as `step` / `self.steps_taken`; the step-limit return uses `self.config.max_steps`.

- [ ] **Step 4: Run the test and the kernel suite**

Run: `cargo test -p loong-kernel turn::tests::the_step_budget_survives_an_approval_resume` then `cargo test -p loong-kernel`
Expected: PASS; fix any test whose `steps` changed because it resumed an approval.

- [ ] **Step 5: Commit**

```bash
git add crates/kernel/src/turn.rs crates/kernel/src/turn/loop.rs crates/kernel/src/turn/build.rs crates/kernel/src/turn/tests.rs
git commit -m "fix(kernel): keep one step budget for the whole turn"
```

---

### Task 2: Reset the turn state only on a new turn

**Files:** `crates/kernel/src/turn/loop.rs`, `crates/kernel/src/turn.rs`

**Interfaces:**
- Produces: `Kernel::begin_turn()` — resets `steps_taken`, the stall state and `map_seated`; `submit_with` calls it, `respond_to_approval` does not.

- [ ] **Step 1: Write the failing test**

Extend the Task 1 test: after the step-limit outcome, `k.submit("go")` must start a fresh budget (the model answers on its first call). Add a second assertion with a fifth scripted response.

- [ ] **Step 2: Implement `begin_turn` and call it exactly once per `submit_with`**

The refusal paths in `submit_with` also need the reset: call `begin_turn()` immediately after the pending check, before command expansion.

- [ ] **Step 3: Run the tests, commit**

```bash
git commit -m "fix(kernel): reset turn state only when a new turn starts"
```

---

### Task 3: Stop a model that is visibly spinning

**Files:** `crates/kernel/src/turn.rs` (stall state, `RecentCall`, `record_tool_result`), `crates/kernel/src/turn/loop.rs` (`dispatch` notes calls, `run_inner` checks the guard, `record_skipped_result`), `crates/kernel/src/turn/build.rs` (initialisers)

**Interfaces:**
- Produces: `Kernel::stall_reason(&self) -> Option<String>`, `Kernel::note_tool_call`, `Kernel::note_scratch_write`, `Kernel::note_tool_result`.
- Produces: `Kernel::record_skipped_result` — a `tool.result` for “not run” that does not count as a failure.

- [ ] **Step 1: Write the three failing tests**

```rust
/// Purpose: Three identical calls in a row stop the turn before the budget, naming the repeat.
#[test]
fn a_repeated_call_stops_as_stalled() { /* max_steps = 20, same read_file 10 times */ }

/// Purpose: Five consecutive failed tool results stop the turn, without needing identical calls.
#[test]
fn a_run_of_failed_calls_stops_as_stalled() { /* read_file missing1..6 */ }

/// Purpose: Five distinct throwaway scripts under `.tmp` stop the turn, even when every call differs.
#[test]
fn scratch_script_churn_stops_as_stalled() { /* write_file .tmp/r1.js .. .tmp/r6.js */ }
```

Each asserts `final_text` contains `stall guard`, `steps` is the trigger count, and the recorded request count matches.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p loong-kernel turn::tests::a_repeated_call_stops_as_stalled turn::tests::a_run_of_failed_calls_stops_as_stalled turn::tests::scratch_script_churn_stops_as_stalled`
Expected: FAIL — the turns run to `max_steps` or answer normally.

- [ ] **Step 3: Implement the guard**

Constants in `turn.rs`; `RecentCall { tool: String, args: String }`; a `VecDeque<RecentCall>` capped at three; `consecutive_failures`; a `HashSet<String>` of scratch paths. `note_tool_call` is called from `dispatch` before the tool runs (including `recall`); `note_scratch_write` when the tool is `write_file`; `record_tool_result` calls `note_tool_result(ok)`; the “not run” loop uses `record_skipped_result`. `run_inner` checks `stall_reason()` at the top of every step, after the interrupt check.

- [ ] **Step 4: Run all three tests and the kernel suite**

Run: `cargo test -p loong-kernel`
Expected: PASS. Update `the_default_step_limit_leaves_room_for_a_real_task` to use distinct paths if its twenty identical reads now trip the repeat guard.

- [ ] **Step 5: Commit**

```bash
git commit -m "feat(kernel): stop a turn that is visibly spinning"
```

---

### Task 4: Keep the map out of an approval resume, and lower the default

**Files:** `crates/kernel/src/turn.rs`, `crates/kernel/src/turn/loop.rs`, `crates/kernel/src/turn/compaction.rs`, `crates/kernel/src/turn/build.rs`, `crates/kernel/src/turn/tests.rs`

**Interfaces:**
- Produces: `Kernel.map_seated: bool`, reset by `begin_turn`, set by `run_inner` after the first seating.
- Produces: `DEFAULT_MAX_STEPS = 50`.

- [ ] **Step 1: Write the failing prefix test**

```rust
/// Purpose: An approval resume starts from the bytes the parked request ended with, so the provider cache still matches.
#[test]
fn an_approval_resume_does_not_move_the_request_prefix() {
    // RecordingModel + context engine; first call asks, second answers.
    // assert_eq!(&second[..first.len()], &first[..]);
}
```

- [ ] **Step 2: Write the idempotence test**

```rust
/// Purpose: Re-seating an unchanged map writes nothing, so a new turn can keep the cache too.
#[test]
fn seat_map_is_idempotent() {
    // build a kernel with an engine, call seat_map twice, assert messages and entry tokens unchanged.
}
```

- [ ] **Step 3: Implement the seating gate**

`run_inner`: `if !self.map_seated { self.refresh_memory(); self.seat_map(); self.map_seated = true; }`. Every clearing pass still calls `seat_map()`. `seat_map` compares the existing message text and returns early when equal. Lower `DEFAULT_MAX_STEPS` to 50 and update its doc comment.

- [ ] **Step 4: Run the tests and the kernel suite**

Run: `cargo test -p loong-kernel`

- [ ] **Step 5: Commit**

```bash
git commit -m "fix(kernel): keep the prompt prefix stable across an approval resume"
```

---

### Task 5: The documents and the gate

**Files:**
- `docs/design/kernel-loop.md` §2, §4, §5, §6
- `docs/design/flows/turn-lifecycle.md` §1, §2
- `docs/design/flows/context-compaction.md` §3, §6
- `docs/design/context-ledger.md` the writer names
- `README.md` the `test-counts` line

- [ ] **Step 1: Update the design documents**

State: the budget is per turn and survives an approval resume; the map is seated once per turn and not on a resume; the three stall triggers and the note; the default is 50; the “Stalled” ending. Keep every document inside its line budget.

- [ ] **Step 2: Run the gates**

Run: `./scripts/check.sh`
Expected: all gates pass. Update the README test count from the suite's reported total.

- [ ] **Step 3: Commit**

```bash
git commit -m "docs: describe the turn budget, the stall guard and the stable prefix"
```
