# 05 — Parity and synthesis: matching Claude Code, absorbing the field

- **Status:** Living checklist (design input — not an ADR)
- **Last updated:** 2026-10-05
- **Purpose:** turn two goals into checkable items — **"feature-wise, don't lose
  to Claude Code"** and **"absorb the best of the field (Pi, DeepSeek Harness,
  Codex, OpenCode, Amp, Cursor, Aider, Goose, Kiro)"**.

Legend: ✅ decided (ADR/topic) · ⚠️ partial · ❌ not covered yet · ⛔ deliberate no.
"Where" points at the ADR or topic that owns the item; "next" is the concrete
action.

Sources: [research/claude-code.md](research/claude-code.md) (CC feature
inventory, verified 2026-09-26), [research/dsh-plugin-ecosystem.md](research/dsh-plugin-ecosystem.md)
(DSH model and seams), the other 13 research notes, and the ADRs.

---

## A. Parity vs Claude Code

CC's own extension model — markdown content + shell/HTTP/MCP/LLM/subagent hooks
+ MCP servers + subagent definitions, bundled as "plugins" distributed from git
marketplaces — is **structurally the same shape we chose in ADR-0004**. So the
gap is not architecture; it is the **feature inventory and its polish**. The
rows below are what "not losing" means concretely.

### A1 · Agent loop and tools

| CC feature | Our status | Where | Next |
|---|---|---|---|
| Built-in Read / Write / Edit / Bash / Glob / Grep / WebFetch / WebSearch | ✅ | ADR-0004 (uniform tool interface) | ship in the first slice |
| **LSP-backed code intelligence** (go-to-def, type errors after edits) | ❌ | — | new **T-22** |
| Agent/Task (subagent spawn), AskUserQuestion, todo tracking | ✅ | ADR-0011, T-17, T-16 | — |
| **Monitor** (background script output surfaced as events) | ❌ | — | new **T-25** |
| Interrupt / mid-run steer / queue-follow-up | ✅ | T-12, ADR-0009 | — |
| Verification loop ("give it a check it can run") | ✅ | ADR-0006 retrieval + tools | make it a first-class pattern in prompts |

### A2 · Context management

| CC feature | Our status | Where | Next |
|---|---|---|---|
| Auto-compaction: **clear old tool outputs first, then summarize**; keep newest ~20k verbatim | ⚠️ | ADR-0006 (compaction visible) | pending amendment **E1** |
| `/compact <focus>`, partial summarization, `/rewind` (summarize from/up to here) | ⚠️ | ADR-0015 + T-12 give the substrate | define the command surface |
| `/context` — what occupies the window | ✅ | ADR-0006 (ledger) | render it |
| **Prompt-cache hit rate** in usage | ❌ | T-02 | new **T-25** |
| **`/btw`** — side question that never enters history | ❌ | — | new **T-25** |
| MCP tool deferral: names eagerly, schemas on demand | ✅ | T-15 | ship it |
| System-prompt discipline ("delete scaffolding every model generation") | ✅ | ADR-0004/vision | keep the deletion pass as a ritual |

### A3 · Memory and instructions

| CC feature | Our status | Where | Next |
|---|---|---|---|
| Instruction hierarchy: managed → user → project → local, most-specific last | ✅ | ADR-0008 | — |
| `@path` imports with depth limit | ❌ | — | small; fold into **T-25** |
| AGENTS.md native + CLAUDE.md compatibility | ✅ | ADR-0008 | importer |
| **Model-written auto memory** with bounded index + topic files | ⚠️ | ADR-0006 (propose-and-confirm), T-10 | define the bounded-index shape |
| `/init`, `/doctor` (generate / trim instruction files) | ❌ | — | new **T-25** |

### A4 · Permissions and sandbox

| CC feature | Our status | Where | Next |
|---|---|---|---|
| Permission modes (plan / manual / accept-edits / auto-classifier / bypass) | ✅ | ADR-0007 | auto-classifier = our Guardian, deferred to v2 |
| allow / ask / deny rule system + durable "don't ask again" | ✅ | ADR-0007/0008 | ship it |
| **Static analysis of shell commands against rules** | ❌ | ADR-0007 (rule-based PDP) | add to **T-11** amendments |
| OS sandbox: Seatbelt / Bubblewrap (+ network per-domain approval) | ✅ | ADR-0007 | — |
| Org-managed deny rules + managed instruction file | ✅ | ADR-0008 (org layer, signed packs) | — |
| Feedback on a denied approval ("comment on the decision") | ✅ | ADR-0007 (structured reason fed back) | — |

### A5 · Deterministic layer (hooks)

| CC feature | Our status | Where | Next |
|---|---|---|---|
| Lifecycle hook events (session start/end, **user-prompt submit**, pre/post tool use, **permission requested/denied**, subagent start/stop, **pre/post compaction**, file changed, cwd changed) | ✅ | ADR-0007 §7 (event set fixed 2026-10-05) | — |
| **Hook kinds: shell / HTTP / MCP tool** (LLM and subagent kinds deferred to v2) | ✅ | ADR-0007 §7, ADR-0004 | build the three transports |
| Exit-code-2 blocks and feeds stderr back to the model | ✅ | ADR-0007 | — |
| Repo-shipped hooks are untrusted | ✅ | ADR-0007 (explicit trust) | — |

### A6 · Skills, commands, plugins

| CC feature | Our status | Where | Next |
|---|---|---|---|
| `SKILL.md` + frontmatter, description-only in context, body on invoke | ✅ | T-18, ADR-0006 (progressive disclosure) | — |
| **Commands merged into skills** (one concept) | ⚠️ | T-16 vs T-18 separated | pending amendment **E3** |
| Bundled workflow skills (`/code-review`, `/batch` fan-out to worktree agents opening PRs, `/verify`, `/run`) | ⚠️ | ADR-0011 + T-20 + T-17 | define the `/batch`-style workflow explicitly |
| Plugins = bundles of skills + hooks + subagents + MCP servers | ✅ | ADR-0004 + ADR-0008 packs | same shape; content bundles |
| Git-based plugin marketplaces | ⏸ | ADR-0008 signed packs | internal catalog later (not v1) |

### A7 · MCP

| CC feature | Our status | Where | Next |
|---|---|---|---|
| stdio + HTTP servers, user/project/local scopes | ✅ | T-15 | ship it |
| **Org allow/deny lists for MCP servers** | ⚠️ | T-05/T-15 (approved-server gateway) | make it a first-class org policy key |
| MCP prompts surface as slash commands | ❌ | T-15 | small; fold into T-15 |
| Lazy tool loading (names first, schemas on demand) | ✅ | T-15 | ship it |
| Injection defence on MCP content | ✅ | ADR-0007 + T-15 | — |

### A8 · Subagents and workflow modes

| CC feature | Our status | Where | Next |
|---|---|---|---|
| Subagents defined as markdown, own prompt / tools / model / memory | ✅ | T-17, ADR-0011 (model routing per subagent) | define the markdown format |
| Separate context, summary-only return | ✅ | ADR-0011 | — |
| Built-ins: Explore (read-only, cheap), Plan, general | ✅ | T-17 | — |
| Description token budget warning | ❌ | T-15/T-17 | small; fold in |
| Plan mode | ✅ | ADR-0004 (first-party battery) | — |
| **`/goal`: persistent completion condition + per-turn evaluator + Stop-hook gating (8-block circuit breaker)** | ❌ | — | new **T-23** |

### A9 · Models, SDK, surfaces, sessions

| CC feature | Our status | Where | Next |
|---|---|---|---|
| Mid-session model switch, effort levels | ⚠️ | ADR-0005/0008: switching allowed **within the profile's allowed set** | state the stance explicitly |
| Subagent-specific model | ✅ | T-17 + ADR-0005 | — |
| SDK (Python + TypeScript) over the same loop | ✅ | ADR-0002 (JSON-RPC + schema) | generate bindings |
| Headless / print mode / CI | ⚠️ | ADR-0009 (not a product surface; internal headless mode) | — |
| Surfaces: CLI, VS Code/Cursor, JetBrains, Desktop, web, mobile, Slack, GH Actions | ⚠️ | ADR-0009: TUI → web → ACP; desktop/CI dropped; Slack/GH Actions not planned | ACP buys the IDE half |
| Session resume / fork / name; JSONL on disk | ✅ | ADR-0003/0015 | — |
| **Session export/import across machines** | ✅ | ADR-0014 | **CC does not have this** (see C) |

---

## B. Absorption list (博采众长)

| # | Item | From | Our status | Where it lands |
|---|---|---|---|---|
| B1 | Hook kinds **shell / HTTP / MCP tool** + full lifecycle event set (LLM and subagent kinds → v2) | CC | ✅ applied 2026-10-05 | ADR-0007 §7, ADR-0004 |
| B2 | Commands merged into skills | CC | ⚠️ two concepts | **E3** (T-16/T-18) |
| B3 | Auto-compaction heuristic (clear tool outputs → summarize; keep newest ~20k) | CC, Pi | ⚠️ | **E1** (ADR-0006) |
| B4 | `/goal` persistent completion + evaluator + turn-end gating | CC | ❌ | **T-23** |
| B5 | LSP code intelligence tools | CC, OpenCode | ❌ | **T-22** |
| B6 | Module declares its config schema; shells render the settings UI | DSH | ❌ | **T-24** |
| B7 | Module hot reload (authoring loop) | DSH, Pi | ❌ | **T-24** |
| B8 | `ignorable` event envelope (unknown event types are safe to ignore) | DSH | ❌ | **E4** (ADR-0003) |
| B9 | Capability-seam naming discipline (Service Definition pkg + provider pkgs) | DSH | ✅ concept | ADR-0004 wording |
| B10 | Monotonic denial (`guard`) | DSH, CC | ✅ | ADR-0008 |
| B11 | `/btw`, Monitor, cache-hit stats, `/init`, `/doctor`, `@path` imports, description budgets | CC | ❌ | **T-25** |
| B12 | Skills/commands hot reload for skills authors | Pi | ❌ | **T-24** |
| B13 | Multi-kind hooks + deterministic-enforcement framing | CC | ✅ (3 kinds) | ADR-0007 §7 |
| B14 | Sandbox self-test before first tool call | Codex | ✅ | ADR-0007 |
| B15 | Capability dial instead of model picker; silent re-route as frontier moves | Amp | ✅ | ADR-0005 + ADR-0008 profiles |
| B16 | Context ring from ledger data | Cursor | ✅ | ADR-0006 |
| B17 | Capability registry as pure data | Aider | ✅ | ADR-0004 |
| B18 | Uniform tool interface incl. built-ins | Goose | ✅ | ADR-0004 |
| B19 | Spec-first with size-scaled ceremony (never spec-as-source) | Kiro | ✅ | T-20 |
| B20 | Two-phase sandboxed environments (setup with secrets, then locked-down agent phase) | Codex | ⏸ | relevant when/if runners return |
| B21 | `/batch`-style fan-out: N worktree subagents each opening a PR | CC | ⚠️ | ADR-0011 + T-20 |

---

## C. Where we are deliberately stronger

| # | Property | Why CC/DSH structurally can't match it |
|---|---|---|
| C1 | **Cost governance as policy**: profiles, local-decides routing, org hard caps enforced as `deny`, escalation as auditable events | CC has subscription windows and `/usage`; DSH leaves budgets to plugins |
| C2 | **Org policy distribution**: signed packs, monotonic layers, org > project > user | CC has managed settings; DSH has none |
| C3 | **Control plane for aggregate audit/statistics** (opt-in, allow-listed uplink) | CC is per-user; DSH has no control plane |
| C4 | **Model-agnostic by design**: any provider, any local model, no vendor lock | CC is Claude-first; DSH is DeepSeek-first |
| C5 | **Session portability**: export/import bundles with ancestry, approvals never travel (ADR-0014) | CC has local JSONL but no first-class portable bundle |
| C6 | **Self-hosted everything**: managed or on-prem same code, including the control plane | CC is SaaS-bound for org features |

---

## D. Deliberate "no"s (kept together so they don't get re-litigated)

| # | Not doing | Why |
|---|---|---|
| D1 | In-process **third-party** code in v1 | Trust boundary; revisit with a provenance tier when outsiders actually arrive (ADR-0004) |
| D2 | Logic-bearing **client UI plugins** | Thin clients, zero agent logic (ADR-0009). *Our own* declarative panels are in scope — see **T-24** |
| D3 | Multi-user shared live sessions | ADR-0010 |
| D4 | Session **sync** service (backend holds session state) | ADR-0012; file-based export/import instead (ADR-0014) |
| D5 | Cross-user / scheduled orchestration, cloud runners | No use case yet; local worktrees only (ADR-0011) |
| D6 | Scheduler for local agents | One orchestrator: the parent's plan (ADR-0011) |
| D7 | Editor fork; desktop app; CI surface | ADR-0009 |
| D8 | Vector DB in v1 | Structural retrieval first (ADR-0006/T-15) |
| D9 | Marketplace in v1 | Signed internal catalog later; git-URL installs at project/user layers |

---

## E. Amendments

### Applied

- **E2 · ADR-0007 §7 / ADR-0004** — ✅ **2026-10-05, narrowed**: hooks now have
  **three v1 kinds** (shell / HTTP / MCP tool) behind one verdict contract, plus
  a fixed lifecycle event set (session start/end, user-prompt submit, pre/post
  tool use, permission requested/denied, subagent start/stop, pre/post
  compaction, file changed, cwd changed). **LLM and subagent hooks are deferred
  to v2** — both spend tokens and the second recurses, so they belong with the
  Guardian reviewer and the goal evaluator (T-23). HTTP hooks are the one
  explicit exception to "nothing leaves the machine": opt-in, org-pinnable, a
  network action class under the PDP.

### Pending (need owner sign-off)

- **E1 · ADR-0006**: add the compaction heuristic — clear stale tool outputs
  first, then summarize; keep the newest ~20k tokens verbatim; compaction stays
  a visible event.
- **E3 · T-16/T-18**: **merge commands into skills** (one concept, one loader,
  one frontmatter) — CC already did this; carrying two concepts costs more than
  it buys.
- **E4 · ADR-0003**: add the **forward-compatibility envelope** — an explicit
  `ignorable` marker so a reader can safely skip unknown event types. DSH
  plugins failed closed without it, and our own modules will version
  independently too.

---

## F. New topics created from this checklist

| Topic | What |
|---|---|
| **T-22** | LSP-backed code intelligence tools (definitions, type errors, references) |
| **T-23** | Persistent goal: completion condition + per-turn evaluator + turn-end gating with a circuit breaker |
| **T-24** | Module metadata and lifecycle: declared config schema rendered by shells, module hot reload, declarative panels (our own surfaces) |
| **T-25** | Interaction smalls: `/btw`, Monitor, prompt-cache stats, `/init`, `/doctor`, `@path` imports, description token budgets |

---

## How this document gets used

1. **A** is the parity ledger: anything still ❌/⚠️ is either scheduled, or it
   moves to **D** with a reason.
2. **B** is the absorption backlog; items with an **E** reference need an ADR
   amendment first.
3. **C** is what we lead with in positioning and design reviews — the firm-facing
   half that CC and DSH structurally cannot serve.
4. This checklist is *not* the roadmap; it feeds `02-topics.md` and the ADRs.
   Nothing here is "Agreed" until the owner signs off in conversation.
