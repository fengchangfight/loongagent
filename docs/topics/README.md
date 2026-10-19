# Topic index

- **Up:** [02-topics.md](../02-topics.md)
- **Authority:** [docs/README.md](../README.md)
- **Code:** `docs/02-topics.md`, `docs/topics/README.md`, `docs/topics/open.md`, `docs/topics/decided.md`, `docs/topics/decided-platform.md`, `docs/topics/decided-extension.md`
- **Verify:** `python3 scripts/check-docs.py`

This is the index of the idea backlog that [02-topics.md](../02-topics.md)
introduces. Each topic keeps its number, status and full text in one detail file;
a topic leaves `open.md` for a `decided*.md` file when an ADR or a merge settles
it, and (except for a merge) its status then names the deciding ADR.

The owner's 2026-09-30 checklist is still covered, item for item: hooks T-09,
memory T-10, budgets T-04/T-07, permissions and sandbox T-11, flexible profiles
T-07, Pi try/retry/fork and mid-run steering T-12, "understand why the agent is
doing X" T-13, teamwork T-14, MCP T-15, commands T-16 (merged into T-18),
subagents T-17, skills marketplace T-18, IDE T-19, git diff and spec-driven
process T-20, the client/server strawman T-05, context visibility T-13, the
minimal loop and cheap-to-change harness in the vision principles, and
approachability T-21.

## Where the topics live

| File | Statuses | Topics |
|---|---|---|
| [open.md](open.md) | Seed, Wanted, Discussing | 10 |
| [decided.md](decided.md) | Promoted, Merged | 5 — model, cost and observability |
| [decided-platform.md](decided-platform.md) | Promoted | 2 — control plane and deployment |
| [decided-extension.md](decided-extension.md) | Promoted, Merged | 9 — safety, people and extension |

`open.md` and `decided.md` are the two files the split started with. The 16
Promoted/Merged topics did not fit `decided.md`'s 250-line budget, so they were
split by theme into the two extra files; no topic text was trimmed.

## The 26 topics

| # | Topic | Status | Detail |
|---|---|---|---|
| T-01 | Smart cascading model routing | Promoted → ADR-0005 | [decided.md](decided.md) |
| T-02 | Audit & statistics & implicit feedback loop | Promoted → ADR-0003, ADR-0005, ADR-0012 | [decided.md](decided.md) |
| T-03 | Deployment & ways of using (local, cloud, team sharing, cloud statistics) | Promoted → ADR-0012, ADR-0010 | [decided-platform.md](decided-platform.md) |
| T-04 | Cost governance (anti-waste by design) | Promoted → ADR-0005, ADR-0006, ADR-0007, ADR-0009 | [decided.md](decided.md) |
| T-05 | Rich backend / control plane (not a local-only tool) | Promoted → ADR-0012 | [decided-platform.md](decided-platform.md) |
| T-06 | Per-role UX: the "this is THE tool for me" moment | Wanted (HIGH PRIORITY) | [open.md](open.md) |
| T-07 | Model profiles, not model pickers | Promoted → ADR-0008, ADR-0016 | [decided.md](decided.md) |
| T-08 | Authentication & identity (flexible, firm-internal first) | Seed | [open.md](open.md) |
| T-09 | Hooks & deterministic enforcement | Promoted → ADR-0007 | [decided-extension.md](decided-extension.md) |
| T-10 | Memory system | Promoted → ADR-0006 | [decided-extension.md](decided-extension.md) |
| T-11 | Permissions & sandbox | Promoted → ADR-0007 | [decided-extension.md](decided-extension.md) |
| T-12 | Session tree: fork, retry, steer | Wanted | [open.md](open.md) |
| T-13 | Observability: "what is the agent doing and *why*" | Promoted → ADR-0006, ADR-0007, ADR-0003 | [decided.md](decided.md) |
| T-14 | Teamwork & sharing | Promoted → ADR-0010 | [decided-extension.md](decided-extension.md) |
| T-15 | MCP & external tool ecosystem | Seed | [open.md](open.md) |
| T-16 | Commands | Merged → T-18 | [decided-extension.md](decided-extension.md) |
| T-17 | Subagents | Promoted → ADR-0011 | [decided-extension.md](decided-extension.md) |
| T-18 | Skills & marketplace | Promoted → ADR-0018 | [decided-extension.md](decided-extension.md) |
| T-19 | IDE integration via ACP | Promoted → ADR-0009 | [decided-extension.md](decided-extension.md) |
| T-20 | Git-diff-centric workflow & spec-driven process | Seed | [open.md](open.md) |
| T-21 | Layered approachability: junior-proof defaults, senior-proof depth | Promoted → ADR-0007, ADR-0008 | [decided-extension.md](decided-extension.md) |
| T-22 | LSP-backed code intelligence | Seed | [open.md](open.md) |
| T-23 | Persistent goal with evaluator | Seed | [open.md](open.md) |
| T-24 | Module metadata & lifecycle | Seed | [open.md](open.md) |
| T-25 | Interaction smalls | Seed | [open.md](open.md) |
| T-26 | Cross-crate helper duplication | Seed | [open.md](open.md) |
