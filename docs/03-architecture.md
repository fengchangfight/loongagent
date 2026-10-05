# 03 — Architecture (question-driven design)

- **Status:** Agreed — AQ-1…AQ-12 all decided (see table below and `decisions/`)
- **Last updated:** 2026-10-04

This document designs the loongagent architecture **iteratively, through
structured architecture questions, numbered `AQ-N`** — **AQ = architecture
question** — discussed with the owner. Method: each AQ lists the decision, the
options, the constraints our principles impose, and a recommended lean. Nothing
here is "Decided" until the owner signs off in conversation; locked decisions
graduate to `decisions/` ADRs. So **AQ-N is an open question; the matching ADR
is the record of its answer** (e.g. AQ-1 → ADR-0001).

When the AQs are answered, the **implementation language (AQ-12) falls out
last** — it is a consequence of the architecture, not a precondition. *(Done
2026-10-04: Rust kernel/servers + TypeScript web client — ADR-0013.)*

---

## Part 1 — Foundations (historical framing, 2026-09-30)

These twelve items were the pre-AQ framing, imported from vision and rounds 1–2
of research before the decision map existed. They are kept as the record of
*why* the questions were asked. The **current, authoritative** statement of each
is in the ADRs on the right; where an item was superseded rather than refined,
it says so.

| # | Framing item (as written 2026-09-30) | Where it lives now |
|---|---|---|
| 1 | Client/server core from day one (T2) | ADR-0001 — one binary, always client/server |
| 2 | Minimal Pi-grade loop inside the server (T1) | ADR-0004 — tiny fixed kernel: loop, event append, context assembly |
| 3 | Sessions are trees (T-12) | ADR-0003 (`id`/`parentId` in one JSONL) + the T-12 feature spec (fork/retry/steer/pick) |
| 4 | Nothing hidden, everything inspectable (T-13) | ADR-0006 (context ledger) + ADR-0007 (the rule that fired) + ADR-0003 (every decision is an event) |
| 5 | Sandbox default-on, approval policy separate, YOLO opt-out (T3/T-11) | ADR-0007 — PDP in the kernel, PEPs at the OS |
| 6 | Model profiles, not model pickers (T-07) | ADR-0005 (local decides, server learns) + ADR-0008 (org layer, signed packs) |
| 7 | Cost-conscious by design (T-04) | ADR-0005 (escalation as an auditable event) + ADR-0006 (token floor) + ADR-0009 (cost-affecting changes are breaking) |
| 8 | Control plane ≠ data plane (T-05) | ADR-0012 (a deliberately small backend) + ADR-0010 (knowledge via git; no shared sessions) |
| 9 | ACP server-side, no editor fork (T-19) | ADR-0009 (one artifact, many faces) + ADR-0002 (ACP as a dialect) |
| 10 | Layered retrieval, no vector DB in v1 (T7) | ADR-0006 — retrieval enters through the context ledger |
| 11 | Sync-first, async-ready (T8) | **Partially superseded:** local worktree isolation → ADR-0011; cloud/async and cross-user/scheduled orchestration are **out of scope** (AQ-10 decision, 2026-10-04) |
| 12 | Junior-proof defaults, senior-proof depth (T-21) | ADR-0007 (per-action ladder defaults) + ADR-0008 (layered config with monotonicity) + ADR-0009 (YOLO as a visible opt-out) |

For the end-to-end picture of the system **as decided**, read
[04-system-overview.md](04-system-overview.md). This document keeps the
question-by-question record.

---

## Part 2 — The decision map (AQ-1 … AQ-12)

Ordered by dependency. Each AQ: the question, options, lean, what it constrains.

### The two-server topology

"Server" is two different machines doing different jobs: the **local** agent
server (AQ-1…AQ-4, AQ-6…AQ-11) and the **firm** control plane (AQ-5). The
canonical, up-to-date diagram lives in
[04-system-overview.md](04-system-overview.md#2-the-shape); the version below is
the 2026-09-30 original, kept for the record, with the two items retired since
then marked:

```
 ┌───────┐ ┌───────┐ ┌────────┐ ┌────────────┐
 │  TUI  │ │ Web   │ │IDE/ACP │ │ ~~CI/headless~~│  surfaces — thin clients (AQ-11);
 └───┬───┘ └───┬───┘ └───┬────┘ └────┬───────┘  CI dropped as a product surface
     └─────────┴────┬────┴───────────┘
                   │ JSON-RPC — unix socket locally, WebSocket remotely (AQ-2)
          ┌────────▼─────────┐
          │ LOCAL AGENT      │  per-user, on the engineer's machine, next to
          │ SERVER           │  the code: session logs, tool loop, sandbox,
          │ (data plane)     │  JSONL truth, retrieval. AQ-1 … AQ-4.
          └────────┬─────────┘
                   │ smart sync — events/stats up, policy & profiles down
                   │ (batch, delta, resumable; offline = degraded, never dead)
          ┌────────▼─────────┐
          │ FIRM CONTROL     │  deliberate scope (ADR-0012): auth (T-08), profile
          │ PLANE            │  & policy packs (ADR-0008), model gateway (keys,
          │ (control plane)  │  metering, shared cache), opt-in aggregate stats.
          │                  │  ~~team sharing~~ (ADR-0010) ~~orchestration~~ (AQ-10)
          └────────┬─────────┘  Self-hosted or managed — identical control-plane
                   │             code either way; its own binary (ADR-0012).
                   │ metered model calls only
          ┌────────▼─────────┐
          │ MODEL PROVIDERS  │
          └──────────────────┘
```

Key property: **code never transits the control plane.** Tool execution and
file access stay local; the only things that flow up are audit events and
aggregate statistics (opt-in, visible, allow-listed — ADR-0012) and the model
calls themselves, which go through the gateway to providers anyway. There is no
cloud/async runner tier: it was dropped for lack of a use case (2026-10-04), so
the old "code lives on the runner" exception no longer applies.


### AQ-1 · Runtime topology — how do client and core meet? — `Decided` → [ADR-0001](decisions/0001-one-binary-client-server.md)

**Question:** what is the process shape of the **local** agent server (the
data-plane box in the topology above)? Scope note: this AQ is only about the
per-user machine; the firm-wide control plane is AQ-5.

- **A) One binary, always client/server (Codex app-server model).** The
  `loongagent` CLI transparently spawns or connects to a per-user local server
  (unix socket / named pipe). The TUI is just a client. Sessions live in the
  server, so they survive TUI detach and can be re-attached from web/IDE.
- **B) In-process by default, server optional (Pi-ish).** TUI hosts the core
  directly; daemon mode only when needed. Simpler, but multi-surface attach
  and roaming become special cases instead of the default.
- **C) Always-on system daemon.** OpenCode 2.0 direction. Maximal sharing,
  heaviest install/ops footprint, worst first-run story.

**Lean: A.** It is the only option where "one agent core, multiple frontends"
and session roaming are *structural* rather than bolted on, and Codex's lesson
("cheap day one, painful retrofit") applies directly. The server hosts session
cores as boring in-process objects — Pi's single-process simplicity lives
*inside* the server process.

**Constrains:** AQ-2 (protocol), AQ-3 (storage ownership), all surfaces.

### AQ-2 · Wire protocol & API shape — `Decided` → [ADR-0002](decisions/0002-json-rpc-wire-protocol.md)

**Question:** what do clients and server speak?

- **A) JSON-RPC 2.0, bidirectional**, server→client events as notifications;
  transports: stdio/unix socket locally, WebSocket remotely. ACP is itself
  JSON-RPC — the IDE adapter becomes a dialect of our own protocol, not a
  second stack.
- **B) REST + SSE** (OpenCode style). Ubiquitous tooling, easy HTTP debugging;
  weaker for bidirectional streaming and mid-run steer/pick.
- **C) gRPC.** Strong typing/streaming, but painful in editors/browsers and
  for third-party client authors.

**Lean: A**, with a versioned, OpenAPI-documented schema (schema-first even if
the transport isn't HTTP). Steer-don't-watch (T-12) and live event streaming
(T-13) are bidirectional by nature; JSON-RPC fits both and buys ACP alignment.

**Constrains:** client SDK count, ACP adapter effort, third-party clients.

### AQ-3 · Session & event storage — `Decided` → [ADR-0003](decisions/0003-jsonl-truth-sqlite-index.md)

**Question:** how are sessions, their event trees, events, and audit data persisted?

- **A) JSONL event log as source of truth** (Pi): one file per session, entries
  carry `id`/`parentId` so the tree is inherent. Append-only, crash-safe,
  human-readable, diffable, exportable. Downside: queries (search, stats,
  tree navigation) mean scanning files.
- **B) SQLite as source of truth.** Queries and stats are free; but opaque
  binary format fights our "documented session format, everything inspectable"
  principle, and JSONL export becomes a lossy afterthought.
- **C) Hybrid:** JSONL is the truth; a SQLite index is *derived* (rebuildable,
  disposable) for tree queries, full-text search, and local stats.

**Lean: C.** Pi proved JSONL-as-truth (lossless compaction, fork, replay);
the derived index gives T-02's statistics and T-12's tree UI without
compromising inspectability. Audit events and session events are the same
stream — the backend's aggregator (T-05) consumes the same JSONL shape.

**Constrains:** T-02 (audit/stats), T-12 (tree), T-13 (inspectability),
backend sync format.

### AQ-4 · Extension model — how deep do plugins go? — `Decided` → [ADR-0004](decisions/0004-microkernel-extension-model.md)

**Question:** what can extend loongagent, and in what execution model?
Four distinct extension kinds demand different answers:

| Kind | Examples | Execution options |
|---|---|---|
| Hooks (T-09) | pre/post-tool guards, blockers | shell / HTTP / MCP tool, one verdict contract (ADR-0007 §7); language-neutral, sandboxable, HTTP is an opt-in network action class |
| Skills (T-18; commands merged in) | prompt templates, bundled scripts | markdown + frontmatter, one loader; a *command* is a manually-invoked skill; scripts run as subprocesses |
| External tools (T-15) | MCP servers | subprocess, JSON-RPC — already the standard |
| **Deep extensions** | custom tools in the loop, UI contributions, behavior overrides | **in-process** (Pi's TS extensions — maximal power, couples to impl. language) vs **subprocess** (language-neutral, slower, isolated) vs **WASM** (in-process-ish, sandboxed, language-neutral, more build complexity) |

- **A) In-process everything** (Pi): simplest mental model, self-modifying
  agent, but locks third-party extensions to the core's language and gives
  them full process trust.
- **B) Subprocess for everything third-party**: language-neutral, sandboxable,
  crash-isolated; latency and ergonomics cost.
- **C) Hybrid:** first-party batteries in-process (we trust ourselves);
  third-party deep extensions via WASM (sandboxed, any source language);
  hooks/skills/MCP as subprocesses per the table.

**Lean: C.** It keeps Pi's power where it's safe (our own batteries), makes
untrusted code structurally contained (T-11 philosophy), and — critically —
**decouples the extension ecosystem from AQ-12's language choice**, so picking
Rust for the core doesn't kill community extensibility.

**Refinement — borrow from the DeepSeek-harness ecosystem (owner's prompt,
2026-10-01): the "everything is a plugin" instinct is right at the *edges* and
wrong at the *center*.** Frame the whole design as a **microkernel**:

- *Pluggable at the edges:* **provider adapters + in-flight request/response
  transformers** are plugins (CCR's transformer chain — the normalization layer
  that absorbs protocol drift: stripping `document` blocks a backend rejects,
  clamping `max_tokens`, rewriting mid-conversation system messages); the
  per-model capability registry is **pure data** (Aider: adding a model = adding
  a record, not code); tools — *including built-ins* — sit behind one uniform
  interface (Goose's MCP-first orthogonality), so permissions, hooks, and cost
  metering each get exactly one chokepoint; routing strategies are pluggable.
- *Fixed at the center:* the agent kernel (minimal model+tool loop, event
  append, context assembly) is **not** a plugin point. Pi's lesson: a stable,
  tiny, observable core is what keeps T-13's "nothing hidden" promise true —
  if the loop itself were swappable, behavior would hide and churn.

Net effect on option C: unchanged for third-party *deep* extensions (WASM),
but the edge-plugin set (providers, transformers, tools, routing) is now
explicitly part of the extension surface — mostly as **data + subprocess +
WASM**, not as core patches.

**Constrains:** AQ-12 (language) most strongly; T-09/T-15/T-16/T-18 all land here.

### AQ-5 · Backend control plane — shape and scope — `Decided` → [ADR-0012](decisions/0012-control-plane.md)

**Question:** what exactly lives server-side, and as what deployment shape?

**Decision (owner sign-off, 2026-10-04): a deliberately small backend.** Three
earlier decisions (ADR-0010, the orchestration drop, and the owner's call on
cross-device sync) had already removed most of T-05's original wish list.

- **Shape:** one backend binary, modular inside where scaling differs (model
  gateway / dashboards & statistics / pack distribution); no microservices;
  same **repository** as the local agent with shared correctness-critical crates,
  built as a **separate binary**, deployable **managed or self-hosted** with
  literally the same code either way (ADR-0012).
- **Data:** Postgres (users, policies, event rollups) + object storage (shared
  prompt-cache blobs, signed packs).
- **Uplink is an allow-list, opt-in by category:** clients push event batches
  (delta, resumable); the server pushes only policy/profile/knowledge packs.
  **Default uplink = audit events + aggregate statistics; session bodies and
  transcripts are not in the contract.** Offline stays fully functional.
- **Identity:** flexible firm-internal auth (SSO / service principal, T-08) —
  the account is the rendezvous for the *person* (policy, profiles, metering),
  not a session-continuity mechanism.
- **Complete job list:** identity/auth; policy & profile pack distribution
  (ADR-0008); the model gateway (provider keys, metering, shared prompt cache);
  opt-in aggregated audit/statistics (T-02).
- **Explicit non-jobs:** session bus / live collaboration server (ADR-0010);
  document or knowledge server (knowledge travels as git + signed packs);
  job scheduler for cross-user/scheduled orchestration (AQ-10, no use case);
  session-state sync service.
- **Personal cross-device session sync: not built for now** (owner,
  2026-10-04). Sessions stay a purely local event stream (ADR-0003); roaming
  (ADR-0009) remains **per machine**. Alternatives recorded: manual
  export/import, or the user's own file sync — with the caveat that git is a
  poor fit for a high-frequency append-only log.
- Agent core + tool execution + sandbox always stay local near the code;
  offline = degraded, never dead. **The model gateway is the one residual
  concentration point** (model traffic passes through it by definition of
  centralized keys/metering/cache) — visible, and avoidable by self-hosting.

Deferred: concrete auth protocol (T-08), rollup schema and retention,
cache-sharing policy details, managed-vs-self-hosted packaging, gateway
failover/quotas.

**Constrains:** T-03 deployment shapes, T-05, T-08, enterprise/self-host story.

### AQ-6 · Model layer — abstraction, routing, execution point — `Decided` (routing philosophy; implementation details deferred per owner) → [ADR-0005](decisions/0005-routing-philosophy.md)

**Question:** how do profiles, routing, and provider protocols fit together?

- Provider adapters for the ~4 wire protocols (Pi's insight) behind one
  internal `Model` interface: `complete(messages, tools, budget) → stream`.
  Adapters are **edge plugins** (AQ-4 microkernel), with a **transformer chain**
  (CCR pattern) that normalizes protocol drift per endpoint — and logs every
  downgrade it applies.
- Per-model **capability registry** + startup endpoint probe (T4): context
  size, edit format reliability, tool-calling, thinking, structured output.
  Registry entries are **pure data** (Aider pattern: new model = new record),
  benchmark-derived where possible (format compliance, not just task success).
- **Routing philosophy — local decides, server learns (owner, 2026-10-02):**
  the **local brain is always responsible for the immediate routing decision**
  — never the server, even when connected. The server's role is to **enhance,
  learn, and close the feedback loop**: local reports its routing decisions
  (batched, on sync); the server evaluates whether those were good decisions
  (cost, outcome, verification results), updates policy when they weren't, and
  **delivers policy packs** so the local brain is smarter next time. This
  dissolves the online/offline authority split — local is authoritative in
  every moment; the server shapes *future* decisions. Implementation details
  (policy-pack format, evaluation signals, update cadence) deferred per owner.
- Escalation loop (T-04): cheap first, escalate on verification failure or low
  confidence; every escalation is an auditable event with a reason — and one
  of the server's key learning signals.
- Shared prompt cache at the gateway (big cost lever, T-05).

**Constrains:** T-01, T-04, T-07; defines what "profile" means concretely.

### AQ-7 · Context engine — assembly, budget, retrieval — `Decided` → [ADR-0006](decisions/0006-context-engine.md)

**Question:** who assembles the model's context, and how is it governed?

**Decision (owner sign-off, 2026-10-04): the kernel alone assembles context;
nothing enters the model's context outside a typed, token-counted ledger
entry.**

- **Context ledger is the only write path.** System prompt, tools, rules,
  memory, MCP tools, retrieval results, history — all typed entries.
  Plugins/MCP/skills/hooks/subagents may *propose* entries; none may append
  directly. There is no side channel to the model.
- **Every entry carries provenance and cost:** category, source + version,
  token count, insertion seq, lifetime (turn/session/persistent),
  pinned vs evictable. "Why is this in my context?" is a query, not an
  inference; the Cursor-style context ring falls out of the same data.
- **Budget policy per profile/model, fully visible:** caps per category,
  eviction order, and the request's token floor — the floor is what AQ-6's
  routing reads before deciding.
- **Compaction is a visible event**, with what was summarized and why; the
  event tree (AQ-3/ADR-0015) keeps the lossless record, so it is reversible in
  principle.
- **Retrieval is a kernel service writing through the ledger** — grep →
  repo map → explore subagent results compete for budget like everything else.
- **Memory write path (T-10) — propose-and-confirm.** The agent proposes into
  a staging area; **low-risk project facts** may auto-accept; anything about
  **user preferences/feedback** requires explicit confirmation. Confirmed
  memories are ordinary ledger entries (inspectable/editable/deletable), and
  the write is an event — auditable and revertible.
- **Per-role context packs (T-06) are versioned ledger entries**, i.e.
  profile + role pack composition, never a hidden per-role prompt.

Deferred to later discussion: ledger schema, default caps per profile,
eviction algorithm details, role-pack versioning/distribution (AQ-9/AQ-5),
memory decay/TTL and cross-session memory for async workers (T-10), and the
exact auto-accept rule set for low-risk project facts.

**Constrains:** T-10, T-13, T-06 role packs; token cost floor (T-04).

### AQ-8 · Permission & sandbox engine — `Decided` → [ADR-0007](decisions/0007-permission-sandbox-engine.md)

**Question:** where do policy decisions happen, and what enforces them?

**Decision (owner sign-off, 2026-10-04): the decision lives in the kernel, the
enforcement lives at the OS.** Terminology (XACML/NIST zero-trust): **PDP** =
policy decision point, **PEP** = policy enforcement point, **PIP** = policy
information point.

- **PDP inside the fixed kernel**, at the single tool chokepoint — built-in
  tools exactly like plugin/MCP tools (ADR-0004). Returns allow / ask / deny
  and always records **the rule that fired**. Hooks may advise or veto (T-09);
  they never *are* the decider.
- **PEPs are structural:** OS sandbox (seatbelt / Landlock+bubblewrap /
  Job Objects), the tool runner (file writes, spawns, path resolution), and
  network egress. A PDP bug grants nothing by itself; a PEP bug still leaves
  the intended decision on record.
- **Policy layering (PIP):** built-in defaults < org policy packs (ADR-0005)
  < project < user < session. Org outranks project/user, and **org hard caps
  are enforced as `deny`** — this is the "forbid rather than advise" edge
  ADR-0005 flagged.
- **Autonomy per action class, not per tool:** investigate = allow;
  write-in-workspace = allow by default, profile-gateable; out-of-bounds
  (network egress, paths outside the workspace, destructive/irreversible
  commands, credential access) = ask, always explicit. Defaults are
  junior-proof; role profiles shift the ladder as data (T-06).
- **Sandbox default-on; approval policy is an orthogonal axis; YOLO is a
  visible opt-out** (the opt-out is itself an event, and it travels in audit).
  `sandbox self-test` before the first tool call. v1 scope: the file-write
  boundary and network egress; the micro-VM tier stays deferred — its original
  driver (async runners) is out of scope for now (AQ-10).
- **Approvals are first-class events:** request, decision, granted scope
  (once / this class for this session / persistent for this project), and
  reason — all in the JSONL stream (ADR-0003), replayable in the session's
  event tree (ADR-0015).
  **Denials are structured and fed back to the model with a reason**, never a
  silent failure. Approval UX belongs to the surfaces (AQ-11).
- **Hook trust gate is absolute:** repo-shipped hooks are untrusted content and
  run only after explicit, content-bound per-repo trust opt-in (T-09).
- **Guardian-style LLM pre-review is v2**, plugged in behind the same decision
  interface; v1 is deterministic rules + approval modes.

Deferred: rule/policy language and pack schema (AQ-9), hook trust UX, network
allow-list granularity, micro-VM tier for async runners (AQ-10), Guardian
model/budget policy.

**Constrains:** T-11, T-09, role autonomy (T-06), async runner safety (T8).

### AQ-9 · Configuration & profile layering — `Decided` → [ADR-0008](decisions/0008-configuration-layering.md)

**Question:** file formats, precedence, and distribution of config?

**Decision (owner sign-off, 2026-10-04): one layered stack with a per-key merge
kind, so no lower layer can loosen a higher one.**

- **Layers/ownership:** built-in defaults < org managed < project
  (repo-committed) < user < session. The **org layer** is admin-authored in the
  control plane (T-05) and reaches the install as a versioned pack; project is
  committed; user/session stay local; a solo install is the same binary with an
  empty org layer. Offline = older pack, never a dead system, and **live
  decisions stay local** (ADR-0005).
- **Merge kinds (monotonicity):** *free-override* (UX prefs, profile choice
  within the allowed set) = lowest layer wins; *narrow-only* (permissions,
  allow/deny lists, autonomy class, budgets, timeouts, token ceilings) =
  strictest wins, lower layers may never widen/replace/disable; *add-only*
  (rules, commands, skills, memory, context packs) = union, and a lower layer
  may disable a specific higher-layer entry by identity as an event (allowed
  because content is advisory, per ADR-0007); *reference* (secrets, endpoints).
  **Replacement is not a merge kind for lists** — it is the widening vector.
- **Formats:** TOML for machine-managed config; markdown + YAML frontmatter for
  human-authored content; **AGENTS.md native**, with CLAUDE.md/`.cursorrules`
  handled by explicit one-time import, never a live dual-source merge.
- **Explainable resolution:** every key answers "why is this the value?" —
  winning layer + source artifact + entry identity; `loongagent config explain
  <key>`, and turn-relevant values can appear as ledger entries (ADR-0006).
- **Secrets never in config:** credentials/keys live in the OS credential store
  or the control-plane gateway (T-08/T-05); config carries only references.
- **Packs:** one **signed envelope** (id, semver, payload schema, issuer, scope,
  signature) for profiles (T-07), policy packs (ADR-0005), and role/context
  packs (ADR-0006/0007). Signature verification is what makes the org layer
  tamper-resistant; **unsigned packs are only allowed at project/user layers**.
  AQ-9 fixes the envelope; payload schemas stay with their domains.

Deferred: key schemas per domain, pack transport/update cadence (AQ-5),
signature algorithm and key distribution, discovery paths, importer
normalization.

**Constrains:** T-07, T-21 (what seniors can open), enterprise managed-config
story (Codex `requirements.toml` precedent).

### AQ-10 · Team/async runtime — `Decided` → [ADR-0011](decisions/0011-local-parallelism.md) (local parallelism only; **cloud/cross-user/scheduled orchestration dropped 2026-10-04 — owner: no use case**, see discussion log)

**Question:** how do parallel agents and worktrees compose locally?

Scope: AQ-10 is now about **local parallelism inside one user's session**.
Cross-user and scheduled orchestration, cloud runners, and a runner scheduler
are **out of scope until a concrete use case appears** (owner, 2026-10-04);
ADR-0010 had already removed their collaboration driver. **Decision (owner
sign-off, 2026-10-04)** — authoritative detail in ADR-0011:

- **Write isolation:** any write-capable subagent runs in its own **git
  worktree**; read-only research subagents share the workspace. Isolation is
  spent only where conflicts are real.
  - *Boundary:* a worktree isolates **files, not external resources** — ports,
    databases, container names, shared caches, and global locks still collide.
    Concurrent write subagents must therefore declare/allocate such resources,
    or the concurrent-write count is capped.
  - *Boundary:* in a **non-git workspace** the fallback is a temporary copy or
    serialization — never shared concurrent writes.
- **Subagents are child sessions** (delegation — ADR-0015): each is linked by its
  parent tool call; results return as a **structured artifact** (files plus a
  structured summary), not as long transcripts re-entering the parent context —
  which is also ADR-0006 token discipline.
- **One orchestrator: the parent agent's plan.** Local orchestration *is* the
  parent's visible, steerable plan, recorded in the session's event tree — there
  is **no separate
  scheduler and no second decision-maker**. This keeps orchestration
  explainable and steerable (T-13/T-12), keeps cost attribution on a visible
  decision (T-04), and avoids a component whose lifetime outlives the session.
  - *Boundary test for any future "put it in the backend" request:* **how long
    must this work live, and who has the authority to decide it?** If it cannot
    outlive the session and the user authorizes it, it belongs to the parent's
    plan. Cross-user/scheduled work would fail that test — but it is out of
    scope for now anyway.
  - *This does not mean:* no parallelism (one spawn call can fan out N), no
    limits (budget caps, the 3–5 ceiling, sandbox and permissions still come
    from the kernel — ADR-0007/0008), or no helper library (a spawn/wait
    utility is fine; what is forbidden is a component that decides the plan).
- **Human ceiling 3–5 parallel agents**, with **tiered review** (LLM pre-review
  + human sign-off) reviewing **the user's own** agents (rescoped by ADR-0010).
  Review-relevant data is first-class in the event stream from v1; the rich
  review UI arrives with the web surface.
- **v1 scope:** local worktrees + subagents only. Review is bounded by human
  attention (3–5), worktree lifecycle/GC is product surface, and the worktree
  isolation buys trustworthy verification, per-step diffs, and free
  abandonment — not scheduling features, which are deliberately refused.

**Constrains:** T-17, T-20 (PR-as-deliverable); T-14 resolved as
knowledge-sharing rather than session-sharing (ADR-0010).

### AQ-11 · Surfaces — thin-client rule, build order, installation — `Decided` → [ADR-0009](decisions/0009-surfaces.md)

**Question:** what does each surface contain, how do users install it, and in
what order do we build?

**Decision (owner sign-off, 2026-10-04).** Authoritative detail in ADR-0009.

- **One artifact, many faces:** the single static binary *contains* the local
  agent server; TUI/web/IDE are clients of it or modes of it (`loongagent web`,
  `loongagent acp`); no per-surface install; IDE plugins are thin wrappers.
  Install paths: curl script / brew / npm / firm mirror (TUI); localhost web
  with zero extra install; team web via control-plane relay; IDE via the
  editor's own ACP client. **Desktop and CI dropped** (owner, 2026-09-30) — a
  headless mode may exist internally but is not a product surface.
- **Thin-client rule (hard):** surfaces render the event stream and send
  intents — **zero agent logic client-side**; presentation only (diff
  rendering, syntax highlighting, pickers, keybindings).
- **No single-surface capability:** implemented once in the server and
  *exposed* per surface; a surface may lag in exposing (visible unsupported
  marker), never diverge locally. No surface-only features, ever.
- **Build order:** TUI first → web → ACP adapter. TUI framework: `ratatui`
  (fell out of AQ-12 → ADR-0013); the web stack is TypeScript and independent
  (served by the binary).
- **Attach semantics:** **one writer + N observers** per session; takeover is
  explicit and is an event (ADR-0003), so roaming never means two fronts
  steering the same session.
- **Distribution & updates:** stable/LTS + canary; **token-cost-affecting
  changes are breaking** (major bump + visible changelog, protecting T-04);
  updates are signature-verified like config packs (ADR-0008); unmanaged
  default = stable channel + auto-update on (flagged reversible), while managed
  installs update from the firm mirror and the org layer may pin
  version/channel as a narrow-only key.
- **Browser-only users (owner, 2026-09-30): code is always local** — both the
  localhost web path and team web drive the user's own local agent server and
  code; runner-hosted workspaces stay out of scope and return with AQ-5 if ever
  needed.
- Per-role UX (T-06) is a *surface + profile* composition, not a fork.

Deferred: TUI framework (AQ-12), control-plane relay protocol (AQ-5), update
cadence and rollback, branded VS Code extension scope, observer write-token UX.

**Constrains:** T-06 wow moments, beachhead sequencing, AQ-12.

### AQ-12 · Implementation language — `Decided` → [ADR-0013](decisions/0013-implementation-language.md)

**Question:** which language, now that the architecture is settled?

**Decision (owner sign-off, 2026-10-04): Rust for the kernel and servers,
TypeScript for the web client.**

| Criterion | Fed by | Resolved by |
|---|---|---|
| Single-binary distribution, per-OS sandbox code | AQ-1, AQ-8 | Rust: one binary per OS; sandbox via native FFI (seatbelt / Landlock+bubblewrap / Job Objects) |
| WASM/plugin embedding story | AQ-4 | Rust + `wasmtime` (reference host); capabilities gated, host API versioned |
| Performance floor (event sourcing, retrieval, repo map) | AQ-3, AQ-7 | Rust: append + SQLite index + repo map at 10M+ LOC scale |
| Signed packs | ADR-0008 | Mature Rust crypto libraries for signature verification |
| Concurrency for sessions/tools/subagents | AQ-1, AQ-10 | Rust async + cheap subprocesses + worktree orchestration |
| Small control plane, shared crates | ADR-0012 | Same Rust workspace, **two binaries**: schema, pack sign/verify, policy resolution, provider adapters and cost math are shared; deployment code is not |
| TUI ecosystem quality | AQ-11 | `ratatui` (consequence of this decision) |
| JSON-RPC/ACP library maturity | AQ-2 | Schema-first protocol (ADR-0002) generates the TS client's types |
| Contributor pool & self-modification | AQ-4, T-21 | Addressed by ADR-0004: extensions are WASM/markdown/hooks, so behavior changes need no Rust; self-modification targets the extension surface, not the kernel |

The web surface is a **thin client** in TypeScript, built to static assets and
served by the same binary, talking JSON-RPC to the local server. The boundary is
one-way and narrow: **types are generated from the protocol schema**, not
hand-maintained. Rust's smaller contributor pool is the accepted cost, mitigated
by keeping the kernel tiny and fixed and pushing iteration to the edges.

Deferred: workspace/toolchain layout (cargo workspace; Bun vs Node for the web
build), how the local-agent and control-plane binaries are versioned and
released together, and the concrete WASM host API surface.

---

## Part 3 — Discussion log

Append-only record of architecture Q&A. Format: date, AQ refs, the question
cluster posed, the owner's direction, resulting doc edits.

### 2026-09-30 — Kickoff
Doc created. Foundations imported from 01/vision. Decision map AQ-1…AQ-12
laid out. First cluster posed to owner: **AQ-1 (topology), AQ-2 (protocol),
AQ-3 (storage), AQ-4 (extension model)** — leans A/A/C/C respectively.

### 2026-09-30 — Two-server clarification
Owner asked whether AQ-1's client/server accounts for the firm-wide remote
server (statistics, team collaboration, orchestration). Answer: yes — that is
the AQ-5/T-05 control plane, distinct from the local data-plane server. Added
the two-server topology diagram + AQ-1 scope note to make the distinction
structural in the doc. AQ-1 still open (owner probing scope before deciding).

### 2026-09-30 — Installation & multi-client story
Owner asked whether "client" means only TUI and how users install the product
(TUI default, web-based, IDE plugin). Clarified: all surfaces are first-class
clients of the same local server from day one. Added to AQ-11 (now
`Discussing`): one-artifact-many-faces install table (TUI / local web / team
web / IDE via ACP / desktop / CI), distribution principles (single static
binary, stable/LTS channels, control plane from same codebase), and the open
sub-question of where code lives for browser-only users (relay vs
runner-hosted; lean: both, relay first).

### 2026-09-30 — AQ-1 decided; surfaces trimmed; web = local code
- **AQ-1 decided (owner sign-off):** one binary, always client/server →
  graduated to ADR-0001.
- Owner dropped **Desktop and CI** as product surfaces; AQ-11 table and build
  order updated.
- Browser-only sub-question **resolved: code is always local** (DeepSeek-harness
  style); runner-hosted workspaces out of scope, may return with AQ-5.
- AQ-5 (firm control plane / team collaboration) **deferred** at owner's
  request — revisit after local-side AQs settle.

### 2026-09-30 — AQ-2 decided
Owner accepted the lean: **JSON-RPC 2.0 bidirectional** (unix socket/stdio
local, WebSocket remote), schema-first and versioned, ACP as a dialect of our
own protocol → ADR-0002. Remaining open in the first cluster: AQ-3 (storage),
AQ-4 (extension model).

### 2026-09-30 — AQ-3 decided
Owner accepted the lean: **JSONL event log as source of truth + derived,
rebuildable SQLite index**; audit and session events are one stream → ADR-0003.
First cluster now down to AQ-4 (extension model).

### 2026-10-01 — AQ-4 refinement: microkernel framing (DeepSeek-harness borrow)
Owner asked whether the DeepSeek-harness "everything is a plugin" approach has
anything worth borrowing. Decision-in-discussion: borrow at the **edges** —
provider adapters + in-flight transformer chain as plugins (CCR), capability
registry as pure data (Aider), uniform tool interface incl. built-ins (Goose),
pluggable routing — but keep the **kernel fixed** (minimal loop, event append,
context assembly stay non-pluggable per Pi's observability lesson). AQ-4 and
AQ-6 updated; AQ-4 lean C unchanged but the edge-plugin surface is now
explicit.

### 2026-10-01 — AQ-4 decided
Owner accepted the microkernel framing: pluggable edges (providers,
transformers, tools, hooks, routing), tiny fixed kernel → ADR-0004. First
cluster complete: AQ-1 ✅ AQ-2 ✅ AQ-3 ✅ AQ-4 ✅. Next cluster proposed:
AQ-6 (model layer / routing execution) + AQ-7 (context engine).

### 2026-10-02 — AQ-6 routing philosophy decided
Owner refined the routing design: **local brain always decides immediate
routing (even when connected); server enhances/learns/closes the feedback
loop** — local reports decisions, server evaluates, updates policy, ships
policy packs. Implementation details (policy-pack format, evaluation signals,
cadence) deferred per owner → ADR-0005. The online/offline authority tension
dissolves as a result. AQ-7 (context engine) and its memory-write sub-question
remain open.

### 2026-10-04 — AQ-7 context engine decided
Owner accepted the lean: **kernel-assembled, ledger-visible, budgeted
context** → ADR-0006. The context ledger is the only write path (proposals
only, no side channel), every entry carries provenance + token cost, budget
policy is per profile/model and visible, compaction is a visible event with the
lossless tree behind it (AQ-3), and retrieval enters through the ledger.
Memory write path (T-10) resolved as **propose-and-confirm**: agent proposes
into a staging area, low-risk project facts may auto-accept, user
preferences/feedback require explicit confirmation. Per-role context packs
(T-06) are versioned ledger entries. Session was resumed from a Kimi Code CLI
transcript (decoded out-of-band; the transfer changed no design content).

### 2026-10-04 — AQ-8 permission & sandbox engine decided
Owner accepted the lean → ADR-0007. The PDP (policy decision point) sits inside
the fixed kernel at the single tool chokepoint and records the rule that fired;
the PEPs are structural (OS sandbox, tool runner, network egress) so a decision
bug grants nothing by itself. Policy layers build-in < org packs < project <
user < session, with org outranking project/user and org hard caps enforced as
`deny` (closing ADR-0005's open edge). Autonomy is allocated per action class
(investigate / write-in-workspace / out-of-bounds), junior-proof by default and
shifted by role profiles as data. Sandbox default-on with approval policy as an
orthogonal axis, YOLO as a visible auditable opt-out; v1 covers file writes and
network egress, micro-VM deferred to async runners. Approvals are first-class
events with granted scope and structured reasons fed back to the model; the
repo-hook trust gate is absolute; Guardian-style LLM pre-review deferred to v2.

### 2026-10-04 — AQ-9 configuration & profile layering decided
Owner accepted the lean → ADR-0008. Owner also asked for one point to be made
explicit, and it is now part of the decision: the **org layer** (profiles,
policy, budgets) is maintained in the firm control plane and distributed to
each user's local install as a **versioned pack**; project config lives in the
repo, user/session config stays local; offline means an older pack, not a dead
system; and live decisions remain local (ADR-0005). The core rule is
**monotonicity**: each key declares a merge kind (free-override / narrow-only /
add-only / reference) and no lower layer may loosen a higher one — which
retires the earlier "lists replace by default, `+` to extend" lean, since
replacement was the widening vector. Formats split TOML (machine) vs markdown +
frontmatter (human), with AGENTS.md native and foreign rules files imported
explicitly once rather than merged live. One **signed pack envelope** now
carries profiles, policy packs, and role/context packs; unsigned artifacts are
limited to project/user layers. Every key is explainable layer-by-layer, and
secrets are references rather than values (§ADR-0008).

### 2026-10-04 — AQ-11 surfaces decided
Owner accepted the closeout lean → ADR-0009. Build order locked as **TUI → web
→ ACP adapter**; the thin-client rule hardened to "zero agent logic in a
surface, presentation only"; and a new rule added on top of it — **no
single-surface capability**: a capability is implemented once in the server and
merely *exposed* per surface, lag is allowed but must be visibly marked, local
divergence never. Multi-surface attach is defined as **one writer + N
observers**, with takeover an explicit event (ADR-0003), so session roaming
cannot produce two fronts steering at once. Distribution keeps the
single-static-binary story, adds signature-verified updates matching the
ADR-0008 pack envelope, treats token-cost-affecting changes as breaking
(major bump + visible changelog, protecting T-04), and lets the org layer pin
version/channel as a narrow-only key while the unmanaged default is stable +
auto-update on (flagged reversible by the owner).

### 2026-10-04 — Team collaboration rescoped (knowledge via git, no shared sessions)
Owner reconsidered "team collaboration" and set a narrower direction → ADR-0010:
**no multi-person real-time shared agent sessions for now.** Collaboration is
**shared context and a shared knowledge base through git**, AGENTS.md-style —
repo-committed rules/commands/skills/knowledge plus signed org knowledge packs
(ADR-0008), reviewed and committed by humans. Shared knowledge is *content*,
never policy (ADR-0007), so it cannot widen permissions or budgets. Sessions and
personal memory stay personal; sharing is always an explicit human act. This
narrows **AQ-5** to identity + packs + gateway + opt-in aggregate stats (not a
session bus or document server) and **removes the collaboration driver from
AQ-10's cloud half**, leaving local parallelism (worktrees, subagents, tiered
review) decidable on its own — lean recorded there, pending sign-off. Open
sub-questions: personal cross-device session sync, knowledge artifact
granularity/attribution, and org knowledge delivery (pack vs mirrored git repo).

### 2026-10-04 — Cross-user/scheduled orchestration dropped; AQ-10 is local-only
Owner: **"跨用户/定时" has no application scenario yet — do not build it for
now.** AQ-10 is therefore rescoped to **local parallelism inside one user's
session**; cloud runners, a runner scheduler, and cross-user/scheduled
orchestration are out of scope until a concrete use case appears (this also
retires the driver ADR-0010 had already weakened). Consequences recorded: AQ-5
loses orchestration from the backend's job list (identity + packs + gateway +
opt-in stats remain), and ADR-0007's micro-VM tier stays deferred because its
driver is gone. The local orchestration rule is stated as **one orchestrator —
the parent agent's visible plan** — with an explicit boundary test for any
future "put it in the backend" request (**how long must the work live, and who
has authority over it?**) and explicit non-meanings (parallelism, kernel
limits, and helper libraries are all unaffected). Two isolation boundaries were
added to the AQ-10 lean: a worktree isolates files but **not** external
resources (ports/DBs/containers/shared caches), and non-git workspaces fall
back to a temporary copy or serialization.

### 2026-10-04 — AQ-10 decided: local parallelism
Owner accepted the lean → **ADR-0011**. Write-capable subagents run in their own
**git worktree** (read-only researchers share the workspace), with the two
boundaries recorded: a worktree isolates files but not external resources, and
non-git workspaces degrade to a temporary copy or serialization. Subagents are
**session-tree branches returning structured artifacts** rather than
transcripts, keeping the parent context small (ADR-0006). There is **one
orchestrator — the parent agent's visible, steerable plan** — with no scheduler,
plus an explicit boundary test for future "put it in the backend" requests
(lifetime + authority) and explicit non-meanings. Review is bounded by human
attention (**3–5 agents**, tiered LLM pre-review + human sign-off over the
user's own agents). Cloud/cross-user/scheduled orchestration stays out of scope
for lack of a use case. **This closes AQ-10**: the local-side decision map is
complete except the deferred AQ-5 and the final AQ-12.

### 2026-10-04 — AQ-5 decided: a deliberately small control plane
Owner accepted the lean, with one explicit call: **personal cross-device session
sync is not built for now** → **ADR-0012**. Shape: one backend binary (modular
inside: gateway / dashboards & stats / pack distribution), no microservices;
Postgres + object storage. The uplink is an **allow-list, opt-in per category** —
audit events and aggregate statistics by default, **session bodies never** — and
the server pushes only policy/profile/knowledge packs. Identity is the person's
rendezvous (policy, profiles, metering), explicitly **not** session continuity,
so roaming (ADR-0009) stays per machine. The complete job list is identity,
pack distribution, model gateway, opt-in aggregate stats; the explicit non-jobs
are session bus (ADR-0010), document server (ADR-0010), job scheduler (AQ-10),
and session-state sync service. The model gateway is named as the one residual
concentration point, mitigated by self-hosting (T-03). **This closes AQ-5** —
AQ-12 (implementation language, decided last) is the only open question left.

### 2026-10-04 — AQ-12 decided: Rust kernel/servers + TypeScript web. Decision map complete
Owner accepted the lean → **ADR-0013**. The language fell out of the decided
architecture: Rust covers the single-binary requirement (ADR-0001/0009), FFI-level
sandbox enforcement (ADR-0007), the WASM host via `wasmtime` (ADR-0004), the
performance floor for event sourcing and retrieval (ADR-0003/0006), and signing
(ADR-0008). TypeScript covers the thin web client only (ADR-0009), with its types
**generated from the schema-first protocol** (ADR-0002) so the boundary cannot
drift. The "harness must stay cheap to change" concern is answered by ADR-0004:
behavior changes arrive as WASM plugins, markdown skills/commands, hooks, and
config — not as core patches; self-modification targets the extension surface,
not the kernel. Accepted cost: two languages and a smaller core contributor pool.
Knock-ons recorded: TUI toolkit `ratatui`, WASM host `wasmtime`. **AQ-1…AQ-12 are
now all decided → 03 is `Agreed`**; ADRs 0001–0013 hold the answers.

### 2026-10-04 — Session export/import in scope (ADR-0014); docs restructured (route B)
Owner asked for **full session context export/import** so a session can continue
on another machine → **ADR-0014**. This is explicitly **not** the sync that
ADR-0012 §6 rejected: the user moves a checksummed bundle (event log + tree
lineage + manifest + optional file-history), the workspace is **re-bound** on
import, and mismatches are reported rather than enforced. The security core:
**authority never travels** — historical approvals are records not grants, config
re-resolves locally, credentials are never included, and an imported session is
untrusted input under the local ladder. ADR-0012 §6 was amended to point here
(sync stays out; continuity is file-based). Also decided this round: the **docs
were restructured route B** — Part 1 of this document is now historical framing
with an ADR mapping table, the canonical topology diagram moved to the new
[04-system-overview.md](04-system-overview.md), and the Kimi handoff bundle was
deleted (its content is fully absorbed; the ADRs are the record).

### 2026-10-05 — Export default fixed: no descendant sessions
Owner decision → folded into **ADR-0014 §1**: `session export <id>` carries the
session's **complete** event log (its internal node history always travels in
full — trimming that is pruning, a different feature) plus its **ancestor
sessions**, and **excludes descendant sessions by default**; `--with-branches`
adds the whole subtree or nothing (a half-included tree breaks lineage). Recorded
rationale: continuation needs only the parent, whose context already holds each
subagent's structured summary (ADR-0006/0011); descendants are archival/review
material; and in the reference session they were ~2,700 events against the
trunk's ~1,400, roughly tripling the bundle. The question that surfaced this —
**is a "session tree" a node tree inside one file (ADR-0003/T-12) or a tree of
sessions (ADR-0011/ADR-0014)?** — is now recorded in ADR-0014 as an **explicit
open dependency** instead of being left implicit: the two can coexist (node tree
*within* a session, session lineage *between* sessions), but the vocabulary must
be fixed, and it decides the granularity of ADR-0009's one-writer rule,
ADR-0011's worktree/GC unit, and this ADR's wording.

### 2026-10-05 — Vocabulary fixed: two tree levels (ADR-0015)
Owner: "区分好这两个概念不要有歧义". → **ADR-0015** names and separates the two
levels: a **session** *contains* an **event tree** (`id`/`parentId`; paths, and
the current **head**), while sessions relate to each other by **session
lineage** (fork, delegation). **"Branch" is reserved for a child session** and
is never used for a path; "tree" unqualified is retired from the docs. The
boundary rule: **a new session is created when the new work needs its own
lifecycle** (write token, worktree, export/GC unit, cost and permission
attribution) — retry, steer, follow-up, compaction, and profile changes do not.
Child sessions **reference** their parent's prefix instead of copying it
(immutable parent history makes the reference stable), which is the structural
reason ADR-0014 must carry ancestor sessions. Wording aligned in ADR-0003,
ADR-0009 (one writer is per session; the head is where its writer is),
ADR-0011 (subagents are child sessions), ADR-0014, T-12, T-17, and the overview.

### 2026-10-05 — "Same code" disambiguated (two binaries, shared crates)
Owner: "这怎么能是同一份代码?" — the phrase was conflating two different claims →
**ADR-0012 §1 rewritten**. **Claim A:** the control plane managed vs self-hosted
is **literally the same code** (T-03) — that one is exact. **Claim B:** the
control plane and the local agent live in **one repository/workspace** and share
only the crates where disagreement is a bug — protocol/event schema
(ADR-0002/0003), pack envelope + sign/verify (ADR-0008), policy and config
resolution (ADR-0005/0008), provider adapters + transformers (ADR-0004), and
token/cost math (ADR-0006) — while being **two separate binaries** with different
deployment code (kernel/sandbox/tools/surfaces vs
identity/Postgres/pack-issuance/quotas/dashboards). Rationale recorded: the two
sides ship on **independent channels** (canary vs stable, ADR-0009), so
duplicated implementations *will* diverge — and a divergent verifier, cost meter,
or policy resolver silently breaks packs, billing, or an org hard cap.
**"Same binary in another mode" is retired as an over-reading.** Wording fixed in
ADR-0008/0009/0013, AQ-5, AQ-12, the topology diagram, and the overview.
