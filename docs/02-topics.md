# 02 — Topic List (idea backlog)

How this works: the owner drops raw ideas in conversation; they land here as
topics with a one-line seed connecting them to what the research already knows
(`research/`, `05-parity-and-synthesis.md`). When a topic gets discussed in
depth, it graduates to its own `NN-<topic>.md` or an ADR in `decisions/`, and
its status here changes to **Promoted**.

Legend: `Seed` (captured, not yet discussed) · `Discussing` · `Promoted → <doc>` · `Wanted` (owner has said "I want this" — direction agreed, details to discuss)

## Coverage map (owner checklist of 2026-09-30)

| Owner's item | Covered by |
|---|---|
| hooks | T-09 |
| memory system | T-10 |
| budget system | T-04 (cost governance) + T-07 (profile budget caps) |
| permission & sandbox | T-11 |
| model provider profile (flexible but controlled) | T-07 |
| pi try/retry/fork | T-12 |
| pi-style "understand why the agent is doing X" | T-13 |
| team work & sharing | T-14 (extends T-03) |
| MCP | T-15 |
| commands | T-16 → merged into T-18 |
| subagents | T-17 |
| skills marketplace | T-18 |
| IDE integration | T-19 |
| git diff & spec-driven dev process | T-20 |
| opencode-style server (client/server) | Strawman #1 / synthesis T2 + T-05 |
| context engineering paramount, nothing hidden | Vision principles + T-13 |
| core = minimal "model + tool loop" | Vision principles + synthesis T1 |
| branching history — event tree in a session + lineage between sessions (Pi's killer feature — "I want this") | T-12 |
| models don't need much harness | Vision principles |
| harness cheap to change, pluggable with good defaults | Vision principles + T-21 |
| be careful and mean with context | Vision principles |
| allow pick/steer mid-run | T-12 |
| avoid Pi's steep learning curve | T-21 |
| one agent core, multiple frontends | Vision principles + T-05 |

---

## T-01 · Smart cascading model routing — `Promoted → ADR-0005`

Seeded 2026-09-26. **Execution point decided 2026-10-02 (ADR-0005):** the local
brain makes every live routing decision; the server learns from the routing and
escalation events reported to it and ships policy packs. Still open: the
cascading strategy itself (fallback chains vs. escalation vs. budget-aware
routing), the policy-pack format, and what the server counts as a bad decision.

Idea: route work across models intelligently instead of one model per session.

Research hooks:
- A coding session is already several workloads (main loop / background /
  deep-think / long-context) — claude-code-router's scenario routing, and
  OpenCode's per-agent model overrides (cheap planner, strong builder).
- Amp replaces the model picker with a **capability dial** (`low`→`ultra`) and
  re-routes silently as the frontier moves — decouples UX from model churn.
- Aider's architect/editor split: strong model proposes, cheap model serializes
  edits. Plus CCR insight: route per *request*, not per session.
- Our T4 registry (per-model capability profiles + endpoint probing) is the
  substrate this builds on; cost guardrails belong to T-02's statistics.
- Open question: cascading = fallback chains on failure? escalation when a
  cheap model's output fails verification? budget-aware routing?

## T-02 · Audit & statistics & implicit feedback loop — `Promoted → ADR-0003, ADR-0005, ADR-0012`

Seeded 2026-09-26. **Substrate decided 2026-09-30 (ADR-0003):** audit events
and session events are one JSONL stream, and the derived SQLite index serves
local statistics. The backend consumes the same stream for **opt-in aggregated**
audit/statistics, under an allow-listed uplink (ADR-0012 §3/§5), and the routing
feedback loop is that same stream (ADR-0005). Still open: which statistics are
shown first and where the opt-in line sits.

Idea: record everything the agent does, measure it, and let usage signals
improve the system without explicit user effort.

Research hooks:
- Event-sourced core (OpenHands event stream; our strawman #1) makes audit
  nearly free: every tool call, approval, cost is a typed event — replay,
  audit, compliance.
- Statistics users actually want: live per-turn token/cost meter, context-ring
  breakdown, cache-hit telemetry (T10 — trust differentiator).
- Implicit feedback: which suggestions get accepted/reverted (Cursor Tab
  accept-rate thinking), which rules get violated, which routes/models produce
  reverts → feeds T-01 routing and memory. Cursor's sidecar-observer memories
  are the passive-collection precedent.
- Tension to resolve: implicit feedback vs. our local-first promise — what
  leaves the machine must be opt-in (OpenCode's stealth-telemetry backlash).

## T-03 · Deployment & ways of using (local, cloud, team sharing, cloud statistics) — `Promoted → ADR-0012, ADR-0010`

Seeded 2026-09-26. **Decided 2026-10-04:** managed and self-hosted are
*literally the same backend code* (ADR-0012 §1); team sharing is git-shared
knowledge, not a shared live session server (ADR-0010); and the backend's whole
job list is identity, pack distribution, the model gateway and opt-in
statistics (ADR-0012 §5). Still open: packaging and the managed/self-hosted
split in practice.

Idea: one core, many deployment shapes — solo local, self-hosted team server,
managed cloud — with sharing and aggregated statistics.

Research hooks:
- Client/server core (T2) is the enabler: local daemon today, team server or
  cloud tomorrow, same API. OpenCode 2.0 is heading exactly here (always-on
  service, sessions synced across devices, agents hopping machines over
  Tailscale); Codex unifies via account identity, not file sync.
- Team sharing precedents: shared rules/memories via git (Claude Code),
  enforced team rules (Cursor Teams), session/thread sharing URLs (Amp,
  OpenCode `/share` — note privacy defaults matter).
- Cloud statistics: Cursor Teams usage analytics, GitHub Copilot dashboards;
  T-02's event stream is the raw material; privacy posture is the design
  constraint.
- Cloud agents: Copilot's "platform is the sandbox" (run on CI infra orgs
  already trust), Codex two-phase environments (setup with secrets/internet,
  agent phase locked down), governance defaults (requester can't approve PR).

## T-04 · Cost governance (anti-waste by design) — `Promoted → ADR-0005, ADR-0006, ADR-0007, ADR-0009`

Seeded 2026-09-26. **Mechanisms decided:** cheap-first escalation, with every
escalation an auditable event with a reason (ADR-0005); per-category budgets and
a token floor, visible before the call (ADR-0006 §3); org hard caps enforced as
`deny` in the PDP so local routing cannot route around them (ADR-0007 §3); and
cost-affecting changes treated as breaking (ADR-0009 §6). Still open: the $0
path for trivial requests (the cheap local classifier) and the task-class policy
layer.

Owner's motivation: when firms rolled out Claude Code, bills exploded because
engineers sent *everything* to the most expensive model — including trivial
shell tasks (`git push`) and off-topic chit-chat ("what did you eat") — burning
Opus-class tokens on $0-value requests. **loongagent must make this structurally
impossible, not merely discouraged.**

Design directions (to discuss):
- **The $0 path for trivial tasks.** Many "agent" requests need no frontier LLM
  at all: `git push`, "run the tests", "what port is X on". A cheap local
  classifier (or even heuristics/ slash-command grammar) can route these to:
  (a) direct execution with confirmation, (b) a tiny/local model, or (c) a
  canned answer. Frontier model is the *last* resort, not the default.
- **Escalation, not just routing (links T-01).** Start cheap; escalate to a
  stronger model only when verification fails or confidence is low. The default
  direction of money flow is upward-on-need, never downward-on-habit.
- **Org policy layer (links T-03).** Admin-set model ceilings per task class
  ("no Opus for chat"), per-user/per-team budgets with hard caps, require
  justification to exceed. Precedent: Codex `requirements.toml` admin
  enforcement; Cursor Enterprise model blocklists — but nobody enforces
  *task-level* cost policy.
- **Visibility as deterrence (links T-02).** Live per-turn cost meter, session
  totals, team dashboards. Cursor's pricing fiasco and Claude Code's limit
  opacity prove cost invisibility destroys trust; visibility alone changes
  behavior.
- **Audit trail for waste (links T-02).** The event log records model, tokens,
  cost, and task class per turn → "who burned $400 on chit-chat" is a query,
  not an argument.
- **Chit-chat handling:** off-topic requests can be answered by the cheapest
  available model (or deflected politely) — the tool should never spend
  frontier tokens outside its job description unless the user explicitly
  overrides.

Research hooks: T10 (cost transparency), CCR scenario routing, Amp capability
dial, Codex enterprise `requirements.toml`, HN "agentic coding only makes sense
through subscription billing" (BYOK users get burned — budgets are the fix).

## T-05 · Rich backend / control plane (not a local-only tool) — `Promoted → ADR-0012`

Seeded 2026-09-26. **Decided 2026-10-04 (ADR-0012):** the backend is
deliberately small — identity/auth, policy & profile pack distribution, the
model gateway (keys/metering/shared cache), and **opt-in aggregated**
audit/statistics. Explicitly not a session bus, collaboration server, document
server, job scheduler, or **session-state sync service** (personal cross-device
session sync: not built for now; roaming stays per machine). The uplink is an
allow-list with session bodies excluded; the model gateway is the one residual
concentration point, avoidable by self-hosting (T-03). Still open: the auth
protocol (T-08), rollup schema/retention, and packaging.

**Rejected (ADR-0005):** the original **routing & policy brain** — live routing,
model ceilings and task-class policy decided *server-side* — is refused, not
deferred: the local brain makes every live call and the server never routes. It
is recorded with the deliberate "no"s in
[05-parity-and-synthesis](05-parity-and-synthesis.md) §D so it is not re-argued.

Owner's idea: loongagent is **not a local-only tool**. There should be a rich
backend that local clients *smartly* communicate with — handling audit,
statistics, cascading/routing decisions, and other smart orchestration work
beyond what a local process should do.

What the backend could own (to discuss):
- **Audit & statistics aggregation** (T-02): org-wide event streams, cost
  dashboards, usage analytics.
- **Routing & policy brain** (T-01/T-04): the cascading router, model ceilings,
  budgets, and task-class policy live server-side so they're centrally managed
  and tamper-resistant (client-side policy is bypassable).
- **Model gateway**: centralized provider keys, billing, cache sharing across
  the team (shared prompt cache is a *big* cost lever), per-org provider config.
- **Orchestration**: cloud/async workers, scheduled jobs, cross-session memory,
  team knowledge, session sync across the user's devices and surfaces.
  **Cross-user and scheduled orchestration dropped for now** (owner,
  2026-10-04: no use case — see AQ-10); personal cross-device session sync is
  **not built** (ADR-0012 §6; export/import is the continuity path, ADR-0014).
- **Identity**: the account as the rendezvous between surfaces (Codex's lesson:
  identity, not file sync, is the coordination mechanism).

Critical design constraint — **control plane, not data plane; enhancement, not
dependency:**
- Cursor's biggest trust liability is routing *all* model requests through its
  backend even with BYOK; OpenCode got burned for phoning home for session
  titles. **The local client must remain fully functional offline/disconnected**
  (degraded: no org stats, no central policy refresh, local routing fallback).
- When connected, sync is smart: batch, delta, resumable — and every byte that
  leaves the machine is visible/opt-in per our local-first principle.
- Deployment shapes (T-03): managed cloud backend OR self-hosted team server,
  same code. Enterprise/privacy customers run their own.
- Open question: does the agent *core* (tool execution, sandbox) always stay
  local near the code, with the backend as pure control plane? (Recommended:
  yes — code never needs to transit our backend except model calls that go to
  providers anyway. Cloud workers are the explicit exception: there the code
  lives on the runner by definition.)

## T-06 · Per-role UX: the "this is THE tool for me" moment — `Wanted` (HIGH PRIORITY)

Seeded 2026-09-26. Owner emphasis: **"I cannot emphasize more about how
important UX is for the success of this project."** **Direction agreed:** a role
is a *surface + profile* composition — a named profile plus a versioned context
pack (ADR-0006 §7) and per-action-class autonomy offsets (ADR-0007 §4) — never
a forked product (ADR-0009). Still to design: the personas' wow moments, the
beachhead, and the wow metrics.

Idea: every role — regardless of seniority or surface preference — should get a
strong "oh my god, this is just the tool I want" feeling. Not one average UX for
everyone; a *deliberate wow moment per persona*.

Personas and their wow moments (owner's examples + extensions):
- **Experienced engineer** → "holy sh*t this CLI is lightning fast and the
  responses are smart." Terse, keyboard-driven, zero ceremony, stays in flow.
  (Research: latency is a feature — Cursor Tab's sub-30s iteration changes
  behavior; Pi's 1.4k-token overhead; native-scrollback TUI.)
- **Engineering manager** → "this really saves money; statistics and feedback
  in a few clicks; everything summarized so clearly I know what to improve."
  (T-02/T-04 dashboards: cost per person/task/model, waste detection, trend
  summaries written in manager language, not engineer language.)
- **PM / product-minded user** → "it clears my ideas visually and efficiently."
  Visual spec canvas: vague idea → structured spec (Kiro-style EARS
  requirements) with drag/refine UX; idea → plan → delegated tasks without
  touching a terminal. (Links: kiro-spec-driven research; T-01 routing lets PM
  queries run on cheap models.)
- **Newbie / web-preferring user** → "wow, the web interface works even better
  than CLI and it's pretty." Zero-install onboarding, guided first session,
  safe defaults (sandbox + preview-then-approve), beautiful by default —
  terminal-aesthetics quality bar applied to the web.
- **DevOps/SRE** → evidence chains, not guesses (incident.io's contract);
  read-only investigate mode that's genuinely safe against prod; runbook
  execution with checkpoints.
- **QA** → visual test review, screenshot diffs, coverage heatmaps.
- **Data engineer** → schema-aware SQL assistance, dry-run/plan before query
  (WrenAI guardrails), notebook-friendly surface.

Design principles implied:
- **Same brain, different faces** — the core agent is shared; the *surface,
  density, vocabulary, and defaults* adapt per role. A manager never sees a
  token trace; an engineer never sees a dashboard card first.
- **Progressive disclosure of power** — resolve the power-vs-simplicity tension
  by layering: the newbie web UI and the wizard's CLI talk to the same core;
  complexity reveals itself as the user asks for it.
- **Beauty is functional** — pretty = trustworthy = adopted. Terminal UI gets
  the same design budget as web (Charm/OpenTUI set the bar).
- **Every persona gets a measurable wow metric** — time-to-first-success,
  time-to-first-insight (manager), time-to-first-spec (PM). If we can't
  measure the wow, we can't defend it.

Open questions:
- Which persona is the **beachhead**? (You can't wow everyone on day one —
  sequencing matters. Candidates: senior engineers for credibility, or
  web-first newbies for volume.)
- Does "PM mode" produce artifacts that engineers' sessions consume? (Specs as
  the handoff protocol between personas — links kiro-spec-driven research.)
- How do role profiles (T: role-specialized research) map to UX personas —
  same thing or two axes (role × seniority)?

## T-07 · Model profiles, not model pickers — `Promoted → ADR-0008, ADR-0016`

Seeded 2026-09-26. **Distribution decided 2026-10-04 (ADR-0008):** the org
layer is admin-authored in the control plane and shipped to local installs as a
**signed, versioned pack** (same envelope as policy/context packs); the local
install verifies the signature, caches the pack, and stays functional offline
with the last synced version; live routing decisions remain local (ADR-0005).
Unsigned artifacts can never occupy the org layer. Still open: the **policy and
context** payload schemas — the envelope is built for them, the payload schemas
are not written (ADR-0016) — while the *profiles* payload schema **is** written
and used to verify (`PROFILES_SCHEMA`, `crates/pack/src/lib.rs`); pack **update
cadence** (transport is decided: HTTPS, or plain HTTP on loopback — ADR-0016
§4); and the power-user escape-hatch design, which ADR-0016 §5 keeps a
profile-layer feature rather than a config-layer loophole.

Owner's idea: in the firm-internal context, engineers should NOT pick raw
models/providers directly (DeepSeek here, Claude there). Instead the org packs
and provides **named profiles** — e.g. `default`, `supersmart` — where each
profile = a curated set of models + a smart routing strategy underneath.

Design shape (to discuss):
- **Profile = named bundle**: model pool per scenario (main loop / deep-think /
  background / edit-apply / review), routing + escalation strategy, budget
  ceiling, cost class. Engineers declare *intent*; the org controls *vendors*.
- **Profiles are defined by admins in the backend control plane** (T-05) —
  tamper-resistant, versioned, auditable. Routing tables refresh from the
  server; local fallback table when offline.
- **Shipping defaults**: a curated ladder, e.g. `fast` (cheapest adequate),
  `default` (balanced), `supersmart` (frontier, escalation-gated), maybe
  `local-only` (air-gapped/private code). Naming by *capability*, never by
  vendor — the provider mix underneath can change without users noticing
  (that's the point).
- **The win**: cost governance becomes structural (T-04 — `supersmart` can
  require justification or have per-day caps); model churn disappears behind
  profiles (Amp's lesson: re-route silently as the frontier moves); support
  and benchmarking become possible ("profile X on task class Y" is measurable).
- **Precedents**: Amp's capability dial (`low`→`ultra`, no model picker —
  users loved it); CCR's scenario routing; Codex `requirements.toml` admin
  bans; Cursor enterprise model blocklists; Chinese vendors' admin-controlled
  model menus ("compliance is the product").
- **Tension to resolve**: power users *love* model freedom (Pi's mid-session
  switching is beloved; blocking Claude Max in OpenCode caused revolt).
  Resolution options: admin-granted escape hatches per team; a visible
  "served-by" line each turn (transparency without control); personal/solo
  tier keeps full freedom while firm tier is profile-gated.

Open questions:
- Profile granularity: per session, per task, or per request (with escalation)?
- Do users see which model actually served each turn? (Recommend: yes, on
  demand — transparency feeds T-02's implicit feedback loop.)
- Can engineers propose profile changes (PR against profile config) rather than
  just file tickets? (Git-native config, per role-specialized lesson.)

## T-08 · Authentication & identity (flexible, firm-internal first) — `Seed`

Seeded 2026-09-26.

Owner's intent: loongagent is a firm-internal tool, so authentication must be
**flexibly adaptable to whatever the firm already has** — SSO, service
principals, or anything else. No hardcoded auth scheme.

Design shape (to discuss):
- **Pluggable identity layer in the backend control plane (T-05)**: SSO via
  OIDC/SAML (Keycloak, Entra ID, Okta, Authentik, firm-internal IdPs), LDAP
  where legacy demands it, and **service principals** (non-human identities
  for CI, cloud workers, automations). Local dev mode: no-auth or personal
  token, clearly marked.
- **Identity is the coordination mechanism** (Codex's lesson): sessions roam
  across surfaces and devices because they belong to a *user*, not a machine.
  Also the anchor for T-02 audit ("who did what"), T-04 budgets ("whose
  quota"), and T-07 profile permissions ("which teams may use supersmart").
- **RBAC lives behind identity**: roles/groups from the IdP map to loongagent
  permissions — profile access, admin policy editing, dashboard visibility.
  Precedents: Cursor Teams (SAML/OIDC + SCIM), Codex managed config,
  GitHub governance defaults.
- **Agent identity is a first-class question**: when an async worker opens a
  PR or runs a migration, whose identity is it? Recommended: the agent acts
  as a distinct service principal *on behalf of* a human (attribution chain:
  human → session → action), never anonymously, and GitHub-style governance
  applies (requester can't approve their own agent's PR).
- **Credential handling**: provider API keys live server-side in the model
  gateway (T-05) — engineers never hold vendor keys; the backend brokers
  model access behind auth. (Also kills the "leaked key in dotfiles" class
  of incident.)
- **Flexibility contract**: auth = a documented adapter interface with 2-3
  reference implementations (OIDC, static token, service principal); a firm
  can plug its own without forking. Zero auth logic in the local client
  beyond token acquisition/refresh.

## T-09 · Hooks & deterministic enforcement — `Promoted → ADR-0007`

Seeded 2026-09-30. **Veto + trust gate decided 2026-10-04 (ADR-0007):** hooks
advise/veto at the PDP (never *are* the decider), their reasons feed back to the
model, and repo-shipped hooks are untrusted content requiring explicit,
content-bound per-repo trust opt-in. Still open: hook language (shell vs
in-process), sync vs async, and the trust UX.

**Decided 2026-10-05 (ADR-0007 §7, narrowed):** hooks support **three kinds** in
v1 — **shell / HTTP / MCP tool** — behind one verdict contract (allow / ask /
deny + reason), over a fixed lifecycle event set (session start/end,
user-prompt submit, pre/post tool use, permission requested/denied, subagent
start/stop, pre/post compaction, file changed, cwd changed). **HTTP hooks are
the one explicit exception to "nothing leaves the machine"**: opt-in,
org-pinnable, and a network action class under the PDP. **LLM and subagent hook
kinds are deferred to v2** (they spend tokens / recurse — they belong with the
Guardian reviewer and the T-23 goal evaluator). Still open: hook language for
repo-shipped hooks in practice, sync vs async, and the trust UX.

The guarantee layer where markdown rules are only advisory (synthesis T5).
Lifecycle events (session start/end, pre/post tool use, compaction, permission
decisions) fire hooks that can block, mutate, or enrich — Claude Code's
exit-code-2 pattern (block + feed reason back to the model), OpenCode's
plugin veto at the tool boundary, Kiro's git-committed agent hooks ("CI for
agent behavior"). Open questions: hook language (shell vs in-process TS/WASM),
sync vs async, trust gating (repo-shipped hooks caused Claude Code CVEs —
untrusted repos must never auto-execute).

## T-10 · Memory system — `Promoted → ADR-0006`

Seeded 2026-09-30. **Write path decided 2026-10-04 (ADR-0006):**
agent-proposed into a staging area; low-risk project facts may auto-accept;
user preferences/feedback require explicit confirmation; memories are ledger
entries and writes are events (auditable, revertible). The rest of the topic
(what earns memory, decay/TTL, scoping, cross-session memory for async
workers) is still open.

Dual authorship: human-written rules (versioned in git, team-shared) vs
agent-written memory (typed notes: user/feedback/project/reference) with a
bounded index loaded each session + topic files read on demand (Claude Code's
design); Cursor's sidecar-observer proposes memories with user confirmation.
Requirements for us: memory must be inspectable/editable/deletable (T-13 —
nothing hidden), scoped (user/project/org), and git-native where team-shared
(WrenAI lesson: org context as PR-reviewable artifacts). Open: what earns
memory, decay/TTL, cross-session memory for cloud workers.

## T-11 · Permissions & sandbox — `Promoted → ADR-0007`

Seeded 2026-09-30. **Engine decided 2026-10-04 (ADR-0007):** PDP inside the
fixed kernel at the single tool chokepoint (allow/ask/deny + the rule that
fired), structural PEPs (OS sandbox, tool runner, network egress), layered
policy with org packs winning, per-action-class autonomy ladder, sandbox
default-on with approval policy orthogonal and YOLO a visible opt-out,
approvals as first-class events with scope + structured reasons, Guardian-style
LLM pre-review deferred to v2. Still open: rule/policy language and pack schema
(AQ-9), hook trust UX, network allow-list granularity, micro-VM tier.

Original seed — per synthesis T3 (direction agreed): OS sandbox default-on
(seatbelt/bubblewrap/micro-VM), approval policy as a separate axis, YOLO as
opt-out. Extras from research: Codex's `sandbox self-test` command;
Guardian-style semantic approval reviewer (later); OpenCode's doom-loop and
external-directory guards as ordinary permission rules; role-specialized
lesson — autonomy allocated **per action** (investigate free, write gated),
varying by role profile. Replit Lemkin incident is the canonical warning:
prompts are not permission systems; safety must be structural.

## T-12 · Session tree: fork, retry, steer — `Wanted`

Seeded 2026-09-30. Owner: **"Session tree is the killer feature — I want this."**

Pi's design, adopted, with the two tree levels now named (ADR-0015): **inside**
a session, events carry `id`/`parentId` forming an **event tree** — `/tree`
navigation over paths, **retry** = a new path from the failure point with the
abandoned path retained and summarized (no pollution of the linear history),
lossless compaction (full history survives). **Between** sessions, a **fork**
creates a **child session** with lineage back to the point it came from.
**Steer vs follow-up** queues (Pi): deliver-after-current-tool vs
deliver-when-idle. **Pick**: the agent presents structured options and the
user picks (AskUserQuestion pattern). Combined with T-13, the event tree is
the user's x-ray into agent behavior — and with shadow git snapshots
(synthesis T9), rewind covers both conversation and code.

## T-13 · Observability: "what is the agent doing and *why*" — `Promoted → ADR-0006, ADR-0007, ADR-0003`

Seeded 2026-09-30. **Decided:** every block entering context is a typed ledger
entry with provenance and token cost, so "why is this here?" is a query
(ADR-0006); every policy decision records the rule that fired (ADR-0007); and
every decision is an event in the session log (ADR-0003).

Pi's founding promise, owner-endorsed: **nothing hidden, everything
inspectable, documented session format.** Concrete requirements: verbose
startup report of exactly what entered context (Pi's silent skill-loading
failure is the anti-pattern); Cursor-style context breakdown per category;
every injected token surfaced in the UI; the event log (T-02) answers "why did
it do that" — tool call, reasoning, cost, policy decision. Observability is
also T-21's teaching tool: juniors learn by watching a legible agent.

## T-14 · Teamwork & sharing — `Promoted → ADR-0010`

Seeded 2026-09-30. Extends T-03. **Rescoped 2026-10-04 (ADR-0010):**
collaboration = **shared context and knowledge via git** (AGENTS.md-style,
PR-reviewed, plus signed org knowledge packs per ADR-0008) — **not** shared live
sessions. No multi-user co-watching, co-steering, or cross-user session viewing
for now; persona handoffs travel as repo artifacts (a spec doc, not a live
session). Shared knowledge is content, never policy, so it cannot widen
permissions or budgets (ADR-0007). Sessions, personal memory, and raw
transcripts stay personal; sharing is an explicit human act. Still open:
knowledge artifact granularity and attribution, personal cross-device session
sync, org knowledge delivery (pack vs mirrored git repo), and whether any
read-only session viewing ever returns.

Session/thread sharing as durable artifacts (Amp threads: shareable URLs,
"show how the agent reached the result"); team rules/skills/memory via git;
**persona handoffs** — PM's spec artifact becomes the engineer's session
input (T-06); live co-watch and human takeover (Devin); review-in-place for
parallel agents (multi-agent-orchestration research: human review is the
3–5-agent ceiling — tiered review is our opening). Privacy: sharing is
opt-in per artifact (OpenCode's auto-sync backlash).

## T-15 · MCP & external tool ecosystem — `Seed`

Seeded 2026-09-30.

MCP supported but **lazily loaded** — Pi's critique stands (Playwright MCP =
21 tools / 13.7k tokens dumped into every context); Amp's pattern: skills
bundle their MCP config so tools enter context only when the skill triggers.
Goose's orthogonality: even built-in tools as MCP servers. Org angle (T-05):
an approved-server gateway/registry; prompt injection via MCP content is the
dominant attack class — pinning, scanning, and trust levels required.

## T-16 · Commands — `Merged → T-18` (2026-10-05)

Seeded 2026-09-30. **Decision (2026-10-05, §E3 of
[05-parity-and-synthesis](05-parity-and-synthesis.md)): commands are not a
separate concept — a command *is* a manually-invoked skill.** Claude Code
already merged them (`.claude/commands/*.md` still load, as skills). One loader,
one frontmatter contract, one directory convention; a legacy commands directory
is read as skills for compatibility. The merge contract lives in **T-18**; this
entry is kept only as the record of the merge.

Slash commands as markdown prompt templates with frontmatter (Pi/OpenCode):
`$ARGUMENTS`, shell-output injection, `@file` inclusion, `subtask: true` to
run in a subagent keeping main context clean. Claude Code's evolution note:
custom commands merged into skills — decide whether our commands and skills
(T-18) are one mechanism or two. Layers: built-in < team < user.

## T-17 · Subagents — `Promoted → ADR-0011`

Seeded 2026-09-30. **Execution model decided 2026-10-04 (ADR-0011):**
write-capable subagents run in their own **git worktree** (read-only ones share
the workspace); each is a **child session** (delegation — ADR-0015) returning a
**structured artifact** (files + summary), not a transcript; there is **one
orchestrator — the parent's visible plan**, no scheduler; concurrency is bounded
by human review attention (**3–5**, tiered review over the user's own agents).
Still open: artifact/summary schema, worktree lifecycle & GC,
external-resource declaration.

Per synthesis T6 (direction agreed): first-class but **observable and
artifact-based** — each subagent is a visible **child session** (T-12/ADR-0015),
writes a structured report artifact, returns only a summary, inherits
the parent sandbox. Built-ins: explore (read-only, fast model), plan,
general. Per-subagent model routing (T-01: cheap explore, strong build).
OpenCode trick: denied subagents are removed from the task tool's description
so the model never attempts them.

## T-18 · Skills & marketplace — `Promoted → ADR-0018`

Seeded 2026-09-30. **Commands merged in (2026-10-05, T-16 → here):** a
**command is a manually-invoked skill**, not a separate concept — one loader,
one frontmatter contract (`name` / `description` / `whenToUse` +
invocation flags such as model-invocable vs user-only), one directory
convention; a legacy `commands/` directory is read as skills for
compatibility. Two loaders and two frontmatter contracts cost more than they
buy, and Claude Code already converged on the single concept.

**Marketplace decided 2026-10-15 (ADR-0018):** content is installed from a git
checkout by name through a curated catalogue in the binary — there is no network
marketplace index, only skills are installed (hooks and MCP servers stay
per-repository trust gates), and a plugin may seat named skill bodies in the
system prompt as `bootstrap`. Still open: the firm-internal registry, trust
tiers/signatures for third-party content, and org-approved distribution.

Agent Skills open standard (Anthropic): description-only in context until
invoked; skills bundle files/scripts and (Amp-style) their own MCP config.
**Marketplace**: firm-internal registry first (org-approved, versioned,
auditable — Continue's "hub" concept is ownerless and ours to take); public
registry later. Trust: skills can carry code → same trust gating as hooks
(T-09). Tessl's post-pivot registry is proof the concept has legs.

## T-19 · IDE integration via ACP — `Promoted → ADR-0009` (direction agreed in round-2 research)

Seeded 2026-09-30. **Surface decision recorded 2026-10-04 (ADR-0009):** ACP is
the third build target (after TUI and web), served by `loongagent acp` as a
dialect of our own JSON-RPC protocol (ADR-0002); editor-side plugins are thin
wrappers, never parallel products, and no capability may be implemented
IDE-side only (exposure may lag, visibly). Still open: the branded VS Code
extension's scope and the relay/editor context-source details.

**ACP server-side from day one; no editor fork, no per-editor plugins.**
Zed, JetBrains, Neovim, Emacs + 25 agents adopted ACP; Void died, Continue
was acqui-hired — thin layers on others' platforms die. The editor is also a
context *source* (open files, selection, diagnostics — Windsurf's flow
awareness, scaled to taste). JetBrains lesson: a mature semantic index beats
text search inside IDEs — our repo map (T7) plugs in here.

## T-20 · Git-diff-centric workflow & spec-driven process — `Seed`

Seeded 2026-09-30.

Two halves of the dev process: (1) **git as the review/undo layer** — shadow
snapshots per message (synthesis T9), per-file staged diff review
(Cursor 3.0's preview-then-approve), PR as the deliverable for async work.
(2) **Spec-first with size-scaled ceremony** — terse repo-native markdown
specs, auto-triage when to spec at all (a bug fix ≠ 4 stories/16 criteria),
requirement↔task traceability links; NEVER spec-as-source (Tessl's $125M
lesson). Specs are the handoff protocol between personas (T-06/T-14).

## T-21 · Layered approachability: junior-proof defaults, senior-proof depth — `Promoted → ADR-0007, ADR-0008`

Seeded 2026-09-30. **Decided:** junior-proof defaults come from the per-action
autonomy ladder (ADR-0007 §4); every layer is openable through monotonic config
layers that a lower layer cannot loosen (ADR-0008); and YOLO is a visible,
recorded opt-out (ADR-0007 §5). Self-modification covers the extension surface —
skills, commands, rules, plugins — not the kernel (ADR-0013 §5).

Owner: avoid Pi's steep learning curve. Pi converts experts but "has no
guardrails to catch" newcomers. Resolution: **defaults carry juniors**
(batteries on, sandbox on, curated profiles, guided first session — T-06
newbie persona) while **every layer is openable for seniors** (extensions,
raw config, self-modification à la Pi's `/reload`). The harness is pluggable
with good defaults — "nobody knows the right shape yet," so both we and our
users must be able to re-shape it cheaply. Onboarding is a designed
progression, not a cliff.

---

## T-22 · LSP-backed code intelligence — `Seed`

Seeded 2026-10-05 (from [05-parity-and-synthesis](05-parity-and-synthesis.md)
§A1/§B5). Claude Code (via LSP plugins) and OpenCode give the model
go-to-definition, find-references, and **type errors after an edit** — the
cheapest possible input to the verification loop, and what stops an agent
guessing at symbol names. Our backlog had **no entry for this**. Design
questions: which language servers to bundle, detection, context cost of
diagnostics, how it composes with grep/repo-map retrieval (ADR-0006), and
whether diagnostics enter context automatically after a write (CC's behaviour)
or only on request.

## T-23 · Persistent goal with evaluator — `Seed`

Seeded 2026-10-05 (§A8/§B4). Claude Code's `/goal`: a completion condition that
survives across turns, re-checked by an **evaluator** after every turn, with
Stop hooks gating turn end and an 8-block circuit breaker so the agent cannot
spin forever. Worth absorbing because it turns "keep going until X" from prompt
text into a mechanism — and it exercises seams already decided: goals as events
(ADR-0003), goal state visible (T-13), turn-end gating as a hook (T-09), and the
evaluator's cost metered (ADR-0006).

## T-24 · Module metadata & lifecycle — `Seed`

Seeded 2026-10-05 (§B6/B7/B12, §D2). With the extension architecture reframed as
**our own iteration architecture** (ADR-0004 Context), module metadata becomes
product surface we need regardless: each module **declares its config schema**
and the surfaces render the settings UI from it (DSH's
`installSection`/`describe`); **hot reload** for skills, commands and modules so
authors iterate without restarting (Pi's `/reload`); and **declarative panels** —
a module declares "render this server-side projection as a panel/tab/column" and
the shell owns the components, so TUI, web and IDE all render it with no
third-party client code. That keeps ADR-0009's thin-client rule and "no
single-surface capability", while letting **us** add panels without forking a
surface.

## T-25 · Interaction smalls — `Seed`

Seeded 2026-10-05 (§A2/§A3/§A7/§B11): `/btw` (side question that never enters
history), `Monitor` (background script output surfaced as events), prompt-**cache
hit rate** in usage, `/init` and `/doctor` (generate / trim instruction files),
`@path` imports in instruction files (depth-limited, approval-gated), and the
description token-budget warning for skills/subagents. Individually small;
collectively the difference between "feature-complete" and "feels finished".

---

## T-26 · Cross-crate helper duplication — `Seed`

Seeded 2026-10-16 (navigability review §3 — **recorded, deliberately not
fixed**). Five small helpers are written again in each crate that needs them —
thirteen definitions in all:

- `now_ms` — `crates/hooks/src/run.rs` (the `pub` one),
  `crates/eventlog/src/lib.rs` (`i64`), `crates/plugin/src/lib.rs` and
  `crates/control/src/lib.rs`;
- `expand_home` — `crates/plugin/src/lib.rs` and `crates/config/src/lib.rs`;
- `first_line` — `crates/cli/src/main.rs` and
  `crates/control/src/bin/loongagent-control.rs` (byte-identical), plus a
  different, length-bounded third in `crates/hooks/src/run.rs`;
- `truncate` — `crates/context/src/store.rs` and `crates/tui/src/lib.rs`;
- `clip` — `crates/tui-app/src/lib.rs` (columns, CJK-aware) and
  `crates/cli/src/main.rs` (characters).

**Not consolidated now:** one home for these means a **new shared public
surface** — a crate or a `pub` module that each of those crates depends on — and
that is an API change, not a tidy-up. Thirteen definitions of five ten-line
helpers do not justify one: the new dependency edge and its public commitment
cost more than the copies do. Revisit when a sixth caller appears, or when one
copy has to change in a way the others must follow.

---

*Next topics go here. Just say the idea — capture is automatic.*
