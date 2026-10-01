# 03 — Architecture (question-driven design)

- **Status:** Discussing
- **Last updated:** 2026-09-30

This document designs the loongagent architecture **iteratively, through
structured questions (AQ-N)** discussed with the owner. Method: each AQ lists
the decision, the options, the constraints our principles impose, and a
recommended lean. Nothing here is "Decided" until the owner signs off in
conversation; locked decisions graduate to `decisions/` ADRs.

When the AQs are answered, the **implementation language (AQ-12) falls out
last** — it is a consequence of the architecture, not a precondition.

---

## Part 1 — Foundations already locked (from 01-landscape-synthesis + vision)

These are agreed direction (owner-endorsed); the AQs design *how*, not *whether*:

1. **Client/server core from day one** (T2). TUI, web, desktop, IDE/ACP, CI
   are all clients of one agent server. Cheap from day one, painful to retrofit.
2. **Minimal Pi-grade loop inside the server** (T1): few tools, tiny prompt,
   no hidden context. Everything else is a *visible, budgeted, disable-able*
   batteries layer.
3. **Sessions are trees** (T-12, owner: "killer feature — I want this");
   fork/retry/steer/pick are first-class. Files: shadow git snapshots (T9).
4. **Nothing hidden, everything inspectable** (T-13): every token in context,
   every policy decision, every cost is an event visible in the UI.
5. **OS sandbox default-on, approval policy separate, YOLO opt-out** (T3/T-11).
6. **Model profiles, not model pickers** (T-07): capability-named profiles
   (`fast`/`default`/`supersmart`), admin-curated, server-distributed.
7. **Cost-conscious by design** (T-04): $0 path for trivial tasks, escalation
   upward-on-need, hard budgets, everything metered.
8. **Control plane ≠ data plane** (T-05): agent core stays local near the code;
   the backend owns auth, profiles, routing brain, model gateway, audit/stats
   aggregation. Fully functional offline, degraded.
9. **ACP server-side for IDEs; no editor fork** (T-19).
10. **Layered retrieval**: grep/symbols → token-budgeted repo map → explore
    subagent. No vector DB in v1 (T7).
11. **Sync-first, async-ready** (T8): worktree isolation + PR-as-deliverable.
12. **Junior-proof defaults, senior-proof depth** (T-21): every layer openable.

---

## Part 2 — The decision map (AQ-1 … AQ-12)

Ordered by dependency. Each AQ: the question, options, lean, what it constrains.

### The two-server topology (clarified 2026-09-30, owner's prompt)

"Server" is two different machines doing different jobs. AQ-1…AQ-4 design the
**local** one; AQ-5 designs the **firm-wide** one:

```
 ┌───────┐ ┌───────┐ ┌────────┐ ┌──────────┐
 │  TUI  │ │ Web   │ │IDE/ACP │ │CI/headless│   surfaces — thin clients (AQ-11)
 └───┬───┘ └───┬───┘ └───┬────┘ └────┬─────┘
     └─────────┴────┬────┴───────────┘
                   │ JSON-RPC — unix socket locally, WebSocket remotely (AQ-2)
          ┌────────▼─────────┐
          │ LOCAL AGENT      │  per-user, on the engineer's machine, next to
          │ SERVER           │  the code: session trees, tool loop, sandbox,
          │ (data plane)     │  JSONL truth, retrieval. AQ-1 … AQ-4.
          └────────┬─────────┘
                   │ smart sync — events/stats up, policy & profiles down
                   │ (batch, delta, resumable; offline = degraded, never dead)
          ┌────────▼─────────┐
          │ FIRM CONTROL     │  firm-wide backend: auth (T-08), model profiles
          │ PLANE            │  & routing brain (T-07/T-01), model gateway
          │ (control plane)  │  (keys, metering, shared cache), audit/stats
          │                  │  (T-02), team sharing (T-14), orchestration.
          └────────┬─────────┘  AQ-5. Self-hosted or managed, same code (T-03).
                   │ metered model calls only
          ┌────────▼─────────┐
          │ MODEL PROVIDERS  │
          └──────────────────┘
```

Key property: **code never transits the control plane.** Tool execution and
file access stay local; the only things that flow up are events/stats (opt-in,
visible) and the model calls themselves, which go through the gateway to
providers anyway. Cloud/async workers (T8) are the deliberate exception —
there the code lives on the runner by definition.

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

**Question:** how are session trees, events, and audit data persisted?

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
| Hooks (T-09) | pre/post-tool guards, blockers | shell executables, exit-code protocol (Claude Code pattern) — language-neutral, sandboxable |
| Commands & skills (T-16/T-18) | prompt templates, bundled scripts | markdown + frontmatter; scripts run as subprocesses |
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

### AQ-5 · Backend control plane — shape and scope — `Open` (deferred per owner, 2026-09-30 — team-collaboration shape not yet clear; revisit after local-side AQs settle)

**Question:** what exactly lives server-side, and as what deployment shape?

Per T-05 (direction agreed): backend owns auth (T-08), profile/routing brain
(T-07/T-01), model gateway (keys, metering, shared cache), audit/stats
aggregation (T-02), team sharing (T-14), async runner orchestration (T8).
Agent core + tool execution + sandbox always local near the code; offline =
degraded, never dead.

Sub-questions to decide:
- **Monolith or services?** Lean: one backend binary, modular inside (module
  boundaries where scaling differs: gateway vs. dashboards vs. runner
  scheduler). Microservices are an ops tax we haven't earned.
- **Data store:** Postgres (events rollup, users, policies) + object storage
  (shared artifacts, cache blobs)? Lean: yes, boring and sufficient.
- **Sync protocol:** clients push event batches (delta, resumable); server
  pushes policy/profile updates. Every byte user-visible and opt-in per
  feature (local-first promise).

**Constrains:** T-03 deployment shapes, T-05, T-08, enterprise/self-host story.

### AQ-6 · Model layer — abstraction, routing, execution point — `Open`

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
- **Where does routing execute?** Options: (a) client-side router with
  server-distributed policy tables (works offline, tamperable); (b) server-side
  router, client sends *intent + task class*, never a model name (tamper-proof,
  dead offline); (c) **both** — server is the brain when connected, a local
  fallback table takes over offline (lean: c).
- Escalation loop (T-04): cheap first, escalate on verification failure or low
  confidence; every escalation is an auditable event with a reason.
- Shared prompt cache at the gateway (big cost lever, T-05).

**Constrains:** T-01, T-04, T-07; defines what "profile" means concretely.

### AQ-7 · Context engine — assembly, budget, retrieval — `Open`

**Question:** who assembles the model's context, and how is it governed?

Design stance from our principles: **the core alone assembles context**;
nothing injects silently (T-13). Concretely:
- A **context ledger**: every block entering context (system prompt, tools,
  rules, memory, MCP tools, retrieval results, history) is a typed entry with
  token count and provenance — the Cursor-style context ring falls out for free.
- **Budget policy** per profile/model: caps per category, eviction/compaction
  order, all visible.
- Retrieval pipeline (foundation #10) runs as core services, results enter
  through the ledger like everything else.
- Compaction is a visible event; the tree keeps the lossless record (AQ-3).

Open sub-questions: memory write path (T-10) — who may add ledger entries
(agent-proposed + user-confirmed?); per-role context packs (T-06) as versioned
bundles.

**Constrains:** T-10, T-13, T-06 role packs; token cost floor (T-04).

### AQ-8 · Permission & sandbox engine — `Open`

**Question:** where do policy decisions happen, and what enforces them?

- **Policy decision point (PDP):** middleware on the tool boundary inside the
  core (foundation #2's guarantee layer, T5/T-09). Inputs: action, role
  profile, session policy, org policy (server-distributed), approval mode.
  Per-action autonomy ladder: investigate free, write gated, vary by role.
- **Enforcement:** OS sandbox per platform — seatbelt (macOS), Landlock/
  bubblewrap (Linux), Job Objects/restricted tokens (Windows); micro-VM as a
  later tier for async runners. `sandbox self-test` command (Codex).
- Hooks can veto at the PDP; hook reasons feed back to the model (exit-code-2
  pattern).
- Open: Guardian-style LLM pre-review of planned actions (Codex) — v1 or v2?

**Constrains:** T-11, T-09, role autonomy (T-06), async runner safety (T8).

### AQ-9 · Configuration & profile layering — `Open`

**Question:** file formats, precedence, and distribution of config?

- Precedence chain: built-in defaults < org managed (server-distributed,
  tamper-resistant) < project (repo-committed) < user < session overrides.
- Format: TOML for machine-managed config (profiles, policies) vs markdown
  for human-authored (rules, commands, skills — already decided by ecosystem
  convention). Lean: both, each where it fits; AGENTS.md as the native rules
  format with CLAUDE.md/.cursor import (01 open Q3).
- Profiles (T-07) are versioned, server-distributed, PR-reviewable (git-native
  config lesson from role-specialized research).

**Constrains:** T-07, T-21 (what seniors can open), enterprise managed-config
story (Codex `requirements.toml` precedent).

### AQ-10 · Team/async runtime — `Open`

**Question:** how do parallel agents, worktrees, and cloud runners compose?

- Local parallelism: worktree-per-agent (Cursor/Amp pattern), subagents as
  visible session-tree branches (T6/T-17), artifact-based reports.
- Coordination: artifact bus (typed docs, MetaGPT's real innovation) over chat
  protocols; orchestration logic lives in the control plane (round-2 lesson),
  not hardcoded inside agents.
- Human ceiling: 3–5 parallel agents — tiered review (LLM pre-review + human
  sign-off) is our differentiator; design the review surface early.
- v1 scope question: local worktrees + subagents only, runners v2? Lean: yes.

**Constrains:** T-14, T-17, T-20 (PR-as-deliverable).

### AQ-11 · Surfaces — thin-client rule, build order, installation — `Discussing`

**Question:** what does each surface contain, how do users install it, and in
what order do we build?

- **Thin-client rule:** surfaces render the event stream and send intents;
  zero agent logic client-side. This is what makes "same brain, many faces"
  true rather than aspirational.
- Build order lean: **TUI first** (dogfood, senior-engineer beachhead, fastest
  iteration loop) → **web** (newbie/manager/PM personas) → **ACP adapter**
  (IDEs come free). **Desktop app and CI surface: dropped** (owner,
  2026-09-30) — not surfaces we build; a headless mode may still exist
  internally for scripting, but it is not a product surface.
- TUI framework depends on AQ-12 (ratatui vs OpenTUI vs ...); web stack is
  independent (backend serves it).
- Per-role UX (T-06) is a *surface+profile* composition, not a fork.

**Installation & distribution (owner's prompt, 2026-09-30).**
Principle: **one artifact, many faces.** The single `loongagent` binary
*contains* the local agent server; every surface is either a client of it or
a mode of it. Installing loongagent = getting that binary — no separate
install per surface (IDE-side ACP plugins are thin wrappers, not parallel
products).

| Surface | Install path | How it connects |
|---|---|---|
| TUI (default) | `curl` install script / brew / npm / firm-internal package mirror | spawns or attaches to the local server (unix socket) |
| Web — local | zero extra install: `loongagent web` serves the UI from the binary on localhost | same machine, same server |
| Web — team | zero install: browser → firm control-plane URL | relays to the user's own local server (see below) |
| IDE | the editor's own ACP client (Zed built-in; JetBrains/Neovim/Emacs plugins) pointed at `loongagent acp`; optional thin loongagent-branded VS Code extension later | ACP/JSON-RPC over stdio to the local binary |
| ~~Desktop~~ / ~~CI~~ | dropped (owner, 2026-09-30) | — |

Distribution principles:
- Single static binary per OS; firm-internal mirror (Artifactory-style) for
  managed rollouts.
- Update channels: **stable/LTS + canary**; token-cost-affecting changes are
  treated as breaking changes (01 §D — daily-release churn is a known
  ecosystem failure mode).
- The firm control plane ships from the **same codebase** (ideally the same
  binary, different mode) — one artifact story end-to-end; sub-question for
  AQ-5.

**Sub-question — where does code live for a browser-only user? — RESOLVED
(owner, 2026-09-30): code is always local.** The web experience works like the
DeepSeek-style local harness: the web UI — whether served by the user's own
binary (`loongagent web`) or reached via the firm control plane — always
operates against the user's **local agent server and local code**. No
runner-hosted workspaces in scope; if the firm-wide story ever needs them, it
comes back with the AQ-5 team-collaboration discussion (deferred by owner).

**Constrains:** T-06 wow moments, beachhead sequencing, AQ-12.

### AQ-12 · Implementation language — `Open` (decided LAST)

Not a question to answer now — it **falls out** of AQ-1…AQ-11. Criteria the
answers above will populate:

| Criterion | Fed by |
|---|---|
| Single-binary distribution, per-OS sandbox code | AQ-1, AQ-8 |
| TUI ecosystem quality | AQ-11 |
| Concurrency model for many sessions/tools per server | AQ-1, AQ-10 |
| WASM/plugin embedding story | AQ-4 |
| JSON-RPC/ACP library maturity | AQ-2 |
| Contributor pool & self-modification ambitions | AQ-4, T-21 |
| Performance floor (event sourcing, retrieval, repo map) | AQ-3, AQ-7 |

Candidates carried forward from research: **Rust** (Codex: robustness, single
binary, sandbox FFI, ratatui), **TypeScript/Bun** (Pi/OpenCode/Claude Code:
extension ecosystem, self-modification, fastest product iteration), **Go**
(Amp/OpenHands-CLI: middle ground, great server ergonomics, weaker TUI).

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
