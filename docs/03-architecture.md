# 03 — Architecture (question-driven design)

- **Up:** [04-system-overview.md](04-system-overview.md)
- **Authority:** the ADRs in [decisions/](decisions/); this file is the question map that points at them
- **Code:** `docs/03-architecture.md`, `docs/archive/03-architecture-history.md`
- **Verify:** `./scripts/check.sh`

This document designs the loongagent architecture through numbered **architecture
questions** (`AQ-N`). An `AQ` is the open question; the matching **ADR** is the
record of the answer (AQ-1 → ADR-0001). The options, the trade-offs and the
discussion live in the ADRs and the archived discussion — this file keeps each
question, the decision in a few lines, and what it constrains.

The **implementation language (AQ-12) fell out last**, as a consequence of the
architecture rather than a precondition: Rust for the kernel and servers,
TypeScript for the web client (ADR-0013).

---

## The decision map (AQ-1 … AQ-12)

Ordered by dependency: each decision constrains the ones below it. The pre-AQ
framing and the discussion log are kept, as history rather than guidance, in
[archive/03-architecture-history.md](archive/03-architecture-history.md).

"Server" means two different machines doing different jobs: the **local** agent
server (AQ-1…AQ-4, AQ-6…AQ-11) and the **firm** control plane (AQ-5). The
canonical diagram is in [04 §2](04-system-overview.md#2-the-shape).

Key property: **code never transits the control plane as files.** Tool execution
and file access stay local; the only things flowing up are opt-in, allow-listed
audit events and aggregate statistics (ADR-0012) and, in gateway mode, model
calls that go to providers through the gateway (local mode calls the user's own
endpoint with the user's own key — ADR-0005). There is no cloud/async runner
tier: it was dropped for lack of a use case (2026-10-04).

### AQ-1 · Runtime topology — how do client and core meet? — `Decided` → [ADR-0001](decisions/0001-one-binary-client-server.md)

**Question:** what is the process shape of the local agent server?

**Decision: one binary, always client/server.** The `loongagent` CLI spawns or
connects to a per-user local server, and the TUI is just a client. Sessions live
in the server, so they survive detach and re-attach. In-process and daemon
variants were rejected as retrofits (ADR-0001).

**Constrains:** AQ-2 (protocol), AQ-3 (storage ownership), all surfaces.

### AQ-2 · Wire protocol and API shape — `Decided` → [ADR-0002](decisions/0002-json-rpc-wire-protocol.md)

**Question:** what do clients and server speak?

**Decision: JSON-RPC 2.0, bidirectional, schema-first.** Server→client events are
notifications; transports are stdio/unix socket locally and WebSocket remotely.
ACP is itself JSON-RPC, so the IDE adapter becomes a dialect of our protocol
rather than a second stack. REST+SSE and gRPC were rejected for weaker
bidirectional streaming and worse editor/browser support.

**Constrains:** client SDK count, ACP adapter effort, third-party clients.

### AQ-3 · Session and event storage — `Decided` → [ADR-0003](decisions/0003-jsonl-truth-sqlite-index.md)

**Question:** how are sessions, their event trees, events and audit data persisted?

**Decision: JSONL is the truth; a SQLite index is derived.** One append-only file
per session carries `id`/`parentId`, so the tree is inherent, crash-safe and
exportable; the index is rebuildable and serves tree queries, search and stats.
Audit and session events are one stream (ADR-0003).

**Constrains:** T-02 (audit/stats), T-12 (tree), T-13 (inspectability), backend
sync format.

### AQ-4 · Extension model — how deep do plugins go? — `Decided` → [ADR-0004](decisions/0004-microkernel-extension-model.md)

**Question:** what can extend loongagent, and in what execution model?

**Decision: a microkernel — pluggable edges, a fixed center.** Hooks, skills, MCP
tools and deep extensions are the four kinds: subprocesses behind one verdict
contract, markdown with commands merged in, subprocess JSON-RPC, and sandboxed
WASM; first-party batteries stay in-process. The kernel loop, event append and
context assembly are not plugin points (ADR-0004).

**Constrains:** AQ-12 (language) most strongly; T-09/T-15/T-16/T-18 land here.

### AQ-5 · Backend control plane — shape and scope — `Decided` → [ADR-0012](decisions/0012-control-plane.md)

**Question:** what exactly lives server-side, and as what deployment shape?

**Decision (owner sign-off, 2026-10-04): a deliberately small backend.** One
binary, modular inside, sharing correctness-critical crates with the local agent,
deployable managed or self-hosted with the same code (ADR-0012). It owns identity,
signed packs, the model gateway and opt-in statistics; session bodies, a session
bus, a scheduler, session-state sync and cross-device sync are non-jobs.

**Constrains:** T-03 deployment shapes, T-05, T-08, enterprise/self-host story.

### AQ-6 · Model layer — abstraction, routing, execution point — `Decided` → [ADR-0005](decisions/0005-routing-philosophy.md)

**Question:** how do profiles, routing and provider protocols fit together?

**Decision: routing follows the keys.** The component holding the provider
credentials calls the provider and picks the model: **local mode** uses the user's
credentials or endpoint and a manual picker; **gateway mode** sends requests to the
gateway, which holds the keys, picks within org policy, meters, enforces caps and
shares the cache. Adapters are one shared crate in both processes (ADR-0005).

**Constrains:** T-01, T-04, T-07; defines what "profile" means concretely.

### AQ-7 · Context engine — assembly, budget, retrieval — `Decided` → [ADR-0006](decisions/0006-context-engine.md)

**Question:** who assembles the model's context, and how is it governed?

**Decision (owner sign-off, 2026-10-04): the kernel alone assembles context;
nothing enters the model outside a typed, token-counted ledger entry.** Plugins,
MCP, skills, hooks and subagents may only *propose* entries; budget, compaction,
retrieval and memory writes are visible ledger events, with memory
propose-and-confirm (ADR-0006, ADR-0015).

**Constrains:** T-10, T-13, T-06 role packs; token cost floor (T-04).

### AQ-8 · Permission and sandbox engine — `Decided` → [ADR-0007](decisions/0007-permission-sandbox-engine.md)

**Question:** where do policy decisions happen, and what enforces them?

**Decision (owner sign-off, 2026-10-04): the decision lives in the kernel, the
enforcement at the OS.** The PDP at the single tool chokepoint returns
allow/ask/deny plus the rule that fired; PEPs are the OS sandbox, the tool runner
and network egress. Org hard caps are `deny`, approvals are events, and hooks are
advisory and trust-gated (ADR-0007).

**Constrains:** T-11, T-09, role autonomy (T-06).

### AQ-9 · Configuration and profile layering — `Decided` → [ADR-0008](decisions/0008-configuration-layering.md)

**Question:** file formats, precedence and distribution of config?

**Decision (owner sign-off, 2026-10-04): one layered stack with a per-key merge
kind, so no lower layer can loosen a higher one.** Layers are built-in < org <
project < user < session; merge kinds are free-override, narrow-only, add-only and
reference, with secrets only as references and one signed envelope for every pack
(ADR-0008).

**Constrains:** T-07, T-21 (what seniors can open), enterprise managed-config
story.

### AQ-10 · Team/async runtime — `Decided` → [ADR-0011](decisions/0011-local-parallelism.md) (local parallelism only; **cloud/cross-user/scheduled orchestration dropped 2026-10-04 — owner: no use case**, see [the discussion log](archive/03-architecture-history.md))

**Question:** how do parallel agents and worktrees compose locally?

**Decision (owner sign-off, 2026-10-04): local parallelism inside one user's
session.** Write-capable subagents get their own git worktree; subagents are child
sessions returning structured artifacts; one orchestrator is the parent's visible
plan. Human ceiling 3–5, local worktrees only — cloud, cross-user and scheduled
orchestration are out of scope until a use case appears (ADR-0011, ADR-0010).

**Constrains:** T-17, T-20 (PR-as-deliverable); T-14 resolved as
knowledge-sharing rather than session-sharing (ADR-0010).

### AQ-11 · Surfaces — thin-client rule, build order, installation — `Decided` → [ADR-0009](decisions/0009-surfaces.md)

**Question:** what does each surface contain, how do users install it, and in
what order do we build?

**Decision (owner sign-off, 2026-10-04): one artifact, many faces.** One static
binary contains the local server; TUI/web/IDE are thin clients or modes of it, with
no per-surface install and no surface-only capability. Build order is TUI → web →
ACP; attach is one writer + N observers, and updates are signed like packs
(ADR-0009).

**Constrains:** T-06 wow moments, beachhead sequencing, AQ-12.

### AQ-12 · Implementation language — `Decided` → [ADR-0013](decisions/0013-implementation-language.md)

**Question:** which language, now that the architecture is settled?

**Decision (owner sign-off, 2026-10-04): Rust for the kernel and servers,
TypeScript for the web client.** Rust wins on single-binary distribution, per-OS
sandbox FFI, WASM via `wasmtime`, performance at event-sourcing scale and a shared
workspace for both binaries; the web client is a thin TypeScript layer served by
the same binary, types generated from the protocol schema (ADR-0013).
