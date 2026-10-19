# 02 — Topic List (idea backlog)

- **Up:** [04-system-overview.md](04-system-overview.md)
- **Authority:** [docs/README.md](README.md)
- **Code:** `docs/02-topics.md`, `docs/topics/README.md`
- **Verify:** `python3 scripts/check-docs.py` · `./scripts/check.sh`

How this works: the owner drops raw ideas in conversation; they land in the
backlog as topics with a one-line seed connecting them to what the research
already knows (`research/`, `05-parity-and-synthesis.md`). When a topic gets
discussed in depth, it graduates to its own `NN-<topic>.md` or an ADR in
`decisions/`, and its status changes to **Promoted**. This file is the entry;
the topics themselves live under [topics/](topics/README.md), which indexes all
26 with one line each.

Status legend: `Seed` (captured, not yet discussed) · `Discussing` · `Wanted`
(owner has said "I want this" — direction agreed, details to discuss) ·
`Promoted → ADR-NNNN` (settled by that decision) · `Merged → T-NN` (folded into
another topic). Every status names a document or topic that exists.

## Where each group moved

| Group | Detail file | Topics |
|---|---|---|
| Seed, Wanted, Discussing | [topics/open.md](topics/open.md) | T-06, T-08, T-12, T-15, T-20, T-22, T-23, T-24, T-25, T-26 (10) |
| Promoted/Merged — model, cost and observability | [topics/decided.md](topics/decided.md) | T-01, T-02, T-04, T-07, T-13 (5) |
| Promoted — control plane and deployment | [topics/decided-platform.md](topics/decided-platform.md) | T-03, T-05 (2) |
| Promoted/Merged — safety, people and extension | [topics/decided-extension.md](topics/decided-extension.md) | T-09, T-10, T-11, T-14, T-16, T-17, T-18, T-19, T-21 (9) |

The 16 Promoted/Merged topics did not fit one 250-line file, so they are split
by theme; `open.md` holds the undecided ones. Every `## T-NN` heading moved
verbatim, so an existing `T-NN` citation still resolves.

[topics/README.md](topics/README.md) carries the index table (number, title,
status, detail file) and the owner's 2026-09-30 coverage map in one paragraph.
New topics go in `open.md`; just say the idea — capture is automatic.
