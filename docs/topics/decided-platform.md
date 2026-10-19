# Decided topics — control plane and deployment

- **Up:** [README.md](README.md)
- **Authority:** [02-topics.md](../02-topics.md)
- **Code:** `docs/topics/decided-platform.md`
- **Verify:** `python3 scripts/check-docs.py`

These topics are settled: `Promoted → ADR-NNNN` names the decision. The full
text is kept, heading and all, so a `T-NN` citation still resolves. This file
holds the control plane and deployment decisions; [decided.md](decided.md) holds
model, cost and observability, and [decided-extension.md](decided-extension.md)
holds safety, people and extension. The index of all 26 is [README.md](README.md).

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

**Rejected — then partly reversed (ADR-0005, revised 2026-10-18):** the original
worry was a **routing & policy brain** divorced from the machine that holds the
keys — server-side live routing while the client holds credentials and does the
calling. That split is still refused: routing follows the keys. What the gateway
now owns in **gateway mode** — model choice, ceilings and task-class policy,
metered and enforced by refusal — is not that brain; it is the same component
that holds the key and pays. Local mode has no router at all. See
[05-parity-and-synthesis](../05-parity-and-synthesis.md) §D (D10).

Owner's idea: loongagent is **not a local-only tool**. There should be a rich
backend that local clients *smartly* communicate with — audit, statistics, and
the model layer's route/cost decisions. ADR-0005's 2026-10-18 revision delivered
exactly that lane for routing: the gateway owns it in gateway mode, and nothing
above the model layer moved off the client.

What the backend owns (ADR-0012), and what it does not:
- **Audit & statistics aggregation** (T-02): org-wide event streams, cost
  dashboards, usage analytics.
- **Routing & policy brain** (T-01/T-04): now real in **gateway mode** — the
  gateway picks the model, applies ceilings and task-class policy, meters, and
  refuses over budget (ADR-0005 §2). Centrally managed and tamper-resistant by
  construction: a client cannot route around a component it does not call.
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
  titles. **Local mode is the offline answer**: no control plane, no uplink, the
  user's own key or endpoint (ADR-0005 §1). **Gateway mode trades that for
  central keys** and is online-only for models — cached packs keep policy and
  tools working, but no gateway means no model call (ADR-0005 §4). What must
  never happen is a managed install silently calling a provider directly with a
  key on disk.
- When connected, sync is smart: batch, delta, resumable — and every byte that
  leaves the machine is visible/opt-in per our local-first principle.
- Deployment shapes (T-03): managed cloud backend OR self-hosted team server,
  same code. Enterprise/privacy customers run their own.
- **Decided:** the agent *core* (tool execution, sandbox, context, log) always
  stays local near the code, with the backend as pure control plane. Model
  prompts are the one code-bearing payload that transits the gateway, in gateway
  mode, because the provider sees them anyway; no other file does. There is no
  cloud runner to make an exception for (dropped, 2026-10-04).
*Next topics go here. Just say the idea — capture is automatic.*
