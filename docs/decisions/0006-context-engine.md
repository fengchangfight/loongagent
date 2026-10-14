# ADR-0006 — Context engine: kernel-assembled, ledger-visible, budgeted

- **Status:** Accepted
- **Date:** 2026-10-04
- **Decides:** AQ-7 context-engine question (docs/03-architecture.md), including
  the memory write path (T-10)

## Context

Context assembly is where every harness promise either becomes mechanically
true or stays a slogan. Two failure modes drove the question: hidden injection
(harnesses adding system-prompt text, tool results, or "reminders" the user
never sees — the thing Pi exists in reaction to) and unaccountable token spend
(context quietly ballooning until cost and latency are unexplainable, which is
T-04's concern expressed at the token level).

The constraints were already fixed by earlier decisions: AQ-4 (§ADR-0004) put
context assembly **inside the non-pluggable kernel** — if assembly were a
plugin point, behavior could hide, breaking T-13. AQ-3 (§ADR-0003) made the
event log the lossless source of truth, so compaction can destroy nothing
permanently. AQ-6 (§ADR-0005) made cost decisions (including routing) depend on
knowing what a request will actually cost, which requires the context to be
countable *before* it is sent.

## Decision

**The kernel alone assembles context. Nothing enters the model's context
outside a typed, visible, token-counted ledger entry.**

1. **Context ledger is the only write path.** Every block entering context —
   system prompt, tool definitions, rule files, memory, MCP tools, retrieval
   results, history, reminders — is a typed ledger entry. Plugins, MCP servers,
   skills, hooks, rule files, and subagents may *propose* entries; none may
   append directly. There is no side channel to the model.
2. **Every entry carries provenance and cost.** Each entry records at minimum:
   category, source (who/what produced it, with the version or path), token
   count, insertion sequence, lifetime (turn / session / persistent), and
   whether it is pinned or evictable. "Why is this in my context?" is always a
   query, never an inference — and the Cursor-style context-ring breakdown
   falls out of the same data for free.
3. **Budget policy is per profile/model and fully visible.** Caps per category,
   eviction order, and the token floor for a request. The floor is what AQ-6's
   routing needs; a profile can therefore be described partly by its context
   budget, not only by its model set.
4. **Compaction is a visible event, not a silent lossy step.** When budget
   pressure triggers compaction, it appears in the ledger and the event stream
   with what was summarized and why; the session's **event tree** (ADR-0003,
   ADR-0015) retains the
   lossless record, so compaction is reversible in principle.
   - **Ordering heuristic** (added 2026-10-05): compaction **first clears stale
     tool outputs** — usually the largest and least-referenced blocks — and only
     then summarizes the remainder. It **keeps the newest window verbatim**
     (default ≈20k tokens; a profile parameter, so the org may cap it and the
     user may tighten it, per ADR-0008's merge kinds). Both Claude Code and Pi
     converge on this order; dropping tool output before prose preserves working
     state at the lowest semantic cost.
   - **Revised by [ADR-0017](0017-layered-context.md) (Proposed, 2026-10-14)**:
     with the layered memory enabled, the second half — dropping the oldest whole
     exchanges — is **disabled**; the original stays in the log and `recall` brings
     it back. With the engine off this section stands exactly as written, and that
     equivalence is a control test rather than a promise.
5. **Retrieval is a kernel service that writes through the ledger.** The
   layered pipeline (grep/symbols → token-budgeted repo map → explore
   subagent) produces ordinary ledger entries. A retrieval result has no
   privileged status: it competes for budget like everything else and is
   visible like everything else.
6. **Memory writes are propose-and-confirm, not direct (T-10).** The agent
   proposes memories into a **staging area**. The model is allowed to
   auto-accept **low-risk project facts** ("this repo uses pnpm and vitest").
   Anything that encodes **user preferences, feedback, or identity** requires
   explicit confirmation before it becomes persistent. Confirmed memories are
   ledger entries like any other and stay inspectable, editable, and
   deletable; the write itself is an event, so memory is auditable and
   revertible.
7. **Per-role context packs (T-06) are versioned ledger entries.** A role's
   pack is a versioned bundle that enters through the same mechanism — a
   composition of profile + role pack, never a hidden per-role prompt.

## Consequences

- **T-13 becomes mechanical.** "Nothing hidden, everything inspectable" is
  enforced by construction: an entry that cannot be described in the ledger
  cannot reach the model.
- **Cost is knowable before the call.** Per-category token counts plus the
  floor make budget enforcement and AQ-6 routing decisions well-founded rather
  than post-hoc.
- **The extension surface grows one documented API (proposal), not a
  back door.** AQ-4's plugin authors get a ledger-proposal interface; the
  kernel keeps the write.
- **Memory has a trust ladder.** Project facts stay frictionless; preference
  capture pays a confirmation cost — deliberately, since a wrong persistent
  preference is expensive to notice and cheap to prevent.
- **Compaction stops being scary.** Because the tree is lossless and the event
  is visible, aggressive context hygiene is safe, which is what "be mean with
  context" requires.
- **Deferred to later discussion:** the concrete ledger schema, per-category
  default caps per profile, eviction algorithm details, how role packs are
  versioned and distributed (rides on AQ-9 config layering and the AQ-5 control
  plane), memory decay/TTL and cross-session memory for async workers (T-10),
  and the exact auto-accept rule set for low-risk project facts.
