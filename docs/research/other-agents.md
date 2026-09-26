# Other Coding Agents — Comparative Scan

One-line description: a comparative teardown of nine notable coding agents (Aider, Cline/Roo Code, Gemini CLI, Amp, Windsurf, Devin, GitHub Copilot, Jules, OpenHands), focused on the distinctive design ideas a new agent should steal.

**Researched: 2026-09-26**

Sources are a mix of official docs/blogs, GitHub repos, maker essays, and community reviews; inline links throughout. Where the landscape shifted in 2026 (Roo Code archival, Windsurf→Cognition, Amp's rewrite), the current state is flagged.

---

## 1. Aider

### What it is
- Open-source (Apache 2.0) terminal pair-programmer by Paul Gauthier; Python; runs locally, BYO API keys, model-agnostic via litellm. Free; you pay your LLM provider directly.
- Surfaces: CLI only (plus a light browser UI and editor integrations via `--watch-files` comment-driven mode).

### Core design philosophy
- "AI pair programming in your terminal" — chat-driven, git-native. Git is the safety net: every AI change is auto-committed with a semantic message, `/undo` reverts the last commit ([Aider guide](https://jiangren.com.au/blog/aider-guide-03-core-features)).
- Explicit user-managed context instead of agentic search: you `/add` the files that matter; aider optimizes *how to edit* them, not whether it may.

### Feature inventory
- **Repo map**: tree-sitter parses every file into (file → key symbols + signatures), a dependency graph is built between files, and a graph-ranking (PageRank-style) algorithm picks the most relevant slice to fit a token budget (`--map-tokens`, default ~1k; budget is multiplied up to 8× when no files are in chat). Sent with every change request ([aider.chat repo map docs](https://aider.chat/docs/repomap.html); mechanism teardown: [ArcYou](https://arcyou.ai/teardowns/aider-repo-map/)).
- **Edit formats** (the other half of the product): `whole` (rewrite file), `diff` (git-conflict-style `<<<<<<< SEARCH / ======= / >>>>>>> REPLACE` blocks), `diff-fenced` (path inside fence, for Gemini models that mangle the diff format), `udiff` (simplified unified diff, introduced to stop GPT-4-Turbo's "lazy coding" `// ... rest unchanged` elisions), and `editor-diff`/`editor-whole` for architect mode ([aider.chat edit formats](https://aider.chat/docs/more/edit-formats.html)). Aider benchmarks edit formats per model and auto-picks the best.
- **Architect/editor mode**: a strong "architect" model describes changes in prose; a cheap "editor" model turns that into syntactically correct edits — splits reasoning from formatting.
- Slash commands (`/add`, `/drop`, `/undo`, `/diff`, `/run`, `/lint`, `/test`), lint+test auto-fix loops, voice input, `.aiderignore`, CONVENTIONS.md / `.aider.conf.yml`, read-only files, auto-commit.
- Loop shape: not a classic tool-calling ReAct loop — historically a chat→edit→commit loop with the user curating context.

### Architecture
- Single Python process; tree-sitter for symbol extraction; networkx PageRank over the file-dependency graph; all state lives in git + the chat history file. Context = chat files (full content) + repo map slice + conventions files.

### Strengths
- Token efficiency on large repos from the repo map; best-in-class git integration makes AI edits feel safe and revertible; polyglot benchmarks (aider's own leaderboard) keep edit formats honest; completely model-agnostic ([digitalapplied deep dive](https://www.digitalapplied.com/blog/aider-deep-dive-cli-agentic-coding-tutorial-2026)).

### Weaknesses
- Malformed search/replace blocks from weaker models cause retry loops that burn tokens ([aider issue #2385](https://github.com/Aider-AI/aider/issues/2385)).
- Manual context curation (`/add`) is friction vs agentic search tools; weaker at big multi-step autonomous tasks; single-repo, single-session mindset.

### Distinctive ideas worth borrowing
1. **Ranked repo map instead of RAG embeddings** — syntax-level symbol index + graph centrality, tuned to a token budget. Deterministic, cheap, debuggable.
2. **Edit format as a per-model adapter** — benchmark multiple edit syntaxes per model and pick empirically; treat "how the model expresses edits" as a pluggable layer.
3. **Architect/editor split** — decouple reasoning from edit serialization; a cheap model can render a smart model's plan into diffs.
4. **Git-as-undo** — every agent turn commits; rollback is `git reset`, diffing is `git diff`. Zero custom checkpoint code needed.

---

## 2. Cline / Roo Code

### What it is
- Cline: open-source (Apache 2.0) VS Code extension agent, BYO key, any model incl. local (Ollama/LM Studio); ~48k+ GitHub stars ([Cline docs](https://docs.cline.bot/getting-started/getting-started-new-coders)). Free extension; you pay the provider.
- Roo Code: community fork of Cline that added custom modes and per-mode rules; **archived/shut down May 2026, officially recommending migration back to Cline** ([migration guide](https://cursor-alternatives.com/blog/roo-code-rules/)) — verify current status before relying on this.

### Core design philosophy
- "Complete transparency": every file read, decision, and token is visible; human approves every step unless auto-approve is on. "Zero trust by design — your code never touches our servers" ([Cline docs](https://docs.cline.bot/getting-started/getting-started-new-coders)).
- No inference games: "We don't profit from AI usage... we give you unrestricted access to any model's full capabilities."

### Feature inventory
- **Plan/Act mode split**: Plan mode explores the codebase and produces a plan without touching code; Act mode executes. The original, widely-copied implementation ([openalternative overview](https://openalternative.co/alternatives/devin)).
- Tools: read/write files (diff edits with inline review), run terminal commands, browser automation (headless browser tool), MCP servers (Cline ran an early "MCP marketplace").
- Context: **Focus Chain** (auto-managed todo list with live progress), **Auto Compact** (summarize when context fills), **Deep Planning** (investigate → clarifying questions → written plan), `.clinerules` files incl. glob-conditional rules (`paths:` frontmatter) and global rules dir ([rules guide](https://qaskills.sh/blog/cline-roo-code-rules-configuration-guide), [rules-file survey](https://hussamahmed.com/ai/ide-agents/rules-files-across-tools)).
- Roo's additions (now folding back into Cline): **custom modes** — user-defined personas each with their own system prompt, tool allow-list, and `.roo/rules-{mode}/` scoped instructions (mode-scoped MCP permissions = "structural safety, not instructed safety") ([Roo vs Cline](https://fast.io/resources/roo-code-vs-cline/)).

### Architecture
- TypeScript VS Code extension; webview UI; everything client-side; state per task (checkpoints via git shadow repo for file rollback).

### Strengths
- Transparency + granular approval UX; best open BYO-model option inside VS Code; MCP ecosystem leadership; checkpoint/restore of workspace state.

### Weaknesses
- **Token gluttony**: recurring reports of $50–200/day autonomous spend and context blowouts (one GitHub issue claims 441k context where cents were expected) ([cline issue #7558](https://github.com/cline/cline/issues/7558), [cost breakdown](https://joingonka.ai/en/knowledge/cline-burn/), [Enterprise DNA practitioner review](https://enterprisedna.co/resources/blog/practitioner-cline-vscode/)).
- Human-in-the-loop-per-step gets click-fatiguing; Roo's fork fragmentation confused users and ended in archival.

### Distinctive ideas worth borrowing
1. **Plan/Act as an explicit state machine**, not just a prompt — plan mode physically cannot edit files.
2. **Custom modes = composable system prompts + tool allow-lists** (Roo's real legacy): let users define "architect/ask/debug" personas with capability restrictions enforced structurally.
3. **Glob-conditional rules** (`paths:` frontmatter) so rules load only when relevant files are in context — token-efficient instruction scoping.
4. **Checkpoints** that snapshot both conversation and workspace (git shadow repo) for one-click rewind.

---

## 3. Gemini CLI

### What it is
- Google's open-source (Apache 2.0) terminal agent, TypeScript/Node, `npx @google/gemini-cli`; free tier 60 req/min + 1,000 req/day with a personal Google account ([Gemini CLI GitHub](https://github.com/google-gemini/gemini-cli)). 100k+ stars within a year ([review](https://www.aicentralresources.com/tool/gemini-cli)).

### Core design philosophy
- "Lightweight access to Gemini... the most direct path from your prompt to our model" — a reference ReAct loop built in the open, plus a vehicle for the 1M-token context window and generous free quota as adoption strategy.

### Feature inventory
- Loop: explicit **ReAct (reason-and-act)** loop with built-in tools and MCP servers ([Milvus explainer](https://milvus.io/ai-quick-reference/what-is-gemini-cli)).
- Built-in tools: Google Search grounding (a first-party tool, not MCP), file ops, shell, web fetch, memory tool.
- Context: `GEMINI.md` context files (hierarchical: global `~/.gemini`, project, subdirectory), conversation checkpointing (save/resume sessions), token caching.
- Sandboxing: opt-in Docker/Podman/macOS Seatbelt sandbox for tool execution; trusted-folders execution policies.
- Extensibility: MCP, custom slash commands (TOML files), extensions framework, settings.json hierarchy (system/user/project).
- Non-interactive scripting: `-p` prompt mode, `--output-format json` and `stream-json` for CI pipelines; Gemini CLI GitHub Action for PR review/issue triage.

### Architecture
- Node/TypeScript monorepo (packages/cli + packages/core); core exposes turn-based scheduler over tool calls; IDE companion via VS Code extension that shares context with the terminal session.

### Strengths
- Free quota made it the default "try agentic coding" tool; 1M-token context window usable directly; genuinely open governance (public issues/PRs/roadmap); simple, hackable codebase — the "read the source" reference agent.

### Weaknesses
- Code output quality and long-session instruction-following trailed category leaders; users "used Claude Code for the hard problems and Gemini CLI for breadth" ([codegen.com tool profile](https://codegen.com/ai-tools/gemini-cli/), [Ofox comparison](https://ofox.ai/blog/agentic-coding-claude-codex-gemini-cursor-2026/)). Agent drift on long sessions; sandbox off by default.

### Distinctive ideas worth borrowing
1. **Search grounding as a built-in tool** — a first-party, always-available web search tool wired into the loop (no MCP setup) makes the agent self-updating on libraries/APIs.
2. **Free-tier-as-distribution** — generous personal quota to build habits and a plugin/MCP ecosystem.
3. **Headless modes as a first-class surface** (`-p`, `--output-format stream-json`) so the same agent is scriptable in CI.
4. **Hierarchical context files** (global → project → subdirectory `GEMINI.md`) with import support.

---

## 4. Amp (Sourcegraph)

### What it is
- Sourcegraph's agent, "built for the frontier": CLI/TUI, web, macOS/iOS apps, editor integrations (VS Code forks, Neovim, Zed), remote "orb" machines per thread. Usage-based pricing; multi-model (GPT-5.6, Claude Fable 5.1 per current manual) — **no model picker**; also "Puck", a meta-agent coordinating threads ([Amp manual](https://ampcode.com/manual), [Sid Bharath's Aug-2026 guide](https://sidbharath.com/blog/amp-code-guide/)).

### Core design philosophy
- "**An LLM, a loop, and enough tokens. The rest is elbow grease**" — Thorsten Ball's canonical essay demonstrates a working code-editing agent in <400 lines of Go with 3 tools ([How to Build an Agent](https://ampcode.com/how-to-build-an-agent)).
- "**Unconstrained tokens**": don't ration context or cap the agent; let it spend what the task needs (recurring theme on their *Raising an Agent* podcast, e.g. [episode 5](https://ampcode.com/podcast/episode-5)).
- Opinionated and disposable: "If we don't use and love a feature, we kill it. No backward compatibility, no legacy features" ([manual](https://ampcode.com/manual)); Amp picks the model stack and routing, you pick a **capability dial** (`low`→`ultra`).

### Feature inventory
- **Threads as durable objects**: the real unit of work — shareable URLs, resumable, controllable from another device; notifications when a thread needs you.
- **Oracle subagent**: a second-opinion reasoning model the main agent calls to challenge diagnoses; distinct from **Task subagents** which get isolated context windows for parallel search/review work (spawned automatically) ([Sid Bharath guide](https://sidbharath.com/blog/amp-code-guide/), [podcast ep. 6](https://ampcode.com/podcast/episode-6)).
- **Orbs**: managed remote machines created per thread with code/tools/plugins pre-set; agents keep running after your laptop closes; `amp --no-tui` turns your own machine into a headless runner for remotely-created threads.
- **Skills** (`.agents/skills/`, loaded on task match — can bundle an `mcp.json` so MCP tools stay hidden until the skill loads) and **plugins** (tools, commands, even experimental model modes like a Kimi K3 mode).
- `AGENTS.md` for repo instructions (hierarchical, per-package files); aggressive default permissions (runs tools without asking), policy plugins for teams.

### Architecture
- Go-based CLI; server-mediated threads (hence cross-device resume); local execution in CLI mode, remote in orbs; tool-call loop close to the 400-line essay's skeleton (read_file / list_files / edit_file-as-string-replacement — notably the same `old_str`/`new_str` edit tool as Claude Code).

### Strengths
- Users love the speed, the "it just picks the right model" routing, thread sharing, and the taste level ("a coding agent built by people who use coding agents all day") ([Sid Bharath](https://sidbharath.com/blog/amp-code-guide/), [hamel.dev notes](https://hamel.dev/notes/coding-agents/amp.html)).

### Weaknesses
- Relentless churn deletes workflows users depended on ("aged like milk" rewrites); cost climbs fast with high/ultra + Oracle + subagents; few knobs if you distrust the routing opinions ([Sid Bharath](https://sidbharath.com/blog/amp-code-guide/)).

### Distinctive ideas worth borrowing
1. **Capability dial instead of model picker** — the user states task difficulty (`low`→`ultra`); the harness routes models per role (main/oracle/subagent) and re-routes silently as the frontier moves. Decouples UX from model churn.
2. **Oracle: an explicit second-opinion tool** the main agent can call mid-task — cheap to implement (a tool that calls a bigger model with the gathered context), big payoff on wrong-theory debugging.
3. **Threads as portable, shareable artifacts** — full transcripts as team knowledge ("show how the agent reached the result"), not just a diff.
4. **Skills that lazily load MCP servers** — tools enter context only when the skill triggers; fixes the "100 MCP tools eating the context window" problem.
5. **Unconstrained tokens as product stance** — never truncate/skim to save money; make cost visible instead of capping capability.

---

## 5. Windsurf (Cascade) — now Cognition

### What it is
- AI-native IDE (VS Code fork, formerly Codeium) built around the Cascade agent; acquired by Cognition in July 2025 after the OpenAI deal collapsed and Google acqui-hired the founders ($2.4B licensing) ([deal timeline](https://www.klover.ai/ai-strategy-windsurf-acquired-by-openai-then-google-then-cognition-ai/), [Capwave recap](https://capwave.ai/blog/windsurf-cognition-and-google-what-just-happened-in-ai)). Windsurf 2.0 (Apr 2026) embeds Devin for cloud execution; subscription pricing (~$20 Pro tier, credit system removed) ([fundesk review](https://www.fundesk.io/windsurf-ide-review-codeium-ai-editor-2026)).

### Core design philosophy
- "**Flow awareness**": the agent tracks *everything the developer does* — edits, terminal commands, clipboard, open files, conversation history — on a shared human/AI timeline, inferring intent instead of requiring explicit context ([codeium.com/cascade](https://codeium.com/cascade), [Alex Merced's context guide](https://iceberglakehouse.com/posts/2026-03-context-windsurf/)).

### Feature inventory
- Cascade agent: multi-file edits, terminal execution, Code/Chat modes, web search, built-in browser + preview, one-click deploys.
- **Riptide** indexing: fast codebase indexing feeding Cascade's context ([youraitoolstack review](https://youraitoolstack.com/blog/windsurf-review-2026)).
- Windsurf Tab: autocomplete + next-edit prediction ("Tab to jump/import").
- Memories (auto-saved context), `.windsurf/rules` + global rules, MCP support, own SWE-1.5 model family on paid plans; Devin handoff: plan locally in Cascade, execute in a cloud VM.

### Architecture
- VS Code fork (Electron); proprietary indexing (Riptide) + action-telemetry pipeline feeding a context engine; own inference infra for Tab/SWE models plus frontier models.

### Strengths
- Zero-friction context — no manual file tagging, the agent "already knows what you were doing"; smooth Tab→Cascade continuum; polish of the IDE UX ([Tooliverse](https://tooliverse.ai/tools/windsurf)).

### Weaknesses
- Telemetry-heavy awareness raises privacy questions; post-acquisition turbulence (price hikes, rough edges per [fundesk](https://www.fundesk.io/windsurf-ide-review-codeium-ai-editor-2026)); locked to the fork — extension/ecosystem lag vs VS Code.

### Distinctive ideas worth borrowing
1. **Implicit context from a shared action timeline** — treat user actions (edits, commands, clipboard) as first-class agent input; intent inference beats explicit `@file` tagging.
2. **Autocomplete and agent as one system** (Tab ↔ Cascade share context) so suggestions anticipate the agent's plan and vice versa.
3. **Local-plan → cloud-execute handoff** inside one product (Cascade → Devin): the IDE is the steering wheel, the cloud VM is the engine.

---

## 6. Devin (Cognition)

### What it is
- Cognition's "autonomous AI software engineer": cloud SaaS, web app + Slack/Teams + API + Devin CLI/Desktop IDE; per-seat + ACU (compute unit) usage pricing, ~$20 entry ([Devin docs](https://docs.devin.ai/), [morphllm comparison](https://www.morphllm.com/comparisons/devin-vs-claude-code)).

### Core design philosophy
- Delegation, not collaboration: "What task can I finish for you while you're away?" Assign a ticket, get a PR ([usecarly comparison](https://www.usecarly.com/blog/devin-vs-cursor/)). Rule of thumb from docs: "if you can do it in three hours, Devin can most likely do it."

### Feature inventory
- Full cloud workspace per session: **shell + browser + embedded IDE** the human can watch live *and take over* mid-session ([docs](https://docs.devin.ai/)).
- Integrations: Linear/Jira tickets, Slack tagging, GitHub PRs; `/handoff` from local CLI to cloud Devin; Devin API; knowledge base ("Devin's machine-learned notes" per repo); Playbooks (repeatable runbooks).
- Planning step before execution; session replay/sharing; parallel sessions for backlog burn-down.
- Best practices enforced culturally: explicit completion criteria, verifiable tasks ("CI passes"), scoped decomposition.

### Architecture
- Sandbox cloud VM per task with shell/browser/editor; original 2024 announcement claimed long-horizon planner + sub-agents, 13.86% SWE-bench at launch (vs 1.96% prior baseline) ([nevo.systems overview](https://nevo.systems/blogs/nevo-journal/ai-agents-write-code)).

### Strengths
- Genuinely good at the "boring, verifiable 10–20% of the backlog" — migrations, dependency upgrades, test writing; async parallelism (many Devins at once); the watch-and-take-over IDE bridges trust gaps ([saas.com.ai review](https://saas.com.ai/devin-ai-review/)).

### Weaknesses
- **Reliability and cost predictability** are the recurring complaints — independent tests and reviewer scores are middling (5.8/10, "a liability anywhere near ambiguity") ([bestaiq](https://bestaiq.com/devin-review/), [saas.com.ai](https://saas.com.ai/devin-ai-review/)); ACU metering ("read the meter, not the headline"); 2024-era demo credibility damage lingers (independent reproductions showed cherry-picking — mark as historical).

### Distinctive ideas worth borrowing
1. **Human takeover of a live agent session** — same IDE, same shell: the human and agent share one environment instead of handing off diffs.
2. **Verifiability as the scoping discipline** — product docs push users to define machine-checkable completion criteria; the agent's loop ends at "CI green", not "looks done".
3. **Session as a replayable artifact** (recording of shell/browser/edits) for review and trust.
4. **Async-ticket interface** (Slack/Linear as the primary UI): the PR, not the chat, is the output.

---

## 7. GitHub Copilot — agent mode + coding agent

### What it is
- GitHub/Microsoft. Two related things: **agent mode** (local, inside VS Code/JetBrains/Visual Studio/Xcode/Eclipse) and the **coding agent** (cloud: assign an issue → agent works in GitHub Actions → draft PR). Subscription tiers (Free/Pro $10/Pro+ $39/Business/Enterprise; usage-based "premium request" billing since mid-2026) ([GitHub blog: meet the new coding agent](https://github.blog/news-insights/product-news/github-copilot-meet-the-new-coding-agent/), [refontelearning 2026 explainer](https://www.refontelearning.com/blog/github-copilot-agent-mode-what-it-does), [penchan pricing notes](https://penchan.co/zh-cn/ai/coding/github-copilot-tutorial/)).

### Core design philosophy
- "Keep you in the magical flow state... To do the things you don't want to do" (Dohmke, [GitHub blog](https://github.blog/news-insights/product-news/github-copilot-meet-the-new-coding-agent/)). The coding agent "excels at low-to-medium complexity tasks in well-tested codebases" — deliberately scoped, not "replace the engineer".

### Feature inventory
- Coding agent flow: assign issue → VM boots → repo clone → env configured via Actions → **RAG over GitHub code search** → commits pushed to a **draft PR** you watch live; reads issue/PR discussions, images in issues (vision), custom repo instructions; review comments on the PR are picked up as new instructions ([GitHub blog](https://github.blog/news-insights/product-news/github-copilot-meet-the-new-coding-agent/)).
- Security posture (the distinctive part): agent pushes only to branches it created; **the requester cannot approve the agent's PR**; agent's internet access is an allowlist; Actions workflows don't run without human approval; honors existing branch protections/rulesets.
- Agent mode (local): edit across files, run terminal commands, iterate on lint/test failures; MCP support; custom agents/`AGENTS.md`; agentic code review on PRs.
- Compute decision: the cloud agent runs on **GitHub Actions** — "the largest CI/CD ecosystem in the world" — reusing existing org infra, billing, and policy surfaces.

### Architecture
- Local agent mode = in-IDE loop on your CPU/devcontainer; coding agent = Actions runner + full clone + GitHub-code-search RAG ([architecture comparison](https://architecturediagram.ai/blog/ai-coding-agent-architecture)).

### Strengths
- Zero-friction adoption where code already lives; governance-by-default is enterprise-catnip; the issue→PR loop maps 1:1 onto existing team workflows ([GitHub blog quotes: EY, Carvana](https://github.blog/news-insights/product-news/github-copilot-meet-the-new-coding-agent/)).

### Weaknesses
- Premium-request metering is confusing and was repriced mid-2026 (credit switch, Actions minutes for code review) ([AICoderScope deep dive](https://aicoderscope.com/blog/github-copilot-agent-mode-deep-dive-2026/)); agent quality perceived as a step behind Claude Code/Cursor on hard tasks; model access gated by plan tier.

### Distinctive ideas worth borrowing
1. **The platform is the sandbox**: run the cloud agent on the CI system the org already trusts (Actions) — inherits secrets policy, logging, billing, network controls for free.
2. **Governance defaults**: requester-can't-approve, allowlisted egress, draft-PR-only, workflow-run approval. Security as features you enumerate, not a settings page.
3. **PR-as-conversation**: review comments become the agent's next prompt — reuse the code-review UX as the steering interface.
4. **Honest scoping**: market "low-to-medium complexity in well-tested codebases" instead of autonomy hype.

---

## 8. Jules (Google)

### What it is
- Google's asynchronous cloud coding agent: connect GitHub repos → Jules clones into a cloud VM → proposes a plan → executes → opens a PR. Free tier (~15 tasks/day historically), paid via Google AI plans; REST API + Jules CLI + GitHub trigger by label ([kie.ai overview](https://kie.ai/blog/what-is-jules), [Zenn API guide](https://zenn.dev/kai_kou/articles/224-jules-api-coding-agent-python-guide?locale=en), [aiidelist feature list](https://aiidelist.com/ide/jules)).

### Core design philosophy
- "Works while you sleep": fully async batch delegation vs babysat synchronous chat ([LinkedIn/dev.to coverage](https://dev.to/pulkitgovrani/jules-googles-async-ai-coding-agent-actually-changes-how-i-think-about-code-reviews-5288)).

### Feature inventory
- **Plan approval gate**: Jules presents an explicit plan; you approve/edit before code is written — with a clever default: **no response within ~5 minutes auto-approves** ([zackproser hands-on](https://zackproser.com/blog/google-jules-review)).
- **Critic agent / critic-augmented generation**: an internal adversarial reviewer challenges every proposed change (bugs, edge cases, security) *before* results are shown ([Google Developers Blog](https://developers.googleblog.com/zh-hans/meet-jules-sharpest-critic-and-most-valuable-ally/)).
- Environment snapshots: capture the VM's post-setup state so subsequent tasks skip reinstalling dependencies; setup scripts per repo ([Jules Week coverage](https://dev.to/daleymottley/jules-20-googles-asynchronous-ai-coding-agent-actually-changes-how-i-think-about-code-reviews-5288)).
- Concurrency: multiple tasks in parallel VMs; responds to CI failures on its own PRs; memory (Jules writes/edits its own knowledge base); proactive "todo bot" direction ([startuphub](https://www.startuphub.ai/news/proactive-agents-shifting-from-reactive-ai-to-intelligent-collaboration)).
- Audio changelogs (tasks summarized as a podcast-style briefing) — a quirky but real feature.

### Architecture
- Cloud VM per task, full repo clone, Gemini models (2.5 Pro era → current); not open source; GitHub-app mediated.

### Strengths
- Zero babysitting; generous free tier; plan-then-execute with critic pass produces cleaner first PRs than raw one-shot agents; environment snapshots make repeated tasks fast.

### Weaknesses
- Gemini-model quality ceiling on hard tasks (same complaint family as Gemini CLI); closed source, limited customization (no MCP-class extensibility historically); async latency makes iterative steering clunky; per-task VM cold start without snapshots.

### Distinctive ideas worth borrowing
1. **Critic pass before surfacing output** — an adversarial self-review stage in the pipeline, separate from the builder context.
2. **Environment snapshots** — snapshot the configured VM; amortize setup cost across all future tasks in that repo.
3. **Auto-approve with a timeout** — default-to-progress UX for async agents; the plan gate protects without blocking.
4. **CI-failure feedback loop on its own PRs** — the agent watches its PR's checks and iterates autonomously.

---

## 9. OpenHands (formerly OpenDevin)

### What it is
- The leading open-source (MIT) autonomous-agent platform (All-Hands AI, from the academic OpenHands project); ~60k+ stars; web UI, CLI, REST API, cloud offering, and **Agent Canvas** (2026): a self-hosted control center that runs OpenHands *or any ACP-compatible agent* (Claude Code, Codex, Gemini) across local/Docker/VM/cloud backends ([OpenHands GitHub README](https://github.com/All-Hands-AI/OpenHands)).

### Core design philosophy
- "An open platform for AI software developers as generalist agents" — research-grade openness: reproducible agents, published architecture, benchmark-driven ([OpenHands paper, arXiv:2407.16741](https://arxiv.org/html/2407.16741v3)).

### Feature inventory
- **CodeAct**: the agent's action space *is executable code* (Python/bash run in the sandbox) rather than JSON tool calls — per the CodeAct paper, ~20% higher multi-step task success than traditional tool use ([paper](https://arxiv.org/html/2407.16741v3), [pattern survey](https://masonailab.com/tech/ai-agent-design-patterns-2026/)).
- **Event-stream architecture**: every agent↔environment interaction is a typed event (Action/Observation) flowing through a central hub — full auditability, replay, and UI streaming for free ([ai-system-design-guide summary](https://github.com/ombharatiya/ai-system-design-guide/blob/main/17-tool-use-and-computer-agents/01-tool-use-landscape.md)).
- Per-session Docker sandbox with full OS capabilities; microVM isolation trend in 2026 deployments.
- Microagents (scoped instruction packages), repo-level `.openhands/microagents` instructions, memory condenser for long contexts, browsing capability, evaluation harnesses (SWE-bench, Terminal-Bench) built in.
- Agent Canvas: automations on schedules/webhooks (Slack, GitHub, Linear), multi-backend Agent Servers behind one frontend, ACP (Agent-Client Protocol) as the interop layer ([README](https://github.com/All-Hands-AI/OpenHands)).
- Benchmarks: OpenHands + CodeAct scaffold + Claude Opus hit ~68.4% SWE-bench Verified (2026), top of the open-source tier ([ssojet roundup](https://ssojet.com/blog/open-source-ai-coding-agents)).

### Architecture
- Python SDK core (`software-agent-sdk`: agents, tools, conversations, workspaces, events, Agent Server REST API) + TypeScript client + separate automation service (schedules/webhooks) — explicitly multi-repo with documented ownership boundaries ([README architecture](https://github.com/All-Hands-AI/OpenHands)).

### Strengths
- Transparency and hackability — the reference implementation for studying agent design; sandbox-first security; benchmark culture keeps quality claims honest; model-agnostic; ACP positions it as the "run any agent" substrate.

### Weaknesses
- Heavyweight to self-host (Docker-in-Docker, VM management); more research platform than polished product — UX roughness vs commercial peers; resource-hungry per session; event-stream verbosity costs tokens.

### Distinctive ideas worth borrowing
1. **CodeAct: code as the action space** — one sandbox-execution tool replaces dozens of bespoke tools; the LLM composes primitives in code (loops, conditionals over actions) instead of chained tool calls.
2. **Event stream as the single source of truth** — Actions and Observations as an append-only typed log: replay, resume, audit, multi-UI (web/CLI/API) all become trivial.
3. **Sandbox per session, always** — full-OS isolation is the default, not a flag.
4. **Agent-Client Protocol interop** — a control plane that can drive *other vendors'* agents; the UI/orchestration layer and the agent are separable products.
5. **Ship the eval harness** — built-in benchmarks make regressions measurable for contributors.

---

## Cross-cutting lessons for a new agent

**Copy:**
- **A ranked repo map over embeddings/RAG** (Aider): deterministic, cheap, inspectable context assembly. Combine with agentic search for the tail.
- **A capability dial, not a model picker** (Amp): users declare difficulty; the harness routes models per role (worker/reviewer/subagent) and can re-route as models change.
- **Plan/Act as a real state machine** (Cline): planning mode that *structurally cannot* edit, with an approval gate — plus Jules' timeout-auto-approve for async contexts.
- **A second-opinion tool** (Amp's Oracle, Jules' critic): a cheap-to-build "ask a bigger model to challenge this" step that catches wrong theories before they cost edits.
- **Sandbox-by-default** (OpenHands/Devin): isolation is the foundation that makes aggressive auto-execution (Amp-style) safe.
- **Git as the undo/audit layer** (Aider, Copilot's draft-PR-only, Cline checkpoints): never invent checkpointing where git already works.
- **PR/issue-native interfaces for async work** (Copilot, Jules, Devin): the PR is the deliverable and the review thread is the steering wheel — with governance defaults (requester can't approve, egress allowlist).
- **Lazy tool loading** (Amp skills with bundled MCP): tools enter context only when relevant; big MCP surfaces otherwise poison the context window.

**Avoid:**
- **Silent token burn** (Cline's #1 complaint): uncontrolled loops and unbounded context assembly destroy trust; make spend visible per turn.
- **Fork churn** (Roo Code's fate): plugin/mode ecosystems fragment if the core doesn't absorb good ideas fast.
- **Autonomy hype** (Devin's 2024 credibility damage): scope claims to "verifiable, well-tested, low-to-medium complexity" — Copilot's honest framing wins enterprise trust.
- **Per-step approval fatigue**: if you gate every action, users flip to full-auto anyway; invest instead in sandboxing + easy rewind so full-auto is safe.
- **Deleting workflows users depend on** (Amp's churn is its biggest user gripe): frontier-chasing is a feature only if migration paths exist.

**The single most important meta-lesson:** the scaffold is the product. The same model scores 20+ points apart on SWE-bench under different harnesses ([cadence analysis](https://cadence.withremote.ai/blog/autonomous-coding-agents-2026), [aiengineeringfromscratch](https://aiengineeringfromscratch.com/lesson?path=phases%2F15-autonomous-systems%2F09-coding-agent-landscape)). Everything distinctive above — repo maps, edit formats, dials, critics, sandboxes, event streams — is harness engineering, not model capability. That's good news: a new agent can compete on scaffolding quality alone.
