# 02 — Topic List (idea backlog)

- **Status:** Living document — append ideas as they come; promote to numbered
  design docs or ADRs when discussed.
- **Last updated:** 2026-09-26 (T-05 added)
How this works: the owner drops raw ideas in conversation; they land here as
topics with a one-line seed connecting them to what the research already knows
(`research/`, `01-landscape-synthesis.md`). When a topic gets discussed in
depth, it graduates to its own `NN-<topic>.md` or an ADR in `decisions/`, and
its status here changes to **Promoted**.

Legend: `Seed` (captured, not yet discussed) · `Discussing` · `Promoted → <doc>`

---

## T-01 · Smart cascading model routing — `Seed`

Seeded 2026-09-26.

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

## T-02 · Audit & statistics & implicit feedback loop — `Seed`

Seeded 2026-09-26.

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

## T-03 · Deployment & ways of using (local, cloud, team sharing, cloud statistics) — `Seed`

Seeded 2026-09-26.

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

## T-04 · Cost governance (anti-waste by design) — `Seed`

Seeded 2026-09-26.

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

## T-05 · Rich backend / control plane (not a local-only tool) — `Seed`

Seeded 2026-09-26.

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

## T-06 · Per-role UX: the "this is THE tool for me" moment — `Seed` (HIGH PRIORITY)

Seeded 2026-09-26. Owner emphasis: **"I cannot emphasize more about how
important UX is for the success of this project."**

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

## T-07 · Model profiles, not model pickers — `Seed`

Seeded 2026-09-26.

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

---

*Next topics go here. Just say the idea — capture is automatic.*
