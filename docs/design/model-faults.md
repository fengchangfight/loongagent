# Model faults: a hiccup, a size refusal, a refusal

- **Up:** [design/README.md](README.md) · [kernel-loop.md](kernel-loop.md) §7
- **Authority:** [ADR-0004](../decisions/0004-microkernel-extension-model.md) · [ADR-0006](../decisions/0006-context-engine.md)
- **Code:** `crates/providers/src/lib.rs` (`ModelError`), `crates/kernel/src/turn/loop.rs` (`complete_with_ladder`)
- **Verify:** `cargo test -p loong-providers` · `cargo test -p loong-kernel` · `./scripts/check.sh`

A model call fails in three shapes. The loop answers each one differently, and the
whole design rests on telling them apart: retrying a refusal wastes the person's
time, and failing on a hiccup throws away work that is already done.

| Shape | How it looks | What the loop does |
|---|---|---|
| **Hiccup** | `HTTP 500` with `internal_error`, `HTTP 429`, connection reset, an EOF mid-stream | Retries the same request after 300 ms, then 900 ms, then fails the turn |
| **Size refusal** | `context_length_exceeded`, "prompt is too long", `413` | Descends the context ladder: compaction, then the oldest exchanges ([kernel-loop.md](kernel-loop.md) §6) |
| **Refusal** | `400`, `401`, `422`, or any `Protocol` error | Fails the turn at once |

## 1. The line: did anything get recorded

A **hiccup** means the request never produced an answer — no message, no tool call,
no event. Asking again is therefore asking once, not asking twice, and the turn
keeps what it has already paid for.

That is not a hypothetical. The report that produced this page: a person approved an
irreversible tool, the tool ran and its result was recorded, the request that
resumed the turn came back

```
approval.respond: model: transport: HTTP 500: {"type":"internal_error", ...}
```

and the task ended there. A file had changed, a prompt had been answered, and the
person was left with half a turn and an error naming the method that happened to
carry it — `approval.respond` rather than `prompt.submit`, because that is the
request the model call was made inside.

The error's `request_id` came from the provider, which is the tell: the request
arrived and the far side failed, so the request was fine. A malformed request would
answer `400`, `401` or `422`. Those stay fatal, because refusing again is the only
thing they can do. A `Protocol` error is the third kind: an answer arrived and could
not be used, and repeating the request earns the same answer.

## 2. How many, and how long

Two retries, at 300 ms then 900 ms (`TRANSIENT_BACKOFF`). Both numbers are a
compromise between two failures: retrying too eagerly piles load on a provider that
is already struggling, and sleeping too long makes a turn that is working look hung.
The schedule stops at 1.2 seconds in total.

Each attempt and its outcome go to stderr, which the TUI and the web surface both
capture as diagnostics, so a turn that survives a hiccup is visible afterwards
without an event of its own.

A retried call may repeat the preview deltas the first attempt already sent. Deltas
are a **preview, never the record** ([kernel-loop.md](kernel-loop.md) §2): the answer
that ends up in the log is sent once, so a duplicated fragment in the preview is a
cosmetic cost, not a corrupted transcript.

## 3. What this does not do

- **It does not retry a refusal.** A bad key, a bad parameter and a request the
  provider will not take are all final.
- **It does not retry across a compaction.** A size refusal is a different fix
  (shed context), and mixing the two would retry a 500 with a smaller request for no
  reason.
- **It does not cancel a turn that is waiting out a backoff.** The pause is at most
  900 ms, so an interrupt lands within that window rather than immediately.
- **It does not fix a provider that is down for a minute.** Two retries ride out a
  hiccup; an outage still ends the turn, and the person can send it again. The tool
  results are in the log, so the second attempt does not repeat their side effects.
