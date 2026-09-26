# Spec-Driven Development Tools: Kiro, GitHub Spec Kit, Tessl

One-line description: How the three flagship spec-driven development (SDD) tools structure the spec → code workflow, what users actually experienced with them, and what a docs-first coding agent should borrow.

Researched: 2026-09-26

---

## 1. What it is

### Kiro (AWS)

- Agentic AI development environment from AWS, launched public preview July 2025, generally available 2025-11-17 ([AI Agent Square review](https://aiagentsquare.com/agents/kiro), [Kiro launch blog](https://kiro.dev/blog/introducing-kiro/)).
- Surfaces: desktop IDE (Code OSS / VS Code fork; macOS, Windows, Linux), CLI (added at GA), web interface (preview, paid plans), mobile companion for monitoring ([AI Agent Square](https://aiagentsquare.com/agents/kiro)).
- Distribution: standalone product, no AWS account required (sign in via GitHub / Google / AWS Builder ID); enterprise SSO via AWS IAM Identity Center (SAML/SCIM).
- Pricing (verified against kiro.dev/pricing 2026-07-04): Free 50 credits/mo; Pro $20 (1,000 credits); Pro+ $40 (2,000); Pro Max $100 (5,000); Power $200 (10,000); add-on credits $0.04 each; team overages disabled by default ([AI Agent Square](https://aiagentsquare.com/agents/kiro)). An earlier Aug 2025 "spec request / vibe request" metering scheme was withdrawn after backlash and a metering bug ([The Register](https://www.theregister.com/software/2025/08/18/aws-pricing-for-kiro-dev-tool-a-wallet-wrecking-tragedy/1467855)).
- Claims 250,000+ developers during preview (Kiro, Nov 2025, cited in [AI Agent Square](https://aiagentsquare.com/agents/kiro)).

### GitHub Spec Kit

- Open-source (MIT) toolkit from GitHub that scaffolds SDD for *whatever agent you already use* — 30+ supported agents (Copilot, Claude Code, Cursor, Gemini CLI, Codex, …) ([speckit.org](https://speckit.org/), [particula.tech comparison](https://particula.tech/blog/spec-driven-development-tools-spec-kit-vs-kiro-vs-tessl)).
- Install: `uv tool install specify-cli`; `specify init my-project --integration copilot` ([speckit.org](https://speckit.org/)).
- Adoption: ~111k GitHub stars, ~9.8k forks by June 2026 ([particula.tech](https://particula.tech/blog/spec-driven-development-tools-spec-kit-vs-kiro-vs-tessl)); ~115k stars per [ssojet](https://ssojet.com/blog/best-spec-driven-development-tools). Explosive growth in <1 year.
- 2026 state: commands are now agent *skills* (`/speckit-*`), with the SDD pipeline in core and two bundled opt-in extensions — bug fixing (`assess → fix → test`) and idea assessment (`intake → research → define → shape → decide`) ([speckit.org](https://speckit.org/)).

### Tessl

- Startup founded by Guy Podjarny (Snyk founder/former president, ex-Akamai CTO). $125M raised ($25M seed boldstart/GV Apr 2024 + $100M Series A led by Index Ventures, Nov 2024) at ~$750M valuation — vision financing before a commercial product existed ([CodeMySpec Tessl review](https://codemyspec.com/blog/tessl-review), [Fortune](https://fortune.com/2024/11/14/tessl-funding-ai-software-development-platform/)).
- Shipped: **Tessl Spec Registry** (open beta 2025-09-16) — "npm for knowledge," 10,000+ versioned spec packages describing library usage so agents stop hallucinating APIs; and **Tessl Framework** (closed beta same day) — the actual spec-generates-code engine ([CodeMySpec](https://codemyspec.com/blog/tessl-review)).
- **Pivot, 2026-01-29**: repositioned as an "Agent Enablement Platform" — skills registry, "loops" (security scanning, docs maintenance, cost benchmarking, model/harness evals), and an enterprise "factory" control plane. Current tagline: "Skills are the new code" ([tessl.io](https://tessl.io), [CodeMySpec](https://codemyspec.com/blog/tessl-review)). The spec-as-source Framework never reached GA and has receded in messaging.

---

## 2. Core design philosophy

**Shared thesis**: move correction left — write an unambiguous spec *before* the agent writes code, because fixing a paragraph is cheaper than discarding 2,000 generated lines. GitHub's framing: "maintaining software means evolving specifications… code is the last-mile approach" (quoted in [Böckeler/Fowler](https://martinfowler.com/articles/exploring-gen-ai/sdd-3-tools.html)).

**Three levels of SDD** (the field's most useful taxonomy, from Birgitta Böckeler's teardown on martinfowler.com):

1. **Spec-first** — spec written before the task, used during it.
2. **Spec-anchored** — spec kept alive after the task for maintenance/evolution.
3. **Spec-as-source** — spec is the only human-edited artifact; code is a regenerated build output.

All tools do spec-first; almost none deliver spec-anchored well; only Tessl attempted spec-as-source ([martinfowler.com](https://martinfowler.com/articles/exploring-gen-ai/sdd-3-tools.html)).

- **Kiro**: "prompt, prompt, prompt, and you have a working application. It's fun and feels like magic. But getting it to production requires more… What assumptions did the model make? … those decisions aren't documented" — Kiro exists to get vibe-coded prototypes to production via specs + hooks ([Kiro launch blog](https://kiro.dev/blog/introducing-kiro/)).
- **Spec Kit**: "Define *what and why* before deciding *how*"; agent-agnostic, repo-native artifacts, human verifies at every phase ("your role isn't just to steer. It's to verify") ([speckit.org](https://speckit.org/), [Fowler](https://martinfowler.com/articles/exploring-gen-ai/sdd-3-tools.html)).
- **Tessl**: "specs — not code — are the primary artifact" ([Fowler](https://martinfowler.com/articles/exploring-gen-ai/sdd-3-tools.html)). Podjarny: "Being spec-centric means you define what you want, and that includes definitions of what correctness looks like." Payoff claim: apps become language-agnostic ("adapted from JavaScript to Python, iPhone to Android") because code is regenerated from the same spec ([CodeMySpec](https://codemyspec.com/blog/tessl-review)).

---

## 3. Feature inventory

### Kiro

**Specs** (the core). Files live in-repo at `.kiro/specs/<feature>/{requirements.md, design.md, tasks.md}`:

- **requirements.md** — user stories ("As a…") with acceptance criteria in **EARS notation** (Easy Approach to Requirements Syntax: WHEN/THEN, IF/THEN, WHILE, WHERE patterns), which makes prompt assumptions explicit and forces edge cases ([Kiro launch blog](https://kiro.dev/blog/introducing-kiro/), [Fowler](https://martinfowler.com/articles/exploring-gen-ai/sdd-3-tools.html)).
- **design.md** — generated by analyzing the *actual codebase* plus approved requirements: data-flow diagrams (Mermaid), TypeScript interfaces, DB schemas, API endpoints ([launch blog](https://kiro.dev/blog/introducing-kiro/)).
- **tasks.md** — sequenced task/sub-task list with dependency ordering; each task links back to requirement IDs (traceability) and includes tests, loading states, accessibility items. Tasks are executable one-by-one from a UI with progress indicators, code diffs, and agent execution history ([launch blog](https://kiro.dev/blog/introducing-kiro/)).
- **Spec/code sync**: ask Kiro to update specs after writing code, or edit a spec to refresh tasks — an explicit answer to "docs rot during implementation" ([launch blog](https://kiro.dev/blog/introducing-kiro/)).
- All spec files are auto-included in conversation context ([kiro.dev docs](https://kiro.dev/docs/specs/best-practices/)).
- Ceremony scales: full spec flow, a lighter quick-plan step, or plain agent chat for small fixes ([AI Agent Square](https://aiagentsquare.com/agents/kiro)).

**Steering** — persistent project/global context in `.kiro/steering/*.md` (default generated trio: `product.md`, `tech.md`, `structure.md`); inclusion modes (always / fileMatch glob / manual); Kiro's analogue of CLAUDE.md / Cursor rules ([ofox.ai guide](https://ofox.ai/zh/blog/kiro-ide-spec-driven-ai-coding-guide-2026/), [designmd.app](https://designmd.app/guides/kiro)). Since Aug 2026, Kiro also loads AGENTS.md as steering context ([kirotutorial.com history](https://kirotutorial.com/history/)).

**Agent hooks** — event-driven automations: natural-language prompt + trigger (file save/create/delete, manual); stored in `.kiro/hooks/`. Examples: regenerate tests on React component save, refresh README when API changes, pre-commit credential scan. Hooks can be committed to git to enforce team-wide standards; Pre/Post Tool Use hooks can block/modify agent actions ([launch blog](https://kiro.dev/blog/introducing-kiro/), [Morph comparison](https://www.morphllm.com/comparisons/kiro-vs-cursor), [ofox.ai](https://ofox.ai/zh/blog/kiro-ide-spec-driven-ai-coding-guide-2026/)). Hooks consume credits like normal agent work ([AI Agent Square](https://aiagentsquare.com/agents/kiro)).

**Other**: property-based testing to *deterministically verify code against the spec* + checkpoints/rollback (GA additions); Auto model router (mixes frontier + specialized models; pinning Sonnet 4.6 ≈ 1.3× credit burn); Kiro Powers (on-demand domain context packs, all plans); MCP support with enterprise server governance; autopilot mode; multi-agent supervision with local sandboxes; image input; parallel task execution ([AI Agent Square](https://aiagentsquare.com/agents/kiro)).

### GitHub Spec Kit

- **Workflow skills** (2026 naming; earlier `/speckit.*` slash commands): `/speckit-constitution` (once per project — non-negotiable principles: code quality, testing standards, performance budgets, UX constraints) → `/speckit-specify` → `/speckit-plan` → `/speckit-tasks` → `/speckit-implement` → `/speckit-converge` (repeat implement→converge until it reports "Converged") ([speckit.org](https://speckit.org/), [Zendoric](https://zendoric.com/en/dia/2026-07-05/15.html)).
- Optional quality gates: `/speckit.clarify` (agent interrogates the spec and resolves ambiguity *before* planning), `/speckit.checklist` (validates the spec itself, not the code — "is this spec solid enough to hand to planning?"), consistency analysis ([data-dynamics.io](https://www.data-dynamics.io/en/blog/spec-kit-specify-clarify)).
- **Artifact layout**: `.specify/` (constitution, templates, scripts) + per-feature directories `specs/NNN-<feature>/` containing spec.md, plan.md, research.md, data-model.md, contracts/, tasks.md, checklists; conventionally one git branch per spec ([Fowler](https://martinfowler.com/articles/exploring-gen-ai/sdd-3-tools.html), [particula.tech](https://particula.tech/blog/spec-driven-development-tools-spec-kit-vs-kiro-vs-tessl)).
- **Three independent entry points** (not one mandatory pipeline): SDD in core; `specify extension add bug` → `/speckit-bug-assess/fix/test` with reports in `.specify/bugs/<slug>/` and an explicit verdict (`verified` / `partial` / `failed`); `specify extension add assess` → idea assessment ending in go / needs-clarification / kill ([speckit.org](https://speckit.org/)).
- Extensibility: extensions, presets, workflows, bundles (role-based packaging), project-local template overrides ([speckit.org](https://speckit.org/)).
- Reported effect: GitHub internal teams ship features with ~10× fewer "regenerate from scratch" cycles vs ad-hoc prompting ([particula.tech](https://particula.tech/blog/spec-driven-development-tools-spec-kit-vs-kiro-vs-tessl) — vendor-reported, treat with care).

### Tessl

- **Framework** (closed beta): CLI that doubles as an MCP server; specs are markdown under `specs/` capturing functional requirements, API contracts, edge cases ("long-term memory" for agents); annotations like `@generate` and `@test` direct generation; `tessl build` emits code stamped `// GENERATED FROM SPEC - DO NOT EDIT`; strict ~1:1 spec-file→code-file mapping at deliberately low abstraction; observed output language JavaScript only ([Fowler](https://martinfowler.com/articles/exploring-gen-ai/sdd-3-tools.html), [CodeMySpec](https://codemyspec.com/blog/tessl-review)).
- **Registry → Skills**: versioned, *evaluated* packages (review evals for structure + task evals on real-world performance), `tessl install <pkg>`, team publishing ([CodeMySpec](https://codemyspec.com/blog/tessl-review)).
- **2026 platform**: "skills → loops → factory" — context control plane, reusable loops (security scans per PR, docs sync, test-and-fix, cost benchmarking, model/harness evals on your own tasks), agent-agnostic (Claude Code, Cursor, Codex, Antigravity, "any agent you run"), free tier of 1,000 credits/month ([tessl.io](https://tessl.io)).

---

## 4. Architecture

- **Kiro**: Code OSS fork; specs/steering/hooks are plain files in `.kiro/` committed to git (portable artifacts, though the workflow doesn't port). Runs Claude models (Sonnet 4.6 / Opus 4.8 on paid tiers) plus an Auto router with specialized intent-detection and caching models; free tier uses open-weight models (Qwen3 Coder Next, DeepSeek v3.2, MiniMax 2.1) ([AI Agent Square](https://aiagentsquare.com/agents/kiro)). CLI v2.x keeps `.kiro/agents`, `.kiro/steering`, hooks as native concepts ([kiro-cli-status note](https://github.com/RaapTechllc/Kiro-Orchestrator-Template/blob/main/docs/research/kiro-cli-status-2026-07.md)).
- **Spec Kit**: deliberately *not* a runtime — a Python CLI (`specify`) that scaffolds agent-specific skill files, markdown templates, and shell scripts into your repo. The agent itself executes the pipeline by reading the generated artifacts; Spec Kit never sits between agent and model ([speckit.org](https://speckit.org/)). This is why 30+ agent integrations are feasible.
- **Tessl**: CLI + MCP server layer *above* agents; generation engine is non-deterministic LLM codegen keyed to spec annotations; registry is centralized with eval pipelines ([Fowler](https://martinfowler.com/articles/exploring-gen-ai/sdd-3-tools.html), [CodeMySpec](https://codemyspec.com/blog/tessl-review)).
- Specs live **in the repo as markdown** in all three (Kiro `.kiro/specs/`, Spec Kit `specs/` + `.specify/`, Tessl `specs/`) — no proprietary app-state silo; the divergence is in what wraps them (IDE vs CLI-scaffolded templates vs registry/MCP layer).

---

## 5. Strengths (community-reported)

- Kiro's spec flow genuinely rescues vibe-coded prototypes: documented reasoning, requirement↔task traceability, and checkable task lists; reviewers call it "the most structured way to do AI-assisted development today" with unusually transparent pricing (8.2/10) ([AI Agent Square](https://aiagentsquare.com/agents/kiro)).
- **Agent hooks are the sleeper hit** — "quietly one of Kiro's best ideas"; enforce the chores developers skip (tests/docs on save), and committed hooks give the whole team identical quality gates ([AI Agent Square](https://aiagentsquare.com/agents/kiro), [HostAdvice](https://hostadvice.com/ai-app-builders/kiro-review/)).
- EARS requirements surface hidden assumptions before code exists; "bringing rigorous engineering discipline back to AI" ([Geshan's blog](https://geshan.com.np/blog/2026/05/aws-kiro/), [launch blog](https://kiro.dev/blog/introducing-kiro/)).
- Spec Kit: kills ad-hoc prompting breakdown on multi-step work; the **constitution + clarify + checklist gates** catch ambiguity early; agent-agnosticism is the decisive feature for teams standardized on Claude Code/Cursor/Copilot ([particula.tech](https://particula.tech/blog/spec-driven-development-tools-spec-kit-vs-kiro-vs-tessl), [Zendoric](https://zendoric.com/en/dia/2026-07-05/15.html)). 111k+ stars = real practitioner pull.
- Tessl: versioned, *evaluated* skill/spec packages are "a real improvement over scattered markdown"; MCP-native and agent-agnostic; governance angle (scanning, audit, policy gating) is genuinely enterprise-sellable ([CodeMySpec](https://codemyspec.com/blog/tessl-review)).

## 6. Weaknesses (community-reported)

- **Verbosity / review overload (all tools, worst in Spec Kit)**: generated planning markdown is repetitive and exhaustive; "I'd rather review code than all these markdown files" — reviewers read hundreds of lines to approve a 3-file change ([Fowler](https://martinfowler.com/articles/exploring-gen-ai/sdd-3-tools.html), [particula.tech](https://particula.tech/blog/spec-driven-development-tools-spec-kit-vs-kiro-vs-tessl)).
- **Problem-size mismatch**: Kiro turned a small bug fix into 4 user stories + 16 acceptance criteria; on a mid-sized feature Spec Kit generated so many artifacts Böckeler never finished implementation and judged plain AI-assisted coding faster ([Fowler](https://martinfowler.com/articles/exploring-gen-ai/sdd-3-tools.html) via [Daniliants summary](https://daniliants.com/insights/understanding-spec-driven-development-kiro-spec-kit-and-tessl/)).
- **False sense of control**: despite constitution files and checklists, agents still ignored instructions (recreating existing classes) or over-applied rules ([Fowler](https://martinfowler.com/articles/exploring-gen-ai/sdd-3-tools.html)).
- **Kiro pricing/metering fiasco** (Aug 2025): spec-request/vibe-request split, 5× cost for spec requests, quota evaporating in 15 minutes, "$550–1,950/mo for real use"; AWS blamed a metering bug, refunded August, and later moved to flat credits ([The Register](https://www.theregister.com/software/2025/08/18/aws-pricing-for-kiro-dev-tool-a-wallet-wrecking-tragedy/1467855), [r/kiroIDE](https://www.reddit.com/r/kiroIDE/comments/1mf3ef7/new_blog_post_kiro_pricing_update_waitlist/)). Credit burn remains hard to forecast even now ([AI Agent Square](https://aiagentsquare.com/agents/kiro)).
- **Kiro IDE lock-in**: spec features require *their* IDE; Open VSX lacks some Microsoft-proprietary extensions ([specs.md comparison](https://specs.md/compare/vs-kiro), [AI Agent Square](https://aiagentsquare.com/agents/kiro)).
- **Production-safety precedent**: Dec 2025 — Kiro (internal, operator-level credentials) deleted and recreated an AWS Cost Explorer production environment → 13-hour outage; Amazon blamed "misconfigured access controls/user error," disputed by reporting. Canonical case for external control planes and permission scoping around agents ([The Register via doit.com](https://www.doit.com/blog/spec-driven-development-with-kiro-ai-code-ownership), [Regulayer incident record](https://regulayer.com/consequences/aws-kiro-delete-and-recreate)).
- **Tessl's non-deterministic compiler**: same spec → different code each run; Böckeler had to iterate the spec itself "to increase the repeatability of the code generation" — undermining "the spec is the source of truth" ([Fowler](https://martinfowler.com/articles/exploring-gen-ai/sdd-3-tools.html), [CodeMySpec](https://codemyspec.com/blog/tessl-review)).
- **Tessl execution gap & pivot**: $125M and ~9 months of closed beta; Framework still not GA; Jan 2026 pivot to skills registry/governance suggests spec-as-source didn't survive contact with the market ([CodeMySpec](https://codemyspec.com/blog/tessl-review)).
- **SDD is weakly validated on brownfield**: all tools are hardest to introduce into existing codebases; no long-term maintenance reports existed as of the Fowler teardown ([Fowler](https://martinfowler.com/articles/exploring-gen-ai/sdd-3-tools.html)).

---

## 7. Distinctive ideas worth borrowing

1. **EARS acceptance criteria as the requirements grammar** (Kiro). Force generated requirements into WHEN/THEN/IF-THEN form. Constraining the *shape* of requirements makes ambiguity machine-detectable and gives the agent explicit edge cases. Cheap to implement: a template + lint pass over `requirements.md`.
2. **Requirement↔task traceability links** (Kiro). Every task references the requirement IDs it satisfies, so nothing "falls through the cracks" and coverage is auditable. Enables a mechanical "which requirements lack tasks/tests?" check.
3. **Project constitution as a once-per-repo artifact** (Spec Kit). A layer *above* per-feature specs holding non-negotiables (testing standards, perf budgets, banned libraries) that every subsequent spec/plan must satisfy. Distinct from steering/rules files: constitution gates *specs*, rules condition *behavior*.
4. **Spec-quality gates before planning** (Spec Kit). `clarify` (agent must interrogate and resolve ambiguities first) and `checklist` (validate the spec itself — not the code — before it's allowed downstream). The gate target is the artifact, not the output.
5. **Event-driven agent hooks** (Kiro). Natural-language prompt + filesystem/tool-event trigger, committed to git so the *team* shares the automation; Pre/Post Tool Use hooks can block or modify agent actions. Generalizes "CI for agent behavior" to the local loop.
6. **Deterministic verification against the spec** (Kiro GA). Property-based testing to check code actually satisfies stated acceptance criteria — machine-checked conformance instead of asking another LLM "does this match?" ([AI Agent Square](https://aiagentsquare.com/agents/kiro)).
7. **Convergence loop as an explicit terminal state** (Spec Kit). `implement → converge` repeated until a verifier declares "Converged" — and the bug extension refuses to call a fix successful without verification (`verified/partial/failed` verdicts). Exit criteria are first-class, not vibes.
8. **Versioned, evaluated context packages** (Tessl registry/skills). Treat reusable specs/skills like software: semver, install command, review evals + task evals measuring real performance. The one part of Tessl's vision that demonstrably shipped and is worth copying wholesale.

---

## 8. Lessons for loongagent

**Copy:**

- Make specs plain markdown files in the repo (all three tools converged here). Files survive tool churn, diff well in PR review, and are agent-portable. Never put specs in proprietary app state.
- Scale ceremony to task size — the single strongest cross-cutting finding. Kiro's three modes (full spec / quick-plan / plain chat) are the right shape; auto-triage (multi-file × multi-session × multi-person × genuine ambiguity ⇒ full SDD; else skip) ([particula.tech triage rule](https://particula.tech/blog/spec-driven-development-tools-spec-kit-vs-kiro-vs-tessl)).
- Spec-anchored maintenance: bidirectional sync (code→spec update, spec→task refresh) like Kiro's; a spec that rots after first implementation is just a fancy prompt transcript.
- Steer the workflow with verifiable gates: clarify/checklist before planning; property-based/deterministic checks after implementation; explicit converge verdicts.
- Hooks (event-triggered agents, git-committed, team-shared) as a first-class control-plane primitive — this maps directly onto loongagent's backend control plane ambition and none of the already-covered agents own it.
- Governance hooks into cost: Kiro's default-off overages and per-prompt credit display are genuinely procurement-friendly designs for loongagent's cost governance goals ([AI Agent Square](https://aiagentsquare.com/agents/kiro)).

**Avoid:**

- **Don't pursue spec-as-source.** Tessl — the best-funded attempt ($125M, strong founder) — shipped JavaScript-only, 1:1 spec-to-file, non-deterministic regeneration, then pivoted away. Non-deterministic codegen makes "spec is truth" unfalsifiable; the MDD (model-driven development) historical parallel applies: right idea, awkward abstraction level ([Fowler](https://martinfowler.com/articles/exploring-gen-ai/sdd-3-tools.html), [CodeMySpec](https://codemyspec.com/blog/tessl-review)).
- **Don't generate verbose planning prose that humans must review.** Review burden shifts, it doesn't shrink. Prefer terse, structured, diffable artifacts; treat every generated line as a review tax.
- **Don't bind the spec workflow to one surface.** Kiro's lock-in cost it users; Spec Kit's 30+ agent support is why it won adoption. For loongagent's multi-surface vision (TUI/IDE/web/app), the spec layer must be a portable protocol consumed identically everywhere — a control-plane concern, not a UI feature.
- **Don't meter "spec requests" and "chat requests" differently** — Kiro's Aug 2025 debacle shows workload-shaped pricing users can't predict breeds instant revolt ([The Register](https://www.theregister.com/software/2025/08/18/aws-pricing-for-kiro-dev-tool-a-wallet-wrecking-tragedy/1467855)).
- **Never let agents inherit operator-level credentials without external approval gates** (the Dec 2025 Kiro/Cost Explorer outage). Specs govern intent; permissions still need an independent control plane ([Regulayer](https://regulayer.com/consequences/aws-kiro-delete-and-recreate)).

**Do differently (open gaps):**

- Brownfield is unsolved everywhere — a spec system that can *excavate* specs from existing code (reverse-generate requirements/design for legacy modules, then keep them anchored) would be genuinely novel.
- Nobody maintains specs across features well: each tool's specs are per-task islands. A project-level spec graph (features referencing shared requirements, impact analysis on spec edits) is unclaimed territory.
- Tessl's registry idea applied to *specs themselves* (versioned, eval-tested spec packages per domain) is only half-built — an opportunity for loongagent's backend control plane.
