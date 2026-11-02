# Token efficiency Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use subagent-driven-development (recommended) or executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give every session a reachable context ceiling, age stale tool output on every step, and keep the request prefix byte-stable inside a turn, so a long session stops re-billing the whole history.

**Architecture:** Three changes inside the kernel's context path — a budget derived from the model window (`turn/compaction.rs`), a proactive `age_tool_outputs` that reuses `compaction.applied`, and a split of `refresh_map` into per-step memory refresh plus once-per-turn `seat_map` — plus one surface change (`tui-app`'s fixed usage row). The probe in Task 1 decides whether the map split is the cache fix or only a guard.

**Tech Stack:** Rust 2021, `cargo test -p loong-kernel`, `loong-eventlog` replay, the kernel's `RecordingModel` fixture, `loong-tui-app` tests.

**Spec:** `docs/superpowers/specs/2026-10-28-token-efficiency-design.md`

## Global Constraints

- **Values are exact:** `default_budget(window) = clamp(window / 4, 32_000, 100_000)`; aging keep = `keep_newest_tokens` (default `20_000`); compaction triggers at `compact_at_percent = 80`; an explicit `budget_tokens` always wins.
- **No schema change:** reuse `compaction.applied` (`tokens_before`, `tokens_after`, `kept_tokens`, `cleared_tool_outputs`, `dropped_messages`, `summary`). No new event kind, no new field.
- **Contracts:** every new `fn`/`struct` carries `Purpose:` plus `Serves`, `Design`, `Up`, `Down`, `Verify` (AGENTS.md); touched files keep their module contract and add the new items to `Up`/`Down`.
- **Gates:** `cargo test -p loong-kernel -p loong-tui-app` green per task; `./scripts/check.sh` green before the final commit.
- **Dirty tree:** 25 files are already modified in this checkout (including `turn.rs`, `loop.rs`, `config/src/lib.rs`, `tui-app/src/lib.rs`). Do not revert them. Before the first code commit, either the human commits/stashes them, or stage only this plan's hunks with `git add -p` and verify `git diff --cached` contains nothing else.
- **Language:** code comments in English, matching the repository; commit messages in the repository's style (`fix:`/`feat:`/`test:`).
- **No pricing guesses:** do not add per-token prices for k3/qwen; `/cost` keeps "价格未知".

## Review Focus

- A model that declares a 1M window and sets no budget: the derived ceiling must make compaction reachable, not leave it at ~1M.
- A single tool output larger than the keep window: `clear_tool_outputs` keeps the newest entry whatever the numbers say; no divide-by-zero, no empty history.
- A short session under every threshold: no `compaction.applied`, no changed messages, no new TUI line.
- The pinned L4 map and instruction blocks: aging must never clear them, and compaction must keep them verbatim.
- Replay: a log containing aging events must reproduce the same cleared markers as the live run.

## File Structure

| File | Responsibility in this change |
|---|---|
| `crates/kernel/src/turn/compaction.rs` | `default_budget`, `age_tool_outputs`, `refresh_memory`/`seat_map` split |
| `crates/kernel/src/turn/loop.rs` | one-step order: refresh memory → age → compact → seat map → request |
| `crates/kernel/src/ledger.rs` | `tool_output_tokens()` |
| `crates/kernel/src/turn.rs` | `context_explain` reports the effective budget |
| `crates/kernel/src/turn/tests.rs` | every kernel-level test below |
| `crates/tui-app/src/spend.rs` | `usage_line()` |
| `crates/tui-app/src/lib.rs` | draw the fixed usage row under the prompt |
| `docs/design/context-ledger.md` | §3–§4 gain the derived default and the aging rule |
| `docs/05-parity-and-synthesis.md` | §A2 T-25 row updated from the probe result |

---

### Task 1: Prove whether the map is what moves the prefix

**Files:**
- Test: `crates/kernel/src/turn/tests.rs`
- Read first: the engine fixture around `tests.rs:2409` (`ContextEngineConfig::new(...).with_summarizer(...)`) and `RecordingModel` at `tests.rs:1347`.

**Interfaces:**
- Consumes: `Kernel::create`, `Kernel::submit`, `RecordingModel::new`, and `seen: Arc<Mutex<Vec<Vec<ChatMessage>>>>`.
- Produces: the test `the_request_prefix_is_append_only_within_a_turn`; its result decides whether Task 4 is a fix or a guard.

- [ ] **Step 1: Write the failing test**

```rust
/// Purpose: Two model calls in one turn differ only by the exchange appended between them.
/// Serves: the cache premise of `docs/superpowers/specs/2026-10-28-token-efficiency-design.md` §4.3.
#[test]
fn the_request_prefix_is_append_only_within_a_turn() {
    let t = Temp::new();
    big_file(&t.dir, "readme.txt", 40);
    let (model, seen) = RecordingModel::new(vec![
        response(vec![call("c1", "read_file", json!({"path": "readme.txt"}))], None),
        response(vec![], Some("read it")),
    ]);
    let mut config = KernelConfig::new(&t.dir);
    config.context_engine = Some(crate::context_engine::ContextEngineConfig::new(
        t.dir.join("session.context.db"),
        t.log.clone(),
    ));
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    k.submit("read readme.txt").unwrap();

    let seen = seen.lock().unwrap();
    assert!(seen.len() >= 2, "the script calls the model twice");
    let first = &seen[0];
    let second = &seen[1];
    assert_eq!(&second[..first.len()], &first[..], "the prefix moved between steps");
}
```

- [ ] **Step 2: Run it and record the answer**

Run: `cargo test -p loong-kernel turn::tests::the_request_prefix_is_append_only_within_a_turn -- --nocapture`
Expected: PASS (no map yet in the first steps) or FAIL with a differing index inside the map block. **Record which**, plus the differing index, in the commit message of Task 1 and in the implementation report. This is the probe: it names the variable instead of assuming it.

- [ ] **Step 3: Commit the test**

```bash
git add crates/kernel/src/turn/tests.rs
git commit -m "test(kernel): pin the request prefix inside one turn"
```

---

### Task 2: A default budget derived from the window

**Files:**
- Modify: `crates/kernel/src/turn/compaction.rs` (`input_ceiling`, new `default_budget`)
- Modify: `crates/kernel/src/turn.rs` (`context_explain`, line ~89 `budget_tokens`)
- Test: `crates/kernel/src/turn/tests.rs`

**Interfaces:**
- Produces: `pub(super) fn default_budget(window: u64) -> u64` in `turn::compaction`.
- Produces: `Kernel::effective_budget(&self) -> Option<u64>` — the explicit budget when one is set, otherwise `default_budget(window)` for the selected model, `None` when neither exists.
- Produces: `ContextExplain.budget_tokens` reports `effective_budget().unwrap_or(0)`.

- [ ] **Step 1: Write the failing unit test**

```rust
#[test]
fn a_default_budget_is_derived_from_the_window() {
    assert_eq!(super::compaction::default_budget(1_048_576), 100_000);
    assert_eq!(super::compaction::default_budget(200_000), 50_000);
    assert_eq!(super::compaction::default_budget(131_072), 32_768);
    assert_eq!(super::compaction::default_budget(32_768), 32_000);
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p loong-kernel turn::tests::a_default_budget_is_derived_from_the_window`
Expected: FAIL — `default_budget` not found.

- [ ] **Step 3: Implement `default_budget` and use it in `input_ceiling`**

In `compaction.rs`, add `default_budget(window: u64) -> u64` returning `(window / 4).clamp(32_000, 100_000)`, and `effective_budget(&self) -> Option<u64>` returning `self.ledger.budget().or_else(|| self.current_window().map(default_budget))`. `input_ceiling` uses `effective_budget()` for its `from_budget` term and keeps the existing `min` against `from_window`. In `turn.rs`, `context_explain` reports `self.effective_budget().unwrap_or(0)` instead of `self.ledger.budget().unwrap_or(0)`.

- [ ] **Step 4: Run the unit test**

Run: `cargo test -p loong-kernel turn::tests::a_default_budget_is_derived_from_the_window`
Expected: PASS.

- [ ] **Step 5: Write the failing integration test**

```rust
#[test]
fn a_huge_window_still_compacts_at_its_derived_budget() {
    let t = Temp::new();
    big_file(&t.dir, "huge.txt", 4_000); // ≈ 10k tokens
    let (model, _seen) = RecordingModel::with_window(
        vec![
            response(vec![call("c1", "read_file", json!({"path": "huge.txt"}))], None),
            response(vec![call("c2", "read_file", json!({"path": "huge.txt"}))], None),
            response(vec![call("c3", "read_file", json!({"path": "huge.txt"}))], None),
            response(vec![call("c4", "read_file", json!({"path": "huge.txt"}))], None),
            response(vec![], Some("done")),
        ],
        200_000, // derived budget 50k, ceiling 40k
    );
    let mut config = KernelConfig::new(&t.dir);
    config.keep_newest_tokens = 500; // aging, once it exists, cannot mask the ceiling
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    k.submit("read it four times").unwrap();
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    assert!(events.iter().any(|e| e.kind == ev::COMPACTION_APPLIED));
}
```

- [ ] **Step 6: Run it to verify it fails**

Run: `cargo test -p loong-kernel turn::tests::a_huge_window_still_compacts_at_its_derived_budget`
Expected: FAIL — no `COMPACTION_APPLIED`.

- [ ] **Step 7: Confirm the test passes after Step 3's implementation**

Run: `cargo test -p loong-kernel turn::tests::a_huge_window_still_compacts_at_its_derived_budget`
Expected: PASS.

- [ ] **Step 8: Write and run the explicit-wins test**

```rust
#[test]
fn an_explicit_budget_wins_over_the_derived_one() {
    let t = Temp::new();
    let (model, _seen) = RecordingModel::with_window(vec![response(vec![], Some("ok"))], 1_048_576);
    let mut config = KernelConfig::new(&t.dir);
    config.budget_tokens = Some(5_000);
    let k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    assert_eq!(k.context_explain().budget_tokens, 5_000);
}
```

Run: `cargo test -p loong-kernel turn::tests::an_explicit_budget_wins_over_the_derived_one`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/kernel/src/turn/compaction.rs crates/kernel/src/turn.rs crates/kernel/src/turn/tests.rs
git commit -m "feat(kernel): derive a default context budget from the model window"
```

---

### Task 3: Age old tool output on every step

**Files:**
- Modify: `crates/kernel/src/ledger.rs` (new `tool_output_tokens`)
- Modify: `crates/kernel/src/turn/compaction.rs` (new `age_tool_outputs`)
- Modify: `crates/kernel/src/turn/loop.rs` (call before `compact_if_needed`)
- Test: `crates/kernel/src/ledger.rs` (`mod tests`), `crates/kernel/src/turn/tests.rs`

**Interfaces:**
- Produces: `Ledger::tool_output_tokens(&self) -> u64` summing `category == History`, `source.starts_with("tool:")`, `!cleared`.
- Produces: `Kernel::age_tool_outputs(&mut self, events: &mut Vec<u64>) -> Result<(), KernelError>`.

- [ ] **Step 1: Write the failing ledger test**

```rust
#[test]
fn tool_output_tokens_counts_only_uncleared_tool_output() {
    let mut l = Ledger::default();
    l.add(Category::History, "tool:shell", "x".repeat(4000).as_str(), Lifetime::Turn, false);
    l.add(Category::SystemPrompt, "system", "y".repeat(4000).as_str(), Lifetime::Persistent, true);
    assert_eq!(l.tool_output_tokens(), 1000);
    l.clear_tool_outputs(0, |e| format!("[cleared {}]", e.source));
    assert_eq!(l.tool_output_tokens(), 0);
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p loong-kernel ledger::tests::tool_output_tokens_counts_only_uncleared_tool_output`
Expected: FAIL — method not found.

- [ ] **Step 3: Implement `tool_output_tokens`**

Add the method next to `tokens_by_category`, with the standard doc block. It sums `entry.tokens` for the three conditions above.

- [ ] **Step 4: Write the failing kernel test**

```rust
#[test]
fn tool_output_ages_before_any_ceiling_is_crossed() {
    let t = Temp::new();
    big_file(&t.dir, "big.txt", 4_000); // ≈ 10k tokens each read
    let (model, _seen) = RecordingModel::with_window(
        vec![
            response(vec![call("c1", "read_file", json!({"path": "big.txt"}))], None),
            response(vec![call("c2", "read_file", json!({"path": "big.txt"}))], None),
            response(vec![call("c3", "read_file", json!({"path": "big.txt"}))], None),
            response(vec![], Some("done")),
        ],
        200_000, // ceiling 40k — total stays below it, so only aging can fire
    );
    let mut config = KernelConfig::new(&t.dir);
    config.keep_newest_tokens = 800;
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    k.submit("read it three times").unwrap();

    let events = loong_eventlog::replay(&t.log).unwrap().events;
    let aging = events
        .iter()
        .find(|e| e.kind == ev::COMPACTION_APPLIED)
        .expect("an aging event");
    assert_eq!(aging.data["dropped_messages"], 0);
    assert!(aging.data["cleared_tool_outputs"].as_u64().unwrap() >= 2);
    assert!(aging.data["tokens_after"].as_u64().unwrap() < aging.data["tokens_before"].as_u64().unwrap());
}
```

- [ ] **Step 5: Run it to verify it fails**

Run: `cargo test -p loong-kernel turn::tests::tool_output_ages_before_any_ceiling_is_crossed`
Expected: FAIL — no `COMPACTION_APPLIED`.

- [ ] **Step 6: Implement `age_tool_outputs` and wire it into the step**

In `compaction.rs`, add `age_tool_outputs(&mut self, events: &mut Vec<u64>)`: return early when `self.ledger.tool_output_tokens() <= self.config.keep_newest_tokens`; otherwise call `clear_tool_outputs(self.config.keep_newest_tokens, marker)` with the same marker text as `compact_if_needed`, splice the cleared marker into `self.messages` exactly as phase 1 does (extract that splice into a private helper shared by both paths), and append `compaction.applied` with `dropped_messages: 0`, `kept_tokens: keep`, `summary: "aged {n} tool output(s) older than the newest {keep} tokens: {before} -> {after}"`.

In `loop.rs`, call `self.age_tool_outputs(&mut events)?;` immediately before `self.compact_if_needed(&mut events)?;`.

- [ ] **Step 7: Run the kernel test**

Run: `cargo test -p loong-kernel turn::tests::tool_output_ages_before_any_ceiling_is_crossed`
Expected: PASS.

- [ ] **Step 8: Write the no-op test**

```rust
#[test]
fn a_short_session_ages_nothing() {
    let t = Temp::new();
    big_file(&t.dir, "small.txt", 40); // ≈ 400 tokens
    let (model, _seen) = RecordingModel::new(vec![
        response(vec![call("c1", "read_file", json!({"path": "small.txt"}))], None),
        response(vec![], Some("done")),
    ]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    k.submit("read it once").unwrap();
    let events = loong_eventlog::replay(&t.log).unwrap().events;
    assert!(!events.iter().any(|e| e.kind == ev::COMPACTION_APPLIED));
}
```

Run: `cargo test -p loong-kernel turn::tests::a_short_session_ages_nothing`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/kernel/src/ledger.rs crates/kernel/src/turn/compaction.rs crates/kernel/src/turn/loop.rs crates/kernel/src/turn/tests.rs
git commit -m "feat(kernel): age stale tool output on every step"
```

---

### Task 4: Seat the L4 map once per turn (guard or fix, per Task 1)

**Files:**
- Modify: `crates/kernel/src/turn/compaction.rs` (`refresh_map` split)
- Modify: `crates/kernel/src/turn/loop.rs` (call sites)
- Test: `crates/kernel/src/turn/tests.rs`

**Interfaces:**
- Produces: `Kernel::refresh_memory(&mut self)` — deterministic `ContextEngine::refresh` only.
- Produces: `Kernel::seat_map(&mut self)` — reads `map_text`, replaces or inserts the pinned map message (today's `refresh_map` body).
- Consumes: Task 1's `the_request_prefix_is_append_only_within_a_turn`.

- [ ] **Step 1: Strengthen Task 1's test**

Task 1's `the_request_prefix_is_append_only_within_a_turn` already asserts the prefix; add the exact size assertion after `submit`:

```rust
    assert_eq!(
        second.len(),
        first.len() + 2,
        "one assistant call and one tool result are the only additions"
    );
```

That makes the test fail loudly if a seat step inserts, removes or rewrites any earlier message between two steps.

- [ ] **Step 2: Run it to see the current answer**

Run: `cargo test -p loong-kernel turn::tests::the_request_prefix_is_append_only_within_a_turn`
Expected: PASS when the map did not change, FAIL at the map block when it did. Record the observed behaviour in the commit message; do not force a failure.

- [ ] **Step 3: Split `refresh_map`**

Rename the current `refresh_map` body to `seat_map` (keep the insert/replace logic and `map_entry`/`map_index` bookkeeping); add `refresh_memory` that only calls `engine.refresh()`. In `run_inner`: call `self.refresh_memory();` at the top of each step (where `refresh_map` was), and call `self.seat_map();` once immediately before the loop and again after a `compact_if_needed` that appended an event. Update the file's module contract `Upstream`/`Downstream` lines and the doc blocks' `Up`/`Down`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p loong-kernel turn::tests`
Expected: PASS, including Task 1's test.

- [ ] **Step 5: Commit**

```bash
git add crates/kernel/src/turn/compaction.rs crates/kernel/src/turn/loop.rs crates/kernel/src/turn/tests.rs
git commit -m "fix(kernel): keep the request prefix stable inside a turn"
```

---

### Task 5: Show the session's usage while it runs

**Files:**
- Modify: `crates/tui-app/src/spend.rs` (new `usage_line`)
- Modify: `crates/tui-app/src/lib.rs` (draw a fixed row under the prompt, in `fn draw` near the bottom stack at ~3030)
- Test: `crates/tui-app/src/tests.rs`

**Interfaces:**
- Produces: `Spend::usage_line(&self) -> Option<String>` — `None` when `calls == 0`; otherwise `Some("入 {tokens} · 出 {tokens} · 缓存 {rate:.0}%")`, omitting the cache part when `cache_rate()` is `None`.
- Consumes: the existing `Spend::{tokens_in, tokens_out, cache_rate}` and `loong_tui::format_tokens`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn the_usage_line_reports_tokens_and_the_cache_rate() {
    let mut spend = Spend::default();
    spend.add(CostEntry::from_event(&serde_json::json!({
        "kind": "cost.recorded",
        "data": {"model": "k3", "tokens_in": 1000, "tokens_out": 100,
                 "cache_hit_tokens": 700, "cache_reported": true,
                 "usd_micros": 0, "price_basis": "unknown"}
    })).unwrap());
    let line = spend.usage_line().expect("a line after one call");
    assert!(line.contains("入"), "{line}");
    assert!(line.contains("70"), "{line}");
    assert!(Spend::default().usage_line().is_none());
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p loong-tui-app the_usage_line_reports_tokens_and_the_cache_rate`
Expected: FAIL — method not found.

- [ ] **Step 3: Implement `usage_line` and draw the row**

Return the string described in Interfaces, with the standard doc block. In `App::draw`, reserve one extra row in the bottom stack when `self.spend.usage_line()` is `Some`, and render `用量  {line}` after the prompt and before the keys. The row is fixed: `App::status` no longer carries a spend suffix, and `loong_tui` no longer draws one `· 令牌 …` line per `cost.recorded` event.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p loong-tui-app -p loong-tui`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/tui-app/src/spend.rs crates/tui-app/src/lib.rs crates/tui-app/src/tests.rs crates/tui/src/lib.rs
git commit -m "feat(tui): keep the session's usage in a fixed row under the prompt"
```

---

### Task 6: Documents, replay and the gate

**Files:**
- Modify: `docs/design/context-ledger.md` §3–§4
- Modify: `docs/05-parity-and-synthesis.md` §A2 (`Prompt-cache hit rate` row)
- Modify: `docs/superpowers/specs/2026-10-28-token-efficiency-design.md` only if the probe changed a claim

- [ ] **Step 1: Update the owner document**

In `context-ledger.md` §3 add the derived-budget formula and its precedence; in §4 add the aging rule (trigger = tool output over `keep_newest_tokens`, scope = `History`/`tool:` only, event = `compaction.applied`). Keep the document within its class budget (`docs/README.md`).

- [ ] **Step 2: Update the parity row from the probe result**

In `05-parity-and-synthesis.md` §A2, replace the `Prompt-cache hit rate ❌ / new T-25` row with what Task 1 measured: either the mechanism now exists (and name the test), or the row states that the prefix is stable and the provider's cache accounting is still unproven. Do not claim a saving that was not measured.

- [ ] **Step 3: Run the replay**

```bash
# Manual, not committed (the log is a person's session, not test data):
# 1. rebuild the ledger from the log, 2. apply §4.1–§4.2 per step,
# 3. print P95 per-request input and the total.
python3 /tmp/token-replay.py ~/.loongagent/projects/home-fengchang-test-fcandroidmusicplayer-64c9bfe7/e289jjjd.jsonl
```

Expected: P95 ≤ 45k and total ≤ 2.6M against the 3.03M baseline. Record both numbers in the implementation report; if either misses, do not change the spec's target silently — report it and let the human decide.

- [ ] **Step 4: Run the whole gate**

Run: `./scripts/check.sh`
Expected: every section OK, exit 0.

- [ ] **Step 5: Commit**

```bash
git add docs/design/context-ledger.md docs/05-parity-and-synthesis.md
git commit -m "docs: record the derived budget, aging and the cache probe"
```

---

## Self-review

- **Spec coverage:** §4.1 → Task 2; §4.2 → Task 3; §4.3 → Tasks 1 and 4; §4.4 → Task 5; §4.6 replay → Task 3's event reuse plus Task 6's replay; §5 probe → Task 1; §5 owner docs → Task 6. Non-goals have no tasks, by design.
- **Type consistency:** `default_budget(u64) -> u64`, `tool_output_tokens(&self) -> u64`, `age_tool_outputs(&mut self, &mut Vec<u64>) -> Result<(), KernelError>`, `refresh_memory(&mut self)`, `seat_map(&mut self)`, `usage_line(&self) -> Option<String>` — each name is used once, in the task that owns it.
- **Review Focus coverage:** huge window → Task 2 Step 5; oversized single output → Task 3 Step 4; short session → Task 3 Step 8; pinned map → Task 3's `tool_output_tokens` test plus Task 4's prefix test; replay → Task 3 Step 4's event and Task 6 Step 3.
