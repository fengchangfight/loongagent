# ADR-0004 — Microkernel extension model: pluggable edges, fixed kernel

- **Status:** Accepted
- **Date:** 2026-10-01
- **Decides:** AQ-4 (docs/03-architecture.md)

## Context

**This is primarily an iteration architecture, and only secondarily a
security architecture.** The owner clarified the intent on 2026-10-05: the
extension model is not built so that outsiders can extend loongagent — it is the
architecture **we** develop in, chosen because it makes iteration easier.
Modules behind seams can be versioned, enabled, replaced and *deleted*
independently; inlined code cannot. "Cheap to change" is the first requirement;
"safe to open to third parties" is a later one.

That ordering matters, because it decides who runs where:

- **Our own modules are in-process.** They are inside our trust boundary by
  definition — the "first-party batteries" row below. Seams give us the
  replaceability, not a sandbox.
- **Third-party code is the case the sandbox exists for**, and it is a *future*
  case. The WASM/capability machinery, the trust tiers, signing and any
  marketplace are therefore **backlog, not v1 critical path** (see
  [05-parity-and-synthesis.md](../05-parity-and-synthesis.md) §D).

Beyond iteration speed, loongagent must still be "pluggable but with strong
defaults" (owner's requirement): cheap to change because nobody knows the right
harness shape yet, without sacrificing the "nothing hidden, everything
inspectable" promise (T-13) or the sandbox-first security posture (T-11).
Ecosystem evidence pulls two ways: Pi's in-process TS extensions are beloved
but couple the ecosystem to one language and grant full process trust;
the DeepSeek-harness ecosystem (CCR, Aider, Goose) shows "everything is a
plugin" working well — at provider, transformer, and tool boundaries. Claude
Code — the most-adopted enterprise agent — sits at the same shape as our edges:
markdown content, subprocess/HTTP hooks, MCP servers, subagent definitions;
**no in-process third-party host code and no third-party client UI**.

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
| Hooks (T-09) | **shell / HTTP / MCP tool** (same verdict contract); LLM and subagent kinds deferred to v2 | language-neutral; HTTP hooks are an opt-in, org-pinnable network action class; repo-shipped hooks require explicit trust opt-in (ADR-0007 §7) |
| Commands & skills (T-16/T-18) | markdown + frontmatter | bundled scripts run as subprocesses |
| Third-party deep extensions | **WASM**, sandboxed, capability-gated, versioned host API | any source language; no full process trust. **Future tier** — see Context |
| First-party batteries | in-process | trusted (it's us): plan mode, subagents, skills runtime |
| Our own modules behind edges | in-process (or subprocess where it fits) | the normal case: we write plugins too, for our own replaceability |

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
