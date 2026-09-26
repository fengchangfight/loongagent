# OpenCode (opencode)

Open-source, model-agnostic AI coding agent for the terminal, built by Anomaly (formerly SST), with a client/server architecture and 75+ LLM providers.

Researched: 2026-09-26

---

## 1. What it is

- **Maker:** Anomaly (formerly SST / Serverless Stack), co-founded by Dax Raad (thdxr) and Jay V (of SST and terminal.shop fame). Repo: [github.com/anomalyco/opencode](https://github.com/anomalyco/opencode) (renamed from `sst/opencode`; GitHub API redirects).
- **Scale (2026-09-26, GitHub API):** ~210k stars, ~27.8k forks, ~6.2k open issues, MIT license, TypeScript. Dax claimed ~7.5M monthly active developers and "largest open-source coding agent" on [Syntax.fm, July 2026](https://www.wilsonhuang.xyz/blog/1021-we-got-addicted-to-an-ai-model-we-cant-talk-about).
- **Target users:** terminal-first developers (vim/tmux crowd), people who want provider choice (including local/self-hosted models), teams wanting self-hostable infrastructure.
- **Surfaces** ([docs](https://opencode.ai/docs/), [README](https://github.com/sst/opencode)):
  - **TUI** — the primary interface (`opencode`).
  - **Desktop app (beta)** — macOS/Windows/Linux builds ([opencode.ai/download](https://opencode.ai/download)).
  - **IDE extension** — VS Code/Cursor/Windsurf/VSCodium; auto-installs when you run `opencode` in the integrated terminal; adds keybinds, selection context sharing, `@File#L37-42` references ([docs/ide](https://opencode.ai/docs/ide/)).
  - **Headless CLI** — `opencode run "prompt"`, `--format json` event stream, `--continue`/`-s` session continuation for CI ([developersdigest](https://www.developersdigest.tech/blog/headless-ai-coding-agents-ci-comparison-2026)).
  - **HTTP server** — `opencode serve`, full OpenAPI 3.1 API + SSE events ([docs/server](https://opencode.ai/docs/server/)).
  - **GitHub Action** — `/opencode` or `/oc` mention in issues/PRs runs the agent in your Actions runner ([docs/github](https://opencode.ai/docs/github/)).
  - **Web** — `opencode web` (part of the server surface; OpenCode 2.0 direction is an always-on service synced across desktop/web/mobile, per [Dax on Syntax.fm](https://www.wilsonhuang.xyz/blog/1021-we-got-addicted-to-an-ai-model-we-cant-talk-about)).
- **Pricing/distribution:** the agent itself is free and MIT-licensed, distributed via npm/Bun/Homebrew/pacman/scoop/choco/mise/nix/Docker/curl installer. Revenue comes from **OpenCode Zen** — a curated, pay-as-you-go AI gateway "sold at cost" with benchmarked model+provider combos ([docs/zen](https://opencode.ai/docs/zen/)) — plus free team-workspace management (beta) and a ~$10/month hosted open-weights subscription ("OpenCode Go") reported by users on [HN](https://brianlovin.com/hn/47460525).
- **Lineage trivia:** the *original* opencode was a Go + Bubble Tea project; its author joined Charm, where that lineage became [Crush](https://github.com/charmbracelet/crush/issues/1097) after a naming controversy in mid-2025, and `sst/opencode` continued as the fork/rewrite ([HN](https://news.ycombinator.com/item?id=44736176), [arcbjorn](https://blog.arcbjorn.com/state-of-cli-coding-agents-2026)).

## 2. Core design philosophy

- **Model-agnostic, no lock-in.** The model is a swappable backend, not the product. From the [Zen docs](https://opencode.ai/docs/zen/): *"Have no lock-in by allowing you to use it with any other coding agent. And always let you use any other provider with OpenCode as well."* Zen's stated goals also include benchmarking real model/provider combinations and never silently downgrading or routing to cheaper providers.
- **Terminal-native, TUI-first.** Self-described as "The AI coding agent built for the terminal" ([README](https://github.com/sst/opencode)). They built their own TUI framework (OpenTUI) rather than compromise on terminal UX.
- **Open source as positioning, not just license.** Dax Raad: their edge comes from "positioning — open-source, model-agnostic, first mover — not product alone" ([AI Shepherd summary of a Dax interview](https://aishepherd.nl/tldr/tldr-building-the-future-of-coding-opencode-with-dax-raad/)).
- **Engineering taste over hype.** The team "is not breathlessly talking about how coding is dead. They have pretty sane takes on AI coding including trying to help people who care about code quality" ([HN: softwaredoug](https://brianlovin.com/hn/47460525)). Dax argues AI "raises the floor for every developer but doesn't erase the gap" and warns agents erode engineers' instinct for good code ([Pragmatic Engineer interview](https://newsletter.pragmaticengineer.com/p/opencode)).
- **Contrast with Claude Code:** Claude Code is a single-vendor, tightly-tuned proprietary product; OpenCode bets the harness should be open, inspectable, and provider-independent so users can route planning to a strong model and bulk edits to a cheap one ([deepinfra comparison](https://deepinfra.com/blog/claude-code-alternative)). Anthropic blocking Claude Max subscriptions from OpenCode in Jan 2026 ([byteiota](https://byteiota.com/anthropic-blocks-claude-max-in-opencode-devs-cancel-200-month-plans/)) was turned into a growth engine — Dax says they "fought for a long time and didn't win" ([Syntax.fm summary](https://www.wilsonhuang.xyz/blog/1021-we-got-addicted-to-an-ai-model-we-cant-talk-about)).

## 3. Feature inventory

### Agent loop & modes
- Two built-in **primary agents**, cycled with `Tab` ([docs/agents](https://opencode.ai/docs/agents/)):
  - **build** — full access, default.
  - **plan** — read-only planning/analysis; implemented purely as a *permission* configuration (edits and bash default to "ask"/deny), not a separate agent implementation.
- Built-in **subagents**: `general` (multi-step work, full tools), `explore` (fast read-only codebase search), `scout` (read-only external docs/dependency research, clones repos into a managed cache).
- Hidden **system agents**: `compaction`, `title`, `summary` — run automatically (context compaction, session titles, session summaries).
- Subagents create **child sessions** navigable with keybinds (parent/child/cycle); invoked automatically by the primary agent based on descriptions, or manually via `@mention`.
- `max steps` config caps agentic iterations; on hitting it the agent is prompted to summarize remaining work.

### Tools the agent has
From [docs/permissions](https://opencode.ai/docs/permissions/) and [docs/agents](https://opencode.ai/docs/agents/):
- `read`, `write`, `edit`, `apply_patch`, `glob`, `grep`, `list`, `bash`, `task` (spawn subagents), `todowrite`/`todoread`, `webfetch`, `websearch`, `lsp`, `skill`, `question` (ask the user mid-run).
- **doom_loop** safety guard: triggers when the same tool call repeats 3 times with identical input (a "stuck agent" detector wired into the permission system).
- Todo list per session, exposed via API (`GET /session/:id/todo`).

### Context management
- **AGENTS.md** as the canonical rules file (project root + global `~/.config/opencode/AGENTS.md`); `/init` analyzes the repo and generates/improves it ([docs/rules](https://opencode.ai/docs/rules/)).
- **Claude Code compatibility fallback:** reads `CLAUDE.md`, `~/.claude/CLAUDE.md`, and `~/.claude/skills/` if no AGENTS.md exists (disable via `OPENCODE_DISABLE_CLAUDE_CODE*` env vars).
- `instructions` config array: extra rule files, globs, **and remote URLs** (fetched with 5s timeout).
- **Compaction:** automatic when context fills; manual via `/compact` (alias `/summarize`); the compaction prompt itself is replaceable via plugin hook (`experimental.session.compacting`).
- **Undo/redo:** `/undo` reverts the last message *and its file changes*, tracked via internal Git snapshots (requires a git repo); `/redo` restores. Server API also has `fork`, `revert`, `unrevert`, and per-session `diff` endpoints ([docs/server](https://opencode.ai/docs/server/)).
- `@file` fuzzy references in the prompt; `!command` runs a shell command and injects output into the conversation; image drag-and-drop into the terminal.

### Permissions / sandboxing
- Three actions per rule: `allow` / `ask` / `deny`; glob patterns with **last-match-wins** semantics; per-agent overrides merged over global config ([docs/permissions](https://opencode.ai/docs/permissions/)).
- Granular per-bash-command rules (`"git *": "allow", "rm *": "deny"`), per-file-path edit rules, `external_directory` gates any path outside the project (supports `~/` expansion), task permission globs control which subagents an agent may spawn (denied subagents are removed from the Task tool description entirely).
- Defaults are permissive; `.env` files are denied to `read` by default; `doom_loop` and `external_directory` default to `ask`.
- `--auto` flag auto-approves anything not explicitly denied (also toggleable in the TUI command palette).
- Approval UX offers once / always (session-scoped pattern whitelist) / reject.
- **No OS-level sandboxing built in** — users run it in containers/VMs themselves; the server exposes the filesystem over REST, a recurring security complaint (see Weaknesses).

### Extensibility
- **Plugins** ([docs/plugins](https://opencode.ai/docs/plugins/)): JS/TS files in `.opencode/plugins/` or `~/.config/opencode/plugins/`, or npm packages listed in config (auto-installed with Bun, cached in `~/.cache/opencode/node_modules/`). Plugin context includes the project, an OpenCode SDK client, and Bun's `$` shell. Hooks: `tool.execute.before/after`, `event` (30+ event types: session, message, file, permission, LSP, TUI…), `shell.env`, `experimental.session.compacting`, and **custom tools** defined with Zod schemas via a `tool()` helper.
- **MCP** ([docs/mcp-servers](https://opencode.ai/docs/mcp-servers/)): local (stdio) and remote servers, automatic OAuth flow with dynamic client registration, tokens stored in `~/.local/share/opencode/mcp-auth.json`, per-agent tool gating with globs, runtime `POST /mcp` to add servers dynamically.
- **Custom commands** ([docs/commands](https://opencode.ai/docs/commands/)): markdown files in `.opencode/commands/` with frontmatter (description/agent/model/subtask); template supports `$ARGUMENTS`, positional `$1..$n`, `` !`shell` `` output injection, `@file` inclusion; `subtask: true` forces execution in a subagent to keep the main context clean. Custom commands override built-ins.
- **Custom agents**: JSON config or markdown files in `.opencode/agents/`; per-agent model, temperature, top_p, prompt file (`{file:./prompts/x.txt}`), permission set, color, hidden flag; `opencode agent create` interactive generator. Any extra config keys pass through to the provider (e.g. `reasoningEffort`).
- **Agent Skills** supported (incl. `~/.claude/skills` fallback).

### Planning UX
- Plan mode = Tab-cycled primary agent with edits denied; iterate on the plan conversationally (including pasting reference images), then Tab back to build and say "go ahead" ([docs intro](https://opencode.ai/docs/)).

### Model support
- 75+ providers via the **Vercel AI SDK** + **[Models.dev](https://models.dev)** registry (an SST-maintained open catalog of providers/models); local models (Ollama, LM Studio) supported ([docs/models](https://opencode.ai/docs/models/)).
- Custom providers via `npm: "@ai-sdk/openai-compatible"` adapter config.
- **Variants**: named per-model option presets (thinking budgets, reasoning effort) — built-in for Anthropic (`high`/`max`), OpenAI (`none`…`xhigh`), Google; user-definable; `variant_cycle` keybind switches at runtime.
- Per-agent model override enables cheap-model-for-plan / strong-model-for-build workflows.
- Model pick order: `--model` flag → config → last used → internal priority.

### Collaboration / sharing
- `/share` creates a public link (`opncd.ai/s/<id>`) by syncing the session to OpenCode's servers; modes `manual` (default) / `auto` / `disabled`; `/unshare` deletes the data; enterprise can self-host or restrict to SSO ([docs/share](https://opencode.ai/docs/share/)).
- GitHub integration: mention-triggered agent in Actions, PR review, issue triage, cron-scheduled runs, OIDC token exchange with the OpenCode GitHub App (or plain `GITHUB_TOKEN`) ([docs/github](https://opencode.ai/docs/github/)).
- `/export` dumps a conversation to Markdown in `$EDITOR`.

## 4. Architecture

- **Client/server split is the core architectural bet.** Running `opencode` starts a TUI *and* an HTTP server; "the TUI is the client that talks to the server." The server publishes an **OpenAPI 3.1 spec** at `/doc` from which the official **SDK is generated** ([docs/server](https://opencode.ai/docs/server/)).
- The API surface is complete: sessions (create/fork/abort/revert/summarize/share/diff/todo), messages (sync + `prompt_async`), commands, shell, files (`find`, `find/file`, `find/symbol`, content, status), config (GET/PATCH), providers (incl. OAuth authorize/callback), LSP/formatter/MCP status, agents, permissions responses, an SSE **event bus** (`/event`, `/global/event`), and **TUI control endpoints** (`POST /tui/append-prompt`, `submit-prompt`, control request/response) that let external programs drive a running TUI — this is how the IDE plugins work.
- `opencode serve` runs headless with optional **mDNS discovery** and basic auth (`OPENCODE_SERVER_PASSWORD`).
- **Language/runtime:** TypeScript on **Bun**, monorepo. Third-party analysis notes the server uses Hono and the monorepo uses Turborepo ([cnblogs analysis](https://www.cnblogs.com/yongchao/p/22851605) — secondary source, treat as approximate).
- **TUI stack:** **OpenTUI** ([github.com/sst/opentui](https://github.com/sst/opentui)) — their own TypeScript TUI library with a **Zig core bridged via Bun FFI** and a **SolidJS** renderer (JSX for the terminal). Built because Ink (React-for-CLI) was too slow; they sponsored an outside developer (kmdrfx) to build it ([stork.ai](https://www.stork.ai/blog/the-tui-library-thats-killing-ink), [daily.dev](https://daily.dev/posts/opentui-react-for-your-terminal-powered-by-bun-and-zig-dsyf9z0oe)). The v0.x Go + Bubble Tea implementation was abandoned largely for TUI performance reasons ([Grokipedia/OpenTUI](https://grokipedia.com/page/OpenTUI) — low-confidence source).
- **LSP:** 30+ built-in language server integrations, auto-installed per project, started lazily by file extension; **disabled by default** — the docs candidly note LSP "can get out of sync, use significant memory… In many projects it is better to have the agent run lint, typecheck, or other diagnostic CLI tools directly" ([docs/lsp](https://opencode.ai/docs/lsp/)).
- **Storage:** local-first; sessions/config/auth under `~/.local/share/opencode` and `~/.config/opencode`; sharing is the opt-in cloud sync.
- **Context assembly:** system prompt per agent + AGENTS.md/instruction files + message history with tool parts; compaction handled by a dedicated hidden agent whose prompt is plugin-replaceable. Notably leaner default prompt than Claude Code (~7k vs ~33k tokens before the user prompt, per a [benchmark discussed on HN](https://news.ycombinator.com/item?id=48883275)).
- **Direction (reported):** OpenCode 2.0 makes the server an always-on background service with sessions synced across desktop/web/mobile; agents can hop across the user's machines over Tailscale ([Dax on Syntax.fm, July 2026](https://www.wilsonhuang.xyz/blog/1021-we-got-addicted-to-an-ai-model-we-cant-talk-about) — reported, not yet fully shipped as of research date).

## 5. Strengths (community-sourced)

- **Provider freedom with real ergonomics.** Users run Claude, Gemini, and local llama.cpp models through one harness; "the LSP integration is great" ([HN: khimaros](https://brianlovin.com/hn/47460525)). Model routing per agent (cheap planner, strong builder) is a genuinely used feature ([deepinfra](https://deepinfra.com/blog/claude-code-alternative)).
- **Client/server architecture pays off:** third-party VS Code extensions (openchambers) and seamless switching between TUI/desktop/web "has hardly a usable alternative" ([HN: jFriedensreich](https://brianlovin.com/hn/47460525)).
- **Leaner token footprint than Claude Code** — ~7k vs ~33k tokens of harness overhead before the prompt ([HN #48883275](https://news.ycombinator.com/item?id=48883275), [systima benchmark](https://systima.ai/blog/claude-code-vs-opencode-token-overhead)).
- **Configurability:** "more configurable, for defining (sub)agent abilities, plugins, and different models/providers" ([HN: esafak](https://brianlovin.com/hn/47460525)).
- **Cost escape hatch:** open-weights models (GLM, Kimi, DeepSeek, MiniMax) via Zen are dramatically cheaper; users report GLM-class models covering "80%+ of my work" ([HN](https://brianlovin.com/hn/47460525)).
- **Fast-moving, transparent development** with sane public takes on AI coding ([HN: softwaredoug](https://brianlovin.com/hn/47460525)).
- **Plan-then-build with Tab** and `/undo` via git snapshots are frequently cited as the workflow that converts Claude Code users ([towardsai two-week trial](https://pub.towardsai.net/i-ran-opencode-instead-of-claude-code-for-two-weeks-it-was-78-slower-and-i-kept-it-anyway-9a84102cc982)).

## 6. Weaknesses (community-sourced)

- **Security posture.** The most repeated serious criticism: the server "will just serve your whole hard drive via REST endpoint and not constrain to project folders"; the web UI is served from OpenCode's servers; unknown API routes fall back to their servers, "potentially leaking data" ([HN: jFriedensreich](https://brianlovin.com/hn/47460525)). No built-in sandbox — safe use requires a container/VM ([same thread](https://brianlovin.com/hn/47460525)).
- **Privacy surprises.** Even with only a local model configured, prompts were sent to the cloud to generate session titles ([HN: heavyset_go](https://brianlovin.com/hn/47460525)); a stealth default model ("Big Pickle", believed to be a MiniMax build) routes data to a third party during free periods ([HN: adam_mckenna](https://brianlovin.com/hn/47460525); confirmed in the [Zen privacy docs](https://opencode.ai/docs/zen/) which state free stealth models *may* train on your data).
- **Slower wall-clock than Claude Code.** Builder.io benchmark: same task, 9m09s (Claude Code) vs 16m20s (OpenCode) — 78% slower, attributed partly to the LSP feedback loop and deliberation steps ([towardsai](https://pub.towardsai.net/i-ran-opencode-instead-of-claude-code-for-two-weeks-it-was-78-slower-and-i-kept-it-anyway-9a84102cc982), [AICoderScope](https://aicoderscope.com/blog/opencode-review-2026/)). Reddit users also call it token-inefficient when paying per API token ([r/opencodeCLI](https://www.reddit.com/r/opencodeCLI/comments/1rpomf5/opencode_vs_claude_code/)).
- **Quality is model/harness-dependent.** Failure mode is a "silent break at the model/harness seam… mid-run, until you notice" vs Claude Code's "changes under you on release" ([composio](https://composio.dev/content/claude-code-vs-open-code)). Non-frontier models produce noticeably worse tool-calling behavior.
- **Churn and reliability.** Very high release velocity with regressions; ~6.2k open issues; some HN commenters blame agent-assisted development for code quality ("thousand reinvented wheels strapped together by tape") ([HN: blks, logicprog](https://brianlovin.com/hn/47460525)). Example: agents hanging indefinitely with `serve`+`attach` when any permission is "ask" ([issue #16367](https://github.com/anomalyco/opencode/issues/16367)).
- **Vendor risk in reverse:** Anthropic cut off Claude Max subscription access (Jan 2026), showing a model-agnostic harness still depends on provider goodwill ([byteiota](https://byteiota.com/anthropic-blocks-claude-max-in-opencode-devs-cancel-200-month-plans/)).
- Windows support historically lagging; Bun-on-Windows install "in progress" per [docs](https://opencode.ai/docs/) (may be resolved — verify).

## 7. Distinctive ideas worth borrowing

1. **The TUI is just a client.** Run an HTTP+SSE server (OpenAPI-described, SDK-generated) as the real product; the terminal UI, desktop app, IDE extension, web app, and CI runner are interchangeable clients. Bonus: expose `/tui/*` endpoints so *other programs can drive the TUI itself* (prefill prompts, submit, wait on control requests) — this is how their IDE integration shares context without a separate agent runtime. Reimplement: define every agent operation as an RPC with an event stream; generate clients from the spec.
2. **Agents are just permission sets + prompts.** "Plan mode" isn't a second agent implementation — it's the build agent with `edit: deny`. Primary vs subagent is a `mode` flag; everything (model, temperature, prompt file, permissions, color) is a markdown file with frontmatter that users can create, version in git, and share. Denied subagents are *removed from the Task tool description* so the model never tries them.
3. **A model registry as shared infrastructure (Models.dev).** Instead of hardcoding providers, maintain an open catalog of provider/model metadata (context limits, costs, capabilities) as a separate project; the agent resolves `provider/model` IDs against it, and per-model "variants" (thinking budgets) are first-class named presets switchable at runtime. Keeps a 75-provider matrix out of the core codebase.
4. **Message-level undo via git snapshots.** Each message's file changes are snapshotted in git; `/undo` reverts files *and* rewinds the conversation, `/redo` restores; the API exposes fork/revert/diff per message. This makes experimentation safe without teaching the agent about version control.
5. **Plugin hooks on the tool boundary, not the model boundary.** `tool.execute.before` can rewrite or veto any tool call (the canonical example: throw if `read` targets `.env`); plugins can inject env into every shell, register entirely new tools (Zod-schema'd), and — unusually — **replace the compaction prompt** (`experimental.session.compacting`), letting teams control what survives context compression. npm plugins auto-install via Bun into a cache dir: zero-friction distribution.
6. **The business model is a curated gateway (Zen).** Benchmark model+provider *combinations* (the same model behaves differently across hosts), sell the verified ones at cost with zero markup beyond processing, explicitly allow BYOK and competing harnesses. Free "stealth models" give users no-cost usage in exchange for feedback/training data — disclosed in the docs. Funds OSS development while reinforcing the no-lock-in brand.
7. **Safety guards as permissions.** `doom_loop` (same tool call 3× with identical input) and `external_directory` (any path outside the worktree) are ordinary permission rules defaulting to `ask` — so "agent stuck" and "agent escaping the project" flow through the same ask/allow/deny UX as everything else, and can be tuned per project.

## 8. Lessons for a new agent

- **Build client/server from day one.** Every surface (TUI, IDE, CI, web) and the entire third-party ecosystem (custom UIs, SDK scripts) comes free from one OpenAPI-described server. OpenCode's multi-surface reach is directly attributable to this; monolith CLIs can't retrofit it cheaply.
- **Copy the config-as-markdown pattern for agents and commands.** Plain files in `.opencode/` and `~/.config/` that users diff, commit, and share beat UI-hidden settings; frontmatter + globs cover 95% of customization without code.
- **Make permissions the only sandbox-adjacent primitive, but go further than OpenCode.** Glob rules with last-match-wins and per-agent overrides are excellent; the lack of any real OS-level sandbox and the whole-filesystem REST exposure are their biggest trust problem. A new agent should ship namespace/container isolation by default, not as user homework.
- **Don't phone home by default.** Sending prompts to the cloud for session titles, stealth default models, and server-routed web UIs generated outsized distrust relative to their convenience value. Local-first must mean *fully* local unless the user opts in per-feature.
- **Token-lean harness is a differentiator.** The 7k-vs-33k overhead story did real marketing work. Small default toolsets, short system prompts, cache-friendly message assembly.
- **Model agnosticism is necessary but not sufficient.** Harness quality still couples to the model (silent mid-run breaks at the model/harness seam). Invest in per-model conformance testing of tool calling, and make the failure loud.
- **Match release velocity to reliability.** 6k+ open issues and regression complaints are the tax on shipping daily; users forgive rough edges but not broken sessions.
- **Plan mode as read-only-agent + Tab is the right planning UX** — simpler and more legible than a separate planning subsystem, and users clearly love it.
