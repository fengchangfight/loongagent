# ADR-0015 — Two levels of tree: sessions have lineage, a session contains an event tree

- **Status:** Accepted
- **Date:** 2026-10-05
- **Decides:** the vocabulary and the boundary between node-level and
  session-level branching. Clarifies ADR-0003 and fixes wording in ADR-0009,
  ADR-0011, and ADR-0014

## Context

The word "tree" was doing two different jobs in the design docs, and that
ambiguity was load-bearing. ADR-0003/T-12 (following Pi) describe a session as
one append-only JSONL whose entries carry `id`/`parentId` — a tree **inside** a
file. ADR-0011 describes subagents as "branches in the session tree", and
ADR-0014 speaks of ancestor and descendant **sessions** — a tree **between**
files. Both models are reasonable; holding both without naming them is not. The
ambiguity decided real questions by accident: how finely ADR-0009's one-writer
rule applies, what unit ADR-0011's worktree isolation and garbage collection
operate on, and what "including descendants" means in an export (ADR-0014).

The two levels **can** coexist, and they should: high-frequency refinement of
one line of work should not create new lifecycle objects, while genuinely
parallel or delegated work needs its own.

## Decision

### The two levels, named

**Level 1 — the event tree, inside a session.**

- A **session** is one append-only JSONL file (ADR-0003) and the single unit of:
  attach and write token, export/import, permission and sandbox binding, cost
  attribution, worktree assignment, and listing.
- Inside it, entries carry `id`/`parentId`, forming the session's **event tree**.
  A **path** is a root-to-leaf chain in that tree; the **head** is the path
  currently being continued.
- **Retry** (a new approach from an earlier point) creates a **new path** in the
  same session; the abandoned path is retained and described by a branch
  summary (Pi's design). Steer-vs-follow-up changes *when* input is delivered,
  not the tree. Compaction rewrites the context, never the history, so it
  branches nothing.
- A session's event tree contains **only its own events**.

**Level 2 — session lineage, between sessions.**

- A session may have a **parent session**, recorded on the child with the origin
  kind (`fork` or `delegation`) and the point in the parent's event tree it came
  from.
- A **branch** is a **child session** and its descendants. The word *branch* is
  reserved for this level and is never used for a path inside a session.
- **Only three things create a session:** **fork** (a deliberate new line of
  work), **delegation** (a subagent), and **import** (a new local session,
  possibly with `importedFrom`). Retry, steer, follow-up, compaction, a model or
  profile change, and a permission-mode change do **not**.

### The boundary test

**A new session is created when the new work needs its own lifecycle** — its own
write token, its own worktree, its own export/GC unit, its own cost and
permission attribution. Retry fails that test: it is the same line of work, in
the same workspace, under the same person at the same head. Fork and delegation
pass it: they can proceed in parallel, be handed off, or be discarded
independently.

### Child sessions reference the parent's prefix; they do not copy it

A child session does **not** duplicate the parent's events. It references the
point it forked or delegated from, and its effective context is assembled by
stitching that ancestor prefix. Parent events are append-only and never
rewritten (ADR-0003), so the reference is stable by construction.

This is why ADR-0014 treats **ancestor sessions as mandatory** in an export:
without them a child session is not merely unexplained, it is **inoperable**.
Descendants remain optional because the parent already holds each child's
structured summary (ADR-0006/ADR-0011).

### Vocabulary (use these words, and only these)

| Term | Meaning |
|---|---|
| **session** | one JSONL event log = one line of work = the unit of attach, export, permissions, cost, worktree, listing |
| **event tree** | the `id`/`parentId` structure **inside** one session |
| **path** | a root-to-leaf chain in a session's event tree |
| **head** | the path currently being continued (where the writer is) |
| **session lineage** | the parent/child relation **between** sessions |
| **branch** | a child session and its descendants — **never** a path inside a session |
| **fork** | the act of creating a child session as a new line of work |
| **delegation** | the act of creating a child session as a subagent |

"Tree" without a qualifier is no longer used in these docs.

## Consequences

- **ADR-0009's one-writer rule is per session.** The write token belongs to the
  session; the head is simply where its writer currently is. Observers watch the
  head's stream; takeover is an explicit event.
- **ADR-0011's worktree and GC unit is the write-capable child session**
  (delegation or fork). Retry stays in the current workspace, because it is
  sequential within one line of work — the isolation cost is spent only where
  parallel writers actually exist.
- **ADR-0014 is now unambiguous:** a bundle = one session's **complete event
  tree** + its **ancestor sessions**; descendants excluded by default. Because
  export carries the event tree whole, trimming it is a different feature
  (pruning), never an export option.
- **The UI has two levels to render:** session lineage (a tree of sessions) and,
  within a session, its paths with the current head. Pi's single-file `/tree`
  view becomes a two-level navigator — accepted, as the price of clean lifecycle
  units.
- **Session files are no longer self-contained**, but the dependency is
  explicit, acyclic by construction, and travel-ready: exporting a child drags
  its ancestor chain, and the chain is immutable.
- **A session can be rebuilt from its chain**, which makes verification cheap:
  replaying the ancestry plus the session's event tree must reproduce the
  session's context exactly (ADR-0006's ledger).
