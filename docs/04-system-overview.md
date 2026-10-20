# 04 — System overview (the architecture as decided)

- **Up:** [00-vision.md](00-vision.md)
- **Authority:** [ADR-0004](decisions/0004-microkernel-extension-model.md)
- **Code:** `crates/kernel/src/lib.rs`
- **Verify:** `./scripts/check.sh`

This is the **one-read picture** of loongagent as it stands. It is *derived* from
the decision record: [03-architecture.md](03-architecture.md) holds the
question-by-question Q&A, and [`decisions/`](decisions/) holds the ADRs. If this
document ever disagrees with an ADR, **the ADR wins** — fix this document.

---

## 1. What we are building

**loongagent** is a coding agent assembled from the strongest parts of Claude
Code, Codex, OpenCode, Cursor, Pi and the DeepSeek-style harnesses — combining
what can be combined, with explicit trade-offs where the sources contradict. It
is **firm-internal first** (org-managed profiles and policy; the control plane is
the same code managed or self-hosted), **all surfaces, one brain** (TUI, web and
IDE/ACP are thin clients of one local server; build order TUI → web → ACP), and
**all engineering roles**. **龙 (loong)** — the strongest parts of many animals in
one creature — is the design philosophy in one word.

**Non-goals:** not another Claude Code / Codex / Cursor, not an editor fork, not
a local-only toy, not a shared-session collaboration platform (§6).

---

## 2. The shape

```
  ┌───────┐  ┌───────┐  ┌─────────┐
  │  TUI  │  │  Web  │  │ IDE/ACP │   thin clients: render events, send intents,
  └───┬───┘  └───┬───┘  └────┬────┘   zero agent logic (ADR-0009)
      └──────────┴─────┬─────┘
                       │ JSON-RPC 2.0 (ADR-0002)
             ┌─────────▼──────────┐
             │  LOCAL AGENT       │  model+tool loop, event log, context ledger,
             │  SERVER (kernel)   │  PDP, sandbox, retrieval, session logs
             └───┬────────────┬───┘
                 │            │
   events/stats  │            │  model calls (metered)
   (opt-in)      │            │
       ┌─────────▼──────────┐ │
       │  CONTROL PLANE     │ │  identity; packs; gateway (keys, metering,
       │  Rust, shared      │ │  shared cache); opt-in stats (ADR-0012)
       │  crates, own bin   │ │
       └─────────┬──────────┘ │
                 └───────┬────┘
                 ┌───────▼────────┐
                 │ MODEL PROVIDERS│
                 └────────────────┘
```

**One artifact, many faces:** the single `loongagent` binary *contains* the local
agent server; `loongagent web` serves the web UI, and ACP to editors is decided,
not yet built (ADR-0009).

**Two binaries, one repository** (ADR-0012): the local agent and the control
plane are separate binaries with different jobs and deployment, not one binary
under two flags. What they share is the code where disagreement would be a bug —
protocol/event schema, pack envelope and sign/verify, policy and config
resolution, provider adapters, token cost math. Managed and self-hosted control
planes are, by contrast, literally the same code.

**Two properties:** code never transits the control plane as files, and the local
server is never dead — **local mode** runs with no control plane at all, while
**gateway mode** keeps tools, sandbox, log and resume alive offline but needs the
gateway for model calls (ADR-0005, ADR-0012).

---

## 3. What the kernel owns (and why nothing else may)

Four things are **fixed and non-pluggable**, because behavior that can be swapped can hide (ADR-0004):

1. **The model + tool loop** — minimal, Pi-grade: few tools, tiny prompt, no
   hidden context.
2. **The event log** — JSONL is the truth, SQLite a derived, rebuildable index.
   A session **contains** an event tree (`id`/`parentId`, paths, a head);
   sessions relate by **lineage** (fork, delegation), and "branch" means a child
   session (ADR-0003, ADR-0015). Every tool call, approval, decision and cost is
   an event.
3. **Context assembly** — the ledger is the only write path to the model, and
   only the kernel appends to it; plugins, MCP servers, skills and subagents have
   no proposal door yet (ADR-0006).
4. **The PDP** — decisions happen in the kernel at the single tool chokepoint,
   returning allow / ask / deny plus the rule that fired; enforcement lives at
   the OS (ADR-0007) — Landlock on Linux today, seatbelt and Job Objects decided
   but not built. Without Landlock, `sandbox self-test` reports `UNAVAILABLE`
   and names `bubblewrap` without applying it.

**Everything else is an edge:** provider adapters, transformers, tools (built-ins
included), hooks (shell / HTTP / MCP), WASM extensions, markdown **skills**
(commands are manually-invoked skills, one concept and one loader) and
configuration packs (ADR-0004).

---

## 4. Every decision, in one table

| AQ | Decision | ADR |
|---|---|---|
| AQ-1 | One binary, always client/server; sessions live in the server | [0001](decisions/0001-one-binary-client-server.md) |
| AQ-2 | JSON-RPC 2.0, bidirectional, schema-first; ACP is a dialect | [0002](decisions/0002-json-rpc-wire-protocol.md) |
| AQ-3 | JSONL event log as truth + derived SQLite index; one event stream | [0003](decisions/0003-jsonl-truth-sqlite-index.md) |
| AQ-4 | Microkernel: pluggable edges, tiny fixed kernel; WASM for deep extensions | [0004](decisions/0004-microkernel-extension-model.md) |
| AQ-5 | Deliberately small control plane: identity, packs, gateway, opt-in stats | [0012](decisions/0012-control-plane.md) |
| AQ-6 | Routing **follows the keys**: local mode = manual picker, no router; gateway mode = the gateway picks, meters and refuses | [0005](decisions/0005-routing-philosophy.md) |
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

The detail behind AQ-2 and AQ-3 is in [events-storage.md](design/events-storage.md)
and [sessions-lineage.md](design/sessions-lineage.md); behind AQ-4 in
[extension-model.md](design/extension-model.md); behind AQ-6 and AQ-9 in
[config-routing.md](design/config-routing.md); and behind AQ-11 and AQ-5 in
[surfaces.md](design/surfaces.md) and [control-plane.md](design/control-plane.md).

## 5. The guarantees, and what enforces them

| Promise | Enforced by |
|---|---|
| **Nothing hidden** — every token, decision and cost is inspectable | Context ledger (ADR-0006) + "the rule that fired" (ADR-0007) + everything is an event (ADR-0003) |
| **Cost is visible before it is spent** | Per-category token counts + the request's token floor (ADR-0006), fed to routing where there is one (gateway mode, ADR-0005); cost-affecting changes are breaking (ADR-0009) |
| **Prompts are not permissions** | Decision in the kernel, enforcement at the OS; a sandbox bug cannot be talked away (ADR-0007) — Landlock today, seatbelt and Job Objects decided, not built |
| **Shared knowledge cannot escalate privilege** | Content keys are add-only; permissions, budgets and autonomy are narrow-only and org-owned (ADR-0008, ADR-0010) |
| **Offline is never dead — per mode** | Local mode: no control plane at all. Gateway mode: cached packs, tools/sandbox/log/resume local; model calls need the gateway, by design (ADR-0005, ADR-0008, ADR-0012) |
| **Private by default** | Uplink is an allow-list; session content is not in the contract; approvals, policy and credentials never travel (ADR-0012, ADR-0014) |
| **Parallel work is verifiable** | Write-capable subagents run in their own git worktree, so "it passed" reflects their own tree (ADR-0011) |
| **You can change your mind cheaply** | Sessions are trees; branches are the unit of accept/reject; compaction is visible and the log is lossless (ADR-0003, ADR-0006) |

## 6. What we deliberately do **not** do

The decisions this project has made to refuse something — each with its reason
and ADR — are kept together in [05-parity-and-synthesis.md](05-parity-and-synthesis.md)
§D, so a decision already made is not re-argued from a shorter copy of itself.

## 7. Trust, in one paragraph

Nothing leaves your machine except three things: **commits you make**, the
**opt-in, allow-listed** event/statistics uplink, and **model calls** — through
the gateway in gateway mode, straight to your own provider with your own key in
local mode. Configuration resolution, permission decisions, context assembly and
the session log stay local; the PDP re-decides every action on every machine, and
an imported session carries no approvals, policy or credentials (ADR-0007,
ADR-0008, ADR-0012, ADR-0014). **What the gateway sees is what a provider would
see:** model prompts, in gateway mode only — never the session log, tool calls,
or anything the model was not shown. The **model gateway** is the one residual
concentration point (keys, metering, shared cache), visible and avoidable by
self-hosting or local mode (ADR-0005, T-03).

## 8. Portability and continuity

- **Sessions are files** (JSONL + derived index), so moving one is a file copy,
  not a service call. `loongagent session export/import` is decided, not yet
  built (ADR-0014): one session's complete event log, its ancestors, a manifest
  and optional file-history snapshots, re-binding the workspace on import and
  reporting mismatches; descendants come only with `--with-branches`.
- **Roaming is per machine** across its surfaces (ADR-0009); cross-machine
  continuity is explicit export/import, never automatic. **Code and team
  knowledge travel through git** — rules, skills, specs, ADRs and signed org
  packs (ADR-0008, ADR-0010).

## 9. Where to read what

The index for every document is [docs/README.md](README.md); outside `docs/`,
start from the root [README.md](../README.md) and [DEVELOPMENT.md](../DEVELOPMENT.md).
Docs describe the current shared understanding; history lives in git.
