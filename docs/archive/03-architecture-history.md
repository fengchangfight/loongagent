# 03 — Architecture history (Part 1 and the discussion log)

Archived from [03-architecture.md](../03-architecture.md) on 2026-10-16. What is here
is **history, not current guidance**: the pre-AQ framing (Part 1) and the discussion
log (Part 3) of the document that asked AQ-1…AQ-12. Part 2 — the decision map — stayed
live in [03-architecture.md](../03-architecture.md), and the ADRs in
[`decisions/`](../decisions/) supersede it as the authority for every answer.

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
[04-system-overview.md](../04-system-overview.md). This document keeps the
question-by-question record.

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
[04-system-overview.md](../04-system-overview.md), and the Kimi handoff bundle was
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
