# ADR-0009 — Surfaces: one artifact, many faces, and no capability that lives in a face

- **Status:** Accepted
- **Date:** 2026-10-04
- **Decides:** AQ-11 surfaces question (docs/03-architecture.md) — thin-client
  rule, build order, installation/update story, multi-surface attach semantics

## Context

The owner's opening requirement was to support every surface (TUI, web, desktop,
IDE) and every role, and AQ-1 already committed to one binary that is always
client/server. AQ-2 made clients speak JSON-RPC 2.0 and made ACP a dialect of
our own protocol rather than a second stack. ADR-0005…ADR-0008 pushed routing,
context assembly, permission decisions, and configuration resolution all into
the server — which means any agent logic that appears in a surface is by
definition duplicated logic that can drift from the guarantees.

Two owner decisions are already in force: **desktop and CI are dropped** as
product surfaces (2026-09-30), and **code is always local** for browser users —
the web UI, however reached, drives the user's local agent server and local code
(2026-09-30).

Round-2 research adds the ecosystem lesson: thin layers on other people's
platforms die (Void discontinued, Continue acqui-hired), while ACP — a protocol
— spread across Zed, JetBrains, Neovim, Emacs and dozens of agents. The winning
move is to be the agent other clients talk to, not another editor.

## Decision

### 1. One artifact, many faces
The single static `loongagent` binary **contains** the local agent server.
Every surface is either a client of it (TUI, web, IDE) or a mode of it
(`loongagent web`, `loongagent acp`). Installing loongagent means getting that
binary — there is no per-surface install. IDE-side ACP plugins are thin
wrappers, not parallel products.

| Surface | Install path | How it connects |
|---|---|---|
| TUI (default) | `curl` script / brew / npm / firm-internal mirror | spawns or attaches to the local server (unix socket) |
| Web — local | zero extra install: `loongagent web` serves the UI on localhost | same machine, same server |
| Web — team | zero install: browser → firm control-plane URL | relays to the user's own local server |
| IDE | the editor's own ACP client pointed at `loongagent acp`; optional thin branded VS Code extension later | ACP/JSON-RPC over stdio to the local binary |
| ~~Desktop~~ / ~~CI~~ | dropped (owner, 2026-09-30) — a headless mode may exist internally, but it is not a product surface | — |

### 2. Thin-client rule (hard)
A surface renders the event stream and sends intents. It contains **zero agent
logic** — no routing, no context assembly, no permission decisions, no policy
evaluation (all server-side per ADR-0005…ADR-0008). Surface-local code is
presentation only: diff rendering, syntax highlighting, file pickers,
keybindings, layout.

### 3. No single-surface capability
A capability is **implemented once, in the server**, and *exposed* on surfaces.
A surface may lag in exposing it — phased delivery is expected — but the gap
must be **visible** (an explicit unsupported marker), never a divergent local
implementation. There are no surface-only features, ever. This is the rule that
keeps "same brain, many faces" from decaying into three products sharing a name.

### 4. Build order
**TUI first** → **web** → **ACP adapter**.

TUI is the dogfooding surface, matches the senior-engineer beachhead (T-06), and
has the fastest iteration loop. Web follows for the newbie/manager/PM personas,
where per-role UX composition shows best. ACP comes third because the adapter is
thin and IDE support is largely bought by adopting the protocol. The TUI
framework choice **falls out of AQ-12**; the web stack is independent (served by
the binary).

### 5. Attach semantics: one writer, many observers
A session may be attached from several surfaces at once, but **exactly one holds
the write token** at a time. Observers receive the live stream read-only.
Acquiring the token (takeover) is **explicit and is an event** (ADR-0003), so
handing a session from TUI to web — or a colleague watching a run — never
produces two fronts issuing steering or forking in parallel. This is what makes
session roaming (T-12's tree plus multi-surface attach) safe rather than racy.

### 6. Distribution & updates
- Single static binary per OS; firm-internal mirror (Artifactory-style) for
  managed rollouts; the control plane ships from the same codebase (AQ-5).
- Channels: **stable/LTS + canary**.
- **Token-cost-affecting changes are breaking changes**: major version bump plus
  a visible changelog note. Cost governance (T-04) must not be silently
  invalidated by an update (01 §D — daily-release churn is a known ecosystem
  failure mode).
- Update artifacts are **signature-verified** exactly like config packs
  (ADR-0008); an unverifiable update is not applied.
- **Default for unmanaged installs: stable channel, auto-update on.**
  **Managed installs update from the firm mirror, and the org layer may pin a
  version or channel** — pinning is a narrow-only key (ADR-0008), so a local
  user cannot unpin themselves past the org.
  *Owner note:* the unmanaged auto-update default is my call, flagged here as
  reversible — say the word and it becomes opt-in.

### 7. Browser-only users
Both web paths — `loongagent web` on localhost and team web via the control
plane — operate against the user's **local agent server and local code**
(owner, 2026-09-30). Runner-hosted workspaces remain out of scope and, if ever
needed, return with the AQ-5 team-collaboration discussion.

## Consequences

- **Every surface is replaceable, including ours.** Because capabilities live
  in the server, a better TUI or a third-party web client can be built without
  forking the product — the ACP bet, applied to ourselves.
- **The exposure gap is manageable but must be legible.** Phased delivery means
  web/IDE will lag the TUI; the unsupported marker plus a capability-inventory
  check keeps that honest instead of letting it become a silent fork.
- **Multi-surface attach stops being a concurrency problem.** The write token
  turns roaming into an explicit handoff with a recorded event.
- **Update policy protects the cost story.** Combining signature verification
  (ADR-0008), the breaking-change rule, and org pinning means an update can
  never quietly widen spend or policy.
- **Deferred:** TUI framework (AQ-12), the control-plane relay protocol (AQ-5),
  update cadence and rollback mechanics, the branded VS Code extension's scope,
  and the observer-side UX for requesting the write token.
