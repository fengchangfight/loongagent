# Cost accounting: provider-reported tokens and an estimated bill

- **Up:** [design/README.md](README.md) · [04 System overview](../04-system-overview.md)
- **Authority:** [ADR-0003](../decisions/0003-jsonl-truth-sqlite-index.md) · [ADR-0005](../decisions/0005-routing-philosophy.md)
- **Code:** `crates/providers/src/lib.rs`, `crates/providers/src/openai_compat.rs`, `crates/config/src/pricing.rs`, `crates/kernel/src/turn.rs`, `crates/tui/src/lib.rs`, `crates/tui-app/src/spend.rs`, `crates/schema/protocol.toml`
- **Verify:** `cargo test -p loong-providers -p loong-config -p loong-kernel -p loong-tui -p loong-tui-app` · `./scripts/check.sh`

The session's token count is what the **provider says it billed**, not what the
sender estimated. That distinction is the whole design: a local character-count
estimate answers "how big is my context?", while a bill answers "what did the
model actually read and write?". This document owns the second question. The
first belongs to [context-ledger.md](context-ledger.md).

## 1. The source of truth is the reply's `usage`

Every OpenAI-compatible reply carries a `usage` object. `OpenAiCompat` parses it
twice — once for a non-streaming reply, once for the final chunk of a streaming
one — and the streaming parser keeps the last value it saw, because a streaming
server normally sends `usage` only at the end. `Usage` therefore has four facts:

| Field | Wire source | Meaning |
|---|---|---|
| `tokens_in` | `usage.prompt_tokens` | all prompt tokens, cached or not |
| `tokens_out` | `usage.completion_tokens` | generated tokens, reasoning tokens included |
| `cache_hit_tokens` | `usage.prompt_cache_hit_tokens`, else `usage.prompt_tokens_details.cached_tokens` | prompt tokens the provider served from its prompt cache |
| `cache_miss_tokens` | `usage.prompt_cache_miss_tokens` | prompt tokens the provider had to read |

The two cache fields are `Option<u64>`. `None` means the provider did **not
report** cache use, which is different from "it reported zero". A surface that
prints 0% for an unreported cache is lying, and `/cost` says `缓存未报告`
instead. DeepSeek reports hit and miss separately; OpenAI-style servers report
only `cached_tokens`, so the miss count is derived as `tokens_in - hit`. When
neither key exists, the call is priced as all-miss.

A provider is not asked for a tokenizer. The provider is the only party that
knows what it charged, and adding a local tokenizer would create a second,
disagreeing count. This is why `Usage` is carried on `CompletionResponse` and
written to the log rather than recomputed at render time.

## 2. One event per model call

`Kernel::record_cost` runs after every successful completion and appends one
`cost.recorded` event. The event is the durable record a resumed session reads,
so it carries everything a later reader needs without a live configuration:

| Field | Meaning |
|---|---|
| `profile` | the configuration profile that was running |
| `model` | the profile slot that answered (the name the model bar shows) |
| `tokens_in`, `tokens_out` | the provider's counts |
| `cache_hit_tokens`, `cache_miss_tokens` | the provider's cache split; both 0 when unreported |
| `cache_reported` | whether the provider reported cache use at all |
| `usd_micros` | estimated cost in micro-USD at the price in force |
| `price_basis` | `local-free`, `list-peak`, `list-off-peak`, or `unknown` |

`price_basis` is part of the event on purpose. A later reader must be able to
tell a genuinely free local call from an unpriced remote one; both have
`usd_micros = 0`, and treating them as the same fact would silently understate
a bill. `cache_miss_tokens` always satisfies
`cache_hit_tokens + cache_miss_tokens == tokens_in` when the provider was
consistent; the event uses the provider's numbers, not a re-derived estimate.

The event is append-only like every other event. A price correction does not
rewrite history: old calls keep the price that was used when they were made, and
a new table only prices new calls.

## 3. The price table

`crates/config/src/pricing.rs` holds the published list prices as integer
micro-USD per million tokens. Integer arithmetic keeps a session total exact to
the micro-dollar and avoids float drift across thousands of calls.

DeepSeek's published prices, checked against
[Models & Pricing](https://api-docs.deepseek.com/quick_start/pricing/) on
2026-10-27:

| Model | Cache hit (peak / off-peak) | Cache miss (peak / off-peak) | Output (peak / off-peak) |
|---|---|---|---|
| `deepseek-flash` | $0.006 / $0.003 | $0.30 / $0.15 | $1.20 / $0.60 |
| `deepseek-v4-pro` | $0.044 / $0.022 | $1.32 / $0.66 | $3.96 / $1.98 |

Peak hours are 01:00–04:00 and 06:00–10:00 UTC, Monday through Friday. The
estimate computes the window from the call's UTC time and picks the matching
half. Chinese public holidays are not in the table, so a holiday call inside a
peak window is estimated at the peak price — the conservative direction. A
session that spans a peak boundary therefore sums two bases, and `/cost` says so.

`deepseek-flash` also answers to the retired names `deepseek-v4-flash` and
`deepseek-v4-flash-vision-exp`, because the vendor still accepts them and bills
them at the Flash price. A model the table does not know is `unknown`: the token
counts are still shown, the cost is not invented.

**Local is free.** A provider whose catalog entry has `local = true` (Ollama
today) or a `base_url` on loopback (`localhost`, `127.0.0.1`, `::1`) is
`local-free` with `usd_micros = 0`. This is a fact about where the computation
runs, not a discount: no vendor bill exists to estimate.

## 4. What the TUI shows

Two surfaces read the same events:

- **The usage row.** A fixed row under the prompt shows the session so far as
  `用量  入 1,053 · 出 5 · 缓存 80% · 上下文 62%`. The cache part disappears when
  no call reported it, and the row is absent until the first call. The context
  share is the last call's `tokens_in` over the effective budget from the most
  recent `ContextExplain`; when no budget is known the row shows tokens
  (`上下文 62,000`) rather than a percentage with no denominator. It does not
  scroll with the page and it stays while a turn runs, so the running total is
  always in the same place. The per-call `cost.recorded` event is not drawn in the
  transcript: one line per model call crowded the conversation, and the event
  is still in the log for `/cost` and a resumed session to read.
- **`/cost`.** The command sums every `cost.recorded` event the session has:
  calls, prompt and completion tokens, the cache hit rate over calls that
  reported cache use, the estimated total, and one row per model. On attach the
  totals are seeded from `session.replay`, so a resumed session starts with its
  past bill rather than zero; a new session resets them.

The sum is accumulated in `loong-tui-app/src/spend.rs` from the events, never
from rendered text or from the ledger's estimate. The context share in the row is
the last call's `tokens_in`, not the ledger's `context_tokens`: the provider's
count is what the window actually carried, while the ledger estimates the next
request before it is sent.

## 5. Limits worth stating

- The bill is an **estimate**, not an invoice. It uses public list prices and the
  provider's own token counts; account discounts, taxes, gateway markups and
  provider-side rounding are outside it.
- An unknown model is reported as unknown. Adding a vendor means adding one table
  row plus a test, not a second cost pipeline.
- The protocol is additive: older `cost.recorded` events lack the new fields and
  are read as `cache_reported = false`, `price_basis = "unknown"` — their token
  counts still add up, and no old log is rewritten.
- `/cost` covers the **turn loop's** model calls. The optional context summariser
  calls a local Ollama model from its own background worker; those tokens are not
  `cost.recorded` events, because they are not part of the conversation's prompt
  and no vendor bills them. Its calls are still visible in the context panel's
  worker counters.
