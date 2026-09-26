# IDE Ecosystem Beyond Cursor: Zed/ACP, Void, JetBrains Junie, Continue, Goose

One-line description: How five very different players embed agents in editors — Zed's protocol-first bet (ACP), Void's dead VS Code fork, JetBrains' index-native Junie, Continue's shareable config hub (now acquired/shut down), and Block's MCP-first goose — and what each proves about where agents should live relative to the editor.

Researched: 2026-09-26

---

## 1. Zed — performance-first editor with Agent Panel and ACP

### What it is
- Maker: Zed Industries (founded by Atom/Tree-sitter alumni Nathan Sobo, Max Brunsfeld, Antonio Scandurra). Public beta March 2023, open-sourced January 2024, **Zed 1.0 shipped April 2026** ([HN thread](https://news.ycombinator.com/item?id=47949027)).
- Surface: native desktop editor (macOS/Linux/Windows) written in Rust with a custom GPU UI framework (GPUI).
- Pricing (as of mid-2026): editor free + open source; Free plan includes 2,000 Zeta edit predictions/mo; **Pro $10/mo** ($5 of token credit included, then usage-based at API list price + ~10%); **Business $30/seat/mo** adds org controls ([tooljunction review](https://www.tooljunction.io/ai-tools/zed), [Zed plans docs](https://zed.dev/docs/account/plans-and-pricing)). BYOK and external agents are unlimited on the free tier. Zed does **not** charge for External Agents ([Zed external-agents docs](https://zed.dev/docs/ai/external-agents)).
- Distribution: direct download; open source (GPL for editor, Apache for GPUI/ACP).

### Core design philosophy
- "Just as the Language Server Protocol unbundled language intelligence from monolithic IDEs, our goal with the Agent Client Protocol is to enable you to switch between multiple agents without switching your editor. If you're an agent developer, we're also giving you a fast and convenient user interface inside an IDE, so you can focus on building the best possible agent rather than forking VS Code." — [Zed blog: Bring Your Own Agent to Zed](https://zed.dev/blog/bring-your-own-agent-to-zed)
- Performance and "multiplayer" collaboration as editor fundamentals; AI layered on as threads/diffs, not popups.
- Privacy stance: "nothing touches our servers, and we don't have access to your code" for third-party agents; rating a response is the only data-sharing path and is opt-in ([Agent Panel docs](https://zed.dev/docs/ai/agent-panel)).

### Feature inventory (Agent Panel)
- First-party **Zed Agent** plus **External Agents** over ACP, side by side in one panel; per-thread agent choice.
- Parallel threads: multiple concurrent agent threads, Threads Sidebar grouped by project, thread history, archiving, `ctrl-tab` thread switcher ([Agent Panel docs](https://zed.dev/docs/ai/agent-panel)).
- **Worktree isolation**: run a thread in a separate git worktree when two threads might touch the same files.
- **Checkpoints**: "Restore Checkpoint" on every message that edited files, including after interruption.
- Multi-buffer **Review Changes** with per-hunk accept/reject; optional inline single-file review mode.
- Context: `@`-mentions for files/dirs/symbols/threads/diagnostics/branch diffs/URLs/skills; pasted selections auto-become @-mentions; image input.
- Message editing & re-submission; **queued messages with "Steer"** (interrupt at next tool boundary); "New From Summary" to seed a compacted continuation thread.
- Token usage display, automatic context compaction near the window limit (with expandable "Context Compacted" entry).
- Agent Profiles (which built-in + MCP tools are enabled), Tool Permissions (allow/deny/confirm).
- **Terminal Threads**: CLI/TUI agents run in terminal-backed threads inside the same panel — an admission that ACP threads don't cover everything.
- Follow-the-agent crosshair (editor jumps to each touched file), prevent-idle-sleep during generation, done-notifications (visual + sound).
- **Zeta** edit prediction (open-source prediction model) — 2,000 free predictions/mo ([edit prediction docs](https://zed.dev/docs/ai/edit-prediction)).

### Architecture
- Rust + GPUI; editor open-sourced Jan 2024.
- ACP: JSON-RPC 2.0 over stdio (local agents as editor subprocesses); remote over HTTP/WebSocket is "work in progress" ([ACP overview](https://agentclientprotocol.com/overview/introduction)). Reuses MCP JSON representations "where possible" but adds agentic-UX types (diffs, permission prompts, streaming). Default text format is Markdown.
- ACP lifecycle: `initialize` handshake with capability negotiation and `protocolVersion`; current stable protocol version is `1`; wire compatibility is negotiated, schema artifacts versioned separately ([zed-industries/agent-client-protocol](https://github.com/zed-industries/agent-client-protocol)). Official SDKs: Rust, TypeScript, Kotlin, Java, Python.
- Zed's own in-process agent was **refactored to use the same code paths as external ACP agents** — "Cleanly separating UI from the core of our agent has already improved our shipping velocity" ([Zed blog](https://zed.dev/blog/bring-your-own-agent-to-zed)).
- **ACP Registry** inside Zed (`zed: acp registry`) for one-click agent install; curated list includes Claude (Agent), Codex, Gemini CLI, OpenCode, Copilot, Cursor, Pi, Poolside; custom agents configured via `agent_servers` in settings.json ([external-agents docs](https://zed.dev/docs/ai/external-agents)).
- Feature variance is explicit: "Restoring threads from history, checkpoints, token usage display… depend on the agent integration" ([Agent Panel docs](https://zed.dev/docs/ai/agent-panel)).
- Zed MCP servers may be **forwarded over ACP** to external agents; agents may also read their own native MCP config.

### Strengths (community)
- Speed is consistently praised; ACP integration of Claude Code/Codex "works very well" and "both support ACP. works really well!" ([HN: Zed 1.0](https://news.ycombinator.com/item?id=47949027)).
- Having code visible while chatting with an agent: "Using just the terminal for dense/long running work feels like a handicap" (same thread).
- "This is the kind of pricing AI tools should have" — BYOK parity with hosted models, no forced switch ([AI Agent Square review](https://aiagentsquare.com/agents/zed-ai)).

### Weaknesses (community)
- ACP-wrapped agents lag native CLI features: `/mcp`, `/plugins`-style self-management commands blocked; "they have to keep up with Claude Code functionality in the other direction. VSCode's Claude code plugin is just like using the CLI" ([HN: Zed 1.0](https://news.ycombinator.com/item?id=47949027)).
- Extension ecosystem immaturity vs VS Code/JetBrains: missing call hierarchy, notebook support, shallow CMake support; 600+ issues open >1 year (same thread).
- Editor papercuts: Wayland color issues, no bitmap fonts, broken shortcuts on non-Latin Linux keyboard layouts, polarizing search-in-new-tab UX (same thread).
- Edit prediction (Zeta) rated below Cursor's ("Cursor… has a much better edit prediction model than Zed", same thread).

### Distinctive ideas
1. **Editor as agent host, not agent vendor**: the same UI primitives (diffs, checkpoints, thread sidebar) serve N agents over one protocol.
2. **Dogfooded protocol boundary**: first-party agent forced through the same interface as third parties.
3. **Steer**: queued message that interrupts the agent at the next tool-call boundary — mid-flight course correction without aborting.
4. **New From Summary** as an explicit UX for context compaction (user-initiated, thread-seeding).
5. **Terminal Threads + ACP threads coexistence**: graceful fallback when the structured protocol can't express something.

---

## 2. Void — the open-source Cursor fork (deprecated)

### What it is
- Maker: Void (YC-backed startup; founders Mathew Pareles et al.). Open-source fork of VS Code, peaked at ~26k GitHub stars (15.9k early; [FB post](https://www.facebook.com/groups/developerkaki/posts/2479421525737079/)).
- **Status: DEPRECATED.** Repo now states "Void is deprecated and no longer accepting contributions… Void remains open source and is still one of the best references to use when forking VS Code" ([github.com/voideditor/void](https://github.com/voideditor/void)). Development was already "paused" earlier in 2026 while the team explored "novel coding ideas" ([cursor-alternatives.com](https://cursor-alternatives.com/blog/void-editor-development-status-2026/)). Exact pivot destination is not officially confirmed.

### Core design philosophy
- Privacy/BYOK first: "Use AI agents on your codebase, checkpoint and visualize changes, and bring any model or host locally. Void sends messages directly to providers without retaining your data" ([Void README](https://github.com/voideditor/void)) — a deliberate contrast to Cursor's proxied-requests model.
- Local-model support (Ollama) as a first-class story.

### Feature inventory (at freeze)
- Cursor-style feature set: Agent mode, chat, inline edit, **checkpoints**, fast apply, autocomplete (FIM), MCP support, BYOK for all major providers + local models ([AI IDE List review](https://aiidelist.com/ide/void)).
- Streaming inline diffs; background file edits synced to buffers.

### Architecture (the durable artifact)
- VS Code fork with documented invasive changes — now a de-facto guide for anyone forking VS Code:
  - **React + Tailwind mounted into the workbench**, requiring build-pipeline extensions (not possible in plain VS Code extension API).
  - Custom GitHub Actions for packaging/signing/auto-update (VS Code's pipeline is private).
  - From-scratch provider layer with **grammars for `<thinking>` tags and tool-call tags**; IPC design satisfying CSP.
  - `EditCodeService` — render diffs token-by-token as they stream in; `VoidModelService` — background file edits synced with text buffers ([Void README](https://github.com/voideditor/void)).

### Strengths
- Proved a community could rebuild ~80% of Cursor's UX in the open; genuinely useful for privacy-sensitive/local-model users.
- Its codebase guide is now the canonical public reference for VS Code forks.

### Weaknesses / failure modes
- **Couldn't survive the treadmill**: a small team maintaining a deep fork of a moving upstream (VS Code) while chasing a fast-moving commercial competitor (Cursor) — the exact failure Zed's ACP blog predicts ("rather than forking VS Code").
- Fork maintenance ate capacity; features trailed Cursor; monetization never landed; project abandoned.

---

## 3. JetBrains Junie & AI Assistant — mature IDE vendor integration

### What it is
- Maker: JetBrains. Product family under one **JetBrains AI subscription**: **AI Assistant** (chat + completion), **Junie** (the agent; IDE plugin + standalone CLI), **Air** (multi-agent orchestration environment running Codex/Claude Agent/Gemini CLI/Junie in parallel), **Mellum** (open-source completion model, free) ([AltAI: What is Junie](https://altaitools.com/what-is-junie/)).
- Timeline: Junie beta Jan 2025; unified AI subscription with IDE 2025.1 (Apr 2025); Junie CLI beta Mar 2026; **GA June 2026** ([tech-insider](https://tech-insider.org/jetbrains-junie-vs-github-copilot-vs-cursor-2026/), [AltAI](https://altaitools.com/what-is-junie/)).
- Pricing: AI Free ($0, small credits + unlimited Mellum); **AI Pro ~$10/mo**; **AI Ultimate ~$30/mo**; agent usage metered in **AI Credits (~1 credit ≈ $1 of model usage)**; BYOK supported (cheaper for heavy users). Some IDEs require paid editions for the plugin.
- Benchmark: **#1 on a 2026 SWE-Rebench cycle, ~61.6% resolved / 72.7% pass@5**, ~$1.14/task ([AICentralResources](https://www.aicentralresources.com/tool/junie), [AltAI review](https://altaitools.com/jetbrains-junie-review/)).

### Core design philosophy
- "The LLM is to an agent what an engine is to a car" — Junie is the harness: read the real project, act, **verify** ([AltAI](https://altaitools.com/what-is-junie/)).
- Leverage 25 years of IDE static analysis: the agent sees code "the way the IDE's navigation and refactoring engine does — not as a folder of loose text files."

### Feature inventory
- **Agent loop: Explore → Plan → Execute → Verify.** Plans are first-class artifacts saved to `.junie/plans`, user-editable before execution; vague requirements trigger multiple-choice/free-form questions.
- **Verify step**: runs existing tests + IDE syntax/semantic inspections after changes, fixes regressions — the signature differentiator.
- **Three modes**: Ask (read-only), Code (step-approved), Brave (no confirmations).
- Steering via **guidelines file** (`.junie/guidelines.md`, now also AGENTS.md); JetBrains ships ready-made guideline plugins (e.g., "modern Go").
- Junie CLI: any terminal/CI; connects back to a running IDE for code intelligence; BYOK incl. OpenRouter; ACP support so Junie runs inside other ACP clients, and JetBrains IDEs act as ACP clients for third-party agents ([mcp.directory](https://mcp.directory/blog/agent-client-protocol-vs-mcp-2026)).
- MCP server support in Junie; model choice across Anthropic/OpenAI/Google/xAI.

### Architecture
- Agent runs on top of the **IDE index** (PSI, symbol resolution, refactoring engine, run configurations) rather than grep — "an agent that can navigate into functions and validate code against the IDE's own analysis database has real advantages over agents that 'can only grep and hope'" (HN commenter via [AltAI](https://altaitools.com/what-is-junie/)).
- CLI + plugin share one agent core; CLI can borrow a running IDE's index.

### Strengths
- Best-in-class for large structured Java/Kotlin codebases; structural refactors benefit measurably from index access.
- Plan-before-code + test-after-change measurably reduces "confidently wrong" output ([vibecoding review](https://vibecoding.app/blog/junie-review)).
- Strong independent benchmark results (SWE-Rebench #1).

### Weaknesses (community)
- **Credit burn**: "credits run out so fast… even a 'small' task can cost several dollars"; one user burned an AI Pro monthly quota in 3 days; BYOK is the community escape valve ([AltAI pricing](https://altaitools.com/junie-pricing/)).
- **Slowness**: plan-and-execute loop "very slowwwww" vs Cursor on long coding days.
- **Over-eagerness**: "industrious junior developer" — does more than asked (extra refactors, dependency bumps); requires explicit scope limits and guidelines.
- Marketplace complaints: ignores explicit instructions, edits forbidden files, intermittent auth errors; no local-model option for agentic tasks (data residency concern) ([AltAI](https://altaitools.com/what-is-junie/)).

### Distinctive ideas
1. **Grounding the agent in the IDE's semantic index** instead of text search — the one advantage a mature IDE vendor has that no CLI agent can replicate.
2. **Plans as durable, user-editable artifacts** committed to the repo (`.junie/plans`).
3. **Verification built into the loop** via the IDE's own inspection engine, not just test runs.
4. **Air**: the IDE vendor becoming the neutral multi-agent orchestrator (runs competitors' agents) rather than locking to its own.

---

## 4. Continue — open-source autopilot with hub "blocks" (acquired, shut down)

### What it is
- Maker: Continue Dev, Inc. (Ty Dunn, Nate Sesti; YC S23; ~$5M raised from Heavybit/YC). OSS extensions for **VS Code + JetBrains**, plus a CLI; Apache 2.0; ~34.3k stars / 4.8k forks at the end ([The New Stack](https://thenewstack.io/cursor-acquires-continue-coding/)).
- **Status: DEAD.** Cursor acqui-hired Continue around **June 16, 2026**; final release v2.0.0 (telemetry removed) shipped June 19; user data deleted after July 15, 2026; repo read-only ([The New Stack](https://thenewstack.io/cursor-acquires-continue-coding/), [continue.dev](https://continue.dev/)). Nate Sesti joined Cursor; two founding engineers went to OpenClaw.

### Core design philosophy
- "The 'one-size-fits-all' AI code assistant will be a thing of the past" — Ty Dunn ([Continue 1.0 launch](https://www.bigtechnology.com/p/continue-launches-10-with-open-source)).
- Data control: "When you use Continue, you get to keep your data… That is not possible in the one-size-fits-all, black box code assistant" — Dunn.

### Feature inventory
- IDE extension modes: chat, **autocomplete**, edit, and an **agent** mode; model-agnostic (any provider incl. local via Ollama).
- **Hub (hub.continue.dev)**: registry of composable **blocks** — Models, MCPs/tools, Rules, Prompts, Data/context providers — published by verified partners (Anthropic, Mistral, Ollama, Voyage, Docker) and community; blocks have Personal/Public/Organization visibility; **assistants = composed bundles of blocks**, remixable and shareable ([1.0 launch](https://www.bigtechnology.com/p/continue-launches-10-with-open-source), [Continue docs](https://docs.continue.dev/hub/introduction)).
- Templating: `${{ inputs.X }}` for user-supplied values, `${{ secrets.Y }}` for secret indirection — shareable configs without leaking credentials.
- Enterprise: governance policies over which blocks may be created/shared/used; **private data plane** deployable in customer infrastructure.
- Late-stage "Mission Control": cloud Agents/Tasks/Workflows on top of the same components.

### Architecture
- YAML-configured assistants; hub as the registry/distribution layer; extensions as the runtime; templated secret injection between them.

### Strengths
- The hub was the **only serious attempt at an open registry for shareable agent config packs** — models + rules + tools as versioned, remixable units with org governance.
- True model/provider agnosticism and JetBrains + VS Code parity from one codebase.

### Weaknesses / failure modes
- **Business failure**: $5M seed couldn't compete with foundation-model-subsidized competitors; acqui-hired and shut down, stranding users with a weeks-long export window — a cautionary tale about OSS-wrapper startups in this market.
- Complexity: blocks/remix/governance added config burden many users found heavy vs. zero-config rivals (recurring Reddit sentiment).
- Agent mode arrived late and was widely considered weaker than Cline/Cursor.

---

## 5. Goose (Block) — open-source MCP-first agent framework

### What it is
- Maker: Block (Jack Dorsey's company), launched as "codename goose" Jan 2025 ([Block blog](https://block.xyz/inside/block-open-source-introduces-codename-goose)); **donated April 2026 to the Agentic AI Foundation (AAIF) at the Linux Foundation** — same umbrella as MCP and AGENTS.md ([outofcontext.dev](https://outofcontext.dev/blog/goose-claude-code-opencode-pi/)).
- Surfaces: **native desktop app (macOS/Linux/Windows), CLI, and an embeddable API**; "built in Rust for performance and portability" ([goose README](https://github.com/block/goose)).
- Pricing: free, Apache 2.0, no markup; pay your provider directly (~$3–8/hr heavy use on frontier models); can reuse existing Copilot/Cursor/Claude/ChatGPT subscriptions ([Koenig Academy](https://academy.kspl.tech/blog/ai-tool-deep-dive-goose), README).
- Adoption: deployed to all ~12,000 Block employees by Oct 2025, "8–10 hours saved per week" claimed; ~50k GitHub stars mid-2026 ([sanj.dev](https://sanj.dev/post/goose-vs-claude-code/), [outofcontext.dev](https://outofcontext.dev/blog/goose-claude-code-opencode-pi/)).
- Positioning: **general-purpose agent that happens to code**, not an "open Claude Code" — research, writing, automation, data analysis.

### Core design philosophy
- Local-first, MCP-native: every capability is an MCP server ("goose's built-in extensions are MCP servers in their own right… you are free to use them with any other agent") ([goose extensions docs](https://block.github.io/goose/docs/getting-started/using-extensions/)).

### Feature inventory
- **Extensions = MCP servers**: built-ins (Developer [default], Computer Controller, Memory, Tutorial, Auto Visualiser) + platform extensions (Apps, Chat Recall, Code Mode, **Extension Manager**, **Summon** [skills/recipes/subagents], Todo, **Top of Mind** [persistent per-turn instructions]); 70+ directory extensions; any MCP server addable; MCP Roots support.
- **Deeplink install**: `goose://extension?cmd=npx&arg=...` one-click extension install.
- **Smart extension recommendation**: agent detects missing capability, searches directory, enables extension mid-session (session-scoped).
- Malware screening of external extensions before activation.
- **Recipes**: portable YAML workflows capturing instructions, prompt, activities, parameters, model/provider, extensions, **subrecipes**, response JSON schema (structured output validated via a `final_output` tool), and **retry logic with shell-based success checks + cleanup**; shareable via deeplink or file; create-from-current-session; **schedulable** (cron, background/foreground); privacy-filtered (excludes memory, credentials, system settings) ([goose recipes docs](https://block.github.io/goose/docs/guides/recipes/session-recipes/)).
- 15+ providers; local via Ollama; subscription auth via ACP.
- **Custom Distributions**: build your own goose distro with preconfigured providers, extensions, branding.
- Parallel sessions, but no inter-agent messaging/shared task lists (vs Claude Code's agent teams) ([Morph comparison](https://www.morphllm.com/comparisons/goose-vs-claude-code)).

### Strengths (community)
- "I actually really like the approach they are taking, especially because I use emacs and not vscode… results are quite good for small, additive features and even ones that are full stack" ([HN launch thread](https://news.ycombinator.com/item?id=42879323)).
- Editor-agnostic by construction; MCP bet paid off as MCP became the standard; enterprise-proven at Block scale; Linux Foundation governance.

### Weaknesses (community)
- Desktop UI polish: "they really need to polish the UI. Comparing to desktop version of chatgpt… they are in much lower league" (HN).
- Reliability of non-coding extensions: Computer Controller "most of the time wouldn't even open a website" (HN).
- "Local" marketing critique: first-run pushes a remote provider; local models possible via Ollama but not the default path (HN).
- Single-agent focus: no native multi-agent coordination (Morph).

### Distinctive ideas
1. **Everything-is-MCP orthogonality**: the agent's own tools are reusable MCP servers, erasing the line between "built-in" and "plugin."
2. **Recipes**: YAML-packaged agent personas+toolsets+retry/validation logic, shareable by link — the most complete "shareable agent pack" implementation after Continue's hub.
3. **Agent-managed extension discovery**: the agent installs its own tools mid-task.
4. **Custom Distributions**: white-label agent packaging for enterprises.

---

## 6. The protocol landscape: ACP vs MCP vs IDE extensions

- **MCP (Anthropic, Nov 2024)**: agent ↔ tools/data. Ubiquitous (79k+ stars on servers repo; every major client). Answers "what can the agent reach."
- **ACP (Zed, first shipped with Gemini CLI integration ~Aug 2025; stable protocol v1)**: editor ↔ agent. JSON-RPC/stdio locally, HTTP/WS remotely (WIP). Answers "where does the agent live in the workflow" ([ACP overview](https://agentclientprotocol.com/overview/introduction), [contra collective](https://contracollective.com/blog/agent-client-protocol-acp-devin-desktop-editor-agent-standard-2026)).
  - Editor-side adopters: **Zed, JetBrains IDEs, Neovim (CodeCompanion), Emacs, marimo, Obsidian**, community clients ([mcp.directory](https://mcp.directory/blog/agent-client-protocol-vs-mcp-2026)).
  - Agent-side: Gemini CLI and Claude Code as reference implementations; registry now includes **Goose, OpenHands, Qwen Code, GitHub Copilot CLI, Codex, OpenCode, Cursor, Pi, Poolside** ([mcp.directory](https://mcp.directory/blog/agent-client-protocol-vs-mcp-2026), [Zed docs](https://zed.dev/docs/ai/external-agents)). Morph claims "adopted by JetBrains, Google, GitHub, and 25+ agents" ([Morph](https://www.morphllm.com/agent-client-protocol)).
  - Scope limits: ACP deliberately reuses MCP's JSON types but adds diff rendering, permission requests, streaming; it does **not** cover tool/data access (that's MCP) or agent↔agent (that's A2A territory). Known gap: agents' self-management commands (e.g., Claude's `/mcp`, `/plugins`) don't traverse ACP — wrappers lag native CLIs ([HN](https://news.ycombinator.com/item?id=47949027)).
- **Native IDE extensions (VS Code / JetBrains plugin APIs)**: still the richest integration surface (full UI, editor internals) but N×M cost, marketplace lock-in, and — as Void proved — forking the editor itself is a losing strategy for small teams.
- Complementary framing that has become consensus: "MCP connects an agent to its tools… ACP connects the editor to the agent… They operate at different layers" ([contra collective](https://contracollective.com/blog/agent-client-protocol-acp-devin-desktop-editor-agent-standard-2026)).

---

## 7. Distinctive ideas worth borrowing (cross-product)

1. **Protocol-first agent/editor boundary (Zed)**: implement ACP server-side so any editor can host loongagent; dogfood it by making loongagent's own UI use the same code path.
2. **Semantic-index grounding (Junie)**: where a real IDE index exists, let the agent query symbols/refs/inspections instead of grep — measurable quality gain on large typed codebases.
3. **Shareable config packs with secret indirection (Continue hub, Goose recipes)**: models + rules + MCP tools as versioned, remixable blocks with `${{ secrets.X }}` templating and org governance — the clearest precedent for loongagent's "rich backend control plane."
4. **Agent self-provisioning of tools (Goose)**: smart extension discovery — the agent finds and enables missing MCP servers mid-session.
5. **Steer-able queue (Zed)**: interrupt at the next tool boundary rather than full abort — mid-run course correction.
6. **Checkpoints + multi-buffer review (Zed, Void)**: per-message restore points and hunk-level accept/reject are table stakes for editor-embedded agents.
7. **Terminal-thread fallback (Zed)**: when the structured protocol can't express a feature, embed the raw TUI rather than blocking the user.

## 8. Lessons for loongagent

- **Copy**: ACP implementation from day one (server-side). It is the LSP moment for agents: one implementation buys presence in Zed, JetBrains, Neovim, Emacs. Goose shows MCP-first extensibility compounds; Continue's hub shows config-pack registries are valued and currently ownerless (a vacuum loongagent's control plane could fill).
- **Copy**: Zed's honest BYOK pricing (no markup lock-in) and feature-variance honesty (declare which features each external agent supports).
- **Avoid**: **Forking an editor (Void's failure)** — maintenance treadmill + chasing Cursor killed it despite 26k stars. Also avoid building a **proprietary per-editor plugin per agent** (N×M).
- **Avoid**: Junie's opaque credit metering — unpredictable agent billing ("quota gone in 3 days") is its #1 complaint; loongagent's cost governance should show per-task cost forecasts and hard caps.
- **Avoid**: Goose's desktop-UI underinvestment and "local-first" marketing that defaults to remote setup — surface mismatch erodes trust.
- **Do differently**: Continue and Void both died as thin OSS layers over others' platforms with no monetization moat. loongagent's moat should be the **control plane** (governance, cost, fleet orchestration) — the layer none of these five own — while treating editor surfaces as protocol-driven commodities.
- **Watch**: JetBrains Air (vendor-neutral multi-agent IDE) and ACP remote-agent support (HTTP/WS) — both point at the cloud-agent-in-editor convergence loongagent's backend should anticipate.
