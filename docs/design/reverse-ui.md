# The reverse study page

- **Up:** [reverse.md](reverse.md)
- **Authority:** [ADR-0009](../decisions/0009-surfaces.md) · [reverse design](reverse.md)
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

Modelled on the craft-anything desktop app, simplified to three bands, top to bottom.

```
┌────────────────────────────────────────────────────────────┐
│ loongagent 逆向研究   会话 …          [聊天] [逆向研究]      │  ← 门户的 tab
├────────────────────────────────────────────────────────────┤
│ ① 选择目标                                                  │
│    〔程序文件〕〔Android APK〕〔网站〕   ← 三个标签，按类型分开  │
│    [ /home/you/app.elf            ] [浏览…]                 │
│    ✓ 已选定：/home/you/app.elf（程序文件）                    │
├────────────────────────────────────────────────────────────┤
│ ② 工作流                                                    │
│    ┌ ① 研究 ──────────┐   ┌ ② 重构 ──────────┐             │
│    │ 把目标查清楚，     │ → │ 按研究笔记重写一份 │             │
│    │ 写进研究笔记       │   │ 研究完成后才可点   │             │
│    │ 〔开始研究〕       │   │ 〔开始重构〕🔒    │             │
│    └──────────────────┘   └──────────────────┘             │
│                        〔标记研究完成〕                       │
├──────────────────┬─────────────────────┬───────────────────┤
│ .reverse/        │                     │                   │
│  study/          │      文件预览        │      聊天          │
│   00-overview.md │  （点左侧文件名）     │  （流式、工具可见） │
│  build/          │                     │                   │
└──────────────────┴─────────────────────┴───────────────────┘
```

The target band sits first and largest because it is the only thing the user must decide
before anything can run. The workflow band comes next so the user can see where they are and
what the next step is. The three panes below are the session's own files (`/reverse/tree`,
`/reverse/file/<path>`), a preview, and the chat socket the portal already serves.

## The target bar: three kinds, because the first tool call differs

A target is not a UI category. It decides which tool opens it (reverse.md §4.1), so the page
asks for the kind up front and keeps it.

| Tab | Input | Validation |
|---|---|---|
| 程序文件 | an absolute path to an ELF/PE/Mach-O file (a directory is allowed too) | absolute and exists |
| Android APK | an absolute path to an `.apk` | absolute and exists |
| 网站 | an `http`/`https` URL | scheme only — it is not on this filesystem |

- Switching the tab changes the placeholder, the validation, and whether 「浏览…」 is shown:
  a URL is not a file, so the directory browser is hidden on that tab.
- Validation happens in the page, so a wrong target is a visible message, not a wasted model
  turn. The URL tab refuses a bare path and the file tabs refuse a URL; each says which tab
  the value belongs in.
- `.reverse/target` holds the value and `.reverse/target-kind` the kind. A target recorded
  before kinds existed has only the first file, so the kind is then inferred from the value
  (`http(s)://` → `url`, `.apk` → `apk`, else `binary`); upgrading loses nothing.
- Keeping the kind matters after a reload: reopening on the wrong tab would hand the model the
  wrong toolset.

Why the page grew its own directory browser: HTML forbids a page from learning the real path of
`<input type="file">` — the standard fakes it to `C:\fakepath\name`. The server runs on the
user's machine as the user, so it can list the filesystem itself (reverse.md §4.1).

## The workflow: study, then rebuild, gated by the user

Two visible steps with a door between them, not two buttons hidden in a header.

| Step | Trigger | Enabled when |
|---|---|---|
| ① 研究 | 「开始研究」 | a target is chosen; repeatable, for continued study |
| ② 重构 | 「开始重构」 | after the user pressed 「标记研究完成」 |

- The user decides when study is finished. Not "`.reverse/study/00-overview.md` exists": the
  model can land a half-written note, and lighting rebuild up from that starts a rebuild on an
  unfinished study. One extra click is the price of never guessing wrong.
- 「标记研究完成」 toggles, so it can be undone. Pressing 「开始研究」 again clears the mark
  back to `todo` — continuing the study means the conclusion is unsettled again.
- The mark is `.reverse/phase` (`done`), not browser `localStorage`: another tab, another
  browser or a restarted server must see the same step.
- The rebuild button stays visible but disabled, with the reason in its tooltip. Hiding it
  would leave the user guessing where the next step went.

## Routes the page calls

All of them are behind the surface token (reverse.md §4.2), sent as the `x-loong-token`
header.

| Route | Answer |
|---|---|
| `GET /reverse/tree` | the `.reverse/` tree as JSON, directories before files |
| `GET /reverse/file/<path>` | one file's bytes; `..` is refused, nothing outside `.reverse/` |
| `GET /reverse/ls?path=<abs dir>` | `{"path":…,"parent":…,"entries":[{"name":…,"path":…,"dir":bool}]}` — names and kinds only, file contents are never read; `path` defaults to `$HOME` |
| `GET`/`POST /reverse/target` | the chosen target; `POST` validates and records it |
| `GET`/`POST /reverse/phase` | `done` or `todo`; `POST` sets the study-finished mark |

`/reverse/target` speaks two lines: the kind (`binary`/`apk`/`url`) on line one, the value on
line two. Two lines rather than JSON because neither a path nor a URL can contain a newline, so
reading it is one `split_once` and this crate needs no serde. `GET` answers the same two lines,
empty when nothing is chosen.

The chat pane is not a route of this crate: it opens the portal's own `/ws` and speaks the
protocol the chat page uses. The file tree refreshes after each assistant message — no polling.

## What the buttons inject

Both prompts are English (see above) and both name the target explicitly rather than hoping
the session guesses it. The study prompt branches on the kind, because the first call differs:

- **程序文件**: `core_target_open` → `core_target_triage` → native tools, then write
  `.reverse/study/00-overview.md`, `01-architecture.md`, `02-findings.md`.
- **APK**: `core_target_open` → `android_package_show` → `android_components_list` → classes,
  methods and resources as needed. The bytecode is smali; the prompt forbids claiming Java
  level decompilation.
- **网站**: `web_site_open` — explicitly *not* `core_target_open` — → `web_pages_list`,
  `web_page_show`, `web_modules_trace`, `web_scripts_save` → `web_site_capture` → `web_site_close`,
  and the notes must name the capture folder each finding came from.
- **重构**: read all of `.reverse/study/` and rebuild under `.reverse/build/`, picking a
  sensible stack unless the user says otherwise.

Neither prompt tells the model to declare the study finished: that is the user's click.
