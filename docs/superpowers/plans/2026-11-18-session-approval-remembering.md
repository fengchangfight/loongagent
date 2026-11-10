# Session approval remembering — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** an irreversible command approved once with "本会话允许" stops asking
again for the rest of the session, without weakening ADR-0020 and without
persisting anything.

**Architecture:** refactor the irreversibility detector in `loong-tools` to
expose *every* irreversible segment (it currently short-circuits on the
first), derive a `(verb, operands)` key per segment, store approved keys in
a `HashSet` on the in-memory `Kernel`, and skip the ask when a new command's
irreversible segments are all remembered. Then make the TUI/web "本会话允许"
button send `ApprovalScope::Session` so the kernel honours it.

**Tech Stack:** Rust workspace (`loong-tools`, `loong-kernel`, `loong-tui-app`,
`loong-web`), protocol already carries `scope` — no schema change.

**Spec:** `docs/superpowers/specs/2026-11-18-session-approval-remembering.md`

## Global Constraints

- ADR-0020 stands: a remembered key comes only from a human approval, never
  from config or YOLO.
- Nothing persists: the set lives on `Kernel`, dies with the session.
- A match must be total: every irreversible segment of a command must be
  remembered, else the whole command asks again.
- Module contracts: every new `fn` carries a doc block (`Purpose:` plus the
  five labels, or `Purpose:` + `Contract:` for a body ≤ 12 lines). See
  `AGENTS.md`.
- Run `XDG_RUNTIME_DIR=$TMPDIR cargo test …` for sandboxed tests.
- README test count: after adding tests, update the "2227 passed" figure in
  `README.md` to the new total from `cargo test --workspace`.

## Review Focus

Inputs the spec implies but no single test pins — each gets a test in the
owning task:

1. **Compound command with one un-remembered irreversible segment** — must
   still ask, even if another segment is remembered. (Task 2)
2. **An irreversible segment with no resolvable operand** (`rm` with no
   path) — `segment_key` is `None`, never auto-approved. (Task 1)
3. **`rm -rf .smoke` vs `rm -rf ~`** — different operand sets must produce
   different keys. (Task 1)
4. **Prefix words and assignments before the verb** (`sudo FOO=1 rm x`) —
   the key's verb is the real verb, not `sudo`. (Task 1)
5. **Restart clears memory** — a fresh `Kernel` has an empty set, so the
   same command asks again. (Task 2)

---

### Task 1: segment keys in `loong-tools`

**Files:**
- Modify: `crates/tools/src/lib.rs` (`irreversible_reason` ~line 736,
  `segments` ~line 776)
- Test: `crates/tools/src/lib.rs` (the crate's own test module)

**Interfaces:**
- Produces:
  - `pub fn irreversible_segments(command: &str) -> Vec<Vec<String>>` — every
    irreversible segment's token list (owned `String`s, not borrows, so the
    kernel can hold them).
  - `pub fn segment_key(segment: &[String]) -> Option<(String, Vec<String>)>`
    — the rememberable `(verb, sorted-operands)` shape, `None` when the
    segment has no resolvable operand.
  - Refactor: `irreversible_reason` keeps its signature but is reimplemented
    as "first segment from `irreversible_segments` mapped to its reason", so
    existing behaviour is unchanged.

- [ ] **Step 1: Write the failing tests**

In the tools test module:

```rust
#[test]
fn every_irreversible_segment_is_returned_not_just_the_first() {
    let segs = irreversible_segments("rm -rf a && git push origin main");
    assert_eq!(segs.len(), 2);
}

#[test]
fn a_deletion_key_is_verb_plus_paths() {
    let segs = irreversible_segments("rm -rf .smoke");
    assert_eq!(
        segment_key(&segs[0]),
        Some(("rm".to_string(), vec![".smoke".to_string()]))
    );
}

#[test]
fn different_targets_make_different_keys() {
    let a = irreversible_segments("rm -rf .smoke");
    let b = irreversible_segments("rm -rf ~");
    assert_ne!(segment_key(&a[0]), segment_key(&b[0]));
}

#[test]
fn a_bare_deletion_has_no_key() {
    // `rm` alone has no operand to remember.
    let segs = irreversible_segments("rm");
    assert!(segs.is_empty() || segment_key(&segs[0]).is_none());
}

#[test]
fn the_verb_skips_prefixes_and_assignments() {
    let segs = irreversible_segments("sudo FOO=1 rm -rf .smoke");
    assert_eq!(
        segment_key(&segs[0]),
        Some(("rm".to_string(), vec![".smoke".to_string()]))
    );
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p loong-tools segment`
Expected: FAIL — `irreversible_segments` / `segment_key` not defined.

- [ ] **Step 3: Implement the refactor and the helpers**

Split `irreversible_reason`'s per-segment loop so the deletion/egress
detection of one segment is a private predicate; `irreversible_segments`
collects every segment for which it fires; `irreversible_reason` maps the
first to its reason string. `segment_key` reuses the same verb-isolation
(skip `PREFIX_WORDS` and `NAME=value` assignments, then `basename`), and
collects operands: for a deletion the non-flag path tokens, for an egress
push the remote/ref tokens. Return owned `String`s. Keep each new `fn`'s
doc block per the module-contract rules.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p loong-tools`
Expected: PASS, including the pre-existing classification tests (the
refactor must not change any existing classification).

- [ ] **Step 5: Commit**

```bash
git add crates/tools/src/lib.rs
git commit -m "tools: expose every irreversible segment and its rememberable key"
```

---

### Task 2: remember and match on the kernel

**Files:**
- Modify: `crates/kernel/src/turn.rs` (`struct Kernel` ~line 495,
  `respond_to_approval`, `auto_approve`)
- Modify: `crates/kernel/src/turn/loop.rs` (the irreversible ask at ~line 411)
- Test: `crates/kernel/src/turn/tests.rs`

**Interfaces:**
- Consumes: `loong_tools::{irreversible_segments, segment_key}` (Task 1);
  `loong_schema::generated::ApprovalScope`.
- Produces:
  - `Kernel.remembered_irreversible: HashSet<(String, Vec<String>)>` —
    private field, empty at construction (this is the "restart clears
    memory" guarantee).
  - `respond_to_approval` on `granted=true, scope=Session` inserts the
    command's keys; the command string comes from the pending approval
    record the kernel already holds for `request_id`.
  - A private `Kernel::is_remembered(command) -> bool`: true iff the command
    has ≥1 irreversible segment and every one yields a key in the set.

- [ ] **Step 1: Write the failing tests**

In `crates/kernel/src/turn/tests.rs` (follow the existing approval-test
harness used by `opting_out_of_prompts_still_asks_before_a_deletion`):

```rust
#[test]
fn a_session_approved_command_does_not_ask_again() {
    // run a turn that runs `rm -rf .smoke`; approve it with Session scope;
    // run the identical command again; assert no second approval.requested.
}

#[test]
fn a_changed_segment_still_asks() {
    // approve `rm -rf .smoke`; then `rm -rf other` must emit approval.requested.
}

#[test]
fn a_compound_command_with_one_new_irreversible_segment_still_asks() {
    // approve `rm -rf .smoke`; then `rm -rf .smoke && git push origin main`
    // must ask, because the push segment is not remembered.
}

#[test]
fn a_fresh_kernel_asks_again() {
    // approve in one kernel; build a new kernel; same command must ask.
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `XDG_RUNTIME_DIR=$TMPDIR cargo test -p loong-kernel session_approved`
Expected: FAIL — no remembering yet, so the re-issued command asks again.

- [ ] **Step 3: Implement remembering and matching**

Add the `HashSet` field. In `respond_to_approval`, on granted+`Session`,
compute keys from the pending approval's command and insert them. In
`loop.rs`'s irreversible branch, before the `pending_approvals.contains`
check, add: `if self.is_remembered(command) { record the decision and run
without asking }`. Keep the existing `approval.decided` event shape; the
reason string notes it was remembered for the session.

- [ ] **Step 4: Run to verify they pass**

Run: `XDG_RUNTIME_DIR=$TMPDIR cargo test -p loong-kernel`
Expected: PASS, including all pre-existing approval tests (Once scope and
refusals unchanged).

- [ ] **Step 5: Commit**

```bash
git add crates/kernel/src/turn.rs crates/kernel/src/turn/loop.rs crates/kernel/src/turn/tests.rs
git commit -m "kernel: remember a session-approved irreversible command's shape"
```

---

### Task 3: honest "本会话允许" buttons (TUI + web)

**Files:**
- Modify: `crates/tui-app/src/lib.rs` (`options()` ~line 3991)
- Modify: `crates/web/src/app.js` (approval respond builder)
- Modify: `docs/design/flows/tool-approval.md`, `docs/design/tools-permissions.md`
- Test: `crates/tui-app/src/tests.rs`

**Interfaces:**
- Consumes: kernel now honours `ApprovalScope::Session` (Task 2).
- Produces: TUI's "本项目允许" → label "本会话允许", sends `Session` (the
  `options()` table already maps to `ApprovalScope`; change the `Project`
  entry). Web's respond gains a scope choice sending `Session`.

- [ ] **Step 1: Write the failing test**

In `crates/tui-app/src/tests.rs`:

```rust
#[test]
fn the_second_approval_option_is_session_not_project() {
    let opts = ApprovalMenu::options();
    assert_eq!(opts[1].2, loong_schema::generated::ApprovalScope::Session);
    assert_eq!(opts[1].0, "本会话允许");
}
```

(Adjust the type name to whatever `options()` is a method on.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p loong-tui-app session_not_project`
Expected: FAIL — currently `Project` / "本项目允许".

- [ ] **Step 3: Implement the TUI and web changes**

TUI: change the `options()` entry. Web: add the scope to the respond
payload and a "本会话允许" affordance. Docs: add the remembering mechanism
to `tool-approval.md` (it has budget headroom) and a one-line pointer in
`tools-permissions.md` §8 (that file is at its 250-line cap — replace a
line, don't add).

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p loong-tui-app && python3 scripts/check-docs.py`
Expected: PASS.

- [ ] **Step 5: Update the README test count and commit**

Run: `XDG_RUNTIME_DIR=$TMPDIR cargo test --workspace 2>&1 | grep -E '^test result:'`
Update `README.md`'s "2227 passed" to the new total, then:

```bash
git add crates/tui-app crates/web docs/design README.md
git commit -m "surfaces: make 本会话允许 honest — send Session scope and remember the shape"
```

---

## Self-review notes

- **Spec coverage:** segment keys (Task 1), kernel remember+match (Task 2),
  surface buttons + docs (Task 3). Persistence, globs, non-shell tools are
  out per the spec's "deliberately out" — no task, correctly.
- **Type consistency:** `(String, Vec<String>)` key type is identical in
  Task 1 (producer) and Task 2 (consumer). `ApprovalScope::Session` matches
  the existing protocol enum.
- **No schema change:** confirmed `ApprovalRespondParams` already has
  `scope`; the plan makes the kernel read it rather than widening the wire.
