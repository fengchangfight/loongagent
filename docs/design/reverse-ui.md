# The reverse study page

- **Up:** [reverse.md](reverse.md)
- **Authority:** [ADR-0009](../decisions/0009-surfaces.md) · [ADR-0020](../decisions/0020-yolo-never-relaxes-irreversible.md) · [reverse design](reverse.md)
- **Code:** `crates/reverse/ui/src/lib.rs`, `crates/reverse/ui/src/page.html`
- **Verify:** `cargo test -p loong-reverse-ui` · `./scripts/check.sh`

`loongagent reverse` starts a portal and puts this page at `/reverse`. The page is one HTML
file with no build step and no CDN, served by the crate in `Code`; the engine-side facts about
what a target *is* stay in [reverse.md](reverse.md) §4.1. This document owns the page: its
layout, its two-step workflow, and the wire format of the routes it calls.

## Interface Chinese, prompts English

Every human-facing string — labels, buttons, status lines, errors — is Chinese. The prompts
the page injects are English: the model follows English instructions more reliably and
translating them buys nothing. The split is fixed, not a per-message choice.

## Layout: pick a target, walk the workflow, watch it happen

Modelled on the craft-anything desktop app, simplified to four bands: a project rail down the
left, then target, workflow and the panes, top to bottom.

```
┌───────────────────────────────────────────────────────────────────┐
│ 🐉 loongagent 逆向研究 · 项目 project-1 · 会话 …   [聊天][逆向研究]  │  ← 门户的 tab
├──────────────┬────────────────────────────────────────────────────┤
│ 项目          │ ① 选择目标                                          │
│ ● project-1  │    〔本地目标〕〔网站〕                              │
│ ○ project-2  │    [ /home/you/app.elf           ] [浏览…]          │
│ 〔+ 新建项目〕 │    ✓ 已选定：/home/you/app.elf（本地目标）            │
│              ├────────────────────────────────────────────────────┤
│              │ ② 工作流                                            │
│              │    ┌ ① 研究 ────────┐   ┌ ② 重构 ─────────┐         │
│              │    │ 〔开始研究〕    │ → │ 〔开始重构〕🔒   │         │
│              │    └────────────────┘   └─────────────────┘         │
│              │              〔标记研究完成〕                        │
├──────────────┼──────────────────────┬─────────────────────────────┤
│              │ .reverse/            │ 聊天                         │
│              │  study/project-1/    │ （流式、工具可见）            │
│              │   00-overview.md     │                             │
│              │  build/project-1/    │ [ 跟这个会话说点什么… ]       │
└──────────────┴──────────────────────┴─────────────────────────────┘
```

The project rail comes first because it decides what everything below it means: a target, a
study and a rebuild all belong to one project (reverse.md §3). The target band is next and
largest because it is the one thing the user must decide before anything can run; the workflow
band says where the user is and what the next step is; the three panes are the projects, the
current project's files (`/reverse/tree`, `/reverse/file/<path>`) and the chat socket the
portal already serves.

There is no preview pane. Reading a note is a glance, not a place to live: clicking a file
name shows it in an overlay over the page, closed by Escape or a click outside it. A third
permanent column was empty most of the time and squeezed the chat.

The three panes are draggable: a 6px grip sits between each pair, and the pages default to 190px
for the rail and 240px for the tree, the chat taking the rest. A width is a wish of this
browser, not a project fact — a second machine may have a second screen — so it lives in
`localStorage` under `reverse.column-widths`, not in `.reverse/`, and a double click on the grip
goes back to the default. Drags clamp at 120/420 for the rail and 140/520 for the tree, so a pane
never gets narrower than its longest button or wider than the chat it steals from.

The header carries the favicon, the product name, the current project's name, the portal's
session id and the model picker. The project name is how the user finds those notes in
`.reverse/study/` after the page is gone. The model picker is the one the chat page has: on
attach the page asks `model.list`, and a change sends `model.select`, so the model that
studies is the model that rebuilds and the model the chat tab uses
([surfaces.md](surfaces.md) §4). `loong-web` serves the favicon at `/favicon.png` for both
pages, so the tab is recognisable and no second copy of the icon is kept per page.

## The project rail: one project at a time

The rail lists every project under `.reverse/projects/` — the current one marked ● — plus
「+ 新建项目」.

- **Switching** makes that project current and reloads the page: the target bar refills from
  `projects/<project>/target`, the study gate returns to where that project left it, and the
  file tree lists only that project's `study/` and `build/`. Nothing of the previous project
  stays on screen.
- **新建项目** asks for a name and starts an empty project: no target, `phase` `todo`, empty
  tree. The old project keeps its notes and stays one click away — this is what "a whole new
  page with the earlier analysis cleared" means, without deleting anything.
- A name is a directory name, so the page refuses `/`, `\`, `..`, a leading dot and anything
  over 64 characters, and says which rule it broke. Chinese and spaces are fine.
- **删除** is the only control that removes anything from disk, so it is a `×` that appears on
  the row under the cursor and then asks the user to type the project name; a different name
  aborts and touches nothing. The server removes the project's three directories and its old
  trash entries (reverse.md §3) — a delete that left notes behind would not be one — and hands
  the page to the next project still standing, so a reload never lands on an empty rail.
- `POST /reverse/project` takes `create:<name>`, `select:<name>` or `delete:<name>`: verbs, so a
  typo cannot silently create a project when the user meant to switch to one.
- The rail is also why 「开始研究」 can be safe to press twice: the project is the unit that
  remembers having been studied (next section).

## The target bar: two kinds, because the first tool call differs

A target is not a UI category; it decides which tool opens it (reverse.md §4.1). There are two,
because `core_target_open` is the one file entrance and `web_site_open` the one URL entrance.

| Tab | Input | Validation |
|---|---|---|
| 本地目标 | an absolute path to a file or folder — ELF/PE/Mach-O, APK, jar, anything | absolute and exists |
| 网站 | an `http`/`https` URL | scheme only — it is not on this filesystem |

An APK had its own tab once; it opened through the same `core_target_open` +
`core_target_triage` as any file, so the tab only added a prompt sentence — and it got an
`.aab`, an `.xapk`, a `.dex`, a repacked zip or a folder holding one wrong.

- Switching the tab changes the placeholder, the validation and whether 「浏览…」 is shown:
  a URL is not a file.
- Validation happens in the page, so a wrong target is a visible message, not a wasted model
  turn: each tab refuses the other kind's value and says where it belongs.
- `projects/<project>/target` holds the value and `target-kind` the kind. A target recorded
  before kinds existed has only the first file, so the kind is inferred (`http(s)://` → `url`,
  else `local`; older builds' `binary`/`apk` read as `local`). Kept, because reopening on the
  wrong tab would hand the model the wrong entrance.
- The target belongs to the project, not to the process: it is recorded in
  `projects/<project>/`, so switching projects switches targets and a second reverse process
  on the same project sees the same target (reverse.md §3). A `target` left at `.reverse/` or
  in a `runs/<run>/` by the old layouts is moved into the current project, so the upgrade
  empties nothing.

Why the page grew its own directory browser: HTML forbids a page from learning the real path of
`<input type="file">` — the standard fakes it to `C:\fakepath\name`. The server runs on the
user's machine as the user, so it can list the filesystem itself (reverse.md §4.1).

## The workflow: study, then rebuild, gated by the user

Two visible steps with a door between them, not two buttons hidden in a header.

| Step | Trigger | Enabled when |
|---|---|---|
| ① 研究 | 「开始研究」 | a target is chosen; pressable again to redo the study |
| ② 重构 | 「开始重构」 | after the user pressed 「标记研究完成」 |

- Pressing 「开始研究」 when `study/<project>/` already holds notes does not silently overwrite
  them: the page asks "本项目已经做过研究了，重做会覆盖之前的笔记，继续吗？", and only on
  confirmation posts `/reverse/study/redo` — which moves the old notes into
  `.trash/<project>-<stamp>/` — before injecting the prompt. Wording and route both matter: a
  second click is either a fresh look at a half-studied target or a deliberate redo.
- A project that was never studied (its directory is empty) skips the question: there is
  nothing to lose. The test is a file existing, not the `phase` mark — the two answer
  different questions.
- The user decides when study is finished. Not "`.reverse/study/<project>/00-overview.md`
  exists": the model can land a half-written note, and lighting rebuild up from that starts a
  rebuild on an unfinished study. One extra click is the price of never guessing wrong.
- 「标记研究完成」 toggles, so it can be undone. Pressing 「开始研究」 again clears the mark
  back to `todo` — continuing the study means the conclusion is unsettled again.
- The mark is `projects/<project>/phase` (`done`), not browser `localStorage`: another tab,
  another browser or a restarted server must see the same step.
- The rebuild button stays visible but disabled, with the reason in its tooltip. Hiding it
  would leave the user guessing where the next step went.

## While a turn runs: the buttons lock and the work streams

One turn runs synchronously on the server: `prompt.submit` answers only when the turn is over,
and the page leans on that. Pressing 「开始研究」 or 「开始重构」 sets the busy state *before* it
sends, so the button greys and its label turns into 「正在研究…」 at once; between that moment
and the answer the socket is full of progress, which the page shows instead of a dead button:

| On the wire | What the page shows |
|---|---|
| `model.delta` | the row being written, with a blinking caret |
| `tool.call` | one line per call: a spinner, the tool's name, its arguments |
| `tool.result` | the same line turns ✓ or ✗, with the output folded under it (open on failure) |
| `policy.decided` | a line only when the verdict is `ask` or `deny` — `allow` would bury the progress |
| `message.appended` | the finished paragraph, replacing the streamed row |
| `approval.requested` | the turn parks: a card with 允许一次 / 拒绝, and the status line reads 「等你批准…」 |
| `approval.decided` | the same card settles (✓ 已允许（只这一次） / ✗ 已拒绝) — also when the answer came from the chat tab or the TUI |
| `permissions.relaxed` | the 自动批准 button in the chat header flips, and one line says what the mode does and does not cover |
| `hook.fired` · `turn.interrupted` · `client.error` | one line saying why the turn did not run |

The approval card is answered once and looks it: pressing either button swaps the pair for the
decision itself (「已允许（只这一次）」/「已拒绝」), so the card never keeps buttons that still
look pressable. A decision that came from the other tab — or from the TUI — arrives as
`approval.decided` and settles the same card here, buttons gone.

The chat header carries one switch, 自动批准 (YOLO): on, the agent stops asking before every
out-of-bounds action. Deleting and sending data out still ask, because neither can be taken
back — the kernel keeps that question and only that one (ADR-0020). The button reads its state
from `session.attach` and follows `permissions.relaxed`, so a mode the TUI turned on shows here,
and pressing it sends `permissions.set`.

Above the panes one line carries the state: what is happening now, the seconds spent, and how
many tool calls have come back. The spinner turns while a turn runs and stops when it parks — a
parked turn waits for the person, it is not working. While a turn runs or waits, the two step
buttons and the prompt box stay locked: a button that cannot be pressed says so better than a
sentence explaining why nothing happens.

Nothing here is polled. The counters belong to one turn and reset on the next.

## Routes the page calls

All of them are behind the surface token (reverse.md §4.2), sent as the `x-loong-token`
header.

| Route | Answer |
|---|---|
| `GET /reverse/tree` | the current project's tree as JSON — only `study/<project>/` and `build/<project>/`, sorted by path so each branch stays together |
| `GET /reverse/file/<path>` | one file's bytes; `..` is refused, nothing outside `.reverse/` |
| `GET /reverse/ls?path=<abs dir>` | `{"path":…,"parent":…,"entries":[{"name":…,"path":…,"dir":bool}]}` — names and kinds only, file contents are never read; `path` defaults to `$HOME` |
| `GET /reverse/projects` | `{"current":"project-1","projects":[{"name":"project-1","studied":true}]}` — the rail's list, `studied` = `study/<name>/` holds a note (a file that is not under `evidence/`) |
| `POST /reverse/project` | body `create:<name>`, `select:<name>` or `delete:<name>`; records `.reverse/project`, creates the directory for `create`, removes three directories and that project's trash for `delete`, refuses a duplicate, an unknown project or an invalid name with the reason |
| `POST /reverse/study/redo` | moves `study/<current>/` into `.trash/<current>-<stamp>/` and resets `phase`; answers with the trash path |
| `GET /reverse/run` | this process's run and the current project: `{"name":"run-001","project":"project-1","study":"study/project-1","build":"build/project-1","studied":true}` — the two paths are the ones the prompts name |
| `GET`/`POST /reverse/target` | the project's target; `POST` validates and records it |
| `GET`/`POST /reverse/phase` | `done` or `todo`; `POST` sets the study-finished mark |
| `GET`/`POST /reverse/session` | the conversation this project opens next: `POST` records the id a person picked in the chat header, `GET` reads it back — the same `projects/<current>/session` the CLI binds at startup (reverse.md §5) |

The target, the kind and the phase are read from `projects/<current>/`, not from `.reverse/`
itself and not from `runs/<run>/` (reverse.md §3): they describe the project, so they outlive
the process, and switching projects switches all three at once. The tree lists only the current
project's two directories; older projects are one click away in the rail rather than scrolled
past in it. `/reverse/file/<path>` takes paths relative to `.reverse/`.

`/reverse/target` speaks two lines: the kind (`local`/`url`) on line one, the value on line two;
older projects' `binary`/`apk` read back as `local`. Two lines rather than JSON because neither a
path nor a URL can contain a newline, so reading it is one `split_once` and this crate needs no
serde. `GET` answers the same two lines, empty when nothing is chosen.

The chat pane is not a route of this crate except for the session records above: it opens the
portal's own `/ws`, and the tree refreshes after each assistant message, with no polling. What it
shows, and how it switches conversations, are [reverse-conversation.md](reverse-conversation.md).

## What the buttons inject

Both prompts are English (see above) and name the target explicitly. Both also name **the current
project's** directories, taken from `GET /reverse/run` at page load, so the model writes into
`study/<project>/` and not into whatever `.reverse/study/` held from an earlier study:

- **本地目标**: `core_target_open` → `core_target_triage` → the toolsets it recommends (native for
  ELF/PE/Mach-O; `android_package_show` → `android_components_list` → classes, methods and
  resources for an APK), then `.reverse/study/<project>/00-overview.md`, `01-architecture.md`
  and `02-findings.md`. An APK's bytecode is smali: claiming Java-level decompilation is out.
- **网站**: `web_site_open` — explicitly *not* `core_target_open` — → `web_pages_list`,
  `web_page_show`, `web_modules_trace`, `web_scripts_save` → `web_site_capture` into a folder
  under `study/<project>/evidence/` → `web_site_close`, and the notes must name the capture
  folder each finding came from.
- **重构**: read all of `.reverse/study/<project>/` (following what a note points to under
  `evidence/`) and rebuild under `.reverse/build/<project>/`, picking a sensible stack unless
  the user says otherwise.
- **Intermediate products**: all three study prompts also name `study/<project>/evidence/` and
  say that unpacked files, saved scripts, traces and captures go there — the folder exists from
  the start, because a capture tool refuses a missing parent — while `build/<project>/` is left
  for the rebuild. Without that sentence the model reads the empty `build/<project>/` in the
  tree as its scratch space.

Neither prompt tells the model to declare the study finished: that is the user's click.
