# The token statistics page

- **Up:** [web-surface.md](web-surface.md)
- **Authority:** [ADR-0009](../decisions/0009-surfaces.md) · [cost-accounting.md](cost-accounting.md)
- **Code:** `crates/web/src/stats.rs`, `crates/web/src/app.rs`, `crates/web/src/app.js`, `crates/web/src/index.html`, `crates/web/src/style.css`
- **Verify:** `cargo test -p loong-web` · `./scripts/check.sh`

What the page answers when someone asks "how many tokens did this cost me": one view
over the whole machine, cut three ways — by model, by workspace, by session. The numbers
are the ones the session already wrote down; nothing here is counted twice or estimated
again. [web-surface.md](web-surface.md) §5 owns the socket method this view rides; this
document owns the view.

This is a **first version on purpose**. The shape below is the one that took an hour, not
the one that ends the argument: no date filter, no chart, no cache. Iterating it means
changing this document with it.

## 1. One question, three cuts

The page is a view switch in the banner, not a fourth region of the chat: `统计` swaps the
main area for the table, `会话` swaps it back. The chat keeps its state — the rail, the
open session and its turn are untouched — because looking at the bill is not leaving the
conversation.

The view holds one headline row (calls, in, out, cache rate, cost) and three tables:

- **按模型** — the profile slot that answered (`data.model`, cost-accounting §2). This is
  the cut that makes a bill readable: one row per model, so a cheap one doing most of the
  work is visible next to an expensive one doing little.
- **按工作区** — the directories the sessions belong to, §3 below.
- **按会话** — the sessions themselves, spendiest first, each with its title so a row can
  be recognised without reading an id.

Every table is sorted by cost, then by tokens, descending: the question the page exists
for is "what is spending", so the answer is at the top. The currency is `usd_micros` as
recorded; the page prints dollars.

## 2. Where the numbers come from

`cost.recorded` is the only writer of token counts (`crates/schema/protocol.toml`): the
provider's `tokens_in`, `tokens_out`, the `cache_hit_tokens` / `cache_miss_tokens` split
with its `cache_reported` flag, `usd_micros` at the price then in force, and `price_basis`.
The view sums those events and never recomputes a price — a bill is the record of what was
charged, not a second guess at it (`cost-accounting.md` §2).

`stats.usage` walks `<root>/projects/*/*.jsonl` itself, in one pass per log, skipping
`history.jsonl` as the rail does. It reads the log and not an index, for the same reason
`session.replay` does: the log is the truth, and an index can be missing. A line that does
not parse is one lost event, not a lost session.

Three consequences, stated rather than hidden:

- **No cache.** Every open of the view re-reads every log. A resumed session writes more
  and a cached number would lie; the cost is a wait on the first open of a large history,
  which is the trade this version makes.
- **All time.** There is no window to choose: the numbers run from the first logged call to
  the last, and the view prints that range so a total is never mistaken for "this month".
- **Cache rate is a ratio of reported numbers.** It is `cache_hit / (cache_hit +
  cache_miss)` over the calls that reported a split; calls with `cache_reported: false`
  are counted separately (`cache_reported_calls`), so a provider with no cache never reads
  as a 0% hit rate.

## 3. Which workspace a session belongs to

The log records a slug (`projects/<slug>/`), not a path, so the view names a workspace
twice over:

1. **Registered** — a workspace in `workspaces.json` matches a slug through
   `sessions::session_dir`, and the row takes the directory's own name.
2. **Everything else** — a directory whose slug belongs to no registered workspace takes
   the `cwd` the session's `session.start` recorded, basename only; a log with no start
   event falls back to the slug with its hash trimmed.

So the numbers cover the **whole home**, not only what this tab has open: a session the
TUI wrote yesterday is in the total, and it is in the total under the directory it ran in.
The alternative — counting only the seats the page knows — would answer "how much did this
tab cost", which nobody asks.

## 4. What the page does not say

No per-day series, no per-tool or per-turn split, no budget or projection, and no write of
any kind: this page is a reader. Cost is `usd_micros` as recorded, and a row's cost says
which kind of zero it is: a row whose calls were all local prints `免费`, a row whose calls
all had no price prints `—`, and the totals count the two apart (`local_free_calls`,
`unpriced_calls`) instead of flattening them into one suspicious `$0`
(`cost-accounting.md` §2). The title on a session row is the first line of the first user
message, cut to 48 characters; a session with no user message shows its id.
