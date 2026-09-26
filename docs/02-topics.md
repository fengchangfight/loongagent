# 02 — Topic List (idea backlog)

- **Status:** Living document — append ideas as they come; promote to numbered
  design docs or ADRs when discussed.
- **Last updated:** 2026-09-26
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

---

*Next topics go here. Just say the idea — capture is automatic.*
