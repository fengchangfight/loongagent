# Skills Directory Convention Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `.loongagent/skills/` with `.agents/skills/` as the workspace skills directory across the entire codebase.

**Architecture:** This is a mechanical string replacement across source files, tests, and documentation. The change is a breaking convention change with no compatibility shim.

**Tech Stack:** Rust, markdown documentation.

**Spec:** `docs/design/extension-model.md` §2 (Skills and commands: content, not code)

## Global Constraints

- AGENTS.md is not modified (per user instruction).
- `.loongagent/commands/` is not modified (out of scope).
- Plugin skill directories (`skills/<name>/SKILL.md` inside a plugin checkout) are not modified.
- All tests must pass after the change.

## Review Focus

1. **Missed occurrence**: a `.loongagent/skills` reference survives in a file not listed in the spec. Test: `grep -r "\.loongagent/skills" crates/ docs/ crates/pack/templates/` returns zero matches.
2. **Broken test**: a test constructs `.loongagent/skills/` and fails because the loader now reads `.agents/skills/`. Test: `cargo test -p loong-skills -p loong-plugin -p loong-cli -p loong-kernel` passes.
3. **Stale documentation**: a doc file still tells users to put skills in `.loongagent/skills/`. Test: `grep -r "\.loongagent/skills" docs/ README.md` returns zero matches.

---

### Task 1: Update `crates/skills/src/lib.rs`

**Files:**
- Modify: `crates/skills/src/lib.rs`

**Interfaces:**
- Consumes: none
- Produces: `SkillSet::load_in` now reads `.agents/skills/<name>/SKILL.md` instead of `.loongagent/skills/<name>/SKILL.md`

- [ ] **Step 1: Update the module doc comment**

Change line 21 from `.loongagent/skills/<name>/SKILL.md` to `.agents/skills/<name>/SKILL.md`.

- [ ] **Step 2: Update `scan_root` workspace layout detection**

Find the line that checks for a `skills/` subdirectory to decide "workspace shape". The logic stays the same; no change needed here because it checks for the literal directory name `skills`, not the parent path.

- [ ] **Step 3: Update `SkillSet::load_in` workspace path**

Find where `load_in` constructs the workspace skills path. Change `.loongagent/skills` to `.agents/skills`.

- [ ] **Step 4: Update tests in the same file**

Update lines 806 and 814: change `.loongagent/skills` to `.agents/skills`.

- [ ] **Step 5: Run tests**

```bash
cargo test -p loong-skills
```

Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/skills/src/lib.rs
git commit -m "refactor: workspace skills dir .loongagent/skills -> .agents/skills"
```

---

### Task 2: Update `crates/plugin/src/lib.rs`

**Files:**
- Modify: `crates/plugin/src/lib.rs`

**Interfaces:**
- Consumes: none
- Produces: collision message and doc comments now reference `.agents/skills/`

- [ ] **Step 1: Update doc comment on line 45**

Change `.loongagent/skills` to `.agents/skills`.

- [ ] **Step 2: Update collision message around line 215**

Change `.loongagent/skills/<name>/` to `.agents/skills/<name>/`.

- [ ] **Step 3: Run tests**

```bash
cargo test -p loong-plugin
```

Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/plugin/src/lib.rs
git commit -m "refactor: plugin collision message uses .agents/skills"
```

---

### Task 3: `crates/cli/src/plugin.rs` — **no-op, verified**

**Files:**
- None. `crates/cli/src/plugin.rs` builds no path of its own: `load_catalogue` calls
  `loong_skills::SkillSet::load_in`, so Task 1 covers it.

- [x] **Step 1: Confirmed no `.loongagent/skills` string in the file**

```bash
grep -n "\.loongagent/skills" crates/cli/src/plugin.rs
```

Expected and observed: no matches.

---

### Task 4: Update `crates/cli/tests/plugins.rs`

**Files:**
- Modify: `crates/cli/tests/plugins.rs`

**Interfaces:**
- Consumes: none
- Produces: integration tests now create skills in `.agents/skills/`

- [ ] **Step 1: Update line 58**

Change `ws.join(".loongagent/skills")` to `ws.join(".agents/skills")`.

- [ ] **Step 2: Run tests**

```bash
cargo test -p loong-cli --test plugins
```

Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add crates/cli/tests/plugins.rs
git commit -m "test: CLI plugin tests use .agents/skills"
```

---

### Task 5: Update `crates/kernel/src/turn/tests.rs`

**Files:**
- Modify: `crates/kernel/src/turn/tests.rs`

**Interfaces:**
- Consumes: none
- Produces: kernel turn tests now use `.agents/skills/`

- [ ] **Step 1: Update line 3165**

Change `t.dir.join(".loongagent/skills/broken")` to `t.dir.join(".agents/skills/broken")`.

- [ ] **Step 2: Run tests**

```bash
cargo test -p loong-kernel
```

Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add crates/kernel/src/turn/tests.rs
git commit -m "test: kernel turn tests use .agents/skills"
```

---

### Task 6: `crates/pack/templates/AGENTS.md` — **no-op, verified**

**Files:**
- None. `crates/pack/templates/` does not exist in this tree; the pack crate ships no
  markdown template that names the skills directory.

- [x] **Step 1: Confirmed**

```bash
ls crates/pack/templates/ ; grep -rn "\.loongagent/skills" crates/pack/
```

Expected and observed: no such directory, no matches.

---

### Task 7: Update documentation

**Files:**
- Modify: `docs/design/extension-model.md`
- Modify: `docs/decisions/0018-plugin-install-and-bootstrap.md`
- Modify: `docs/05-parity-and-synthesis.md`

**Interfaces:**
- Consumes: none
- Produces: all docs reference `.agents/skills/` as the workspace skills directory

- [ ] **Step 1: Update `docs/design/extension-model.md` line 70**

Change `.loongagent/skills/<name>/SKILL.md` to `.agents/skills/<name>/SKILL.md`.

- [ ] **Step 2: Update `docs/decisions/0018-plugin-install-and-bootstrap.md` lines 57 and 189**

Change both occurrences of `.loongagent/skills` to `.agents/skills`.

- [ ] **Step 3: Update `docs/05-parity-and-synthesis.md` line 90**

Change `.loongagent/skills/<name>/SKILL.md` to `.agents/skills/<name>/SKILL.md`.

- [ ] **Step 4: Verify no remaining references**

```bash
grep -r "\.loongagent/skills" crates/ docs/ README.md crates/pack/templates/
```

Expected: zero matches (or only matches in `.superpowers/` archive files, which are historical records).

- [ ] **Step 5: Run full test suite**

```bash
./scripts/check.sh
```

Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add docs/design/extension-model.md docs/decisions/0018-plugin-install-and-bootstrap.md docs/05-parity-and-synthesis.md
git commit -m "docs: update skills directory convention to .agents/skills"
```

---

## Execution record

Done in one pass on branch `skills-directory-convention` (worktree `.worktrees/skills-dir`).

**Deviations from the plan as written**

- **Task 1 grew a real refactor.** `scan_root` did not just name a path: it read one root in
  both shapes (`skills/` and `commands/`). Since the two now live under different parents, it
  was split into `scan_skills_root` and `scan_commands_root`. `load_in` scans
  `.agents/` for skills, `.loongagent/` for commands, and each `skill_dirs` root for both.
  Two `skills` crate tests asserted the old single-root behaviour and were updated.
- **Task 3 and Task 6 were no-ops.** The CLI reaches the loader through `SkillSet::load_in`
  and never builds the path itself; `crates/pack/templates/` does not exist.
- **No standalone design document.** The decision was folded into
  `docs/design/extension-model.md` §2, which already owns the skills loader. A second
  document for one path would have given one topic two authorities.
- **`README.md` was also updated** (it lists the workspace skill sources), beyond the three
  documents the plan named.

**Verification**

- `cargo test -p loong-skills` — 17 passed
- `cargo test -p loong-plugin` — 19 passed
- `cargo test -p loong-kernel` — 130 passed, 1 ignored
- `cargo test -p loongagent --test plugins` — 6 passed
- `python3 scripts/check-docs.py` — OK
- `python3 scripts/check-modules.py` — OK
- `grep -rn "\.loongagent/skills"` — no matches outside `.superpowers/` and `target/`

**Pre-existing failures, not caused by this change:** six tests in
`crates/cli/src/instructions.rs` (`a_directory_named_agents_md_warns`, `a_fifo_is_not_opened`,
`no_git_reads_only_the_working_directory`, `skips_a_non_utf8_file`, `skips_an_empty_file`,
`truncates_at_the_file_cap`) fail identically on the branch point with the changes stashed.
