# The VS Code panel

- **Up:** [surfaces.md](surfaces.md)
- **Authority:** [ADR-0009](../decisions/0009-surfaces.md) · [VS Code surface spec](../superpowers/specs/2026-10-26-vscode-webview-design.md)
- **Code:** `clients/vscode/src/extension.ts`, `clients/vscode/src/chatView.ts`,
  `clients/vscode/src/sessionAgent.ts`, `clients/vscode/src/binary.ts`,
  `clients/vscode/src/questions.ts`, `clients/vscode/webview/main.ts`
- **Verify:** `cd clients/vscode && npm run compile && npm test` · `./scripts/check.sh`

The VS Code surface is a client of the same local server as the TUI and the browser. It opens
an editor `WebviewPanel` beside the active editor (a separate editor group, so the file and
the chat remain visible together) from a loongagent icon in the editor title bar (and from
`loongagent: Open Chat`), not in VS Code's native Chat view: that view is the Copilot surface,
requires a VS Code language model, and no longer supports `@` participants in the Copilot
harness. The panel needs neither Copilot nor a VS Code model. The first click opens the panel;
clicking the icon again starts another conversation in a new internal tab.

## 1. Topology

```text
Editor tab WebviewPanel  loongagent.chat
  │  typed JSON messages
  ▼
Extension host
  ├── ChatViewProvider       panel, message bridge, commands, diff providers
  ├── TabManager             tabs, active tab, the five-tab ceiling
  ├── tabStore               persisted tab ids, session ids and titles
  ├── SessionAgent           one child, one LoongSession, one TurnGate per tab
  ├── Renderer               EventEnvelope -> UiPart
  └── BinaryResolver         setting, picked path, PATH, common install locations
        │  newline-delimited JSON-RPC 2.0
        ▼
  loongagent server --stdio --cwd <workspace> [--session <id> | --new]
```

The Webview has no `vscode` import and knows no protocol method. The host holds the transcript,
the attachments, the pending approvals and the diff snapshots; a rebuilt Webview receives them
again instead of replaying events.

## 2. One conversation, one child

A tab owns one session and one server child. A new conversation starts with `--new`; a reopened
one uses `--session <id>`. The same session is never opened twice in one window; the existing tab
is focused instead. Tabs are capped at five; closing an internal tab disposes its child and
denies any pending approval with reason `tab closed`. Closing the editor panel denies pending
approvals with `tab closed` but keeps the agent, so reopening the panel continues the session.

Tabs persist as `{id, sessionId, title}` in workspace state; the active tab is a separate key.
A reload restores them, attaches the active tab, and lazily starts the others on first
activation. A session that cannot attach is dropped with a visible message. History is
backed by the read-only `session.list`, injected by the CLI; the extension never reads the
session directory. Tabs are capped at five, and history returns the newest sessions first.

## 3. Approvals and diffs

`tool.call` arrives before the tool runs, so the first mention of a path is the pre-turn content.
For `write_file` and `edit` the approval card carries a line diff:

- `write_file`: the current file → the requested content.
- `edit`: the current file with `old_string` → `new_string`, honoring `replace_all`.
- An absent or ambiguous `old_string` is not previewed; the card says the kernel will refuse.
- Allow once → `once`, Allow always → `action_class`, Deny → `once` with the fixed reason
  `denied in VS Code`; a closed card is Deny.
- `project` scope is not offered.

A parked approval lives in the kernel's memory, not in the log
([sessions-lineage](sessions-lineage.md)). The transcript, however, is drawn from the log, so a
replayed session can show a card nothing is listening for. Two rules keep that card from becoming
a dead button:

- `session.attach` answers with `pending_approval`, the one request the kernel still holds. A card
  carrying any other id is retired: it keeps its place in the record, loses its buttons, and one
  notice line names it. This is the TUI's rule, not a new one.
- A held question with no turn driving it (no surface is running in this window) is answered with
  a denial and reason `no surface is holding this turn` — the same answer `TurnGate` gives a turn
  no surface holds — so the next prompt is accepted instead of refused as `out of order`.
- An answer to a card the host is not listening for is never swallowed: the card is retired and
  the reason said aloud.
- A prompt the server refuses with `out of order: approval <id> is still pending` is treated as a
  question to clear: that id is denied, the person is told, and the prompt is offered once more.
- Stop answers the pending question instead of interrupting. Denying *and* sending `SIGINT` in the
  same breath races, and when the interrupt wins the kernel keeps the question for good: no card,
  nothing to stop, and every later prompt refused.

The review-before-write flow needs the user's policy `[permissions] write_workspace = "ask"`.
The extension never changes policy; with `allow` it says the write landed unreviewed.

After a turn, the paths whose current content differs from the first-mention snapshot become a
review card. `vscode.diff` opens with the snapshot on the left and the real file on the right;
the approval preview uses a virtual right side under `loongagent-preview`.

Nothing outside the workspace is snapshotted or previewed. A path outside it stays a name in the
transcript, never a file the extension reads.

## 4. Attachments and model

- `@ file` attachments are absolute paths; the kernel reads the content. Typing `@` at a
  word boundary opens an inline list above the composer; typing filters it by prefix.
  Choosing an item leaves `@relative/path ` in the input and adds the attachment, so the
  typed message stays visible. There is no file browser, no window-level picker, and no
  separate button.
- The active editor selection is inlined automatically: path, line range and text, because
  `read_file` has no line-range parameter. A passive chip names it; it survives clicking into
  the chat (the active editor becomes undefined), and a new tab inherits it. Deselecting or
  switching to another text editor clears it.
- When nothing is selected, the focused workspace file and its 1-based cursor line are named
  in the prompt as `current file: <path> (cursor at line N)`, so "the current file" has a
  referent. A passive `file · <path>:<line>` chip shows it; it survives clicking into the
  chat, follows cursor movement and editor switches, and a new tab inherits it. A selection
  block already names its file, so the line is omitted then. A non-workspace or untitled
  editor clears it. The kernel's `read_file` fetches the body; the extension only names it.
- The model dropdown comes from `model.list`; a change is `model.select` and is recorded as
  `model.selected`.
- While a turn is running without a live delta, the bottom of the transcript shows an animated
  `Thinking…`/`Working…`/`Running <tool>…` row. The protocol streams model output deltas but
  no hidden reasoning stream, so this is activity, not a fabricated chain of thought.
- `/new`, `/model`, `/context`, `/yolo`, `/god` and `/ask` are composer commands. The first three
  change what is attached; the rest change what is asked. `/yolo` and `/god` toggle the way the
  TUI toggles them: naming the mode already on turns it off, and `/yolo` from `god` lands on
  `yolo`. `/ask` always asks again, and a click on the header chip walks `ask → yolo → god → ask`.
- Switching takes one gesture, with no confirmation dialog — the TUI switches on one keystroke.
  ADR-0007 §5 asks that a relaxed session *look* relaxed, not that changing it be laborious; the
  header mark is what carries the warning, so it must never lag the mode.
- A switch never blocks on a running turn. The server reads its next request only when the turn
  ends — `prompt.submit` runs the whole turn inside dispatch, and the connection is read one line
  at a time — so a `permissions.set` sent mid-turn would not even be *read* until the turn
  finished. Instead the panel shows the chosen mode at once, drawn dashed with the words "takes
  effect when this turn ends", and the switch lands in the turn's `finally`. A second switch
  during the same turn replaces the first; switching back to the mode already in force withdraws
  it. This is the reason the TUI needed a private control channel for the same gesture; the panel
  queues instead of adding a second connection.
- The mode belongs to the server, and only this process's word counts. A new kernel starts from
  the config file, so the panel reads `session.attach`'s `auto_approve` plus the
  `permissions.relaxed` events that arrived *after* attach — `existing_events` is the boundary —
  and never a resumed log's older events, which describe a process that is gone.
- A new process starts from the workspace's remembered choice (`ask` or `yolo`, never `god`: that
  is a decision about the turn in front of you, not a property of the workspace) or, failing that,
  from `loongagent.autoApprove`. The choice is re-applied as a recorded `permissions.relaxed`, so
  the panel and the log agree. A remembered `ask` is not re-applied: asking is already the default,
  and turning asking on is never the dangerous direction.
- Each mode change writes one notice stating that mode's own promise: `yolo` leaves the sandbox
  alone and still asks about irreversible actions, while `god` stops asking about deletions and
  outward sends and refuses two shapes outright (ADR-0020, ADR-0022). A person who believes
  nothing is asked any more would be wrong.
- The header carries the mode's own name while it is relaxed — `⚠ YOLO`, `⚡ GOD` — and a plain
  `ask` otherwise. The words are the commands, not a description of them: the TUI's own badge
  reads `⚠ 免确认`, and either way a silenced session is never silent about being silenced.

## 5. Finding the binary

The extension does not bundle the Rust binary. The server defaults to local mode; pass
`--centered` through `loongagent.extraArgs` to read the managed profile/control pack.
`BinaryResolver` tries the `loongagent.path`
setting, the path the user picked with **Locate**, PATH and the common install locations, then
runs `--version`. A schema mismatch is a warning, never a refusal.

`loongagent.path`, `loongagent.autoApprove` and `loongagent.extraArgs` are machine-scoped. A
repository's `.vscode/settings.json` must not be able to choose the executable or relax
approvals. No workspace-relative binary is auto-detected; a developer sets the path or uses
Locate.

## 6. Security

- Webview CSP: scripts and styles only from the Webview's own source; no external resources.
- Model text goes through the host-side Markdown subset; raw HTML is escaped, tool output uses
  `textContent`, and links are limited to http/https.
- The Webview sends intents only; the host owns every request to the child.
- Approval answers are keyed by tab and request id; an answer to a request this host is not
  listening for retires the card and says so, rather than being dropped in silence.

## 7. Limits

- No per-hunk accept/reject and no diff for shell writes.
- No syntax highlighting, images or URL context.
- Stop answers the pending question when there is one, and is a POSIX `SIGINT` otherwise; Windows
  is documented as unsupported.
- One window per session: the server has no cross-window write token yet.
- The native `@loong` participant is gone; the editor panel is the VS Code entry point.
