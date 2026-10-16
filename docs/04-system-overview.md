# 04 — System overview (the architecture as decided)

- **Status:** Agreed (derived — reflects ADR-0001…ADR-0018)
- **Last updated:** 2026-10-16

This is the **one-read picture** of loongagent as it stands. It is *derived*
from the decision record: [03-architecture.md](03-architecture.md) holds the
question-by-question Q&A, and [`decisions/`](decisions/) holds the ADRs. If this
document ever disagrees with an ADR, **the ADR wins** — fix this document.

---

## 1. What we are building

**loongagent** is a coding agent assembled from the strongest parts of Claude
Code, Codex, OpenCode, Cursor, Pi, and the DeepSeek-style open-model harnesses —
combining what can be combined, and making **explicit trade-offs** where the
sources contradict.

- **Firm-internal first.** Profiles and policy are org-managed; the control
  plane runs the **same code** whether managed or self-hosted.
- **All surfaces, one brain.** TUI, web, and IDE (via ACP) are thin clients of a
  single local agent server. Build order: TUI → web → ACP.
- **All engineering roles.** Frontend, backend, DevOps, QA, data — plus the
  manager and PM who need to see cost and progress, and the newcomer who wants
  a browser.
- **Name:** 龙 (loong) — a creature composed of the strongest parts of many
  animals; the design philosophy in one word.

**Non-goals:** not "another Claude Code / Codex / Cursor"; not an editor fork;
not a local-only toy (there is a small control plane); not a shared-session
collaboration platform (see §6).

---

## 2. The shape

```
  ┌───────┐  ┌───────┐  ┌─────────┐
  │  TUI  │  │  Web  │  │ IDE/ACP │     thin clients: render events, send intents,
  └───┬───┘  └───┬───┘  └────┬────┘     zero agent logic (ADR-0009)
      └──────────┴─────┬─────┘
                       │ JSON-RPC 2.0 — unix socket locally, WebSocket remotely (ADR-0002)
             ┌─────────▼──────────┐
             │  LOCAL AGENT       │  per user, on the engineer's machine, next to the
             │  SERVER (kernel)   │  code: model+tool loop, event log, context ledger,
             │  Rust              │  PDP, sandbox, retrieval, session logs
             └───┬────────────┬───┘
                 │            │
   events/stats  │            │  model calls (metered)
   (opt-in,      │            │
   allow-listed) │            │
       ┌─────────▼──────────┐ │
       │  CONTROL PLANE     │ │      identity/auth; policy & profile packs;
       │  Rust, shared      │ │      model gateway (keys, metering, shared cache);
       │  crates, own bin   │ │      opt-in aggregate audit/statistics (ADR-0012)
       │  (ADR-0012)        │ │
       └─────────┬──────────┘ │
                 │            │
                 └───────┬────┘
                 ┌───────▼────────┐
                 │ MODEL PROVIDERS│
                 └────────────────┘
```

**One artifact, many faces:** the single `loongagent` binary *contains* the
local agent server. `loongagent web` serves the web UI; ACP to editors is
decided, not yet built. Installing loongagent means getting that binary
(ADR-0009).

**Two binaries, one repository** (ADR-0012). The local agent and the control
plane are **separate binaries** with genuinely different jobs and deployment —
they are *not* one binary under two flags. What they **share** is the code where
disagreement would be a bug: the protocol/event schema, the pack envelope and
its sign/verify path, policy and config resolution, provider adapters, and token
cost math. Managed and self-hosted control planes are, by contrast, *literally
the same code* — that is a different claim, and the two are easy to conflate.

**Two properties worth remembering:** code never transits the control plane,
and the local server is fully functional offline — the control plane only makes
it smarter, never alive (ADR-0001, ADR-0012).

---

## 3. What the kernel owns (and why nothing else may)

Four things are **fixed and non-pluggable**, because behavior that can be
swapped can hide (ADR-0004):

1. **The model + tool loop** — minimal, Pi-grade: few tools, tiny prompt, no
   hidden context.
2. **The event log** — JSONL is the source of truth; SQLite is a derived,
   rebuildable index. Two tree levels, never conflated: a session **contains**
   an event tree (`id`/`parentId`, paths, a head), and sessions relate to each
   other by **lineage** (fork, delegation); "branch" means a child session
   (ADR-0003, ADR-0015). Every tool call, approval, policy decision, and cost is
   an event.
3. **Context assembly** — the **context ledger is the only write path** to the
   model. Plugins, MCP servers, skills, and subagents may *propose* entries;
   none may append directly (ADR-0006).
4. **The PDP** — policy decisions happen in the kernel at the single tool
   chokepoint, returning allow / ask / deny **plus the rule that fired**;
   enforcement lives at the OS (ADR-0007) — Landlock on Linux today; seatbelt
   and Job Objects are decided, not yet built. `bubblewrap` is detected and
   named by `sandbox self-test` but not applied, and without Landlock that
   self-test reports `UNAVAILABLE`.

**Everything else is an edge:** provider adapters, protocol transformers, tools
(built-ins included), hooks (shell / HTTP / MCP), routing strategies, WASM
extensions, markdown **skills** — commands are manually-invoked skills, one
concept and one loader — and configuration packs (ADR-0004).

---

## 4. Every decision, in one table

| AQ | Decision | ADR |
|---|---|---|
| AQ-1 | One binary, always client/server; sessions live in the server | [0001](decisions/0001-one-binary-client-server.md) |
| AQ-2 | JSON-RPC 2.0, bidirectional, schema-first; ACP is a dialect | [0002](decisions/0002-json-rpc-wire-protocol.md) |
| AQ-3 | JSONL event log as truth + derived SQLite index; one event stream | [0003](decisions/0003-jsonl-truth-sqlite-index.md) |
| AQ-4 | Microkernel: pluggable edges, tiny fixed kernel; WASM for deep extensions | [0004](decisions/0004-microkernel-extension-model.md) |
| AQ-5 | Deliberately small control plane: identity, packs, gateway, opt-in stats | [0012](decisions/0012-control-plane.md) |
| AQ-6 | Routing: **local decides live**, server learns and ships policy packs | [0005](decisions/0005-routing-philosophy.md) |
| AQ-7 | Context ledger only; per-profile budgets; visible compaction; memory = propose-and-confirm. Layered reversible memory (optional, opt-in) | [0006](decisions/0006-context-engine.md), [0017](decisions/0017-layered-context.md) (Proposed) |
| AQ-8 | PDP in the kernel, PEPs at the OS; per-action autonomy ladder; approvals are events | [0007](decisions/0007-permission-sandbox-engine.md) |
| AQ-9 | Monotonic config layers; TOML vs markdown; one signed pack envelope | [0008](decisions/0008-configuration-layering.md) |
| AQ-10 | Local parallelism: worktree isolation, artifact-returning subagents, one orchestrator | [0011](decisions/0011-local-parallelism.md) |
| AQ-11 | One artifact many faces; no single-surface capability; one writer + N observers | [0009](decisions/0009-surfaces.md) |
| AQ-12 | Rust kernel/servers, TypeScript web client (`ratatui`, `wasmtime`) | [0013](decisions/0013-implementation-language.md) |
| T-14 | Team collaboration = git-shared knowledge, **not** shared live sessions | [0010](decisions/0010-team-collaboration-knowledge-not-sessions.md) |
| — | Session export/import: file-based continuity, never sync | [0014](decisions/0014-session-export-import.md) |
| — | Two tree levels: a session contains an event tree; sessions have lineage | [0015](decisions/0015-two-tree-levels.md) |
| T-07 | Pack distribution: one signed envelope (Ed25519), key pinned out of band; a verified pack is the locked `control` layer | [0016](decisions/0016-pack-distribution.md) |
| — | Plugins: install content by name, seat what a plugin asks for, **execute nothing** — hooks stay a per-repository gate | [0018](decisions/0018-plugin-install-and-bootstrap.md) |

---

## 5. The guarantees, and what enforces them

| Promise | Enforced by |
|---|---|
| **Nothing hidden** — every token, decision, and cost is inspectable | Context ledger (ADR-0006) + "the rule that fired" (ADR-0007) + everything is an event (ADR-0003) |
| **Cost is visible before it is spent** | Per-category token counts + the request's token floor (ADR-0006), fed to routing (ADR-0005); cost-affecting changes are breaking (ADR-0009) |
| **Prompts are not permissions** | Decision in the kernel, enforcement at the OS; a sandbox bug cannot be talked away (ADR-0007) — Landlock on Linux today, seatbelt and Job Objects decided, not yet built |
| **Shared knowledge cannot escalate privilege** | Content keys are add-only; permissions, budgets, and autonomy are narrow-only and org-owned (ADR-0008, ADR-0010) |
| **Offline is never dead** | Cached packs; the local brain decides every live call (ADR-0005, ADR-0008, ADR-0012) |
| **Private by default** | Uplink is an allow-list; session content is not in the contract; approvals, policy, and credentials never travel (ADR-0012, ADR-0014) |
| **Parallel work is verifiable** | Write-capable subagents run in their own git worktree, so "it passed" reflects their own tree (ADR-0011) |
| **You can change your mind cheaply** | Sessions are trees; branches are the unit of accept/reject; compaction is visible and the log is lossless (ADR-0003, ADR-0006) |

---

## 6. What we deliberately do **not** do

Each of these was decided, not forgotten:

- **No multi-user live sessions** — no co-watching, co-steering, or cross-user
  session viewing. Collaboration is git-shared rules and knowledge (ADR-0010).
- **No session sync service** — the backend never holds session state.
  Continuity is a **file you move**: export/import (ADR-0012 §6, ADR-0014).
- **No scheduler, no cross-user or scheduled orchestration, no cloud runners** —
  no use case yet. Local orchestration is the parent agent's visible plan
  (ADR-0011).
- **No desktop app and no CI surface** — dropped as product surfaces; a headless
  mode may exist internally (ADR-0009).
- **No editor fork** — be the agent other editors talk to, via ACP (ADR-0009).
- **No vector DB in v1** — structural retrieval first: grep/symbols → repo map →
  explore subagent, all entering through the ledger (ADR-0006).
- **No hidden injections, ever** — if it cannot be described as a ledger entry,
  it cannot reach the model (ADR-0006).
- **No merge** — with no sync, there is nothing to merge, so there is no merge
  to get wrong (ADR-0014).

---

## 7. Trust, in one paragraph

Nothing leaves your machine except three things: **commits you make**, the
**opt-in, allow-listed** event/statistics uplink, and **model calls that were
going to a provider anyway**. Configuration resolution, permission decisions,
context assembly, and the session log all live locally; the server cannot see
your code, and the PDP re-decides every action on every machine — an imported
session file carries no approvals, no policy, and no credentials (ADR-0007,
ADR-0008, ADR-0012, ADR-0014). The one residual concentration point is the
**model gateway** (centralized keys, metering, shared cache), which is visible
and avoidable by self-hosting (T-03).

---

## 8. Portability and continuity

- **Sessions are files** (JSONL + derived index), so moving one is a file copy,
  not a service call.
- **`loongagent session export/import` is decided, not yet built** (ADR-0014).
  The decision: one session's **complete** event log, its **ancestor** sessions
  (the lineage that makes it meaningful), a manifest (workspace path, git
  revision, profiles, plugin/MCP inventory), and optionally file-history
  snapshots — then **re-binds** the workspace on import and reports mismatches
  instead of enforcing them. **Descendant sessions are not included by
  default**; `--with-branches` adds the whole subtree. The parent's context
  already holds each subagent's summary, which is what continuation needs.
- **Roaming is per machine** across that machine's surfaces (ADR-0009);
  cross-machine continuity is explicit export/import, never automatic.
- **The code travels through git**, not through session bundles.
- **Team knowledge travels through git too** — rules, skills, specs, ADRs, and
  signed org packs (ADR-0008, ADR-0010).

---

## 9. Where to read what

The index for every document is [docs/README.md](README.md). For the project
outside `docs/`, start from the root [loongagent README.md](../README.md) and
[DEVELOPMENT.md](../DEVELOPMENT.md).

**Rule of the docs:** they describe the *current* shared understanding; history
lives in git. Nothing is "Agreed" until the owner signs off in conversation.

---

## 10. Status and next step

**Design: complete.** All twelve architecture questions are decided and recorded
in the ADRs; the topics backlog remains open as product-level work.

**Implementation: built.** The build order this document used to state as a plan
is now history — all three steps are behind us:

1. **Scaffolding** — the cargo workspace and the one protocol schema that
   generates both the Rust and the TypeScript types (ADR-0002, ADR-0013).
2. **Kernel skeleton** — the minimal model+tool loop, the JSONL event log with
   its derived index, and the context ledger (ADR-0003, ADR-0004, ADR-0006).
3. **The first vertical slice** — TUI to local server to a provider adapter,
   ending at the sandbox and PDP boundary (ADR-0007, ADR-0009).

What stands on it: the six built-in tools, hooks (shell / HTTP / MCP), MCP over
stdio, skills, commands and plugins, four surfaces (`tui`, `web`, `repl`,
`run`), streaming answers, and optional layered memory — on Linux and Windows,
with the same command surface. What is still missing is named in the README's
status section, and the topics backlog is where the product-level work remains.
