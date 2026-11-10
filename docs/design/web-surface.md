# The web surface

- **Up:** [design/README.md](README.md) · [design/surfaces.md](surfaces.md)
- **Authority:** [ADR-0009](../decisions/0009-surfaces.md) · [ADR-0002](../decisions/0002-json-rpc-wire-protocol.md)
- **Code:** `crates/web/src/lib.rs`, `crates/web/src/app.rs`, `crates/web/src/sessions.rs`, `crates/web/src/index.html`, `crates/web/src/app.js`, `crates/web/src/style.css`, `crates/cli/src/web.rs`
- **Verify:** `cargo test -p loong-web` · `./scripts/check.sh`

The web surface is one thin client more, under ADR-0009: the browser renders and
sends intents; `loongagent server --stdio` children own the sessions. Unlike the
TUI it is **multi-window**: several browser tabs sit on one HTTP listener, and
the surface itself owns what the TUI never needed — a catalogue of workspaces
and sessions, and a fan-out that shares one server's event stream between tabs.

The surface holds **no session of its own**. `loongagent web` starts the HTTP
listener and registers one workspace (its launch directory); it does not open a
session. A session comes into being only when the user creates one or picks one
from the rail — that is the action that spawns a server child. There is no
eager "default" session waiting to collide with the user's first choice.

## 1. Workspaces — many, remembered

A **workspace** is a directory the user works in. `loongagent web` registers its
launch directory as the **home workspace**; the page opens on it. The banner's
directory button opens a picker (server-side directory listing, directories
only); choosing a directory **adds** a workspace.

Workspaces are **many and persistent**, not one extra per browser. The catalogue
of workspaces lives in a small file under the config dir (`workspaces.json`),
so the list survives a restart of `loongagent web`. Each entry records the
directory path and a display title (defaulting to the final path segment).
Removing a workspace from the list never deletes its directory or its session
history — the sessions simply stop being grouped under it; the files stay on
disk and remain openable as ungrouped history.

The rail lists workspaces **newest added first**: the one just added by the
picker sits at the top, and the home workspace — registered first, so the oldest
— sits at the bottom. The order is display only. Every `index` travels with its
entry and is still what the calls take, so reordering the rail never changes
which workspace a click means; the page looks a workspace up by its `index`, not
by its position.

A workspace is identified by its canonicalised path (`realpath`, with symlinks,
`..` and trailing slashes resolved), so two spellings of the same directory do
not become two workspaces.

## 2. The two-level tree — workspace, then its sessions

The left rail is a **two-level tree**: workspaces on the first level, and under
each workspace the sessions that belong to it on the second. A session belongs
to the workspace whose canonicalised path matches the directory the session's
`.jsonl` lives in — membership is read off the files, never a separate record.

The catalogue (`sessions.rs`) mirrors what `crates/cli/src/session.rs` reports
for a directory: id, title, created, modified. New, resume and delete are
surface operations, and none of them reuses a sibling session's server child
(§3):

- **New** — spawn a server child for a fresh session in that workspace, then
  `session.new` on it. Because the child starts on the new session, it answers
  `session.attach` with the real id; the surface never has to guess it.
- **Resume** — spawn (or reuse) a server child bound to that session with
  `--session <id>`, then attach. The surface never forwards a bare
  `session.open` across sessions on a shared child, so the child's
  single-session guard (`wrong_session`) is never tripped from the page.
- **Delete** — refuse while the session is attached anywhere; otherwise send
  `session.detach`, drop the child, and remove `dir/<id>.jsonl` plus
  `dir/<id>.index.bin`. The index is derived (sessions-lineage §6), so deleting
  the pair is complete.

Deleting removes files. There is no trash; the UI says so on the button.

## 3. One child per session — concurrent, never killed on switch

The surface keys its server children by **(workspace, session)**, not by
workspace. Each session that is open gets its own `loongagent server --stdio`
child, bound to that session with `--session <id>` (`ChildTransport::spawn`
reads the session out of the launch `Args`). Two sessions in the same workspace
therefore run as two children, **side by side and alive at the same time**.

This is what makes the underlying server's single-session guard irrelevant to
the page. A `loongagent server` child holds exactly one `session_id` and rejects
any request that names another (`wrong_session`). The surface never asks one
child to change sessions: to switch sessions it spawns a child for the target
and attaches there, leaving the previous session's child untouched. Switching
the rail selection **does not kill a child**, so a turn running in one session
keeps running while the user reads another — the property the per-workspace
child could not give.

A child is reclaimed only when its session is deleted, or when `loongagent web`
exits. There is no idle reaper in v1; the cost is one child per open session,
bounded by what the user actually has open.

One browser tab holds one WebSocket; the tab's active session is its
**attachment**. A session's child is shared across tabs: every tab attached to
that session receives the same events and deltas (`app.rs` keeps one broadcast
queue per session child; `handle_line_streaming` already interleaves
notifications with responses). Two tabs on the *same* session both see the turn;
a prompt from either is one prompt to the kernel.

Attaching replays the log (`session.replay`) so a late tab catches up; live
events then continue the same stream. The tab computes the numbers the TUI
computes — the spend accumulator of `docs/design/cost-accounting.md` §4, in
JavaScript — from `cost.recorded` events, both replayed and live.

## 4. The page

Three regions, all fed by the event stream:

- **Banner** — favicon and wordmark, the workspace button, then the model picker:
  a dropdown listing every `model.list` entry, the active one selected; choosing
  one sends `model.select` (the TUI's picker, ADR-0008, as a control) and folds the
  returned `ModelList` back into the dropdown. A running turn disables the picker
  until the turn ends.
- **Session rail** — the two-level tree of §2: workspaces newest added first, each
  expanding to its sessions **newest started first**, with new/delete; clicking a
  session attaches to it. A session is ordered by when it was created — the first
  `time_ms` in its log; a brand-new session with no event yet takes the log file's
  own time, so it appears at the top the moment it is made. Creation time cannot
  move afterwards, so the rail never reshuffles: neither opening a session (which
  writes the session-start records `skills.loaded`, `model.selected`,
  `profile.selected`, and touches the file) nor talking in an old one lifts it. The
  "2m ago" label reads the conversation clock — the latest conversational `time_ms`,
  those start records excluded — so it says when the session was last spoken in
  rather than when its file was last written.
- **Chat** — the transcript, and above the composer a **process panel**: the
  live turn as it happens. Text deltas append to the draft assistant message;
  `tool.called`/`decision.*`/`tool.result` render as step cards with the tool's
  output collapsible; `approval.requested` renders an approve/refuse pair that
  answers `approval.respond`. The point is legibility of the model's process:
  the framework's steps and the model's words are both visible as they stream.

The composer shows the TUI's fixed usage row — 入/出 tokens, calls, cache rate,
context share — recomputed from `cost.recorded` after every event.

## 5. Wire additions

No new session protocol methods: the surface speaks the same JSON-RPC the TUI
speaks, over WebSocket instead of stdio. The web crate adds HTTP routes for its
static assets (`app.js`, `style.css`, `/favicon.ico` from the bundled
`assets/favicon.png`) and one surface-local JSON route, `/api/browse`, which
lists subdirectories for the workspace picker; it belongs to the control plane
and stays in the web crate. The workspace catalogue of §1 is driven over the
socket (`workspace.list`/`add`/`close`) and persisted internally to
`workspaces.json` — there is no `/api/workspaces` HTTP route. One surface-local
socket method, `session.tree`, answers the two-level rail in one call: every
workspace with its sessions nested, each flagged live/busy.

One launch flag changes meaning: `loongagent web` no longer takes `--session`
or `--new` to pre-select a session, because the surface now starts empty (the
preamble). The launch directory is the only thing `loongagent web` fixes at
startup; everything session-shaped is chosen in the page.
