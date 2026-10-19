# ADR-0011 — Local parallelism: isolated writers, artifact-returning branches, one orchestrator

- **Status:** Accepted
- **Up:** [../design/README.md](../design/README.md)
- **Date:** 2026-10-04
- **Decides:** AQ-10 (docs/03-architecture.md) — local parallelism. Also records
  the scope narrowing: cross-user, scheduled, and cloud-runner orchestration are
  out of scope until a concrete use case appears

## Context

Parallel agents are a headline capability: T-17 makes subagents first-class,
T-12 makes the session a tree, T-20 wants PR-shaped deliverables, and the
round-2 multi-agent research gave us the durable lessons — the human review
ceiling sits at 3–5 parallel agents, artifacts (not chat) are the real
coordination medium, and orchestration logic should not be hardcoded inside
agents.

Two things narrowed the problem before this decision. ADR-0010 removed
multi-user shared sessions, which had been the collaboration driver for
"async". The owner then dropped cross-user and scheduled orchestration outright
(2026-10-04) for lack of any application scenario. What remains is **one user's
session fanning out across a handful of subagents** — small enough that the
interesting risks are not scheduling efficiency but **trust**:

- Concurrent writers in one working tree silently clobber each other, and worse,
  invalidate each other's **verification** — a test run that passes or fails
  against a half-finished sibling's edits is a false conclusion the agent then
  reasons from.
- Orchestration can hide. A component that decides the plan on its own becomes a
  second decision-maker whose reasoning, retries, and spend are invisible —
  exactly what ADR-0004 and T-13 forbid.
- Subagent transcripts re-entering the parent context blow the budget that
  ADR-0006 exists to protect.
- Review is the real bottleneck (the 3–5 ceiling), so the design must make the
  review queue visible rather than pretend parallelism is free.

## Decision

### 1. Isolate writers with git worktrees; let readers share
Any subagent that can **write** runs in its **own git worktree** — same
repository, same object store and history, its own working directory, index, and
branch. Read-only research subagents share the main workspace: they produce no
state, so isolation would buy nothing but duplicated disks and dependency
installs.

- **Boundary — files only.** A worktree isolates files, **not** external
  resources: ports, databases, containers, shared caches, and global locks still
  collide. Concurrent write subagents must declare/allocate such resources, or
  the concurrent-write count is capped.
- **Boundary — non-git workspaces.** Fall back to a temporary copy or to
  serialization. Never shared concurrent writes.
- **Boundary — visibility.** A writer's changes are invisible to readers until
  merged. That is usually desirable (readers answer from a stable baseline), but
  it is a real behavior users must be told about.
- Rationale: isolation is a **filesystem+git guarantee**, not a prompt asking
  the model to be careful. "Prompts are not permission systems" applies to
  concurrency too.

### 2. Subagents are child sessions returning artifacts
Each subagent is a **child session** (delegation — ADR-0015), linked by the
parent tool call that spawned it and recording the point in the parent's event
tree it came from; it inherits the parent's sandbox and permission policy. It
returns a **structured artifact** — the files it wrote plus a structured
summary — and **not** a long transcript. The parent's context receives the
summary; the full record stays in the child session, inspectable (T-12/T-13) and
lossless (ADR-0003). A child session **references** its parent's prefix rather
than copying it (ADR-0015), so the parent is never duplicated.

Subagents participate in routing like any other call site in gateway mode
(ADR-0005); in local mode the user's configured pick applies. A read-only
explore subagent may run on a cheap profile, a build subagent on a strong one —
a visible routing decision, never a hidden one.

### 3. One orchestrator: the parent agent's visible plan
Local orchestration **is** the parent's plan. There is **no separate scheduler
and no second decision-maker**.

- Keeps orchestration explainable and steerable: the plan, each spawn, its
  prompt, and its result are all in one chain the user can read and interrupt
  (T-13/T-12).
- Keeps spend attributable: every spawn is a visible decision with a reason
  (T-04).
- Keeps the lifetime bounded: orchestration cannot outlive the session, so
  there is no orphaned queue, no resumed job, and no question of who owns a
  worktree after the TUI closes.
- **Boundary test** for any future "put this in the backend" request: *how long
  must this work live, and who has the authority to decide it?* If it cannot
  outlive the session and the user authorizes it, it belongs to the parent's
  plan.
- **This does not mean:** no parallelism (one spawn call can fan out N), no
  limits (budget caps, the concurrent-write cap, sandbox and permission
  decisions still come from the kernel — ADR-0007/0008), or no helper library
  (a spawn/wait/collect utility is fine; a component that *decides the plan* is
  not).

### 4. The ceiling is human attention: 3–5 agents, tiered review
Concurrency is bounded by review capacity, not by machine capacity. **3–5
parallel agents** is the working ceiling. Review is **tiered**: an LLM
pre-review filters and summarizes, a human signs off; with no shared sessions
(ADR-0010) this reviews **the user's own** agents. Review-relevant data is
first-class in the event stream from v1; the rich review UI arrives with the
web surface (ADR-0009 build order).

### 5. Scope: local only
Cross-user orchestration, scheduled work, cloud runners, and a runner scheduler
are **out of scope until a concrete use case appears** (owner, 2026-10-04).
ADR-0007's micro-VM tier stays deferred with them, since async runners were its
driver.

## Consequences

- **"It verified" becomes a trustworthy sentence.** Each writer's test run
  reflects its own tree, so verification conclusions, diffs, and failure
  attribution all hold — this is the point of the decision, not disk layout.
- **Review becomes a diff, and abandonment becomes free.** A child session (a
  *branch*, ADR-0015) is the unit of accept/reject; discarding a bad attempt
  costs one worktree.
- **Parallelism has a visible price.** Per-worktree dependency installs and disk
  are real costs; worktree lifecycle and garbage collection are now product
  surface, not plumbing detail. The default UX must make "accept this branch"
  one explicit action, or the isolation cost lands on beginners.
- **The parent context stays small**, so a 7-way swarm does not blow the budget
  (ADR-0006) — artifacts are the interface, summaries are the payload.
- **The design deliberately refuses scheduling features** (preemption,
  priorities, cross-session queues, crash-resume). Accepted: with a ceiling of
  3–5 and the session as the unit of work, those features have nothing to
  optimize.
- **Deferred:** the artifact/summary schema, worktree lifecycle and GC policy,
  how external resources are declared, the concurrent-write cap's default, the
  review UI, and the non-git workspace UX.
