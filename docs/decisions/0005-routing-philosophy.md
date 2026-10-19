# ADR-0005 — Routing philosophy: routing lives where the keys live

- **Status:** Accepted (revised in place, 2026-10-18; the 2026-10-02 version is
  quoted at the end, not silently dropped)
- **Up:** [../design/README.md](../design/README.md)
- **Date:** 2026-10-02 (original) · 2026-10-18 (revision)
- **Decides:** AQ-6 routing-execution question (docs/03-architecture.md)

## Context

The original decision (2026-10-02) put the live routing decision in the local
brain and gave the server a learning role: local reports what it routed, the
server evaluates and ships policy packs. It was chosen for two properties — no
authority seam when connectivity changes, and identical behaviour online and
offline.

The product has since settled into two modes — **pure local** and **gateway**
(the managed mode's data path; `mode = "local"` vs `mode = "managed"` in config)
— and the force that decides where routing must happen turns out not to be
connectivity but **credentials**.

- In gateway mode the firm's provider keys live at the gateway. If the
  forwarding step ran on the engineer's machine, those keys (or an equivalent
  bearer credential) would have to be distributed to every laptop — in config
  files, environment variables, process memory, backups. One compromised laptop
  would then leak a firm-wide credential, and rotating it would mean touching
  every machine. The only structural fix is that the machine holding the key is
  the machine making the call.
- In local mode there is no firm credential at all: the user brings their own key
  or a local endpoint. There is nobody to route on their behalf, and nothing to
  learn from centrally.

So the split is by mode, and the rule inside each mode is the same:
**whoever holds the key decides.**

## Decision

**Routing follows the keys.** The component that holds the provider credentials
is the component that calls the provider — and therefore the component that picks
the model. Two modes implement one rule.

### 1. Local mode — explicit, user-controlled, no routing engine

Pure local: the user's own credentials (or a local endpoint such as Ollama), no
control plane, no pack sync, no uplink. Routing is **manual**: one explicit model
at a time, switchable with one click, mid-session. No automatic escalation, no
fallback chain, no silent re-route, and no learning loop fed from this machine.
"What you picked is what runs" is the feature: the user can account for every
model the session used because they chose it. The model actually used per turn is
still an event in the local log (ADR-0003), so local statistics and cost meters
work unchanged.

This is the deliberate exception to T-07's "profiles, not pickers" — that rule
describes gateway mode, where choosing on the user's behalf is the gateway's job.

Local mode is outside org cost governance **by construction** — the user's
wallet, not the firm's. That is a deliberate trade, not an oversight: firms that
need metering and caps must run gateway mode.

### 2. Gateway mode — the gateway decides, because it holds the key and pays the bill

Every model request goes to the remote **model gateway** (ADR-0012; managed or
self-hosted — the same code either way). The gateway:

- holds the provider keys — they never reach a client;
- chooses the model per request within org policy, from the client's *intent*
  (task class, profile, budget, required capabilities), never from a credential;
- meters the call, enforces hard caps **by refusing**, and shares the prompt
  cache across requests. The kernel's PDP keeps denying locally per ADR-0007;
  the gateway's refusal is the part a client cannot talk its way around.

The client sends intent and records the model the response reports as used; that
lands in the local event log like any other decision, so "what did this session
use, and what did it cost" is still a local query. This makes the old
report-then-learn loop structural: the gateway is already in the request path, so
evaluation and policy updates need no reporting protocol — it sees the call, the
cost, and the outcome report that follows. Policy packs (ADR-0008, ADR-0016)
still shape the local install in this mode — permissions, profile menus, PDP
inputs; the gateway's own routing policy is server-side, not a pack. Local mode
has no control plane and therefore no packs.

Provider adapters run at the gateway in gateway mode and locally in local mode —
the same shared crate in two processes (ADR-0012). What stays local in both:
context assembly (ADR-0006), the tool loop, the PDP and sandbox (ADR-0007), and
the session log. The gateway is a model broker, not a tool executor; it never
sees a tool call before the kernel has decided it.

### 3. The mode is an explicit choice, never a runtime fallback

A local run resolves only local layers (`local.toml`), a managed run only the
managed ones; the managed layer cannot set `mode`, and neither mode can silently
inherit the other's policy (ADR-0008 layering). Choosing local on a managed
install is a deliberate act (`loongagent tui --local`), never something a config
file or a connectivity change does on the user's behalf. The authority seam
therefore moves from *connectivity* to an *explicit mode choice* — and if a firm
needs local mode disallowed, that is an org-policy question stated where the
policy lives, not a branch buried in the request path.

### 4. Offline, restated

The old guarantee "offline is never dead" is now **mode-specific**:

- **local mode** works offline as far as the user's own endpoint does — a local
  model works air-gapped; the user's own remote provider may not;
- **gateway mode** is online-only for models: no gateway, no model call. Tools,
  sandbox, event log, and session resume all still work locally, but the agent
  cannot think without the gateway — the price of never holding the key.

This is a real trade, taken knowingly: availability depends on the gateway in
gateway mode, and the gateway is the concentration point ADR-0012 already named.

**Transition (2026-10-18).** Until the gateway is built, a managed profile can
still name a provider endpoint directly and resolve its key on the client
(ADR-0008 §5). That configuration is the interregnum this ADR retires: it is
visible and temporary, and it does not change the rule — the key ends up on the
client, which is precisely the leak gateway mode exists to remove.

## Consequences

- **Firm credentials never distribute.** A lost laptop cannot leak a provider key,
  and rotation is a gateway operation, not a fleet-wide one.
- **Cost governance gets stronger where it exists.** A local PDP can be argued
  with; the key holder can simply refuse. Caps and metering live where the bill
  is.
- **Less machinery, fewer questions.** No local router, no escalation heuristics,
  no "why did it silently switch models" — local mode answers with a picker,
  gateway mode answers with policy plus a recorded decision.
- **The seam moved; it did not disappear.** Online/offline is no longer the
  authority boundary; the mode is.
- **Per-request routing survives, but only where keys live.** Cheap-first
  escalation, per-task model choice, and cache sharing are gateway policy now.
  They were never mostly about locality — they were about metering, and metering
  lives with the key.
- **Local visibility is unchanged.** The gateway reports which model ran and what
  it cost; the local log records both.
- **The data path is honest about itself.** In gateway mode model prompts — code
  included — transit firm infrastructure, exactly as they already did through the
  model gateway in ADR-0012. What the mode adds is that this is now the only
  model path on that install; the alternative, keys on the laptop, is worse.

## Earlier version (2026-10-02, superseded)

> The local brain is always responsible for the immediate routing decision — even
> when connected. The server never makes live routing calls. The server's role is
> to enhance, learn, and close the feedback loop: local reports its routing
> decisions on sync; the server evaluates them (cost, outcome, verification),
> updates policy when they weren't good, and delivers policy packs so local is
> smarter next time. Rationale: no authority seam at connectivity boundaries, and
> misuse is visible after the fact rather than prevented in-band.

It was superseded because it made routing authority depend on *not* holding the
credentials — stable offline, but impossible to secure in gateway mode, where the
key must stay on the gateway and the request must go there anyway. The
properties it protected (no seam at connectivity, identical behaviour) are
preserved in the mode-specific form above: one rule per mode, and the rule is
about keys.

## Deferred

Gateway routing policy format and intent schema; cache-key scope across users;
whether a gateway-mode user may attach a personal BYOK key (default: no);
whether a managed install may forbid `--local`; local-mode credential storage
details; gateway failover and quotas.
