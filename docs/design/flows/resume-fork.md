# Flow: resume and fork

- **Up:** [sessions-lineage.md](../sessions-lineage.md)
- **Authority:** [ADR-0003](../../decisions/0003-jsonl-truth-sqlite-index.md) · [ADR-0015](../../decisions/0015-two-tree-levels.md)
- **Code:** `crates/server/src/lib.rs`, `crates/kernel/src/turn/session.rs`, `crates/cli/src/session.rs`
- **Verify:** `cargo test -p loong-server -p loong-kernel` · `./scripts/check.sh`

A session **is** its JSONL file, so reopening is a disk read and forking is a new
file that references an old one. This page follows a person from the session list to
a working client, and then to a child session. The file format and the event tree are
owned by [events-storage.md](../events-storage.md); the lifecycle rules are
[sessions-lineage.md](../sessions-lineage.md).

## 1. List what exists

`loongagent sessions [--all] [--limit N] [--cwd DIR]` calls `list_sessions`:

- `session_summaries_in` reads each `*.jsonl` with `replay_lenient`, counts events
  and turns, takes the first user prompt and the fork reference (`parent@seq`), and
  returns newest first.
- `--all` uses `all_session_summaries`; otherwise `session_summaries` reads this
  project's directory (`project_slug`), so two checkouts do not share a list.
- `newest_session` marks the one a bare `loongagent tui` would continue.

## 2. Choose the file

`Args::finish_session` settles the choice once for every command:

- `--new` mints an id with `new_session_id` and creates `<id>.jsonl`.
- `--session ID|PATH` (and `resume`) goes through `resolve_session`: an exact id, an
  unambiguous prefix, `latest`, or a path. A prefix that fits two sessions is
  refused, and a session from another project goes through `match_session` rather
  than being guessed.
- Nothing named means the project's `newest_session`, or a fresh id when none exists.

`build_kernel` (`crates/cli/src/server.rs`) creates the directory, then calls
`Kernel::open_with_models` for an existing log (`SessionLog::open`) or
`Kernel::create_with_models` for a new one (`SessionLog::create`). `Kernel::build`
rebuilds the derived SQLite index from the log (`Index::rebuild_into`) and calls
`replay_history`.

## 3. Rebuild the context

`Kernel::replay_history` (`crates/kernel/src/turn/session.rs`) is the whole of reopen:

1. `Kernel::lineage` follows the first event of each file. A `session.forked` names
   `parent_session` and `parent_seq`; the chain is collected newest file first, then
   reversed so the oldest ancestor comes first, and finished with the session itself
   (`upto = u64::MAX`).
2. A missing ancestor or a cycle is `KernelError::Session` naming the chain. The
   session refuses to open: without its prefix it is inoperable (ADR-0014).
3. Each file is read with `replay_lenient(path)` and stopped at `upto`.
4. `message.appended` with `role: user` pushes the message and its ledger entry. An
   assistant message is held in `pending` until its calls are known, then
   `flush_pending` calls `push_assistant` and `push_tool_result`, the same lockstep
   the live turn uses.
5. `tool.call` adds a `ToolCallRequest` to `pending`; `tool.result` adds its output.
   Both are flushed together, so a turn with several calls keeps all of them.
6. `compaction.applied` calls `replay_compaction`, which re-applies the recorded
   `kept_tokens`, `cleared_tool_outputs` and `dropped_messages`.
7. Everything else (`cost.recorded`, `model.selected`, `hook.fired`, `approval.*`,
   `control.synced`) records what happened around the conversation and is skipped.

Inherited history is labelled (`user from <id>`, `assistant from <id>`,
`tool:<call_id> from <id>`), so a surface can say where a message came from.

## 4. Attach a client

`full_screen` starts `loongagent server --stdio --session PATH`; the CLI builds
`Server::new(kernel, session_id)` with a kernel factory for `session.new`. The TUI's
`Client::attach` sends `session.attach {cwd, client}`; the server answers
`{session_id, schema_version, existing_events: last_seq}` and writes nothing.
`Client::replay` then sends `session.replay`; the server answers with the events
`replay_lenient(log)` returns — a read, not a re-append.

Live events reach the client as they are appended: `prompt.submit` installs an
`event_sink`, streams each envelope, and `drain_events` with `last_sent` keeps the
response from repeating them. `session.new` swaps the kernel and resets `last_sent`
and `turns`, because those counters describe the old log.

## 5. Fork: a child that references, not copies

`loongagent fork [ID] [--at SEQ]` is a CLI command, not a wire method:

- The parent is the named id (`resolve_session`) or `newest_session`.
- The fork point is `--at`, else the parent's last `seq`. Zero is refused ("no events
  yet"), and a number past the end is refused with the real last seq.
- `new_session_id_in_dir` mints an id in the parent's directory; `SessionLog::create`
  creates the child; one `session.forked` event records `origin: "fork"`,
  `parent_session` and `parent_seq`. Nothing is copied.
- Reopening the child runs §3 again: ancestor prefix first, then the child's own
  events. The child lives beside its parent, so the parent id resolves to a file
  name rather than a path that can go stale.

## 6. Boundaries: resume, retry, steer, delete

| Action | What it does | Session boundary |
|---|---|---|
| **Resume** | opens the log, `replay_history`, `session.attach` refills the surface | same session |
| **Retry** | `Alt+R` sends the last prompt again as a new turn, not a rewind | same session |
| **Steer** | not implemented: a message typed during a turn waits and runs afterwards | — |
| **Delete** | `delete` removes the whole branch (`branch_of`), refuses a multi-session branch without `--force`, and refuses while `writers_of` finds an open process | branch |

A **parked approval does not survive a restart**: `Kernel.pending` is in memory only,
so resume rebuilds the conversation but not the question; the person starts a new
turn. `interrupt` is the only control that reaches a turn already running.

## 7. One resume and one fork, in sequence

```mermaid
sequenceDiagram
    autonumber
    participant U as Person
    participant C as CLI
    participant S as Server
    participant K as Kernel
    participant L as SessionLog
    participant T as TUI

    U->>C: sessions / resume <id>
    C->>L: replay_lenient each *.jsonl (summary)
    C-->>U: id · age · turns · events · parent@seq
    C->>C: Args::finish_session -> resolve_session
    C->>K: Kernel::open_with_models(path)
    K->>L: SessionLog::open
    K->>K: lineage()
    loop ancestors oldest first, then the session
        K->>L: replay_lenient(path) up to parent_seq
        K->>K: replay_history (messages + ledger)
    end
    C->>S: Server::new(kernel, id), run_stdio
    T->>S: session.attach {cwd, client}
    S-->>T: session_id, schema_version, existing_events
    T->>S: session.replay {session_id}
    S->>L: replay_lenient(log)
    S-->>T: events (a read; nothing appended)
    U->>C: fork <parent> --at <seq>
    C->>L: SessionLog::create(child)
    C->>L: session.forked {parent_session, parent_seq}
    Note over K,L: the child reopens through the same lineage replay
```

The kernel rebuilds the conversation before any client attaches, so a client that
joins late sees the same history the next model call will see.

## 8. Where to read next

- [sessions-lineage.md](../sessions-lineage.md) — the lifecycle and the lineage model.
- [events-storage.md](../events-storage.md) §5 — the event envelope and the two levels.
- [ADR-0014](../../decisions/0014-session-export-import.md) — why a child needs its ancestors.
- [ADR-0015](../../decisions/0015-two-tree-levels.md) — path, head, branch, and what is not built.
- `crates/tui/src/lib.rs` — `Client::attach` and `Client::replay`.
