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

## The decision map (AQ-1 … AQ-12)

The pre-AQ historical framing and the discussion log this document used to carry
beside the map are kept as history, not guidance, in
[archive/03-architecture-history.md](archive/03-architecture-history.md).

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
signature algorithm and key distribution (both settled later by ADR-0016),
discovery paths, importer normalization.

**Constrains:** T-07, T-21 (what seniors can open), enterprise managed-config
story (Codex `requirements.toml` precedent).

### AQ-10 · Team/async runtime — `Decided` → [ADR-0011](decisions/0011-local-parallelism.md) (local parallelism only; **cloud/cross-user/scheduled orchestration dropped 2026-10-04 — owner: no use case**, see [the discussion log](archive/03-architecture-history.md))

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
