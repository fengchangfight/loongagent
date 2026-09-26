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
- Every doc starts with a header: **Status** (`Draft` / `Discussing` / `Agreed` /
  `Superseded by NN`) and **Last updated** date.
- Docs are updated in place whenever discussion changes the design — they reflect
  the *current* shared understanding, not a history. History lives in git.
- Nothing is "Agreed" until the human explicitly signs off in conversation.

## Index

| Doc | Topic | Status |
|-----|-------|--------|
| [00-vision.md](00-vision.md) | Vision, ambition, non-goals, target users | Draft |
| [01-landscape-synthesis.md](01-landscape-synthesis.md) | Cross-tool synthesis: features, pros/cons, design tensions | Draft |
| [02-topics.md](02-topics.md) | Idea backlog — raw topics, waiting to be discussed | Living |
| research/ | Per-tool deep dives | — |
