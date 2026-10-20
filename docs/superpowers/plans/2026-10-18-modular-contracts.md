# Modular Contracts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use subagent-driven-development (recommended) or executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Make every code and document node navigable back to its subsystem and the top-level design, and keep those edges true with gates that have been shown to fail.

**Architecture:** Four phases. Phase A writes the rules and the three gate scripts, then pilots the whole chain on one kernel file and three documents. Phase B builds the document tree, because the code contracts need real `Design:` targets. Phase C annotates the code crate by crate. Phase D removes the migration baseline and audits the whole network.

**Tech Stack:** Rust 2021 workspace (24 crates), Python 3 for gates, POSIX shell for `scripts/check.sh`, Markdown with Mermaid diagrams.

**Spec:** `docs/superpowers/specs/2026-10-18-modular-contracts-design.md` — read it first; this plan argues from it.

## Global Constraints

Copied from the spec. Every task includes these.

- **Zero behaviour change, zero `pub` API change, zero crate-boundary change, zero new dependencies.** Source edits are comment lines only.
- **Every `.rs` file gets a five-field module contract** — `Serves`, `Design`, `Upstream`, `Downstream`, `Verify` — in a leading `//!` block, unless it is in `scripts/contract-exemptions.txt` with a reason.
- **Every `fn` / `struct` / `enum` / `trait` / `union` / `type` gets a doc comment.** `pub` and non-trivial private items carry `Purpose` plus all five contract fields. A private function whose body is at most 12 lines may use `Purpose:` plus `Contract: <file>`. Types always carry the full block. Test functions carry one `Purpose` line.
- **`mod` declarations are exempt:** the file module contract covers them.
- **Exempt files:** `crates/schema/src/generated.rs` and the two real crate-root `build.rs` files only. `crates/kernel/src/turn/build.rs` is a module, not a build script, and is **not** exempt. Every exemption carries a reason and a stale exemption fails the gate.
- **Every maintained document gets four header fields** — `Up`, `Authority`, `Code`, `Verify` — except ADRs (which gain only `Up`) and the record classes `research/`, `archive/`, `superpowers/`, `human-written/`.
- **Length budgets:** `README.md` 250; `DEVELOPMENT.md` 300; `docs/README.md` 150; `docs/00` 150; `docs/02` 150; `docs/03` 250; `docs/04` 180; `docs/05` 250; `docs/06` 60; `docs/07` 60; `docs/design/**` 250; `docs/context/**` 250; `docs/topics/**` 250; `docs/extending/**` 150; `docs/runbooks/**` 300; `docs/decisions/**` 300.
- **Language:** `README.md` is Chinese; `docs/**` is English except `docs/06`, `docs/context/**` and ADR-0017, which carry `Language: zh`.
- **Baseline ratchet.** During migration, `scripts/code-contract-baseline.txt` and `scripts/doc-contract-baseline.txt` list files that do not yet comply. The gates skip those files, name them in the summary, and **fail on a stale entry** (an entry that now passes). Each task removes its own entries. Phase D deletes both files.
- **Every new gate check is shown failing on a seeded defect before it is trusted.**
- **`./scripts/check.sh` is green at every commit.** It runs formatting, schema generation, layering, human-written, both new gates, tests and the release build.
- **Commit identity:** `git -c user.name="loongagent" -c user.email="noreply@local" commit`. The snippets below write `git commit`; run each one with that prefix.
- **Commit messages:** `type(scope): a sentence about why`, body naming the problem. One logical change per commit.
- **Do not touch:** `docs/human-written/`; the untracked root files `tank-battle.html`, `.smoke.js`, `:memory:.ses`; gitignored `dist/` and `target/`; the 21 pre-existing uncommitted document edits except where a task explicitly rewrites that file.
- **Test counts:** `README.md` keeps `<!-- test-counts: N passed, M ignored -->` on its own line, with the real suite totals.

## Review Focus

These are the failure modes no single gate catches. Each gets an explicit check in the task that owns it.

1. **The lexical scanner misses or over-counts items inside macros, raw strings or nested block comments.** Task 2's fixtures include all three, and the gate's own tests must fail when the scanner is deliberately blinded.
2. **A `Design:` link exists but points at the wrong subsystem.** Task 5 and the final audit read ten sampled items and check that the named document actually describes the code, not merely that the path exists.
3. **A document split drops a section.** Tasks 12–16 compare the old heading list with the new files and account for every heading.
4. **The `Up:` chain is decorative.** The `reachable` check requires the path through `docs/design/README.md`, and each subsystem document names its parent subsystem, not only `docs/04`.
5. **A comment edit accidentally changes code.** `check-comment-only.py` runs before every code commit; `cargo fmt --check` and the test suite run after it.
6. **The baseline becomes permanent debt.** The gate fails on a stale entry, and Phase D deletes both baseline files.
7. **A moved document breaks inbound links.** The existing `links` check runs after every move; code-span paths, which that check masks, are found by `grep` and updated by hand in the same task.

## Annotation Map

Every code file names one primary design document. This table is the source for the `Design:` field; a file that genuinely spans two names both.

| Crate / area | Primary design document | Secondary |
|---|---|---|
| `schema`, `protocol`, `eventlog`, `index` | `docs/design/events-storage.md` | `sessions-lineage.md` |
| `http` | `config-routing.md` §8 | `control-plane.md` |
| `proc` | `tools-permissions.md` | — |
| `config`, `manifest`, `providers` | `config-routing.md` | `security-trust.md` |
| `sandbox`, `tools` | `tools-permissions.md` | `security-trust.md` |
| `hooks`, `skills`, `mcp`, `plugin`, `pack` | `extension-model.md` | `security-trust.md` |
| `kernel` | `kernel-loop.md` | `context-ledger.md`, `sessions-lineage.md` |
| `context` | `context-ledger.md` | `kernel-loop.md` |
| `server` | `sessions-lineage.md` | `surfaces.md` |
| `cli`, `tui`, `tui-app`, `web` | `surfaces.md` | `sessions-lineage.md` |
| `control` | `control-plane.md` | `security-trust.md` |

## Per-Crate Annotation Procedure

Every Phase C task uses this procedure. The task's Files list is the only input that changes.

- [x] **Step A: Read the design document named in the task's Design column, and the files to annotate.**

- [x] **Step B: Write the module contract at the top of every file in the task.** Five labelled lines exactly as the spec's §4.3 example. `Design` names the task's document as a backticked `docs/...` path. `Verify` names the test target and, where useful, `./scripts/check.sh`.
- [x] **Step B2: Regenerate the crate map.** The map's middle column is each crate's first `//!` line, so after writing the module contract run `python3 scripts/layering_rules.py --emit-map . > docs/crate-map.md` and include `docs/crate-map.md` in the task commit.

- [x] **Step C: Write the item docs.** For each item: `Purpose` first. `pub` items, non-trivial private functions and every type get `Serves`, `Design`, `Up`, `Down`, `Verify`. Trivial private functions get `Purpose` plus `Contract: <file>`. Test functions get one `Purpose` line naming the behaviour they pin. Never restate the signature.

- [x] **Step D: Run the code gate on the task's files.**

Run: `python3 scripts/check-modules.py --paths <comma-separated files>`
Expected: exit 0, `documented N/N`, no missing label.

- [x] **Step E: Prove the source did not change.**

Run: `python3 scripts/check-comment-only.py --base HEAD`
Expected: exit 0, `comment-only: OK (<n> files)`.

- [x] **Step F: Remove the task's files from `scripts/code-contract-baseline.txt`.**

- [x] **Step G: Run the crate tests and the full gate.**

Run: `cargo fmt --all -- --check && cargo test -p <crates> && ./scripts/check.sh`
Expected: `all gates passed`.

- [x] **Step H: Commit.**

```bash
git add <the annotated files> scripts/code-contract-baseline.txt
git commit -m "docs(<scope>): add contracts to <crate>, without changing code"
```

---

## Phase A — rules, gates and the pilot

### Task 1: the rules are written before the work

**Files:**
- Modify: `AGENTS.md`
- Modify: `docs/README.md`

**Interfaces:**
- Produces: the modularity section of `AGENTS.md` and the header/budget conventions in `docs/README.md`. Later tasks quote these rules in their reports.

- [x] **Step 1: Add the modularity section to `AGENTS.md`**

Four short subsections, in the file's plain-English voice:

1. **Documents first.** Before changing code, name the design document that owns the behaviour. If none exists, write or split one first, then the module plan, then the code.
2. **Every node has a contract.** The file module contract, the item block, the five/six labels, the trivial-item compression, the exemption file and the reason rule.
3. **Every document has a path up.** The four header fields, the budgets, the one-authority rule, and the rule that detail links down instead of restating.
4. **The gates are the enforcement.** Name `check-modules.py`, `check-docs.py`, `check.sh`, the seeded-defect rule, and the baseline ratchet with the instruction that a task removes its own baseline entries.

Keep the section under 60 lines. It states rules, not a tutorial; the spec carries the reasoning.

- [x] **Step 2: Update `docs/README.md` conventions**

Add the header schema, the length budgets, the record-class exemptions, and one sentence that `docs/design/` is the subsystem layer and `docs/README.md` remains the only index. Add rows for `design/`, `context/`, `topics/` and `extending/` in the tasks that create those entries, so the index never links a file that does not exist yet.

- [x] **Step 3: Verify and commit**

Run: `python3 scripts/check-docs.py` (should still be green; no new document yet).
Read the new `AGENTS.md` section once. Then commit.

```bash
git add AGENTS.md docs/README.md
git commit -m "docs(agents): state the contract and navigation rules before changing the tree"
```

### Task 2: `scripts/check-modules.py`

**Files:**
- Create: `scripts/check-modules.py`
- Create: `scripts/contract-exemptions.txt`
- Create: `scripts/code-contract-baseline.txt` (generated)
- Create: `scripts/tests/test_check_modules.py`
- Modify: `scripts/check.sh`

**Interfaces:**
- Produces: `check-modules.py [--root DIR] [--paths P1,P2] [--baseline FILE] [--emit-baseline FILE] [--list]`; exit 0 when every in-scope file passes, 1 with one line per failure.
- Produces: the exemption format `path | reason` and `path#item | reason`.

- [x] **Step 1: Write the failing tests**

`scripts/tests/test_check_modules.py`, using `unittest` and temp directories. Fixtures are inline Rust strings. Tests:

- a file with a five-label `//!` block and documented items passes;
- a missing `Serves:` fails with `file:line`;
- an undocumented `fn` fails;
- a `pub fn` with only `Purpose` fails;
- a private 20-line function with only `Purpose` fails (non-trivial);
- a private 5-line function with `Purpose` + `Contract:` passes;
- a `struct` with only `Purpose` fails (types always full);
- an item with `#[derive(...)]` and a blank line between doc and item is still seen;
- a `fn` inside a raw string, a nested block comment and a line comment is ignored;
- a `mod` declaration needs no item block;
- an exempted file passes and a stale exemption fails;
- a `Design:` path that does not exist fails;
- `--emit-baseline` writes the failing paths, and `--baseline` skips them.

Run: `python3 -m unittest scripts.tests.test_check_modules -v`
Expected: FAIL, module not found.

- [x] **Step 2: Implement the scanner and the checks**

Implementation order: mask comments/strings/raw strings while preserving newlines; find items with a line regex including indentation and visibility; find the doc block upward through attributes; measure the body span by brace matching on the masked text; apply the field rules; resolve backticked `docs/...` paths and `ADR-NNNN`; read exemptions and baseline; print the summary.

Key decisions fixed here: item kinds are `fn|struct|enum|trait|union|type`; `mod` is skipped; a private `fn` is trivial when the span from its signature line to its closing brace is ≤12 lines; the five module labels are `Serves`, `Design`, `Upstream`, `Downstream`, `Verify`; the item labels are `Purpose`, `Serves`, `Design`, `Up`, `Down`, `Verify`.

- [x] **Step 3: Run the tests to green**

Run: `python3 -m unittest scripts.tests.test_check_modules -v`
Expected: PASS.

- [x] **Step 4: Create the exemption file and the initial baseline**

`scripts/contract-exemptions.txt`:

```
crates/schema/src/generated.rs | generated by scripts/gen-schema.py; hand edits are overwritten
crates/cli/build.rs | Cargo build script, not application code
crates/control/build.rs | Cargo build script, not application code
```

Generate the baseline from the real tree:

```
python3 scripts/check-modules.py --emit-baseline scripts/code-contract-baseline.txt
```

Expected: the file lists every non-exempt `.rs` that does not yet pass, one path per line.

- [x] **Step 5: Prove the gate fails on a seeded defect**

Temporarily remove the `Serves:` line from `crates/kernel/src/turn/loop.rs`'s `//!` block (it has no block yet, so add a minimal one first, run, then remove one label), or use a fixture copy outside the baseline. Run `check-modules.py --paths crates/kernel/src/turn/loop.rs`.
Expected: exit 1 naming the missing label. Restore.

- [x] **Step 6: Wire it into `scripts/check.sh`**

Add a `bold "code contracts"` step running `python3 scripts/check-modules.py` after the layering step and before the documents step. Add one line to the header comment naming what it catches.

- [x] **Step 7: Run the full gate and commit**

Run: `./scripts/check.sh`
Expected: `all gates passed` (the baseline keeps the old tree green).

```bash
git add scripts/check-modules.py scripts/tests/test_check_modules.py scripts/contract-exemptions.txt scripts/code-contract-baseline.txt scripts/check.sh
git commit -m "gate(code): a contract check with a baseline ratchet, so the old tree can migrate"
```

### Task 3: `check-docs.py` gains headers, budgets, anchors and reachability

**Files:**
- Modify: `scripts/check-docs.py`
- Create: `scripts/tests/test_check_docs.py`
- Create: `scripts/doc-contract-baseline.txt` (generated)
- Modify: `scripts/check.sh`

**Interfaces:**
- Produces: four new `--check` names: `headers`, `lengths`, `code-paths`, `reachable`; the existing `links`, `crate-map`, `orphans`, `citations`, `counts` keep their names.
- Produces: `--emit-baseline FILE` writing the maintained documents that do not yet pass headers or lengths.

- [x] **Step 1: Write the failing tests**

Inline temp git repositories. Tests: a doc with all four fields passes; a missing `Up:` fails; a 260-line design doc fails `lengths`; a `Code:` path that does not exist fails `code-paths`; a design doc whose `Up:` reaches `design/README.md` and `04` passes `reachable`; a broken `Up:` fails; an ADR needs only `Up:`; `research/` is exempt.

Run: `python3 -m unittest scripts.tests.test_check_docs -v`
Expected: FAIL.

- [x] **Step 2: Implement the four checks**

Add them to `CHECKS` and the dispatch. Reuse `tracked_markdown`, `code_free`, `find_links` and `judged_documents`. Parse a header field as the first matching `^\- \*\*Field:\*\*` line within the first 20 lines. Resolve relative links against the document's directory. The `reachable` check is a BFS over `Up:` edges with a visited set and a root set of `docs/design/README.md` and `docs/04-system-overview.md`.

The `citations` check scans `docs/02-topics.md` **and** `docs/topics/*.md` for `^## T-NN` during the migration, so it works before and after Task 14.

- [x] **Step 3: Add the baseline and prove each check fails**

Generate `scripts/doc-contract-baseline.txt` from the current tree. Seed each defect in a scratch document outside the baseline: remove `Up:`, add 20 lines over a budget, break a `Code:` path, cut an `Up:` link. Record the four red runs, then remove the scratch document.

- [x] **Step 4: Wire into `check.sh`, run the full gate and commit**

The documents step already runs `check-docs.py`; the new checks join the default set. Run `./scripts/check.sh`, expected green.

```bash
git add scripts/check-docs.py scripts/tests/test_check_docs.py scripts/doc-contract-baseline.txt scripts/check.sh
git commit -m "gate(docs): check headers, budgets, code anchors and the path back to the top"
```

### Task 4: `scripts/check-comment-only.py`

**Files:**
- Create: `scripts/check-comment-only.py`
- Create: `scripts/tests/test_check_comment_only.py`

**Interfaces:**
- Produces: `check-comment-only.py [--root DIR] [--base REV]`; exit 0 when every changed `.rs` has an identical comment-stripped token stream, 1 with a per-file diff otherwise. Not wired into `check.sh`.

- [x] **Step 1: Write the failing tests**

Temp git repo with a committed file, then a worktree edit that only adds `///` lines (passes), then an edit that changes a literal (fails), then a new file with only comments (passes), then a deleted file (reported, not a crash).

- [x] **Step 2: Implement the token comparison**

Reuse the masking approach from Task 2. Strip comments, then remove all whitespace outside strings. Compare the resulting strings. On mismatch print the first difference with line numbers.

- [x] **Step 3: Prove it red on a seeded change, then green on a no-op**

Edit a function body, run, expect exit 1; restore; run, expect `comment-only: OK`.

- [x] **Step 4: Commit**

```bash
git add scripts/check-comment-only.py scripts/tests/test_check_comment_only.py
git commit -m "gate(source): prove a migration changed comments and nothing else"
```

### Task 5: the pilot — one kernel file, two design documents, one header

**Files:**
- Modify: `crates/kernel/src/turn/loop.rs`
- Create: `docs/design/README.md`
- Create: `docs/design/kernel-loop.md`
- Modify: `docs/04-system-overview.md` (header only)
- Modify: `docs/README.md` (index row for `design/`)
- Modify: `scripts/code-contract-baseline.txt`, `scripts/doc-contract-baseline.txt`

**Interfaces:**
- Consumes: all three gate scripts.
- Produces: the concrete templates every later task copies.

- [x] **Step 1: Write `docs/design/README.md` (≤150 lines)**

Header: Up `../04-system-overview.md`; Authority `../decisions/0004-microkernel-extension-model.md`; Code `crates/kernel/src/lib.rs`; Verify `./scripts/check.sh`. Body: the Mermaid layer diagram (schema at the bottom, surfaces at the top), a table of the eleven subsystem documents, and three reading paths (newcomer, maintainer, debugger). Index only the documents that exist; add rows as later tasks create them.

- [x] **Step 2: Write `docs/design/kernel-loop.md` (≤250 lines)**

Required sections: the turn's inputs and outputs; the step loop; where the model call, tool call, ledger append and event append happen; the step budget and nudge; interruption and resume; the failure modes that end a turn. Include one Mermaid `sequenceDiagram` of a turn. Every claim names a code path; link ADR-0004, ADR-0006 and `../design/flows/turn-lifecycle.md` (created in Task 10). `Code:` names `crates/kernel/src/turn.rs`, `turn/loop.rs`, `turn/model.rs`.

- [x] **Step 3: Annotate `crates/kernel/src/turn/loop.rs`**

Follow the Per-Crate Annotation Procedure with this one file. Its primary `Design:` is `docs/design/kernel-loop.md`. Expect ~14 items.

- [x] **Step 4: Add the header to `docs/04-system-overview.md`**

Up: `00-vision.md`; Authority: `decisions/`, naming ADR-0004 as the architecture anchor; Code: `crates/kernel/src/lib.rs`; Verify: `./scripts/check.sh`. Do not rewrite the body in this task.

- [x] **Step 5: Add the `design/` index row and remove the pilot from both baselines**

`docs/README.md` gains `| [design/](design/README.md) | Subsystem layer: ... |` and the `design/` first-segment row is enough for the orphan check. Remove `crates/kernel/src/turn/loop.rs` from the code baseline. New documents are not in the doc baseline. **Keep `docs/04` in the doc baseline**: it is 207 lines and its 180-line budget is met in Task 17, so removing it now would fail `lengths`.

- [x] **Step 6: Prove the whole chain**

Run:
```
python3 scripts/check-modules.py --paths crates/kernel/src/turn/loop.rs
python3 scripts/check-comment-only.py --base HEAD
python3 scripts/check-docs.py
cargo test -p loong-kernel
./scripts/check.sh
```
Expected: all exit 0. `check-docs.py --check reachable` prints a path from `design/kernel-loop.md` through `design/README.md` to `04`.

- [x] **Step 7: Commit**

```bash
git add crates/kernel/src/turn/loop.rs docs/design/README.md docs/design/kernel-loop.md docs/04-system-overview.md docs/README.md scripts/code-contract-baseline.txt scripts/doc-contract-baseline.txt
git commit -m "docs(pilot): one annotated kernel file and the first design document prove the chain"
```

---

## Phase B — the document tree

Every Phase B task ends with `python3 scripts/check-docs.py && ./scripts/check.sh`, green, and removes the documents it rewrites from `scripts/doc-contract-baseline.txt` once they comply. Headers follow the spec's §4.2. New documents are never added to the baseline.

### Task 6: context and tool/permission design

**Files:**
- Create: `docs/design/context-ledger.md`
- Create: `docs/design/tools-permissions.md`
- Modify: `docs/design/README.md` (two rows)
- Modify: `docs/04-system-overview.md` (links where it describes these)

- [x] **Step 1: Write `context-ledger.md` (≤250)** — the ledger as the only write path; entry kinds and lifetimes; budget and eviction; compaction; recall; what the model sees. Mermaid flowchart of proposal → ledger → model. Authority ADR-0006 and ADR-0017. Code: `crates/kernel/src/ledger.rs`, `context_engine.rs`, `turn/compaction.rs`, `turn/recall.rs`. Up: `README.md` and `../04-system-overview.md`. Detail links to `../context/README.md`.
- [x] **Step 2: Write `tools-permissions.md` (≤250)** — the uniform tool interface; action classes; PDP allow/ask/deny; where enforcement happens; confinement and the `ExecContext`; the built-in tools. Mermaid flowchart of proposal → PDP → execution. Authority ADR-0004 and ADR-0007. Code: `crates/tools/src/lib.rs`, `crates/sandbox/src/`, `crates/kernel/src/turn/confinement.rs`, `crates/proc/src/lib.rs`.
- [x] **Step 3: Add the two rows to `design/README.md`, link from `04` where it already describes the mechanisms, and run the gate.**

```bash
git add docs/design/context-ledger.md docs/design/tools-permissions.md docs/design/README.md docs/04-system-overview.md scripts/doc-contract-baseline.txt
git commit -m "docs(design): the ledger and the permission chokepoint get one home each"
```

### Task 7: events/storage and configuration/routing design

**Files:**
- Create: `docs/design/events-storage.md`
- Create: `docs/design/config-routing.md`
- Modify: `docs/design/README.md`, `docs/04-system-overview.md`

- [x] **Step 1: Write `events-storage.md` (≤250)** — the schema as the source of truth; JSONL as truth; SQLite as a derived index; session versus event tree; the wire protocol on top. Mermaid flowchart of event → JSONL → index and the two tree levels. Authority ADR-0002, ADR-0003, ADR-0015. Code: `crates/schema/protocol.toml`, `crates/protocol/src/lib.rs`, `crates/eventlog/src/lib.rs`, `crates/index/src/lib.rs`.
- [x] **Step 2: Write `config-routing.md` (≤250)** — the layer order; profile packs and their signatures; manifest trust; how the router picks a model; the provider interface. Mermaid flowchart of layers and routing. Authority ADR-0005, ADR-0008, ADR-0016. Code: `crates/config/src/`, `crates/manifest/src/lib.rs`, `crates/providers/src/`.
- [x] **Step 3: Update the design index, link from `04`, gate, commit** with message `docs(design): storage and routing each get one authority`.

### Task 8: extension model and sessions/lineage design

**Files:**
- Create: `docs/design/extension-model.md`
- Create: `docs/design/sessions-lineage.md`
- Modify: `docs/design/README.md`, `docs/04-system-overview.md`

- [x] **Step 1: Write `extension-model.md` (≤250)** — the four extension kinds (hooks, skills/commands, MCP tools, plugins/packs), their trust gates, and the one loader each. Mermaid flowchart of install → trust → load → invoke. Authority ADR-0004, ADR-0007 §7, ADR-0016, ADR-0018. Code: `crates/hooks/src/`, `crates/skills/src/`, `crates/mcp/src/`, `crates/plugin/src/`, `crates/pack/src/lib.rs`. Detail links to `../extending/README.md`.
- [x] **Step 2: Write `sessions-lineage.md` (≤250)** — session contains an event tree; fork and delegation are lineage; resume, retry, steer; export/import (decided, not built). Mermaid flowchart of session → event tree → lineage. Authority ADR-0003, ADR-0014, ADR-0015. Code: `crates/server/src/lib.rs`, `crates/kernel/src/turn/session.rs`, `crates/eventlog/src/lib.rs`.
- [x] **Step 3: Index, link, gate, commit** with message `docs(design): extensions and session lineage get one authority each`.

### Task 9: surfaces, control plane and trust design

**Files:**
- Create: `docs/design/surfaces.md`
- Create: `docs/design/control-plane.md`
- Create: `docs/design/security-trust.md`
- Modify: `docs/design/README.md`, `docs/04-system-overview.md`

- [x] **Step 1: Write `surfaces.md` (≤250)** — CLI, local server, TUI, web, ACP; what each may do; the protocol they share. Mermaid flowchart of clients → server. Authority ADR-0001, ADR-0002, ADR-0009. Code: `crates/cli/src/main.rs`, `crates/server/src/lib.rs`, `crates/tui/`, `crates/tui-app/`, `crates/web/`.
- [x] **Step 2: Write `control-plane.md` (≤250)** — local versus gateway mode; what the control plane serves; the pack signing and org-layer trust; deployment. Mermaid flowchart of the two modes and where the key lives. Authority ADR-0012, ADR-0016. Code: `crates/control/src/`, `crates/pack/src/lib.rs`, `crates/config/src/lib.rs`.
- [x] **Step 3: Write `security-trust.md` (≤250)** — the trust boundaries: generated schema, manifest, packs, hooks, sandbox, secrets, approval. A table of "what is trusted, who signs it, what enforces it". Authority ADR-0007, ADR-0016, ADR-0018. Code: `crates/sandbox/`, `crates/manifest/`, `crates/pack/`, `crates/config/src/secret.rs`.
- [x] **Step 4: Index, link, gate, commit** with message `docs(design): the surfaces, the control plane and the trust boundaries`.

### Task 10: flows — turn lifecycle and tool approval

**Files:**
- Create: `docs/design/flows/turn-lifecycle.md`
- Create: `docs/design/flows/tool-approval.md`

- [x] **Step 1: Write `turn-lifecycle.md` (≤200)** — Mermaid sequence from user intent through context assembly, model call, tool proposal, approval, execution, event append, next step, turn end. Name the exact event kinds and the functions that emit them.
- [x] **Step 2: Write `tool-approval.md` (≤200)** — Mermaid sequence from tool proposal through action-class classification, PDP decision, ask/deny path, confinement, execution, result event. Name the allow/ask/deny outcomes and the event that records the rule that fired.
- [x] **Step 3: Link both from `design/README.md` and the owning subsystem docs; gate; commit** with message `docs(flows): one turn and one tool approval, with the events they emit`.

### Task 11: flows — resume/fork, compaction, pack install

**Files:**
- Create: `docs/design/flows/resume-fork.md`
- Create: `docs/design/flows/context-compaction.md`
- Create: `docs/design/flows/pack-install.md`

- [x] **Step 1: Write `resume-fork.md` (≤200)** — session list → open → rebuild context → attach a client; fork as a child session with lineage.
- [x] **Step 2: Write `context-compaction.md` (≤200)** — trigger, digest, synthesis, ledger replacement, event record; what is reversible.
- [x] **Step 3: Write `pack-install.md` (≤200)** — resolve by name → fetch → verify signature → seat the layer → record; what happens on a bad signature.
- [x] **Step 4: Link, gate, commit** with message `docs(flows): resume, compaction and pack install, each with its failure path`.

### Task 12: split `docs/06` into `docs/context/`

**Files:**
- Create: `docs/context/README.md`, `principles.md`, `data-model.md`, `pipeline.md`, `recall.md`, `failure-modes.md`, `config.md`, `rollout.md`
- Rewrite: `docs/06-context-engineering.md` (≤60)
- Modify: every file that links to `06` (find with `grep -rn '06-context-engineering'`)
- Modify: `docs/README.md` (index row for `context/`)

**Interfaces:**
- Consumes: `docs/design/context-ledger.md` from Task 6 as the Up target for `context/README.md`.

- [x] **Step 1: Capture the old heading list and word count** with `grep -n '^#' docs/06-context-engineering.md` and `wc -w`. Keep it for the coverage check.
- [x] **Step 2: Move sections by topic** — §0–1 → `README.md`; §4 → `principles.md`; §5 → `data-model.md`; §6 → `pipeline.md`; §7 → `recall.md`; §8 → `failure-modes.md`; §9–11.1 → `config.md`; §12–14 → `rollout.md`. Preserve the Chinese prose; do not translate or summarise away detail. Each file gets `Language: zh` under its title and the four header fields. `Up` is `context/README.md` (except the README, which points at `../design/context-ledger.md`); Authority is ADR-0006 and ADR-0017; Code is the matching `crates/context/` or `crates/kernel/` path; Verify is `cargo test -p loong-context` plus the relevant test module.
- [x] **Step 3: Rewrite `docs/06` as a 60-line entry** — `Language: zh`, the four fields, one paragraph, and links to the eight files. Every old anchor that external documents use stays reachable through the entry.
- [x] **Step 4: Check coverage and links** — every heading from Step 1 is either in a new file or deliberately folded into the entry; `python3 scripts/check-docs.py` green; `grep -rn '06-context-engineering' docs README.md DEVELOPMENT.md` shows only working links.
- [x] **Step 5: Remove `docs/06` from the doc baseline, gate, commit** with message `docs(context): split the 1,059-line context design into eight linked documents`.

### Task 13: split `docs/07` into `docs/extending/`

**Files:**
- Create: `docs/extending/README.md`, `builtin-tool.md`, `event.md`, `hook.md`, `routing.md`
- Rewrite: `docs/07-extending.md` (≤60)
- Modify: inbound links, `docs/README.md`

- [x] **Step 1: Move one procedure per file.** `builtin-tool.md`: the `Tool` trait, action classes, registration, the permission declaration, the test. `event.md`: `crates/schema/protocol.toml`, `scripts/gen-schema.py`, the generated files, the `ignorable` rule. `hook.md`: `hooks.toml`, the verdict combination in `crates/hooks/src/run.rs`, the per-repository trust gate. `routing.md`: ADR-0005, `Router`/`RouterRule`/`parse_condition`, the provider layer. Each ≤150 lines, four header fields, `Up` to `extending/README.md` (README to `../design/extension-model.md`).
- [x] **Step 2: Rewrite `docs/07` as a 60-line entry** with the four fields and links to the five files.
- [x] **Step 3: Verify each procedure against the code** — follow the first instruction in each file and confirm the named file does the job. Run the gate.
- [x] **Step 4: Remove `docs/07` from the baseline, commit** with message `docs(extending): four procedures, one file each, checked against the code`.

### Task 14: split `docs/02` into `docs/topics/`

**Files:**
- Create: `docs/topics/README.md`, `open.md`, `decided.md`
- Rewrite: `docs/02-topics.md` (≤150)
- Modify: inbound links, `docs/README.md`

- [x] **Step 1: Inventory all T-NN headings** with `grep -n '^## T-' docs/02-topics.md`; expect T-01…T-26, 26 total.
- [x] **Step 2: Write `topics/README.md`** — a table of all 26: number, one-line title, status (`Seed` / `Wanted` / `Discussing` / `Promoted → ADR-NNNN` / `Merged → T-NN`), and a link to the detail file. Keep the coverage map's intent in one short paragraph.
- [x] **Step 3: Write `open.md` and `decided.md`** — move the full topic text, keeping every `## T-NN` heading verbatim so citations still resolve. `open.md` holds Seed/Wanted/Discussing; `decided.md` holds Promoted/Merged. Each ≤250 lines; if either overflows, split by theme and add a row to the README.
- [x] **Step 4: Rewrite `docs/02` as a 150-line entry** — status legend, the table or a pointer to it, and links. Update `check-docs.py`'s topic scan to read `docs/topics/*.md` (already done in Task 3 for both locations).
- [x] **Step 5: Verify 26 topics, no duplicates, every status has an ADR that exists; gate; remove from baseline; commit** with message `docs(topics): a 696-line backlog becomes an index and two detail files`.

### Task 15: `DEVELOPMENT.md` becomes the maintainer entry

**Files:**
- Rewrite: `DEVELOPMENT.md` (≤300)
- Modify: `docs/design/*` and `docs/runbooks/*` only if a moved section has no destination yet

- [x] **Step 1: Inventory headings and keep a map** of where each goes: toolchain/build/gates/release stay; mechanism descriptions go to the design document named in the Annotation Map; control-plane operation goes to the runbooks; troubleshooting goes to `runbooks/`.
- [x] **Step 2: Rewrite to ≤300 lines** — the four header fields, the toolchain, the one-command gate, how to run a focused test, how to change code (the documents-first path), release, and links. No duplicated mechanism prose: link.
- [x] **Step 3: Verify every removed section has a destination** — compare the Step 1 map with the new files; `grep` for any command that disappeared and confirm it exists in a runbook.
- [x] **Step 4: Gate, remove from baseline, commit** with message `docs(dev): the maintainer guide keeps the commands and links the mechanisms`.

### Task 16: split the control-plane runbook

**Files:**
- Create: `docs/runbooks/README.md`, `control-plane-bootstrap.md`, `control-plane-operate.md`, `control-plane-security.md`
- Rewrite: `docs/runbooks/control-plane.md` (≤60, an entry that preserves inbound links)
- Modify: inbound links

- [x] **Step 1: Split by when a reader needs it** — bootstrap (stand it up), operate (run it day to day), security (keys, signing, what is trusted). Preserve commands verbatim; no prose rewrite.
- [x] **Step 2: Write `runbooks/README.md`** — one line per runbook, including `windows-build.md`.
- [x] **Step 3: Keep `control-plane.md` as a 60-line entry** with links, so every existing link and muscle-memory path still works.
- [x] **Step 4: Gate, remove from baseline, commit** with message `docs(runbooks): bootstrap, operate and security are three different jobs`.

### Task 17: trim `docs/03` and header the remaining design documents

**Files:**
- Rewrite: `docs/03-architecture.md` (≤250)
- Modify: `docs/00-vision.md`, `docs/02-topics.md`, `docs/04-system-overview.md`, `docs/05-parity-and-synthesis.md`, `docs/06-context-engineering.md`, `docs/07-extending.md` (headers and links)

- [x] **Step 1: Trim `03`** — keep the AQ-1…AQ-12 map, the dependency order and each decision in 3-5 lines with its ADR link. The options and trade-offs live in the ADRs and the archived discussion; do not restate them. Target ≤250 lines.
- [x] **Step 2: Add the four fields to `00`, `02`, `04`, `05`, `06`, `07`** and fix their links to the new design/topics/context/extending entries. `00`'s Up is `../README.md`; `02`'s Up is `04-system-overview.md` and `topics/README.md`'s Up is `../02-topics.md` (that direction keeps the chain acyclic); `05`'s Up is `04`; `06`'s Up is `context/README.md`; `07`'s Up is `extending/README.md`.
- [x] **Step 3: Check the numbers in the documents** — ADR range, topic range, test counts, crate count. Correct only what is false; the gate catches impossible citations, not stale prose.
- [x] **Step 4: Gate, remove the six documents from the baseline, commit** with message `docs(arch): the decision map keeps the decisions and links the reasoning`.

### Task 18: ADR navigation and the one index

**Files:**
- Modify: all 18 `docs/decisions/*.md` (one `Up:` line each)
- Modify: `docs/README.md` (all new rows, conventions, budgets)
- Modify: `README.md` (header and links; ≤250)

- [x] **Step 1: Add one `Up:` line to each ADR**, immediately below `Status:`, pointing at `../design/README.md`. Do not touch `Status:` or the body.
- [x] **Step 2: Update the index** — rows for `design/`, `design/flows/`, `context/`, `topics/`, `extending/`, the three control-plane runbooks, and the new top-level entries. The index explains the header schema and budgets in two lines.
- [x] **Step 3: Update `README.md`** — four header fields (Up may be the root), links to the design entry and the status authority, and the test-counts comment. Keep it ≤250 lines; move installation detail only if it is duplicated in a runbook.
- [x] **Step 4: Run `check-docs.py --check reachable`; fix every chain; gate; remove the ADRs and index from the baseline; commit** with message `docs(index): every document now has one way up and one way in`.

### Task 19: documents audit

**Files:**
- Delete: `scripts/doc-contract-baseline.txt` (after it is empty)
- Modify: anything the audit finds

- [x] **Step 1:** Ensure the doc baseline is empty, then delete it. The gate now checks every maintained document.
- [x] **Step 2:** `python3 scripts/check-docs.py --check headers`, `--check lengths`, `--check code-paths`, `--check reachable` — all green.
- [x] **Step 3:** Prove the checks still fail on seeded defects: remove one `Up:`, push a design doc 10 lines over budget, break one `Code:` path, cut one `Up:`. Restore after each.
- [x] **Step 4:** Read the design map end to end: every row links a file that exists; every flow's participants are real; no document exceeds its budget.
- [x] **Step 5:** `./scripts/check.sh` green; commit `docs(audit): the document tree is complete and the baseline is gone`.

---

## Phase C — the code contracts

Every task follows the Per-Crate Annotation Procedure. The Files list is exact; item counts are a size guide, not a quota. Generated files are never edited.

### Task 20: schema and protocol

**Files:** `crates/schema/src/lib.rs`, `crates/schema/tests/*.rs`, `crates/protocol/src/lib.rs` (107 items)
**Design:** `docs/design/events-storage.md`
**Verify:** `cargo test -p loong-schema -p loong-protocol`

- [x] **Step 1:** Follow the procedure. `crates/schema/src/generated.rs` stays untouched and exempt. Remove the three schema files and protocol from the code baseline.
- [x] **Step 2:** Gate, comment-only, tests, full gate, commit: `docs(schema): contracts for the wire schema and the JSON-RPC envelope`.

### Task 21: eventlog and index

**Files:** `crates/eventlog/src/lib.rs`, `crates/index/src/lib.rs` (54 items)
**Design:** `docs/design/events-storage.md`
**Verify:** `cargo test -p loong-eventlog -p loong-index`

- [x] **Step 1:** Procedure; remove both from the baseline.
- [x] **Step 2:** Commit: `docs(storage): contracts for the JSONL truth and the derived index`.

### Task 22: http, proc, manifest, pack, web

**Files:** `crates/http/src/lib.rs`, `crates/proc/src/lib.rs`, `crates/manifest/src/lib.rs`, `crates/pack/src/lib.rs`, `crates/web/src/lib.rs` (139 items)
**Design:** `docs/design/config-routing.md` §8 + `docs/design/control-plane.md` (http), `tools-permissions.md` (proc), `security-trust.md` (manifest, pack), `surfaces.md` (web)
**Verify:** `cargo test -p loong-http -p loong-proc -p loong-manifest -p loong-pack -p loong-web`

- [x] **Step 1:** Procedure; remove the five from the baseline.
- [x] **Step 2:** Commit: `docs(util): contracts for transport, child processes, trust and the web surface`.

### Task 23: config

**Files:** `crates/config/src/{lib,profile,layer,catalog,paths,merge,secret,tests}.rs` (140 items)
**Design:** `docs/design/config-routing.md`
**Verify:** `cargo test -p loong-config`

- [x] **Step 1:** Procedure; remove the eight files from the baseline.
- [x] **Step 2:** Commit: `docs(config): contracts for the layer order, profiles and the secret path`.

### Task 24: sandbox and tools

**Files:** `crates/sandbox/src/*.rs`, `crates/tools/src/{lib,search}.rs` (171 items)
**Design:** `docs/design/tools-permissions.md`
**Verify:** `cargo test -p loong-sandbox -p loong-tools`

- [x] **Step 1:** Procedure; remove the files from the baseline.
- [x] **Step 2:** Commit: `docs(tools): contracts for the tool interface and the enforcement point`.

### Task 25: providers and mcp

**Files:** `crates/providers/src/*.rs`, `crates/mcp/src/*.rs` (142 items)
**Design:** `docs/design/config-routing.md` (providers), `extension-model.md` (mcp)
**Verify:** `cargo test -p loong-providers -p loong-mcp`

- [x] **Step 1:** Procedure; remove from the baseline.
- [x] **Step 2:** Commit: `docs(models): contracts for provider adapters and external tools`.

### Task 26: hooks and skills

**Files:** `crates/hooks/src/*.rs`, `crates/skills/src/*.rs` (142 items)
**Design:** `docs/design/extension-model.md`
**Verify:** `cargo test -p loong-hooks -p loong-skills`

- [x] **Step 1:** Procedure; remove from the baseline.
- [x] **Step 2:** Commit: `docs(extensions): contracts for hooks and the one skill loader`.

### Task 27: plugin and control

**Files:** `crates/plugin/src/lib.rs`, `crates/control/src/lib.rs`, `crates/control/src/bin/loongagent-control.rs`, `crates/control/tests/*.rs` (168 items; `build.rs` exempt)
**Design:** `docs/design/extension-model.md` (plugin), `control-plane.md` (control)
**Verify:** `cargo test -p loong-plugin -p loong-control`

- [x] **Step 1:** Procedure; remove from the baseline. Leave `crates/control/build.rs` exempt.
- [x] **Step 2:** Commit: `docs(plugins): contracts for install-by-name and the control-plane client`.

### Task 28: kernel core

**Files:** `crates/kernel/src/{lib,ledger,context_engine}.rs`, `crates/kernel/src/turn.rs`, `crates/kernel/src/turn/{build,compaction,confinement,events,hooks,model,nudge,recall,session}.rs` (181 items excluding tests)
**Design:** `docs/design/kernel-loop.md` (primary), `context-ledger.md`, `sessions-lineage.md`
**Verify:** `cargo test -p loong-kernel`

- [x] **Step 1:** Procedure. `turn/build.rs` is a module and gets a module contract; it is not the Cargo build script. Remove the files from the baseline.
- [x] **Step 2:** Commit: `docs(kernel): contracts for the turn, the ledger and the context engine`.

### Task 29: kernel tests

**Files:** `crates/kernel/src/turn/tests.rs` (119 items, all test functions)
**Design:** `docs/design/kernel-loop.md`
**Verify:** `cargo test -p loong-kernel`

- [x] **Step 1:** Procedure with the test rule: one `Purpose` line per test, naming the behaviour and the regression it pins. The file still gets a module contract whose `Verify` is `cargo test -p loong-kernel turn::tests`.
- [x] **Step 2:** Commit: `docs(kernel): name what each turn test pins, not just that it exists`.

### Task 30: context core

**Files:** `crates/context/src/{lib,store,summaries,summarizer,materialize,digest,worker,synthesis,tail}.rs` (196 items)
**Design:** `docs/design/context-ledger.md`, detail `docs/context/data-model.md` and `pipeline.md`
**Verify:** `cargo test -p loong-context`

- [x] **Step 1:** Procedure; remove from the baseline.
- [x] **Step 2:** Commit: `docs(context): contracts for the reversible memory pipeline`.

### Task 31: context tests

**Files:** `crates/context/tests/*.rs` (130 items)
**Design:** `docs/design/context-ledger.md`
**Verify:** `cargo test -p loong-context`

- [x] **Step 1:** Procedure with the test rule; remove from the baseline.
- [x] **Step 2:** Commit: `docs(context): each memory test says which invariant it protects`.

### Task 32: server and tui

**Files:** `crates/server/src/lib.rs`, `crates/tui/src/lib.rs` (128 items)
**Design:** `docs/design/sessions-lineage.md` (server), `surfaces.md` (tui)
**Verify:** `cargo test -p loong-server -p loong-tui`

- [x] **Step 1:** Procedure; remove from the baseline.
- [x] **Step 2:** Commit: `docs(surfaces): contracts for the local server and the terminal client`.

### Task 33: cli core

**Files:** `crates/cli/src/{main,local,session,control,plugin,config_cmd,context,tui,web,repl,hooks,mcp,sandbox,headless,server}.rs` (~220 items; `build.rs` exempt)
**Design:** `docs/design/surfaces.md` (primary), `control-plane.md`, `sessions-lineage.md`
**Verify:** `cargo test -p loongagent`

- [x] **Step 1:** Procedure; remove the files from the baseline. Leave `crates/cli/build.rs` exempt.
- [x] **Step 2:** Commit: `docs(cli): contracts for every command arm and its wiring`.

### Task 34: cli integration tests

**Files:** `crates/cli/tests/*.rs`, `crates/cli/tests/common/mod.rs` (~87 items)
**Design:** `docs/design/surfaces.md`
**Verify:** `cargo test -p loongagent --tests`

- [x] **Step 1:** Procedure with the test rule; remove from the baseline.
- [x] **Step 2:** Commit: `docs(cli): the end-to-end tests name the user journey they protect`.

### Task 35: tui-app core

**Files:** `crates/tui-app/src/{lib,wizard,mention,setup,surface,worker}.rs` (~130 items)
**Design:** `docs/design/surfaces.md`
**Verify:** `cargo test -p loong-tui-app`

- [x] **Step 1:** Procedure; remove from the baseline.
- [x] **Step 2:** Commit: `docs(tui): contracts for the full-screen surface, its wizard and its worker`.

### Task 36: tui-app tests

**Files:** `crates/tui-app/src/tests.rs` (173 items)
**Design:** `docs/design/surfaces.md`
**Verify:** `cargo test -p loong-tui-app`

- [x] **Step 1:** Procedure with the test rule; remove from the baseline.
- [x] **Step 2:** Commit: `docs(tui): each surface test says which rendering promise it holds`.

---

## Phase D — the audit

### Task 37: the whole network, with no baseline

**Files:**
- Delete: `scripts/code-contract-baseline.txt` (after it is empty)
- Modify: anything the audit finds

- [x] **Step 1: Empty and delete the code baseline.** Run `python3 scripts/check-modules.py`; expect zero missing contracts and zero undocumented items, with the three exemptions named.
- [x] **Step 2: Prove the gate still fails on a seeded defect.** Remove a `Serves:` label from one file, add an undocumented `fn`, break one `Design:` path; record each red run; restore.
- [x] **Step 3: Run the comment-only proof across the whole migration.** `python3 scripts/check-comment-only.py --base d4af709` (the branch point); expect exit 0 or a report that names only the files whose *documentation* changed. If any file differs in code, stop and fix.
- [x] **Step 4: Run the full gate.** `./scripts/check.sh`; expect `all gates passed`, same test counts as before.
- [x] **Step 5: Sample ten items across crates.** For each: the `Design:` document exists and describes the code; the `Up:` and `Down:` symbols exist (`grep -n`); the `Verify:` command or test module exists. Record the ten in the commit body.
- [x] **Step 6: Read `AGENTS.md`, `docs/README.md` and `docs/design/README.md` end to end.** Confirm the written rule matches what the gates enforce; fix either the rule or the gate if they differ.
- [x] **Step 7: Commit** with message `docs(audit): every node has a checked path back to the design`.

---

## Self-Review

**Spec coverage:** §4.1 tree → Tasks 5–18; §4.2 headers/budgets → Tasks 3, 17–19; §4.3 code contract → Tasks 2, 5, 20–36; §4.4 gates → Tasks 2–4; §4.5 network invariants → Tasks 3, 5, 19, 37; §4.6 diagrams → Tasks 5–11; §4.7 migration map → Tasks 12–16; §5 verification → each phase's final step plus Task 37; §6 sequencing → phases A–D; §7 risks → Review Focus and the seeded-defect steps; §8 decisions → the baseline is the plan's ratchet, `docs/02` is split, Mermaid is used.

**Step scan:** every step names an action and a checkable result. The code tasks use one shared procedure instead of repeating eight steps fifteen times; the procedure is defined once and each task supplies only its files, design document and test command.

**Type consistency:** script names and flags are `check-modules.py` (`--paths`, `--baseline`, `--emit-baseline`), `check-docs.py` (`--check headers|lengths|code-paths|reachable`), `check-comment-only.py` (`--base`). Baseline files are `scripts/code-contract-baseline.txt` and `scripts/doc-contract-baseline.txt`. The field names are `Purpose`, `Serves`, `Design`, `Up`, `Down`, `Verify` at item level and `Serves`, `Design`, `Upstream`, `Downstream`, `Verify` at file level.

**Review Focus:** each item in the list is pinned to a task: scanner fixtures (Task 2), wrong-subsystem links (Tasks 5 and 37), dropped sections (Tasks 12–16), decorative `Up` (Task 3's reachability), code edits (Tasks 4 and 37), baseline debt (Tasks 2, 3, 19, 37), broken inbound links (Tasks 12–16).

**Proportion:** the plan decides files, gates, interfaces, budgets and test commands; it does not write the documents' prose or the annotation bodies. Those are the implementer's work, guided by the spec and the code.
