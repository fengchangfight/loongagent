# Decided topics — model, cost and observability

- **Up:** [README.md](README.md)
- **Authority:** [02-topics.md](../02-topics.md)
- **Code:** `docs/topics/decided.md`
- **Verify:** `python3 scripts/check-docs.py`

These topics are settled: `Promoted → ADR-NNNN` names the decision, and a
`Merged → T-NN` topic names where it went. The full text is kept, heading and
all, so a `T-NN` citation still resolves. The 16 Promoted/Merged topics did not
fit one 250-line file, so they are split by theme: this file holds the model,
cost and observability decisions; [decided-platform.md](decided-platform.md)
holds the control plane and deployment; [decided-extension.md](decided-extension.md)
holds safety, people and extension. The index of all 26 is [README.md](README.md).

## T-01 · Smart cascading model routing — `Promoted → ADR-0005`

Seeded 2026-09-26. **Execution point revised 2026-10-18 (ADR-0005): routing
follows the keys.** Local mode has no router — the user picks a model and
switches with one click; gateway mode routes at the gateway, which holds the
keys, meters, and refuses over caps. Still open: the gateway's routing policy
format, the client's intent schema, and what counts as a bad decision.

Idea (gateway mode): route work across models intelligently instead of one model
per session; local mode deliberately keeps the user's single explicit pick.

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
audit/statistics, under an allow-listed uplink (ADR-0012 §3/§5). The routing
feedback loop no longer needs that stream: in gateway mode the gateway is already
in the request path and sees cost and outcome directly (ADR-0005 §2); in local
mode nothing about routing leaves the machine. Still open: which statistics are
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

## T-04 · Cost governance (anti-waste by design) — `Promoted → ADR-0005, ADR-0006, ADR-0007, ADR-0009`

Seeded 2026-09-26. **Mechanisms decided:** in gateway mode, cheap-first
escalation and task-class policy at the gateway, every escalation an auditable
event with a reason (ADR-0005 §2); hard caps refused at the gateway and still
`deny`ed locally by the PDP (ADR-0007 §3); per-category budgets and a token
floor, visible before the call (ADR-0006 §3); cost-affecting changes treated as
breaking (ADR-0009 §6). **Local mode is outside org cost governance by
construction** — the user's wallet, no caps (ADR-0005 §1). Still open: the $0
path for trivial requests (the cheap local classifier) and the gateway policy
language.

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

## T-07 · Model profiles, not model pickers — `Promoted → ADR-0008, ADR-0016`

Seeded 2026-09-26. **Distribution decided 2026-10-04 (ADR-0008):** the org
layer is admin-authored in the control plane and shipped to local installs as a
**signed, versioned pack** (same envelope as policy/context packs); the local
install verifies the signature, caches the pack, and stays functional offline
with the last synced version. **Routing follows the keys (ADR-0005, revised
2026-10-18):** in gateway mode the profile is the intent the gateway routes by,
and its routing policy is server-side; in local mode the user is the picker and
no pack exists.
Unsigned artifacts can never occupy the control layer (above org). Still open: the **policy and
context** payload schemas — the envelope is built for them, the payload schemas
are not written (ADR-0016) — while the *profiles* payload schema **is** written
and used to verify (`PROFILES_SCHEMA`, `crates/pack/src/lib.rs`); pack **update
cadence** (transport is decided: HTTPS, or plain HTTP on loopback — ADR-0016
§4); and the power-user escape-hatch design, which ADR-0016 §5 keeps a
profile-layer feature rather than a config-layer loophole.

Owner's idea: in the firm-internal context, engineers should NOT pick raw
models/providers directly (DeepSeek here, Claude there). Instead the org packs
and provides **named profiles** — e.g. `default`, `supersmart` — where each
profile = a curated set of models + a smart routing strategy underneath. This is
**gateway mode**; local mode is the explicit exception where the user does pick,
and owns the consequence (ADR-0005 §1).

Design shape (to discuss):
- **Profile = named bundle**: model pool per scenario (main loop / deep-think /
  background / edit-apply / review), routing + escalation strategy, budget
  ceiling, cost class. Engineers declare *intent*; the org controls *vendors* —
  and in gateway mode the gateway executes the strategy (ADR-0005 §2).
- **Profiles are defined by admins in the backend control plane** (T-05) —
  tamper-resistant, versioned, auditable. There is no local routing table to
  fall back to in gateway mode: no gateway means no model call (ADR-0005 §4).
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
- **Tension, resolved by the two modes (ADR-0005)**: power users *love* model
  freedom (Pi's mid-session switching is beloved; blocking Claude Max in OpenCode
  caused revolt). Local mode keeps that freedom at the user's own cost; gateway
  mode is profile-gated with a visible "served-by" line each turn. Admin-granted
  escape hatches per team remain a possible middle.

Open questions:
- Profile granularity: per session, per task, or per request (with escalation)?
- Do users see which model actually served each turn? (Decided: yes — the
  gateway reports it and the local log records it; ADR-0005 §2.)
- Can engineers propose profile changes (PR against profile config) rather than
  just file tickets? (Git-native config, per role-specialized lesson.)

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
