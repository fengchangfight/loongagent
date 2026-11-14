# The web surface

- **Up:** [design/README.md](README.md) · [design/surfaces.md](surfaces.md)
- **Authority:** [ADR-0009](../decisions/0009-surfaces.md) · [ADR-0002](../decisions/0002-json-rpc-wire-protocol.md)
- **Code:** `crates/web/src/lib.rs`, `crates/web/src/app.rs`, `crates/web/src/sessions.rs`, `crates/web/src/index.html`, `crates/web/src/app.js`, `crates/web/src/style.css`, `crates/cli/src/web.rs`
- **Verify:** `cargo test -p loong-web` · `./scripts/check.sh`

The web surface is one thin client more, under ADR-0009: the browser renders and sends
intents; `loongagent server --stdio` children own the sessions. Unlike the TUI it is
**multi-window**: several tabs sit on one HTTP listener, and the surface owns what the TUI
never needed — a catalogue of workspaces and sessions, and a fan-out that shares one
server's event stream between tabs.

The surface holds **no session of its own**. `loongagent web` starts the listener and
registers one workspace (its launch directory); a session exists once the user creates or
picks one, which is the action that spawns a child. No eager "default" session waits to
collide with the user's first choice.

## 1. Workspaces — many, remembered

A **workspace** is a directory the user works in. `loongagent web` registers its launch
directory as the **home workspace**, and the page opens on it. The rail's **add-workspace
button** (its head, §4) opens a picker (server-side listing, directories only); choosing one
**adds** a workspace. One control for one list: it names what it does — `＋ 添加工作区` — not
the seated workspace, which the tree below already shows.

Workspaces are **many and persistent**, not one extra per browser: the catalogue lives in
`workspaces.json` under the config dir, so it survives a restart. Each entry records the
directory path and a display title (the final path segment). Removing one deletes nothing:
its sessions fall out of the grouping and stay openable as ungrouped history.

The rail lists workspaces **newest added first**: the one just added by the picker on top,
the home workspace — registered first, so oldest — at the bottom. Display only: every
`index` travels with its entry and is what the calls take, so the page looks a workspace up
by its `index`, never by its position. Identity is the canonicalised path (`realpath`), so
two spellings of one directory stay one workspace.

## 2. The two-level tree — workspace, then its sessions

The left rail is a **two-level tree**: workspaces, and under each its sessions. A session
belongs to the workspace whose canonicalised path matches the directory its `.jsonl` lives
in — membership is read off the files, never a record.

The catalogue (`sessions.rs`) mirrors `crates/cli/src/session.rs`: id, title, created,
modified. New, resume and delete are surface operations, none reusing another session's
server child (§3):

- **New** — spawn a server child for a fresh session in that workspace, then `session.new`
  on it, whose optional `workspace` index names the row the `＋` was clicked in (§4) and
  moves the tab there. The child starts on the new session and answers with its real id.
- **Resume** — spawn (or reuse) a server child bound to that session with
  `--session <id>`, then attach. The surface never forwards a bare `session.open` across
  sessions on a shared child, so the child's single-session guard (`wrong_session`) is
  never tripped from the page.
- **Delete** — refuse while the session is attached anywhere; otherwise send
  `session.detach`, drop the child, and remove `dir/<id>.jsonl` plus
  `dir/<id>.index.bin`, the index being derived (sessions-lineage §6).

Deleting removes files: no trash, and the button says so.

## 3. One child per session — concurrent, never killed on switch

The surface keys its server children by **(workspace, session)**, not by workspace. Each
open session gets its own `loongagent server --stdio` child, bound to it with
`--session <id>` (`ChildTransport::spawn` reads the session out of the launch `Args`), and
that child is the file `loongagent web` is **running**, not the path it was launched as, so
a rebuild under a live portal cannot strand the next session. Two sessions in one
workspace run as two children, **side by side and alive at the same time**.

This makes the server's single-session guard irrelevant: a child holds one `session_id`
and rejects any other (`wrong_session`), and the surface never asks it to switch — it
spawns the target's child and attaches there. Switching the rail selection **does not kill
a child**, so a turn keeps running while the user reads another. A child is reclaimed only
when its session is deleted or `loongagent web` exits: no idle reaper in v1.

One browser tab holds one WebSocket; the tab's active session is its **attachment**. A
session's child is shared across tabs, so two tabs on the *same* session see one turn and
a prompt from either is one prompt to the kernel. The turn's lines go to the tab that
asked; the other tab catches up by replaying the log, which is where they are written.

The tab's socket is **read and written by one thread**, and no turn holds it: a turn runs on
a thread of its own and hands its lines to the tab's outbox, which the read loop writes
between browser messages — it times the read out every 20 ms so a quiet turn still gets its
lines out. A turn that owned the socket is what used to stop the page: the loop sat inside
`exchange` until the turn ended, so `workspace.add` and `session.open` waited unread. Every
forwarded line names its session (`session_id`) and the page draws only its own, so a turn
left behind cannot paint into the session that replaced it.

Attaching replays the log (`session.replay`) so a late tab catches up, then live events
continue the stream. The surface answers that replay from the log file itself, never through
the session's child: a child answers only when its turn ends, and the moment a reader most
wants the past is the moment the session is busy writing the present. Every event carries
its log `seq` and the page drops one it has already drawn — a line replayed and the same
line arriving live are one event (§5). The tab computes what the TUI computes — the spend
accumulator of `docs/design/cost-accounting.md` §4, in JavaScript — from `cost.recorded`.

## 4. The page

The page speaks **Chinese**: its chrome — buttons, empty states, toasts, chips, panels,
pickers — renders user-facing text in Chinese, while identifiers that name things outside
the page (model names, paths, the wordmark, `YOLO`) stay as they are.

Three regions, all fed by the event stream:

- **Banner** — favicon, the wordmark in capitals, and the terminal's own inscription
  `⟬ ᛭ 兴云吐雾 ⋄ 隐介藏形 ᛭ ⟭` (`surfaces.md` §3, the TUI's `MARK_SUBLINE`), so the two
  surfaces introduce themselves alike; it sheds runes, then itself, as the window narrows.
  Then the **model bar**: a flat row of slots, one per `model.list` entry, every model
  drawn at once — the TUI's own bar (ADR-0008, `surfaces.md` §3), not a dropdown, because
  reaching for another model is usually mid-sentence and a menu costs a click and a read.
  Choosing a slot sends `model.select` and folds the returned `ModelList` back into the
  row. The selected model is its lit slot and never a hue alone: a filled dot, an accent
  fill, a double border and a bold name, with `aria-current` for a reader that cannot see
  the colour — said there once, and derived from `ModelList.selected`, never from a field
  on an offering. The keys are `alt+n` / `alt+p`
  and `ctrl+←` / `ctrl+→`, printed on the bar rather than hidden, and they are **two
  different acts**: the letters switch model, wrapping; the arrows **move the current
  model one slot** (`surfaces.md` §3) — the lit slot stays lit while the row rearranges
  around it, so the arrows send `model.reorder`, never `model.select`.

  Which slot holds what the session cannot answer: its child read the profile at startup
  and keeps answering `model.list` in *declaration* order, so the surface remembers the
  arrangement per tab. `model.reorder {session_id, delta}` takes the bar's order as its
  base and swaps one slot, answering the reordered `ModelList` plus `moved {name, from,
  to}` and `persisted`; every later `model.list` is laid over the same order, so a refresh
  does not snap back. A model added since appears last, one removed has no match, and an
  edge is an answer, not a wrap (`alpha is already in the last slot`).

  `persisted` is where the modes differ, said rather than implied. On a local profile the
  order goes back into `local.toml`'s `[[models]]` blocks, so later sessions start on the
  new first block — hence the move is also said in the transcript, like the TUI's. On a
  managed run, or with `--profile`, no file is this run's to rewrite: `persisted: false`,
  arrangement on the bar. A failed write does not unmake the move — the row stays arranged
  and the refusal returns in `warning` — and `model.add` drops the arrangement.

  The chord split follows what the browser claims: `Ctrl+N` and `Ctrl+P` are its own,
  `Alt+←/→` are navigation — so the letters sit on Alt, the arrows on Ctrl. A running turn
  disables the slots until it ends; the row's right end carries **＋ Add model**, the panel
  of §6 beside the row it extends.
- **YOLO switch** — the banner's right edge shows this session's approval state as a
  pill that says itself in words: `YOLO 关` quiet and outlined while prompts are on,
  `YOLO 开` filled red while the session lets every tool call through — red because a
  turn that never asks is the dangerous state, and the page must not make it look calm.
  It draws what the session says, never its own guess: `permissions.relaxed` is the record
  and `permissions.set`'s answer the confirmation, so an old session shows the opt-out it
  really has (ADR-0007 §5, ADR-0020). A turn owns the session's bridge, so the flip cannot
  ride it: the surface reaches a running child over ADR-0019's control channel — the same
  private descriptor 3 the TUI uses — and answers at once with `queued: false` and when it
  lands, never as if it had already landed. A platform with no channel forwards the
  request, and the flip lands when the turn ends.
- **Session rail** — the two-level tree of §2: workspaces newest added first, each
  expanding to its sessions **newest started first**, with new/delete/copy. Its head is the
  **add-workspace button** of §1, which opens the picker. Every workspace row also carries
  its own **＋ 新会话**, so the row is the target and the page says where a session goes by
  where the button sits; clicking it moves the chat there as it starts. Copy puts the id on
  the clipboard, because the id names the session to every other surface (`loongagent
  --resume <id>`) while the row shows a title. Order is creation time — the first `time_ms`
  in the log, or the file's own time for a session with no event yet, so it appears at the
  top the moment it is made — and it never moves afterwards: the rail does not reshuffle,
  not when a session is opened (which writes its start records) nor when an old one is
  talked in. The "几分钟前" label reads the latest conversational `time_ms`, those start
  records excluded. Switching is never refused: a working session keeps working off
  screen, and running belongs to the session on screen, not to the page.
- **Chat** — the transcript, drawn from the session log and nothing else: the user's
  own message arrives as a `message.appended` with `role: "user"` like any other
  event, so drawing it at send time would show it twice. A turn is one block holding
  its machinery and its words **in the order they happened** — tool calls, policy
  decisions and the model's own sentences interleaved. The card and its spinner appear
  when the prompt is **sent**, not at the first event: a model that thinks for ten
  seconds must not look like a stall. A turn stays open when it ends — folding it away
  hid the words the reader was waiting for — so only a **replayed** turn folds shut, and a
  resumed session reads as a stack of summaries, unless it still holds a question nobody
  has answered, whose buttons are its live content. A user message closes the turn before
  it — except the one opened for the prompt being answered, which is the turn it belongs
  to and where it is placed above.

  A turn is opened by a prompt in flight or by a replay, never by an event that merely
  arrives. A line that reaches the page after its turn closed — a trailing
  `compaction.applied` — joins the turn just closed instead of opening a new card, which
  would have no prompt left to close it and would leave a `Working…` spinner turning.

  Inside a turn:
  - `model.delta` appends to the streaming draft: every delta lands in the *same*
    bubble, so a long reply grows in place. The finished `message.appended` renders
    once, properly — fenced blocks become code with their language tagged, `inline`
    becomes `<code>`, `**this**` bold, blank lines paragraphs. Every node is built,
    never parsed from output into markup, so a reply cannot inject HTML.
  - `tool.call` is a card: name, arguments inline on the summary line and in full
    underneath. `tool.result` attaches to the card whose `call_id` it carries —
    arguments and answer stay one card — showing `ok` or `failed`, the first lines,
    and the whole output one click away.
  - `policy.decided` is the most numerous event by far, so it is a one-line
    chip: the verdict (`allow` quiet, `ask` amber, `deny` red), the action
    class and the rule.
  - `approval.requested` renders three answers — once, for the session
    (`scope: "session"`, remembered so the same shape does not ask again) and
    refuse — answering `approval.respond` with the event's **`data.request_id`**.
    Three states, three doors: `approval.decided` greys it (a click, a replay, another
    tab), a replay draws it **held**, and `pending_approval` alone opens it — the rest are
    retired with a line about the process that is gone (sessions-lineage §5).
  - `skill.invoked`, `compaction.applied`, `context.degraded`, `approval.decided` and
    `turn.interrupted` are chips carrying their own facts.
  - Session-start bookkeeping (`skills.loaded`, `profile.selected`, `hook.fired`,
    `control.synced`, `permissions.relaxed`, `session.forked`) is not drawn: it
    says the session exists, not what the turn did.

  There is no turn-started or turn-finished event: the turn runs while `prompt.submit` is
  open and ends with its answer. The transcript is one column against the **left** edge,
  the pane less a margin: a centred chat reads as a document.

- **Stop** — while a turn runs the composer's row carries a spinner and a **Stop** button
  (`Esc` too): the TUI's `esc`, sent as `POST /api/interrupt` because a busy session reads
  no line. [web-composer.md](web-composer.md) §4 owns it, §5 the spinner a dead wire ends.

The transcript's vocabulary is the protocol's, and a test holds the page to it: every
event kind the page switches on must be one `loong_schema::generated::EVENT_KINDS`
defines (or the surface's own `client.error`) — the page once handled `tool.called` and
`turn.completed`, which the protocol never sends, silently dropping every tool call.

The composer shows the TUI's usage row — 入/出 tokens, calls, cache rate, context share —
recomputed from `cost.recorded`; the share is the one number no cost event carries, so it
comes from `model.selected`'s `context_tokens`, the window from the active `model.list`
entry. What the input does — `Enter`, `Shift+Enter`, `@` naming a workspace file — is
[web-composer.md](web-composer.md)'s.

## 5. Wire additions

No new session protocol methods: the surface speaks the TUI's JSON-RPC over WebSocket
instead of stdio. The web crate adds HTTP routes for its static assets (`app.js`,
`style.css`, `/favicon.ico` from the bundled `assets/favicon.png`) and three surface-local
JSON routes: `/api/browse`, listing subdirectories for the workspace picker,
`/api/files`, the composer's ranked file list behind `@` ([web-composer.md](web-composer.md)
§2), and `/api/interrupt` ([web-composer.md](web-composer.md) §4), the one route that is
not a read — a thread per connection, as the socket has, so neither waits on the other. The catalogue of §1 rides the socket (`workspace.list`/`add`/
`close`) and persists to `workspaces.json`; there is no `/api/workspaces` route. One
surface-local socket method, `session.tree`, answers the two-level rail in one call: every
workspace with its sessions nested, each flagged live/busy. `session.replay` is
surface-local too, read from the log and not from the child (§3). §6's `model.*` reads are
surface-local, and so is `model.reorder` (§4): an arrangement the bar owns, plus a write to
`local.toml` on a local profile.

`--session` and `--new` no longer pre-select a session: the surface starts empty, and the
launch directory is all `loongagent web` fixes.

## 6. Choosing models

The page can put a model on the bar and take it off again: a banner when none is
configured, and a panel beside the row that writes the ticked set — the `local add` wizard
without a terminal. One writer for `local.toml`, the same two files, the same rules:
[web-models.md](web-models.md) carries the mechanism, config-routing.md §3 and §8 the write.
