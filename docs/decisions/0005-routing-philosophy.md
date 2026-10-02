# ADR-0005 — Routing philosophy: local decides, server learns

- **Status:** Accepted (philosophy; implementation details deferred per owner)
- **Date:** 2026-10-02
- **Decides:** AQ-6 routing-execution question (docs/03-architecture.md)

## Context

Model routing (T-01/T-04/T-07) needs an execution point. The naive split —
server routes when connected, client falls back when offline — creates two
problems: an authority seam at connectivity changes, and a policy-integrity
question when the client decides alone. The owner set the philosophy directly:
the local brain and the server **collaborate**, but with distinct roles.

## Decision

**The local brain is always responsible for the immediate routing decision —
even when connected.** The server never makes live routing calls.

The server's role is to **enhance, learn, and close the feedback loop**:

1. Local decides what to route, in the moment, every time.
2. Local **reports its routing decisions** to the server (batched, on sync —
   same event stream as ADR-0003).
3. The server **evaluates** whether those were good decisions (cost, outcome,
   verification results, escalation patterns).
4. When they weren't, the server **updates policy** and **delivers policy
   packs** to local installs.
5. Next time, the local brain is smarter.

## Consequences

- **No authority seam at connectivity boundaries** — local decides identically
  online and offline; offline is just "policy pack slightly older."
- The offline policy-integrity tension dissolves: misuse is not prevented
  in-band but is **visible after the fact** (routing decisions are reported
  events; audit catches abuse on sync).
- The feedback loop (T-02) and routing (T-01) become one mechanism: the event
  stream is both the audit trail and the training signal for policy.
- Escalation events (cheap→strong model, with reasons) are first-class
  learning signals for the server.
- Deferred to later discussion (owner's call): policy-pack format and
  versioning, which evaluation signals count as "bad decision," update
  cadence, and how org hard-caps (T-04) interact with local authority —
  hard caps likely remain the one case where a policy pack *forbids* rather
  than advises.
