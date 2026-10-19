# ADR-0012 — Backend control plane: small, boring, and explicit about what it does not do

- **Status:** Accepted
- **Up:** [../design/README.md](../design/README.md)
- **Date:** 2026-10-04
- **Decides:** AQ-5 (docs/03-architecture.md) — backend control-plane shape and
  scope; also records that personal cross-device session sync is not built for
  now

## Context

T-05 originally imagined a rich backend: audit and statistics, a routing/policy
brain, a model gateway, orchestration, team sharing, and session sync across a
user's devices. Three later decisions removed most of that surface before this
ADR was written:

- **ADR-0010** removed multi-user shared sessions — collaboration is git-shared
  knowledge, so the backend is not a session bus or a document server.
- **The 2026-10-04 orchestration decision** removed cross-user and scheduled
  orchestration for lack of a use case — so it is not a job scheduler either.
- **The owner's decision on personal cross-device session sync** (see §6) removes
  the last item that would have put session *state* on the server.

What remains is small, and the design principle is to **keep it small on
purpose**: T-05's standing constraints are control-plane-not-data-plane, code
never transits the backend as files, offline is never dead — local mode runs with
no control plane, gateway mode needs the gateway only for model calls (ADR-0005)
— and every byte leaving the machine is visible and opt-in (T-02). A smaller
backend is a smaller trust surface and a shorter security review.

## Decision

### 1. Shape: one backend binary, modular inside
A single backend binary with internal module boundaries where scaling actually
differs — **model gateway / dashboards & statistics / pack distribution**.
Microservices are an ops tax not yet earned.

**Two different "same code" claims, kept apart** (they were previously conflated,
which is misleading):

- **Managed vs self-hosted control plane: literally the same code.** One backend
  binary, deployed as a managed service or on the firm's own hardware. This is
  T-03's claim, and it is exact.
- **Control plane vs local agent: the same repository, not the same binary.**
  Both live in one crate workspace and **share the crates where agreement is
  correctness-critical** — the protocol/event schema (ADR-0002/0003), the pack
  envelope and its sign/verify path (ADR-0008), policy and config resolution
  incl. merge kinds (ADR-0005/0008), provider adapters and the transformer chain
  (ADR-0004), and token counting / cost math (ADR-0006). They are then built as
  **two binaries** with genuinely different deployment code: the local agent
  adds the kernel loop, sandbox, tools, and surfaces; the control plane adds
  identity/SSO, Postgres rollups, pack issuance, quotas, and dashboards.

Why the sharing is not optional: the two sides are **deployed independently**
(canary and stable channels, ADR-0009), so a duplicated implementation *will*
diverge. A divergent pack verifier rejects valid packs or accepts invalid ones;
divergent cost math makes the local meter disagree with the bill; a divergent
policy resolver makes an org hard cap silently unenforced (ADR-0005, ADR-0007). Sharing a
crate turns "client and server agree" into a compiler-enforced property instead
of a test-suite hope. **"Same binary in another mode" is explicitly not
required** — that reading of "same code" was an over-reach and is retired here.

### 2. Data: Postgres + object storage
Postgres for users, policies, and event rollups; object storage for shared
prompt-cache blobs and signed packs. Boring and sufficient; no exotic store
until something forces one.

### 3. Uplink minimization, opt-in by category
Clients push events in **batches (delta, resumable)**; the server pushes **only**
policy/profile/knowledge packs. The uplink contract is an **allow-list**, not a
filter:

- **Default uplink = audit events + aggregate statistics** (T-02).
- **Session bodies and transcripts are not in the contract** — no feature
  requires them.
- Every category is individually visible and disable-able; nothing is sent
  silently. Offline is always fully functional with the last synced packs.

### 4. Identity: the person's rendezvous, not session continuity
Flexible firm-internal authentication — SSO, service principal, or whatever the
firm already runs (T-08). The account is the **rendezvous for the person**
(policy, profiles, metering, entitlement) — deliberately **not** a mechanism for
carrying session state between machines. Codex's lesson (identity, not file
sync, is the coordination mechanism) applies to *who you are*, not to *what your
sessions contain*.

### 5. The complete job list — and the explicit non-jobs
**Does:** identity/auth (T-08); policy & profile pack distribution (ADR-0008);
the **model gateway** — provider keys, metering, shared prompt cache (the big
cost levers, ADR-0005/T-05); **opt-in aggregated** audit and statistics (T-02).

**Does not** (each retired by an explicit decision): session bus or live
collaboration server (ADR-0010); document/knowledge server (ADR-0010 — knowledge
travels as git and signed packs); **job scheduler** for cross-user or scheduled
orchestration (2026-10-04, no use case); **session-state sync service** (§6).

### 6. Personal cross-device session sync: not for now
**Owner decision, 2026-10-04: do not build it.** Sessions remain a **purely
local event stream** (ADR-0003). Consequences and alternatives, recorded so this
is a decision rather than an omission:

- A second machine starts with fresh sessions **unless the user moves a session
  bundle**: cross-device *continuity* exists via first-class **export/import**
  (ADR-0014); cross-device *sync* does not. Session "roaming" (ADR-0009) remains
  **per machine**, across that machine's surfaces.
- **Export/import (ADR-0014) is the continuity path** — a file the user moves,
  not a service. Git remains a poor fit for a high-frequency append-only log
  (constant conflicts); the bundle format exists for exactly this, and git stays
  right for *knowledge* artifacts.
- Sync returns only with a concrete need, and it would be the one item that adds
  session state to the backend's responsibility.

## Consequences

- **The trust claim becomes short and reviewable.** The backend holds identity,
  policy, provider keys, and aggregates — **no conversation content by
  default** — and that is enforceable because the uplink is an allow-list.
- **The gateway is the residual concentration point, and we say so.** Centralized
  keys, metering, and shared cache inherently mean model traffic passes through
  the backend; that is visible (served-by, cost events, ADR-0005/0006) and
  avoidable for those who refuse it by **self-hosting** (T-03).
- **v1 is not blocked on the backend.** A solo install runs with an empty org
  layer (ADR-0008); the control plane can ship later without changing the
  client.
- **Cross-device continuity is file-based, never automatic:** export/import
  (ADR-0014) covers moving a session between machines; there is no sync, so the
  backend still holds no session state.
- **Deferred:** concrete auth protocol (T-08), rollup schema and retention
  policy, cache-sharing policy details, managed-vs-self-hosted packaging, and
  gateway failover/quotas.
