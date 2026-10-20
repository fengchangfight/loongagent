# AGENTS.md

Rules for an AI agent working in this repository, in addition to the documents
it points to.

## Speak plainly (“说人话”)

Apply this to every human-facing reply, explanation, and document:

- Lead with the conclusion or action, then add only the context needed. Use
  short sentences, and cut filler, repetition, and showing off.
- Prefer everyday words over jargon, acronyms, and translationese. Explain any
  necessary technical term in plain language, with a concrete example if useful.
- Make complex ideas easy without making them shallow: break them into small
  steps, use analogies only when they clarify, and keep key constraints, risks,
  and uncertainty explicit.
- Match the user's language. Be concise, direct, and natural; never sacrifice
  accuracy for simplicity.

## `docs/human-written/` is human-only

`docs/human-written/` holds notes the human wrote for themselves. It is **input
you may read and cite, never output you may edit**.

- Do **not** create, edit, rename, move, or delete any file under
  `docs/human-written/` — not even a typo fix, not even when the change looks
  obviously right, not even a `README.md` explaining the directory.
- Do **not** weaken or bypass the gate to get a green run: leave
  `scripts/check-human-written.sh`, its call in `scripts/check.sh`, and the CI
  steps that set `HUMAN_WRITTEN_BASE` alone. Never write a
  `Human-Written: true` trailer into a commit — that trailer is the human's
  signature, not the agent's.
- If a task seems to require a change there, stop and ask the human to make it.
- A rule that is only stated is not enforced: `scripts/check-human-written.sh`
  fails the gate when the directory changes, and `scripts/check.sh` (the same
  set CI runs) invokes it. The human's own commits carry a
  `Human-Written: true` trailer to attest them.

The rest of `docs/` is the opposite: it is maintained, updated in place, and
its conventions live in [`docs/README.md`](docs/README.md).

## Contracts and navigation

Rules for keeping every code and document node linked to the design that owns
it. [`docs/README.md`](docs/README.md) carries the document conventions; the
[modular-contracts spec](docs/superpowers/specs/2026-10-18-modular-contracts-design.md)
carries the reasoning.

### Documents first

Before you change code, name the design document that owns the behaviour. If
none exists, write or split one first, then the module plan, then the code.

### Every code node has a contract

Every `.rs` file opens with a `//!` module contract: `Purpose:` and the five
labels `Serves`, `Design`, `Upstream`, `Downstream`, `Verify`. `Design` names
the owning document by a backticked `docs/...` path, or by an `ADR-NNNN` that
exists; `Verify` names the command that proves the file.

Every `fn`, `struct`, `enum`, `trait`, `union` and `type` carries a doc block.
`pub` items and non-trivial private items carry `Purpose:` plus `Serves`,
`Design`, `Up`, `Down`, `Verify`; items say `Up`/`Down`, the file contract says
`Upstream`/`Downstream`. A private function whose body is at most 12 lines may
use `Purpose:` and `Contract:` in place of the five. Types always carry the full
block, private or not. A test function carries one `Purpose` line; the file's
module contract names the test target. A declaration-only function — a trait
method, or a body-less signature outside a trait — may carry one `Purpose` line;
the trait's or file's contract carries the rest. `mod` declarations need
nothing: the file contract covers them.

A file that cannot carry a contract is listed, with a reason, in
`scripts/contract-exemptions.txt`: the generated schema files
`crates/schema/src/generated.rs` and
`crates/schema/tests/generated_roundtrip.rs`, and the crate-root build scripts
`crates/cli/build.rs` and `crates/control/build.rs`.
`crates/kernel/src/turn/build.rs` is a module, not a build script. A single item
may use `// Contract-exempt: <reason>`, or be listed as `path#item | reason` in
the same file; a reason shorter than 10 characters exempts nothing, and a stale
exemption fails the gate.

### Every document has a path up

Every maintained document starts with four header fields: `Up` (its parent
edge), `Authority` (the ADR or higher document that decides the topic), `Code`
(the files it describes) and `Verify` (the command that proves them).
`research/`, `archive/`, `superpowers/` and `human-written/` are records and
need no header; the generated `docs/crate-map.md` is a product and needs none
either; ADRs gain only `Up`; `README.md` and the index (`docs/README.md`) are
roots and need no `Up`. Every document stays within its class budget, stated
with the other exceptions in [`docs/README.md`](docs/README.md); an over-run
fails the gate.

One document is the authority for a topic. Other documents link to it instead of
restating it. Detail links down: a parent summarises and points.

### The gates are the enforcement

- `scripts/check-modules.py` checks the code contracts.
- `scripts/check-docs.py` checks links, the index, the crate map, citations,
  test counts, headers, budgets, code paths and the `Up` chains.
- `scripts/check.sh` runs everything CI runs and must be green before a commit.

Every check is shown failing on a seeded defect before it is trusted; a check
that has never failed has not been shown to work. Both migration baselines are
gone: `scripts/doc-contract-baseline.txt` was deleted after the document-tree
audit, and `scripts/code-contract-baseline.txt` after the code audit, so
`scripts/check-docs.py` and `scripts/check-modules.py` now check the whole
maintained tree. A future migration that needs a baseline must recreate the
file explicitly; while one exists it is a ratchet that only shrinks and fails on
a stale entry, so a task removes its own entries and never leaves one behind.
