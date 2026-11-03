# Turn budget, stall guard and a cache-stable approval resume

- **Date:** 2026-11-03
- **Status:** Approved in conversation; this document is the spec the implementation plan is written from
- **Scope:** make the step budget belong to the **turn** rather than to one run of the loop, stop a model that is visibly spinning, and stop an approval resume from rewriting the prompt prefix. Four small changes inside the kernel; no new event kind, no new configuration key.

## 1. Problem

A real session on this machine (`7c86phhd`, `~/test/fcdraw`, 2026-11-03
10:40–11:23, 938 log records) ended with “stopped after 100 steps” after spending
the user's allowance in 43 minutes:

| Measured fact | Value |
|---|---|
| Model calls / tool calls | 171 / 183 |
| Input / output tokens | 8,347,721 / 46,214 |
| Cache hit / miss tokens | 7,625,472 (91.3%) / 722,249 |
| Calls after the last approval (`/yolo`, 11:02) | 99, 5.89M input in 21.7 min |
| Tool calls spent fighting headless Chrome | 60, 3.06M input, 14.3 min |
| Tool calls spent writing `repro1..15` / `vmrepro` | 51, 3.37M input, 10.1 min |
| Prefix-cache resets (hit drops to 2,816) | 17; 603,032 of the 722,249 misses |

Four mechanisms produced the number:

1. **The step budget restarts after every approval.** `Kernel::respond_to_approval`
   calls `Kernel::run` → `Kernel::run_inner`, whose loop is
   `for step in 1..=self.config.max_steps`. The count is therefore per *run*, not
   per *turn*. Nine approvals plus `/yolo` split one user turn into ten runs: the
   turn took 159 model calls while the person was told “100 steps”.
2. **Nothing notices a model that repeats itself.** The Chrome phase ran 13
   near-identical launch/`cdp.js` commands and ten of them failed; the repro phase
   wrote 17 throwaway scripts under `.tmp/`. Neither pattern is a protocol error,
   so the loop treated every call as new work.
3. **An approval resume rewrites the L4 map before the conversation.** `run_inner`
   calls `refresh_memory` + `seat_map` at the start of every run. The map sits
   before the conversation, so its new text invalidates every provider cache entry
   after it. All nine approval resets and six compactions account for 17 of the
   722k full-price tokens.
4. **`DEFAULT_MAX_STEPS = 100` is larger than a per-turn cost cap can justify.**
   With a steady-state request near 60k tokens, one full budget is ≈ 6M input
   tokens even when every step is honest work.

## 2. Goals and success criteria

| # | Criterion | How it is measured |
|---|---|---|
| 1 | The step budget is one budget for the whole turn, including approval resumes | A scripted turn whose first call asks and whose `max_steps` is 3 takes exactly three model calls across the resume, then reports the step limit |
| 2 | A model that repeats a call, keeps failing, or churns throwaway files is stopped before the budget is spent | Three kernel tests pin each trigger and the note each produces |
| 3 | None of the guard's state resets on an approval resume | Criterion 1's test crosses an approval; the three counters are cleared only by `Kernel::begin_turn` |
| 4 | An approval resume does not move the request prefix | Two recorded requests across an approval: the second begins with the first, byte for byte |
| 5 | The default budget is 50 steps, and the docs say why | `DEFAULT_MAX_STEPS` and `docs/design/kernel-loop.md` §4 agree |
| 6 | Nothing changes when a turn makes ordinary progress | Existing suites pass; a short turn emits no new event and no new message |

## 3. Non-goals

- **A cumulative token quota.** `budget_tokens` bounds one request, and the spec
  for it says so explicitly. A per-turn token meter would need a new key, a new
  narrow-only merge rule and a policy for what happens when a model output is
  already paid for; that is a separate decision.
- **Subagents for verification.** They remain the structural answer to the Chrome
  and repro loops, and remain out of scope here.
- **Prices for subscription plans.** k3 still reports `price_basis: "unknown"`;
  this spec does not invent a number.
- **A new event kind.** The guard's note is an ordinary assistant
  `message.appended`, like the step-limit note.

## 4. Design

### 4.1 The budget belongs to the turn

`Kernel` gains `steps_taken: usize`, reset by `Kernel::submit_with` (a new turn)
and preserved by `Kernel::respond_to_approval` (the same turn). `run_inner`
iterates `start+1 ..= max_steps` where `start = steps_taken`, so:

- the router's `Facts.step` keeps counting across a resume;
- `TurnOutcome.steps` is the number of completions this **turn** has taken, not
  this run;
- an approval consumes one step from the same budget, which is what the person
  reading “100 steps” already assumes.

`Kernel::submit_with` also resets the stall state (§4.2) and the map-seated flag
(§4.3). A user who replies “continue” starts a new turn with a fresh budget; that
is the documented way to ask for more work.

### 4.2 A stall guard

Three independent, deliberately narrow triggers. They are checked at the step
boundary, before the next model call, so a tool that started finishes:

| Constant | Value | Trigger |
|---|---|---|
| `STALL_REPEAT_CALLS` | 3 | the last three dispatched calls have the same tool and the same canonical JSON arguments |
| `STALL_FAILURES` | 5 | the last five recorded tool results are `ok: false` |
| `STALL_SCRATCH_WRITES` | 5 | five distinct files were written in this turn under a `.tmp` component or the system temp directory |

The third exists because the repro phase wrote new paths (`repro1.js`,
`repro2.js`, …) whose arguments were never equal: an equality check alone would
not see that loop. It counts `write_file` only, and only paths that are already
throwaway by convention; ordinary source edits never match.

When a trigger holds, the loop appends one assistant `message.appended`:

```
stopped after N steps without progress (stall guard): <reason> — tell me how you want to proceed
```

It returns a normal `TurnOutcome` with `final_text` set to that note and `steps`
to the count already taken. No new event kind; the log already shows the repeated
calls, the failures and the scratch writes that justify the stop.

`record_tool_result` feeds the failure counter. The “not run” results written for
the calls after a parked approval do not: nothing ran, so nothing failed.

### 4.3 The map is seated per turn, not per run

`Kernel` gains `map_seated: bool`.

- `run_inner` seats the map only when the flag is false, then sets it.
- An approval resume finds it true and starts the request with the same bytes the
  parked request ended with.
- A clearing pass (`age_tool_outputs` or `compact_if_needed`) re-seats it, because
  the messages after the map already changed; the flag stays true.
- `seat_map` itself becomes idempotent: when the map message already carries the
  new text, it writes nothing. A new turn whose map is unchanged therefore keeps
  the provider's cache too.

The map may be one turn stale. That is already the accepted trade in
`docs/superpowers/specs/2026-10-28-token-efficiency-design.md` §4.3, and the
background enrichment already defers while a turn is running.

### 4.4 A smaller default budget

`DEFAULT_MAX_STEPS` becomes 50. One budget is now one turn, and 50 completions
cover the “dozens” a page of working HTML needs while halving the worst case. A
person who wants more replies, raises `max_steps`, or grants `/yolo`; all three
remain visible.

## 5. Verification

| Check | Where |
|---|---|
| Budget survives an approval resume | `cargo test -p loong-kernel turn::tests::the_step_budget_survives_an_approval_resume` |
| Repeated call / repeated failure / scratch churn each stop with the stall note | `cargo test -p loong-kernel turn::tests::` (three tests named after the trigger) |
| The prefix does not move across an approval | `cargo test -p loong-kernel turn::tests::an_approval_resume_does_not_move_the_request_prefix` |
| `seat_map` does not rewrite an unchanged map | `cargo test -p loong-kernel turn::tests::seat_map_is_idempotent` |
| The default is 50 and honest work still fits | `cargo test -p loong-kernel turn::tests::the_default_step_limit_leaves_room_for_a_real_task` |
| The whole gate | `./scripts/check.sh` |

## 6. Risks

- **A false stall on honest work.** Three identical calls in a row, five failures
  in a row, or five scratch scripts are all unusual in a task that is making
  progress; the note names the reason and asks the person, so a false positive
  costs one reply, not a lost turn.
- **The budget now ends a turn that used to continue through an approval.** That
  is the intent: the person approved a call inside a turn, not a new turn's worth
  of work. The step-limit note says how to continue.
- **A stale map for a long turn.** Unchanged from the 2026-10-28 spec; compaction
  re-seats it and the next turn refreshes it.
- **Lowering the default surprises someone with a 60-step task.** `max_steps` is
  already reported by `config show`, and the note names the key.

## 7. Alternatives considered

- **Only reset `steps_taken` in `submit_with` and keep seat_map per run.** Fixes
  the count but leaves the 17 cache resets; rejected because the map change is
  free to avoid.
- **Detect progress by hashing the workspace.** Expensive, and a legitimate
  investigation changes nothing for many steps; rejected as the primary signal.
- **Make the stall guard emit a new event.** The existing `message.appended`
  already tells the surface and the log everything a person needs; a new event
  would need schema, replay and surface work for no new fact.
- **A per-turn token ceiling instead of a step ceiling.** The right long-term
  meter, but it is a new configuration decision (§3); the step budget is the
  control that already exists.
