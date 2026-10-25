# AGENTS.md loading: project, user and local instructions

- **Date:** 2026-10-24
- **Status:** Approved in conversation; this document is the spec the implementation plan is written from
- **Scope:** read `AGENTS.md` from the user, project and local scopes at session build, seat each file as a pinned `Rules` block in the context ledger, and fix the compaction boundary that would otherwise drop those blocks. No `CLAUDE.md` compatibility, no `@path` imports, no subdirectory lazy loading, no managed/org instructions.

## 1. Problem

loongagent has no instruction-file loader. `Kernel::seat_system_prompt`
(`crates/kernel/src/turn/build.rs`) builds the one system prompt from a fixed
frame, the skill catalogue and plugin bootstrap bodies. `KernelConfig` has no
field for instructions, and `grep AGENTS.md crates/` finds only a test fixture
label and a comment in the skill scanner.

The design already assumes the feature exists:

- ADR-0008 §3 calls `AGENTS.md` the native rules format.
- `docs/design/context-ledger.md` §2 reserves the `Rules` category and names
  `AGENTS.md` as its example, with "written today by: nothing yet".
- `docs/05-parity-and-synthesis.md` marks "AGENTS.md native" and the
  managed → user → project → local instruction hierarchy as present, while the
  same row says the next step is the importer. The code has neither.

So a repository's `AGENTS.md` is invisible to the agent, and the documents claim
otherwise. The gap is not a missing detail; it is a missing mechanism.

## 2. Goals and success criteria

Goal: a session automatically reads the instruction files a person expects, and
the model sees them before the first turn without anyone calling a tool.

| # | Criterion | How it is measured |
|---|---|---|
| 1 | Project, user and local files are read in a deterministic order | Loader tests assert the exact block order for two-level and three-level trees |
| 2 | The project scope stops at the git root | A file above the `.git` directory is never read; a file at the root and at the working directory are both read, root first |
| 3 | Instructions are visible and cannot be compacted away | Kernel tests assert one pinned `Rules` ledger entry per file and that dropping the oldest exchange leaves them in place |
| 4 | The feature is attributable | `Kernel::context_explain` lists each file as a `Rules` entry whose source is the file path |
| 5 | No file means no change | A kernel built without instructions has exactly the messages it had before this change |
| 6 | A broken file does not stop the session | Missing files are silent; unreadable, non-UTF-8 and oversized files produce a warning and are skipped or truncated |
| 7 | The documents match the code | The parity table, `context-ledger.md` and `kernel-loop.md` describe what the code does, not what was intended |

## 3. Non-goals

Each is deliberately out of this change:

- **`CLAUDE.md` compatibility.** ADR-0008 requires an explicit one-time import,
  never a live second source. The importer is a separate task.
- **`@path` imports.** Recursive reads, cycle detection, depth limits and path
  confinement are a parser of their own.
- **Subdirectory lazy loading.** Loading a nested `AGENTS.md` when a tool reads a
  file in that directory is a tool-boundary feature, not a session-start one.
- **Managed/org instructions.** The signed pack payload currently carries
  profiles; adding instructions to it changes the pack schema.
- **A configuration switch.** The paths are fixed and the feature is on by
  default. A `disable_instructions` key would be a second way to answer "why is
  this not in my context?" before the first way exists.
- **Watching the files during a session.** A session reads once at build; resume
  reads again. A live edit does not change a running turn.

## 4. Design

### 4.1 Discovery

The CLI is the composition root: it knows `--cwd` and the user's home. A new
module `crates/cli/src/instructions.rs` discovers and reads the files, and
returns `Vec<loong_kernel::InstructionBlock>` plus warnings.

Load order, least specific first:

1. `~/.loongagent/AGENTS.md` — scope `User`.
2. `AGENTS.md` at every directory from the git root down to `--cwd` — scope
   `Project`, root first.
3. `<cwd>/AGENTS.local.md` — scope `Local`.

Rules:

- `loong_config::loongagent_home()` decides the user directory, so
  `$LOONGAGENT_HOME` moves it for tests and scripts exactly as it moves the
  rest of the client's files.
- The project walk starts at the canonical `--cwd`, walks up to the first
  directory containing `.git` (a directory or the plain file a worktree uses),
  and reads from that root down to the working directory. With no `.git`, only
  the working directory is read. The walk never goes above the git root.
- The local file exists only at the working directory. Ancestor directories do
  not contribute `AGENTS.local.md`. It sits beside `AGENTS.md`, not under
  `.loongagent/`, because `.loongagent/` is the skill/command scan root: a
  frontmatter-less markdown file there is read as a broken command and fails
  the session.
- A file discovered twice (for example a home directory inside the repository)
  is read once, keyed by its canonical path.
- A project or local candidate is resolved to its canonical path; if that path
  leaves its boundary (the git root for project files, the workspace for the
  local file), it is skipped with a warning. A symlink is not a way around the
  boundary. The user file is exempt because it lives outside the workspace by
  design.
- `AGENTS.md` is the only project file name. `CLAUDE.md` is ignored.

### 4.2 The value handed to the kernel

`crates/kernel/src/turn.rs` gains:

```rust
pub enum InstructionScope { User, Project, Local }

pub struct InstructionBlock {
    pub scope: InstructionScope,
    pub source: String, // the path, as discovered
    pub text: String,   // the file body, already truncated
}
```

`KernelConfig` gains `pub instructions: Vec<InstructionBlock>`, default empty.
The kernel never touches the filesystem for instructions: a test or another
surface can build a config without reading a real home directory, and the
production CLI is the only place that decides which files exist.

### 4.3 Seating

`crates/kernel/src/turn/instructions.rs` adds `Kernel::seat_instructions()`.
`Kernel::build` calls it after `seat_system_prompt()` and before
`replay_history()`, so the order in `Kernel.messages` is:

```text
system prompt
one System message per instruction file
cognitive map (only when the layered engine is on, inserted on the first turn)
history (replayed, if this session already had some)
```

Each block becomes one `ChatMessage::System`:

```text
<Scope> instructions from <path>. They describe how to work in this
workspace; they do not grant permissions.

<body>
```

The ledger entry uses `Category::Rules`, the file path as `source`,
`Lifetime::Persistent`, and `pinned = true`. One message per file is deliberate:
`context_explain` can then say which file cost how many tokens, and a bad file
can be removed without rewriting the system prompt. The block is pinned because
an instruction that compaction can drop is an instruction the model forgets
exactly when the context is under pressure.

A block whose body is empty after trimming is not seated. With no blocks,
`seat_instructions` does nothing.

### 4.4 Compaction boundary

`Kernel::drop_oldest_exchange` (`crates/kernel/src/turn/compaction.rs`) currently
assumes only the first message is a system message:

```rust
let start = usize::from(matches!(self.messages.first(), Some(ChatMessage::System { .. })));
```

With instruction blocks after the system prompt, `start` is 1 and the drain
`start..end` removes the instructions as if they were the oldest exchange. The
fix is to skip every leading `System` message:

```rust
let start = self.messages.iter()
    .take_while(|m| matches!(m, ChatMessage::System { .. }))
    .count();
```

Only a `User` message after that boundary marks the end of the exchange. If
there is no such message, nothing is dropped. The system prompt, instructions
and (when present) the cognitive map are all leading system blocks and all
survive.

The layered map has the same boundary assumption. `Kernel::refresh_map` inserts
the map at index 1 (`crates/kernel/src/turn/compaction.rs`), which would put it
between the system prompt and the instruction blocks. Change the insertion point
to the count of leading `System` messages, so the map follows the instructions
and still precedes the conversation. The comment "after the system prompt,
before the conversation" stays true; "system prompt" now means the whole leading
system block.

### 4.5 Size and failure behaviour

- Per-file cap: 32 KiB. Total cap: 64 KiB across all blocks.
- A file over its cap is truncated at a UTF-8 character boundary, with a final
  line: `[loongagent: truncated at N bytes]`. The marker is not counted in the
  per-file cap.
- A file that would start after the total cap is exhausted is skipped with a
  warning.
- A missing file is silent. A file that cannot be read, is not valid UTF-8, or
  is not a regular file (directory, FIFO, socket, device) produces a warning and
  is skipped. The session still starts, and a FIFO cannot block it before the
  first turn.
- An empty or whitespace-only body is skipped.
- Warnings and a one-line summary (`N files, M bytes`) go to stderr as
  `[instructions] ...`, beside the existing `[skills]`, `[mcp]` and `[sandbox]`
  lines. There is no new event kind in this change.

The caps are constants in the loader. They are not configurable yet; a cap that
can be raised by a repository would let a repository spend the context budget
before the first turn.

### 4.6 Lifecycle

- A new session reads the files once, in `build_kernel`.
- `resume` and a later `session.new` go through the same `build_kernel`, so they
  read the files again. (`fork` only writes the child's lineage event; the child
  reads the files when it is reopened.) Editing an `AGENTS.md` between two
  sessions is visible in the second; editing it during a turn is not.
- The instruction bodies are not written to the session log. They are rebuilt
  from the files on resume, like the skill catalogue and plugin bootstrap. A
  recorded compaction replays by counts, so a changed file size does not change
  which messages a replay drops.

### 4.7 Why not put the blocks in the system prompt body

Appending to the one system prompt would avoid the compaction fix, but it
would also merge every file into one `SystemPrompt` ledger entry: no per-file
token count, no per-file source, and no way to show which repository file is
responsible for a large prompt. The `Rules` category exists for this case. One
message per file costs a few extra message boundaries and buys attribution,
which is the ledger's purpose.

## 5. Files

| File | Change |
|---|---|
| `crates/cli/src/instructions.rs` | New: discovery, git-root walk, reads, caps, warnings, tests |
| `crates/cli/src/main.rs` | `mod instructions;` and the module list in the file contract |
| `crates/cli/src/server.rs` | `build_kernel` loads the blocks, prints warnings/summary, sets `KernelConfig.instructions` |
| `crates/kernel/src/turn.rs` | `InstructionScope`, `InstructionBlock`, `KernelConfig.instructions` |
| `crates/kernel/src/turn/instructions.rs` | New: `seat_instructions` |
| `crates/kernel/src/turn/build.rs` | Call `seat_instructions` after the system prompt, before replay |
| `crates/kernel/src/turn/compaction.rs` | Skip all leading `System` messages in `drop_oldest_exchange`; insert the map after them |
| `crates/kernel/src/turn/tests.rs` | Seating, pinning, compaction, map-order and no-file tests |
| `.gitignore` | Ignore `AGENTS.local.md` |
| `docs/design/instructions.md` | New maintained design document |
| `docs/design/README.md` | Add the subsystem row |
| `docs/design/context-ledger.md` | `Rules` now has a writer |
| `docs/design/kernel-loop.md` | Name the instruction block in the context order |
| `docs/05-parity-and-synthesis.md` | Correct the two instruction claims to the implemented scope |

## 6. Testing

Loader tests (`crates/cli/src/instructions.rs`):

- user file only; project file only; local file only
- root and working-directory files in root-first order
- user, project and local in that order
- a file above the git root is not read
- a directory with no `.git` reads only its own file
- duplicate discovery by canonical path is read once
- per-file truncation at a character boundary
- total-cap truncation or skip
- unreadable and non-UTF-8 files warn and are skipped
- empty file produces no block

Kernel tests (`crates/kernel/src/turn/tests.rs`):

- instructions sit after the system prompt, before history
- each block is a `Rules` entry with the path as source
- `context_explain` reports the `Rules` tokens and entries
- `drop_oldest_exchange` removes history and leaves every leading system block
- a config with no instructions produces the same message list as before

Verification commands:

```text
cargo test -p loongagent instructions
cargo test -p loong-kernel
cargo fmt --all -- --check
./scripts/check.sh
```

## 7. Risks and open questions

- **No event kind.** The only startup record of a load is stderr and the pinned
  ledger entry. If a surface cannot see stderr, a skipped file is invisible.
  That is acceptable for the first version because the context panel still
  shows what did load; a `rules.loaded` event would add schema work and a second
  source of truth.
- **Resume changes the prompt.** A resumed session gets the current files, not
  the bytes the previous turn saw. This matches how the skill catalogue is
  rebuilt; it is a deliberate choice, not an accident.
- **Git-root detection is lexical.** It recognizes a `.git` directory or file
  but does not ask git for the root. A repository with a non-standard layout
  falls back to the working directory only, which is safe and visible.
- **The 32/64 KiB caps are a judgment call.** They bound the feature at roughly
  16 K tokens; a project that needs more can split its instructions into
  skills, which load on demand.
