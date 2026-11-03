# Paste, busy prompt and approval ids Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a pasted block one prompt, stop enter from queueing a prompt behind a running turn, continue approval ids across a resume, and make the pending-approval refusal readable.

**Architecture:** Three independent seams — the terminal event loop (`surface::run` + `Event::Paste`), the key handler (`App::handle_key` and a new `App::paste`), and the kernel's approval counter (`Kernel::build` seeds it from the index). Tests sit in `crates/tui-app/src/tests.rs` and `crates/kernel/src/turn/tests.rs`.

**Tech Stack:** Rust 2021, crossterm bracketed paste, ratatui, `cargo test -p loong-tui-app -p loong-kernel`.

**Spec:** `docs/superpowers/specs/2026-11-03-tui-paste-and-approval-ids-design.md`

## Global Constraints

- **No schema change, no config key.** The paste is a terminal event; the id is derived from the log.
- **Contracts:** every new `fn`/field carries `Purpose:` plus `Serves`, `Design`, `Up`, `Down`, `Verify`; touched files update their item lists.
- **Gates:** both crate suites green per task; `./scripts/check.sh` green before the final commit.
- **Language:** code comments English; user-facing TUI lines Chinese; commit messages `fix:`/`feat:`/`docs:`.

## File Structure

| File | Responsibility in this change |
|---|---|
| `crates/tui-app/src/surface.rs` | enable/disable bracketed paste, route `Event::Paste` |
| `crates/tui-app/src/lib.rs` | `App::paste`, the busy-enter guard, the friendly failure line |
| `crates/tui-app/src/tests.rs` | paste, busy-enter and error-text tests |
| `crates/kernel/src/turn/build.rs` | seed `approvals` from the log |
| `crates/kernel/src/turn/loop.rs` | the actionable pending message |
| `crates/kernel/src/turn/tests.rs` | the appr-2 test |
| `docs/design/kernel-loop.md` | §5 approval ids |

---

### Task 1: A pasted block is one prompt

**Files:** `crates/tui-app/src/lib.rs`, `crates/tui-app/src/surface.rs`, `crates/tui-app/src/tests.rs`

- [ ] **Step 1: Write the failing test**

```rust
/// Purpose: A pasted block lands in the input as one block and never submits itself.
#[test]
fn a_pasted_block_stays_one_prompt() {
    let mut app = App::new("s".into(), "schema".into());
    let action = app.paste("第一行\r\n第二行\n");
    assert!(matches!(action, Action::None), "{action:?}");
    assert_eq!(app.input, "第一行\n第二行");
    assert!(!app.busy);
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p loong-tui-app tests::a_pasted_block_stays_one_prompt`
Expected: FAIL — `App::paste` does not exist.

- [ ] **Step 3: Implement `App::paste` and the terminal wiring**

`App::paste(&mut self, text: &str) -> Action`: normalise `\r\n`/`\r` to `\n`,
strip one trailing newline, `self.insert(&normalised)`, return `Action::None`.
In `surface::run`, add `EnableBracketedPaste` to the enter sequence and
`DisableBracketedPaste` to the exit, and route `event::Event::Paste(text)` to
`app.paste(&text)`.

- [ ] **Step 4: Run the TUI suite**

Run: `cargo test -p loong-tui-app`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/tui-app/src
git commit -m "fix(tui): paste a block as one prompt"
```

---

### Task 2: Enter while a turn runs refuses, but commands still work

**Files:** `crates/tui-app/src/lib.rs`, `crates/tui-app/src/tests.rs`

- [ ] **Step 1: Write the failing tests**

```rust
/// Purpose: A plain prompt typed during a turn is refused; a slash command still runs.
#[test]
fn enter_while_busy_refuses_a_prompt_but_not_a_command() {
    let mut app = App::new("s".into(), "schema".into());
    app.busy = true;
    app.input = "do it again".into();
    assert!(matches!(app.handle_key(key(KeyCode::Enter)), Action::None));
    assert!(app.transcript().contains("上一轮还在跑"));

    app.input = "/yolo".into();
    assert!(matches!(app.handle_key(key(KeyCode::Enter)), Action::Send(_) | Action::None));
}
```

- [ ] **Step 2: Implement the guard**

In the enter arm, keep `surface_command` first; then, if `self.busy`, push the
line and return `Action::None`; otherwise `begin_submit`.

- [ ] **Step 3: Run the suite and commit**

Run: `cargo test -p loong-tui-app`, then
`git commit -m "fix(tui): do not queue a prompt behind a running turn"`.

---

### Task 3: Approval ids continue, and the refusal is readable

**Files:** `crates/kernel/src/turn/build.rs`, `crates/kernel/src/turn/loop.rs`, `crates/kernel/src/turn/tests.rs`, `crates/tui-app/src/lib.rs`, `crates/tui-app/src/tests.rs`

- [ ] **Step 1: Write the failing kernel test**

```rust
/// Purpose: A reopened log's next approval id continues instead of colliding with the first.
#[test]
fn approval_ids_continue_after_a_resume() {
    // create → submit (asks appr-1) → approve → drop → open → submit (asks appr-2)
}
```

- [ ] **Step 2: Implement the seed**

`build.rs`: parse the index's `approval.requested` count and use it for
`approvals`. `loop.rs`: make the pending message say what to do.

- [ ] **Step 3: Write the failing TUI text test**

```rust
/// Purpose: The pending-approval refusal is rendered as an instruction, not a protocol error.
#[test]
fn a_pending_approval_error_reads_as_advice() {
    // FromWorker::Failed("out of order: approval appr-1 is still pending")
    // → transcript contains 还有一条操作在等审批
}
```

Map the message in `App::drain`'s `Failed` arm.

- [ ] **Step 4: Run both suites, the gate and commit**

Run: `cargo test -p loong-kernel -p loong-tui-app` then `FAST=1 ./scripts/check.sh`.
Update the README `test-counts` line.

```bash
git commit -m "fix(kernel): continue approval ids and explain a pending refusal"
```

---

### Task 4: The document

**Files:** `docs/design/kernel-loop.md` §5.

- [ ] **Step 1: State that `approvals` is seeded from the log, so a resume continues the ids.**
- [ ] **Step 2: Run `python3 scripts/check-docs.py` and `FAST=1 ./scripts/check.sh`.**
- [ ] **Step 3: Commit**

```bash
git commit -m "docs: approval ids continue after a resume"
```
