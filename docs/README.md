# Design Docs — loongagent

This folder is the living memory of **loongagent** (龙 — the Chinese dragon,
a creature assembled from the strongest parts of many animals: our product
philosophy in one name). All design discussion between the human and the agent
is tracked here so decisions survive context windows and sessions.

## Conventions

- `NN-<topic>.md` — numbered top-level design docs, in rough order of creation.
- `research/` — investigation notes on existing tools, papers, techniques.
- `decisions/` — ADRs (Architecture Decision Records), one per significant decision,
  once we start locking things in.
- Status is recorded in the table below: a design document or research note
  carries a title and its content, with no status or date line of its own. An
  ADR keeps its `Status:` line (`Proposed` / `Accepted` / `Superseded`), because
  a decision's status is part of the record rather than metadata about the file;
  `archive/` keeps its headers, because archived material is history and is not
  maintained; and the `superpowers/` spec keeps its `Date:` and `Status:`.
- Docs are updated in place whenever discussion changes the design — they reflect
  the *current* shared understanding, not a history. History lives in git.
- Nothing is "Agreed" until the human explicitly signs off in conversation.

## Index

| Doc | Topic | Status |
|-----|-------|--------|
| [00-vision.md](00-vision.md) | Vision, ambition, non-goals, target users | Draft |
| [01-landscape-synthesis.md](archive/01-landscape-synthesis.md) | Cross-tool synthesis: features, pros/cons, design tensions | Superseded by 05 |
| [02-topics.md](02-topics.md) | Idea backlog — raw topics, waiting to be discussed | Living |
| [03-architecture.md](03-architecture.md) | Architecture, question-driven (AQ-1…AQ-12 decision map) | Agreed |
| [04-system-overview.md](04-system-overview.md) | The architecture as decided — one-read end-to-end picture | Agreed |
| [05-parity-and-synthesis.md](05-parity-and-synthesis.md) | Parity vs Claude Code + absorption backlog from the field + deliberate "no"s | Living |
| [06-context-engineering.md](06-context-engineering.md) | Context engineering — layered reversible memory (L1–L4 + raw archive, background increment, `recall`) | Discussing (P1–P5.5 implemented) |
| [crate-map.md](crate-map.md) | The workspace's crates and the dependency edges allowed between them — generated from `scripts/layering_rules.py`, so it must never be hand-edited | Generated |
| [dsh-plugin-ecosystem.md](research/dsh-plugin-ecosystem.md) | DSH plugin ecosystem — can our extension model host DSH-style plugins? | Research note (input to a future ADR-0004 revision) |
| archive/ | Superseded history, kept as the record — `01`'s cross-tool synthesis and `03`'s pre-AQ framing + discussion log; not current guidance | — |
| decisions/ | ADRs — one per locked decision (0001: runtime topology; 0002: wire protocol; 0003: session/event storage; 0004: extension model; 0005: routing philosophy; 0006: context engine; 0007: permission & sandbox engine; 0008: configuration & profile layering; 0009: surfaces; 0010: team collaboration as shared knowledge; 0011: local parallelism; 0012: control plane; 0013: implementation language; 0014: session export/import; 0015: session vs event tree; 0016: pack distribution; 0017: layered context, *Proposed*; 0018: plugins — install by name, seat what a plugin asks for) | — |
| runbooks/ | Doing, not deciding — [control-plane.md](runbooks/control-plane.md) stands the control plane up; [windows-build.md](runbooks/windows-build.md) builds and runs the agent on Windows | — |
| research/ | Per-tool deep dives (round 1: major agents; round 2: categories) | — |
| superpowers/ | Design specs and implementation plans produced while working, one dated pair per plan — [2026-10-16-navigability-design.md](superpowers/specs/2026-10-16-navigability-design.md) | — |

**Round 1:** claude-code, codex, opencode, cursor, pi, deepseek-harness, other-agents
**Round 2:** kiro-spec-driven, role-specialized, multi-agent-orchestration,
web-app-builders, ide-ecosystem, china-ecosystem, enterprise-context-review
**Ecosystem assessment:** [dsh-plugin-ecosystem](research/dsh-plugin-ecosystem.md)
