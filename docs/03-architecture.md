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
connects to a per-user local server (unix socket / named pipe), and the TUI is
just a client. Sessions live in the server, so they survive TUI detach and
re-attach from web/IDE. In-process-by-default and always-on-daemon were rejected
as retrofits that make multi-surface attach a special case; the reasons are in
the ADR.

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

**Decision: JSONL is the truth; a SQLite index is derived.** One append-only
file per session carries `id`/`parentId`, so the tree is inherent, crash-safe,
human-readable and exportable. The index is rebuildable and disposable, and
exists for tree queries, search and local stats. Audit events and session events
are the same stream. SQLite-as-truth and JSONL-only were rejected: the first
fights inspectability, the second makes queries scan files.

**Constrains:** T-02 (audit/stats), T-12 (tree), T-13 (inspectability), backend
sync format.

### AQ-4 · Extension model — how deep do plugins go? — `Decided` → [ADR-0004](decisions/0004-microkernel-extension-model.md)

**Question:** what can extend loongagent, and in what execution model?

**Decision: a microkernel — pluggable edges, a fixed center.** Hooks, skills,
external tools and deep extensions are four different kinds: hooks are
shell/HTTP/MCP subprocesses behind one verdict contract; skills are markdown plus
frontmatter with commands merged in; MCP tools are subprocess JSON-RPC; deep
third-party extensions are WASM, sandboxed and language-neutral. First-party
batteries stay in-process. The kernel itself — model+tool loop, event append,
context assembly — is not a plugin point. Unchanged after the DeepSeek-harness
review: provider adapters, transformers and tools are edge plugins too, and
routing follows the keys (ADR-0005), not this layer.

**Constrains:** AQ-12 (language) most strongly; T-09/T-15/T-16/T-18 land here.

### AQ-5 · Backend control plane — shape and scope — `Decided` → [ADR-0012](decisions/0012-control-plane.md)

**Question:** what exactly lives server-side, and as what deployment shape?

**Decision (owner sign-off, 2026-10-04): a deliberately small backend.** One
backend binary, modular inside (gateway / dashboards & statistics / pack
distribution), no microservices; same repository as the local agent with shared
correctness-critical crates, built as a separate binary, deployable managed or
self-hosted with the same code (ADR-0012). Postgres for users, policies and
rollups; object storage for shared prompt-cache blobs and signed packs. Uplink is
an allow-list, opt-in by category: clients push event batches, the server pushes
only policy/profile/knowledge packs; session bodies and transcripts are not in
the contract, and offline stays fully functional. Identity is firm-internal
(SSO/service principal). Explicit non-jobs: session bus, knowledge server,
scheduler, session-state sync. Personal cross-device session sync is not built;
roaming stays per machine (ADR-0009).

**Constrains:** T-03 deployment shapes, T-05, T-08, enterprise/self-host story.

### AQ-6 · Model layer — abstraction, routing, execution point — `Decided` → [ADR-0005](decisions/0005-routing-philosophy.md)

**Question:** how do profiles, routing and provider protocols fit together?

**Decision: routing follows the keys.** The component holding the provider
credentials calls the provider and therefore picks the model. Provider adapters
cover the ~4 wire protocols behind one internal interface, with a transformer
chain that normalizes protocol drift and logs every downgrade; the per-model
capability registry is pure data. Two modes, one rule: **local mode** uses the
user's own credentials or local endpoint with no control plane and routing is
manual — the picker is the feature; **gateway mode** sends every model request to
the remote gateway, which holds the keys, picks per request within org policy
from the client's intent, meters, enforces hard caps by refusing, and shares the
prompt cache. Adapters run at the gateway in gateway mode and locally in local
mode — the same shared crate in two processes. Context assembly, the tool loop,
the PDP/sandbox and the session log stay local in both modes. Offline is
mode-specific (ADR-0005 §4); escalation (T-04) is gateway-mode policy, absent in
local mode.

**Constrains:** T-01, T-04, T-07; defines what "profile" means concretely.

### AQ-7 · Context engine — assembly, budget, retrieval — `Decided` → [ADR-0006](decisions/0006-context-engine.md)

**Question:** who assembles the model's context, and how is it governed?

**Decision (owner sign-off, 2026-10-04): the kernel alone assembles context;
nothing enters the model's context outside a typed, token-counted ledger entry.**
System prompt, tools, rules, memory, MCP tools, retrieval results and history are
all typed entries with provenance and cost (category, source+version, token
count, insertion seq, lifetime, pinned/evictable). Plugins, MCP, skills, hooks
and subagents may *propose* entries; none may append directly — there is no side
channel. Budget policy is per profile/model and fully visible, including the
request's token floor, which AQ-6's routing reads. Compaction is a visible event,
made reversible by the lossless event tree (ADR-0015). Retrieval is a kernel
service writing through the ledger. Memory (T-10) is propose-and-confirm:
low-risk project facts may auto-accept; user preferences require explicit
confirmation; confirmed memories are ordinary ledger entries and the write is an
event. Per-role context packs (T-06) are versioned ledger entries, not hidden
prompts.

**Constrains:** T-10, T-13, T-06 role packs; token cost floor (T-04).

### AQ-8 · Permission and sandbox engine — `Decided` → [ADR-0007](decisions/0007-permission-sandbox-engine.md)

**Question:** where do policy decisions happen, and what enforces them?

**Decision (owner sign-off, 2026-10-04): the decision lives in the kernel, the
enforcement lives at the OS.** The PDP sits in the fixed kernel at the single
tool chokepoint, returns allow/ask/deny and records the rule that fired; hooks
may advise or veto but never decide. PEPs are structural: the OS sandbox
(seatbelt / Landlock+bubblewrap / Job Objects), the tool runner and network
egress. Policy layers are built-in defaults < org packs < project < user <
session; org hard caps are enforced as `deny`. Autonomy is per action class, not
per tool; sandbox is default-on, approval policy is orthogonal, and YOLO is a
visible opt-out that is itself an event. Approvals and denials are first-class
events with a reason, replayable in the event tree. Repo hooks are untrusted until
a content-bound per-repo trust opt-in; Guardian-style LLM pre-review is v2 behind
the same interface.

**Constrains:** T-11, T-09, role autonomy (T-06), async runner safety (T8).

### AQ-9 · Configuration and profile layering — `Decided` → [ADR-0008](decisions/0008-configuration-layering.md)

**Question:** file formats, precedence and distribution of config?

**Decision (owner sign-off, 2026-10-04): one layered stack with a per-key merge
kind, so no lower layer can loosen a higher one.** Layers are built-in defaults <
org managed < project < user < session; the org layer arrives as a versioned,
signed pack. Merge kinds are *free-override* (lowest wins), *narrow-only*
(strictest wins; replacement is not a merge kind for lists), *add-only* (union,
with an evented disable-by-identity) and *reference* (secrets, endpoints).
Formats: TOML for machine-managed config, markdown + YAML frontmatter for
human-authored content, AGENTS.md native with a one-time import for
CLAUDE.md/.cursorrules. Every key can explain its winning layer and artifact.
Secrets live in the OS credential store or the gateway, never in config. All
packs — profiles, policy, role/context — share one signed envelope; unsigned
packs are allowed only at project/user layers.

**Constrains:** T-07, T-21 (what seniors can open), enterprise managed-config
story.

### AQ-10 · Team/async runtime — `Decided` → [ADR-0011](decisions/0011-local-parallelism.md) (local parallelism only; **cloud/cross-user/scheduled orchestration dropped 2026-10-04 — owner: no use case**, see [the discussion log](archive/03-architecture-history.md))

**Question:** how do parallel agents and worktrees compose locally?

**Decision (owner sign-off, 2026-10-04): local parallelism inside one user's
session.** Any write-capable subagent gets its own git worktree; read-only
research subagents share the workspace. A worktree isolates files, not external
resources — ports, databases, caches and locks still need declaration or a cap,
and a non-git workspace falls back to a temporary copy or serialization.
Subagents are child sessions linked by their parent tool call, returning a
structured artifact rather than a long transcript. There is one orchestrator: the
parent's visible, steerable plan — no separate scheduler and no second
decision-maker. Human ceiling 3–5 parallel agents with tiered review; v1 is local
worktrees + subagents only.

**Constrains:** T-17, T-20 (PR-as-deliverable); T-14 resolved as
knowledge-sharing rather than session-sharing (ADR-0010).

### AQ-11 · Surfaces — thin-client rule, build order, installation — `Decided` → [ADR-0009](decisions/0009-surfaces.md)

**Question:** what does each surface contain, how do users install it, and in
what order do we build?

**Decision (owner sign-off, 2026-10-04): one artifact, many faces.** The single
static binary contains the local agent server; TUI/web/IDE are clients of it or
modes of it (`loongagent web`, `loongagent acp`), with no per-surface install and
thin IDE wrappers. Desktop and CI are dropped. The hard thin-client rule: surfaces
render the event stream and send intents — zero agent logic client-side — and no
surface-only capability may exist; a surface may lag in exposing something but
never diverge. Build order is TUI → web → ACP, with `ratatui` from AQ-12 and an
independent TypeScript web stack served by the binary. Attach is one writer + N
observers per session, with explicit takeover recorded as an event. Distribution
is stable/LTS + canary, signature-verified like config packs; token-cost-affecting
changes are breaking. Browser-only users still drive their own local agent
server; code is always local.

**Constrains:** T-06 wow moments, beachhead sequencing, AQ-12.

### AQ-12 · Implementation language — `Decided` → [ADR-0013](decisions/0013-implementation-language.md)

**Question:** which language, now that the architecture is settled?

**Decision (owner sign-off, 2026-10-04): Rust for the kernel and servers,
TypeScript for the web client.** Rust wins on single-binary distribution,
per-OS sandbox FFI, WASM via `wasmtime`, performance at event-sourcing scale,
signed packs, concurrency and a shared workspace for both binaries; `ratatui`
follows for the TUI. The web surface is a thin TypeScript client built to static
assets, served by the same binary, with types generated from the protocol schema.
Rust's smaller contributor pool is the accepted cost, mitigated by a tiny fixed
kernel and an extension surface that needs no Rust.
