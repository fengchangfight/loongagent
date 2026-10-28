# Token efficiency: default budget, tool-output aging and a stable prefix

- **Date:** 2026-10-28
- **Status:** Approved in conversation; this document is the spec the implementation plan is written from
- **Scope:** bound an ordinary session's per-request input and make its prompt prefix cacheable, without changing what the model can do. Three mechanisms — a default input budget derived from the model window, proactive aging of old tool output, and freezing the L4 cognitive map inside a turn — plus the session-level visibility they need. No subagents, no new truncation policy, no `read_file` offsets.

## 1. Problem

A real session on this machine (`e289jjjd`, working copy of a small Android
music player, 2026-10-28 15:17:52–15:43:03, 443 log records) spent the user's
daily token allowance in 25 minutes:

| Measured fact | Value | Evidence |
|---|---|---|
| Model calls | 80 | sum of `cost.recorded`, `e289jjjd.jsonl:3`–`:439` |
| Input / output tokens | 3,029,928 / 25,923 | same |
| Input of the first vs last call | 3,501 → 61,526 | `:3`, `:439` |
| Calls after the first failed build | 32 calls, 1,753,254 tokens (57.9%) | `:277`–`:443` |
| Cache hits | 214,400 (7.1 %); k3 reports exactly 2,816 on all 75 calls | `:439` |
| `compaction.applied` events | 0 | no such record in 443 |

The session that built the app (`ghnraebr`, 12:52–15:07) is the larger half:
184 calls, 17,217,472 input tokens, context rising to 167,744 with no compaction
(`ghnraebr.jsonl:1000`).

Four mechanisms explain the numbers; only three of them are this spec's.

1. **Nothing bounds the input but the model's window.** `Kernel::input_ceiling`
   (`crates/kernel/src/turn/compaction.rs`) takes the stricter of the explicit
   `budget_tokens` and the window minus `reserved_context_size`. Local mode sets
   no budget and k3 declares a 1,048,576-token window (`~/.loongagent/local.toml:17`), so the
   ceiling is 1,032,192. Compaction cannot fire in a 25-minute session, and the
   four budget keys are unset by every layer (`loongagent config explain
   budget_tokens`).
2. **Tool output is only cleared when the ceiling is already crossed.**
   `Ledger::clear_tool_outputs` (`crates/kernel/src/ledger.rs`) is called from
   the compaction path alone. Below the ceiling, every build log and every
   re-read file stays verbatim for the rest of the session.
3. **The prompt prefix is rewritten every step.** `Kernel::refresh_map` runs
   inside `run_inner`'s step loop (`crates/kernel/src/turn/loop.rs`) and
   replaces the L4 `System` message that `refresh_map` itself seats *before* the
   conversation (`compaction.rs`). A provider that caches a common prefix can
   reuse nothing after that block once its text changes; a constant 2,816-token
   hit is consistent with only the fixed preamble surviving. This is a
   hypothesis, not a measurement — §5 makes proving it the first task.
4. **The agent made too many calls.** 40 of 90 tool calls are edits and 32 are
   the unplanned dexer investigation. No context policy fixes that; it is why
   §3 keeps subagents out of scope and why §2's token target is modest.

Money is already visible in principle: `crates/config/src/pricing.rs` prices
DeepSeek, `cost.recorded` carries `usd_micros`, and the TUI has `/cost` and a
spend summary. Models without a published list price report
`price_basis: "unknown"` and the UI says so — that is correct, and this spec
does not guess prices.

## 2. Goals and success criteria

Goal: an ordinary turn sends a bounded, cacheable prompt, and a person can see
what it costs. "At least as good as Claude Code" means the same mechanisms
(auto-compaction with a real ceiling, micro-compaction of stale tool output,
prefix-cache reuse), not the same model behaviour.

| # | Criterion | How it is measured |
|---|---|---|
| 1 | Every model with a declared window gets a derived ceiling below the window, even with no `budget_tokens` | Unit tests assert `clamp(window/4, 32_000, 100_000)`, its interaction with `compact_at_percent`, and that an explicit budget always wins |
| 2 | Compaction fires before a request would exceed the ceiling | A synthetic long session produces `compaction.applied` with `tokens_after <= ceiling` and the newest window intact |
| 3 | Tool output older than the keep window is cleared on every step, not only at the ceiling | Ledger test: a session whose tool output alone passes the keep window clears the oldest entries and keeps the newest; pinned entries never move |
| 4 | The request prefix does not change between two steps of one turn | A scripted provider records both `CompletionRequest`s; the second's messages begin with the first's, byte for byte |
| 5 | The cache hypothesis is settled by evidence, not asserted | A `--debug-model` capture of two consecutive real requests, diffed; the finding is written into the implementation report either way |
| 6 | Replay of `e289jjjd` shows a bounded context | Offline replay (no model calls): P95 per-request input ≤ 45k, total input ≤ 2.6M (baseline 3.03M) |
| 7 | The person can see the session's usage | Status line shows cumulative input/output tokens and cache-hit ratio; `/cost` keeps "价格未知" for unpriced models |
| 8 | No behaviour change when nothing is configured | A short session under the keep window emits no new events and sends the same messages as today |

## 3. Non-goals

- **Subagents / task isolation.** The structural fix for the 32-call
  investigation is a separate spec: a child context that returns a summary. It
  changes tools, permissions, protocol and the session tree; it is not folded
  into a budget change.
- **A new truncation policy for tool output.** The 20 KB cap is unchanged. This
  spec ages what is already in context; it does not shrink what enters it.
- **`read_file` offsets/limits.** Useful, but an interface change to the tool.
- **Prices for subscription plans.** k3 is sold as a coding plan, not per
  token; a per-token guess would make `/cost` confidently wrong. Unknown stays
  unknown.
- **Changing the memory model (L1–L4).** The map moves in time, not in kind.
- **Mid-turn memory freshness.** After this change the L4 map is refreshed at
  turn boundaries, not every step. That is the trade the cache needs.

## 4. Design

### 4.1 A default budget when none is set

`input_ceiling` gains a third source, used only when `budget_tokens` is `None`:

```
default_budget(window) = clamp(window / 4, 32_000, 100_000)
```

| Window | Default budget | Ceiling at `compact_at_percent = 80` |
|---|---|---|
| 1,048,576 (k3, deepseek-flash) | 100,000 | 80,000 |
| 200,000 | 50,000 | 40,000 |
| 131,072 | 32,768 | 26,214 |
| 32,768 | 32,000 | 16,384 (window reserve wins) |
| none declared | none (unchanged) | window only |

Precedence is unchanged: an explicit `budget_tokens` from any layer wins; the
default is a floor under the window, never an override. `context_explain`'s
`budget_tokens` reports the **effective** budget so the F2 panel and
`/context` can explain it; `loongagent config show` continues to report what the
layers set, and names the derived default in the same line.

The constant is a safety net, not the active control: §4.2 keeps the context
well below it in normal work. It exists so a 1M-token window stops being a
licence to grow to 1M.

### 4.2 Aging old tool output every step

New `Ledger::tool_output_tokens()` sums entries with `category == History`,
`source` starting `tool:`, and `cleared == false`.

At every step boundary, before `compact_if_needed`:

```
if ledger.tool_output_tokens() > keep_newest_tokens {
    cleared = ledger.clear_tool_outputs(keep_newest_tokens, marker);
    if !cleared.cleared.is_empty() {
        emit compaction.applied {
            tokens_before, tokens_after,
            kept_tokens: keep_newest_tokens,
            cleared_tool_outputs: n,
            dropped_messages: 0,
            summary: "aged N tool output(s) older than the newest K tokens: A -> B",
        }
    }
}
```

- **What is cleared:** only non-pinned `History` entries whose source starts
  `tool:`. The map, the system prompt, instructions, user and assistant
  messages are untouched. The newest entry is always kept
  (`clear_tool_outputs` already guarantees this).
- **Marker:** the existing `[output of <source> cleared to save context — re-run
  the command if you need it again]`, unchanged; a model that needs the output
  asks for it again.
- **Event:** `compaction.applied`, which already carries `cleared_tool_outputs`
  and is already replayed by `Kernel::replay_compaction`. No schema change.
- **Default keep:** `keep_newest_tokens` is already 20,000 in
  `KernelConfig::new`; the aging threshold is that same number, so one key
  controls both the ceiling-triggered and the proactive path.
- **Why this and not a lower ceiling:** the ceiling triggers once and then stops
  growing; aging triggers repeatedly and keeps the steady state flat. On
  `e289jjjd` the tool output alone accounts for most of the 32k→61k growth in
  the dexer loop, so aging is the mechanism that pays.

### 4.3 A prefix that does not change inside a turn

Split `refresh_map` into two calls with different frequencies:

| Call | Does | When |
|---|---|---|
| `refresh_memory` | `ContextEngine::refresh` — deterministic sync/digest, wakes the worker | every step (unchanged) |
| `seat_map` | reads `map_text`, replaces or seats the pinned L4 `System` message | turn start, immediately before the first request; and again after a `compaction.applied` |

`run_inner` stops calling `refresh_map` inside the loop. The map is seated once
before the loop and re-seated after compaction. Between two model calls of one
turn the `messages` array is therefore append-only: the only changes are the
assistant message and tool result appended at the end, which is exactly the
shape prefix caching needs.

Consequences, stated so they are not discovered later:

- The map can be up to one turn stale. Memory enrichment already defers while
  a turn is busy (`Foreground`), so the map's content rarely changed mid-turn
  anyway; this makes the behaviour explicit.
- A compaction that clears output keeps the map (it is pinned), so the prefix
  after it is stable again until the next compaction.
- `--debug-model` captures are the evidence for criterion 5: two consecutive
  requests from one turn, diffed. If the map turns out not to be the variable,
  the capture names the real one and the finding goes into the implementation
  report before any further change.

### 4.4 What the person sees

- **Status line:** cumulative session `in / out` and cache-hit ratio, from the
  same `cost.recorded` stream the spend summary already consumes.
- **`/cost`:** unchanged sections plus one line for the cache ratio; unpriced
  models keep "价格未知" and show tokens first.
- **F2 context panel:** `budget_tokens` is the effective budget from §4.1, and
  the category list shows `History` shrinking when aging runs.

### 4.5 One step, in order

```
apply_requested_permissions
if interrupted -> stop
refresh_memory                     (deterministic only)
age_tool_outputs                   (new, §4.2)
compact_if_needed                  (existing, now has a reachable ceiling)
if this is the turn's first step or compaction applied: seat_map   (§4.3)
build CompletionRequest (messages + tools)
route, call, record cost, run tools, append results
```

### 4.6 Compatibility

- **Old logs replay unchanged:** no new event kind, no new field. An
  `e289jjjd`-era session has no `compaction.applied`, so replay reproduces it
  exactly.
- **New logs replay exactly:** aging emits the existing event, and
  `replay_compaction` (`crates/kernel/src/turn/session.rs`) already applies
  `kept_tokens` to `clear_tool_outputs`.
- **Config layers unchanged:** the default budget is derived, not written;
  `narrow-only` merging is untouched.
- **Managed profiles:** a pack that sets `budget_tokens` still wins; a pack that
  wants no default can set a very large budget explicitly.

## 5. Verification

| Check | Where |
|---|---|
| `default_budget` formula, clamp edges, explicit-wins | `cargo test -p loong-kernel turn::tests` |
| Aging clears oldest tool output, keeps newest, never pinned | `cargo test -p loong-kernel turn::tests` plus a `ledger` unit test |
| Compaction event carries `kept_tokens` and `cleared_tool_outputs`; replay reproduces it | `cargo test -p loong-kernel turn::tests` |
| Two-step prefix is append-only | a scripted provider test in `turn::tests` |
| Effective budget appears in `context_explain` | `cargo test -p loong-kernel turn::tests` |
| Status line and `/cost` show the ratio | `cargo test -p loong-tui-app` |
| No new events on a short session | `cargo test -p loong-kernel turn::tests` |
| The whole gate | `./scripts/check.sh` |

**Offline replay of `e289jjjd`** is a manual verification, not a committed
fixture: the session is a person's log, not test data. The procedure is a small
script that reads the log, rebuilds the ledger, applies §4.1–§4.2 per step, and
prints P95 and total input. It is run once and its numbers recorded in the
implementation report.

**Probe (criterion 5)** is the first implementation task, not a later one: run a
two-step turn with `--debug-model` and diff the two raw requests. If the only
difference is the appended exchange, the map was not the invalidator and the
cache section is revised before code is written.

**Owner documents.** `docs/design/context-ledger.md` §3–§4 gains the derived
default and the aging rule; `docs/05-parity-and-synthesis.md` §A2's T-25 row
changes only after the probe result is known, so the table says what happened
rather than what was hoped.

## 6. Risks

- **Aging clears something the model needs.** It keeps the newest 20k and says
  how to get the rest; the cost is a re-run, not a wrong answer. If real sessions
  show re-read churn, the keep goes up, not the trigger down.
- **The map is stale for a long turn.** Accepted; enrichment already defers to
  the foreground, and the map is re-seated after compaction.
- **The default budget truncates a genuinely large task.** A person or a pack
  can raise it; `context_explain` makes the number visible rather than silent.
- **The cache hypothesis is wrong.** The probe runs first. If the prefix was
  already stable, the spec still delivers §4.1–§4.2 and reports the cache
  finding without claiming a fix.
- **Event volume.** Aging emits at most one event per step that clears
  something; in a steady state that is rare. A pathologically chatty session
  can be bounded by the keep window.

## 7. Alternatives considered

- **Only lower the ceiling (no aging).** Fires once, then the context sits at
  the ceiling forever; the dexer loop would have stayed at 60k. Rejected.
- **A much lower default budget (32k for every model).** More savings, but
  truncates work that legitimately needs the context; 100k with 20k aging is the
  safer split.
- **Provider-specific `cache_control` breakpoints.** The OpenAI-compatible
  adapter has no such field, and Kimi/DeepSeek cache automatically. Prefix
  stability is the portable lever; markers would be a second, provider-specific
  one.
- **Subagents now.** The right structural answer, but a different spec and a
  much larger surface (§3).

## 8. Open questions

- Whether k3's coding endpoint caches message prefixes at all, or only the
  system block — the probe answers it; criterion 4 is under our control either
  way, criterion's 70 % target is a live-session expectation, not a unit test.
- Whether 100k is the right cap after a week of real sessions; it is one
  constant, and the replay procedure makes re-evaluating it cheap.
- Whether the effective default should be reported by `config show` as a real
  value or as "derived"; §4.1 picks "derived in the same line" and a review can
  change it before implementation.
