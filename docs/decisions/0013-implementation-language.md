# ADR-0013 — Implementation language: Rust kernel and servers, TypeScript web client

- **Status:** Accepted
- **Date:** 2026-10-04
- **Decides:** AQ-12 (docs/03-architecture.md) — the implementation language

## Context

AQ-12 was deliberately left last: the implementation language is a **consequence
of the architecture, not a precondition** (03-architecture.md). With AQ-1…AQ-11
answered, the requirements are concrete rather than speculative:

| Requirement | Source | What it demands of the language |
|---|---|---|
| One artifact, many faces | ADR-0001, ADR-0009 | A single binary per OS that *contains* the local agent server; web and IDE are thin clients |
| OS sandbox default-on | ADR-0007 | FFI-level access to seatbelt, Landlock + bubblewrap, Job Objects — plus a real `sandbox self-test` |
| Pluggable edges, fixed kernel | ADR-0004 | A first-class **WASM host** (capability-gated, versioned host API) and a tiny loop that cannot be swapped |
| JSONL truth + derived index, context ledger | ADR-0003, ADR-0006 | Fast append, fast SQLite indexing, exact token accounting |
| Retrieval floor at enterprise scale | T7 / enterprise research | grep, parsing, repo map and indexing that hold up on 10M+ LOC repos |
| Signed packs | ADR-0008 | Mature crypto libraries and signature verification on the client |
| Local parallelism | ADR-0011 | Cheap subprocesses, git-worktree orchestration, many concurrent sessions |
| Small control plane, shared crates | ADR-0012 | One workspace, **two binaries**; shared schema, pack sign/verify, policy resolution, provider adapters, cost math |

Ecosystem evidence points three ways: **Codex is Rust** (robustness, single
binary, sandbox FFI), **Pi / OpenCode / Claude Code are TypeScript** (iteration
speed, extension ecosystem, self-modification), **Amp / OpenHands are Go**
(middle ground, strong server ergonomics, weaker TUI).

The strongest argument *against* a systems language was "the harness must be
cheap to change, because nobody knows the right shape yet." **ADR-0004 already
answered it**: behavior changes arrive as WASM plugins, markdown skills and
commands, hooks, and config packs — the edges — while the core is deliberately
small, fixed, and rarely touched.

## Decision

### 1. Rust for the kernel and servers
The local agent server (the kernel: loop, event append, context assembly, PDP,
storage, retrieval), the control-plane backend modules, and the **TUI** are
written in Rust.

### 2. TypeScript for the web client
The web surface (ADR-0009) is TypeScript, built to static assets and
served/embedded by the same binary. It is a **thin client**: rendering and
intents only, zero agent logic.

### 3. The boundary is one-way, narrow, and schema-defined
The web client speaks JSON-RPC (ADR-0002) to the local server. ADR-0002 already
made the protocol **schema-first**, so the TypeScript client's types are
**generated from the protocol schema** rather than hand-maintained — one
source of truth, no drift, no shared runtime.

### 4. Expected implementation choices that follow
Recorded so they don't get re-litigated as architecture:

- **TUI toolkit: `ratatui`** — the consequence of AQ-11's "framework falls out of
  AQ-12". Revisable as an implementation detail, not an ADR.
- **WASM host: `wasmtime`** — the reference implementation, satisfying ADR-0004's
  capability-gating and versioned-host-API requirements.

### 5. What this does not mean
- **Extension authors do not need Rust.** Skills, commands, rules, hooks, and
  WASM plugins (in any source language) remain the extension surface (ADR-0004).
- **Self-modification means the extension surface, not the kernel.** The agent
  edits its own skills, commands, rules, and plugins. Editing the Rust kernel is
  explicitly *not* a v1 goal; if it ever becomes one, it returns as a new
  discussion.
- **The web is not a second product.** It is a thin client of the same server
  (ADR-0009).
- **Users never need to know any of this.** They install one binary.

## Consequences

- **The two hardest requirements are native, not worked around.** Sandbox
  enforcement and the WASM host are exactly where Rust is strongest and where
  retrofitting later would be most expensive.
- **The performance floor is comfortably met** — event append, SQLite indexing,
  repo map, and retrieval at enterprise scale.
- **We accept two languages and a smaller core contributor pool.** Mitigations,
  in order of importance: the kernel stays tiny and fixed (ADR-0004); product
  iteration happens at the edges (plugins, skills, hooks, config); the protocol
  schema generates the TypeScript types so the boundary cannot rot; and
  contributing to the edges requires no Rust at all.
- **Onboarding cost is uneven by design.** TUI/kernel work has a steeper ramp
  than web or extension work — accepted, because the parts that are hardest to
  change later are the parts that most deserve the stricter language.
- **Hiring/contribution trades breadth for robustness.** The Rust pool is
  smaller than the TypeScript pool and growing; the decision deliberately spends
  contributor breadth on the components where correctness is expensive to
  recover.
- **The protocol schema becomes a build artifact for both sides**, which makes
  AQ-2's schema-first choice pay off a second time.
- **Deferred:** workspace/toolchain specifics (cargo workspace layout; Bun vs
  Node for building the web client), how the two binaries (local agent, control
  plane) are versioned and released together, and the concrete WASM host API
  surface (ADR-0004).
