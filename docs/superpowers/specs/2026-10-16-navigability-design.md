# Navigability: one authority per topic, and a gate that keeps it true

- **Date:** 2026-10-16
- **Status:** Approved in conversation; this document is the spec the implementation plan is written from
- **Scope:** documentation architecture, documentation enforcement, mechanical code splits, and the
  zero-API-change part of redundancy removal. No behaviour changes, no `pub` API changes, no crate
  boundary changes.

## 1. Problem

The repository is healthy where it is machine-checked and rotted where it is not.

`scripts/check.sh` gates formatting, schema generation, crate layering, 736 tests and a release
build. Nothing checks the documents, and 10 700 lines of documents show it:

| Defect | Evidence |
|---|---|
| The flagship "one-read picture" says the project has not started | `docs/04-system-overview.md` §10: "**Implementation: not started.**" — with ~55 k lines of Rust and 736 passing tests behind it |
| Its own numbers are four ADRs behind | `docs/04` header and §9 cite `ADR-0001…ADR-0014` (18 exist), `14 cited deep dives` (15 exist), `T-01…T-21` (T-25 exists); its decision table omits ADR-0016 and ADR-0018 |
| A maintainer document contradicts itself about an implemented feature | `DEVELOPMENT.md:756` "Response streaming is not implemented" versus `:917-923` describing SSE streaming and `model.delta` as shipped |
| A maintainer document mixes shipped work with unshipped in one list | `DEVELOPMENT.md:1025-1026` "**Not started**: session export/import, WASM extensions, the web client, compaction, the control plane." Of the five, the web client (`crates/web`, the `web` arm), compaction (`Kernel::compact_if_needed`) and the control plane (`crates/control/src/bin/loongagent-control.rs`) exist; session export/import and WASM extensions do not. The line is therefore neither wholly true nor wholly false, and a reader cannot tell which half to trust without reading the code — which is worse than either |
| The crate map is hand-copied and 13 crates short | `DEVELOPMENT.md:707` "Nine of the eleven crates"; the workspace has 24. The authoritative list is `scripts/layering_rules.py` |
| Test counts are hand-typed and already stale twice over | `DEVELOPMENT.md:698` "69 tests"; README says 736 |
| The stated document convention is honoured by almost nobody | `docs/README.md:14-15` requires Status and Last updated on every doc; 13 of 15 `research/` docs have no Status, none has a date, both runbooks have neither, and `docs/README.md` itself has no Status |
| Three indexes, three "deliberate no"s lists, two CI configs that have already drifted apart | `docs/README.md:20-33` / `docs/04` §9 / `README.md:371-381`; `docs/01` §D / `docs/05` §D / `docs/04` §6; `.gitea/workflows/ci.yml` vs `.github/workflows/ci.yml` |
| One conflict nobody can resolve from the docs as written | The control-plane pack *is* signed and verified (`crates/pack/src/lib.rs:1-22` Ed25519 against a pinned key; `crates/cli/src/main.rs:743` verifies before the pack occupies the org layer), while `README.md:360/:366` say signing is not implemented. What is genuinely unsigned is the *local* org layer — a plain `/etc/loongagent/profile.toml` read as a layer at `crates/config/src/lib.rs:1401`. No document states the distinction |

Two measured structural gaps, which are the reason this is worth doing at all:

- **No authority exists for the topics that matter.** A `grep` over documents and sources finds 22
  files mentioning the permission decision and 25 mentioning the wire protocol. When a topic is
  written down in five places, the reader's cost is not the reading, it is deciding which one is
  current.
- **The maintainer layer does not exist.** Zero documents explain how to add a built-in tool, and
  zero explain how routing picks a model. `DEVELOPMENT.md` says how to *run* this project, not how
  to *change* it.

### Root cause

Not wrongness — **absence of verification**. Three duplicates of the same list drift apart because
nothing compares them; a hand-typed test count rots because nothing recomputes it. Any design that
only fixes today's wrong sentences will be wrong again in a month.

## 2. Goals and success criteria

Goal: a person or an agent can find the authoritative answer to a question in one hop, and the
answers cannot silently rot.

| # | Criterion | How it is measured |
|---|---|---|
| 1 | Exactly one authority per topic | One place each for: the current architecture, status, the "deliberate no"s, the crate map, the document index |
| 2 | Every statically falsifiable statement is true | Every number, ADR range, link and status claim in §1 above verified after the change |
| 3 | Document rot is caught by a gate | `scripts/check-docs.py` runs inside `scripts/check.sh` and each of its checks has been shown to fail on a seeded defect |
| 4 | Four oversized files are split with zero API and behaviour change | Every moved item token-identical (whole items, not bare bodies — see §4.4); `cargo test -- --list` name set unchanged; no `Cargo.toml` diff; 736 tests still pass |
| 5 | Redundancy that costs nothing to remove is gone | Zero declared-but-unused dependencies; zero dead functions |

## 3. Non-goals

Deliberately out of scope, each for a stated reason:

- **No crate boundary changes.** `loong-tui` / `loong-tui-app`, the 389-line `loong-web`, and the
  eleven-crate layer taxonomy stay as they are. Moving boundaries is a design change, not a
  navigability change, and it would invalidate the mechanical-split verification.
- **No `pub` API changes and no behaviour changes.** The splits may add `pub(crate)` / `pub(super)`
  only, and only where a moved item is called from a sibling module.
- **`docs/00-vision.md`, `docs/01-landscape-synthesis.md`, `docs/02-topics.md` are not deleted.**
  `01` is superseded by `05` and is archived. `00` is the only statement of why the project exists.
  `02` holds 21 topics still marked `Seed` — undiscussed ideas, not history — and deleting it would
  destroy live material. Its real defect is that statuses did not follow decisions (T-11 and T-14
  are `Promoted`, T-16 is `Merged`, the rest are not), and that is what gets fixed.
- **`docs/06-context-engineering.md` and `ADR-0017` are not translated.** ~1 400 lines of Chinese
  technical prose; the translation cost and terminology loss exceed the benefit. They are marked as
  the documented language exceptions instead.
- **Cross-crate helper duplication is reported, not removed.** Measured after execution: `now_ms` (4
  copies — `control`, `hooks` — where it is already `pub` — `plugin` and `eventlog`), `first_line` (3),
  `expand_home` (2), `truncate` (2) and `clip` (2): five names, thirteen definitions, and the first
  draft of this list both miscounted `now_ms` as 3 and omitted `truncate` and `clip` entirely.
  Consolidating any of them needs a new shared public surface, which is an API change, and five
  ten-line functions do not justify one. Cost of leaving them: thirteen copies that can drift; cost of
  consolidating: a new public surface in a workspace whose whole design is that the kernel is the only
  fixed thing. Recorded as T-26 in `docs/02-topics.md` rather than fixed.
- **`impl App` in `tui-app` is not split.** 48 mutually-calling methods and 4 private fields the
  tests write directly; splitting it costs ~48 method-level visibility changes for a smaller
  readability gain than moving the 3 734 lines of tests out of the file already gives.
- **The two identical `build.rs` files and the `named` test helper pair stay.** A `build.rs` per
  crate is normal shape, and the two `named` helpers belong to different crates' test modules.

## 4. Design

### 4.1 Documentation architecture

Three entries, each with one job:

- `README.md` (Chinese) — the newcomer's entry: what this is, install, run, current status.
- `DEVELOPMENT.md` (English) — the maintainer's entry: toolchain, gates, how to change things, how
  to release.
- `docs/README.md` (English) — the only index.

Authority assignment. Every topic below is expanded in exactly one place; every other mention links
to it:

| Topic | Sole authority | Duplicates removed |
|---|---|---|
| The architecture as it stands | `docs/04-system-overview.md` | — |
| Decisions | `docs/decisions/*` (18 ADRs; **body text unchanged**) | — |
| Status: what works, what does not | `README.md` §"状态，说实话" | `DEVELOPMENT.md` §"What works today" and §"What is not done yet" shrink to a pointer |
| The deliberate "no"s | `docs/05` §D | `docs/04` §6; `docs/01` §D goes with `01` into the archive |
| Crate map and layer rules | **generated** `docs/crate-map.md`, from `scripts/layering_rules.py` | `DEVELOPMENT.md` §Layout table |
| Index of documents | `docs/README.md` | `docs/04` §9, the `README.md` §文档 table |
| Superseded history | `docs/archive/` | `docs/03` Part 1 and Part 3 (~290 lines of discussion log), `docs/01` |
| Idea backlog | `docs/02-topics.md`, retained | — |

Rewritten rather than deleted, each a currently false statement:

- `docs/04` §10 becomes the current state; its §4 decision table gains ADR-0016 and ADR-0018; its
  header and §9 counts are corrected.
- `DEVELOPMENT.md` §"What is not done yet" is corrected to facts (its item 12 and several others),
  and the "Response streaming is not implemented" sentence is removed.
- The signing story is stated once, in the form the code proves: the control-plane pack is signed
  and verified before it can occupy the org layer; the *local* org layer file is not signed and is
  trusted because a machine administrator owns it. `README.md` lines 360 and 366 and `docs/05` §C
  are corrected to this wording.
- `docs/00-vision.md`'s "desktop app" surface bullet and its three resolved open questions are
  corrected; ADR-0009 is the authority on surfaces.

New document `docs/07-extending.md`, closing the measured zero-coverage gap: how to add a built-in
tool, how to add an event, how a hook attaches, and how routing picks a model. It is written from
the code and the ADRs and points at them rather than restating them.

### 4.2 Conventions

**Status lives in the index, not in every document.** The `docs/README.md` table's Status column
becomes the only place a numbered design document's or research note's status is recorded. The
per-document header requirement in `docs/README.md:14-15` is removed: **35 of the 43 documents did
not honour it** — only `docs/00`–`06` and one research note carried both lines, the other 13 research
notes and both runbooks carried neither, and `docs/README.md` itself has a date but no status. A
convention that is almost universally ignored teaches the reader to ignore conventions. Those
documents keep a title; they no longer carry a Status or Last updated line.

**ADRs are the exception and keep their `Status:` line.** All 18 have one, all 18 are accurate, and
an ADR's status (`Proposed` / `Accepted` / `Superseded`) is part of the record rather than metadata
about the file. `docs/README.md`'s Status column points at the ADR for those, and the index row for
`decisions/` already carries the summary.

**Language, by layer.** `README.md` is Chinese (its audience is the person installing it).
`DEVELOPMENT.md`, `docs/**` and `docs/decisions/**` are English. `docs/06-context-engineering.md`
and `docs/decisions/0017-layered-context.md` are Chinese and are explicitly marked as the two
exceptions with a `Language: zh` line; translating them is not in scope.

### 4.3 The document gate — `scripts/check-docs.py`

Wired into `scripts/check.sh` beside the existing gates. Five checks:

1. **Every relative link resolves.** Every `[text](target)` in every tracked `.md` whose target is
   not a URL or a fragment must exist on disk. *(Implemented during this work; it is what found
   `DEVELOPMENT.md:82`.)*
2. **The crate map is current.** `docs/crate-map.md` must equal what `scripts/layering_rules.py`
   generates from `ALLOWED` — the same regenerate-and-diff shape as `gen-schema.py --check`, so the
   human-readable map cannot disagree with the enforced rules.
3. **No orphan documents.** Every `.md` under `docs/` is either named in the `docs/README.md` index
   or lives under a subdirectory (`decisions/`, `research/`, `runbooks/`, `superpowers/`) that the
   index lists as a row. This is what keeps "the only index" true.
4. **The stated test counts are the real ones.** `README.md` states how many tests pass and how
   many are ignored, and the gate recomputes both from the suite's own output: it sums the `passed`
   and `ignored` fields of every `test result:` line that `cargo test --workspace` prints, which
   `check.sh` already runs, and fails when either number disagrees with `README.md`. The values
   today are **736 passing, 11 ignored**, and 736 + 11 = 747, which is exactly the number of entries
   `cargo test -- --list` prints — so the three numbers reconcile. Source-level counts do not:
   there are 748 `#[test]` / `#[tokio::test]` attributes and 13 occurrences of the string
   `#[ignore` (11 attributes, 2 of them prose in doc comments, and all 11 attributes account for the
   11 ignored results). The suite's own totals are the authority, which is why the gate parses them
   rather than the source.
5. **No impossible citations.** No document may cite an `ADR-00NN` above the highest existing ADR
   number, nor a `T-NN` above the highest existing topic.

Requirement for every check in this file and in §5: **each check must be demonstrated to fail on a
seeded defect before it is trusted.** A gate that has never failed has not been shown to work.

### 4.4 Mechanical code splits

Hard constraint: the only lines that may change inside the moved code are the addition of `mod`,
`use`, `pub use`, `pub(crate)` and `pub(super)`. Every moved item — signature, attributes and
visibility included — is token-identical; byte-identity is unachievable because rustfmt reflows a
dedented block and `cargo fmt --check` is a gate (§4.4 has the corrected standard).

| File | Lines now | Target | Visibility cost |
|---|---|---|---|
| `crates/cli/src/main.rs` | 6 924 | `main.rs` ≈640 (doc, `use`, `USAGE`, `main`, `Args` + `impl Args`, `config_dir`, `run`) plus `control`, `session`, `context`, `server`, `tui`, `local`, `web`, `repl`, `mcp`, `config_cmd`, `plugin`, `hooks`, `sandbox`, `headless` | ~61 items `pub(crate)`; a binary crate, so nothing is exported |
| `crates/tui-app/src/lib.rs` | 7 661 | `tests.rs` (3 734 lines, `#[cfg(test)] mod tests;`) plus `worker`, `setup`, `wizard`, `surface`; `lib.rs` ≈2 520 | 2 items `pub(crate)`, plus `pub use` re-exports keeping every existing path |
| `crates/kernel/src/turn.rs` | 6 509 | `turn.rs` (≈340) plus `turn/{build,confinement,hooks,session,model,loop,compaction,events,recall,nudge,tests}.rs` | ~16 items `pub(super)`, because the write spine stays in the parent; ≈30 if it were moved too |
| `crates/config/src/lib.rs` | 3 155 | `lib.rs` (≈190, keeps the `pub use` re-exports) plus `{profile,layer,secret,paths,merge,tests}.rs` | ~8–10 items `pub(super)` |

Three facts make this mechanical rather than invasive:

- An item's default visibility covers the defining module **and its descendants**. Putting the new
  modules under the original module (`turn/tests.rs` reached from `turn.rs`, `crate::…` reached from
  `main.rs`) is what keeps `Kernel`'s 38 private fields and `config`'s `Raw*` structure private. Only
  items called *across sibling* modules need widening.
- The items that span every topic stay in the parent: in `turn.rs`, `append`, the `record_*` family,
  `fire_hooks`, `tool_specs`; in `cli`, `Args`, `impl Args`, `PackState`, `USAGE`, `run`. This is
  what keeps the visibility budget at ~16 rather than ~30 for the kernel.
- `loong_config`'s `Raw*` structs (7 types, ~40 serde fields) must stay in the same module as
  `LayerValues::from_file`, because `merge` consumes their private fields. Same-module placement is
  the whole reason no field-level widening is needed.

`crates/kernel/src/lib.rs` and the `pub use` shape of `loong_config` and `loong_tui_app` are
unchanged, so all external call sites — including the 12 `loong_tui_app::X` uses inside the CLI —
keep compiling untouched.

### 4.5 Redundancy removal (all zero-API-change)

- **Delete 7 declared-but-unused dependencies**: `loong-protocol` from `crates/cli/Cargo.toml`
  (`loong_protocol` appears zero times in the whole crate), and `serde` from `eventlog`, `sandbox`,
  `skills`, `tools`, `tui-app` and `control` (`Serialize`, `Deserialize` and `serde::` appear zero
  times in all six). *Corrected during execution (Task 16):* the first draft added "each uses
  `serde_json`", which is false for `loong-sandbox` — it uses neither `serde` nor `serde_json`, and its
  declaration was unused all the same, so the removal stands. `serde` must also **not** be expected to
  leave the dependency graph: it stays reachable transitively, and `crates/cli` keeps its own
  declaration because `crates/cli/src/main.rs` derives `Serialize`. The proof is that the target stops
  being a *direct* dependency, which is why the plan's `cargo tree` check needs `--depth 1`.
- **Delete one dead function**: `pack_of` and its `#[allow(dead_code)]` at
  `crates/control/src/bin/loongagent-control.rs:706`. It is the workspace's only `allow(dead_code)`,
  and the workspace compiles with zero warnings without it.
- **Collapse one duplicated test helper**: `isolated_command`, three byte-identical copies in
  `crates/cli/tests/{tui_surface,web_surface,vertical_slice}.rs`, moves to
  `crates/cli/tests/common/mod.rs`.
- **Record, do not fix**: the five cross-crate helper duplicates named in §3.
- **Not touched without an explicit instruction**: the three untracked files at the repository root
  (`tank-battle.html`, `.smoke.js`, `:memory:.ses`), and the stale local artefacts under gitignored
  paths (`dist/`'s 4 MB tarball from 2026-10-08, `target/`'s `lib.rs.bak` / `lib.rs.good` /
  `turn.rs.good`).

## 5. Verification

Every phase ends with `./scripts/check.sh` green. The splits additionally require four independent
pieces of evidence, because "it compiles and the tests pass" does not prove nothing moved:

1. **Every moved item is token-identical.** A script extracts each whole item — `fn`, `impl`,
   `struct`, `enum`, `trait`, `type`, `const`, `static`, `mod`, `use`, `macro_rules!`, with its
   attributes, visibility and signature included — from `git show HEAD:<old path>` and from the new
   files, hashes each item's canonical token stream, and compares `(sha256, kind, name, visibility)`
   tuples; they must be equal. The script must be shown to fail both when an item's body is edited and
   when only a **signature** or visibility is edited. *Corrected during execution (Task 19):* this
   originally said "function bodies are byte-identical", which is unachievable — rustfmt dedents and
   reflows moved text and `cargo fmt --check` is a gate — and which left signatures, visibility,
   attributes and every non-`fn` item outside the evidence. Alongside the comparison, each split shows
   that the committed file equals rustfmt's output applied to a byte-preserving dedent of the original,
   so that the only difference between the two is rustfmt's own reflow. *Corrected again during
   execution (Task 20):* that assertion cannot hold literally for a **submodule** split, because a
   submodule does not inherit the parent's `use` lines and so each new file must carry a header no
   contiguous range of the old file contains — and the body may carry a permitted visibility widening
   the old text lacks. The check therefore takes two bounded allowances, `--header-file` for the
   permitted leading lines and `--allow-widening NAME` for a line that differs by its visibility
   keyword only (verified by stripping the visibility token from both sides), and still fails on
   anything else.
   **Strict equality is Task 19's case only.** Tasks 20–23 split a file into submodules, which
   legitimately adds `mod` / `use` / `pub use` lines and may widen an item to `pub(crate)` or
   `pub(super)` so a sibling module can reach it. Their rule is directional: every before-item still
   present on `(kind, name)`; its hash unchanged, *or* differing only by a visibility widening —
   demonstrated by a further **visibility-stripped hash** that must match; every other extra item
   listed explicitly in the report with its reason. An unexplained extra is a finding.
2. **The test set is unchanged.** `cargo test --workspace -- --list` sorted, before and after, must
   be identical — including the tests defined inside the two library files being split (49 in
   `cli/main.rs`, 133 in `tui-app/lib.rs`), none lost and none renamed. Normalise the
   `Running … (target/debug/deps/<name>-<hash>)` lines down to `<name>` **before** comparing: the
   hash changes whenever the code changes, so a raw comparison fails for the wrong reason.
3. **No dependency or manifest change.** `git diff -- '**/Cargo.toml'` is empty for the split
   commits, and `cargo tree -p loongagent` is unchanged.
4. **The real binary still works.** The 8 integration tests under `crates/cli/tests/`, which launch
   the actual binary (for example `vertical_slice.rs`), pass.

## 6. Sequencing

1. **Facts and the gate.** Every false statement in §1 corrected, stale counts fixed, crate map
   generated, and `scripts/check-docs.py` added to `check.sh`. This goes first so that the gate is
   protecting every later change, including its own.
2. **Index and authority collapse.** Archive the superseded history, remove the duplicate indexes
   and the duplicate "no"s lists, adopt status-in-the-index.
3. **The maintainer layer.** `docs/07-extending.md`.
4. **Redundancy.** The 7 dependencies, the dead function, the test helper.
5. **The four splits**, one commit each, each followed by the full gate.

## 7. Risks

| Risk | Containment |
|---|---|
| A "mechanical" move silently changes behaviour | The four pieces of evidence in §5 are required per split, not per phase |
| Widening visibility leaks into the public API | `pub(crate)` / `pub(super)` cannot reach a `pub` surface; a rustdoc item-path snapshot (or `cargo public-api`) is compared before and after for the three library crates being split — `loong-kernel`, `loong-config` and `loong-tui-app` — and must be unchanged |
| The generated crate map loses the narrative the hand-written table had | The generator emits each crate's existing `//!` header line (all 24 crates have 5–38 lines of it) beside its allowed edges |
| Collapsing the index or the status line hides a document | Check 3 of §4.3 makes an unindexed document a gate failure |
| The document gate becomes a rubber stamp | §4.3 requires every check to be shown failing on a seeded defect |
| Deleting `docs/01` loses a synthesis someone still wants | It moves to `docs/archive/`, and `docs/05` already supersedes it; nothing is destroyed |

## 8. Decisions left to the human

These are deliberately not decided here:

- Whether `tank-battle.html` and `.smoke.js` should be moved out of the repository (they belong to a
  different project and a `git add -A` would sweep them in). Nothing is moved until asked.
- Whether the stale local artefacts under `dist/` and `target/` should be deleted. They are
  gitignored, so they cost review noise rather than repository weight.
