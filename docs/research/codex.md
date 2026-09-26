# OpenAI Codex — CLI, IDE extension, and cloud agent

One-liner: OpenAI's coding-agent product family — an open-source Rust CLI/TUI, an IDE extension, a desktop app, and a parallel cloud task runner — unified by one ChatGPT account, one config stack, and one agent "harness".

Researched: 2026-09-26

Primary sources: [developers.openai.com/codex](https://developers.openai.com/codex), [github.com/openai/codex](https://github.com/openai/codex), [Codex changelog](https://developers.openai.com/codex/changelog), OpenAI launch posts.

## 1. What it is

- **Maker:** OpenAI. Not to be confused with the retired 2021 "Codex" API model; the current product launched as Codex CLI (April 2025) and the Codex cloud agent in ChatGPT (May 2025), unified into one product in Sept 2025 ([GPT-5-Codex post](https://openai.com/index/introducing-upgrades-to-codex/)).
- **Target users:** professional developers and teams; enterprise emphasis (managed config, OTEL audit, admin-enforced requirements, security scanning).
- **Surfaces:**
  - **Codex CLI** — open-source (Apache-2.0) terminal agent, Rust binary; install via `curl -fsSL https://chatgpt.com/codex/install.sh | sh`, `npm i -g @openai/codex`, Homebrew, or GitHub releases ([README](https://github.com/openai/codex)). Latest release 0.157.1 (2026-09-26).
  - **Codex IDE extension** — VS Code, Cursor, Windsurf and other forks; shares config and MCP setup with the CLI.
  - **Codex cloud** — parallel tasks in OpenAI-managed containers; start from web (chatgpt.com/codex), GitHub, GitLab (beta), Linear, Slack, CLI (`codex cloud`), or mobile.
  - **Codex app / ChatGPT desktop app** — macOS/Windows "command center" with multi-agent threads, worktrees, review pane, automations, voice.
  - **ChatGPT iOS app** — start/review tasks remotely.
  - **Agents API** (public beta, 2026-09-10): managed service exposing the Codex harness for building custom cloud agents ([OpenAI blog](https://openai.com/index/introducing-the-agents-api/)).
  - **Codex Security** — appsec scanning agent built on the same stack: plugin, `@openai/codex-security` CLI, TypeScript SDK, cloud scans ([docs](https://developers.openai.com/codex/security)).
- **Pricing/distribution:** bundled with ChatGPT Free/Go (limited), Plus $20, Pro $200, Business/Edu/Enterprise; usage is token/credit-based with per-plan limits; Business/Enterprise can buy shared credit pools. API-key usage billed per token. GPT-6 Sol/Luna roll out at lower token prices than GPT-5.6 predecessors ([changelog 2026-09-22](https://developers.openai.com/codex/changelog)).
- **Scale:** repo has ~126.5k stars, ~19k open issues, Rust as primary language (GitHub API, 2026-09-26).

## 2. Core design philosophy

- **A teammate, not an autocomplete.** "Codex moves closer to what we've been building toward all along — a teammate that understands your context, works alongside you, and reliably takes on work for your team" ([GPT-5-Codex post](https://openai.com/index/introducing-upgrades-to-codex/)).
- **Two converging modes: real-time pairing + async delegation.** "We imagine a future where developers drive the work they want to own and delegate the rest to agents… these two modes of interaction — real-time pairing and task delegation — [will converge]" ([Introducing Codex, May 2025](https://openai.com/index/introducing-codex/)). This drives the whole multi-surface design: same agent, same account, local and cloud.
- **Verifiability over trust.** "Users can check Codex's work through citations, terminal logs and test results"; tasks ship with evidence, and code review is a first-class, model-trained feature rather than an afterthought.
- **Safe by default, configurable by risk tolerance.** Sandbox on, network off by default, locally and in the cloud; "Developers can customize security settings to match their risk tolerance" ([GPT-5-Codex post](https://openai.com/index/introducing-upgrades-to-codex/)).
- **The harness is the product.** "Useful agents need a powerful harness that manages context, uses tools efficiently, and coordinates subagents… infrastructure that keeps them running reliably for days" ([Agents API post](https://openai.com/index/introducing-the-agents-api/)). OpenAI sells the loop around the model (context, tools, policy, approvals) as reusable infrastructure.
- **Models trained for the harness, not vice versa.** GPT-5-Codex was "purpose-built for Codex CLI, the Codex IDE extension, the Codex cloud environment, and working in GitHub" and OpenAI recommends using it *only* in Codex-like environments — tight model↔harness co-design.

## 3. Feature inventory

### Agent loop & tools
- Classic tool-calling loop; interactive TUI plus non-interactive `codex exec` for scripts/CI ([CLI docs](https://developers.openai.com/codex/cli)).
- Tools: `shell` (PTY-backed `unified_exec` by default except Windows), file edit via `apply_patch` (a dedicated patch tool, own crate), file search, web search (see below), image input (`codex --image` or paste), subagent spawn tools ("Multi-Agent V2"), code-mode (programmatic tool calling / JS-REPL-style execution; crates `code-mode*`, cf. Agents API "programmatic tool calling").
- Steering: interrupt mid-turn, queue prompts, answer inline questions without losing drafts, fork conversations, `codex resume`, session search.

### Context management
- **AGENTS.md** layered instructions (see below).
- **Compaction:** streamed "remote compaction" for supported providers; compaction checkpoints persist resume metadata; sessions resume "from the latest compaction boundary"; opt-in compaction after final responses (changelog 0.155–0.157).
- **Memories:** experimental feature flag `memories` (default off); "memory v2" with extraction/consolidation workers and isolated storage ([config docs](https://developers.openai.com/codex/config-basic), changelog).
- **Goals:** stable feature — persisted goals with automatic continuation across turns; blocked after 3 empty auto-continuation turns.
- Sessions persisted as JSONL "rollouts" + SQLite state; `codex resume`, `/import`, rollout compression endpoints.
- Tool-output truncation: head/tail kept, **middle dropped** — a known weakness (see §6); per-MCP-tool `output_token_limit` override exists ([MCP docs](https://developers.openai.com/codex/mcp)).

### Permissions & sandboxing
- Two orthogonal layers: **sandbox mode** (what's technically possible) × **approval policy** (when to ask) ([security docs](https://developers.openai.com/codex/sandbox)).
- Sandbox modes: `read-only`, `workspace-write` (default; writes limited to workspace + /tmp; network off), `danger-full-access` (alias `--yolo` / `--dangerously-bypass-approvals-and-sandbox`).
- Approval policies: `on-request`, `never`; legacy `untrusted` retired/migrated. Presets: `Auto` (`--full-auto` = workspace-write + on-request), `read-only`.
- OS enforcement: macOS Seatbelt (`sandbox-exec`), Linux Landlock + seccomp (also `bwrap` crate), native Windows sandbox (experimental; known limitation: can't protect world-writable dirs; `sandbox = "elevated"` recommended), WSL inherits Linux semantics. `codex sandbox macos|linux [CMD]` lets you test the sandbox yourself.
- Network: off by default locally; opt-in per workspace-write config; **enforced across redirects and ongoing HTTP/WebSocket traffic**, revocable mid-session (changelog 0.157).
- Web search: `web_search = "cached"` (default — an OpenAI-maintained pre-indexed cache, reduces prompt-injection exposure), `"indexed"`, `"live"` (`--search`), `"disabled"`.
- Protected paths inside writable roots (`.git`, `.codex`); project trust system — untrusted projects skip all project-scoped config/hooks/rules.
- **Guardian** (internal name visible in changelog): an AI "automatic approval reviewer" that scores planned actions for safety, caches approvals, preserves "authorization evidence" across compaction, and re-reviews when instructions change.
- Enterprise: `requirements.toml` admin enforcement (can ban `approval_policy = "never"`, `danger-full-access`), managed config layers (`/etc/codex/managed_config.toml`, macOS MDM payload), opt-in OTEL telemetry with redacted prompts.

### Extensibility
- **MCP:** stdio + streamable HTTP servers; OAuth (CIMD and DCR), bearer tokens, header helpers; per-server and per-tool approval modes (`auto`/`prompt`/`writes`/`approve`), tool allow/deny lists, timeouts; `codex mcp add/list/login`; config shared across CLI, IDE extension, desktop app ([MCP docs](https://developers.openai.com/codex/mcp)).
- **Skills:** Anthropic's open agent-skills standard; skills in `.agents/skills/`; `$skill` invocation; plugins bundle skills ([skills docs](https://developers.openai.com/codex/skills)).
- **Plugins** (March 2026): installable bundles packaging skills + app integrations + MCP server config; curated plugin directory, marketplace config, plugin-provided MCP servers controllable via `plugins.<plugin>.mcp_servers.*`.
- **Hooks:** lifecycle hooks via `hooks.json` or inline `[hooks]`; stable feature; admins can force `allow_managed_hooks_only`.
- **Slash commands:** `/init` (generate AGENTS.md), `/status`, `/permissions`, `/model`, `/review`, `/compact`, `/plan`, `/mcp`, `/usage`, `/tui`, `/voice`, `/daemon`, `/worktree`…
- **SDKs:** TypeScript SDK, Python SDK (`openai-codex`), app-server JSON-RPC protocol (`app-server-protocol` crate) for embedding the agent.

### Subagents / parallelism
- Subagent collaboration tools stable (`multi_agent` flag, default on); "Multi-Agent V2" with `spawn_agent`, per-child captured settings, MCP elicitation from subagents, agent message board (SQLite-backed inter-agent channels).
- Codex app: parallel agent threads per project with git **worktrees** so agents don't collide; worktrees enabled by default since 0.156; CLI `--worktree` / `/worktree`.
- Cloud: many tasks in parallel, each in its own container; `codex cloud exec --attempts N` best-of-N generation (community-reported).

### Planning UX
- `/plan` mode (restored on resume); to-do list tracking for complex work; `/goal` persisted objectives; inline questions with suggested choices.

### Models
- Current lineup (2026-09): **GPT-6 Sol** (complex coding/agentic, CLI default `gpt-6-sol`), **GPT-6 Luna** (focused, high-volume, cheaper), **GPT-6-Astra** (also used by Agents API examples), GPT-5.6 Sol (retiring GPT-5.5 on 2026-10-14). GPT-5.3-Codex-Spark deprecated 2026-09-14 ([changelog](https://developers.openai.com/codex/changelog)).
- Lineage: codex-1 (o3-based, May 2025) → codex-mini-latest → GPT-5-Codex (Sept 2025, dynamic thinking time: 93.7% fewer tokens on easy turns, 2× longer on hard ones; >7h autonomous runs) → 5.1/5.2/5.3-Codex (Feb 2026, self-improvement-assisted development, Trusted Access for Cyber) → 5.5/5.6 → GPT-6 Sol/Luna.
- `/model` or `--model` switching mid-session; `model_reasoning_effort` (`high`, plus `max`/`ultra` in SDK); Fast mode (`service_tier = "fast"`); Amazon Bedrock support; personality setting retired.
- Frequent model churn and forced migration prompts are the norm (GPT-5.2/5.3-Codex pulled from Codex June 2026).

### Collaboration / sharing / review
- **Code review:** local `/review` against uncommitted changes / commit / base branch — read-only, prioritized findings, never touches the working tree; optional detached review chat. GitHub: automatic PR review on draft→ready, `@codex review` with scoped instructions ("review for security vulnerabilities"); custom `## Code Review Rules` sections in AGENTS.md ([code review docs](https://developers.openai.com/codex/code-review), [AGENTS.md guide](https://developers.openai.com/codex/guides/agents-md)).
- **Review pane** (app): diff views (unstaged/staged/commit/branch/last turn), inline line comments fed back to the agent, per-hunk stage/revert.
- **Integrations:** GitHub (PRs, `@codex` mentions to delegate), GitLab beta, Linear, Slack; ChatGPT account is the shared identity; MCP config shared across surfaces.
- Browser self-review: cloud agent can spin up a browser, inspect its own frontend work, and attach screenshots to the task/PR.

## 4. Architecture

- **Language/runtime:** Rust workspace (`codex-rs/`) with ~100 crates; legacy TS CLI (`codex-cli/`) superseded. Built with Bazel; distributed as static binaries (musl on Linux), npm wrapper, Homebrew cask ([repo](https://github.com/openai/codex)).
- **Process model:** thin clients around an **app-server** (JSON-RPC protocol, `app-server-protocol` crate) that can run as a shared local **daemon** (`/daemon`, `--no-daemon`; auto-start since 0.157; survives CLI restarts, restores threads and goals). CLI TUI, IDE extension, and desktop app are all clients of the same app-server; a "background server" enables remote control from mobile/web.
- Notable crates revealing internals: `core` (agent loop), `execpolicy` (command policy), `guardian-context` (approval reviewer), `code-mode*` (programmatic tool calling runtime), `cloud-tasks*`, `linux-sandbox`, `bwrap`, `apply-patch`, `agent-message-board-client`, `hooks`, `file-watcher`, `keyring-store`, `attachment-store`.
- **Context assembly:** instruction chain = global `~/.codex/AGENTS.md` (or `AGENTS.override.md`) + per-directory files walked **from repo root down to cwd** (`AGENTS.override.md` > `AGENTS.md` > fallback names), concatenated root-first so deeper files win; capped at `project_doc_max_bytes` (32 KiB default); one file per directory; rebuilt every session ([AGENTS.md guide](https://developers.openai.com/codex/guides/agents-md)).
- **Cloud execution:** per-task container from `codex-universal` base image ([openai/codex-universal](https://github.com/openai/codex-universal), public Dockerfile); phases: clone → setup script (internet ON, secrets available) → maintenance script (on cache resume) → agent phase (internet OFF by default; secrets stripped) → diff + PR. Container state cached up to 12h; cache shared across workspace users on Business/Enterprise; all egress via HTTP/HTTPS proxy ([cloud environments](https://developers.openai.com/codex/cloud/environments)).
- **Cloud↔local sync:** unified by ChatGPT account (Sept 2025). `codex cloud` from the CLI browses/submits cloud tasks and applies the resulting diff to the local repo (`codex apply` per community guides); IDE extension can open cloud tasks with context preserved; worktrees give each local agent thread an isolated checkout.
- The codex-1 system message was published in the launch post — includes the AGENTS.md spec (scoping, precedence, "you MUST run all programmatic checks") and a citation format (`【F:path†L1-L2】`) for verifiable output ([Introducing Codex](https://openai.com/index/introducing-codex/)).
- The **Agents API** exposes the same harness (compaction, tool search, programmatic tool calling, subagents) as a managed, versioned service with pluggable sandbox providers (E2B, Modal, Vercel, etc.) — confirmation that OpenAI treats the harness as separable, reusable infrastructure.

## 5. Strengths (community-cited)

- **Generous limits / price-performance:** at $20, "Plus rarely makes me think about limits"; in a controlled 30-task harness bake-off Codex matched Claude Code's pass rate at ~58% lower cost ($0.08 vs $0.19 per success) ([Composio, 2026-08](https://composio.dev/content/claude-code-vs-openai-codex)).
- **Delegate-and-review loop is best in class:** `codex cloud` + `/review` called "the cleanest in-loop code review either tool ships"; `@codex review` GitHub flow widely praised ([tembo.io](https://www.tembo.io/blog/codex-vs-claude-code-vs-gemini-cli), Composio).
- **Steadiness/instruction obedience:** follows AGENTS.md instructions reliably over long sessions; "behave[s] the same this week as last week" (Composio). Code review quality trained to minimize false positives; OpenAI says Codex reviews "the vast majority" of its own PRs, catching hundreds of issues daily.
- **Polished app & multi-agent UX:** worktrees, parallel threads, voice; "the Codex macOS app is much cleaner and more polished" than rivals (Composio).
- **Best-of-N cloud attempts** (`--attempts`) and browser self-review with screenshots are called out as ahead of competitors (Composio).
- Sandbox model with real OS enforcement is respected; "Codex has the nicer sandbox" (Composio).

## 6. Weaknesses (community-cited)

- **Context handling under pressure:** large tool outputs are head/tail truncated with the middle dropped, losing critical content; long-session memory after compaction judged weaker than Claude Code's (Composio hands-on).
- **Over-eagerness:** "Give the CLI full autonomy and it rewrites massive amounts of code… feels like you're forced to vibe code instead of direct it"; "Send one ticket, it handles half then asks 'Should I also do X?' No! Focus." ([Reddit synthesis](https://dev.to/sangrokjung/claude-code-vs-codex-2026-what-500-reddit-developers-really-think-31pb)).
- **Cloud latency & scope limits:** small tasks take ~90s+ just to spin up a cloud container; cloud tasks are single-repo, awkward for multi-repo work ([Jens.Marketing](https://jens.marketing/tool/openai-codex/)).
- **Approval fatigue:** persistent community genre of "stop fighting Codex approval prompts" config guides ([The Deep Feed](https://www.thedeepfeed.ai/posts/2026-06-10-stop-fighting-codex-cli-approval-prompts/)).
- **Windows sandbox is experimental** with documented holes (world-writable dirs); sandbox can silently not work in Docker without Landlock/seccomp support.
- **Churn:** rapid model retirements (Spark deprecated, GPT-5.5 retiring), retired config options (`untrusted` policy, personalities), feature flags flipping — configs and scripts rot quickly.
- **Reasoning depth:** on hard architectural problems and deep multi-step debugging, reviewers consistently rank Claude Code ahead ([joinleland.com](https://www.joinleland.com/library/a/ai-coding-agents), NxCode blind tests).
- ~19k open GitHub issues; HN commenters note the cloud product and GUI are proprietary even though the CLI is open source ([HN thread](https://news.ycombinator.com/item?id=49281916)).

## 7. Distinctive ideas worth borrowing

1. **Sandbox mode × approval policy as two orthogonal axes.** Technical capability (read-only / workspace-write / full access, enforced by OS: Seatbelt, Landlock+seccomp) is separated from the asking policy (on-request / never). Presets (`--full-auto`) are just named points in this 2-D space. Bonus: a `codex sandbox <os> <cmd>` subcommand to *empirically test* what the sandbox blocks — turns security config into something debuggable.
2. **Cached web search as a prompt-injection defense.** Default `web_search = "cached"` serves results from a vendor-maintained pre-indexed cache instead of fetching live arbitrary pages; live fetching is an explicit opt-in (`--search`). Cheap way to give agents web knowledge without exposing them to attacker-controlled content.
3. **Layered instruction chain with deterministic precedence.** Walk from repo root *down* to cwd, one file per directory, `AGENTS.override.md` > `AGENTS.md` > configurable fallbacks, concatenated so deeper wins; global file in `$CODEX_HOME`; hard byte cap (32 KiB) with skip-empty rules. Fully deterministic "which rule won" semantics. (Codex championed AGENTS.md as a cross-vendor open standard.)
4. **Two-phase cloud environments with secrets hygiene.** Setup script runs with internet ON and secrets available; agent phase runs with internet OFF and secrets stripped; container state cached ~12h with explicit invalidation triggers; optional maintenance script on resume. Separating "prepare the environment" from "let the model act" shrinks the exfiltration surface while keeping tasks reproducible.
5. **Code review as a separate trained capability + read-only reviewer turn.** `/review` runs a reviewer that reports prioritized findings *without touching the working tree*; on GitHub, `@codex review [focus]` triggers scoped reviews, and repo-specific review policy lives in a `## Code Review Rules` section of AGENTS.md — review criteria are versioned with the code.
6. **Guardian: an LLM approval reviewer behind the human approvals.** Before asking the user (or auto-approving), a separate reviewer model scores the planned action with bounded context, caches decisions, invalidates them when user instructions change, and preserves "authorization evidence" across compaction. Turns approvals from string-matching into semantic risk review.
7. **Worktree-per-agent + unified account as the sync layer.** Each parallel agent thread (local app or cloud) gets its own git worktree/container; the ChatGPT account is the rendezvous — the same task can be started in the CLI, watched on a phone, and applied locally with `codex apply`. Identity, not file sync, is the coordination mechanism.

## 8. Lessons for a new agent

- **Copy:** the two-axis permission model with OS-enforced sandboxing and a sandbox self-test command; deterministic layered rules files with a size cap; secrets-allowed-only-in-setup cloud phases; read-only review turns; cached-by-default web access; shipping the system-prompt/instruction spec publicly (it built an ecosystem around AGENTS.md).
- **Copy with care:** account-unified multi-surface delegation is Codex's deepest moat but requires serious infrastructure (daemon + app-server + cloud containers). A JSON-RPC app-server that all frontends share is the enabling architecture — worth adopting early, cheap to do from day one, painful to retrofit.
- **Avoid:** middle-truncating large tool outputs (save full output to a file and let the model page it — Claude Code's approach wins hands-on comparisons); approval-prompt fatigue (invest in smart auto-approval like Guardian, or granular remembered rules); rapid deprecation of config keys and models without long migration windows.
- **Do differently:** keep large-output and long-session context fidelity as a first-class benchmark — it's where Codex visibly loses; make multi-repo tasks a native concept (Codex cloud is single-repo); reduce container cold-start for small tasks or route small tasks locally; commit to a stable config schema — Codex's churn (retired policies, personalities, feature flags) is a recurring tax on users.
