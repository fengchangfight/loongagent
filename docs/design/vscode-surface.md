# The VS Code sidebar

- **Up:** [surfaces.md](surfaces.md)
- **Authority:** [ADR-0009](../decisions/0009-surfaces.md) · [VS Code sidebar spec](../superpowers/specs/2026-10-26-vscode-webview-design.md)
- **Code:** `clients/vscode/src/extension.ts`, `clients/vscode/src/chatView.ts`,
  `clients/vscode/src/sessionAgent.ts`, `clients/vscode/src/binary.ts`,
  `clients/vscode/webview/main.ts`
- **Verify:** `cd clients/vscode && npm run compile && npm test` · `./scripts/check.sh`

The VS Code surface is a client of the same local server as the TUI and the browser. It renders
in its own Activity Bar `WebviewView`, not in VS Code's native Chat view: that view is the
Copilot surface, requires a VS Code language model, and no longer supports `@` participants in
the Copilot harness. The sidebar needs neither Copilot nor a VS Code model.

## 1. Topology

```text
Activity Bar WebviewView  loongagent.chat
  │  typed JSON messages
  ▼
Extension host
  ├── ChatViewProvider       view, message bridge, commands, diff providers
  ├── TabManager             tabs, active tab, the five-tab ceiling
  ├── SessionAgent           one child, one LoongSession, one TurnGate, one transcript
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
is focused instead. Tabs are capped at five; closing a tab disposes its child and denies any
pending approval with reason `tab closed`.

The tab model caps at five, and `session.list` is the server capability the history needs.

## 3. Approvals and diffs

`tool.call` arrives before the tool runs, so the first mention of a path is the pre-turn content.
For `write_file` and `edit` the approval card carries a line diff:

- `write_file`: the current file → the requested content.
- `edit`: the current file with `old_string` → `new_string`, honoring `replace_all`.
- An absent or ambiguous `old_string` is not previewed; the card says the kernel will refuse.
- Allow once → `once`, Allow always → `action_class`, Deny → `once` with the fixed reason
  `denied in VS Code`; a closed card is Deny.
- `project` scope is not offered.

The review-before-write flow needs the user's policy `[permissions] write_workspace = "ask"`.
The extension never changes policy; with `allow` it says the write landed unreviewed.

After a turn, the paths whose current content differs from the first-mention snapshot become a
review card. `vscode.diff` opens with the snapshot on the left and the real file on the right;
the approval preview uses a virtual right side under `loongagent-preview`.

Nothing outside the workspace is snapshotted or previewed. A path outside it stays a name in the
transcript, never a file the extension reads.

## 4. Attachments and model

- `#file` attachments are absolute paths; the kernel reads the content.
- A selection is the one inlined body: path, line range and text, because `read_file` has no
  line-range parameter.
- The model dropdown comes from `model.list`; a change is `model.select` and is recorded as
  `model.selected`.
- `/new`, `/model`, `/context` and `/yolo` are composer commands. `/yolo` asks for a modal
  confirmation and renders `permissions.relaxed`.

## 5. Finding the binary

The extension does not bundle the Rust binary. `BinaryResolver` tries the `loongagent.path`
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
- Approval answers are keyed by tab and request id; an unknown answer is ignored.

## 7. Limits

- No per-hunk accept/reject and no diff for shell writes.
- No syntax highlighting, images or URL context.
- Stop is a POSIX `SIGINT`; Windows is documented as unsupported.
- One window per session: the server has no cross-window write token yet.
- The native `@loong` participant is gone; the sidebar is the VS Code entry point.
