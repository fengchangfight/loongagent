# Modular contracts: every node findable, every edge checked

- **Date:** 2026-10-18
- **Status:** Draft for human review
- **Scope:** documentation architecture, code-level contracts, and the gates that
  keep both true. Comments and documents only: zero behaviour change, zero `pub`
  API change, zero crate-boundary change, zero new dependencies.

## 1. Problem

The repository has good prose and no navigable structure.

Code comments explain *what a block does* when someone remembered to write one.
They rarely say which design document owns the behaviour, what calls the block,
what it calls, or how to prove it still works. The documents are numbered by
creation order, not by subsystem: a reader who wants the kernel loop starts at
`docs/04`, follows a link into a 507-line decision map, then a 1,059-line
context document. Nothing checks either property, so both decay silently.

The measured shape:

| Fact | Measurement |
|---|---|
| Rust source | 107 files, 55,445 lines |
| Annotatable items | 2,527 matches for `fn` / `struct` / `enum` / `trait` / `type` / `mod`; **2,422** after excluding 105 `mod` declarations (a file's contract covers its modules) |
| Public declarations | 868, of which 839 are not `mod` |
| Files with a `//!` module header | 69 of 107 — **38 have none** (for example `crates/kernel/src/turn/loop.rs`, `crates/cli/src/local.rs`, `crates/config/src/merge.rs`) |
| Module headers that name an ADR | 40 of 107 |
| Module headers that mention a verification command or test | 41 of 107 |
| Maintained documents (excluding `research/`, `archive/`, `superpowers/`, `human-written/`) | 7,779 lines across 31 files; the whole tracked markdown tree is 13,730 lines across 53 files |
| Documents over 250 lines | `docs/06` 1,059; `DEVELOPMENT.md` 1,111; `docs/02` 696; `runbooks/control-plane.md` 539; `docs/03` 507; plus the ADR and research records listed in §4.4 |
| Existing document gate | five checks: links, generated crate map, orphans, citations, test counts. No check reads code comments, document headers, lengths or parent links |

Two structural gaps cause the rot:

- **No edge contract.** A comment is free prose. Nothing requires the module to
  name its design authority, its callers, its callees or its proof, and nothing
  fails when that statement becomes false.
- **No hierarchy.** The numbered documents are a creation log, not a tree. The
  only parent link is "read 04 first", which is a convention, not a checked
  edge. A subsystem has no single entry, so every reader re-derives one.

## 2. Goals and success criteria

Goal: a person or an agent at any code or document node can reach the owning
subsystem and the top-level design in at most three hops, and every one of those
hops is checked by a gate.

| # | Criterion | How it is measured |
|---|---|---|
| 1 | Every code file has a five-field module contract | `check-modules.py` passes on all non-exempt files; the five labels are `Serves`, `Design`, `Upstream`, `Downstream`, `Verify` |
| 2 | Every function and type has a doc comment | `check-modules.py` reports zero undocumented items; `pub` and non-trivial private items carry `Purpose` plus the five contract fields; trivial private items carry `Purpose` and `Contract:` |
| 3 | Every maintained document has a four-field navigation header | `Up`, `Authority`, `Code`, `Verify`; `Up` resolves and reaches both `docs/design/README.md` and `docs/04-system-overview.md` |
| 4 | No maintained document exceeds its budget | the per-directory limits in §4.2; the gate prints the over-run and fails |
| 5 | A code contract's `Design` and `Verify` anchors exist | every backticked `docs/...` path resolves; every `ADR-NNNN` names a real ADR; `Verify` names a test module or a command |
| 6 | A document's `Code` anchors exist | every backticked `crates/...` path resolves |
| 7 | The change is comments and documents only | `check-comment-only.py` compares the comment-stripped token stream of every changed `.rs` against `HEAD`; no diff for source files |
| 8 | No gate is trusted before it has failed | each new check is run against a seeded defect, and the red run is recorded in the task report |

The chain the criteria build:

```
item  --Purpose/Design-->  subsystem doc  --Up-->  docs/design/README.md  --Up-->  docs/04  -->  docs/00
file  --# Contract----->  subsystem doc
doc   --Code----------->  file  -->  item
```

Every arrow above is either machine-checked or has a documented exception.

## 3. Non-goals

- **No behaviour, API, crate or dependency change.** The only source edit is
  added comment lines. `cargo test --workspace`, `cargo fmt --check` and the
  layering gate must be unchanged.
- **No rewrite of records.** `docs/decisions/*` keep their bodies and `Status:`;
  `docs/research/`, `docs/archive/`, `docs/superpowers/` and
  `docs/human-written/` are history or human input and are never edited. ADRs
  gain one `Up:` line each so the graph is complete; nothing else in them moves.
- **No translation.** `docs/06-context-engineering.md` and ADR-0017 stay
  Chinese and keep their `Language: zh` marker.
- **No per-item five-field block for trivial private functions.** Approved
  choice B: the module contract carries the shared design information, and a
  trivial helper points back with `Contract: <file>`. A full five-field block
  on every ten-line helper would be duplicate prose that rots differently from
  the module it copies.
- **No semantic freshness guarantee.** The gate checks structure and edges, not
  whether a sentence is still true. Reviewing truth stays a reading task.
- **No new crate or runtime dependency.** The gates are Python 3 and POSIX
  shell, like the existing ones.

## 4. Design

### 4.1 Document tree

```
README.md                     newcomer entry: what it is, install, run, status (<=250)
DEVELOPMENT.md                maintainer entry: toolchain, gates, change path, release (<=300)
docs/
  README.md                   the only index; legend for headers and budgets (<=150)
  00-vision.md                why the project exists (<=150)
  02-topics.md                thin entry: topic index and status legend (<=150)
  topics/
    README.md                 all T-NN, one line each, with their status (<=150)
    open.md                   Seed / Discussing topics, detail (<=250)
    decided.md                Promoted / Merged topics, with the deciding ADR (<=250)
  03-architecture.md          AQ-1..AQ-12 decision map; each AQ 3-5 lines -> ADR (<=250)
  04-system-overview.md       the one-read picture of the architecture (<=180)
  05-parity-and-synthesis.md  parity, absorbed lessons, deliberate "no"s (<=250)
  06-context-engineering.md   thin entry, Language: zh, -> context/ (<=60)
  07-extending.md             thin entry -> extending/ (<=60)
  design/
    README.md                 subsystem map, reading paths, Mermaid layer diagram (<=150)
    kernel-loop.md            model+tool loop, steps, nudge, interruption
    context-ledger.md         ledger, compaction, recall; detail -> context/
    tools-permissions.md      tool interface, PDP, confinement
    events-storage.md         schema/protocol, JSONL truth, SQLite index
    config-routing.md         config layers, manifest, provider routing
    extension-model.md        hooks, skills, MCP, plugins, packs
    sessions-lineage.md       session/event tree, fork, resume, export
    surfaces.md               CLI, server, TUI, web, ACP
    control-plane.md          control plane, gateway, org layer
    security-trust.md         trust boundaries, signing, sandbox, approval
    flows/
      turn-lifecycle.md       one turn, end to end
      tool-approval.md        a tool call from proposal to enforcement
      resume-fork.md          reopening and branching a session
      context-compaction.md   when and how the ledger compacts
      pack-install.md         fetch, verify, seat a profile or plugin pack
  context/                    split of the old 06, Chinese preserved
    README.md  principles.md  data-model.md  pipeline.md
    recall.md  failure-modes.md  config.md  rollout.md
  extending/                  split of the old 07
    README.md  builtin-tool.md  event.md  hook.md  routing.md
  runbooks/
    README.md                 what each runbook is for
    control-plane-bootstrap.md  control-plane-operate.md  control-plane-security.md
    windows-build.md
  decisions/  research/  archive/  superpowers/  human-written/
```

Every leaf under `design/` has one job. If a subsystem grows past 250 lines, it
gets a child document and a link, not a longer file.

### 4.2 Navigation header and budgets

Every maintained document starts with a title and four lines:

```markdown
# Kernel loop

- **Up:** [design/README.md](README.md) · [04 System overview](../04-system-overview.md)
- **Authority:** [ADR-0004](../decisions/0004-microkernel-extension-model.md)
- **Code:** `crates/kernel/src/turn.rs`, `crates/kernel/src/turn/loop.rs`
- **Verify:** `cargo test -p loong-kernel` · `./scripts/check.sh`
```

`Up` is the parent edge, and the reachability gate follows it. `Authority` names
the ADR or higher document that decides the topic. `Code` names the files the
document describes. `Verify` names the command that proves them.

Exceptions, each for a stated reason:

- **ADRs** keep `Status:` and gain one `- **Up:**` line pointing at
  `docs/design/README.md`; their body and decision text are untouched.
- **`docs/README.md`** is the root: it has `Code` and `Verify` but no `Up`.
- **`README.md`**, **`DEVELOPMENT.md`** and **`docs/00`** are entry points; their
  `Up` may name `docs/04` (or be the root for `README.md`), and their budgets are
  the entry budgets above.
- **`docs/research/`**, **`docs/archive/`**, **`docs/superpowers/`** and
  **`docs/human-written/`** are records or human input. They are indexed and
  link-checked but need no header and have no length cap.

Length budgets, enforced by directory or file name:

| Class | Budget |
|---|---|
| `docs/README.md`, `docs/00`, `docs/02` entry, `docs/04`, `docs/design/README.md`, `docs/topics/README.md` | 150-200 lines as stated in the tree |
| `README.md` | 250 |
| `docs/06` entry and `docs/07` entry | 60 |
| `docs/design/*.md`, `docs/design/flows/*.md` | 250 |
| `docs/context/*.md`, `docs/topics/*.md`, `docs/03`, `docs/05` | 250 |
| `docs/extending/*.md` | 150 |
| `docs/runbooks/*.md` | 300 |
| `DEVELOPMENT.md` | 300 |
| `docs/decisions/*.md` | 300 (records; only 0017 is close) |

### 4.3 Code contract

**File module contract**, immediately at the top of every non-exempt `.rs` file:

```rust
//! Purpose: The model+tool loop and its step budget.
//!
//! # Contract
//! - **Serves:** a trustworthy turn: every step is an event (ADR-0003/0004).
//! - **Design:** `docs/design/kernel-loop.md`, `docs/design/flows/turn-lifecycle.md`
//! - **Upstream:** `Kernel::run` from `crates/server`; the `run` and `web` CLI arms.
//! - **Downstream:** `Ledger`, `Model`, `ToolRegistry`, `Pdp`, `EventLog`.
//! - **Verify:** `cargo test -p loong-kernel turn::` · `./scripts/check.sh`
```

**Item contract**, above every function and type:

```rust
/// Purpose: Decide whether the loop may take another step.
///
/// Serves: a bounded turn that cannot spin forever (`kernel-loop.md`).
/// Design: `docs/design/kernel-loop.md` (ADR-0004).
/// Up: `Kernel::run` calls this before every model call.
/// Down: `steps_or_unlimited`, `KernelConfig::max_steps`.
/// Verify: `cargo test -p loong-kernel turn::tests::step_budget`.
```

Rules:

- `fn`, `struct`, `enum`, `trait`, `union` and `type` are annotatable. `mod`
  declarations are not: the file's module contract covers them.
- `pub` items and non-trivial private items carry `Purpose` plus the five
  contract fields. A private function is **trivial** when its body — the
  signature line through its closing brace — is at most 12 lines; the gate
  measures that span. It may then use one line:
  `/// Purpose: … . `Contract: turn/events.rs`.`
- Types always carry all five labels, private or public: a type is a contract.
- Test functions carry one `Purpose` line; the test file still has a module
  contract whose `Verify` names the test target.
- Trait method declarations carry one `Purpose` line; the trait itself carries
  the five fields.
- Exempt files are listed in `scripts/contract-exemptions.txt` with a reason:
  `crates/schema/src/generated.rs` (generated) and the two real crate-root
  `build.rs` files. `crates/kernel/src/turn/build.rs` is **not** exempt: it is a
  module named `build`, not a Cargo build script. The gate fails if an exemption
  names a file or item that no longer exists, so the list cannot grow stale.
- A single line `// Contract-exempt: <reason>` may exempt one item, but the
  reason must be at least 10 characters and the count is printed by the gate.

### 4.4 Gates

**`scripts/check-modules.py`** — the code contract gate.

- Reads `git ls-files 'crates/**/*.rs'`, so scratch files never enter the gate.
- Masks comments, strings, chars and raw strings before looking for items, so a
  `fn` inside a string or a doc comment is never counted.
- Finds each item at line start with its indentation, including methods inside
  `impl` blocks. Attributes and doc comments between the doc block and the item
  are allowed.
- Requires a `//!` contract on every non-exempt file with all five labels.
- Requires a doc block on every item. For `pub` and non-trivial private items,
  requires `Purpose:`, `Serves:`, `Design:`, `Up:`, `Down:`, `Verify:`.
  Trivial items require `Purpose:` and either the five labels or `Contract:`.
- Resolves `Design:`: at least one backticked `docs/...md` path must exist, or an
  `ADR-NNNN` must name an existing ADR. Resolves every backticked `crates/...`
  path in `Up:`/`Down:`/`Verify:` when present.
- Prints one line per failure with `file:line` and the missing label, and a
  summary: files, items, documented, full contracts, compact contracts,
  exemptions.

**`scripts/check-docs.py`** — four new checks beside the existing five:

- `headers`: every maintained document has `Up`, `Authority`, `Code`, `Verify`
  in its first 20 lines, except the documented root and record classes. ADRs
  have `Up`.
- `lengths`: every maintained document is within its budget. The check prints
  `path: N > budget` and fails.
- `code-paths`: every backticked `crates/...` path in `Code:` exists, and every
  `docs/...` path in `Design:`/`Up:` exists.
- `reachable`: following `Up:` links from every design document reaches both
  `docs/design/README.md` and `docs/04-system-overview.md`; following `Up:` from
  every context/extending/runbook document reaches `docs/design/README.md` or
  `docs/04-system-overview.md`.

The existing `citations` check moves its topic scan from `docs/02-topics.md` to
every `docs/topics/*.md`, because the T-NN headings move there. The existing
`orphans` check already covers a new direct child of `docs/` once the index has
a row for the first path segment; the three new rows are `design/`, `context/`,
`extending/`.

**`scripts/check-comment-only.py`** — migration evidence, not a permanent gate.

For every `.rs` file changed against `HEAD`, strip comments and whitespace from
both versions and compare the resulting token stream. Any difference fails. The
script is run in each code-annotation phase and its output is recorded in the
task report. It is deliberately not wired into `check.sh`: future work changes
code on purpose, so a permanent comment-only gate would be wrong.

**The seeded-defect rule.** Each check is run once against a deliberate defect
before it is trusted: remove a module-contract label, add an undocumented
function, break a `Design:` path, remove a document header, exceed a budget, and
cut an `Up:` link. The red output is recorded. A check that has never failed has
not been shown to work.

### 4.5 The network invariants

1. **Every source file** names a `docs/design/...` document in `Design`.
2. **Every design document** names at least one existing `crates/...` path in
   `Code`, so the document-to-code edge is never empty.
3. **Every design document** has an `Up` edge, and the transitive closure of
   those edges reaches `docs/design/README.md` and `docs/04`.
4. **The index is the only list.** `docs/README.md` indexes every maintained
   subtree; the orphan check keeps it true.
5. **One authority per topic.** The subsystem documents own mechanisms; ADRs own
   decisions; `docs/04` owns the current architecture; `README.md` owns status.
   Other documents link instead of restating.

### 4.6 Diagrams

- `docs/design/README.md`: a Mermaid `flowchart` of the crates in dependency
  layers, plus the three reading paths (newcomer, maintainer, debugger).
- `docs/design/flows/*.md`: one Mermaid `sequenceDiagram` per flow, with the
  event names and the approval point that the code actually emits.
- `docs/design/events-storage.md`: a Mermaid `flowchart` of session -> event tree
  -> JSONL -> derived index, including what is rebuildable.
- `docs/design/control-plane.md`: a Mermaid `flowchart` of local mode versus
  gateway mode, naming where the key and the policy live.
- Existing ASCII overview diagrams stay; Mermaid is added where a second
  dimension (time or deployment) is being shown.

### 4.7 Document migration map

| Old | New | Rule |
|---|---|---|
| `docs/06-context-engineering.md` (1,059) | `docs/06` entry (<=60) + `docs/context/*` (8 files including the README) | move sections by topic; preserve Chinese; every Chinese file carries `Language: zh`; update every inbound link |
| `docs/07-extending.md` (220) | `docs/07` entry (<=60) + `docs/extending/*` (5 files including the README) | one procedure per file; the old anchors keep working through the entry |
| `docs/02-topics.md` (696) | `docs/02` entry + `docs/topics/{README,open,decided}.md` | every T-NN keeps its number and gets one line in the index; status decisions move with the topic |
| `DEVELOPMENT.md` (1,111) | `DEVELOPMENT.md` (<=300) + `docs/design/*` + `docs/runbooks/*` | keep toolchain, gates, release; move mechanism descriptions to design docs, troubleshooting to runbooks |
| `docs/runbooks/control-plane.md` (539) | `runbooks/README.md` + `control-plane-bootstrap/operate/security.md` | split by when a reader needs it; no content change |
| `docs/03-architecture.md` (507) | same file (<=250) | each AQ keeps the question and the decision in 3-5 lines, then links the ADR; the historical discussion stays in `archive/` |

Every moved section keeps its content. The move is checked by comparing inbound
links before and after (`check-docs.py links`) and by reading the old and new
files side by side in the task that owns the move.

## 5. Verification

Every phase ends with `./scripts/check.sh` green. The migration additionally
needs the following evidence, in the task that owns it:

1. **Document tree.** `python3 scripts/check-docs.py` passes all nine checks;
   `python3 scripts/check-docs.py --check reachable` prints one path per design
   document to `design/README.md` and `docs/04`.
2. **Code contracts.** `python3 scripts/check-modules.py` reports zero missing
   module contracts and zero undocumented items, with the exemption count and
   each exemption named.
3. **Comment-only.** `python3 scripts/check-comment-only.py --base HEAD` reports
   zero source differences for every changed `.rs` file.
4. **Tests unchanged.** `cargo test --workspace --locked` reports the same passed
   and ignored totals as before the change; `README.md` carries the
   machine-readable counts comment.
5. **Sample audit.** Ten items chosen across crates: each `Design:` path exists,
   each `Up:`/`Down:` symbol exists (by `grep`), each `Verify:` command or test
   module exists.
6. **Seeded failures.** Each new check has a recorded red run: missing label,
   undocumented item, broken path, missing header, over-budget document, broken
   `Up`.
7. **No API change.** `git diff -- '**/Cargo.toml'` is empty and
   `cargo tree -p loongagent --depth 1` is unchanged.

Definition of done: all seven hold, `docs/README.md` indexes every new subtree,
and `AGENTS.md` states the rules so the next change starts from a document.

## 6. Sequencing

1. **Principles first.** Add the modularity rules to `AGENTS.md` and the
   conventions to `docs/README.md`. This goes first so every later task is
   judged against the written rule, including the spec itself.
2. **The gates, with a pilot.** Write `check-modules.py`, the four
   `check-docs.py` checks and `check-comment-only.py`; pilot them on
   `crates/kernel` and three documents; prove each check fails on a seeded
   defect. A gate that arrives after the migration cannot protect it.
3. **The document tree.** Split 06, 07, 02, `DEVELOPMENT.md` and the control-plane
   runbook; write the eleven subsystem documents and five flow documents; add
   headers, diagrams and index rows. Documents before code, so the code
   contracts have a real `Design:` target.
4. **The code contracts.** One crate at a time, foundation first
   (`schema`, `protocol`, `eventlog`, `index`), then the kernel and its
   dependencies, then the edges. Each crate: module contracts, then items,
   then `check-modules.py`, then `check-comment-only.py`.
5. **The audit.** Full gate, sample audit, reachability report, final index and
   AGENTS.md read-through.

## 7. Risks

| Risk | Containment |
|---|---|
| 2,422 item blocks become boilerplate that hides real documentation | the module contract carries the shared fields; trivial items get one line; review per crate reads for meaning, not presence |
| The lexical scanner misreads macros, raw strings or attributes and the gate becomes noise | mask strings/comments first; allow attributes between doc and item; prove the scanner on a file with raw strings and nested comments; every false positive gets a fixture in the gate's own tests |
| A document split silently drops a section | move, do not rewrite; compare section headings and word counts before and after; inbound link check |
| `Up:` chains become decorative links that all point at `04` | the reachability gate requires the path through `design/README.md`, and each subsystem document must also name its parent subsystem, not only the root |
| The gate blocks legitimate future code | exemptions are a checked file with reasons; a new exemption is a review decision, and a stale one fails |
| The migration touches uncommitted work in the tree | work on top of the current tree; commit only files this work changes; never reset or stash the human's in-progress changes |
| Comment edits accidentally change code | `check-comment-only.py` after every crate; `cargo fmt --check` and the full suite at every commit |

## 8. Decisions left to the human

- **The uncommitted baseline.** The working tree already holds 21 modified
  documents and scripts plus untracked `AGENTS.md` and
  `scripts/check-human-written.sh` from the previous navigability work. This spec
  and plan build on them. Whether to commit that baseline first, or to keep it
  uncommitted and commit only this work's files, is a human decision.
- **`docs/02` splitting.** The plan splits it into an entry plus `topics/`.
  Keeping one 696-line backlog is the alternative; the split is chosen because
  the file grows with every new idea.
- **Mermaid.** GitHub renders it; Gitea's renderer is newer. If the team's Gitea
  does not render Mermaid, the diagrams fall back to fenced ASCII and the gate
  does not care either way.
