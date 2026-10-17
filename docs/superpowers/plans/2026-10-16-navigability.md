# Navigability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use subagent-driven-development (recommended) or executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the repository's answers findable in one hop and unable to silently rot, by giving every topic exactly one authority, adding a documentation gate that catches rot, correcting today's false statements, removing the redundancy that costs nothing, and splitting four oversized files with zero behaviour and zero API change.

**Architecture:** Five phases. Phase A builds the gate first, so it protects every later change including its own. Phases B–D make the documents true and single-sourced. Phase E removes redundancy that needs no API change. Phase F does the four mechanical splits, each as its own commit with four independent proofs that nothing but placement changed.

**Tech Stack:** Rust 2021 workspace (24 crates, edition 2021, `rust-version = 1.80`), Python 3 for the existing gate scripts, `bash` for `scripts/check.sh`.

**Spec:** `docs/superpowers/specs/2026-10-16-navigability-design.md` — the plan argues from it; read both.

## Global Constraints

Copied from the spec. Every task's requirements include these.

- **Zero behaviour change.** No change to what any program computes, prints or writes.
- **Zero `pub` API change.** The splits may add `pub(crate)` / `pub(super)` and `pub use` re-exports only. `pub(crate)` and `pub(super)` cannot reach a public surface.
- **Zero crate boundary change.** No new crate, no removed crate, no new edge between crates.
- **Inside moved code, the only lines that may be added are `mod`, `use`, `pub use`, `pub(crate)` and `pub(super)`.** Every moved *item* — function, `impl`, `struct`, `enum`, `trait`, `const`, `static`, `type`, `mod`, `macro_rules!` — is token-identical, including its attributes, its visibility and its signature.

*Corrected during execution (Task 19).* This read "function bodies are byte-identical", which is **unachievable** and was wrong in two independent ways:

1. **Byte-identity is impossible for any move that changes indentation depth.** rustfmt dedents and reflows the moved text, and `cargo fmt --check` is a gate, so the moved copy *cannot* be byte-equal to the original. Task 19's move (a nested `mod tests` becoming a sibling file) showed exactly this: of 266 bodies, 265 are byte-identical and all 266 are token-identical, the difference being two whitespace-only hunks — one of them a signature's optional trailing comma, which rustfmt removed. The achievable and meaningful standard is that the moved text equals rustfmt's output applied to a byte-preserving dedent of the original; Tasks 19–23 each demonstrate that directly rather than by argument.
2. **Hashing bodies left the things that matter uncovered.** A signature change, a visibility change, an added or removed attribute, or a change to any non-`fn` item would all have passed a body-hash comparison while breaking the plan's "zero `pub` API change" constraint — and a signature change shipped with updated callers compiles and passes every test. The evidence therefore hashes whole items — `fn`, `impl`, `struct`, `enum`, `union`, `trait`, `type`, `const`, `static`, `mod`, `use` and `macro_rules!`, with attributes, visibility and signature — and compares `(sha256, kind, name, visibility)` tuples, so a visibility or kind change is visible in the comparison rather than merely implied by a hash.

**The comparison is directional, and strict equality is only Task 19's case.** Task 19 moved an entire `mod tests` into a sibling file, adding nothing, so its before and after item sets are equal and can be compared as multisets. Tasks 20–23 cannot be: splitting a file into submodules legitimately **adds** `mod X;` declarations, `use` / `pub use` lines, and sometimes a `pub(crate)` / `pub(super)` widening on an item that a sibling module now has to reach. Requiring set equality there would either fail a correct split or, worse, pressure an implementer into avoiding a widening the code needs. The rule for those tasks is:

- **Every item present before is still present**, matched on `(kind, name)`.
- **Its hash is unchanged**, *or* the only difference is a visibility widening — which must be demonstrated, not asserted: the script also emits a **visibility-stripped hash**, and a widened item matches its original on that hash. A visibility change that alters anything else fails this and is a finding, not a widening.
- **Every item present after but not before is on an explicit list** in the task's report, one line each, saying what it is (`mod`, `use`, `pub use`) and why the split needed it. An unexplained extra is a finding.

**The item model has three granularities, and the splits need all three** (Ruling 50, Task 20; premise and figures corrected in Rulings 51 and 52). A free function is one item. A **method inside an `impl` block is also one item**, named for its type (`Kernel::build`), because the splits move methods as well as free functions and re-form the containers around them. Finally an `impl` **container** is a third thing, hashed over its attributes and header only (`impl Kernel`, generics, where-clause) and never its methods, so that re-creating `impl Kernel` in each new file yields identical containers instead of eleven "changed" ones. A container is the **only** item allowed to become several; every copy must hash identically and `compare` must name each.

The measured shape, from the tool itself: `crates/kernel/src/turn.rs` holds **86 methods across 16 `impl` blocks** (10 of them top-level) plus 118 free functions, and its single `impl Kernel` block alone carries 68 methods which Task 22 distributes across files. `crates/cli/src/main.rs` holds 19 methods across 9 blocks plus 193 free functions; `crates/config/src/lib.rs` 18 methods across 10 blocks plus 76. Free functions and methods both move, and wherever methods leave a container the container is re-created — which is why the model cannot have only one granularity.

*The premise was wrong the first time and is corrected here.* This paragraph originally asserted that the tool "could not see a single" method — that `fn` counted only free functions. **That was false.** The tool did extract methods as `fn` items; the controller's check for it used `grep -E '\tmethod\t'` and `grep -E` does not interpret `\t` as a tab, so it matched a literal `t` and returned zero for a pattern that was present. The two real defects were narrower and are why the model changed anyway: methods were named **without their type**, so two same-named methods in different `impl` blocks were indistinguishable in a `(kind, name)` comparison; and a container was hashed over its **whole body**, so distributing one `impl Kernel` across files re-formed it into several items whose hashes all differ — which the addition interlock would reject, showing a correct Task 22 as one `CHANGED impl Kernel` plus roughly ten unpermitted `ADDED impl` entries. The implementer recorded the measurement instead of the claim, which is how the error was caught — and then the corrected paragraph carried **wrong figures of its own**: it said "183 methods in 10 blocks" where the tool measures 86 across 16. That number came from a second bad count, `grep -cE '^    (pub…)?fn '`, which sweeps up the free functions inside inline `mod tests`. Both of the controller's counts were wrong in different ways; only the tool's measurement was right.

**`reflow` applies to a contiguous block move; it does not apply to a method distribution.** Where a whole range moves between files — Task 19's `mod tests`, Task 20's four blocks — `reflow` proves rustfmt's reflow is the whole difference. Where a file is assembled from scattered methods, no `--start/--end` range describes it, so the evidence is the item identity plus the `formatting` gate, and each task's report says which of the two it is. Stretching `reflow` to cover the second case would weaken what it asserts about the first.

**And rustfmt can change a token stream, so "token-identical" is not unconditional** (Ruling 55, Task 22). `cargo fmt` is not purely cosmetic on every construct: Task 22's moved `mod tests` had a trailing comma dropped from a `use` group and a block-bodied `find_map` arm collapsed to `=> …,`, both of which alter an item's canonical tokens. A pure, correct move therefore *can* report LOST or CHANGED against item identity. The rule that resolves it: **a contiguous block whose reflow legitimately alters tokens is evidenced by `reflow`** (whole-file byte equality after dedent, header and widenings), **not** by item identity — and because one commit cannot carry both kinds of evidence honestly, Task 22 is two commits, the tests move first. A task that finds this in its own split should say so and separate the commits rather than bending the comparison; and it must not use it as a general excuse, because a *distribution* has no reflow to fall back on and its item identity is the whole proof.

**The direction is enforced in code, not by convention** (Ruling 49, Task 20). The only permitted visibility transitions are **private → `pub(crate)` or `pub(super)`**. A transition that reaches `pub` is a widening *into the public API*, which this plan forbids outright, and it will compile — an empty `git diff --stat crates/cli/` catches it only if a consumer happens to use the item, so the compiler and the CLI check cannot be relied on. `move_evidence.py compare` takes two `items` captures and prints the classification (exact, widened with both hashes, added with kind, lost), exiting non-zero on a lost item, on an unclassified or unlisted addition, on a hash that moved without an allowed widening, and on any forbidden visibility transition. A widening must still show a changed field 1 with a matching field 6. Tasks 21–23 use it instead of a per-task script: the comparison is evidence, and evidence that only one task can run is not evidence.

Cost if wrong: a widening whose vis-token rewrite accidentally took a `&mut` with it would match on `(kind, name)` and pass on the stripped hash only if the signature were otherwise identical — which is exactly what the stripped hash checks, so the residual risk is a change *inside* the visible-signature text, which the full hash covers for all non-widened items.
- **No `Cargo.toml` diff in any split commit** (`git diff -- '**/Cargo.toml'` is empty for Phase F).
- **Every new gate check must be shown failing on a seeded defect before it is trusted.** A check that has never failed has not been shown to work.
- **`./scripts/check.sh` is green at every commit.** It gates formatting, `gen-schema.py --check`, `check-layering.sh`, `cargo test --workspace --locked`, and a release build.
- **Language:** `README.md` is Chinese. `DEVELOPMENT.md`, `docs/**` and `docs/decisions/**` are English, **except** `docs/06-context-engineering.md` and `docs/decisions/0017-layered-context.md`, which are Chinese and get a `Language: zh` line.
- **ADRs keep their `Status:` line.** Every other document loses its `Status` / `Last updated` line; status lives in the `docs/README.md` index.
- **Machine-readable test counts** live in `README.md` as an HTML comment of exactly this shape, on its own line:
  `<!-- test-counts: 736 passed, 11 ignored -->`
- **Commit identity:** `git -c user.name="loongagent" -c user.email="noreply@local" commit`.
- **Commit messages:** `type(scope): a sentence about why`, with a body that names the problem. One logical change per commit.
- **Do not touch:** the untracked root files `tank-battle.html`, `.smoke.js`, `:memory:.ses`; the gitignored local artefacts under `dist/` and `target/`.

## Review Focus

Six things the spec implies that no gate will catch, so each gets a test in the task that owns the code.

1. **A moved document's relative links break.** `git mv docs/01-landscape-synthesis.md docs/archive/` silently breaks every `research/...` link inside it. Task 10 fixes the links as part of the move and proves the gate saw them before and after.
2. **A body-hash extractor that matches nothing passes vacuously.** Brace matching can extract zero bodies and report "identical". Task 19 requires `--min` and a step that proves the script fails when `--min` exceeds what it found.
3. **The test-name multiset is not a set.** `named` is defined both in `crates/cli/src/main.rs`'s test module and in `crates/kernel/src/turn.rs`'s, so a global set of names would collapse two tests into one. The comparison must stay per target — and cargo's `Running … (target/debug/deps/<name>-<hash>)` lines cannot be compared raw, because the hash changes for reasons unrelated to the move. Normalise them to the binary name first.
4. **`cargo` does not warn about declared-but-unused dependencies.** Removing `serde` from six crates cannot be verified by the compiler. Task 16 verifies by `cargo tree -p <crate>` no longer naming `serde`.
5. **Stripping a document's status before the index records it loses the information.** Task 13 writes the index first, then removes the headers.
6. **The gate cannot detect stale prose** — only impossible citations, broken links, orphan documents, a stale crate map and wrong counts. A sentence that is merely out of date still passes. That is why Phase B exists and is reviewed by reading, not by a green gate, and why Phase B must not be skipped on the grounds that the gate is green.

---

## Phase A — the gate

### Task 1: `scripts/check-docs.py` and the relative-link check

**Files:**
- Create: `scripts/check-docs.py`
- Modify: `scripts/check.sh`

**Interfaces:**
- Produces: `scripts/check-docs.py [--root DIR] [--check NAME] [--test-counts FILE]`, exit 0 when every requested check passes and 1 with one line per failure otherwise. `--root` defaults to the repository root; `--check` runs one check and defaults to all. Later tasks add checks to this same file behind the same interface.

- [ ] **Step 1: Write the check, and a seeded defect to prove it fails**

Create `scripts/check-docs.py` with one check, `links`: walk `--root` for tracked `.md` files (use `git -C root ls-files '*.md'` so untracked scratch piles are never scanned), parse every `[text](target)`, skip `http:`, `https:`, `mailto:` and `#` targets, and fail for any other target where `os.path.normpath(join(dirname(file), target))` does not exist. Ignore anything inside fenced code blocks (triple backticks) and inline code spans, so a document may *show* a broken link without failing.

- [ ] **Step 2: Prove it catches a real defect**

Temporarily change the first relative link in `DEVELOPMENT.md` to point at a non-existent file.
Run: `python3 scripts/check-docs.py --check links`
Expected: exit 1, one line naming `DEVELOPMENT.md` and the missing target.
Then restore the file with `git checkout -- DEVELOPMENT.md`.

*Corrected during execution (Ruling 9): the first draft named `docs/runbooks/windows-build.md`, which
contains zero markdown links — its paths are code spans and table cells — so the seed was impossible
there. `DEVELOPMENT.md` is the file the spec's §4.3 says this check was designed around, and it is
the file whose one broken link started this work.*

- [ ] **Step 3: Prove it passes on the real tree**

Run: `python3 scripts/check-docs.py --check links`
Expected: exit 0, no output. (The tree has 0 broken relative links as of 2026-10-16, after `4add563` fixed the one in `DEVELOPMENT.md`.)

- [ ] **Step 4: Wire it into the gate**

In `scripts/check.sh`, add a `bold "documents"` step running `python3 scripts/check-docs.py` immediately after the `check-layering.sh` step, and add a line to the file's header comment listing this gate and what it has caught.

- [ ] **Step 5: Run the whole gate and commit**

Run: `./scripts/check.sh`
Expected: `all gates passed`, exit 0.

```bash
git add scripts/check-docs.py scripts/check.sh
git commit -m "docs: a gate for the documents, because nothing checked 10 700 lines of them"
```

### Task 2: the crate map is generated, not hand-copied

**Files:**
- Modify: `scripts/layering_rules.py`, `scripts/check-docs.py`, `DEVELOPMENT.md` (the `## Layout` table), `docs/README.md` (one index row)
- Create: `docs/crate-map.md`

*Corrected during execution (Ruling 12): the file list omitted `docs/README.md`. This task creates a
new direct child of `docs/`, which Task 3's forthcoming orphan check requires to be indexed, so the
task that creates a document adds its own index row — the same rule as Task 15's `docs/07-extending.md`
(Ruling 6). Without it, Task 3's Step 2 and Step 4 predictions would both have been false.*

**Interfaces:**
- Consumes: `--check` dispatch from Task 1.
- Produces: `scripts/layering_rules.py --emit-map` writing the map to stdout; `check-docs.py --check crate-map` regenerating and diffing.

- [ ] **Step 1: Make `layering_rules.py` able to emit the map**

Add an `--emit-map PATH_TO_ROOT` flag. It prints a markdown table with one row per key of `ALLOWED`, sorted by crate name: the crate name, the first line of that crate's `//!` header, and its allowed internal edges sorted. Resolve a crate's directory by stripping the `loong-` prefix, with the single exception `loongagent` → `cli`, and take the header's first line from `crates/<dir>/src/lib.rs` (or `src/main.rs` for `loongagent`). Every one of the 24 crates has a `//!` header of 5–38 lines; fail loudly if one is missing rather than emitting a blank cell.
Keep the existing stdin-validation behaviour when `--emit-map` is absent.

- [ ] **Step 2: Generate the map and prove the check fails when it drifts**

Run: `python3 scripts/layering_rules.py --emit-map . > docs/crate-map.md`
Add the `crate-map` check to `check-docs.py`: regenerate into a string and compare with the file's contents; on mismatch, exit 1 and print the diff.
Then hand-edit `docs/crate-map.md` (delete one row).
Run: `python3 scripts/check-docs.py --check crate-map`
Expected: exit 1 showing the missing row.
Run: `python3 scripts/layering_rules.py --emit-map . > docs/crate-map.md` and re-run the check. Expected: exit 0.

- [ ] **Step 3: Point `DEVELOPMENT.md` at the authority**

Replace the `## Layout` section's table (`DEVELOPMENT.md:705-722`, the "Nine of the eleven crates" table, which names 11 of 24 crates) with two sentences and a link to `docs/crate-map.md`. Keep one sentence stating the rule the hand-written table conveyed — dependencies may only point down — and note that `scripts/layering_rules.py` is what enforces it.

- [ ] **Step 4: Gate and commit**

Run: `./scripts/check.sh`
Expected: `all gates passed`.

```bash
git add scripts/layering_rules.py scripts/check-docs.py docs/crate-map.md DEVELOPMENT.md
git commit -m "docs: generate the crate map from the layering rules, which are the real authority"
```

### Task 3: no orphan documents, and index the new subtree

**Files:**
- Modify: `scripts/check-docs.py`, `docs/README.md`

**Interfaces:**
- Consumes: the `--check` dispatch from Task 1.
- Produces: `check-docs.py --check orphans`.

- [ ] **Step 1: Add the check**

An `.md` under `docs/` is indexed when the index names either its repository-relative path or its
**first path segment below `docs/`**. "Names" means a markdown link target *or* a table row's first
cell. Fail listing every unindexed document.

*Corrected during execution (Rulings 13–15).* Three things the first draft got wrong, each of which
would have made the check either unusable or vacuous:

- **Ruling 13:** the draft said "appears in a markdown link target". Two of the index's directory rows
  (`decisions/`, `research/`) are **bare table cells, not links**, so that literal reading left all 18
  ADRs and all 15 research notes failing. Table-row first cells count.
- **Ruling 14:** the check enumerates the **working tree** (`git ls-files --cached --others
  --exclude-standard -- docs`) while `links` and `crate-map` read tracked files only. Without that, the
  untracked probe in Step 2 would pass when it must fail, making the red proof vacuous. The consequence
  is a behaviour change for people, not programs: a stray unindexed `.md` under `docs/` fails the gate
  until it is indexed or deleted.
- **Ruling 15:** "direct subdirectory" means the **first** segment below `docs/`, not the immediate
  parent. Under the immediate-parent reading a single `superpowers/` row could not cover
  `superpowers/plans/`, and Step 3's one-row fix would have been impossible.

- [ ] **Step 2: Prove it fails**

Temporarily create `docs/orphan-probe.md`.
Run: `python3 scripts/check-docs.py --check orphans`
Expected: exit 1 naming `docs/orphan-probe.md`.
Delete the probe and re-run. Expected: exit 1 naming **both** files under `docs/superpowers/` — the
spec and the plan — because the `superpowers/` subtree is not yet indexed, which is the real defect
this task fixes.

*Corrected during execution:* the first draft predicted only the spec file. Both are unindexed without
the row, and naming both is the rule working as designed rather than a deviation.

- [ ] **Step 3: Index the subtree**

Add a `superpowers/` row to the `docs/README.md` index table, describing it as the design specs and implementation plans produced while working, and linking `superpowers/specs/2026-10-16-navigability-design.md`.

- [ ] **Step 4: Gate and commit**

Run: `python3 scripts/check-docs.py --check orphans && ./scripts/check.sh`
Expected: both exit 0.

```bash
git add scripts/check-docs.py docs/README.md
git commit -m "docs: an index that can be checked for orphans, and the spec subtree indexed"
```

### Task 4: impossible citations

**Files:**
- Modify: `scripts/check-docs.py`

**Interfaces:**
- Consumes: the `--check` dispatch from Task 1.
- Produces: `check-docs.py --check citations`.

- [ ] **Step 1: Add the check**

Derive the highest ADR number from the `docs/decisions/NNNN-*.md` filenames and the highest topic from `^## T-(\d+)` in `docs/02-topics.md`. Fail for any document mentioning `ADR-(\d{4})` above the former or `T-(\d{2})` above the latter. Document in the check's own comment that it catches *impossible* citations, not stale ones — `ADR-0001…ADR-0014` passes even though 18 exist, because 14 is a real ADR.

- [ ] **Step 2: Prove it fails, then gate and commit**

Write `ADR-0099` into a scratch line of `docs/README.md` **as bare prose, not inside backticks**, run
`python3 scripts/check-docs.py --check citations`, expected exit 1 naming the file; remove the line,
re-run, expected exit 0. Then `./scripts/check.sh` and commit.

*Corrected during execution (Rulings 16–17).* This plan file itself contains the literal `ADR-0099`
at this very line, inside backticks, as the seed the step asks for. A check that scanned raw text
would therefore fail on the plan that specifies it. The check masks fenced blocks and inline code
spans exactly as the `links` check does — a document may *show* something without owning it — so the
seed must be bare prose or the red run is vacuous. Proving both halves is the point: bare prose fails,
and this line's backticked occurrence is tolerated. Exempting `docs/superpowers/` was considered and
rejected, because it would put the plan and spec outside a rule every other document obeys.

```bash
git add scripts/check-docs.py
git commit -m "docs: refuse citations of an ADR or a topic that does not exist"
```

### Task 5: the test counts in `README.md` are the reported ones

**Files:**
- Modify: `scripts/check-docs.py`, `scripts/check.sh`, `README.md:393`

**Interfaces:**
- Consumes: the `--test-counts FILE` parameter from Task 1.
- Produces: `check-docs.py --check counts --test-counts FILE`.

- [ ] **Step 1: Capture the suite's own totals in `check.sh`**

Change the test step at `scripts/check.sh:31` to tee `cargo test --workspace --locked` into a temporary file and pass its path to `check-docs.py --check counts --test-counts "$file"`. `set -o pipefail` is already set, so a failing test still fails the gate.

- [ ] **Step 2: Add the check**

Sum the `passed` and `ignored` fields of every `test result:` line in the captured file. Fail unless `README.md` contains an HTML comment of exactly the shape fixed in Global Constraints whose two numbers match. Compare numbers only, never the surrounding prose.

- [ ] **Step 3: State the counts in `README.md`**

Replace the bare `736 个测试。` at `README.md:393` with a sentence naming both figures and carrying the machine-readable comment on its own line beneath it. The values today are `736 passed, 11 ignored`.

- [ ] **Step 4: Prove the check fails**

Change the comment's passed count to `700`, run `./scripts/check.sh`, expected failure naming the two numbers. Restore, re-run, expected `all gates passed`.

- [ ] **Step 5: Commit**

```bash
git add scripts/check-docs.py scripts/check.sh README.md
git commit -m "docs: check the test counts against the suite, instead of retyping them"
```

---

## Phase B — the documents are true

Phase B has no gate to lean on (Review Focus 6). Each task's proof is the quoted evidence, re-read after the edit.

### Task 6: `docs/04-system-overview.md` is brought current

**Files:**
- Modify: `docs/04-system-overview.md`

- [ ] **Step 1: Fix the four stale numbers**

Header (`:3`): `ADR-0001…ADR-0014` → `ADR-0001…ADR-0018`. `:212` `T-01…T-21` → `T-01…T-25`. `:215` `ADRs 0001–0014` → `0001–0018`. `:216` `14 cited deep dives` → `15`. `:225-226` `fourteen ADRs` → `eighteen`.

- [ ] **Step 2: Complete the decision table**

In §4 ("Every decision, in one table"), add the two missing rows: ADR-0016 (pack distribution) and ADR-0018 (plugin install and bootstrap). Read both ADRs and write one line each in the style of the existing rows.

- [ ] **Step 3: Replace the "not started" ending**

Rewrite §10 (`:223-236`) from "**Design: complete.** … **Implementation: not started.**" to the current state: the design is complete and the implementation exists, with the build order named as history rather than as a plan. Take the facts from `README.md` §状态 and from `git log`.

- [ ] **Step 4: Verify by reading**

Assert the *stale phrasings* are gone, not that the bare number is:

Run: `grep -n 'not started\|T-21\|14 cited\|fourteen\|ADR-0001…ADR-0014\|ADRs 0001–0014' docs/04-system-overview.md`
Expected: no output.
Run: `grep -c 'ADR-0014\|0014' docs/04-system-overview.md`
Expected: several — `0014` is a real ADR with a row in §4's table and six legitimate prose references. A fix that removed them all would be the over-deletion this step exists to catch.

*Corrected during execution:* the first draft of this step ran a single grep for `not started\|0014\|T-21\|14 cited\|fourteen` and expected no output. It could never produce none — `:129`, `:143`, `:156`, `:168`, `:180` and `:197` all cite ADR-0014 correctly — so the step would have failed a correct fix, and an implementer trying to satisfy it would have deleted real content.

Run: `python3 scripts/check-docs.py` and `./scripts/check.sh`. Expected: exit 0.

- [ ] **Step 5: Commit**

```bash
git add docs/04-system-overview.md
git commit -m "docs: the one-read picture said the implementation had not started"
```

### Task 7: `DEVELOPMENT.md` is brought current

**Files:**
- Modify: `DEVELOPMENT.md`

- [ ] **Step 1: Fix the test count and the numbering**

`:698` `# 69 tests` → the current figures. `:1031` and `:1034` are both numbered `11.`; renumber the second through the rest of that list.

- [ ] **Step 2: Remove the self-contradiction about streaming**

Delete the sentence "Response streaming is not implemented, so the answer appears when the turn completes." at `:756-757`. The same file describes streaming as shipped at `:917-923`; that description is the true one, verified by `crates/providers/src/openai_compat.rs`'s SSE handling and the `model.delta` notification.

- [ ] **Step 3: Correct the "not started" list**

`DEVELOPMENT.md:1025-1026` lists five things as "Not started": session export/import, WASM extensions, the web client, compaction and the control plane. **Verify each of the five against the code before removing anything** — do not trust this paragraph's summary of them, which was wrong when it was written.

Verified during execution (Ruling 19): the web client (`crates/web`, the `web` dispatch arm at `crates/cli/src/main.rs:2271`), compaction (`Kernel::compact_if_needed` in `crates/kernel/src/turn.rs`) and the control plane (`crates/control/src/bin/loongagent-control.rs`, ADR-0012, a runbook) all exist, so listing them as not started is false. **Session export/import and WASM extensions genuinely do not exist**: ADR-0014 is `Status: Accepted` — a decision, not an implementation — and the CLI's dispatch arms are `config context control delete fork hooks local loongagent mcp plugin profiles repl resume run sandbox server sessions setup skills tui use --version web`, with no `export`, `import` or `acp` anywhere and every `export` token in the file concerning environment variables.

So the correct fix is not "delete the list" and not "keep the list" but "split it": remove the three that exist, keep the two that do not, and say plainly which is which. A list that mixes both is worse than either, because the reader cannot tell which half to trust.

- [ ] **Step 4: Verify by reading**

Run: `grep -n '69 tests\|not implemented\|Not started' DEVELOPMENT.md`
Expected: no false claim among the hits — read each one.
Run: `./scripts/check.sh`. Expected: exit 0.

- [ ] **Step 5: Commit**

```bash
git add DEVELOPMENT.md
git commit -m "docs: the maintainer guide called shipped work unstarted, and contradicted itself"
```

### Task 8: the signing story, stated once and correctly

**Files:**
- Modify: `README.md`, `docs/05-parity-and-synthesis.md`

- [ ] **Step 1: Establish what is true from the code**

Confirm three things and quote them in the commit body: `crates/pack/src/lib.rs:1-22` (the signed envelope, Ed25519 against a pinned key), `crates/cli/src/main.rs:743` (the pack is verified before it can occupy the org layer), and `crates/config/src/lib.rs:1401` (the local org layer is a plain `/etc/loongagent/profile.toml` with no signature).

- [ ] **Step 2: Correct `docs/05` §C**

The C2 row at `docs/05:164` says the signed envelope does not ship and nothing should be called tamper-resistant. Correct it: the signed envelope ships and is verified for *packs*; the *local* org layer file is unsigned, and that is the thing that must not be called tamper-resistant.

- [ ] **Step 3: Correct `README.md`**

`:360` and `:366` and the status list at `:395` say signing is not implemented. Rewrite them to the same distinction: packs are signed and verified; the local org file is not.

- [ ] **Step 4: Verify by reading**

Run: `grep -rn '签名\|signed\|tamper' README.md docs/05-parity-and-synthesis.md docs/decisions/0016-pack-distribution.md`
Expected: every hit agrees that packs are signed and the local org layer is not.
Run: `./scripts/check.sh`. Expected: exit 0.

- [ ] **Step 5: Commit**

```bash
git add README.md docs/05-parity-and-synthesis.md
git commit -m "docs: a signed pack and an unsigned local org file are two different things"
```

### Task 9: `docs/00` and `docs/02`

**Files:**
- Modify: `docs/00-vision.md`, `docs/02-topics.md`

- [ ] **Step 1: Correct the vision's surfaces and resolved questions**

`:29` lists a desktop app as a surface every user may choose; ADR-0009 `:18` drops desktop and CI. Correct the bullet to the surfaces that exist (TUI, web, REPL, headless) and cite ADR-0009 as the authority. Then resolve the three open questions at `:81-87`, which are decided: check each against the ADRs and mark it resolved with the ADR that decided it.

- [ ] **Step 2: Bring `docs/02-topics.md`'s statuses up to the decisions**

Only T-11 (`:382` → ADR-0007), T-14 (`:430` → ADR-0010) and T-16 (`:463`, merged into T-18) carry a non-`Seed` status, while 21 topics remain `Seed`. Go through them and mark `Promoted → ADR-00NN` wherever an ADR already decides the topic, and `Wanted`/`Discussing` where the conversation has moved on. Leave genuinely undiscussed topics as `Seed`, and be able to say why each one is still open. Do not delete any topic.

- [ ] **Step 3: Record the duplicates that are deliberately not consolidated**

Add one topic to the backlog covering the five small cross-crate helper duplicates the spec lists under "Record, do not fix" — **measured during execution: `now_ms` (4 copies: `control`, `hooks` — already `pub` — `plugin`, `eventlog`), `first_line` (3), `expand_home` (2), `truncate` (2), `clip` (2)** — naming each with its files, marking it `Seed`, and stating why it is not being done now: consolidating needs a new shared public surface, which is an API change, and five ten-line functions do not justify one. This is the record; the spec deliberately removes nothing.

*Corrected during execution (Ruling 33):* the first draft said `now_ms` (3 copies) and named only `expand_home` and `first_line` — omitting `truncate` and `clip` and undercounting `now_ms` by one, because a `grep` for `^fn now_ms` does not match `pub fn now_ms`, which is the copy in `hooks`. Use the measured list above.

- [ ] **Step 4: Verify by reading**

Run: `grep -c 'Promoted' docs/02-topics.md`
Expected: substantially more than 2. Read the ones left as `Seed` and be able to say why each is still open.
Run: `python3 scripts/check-docs.py && ./scripts/check.sh`. Expected: exit 0.

- [ ] **Step 5: Commit**

```bash
git add docs/00-vision.md docs/02-topics.md
git commit -m "docs: the vision promised a desktop app, and the backlog had not caught up with the ADRs"
```

---

## Phase C — one authority each

### Task 10: the superseded history moves to `docs/archive/`

**Files:**
- Create: `docs/archive/` (with a short `README.md` explaining what is in it and why)
- Move: `docs/01-landscape-synthesis.md`, part of `docs/03-architecture.md`
- Modify: `docs/README.md`, `scripts/check-docs.py` if the orphan rule needs the new subtree

**Interfaces:**
- Consumes: Task 1's link check, which is what proves the move did not break links (Review Focus 1).

- [ ] **Step 1: Move `docs/01` and fix its links**

`git mv docs/01-landscape-synthesis.md docs/archive/`. Every relative link inside it that pointed at a sibling of `docs/` now resolves one directory higher; repair each, then confirm with `python3 scripts/check-docs.py --check links` that no link broke. Run the link check *before* the move and save the output, so the before-and-after can be compared rather than assumed.

*Corrected during execution (Task 10):* this step asks the gate to prove half the move with a check that **cannot see it**. `docs/01` contains zero markdown links — `grep -c '](' ` returns 0 — because its `research/…` references are code spans, which the `links` check masks by design. The repair there has to be done by grep and reported by hand, since a bare path in backticks is invisible to every check in the gate. That masking is correct (it is what lets these documents quote paths), but it means a code-span path can rot unobserved, and the count of paths repaired by hand is the only record. Same defect class as Ruling 9.

*Also corrected:* the move leaves `docs/03` with a dangling `## Part 2` heading (the only part left) and AQ-10's "see discussion log" pointing at a file that has moved. Closing both belongs to this task, because the move created them; the brief's "keep Part 2 intact" was about the decision map's *content*, which is untouched.

- [ ] **Step 2: Move `docs/03`'s history and keep its decision map**

Move Part 1 (`:20-48`, "Foundations — historical framing") and Part 3 (`:539-808`, the discussion log, ~290 lines) into `docs/archive/03-architecture-history.md`, and leave Part 2 (the AQ-1…AQ-12 decision map) in `docs/03-architecture.md`. Repair relative links in both halves. The discussion log is the owner's verbal trade-offs, which a commit message cannot reproduce — that is why it is archived rather than deleted.

- [ ] **Step 3: Write `docs/archive/README.md` and index it**

One short document: what is archived, that it is history rather than current guidance, and that `docs/05` supersedes `01` while the ADRs supersede `03` Part 2. Add a row for `archive/` to the `docs/README.md` index.

- [ ] **Step 4: Verify and commit**

Run: `python3 scripts/check-docs.py && ./scripts/check.sh`
Expected: exit 0, and the orphan check accepts the new subtree.

```bash
git add -A docs/
git commit -m "docs: the superseded synthesis and discussion log are history, not guidance"
```

### Task 11: one index

**Files:**
- Modify: `docs/04-system-overview.md`, `README.md`

- [ ] **Step 1: Replace `docs/04` §9 with a pointer**

§9 ("Where to read what", `:206-221`) is a second index with its own two errors. Replace the table with two sentences pointing at `docs/README.md` as the index and at `README.md`/`DEVELOPMENT.md` as the two entries.

- [ ] **Step 2: Replace `README.md`'s document table with a pointer**

The table at `:371-381` lists seven rows of the same map, in Chinese. Replace it with one sentence and a link to `docs/README.md`, keeping any row that is not in the index.

- [ ] **Step 3: Verify and commit**

Run: `python3 scripts/check-docs.py --check orphans && ./scripts/check.sh`
Expected: exit 0.

```bash
git add docs/04-system-overview.md README.md
git commit -m "docs: three indexes drift apart; one of them is enough"
```

### Task 12: one "deliberate no"s list

**Files:**
- Modify: `docs/04-system-overview.md`

- [ ] **Step 1: Replace `docs/04` §6 with a pointer**

`docs/04` §6 ("What we deliberately do **not** do", `:149-171`) restates the list whose authority is `docs/05` §D (`:172-185` — D1–D10; D10 was added by Task 9's fix round, so any range you were given earlier that ends at 184 is one row short). Check each item is present in `docs/05` §D first — if any is not, add it there — then replace §6 with a two-sentence pointer. `docs/01` §D leaves with the archive in Task 10.

One item is known to be missing on the `docs/04` side: §6 has no bullet for **server-side live routing**, which §D's D10 now records as rejected. Reconciling the two lists is this task's job rather than a separate one, because §6 is about to be replaced by a pointer to §D and the pointer is only honest if §D is the complete list.

- [ ] **Step 2: Verify and commit**

Run: `grep -n 'deliberately' docs/04-system-overview.md docs/05-parity-and-synthesis.md`
Expected: the list expands once, in `docs/05`.
Run: `./scripts/check.sh`. Expected: exit 0.

```bash
git add docs/04-system-overview.md docs/05-parity-and-synthesis.md
git commit -m "docs: the things we refuse to do were written down three times"
```

### Task 13: status lives in the index

**Files:**
- Modify: `docs/README.md`, `docs/00-vision.md`, `docs/01`'s replacement if any, `docs/02-topics.md`, `docs/03-architecture.md`, `docs/04-system-overview.md`, `docs/05-parity-and-synthesis.md`, `docs/06-context-engineering.md`

**Interfaces:**
- Consumes: nothing. Order matters internally (Review Focus 5).

- [ ] **Step 1: Fill the index's Status column for every document**

Every row in the `docs/README.md` table, plus a row for each of `00`–`07`, carries a status. Read each document's current header where it has one and its content where it does not. This step comes first so nothing is lost.

- [ ] **Step 2: Rewrite the convention paragraph**

Replace `docs/README.md:14-15` ("Every doc starts with a header: **Status** … and **Last updated** date") with the new rule: status is recorded in this table, documents carry a title, and ADRs keep their own `Status:` line because an ADR's status is part of its record.

- [ ] **Step 3: Strip the per-document headers**

Remove the `- **Status:** …` and `- **Last updated:** …` lines from `docs/00`, `02`, `03`, `04`, `05`, `06` and from `docs/research/dsh-plugin-ecosystem.md`, whose status is now in the index. Leave every `docs/decisions/*.md` header untouched.

- [ ] **Step 4: Verify and commit**

Run: `grep -rln 'Last updated' docs/ | grep -v 'decisions/\|archive/\|superpowers/'`

*Corrected during execution, twice.* The original grep excluded only `decisions/`, but `docs/archive/` keeps its headers under Ruling 5 (archived material is history and is not maintained). Then Task 13 found the second hole: the plan and the spec under `docs/superpowers/` both **quote** the phrase "Last updated" while specifying exactly this change, so the grep matched the requirement documents themselves. A quotation is not a header — the third exclusion is the whole remaining correction.
Expected: no output.
Run: `python3 scripts/check-docs.py && ./scripts/check.sh`. Expected: exit 0.

```bash
git add docs/
git commit -m "docs(index): most documents ignored the header rule, so keep status where it is read"
```

### Task 14: `DEVELOPMENT.md`'s status sections stop being a second verdict

**Files:**
- Modify: `DEVELOPMENT.md`

**Interpretation, stated so it can be rejected:** the spec makes `README.md` §"状态，说实话" the sole authority for the works/not-works verdict, and says `DEVELOPMENT.md`'s §"What works today" and §"What is not done yet" shrink to a pointer. Taken literally that deletes roughly 220 lines of per-feature detail, which is information rather than duplication. This plan implements the *authority* rule and keeps the detail: the verdict is stated once, in `README.md`, and both `DEVELOPMENT.md` sections open with a line naming `README.md` as the authority so they read as detail behind it, not as a competing verdict.

- [ ] **Step 1: Make the subordination explicit**

Add to the head of §"What works today" (`:766`) and §"What is not done yet" (`:989`) one line each: the verdict is `README.md` §状态; this section is the detail behind it. Remove any sentence in either section that states a works/not-works verdict rather than describing behaviour.

- [ ] **Step 2: Make sure the two agree**

Walk `README.md` §状态's two lists against `DEVELOPMENT.md`'s. Where they disagree about an item's state, `README.md` wins and the detail section is corrected. This is the drift the change exists to stop.

- [ ] **Step 3: Verify and commit**

Run: `./scripts/check.sh`. Expected: exit 0.
Then read both sections and the README list side by side; there must be no item whose state differs.

```bash
git add DEVELOPMENT.md README.md
git commit -m "docs: one place states whether it works; the other explains why"
```

---

## Phase D — the maintainer layer

### Task 15: `docs/07-extending.md`

**Files:**
- Create: `docs/07-extending.md`
- Modify: `docs/README.md`, `README.md`, `DEVELOPMENT.md` (pointers only)

**Interfaces:**
- Consumes: Task 3's orphan check, which forces the new document into the index.

- [ ] **Step 1: Establish the four procedures from the code, quoting where each lives**

Read and record the exact anchors for each procedure before writing a word:

| Procedure | Anchors |
|---|---|
| Add a built-in tool | `crates/tools/src/lib.rs:72` (`pub trait Tool`), `:116` (`pub fn specs`), the kernel assembling them at `crates/kernel/src/turn.rs:2668`, and the permission classification the tool must declare |
| Add an event | `crates/schema/protocol.toml` (events are `kind = "enum"` / `"struct"` declarations), `scripts/gen-schema.py` (generates the Rust, the TypeScript and the samples), and `crates/eventlog`'s envelope rules including `ignorable` |
| Attach a hook | `crates/hooks` (`hooks.toml`, verdict combination in `crates/hooks/src/run.rs`), and why a hook is inert until `loongagent hooks trust` |
| How routing picks a model | ADR-0005, `crates/config/src/lib.rs`'s `Router`/`RouterRule`/`parse_condition`, and the provider layer |

- [ ] **Step 2: Write the document**

One section per procedure, each with the files to touch, the order to touch them, and the one command that proves the change took effect. Point at the ADRs and at the code rather than restating them — long restatements are how the duplicates in §1 accumulated. Include the two things a newcomer cannot infer: that `gen-schema.py` must be run (and that `check.sh` fails if its output is stale), and that a hook is trust-gated per repository.

- [ ] **Step 3: Index it and point at it**

Add the row to `docs/README.md`. Add one line to `DEVELOPMENT.md` near the gates pointing a maintainer at `07-extending.md`.

- [ ] **Step 4: Verify and commit**

Run: `python3 scripts/check-docs.py && ./scripts/check.sh`
Expected: exit 0.
Then check the document's own claim: for each procedure, follow its first instruction and confirm the file it names is the file that does the job.

```bash
git add docs/07-extending.md docs/README.md DEVELOPMENT.md
git commit -m "docs: nothing explained how to add a tool or how routing picks a model"
```

---

## Phase E — redundancy that costs no API change

### Task 16: seven declared-but-unused dependencies

**Files:**
- Modify: `crates/cli/Cargo.toml`, `crates/eventlog/Cargo.toml`, `crates/sandbox/Cargo.toml`, `crates/skills/Cargo.toml`, `crates/tools/Cargo.toml`, `crates/tui-app/Cargo.toml`, `crates/control/Cargo.toml`, `Cargo.lock`

- [ ] **Step 1: Record the before state, per crate (Review Focus 4)**

Run: `cargo tree -p loongagent -e normal --depth 1 --prefix none | grep -c '^loong-protocol$'` and the same shape for `serde` against each of `loong-eventlog`, `loong-sandbox`, `loong-skills`, `loong-tools`, `loong-tui-app` and `loong-control`. Save the output — the compiler will *not* tell us if the removal is wrong, because cargo does not warn about unused declared dependencies.

*Corrected during execution (Task 16).* The first draft used `cargo tree -e normal | grep -c '^serde '`, which is unusable: `cargo tree` indents and glyph-prefixes its output, so an anchored `^serde ` matches nothing **before or after** the change — a verification step that cannot fail in either direction, and the sixth such step this plan produced. Two further corrections to this task's text: the proof is that the target stops being a **direct** dependency (`--depth 1`), because `serde` legitimately remains reachable transitively through `loong-schema` and `serde_json`; and "all six use `serde_json`" is false for `loong-sandbox`, which uses neither `serde` nor `serde_json` — its declaration was unused all the same, so the removal stands.

- [ ] **Step 2: Remove them**

Delete the `loong-protocol.workspace = true` line from `crates/cli/Cargo.toml` and the `serde.workspace = true` line from the six crates. Do not touch `serde_json` anywhere; it is a separate dependency — five of the six use it, and `loong-sandbox` uses neither.

- [ ] **Step 3: Prove the removal took effect and broke nothing**

Re-run the `cargo tree --depth 1` checks from Step 1: the target must no longer be a **direct** dependency of any of the seven. Do not assert that `serde` has left the graph — it has not, and should not: `serde` remains reachable transitively, and `crates/cli` keeps its own declaration because `crates/cli/src/main.rs` derives `Serialize`.
Run: `cargo check --workspace --all-targets` and confirm the same 0 warnings.
Run: `./scripts/check.sh` with `FAST=1` first (skips the slow release build), expected `all gates passed`; then the full `./scripts/check.sh`.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock crates/*/Cargo.toml
git commit -m "chore: seven dependencies nobody referenced, including serde in six crates that only use serde_json"
```

### Task 17: one dead function

**Files:**
- Modify: `crates/control/src/bin/loongagent-control.rs`

- [ ] **Step 1: Delete `pack_of` and its `#[allow(dead_code)]`**

`pack_of` is defined once and called nowhere (`grep -rn pack_of crates/` returns only the definition), and its `#[allow(dead_code)]` is the workspace's only **unconditional** one — the base tree's other `allow(dead_code)` is a `#[cfg_attr(not(unix), allow(dead_code))]` in `crates/kernel/src/turn.rs`, which stays.

- [ ] **Step 2: Prove the workspace still compiles warning-free**

Run: `cargo check --workspace --all-targets`
Expected: exit 0 and no `warning:` line — which is what the `allow` was hiding, so its absence is the proof.
Run: `./scripts/check.sh`. Expected: `all gates passed`.

- [ ] **Step 3: Commit**

```bash
git add crates/control/src/bin/loongagent-control.rs
git commit -m "chore: pack_of was called by nothing, and the allow(dead_code) kept it quiet"
```

### Task 18: three copies of one test helper

**Files:**
- Create: `crates/cli/tests/common/mod.rs`
- Modify: `crates/cli/tests/tui_surface.rs`, `crates/cli/tests/web_surface.rs`, `crates/cli/tests/vertical_slice.rs`

- [ ] **Step 1: Move the helper**

`isolated_command` is byte-identical in the three files (`tui_surface.rs:9`, `web_surface.rs:17`, `vertical_slice.rs:15`). Move it to `crates/cli/tests/common/mod.rs`, keep it `pub`, and have each of the three declare `mod common;` and call `common::isolated_command`. Do not change its body. Leave the two duplicated `build.rs` `fn main`s and the two `named` test helpers alone — a `build.rs` per crate is normal shape and the two `named` helpers belong to different crates.

- [ ] **Step 2: Prove the tests still run and the duplication is gone**

Run: `cargo test -p loongagent --test tui_surface --test web_surface --test vertical_slice`
Expected: all pass, same count as before (record it first).
Re-run the item-hash extractor from Task 19 if it already exists, or `grep -rn 'fn isolated_command' crates/`
Expected: exactly one definition.

- [ ] **Step 3: Gate and commit**

Run: `./scripts/check.sh`. Expected: `all gates passed`.

```bash
git add crates/cli/tests/
git commit -m "test: one isolated_command instead of three byte-identical copies"
```

---

## Phase F — the four mechanical splits

Read the module trees, the visibility budgets and the "three facts that make this mechanical" from the spec's §4.4; they are not repeated here. Each task below is one commit and must produce all four proofs.

**The four proofs, common to Tasks 19–23:**
1. `python3 scripts/move_evidence.py items --rev HEAD --path <old files> --min <N>` and the same with `--worktree` produce identical output.
2. The test inventory is unchanged. Capture `cargo test --workspace --locked -- --list 2>&1`, pipe it through `scripts/normalise_test_list.py` — which reduces every `Running … (target/debug/deps/<name>-<hash>)` line to `<name>` and sorts with duplicates preserved — and require the result identical to the before capture. The hash changes when the code changes, so a raw comparison would fail for the wrong reason; and keeping the normalised target name on each group is what stops two same-named tests in different targets from cancelling out. *Corrected during execution (Task 19):* this paragraph originally described the normalisation in prose, which would have had Tasks 20–23 re-derive it four times; the script is the single implementation.

   *Corrected again during execution (Task 23):* **"identical" is unachievable for a split that moves a root-level test module into a host submodule.** Task 23's 49 CLI unit tests all stay, with identical leaf names, but their qualified names gain the host path — `tests::x` becomes `local::tests::x`, `session_tests::x` becomes `session::x`, and so on for four modules, exactly 49 lines. Requiring byte-identical output there would fail a correct split. The check is therefore: **the leaf-name multiset is identical, both workspace-wide and within each target**, and every qualified-name difference is a module-path relocation, enumerated one line each in the report. Note the contrast with Task 22, whose tests also moved but stayed at the same module path (`turn::tests` either way), so its output really was identical — whether this correction applies depends on the depth the tests land at, not on whether they moved.
3. `git diff -- '**/Cargo.toml'` is empty.
4. `./scripts/check.sh` is green, including the 8 integration tests under `crates/cli/tests/`.

### Task 19: the evidence script, and `tui-app`'s tests leave the file

**Files:**
- Create: `scripts/move_evidence.py`, `crates/tui-app/src/tests.rs`
- Modify: `crates/tui-app/src/lib.rs`

**Interfaces:**
- Produces: `scripts/move_evidence.py`, with two subcommands that Tasks 20–23 consume:
  - `items (--rev REV | --worktree) --path PATH [--path PATH ...] --min N`, printing one line per **item** as `sha256<TAB>kind<TAB>name<TAB>vis<TAB>path<TAB>vis-stripped-sha256` sorted, and exiting 1 if fewer than `N` items were extracted or if the two modes disagree. *Corrected during execution (Task 19):* this was `bodies … sha256<TAB>name<TAB>path`, hashing only the text between a function's braces. The implementer showed what that missed — a seeded `width: u16` → `u32` signature change altered one item while every body hash stayed identical — so `items` covers whole items with attributes, visibility and signature, and the visibility column makes a widening visible rather than implied. **The directional rule compares fields 1–4** (`sha256`, `kind`, `name`, `vis`), and falls back to **field 6** to demonstrate that a visibility change altered nothing else: a private item's stripped hash equals its own full hash, so a widening shows a changed field 1 with an unchanged field 6, and any other edit inside the item shows both changing. The implementer proved all three cases by seeding a widening, a widening-plus-signature-edit, and a `pub` → private narrowing.
  - `compare BEFORE AFTER`, reading two `items` captures and printing the directional classification — exact matches, visibility widenings with both hashes, added items with their kind, lost items — and exiting non-zero on a lost item, on an unlisted or unclassified addition, on a hash that moved without an allowed widening, or on a forbidden visibility transition. *Added during execution (Task 20):* the comparison first lived in gitignored per-task scratch and, unlike `reflow`, had **no direction check** — it would have accepted a `pub(crate)` → `pub` widening, which reaches the public API and compiles. Tasks 21–23 use this subcommand so all three compare with the same rules rather than each re-deriving them.
  - `reflow --rev REV --from-path OLD --start A --end B --dedent N --to-path NEW [--to-rev REV] [--header-file FILE] [--allow-widening NAME ...] [--edition ED]`, asserting `rustfmt(dedent(old block)) == the new file` byte-for-byte. The new side is the worktree unless `--to-rev REV` names a revision, in which case it is `git show REV:<path>` — which is what a task that runs mid-split on a dirty tree needs. This is how each split shows that rustfmt's own reflow is the *whole* of the difference between the moved text and its original, instead of arguing it hunk by hunk. *Corrected during execution (Task 20):* the literal form can **never** pass for a submodule split, because a submodule does not inherit the parent's `use` lines — each new file must carry a header no contiguous range of the old file contains — and the body may carry a permitted visibility widening the old text lacks. The two allowances are bounded and named: `--header-file` supplies the permitted leading lines, and `--allow-widening NAME` permits that item's line to differ **by its visibility keyword only**, checked by stripping the visibility token from both sides and requiring them equal. `reflow` therefore still exits non-zero on anything else.
- Produces: `scripts/normalise_test_list.py`, reading the merged stdout and stderr of `cargo test --workspace --locked -- --list 2>&1` on stdin and writing the normalised inventory on stdout — `Running …` lines with `target/debug/deps/<name>-<hash>` reduced to `<name>`, `Doc-tests` lines and `NAME: test` lines, sorted with duplicates preserved. `--count` also writes the number of test lines to stderr. Proof 2 uses this one implementation in every split rather than re-deriving the hash-stripping four times.

- [ ] **Step 1: Write the extractor, and make it refuse to pass vacuously**

Brace-matching extraction of every **item** — `fn`, `impl`, `struct`, `enum`, `union`, `trait`, `type`, `const`, `static`, `mod`, `use`, `macro_rules!` — with its attributes, visibility and signature hashed together, printing `sha256<TAB>kind<TAB>name<TAB>vis<TAB>path<TAB>vis-stripped-sha256`. `--min N` is mandatory so that an extractor which matches nothing fails instead of reporting two empty sets as equal (Review Focus 2). Skip braces inside string, char and raw-string literals and inside comments, or a body containing `'}'` will truncate silently. *Corrected during execution (Task 19):* this step said "every `fn` and `impl` body", which is the hole the interface paragraph explains — a signature, a visibility, an attribute or any non-`fn` item could change while every body hash stayed identical.

- [ ] **Step 2: Prove the extractor fails when it should**

Run: `python3 scripts/move_evidence.py items --worktree --path crates/tui-app/src/lib.rs --min 999999`
Expected: exit 1 saying it found fewer bodies than `--min`. Then run it with the real count as `--min` and expected exit 0.

- [ ] **Step 3: Capture the before state**

Run: `cargo test --workspace --locked -- --list > /tmp/before-tests-raw`, then normalise it exactly as proof 2 describes and save the result as `/tmp/before-tests`. Sanity-check that it contains 747 `: test` lines as of 2026-10-16.
Run: `python3 scripts/move_evidence.py items --rev HEAD --path crates/tui-app/src/lib.rs --min 1 > /tmp/before-items` and note how many items it found; that number becomes `--min` for this task.

- [ ] **Step 4: Move the tests**

`git mv` is not applicable — the tests stay in the crate. Create `crates/tui-app/src/tests.rs` containing the `#[cfg(test)] mod tests` body from `lib.rs:3928-7661` (3 734 lines), and in `lib.rs` replace it with `#[cfg(test)] mod tests;`. `mod tests` reached from `lib.rs` still sees the same `super::*`, so this step adds no visibility change at all — it is the pilot precisely because it is the cheapest and the most valuable (48.7% of the file).

- [ ] **Step 5: All four proofs, then commit**

Run the four proofs above with `--min` set from the before state.
If any fails, the move is not mechanical — fix the move, never the proof.

```bash
git add scripts/move_evidence.py crates/tui-app/src/
git commit -m "refactor(tui-app): the tests are half the file; they can still be in the same module"
```

### Task 20: `tui-app`'s four blocks leave `lib.rs`

**Files:**
- Create: `crates/tui-app/src/{worker,setup,wizard,surface}.rs`
- Modify: `crates/tui-app/src/lib.rs`

- [ ] **Step 1: Move the four blocks and re-export**

Move `spawn_worker`, the setup form, the wizard and `run` into the four files named in the spec's §4.4 tree, and add the `pub use` lines the spec lists so that every existing path — including the CLI's `loong_tui_app::X` uses — resolves unchanged. The palette, the free helpers and `impl App` stay in `lib.rs` (they are used by both the root and the moved draw functions).
Expect only two widening changes: `draw_setup_form` and `draw_local_wizard` become `pub(super)`. *Corrected during execution (Task 20):* this said "the 12 `loong_tui_app::X` uses inside the CLI"; there are 15 occurrences across 8 distinct paths. The count was mine and it was wrong — and the count is not the evidence anyway: what matters is that `git diff --stat crates/cli/` stays empty, which Task 20 showed. Use `pub(super)` rather than the spec's `pub(crate)`; the reach is identical here and the brief is the more specific instruction.

- [ ] **Step 2: All four proofs, then commit**

One extra check this task needs: run `cargo check -p loongagent` and confirm the CLI, which is the only consumer, compiles without a single edit to its own source — `git diff --stat crates/cli/` must be empty.

```bash
git add crates/tui-app/src/
git commit -m "refactor(tui-app): the wizard and the worker are not the screen"
```

### Task 21: `config`'s `lib.rs`

**Files:**
- Create: `crates/config/src/{profile,layer,secret,paths,merge,tests}.rs`
- Modify: `crates/config/src/lib.rs`

- [ ] **Step 1: Move by the spec's tree, keeping the `Raw*` structs with `from_file`**

Follow the spec's §4.4 tree exactly. Keep the `pub use` re-exports in `lib.rs` so `loong_config::Profile` and the other public paths are unchanged, and keep a `pub(crate) use` for anything `catalog.rs`'s rustdoc links to (`catalog.rs:11` links `crate::check_secret_reference`, so a bare move would break the doc link rather than the build). *Corrected during execution (Task 21):* this step named the wrong coupling. `merge` reads `LayerValues`' three private fields, not the `Raw*` structs; the real `Raw*` coupling is `profiles_from_payload`, which reads twelve private `RawModel` / `RawRouter` / `RawRule` fields, so it moves with `LayerValues::from_file` into `layer.rs`. Nothing about the `Raw*` fields changed. The instruction's *conclusion* — keep them together — survived by accident, which is the only reason it did no harm.

- [ ] **Step 2: All four proofs, plus the public surface**

Additionally: snapshot the public item paths of `loong_config` before and after and require them identical. *Corrected during execution (Task 21):* this said "rustdoc JSON or `cargo public-api`", and **neither is available in this environment** — there is no `cargo public-api` and no nightly toolchain (the rustup directory is read-only). Task 21 substituted two checks that do test the claim: `cargo doc` output for the crate before and after (the 29 root item pages, `sidebar-items.js` and `all.html` byte-identical), and the item tool's own output filtered to `vis == pub`, which it can do because every line carries a visibility column (45 of 45 public items present with identical hashes). Do not promise a tool without checking it exists.

```bash
git add crates/config/src/
git commit -m "refactor(config): layering, profiles, secrets and merging are four things"
```

### Task 22: `kernel`'s `turn.rs`

**Files:**
- Create: `crates/kernel/src/turn/{build,confinement,hooks,session,model,loop,compaction,events,recall,nudge,tests}.rs`
- Modify: `crates/kernel/src/turn.rs` (unchanged in name; `lib.rs` untouched)

- [ ] **Step 1: Move the blocks, keeping the write spine in the parent**

`turn.rs` keeps the module doc, the error and config types, the `Kernel` struct, and the items that span every topic: `append`, the `record_*` family, `fire_hooks`, `tool_specs`. That is what keeps this to ~16 `pub(super)` rather than ~30. The other eleven files take the blocks named in the spec's §4.4 tree. `crates/kernel/src/lib.rs` is 17 lines and does not change.
The tests move to `turn/tests.rs`; they reach private items across module boundaries, and the brief's list of three was incomplete — *corrected during execution (Task 22):* there is a fourth, `RECALL_MAX_CHARS`, which moves with `recall_tool` and is widened in `recall.rs`. Of the named three, `narrow_budget` stays in the parent and costs zero widenings while `leaked_tool_call` and `looks_like_pasted_file` move to `nudge.rs` and cost two, so the tests file gains two `use` lines. Decide by which needs fewer widened items, and verify the list against the code rather than against this sentence.
*Two more corrections from the same task.* The brief prescribed one commit; **it is two**, because the moved tests module's rustfmt reflow alters two token streams and `compare` can only read that as LOST or CHANGED — a contiguous block is evidenced by `reflow` instead, and one commit cannot carry both kinds of evidence. See the note above on Ruling 55. And `loop` is a Rust keyword, so the file is `turn/loop.rs` declared as `mod r#loop;` — the plan's tree names the path, not the declaration.

- [ ] **Step 2: All four proofs, plus the public surface**

Additionally: the `loong_kernel` public item paths must be identical before and after (all public types stay in `turn.rs`, so nothing should move in the public path space). Then commit.

```bash
git add crates/kernel/src/
git commit -m "refactor(kernel): the turn loop, its events and its compaction were one 6 500-line file"
```

### Task 23: the CLI

**Files:**
- Create: `crates/cli/src/{control,session,context,server,tui,local,web,repl,mcp,config_cmd,plugin,hooks,sandbox,headless}.rs`
- Modify: `crates/cli/src/main.rs`

- [ ] **Step 1: Move the subcommand families, keeping the spine in `main.rs`**

`main.rs` keeps the module doc, `USAGE`, `main`, `Args` and `impl Args` (43 uses of `args.cwd` and 16 cross-module calls to `settings_with_pack()` make it the free place to stay), `PackState` (its private fields are read from ten sites), `ControlFile`, `config_dir` and `run`. Everything else moves by the spec's §4.4 tree.
This is the only task with a large visibility budget (~61 items `pub(crate)`). It is a binary crate, so `pub(crate)` cannot leak: the `[bin]` target has no exported surface at all.

*Corrected during execution (Task 23), on two counts.* **`ControlFile` stays in the crate root**, not `control.rs` as the tree implies: `tui.rs` and — the larger constraint — `local.rs`'s test module construct its three private fields from ten sites while `control.rs` reads them, and sibling modules cannot see another module's private fields. Field-level widening is unrepresentable in the evidence tool (see §4.4 and Ruling 54), so the root is the only home that costs nothing. The tree's per-topic file names describe where the *commands* go, not where every type they use must live. And the budget came out at **59** widenings rather than the predicted ~61 — a prediction, not a requirement, and the report gives the per-file counts.

- [ ] **Step 2: The four inline test modules move with their hosts**

Each of `mod tests`, `session_tests`, `context_command_tests` and `endpoint_key_tests` moves into the file that now owns the code it tests, which changes what `use super::*` reaches: `super` becomes the host module rather than the crate root, so items they use from the root (`Args`, `SessionSource`, `ControlFile`) need a `use crate::…` line. Moving the tests into the hosts is also what saves about 17 items from needing `pub(crate)`.

- [ ] **Step 3: All four proofs, then commit**

`--min` comes from the before state. The 8 integration tests are the ones that matter most here, because they launch the real binary.

```bash
git add crates/cli/src/
git commit -m "refactor(cli): fourteen subcommand families, each in its own file"
```

---

## Self-review notes

- **Spec coverage:** §1 problem → Phase B. §2 criteria 1 → Phase C, 2 → Phase B, 3 → Phase A, 4 → Phase F, 5 → Phase E. §3 non-goals → honoured by omission and by the "leave alone" clauses in Tasks 18, 19 and 23. §4.1 → Phases B–D. §4.2 → Task 13. §4.3 checks 1–5 → Tasks 1–5. §4.4 → Phase F. §4.5 → Phase E. §5 → the four proofs. §6 sequencing → the phases. §7 risks → the Review Focus list. §8 → the Global Constraints' "do not touch".
- **One spec line the plan interprets rather than transcribes:** Task 14, stated in the task so it can be rejected during review.
- **Task ordering:** `cli/tests/common/mod.rs` in Task 18 and the four splits in Phase F are independent of Phase B; Phase A must come first because it protects the rest.
- **The split order is deliberate:** cheapest and highest-value first (`tui-app`'s tests), hardest and largest last (the CLI), so the evidence machinery is exercised before it carries the biggest move.

---

## Execution rulings

Recorded here so the plan and the execution ledger tell the same story. Each was made during
execution, against the spec as the binding authority, and each names what it costs if it is wrong.
The inline corrections above cite the relevant one.

**This table covers rulings 9 to 18 only** — the ones made while Tasks 1 to 5 were executing. It is a
partial index rather than the record, which is a small deliberate retreat from the sentence above: the
rulings from 19 onward are written *inline at the tasks they affected*, where a reader meets them next
to the text they correct, and duplicating them here would recreate exactly the two-copies-of-one-fact
drift this plan exists to remove. The complete list, including every deferred Minor, is in the
execution ledger (`.superpowers/sdd/2026-10-16-navigability/progress.md`, which is gitignored because
it is a working record rather than a document), and the final report collects the rulings for the
human. A reader of this plan alone sees every correction that changes what to do; what they do not see
here is the bookkeeping.

| # | Task | Ruling | Cost if wrong |
|---|---|---|---|
| 9 | 1 | Seed the broken link in `DEVELOPMENT.md`, not `docs/runbooks/windows-build.md`, which has no links | A reader of the uncorrected draft retries an impossible seed |
| 10 | 1 (fix) | Two Minors joined the fix round: closing-fence and unclosed-fence handling, and the traceback past `except OSError`. Both are the same silent-masking seam as the Important finding | The round was two small edits larger than the review required |
| 11 | 1 | Ledger correction: the missing-`--test-counts` case exits 1, not 2. (The "exit 2" was the ledger's own wording, not the plan's) | A reader trusts a wrong exit code, which no caller branches on |
| 12 | 2 | The task that creates a document adds its index row, following Ruling 6 | An unindexed generated map, which Task 3's own check would report |
| 13 | 3 | The index "names" a document in a link target **or** a table row's first cell | 33 documents would fail on a literal reading |
| 14 | 3 | `orphans` reads the working tree; `links` and `crate-map` read tracked files | A stray uncommitted draft in `docs/` fails the gate until indexed or deleted |
| 15 | 3 | "Direct subdirectory" means the first segment below `docs/` | Documents two levels deep need their own row instead of their top segment's |
| 16 | 4 | The citation check masks code spans and fences rather than exempting `docs/superpowers/` | An impossible citation written inside backticks goes unnoticed — the tolerance the `links` check already extends |
| 17 | 4 | The seed is bare prose, and the same task proves the backticked plan occurrence is tolerated | A vacuous red proof, or a check that silently ignores real citations |
| 18 | 4 (fix) | The empty-walk guard joined the fix round, because the Important finding opened that exact seam | Two lines more than the review asked for; a markdown-free `--root` now errors instead of reporting a vacuous OK |

One correction to the ledger itself, recorded because the rulings list is what the human reads: the
first version of Ruling 11 attributed an "exit 2" to the plan's Task 5 text. `grep -n 'exit 2'` over
this plan returns nothing — the phrase was the ledger's own Ruling 2 wording, misattributed. No plan
correction was owed for it.

**Deferred Minors.** Every task's Minor findings are listed in the execution ledger with a one-line
description, for the final whole-branch review to triage. They are not reproduced here: the ledger is
their single home, and duplicating them is the drift this plan exists to remove.
