# ADR-0008 — Configuration & profile layering: monotonic layers, signed packs, explainable resolution

- **Status:** Accepted
- **Date:** 2026-10-04
- **Decides:** AQ-9 configuration & profile layering (docs/03-architecture.md);
  fixes the distribution envelope for T-07 profiles, ADR-0005 policy packs, and
  ADR-0006/ADR-0007 role and context packs

## Context

Configuration is where several earlier decisions either get teeth or quietly
leak. ADR-0005 made the local brain authoritative for live routing while the
server shapes future decisions through policy packs — that only holds if a
local user cannot rewrite the pack. ADR-0007 put org hard caps in the PDP as
`deny`, which only means something if the local layer cannot widen them.
T-07 says engineers pick a *profile*, never a model — which requires the profile
to be admin-maintained and non-tamperable. ADR-0006 made context auditable
entry by entry, and the same "why is this here?" question applies to every
config value.

The conventional answer — a precedence chain where the lowest layer wins — is
insufficient, because "wins" is exactly how a local layer widens an org limit.
Ecosystem precedent cuts both ways: AGENTS.md/CLAUDE.md have made
markdown-plus-frontmatter the human-authored convention, while enterprise
managed config (Codex `requirements.toml`, Cursor blocklists, Chinese vendors'
admin-controlled model menus) shows org-managed policy working — and power-user
revolt (OpenCode blocking Claude Max) shows what happens when it is done
without transparency or escape hatches.

## Decision

**One layered config stack, with a per-key merge kind that makes lowering a
higher layer's guarantee impossible.**

### 1. Layers and ownership

Lowest to highest precedence: **built-in defaults < org managed < project
(repo-committed) < user < session overrides.**

Ownership follows the layer: the **org layer** lives in the firm control plane
(T-05), is authored by admins, and reaches the local install as a **versioned
pack**; the **project layer** is committed in the repository; the **user and
session layers** stay local. A solo install is the same code with an empty org
layer — built-in defaults simply become the top.

Distribution is **not** a live dependency: the local install caches the last
synced pack and keeps working offline (offline = slightly older policy, not
degraded function). ADR-0005's rule still holds unchanged — the pack shapes
*future* decisions; **the local brain makes every live decision.**

### 2. Merge kinds — the monotonicity rule

**No lower layer may ever loosen a higher one.** Each key declares one of four
merge kinds, and merge behavior follows the key's kind rather than the value's
shape:

| Kind | Applies to | Merge rule |
|---|---|---|
| **free-override** | UX preferences, theme, verbosity, profile choice *within the allowed set* | lowest layer wins |
| **narrow-only** | permissions, allow/deny lists, autonomy class, budgets, timeouts, token ceilings | strictest wins; lower layers may only narrow — never widen, replace, or disable |
| **add-only** | rules, instructions, commands, skills, memory, context packs | union across layers; a lower layer may *disable a specific higher-layer entry by identity*, recorded as an event — permitted precisely because content is advisory, not enforcement (ADR-0007) |
| **reference** | secrets, credentials, endpoints | not merged; the key names a source (below) |

This **refines the earlier "lists replace by default, `+` to extend" lean**:
replacement is no longer a merge kind for lists at all, because "replace" is
precisely the vector by which a lower layer widens a narrow-only set. Lists are
either narrow-only or add-only, and which one is a property of the key.

### 3. Formats

- **Machine-managed config** (profiles, policy, budgets, settings): **TOML**.
- **Human-authored content** (rules, commands, skills): **markdown with YAML
  frontmatter** — the ecosystem convention we do not fight.
- **AGENTS.md is the native rules format.** CLAUDE.md, `.cursorrules` and
  similar are handled by an **explicit one-time import**, never a live
  dual-source merge: silently reading a second rules file from behind is
  exactly the hidden-context failure T-13 exists to prevent.

### 4. Explainable resolution

Every key answers **"why is this the value?"**: the winning layer, the source
artifact (file path or pack id + version), and the entry identity for list
members. Exposed as a first-class command (`loongagent config explain <key>`),
and the resolved values relevant to a turn can appear as ledger entries
(ADR-0006). This is the same provenance principle as the context ledger and
ADR-0007's "the rule that fired" — one idea in three places, not three ideas.

### 5. Secrets never live in config

Config files and packs are shared, reviewed, and diffed; a secret in one is a
leak by construction. Credentials and provider keys live in the OS credential
store or the control plane's model gateway (T-08/T-05), and config carries only
a **reference** (`keychain://…`, `env:…`, `gateway:…`). This also means a
profile can be PR-reviewed in the open without redaction.

### 6. Packs: one signed envelope for every distributed artifact

Profiles (T-07), policy packs (ADR-0005), and role/context packs
(ADR-0006/0007) share **one envelope format**: artifact id, semantic version,
payload schema id, issuer, target scope, and a **signature**.

- The local install **verifies the signature** before the pack can occupy the
  org layer. This is what makes "tamper-resistant org layer" an enforceable
  property rather than a description.
- **Unsigned packs are allowed only at the project and user layers.** An
  unsigned artifact can never be installed as org policy.
- Packs are **versioned and PR-reviewable** (git-native config, per the
  role-specialized research lesson): the same artifact is what admins review
  and what machines verify.

AQ-9 fixes the **envelope** — versioning, signing, precedence, merge kinds,
provenance. The **payload schemas** stay with their domains: profile contents
(T-07), policy/rule language (ADR-0007's deferral), context-pack contents
(ADR-0006).

## Consequences

- **The guarantees above this ADR stop being honor-system.** Org hard caps
  (ADR-0005) and policy non-widening (ADR-0007) are enforced by merge kind plus
  signature, not by convention.
- **The "can't turn it off" anti-pattern is avoided where it is safe and
  accepted where it is not.** Content entries can be disabled (visibly,
  audited); policy cannot — but policy is also not where annoyance lives.
- **Transparency is the mitigation for profile-gating tension (T-07).** A
  visible served-by line plus `config explain` gives power users visibility
  without giving away the org's vendor control; admin-granted escape hatches
  remain a profile-layer feature, not a config-layer loophole.
- **One envelope, three payloads** keeps the control plane's distribution code
  small and auditable, and matches ADR-0004's microkernel economy.
- **Config itself becomes testable**: a resolved-config snapshot answers "what
  would this install actually do?" — useful for support and for AQ-10's async
  runners.
- **Deferred:** concrete key schemas per domain, pack transport and update
  cadence (AQ-5), signature algorithm and key distribution, config discovery
  paths and file naming, and how importers normalize foreign rules files.
