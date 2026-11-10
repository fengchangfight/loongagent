# The reverse page's conversation: a project's sessions, and the one that is open

- **Up:** [reverse-ui.md](reverse-ui.md)
- **Authority:** [reverse.md](reverse.md) §5 · [sessions-lineage.md](sessions-lineage.md) §1 · [cost-accounting.md](cost-accounting.md) §4
- **Code:** `crates/cli/src/reverse.rs`, `crates/cli/src/session.rs`, `crates/cli/src/server.rs`, `crates/reverse/ui/src/page.html`, `crates/reverse/ui/src/lib.rs`, `crates/server/src/lib.rs`, `crates/schema/protocol.toml`
- **Verify:** `cargo test -p loong-reverse-ui` · `cargo test -p loongagent --bins reverse_` · `cargo test -p loong-server session_open`

The chat beside the study speaks the portal's own protocol on `/ws` (reverse-ui.md, 「Routes the
page calls」), and it is where a person spends most of the page's time. What it shows is the
tab's business, and so is which conversation it shows: a project may hold several, and the page
lists them. This document owns that: where the histories live, which one opens, how the picker
switches, the usage line under the input, and the one button beside it.

## A project's sessions

Every session of a project is a normal session log, in a **drawer of its own**:
`~/.loongagent/projects/reverse-<project>-<hash>/<id>.jsonl` (sessions-lineage.md §1). The log
format, the lock and the events are the TUI's; only the directory differs. That is the whole
isolation: `session.new`, `session.open` and `session.list` work in the directory the surface
names, so a project's picker answers with its own conversations and cannot answer with the
workspace's.

The workspace's directory is the wrong list, and it is why the drawer exists. It is shared with
the TUI, `loongagent resume`, VS Code and every other reverse study in the same checkout: a
workspace used for months holds dozens of conversations about the source tree, and none of them
is this study's. Filtering that list was tried and does not work — the list itself is the wrong
question to ask, and a filter can be widened, mis-clicked or forgotten. One drawer per project
also means two studies in one checkout never show each other's conversations, which filtering by
workspace could not do at all. The name carries the project so the folders are readable:
`ls ~/.loongagent/projects` shows which are reverse projects and which project each belongs to.

The consequence is intended: `loongagent resume` and the TUI do not list a reverse project's
conversations. They are the study's, and the study is where they are continued.

Inside the drawer nothing else is needed. The drawer *is* the list, so there is no record of
which conversations a project has used, and no route that serves one: `session.list` is the
answer, the page asks for it on load and after every switch, and it never reads a session
directory itself.

`.reverse/projects/<project>/session` holds **the id to open next**. It sits beside `target` and
`phase` because "which conversation was I having" is a property of the project, and a start reads
it before it scaffolds the run (reverse.md §3, §5). So a restart lands back in the conversation
the person used last, without typing anything — and the other sessions stay one click away in the
picker instead of being lost.

A project with no record — or whose log is gone — gets a fresh session and records it. Naming one
on the command line (`--session <id|latest>`, `--new`) is how the person overrides the default:
the named log becomes the recorded one, so the next start continues it.

A project that bound conversations before drawers existed still lists them in
`.reverse/projects/<project>/sessions`, and those logs sit in the workspace's directory. The
first start moves them into the drawer (a rename, idempotent, once per id), so yesterday's
conversation stays in the picker instead of looking deleted; nothing writes that file any more.

The log has one writer (`eventlog`'s sidecar lock, events-storage.md §3). Binding probes it by
opening it: a log this process can open is a log nobody else is writing. When another process
holds the recorded one, this start talks in a session of its own and says so on the terminal —
the page still opens, and the record is left alone, because the conversation in progress belongs
to the process writing it. `runs/<run>/session` records which session each run used, so a run's
notes stay traceable to the conversation that wrote them.

## The picker: which conversation is on screen

The chat pane's header carries a `<select>` of the project's sessions and a 「＋ 新会话」 button.
The list is `session.list` — the read-only method the server answers from the CLI's lister, in the
project's drawer (sessions-lineage.md §1) — so the page never reads a session directory itself,
and every row is this project's by construction (「A project's sessions」 above). Each row shows the session's title (its first prompt), when it was last written,
and how many events it holds; the one on screen is selected. A conversation whose log has been
deleted stays on the list marked 「日志已不在」: opening it fails and says so, which is truer than
silently dropping it.

Switching is `session.open` — a new method, and the counterpart of `session.new`: the server asks
the CLI's opener for a kernel on that existing id, swaps `Server.kernel`, and resets `last_sent`
and `turns` exactly as `session.new` does (sessions-lineage.md §1). The old log is closed and
kept; the record in `.reverse/` is what changes. The page then replays the new session
(「Replaying on open」 below) and reports the switch in one line, so a person can tell a switch
from a scroll.

「＋ 新会话」 is `session.new`, then the same record-and-replay. One gesture, one new thread —
useful when a study is finished and the next one should start clean, and the only way to a second
conversation without a second terminal.

Two things are refused rather than faked:

- **While a turn runs or waits for an approval, the picker is disabled.** Switching under a
  streaming turn would leave that turn writing into a session the page is no longer showing, and
  its events would arrive with an id the page does not have. Stop first, then switch.
- **A session another process holds cannot be opened.** The kernel's open fails on the lock, and
  the page shows that error and stays where it was — it does not quietly open a fresh log and
  call it the same conversation.

After a successful switch or a new session, the page `POST`s the id to `/reverse/session`, and
the surface writes two records: `projects/<project>/session` — the page is the only place that
knows a person chose a different conversation, and the next start should follow that choice — and
this run's own `session`, so the notes in `.reverse/` stay traceable to the conversation writing
them. The picker needs no third record: the drawer is its list.

## Replaying on open

Attach, then `session.replay`: the page asks for the whole log and lays it out *before* it says
it is connected, so 「已连接」 reads as "this is where you are" — under the history, not over a
blank page. A replayed row is laid out quietly: a tool call keeps its one line and its folded
output, but does not turn the progress spinner, does not count toward this turn's tool calls,
and does not offer an approval button. That question was answered — or was never answered — long
ago, and a button that cannot act is worse than no button. The tree loads once at the end,
instead of once per replayed message.

A replay that fails is a message, not a dead page: the page says the history did not come back
and stays usable for the next turn.

## The usage line under the input

The same reading as the TUI's bottom bar (cost-accounting.md §4), because two surfaces that
disagree about what a session cost are worse than one that shows nothing:

```
入 12,345 · 出 4,567 · 调用 3 次 · 缓存命中 8,000（65%）· 估算 $0.000335
```

Every `cost.recorded` — live or replayed — adds one call, its prompt and completion tokens, its
cached tokens and its estimate. The hit rate is counted over the calls that reported cache at all
(`cache_reported`), with their reported prompt tokens as the base: a provider that says nothing
about caching must not be drawn as a provider that missed. The last field reads 「免费（本地）」
for a local model, 「估算 $…」 when a price was known, and 「价格未知，不估算」 otherwise — an
invented number would be a lie with six decimal places.

The line is per conversation, not per process: attaching, switching or starting a session resets
it to that session's own total, because a person reading 「调用 3 次」 under a conversation that
made none would be misled about where the money went.

## One button, two jobs

The button right of the input is 「发送」 while the page is idle and 「停止」 while a turn runs or
waits for an approval. One control, because the two never apply at once — and a send button that
cannot send is the kind of thing a person presses three times.

Sending is what Enter does. Stopping is `POST /reverse/interrupt`, which this surface answers by
signalling its server child: the same SIGINT that Ctrl+C raises in the TUI (surfaces.md §7). It
has to be a signal. The server reads one line at a time, so a JSON-RPC request sent while a turn
runs sits in the pipe until that turn is over — which is the very turn the person wanted to stop.
The hook is handed in by `crates/cli/src/reverse.rs`, the only process that knows the child's pid;
the surface answers `200` with `{"interrupted":true}` and refuses a `GET` with `405`.

A stop lands between steps: the kernel checks the flag at each decision boundary, so a model call
already streaming finishes first. Then the page reports 「这一轮停了」 with the seconds and the
tool calls spent. The same event arrives when the turn was interrupted from somewhere else, and
the page reports that as the end it was — the wording never claims a finished turn for a stopped
one.

A turn parked on an approval needs one more thing, because a signal alone would leave it waiting
for an answer nobody is going to give: the page refuses the pending approval first — the socket is
free while the turn is parked, so that write goes through — and the card closes as 已拒绝. Then
the signal unwinds the turn at its next boundary.

## What this does not fix

- **A refresh in the middle of a turn** cannot know the turn is still running: nothing in
  `session.attach` says so, so the page comes back looking idle while the server is still
  working, and a prompt sent then queues behind that turn. The stop button is the way out.
- **A request sent over `/ws` while the model is streaming waits for it.** The server handles one
  line at a time and the turn's own handler is the one running, so `turn.interrupt` as a
  JSON-RPC method would arrive after the very turn it was meant to stop. A *parked* turn — one
  waiting for an approval — does not have that problem: its handler returned, the socket is free,
  and the approval card's answer goes straight through. Stopping cannot rely on that, because the
  turn worth stopping is the one still streaming — which is why the stop is a signal, and a route
  of its own.
- **A session is listed by title and time, not by what it concluded.** Two studies that both began
  with 「看看这个二进制」 look alike in the picker; the title is the first prompt, and nothing
  summarises the thread. Reading the notes in `.reverse/study/` is still how to tell them apart.
- **A conversation started outside reverse stays out of the list until it is picked.** That is the
  point — the list means "this study's conversations" — but it also means a TUI conversation that
  turned into a study has to be found through 「全部会话」 once, by hand.
