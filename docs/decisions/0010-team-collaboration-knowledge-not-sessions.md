# ADR-0010 — Team collaboration: shared knowledge through git, not shared live sessions

- **Status:** Accepted (scope direction; sharing mechanics deferred)
- **Date:** 2026-10-04
- **Decides:** T-14 collaboration model (docs/02-topics.md); narrows AQ-5
  control-plane scope and removes the collaboration driver from AQ-10's cloud
  half

## Context

AQ-5 was deferred on 2026-09-30 precisely because the team-collaboration shape
was unclear — the T-14 seed had collected the usual suspects: shareable session
URLs (Amp), live co-watching, human takeover (Devin), and persona handoffs
carried by live sessions. Designing the control plane around those features
would have made the server a session bus, which sits badly next to the
local-first constraints already locked: control plane, not data plane (T-05);
code never transits the backend; every byte leaving the machine is visible and
opt-in (T-02, T-14 — OpenCode's auto-sync backlash is the cautionary tale).

The owner reconsidered the problem on 2026-10-04 and set the direction: **for
now, do not build multi-person real-time shared agent sessions.** Collaboration
should work the way it already works for Claude Code users — **shared context
and a shared knowledge base, managed through git, AGENTS.md-style.**

This fits decisions already made. Repo-shipped rules are *content*, and
ADR-0007 established that content is advisory while permissions are enforced
locally — so a shared rule can never quietly widen anyone's access. ADR-0008
already defines how content layers and merges (add-only) and how org-wide
artifacts are distributed (signed packs). ADR-0009's "one writer + N observers"
was about **one person's surfaces**, not several people in one session; this
ADR keeps that boundary explicit.

## Decision

### 1. Non-goals, for now
- No multi-user live session sharing, co-watching, or co-steering.
- No cross-user session viewing and no server-mediated shared session state.
- No "handoff" that requires the recipient to open the sender's session.
  Persona handoffs (T-06) happen as **artifacts in the repo** — a PM's spec doc
  becomes the engineer's own session input.

### 2. Collaboration = shared context and knowledge, via git
The collaboration surface is the repository, not a server feature:
- **Project layer:** rules and instructions committed in the repo, **AGENTS.md
  native** (ADR-0008), plus commands, skills, specs, research and decision docs.
- **Org layer:** org-wide knowledge distributed as **signed packs** (ADR-0008),
  authored by admins, PR-reviewable like any other artifact.
- **Mechanism:** review and commit. The git-native path already used for
  `AGENTS.md`/`CLAUDE.md`/skills repos — no new protocol, no new service.

### 3. Shared vs personal — a hard boundary
| Shared (git) | Personal (never auto-shared) |
|---|---|
| Rules, instructions, commands, skills | Sessions and their trees |
| Curated project/org knowledge, specs, ADRs, research | Personal memory |
| Org policy/profile packs (ADR-0008) | Raw transcripts |
| Aggregate statistics (opt-in, T-02) | Per-decision routing/telemetry detail |

### 4. Sharing is an explicit human act
The agent may **propose** a knowledge artifact — a diff, a branch, a PR — but a
human reviews and commits it. Nothing from a session enters the shared base
silently. This is ADR-0006's propose-and-confirm extended to the team boundary,
and it is what keeps the privacy claim ("nothing leaves the machine that you
did not commit or opt into") stateable in one sentence.

### 5. Shared knowledge is content, never policy
Repo rules are advisory; repo-shipped **hooks remain untrusted and gated**
(ADR-0007). Shared knowledge cannot widen permissions, autonomy class, or
budgets — those are narrow-only policy keys (ADR-0008) and org-owned. A wrong
or malicious shared rule can mislead the model, but it cannot escalate
privilege; this is the same containment property ADR-0007 gives prompt
injection.

### 6. Attribution
Shared knowledge carries provenance: who or what produced it, and which
document/session it came from, so a reviewer can judge it and a consumer can
trace it (T-13). Composes with the context ledger (ADR-0006) on the reading
side: a shared entry that lands in context is still an ordinary, visible ledger
entry.

### 7. Consequences for the control plane (AQ-5 narrows, does not close)
The backend's job becomes: identity/auth (T-08), model profiles and policy
packs (ADR-0008), the model gateway (keys, metering, shared cache), and
**opt-in aggregated** audit/statistics (T-02). It is **not** a collaboration
server, **not** a session bus, and **not** a document server — knowledge moves
as git and signed packs. AQ-5 remains open, but with a much smaller and more
answerable scope.

### 8. Consequences for AQ-10 (cloud/async)
The cloud-runner half of AQ-10 loses its main near-term justification: with no
shared live sessions, "async" is no longer pulling collaboration along with it.
Local parallelism — worktree isolation, subagents as session-tree branches,
artifact reports, the 3–5 agent human ceiling, tiered review — stands on its own
and can be decided now. Runners return only if a fresh driver appears
(long-running scheduled jobs, org-wide batch work), and they are a *personal*
capability at that point, not a collaboration feature.

### 9. The door is not nailed shut
Revisiting shared sessions requires an explicit owner decision. Nothing in the
architecture precludes it later: sessions are already event streams with a
documented on-disk format (ADR-0003), so a future sharing feature would be
additive rather than structural — which is the right way to leave it.

## Open sub-questions (deferred)
- **Personal cross-device session sync** (Codex's identity-based model) — this
  is a *personal* continuity problem, not multi-user collaboration. Decide
  separately, probably with AQ-5.
- Knowledge artifact granularity: one AGENTS.md vs a knowledge directory with
  topic files, and where curated knowledge ends and agent memory begins.
- Attribution fields, and how much session context a proposed artifact may
  carry.
- Org-wide knowledge delivery: signed pack (ADR-0008) vs a git repository the
  control plane mirrors.
- Whether any read-only "show me a colleague's session" ever returns, and
  behind which privacy gates.

## Consequences

- **The privacy story becomes short.** Nothing leaves the machine except commits
  the human made and opt-in aggregate statistics — no session bus to explain
  away, no co-watching surface to secure.
- **Less to build before v1.** The control plane shrinks to identity, packs,
  gateway, and stats; the deferred AQ-5 gets materially easier.
- **Collaboration quality depends on review culture, not server features** —
  which matches how teams already share rules and skills today, and keeps
  "harness is the product" honest.
- **The capability lost is real-time pairing on one agent run.** Mitigations if
  it ever hurts: screen sharing exists, and sessions are reproducible artifacts
  already; neither is a reason to build a session bus now.
- **Tiered review stays, rescoped.** It was justified by the 3–5 parallel-agent
  ceiling; with no co-watching, it reviews **the user's own** parallel agents,
  not colleagues'.
