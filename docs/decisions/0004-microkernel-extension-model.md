# ADR-0004 — Microkernel extension model: pluggable edges, fixed kernel

- **Status:** Accepted
- **Date:** 2026-10-01
- **Decides:** AQ-4 (docs/03-architecture.md)

## Context

loongagent must be "pluggable but with strong defaults" (owner's requirement):
cheap to change because nobody knows the right harness shape yet, open to a
third-party ecosystem, but without sacrificing the "nothing hidden, everything
inspectable" promise (T-13) or the sandbox-first security posture (T-11).
Ecosystem evidence pulls two ways: Pi's in-process TS extensions are beloved
but couple the ecosystem to one language and grant full process trust;
the DeepSeek-harness ecosystem (CCR, Aider, Goose) shows "everything is a
plugin" working well — at provider, transformer, and tool boundaries.

## Decision

**A microkernel: everything at the edges is a plugin; the kernel is tiny and
fixed.**

### Fixed kernel (NOT a plugin point)
The minimal model+tool loop, event append (ADR-0003), and context assembly.
Keeping these non-swappable is what makes T-13's observability promise true:
behavior cannot hide inside a replaced core.

### Pluggable edges

| Edge | Execution model | Notes |
|---|---|---|
| Provider adapters | plugin + data | ~4 wire protocols behind one `Model` interface |
| Request/response transformers | plugin chain (CCR pattern) | normalizes protocol drift per endpoint; **logs every downgrade** |
| Per-model capability registry | pure data (Aider pattern) | new model = new record; benchmark-derived where possible |
| Tools, incl. built-ins | one uniform interface (Goose pattern: MCP-shaped) | single chokepoint for permissions, hooks, cost metering |
| Routing strategies | plugin | feeds T-01 cascading |
| Hooks (T-09) | shell executables, exit-code protocol | language-neutral; repo-shipped hooks require explicit trust opt-in |
| Commands & skills (T-16/T-18) | markdown + frontmatter | bundled scripts run as subprocesses |
| Third-party deep extensions | **WASM**, sandboxed, capability-gated, versioned host API | any source language; no full process trust |
| First-party batteries | in-process | trusted (it's us): plan mode, subagents, skills runtime |

## Alternatives considered

- **In-process everything (Pi).** Rejected: locks third-party extensions to the
  core's implementation language and grants them full process trust — conflicts
  with T-11.
- **Subprocess for everything.** Rejected: uniform but slow and clunky for deep
  integrations; WASM gives near-in-process ergonomics with sandboxing.

## Consequences

- The extension ecosystem is **decoupled from the core's implementation
  language** (AQ-12 can choose Rust without killing community extensibility).
- WASM extensions are a less-trodden path — no major agent has proven one at
  scale; we accept early-mover cost for the strategic payoff, and must budget
  for a good host-API + toolchain story.
- Every plugin boundary is also an audit/observability boundary: loads,
  vetoes, and downgrades are events in the JSONL stream (ADR-0003).
