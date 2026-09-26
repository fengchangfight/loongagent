# Multi-Agent Orchestration: Role-Based Frameworks & Parallel-Session Managers

Two sub-genres of "many agents": (a) frameworks that assign LLM agents organizational roles (MetaGPT, ChatDev, CrewAI, AutoGen) to automate software development or business workflows, and (b) tools that manage many parallel human-steered coding-agent sessions (vibe-kanban, Conductor, Crystal, claude-squad). The first genre simulates an org chart inside one run; the second treats the human as the org chart and the agents as workers.

Researched: 2026-09-26

---

## 1. What it is

### (a) Role-based multi-agent frameworks

- **MetaGPT** (DeepWisdom / FoundationAgents, open source, ~64k GitHub stars; paper at ICLR 2024). A Python framework that takes a one-line requirement and runs a simulated software company: PM, architect, project manager, engineer, QA roles executing SOPs that emit structured artifacts (PRDs, API designs, code). Commercialized as **MGX** (mgx.dev, launched Feb 2025), a hosted "AI agent development team". Also ships Data Interpreter (plan-and-code data-analysis agent) and research lines (AFlow, ICLR 2025 oral). ([GitHub](https://github.com/FoundationAgents/MetaGPT), [paper](https://arxiv.org/abs/2308.00352))
- **ChatDev** (OpenBMB / THUNLP, open source). A "virtual software company" where agent pairs (instructor/assistant: CEO↔CTO, CTO↔programmer, programmer↔tester) hold multi-turn dialogues through a fixed **chat chain** of phases (design → code → test → document). Pivoted in Jan 2026 to **ChatDev 2.0 "DevAll"**, a zero-code YAML-configured multi-agent orchestration platform (Vue web console, FastAPI backend); classic v1 moved to a legacy branch. ([GitHub README](https://github.com/OpenBMB/ChatDev), [paper](https://arxiv.org/abs/2307.07924))
- **CrewAI** (crewai.com; open-source Python framework + paid Enterprise tier launched Mar 2026). The most commercially successful descendant of the MetaGPT idea: agents defined by `role` / `goal` / `backstory`, tasks with `expected_output`, sequential or hierarchical processes, and **Flows** (event-driven state machines) as the deterministic backbone around autonomous **Crews**. Claims ~60% of Fortune 500 multi-agent PoCs touch it (vendor-adjacent source; treat with care). ([docs](https://docs.crewai.com/en/introduction), [Kanerika comparison](https://kanerika.com/blogs/crewai-vs-autogen-microsoft-agent-framework/))
- **Microsoft AutoGen** (Microsoft Research, fall 2023). Conversation-as-coordination: agents solve tasks by talking to each other (`GroupChat`, `AssistantAgent`/`UserProxy`). v0.4 was a full actor-model rewrite that broke backward compatibility, triggering the community fork **AG2**. In Oct 2025 Microsoft merged AutoGen + Semantic Kernel into the **Microsoft Agent Framework**; AutoGen went to maintenance mode (Feb 2026), MAF reached 1.0 GA (Apr 2026). ([AutoGen update discussion](https://github.com/microsoft/autogen/discussions/7066), [VS Magazine](https://visualstudiomagazine.com/articles/2026/04/06/microsoft-ships-production-ready-agent-framework-1-0-for-net-and-python.aspx))

### (b) Parallel-session managers ("control planes" for coding agents)

A 2025-2026 product category: all drive the same first-party CLIs (Claude Code, Codex, Gemini CLI, …) and differ only in the supervision layer around them. ([CodeAgentSwarm category guide](https://www.codeagentswarm.com/en/guides/best-tools-to-run-multiple-ai-coding-agents), [Abralo comparison](https://abralo.com/alternatives))

- **vibe-kanban** (Bloop; open source, Apache-2.0, ~27.9k stars). Local server + browser UI: kanban board where each task spawns an agent in its own git worktree, with diff review, inline comments, dev-server preview, PR creation; supports 10+ agents. **Bloop wound down April 2026; project is now community-maintained with last public commit 2026-04-24 and a "sunsetting" banner on the README.** ([GitHub](https://github.com/BloopAI/vibe-kanban))
- **Conductor** (Melty Labs, YC S24; closed source, free). macOS app running each agent in an isolated git worktree with a polished diff/review/merge flow; evolved into cloud workspaces (isolated microVMs with repos pre-installed), multiplayer workspaces (share links, watch teammates' agents), an API, and mobile access. Agents: Claude Code, Codex, Cursor, OpenCode. ([conductor.build](https://www.conductor.build/), [Abralo](https://abralo.com/alternatives))
- **Crystal** (stravu; open source, Electron). Parallel sessions in worktrees with side-by-side run comparison. **Deprecated Feb 2026, replaced by Nimbalyst** (MIT, cross-platform "AI-native workspace" with worktree isolation and multi-format editors). ([Crystal repo](https://github.com/stravu/crystal))
- **claude-squad** (smtg-ai; open source, AGPL-3.0, ~8.4k stars). Minimal TUI: tmux + git worktrees, agent-agnostic (`cs -p "codex"`, `aider`, …), background execution with `-y` autoyes mode, per-session diff preview, checkout/pause/resume, branch push via `gh`. Works over SSH with no desktop. ([GitHub](https://github.com/smtg-ai/claude-squad))
- Fast-growing neighbors worth knowing: **Superset** (source-available, 10+ agents, ~13.5k stars), **T3 Code** (MIT, ~21k stars in <7 months, fastest growth in category), **Paseo** (self-hosted daemon + mobile clients), **Sculptor** (Imbue; Docker-container isolation instead of worktrees), **Abralo**, **CodeAgentSwarm**. ([CodeAgentSwarm guide](https://www.codeagentswarm.com/en/guides/best-tools-to-run-multiple-ai-coding-agents))

## 2. Core design philosophy

**MetaGPT**: "`Code = SOP(Team)` is the core philosophy. We materialize SOP and apply it to teams composed of LLMs." The bet: human coordination knowledge (standardized operating procedures, structured documents) is the missing ingredient, not more agents. ([README](https://github.com/FoundationAgents/MetaGPT))

**ChatDev**: language itself is the coordination protocol — agents "are guided in what to communicate (via chat chain) and how to communicate (via communicative dehallucination)... establishing language as a unifying bridge". Notably, the ChatDev team's own follow-up research (MacNet, 2024; Puppeteer, NeurIPS 2025) moved *away* from the fixed chain toward learned/evolving topologies — an implicit admission that hardcoded org charts don't scale. ([paper](https://arxiv.org/abs/2307.07924), [README news](https://github.com/OpenBMB/ChatDev))

**CrewAI**: "Flow is the manager... Crews are the teams that do the heavy lifting" — autonomy where you need it, deterministic control around it. Their docs now advise "for any production-ready application, start with a Flow", i.e. role-play is a component, not the architecture. ([CrewAI docs](https://docs.crewai.com/en/introduction))

**AutoGen**: natural-language dialogue as the primary coordination mechanism. ([MDPI survey](https://www.mdpi.com/1999-5903/18/6/326))

**Parallel-session managers**: the shared philosophy is that *the agent stays dumb about other agents*. "Every tool in this guide does the same job. It launches coding CLIs that you already pay for, runs several at once, and gives you one place to watch, review and steer them." Vibe Kanban's framing: "In a world where software engineers spend most of their time planning and reviewing coding agents, the most impactful way to ship more is to get faster at planning and review." ([CodeAgentSwarm](https://www.codeagentswarm.com/en/guides/best-tools-to-run-multiple-ai-coding-agents), [vibe-kanban README](https://github.com/BloopAI/vibe-kanban))

**The genre's two canonical position essays**:

- Anthropic (pro, with caveats): multi-agent wins for "heavy parallelization, information that exceeds single context windows, and interfacing with numerous complex tools" — but "most coding tasks involve fewer truly parallelizable tasks than research". Their multi-agent system beat single-agent Opus 4 by 90.2% on internal research evals, while burning ~15× the tokens of a chat. ([Anthropic engineering blog](https://www.anthropic.com/engineering/multi-agent-research-system))
- Cognition (contra, for coding): "Don't Build Multi-Agents" — sub-agent task-splitting is "very fragile" because subagents can't see each other's actions; "Actions carry implicit decisions, and conflicting decisions carry bad results." Their default: single-threaded linear agent with full shared context, plus context compression for long tasks. ([Cognition blog](https://cognition.ai/blog/dont-build-multi-agents))

## 3. Feature inventory

### Role-based frameworks

**MetaGPT**
- Roles with SOP-encoded action sequences: ProductManager (PRD, user stories, competitive analysis), Architect (system design, API spec), ProjectManager (task breakdown), Engineer, QA.
- **Structured artifacts, not chat**: every role output is a typed document (PRD.md, design.md, code files) — the key differentiator vs pure-dialogue systems.
- **Publish-subscribe shared message pool**: roles subscribe to artifact types instead of pairwise chatting (reduces noise vs broadcast).
- Executable feedback: QA runs generated code, failures loop back to Engineer.
- One-line requirement → repo (`metagpt "Create a 2048 game"`); Data Interpreter for ML/data tasks; MGX hosted product.
- Research spinoffs: AFlow (agentic workflow auto-generation via Monte Carlo search, ICLR 2025 oral), SPO (self-supervised prompt optimization). ([GitHub](https://github.com/FoundationAgents/MetaGPT), [paper](https://arxiv.org/abs/2308.00352))

**ChatDev (v1)**
- Chat chain: fixed waterfall phases (designing, coding, testing, documenting), each decomposed into sub-chats between exactly two agents (instructor/assistant).
- **Inception prompting / role specialization**: system prompts assign roles; **communicative dehallucination** (assistant reformulates its understanding before acting, instructor challenges); **thought injection** in coding phase to preempt hidden bugs; role-flip prevention guardrails.
- **Git mode** (agents do version control), **Human-Agent-Interaction mode** (human plays the reviewer role in the loop), Art mode (designer agent generates assets), incremental development on existing codebases, replay/visualization tools, Docker sandbox. Reported economics for v1 demos: roughly one simple app per run in minutes for under ~$1 (2023 GPT-3.5/4 pricing; outdated today).
- Research lineage: Experiential Co-Learning (agents accumulate "shortcut experiences" across tasks), MacNet (DAG topologies, >1000 agents without context overflow), Puppeteer (RL-trained central orchestrator that dynamically activates/sequences agents, NeurIPS 2025). ([README](https://github.com/OpenBMB/ChatDev), [paper](https://arxiv.org/abs/2307.07924))

**CrewAI**
- Agent = role + goal + backstory + tools + optional LLM override; Task = description + expected_output + assigned agent; Crew = agents + tasks + Process (sequential | hierarchical with auto-manager-agent | consensual).
- Flows: decorated Python (`@start`, `@listen`, `@router`) for state, branching, and wrapping crews; docs recommend Flow-first for production.
- YAML or code config, tool ecosystem, memory/knowledge, guardrails, human-in-the-loop tasks; Enterprise tier (Mar 2026): observability, scheduling, RBAC, audit logs, on-prem. ([docs](https://docs.crewai.com/en/introduction), [Kanerika](https://kanerika.com/blogs/crewai-vs-autogen-microsoft-agent-framework/))

**AutoGen / Microsoft Agent Framework**
- Conversational agents (AssistantAgent, UserProxy for code execution/HITL), GroupChat with pluggable speaker selection, code-execution sandboxing.
- v0.4: async actor-model runtime, cross-language, observability (OpenTelemetry), AutoGen Studio UI; community fork AG2 keeps the 0.2 API alive.
- MAF (successor): type-safe agent interactions, durable sessions/checkpointing, telemetry, MCP + A2A protocol support, Python + .NET. ([AutoGen discussion #7066](https://github.com/microsoft/autogen/discussions/7066), [MDPI survey](https://www.mdpi.com/1999-5903/18/6/326))

### Parallel-session managers

**vibe-kanban**
- Kanban board as planning surface; each task → workspace = branch + worktree + terminal + dev server; inline diff comments that get *sent back to the agent* as feedback; in-UI app preview (browser + devtools + device emulation); PR creation with AI-generated descriptions; MCP server so agents/Claude Code can read & update the board; agent-agnostic executor layer (10+ CLIs incl. Claude Code, Codex, Gemini CLI, Copilot, Amp, Cursor, OpenCode, Kimi Code CLI); Rust backend + web UI, SQLite; cloud/team features + relay-tunnel mode for remote servers. ([README](https://github.com/BloopAI/vibe-kanban))

**Conductor**
- One workspace = one worktree on one repo; diff review, PR + Linear/GitHub integration; **Conductor Cloud**: per-agent isolated microVM sandboxes with deps pre-installed, "spin up in seconds so agents can run for hours"; **multiplayer**: shareable workspace links, live presence, co-prompting; public API; mobile access; runs first-party CLIs under the user's own subscriptions (BYOK). macOS-only local app (Windows waitlist). ([conductor.build](https://www.conductor.build/), [user notes](https://julianastrada.com/blog/conductor-parallel-agents/))

**claude-squad**
- tmux session + git worktree per task; attach/detach; diff tab vs preview tab; `checkout` (commit + pause) / `resume`; commit-and-push via `gh`; profiles mapping names to launch commands (any CLI); `-y/--autoyes` experimental auto-accept; config at `~/.claude-squad/config.json`. No diff-commenting, no PR flow, no notifications — deliberately minimal. ([README](https://github.com/smtg-ai/claude-squad))

**Category-standard feature set** (from comparisons): worktree-per-session isolation, diff review, "agent stopped / needs permission" notifications, run comparison (Crystal pioneered side-by-side), task board, dev-server preview, mobile supervision (Paseo, Nimbalyst iOS), container isolation as worktree alternative (Sculptor). Notably, even Claude Code itself now ships first-party `--worktree` support, absorbing the category's core trick. ([Claude Code worktrees docs](https://code.claude.com/docs/en/worktrees))

## 4. Architecture

- **MetaGPT**: Python framework; roles subscribe to message pool via a publish-subscribe protocol; SOPs materialized as prompt chains over structured schemas (each action validates/serializes typed outputs). Heavy prompt scaffolding — every role carries document templates. ([paper](https://arxiv.org/pdf/2308.00352))
- **ChatDev v1**: deterministic phase controller over LLM dialogues; each sub-chat is a two-agent loop with turn limits and termination heuristics; ChatDev 2.0 re-platformed onto FastAPI backend + `runtime/` agent abstraction + YAML workflow definitions + Vue 3 console + PyPI `chatdev` SDK. ([README](https://github.com/OpenBMB/ChatDev))
- **AutoGen v0.4**: event-driven actor runtime (`autogen-core`), agents as actors exchanging typed messages; layered API (core → AgentChat → extensions). MAF adds durable execution + checkpointing on top. ([MDPI survey](https://www.mdpi.com/1999-5903/18/6/326))
- **vibe-kanban**: Rust backend (axum) + web frontend, SQLite per project, spawns agent CLIs as child processes in worktrees, normalizes their streaming logs into a unified conversation view via per-agent "executor" adapters; exposes MCP server for agents to manipulate the board. ([README dev docs](https://github.com/BloopAI/vibe-kanban))
- **claude-squad**: thin Go TUI; literally orchestrates tmux sessions and `git worktree add`; no protocol layer — the agent's own TUI is the interface. ([README](https://github.com/smtg-ai/claude-squad))
- **Conductor**: closed-source native macOS app; local mode = worktrees; cloud mode = Firecracker-style microVMs per agent with repo/dependency images. ([conductor.build](https://www.conductor.build/))

## 5. Strengths (community-sourced)

- **Structured artifacts beat chat transcripts** (MetaGPT): PM/Architect hand off real documents, making intermediate work inspectable and reusable; community credits it as the best "software factory in a box" demo and the source of CrewAI's entire abstraction set. ([AIGC Camp analysis](https://aigccamp.com/multi-agent/metagpt-chatdev), [nolist.ai review](https://nolist.ai/item/metagpt))
- **CrewAI's predictability is the enterprise selling point**: role-scoped agents + sequential processes produce auditable pipelines; reported 30–60% better token efficiency than conversational frameworks on structured tasks because role-play avoids negotiation back-and-forth. ([Kanerika](https://kanerika.com/blogs/crewai-vs-autogen-microsoft-agent-framework/))
- **Worktree-per-agent isolation genuinely works**: near-universal praise across the session-manager category; "same repo, shared history, no duplication" beats multiple clones or tmux hacks. ([mindstudio](https://www.mindstudio.ai/blog/claude-code-git-worktree-parallel-branches))
- **Diff-review-in-the-tool is the feature people stay for**: vibe-kanban's inline comments sent back to the agent, Conductor's review-and-merge flow, Crystal's run comparison. Engineers report the job shifting "from writing code to managing/reviewing AI output" and these tools match that shift. ([note.com field report](https://note.com/lilfumi/n/ne1ca4393171a), [vibe-kanban README](https://github.com/BloopAI/vibe-kanban))
- **Anthropic's orchestrator-worker results**: 90.2% improvement over single-agent on research evals; parallelization cut research time "by up to 90% for complex queries". Strongest public evidence multi-agent *can* pay off. ([Anthropic](https://www.anthropic.com/engineering/multi-agent-research-system))
- **claude-squad's minimalism**: terminal-native, SSH-friendly, agent-agnostic with a one-flag switch — loved by the tmux crowd precisely because it adds almost nothing. ([Abralo](https://abralo.com/alternatives))

## 6. Weaknesses (community-sourced)

- **Role play doesn't buy much beyond one role.** MetaGPT/ChatDev gains on benchmarks are often minimal vs a strong single agent; the Berkeley MAST study (1,600+ annotated traces across 7 frameworks) found most multi-agent failures are systemic design/coordination failures (specification errors, inter-agent misalignment, verification gaps), not model limits. ([MAST paper](https://arxiv.org/abs/2503.13657))
- **Token cost explosion.** Multi-agent ≈ 15× chat tokens (Anthropic); crews spawn runaway loops and cost blowups; one production team cut prompt tokens 38% just by *removing* role-play overhead (personas, manager chatter). ([Anthropic](https://www.anthropic.com/engineering/multi-agent-research-system), [BirJob comparison](https://www.birjob.com/blog/ai-agent-framework-showdown-2026))
- **Demo-ware ceiling.** MetaGPT/ChatDev shine on greenfield toy apps ("2048 game") and degrade on real codebases: long sequential chains compound errors, generated docs drift from code, no persistence across runs in v1. Reddit sentiment: MetaGPT is "a niche power tool for 'software-factory' outputs rather than general-purpose agency". ([nolist.ai](https://nolist.ai/item/metagpt), [ChatDev vs MetaGPT analysis](https://inferensys.com/differences/agentic-workflow-orchestration-frameworks/multi-agent-orchestration-frameworks/chatdev-vs-metagpt))
- **Framework churn/abandonment.** AutoGen's v0.4 rewrite broke everything (→ AG2 fork), then Microsoft deprecated it again for MAF within ~2 years. Crystal deprecated → Nimbalyst. Vibe Kanban's company wound down (Apr 2026); most-starred tool in its category is community-maintained. Wrappers die silently: "a wrapper that stops being maintained does not announce itself... one CLI update lands and the diff view goes blank." ([AutoGen discussion](https://github.com/microsoft/autogen/discussions/7066), [CodeAgentSwarm](https://www.codeagentswarm.com/en/guides/best-tools-to-run-multiple-ai-coding-agents))
- **The human is the bottleneck at >3-5 agents.** HN: "for real work I can only do 2-3 agents in parallel. Human review still remains the eventual bottleneck." Cost multiplies per parallel agent; review responsibility multiplies too: "The risk is treating the parallelism as a reason to reduce review, not maintain it." ([HN Superset thread](https://news.ycombinator.com/item?id=46368739), [lowcode.agency](https://www.lowcode.agency/blog/claude-code-git-worktrees))
- **Worktree isolation ≠ conflict freedom.** Worktrees prevent *live* interference but merge-time conflicts remain; none of the tools do semantic merge assistance — they surface diffs and leave reconciliation to the human or a follow-up agent prompt. Subagents also produce *stylistically inconsistent* work (Cognition's Flappy Bird example: two subagents built mismatched game assets). ([Cognition](https://cognition.ai/blog/dont-build-multi-agents))
- **Coordination overhead grows super-linearly.** Anthropic's early agents spawned 50 subagents for simple queries, duplicated each other's searches, and needed extensive prompt-engineered guardrails; synchronous orchestration "creates bottlenecks" they hadn't solved. ([Anthropic](https://www.anthropic.com/engineering/multi-agent-research-system))

## 7. Distinctive ideas worth borrowing

1. **Artifacts as the communication bus (MetaGPT).** Agents don't chat; they produce typed documents (PRD, design doc, diff) written to a shared store, and other agents subscribe to artifact *types* via publish-subscribe. Reimplementable as: an orchestrator where every inter-agent message must be a schema-validated artifact persisted to disk/DB, with roles subscribing to artifact kinds. This makes runs auditable, resumable, and debuggable in a way message-passing never is.
2. **Communicative dehallucination (ChatDev).** Before acting on an instruction, the receiving agent restates its understanding and assumptions; the sender confirms or corrects. A two-line protocol addition (paraphrase-then-confirm) that measurably reduces hallucinated requirements in multi-agent handoffs.
3. **Teach the orchestrator to delegate with explicit task contracts (Anthropic).** Each subtask spec must contain: objective, output format, allowed tools/sources, and *clear boundaries vs sibling tasks* — vague delegation caused subagents to duplicate work. Pair with **effort-scaling rules in the prompt** ("simple → 1 agent, 3-10 tool calls; complex → 10+ subagents") so the orchestrator budgets tokens proportional to query complexity.
4. **The tool-description-improvement loop (Anthropic).** A meta-agent that takes a failing tool, exercises it dozens of times, and rewrites its description — yielded 40% task-time reduction. Generalizes to: agents that lint and improve their own tool/skill interfaces offline.
5. **Worktree-per-task as the universal isolation primitive (session managers).** One git worktree + branch per agent session gives free isolation, reviewable diffs, and clean discard semantics with zero agent-side changes. The entire parallel-session category is basically this one mechanism plus notification UI — proof that boring infrastructure beats clever agent protocols.
6. **Inline review comments routed back into the agent loop (vibe-kanban).** The human annotates the diff; the tool turns annotations into the agent's next prompt. Closes the review→revise loop without leaving the review surface — the highest-leverage UX idea in the category.
7. **Cloud sandbox-per-agent with live multiplayer observation (Conductor).** Each agent gets a microVM with deps pre-installed so it can run for hours unattended; teammates join a workspace link to watch/steer the same agent session in real time. Turns "my parallel agents" into a shared team surface.

## 8. Lessons for loongagent

**Copy**
- **Build orchestration as a control plane *above* agents, not inside them.** Session managers won by staying agent-agnostic adapters around first-party CLIs; role frameworks that hardwired agent internals (AutoGen v0.4, ChatDev v1) all got rewritten or abandoned. Loongagent's multi-agent layer should manage processes/worktrees/sandboxes and *review surfaces*, with per-agent executor adapters — exactly vibe-kanban's Rust-backend/executor-adapter split, but keep it maintained by making it core to the product.
- **Artifact-based coordination.** Skip agent-to-agent chit-chat; agents exchange typed, persisted artifacts over a publish-subscribe bus. Cheaper, auditable, resumable — and it feeds your cost-governance story (token accounting per artifact).
- **Delegation contracts + effort budgets.** Adopt Anthropic's subtask-spec fields and complexity-scaled agent counts as first-class config; expose token budgets per subtask. This is where loongagent's cost governance becomes a differentiator: no incumbent session manager tracks per-agent spend well.
- **Review-in-place UX.** Inline diff comments → agent prompt; run comparison (Crystal's idea) for "same task, two agents/models, pick the winner" — a natural fit for best-of-breed composition across surfaces.
- **Notifications/attention routing.** The scarce resource is human attention: "agent stopped / needs permission / hit limit" alerts, mobile supervision (Conductor, Paseo) matter more than another agent role.

**Avoid**
- **Hardcoded org charts.** CEO/CTO/QA role-play is prompt theater for coding tasks; MAST, Cognition, and Anthropic all converge: roles help only insofar as they encode *division of labor + boundaries*, and fixed chains break on real work. If you support roles, make them capability + permission scopes (which tools, which files, which budget), not personas.
- **Synchronous, single-repo parallel fan-out for coding.** "Most coding tasks involve fewer truly parallelizable tasks than research" (Anthropic); subagents writing to the same codebase produce merge conflicts and stylistic drift (Cognition). Default to a single-threaded primary agent; parallelize only at well-separated seams (independent modules, test-gen vs impl, research/exploration).
- **The maintenance trap.** Every wrapper in this category is one CLI update from a blank diff view; star counts hide abandonment. If loongagent ships adapters for other agents' CLIs, they need contract tests against CLI releases, or don't ship them.

**Do differently**
- **Solve the human-review bottleneck as the product.** Everyone acknowledges review is the ceiling at 3-5 agents; nobody has solved it. Opportunity: tiered review (auto-merge green checks, LLM-judge pre-review with rubric like Anthropic's citation/completeness judge, human only for architectural diffs), review queues ranked by risk, and per-repo "merge policy" config. The agent that makes 10 parallel sessions *reviewable* wins the category.
- **Semantic conflict handling.** Worktrees defer conflicts to merge time and current tools punt. A merge-time agent that understands both branches' intent (from their task specs/artifacts, not just diffs) and proposes reconciliations is unoccupied territory.
- **Learned/evolving orchestration as the long game.** ChatDev's own Puppeteer work (RL-trained orchestrator, NeurIPS 2025) and MetaGPT's AFlow (search over workflow graphs) point past hand-written topologies. Loongagent's backend control plane should log orchestration traces (who was spawned, with what spec, at what cost, with what outcome) from day one — that data is the prerequisite for learning better orchestration later.
