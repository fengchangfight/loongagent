# Decided topics — safety, people and extension

- **Up:** [README.md](README.md)
- **Authority:** [02-topics.md](../02-topics.md)
- **Code:** `docs/topics/decided-extension.md`
- **Verify:** `python3 scripts/check-docs.py`

These topics are settled: `Promoted → ADR-NNNN` names the decision, and a
`Merged → T-NN` topic names where it went. The full text is kept, heading and
all, so a `T-NN` citation still resolves. This file holds the safety, people and
extension decisions; [decided.md](decided.md) holds model, cost and observability,
and [decided-platform.md](decided-platform.md) holds the control plane and
deployment. The index of all 26 is [README.md](README.md).

## T-09 · Hooks & deterministic enforcement — `Promoted → ADR-0007`

Seeded 2026-09-30. **Veto + trust gate decided 2026-10-04 (ADR-0007):** hooks
advise/veto at the PDP (never *are* the decider), their reasons feed back to the
model, and repo-shipped hooks are untrusted content requiring explicit,
content-bound per-repo trust opt-in. Still open: hook language (shell vs
in-process), sync vs async, and the trust UX.

**Decided 2026-10-05 (ADR-0007 §7, narrowed):** hooks support **three kinds** in
v1 — **shell / HTTP / MCP tool** — behind one verdict contract (allow / ask /
deny + reason), over a fixed lifecycle event set (session start/end,
user-prompt submit, pre/post tool use, permission requested/denied, subagent
start/stop, pre/post compaction, file changed, cwd changed). **HTTP hooks are
the one explicit exception to "nothing leaves the machine"**: opt-in,
org-pinnable, and a network action class under the PDP. **LLM and subagent hook
kinds are deferred to v2** (they spend tokens / recurse — they belong with the
Guardian reviewer and the T-23 goal evaluator). Still open: hook language for
repo-shipped hooks in practice, sync vs async, and the trust UX.

The guarantee layer where markdown rules are only advisory (synthesis T5).
Lifecycle events (session start/end, pre/post tool use, compaction, permission
decisions) fire hooks that can block, mutate, or enrich — Claude Code's
exit-code-2 pattern (block + feed reason back to the model), OpenCode's
plugin veto at the tool boundary, Kiro's git-committed agent hooks ("CI for
agent behavior"). Open questions: hook language (shell vs in-process TS/WASM),
sync vs async, trust gating (repo-shipped hooks caused Claude Code CVEs —
untrusted repos must never auto-execute).

## T-10 · Memory system — `Promoted → ADR-0006`

Seeded 2026-09-30. **Write path decided 2026-10-04 (ADR-0006):**
agent-proposed into a staging area; low-risk project facts may auto-accept;
user preferences/feedback require explicit confirmation; memories are ledger
entries and writes are events (auditable, revertible). The rest of the topic
(what earns memory, decay/TTL, scoping, cross-session memory for async
workers) is still open.

Dual authorship: human-written rules (versioned in git, team-shared) vs
agent-written memory (typed notes: user/feedback/project/reference) with a
bounded index loaded each session + topic files read on demand (Claude Code's
design); Cursor's sidecar-observer proposes memories with user confirmation.
Requirements for us: memory must be inspectable/editable/deletable (T-13 —
nothing hidden), scoped (user/project/org), and git-native where team-shared
(WrenAI lesson: org context as PR-reviewable artifacts). Open: what earns
memory, decay/TTL, cross-session memory for cloud workers.

## T-11 · Permissions & sandbox — `Promoted → ADR-0007`

Seeded 2026-09-30. **Engine decided 2026-10-04 (ADR-0007):** PDP inside the
fixed kernel at the single tool chokepoint (allow/ask/deny + the rule that
fired), structural PEPs (OS sandbox, tool runner, network egress), layered
policy with org packs winning, per-action-class autonomy ladder, sandbox
default-on with approval policy orthogonal and YOLO a visible opt-out,
approvals as first-class events with scope + structured reasons, Guardian-style
LLM pre-review deferred to v2. Still open: rule/policy language and pack schema
(AQ-9), hook trust UX, network allow-list granularity, micro-VM tier.

Original seed — per synthesis T3 (direction agreed): OS sandbox default-on
(seatbelt/bubblewrap/micro-VM), approval policy as a separate axis, YOLO as
opt-out. Extras from research: Codex's `sandbox self-test` command;
Guardian-style semantic approval reviewer (later); OpenCode's doom-loop and
external-directory guards as ordinary permission rules; role-specialized
lesson — autonomy allocated **per action** (investigate free, write gated),
varying by role profile. Replit Lemkin incident is the canonical warning:
prompts are not permission systems; safety must be structural.

## T-14 · Teamwork & sharing — `Promoted → ADR-0010`

Seeded 2026-09-30. Extends T-03. **Rescoped 2026-10-04 (ADR-0010):**
collaboration = **shared context and knowledge via git** (AGENTS.md-style,
PR-reviewed, plus signed org knowledge packs per ADR-0008) — **not** shared live
sessions. No multi-user co-watching, co-steering, or cross-user session viewing
for now; persona handoffs travel as repo artifacts (a spec doc, not a live
session). Shared knowledge is content, never policy, so it cannot widen
permissions or budgets (ADR-0007). Sessions, personal memory, and raw
transcripts stay personal; sharing is an explicit human act. Still open:
knowledge artifact granularity and attribution, personal cross-device session
sync, org knowledge delivery (pack vs mirrored git repo), and whether any
read-only session viewing ever returns.

Session/thread sharing as durable artifacts (Amp threads: shareable URLs,
"show how the agent reached the result"); team rules/skills/memory via git;
**persona handoffs** — PM's spec artifact becomes the engineer's session
input (T-06); live co-watch and human takeover (Devin); review-in-place for
parallel agents (multi-agent-orchestration research: human review is the
3–5-agent ceiling — tiered review is our opening). Privacy: sharing is
opt-in per artifact (OpenCode's auto-sync backlash).

## T-16 · Commands — `Merged → T-18` (2026-10-05)

Seeded 2026-09-30. **Decision (2026-10-05, §E3 of
[05-parity-and-synthesis](../05-parity-and-synthesis.md)): commands are not a
separate concept — a command *is* a manually-invoked skill.** Claude Code
already merged them (`.claude/commands/*.md` still load, as skills). One loader,
one frontmatter contract, one directory convention; a legacy commands directory
is read as skills for compatibility. The merge contract lives in **T-18**; this
entry is kept only as the record of the merge.

Slash commands as markdown prompt templates with frontmatter (Pi/OpenCode):
`$ARGUMENTS`, shell-output injection, `@file` inclusion, `subtask: true` to
run in a subagent keeping main context clean. Claude Code's evolution note:
custom commands merged into skills — decide whether our commands and skills
(T-18) are one mechanism or two. Layers: built-in < team < user.

## T-17 · Subagents — `Promoted → ADR-0011`

Seeded 2026-09-30. **Execution model decided 2026-10-04 (ADR-0011):**
write-capable subagents run in their own **git worktree** (read-only ones share
the workspace); each is a **child session** (delegation — ADR-0015) returning a
**structured artifact** (files + summary), not a transcript; there is **one
orchestrator — the parent's visible plan**, no scheduler; concurrency is bounded
by human review attention (**3–5**, tiered review over the user's own agents).
Still open: artifact/summary schema, worktree lifecycle & GC,
external-resource declaration.

Per synthesis T6 (direction agreed): first-class but **observable and
artifact-based** — each subagent is a visible **child session** (T-12/ADR-0015),
writes a structured report artifact, returns only a summary, inherits
the parent sandbox. Built-ins: explore (read-only, fast model), plan,
general. Per-subagent model routing (T-01: cheap explore, strong build).
OpenCode trick: denied subagents are removed from the task tool's description
so the model never attempts them.

## T-18 · Skills & marketplace — `Promoted → ADR-0018`

Seeded 2026-09-30. **Commands merged in (2026-10-05, T-16 → here):** a
**command is a manually-invoked skill**, not a separate concept — one loader,
one frontmatter contract (`name` / `description` / `whenToUse` +
invocation flags such as model-invocable vs user-only), one directory
convention; a legacy `commands/` directory is read as skills for
compatibility. Two loaders and two frontmatter contracts cost more than they
buy, and Claude Code already converged on the single concept.

**Marketplace decided 2026-10-15 (ADR-0018):** content is installed from a git
checkout by name through a curated catalogue in the binary — there is no network
marketplace index, only skills are installed (hooks and MCP servers stay
per-repository trust gates), and a plugin may seat named skill bodies in the
system prompt as `bootstrap`. Still open: the firm-internal registry, trust
tiers/signatures for third-party content, and org-approved distribution.

Agent Skills open standard (Anthropic): description-only in context until
invoked; skills bundle files/scripts and (Amp-style) their own MCP config.
**Marketplace**: firm-internal registry first (org-approved, versioned,
auditable — Continue's "hub" concept is ownerless and ours to take); public
registry later. Trust: skills can carry code → same trust gating as hooks
(T-09). Tessl's post-pivot registry is proof the concept has legs.

## T-19 · IDE integration via ACP — `Promoted → ADR-0009` (direction agreed in round-2 research)

Seeded 2026-09-30. **Surface decision recorded 2026-10-04 (ADR-0009):** ACP is
the third build target (after TUI and web), served by `loongagent acp` as a
dialect of our own JSON-RPC protocol (ADR-0002); editor-side plugins are thin
wrappers, never parallel products, and no capability may be implemented
IDE-side only (exposure may lag, visibly). Still open: the branded VS Code
extension's scope and the relay/editor context-source details.

**ACP server-side from day one; no editor fork, no per-editor plugins.**
Zed, JetBrains, Neovim, Emacs + 25 agents adopted ACP; Void died, Continue
was acqui-hired — thin layers on others' platforms die. The editor is also a
context *source* (open files, selection, diagnostics — Windsurf's flow
awareness, scaled to taste). JetBrains lesson: a mature semantic index beats
text search inside IDEs — our repo map (T7) plugs in here.

## T-21 · Layered approachability: junior-proof defaults, senior-proof depth — `Promoted → ADR-0007, ADR-0008`

Seeded 2026-09-30. **Decided:** junior-proof defaults come from the per-action
autonomy ladder (ADR-0007 §4); every layer is openable through monotonic config
layers that a lower layer cannot loosen (ADR-0008); and YOLO is a visible,
recorded opt-out (ADR-0007 §5). Self-modification covers the extension surface —
skills, commands, rules, plugins — not the kernel (ADR-0013 §5).

Owner: avoid Pi's steep learning curve. Pi converts experts but "has no
guardrails to catch" newcomers. Resolution: **defaults carry juniors**
(batteries on, sandbox on, curated profiles, guided first session — T-06
newbie persona) while **every layer is openable for seniors** (extensions,
raw config, self-modification à la Pi's `/reload`). The harness is pluggable
with good defaults — "nobody knows the right shape yet," so both we and our
users must be able to re-shape it cheaply. Onboarding is a designed
progression, not a cliff.
