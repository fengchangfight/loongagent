# 01 — Landscape Synthesis: What the Best Coding Agents Teach Us

- **Status:** Draft (basis for brainstorming round 1)
- **Last updated:** 2026-09-26
- **Sources:** `research/claude-code.md`, `research/codex.md`, `research/opencode.md`,
  `research/cursor.md`, `research/pi.md`, `research/deepseek-harness.md`,
  `research/other-agents.md` (all with inline citations)

This document compresses ~1,300 lines of research into: (A) a feature matrix,
(B) the design **tensions** where tools contradict each other and we must pick a
side, and (C) a strawman "best-of" blueprint to argue against.

---

## A. The landscape in one table

| | **Claude Code** | **Codex (OpenAI)** | **OpenCode** | **Cursor** | **Pi** | **Aider** |
|---|---|---|---|---|---|---|
| Core bet | Minimal shell, maximal model | Harness as separable product; local+cloud converge | Open client/server, model = swappable backend | Fork the IDE, rebuild around AI | Tiny observable harness; extend it yourself | Model-adaptive harness; git as safety net |
| Surfaces | CLI flagship + IDE/desktop/web/Slack/CI | CLI + IDE + app + cloud + mobile + Agents API | TUI + desktop + IDE + HTTP server + CI | IDE flagship + CLI + web + cloud agents | TUI + headless + RPC + SDK | CLI |
| Permissions | Trust ladder + OS sandbox + classifier auto-mode | Sandbox mode × approval policy (2 axes), Guardian LLM reviewer | Glob rules only; **no sandbox** | Per-file approve; sandboxed terminal; cloud = own VM | **None** (YOLO by philosophy) | None (git is the net) |
| Context mgmt | Progressive disclosure everywhere; compaction; auto-memory | Middle-truncation (weakness); AGENTS.md chain w/ byte cap | Lean (~7k tok overhead); AGENTS.md + CLAUDE.md fallback | Context ring UI; rules decay under pressure | <1.4k tok prompt; tree sessions; lossless JSONL | Repo map (tree-sitter + PageRank, token-budgeted) |
| Undo | Checkpoints + `/rewind` (conv+code) | Rollouts; `codex apply` | `/undo` per-message git snapshots | Checkpoints (files only) | `/tree`, `/fork`, branch summaries | Auto-commit per change; `git revert` |
| Subagents | First-class, separate context, return summaries | Multi-Agent V2, message board | build/plan primaries + child sessions | Explore subagent; up to 8 parallel (worktrees) | **Refused** (tmux/extensions instead) | None |
| Extensibility | Hooks (deterministic), skills, plugins, MCP, SDK | MCP, skills, hooks, plugins, JSON-RPC app-server | Plugins veto tool calls; commands/agents as markdown | MCP scoped per-agent; rules marketplace | In-process TS extensions + `/reload` (agent edits itself) | Config + convention files |
| Models | Claude only (+ third-party hosted Claude) | OpenAI only, co-designed with harness | 75+ providers via models.dev | Multi-model + own Tab/Apply/Composer models | 15+ providers, mid-session switching | 100+ via litellm, per-model settings registry |
| Notable weakness | Cost opacity, compaction rot, closed source | Over-eager scope, middle truncation, config churn | No sandbox, privacy surprises, slower | Pricing trust crisis, Electron perf, agent overreach | DIY security, steep curve, churn | Manual context, no subagents/sandbox |

**Meta-lesson (the good news):** the scaffold is the product. The same model
swings 20+ SWE-bench points between harnesses. Everything distinctive in this
landscape — repo maps, edit formats, trust ladders, critics, session trees,
sandboxes — is *harness engineering*, which is exactly what we're building.

---

## B. The design tensions (where we must choose, not combine)

These are the genuine contradictions. For each: the two poles, who's right about
what, and the recommended trade-off (**T1–T10** will become ADRs once decided).

### T1. Minimal core vs. batteries included
- **Pi:** 4 tools, ~1k-token prompt; "models are RL-trained to be coding agents;
  big harnesses inject hidden context and churn." Measured: 1.4k vs 31k tokens
  for the same prompt; near top of Terminal-Bench 2.0 *without compaction*.
- **Claude Code / Cursor:** rich scaffolding (plan mode, subagents, skills,
  rules) demonstrably helps long tasks; but users pay in token bloat and churn.
- **Telling fact:** Pi's most-installed community packages rebuild exactly what
  core refused (MCP adapter, subagents) — the batteries boomerang.
- **Recommended trade-off:** *layered architecture.* A Pi-grade minimal,
  fully-observable core (few tools, tiny prompt, documented session format) with
  an **official, versioned "batteries" layer** (plan mode, subagents, skills,
  MCP) that is visible in the context budget and individually disable-able. We
  get Pi's inspectability without its onboarding cliff.

### T2. Single process vs. client/server
- **Pi:** one process, no daemon; server model criticized as complexity.
- **OpenCode / Codex:** everything is a client of an OpenAPI/JSON-RPC server;
  TUI, IDE, web, desktop, CI all come *free* from that decision. Codex lesson:
  "cheap to do from day one, painful to retrofit."
- **Our ambition (all surfaces) forces this one.** **Recommended: client/server
  core from day one** — an event-sourced agent server with a versioned API; TUI
  as just another client. Pi's single-process simplicity is preserved *inside*
  the server (the session core remains a boring in-process library).

### T3. Permissions: dialogs vs. sandbox vs. nothing
- **Pi:** "permission prompts are theater; everyone runs YOLO anyway."
- **Codex:** two orthogonal axes — OS-enforced sandbox (Seatbelt/Landlock) ×
  approval policy; plus a `sandbox self-test` command; Guardian = an LLM that
  reviews planned actions semantically.
- **Claude Code:** graduated ladder; needed an ML classifier because "after the
  tenth approval you're clicking through, not reviewing."
- **Consensus from the research:** Pi is right that dialogs don't scale, wrong
  to conclude "ship nothing." **Recommended: OS sandbox by default
  (seatbelt/bubblewrap/micro-VM), YOLO as opt-out; approval policy as a separate
  axis; invest later in a classifier/Guardian-style auto-approver.** Sandbox is
  what makes high autonomy *safe*, which is what makes users stop clicking.

### T4. One tuned model vs. model-agnostic
- **Claude Code / Codex:** harness co-designed with one model family; quality
  peak, total lock-in (Anthropic cut OpenCode off overnight — vendor risk is real).
- **OpenCode / Pi / Aider:** provider freedom; but "silent breaks at the
  model/harness seam" — DeepSeek subagent spawns 400, Kimi rejects document
  blocks, weak models drown in context, edit formats fail 30% of the time.
- **DeepSeek research's key insight:** model swaps fail at the *seams*
  (subagents, PDFs, thinking params), not the center.
- **Recommended trade-off (core to our vision):** model-agnostic with a
  **per-model capability registry** (Aider-style: context budget, edit format,
  tool-calling reliability, thinking support) + a **startup probe layer** that
  feature-detects the endpoint and scales harness behavior down for weak models
  (small repo map, whole-file edits, no parallel tools). Frontier models get the
  full harness; open models get a harness that fits them. This registry is a
  genuine differentiator nobody has done well.

### T5. Rules in the prompt vs. enforcement in code
- Every tool ships markdown rules (CLAUDE.md / AGENTS.md / .cursor/rules); every
  tool's users report **rules get ignored at length** ("edited forbidden files
  despite 30 pages of rules"; "rules decay after ~an hour").
- **Claude Code's answer:** hooks — deterministic interceptors whose exit code 2
  blocks the action *and feeds the reason back to the model*. OpenCode: plugins
  veto at the tool boundary.
- **Recommended: two layers, documented as such.** Advisory layer (rules files,
  hierarchical, lazy-loaded) + guarantee layer (hooks/middleware on tool
  boundaries). Anything compliance-critical must live in the guarantee layer.

### T6. Subagents vs. "black box within a black box"
- **Claude Code/Cursor/Amp:** subagents with isolated contexts returning
  summaries — the feature that makes long tasks tractable; Cursor's Explore
  subagent replaced their embedding-RAG entirely.
- **Pi:** refuses them; spawn separate sessions, communicate via file artifacts;
  "mid-session subagents are a sign you didn't plan ahead." Community data cuts
  both ways (subagents "go wild producing comprehensive reports").
- **Recommended trade-off:** first-class subagents, but **observable and
  artifact-based**: each subagent writes a structured report file (its "summary"
  is a real artifact, linkable, auditable), runs in the session tree as a
  visible branch, and inherits the parent sandbox. And keep Pi's escape hatch:
  fresh-session-with-artifact is a supported, documented pattern.

### T7. Retrieval: embeddings-RAG vs. repo map vs. agentic search
- Cursor **quietly abandoned** embedding-RAG for "Instant Grep" + an Explore
  subagent ("don't build a vector DB until grep fails you").
- Aider's tree-sitter + PageRank **repo map** (token-budgeted) is deterministic,
  cheap, debuggable — but must be disabled for weak models.
- **Recommended: layered retrieval** — fast local grep/symbols (always) +
  budgeted repo map (model-permitting) + explore subagent for broad questions.
  No vector DB in v1.

### T8. Sync pairing vs. async delegation
- The field splits: collaborative transparency (Aider, Cline approve-every-step)
  vs. assign-a-ticket-get-a-PR (Devin, Jules, Copilot coding agent, Codex cloud).
- Codex's bet (and 2026 reality): **these converge** — same agent, same account,
  real-time locally and async in the cloud; worktree/container per agent;
  identity (not file sync) is the coordination mechanism.
- Governance defaults matter for async: requester-can't-approve-the-PR, egress
  allowlists, draft-PR-only (GitHub got this right).
- **Recommended: sync-first architecture, async-ready.** The event-sourced
  server (T2) + worktree isolation + PR-as-deliverable interface means async
  cloud workers are a deployment of the same agent, not a second product. v1
  local; v2 add cloud runners with Copilot-style governance defaults.

### T9. Checkpoints: git-native vs. snapshot system
- **Aider/OpenCode:** git is the undo layer — auto-commit or per-message
  snapshots; zero new UX, works offline.
- **Cursor/Claude Code:** custom checkpoints orthogonal to git (files-only or
  files+conversation rewind).
- **Pi:** the session itself is a tree in one JSONL file — fork/branch/rewind
  the *conversation*; git handles the files.
- **Recommended trade-off:** **git for files** (auto-snapshot commits on a
  shadow branch, per message) + **tree-structured session log** (Pi's design:
  id/parentId, fork, branch summaries, lossless compaction). Don't build a
  third snapshot system; compose these two.

### T10. Cost transparency
- Universal pain: Claude Code limit opacity, Cursor's pricing fiasco ("I no
  longer trust Cursor"), Cline's $50–200/day surprises, Codex middle-truncation
  hiding waste.
- **Recommended (no real tension, just do it):** live per-turn token/cost meter,
  session budget caps, cache-hit telemetry, and a Cursor-style context-ring
  breakdown (system prompt / tools / rules / MCP / history) on every surface.
  Cheap to build early, fatal to retrofit trust.

---

## C. Strawman blueprint ("loongagent v0") — to be argued against

**One brain, many faces, sized to the model in front of it.**

1. **Core** — an event-sourced agent server (typed append-only event log à la
   OpenHands; OpenAPI-described à la OpenCode/Codex). Everything — tool call,
   approval, compaction, cost — is an event. Replay, resume, audit, multi-UI
   for free.
2. **Minimal loop inside** — Pi-grade: 4-6 tools (read/write/edit/bash/grep/
   glob), tiny system prompt, no hidden context. Everything else is a
   *visible, budgeted layer* on top (T1).
3. **Sessions as trees in JSONL** (Pi) + **shadow git snapshots for files**
   (Aider/OpenCode) (T9).
4. **Trust** — OS sandbox default-on; approval policy as separate axis;
   hook/plugin interception at the tool boundary as the guarantee layer (T3, T5).
5. **Model layer** — provider abstraction over the ~4 wire protocols (Pi's
   insight: 4 protocols cover ~30 providers); per-model capability registry +
   startup endpoint probe; scenario routing (planner/worker/editor/reviewer can
   be different models); live cost meter (T4, T10).
6. **Retrieval** — grep/symbols + token-budgeted repo map + explore subagent;
   no vector DB (T7).
7. **Surfaces** — TUI first (native scrollback, steer/follow-up queues), then
   VS Code extension and web UI as *clients of the same server*. Desktop app
   = web UI in a shell, later. **No IDE fork** — Cursor paid for full control
   with permanent marketplace/exile costs; a plugin over ACP gets 90% (T2).
8. **Async** — worktree-per-agent locally; cloud runners later with
   governance-by-default (draft PRs, requester can't approve, egress allowlist)
   (T8).
9. **Roles beyond "developer"** — *our blue-ocean bet, see below.*

### The role angle (nobody does this well)
Every tool in the landscape targets "software developer writing application
code." Our vision (DevOps, QA, data engineers, ...) maps naturally onto
existing primitives:
- **Role profile = tool pack + permission profile + rules pack + context
  sources.** e.g. a `devops` profile adds kubectl/terraform tools, denies
  production writes by default, loads runbook rules; a `qa` profile adds
  browser/test-runner tools and screenshot verification; a `data` profile adds
  SQL/notebook tools and schema context.
- Roo Code's custom modes proved users want this; nobody made it first-class
  and cross-surface.

---

## D. What we deliberately do NOT copy

- Cursor's backend-mediated prompt assembly (even BYOK routes through their
  servers) and OpenCode's phone-home defaults — **local-first must mean fully
  local**; any cloud call is per-feature opt-in.
- Daily-release churn without stability channels (Claude Code, Codex, Pi,
  OpenCode all burn users here) — stable/LTS channel + treat token-cost-affecting
  changes as breaking changes.
- Devin-style autonomy hype — scope claims honestly ("verifiable tasks in
  well-tested repos") like Copilot does.
- Middle-truncation of tool outputs (Codex) — full output to file, model pages it.

## E. Open questions for brainstorming round 1

1. Implementation language for the core server? (Rust = Codex's robustness/single
   binary; TypeScript/Bun = Claude Code/OpenCode/Pi ecosystem + self-modification;
   Go = Amp/OpenHands-CLI middle ground.) This decision constrains everything.
2. TUI framework strategy if TS (OpenTUI?) vs. if Rust (ratatui)?
3. Do we adopt **AGENTS.md as the native rules format** (cross-vendor standard)
   with CLAUDE.md/.cursor import?
4. How much async/cloud is in v1 scope vs. architecture-only?
5. Which role profiles ship first? (suggest: devops + qa, biggest gap)
6. Name?
