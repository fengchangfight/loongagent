# VS Code sidebar surface: an independent chat window with sessions and review-before-write

> **Surface amendment (2026-10-26):** The implemented surface is an editor
> `WebviewPanel` opened from a loongagent icon in the editor title bar and the
> `loongagent.openChat` command. The Activity Bar `WebviewView` described below was
> the initial surface. The session, tab, approval, attachment and protocol sections
> still apply; read `WebviewView` as `WebviewPanel` where they name the container.

- **Date:** 2026-10-26
- **Status:** Approved on 2026-10-26. The implementation plan is written from this
  file.
- **Scope:** a personal-use VS Code extension that drives the existing
  `loongagent server --stdio` through its existing JSON-RPC wire, rendered in its own
  Activity Bar Webview. One additional read-only protocol method, `session.list`, is
  added so the surface can show history without reading the session directory itself.
  No kernel behaviour change.
- **Supersedes:** the UI assumption of
  [2026-10-16-vscode-surface-design.md](2026-10-16-vscode-surface-design.md). That spec's
  transport, turn/approval state machine, snapshots and diff insights remain; its
  "render inside VS Code's native Chat view" decision does not.

## 1. Problem

The merged VS Code surface renders inside VS Code's Chat view as an `@loong` chat
participant. That view is the Copilot chat surface: every request needs a VS Code
language model, `@` participants are deprecated and unsupported in the Copilot
harness, and a user who does not sign in to Copilot sees "Language model unavailable"
before the participant handler runs. A placeholder language model can unlock the
input, but it is a workaround: the extension still lives inside somebody else's chat
window, cannot show tool cards or diffs the way the agent needs, and depends on a
surface that VS Code is retiring.

What the owner wants is the editor experience the work actually needs: a chat window
that belongs to loongagent, works without Copilot or any VS Code model, can carry the
editor selection and file references, shows what the agent is doing, asks for approval
with a real diff, lets a write be reviewed before it happens, and keeps several
conversations open.

## 2. Goals and success criteria

| # | Criterion | How it is verified |
|---|---|---|
| 1 | The sidebar opens and streams an answer without Copilot sign-in or a VS Code language model | Open the view in a fresh profile; type a prompt; markdown appears incrementally |
| 2 | Code selection and file references reach the agent | Select code and attach a file; on send the prompt shown in the log names the path, the line range, and the selected text; no selection button is needed |
| 3 | The model comes from loongagent, not VS Code | The model dropdown lists `model.list`; selecting one records `model.selected` |
| 4 | A write can be reviewed before it happens | With `write_workspace = "ask"`, the approval card shows a diff; Deny leaves the file byte-identical; Allow writes it |
| 5 | A turn's changes are reviewable afterwards | Every path the turn touched opens in `vscode.diff` with the content it held at first mention |
| 6 | Several sessions are open at once | Three tabs, two turns running concurrently, each transcript isolated; closing a tab stops its child |
| 7 | Sessions survive a window reload | Reopen the window; the open tab ids and the active transcript are restored |
| 8 | The extension contains no agent logic | No routing, no context assembly beyond the explicit selection text, no policy verdict of its own |
| 9 | The binary is found, or the user is told exactly what to do | A missing binary produces a Locate action and a named setting, never a stack trace |

## 3. Non-goals

- **Bundling the Rust binary.** The extension is TypeScript only. The binary is
  installed separately and located as described in §11.
- **Marketplace publishing, `vsce` packaging, publisher identity.** Personal use, run
  from source with `--extensionDevelopmentPath` or a local VSIX later.
- **Proposed VS Code APIs.** Stable APIs only; no `enabledApiProposals`.
- **Per-hunk accept/reject.** The approval card and the review diff are whole-file.
- **A diff for shell writes.** `shell` can write anywhere the sandbox allows; the
  approval card shows the command and its classification, not a preview.
- **Syntax highlighting, images, URL context, multi-root workspaces.** Later, if ever.
- **A cross-window write token.** Two VS Code windows can still open the same session;
  the server has no write token yet. The extension refuses to open the same session in
  two tabs of one window; across windows, the limitation is documented.
- **Windows cancellation.** Stop remains a POSIX `SIGINT`; Windows is documented as
  unsupported, as in the earlier spec.
- **ACP.** The adapter is a separate, later subsystem.

## 4. Decisions already made

These are owner decisions from the design conversation, not open questions:

1. **Placement:** Activity Bar sidebar (`WebviewView`), not an editor tab and not the
   native Chat view.
2. **UI stack:** vanilla TypeScript + CSS, no framework. The Webview bundle is built by
   esbuild; the host and the Webview communicate only through typed JSON messages.
3. **Write flow:** review-before-write. The approval card shows a diff for
   `write_file` and `edit`; Deny leaves the file untouched. This relies on the user's
   policy `[permissions] write_workspace = "ask"`. The extension never changes policy.
4. **Sessions:** full multi-tab + history in scope. One tab is one session and one
   `loongagent server` child process.
5. **Delivery:** M1 is a single-tab usable sidebar (chat, stream, model, attachments,
   approvals, diff, review, stop, reload). M2 adds the tab bar, history and
   `session.list`. Both ship; M1 is a milestone, not a reduced feature set.
6. **Binary:** no bundling. A resolver checks the explicit setting, PATH, common
   install locations and a user-chosen path; it validates `--version` and shows the
   schema. Workspace-provided binary paths are not trusted.
7. **Native Chat is removed:** the `chatParticipants` contribution, the participant
   handler and the placeholder `languageModelChatProviders` model all go away. `@loong`
   in the Copilot chat is no longer a supported entry point.

## 5. Architecture

```text
VS Code Activity Bar
  └── WebviewView  loongagent.chat
        │  postMessage (typed JSON only)
        ▼
  Extension Host (Node)
        ├── ChatViewProvider        view, message bridge, commands, diff providers
        ├── TabManager (pure)       open tabs, active tab, max, dedupe
        ├── SessionAgent[tabId]     one child, one LoongSession, one TurnGate, one Transcript
        ├── Transcript[tabId]       ordered UiParts; the Webview's source of truth
        ├── Renderer                EventEnvelope -> UiPart
        ├── FileSnapshots           per-turn "before" content
        ├── DiffProvider            vscode.diff left side; previewProvider serves the right
        └── BinaryResolver          setting -> PATH -> candidates -> user choice
              │  newline-delimited JSON-RPC 2.0 over stdin/stdout
              ▼
        loongagent server --stdio --cwd <workspace> [--session <id> | --new]
              ▼
        ~/.loongagent/projects/<project>/<id>.jsonl   ← still the only source of truth
```

Boundaries:

- **The Webview has no `vscode` import and no protocol knowledge.** It renders parts,
  holds input, attachments and scroll position, and sends intents (`send`, `approval`,
  `cancel`, `newTab`, `selectTab`, `closeTab`, `history`, `attachFile`,
  `selectModel`, `review`, `openDiff`, `yolo`). The editor selection and the workspace file
  list are host state, not Webview intents.
- **The extension host holds every fact the view needs.** A hidden or rebuilt Webview is
  restored by sending `bootstrap` with the tabs and the active transcript, not by
  replaying agent logic.
- **One `SessionAgent` per tab.** It owns the child process, the `LoongSession`, the
  `TurnGate` and the `Transcript`. Closing the tab disposes the child. Two tabs never
  share a session.
- **Existing modules stay.** `agentSession.ts`, `session.ts`, `turn.ts`, `render.ts`,
  `snapshots.ts` and `diffProvider.ts` are reused. `workspaceAgent.ts` becomes
  `sessionAgent.ts` with one instance per tab and the stale-gate bug fixed.
- **No agent logic in the view.** The view never chooses a model, assembles context
  beyond the explicitly attached selection text, or turns a policy verdict into an
  allow.

## 6. Multiple sessions and history

### 6.1 TabManager

`TabManager` is pure state, tested without VS Code:

```ts
interface TabState {
  id: string;                 // stable UI id, e.g. "tab-3"
  sessionId?: string;         // known after attach
  title: string;              // "New session" until the first user message
  status: "idle" | "running" | "waiting" | "stopped";
  unread?: boolean;           // a finished/blocked turn in a background tab
}

class TabManager {
  open(tab: TabState): void;               // refuses more than MAX_TABS
  close(id: string): void;                 // active moves to a neighbour
  activate(id: string): void;
  find(sessionId: string): TabState | undefined;
  active(): TabState | undefined;
}
```

Rules:

- `MAX_TABS = 5`. Opening a sixth tab answers with an actionable message.
- Opening a session already present focuses that tab; it never spawns a second writer
  on the same log in one window.
- Closing a tab disposes its child. If a turn is running, it is stopped; the UI says so.
- The tab title starts as `New session`, becomes the first prompt truncated to 60
  characters after the first `send`, and is never inferred from the model.
- Background tabs keep running and keep their own approvals. The tab bar shows a badge
  for `running` and `waiting`. Closing a tab with a pending approval answers Deny with
  reason `tab closed`.

### 6.2 `session.list`

History cannot be read by the surface from `~/.loongagent/projects/...`: that would
copy the project-slug rule and the log format into a client and violate "a capability
is implemented once, in the server" (ADR-0009 §3). The CLI already has
`SessionSummary` in `crates/cli/src/session.rs`; it is exposed through a read-only
protocol method, with a small `pub(crate)` mapping rather than a second parser.

```text
session.list   { limit?: number }
  → { sessions: [
        { id, title, updated_at, events, turns }
      ] }
```

- `title` is the first user prompt with whitespace collapsed, truncated to 80
  characters; `New session` when there is none.
- `updated_at` is Unix seconds.
- `limit` defaults to 50 and is clamped to 1..200.
- The method records no event; it is a read.
- Layering: `crates/schema/protocol.toml` owns the types; `crates/server` owns the
  dispatch and takes an injected `session_lister` closure; `crates/cli/src/server.rs`
  provides the real closure from `crate::session::session_summaries(&args.cwd)`. The
  server crate must not depend on the CLI. A server without a lister returns an empty
  list, not invented data.
- Regeneration: `scripts/gen-schema.py` updates Rust, TypeScript and samples. Tests
  cover schema round-trip, server dispatch with a fake lister, and the CLI mapping.

### 6.3 Lifecycle and reload

- Persist `{tabs: [{id, sessionId}], activeTabId}` in `workspaceState`.
- On view open, restore the tab list and attach the active tab. Other tabs are lazy:
  their child starts on first activation. A restored tab whose session no longer
  resolves is dropped with a visible message.
- `+` creates a tab and spawns `--new`. History picks a session and spawns
  `--session <id>`.
- The child is killed in `SessionAgent.dispose()` and on extension deactivate.
- A child that exits unexpectedly clears its `LoongSession`, `TurnGate` and `live`
  references; the next send in that tab respawns it. This also fixes the current bug
  where the gate kept pointing at the dead session.

## 7. UI

```text
┌ loongagent  [session A ×] [session B ×] [+]   [History]   [model: qwen ▾] [Stop]
├──────────────────────────────────────────────────────────────────────────────
│  you
│    explain this function
│
│  assistant
│    ...markdown streaming...
│
│  ▸ tool read_file (investigate) src/a.ts                         ok
│  ▸ tool write_file (write_workspace) src/a.ts                    waiting
│
│  ┌ approval: write_file · src/a.ts                       -3 +5 ┐
│  │   @@ -10,3 +10,5 @@                                         │
│  │   - old line                                               │
│  │   + new line                                               │
│  │   [Open full diff] [Allow once] [Allow always] [Deny]      │
│  └─────────────────────────────────────────────────────────────┘
│
│  ✓ approval granted (once)
│  [Review 2 changed files]
├──────────────────────────────────────────────────────────────────────────────
│  attachments: [src/a.ts ×] [selection · src/a.ts:10-18]
│  ┌────────────────────────────────────────────────────────────┐
│  │ Ask loongagent…                                            │
│  └────────────────────────────────────────────────────────────┘
│  [@ file] [Send]
└ status: running · waiting for approval · idle
```

- **Tab bar:** active tab, close `×`, `+`, History, model dropdown, Stop. Status dots
  for running and waiting.
- **History overlay:** sessions from `session.list`, newest first, with title, updated
  time and turns. Clicking focuses an open tab or opens a new one. No delete/rename in
  this version.
- **Transcript:** user messages, assistant markdown, tool cards, approval cards, review
  cards and notices in one stream.
- **Tool card:** tool name, action class, paths, status, collapsible output, truncation
  marker. Output is plain text.
- **Approval card:** path, `-N +M`, folded unified diff for `write_file`/`edit`,
  `Open full diff`, three buttons. A closed card is Deny. `project` scope is not
  offered.
- **Composer:** Enter sends, Shift+Enter newlines, Esc stops; `@` at a word boundary opens
  an inline file list above the input; typing filters by prefix, ↑/↓ moves, Enter/Tab
  attaches, Esc closes. The active editor selection is attached automatically when
  non-empty and shown as a passive chip. File attachments have remove buttons; deselecting
  removes the selection.
- **Slash commands:** `/new` opens a new tab, `/model` focuses the model dropdown,
  `/context` asks the host for `context.explain` and renders the numbers as a notice,
  and `/yolo` toggles approval prompts after an explicit confirmation. `/yolo` never
  changes the sandbox, and its `permissions.relaxed` event is shown.
- **Markdown:** a host-side renderer supports fenced code, inline code, bold, italic,
  lists and http/https links; it never emits raw HTML. Tables, images and syntax
  highlighting are out.
- **Empty state:** a short instruction and the resolved binary/session status.
- **Theme:** only VS Code `--vscode-*` variables.
- **Favicon:** derived from the owner's `favicon.png` (742×793 RGBA, transparent):
  `media/loong-24.png`, `media/loong-48.png` and `media/loong-128.png`. The 48px file is
  the Activity Bar icon; the 128px file is the Webview `<link rel="icon">`. The source
  stays outside `clients/vscode`; only the small derived assets are committed.

## 8. Host ↔ Webview protocol

All messages are plain JSON. `webviewProtocol.ts` owns the types and type guards; an
unknown or malformed message is logged and ignored.

**Host → Webview**

```ts
type HostMessage =
  | { type: "bootstrap"; tabs: TabState[]; activeTabId?: string; activeTranscript: UiPart[];
      models: ModelOffering[]; sessionId?: string; binary?: { path: string; version: string } }
  | { type: "tabs"; tabs: TabState[] }
  | { type: "transcript"; tabId: string; parts: UiPart[] }
  | { type: "delta"; tabId: string; text: string }
  | { type: "part"; tabId: string; part: UiPart }
  | { type: "assistantFinal"; tabId: string; text: string; html: string }
  | { type: "attachments"; tabId: string; items: Attachment[] }
  | { type: "selection"; selection?: Attachment }
  | { type: "files"; files: FileChoice[] }
  | { type: "sessions"; sessions: SessionInfo[] }
  | { type: "error"; tabId?: string; message: string; detail?: string };
```

**Webview → Host**

```ts
type WebviewMessage =
  | { type: "ready" }
  | { type: "send"; tabId: string; text: string }
  | { type: "approval"; tabId: string; requestId: string; choice: "once" | "always" | "deny" }
  | { type: "cancel"; tabId: string }
  | { type: "newTab" }
  | { type: "selectTab"; tabId: string }
  | { type: "closeTab"; tabId: string }
  | { type: "history" }
  | { type: "openSession"; sessionId: string }
  | { type: "selectModel"; tabId: string; name: string }
  | { type: "context"; tabId: string }
  | { type: "attachFile"; tabId: string; path: string }
  | { type: "removeAttachment"; tabId: string; id: string }
  | { type: "review"; tabId: string; token: string; paths: string[] }
  | { type: "openDiff"; tabId: string; token: string; path: string }
  | { type: "yolo"; tabId: string; on: boolean };
```

The host never trusts a `tabId` it does not know; the Webview never sends a method name
or a raw event.

`UiPart` is the only thing a transcript contains. It is plain JSON and never carries a
function or a VS Code object:

```ts
type UiPart =
  | { kind: "user"; text: string }
  | { kind: "assistant"; text: string; html: string }   // html is sanitized by the host
  | { kind: "progress"; text: string }
  | { kind: "tool"; callId: string; tool: string; actionClass: string; paths: string[];
      status: "running" | "ok" | "failed"; output?: string; truncated?: number }
  | { kind: "approval"; requestId: string; actionClass: string; reason: string;
      paths: string[]; diff?: DiffPreview; state: "pending" | "allowed" | "denied" }
  | { kind: "review"; token: string; paths: string[] }
  | { kind: "notice"; level: "info" | "warn" | "error"; text: string };

type DiffPreview = { unified: string; added: number; removed: number; truncated: boolean };

type Attachment =
  | { id: string; kind: "file"; path: string }
  | { id: string; kind: "selection"; path: string; startLine: number; endLine: number; text: string };
```

`assistant.html` is the host-side Markdown subset renderer's output; the Webview sets
it with `innerHTML` only because the host escaped it. `tool.output` is plain text and
is set with `textContent`.

## 9. One turn, approvals and diffs

1. `send` refuses if that tab already has a turn running. It ensures the session,
   clears the tab's attachments into the prompt, and calls `beginTurn` with a sink.
2. `model.delta` becomes `{type:"delta"}`; the Webview appends to a live assistant
   bubble. `message.appended` for the assistant becomes `assistantFinal`, which
   replaces the live bubble and is stored in the transcript.
3. `event` goes through `Renderer.event`, producing structured `UiPart`s. Tool calls
   and results update one card by `call_id`; a result is never appended as a second
   tool.
4. `approval.requested` becomes an approval part. For `write_file` and `edit`, the
   host computes a preview:
   - `write_file`: before = the current file (already snapshotted), after = `args.content`.
   - `edit`: before = current file; after = `old_string` → `new_string` in memory,
     honoring `replace_all`. If `old_string` is absent, or ambiguous without
     `replace_all`, the card says the kernel will refuse and shows no diff.
   - The diff is a line-based `unifiedDiff`, capped; it is never presented as exact
     when the preview failed.
5. The Webview's answer resolves the pending decision; the host calls
   `approval.respond`. `Allow once` maps to `once`, `Allow always` to `action_class`,
   and `Deny` to `granted: false, scope: "once", reason: "denied in VS Code"`.
   Dismissing the card is Deny. Closing the tab answers Deny with reason `tab closed`.
   The `project` scope is never offered.
6. `Open full diff` opens `vscode.diff` with the turn's snapshot on the left and a
   `loongagent-preview` virtual document on the right. After acceptance the real file
   is what it is; the preview tab is explicitly a preview.
7. If the policy is `write_workspace = "allow"`, no approval event exists. The tool
   result card says it was written unreviewed, and a hint names
   `[permissions] write_workspace = "ask"`. The extension never changes the policy.
8. When a turn ends, paths harvested from `tool.call` args are normalized against the
   workspace (absolute paths, and known `path` args of `read_file`/`write_file`/`edit`)
   and filtered against their snapshots; if any differ, the review card lists them.
9. `/context` calls `context.explain` and renders the quoted numbers (context tokens,
   budget, messages, tools, compactions) as a notice; it computes no percentage of its
   own. `/yolo` asks the host for an explicit warning confirmation, then calls
   `permissions.set`; the resulting `permissions.relaxed` event is rendered.
10. Stop sends `SIGINT` to that tab's child. The child survives; `turn.interrupted`
    renders as a warning. The next send is accepted.
11. A child that exits mid-turn rejects the open request; the tab shows the exit code
    and the next send respawns. A request that goes silent for 120 s is a stalled turn,
    not a hung UI.

## 10. Attachments and selection

- **File:** typing `@` at a word boundary opens an inline list above the input; there is
  no file browser, no window-level picker, and no separate button. Typing filters the
  labels by prefix, ↑/↓ moves, Enter/Tab attaches, and Esc closes. The list is
  `workspace.findFiles`, capped at 2000, excluding `node_modules` and `.git`, sorted by
  relative path and sent to the Webview once. The attachment stores an absolute path. The
  prompt appends `referenced files:\n- <path>`; the kernel's own read tools fetch the
  content. The extension does not inline file bodies.
- **Selection:** the host listens to `onDidChangeTextEditorSelection` and
  `onDidChangeActiveTextEditor`; a non-empty selection in a workspace file becomes an
  attachment automatically, with no button or context-menu command. The attachment
  stores `{path, startLine, endLine, text}`. On send, the prompt appends one fenced block
  with the path, the line range and the text. This is the one inlined body, because
  `read_file` has no line-range parameter. The Webview shows a passive chip; deselecting,
  switching files, leaving the workspace, or selecting an empty range removes it.
- File attachments are per tab and live in the host; the Webview renders chips and can
  remove them. Removing one updates the host's list. The current selection is a host
  fact, not a removable attachment: it stays for the next send while the editor still
  has it selected.

## 11. Binary discovery and version

The extension contains no binary. `BinaryResolver` runs before the first spawn in a
window:

1. `loongagent.path` if set and non-empty. An absolute path is checked for existence
   and executability; a bare name is resolved through PATH.
2. A path the user picked with the **Locate loongagent** command, stored in
   `globalState`. An explicit choice beats auto-detection.
3. PATH via `which`/`where` semantics.
4. Common locations: `~/.cargo/bin/loongagent`, `~/.local/bin/loongagent`,
   `/usr/local/bin/loongagent`, `/opt/homebrew/bin/loongagent`, plus `.exe` on Windows.
5. Workspace-relative `target/release/loongagent` or `target/debug/loongagent` is
   **never** auto-detected. A developer sets `loongagent.path` explicitly or uses
   Locate.

After resolution:

- Run `loongagent --version` once (short timeout), parse the version, schema and commit,
  and show the path and schema in the output channel and the empty state.
- Compare the schema with `SCHEMA_VERSION` from `protocol.generated.ts`; a mismatch is a
  visible warning, not a silent refusal.
- A missing binary produces an error card with the Locate action and the setting name;
  no stack trace.

Security:

- `loongagent.path`, `loongagent.autoApprove` and `loongagent.extraArgs` are
  **machine-scoped** settings. A workspace's `.vscode/settings.json` must not be able to
  point the extension at its own binary or silently relax approvals. `loongagent.model`
  may stay resource-scoped because it selects a configured model, not code.
- The extension never executes a workspace-relative binary automatically.
- The child is spawned with an argument array, never a shell, with the workspace as
  `cwd`.

## 12. Security

- Webview CSP: `default-src 'none'`; scripts and styles only from
  `webview.cspSource`; images from `webview.cspSource` and `data:`; no external
  resources.
- Model text is rendered by the host-side Markdown subset renderer. It escapes text and
  emits only a fixed set of tags; raw HTML is never passed through. Tool output is set
  with `textContent`.
- Links are limited to `http`/`https`; no `javascript:` or command URIs.
- The Webview cannot invoke a protocol method; only the host can.
- Approvals are tied to `{tabId, requestId}`; an unknown or stale request is ignored.
- The host refuses to open the same session in two tabs in one window.

## 13. Files and build

```text
clients/vscode/
  package.json            viewsContainers.activitybar, views, commands, menus, configuration
  esbuild.mjs             extension (CJS, vscode external) + webview (IIFE)
  src/
    extension.ts           activate, register view/commands/providers, dispose
    chatView.ts            WebviewViewProvider, bridge, tabs, attachments, approvals
    tab.ts                 TabManager (pure)
    sessionAgent.ts        one child + session + gate + transcript per tab
    transcript.ts          ordered UiParts per tab (pure)
    webviewProtocol.ts     message types and guards (pure)
    diff.ts                write/edit preview + unified diff (pure)
    previewProvider.ts     the "after" side of a review-before-write diff (virtual document)
    markdown.ts            safe Markdown subset renderer (pure)
    binary.ts              BinaryResolver (pure with injected fs/path)
    render.ts              EventEnvelope -> UiPart
    prompt.ts              file paths + selection block
    agentSession.ts / session.ts / turn.ts / snapshots.ts / diffProvider.ts
                           reused; previewProvider.ts serves the right side
  webview/
    main.ts                DOM, tab bar, history overlay, cards, composer
    style.css              VS Code theme variables
  media/
    loong-24.png           derived from favicon.png
    loong-48.png           Activity Bar icon
    loong-128.png          Webview favicon
  test/                    protocol, tab, transcript, diff, markdown, binary, prompt, manifest
```

- Remove `modelProvider.ts` and the `languageModelChatProviders` contribution; remove
  `chatParticipants`, the participant handler and `onStartupFinished`.
- Add `viewsContainers.activitybar` with `media/loong-48.png`, and `views.loongagent`
  with `{id: "loongagent.chat", type: "webview"}`.
- Commands: `loongagent.openChat`, `loongagent.locateBinary`,
  `loongagent.reviewChanges`. Selection is automatic, so there is no attach command or
  editor context-menu item. The `loongagent.smoke` command and its `runSmoke` handler are
  removed; the wire proof is the test suite and the manual acceptance below.
- `scripts/check.sh`: the extension step becomes `npm run compile && npm test` so the
  Webview bundle is built in the gate.
- Rust: `crates/schema/protocol.toml`, regenerated files, `crates/server/src/lib.rs`,
  `crates/cli/src/server.rs`, `crates/cli/src/session.rs`.
- Documentation: this spec; update [surfaces.md](../../design/surfaces.md) with the
  VS Code Webview, its code paths and `Verify` command;
  [05-parity-and-synthesis.md](../../05-parity-and-synthesis.md) with the new surface;
  `DEVELOPMENT.md` and the root `README.md` with the run and Locate instructions; and a
  `clients/vscode/README.md` covering settings, policy, tabs and limitations. The old
  `2026-10-16-vscode-surface-design.md` stays as a record; this file states what it
  supersedes.

## 14. Tests and verification

Pure TypeScript tests (no VS Code):

- `webviewProtocol`: valid messages, unknown type, missing field, malformed input.
- `tab`: open/close/activate/dedupe/max/neighbour.
- `transcript`: append, update tool by id, reset, per-tab isolation.
- `diff`: new file, append, replace, `replace_all`, absent `old_string`, cap, no-op.
- `markdown`: code fence, inline code, bold, links, XSS payload, raw HTML.
- `binary`: explicit path, PATH hit/miss, candidates, user choice, version parse,
  schema warning, no workspace auto-run.
- `prompt`: file path, selection block, dedupe.
- `render`: structured parts and normalized relative paths.
- `manifest`: view/commands exist; no `chatParticipants`; no proposed APIs.

Rust tests:

- `session.list` schema round-trip.
- server dispatch with a fake lister, including the empty case.
- CLI mapping from `SessionSummary` to `SessionInfo`, title truncation, limit clamp.

The existing extension tests stay green (the temporary placeholder-model test is
removed with the workaround); the goal is 115+.

Manual acceptance (no Copilot sign-in):

1. Sidebar opens; the binary path and schema are shown; a prompt streams.
2. `@` file and selection: the prompt/log names the path, lines and text.
3. Model dropdown changes the model; `model.selected` appears.
4. `write_workspace = "ask"`: Deny leaves the file byte-identical; Allow writes it.
5. Two files changed → Review opens both diffs with the pre-turn left side.
6. Three tabs; two concurrent turns; closing a tab kills its child; reload restores.
7. Kill a server child; the next send respawns and says why.
8. `./scripts/check.sh` green (including the extension compile/test step).

## 15. Phases

**M1 — single-tab sidebar, usable daily**

- Webview shell, tabs data model with one tab, streaming, Markdown, tool cards,
  approvals with diff, review, stop, reload, attachments, model dropdown, binary
  resolver, error paths.
- Native chat participant and placeholder model removed.
- This milestone is usable and reviewable without the multi-process work.

**M2 — multiple sessions**

- Tab bar, history overlay, `session.list` in schema/server/CLI, one child per tab,
  concurrent turns, tab persistence and lazy restore, tab badges, close/deny.
- Full acceptance criteria 6 and 7.

Both milestones are in scope. M1 is not a reduced product; it is the order in which the
risk is retired.

## 16. Risks

| Risk | Why it bites | Mitigation |
|---|---|---|
| Webview XSS | Model and tool output are untrusted | Host-side Markdown subset renderer; `textContent` for tool output; CSP |
| Webview rebuild loses state | A hidden view can be recreated | Host owns `Transcript`; `bootstrap` on `ready` |
| Preview is wrong for `edit` | The tool refuses ambiguous matches | Detect absent/ambiguous `old_string`; say "kernel will refuse"; never fabricate a diff |
| Policy stays `allow` | No approval event exists, so no pause | Say "written unreviewed"; name the policy key; review afterwards |
| Approval never answered | A hidden/closed tab | Badge; close = Deny; no silent allow |
| Five children are heavy | Each loads config and sandbox | `MAX_TABS = 5`; lazy restore; dispose on close |
| Same session in two windows | The server has no write token yet | Same-window dedupe; document the cross-window limitation |
| `session.list` reads many logs | Large history | `limit` 50, clamp 200; reuse the CLI's listing |
| Binary path is abused by a workspace | Running a repo-provided executable | `path`/`autoApprove`/`extraArgs` machine-scoped; no workspace auto-detection |
| Favicon is too detailed at 24px | A 742×793 illustration loses detail | Derived 24/48/128 with transparent padding; a simplified mark later if needed |
| Tab lifecycle bugs | Multiple children and sessions | `TabManager` pure tests; one child per tab; no shared gate |

## 17. What is deliberately left out

- Per-hunk accept/reject and inline edit widgets.
- Shell command diff previews.
- Session delete/rename/fork in the history panel.
- Multi-root workspaces; the first folder remains the workspace.
- Image and URL attachments; syntax highlighting.
- A cross-window write token (ADR-0009 §5).
- ACP, Marketplace packaging, Windows cancellation, project-scope approvals.
