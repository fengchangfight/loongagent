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
  maintained; and specs under `superpowers/` keep their `Date:` and `Status:`.
- **No status, progress, build-order, or next-step section inside a document.**
  Those describe process, not design. A document states what *is* true; what
  changed, when, and what comes next lives in git history — or, where the
  document keeps a dated amendment log (05), in that log. This is why a state
  document like `04-system-overview.md` ends with what the system is, not with
  where the project stands.
- **`human-written/` is the human's notebook.** It is input to the project,
  never output for an agent: `AGENTS.md` forbids creating, editing, moving, or
  deleting anything there, and `scripts/check-human-written.sh` fails the gate
  when it changes. Its notes follow no conventions of this folder — the docs
  checks (links, orphans, citations) skip the directory entirely.
- A document under `docs/` whose body is not English carries a `Language: <code>`
  line immediately below its title, so a reader who cannot read the language is
  warned before the first paragraph. `docs/06-context-engineering.md` and
  `docs/decisions/0017-layered-context.md` are the two marked exceptions, and
  translating them is not in scope. (`README.md` is Chinese throughout and is
  not marked: it is the entry document, not an exception inside `docs/`.)
- Docs are updated in place whenever discussion changes the design — they reflect
  the *current* shared understanding, not a history. History lives in git.
- Nothing is "Agreed" until the human explicitly signs off in conversation.

## Document tree

Every maintained document starts with a title and four header fields, so a
reader can walk up to the design that owns it:

```markdown
# Kernel loop

- **Up:** [design/README.md](README.md)
- **Authority:** [ADR-0004](../decisions/0004-microkernel-extension-model.md)
- **Code:** `crates/kernel/src/turn.rs`
- **Verify:** `cargo test -p loong-kernel`
```

`Up` is the parent edge, and the reachability check follows it. `Authority` names
the ADR or higher document that decides the topic. `Code` names the files the
document describes. `Verify` names the command that proves them.

Exceptions, each for a stated reason:

- ADRs keep their `Status:` line and gain only `Up`. Their decision text is the
  record and does not change.
- `docs/README.md` is the root: it carries `Code` and `Verify`, but no `Up`.
- `README.md`, `DEVELOPMENT.md` and `docs/00-vision.md` are entry points; their
  `Up` may name `docs/04-system-overview.md`, and `README.md` may be the root.
- `research/`, `archive/`, `superpowers/` and `human-written/` are records or
  human input. They are indexed and link-checked but need no header and have no
  length cap.

Length budgets, by class:

| Class | Budget |
|-------|--------|
| `README.md` | 250 |
| `DEVELOPMENT.md` | 300 |
| `docs/README.md`, `docs/00`, `docs/02` | 150 |
| `docs/04` | 180 |
| `docs/03`, `docs/05` | 250 |
| `docs/06` entry, `docs/07` entry | 60 |
| `docs/design/**`, `docs/context/**`, `docs/topics/**` | 250 |
| `docs/extending/**` | 150 |
| `docs/runbooks/**` | 300 |
| `docs/decisions/**` | 300 |

One document is the authority for a topic: a subsystem document owns its
mechanism, an ADR owns its decision, `docs/04` owns the current architecture and
`README.md` owns status. Another document links to that authority instead of
restating it. Detail links down: a parent summarises and points to the child
that carries the detail.

`docs/design/` is the subsystem layer — one document per mechanism, one flow
document per process — while `docs/README.md` remains the only index; a new
directory gets its row in the task that creates it, so the index never links a
file that does not exist yet.

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
| [07-extending.md](07-extending.md) | Changing the machinery — a new built-in tool, a new event, a hook, how routing picks a model | Living |
| human-written/ | The human's own notes — input, never edited by an agent (`AGENTS.md`, `scripts/check-human-written.sh`) | — |
| [crate-map.md](crate-map.md) | The workspace's crates and the dependency edges allowed between them — generated from `scripts/layering_rules.py`, so it must never be hand-edited | Generated |
| [dsh-plugin-ecosystem.md](research/dsh-plugin-ecosystem.md) | DSH plugin ecosystem — can our extension model host DSH-style plugins? | Research note (input to a future ADR-0004 revision) |
| archive/ | Superseded history, kept as the record — `01`'s cross-tool synthesis and `03`'s pre-AQ framing + discussion log; not current guidance | — |
| decisions/ | ADRs — one per locked decision (0001: runtime topology; 0002: wire protocol; 0003: session/event storage; 0004: extension model; 0005: routing philosophy; 0006: context engine; 0007: permission & sandbox engine; 0008: configuration & profile layering; 0009: surfaces; 0010: team collaboration as shared knowledge; 0011: local parallelism; 0012: control plane; 0013: implementation language; 0014: session export/import; 0015: session vs event tree; 0016: pack distribution; 0017: layered context, *Proposed*; 0018: plugins — install by name, seat what a plugin asks for) | — |
| runbooks/ | Doing, not deciding — [control-plane.md](runbooks/control-plane.md) stands the control plane up; [windows-build.md](runbooks/windows-build.md) builds and runs the agent on Windows | — |
| research/ | Per-tool deep dives (round 1: major agents; round 2: categories) | — |
| superpowers/ | Design specs and implementation plans produced while working, each dated and named after the work it belongs to — [2026-10-16-navigability-design.md](superpowers/specs/2026-10-16-navigability-design.md) | — |

**Round 1:** claude-code, codex, opencode, cursor, pi, deepseek-harness, other-agents
**Round 2:** kiro-spec-driven, role-specialized, multi-agent-orchestration,
web-app-builders, ide-ecosystem, china-ecosystem, enterprise-context-review
**Ecosystem assessment:** [dsh-plugin-ecosystem](research/dsh-plugin-ecosystem.md)
