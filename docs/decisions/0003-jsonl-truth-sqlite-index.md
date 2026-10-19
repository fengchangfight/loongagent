# ADR-0003 — JSONL event log as source of truth, derived SQLite index

- **Status:** Accepted
- **Up:** [../design/README.md](../design/README.md)
- **Date:** 2026-09-30
- **Decides:** AQ-3 (docs/03-architecture.md)

## Context

Sessions are trees (T-12: fork/retry/steer/pick), audit and statistics (T-02)
must be nearly free, and our core promise is "nothing hidden, everything
inspectable — documented session format" (T-13). Pi proved the model: a
session is an append-only JSONL file where entries carry `id`/`parentId`,
making the tree inherent; compaction is lossless because the full history
survives. But fast queries — tree navigation, full-text search, statistics —
do not want to scan JSONL files.

## Decision

**Hybrid storage:**

- **JSONL is the source of truth.** One append-only file per session; every
  entry is a typed event (message, tool call, approval, cost, policy decision,
  compaction, …) with `id`/`parentId`, forming that session's **event tree**.
  The file is human-readable, diffable, exportable, and its format is
  **documented and versioned**.
- **Two tree levels, never conflated** (ADR-0015): the `id`/`parentId`
  structure lives **inside** a session (paths, and the current head), while
  relations **between** sessions (fork and delegation lineage) are a separate
  level. A child session **references** its parent's prefix rather than copying
  it, so a session file is not self-contained — its ancestor chain is part of
  what it needs, and that chain is immutable by construction.
- **Every event carries a forward-compatibility envelope** (added 2026-10-05).
  Each record declares whether it is **`ignorable`** — whether a reader that
  does not understand its `type` may safely skip it. The rule is two-sided:
  **readers skip unknown ignorable types** (never fail, never mis-render), and
  **writers classify every event type**. Core conversation events (message, tool
  call/result, approval, policy decision, compaction, cost) are **not**
  ignorable: a reader that cannot understand one must fail loudly rather than
  silently display a wrong session. This is what lets modules version
  independently and add event types without breaking older readers — the same
  "add-only" discipline ADR-0008 applies to content layers. Why it is not
  optional: in the DeepSeek-Harness ecosystem the missing marker forced two real
  plugins to keep their audit writes disabled/fail-closed
  ([research/dsh-plugin-ecosystem.md](../research/dsh-plugin-ecosystem.md) §5, G8).
- **SQLite is a derived index** — rebuildable and disposable — powering tree
  queries, search, and local statistics. It may be deleted at any time; it
  rebuilds from the JSONL.
- **Audit events and session events are one stream.** The same JSONL shape is
  what the firm control plane's aggregator (T-05) consumes during sync.
- The server (ADR-0001) owns writes: append with write-ahead discipline,
  crash-safe, replayable on restart.

## Alternatives considered

- **JSONL only (Pi).** Rejected: statistics and tree queries degrade to file
  scans; T-02's dashboards deserve a real query layer.
- **SQLite as source of truth.** Rejected: an opaque binary database fights
  the inspectability principle, and JSONL export would become a lossy
  afterthought.

## Consequences

- The event-tree UI (T-12), the statistics engine (T-02), and the context
  inspector (T-13) all read the same event vocabulary — define the event
  schema early and treat it as a public, versioned contract.
- Sync to the firm control plane (AQ-5) is batched event upload of an already
  well-defined stream.
- Index corruption is a non-event: rebuild from truth.
