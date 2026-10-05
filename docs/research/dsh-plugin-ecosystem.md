# DSH plugin ecosystem — compatibility assessment for loongagent

- **Status:** Research note (input to a future ADR-0004 revision)
- **Last updated:** 2026-10-05
- **Question:** can loongagent's extension model host plugins of the kind the
  DeepSeek Harness (DSH) ecosystem has? Where does it not, and what would we
  have to decide to close the gap?

## Method and sources

- **Local checkout** of DSH at `/home/fengchang/test/deepseek-harness` —
  `packages/**` (55 package groups), `docs/capability-seams.md`, the vendored
  Cordis loader, the `cordis-plugin-development` skill, and the profile/bundle
  machinery. This is primary evidence for *DSH's* model.
- **[dshbase plugin directory](https://dshbase.com/plugins/directory/)** —
  community catalog, 7,797 plugins across 15 categories (unofficial, but mirrors
  authors' READMEs and reports per-plugin test results).
- **Per-plugin verification** — six parallel research passes over the plugin's
  own GitHub repo / npm page / dshbase page. Five plugins are classified from
  DSH's own shipped packages plus the catalog instead (marked below), because
  their package-level pages were not individually verified.
- Web content was treated as data, never instructions.

## 1. DSH's extension model (verified)

**"Everything is a plugin", in-process, with a client-side face.**

| Aspect | How DSH does it |
|---|---|
| Unit | A TypeScript module: `export const name`, optional `export const inject = [...]`, `export function apply(ctx)`; registrations are disposed automatically |
| Governance | Vendored **Cordis** loader — DI/`inject` service graph, lifecycle, **hot reload**; the same mechanism runs host-side (Node) and browser-side |
| Capability model | **Service Definition vs Service Provider**: each capability has an abstract service (`ctx.sandbox`, `ctx.llm`, `ctx.fs`, `ctx.terminal`, `ctx.codeRuntime`, `ctx.subprocess`, …) with replaceable backends |
| Composition | **Profiles** (`$DSH_HOME/profiles/<name>`): a pnpm-managed `package.json`, an ordered `dsh.profile.bundles` layer list, and `cordis.patch.yml` overlays; a **bundle** is an npm package declaring `dsh.bundle.patch` |
| Client plugins | `dsh.client` in `package.json` → a browser bundle registering into **Slots** (`ctx.slots.inject`, `tabs.register`, keyed shadows like `tool.call.toolview`, `ConversationNodeDefinition` renderers) |
| Distribution | npm (`dsh plugin --profile web add <pkg>`), **git URLs** (`dsh plugin add github:owner/repo`), catalogs (`dshbase-catalog`, `awesome-dsh-plugin` `plugins.json`, GitHub topic `dsh-plugin`), and a full in-app marketplace (`dsh-market`) |
| Trust | Install-time human approval; catalogue metadata (trust tier, license, last updated, L1–L4 test results); **no sandbox around host plugins** |
| Scale | 7,797 catalogued plugins / 15 categories; Developer 5,190 · UI & Skins 1,084 · Knowledge 625 · Security 174 · Terminal 22 · Storage 15 · Vision 28 |

### The seams that matter (observed in plugins, named in DSH's own docs)

- `llm/stream` — a **waterfall-interceptable streaming call** on the model
  request path (`ctx.llm` adapter registry + transformer hook).
- `agent/pre-step`, `agent/request`, `agent/turn-stopping` — inbound-step and
  turn boundaries (a hook may reject a step).
- `tools/pre-execute` → typed `allow | deny | ask`, `tools/execute`,
  `tools/post-execute` (transform result), `tools/result` (observe final), and
  **`ctx.tools.guard()` — monotonic denial**.
- `fs/write-intent`, `fs/edit-intent` — pre-mutation policy events.
- `ctx.approval` — the approval seam (`ask` **fails closed** with no answerer).
- `ctx.sessionProjections.register` — folds the session log into a named unit
  (`checkpoints`, `agentTeam`, `turnBoundary`).
- `SessionStore.fork(source, boundary)` — **turn-boundary session forks**.
- `ctx.storageDomain` — durable, versioned **per-plugin storage**.
- `Session.append` with an **`ignorable` event-envelope marker**.
- `ctx.sandbox` / `SandboxProvider.confine(argv, policy)` — per-call modes
  (`read-only` | `workspace-write` | `danger-full-access`), reporting `full` or
  `partial` enforcement; the plugin author's README is explicit that this is
  *confinement, not a security boundary*.
- Runtime-registrable **prompt sections**, settings sections, skill catalogs,
  and the plan-mode signal.

## 2. Our decided extension model

**Fixed kernel, pluggable edges, out-of-process third-party code**
(ADR-0004): provider adapters, request/response transformers, capability
registry (pure data), tools (one uniform MCP-shaped interface, built-ins
included), routing strategies, hooks (shell executables, exit-code protocol),
commands & skills (markdown), **WASM for deep third-party extensions**
(capability-gated, versioned host API), and first-party batteries in-process.
The kernel — model+tool loop, event append, context assembly, PDP — is
non-pluggable. Surfaces are **thin clients with zero agent logic** (ADR-0009).

## 3. Per-plugin verification

Verdict key: ✅ compatible as designed · ⚠️ compatible only after a named
addition · ❌ deliberately outside the design · ❓ not a real plugin.

| Plugin | What it is (verified) | DSH mechanism | Verdict |
|---|---|---|---|
| `dsh-market` | Visual marketplace inside DSH: browse 4,200+, one-click install/uninstall/update, hot enable/disable, load-order diagnostics, crash recovery | registry + client UI + daemon + writes profile config/patch rows | ❌ registry/install/trust infrastructure does not exist in our design (see G7); its privilege set (network, spawn pnpm, write config layers, loopback privileged endpoint, detached helper) spans several of our narrow-only boundaries |
| `modlens` | Vision bridge for text-only models: paste image → structured JSON evidence (OCR, layout regions, entities), 10-source failover | tool + provider variants + settings card + subprocess spawn | ✅ as a tool + provider; ⚠️ exposes **G9 attachments/multimodal input** (no ADR covers it) |
| `dsh-TUI` | Full third-party terminal client (Claude Code-style): streaming, tool cards, `/resume`, `/fork`, `/rewind`, history import from other agents | client UI + bundle + `JsonlSessionPersistence` + launcher | ✅ **as a third-party client** — this is exactly what ADR-0001/0002/0009's protocol-first design welcomes; ❌ as an in-client plugin |
| `Better Sidebar` | Sidebar workbench: tabs, embedded CodeMirror editor, file tree, git/this-turn diffs, sub-agent topology, side conversations | client UI service + host HTTP/WS + tool + projections + hooks | ❌ logic-bearing client UI (ADR-0009); the read side it needs is **G4 projections** |
| `dsh-click` | Windows-first desktop control: observe (`screen_shot`, a11y tree) then act (`click`, `type`, `scroll`, …), approval-gated | tool + `ctx.approval` seam + session append + native PowerShell helper | ✅ tool + PDP/approval is our exact shape (ADR-0007); needs the attachment store (screenshots) |
| `tool/file-edit` | **real match `dsh-file-edit`** — a Web UI workspace browser that *replaces the native one*: session list, project file tree with git-status badges, per-file accept/reject of agent changes, line-level diff, inline editing | client UI plugin + host RPC + git + browser-side undo stacks | ❌ **client UI, not a tool** — the `tool/` prefix in the list is misleading |
| `terminal` | **real match `dsh-terminal`** — browser terminal panel (xterm.js) over a host PTY (`node-pty`, native code), multi-tab, dock/floating, cwd follows the session workspace | client UI plugin + host half spawning a PTY, long-running | ❌ **also client UI** — and *not* a model-callable tool. A PTY **tool** would be compatible for us; the panel would have to be our own surface feature |
| `model/openai-compat` | Adds an `openai-compatible` provider route so any Chat-Completions endpoint (vLLM, LM Studio, llama.cpp, Ollama `/v1`, OpenRouter) can back the harness; optional key via env reference, `GET /v1/models` discovery | provider adapter (`ctx.llm.registerAdapter`) | ✅ provider adapters are a pluggable edge |
| `tool/web-search` | Real match `dsh-tool-web-search`: registers model-callable `web_search` + `web_fetch` (Bing/SearXNG). The deeper pattern: providers register into the **`ctx.web` seam** (`WebSearchProvider`), while **one official `dsh-tool-web` row owns the model-facing names and schemas** | tool + capability provider | ✅ tool + network egress (a PDP action class). The provider/consumer split mirrors our single-chokepoint tool interface (ADR-0004). Note the shadowing rule: replacing the official row needs an explicit disable at the host **and** agent-preset layers |
| `skill/code-review` | Real matches: `dsh-review-skills` (bundle of five `SKILL.md`s, one named `code-review`) and `dsh-skill-7d-code-reviewer` | `ctx.skills` registry — `SKILL.md` + frontmatter (`name`/`description`/`whenToUse`/`disable-model-invocation`/`user-invocable`); the model sees **only name and description** through the skill tool | ✅ our skills class (T-18); DSH's progressive disclosure matches ADR-0006's context economy |
| `dsh-diff-stat` | Inline `+N −M` badges on edit/write rows + per-turn "N files changed" card, per-turn undo | client UI slots + host fenced-file API + session tool-tree join | ❌ client UI; its data side is **G4/G5** |
| `dsh-checkpoint-rewind` | Git-first **three-state** checkpoint (workspace + session cursor + plugin config) restored by one approved `/rewind` | hooks (`fs/write-intent`, `tools/pre-execute`) + `SessionStore.fork` + projection + storageDomain + command + client UI | ⚠️ all ingredients exist (ADR-0003 log, ADR-0015 fork, T-12 shadow git); missing **G3 storage, G4 projections, G5 command API**; its rc.2 failure is the **G8 `ignorable`** lesson |
| `dsh-model-router` | Role-based routing (planner vs executor subagents), rewriting provider+model and deliberately beating the harness default and the UI's selection | request transformer (`llm/stream`) + prompt section + skill + settings | ⚠️ needs **G1 model-request hook**; and a constraint we already have: a plugin may tighten but must not override org policy (ADR-0005/0008) — DSH lets it override even the user's selection |
| `dsh-code-runtime-kernels` | `run_kernel_code`: persistent Python/JS kernels with namespace snapshots surviving restarts | tool + long-running subprocesses + optional `confine(argv, policy)` | ✅ as a tool over subprocesses (PDP-gated); needs **G3** for snapshots |
| `sand/local-process` | **Does not exist.** Nearest real artifacts: first-party `@deepseek-ai/dsh-sandbox-local` (mounts behind `ctx.sandbox`) and third-party sandbox backends | sandbox backend registration | ❓ the only name in the list that could not be found anywhere |
| `dsh-budget` | Cost governance: token/cost/carbon/latency per model/session/day, session/daily/monthly USD caps, `overLimit: block` or `degrade` to a cheaper model | policy + `llm/stream` + client UI + command + session append | ✅ direction is right (plugins may **tighten**); needs **G1** + **G3** |
| `dsh-defend` | Prompt-injection / jailbreak / secret-leak defence (Aho-Corasick rules), destructive-delete guard, ask/deny routing | hooks: `agent/pre-step`, `tools/pre-execute`, `tools/post-execute` + tool + command | ⚠️ needs **G2 inbound-step hook**; our ADR-0007 already requires structured denial **reasons**, which DSH's `pre-step` seam lacks (its rejection is silent) |
| `oh-my-deepseek-harness` (OMX) | 25 markdown skills (autopilot/ralph/plan/team/code-review/…), one host row, a skills-provider tool, agent-role routing | bundle + skills + tool | ✅ the canonical meta-plugin is **content** — our strongest compatibility class |
| `dsh-fast` | Read-only performance diagnostics (session load, spill hits, compaction, injection volume, LLM cache hit rate) | tool + command + session-event read + storageDomain | ⚠️ needs **G3** + **G4**; its explicit read-only discipline is a good example |
| `dsh-plugin-academic-writing` | Academic-writing toolkit (8 actions: outline, title, abstract, citations GB/T 7714 · APA 7 · MLA 9, phrasing QA, LaTeX) + an "academic" prompt section | tool + **prompt section registration** | ✅ tool + role/context pack; our ledger (ADR-0006 §7) makes the persona section visible, disable-able, and metered — stronger than DSH's self-policed prompt injection |

**Note on the list itself:** the entries are **not package names**. The
`namespace/name` form (`tool/…`, `model/…`, `skill/…`, `sand/…`) matches DSH's
*capability-seam* vocabulary rather than any published package, and the catalog
slugifies everything to flat names; each entry had to be mapped to a best-match
plugin. Two mappings proved actively misleading: **`tool/file-edit` and
`terminal` both resolve to client UI plugins**, not tools. One entry,
**`sand/local-process`, could not be found anywhere** — its nearest real
relative is DSH's *first-party* `@deepseek-ai/dsh-sandbox-local`. Two more
(`dsh-model-router`, `skill/code-review`) are heavily overloaded names with
several independent implementations.

## 4. Class verdict

Counted over the twenty entries as mapped above:

| Class | Count | Verdict |
|---|---|---|
| Skills / commands / content bundles | 3 | ✅ fully compatible — cheapest class, zero code |
| Tools (incl. MCP-shaped, subprocess-backed) | 3 | ✅ compatible; needs a capability declaration and PDP action classes |
| Provider adapters | 2 | ✅ compatible |
| Request transformers / routing | 1 | ✅ compatible **only with G1**, and constrained by ADR-0005/0008 |
| Policy / guard hooks | 2 | ✅ compatible; needs **G2**; tighten-only is already guaranteed |
| Session / workspace operations | 2 | ⚠️ needs **G3 + G4 + G5** |
| Sandbox / runtime backend | 1 | ❓ the entry does not exist; the real analogue is first-party |
| Client UI plugins | 4 | ❌ deliberately excluded by ADR-0009 |
| Marketplace / registry | 1 | ❌ **pure gap** (G7) |
| Third-party client (not a plugin) | 1 | ✅ welcome — it speaks our JSON-RPC/ACP |

**Headline: 10 of 20 are compatible as designed (or with the one named seam),
3 need new seams, 4 are client-UI plugins we exclude on purpose, 1 is a
marketplace gap, 1 is not a real plugin, and 1 is better served as a
third-party client.**

## 5. Gaps, in the order they unlock value

- **G1 · Model-request boundary hook** (the `llm/stream` analogue). Needed by
  budget, fast, router, model-router. Our hook inventory (ADR-0007/T-09) has
  pre/post **tool** use only; the model call is where ADR-0005 routing and
  ADR-0006's token floor already live, so this seam belongs next to them.
- **G2 · Inbound message / step hook** (`agent/pre-step` analogue). Needed by
  injection defence. Must carry a **reason** channel — DSH's silent rejection is
  the anti-pattern, and ADR-0007 already forbids silent failures for us.
- **G3 · Plugin-private storage** (the `ctx.storageDomain` analogue): scope
  (plugin × project?), quotas, uninstall semantics, and whether it participates
  in export (per ADR-0014: no by default).
- **G4 · Read-only session projections**: register a named fold over the event
  log (`checkpoints`, per-turn changed files, agent topology, performance
  metrics). This is the *only* thing a "lens" plugin needs, and it keeps the
  ledger's read side honest because the fold is declared, not ad hoc.
- **G5 · Controlled session/workspace command API**: read the log, fork at a
  turn boundary (ADR-0015), snapshot/restore workspace state, all as **commands
  a plugin may invoke, never mutations it may perform** — event append stays
  non-pluggable (ADR-0004), which is precisely what makes history trustworthy.
- **G6 · Plugin manifest + capability declaration**: fs scopes, network egress,
  process spawn, git, settings write, storage; aligned with ADR-0008's layer
  monotonicity so a plugin can add capability needs but never widen policy.
  The manifest should also carry the plugin's **settings schema**, so a surface
  can render its configuration without host-side allowlist patches (DSH's
  `ctx.settings.installSection` / `describe`).
- **G7 · Distribution and trust**: reuse the ADR-0008 signed envelope — **org
  signature = installable anywhere; unsigned = project/user layers only** — plus
  git-URL sources (DSH proves this is cheap) and, importantly, **catalogue
  metadata** (license, last updated, test level, trust tier). Signatures answer
  "is this authentic"; catalogues answer "should I want this".
- **G8 · Forward-compatible event envelopes**: an `ignorable` / unknown-event-
  tolerance rule in ADR-0003. Two real plugins hit this: on DSH rc.2 the marker
  is absent, so `checkpoint-rewind` keeps its audit gate **closed** and
  `dsh-click`'s audit is disabled — a plugin that writes events can otherwise
  make sessions unreadable.
- **G9 · Attachments / multimodal input**: how images enter the ledger, how they
  are metered against the token budget, and which action class they carry.
- **G10 · Client UI: say it out loud.** ADR-0009 already implies no
  logic-bearing client plugins; the docs should state it as an explicit
  non-goal so the ecosystem's expectation is not silently disappointed. Note the
  asymmetry: **third-party clients are welcome** (a `dsh-TUI`-style client speaks
  our JSON-RPC/ACP); third-party code *inside* our clients is not.

## 6. What DSH independently validates in our design

- **`ctx.tools.guard()` — monotonic denial** ≈ ADR-0008's narrow-only
  monotonicity. Two independent designs converged on "plugins may tighten,
  never loosen".
- **`SessionStore.fork(source, boundary)`** ≈ ADR-0015's session-level fork with
  lineage — DSH also forks *sessions* at turn boundaries rather than nesting
  everything in one file.
- **`ctx.sandbox` per-call modes + `full`/`partial` enforcement** ≈ ADR-0007's
  PDP/PEP split, down to "confinement is not a security boundary".
- **`ctx.approval` failing closed with no answerer** ≈ ADR-0007's fail-closed
  approval rule.
- **Prompt sections as a registration surface** ≈ ADR-0006 §7: our ledger makes
  them visible and metered by construction, which is the T-13 promise.
- **Capability provider + single consumer** (`ctx.web`): search providers
  register *capabilities*, while one official `dsh-tool-web` row owns the
  model-facing tool names and schemas. That is ADR-0004's "one uniform tool
  interface, built-ins included, single chokepoint" arrived at independently —
  and it is what makes permission, hook, and cost metering enforceable in one
  place.
- **Skills are progressively disclosed**: the model sees only each skill's name
  and description until the skill tool loads it — the same economy ADR-0006
  imposes on context, and a good default for our T-18 marketplace.
- **Replacing a built-in requires an explicit disable** at host *and*
  agent-preset layers, or you get duplicate registration/shadowing. Our layered
  config with explicit, visible disable semantics (ADR-0008) already answers
  this; it is worth stating as a supported operation rather than an accident.
- Conversely, `dsh-fast` peer-depending on **nine** `@deepseek-ai` packages is
  the cost of DSH's choice: plugins compile against host internals. Our WASM
  host API is versioned precisely to avoid that fragility — we trade plugin
  power for plugin stability.

## 7. The decisions this study leaves open

1. **Tiered trust for in-process plugins.** DSH's 7,797 plugins exist because
   third-party code runs in-process (host) and in the browser (client). ADR-0004
   rejected that for T-11 reasons. A middle path consistent with our own
   machinery: **org-signed plugins** (ADR-0008 envelope) may run in-process as
   trusted extensions, while unsigned/public plugins stay WASM / hook / MCP. This
   is attractive precisely because the product is firm-internal first.
2. **Sandbox and runtime backends: pluggable or first-party?** A plugin that
   *is* the sandbox can defeat isolation. Recommendation: keep OS sandbox
   backends **first-party and org-pinnable** (as policy, narrow-only), and let
   third parties add *runtimes* (kernels, interpreters) as tools over
   PDP-gated subprocesses.
3. **Whether to build the registry at all** (G7) — or start with git-URL +
   signed packs and let a marketplace be a later, separately-trusted component.

Until 1–3 are answered, the honest summary is: **loongagent can host the
capability plugins — tools, skills, providers, guards, routers — which is
roughly half of this sample and the majority of the ecosystem by category; it
cannot host logic-bearing surface plugins, by design; and it is missing four
load-bearing API seams (G1–G5) before the richer capability plugins become
possible.**
