# Instructions: AGENTS.md at session start

- **Up:** [design/README.md](README.md) · [04 System overview](../04-system-overview.md)
- **Authority:** [ADR-0008](../decisions/0008-configuration-layering.md) · [ADR-0006](../decisions/0006-context-engine.md)
- **Code:** `crates/cli/src/instructions.rs`, `crates/kernel/src/turn.rs`, `crates/kernel/src/turn/build.rs`, `crates/kernel/src/turn/instructions.rs`, `crates/kernel/src/turn/compaction.rs`
- **Verify:** `cargo test -p loongagent instructions` · `cargo test -p loong-kernel` · `./scripts/check.sh`

A session reads three scopes of instruction files before its first turn. The
CLI finds and reads them; the kernel seats each one as a pinned `Rules` block.
The split is deliberate: the kernel never touches the filesystem for
instructions, so a test or another surface can build a session without reading
a real home directory.

## 1. What loads, and in what order

Least specific first, so a later file can correct an earlier one:

| Order | Path | Scope |
|---|---|---|
| 1 | `~/.loongagent/AGENTS.md` | `User` |
| 2 | `<git root>/AGENTS.md` … `<cwd>/AGENTS.md` | `Project`, root first |
| 3 | `<cwd>/AGENTS.local.md` | `Local` |

`loong_config::loongagent_home()` decides the user directory, so
`$LOONGAGENT_HOME` moves it for tests and scripts exactly as it moves the rest
of the client's files. In local mode the same user path applies; the file is
the person's own, not the firm's.

The project walk canonicalizes `--cwd`, finds the nearest ancestor containing
`.git` (a directory, or the plain file a worktree uses), and reads `AGENTS.md`
from that root down to the working directory. With no `.git`, only the working
directory is read. The walk never goes above the git root.

The local file exists only at the working directory; ancestors do not
contribute `AGENTS.local.md`. It sits beside `AGENTS.md`, not under
`.loongagent/`, because `.loongagent/` is also the skill/command scan root: a
markdown file there without frontmatter is read as a broken command and fails
the session. A file discovered twice, for example a home directory inside the
repository, is read once, keyed by its canonical path.

A project or local candidate is resolved to its canonical path. If that path
leaves its boundary — the git root for project files, the workspace for the
local file — it is skipped with a warning; a symlink is not a way to make the
agent read an outside file. The user file is exempt because it lives outside
the workspace by design.

`AGENTS.md` is the only project file name. `CLAUDE.md` is not read: ADR-0008
requires an explicit one-time import, never a live second source.

## 2. The value the kernel receives

The CLI reads each file into an `InstructionBlock`:

```rust
pub enum InstructionScope { User, Project, Local }

pub struct InstructionBlock {
    pub scope: InstructionScope,
    pub source: String, // the path, as discovered
    pub text: String,   // the body, already capped
}
```

`KernelConfig.instructions` carries the blocks in load order. Empty is the
default and the rollback path: a session with no blocks behaves exactly as it
did before the feature existed. The kernel does not know which paths exist or
how a repository is laid out; that policy lives in one CLI module.

## 3. How the blocks enter context

`Kernel::build` seats the system prompt, then calls `Kernel::seat_instructions`,
then replays history. Each non-empty block becomes one `ChatMessage::System`:

```text
<Scope> instructions from <path>. They describe how to work in this
workspace; they do not grant permissions.

<body>
```

The ledger entry uses `Category::Rules`, the path as its source,
`Lifetime::Persistent` and `pinned = true`. `context_explain` therefore says
which file costs how many tokens, and compaction cannot drop an instruction
exactly when the context is under pressure.

One message per file, rather than one concatenated system prompt, is what makes
that attribution possible. It also keeps the operator's prompt in one place:
a repository file cannot rewrite it, only follow it.

Instructions are **advisory text**. They cannot grant permissions, change the
PDP, widen a configuration cap or add a tool. The header says so, and the
kernel's enforcement paths do not read them.

## 4. Caps and failure behaviour

- 32 KiB per file; 64 KiB across all files.
- A file over its cap is truncated at a UTF-8 character boundary, with a final
  line `[loongagent: truncated at N bytes]`. The marker is not counted in the
  per-file cap.
- A file that would start after the total cap is exhausted is skipped with a
  warning.
- A missing file is silent. A file that cannot be read, is not valid UTF-8, or
  is not a regular file (directory, FIFO, socket, device) produces a warning and
  is skipped; the session still starts. A FIFO is rejected by metadata before
  `open`, so it cannot block the session before its first turn.
- An empty or whitespace-only body is skipped.
- Warnings and a one-line summary (`N files, M bytes`) go to stderr as
  `[instructions] ...`, beside `[skills]`, `[mcp]` and `[sandbox]`. There is no
  new event kind.

The caps are constants. A repository cannot raise them, because that would let
a repository spend the context budget before the first turn.

## 5. Lifecycle

A new session reads the files once, in `build_kernel`. `resume` and a later
`session.new` go through the same path, so they read the files again. Editing
an `AGENTS.md` between two sessions is visible in the second; editing it during
a turn is not.

The bodies are not written to the session log. They are rebuilt from the files
on resume, like the skill catalogue and plugin bootstrap. A recorded compaction
replays by counts, so a changed file size does not change which messages a
replay drops.

## 6. Non-goals

- `CLAUDE.md` compatibility; ADR-0008 requires an explicit one-time importer.
- `@path` imports, with their recursive reads, cycle detection and confinement.
- Subdirectory lazy loading when a tool touches a nested file.
- Managed/org instruction files carried by a signed pack.
- A configuration switch or a live file watcher.

Each is a separate mechanism. The first version answers the common case: a
repository and a person have instructions, and the model reads them.

## 7. Ordering with the other pinned blocks

The leading messages are all pinned: the operator's system prompt, the
instruction blocks, and, when the layered engine is on, the L4 cognitive map.
`Kernel::drop_oldest_exchange` skips every leading `System` message before it
looks for a user turn, and `Kernel::refresh_map` inserts the map after those
blocks. Compaction therefore removes history, never the prompt or its
instructions.

The tests that pin this are
`turn::tests::instructions_are_seated_after_the_system_prompt`,
`turn::tests::compaction_keeps_leading_system_blocks` and
`turn::tests::the_map_follows_instruction_blocks`. The loader tests in
`crates/cli/src/instructions.rs` pin the order, the git-root boundary, the caps
and the broken-file cases.
