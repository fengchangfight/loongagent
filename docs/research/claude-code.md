# Claude Code — Anthropic's terminal coding agent

One-line description: Anthropic's agentic coding tool — a terminal-first CLI (plus IDE, desktop, web, and CI surfaces) where a Claude model drives a gather-context → act → verify loop over your local filesystem and shell.

Researched: 2026-09-26

Primary sources: [official docs](https://code.claude.com/docs/en/overview), [Anthropic best-practices guide](https://www.anthropic.com/engineering/claude-code-best-practices), [Pragmatic Engineer interview with the founding team](https://newsletter.pragmaticengineer.com/p/how-claude-code-is-built).

## 1. What it is

- **Maker**: Anthropic. Started Sept 2024 as Boris Cherny's personal terminal prototype (initially just AppleScript + filesystem tools); dogfooded internally Nov 2024, went GA May 2025 ([Pragmatic Engineer](https://newsletter.pragmaticengineer.com/p/how-claude-code-is-built)). Reportedly >$500M annual run-rate revenue by late 2025 (same source).
- **Target users**: professional software engineers first, but adopted broadly by data scientists and non-engineers inside Anthropic ("they all have Claude Code running" — Cherny).
- **Surfaces** ([overview](https://code.claude.com/docs/en/overview)): Terminal CLI (flagship), VS Code / Cursor extension, JetBrains plugin, standalone Desktop app (macOS/Windows/Linux beta), web at claude.ai/code, iOS/Android via Claude app, Slack (`@Claude` mention → PR), GitHub Actions / GitLab CI, and Remote Control (web UI driving a local session). Sessions move between surfaces (`claude --teleport`, `/desktop` handoff).
- **Pricing/distribution**: closed-source; distributed via native installer, npm, Homebrew, WinGet. Auth via Claude subscription (Pro ~$20/mo, Max 5x ~$100/mo, Max 20x ~$200/mo, Team/Enterprise seats with 5-hour + weekly usage windows) or pay-per-token API key (Anthropic Console, Bedrock, Vertex/GCP Agent Platform, Azure Foundry) ([costs docs](https://code.claude.com/docs/en/costs), [pricing overview](https://www.morphllm.com/claude-code-pricing)). Enterprise average ≈ $13/dev/active-day, $150–250/dev/month ([costs docs](https://code.claude.com/docs/en/costs)). Third-party providers supported on CLI/VS Code/JetBrains.

## 2. Core design philosophy

- **"Feel the model as raw as possible."** Boris Cherny: "We try to put as little code as possible around the model… Every time there's a new model release, we delete a bunch of code. With the 4.0 models, we deleted around half the system prompt." Claude Code is deliberately "a lightweight shell on top of the Claude model" — define UI hooks, expose tools, then get out of the way ([Pragmatic Engineer](https://newsletter.pragmaticengineer.com/p/how-claude-code-is-built)).
- **Simplicity over infrastructure**: "With every design decision, we almost always pick the simplest possible option." Hence local execution with no VM/Docker virtualization ([same source](https://newsletter.pragmaticengineer.com/p/how-claude-code-is-built)).
- **Exploit "product overhang"**: the founding insight was that the model could already explore filesystems and chain reads through imports — the product just needed to expose tools and not hobble it ([same source](https://newsletter.pragmaticengineer.com/p/how-claude-code-is-built)).
- **The context window is the scarcest resource**: "Claude's context window fills up fast, and performance degrades as it fills. The context window is the most important resource to manage" ([best practices](https://www.anthropic.com/engineering/claude-code-best-practices)). Nearly every feature (subagents, skills, compaction, MCP tool deferral) is downstream of this.
- **Unix philosophy / composability**: pipe in, pipe out; `tail -200 app.log | claude -p "…"` is a first-class usage ([overview](https://code.claude.com/docs/en/overview)).
- **Human-in-the-loop by default, with graduated trust**: permissions are "our most important principle… it shouldn't change things on your system without permission" (Cherny), but with explicit escape hatches (allowlists, accept-edits, sandbox, auto mode, bypass).
- **Verification closes the loop**: "Give Claude a check it can run… It's the difference between a session you watch and one you walk away from" ([best practices](https://www.anthropic.com/engineering/claude-code-best-practices)).

## 3. Feature inventory

### Agent loop
- Three blending phases: **gather context → take action → verify results**; the model chains dozens of tool calls, course-correcting per step; user can interrupt/steer any time (Esc, or queue a message mid-turn) ([How Claude Code works](https://code.claude.com/docs/en/how-claude-code-works)).
- The harness ("agentic harness") is the layer around the model providing tools + context assembly; the same loop runs on every surface and in the Agent SDK.
- Built-in tools: Read, Write, Edit, Bash, Glob, Grep, WebFetch, WebSearch, plus orchestration tools: Agent/Task (spawn subagents), AskUserQuestion, Monitor (watch background script output as events), todo tracking ([Agent SDK tool table](https://code.claude.com/docs/en/sdk)). Code-intelligence tools (go-to-def, type errors after edits) come via LSP-based plugins.

### Context management
- **Auto-compaction**: near the context limit, Claude Code first clears old tool outputs, then summarizes the conversation; root CLAUDE.md is re-injected after compaction; configurable via `/compact <focus>` and a "Compact Instructions" CLAUDE.md section ([best practices](https://www.anthropic.com/engineering/claude-code-best-practices), [memory docs](https://code.claude.com/docs/en/memory)). Thrash protection: stops auto-compacting with an error if context refills immediately.
- **Partial summarization**: `/rewind` (or Esc Esc) → "Summarize from here" / "Summarize up to here".
- **`/btw`**: ask a side question whose answer never enters conversation history.
- **`/context`**: visualize what occupies the window; **prompt cache stats** exposed in `/usage` (hit rate, misses, warm/cold TTL) ([costs docs](https://code.claude.com/docs/en/costs)).
- **MCP tool deferral**: only tool names + server instructions load at startup; full schemas load on demand via tool search.
- 1M-token context window on Opus 4.6+/Sonnet 4.6 models ([comparison source](https://ongboit.com/claude-code-context-compaction/)).

### Memory & rules
- **CLAUDE.md** hierarchy: managed (org-wide) → user (`~/.claude/CLAUDE.md`) → project (`./CLAUDE.md` or `./.claude/CLAUDE.md`) → local (`CLAUDE.local.md`, gitignored); ancestor directories loaded at launch, subdirectory files lazy-loaded on file access; all concatenated, most-specific last ([memory docs](https://code.claude.com/docs/en/memory)).
- **`@path` imports** in CLAUDE.md (max depth 4, external imports gated by approval dialog); HTML comments stripped before injection (free maintainer notes).
- **`.claude/rules/*.md`**: modular instruction files with YAML `paths:` glob frontmatter for path-scoped lazy loading; symlink support; user-level `~/.claude/rules/`.
- **AGENTS.md compatibility**: read directly (v2.1.277+), configurable precedence via `/config` → "Project instructions" (`claude-md-or-agents-md` default).
- **Auto memory**: Claude self-writes typed notes (`user`/`feedback`/`project`/`reference`) to `~/.claude/projects/<project>/memory/`; `MEMORY.md` index (first 200 lines/25KB loaded each session), topic files read on demand; per-repo, shared across worktrees, machine-local.
- `/init` generates a starter CLAUDE.md; `/doctor` proposes trimming derivable content.

### Permissions & sandboxing
- **Permission modes** (Shift+Tab to cycle): **Auto** (separate classifier model reviews each action in background, blocks scope escalation/unknown infra/hostile-content actions; GA ~March 2026, default on paid plans, [best practices](https://www.anthropic.com/engineering/claude-code-best-practices)), **Manual** (prompt before edits/commands), **Accept edits** (auto-approves file edits + common fs commands), **Plan** (read-only exploration + plan proposal). Plus `--dangerously-skip-permissions` bypass.
- **Rule system**: `permissions.allow/ask/deny` in settings.json; "don't ask again" saves durable per-repo rules to `.claude/settings.local.json`; static analysis of Bash commands against rules (Cherny interview). Tab on a prompt attaches a comment to an approve/deny decision, fed back to the model.
- **OS-level sandbox** for Bash/PowerShell: macOS Seatbelt, Linux/WSL2 Bubblewrap (+ optional seccomp); filesystem write-scoping and per-domain network approval; sandboxed commands can auto-allow; subagents inherit the parent sandbox in-process ([sandboxing docs](https://code.claude.com/docs/en/sandboxing)).
- Managed/enterprise settings: org-wide deny rules, `forceLoginMethod`, managed CLAUDE.md, server-managed settings.

### Extensibility
- **Hooks**: shell commands, HTTP endpoints, MCP tool calls, LLM prompts, or subagents fired at lifecycle events — SessionStart/End, UserPromptSubmit, PreToolUse/PostToolUse(+Failure), PostToolBatch, PermissionRequest/Denied, SubagentStart/Stop, PreCompact/PostCompact, CwdChanged, FileChanged, etc. Exit code 2 blocks and feeds stderr back to the model; hooks are the *deterministic* enforcement layer where CLAUDE.md is advisory ([hooks reference](https://code.claude.com/docs/en/hooks)).
- **Skills** (Agent Skills open standard): `.claude/skills/<name>/SKILL.md` with frontmatter; only the description sits in context, body loads on invocation or model-detected relevance; can bundle supporting files/scripts; `disable-model-invocation` for manual-only; **custom slash commands were merged into skills** (`.claude/commands/*.md` still work). Bundled skills: `/doctor`, `/code-review`, `/batch` (fan-out 5–30 worktree subagents each opening a PR), `/debug`, `/loop`, `/verify`, `/run` ([skills docs](https://code.claude.com/docs/en/skills)).
- **Plugins**: bundle skills + hooks + subagents + MCP servers; installable from Git-based marketplaces via `/plugin`.
- **MCP**: stdio / streamable-HTTP servers; scopes user/project(`.mcp.json`)/local; managed allow/deny lists for orgs; MCP prompts surface as `/mcp__server__prompt` commands; servers can push messages into a session as channels ([MCP docs](https://code.claude.com/docs/en/mcp)).
- **Output styles**, status line scripts, spinner verbs — broad settings surface.

### Subagents
- Defined in `.claude/agents/*.md` with own system prompt, tool restrictions, model choice, optional own auto-memory; run in **separate context windows**, return only summaries to the main loop ([subagents docs](https://code.claude.com/docs/en/sub-agents)).
- Built-ins: **Explore** (read-only, skips CLAUDE.md/git status for speed), **Plan**, general-purpose. Description token budget: startup warning past 15k tokens of descriptions.
- Beyond subagents: background agents, cross-session messaging, experimental **agent teams** (multi-session coordination with a team lead; ~7x token cost), dynamic workflows.

### Planning UX
- **Plan mode**: read-only phase, `Shift+Tab` cycle or `claude --permission-mode plan`; `Ctrl+G` opens plan in your editor; approval gates implementation. Caveat: under bypass permissions the blocks degrade to instructions ([PolySkill](https://polyskill.ai/blog/claude-code-plan-mode)).
- Recommended workflow: Explore → Plan → Implement → Commit ([best practices](https://www.anthropic.com/engineering/claude-code-best-practices)).
- **Checkpoints**: file snapshots before every edit; `/rewind` restores conversation/code/both; survives restarts; doesn't cover Bash side effects.
- `/goal`: persistent completion condition re-checked by an evaluator after every turn; Stop hooks gate turn end (8-block circuit breaker).

### Model support
- Claude-only by default: Sonnet for most work, Opus for hard reasoning, Haiku for cheap subagents; `/model` switching mid-session; effort levels for adaptive reasoning; Opus 5.5 / Fable models always-think. Third-party providers: Bedrock, Vertex/GCP Agent Platform, Azure Foundry, Claude Platform on AWS ([SDK docs](https://code.claude.com/docs/en/sdk)).

### Headless / SDK / automation
- **Headless**: `claude -p "…"` with `--output-format text|json|stream-json`, `--max-turns`, `--allowedTools`, `--append-system-prompt`; sessions resumable unless `--no-session-persistence`.
- **Agent SDK** (Python + TypeScript): same tools, agent loop, hooks, subagents, MCP, permissions, session resume/fork, programmatically; TS bundles the native binary. Managed Agents = hosted REST API counterpart (stateless harness + append-only event log + sandbox decomposition) ([SDK docs](https://code.claude.com/docs/en/sdk), [Managed Agents writeup](https://rits.shanghai.nyu.edu/ai/anthropic-launches-claude-managed-agents-for-scalable-ai-deployment/)).
- **GitHub Actions**: `anthropics/claude-code-action@v1`; `@claude` mentions in issues/PRs (interactive mode) or `prompt:` input on any event incl. cron (automation mode); auth via API key, OAuth token, or OIDC workload-identity federation; `/install-github-app` bootstraps everything; separate Code Review product auto-reviews every PR with an agent team ([GitHub Actions docs](https://code.claude.com/docs/en/github-actions), [Code Review coverage](https://gadgetsnow.indiatimes.com/apps/anthropic-launches-claude-code-review-how-it-works-cost-and-other-things-to-know/articleshow/129384012.cms)).
- **Scheduling**: cloud **Routines**, Desktop scheduled tasks, in-session `/loop`, `/schedule`.

### Collaboration/sharing
- Sessions stored as plaintext JSONL in `~/.claude/projects/`; resume/fork/name sessions; team-shared settings + CLAUDE.md via git; plugin marketplaces; Slack integration; cross-surface teleport.

## 4. Architecture

- **Language/stack**: TypeScript, React via Ink (terminal UI), Yoga layout, Bun for build/packaging; historically distributed on npm, now also a bundled native binary ([Pragmatic Engineer](https://newsletter.pragmaticengineer.com/p/how-claude-code-is-built), [single-binary repo](https://github.com/winfunc/claude-code-single-binary)). Stack deliberately "on distribution" — chosen because Claude is good at TS/React so Claude Code can (and does, ~90% per the team) write itself.
- **Process model**: single local process owning the agent loop; subagents run in the same process with separate context and inherited sandbox; tools execute locally with no virtualization by default; optional OS sandbox (Seatbelt/Bubblewrap) wraps shell execution. A March 2026 source-map incident exposed the full TS source ("57K lines, 0 tests" per one account — treat as community claim) ([DEV](https://dev.to/kolkov/we-reverse-engineered-12-versions-of-claude-code-then-it-leaked-its-own-source-code-pij), [LinkedIn](https://www.linkedin.com/posts/lakin-mohapatra_this-claude-code-incident-isnt-just-a-leak-activity-7444782638156120064-z_T2)).
- **Context assembly** (from docs + reverse engineering): system prompt → CLAUDE.md/rules/memory files delivered as user message after the system prompt (not system-level, hence advisory) → skill/subagent descriptions (bodies deferred) → MCP tool names (schemas deferred) → conversation incl. tool results; git status snapshot at start. Auto-compaction = model-written summary that replaces history, with root CLAUDE.md re-injected after.
- **Local vs cloud**: identical harness; local (default), Anthropic-managed cloud VMs, or Remote Control (local execution, web UI). GitHub repo `anthropics/claude-code` is primarily an issue tracker + plugin marketplace, not full source.
- **Cadence**: extremely fast release train — ~60–100 internal releases/day, ~1 external/day; 30+ releases in 5 weeks in early 2026 ([Pragmatic Engineer](https://newsletter.pragmaticengineer.com/p/how-claude-code-is-built), [apiyi changelog analysis](https://help.apiyi.com/en/claude-code-changelog-2026-april-updates-en.html)).

## 5. Strengths (community-cited)

- **Best-in-class output quality on complex work**: "code that needs fewer follow-up corrections"; ~80.9% SWE-bench and 67% blind-eval win rate cited in comparisons ([totalum](https://www.totalum.app/blog/gemini-cli-vs-claude-code-2026), [wetheflywheel](https://wetheflywheel.com/en/comparisons/codex-vs-claude-code-vs-gemini-cli/)).
- **The unglamorous durability features**: "Plan mode… permissions and diff review keep a human in the loop by design. Hooks and slash commands let a team script behavior, and CLAUDE.md encodes project conventions" — strengths "that show up on day forty rather than day one" ([xcloud.host](https://xcloud.host/deepseek-harness-vs-claude-code/)).
- **Richest ecosystem**: MCP connectors, plugin marketplaces, Agent Skills standard, GitHub Action, SDK — "the richest agent ecosystem of any terminal tool" ([wetheflywheel](https://wetheflywheel.com/en/comparisons/codex-vs-claude-code-vs-gemini-cli/)).
- **Local-first, zero-setup feel**: "Adding file tools gave Claude access to your local environment. No setup, no sync, just code that feels native" ([MLOps Community](https://mlopscommunity.substack.com/p/inside-claude-code-how-anthropic)).
- **Meaningful org-level throughput**: Anthropic reports +67% PR throughput while doubling engineering headcount, attributed to Claude Code ([Pragmatic Engineer](https://newsletter.pragmaticengineer.com/p/how-claude-code-is-built)).
- **Subagents for context isolation** widely praised as the feature that makes long tasks tractable ([Squid Club](https://www.squid-club.com/blog/sub-agents-in-claude-code-why-you-should-try-them-now)).

## 6. Weaknesses (community-cited)

- **Usage limits & cost opacity**: recurring HN/Reddit complaint of hitting limits "way faster than before"; one user reverse-engineered a cache-invalidation behavior inflating token burn; 1M-context rollout suspected of wasting tokens via weak compression ([HN thread](https://news.ycombinator.com/item?id=47586176), [Reddit](https://www.reddit.com/r/ClaudeAI/comments/1s3bcit/your_claude_code_limits_didnt_shrink_i_think_the/)). Third-party proxies exist just to defend against cache-economics regressions ([claude-code-cache-fix](https://github.com/cnighswonger/claude-code-cache-fix)).
- **Compaction degrades long sessions**: summarized context loses detail; unattended overnight runs hit resume dialogs and drift; "context rot" well before the hard limit ([amux guide](https://amux.io/guides/claude-code-context-compaction/), [LinkedIn](https://www.linkedin.com/posts/arushee-garg_claudecode-contextwindowcompaction-humaninloop-activity-7439450706727636992-B_hS), [agentnative pattern doc](https://www.agentnative.dev/patterns/context-compaction-pattern-for-long-running-agents)).
- **Instruction adherence at scale**: CLAUDE.md is advisory, not enforced — GitHub issues document Claude repeatedly editing forbidden files despite 30 pages of rules, and removing error handling against explicit instructions (cited in [arXiv 2604.13536](https://arxiv.org/pdf/2604.13536)).
- **Churn/regressions from the release cadence**: daily releases cause "silent regressions"; advice in community is to triangulate via the issue tracker within 48h of a release because "the changelog is not authoritative" ([hashnode](https://yurukusa.hashnode.dev/silent-regressions-in-claude-code-late-april-through-early-may-2026)).
- **Security surface**: repo-file attacks via `.claude/settings.json` hooks (GHSA-ph6w-f82w-28w6, CVSS 8.7), pre-trust API-key exfiltration (CVE-2026-21852), prompt injection via MCP content ([CSA research note](https://labs.cloudsecurityalliance.org/wp-content/uploads/2026/04/CSA_research_note_ai-coding-assistant-attack-surface_20260403-csa-styled.pdf), [SG gov advisory PDF](https://isomer-user-content.by.gov.sg/36/245e0e13-a722-4f81-a24c-9593e6eac651/28_Jan_2026.pdf)).
- **Resource footprint & closed source**: "TypeScript/Node.js (Bun) implementation has well-documented, systemic resource management problems" ([GitHub issue #22340](https://github.com/anthropics/claude-code/issues/22340)); no real source access despite an issue-tracker repo.
- **Plan mode leaks under bypass permissions** ([PolySkill](https://polyskill.ai/blog/claude-code-plan-mode)); Windows sandbox requires WSL2 ([sandboxing docs](https://code.claude.com/docs/en/sandboxing)).

## 7. Distinctive ideas worth borrowing

1. **Minimal harness, maximal model.** Ship the smallest shell — tools, UI hooks, context plumbing — and *delete* scaffolding with each model generation (Anthropic halved the system prompt for 4.0). Design assumption: capability lives in the model; your job is to not obstruct it. Reimplementable as: a policy that every hardcoded heuristic must justify itself against the current model, with a recurring "deletion pass."
2. **Progressive disclosure of context.** Everything enters the window lazily and at the cheapest granularity first: CLAUDE.md eagerly but subdirectory copies lazily; skill *descriptions* eagerly, bodies on invoke; MCP tool *names* eagerly, schemas via tool-search; subagent work happens entirely in a foreign context and only a summary returns. One principle ("pay for context only when used") applied at five different layers.
3. **Two memory systems with different authors.** Human-written CLAUDE.md (instructions, checked into git, team-shared) vs model-written auto memory (typed notes `user`/`feedback`/`project`/`reference`, per-repo, with a bounded 200-line/25KB index loaded every session and detail files read on demand). The bounded-index-plus-topic-files pattern is a clean solution to "memory grows unboundedly."
4. **Deterministic hooks as the enforcement layer.** CLAUDE.md is explicitly advisory; anything that must always happen (lint after edit, block writes to migrations/, log which rules loaded) becomes a hook — a shell command fired at a lifecycle event whose exit code 2 blocks the action *and feeds the reason back to the model as an error*. This cleanly separates "guidance" from "guarantee."
5. **Graduated trust ladder.** Manual prompts → per-command "don't ask again" rules persisted to a local settings file → accept-edits mode → OS-level sandbox (Seatbelt/Bubblewrap) that makes auto-allow safe → a classifier model ("auto mode") that judges each action → full bypass. Each rung trades review effort for speed; sandboxing is what makes the top rungs defensible.
6. **Checkpoints as a first-class undo.** Snapshot every file before each agent edit; a rewind menu restores conversation, code, or both to any turn boundary, and survives process restarts. This converts "letting the agent try something risky" from a git discipline into a UX primitive.
7. **The agent interviews the user.** `AskUserQuestion` as a tool lets the model drive structured clarification (multiple-choice), and the recommended large-feature workflow is: minimal prompt → Claude interviews you → writes SPEC.md → *fresh session* implements it. Spec-then-fresh-execution is a deliberate context-hygiene pattern.

## 8. Lessons for a new agent

- **Copy: the context-economy mindset.** Treat the context window as the primary resource; build lazy-loading (skills/descriptions, deferred tool schemas), context isolation (subagents returning summaries), `/context`-style visibility, and cache-hit telemetry from day one. Anthropic's docs show users needed all of these.
- **Copy: verification-as-loop-closure.** The single highest-leverage prompt pattern is "here's a check; iterate until it passes." Build first-class support: goal conditions re-evaluated per turn, Stop-hook gates with a circuit breaker, and "show evidence, not assertions."
- **Copy: separate advisory guidance from hard enforcement.** Markdown rules drift and get ignored at length; hooks (or equivalent deterministic interceptors) are the only reliable guarantee. Design both layers and document which is which.
- **Copy: sessions as files.** Plaintext JSONL transcripts enable resume, fork, rewind, `/insights`-style self-analysis, and external tooling for free.
- **Copy: permissions as a ladder, not a switch** — and note that Anthropic eventually needed an ML classifier because manual approval doesn't scale ("after the tenth approval you're clicking through rather than reviewing").
- **Avoid: release-train regressions.** Daily external releases eroded trust; silent cache-economics regressions cost users real money. Ship with behavioral regression tests and treat token-billing-impacting changes as breaking changes.
- **Avoid: repo-controlled execution without trust gates.** Hooks/settings auto-loading from a cloned repo produced multiple CVEs. Untrusted-repo configuration must never execute or phone home before explicit trust.
- **Do differently: plan for adversarial content from the start** (prompt injection via MCP/tool output is the dominant attack class), and consider a lighter-weight runtime — the TS/Bun footprint is a recurring community complaint that a native/Rust/Go harness could exploit.
- **Do differently: make compaction lossless where possible.** Compaction quality is Claude Code's most persistent long-session complaint; alternatives worth exploring: structured scratchpads (plans/decisions files the agent maintains), hierarchical summaries, or resumable summaries offered proactively instead of reactively.
