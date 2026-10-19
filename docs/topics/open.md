# Open topics — Seed, Wanted, Discussing

- **Up:** [README.md](README.md)
- **Authority:** [02-topics.md](../02-topics.md)
- **Code:** `docs/topics/open.md`
- **Verify:** `python3 scripts/check-docs.py`

These are the topics the backlog has not decided: `Seed` (captured, not yet
discussed), `Wanted` (the owner has said "I want this" — direction agreed,
details to discuss) and, when one is being discussed, `Discussing`. A topic
leaves this file when an ADR or a merge settles it; its status line then names
the decision. The full text is kept, heading and all, so a `T-NN` citation still
resolves. The index of all 26 is [README.md](README.md).

## T-06 · Per-role UX: the "this is THE tool for me" moment — `Wanted` (HIGH PRIORITY)

Seeded 2026-09-26. Owner emphasis: **"I cannot emphasize more about how
important UX is for the success of this project."** **Direction agreed:** a role
is a *surface + profile* composition — a named profile plus a versioned context
pack (ADR-0006 §7) and per-action-class autonomy offsets (ADR-0007 §4) — never
a forked product (ADR-0009). Still to design: the personas' wow moments, the
beachhead, and the wow metrics.

Idea: every role — regardless of seniority or surface preference — should get a
strong "oh my god, this is just the tool I want" feeling. Not one average UX for
everyone; a *deliberate wow moment per persona*.

Personas and their wow moments (owner's examples + extensions):
- **Experienced engineer** → "holy sh*t this CLI is lightning fast and the
  responses are smart." Terse, keyboard-driven, zero ceremony, stays in flow.
  (Research: latency is a feature — Cursor Tab's sub-30s iteration changes
  behavior; Pi's 1.4k-token overhead; native-scrollback TUI.)
- **Engineering manager** → "this really saves money; statistics and feedback
  in a few clicks; everything summarized so clearly I know what to improve."
  (T-02/T-04 dashboards: cost per person/task/model, waste detection, trend
  summaries written in manager language, not engineer language.)
- **PM / product-minded user** → "it clears my ideas visually and efficiently."
  Visual spec canvas: vague idea → structured spec (Kiro-style EARS
  requirements) with drag/refine UX; idea → plan → delegated tasks without
  touching a terminal. (Links: kiro-spec-driven research; T-01 routing lets PM
  queries run on cheap models.)
- **Newbie / web-preferring user** → "wow, the web interface works even better
  than CLI and it's pretty." Zero-install onboarding, guided first session,
  safe defaults (sandbox + preview-then-approve), beautiful by default —
  terminal-aesthetics quality bar applied to the web.
- **DevOps/SRE** → evidence chains, not guesses (incident.io's contract);
  read-only investigate mode that's genuinely safe against prod; runbook
  execution with checkpoints.
- **QA** → visual test review, screenshot diffs, coverage heatmaps.
- **Data engineer** → schema-aware SQL assistance, dry-run/plan before query
  (WrenAI guardrails), notebook-friendly surface.

Design principles implied:
- **Same brain, different faces** — the core agent is shared; the *surface,
  density, vocabulary, and defaults* adapt per role. A manager never sees a
  token trace; an engineer never sees a dashboard card first.
- **Progressive disclosure of power** — resolve the power-vs-simplicity tension
  by layering: the newbie web UI and the wizard's CLI talk to the same core;
  complexity reveals itself as the user asks for it.
- **Beauty is functional** — pretty = trustworthy = adopted. Terminal UI gets
  the same design budget as web (Charm/OpenTUI set the bar).
- **Every persona gets a measurable wow metric** — time-to-first-success,
  time-to-first-insight (manager), time-to-first-spec (PM). If we can't
  measure the wow, we can't defend it.

Open questions:
- Which persona is the **beachhead**? (You can't wow everyone on day one —
  sequencing matters. Candidates: senior engineers for credibility, or
  web-first newbies for volume.)
- Does "PM mode" produce artifacts that engineers' sessions consume? (Specs as
  the handoff protocol between personas — links kiro-spec-driven research.)
- How do role profiles (T: role-specialized research) map to UX personas —
  same thing or two axes (role × seniority)?

## T-08 · Authentication & identity (flexible, firm-internal first) — `Seed`

Seeded 2026-09-26.

Owner's intent: loongagent is a firm-internal tool, so authentication must be
**flexibly adaptable to whatever the firm already has** — SSO, service
principals, or anything else. No hardcoded auth scheme.

Design shape (to discuss):
- **Pluggable identity layer in the backend control plane (T-05)**: SSO via
  OIDC/SAML (Keycloak, Entra ID, Okta, Authentik, firm-internal IdPs), LDAP
  where legacy demands it, and **service principals** (non-human identities
  for CI, cloud workers, automations). Local dev mode: no-auth or personal
  token, clearly marked.
- **Identity is the coordination mechanism** (Codex's lesson): sessions roam
  across surfaces and devices because they belong to a *user*, not a machine.
  Also the anchor for T-02 audit ("who did what"), T-04 budgets ("whose
  quota"), and T-07 profile permissions ("which teams may use supersmart").
- **RBAC lives behind identity**: roles/groups from the IdP map to loongagent
  permissions — profile access, admin policy editing, dashboard visibility.
  Precedents: Cursor Teams (SAML/OIDC + SCIM), Codex managed config,
  GitHub governance defaults.
- **Agent identity is a first-class question**: when an async worker opens a
  PR or runs a migration, whose identity is it? Recommended: the agent acts
  as a distinct service principal *on behalf of* a human (attribution chain:
  human → session → action), never anonymously, and GitHub-style governance
  applies (requester can't approve their own agent's PR).
- **Credential handling**: provider API keys live server-side in the model
  gateway (T-05) — engineers never hold vendor keys; the backend brokers
  model access behind auth. (Also kills the "leaked key in dotfiles" class
  of incident.)
- **Flexibility contract**: auth = a documented adapter interface with 2-3
  reference implementations (OIDC, static token, service principal); a firm
  can plug its own without forking. Zero auth logic in the local client
  beyond token acquisition/refresh.

## T-12 · Session tree: fork, retry, steer — `Wanted`

Seeded 2026-09-30. Owner: **"Session tree is the killer feature — I want this."**

Pi's design, adopted, with the two tree levels now named (ADR-0015): **inside**
a session, events carry `id`/`parentId` forming an **event tree** — `/tree`
navigation over paths, **retry** = a new path from the failure point with the
abandoned path retained and summarized (no pollution of the linear history),
lossless compaction (full history survives). **Between** sessions, a **fork**
creates a **child session** with lineage back to the point it came from.
**Steer vs follow-up** queues (Pi): deliver-after-current-tool vs
deliver-when-idle. **Pick**: the agent presents structured options and the
user picks (AskUserQuestion pattern). Combined with T-13, the event tree is
the user's x-ray into agent behavior — and with shadow git snapshots
(synthesis T9), rewind covers both conversation and code.

## T-15 · MCP & external tool ecosystem — `Seed`

Seeded 2026-09-30.

MCP supported but **lazily loaded** — Pi's critique stands (Playwright MCP =
21 tools / 13.7k tokens dumped into every context); Amp's pattern: skills
bundle their MCP config so tools enter context only when the skill triggers.
Goose's orthogonality: even built-in tools as MCP servers. Org angle (T-05):
an approved-server gateway/registry; prompt injection via MCP content is the
dominant attack class — pinning, scanning, and trust levels required.

## T-20 · Git-diff-centric workflow & spec-driven process — `Seed`

Seeded 2026-09-30.

Two halves of the dev process: (1) **git as the review/undo layer** — shadow
snapshots per message (synthesis T9), per-file staged diff review
(Cursor 3.0's preview-then-approve), PR as the deliverable for async work.
(2) **Spec-first with size-scaled ceremony** — terse repo-native markdown
specs, auto-triage when to spec at all (a bug fix ≠ 4 stories/16 criteria),
requirement↔task traceability links; NEVER spec-as-source (Tessl's $125M
lesson). Specs are the handoff protocol between personas (T-06/T-14).

## T-22 · LSP-backed code intelligence — `Seed`

Seeded 2026-10-05 (from [05-parity-and-synthesis](../05-parity-and-synthesis.md)
§A1/§B5). Claude Code (via LSP plugins) and OpenCode give the model
go-to-definition, find-references, and **type errors after an edit** — the
cheapest possible input to the verification loop, and what stops an agent
guessing at symbol names. Our backlog had **no entry for this**. Design
questions: which language servers to bundle, detection, context cost of
diagnostics, how it composes with grep/repo-map retrieval (ADR-0006), and
whether diagnostics enter context automatically after a write (CC's behaviour)
or only on request.

## T-23 · Persistent goal with evaluator — `Seed`

Seeded 2026-10-05 (§A8/§B4). Claude Code's `/goal`: a completion condition that
survives across turns, re-checked by an **evaluator** after every turn, with
Stop hooks gating turn end and an 8-block circuit breaker so the agent cannot
spin forever. Worth absorbing because it turns "keep going until X" from prompt
text into a mechanism — and it exercises seams already decided: goals as events
(ADR-0003), goal state visible (T-13), turn-end gating as a hook (T-09), and the
evaluator's cost metered (ADR-0006).

## T-24 · Module metadata & lifecycle — `Seed`

Seeded 2026-10-05 (§B6/B7/B12, §D2). With the extension architecture reframed as
**our own iteration architecture** (ADR-0004 Context), module metadata becomes
product surface we need regardless: each module **declares its config schema**
and the surfaces render the settings UI from it (DSH's
`installSection`/`describe`); **hot reload** for skills, commands and modules so
authors iterate without restarting (Pi's `/reload`); and **declarative panels** —
a module declares "render this server-side projection as a panel/tab/column" and
the shell owns the components, so TUI, web and IDE all render it with no
third-party client code. That keeps ADR-0009's thin-client rule and "no
single-surface capability", while letting **us** add panels without forking a
surface.

## T-25 · Interaction smalls — `Seed`

Seeded 2026-10-05 (§A2/§A3/§A7/§B11): `/btw` (side question that never enters
history), `Monitor` (background script output surfaced as events), prompt-**cache
hit rate** in usage, `/init` and `/doctor` (generate / trim instruction files),
`@path` imports in instruction files (depth-limited, approval-gated), and the
description token-budget warning for skills/subagents. Individually small;
collectively the difference between "feature-complete" and "feels finished".

## T-26 · Cross-crate helper duplication — `Seed`

Seeded 2026-10-16 (navigability review §3 — **recorded, deliberately not
fixed**). Five small helpers are written again in each crate that needs them —
thirteen definitions in all:

- `now_ms` — `crates/hooks/src/run.rs` (the `pub` one),
  `crates/eventlog/src/lib.rs` (`i64`), `crates/plugin/src/lib.rs` and
  `crates/control/src/lib.rs`;
- `expand_home` — `crates/plugin/src/lib.rs` and `crates/config/src/paths.rs`;
- `first_line` — `crates/cli/src/control.rs` and
  `crates/control/src/bin/loongagent-control.rs` (byte-identical), plus a
  different, length-bounded third in `crates/hooks/src/run.rs`;
- `truncate` — `crates/context/src/store.rs` and `crates/tui/src/lib.rs`;
- `clip` — `crates/tui-app/src/lib.rs` (columns, CJK-aware) and
  `crates/cli/src/session.rs` (characters).

**Not consolidated now:** one home for these means a **new shared public
surface** — a crate or a `pub` module that each of those crates depends on — and
that is an API change, not a tidy-up. Thirteen definitions of five ten-line
helpers do not justify one: the new dependency edge and its public commitment
cost more than the copies do. Revisit when a sixth caller appears, or when one
copy has to change in a way the others must follow.

---

*Next topics go here. Just say the idea — capture is automatic.*
*Next topics go here. Just say the idea — capture is automatic.*
