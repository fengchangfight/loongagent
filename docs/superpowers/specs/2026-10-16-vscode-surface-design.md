# VS Code surface: the agent in the editor, as a thin client of the existing server

- **Date:** 2026-10-16
- **Status:** Approved in conversation; this document is the spec the implementation plan is written from
- **Scope:** a personal-use VS Code extension (`clients/vscode/`) that drives the existing
  `loongagent server` over the existing stdio JSON-RPC wire, rendered in VS Code's native Chat view.
  No kernel changes, no new protocol methods, no ACP adapter (deferred, §4.2), no Marketplace
  packaging.

## 1. Problem

The TUI works and is the dogfooding surface (ADR-0009 §4). What is missing is the surface where the
work actually happens: the editor. Using loongagent while editing code today means alt-tabbing to a
terminal, and every piece of context the editor already knows — the workspace root, the open file,
the selection — is retyped by hand. `docs/04-system-overview.md` states the direction ("All
surfaces, one brain. TUI, web, and IDE (via ACP) are thin clients of a single local agent server")
and ADR-0009 put ACP third, after TUI and web.

Three facts shape this design:

1. **The wire already exists and is already used by two surfaces.** `loongagent server --stdio`
   speaks newline-delimited JSON-RPC 2.0; the TUI (`loong_tui::Client`) and the web surface
   (`crates/web`) are both clients of it. `clients/ts/protocol.generated.ts` is generated from
   `crates/schema/protocol.toml`, so the TypeScript side of the wire is already typed and kept in
   step by `scripts/gen-schema.py`.
2. **VS Code is not an ACP client.** Nothing in VS Code speaks ACP, so *some* extension has to be
   written either way. ACP would add a Rust adapter (the agent's side) without removing the
   extension — see §4.2 for what it buys and why it is deferred.
3. **The stable chat API is narrower than Copilot Chat's.** `ChatResponseStream` has exactly seven
   methods in stable VS Code (`markdown`, `anchor`, `button`, `filetree`, `progress`, `reference`,
   `push`). Tool-call cards, diff cards, and inline confirmation parts are **proposed-only**
   (`chatParticipantAdditions`). §4.3 is therefore explicit about how each Copilot-Chat affordance
   is expressed without them.

## 2. Goals and success criteria

Goal: the owner can do their daily loongagent work inside VS Code, in the native Chat view, without
leaving the editor and without a second implementation of the agent.

| # | Criterion | How it is measured |
|---|---|---|
| 1 | A prompt typed at `@loong` reaches the real kernel and streams back | Markdown appears incrementally while the turn runs, matching the same session's TUI rendering |
| 2 | An approval is a decision made in VS Code and recorded in the log | The file is written only after the human answers; `approval.decided` carries the scope that was actually chosen |
| 3 | A turn's file changes are reviewable in one action | Every path the turn touched opens in a `vscode.diff` with its pre-turn content on the left |
| 4 | Sessions survive a window reload | Reload and the same session is re-attached; its last messages are visible, not an empty chat |
| 5 | The extension contains no agent logic | No routing, no context assembly, no policy evaluation; a grep for a model choice or a verdict never hits extension code (ADR-0009 §2) |
| 6 | The mapping is testable without VS Code | `agentSession.ts` and `render.ts` have no `vscode` import and are covered by `node --test` |
| 7 | Nothing about the kernel's guarantees is weakened | Sandbox, approval rules, and the event log behave identically to the TUI; the extension can neither widen nor bypass them |

## 3. Non-goals

- **Publishing.** Personal use, run from source with F5 / `--extensionDevelopmentPath`. No `vsce`
  packaging, no Marketplace, no publisher identity.
- **Proposed APIs.** No `enabledApiProposals`, so no `--enable-proposed-api` launch flag and no
  breakage on VS Code upgrade. The escape hatch if the UX proves too flat is recorded in §8.
- **The ACP adapter.** Designed in §4.2, built in a later phase, not here.
- **Review-before-apply.** The kernel writes files through its own tools, under its own sandbox and
  its own approval rules (ADR-0007: the kernel decides, the OS enforces). Making the extension
  apply edits instead would move a guarantee out of the kernel, which ADR-0009 §2/§3 forbid. The
  extension reviews **after** the fact; `git` is the undo.
- **Inlining referenced files.** `#file` references are forwarded as paths; the kernel's own read
  tools fetch the content. Inlining bodies into the prompt would duplicate context assembly in a
  surface.
- **Windows cancellation.** Cancel is a process signal (§4.6); the TUI's spelling of it
  (`CREATE_NEW_PROCESS_GROUP` + `CTRL_BREAK_EVENT`) has no reliable Node equivalent. Linux/macOS
  first; Windows is documented as "no cancel" rather than silently broken.
- **Multi-window session sharing.** One window, one session (§4.6). Observers and the write token
  (ADR-0009 §5) are not exercised.

## 4. Design

### 4.1 Topology — a third client of the same wire

```
VS Code window
  └── extension host (Node)
        ├── extension.ts      VS Code glue: chat participant, commands, modals, diff provider
        ├── agentSession.ts   transport + protocol state machine  (no vscode import)
        └── render.ts         EventEnvelope → UI parts, pure     (no vscode import)
              │  newline-delimited JSON-RPC 2.0 over stdin/stdout
              ▼
        loongagent server --stdio --cwd <workspaceFolder>
              │  the kernel: model loop, sandbox, approvals, JSONL log
              ▼
        ~/.loongagent/sessions/<id>.jsonl   ← still the only source of truth
```

The extension spawns the child itself (`child_process.spawn`, cwd = the workspace folder, never the
extension host's cwd, which is not documented to be the workspace root). It is a client of the same
wire as the TUI and the browser, and it holds no state the log does not have.

Methods used, all existing: `session.attach`, `session.new`, `session.replay`, `prompt.submit`,
`approval.respond`, `permissions.set`, `model.list`, `model.select`, `context.explain`.
Notifications consumed: `model.delta` (`{turn_id, text}`) and `event` (`EventEnvelope`).

One doc-only schema change: `SessionAttachParams.client`'s doc string enumerates `tui | web | acp`;
the extension passes `"vscode"` and `crates/schema/protocol.toml` is widened to say so. No field,
type, or wire change — `scripts/gen-schema.py` regenerates the same shape with a truer comment.

### 4.2 Why our own protocol now, and what ACP would be later

ACP is JSON-RPC 2.0 over stdio with the **editor as the client and the agent as a subprocess**;
`initialize` negotiates a `protocolVersion` (v1 is stable, v2 is draft), then `session/new` →
`session/prompt` → a stream of `session/update` notifications, with approvals travelling the other
way as `session/request_permission`
([overview](https://agentclientprotocol.com/protocol/v1/overview),
[transports](https://agentclientprotocol.com/protocol/v1/transports),
[prompt turn](https://agentclientprotocol.com/protocol/v1/prompt-turn)).

The mapping onto what the kernel already emits is direct, which is exactly why the adapter is thin:

| loongagent today | ACP |
|---|---|
| `session.attach` / `session.new` | `session/new` |
| `prompt.submit` | `session/prompt` |
| `model.delta` | `session/update` → `agent_message_chunk` |
| `message.appended` (assistant) | the message's final chunk |
| `tool.call` / `tool.result` | `session/update` → `tool_call` / `tool_call_update` |
| `approval.requested` + `approval.respond` | agent→client `session/request_permission` |
| `context.explain`, `model.list`, `model.select` | no ACP equivalent → `_loongagent/*` methods, or absent |
| Ctrl-C / `SIGINT` (`catch_interrupts`, `use_interruptor`) | `session/cancel` — **needs a real cancel entry point; today it is a signal, not a message** |

Deferred, because VS Code buys nothing from it: VS Code has no ACP client, so the extension is
written either way, and ACP only adds the adapter. It buys the *other* editors (Zed, JetBrains,
Neovim) and, notably, the off-the-shelf VS Code client
[`formulahendry.acp-client`](https://github.com/formulahendry/vscode-acp) — once `loongagent acp`
exists, that extension can drive loongagent with no code from us. When the phase comes:
`crates/acp` + a `loongagent acp` CLI arm, `agent-client-protocol = { version = "3", features =
["stdio"] }` on the server side, a second implementation behind `agentSession.ts`'s interface, and
one new kernel-facing capability (a cancel entry point) that the signal path cannot express.

### 4.3 The UI — stable Chat Participant, and how each affordance is expressed

Contribution: `chatParticipants` (id `loongagent.chat`, `name: "loong"`), `engines.vscode:
^1.138.0` (the installed version), registered with `vscode.chat.createChatParticipant`. The
participant handler receives `(request, context, stream, token)`; `token` is the Stop button.

Because stable has no tool-call, diff, or confirmation parts, each Copilot-Chat affordance gets an
honest stable expression:

| Copilot-Chat affordance | How it is expressed here |
|---|---|
| Streaming answer | `stream.markdown(chunk)` per `model.delta` |
| Tool call in progress | `stream.progress("<tool> — <action_class>")`, then a markdown line per `tool.call`, mirroring the TUI's `policy: allow by …` line |
| Tool result | `stream.markdown` with the output fenced and truncated to a fold, like the TUI's folded output |
| Approval prompt | `window.showWarningMessage(reason, { modal: true }, "Allow once", "Allow always", "Deny")`, answered inside the same request handler |
| Approval recorded | A markdown line per `approval.decided`: granted/denied and the scope |
| Changed files | Paths are harvested from the turn's `tool.call` args; a `stream.button` opens a `vscode.diff` per file (left side = pre-turn content, via a `TextDocumentContentProvider` under a private scheme) |
| Model | `/model` → `model.list` → `window.showQuickPick` → `model.select`; the current model is shown in the response header line |
| Context | `/context` → `context.explain` rendered as a small markdown table |
| New / resumed session | `/new` → `session.new`; on activation, attach and `session.replay` (§4.6) |
| Prompt relaxation | `/yolo` → `permissions.set`, with the resulting `permissions.relaxed` event rendered (§4.4) |
| Slash-command discovery | `commands[]` in the contribution, plus `followupProvider` for "Review changes" / "Explain" |
| Stop | `token.onCancellationRequested` → signal the child (§4.6) |
| `#file` references | `request.references` values are appended to the prompt as absolute paths |
| Rule-based decisions (`policy.decided`) | Rendered like the TUI's line, so "allowed by rule X, not by me" stays visible |

The rendering rules live in `render.ts` as pure functions over `EventEnvelope`, in the same spirit
as `loong_tui::render_event`: the extension and the TUI should not disagree about what
`tool.result` means.

### 4.4 Approvals

The kernel pauses *inside* a turn: `prompt.submit` returns at each approval boundary, having
streamed `approval.requested` first, and the turn resumes on `approval.respond`. The extension must
therefore:

- keep the participant's `ChatResponseStream` open across the pause (the handler awaits the modal,
  not the turn's end);
- track the pending `request_id` from the events, exactly as `loong_tui::Client::pending_approval`
  does, because the approval arrives as an event and the turn "ends" several times per turn;
- map the three buttons onto the real scopes: **Allow once** → `once`, **Allow always** →
  `action_class`, **Deny** → `granted: false`, `scope: once` and a fixed reason string ("denied in
  VS Code") so the model is told why (ADR-0007 §6) — a modal cannot collect free text, and a fixed
  honest sentence beats a fabricated one;
- show `permissions.relaxed` if `/yolo` is ever used, and default `loongagent.autoApprove` to
  false. Relaxing prompts is a session event that the interface must show (ADR-0007 §5); the
  extension never sets it silently.

The `project` scope exists in the protocol and is not offered by the modal: three buttons is already
the limit of what a modal can carry honestly. That is an exposure gap, and §8 records it.

### 4.5 Changed files and diff

The extension does not compute edits; it reports what happened.

- During a turn, `tool.call` events are collected. A call whose args name a path (the built-in write
  tools do) contributes to the turn's touched-file set; the args are opaque JSON, so the harvest is
  "look for absolute-looking path-shaped strings", and a miss degrades to a less complete diff list,
  never to a wrong claim.
- The left side of the diff is the file's content **at the moment its path was first mentioned**,
  read then and kept in memory. The *timing* is exact: `tool.call` is recorded *before* the tool
  runs (and before it is approved), so for any path the first mention precedes its first write, and
  unlike `git show HEAD:<path>` the snapshot stays correct when the tree was already dirty before
  the turn. The *harvest* is not exact — see the bullet above — so the diff list can be incomplete.
  Verified in the kernel: `dispatch` appends `tool.call` before the hooks, the verdict and the
  execution (`crates/kernel/src/turn/loop.rs`), and `append` streams each event to the live sink at
  the moment it is written (`crates/kernel/src/turn.rs:377-395`).
- A path first seen after the extension attached to an in-flight session (no snapshot) falls back to
  `git show HEAD:<path>` through a `TextDocumentContentProvider`; a file with no HEAD version
  renders as an empty left side. The transcript names which source produced the left side instead of
  implying it is always the pre-turn content.
- At the end of a turn, one `stream.button` ("Review N changed files") runs a command that opens
  `vscode.diff` per file. Per-hunk accept/reject is not available on stable APIs (no extension API
  exposes it), so there is none — the diff is for reading, and `git` is for undoing.

### 4.6 Sessions, workspace, lifecycle

- **One child, one session, per workspace folder.** Session id is remembered in
  `context.workspaceState`; on activation the extension spawns the child with `--cwd <folder>` and
  either `--session <id>` (resume) or `--new`.
- **Replay.** After attach, `session.replay` returns the log. The extension renders a compact
  "resumed session <id> · N events" card plus the last few messages as markdown on the first turn,
  so a reopened chat is not an empty chat. It does not try to reconstruct VS Code chat history
  objects — chat history belongs to the window, the session belongs to the log.
- **The session id is a shared invariant.** The server refuses any request naming a session it is
  not on (`wrong_session`), so every request carries the id the extension last saw from
  `session.new`/`session.attach`; a refusal is handled by re-attaching, never by retrying blindly.
- **Stop.** `token.onCancellationRequested` sends `SIGINT` to the child on POSIX (`child.kill`
  delivers the same signal the TUI sends). The turn's own stream reports the outcome
  (`turn.interrupted`), and the child survives — Ctrl-C stops the turn, not the server. On Windows
  this is not implemented (§3).
- **Teardown.** The child is killed in `deactivate()` and when the last workspace folder closes; a
  child that exits on its own is reported in the `loongagent` output channel and restarted lazily on
  the next request, with the exit code shown.
- **Lifecycle states** exposed to the user: spawning, ready, turn running, awaiting approval,
  stopped.

### 4.7 Trust, sandbox, and what the extension must not do

- `capabilities.untrustedWorkspaces: { supported: "limited", description: "The agent operates on
  this workspace and runs its own tools." }`, and the spawn is gated on `workspace.isTrusted`.
  The kernel's sandbox and approval rules are the real guarantee; VS Code's trust state is a second
  gate in front of them, not a replacement.
- The extension makes **no** policy decision. It relays a human's answer; it never turns a verdict
  into an allow. Every line it shows about a decision comes from `policy.decided`,
  `approval.requested`, or `approval.decided` — never from its own inference.
- It writes nothing into the workspace. Its only writes are to `context.workspaceState` (a session
  id) and the `loongagent` output channel.
- Configuration surface: `loongagent.path` (binary; defaults to `loongagent` on `PATH`),
  `loongagent.model` (optional `--model`), `loongagent.autoApprove` (default false),
  `loongagent.extraArgs`.

### 4.8 Project layout and build

```
clients/vscode/
  package.json          contribution points, scripts, engines ^1.138.0
  tsconfig.json         includes ../ts/protocol.generated.ts
  esbuild.mjs           bundle → dist/extension.js (external: vscode)
  src/extension.ts      activation, participant, commands, diff provider, settings
  src/agentSession.ts   spawn, framing, pending-request map, notification dispatch, session id
  src/render.ts         pure EventEnvelope → UI parts
  test/*.test.ts        node --test over agentSession + render with a fake child
  .vscode/launch.json   Extension Development Host (F5)
```

`clients/ts/protocol.generated.ts` is imported directly rather than copied, so there is exactly one
generated wire description; `scripts/gen-schema.py` is untouched. `tsc --noEmit` joins
`scripts/check.sh` once the extension exists (it is a JS project, so it cannot join the cargo
layering check; the layering rule is preserved by the extension having no Rust dependency at all).

### 4.9 Errors

- Malformed line from the child → logged with the raw line to the `loongagent` output channel;
  the transport keeps reading (the log is the record, the UI is a view).
- Child dies mid-turn → the request handler fails with the exit code; the transcript says so; the
  next request respawns.
- Binary not found → an actionable message naming `loongagent.path`, not a stack trace.
- A response that never arrives → the pending-request map is keyed by id with a timeout, and the
  timeout is reported as a stalled turn rather than a hung chat.

## 5. Verification

| Step | Verification |
|---|---|
| 0 | From the extension's output channel: an attached session id, a streamed answer to one prompt, and at least one `tool.call` line. Cross-checked against the same session's JSONL. |
| 1 | Reload the window; the same session id is re-attached and history is visible. `node --test` green over the transport and the render mapping (fake child, no VS Code). |
| 2 | A file write blocks on the modal; denying leaves the file byte-identical and records `approval.decided {granted:false}`; allowing writes it and records the scope that was chosen. |
| 3 | A prompt that touches ≥2 files yields a "Review N changed files" button; each diff's left side equals the content the file held when its path was first seen in `tool.call` (checked against a copy taken before the turn). |
| 4 | `tsc --noEmit` clean in `scripts/check.sh`; `/model`, `/context`, `/new` each change observable state (log event or response header). Manual: a working afternoon without a restart. |

Each step lands as its own commit; a step is not done until its row is demonstrated, not asserted.

## 6. Sequencing

| Step | Deliverable | Terminal state |
|---|---|---|
| **0** | Link proven: spawn, attach, prompt, stream into the output channel | Protocol, cwd, binary path and approval stalls are known instead of assumed |
| **1** | `@loong` participant: streaming markdown, `/new`, replay on activate, child lifecycle, `agentSession.ts` + `render.ts` under `node --test` | Daily-drivable for read-only questions |
| **2** | Approvals: modal → scope mapping, `/yolo` with a visible `permissions.relaxed`, `approval.decided` lines | The owner can let it write files from VS Code |
| **3** | Tool-call rendering and the changed-files diff review | The Copilot-Chat loop is complete enough to trust |
| **4** | `/model`, `/context`, settings, error paths, `tsc --noEmit` in `check.sh` | Personal daily-use threshold |
| **5** *(later, not this spec)* | `crates/acp` + `loongagent acp`; `agentSession.ts` gains an ACP implementation | Zed/JetBrains/Neovim, and `formulahendry.acp-client` works as-is |

## 7. Risks

| Risk | Why it bites | Mitigation |
|---|---|---|
| The stable participant UX feels flat next to Copilot Chat | No tool-call cards, no diff cards, no inline confirmation — those are proposed-only | Markdown + progress + buttons + modal is the honest ceiling; §8 records the escalation if it is not enough |
| VS Code 1.138 (installed) vs 1.142 (current) | The chat panel keeps moving; agent sessions are the direction | `engines: ^1.138.0`, stable APIs only, no proposals; verify each step on the installed version |
| Orphaned server processes | A window reload or crash can leave a child behind | Kill in `deactivate()`, `onDidDispose`, and process exit; report unexpected exits |
| The turn/approval pause is misread as "turn over" | `prompt.submit` returns at every approval boundary | Pending-approval tracking from events, mirroring `loong_tui::Client`; unit-tested |
| A wrong or stale session id | The server refuses `wrong_session` rather than corrupting a log | Every request carries the last known id; a refusal re-attaches |
| Path harvesting misses a changed file | `tool.call` args are opaque JSON | Degrade to a less complete list; never claim completeness |

## 8. Decisions left to the human

1. **If step 3's UX is not enough** (most likely complaint: tool calls and diffs are prose rather
   than cards), the fork is: (a) add the `chatParticipantAdditions` proposal and accept
   `--enable-proposed-api` plus upgrade churn; (b) move the chat into a `secondarySidebar` webview
   view (stable since 1.106, the Cline/Roo/Kilo architecture) and keep the native chat view as the
   `@loong` entry point; (c) live with it. Deferred to after step 3, on evidence.
2. **`project`-scope approvals** are in the protocol but not offered by a three-button modal. Either
   a fourth button or a follow-up quick pick, when it starts to matter.
3. **`#file` content inlining** (today: paths only). Revisit if the kernel's reads prove too coarse.
4. **Whether the extension should also register `languageModelTools`** so VS Code's own agent can
   call loongagent's tools. Interesting, and explicitly out of scope here.
