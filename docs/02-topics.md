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

---

*Next topics go here. Just say the idea — capture is automatic.*
