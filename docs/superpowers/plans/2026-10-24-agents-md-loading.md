# AGENTS.md Loading Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use subagent-driven-development (recommended) or executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Load `AGENTS.md` from the user, project (git root to working directory) and local scopes at session build, and seat each file as a pinned `Rules` block the model sees before its first turn.

**Architecture:** The CLI discovers and reads the files, because it is the composition root that knows `--cwd` and the user's home. The kernel receives `InstructionBlock` values in `KernelConfig`, seats one `System` message per file, and accounts for it in the ledger as `Rules`. Compaction and the cognitive map learn that the system prompt may be followed by several more leading system blocks.

**Tech Stack:** Rust workspace; `cargo test`; no new dependency.

**Spec:** `docs/superpowers/specs/2026-10-24-agents-md-loading-design.md`

## Global Constraints

- ADR-0008: `AGENTS.md` is native; `CLAUDE.md` is not read live and no importer is built here.
- Instructions are advisory text. They must not be able to grant permissions, change the PDP, or widen a config cap.
- One `System` message per file; ledger `Category::Rules`; `Lifetime::Persistent`; `pinned = true`.
- Caps: 32 KiB per file, 64 KiB total. Truncate at a UTF-8 character boundary and append `[loongagent: truncated at N bytes]`. A broken file warns and is skipped; it never stops the session.
- No new event kind. Startup warnings and the summary go to stderr as `[instructions] ...`.
- Every new `.rs` file opens with the repository's `Purpose:` / `Serves` / `Design` / `Upstream` / `Downstream` / `Verify` module contract, and every item carries its doc block. `python3 scripts/check-modules.py` is part of the gate.
- New maintained documents carry `Up`, `Authority`, `Code`, `Verify`; the design doc must stay within 250 lines and be linked from `docs/design/README.md`.
- `README.md` has one `<!-- test-counts: N passed, M ignored -->` line; update it to the totals the final test run reports.
- Commit only the files named in each task. The worktree may contain unrelated changes; never `git add -A`.

## Review Focus

These are the input classes most likely to bite a person, in order, and where each is pinned:

1. **An unreadable, non-UTF-8, oversized or empty file must not stop the session.** Task 3's `skips_a_non_utf8_file`, `skips_an_empty_file` and `truncates_at_the_file_cap` tests.
2. **A directory with no `.git` must not read a parent's `AGENTS.md`.** Task 3's `no_git_reads_only_the_working_directory` test.
3. **Compaction must not drop instruction blocks.** Task 2's `compaction_keeps_leading_system_blocks` test.
4. **The cognitive map must not split the system prompt from its instructions.** Task 2's `the_map_follows_instruction_blocks` test.
5. **The wiring must actually set `KernelConfig.instructions`; an empty vector is the likely silent failure.** Task 4's `build_kernel_seats_agents_md_as_a_rule` test.

---

### Task 1: Kernel instruction values and seating

**Files:**
- Modify: `crates/kernel/src/turn.rs` (types, `KernelConfig`, `mod instructions;`)
- Modify: `crates/kernel/src/lib.rs` (re-export the two new types)
- Create: `crates/kernel/src/turn/instructions.rs`
- Modify: `crates/kernel/src/turn/build.rs` (call `seat_instructions`)
- Test: `crates/kernel/src/turn/tests.rs`

**Interfaces:**
- Consumes: `KernelConfig`, `ChatMessage`, `Ledger`, `Category`, `Lifetime`.
- Produces: `pub enum InstructionScope { User, Project, Local }` (derive `Debug, Clone, Copy, PartialEq, Eq`); `pub struct InstructionBlock { pub scope: InstructionScope, pub source: String, pub text: String }` (derive `Debug, Clone, PartialEq, Eq`); `KernelConfig.instructions: Vec<InstructionBlock>`; `Kernel::seat_instructions()`.

- [ ] **Step 1: Write the failing seating test**

Add to `crates/kernel/src/turn/tests.rs`:

```rust
/// Purpose: A file's instructions sit after the system prompt and before the conversation, accounted as pinned rules.
#[test]
fn instructions_are_seated_after_the_system_prompt() {
    let t = Temp::new();
    let (model, seen) = RecordingModel::new(vec![response(vec![], Some("ok"))]);
    let mut config = KernelConfig::new(&t.dir);
    config.instructions = vec![InstructionBlock {
        scope: InstructionScope::Project,
        source: "/repo/AGENTS.md".to_string(),
        text: "ALWAYS-READ-THE-SPEC".to_string(),
    }];
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    k.submit("say hi").unwrap();

    let seen = seen.lock().unwrap();
    let sent = &seen[0];
    assert!(matches!(sent.first(), Some(ChatMessage::System { .. })));
    let instructions = match sent.get(1) {
        Some(ChatMessage::System { text }) => text,
        other => panic!("the second message is the instruction block, got {other:?}"),
    };
    assert!(instructions.contains("Project instructions from /repo/AGENTS.md"));
    assert!(instructions.contains("ALWAYS-READ-THE-SPEC"));
    assert!(
        instructions.contains("do not grant permissions"),
        "the block says it is advisory: {instructions}"
    );

    let row = k
        .ledger()
        .explain()
        .into_iter()
        .find(|row| row.source == "/repo/AGENTS.md")
        .expect("the instruction has a ledger entry");
    assert_eq!(row.category, "rules");
    assert!(row.pinned, "rules are pinned");
}
```

- [ ] **Step 2: Write the no-file and context-explain tests**

```rust
/// Purpose: No instruction files means the request has exactly the system prompt and the user message.
#[test]
fn no_instructions_means_no_extra_messages() {
    let t = Temp::new();
    let (model, seen) = RecordingModel::new(vec![response(vec![], Some("ok"))]);
    let mut k = Kernel::create(&t.log, KernelConfig::new(&t.dir), Box::new(model)).unwrap();
    k.submit("say hi").unwrap();

    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].len(), 2, "system prompt plus the user message");
    assert!(!seen[0].iter().any(|m| matches!(
        m,
        ChatMessage::System { text } if text.contains("instructions from")
    )));
}

/// Purpose: The context view reports loaded instructions under the rules category.
#[test]
fn context_explain_reports_instructions_as_rules() {
    let t = Temp::new();
    let mut config = KernelConfig::new(&t.dir);
    config.instructions = vec![InstructionBlock {
        scope: InstructionScope::User,
        source: "/home/person/.loongagent/AGENTS.md".to_string(),
        text: "PREFER-SMALL-DIFFS".to_string(),
    }];
    let k = Kernel::create(
        &t.log,
        config,
        Box::new(ScriptedModel::saying("hi")),
    )
    .unwrap();

    let explain = k.context_explain();
    let entry = explain
        .entries
        .iter()
        .find(|entry| entry.source == "/home/person/.loongagent/AGENTS.md")
        .expect("the instruction is in the context view");
    assert_eq!(entry.category, "rules");
}
```

- [ ] **Step 3: Run the tests and confirm they fail to compile**

Run: `cargo test -p loong-kernel turn::tests::instructions_are_seated_after_the_system_prompt`
Expected: FAIL — `InstructionBlock` and `InstructionScope` are not defined, `KernelConfig` has no `instructions` field.

- [ ] **Step 4: Add the types and the config field**

In `crates/kernel/src/turn.rs`, above `KernelConfig`, add the two types with the repository's doc blocks. Add `pub instructions: Vec<InstructionBlock>` to `KernelConfig` and `instructions: Vec::new()` to `KernelConfig::new`. Add `mod instructions;` beside the other `turn` submodules.

In `crates/kernel/src/lib.rs`, add `InstructionBlock` and `InstructionScope` to the `pub use turn::{...}` list, so the CLI can name them as `loong_kernel::InstructionBlock`.

The field doc must say: each block is one file, already read and truncated by the caller; empty is the rollback path; the kernel never touches the filesystem for instructions.

- [ ] **Step 5: Implement `Kernel::seat_instructions`**

Create `crates/kernel/src/turn/instructions.rs`:

```rust
//! Purpose: Seat caller-supplied instruction files as pinned system blocks.
//!
//! # Contract
//! - **Serves:** a session whose model reads `AGENTS.md` before the first turn.
//! - **Design:** `docs/design/instructions.md` §3; `docs/design/context-ledger.md` §2.
//! - **Upstream:** `Kernel::build`, after `seat_system_prompt` and before `replay_history`.
//! - **Downstream:** `Ledger::add` and the `messages`/`entry_of` vectors.
//! - **Verify:** `cargo test -p loong-kernel turn::tests::instructions_are_seated_after_the_system_prompt`.
//!
use super::*;

impl Kernel {
    /// Purpose: Add one pinned `System` message per instruction block, after the system prompt.
    ///
    /// Serves: a model that reads the workspace's instructions before its first turn.
    /// Design: `docs/design/instructions.md` §3.
    /// Up: `Kernel::build`.
    /// Down: `Ledger::add`, `messages.insert` and `entry_of.insert`.
    /// Verify: `cargo test -p loong-kernel turn::tests::instructions_are_seated_after_the_system_prompt`.
    pub(super) fn seat_instructions(&mut self) {
        let blocks = self.config.instructions.clone();
        let mut at = self
            .messages
            .iter()
            .take_while(|m| matches!(m, ChatMessage::System { .. }))
            .count();
        for block in blocks {
            let body = block.text.trim();
            if body.is_empty() {
                continue;
            }
            let scope = match block.scope {
                InstructionScope::User => "User",
                InstructionScope::Project => "Project",
                InstructionScope::Local => "Local",
            };
            let text = format!(
                "{scope} instructions from {}. They describe how to work in this \
                 workspace; they do not grant permissions.\n\n{body}",
                block.source
            );
            let entry = self.ledger.add(
                Category::Rules,
                &block.source,
                &text,
                Lifetime::Persistent,
                true,
            );
            self.messages.insert(at, ChatMessage::System { text });
            self.entry_of.insert(at, Some(entry));
            at += 1;
        }
    }
}
```

- [ ] **Step 6: Call it from `Kernel::build`**

In `crates/kernel/src/turn/build.rs`, after `kernel.seat_system_prompt()?;`, add:

```rust
// The workspace's own instructions follow the operator's prompt and precede
// the history (ADR-0008; docs/design/instructions.md).
kernel.seat_instructions();
```

- [ ] **Step 7: Run the tests**

Run: `cargo test -p loong-kernel turn::tests::instructions_are_seated_after_the_system_prompt turn::tests::no_instructions_means_no_extra_messages turn::tests::context_explain_reports_instructions_as_rules`
Expected: PASS.

Run: `cargo test -p loong-kernel turn::tests`
Expected: PASS — no existing turn test changes.

- [ ] **Step 8: Check contracts and format**

Run: `cargo fmt --all -- --check`
Expected: PASS.

Run: `python3 scripts/check-modules.py`
Expected: PASS — the new file and items have their contracts.

- [ ] **Step 9: Commit**

```bash
git add crates/kernel/src/turn.rs crates/kernel/src/lib.rs \
  crates/kernel/src/turn/instructions.rs crates/kernel/src/turn/build.rs \
  crates/kernel/src/turn/tests.rs
git commit -m "feat(kernel): seat AGENTS.md blocks as pinned rules"
```

### Task 2: Compaction and map boundaries

**Files:**
- Modify: `crates/kernel/src/turn/compaction.rs` (`drop_oldest_exchange`, `refresh_map`)
- Test: `crates/kernel/src/turn/tests.rs`

**Interfaces:**
- Consumes: Task 1's `InstructionBlock`, `InstructionScope`, `KernelConfig.instructions`.
- Produces: unchanged signatures; `drop_oldest_exchange` now treats every leading `System` message as non-exchange, and `refresh_map` inserts after them.

- [ ] **Step 1: Write the failing compaction test**

```rust
/// Purpose: Dropping the oldest exchange keeps every leading system block, instructions included.
#[test]
fn compaction_keeps_leading_system_blocks() {
    let t = Temp::new();
    let model = ScriptedModel::new(vec![
        response(vec![], Some("one")),
        response(vec![], Some("two")),
    ]);
    let mut config = KernelConfig::new(&t.dir);
    config.instructions = vec![InstructionBlock {
        scope: InstructionScope::Project,
        source: "AGENTS.md".to_string(),
        text: "RULES-SURVIVE-COMPACTION".to_string(),
    }];
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    k.submit("first").unwrap();
    k.submit("second").unwrap();

    let dropped = k.drop_oldest_exchange();
    assert_eq!(dropped, 2, "the first user turn and its answer");
    let leading_systems = k
        .messages()
        .iter()
        .take_while(|m| matches!(m, ChatMessage::System { .. }))
        .count();
    assert_eq!(leading_systems, 2, "the prompt and the instruction block");
    assert!(matches!(
        k.messages().get(2),
        Some(ChatMessage::User { text }) if text == "second"
    ));
    assert!(k
        .ledger()
        .explain()
        .iter()
        .any(|row| row.category == "rules" && row.source == "AGENTS.md"));
}
```

- [ ] **Step 2: Write the failing map-order test**

```rust
/// Purpose: The cognitive map follows the instruction blocks instead of sitting between the prompt and them.
#[test]
fn the_map_follows_instruction_blocks() {
    let t = Temp::new();
    let (model, seen) = RecordingModel::new(vec![
        response(
            vec![call(
                "c1",
                "edit_file",
                json!({"path": "src/kernel/a.rs", "new_string": "x"}),
            )],
            None,
        ),
        response(vec![], Some("done")),
        response(vec![], Some("second turn")),
    ]);
    let mut config = KernelConfig::new(&t.dir);
    config.instructions = vec![InstructionBlock {
        scope: InstructionScope::Project,
        source: "AGENTS.md".to_string(),
        text: "RULES-BEFORE-MAP".to_string(),
    }];
    config.context_engine = Some(crate::context_engine::ContextEngineConfig::new(
        t.dir.join("session.context.db"),
        t.log.clone(),
    ));
    let mut k = Kernel::create(&t.log, config, Box::new(model)).unwrap();
    k.submit("修一下 src/kernel/a.rs").unwrap();
    k.submit("继续").unwrap();

    let seen = seen.lock().unwrap();
    let last = seen.last().expect("a request happened");
    let positions: Vec<usize> = last
        .iter()
        .enumerate()
        .filter(|(_, m)| matches!(m, ChatMessage::System { .. }))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(positions, vec![0, 1, 2], "prompt, instructions, map");
    assert!(matches!(
        &last[1],
        ChatMessage::System { text } if text.contains("RULES-BEFORE-MAP")
    ));
    assert!(matches!(
        &last[2],
        ChatMessage::System { text } if text.starts_with("L4")
    ));
}
```

- [ ] **Step 3: Run both tests and confirm they fail**

Run: `cargo test -p loong-kernel turn::tests::compaction_keeps_leading_system_blocks turn::tests::the_map_follows_instruction_blocks`
Expected: FAIL — the compaction test drops the instruction block (or returns a different count), and the map test finds the map at index 1.

- [ ] **Step 4: Fix `drop_oldest_exchange`**

Replace the `start` calculation in `crates/kernel/src/turn/compaction.rs` with:

```rust
// Every leading System message is a pinned block, not an exchange: the
// operator's prompt, the workspace's instructions, and the cognitive map.
let start = self
    .messages
    .iter()
    .take_while(|m| matches!(m, ChatMessage::System { .. }))
    .count();
```

Keep the rest of the function: `end` is the next `User` after `start`; no user means nothing is dropped. Update the comment to name the instruction blocks.

- [ ] **Step 5: Fix `refresh_map`**

Replace the `let at = usize::from(matches!(...))` block with:

```rust
// After all leading system blocks (prompt, instructions), before the conversation.
let at = self
    .messages
    .iter()
    .take_while(|m| matches!(m, ChatMessage::System { .. }))
    .count();
```

- [ ] **Step 6: Run the two tests, then the whole kernel suite**

Run: `cargo test -p loong-kernel turn::tests::compaction_keeps_leading_system_blocks turn::tests::the_map_follows_instruction_blocks`
Expected: PASS.

Run: `cargo test -p loong-kernel`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/kernel/src/turn/compaction.rs crates/kernel/src/turn/tests.rs
git commit -m "fix(kernel): keep leading system blocks out of compaction"
```

### Task 3: CLI loader

**Files:**
- Create: `crates/cli/src/instructions.rs`
- Test: `crates/cli/src/instructions.rs` (inline `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `loong_kernel::{InstructionBlock, InstructionScope}`; `loong_config::loongagent_home` is called by Task 4, not here.
- Produces: `pub(crate) struct LoadedInstructions { pub blocks: Vec<InstructionBlock>, pub warnings: Vec<String>, pub summary: String }`; `pub(crate) fn load(workspace: &Path, home: &Path) -> LoadedInstructions`; `pub(crate) const MAX_FILE_BYTES: usize = 32 * 1024`; `pub(crate) const MAX_TOTAL_BYTES: usize = 64 * 1024`.

- [ ] **Step 1: Write the failing order test and the tree helper**

Add the module file with its module contract and a test module. Start with the helper and one test:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use loong_kernel::InstructionScope;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    struct Tree {
        root: PathBuf,
    }

    impl Tree {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "loong-instructions-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir_all(&root).unwrap();
            Tree { root }
        }

        fn path(&self, rel: &str) -> PathBuf {
            self.root.join(rel)
        }

        fn write(&self, rel: &str, text: &str) {
            let path = self.path(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }

        fn mkdir(&self, rel: &str) {
            std::fs::create_dir_all(self.path(rel)).unwrap();
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// Purpose: User, project and local files load least-specific first.
    #[test]
    fn user_project_and_local_load_in_order() {
        let tree = Tree::new();
        tree.write("home/AGENTS.md", "USER");
        tree.mkdir("repo/.git");
        tree.write("repo/AGENTS.md", "ROOT");
        tree.write("repo/sub/AGENTS.md", "SUB");
        tree.write("repo/sub/AGENTS.local.md", "LOCAL");

        let loaded = load(&tree.path("repo/sub"), &tree.path("home"));
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        let seen: Vec<(InstructionScope, &str)> = loaded
            .blocks
            .iter()
            .map(|block| (block.scope, block.text.as_str()))
            .collect();
        assert_eq!(
            seen,
            vec![
                (InstructionScope::User, "USER"),
                (InstructionScope::Project, "ROOT"),
                (InstructionScope::Project, "SUB"),
                (InstructionScope::Local, "LOCAL"),
            ]
        );
    }
}
```

- [ ] **Step 2: Run it and confirm it fails to compile**

Run: `cargo test -p loongagent instructions::tests::user_project_and_local_load_in_order`
Expected: FAIL — `load`, `LoadedInstructions` and the constants are not defined.

- [ ] **Step 3: Implement the loader**

In `crates/cli/src/instructions.rs`, implement:

```rust
pub(crate) fn load(workspace: &Path, home: &Path) -> LoadedInstructions
```

- Candidate order: `(User, home.join("AGENTS.md"))`; every directory from the git root down to the canonical workspace, each with `AGENTS.md`; then `(Local, workspace.join("AGENTS.local.md"))`.
- `project_dirs`: canonicalize `workspace`; find the nearest ancestor whose `.git` exists; if none, return only the workspace; otherwise collect from that root down to the workspace and reverse.
- Deduplicate by `std::fs::canonicalize(&path).unwrap_or(path.clone())`.
- Resolve each project/local candidate to its canonical path; if it leaves its boundary (the git root for project files, the workspace for local), warn and skip. The user file is exempt.
- Before opening, require `metadata.is_file()`; a directory, FIFO, socket or device is a warning, so a FIFO cannot block the session.
- Read at most `min(MAX_FILE_BYTES, remaining_total) + 4` bytes with `File::open` and `Read::take`. The extra four bytes are the maximum UTF-8 character width, so a character that starts before the limit is still complete when the bytes are decoded. `ErrorKind::NotFound` is silent; every other error is a warning. `String::from_utf8` failure is a warning.
- If the bytes exceed the limit, truncate at the largest `char` boundary and append `\n\n[loongagent: truncated at {limit} bytes]`. If the remaining total is 0, warn and skip.
- Skip a body that is empty after `trim`.
- `summary` is `format!("{} file{}, {} bytes", n, if n == 1 { "" } else { "s" }, total_bytes)`.
- Warning strings name the path and the reason; Task 4 prefixes them with `[instructions]`.

- [ ] **Step 4: Add the remaining loader tests**

Add these tests with the assertions named:

- `a_file_above_the_git_root_is_not_read`: `OUTSIDE` at the tree root, `repo/.git`, `repo/AGENTS.md` = `ROOT`; load `repo`; assert exactly one block with `ROOT`.
- `no_git_reads_only_the_working_directory`: no `.git`; `AGENTS.md` at the tree root = `OUTSIDE` and `work/AGENTS.md` = `INSIDE`; load `work`; assert only `INSIDE`.
- `git_file_marks_the_root`: write `repo/.git` as a **file**; `repo/AGENTS.md` = `ROOT`, `repo/sub/AGENTS.md` = `SUB`; load `repo/sub`; assert both, root first.
- `duplicate_path_is_read_once`: pass `home = repo` with `repo/AGENTS.md` = `ONCE`; assert one block.
- `truncates_at_the_file_cap`: write a user file of `"a".repeat(MAX_FILE_BYTES - 2)` + `"😀"` + `"tail"`; assert one block, it ends with `[loongagent: truncated at 32768 bytes]`, and it does **not** contain `\u{FFFD}` (the four-byte character would be split by a naive byte cut, so keeping it out proves the boundary is respected).
- `skips_a_non_utf8_file`: write `[0xff, 0xfe]` to the user path; assert no blocks and one warning naming `AGENTS.md`.
- `skips_an_empty_file`: write `"   \n"`; assert no blocks.
- `stops_at_the_total_cap`: user and root files are each `MAX_FILE_BYTES` long; `repo/sub/AGENTS.md` is one byte; load `repo/sub`; assert two blocks and a warning naming `sub/AGENTS.md`.
- `a_directory_named_agents_md_warns`: `mkdir("home/AGENTS.md")`; assert no blocks and a warning.
- `a_project_symlink_outside_the_git_root_is_not_read` (`#[cfg(unix)]`): a `repo/AGENTS.md` symlink to `outside/secret.md`; assert no blocks and a `points outside` warning.
- `a_fifo_is_not_opened` (`#[cfg(unix)]`): `mkfifo home/AGENTS.md`; assert no blocks and one warning, proving metadata is checked before `open`.

- [ ] **Step 5: Run the loader tests**

Run: `cargo test -p loongagent instructions::tests`
Expected: PASS.

- [ ] **Step 6: Format and check contracts**

Run: `cargo fmt --all -- --check`
Expected: PASS.

Run: `python3 scripts/check-modules.py`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/cli/src/instructions.rs
git commit -m "feat(cli): discover and load AGENTS.md files"
```

### Task 4: Wire the loader into `build_kernel`

**Files:**
- Modify: `crates/cli/src/main.rs` (`mod instructions;`, module list)
- Modify: `crates/cli/src/server.rs` (`build_kernel`)
- Test: `crates/cli/src/server.rs` (new test module)

**Interfaces:**
- Consumes: Task 3's `load` and `LoadedInstructions`; Task 1's `KernelConfig.instructions`.
- Produces: every production session built by `build_kernel` gets the discovered blocks; stderr gets `[instructions] <warning>` and `[instructions] <summary>` when files load.

- [ ] **Step 1: Write the failing wiring test**

Add to `crates/cli/src/server.rs`:

```rust
#[cfg(test)]
mod instruction_wiring_tests {
    use super::build_kernel;
    use crate::Args;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    /// Purpose: A real build reads AGENTS.md and seats it as a rules entry, not just an empty config field.
    #[test]
    fn build_kernel_seats_agents_md_as_a_rule() {
        let dir = std::env::temp_dir().join(format!(
            "loong-cli-instructions-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("AGENTS.md"), "WIRING-MARKER").unwrap();
        let session = dir.join("session.jsonl");
        let argv: Vec<String> = vec![
            "run".into(),
            "hi".into(),
            "--cwd".into(),
            dir.display().to_string(),
            "--session".into(),
            session.display().to_string(),
            "--model".into(),
            "echo".into(),
            "--local".into(),
        ];
        let args = Args::parse(&argv).unwrap();
        let settings = loong_config::Profile::default();
        let kernel = build_kernel(&args, &settings, &session).unwrap();

        let explain = kernel.context_explain();
        assert!(
            explain
                .entries
                .iter()
                .any(|entry| entry.category == "rules"
                    && entry.source.ends_with("AGENTS.md")),
            "{:?}",
            explain.entries
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p loongagent instruction_wiring_tests`
Expected: FAIL — no `rules` entry, because `build_kernel` never loads the file.

- [ ] **Step 3: Add the module declaration**

In `crates/cli/src/main.rs`, add `mod instructions;` beside `mod hooks;` and `mod local;`. Extend the file contract's downstream list to name `instructions`.

- [ ] **Step 4: Load and assign in `build_kernel`**

In `crates/cli/src/server.rs`, after `let mut config = kernel_config(args, settings, path);`, add:

```rust
// The workspace's and the person's own instructions (ADR-0008;
// docs/design/instructions.md). Read here, once per session, so the kernel
// never depends on the filesystem for a test.
let loaded = crate::instructions::load(&args.cwd, &loong_config::loongagent_home());
for warning in &loaded.warnings {
    eprintln!("[instructions] {warning}");
}
if !loaded.blocks.is_empty() {
    eprintln!("[instructions] {}", loaded.summary);
}
config.instructions = loaded.blocks;
```

- [ ] **Step 5: Run the wiring test and the whole CLI suite**

Run: `cargo test -p loongagent instruction_wiring_tests`
Expected: PASS.

Run: `cargo test -p loongagent`
Expected: PASS.

- [ ] **Step 6: Manual smoke check**

```bash
tmp=$(mktemp -d)
printf '# AGENTS.md\n\nSMOKE-MARKER\n' > "$tmp/AGENTS.md"
LOONGAGENT_HOME="$tmp/home" ./target/debug/loongagent run "hi" \
  --cwd "$tmp" --session "$tmp/session.jsonl" --model echo --local
```

Expected: stderr contains `[instructions] 1 file,` (or `1 files`) and the run completes. Remove `$tmp` afterwards.

- [ ] **Step 7: Commit**

```bash
git add crates/cli/src/main.rs crates/cli/src/server.rs
git commit -m "feat(cli): load AGENTS.md for every session"
```

### Task 5: Documents, ignore rule and final gate

**Files:**
- Create: `docs/design/instructions.md`
- Modify: `docs/design/README.md`
- Modify: `docs/design/context-ledger.md`
- Modify: `docs/design/kernel-loop.md`
- Modify: `docs/05-parity-and-synthesis.md`
- Modify: `.gitignore`
- Modify: `README.md` (test-count line)

**Interfaces:**
- Consumes: everything from Tasks 1–4.
- Produces: a maintained design document that owns the mechanism, an accurate parity table, and a green gate.

- [ ] **Step 1: Write `docs/design/instructions.md`**

Use this header exactly, then explain the mechanism in at most 250 lines:

```markdown
# Instructions: AGENTS.md at session start

- **Up:** [design/README.md](README.md) · [04 System overview](../04-system-overview.md)
- **Authority:** [ADR-0008](../decisions/0008-configuration-layering.md)
- **Code:** `crates/cli/src/instructions.rs`, `crates/kernel/src/turn.rs`, `crates/kernel/src/turn/build.rs`, `crates/kernel/src/turn/instructions.rs`, `crates/kernel/src/turn/compaction.rs`
- **Verify:** `cargo test -p loongagent instructions` · `cargo test -p loong-kernel` · `./scripts/check.sh`
```

Sections: what loads and in what order; the git-root boundary; the `InstructionBlock` value and why the kernel does not read files; seating as pinned `Rules` (one message per file, advisory header); 32/64 KiB caps and failure behaviour; session-start/resume lifetime; non-goals (`CLAUDE.md`, `@path`, subdirectory lazy loading, managed/org, live reload).

- [ ] **Step 2: Add the subsystem row**

In `docs/design/README.md`'s table, add:

```markdown
| [instructions.md](instructions.md) | Session-start instruction files: which `AGENTS.md` files load, in what order, and how they enter the ledger as pinned rules. | `crates/cli/src/instructions.rs`, `crates/kernel/src/turn/instructions.rs` |
```

- [ ] **Step 3: Update the ledger document**

In `docs/design/context-ledger.md`:

- §1's proposal list gains `P4["AGENTS.md instruction blocks<br/>seat_instructions"]` and an edge to the ledger.
- The sentence counting the writers changes from "those five functions" to "those six functions" and names `seat_instructions`.
- §2's table row becomes `| Rules | 40 | seat_instructions | AGENTS.md, AGENTS.local.md |`.

- [ ] **Step 4: Update the kernel-loop document**

In `docs/design/kernel-loop.md`:

- §1's conversation row: after `seat_system_prompt`, name `seat_instructions`, then `replay_history`.
- §3's ledger append row: add `seat_instructions` to the functions that keep the ledger and `messages` in lockstep.
- §4/compaction: state that leading system blocks (prompt, instructions, map) are never exchanges.

- [ ] **Step 5: Correct the parity claims**

In `docs/05-parity-and-synthesis.md` §A3, replace the two rows with:

```markdown
| Instruction hierarchy: managed → user → project → local, most-specific last | ⚠️ | ADR-0008; user/project/local implemented in [instructions.md](design/instructions.md) | managed/org layer |
| AGENTS.md native (user → project → local) | ✅ | ADR-0008; [instructions.md](design/instructions.md) | — |
| CLAUDE.md compatibility | ❌ | ADR-0008 requires an explicit one-time import | importer |
```

- [ ] **Step 6: Ignore the local file**

Add to `.gitignore`, in the loongagent block:

```gitignore
AGENTS.local.md
```

- [ ] **Step 7: Run the tests and update the count**

Run: `cargo test --workspace --locked 2>&1 | tee /tmp/loong-tests.txt`
Then set the `<!-- test-counts: N passed, M ignored -->` line in `README.md` to the totals the run reported.

- [ ] **Step 8: Run the document and contract gates**

Run: `python3 scripts/check-docs.py`
Expected: PASS — links, headers, lengths, code paths and reachability.

Run: `python3 scripts/check-modules.py`
Expected: PASS.

Run: `cargo fmt --all -- --check`
Expected: PASS.

- [ ] **Step 9: Run the full gate**

Run: `./scripts/check.sh`
Expected: all gates pass, including the release build.

- [ ] **Step 10: Commit**

```bash
git add docs/design/instructions.md docs/design/README.md \
  docs/design/context-ledger.md docs/design/kernel-loop.md \
  docs/05-parity-and-synthesis.md .gitignore README.md
git commit -m "docs: describe AGENTS.md loading and correct the parity claims"
```
