# Context visibility and compaction thrash protection

- **Date:** 2026-11-03
- **Status:** Approved in conversation; this document is the spec the implementation plan is written from
- **Scope:** two follow-ups to the 2026-10-28 token-efficiency and 2026-11-03 turn-budget changes — show how full the context is in the fixed usage row, and stop a turn whose clearing passes fire so often that they cannot be keeping up. No schema change, no new configuration key.

## 1. Problem

After the budget, aging and turn-budget work, two gaps remain visible in a real session:

1. **The person cannot see the context filling.** The fixed row under the prompt
   shows session input/output and the cache rate; the token/budget pair lives
   behind F2. Claude Code's most-read number is the context percentage, and it is
   the one number that tells a person when to compact, stop, or split a task.
2. **A clearing pass that fires every step has no stop.** `age_tool_outputs` and
   `compact_if_needed` can each free a little and be refilled immediately. The
   loop then pays a `compaction.applied` event and a re-seated map on every step,
   while the context stays at the ceiling. Claude Code calls this thrash and stops
   auto-compacting when the context refills immediately; we have no equivalent.

## 2. Goals and success criteria

| # | Criterion | How it is measured |
|---|---|---|
| 1 | The usage row shows the last request's context share when a budget is known | `Spend::usage_line` test: `上下文 62%` for 62k of a 100k budget |
| 2 | Without a useful budget it shows tokens instead of a made-up percentage | `Spend::usage_line` test: `上下文 62,000`, no `%` |
| 3 | The number is live during a turn, not only after it | The worker sends `RefreshContext` at startup; each `cost.recorded` updates `last_request_tokens` |
| 4 | Three ineffective clearing passes within five steps stop the turn with a note | Kernel test: passes that leave the context over its ceiling stop; passes that free real room do not |
| 5 | The guard resets with the turn, like the other stall state | `Kernel::begin_turn` clears `compaction_steps`; the next turn starts with an empty window |
| 6 | Ordinary sessions are unchanged | Existing suites pass; no event kind or config key changes |

## 3. Non-goals

- **“Until auto-compact” in the row.** That needs `compact_at_percent` on the
  `ContextExplain` wire; the percentage alone answers the question without a
  schema change.
- **A provider quota meter.** k3's subscription window is server-side and not
  exposed by the API; this spec does not guess it.
- **A cumulative token cap.** Decided against in the 2026-11-03 discussion: the
  subscription's unit is opaque, and Claude Code does not do it either.
- **A new event kind.** The thrash stop is an assistant `message.appended`, like
  the step-limit and stall notes.

## 4. Design

### 4.1 The context share in the usage row

`Spend` gains `last_request_tokens`, set from `tokens_in` on every
`cost.recorded`. That value is the provider's own count of the prompt just sent,
which is exactly “how full was the window on the last call”.

`Spend::usage_line(budget: Option<u64>)` appends:

- `· 上下文 62%` when `budget` is `Some(n)` with `n > 0`;
- `· 上下文 62,000` when the budget is unknown or zero.

The draw path passes `self.context.as_ref().map(|c| c.budget_tokens)`. The
worker already refreshes the context after every command; `surface::run` sends
one `ToWorker::RefreshContext` at startup so the budget is known before the
first turn finishes. During a turn the numerator moves with every cost event;
the denominator changes only when the model or config changes.

The row stays one line and keeps its existing segments; the context share is the
last one, so it is the first thing lost if a terminal is too narrow — a truncated
row is still readable.

### 4.2 Compaction thrash

`Kernel` gains `compaction_steps: VecDeque<(step, before, after)>` (at most
three entries), reset by `Kernel::begin_turn`. After `age_tool_outputs` and
`compact_if_needed` append anything, `run_inner` records the step and what the
pass freed. A pass is **ineffective** when it freed less than `COMPACTION_THRASH_FREE_PERCENT`
(5%) of the context before it, or when the context is still at or over its
ceiling after it. When the window holds three passes, they all fall inside five
steps, and every one of them is ineffective, the turn ends before the next model
call with:

```
stopped after N steps without progress (compaction thrash): clearing fired 3 times
within 4 steps, and each freed less than 5% or left the context over its ceiling —
raise `budget_tokens` or `keep_newest_tokens`, or split the task
```

The note goes through `record_assistant`, exactly like the step-limit note, so
every surface already renders it. The compaction events that preceded it are
already in the log, which is what justifies the stop.

Frequency alone would be wrong: a busy step can age three times and free real
room each time, which is ordinary housekeeping. The freed ratio is what separates
"the pass is working" from "the pass is chasing its tail", and the ceiling check
covers the case where even a large clear cannot get under the budget because the
newest output alone is larger than it.

## 5. Verification

| Check | Where |
|---|---|
| Percentage with a budget, tokens without | `cargo test -p loong-tui-app spend::tests::the_usage_line_reports_the_context_share` |
| Three ineffective passes in five steps stop; effective passes do not | `cargo test -p loong-kernel turn::tests::compaction_thrash_stops_the_turn` · `turn::tests::a_compaction_that_frees_room_is_not_thrash` |
| The whole gate | `./scripts/check.sh` |

## 6. Risks

- **A narrow terminal drops the context share.** Accepted: it is the last
  segment, and F2 still has the full ledger.
- **The percentage is the last request, not the next one.** A turn that has just
  added a large tool result shows it on the following cost event. This is the
  same lag Claude Code has and is cheaper than a request per step.
- **A legitimate aggressive keep window trips the thrash stop.** Three passes
  within five steps means each step's output exceeds the keep window; the note
  names the two keys to raise, and one reply resumes with a fresh turn.
- **A budget of zero in `ContextExplain`** means no window and no budget; the row
  shows tokens rather than dividing by zero.
