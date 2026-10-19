# Flow: resume and fork

- **Up:** [sessions-lineage.md](../sessions-lineage.md)
- **Authority:** [ADR-0003](../../decisions/0003-jsonl-truth-sqlite-index.md) · [ADR-0015](../../decisions/0015-two-tree-levels.md)
- **Code:** `crates/server/src/lib.rs`, `crates/kernel/src/turn/session.rs`, `crates/cli/src/session.rs`
- **Verify:** `cargo test -p loong-server -p loong-kernel` · `./scripts/check.sh`

A session is its JSONL file, so reopening is a disk read and forking is a new file
that references an old one. This page keeps the sequence. The file format and the
event tree are owned by [events-storage.md](../events-storage.md); the lifecycle rules
are [sessions-lineage.md](../sessions-lineage.md).

## 1. List, choose, open

`loongagent sessions [--all] [--limit N] [--cwd DIR]` calls `list_sessions`:

- `session_summaries_in` reads each `*.jsonl` with `fs::read_to_string` and parses its
  lines: events, turns, the first user prompt and the first-event `session.forked`
  reference (`parent@seq`), newest first. `--all` uses `all_session_summaries`;
  `newest_session` marks the one a bare `tui` would continue.
- `Args::finish_session` settles one path: `--new` uses `new_session_id`;
  `--session` / `resume` uses `resolve_session` (exact id, unambiguous prefix,
  `latest`, or a path; `match_session` in this project,
  `match_session_across_projects` elsewhere); nothing means `newest_session`.
- `build_kernel` (`crates/cli/src/server.rs`) calls `Kernel::open_with_models` for an
  existing log or `Kernel::create_with_models` for a new one. `Kernel::build` rebuilds
  the derived index (`Index::rebuild_into`) and calls `replay_history`.

## 2. Rebuild the conversation

`Kernel::replay_history` first calls `Kernel::lineage`: it follows each file's first
`session.forked`, oldest ancestor first, and ends with the session itself
(`upto = u64::MAX`). A missing ancestor is refused with the chain named; a cycle names
the session that repeated. Each file is read with `replay_lenient` up to its
`parent_seq` (or the end) and replayed into messages and the ledger:

- `message.appended`: a user message at once; an assistant message held until its
  calls are known.
- `tool.call` / `tool.result`: attached to the held assistant turn and flushed
  together by `flush_pending` → `push_assistant` / `push_tool_result`.
- `compaction.applied`: `replay_compaction` re-applies the recorded `kept_tokens`,
  `cleared_tool_outputs` and `dropped_messages`.

Everything else (`cost.recorded`, `model.selected`, `hook.fired`, `approval.*`,
`control.synced`) is skipped. Only inherited **user** messages keep the label
`user from <ancestor>`; replayed assistant and tool entries are ordinary history.
(`crates/kernel/src/turn/session.rs` owns this replay; the tree is
`events-storage.md` §5.)

## 3. Attach a client

`full_screen` starts `loongagent server --stdio --session PATH`; `Server::new(kernel,
session_id)` carries a factory for `session.new`. `Client::attach` sends
`session.attach {cwd, client}` and receives `{session_id, schema_version,
existing_events: last_seq}`; `Client::replay` sends `session.replay` and receives the
log's events. Neither writes. Live events stream through the `event_sink`, and
`last_sent` stops the response from repeating them. `session.new` swaps the kernel and
resets `last_sent` and `turns`. The lifecycle is sessions-lineage §1.

## 4. Fork

`loongagent fork [ID] [--at SEQ]` is a CLI command, not a wire method:

- Parent: `resolve_session` or `newest_session`.
- Fork point: `--at`, else the parent's last `seq`; zero and a number past the end are
  refused.
- `new_session_id_in_dir` + `SessionLog::create`; one `session.forked` event
  (`origin: "fork"`, `parent_session`, `parent_seq`). Nothing is copied.

Reopening the child runs §2: ancestor prefix first, then the child's own events.

## 5. Boundaries

Resume keeps the same session; retry is a new turn in it; steer is not implemented;
delete removes the branch and refuses a multi-session branch without `--force` or
while `writers_of` finds an open process. A parked approval is memory-only, so resume
rebuilds the conversation but not the question. These rules are sessions-lineage §5.

## 6. One resume and one fork, in sequence

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
    C->>L: read_to_string each *.jsonl; parse lines (summary)
    C-->>U: id · age · turns · events · parent@seq
    C->>C: finish_session -> resolve_session (match_session_across_projects elsewhere)
    C->>K: open_with_models(path)
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
joins late sees the history the next model call will see.

## 7. Where to read next

- [sessions-lineage.md](../sessions-lineage.md) — lifecycle, lineage and the boundaries.
- [events-storage.md](../events-storage.md) §5 — the tree; §6 — the envelope.
- [ADR-0014](../../decisions/0014-session-export-import.md) — why a child needs its ancestors.
- [ADR-0015](../../decisions/0015-two-tree-levels.md) — path, head, branch, and what is not built.
- `crates/tui/src/lib.rs` — `Client::attach` and `Client::replay`.
