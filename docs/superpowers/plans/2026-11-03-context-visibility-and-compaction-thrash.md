# Context visibility and compaction thrash Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Put the last request's share of the context budget in the fixed usage row, and stop a turn whose clearing passes fire three times within five steps.

**Architecture:** `Spend` gains `last_request_tokens` and a budget parameter on `usage_line`; `surface::run` asks for the context once at startup. The kernel gains a small per-turn window of compaction steps and a note that ends the turn, next to the existing stall guard.

**Tech Stack:** Rust 2021, `cargo test -p loong-kernel -p loong-tui-app`, ratatui, the existing `ScriptedModel`/`RecordingModel` fixtures.

**Spec:** `docs/superpowers/specs/2026-11-03-context-visibility-and-compaction-thrash-design.md`

## Global Constraints

- **Exact values:** `COMPACTION_THRASH_PASSES = 3`, `COMPACTION_THRASH_STEPS = 4`.
- **No schema change, no new config key.** The usage row reads `ContextExplain.budget_tokens`; the thrash note is an assistant `message.appended`.
- **Contracts:** every new `fn`/field carries its `Purpose` plus `Serves`, `Design`, `Up`, `Down`, `Verify`; touched files update the item lists.
- **Gates:** `cargo test -p loong-kernel -p loong-tui-app` green per task; `./scripts/check.sh` green before the final commit.
- **Language:** code comments in English; commit messages `feat:`/`fix:`/`docs:`.

## File Structure

| File | Responsibility in this change |
|---|---|
| `crates/tui-app/src/spend.rs` | `last_request_tokens`, `usage_line(budget)`, tests |
| `crates/tui-app/src/lib.rs` | pass the budget to `usage_line` |
| `crates/tui-app/src/surface.rs` | one `RefreshContext` at startup |
| `crates/kernel/src/turn.rs` | `compaction_steps`, the thrash constants, field init |
| `crates/kernel/src/turn/loop.rs` | record a pass, stop with the note |
| `crates/kernel/src/turn/tests.rs` | the thrash test |
| `docs/design/cost-accounting.md`, `docs/design/surfaces.md`, `docs/design/kernel-loop.md` | the row text, the mock line, the ending |

---

### Task 1: The context share in the usage row

**Files:** `crates/tui-app/src/spend.rs`, `crates/tui-app/src/lib.rs`, `crates/tui-app/src/surface.rs`

- [ ] **Step 1: Write the failing test**

```rust
/// Purpose: The usage row reports the last request's share of the budget, and plain tokens without one.
#[test]
fn the_usage_line_reports_the_context_share() {
    let mut spend = Spend::default();
    spend.add(CostEntry::from_event(&cost_event("k3", "unknown", Some(700), 0)).unwrap());
    let line = spend.usage_line(Some(100_000)).expect("a row");
    assert!(line.contains("上下文"), "{line}");
    // cost_event uses tokens_in = 1_000, so one percent.
    assert!(line.contains("上下文 1%"), "{line}");
    let line = spend.usage_line(None).expect("a row");
    assert!(line.contains("上下文 1,000"), "{line}");
    assert!(!line.contains('%'), "no made-up percentage: {line}");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p loong-tui-app spend::tests::the_usage_line_reports_the_context_share`
Expected: FAIL — `usage_line` takes no argument.

- [ ] **Step 3: Implement the field, the parameter and the startup refresh**

Add `last_request_tokens` set in `Spend::add`; change the signature to
`usage_line(&self, budget: Option<u64>)`; append the segment described in the
spec §4.1. In `App::draw`, pass
`self.context.as_ref().map(|c| c.budget_tokens).filter(|b| *b > 0)`. In
`surface::run`, after `spawn_worker`, send `ToWorker::RefreshContext` once.

- [ ] **Step 4: Run the suite**

Run: `cargo test -p loong-tui-app`
Expected: PASS; update the existing `the_usage_line_reports_tokens_and_the_cache_rate` call site.

- [ ] **Step 5: Commit**

```bash
git add crates/tui-app/src
git commit -m "feat(tui): show the context share in the usage row"
```

---

### Task 2: Stop a compaction that cannot keep up

**Files:** `crates/kernel/src/turn.rs`, `crates/kernel/src/turn/loop.rs`, `crates/kernel/src/turn/build.rs`, `crates/kernel/src/turn/tests.rs`

- [ ] **Step 1: Write the failing test**

```rust
/// Purpose: Three clearing passes within five steps stop the turn before it re-bills the same clear.
#[test]
fn compaction_thrash_stops_the_turn() {
    // six distinct big files, keep_newest_tokens = 100; the passes land on steps 3, 4, 5.
    // assert the note contains "compaction thrash" and steps == 4.
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p loong-kernel turn::tests::compaction_thrash_stops_the_turn`
Expected: FAIL — the turn runs to the scripted answer.

- [ ] **Step 3: Implement the window and the note**

Add `compaction_steps: VecDeque<usize>` to `Kernel` and clear it in
`begin_turn`. In `run_inner`, after `age_tool_outputs` + `compact_if_needed`
append anything, push the step and, when the window holds three passes spanning
at most four steps, append the note and return. Constants in `turn.rs`.

- [ ] **Step 4: Run the kernel suite and the gates**

Run: `cargo test -p loong-kernel` then `FAST=1 ./scripts/check.sh`.
Expected: PASS; update `README.md`'s `test-counts` line if the suite total changed.

- [ ] **Step 5: Commit**

```bash
git commit -m "fix(kernel): stop a compaction that fires every step"
```

---

### Task 3: The documents

**Files:** `docs/design/cost-accounting.md` §4, `docs/design/surfaces.md` (the mock line), `docs/design/kernel-loop.md` §4 and §6.

- [ ] **Step 1: Update the three documents and keep them inside their budgets.**
- [ ] **Step 2: Run `python3 scripts/check-docs.py` and then `FAST=1 ./scripts/check.sh`.**
- [ ] **Step 3: Commit**

```bash
git commit -m "docs: describe the context row and compaction thrash"
```
