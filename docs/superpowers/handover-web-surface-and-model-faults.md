# Handover: the web surface's layout, its Stop button, and retried model faults

Written 2026-11-10. The tree is **dirty on purpose** and holds two unrelated bodies of
work; §5 says which is whose. Read §4 before you trust any green.

## 1. What the person asked for

Three changes to the `loongagent web` page, then one fix to a recurring error:

1. The workspace picker sat in the banner. Move it to the top of the left rail, as a
   button that opens the picker.
2. The transcript was a ~780 px column centred in the window. Make it left-aligned and
   as wide as the pane.
3. While a turn runs, show that it is thinking and offer a button that does what `esc`
   does in the TUI.
4. `approval.respond: model: transport: HTTP 500: {"type":"internal_error"}` kept killing
   turns. Find out why and whether it needs fixing.

All four are done. §2 says how, §3 says how they are tested, §4 says what is still
unverified, §6 lists what is left.

## 2. What changed, and where the design lives

Every change below was written into its owning document **first**, as `AGENTS.md`
requires; the doc link is the way back in.

### 2.1 The workspace button moved to the rail

- `crates/web/src/index.html` — `#workspace-btn` moved out of `<header id="banner">` into
  `<aside id="rail">`, above `.rail-head`. The banner now holds only the favicon, the
  wordmark and the inscription.
- `crates/web/src/style.css` — `#workspace-btn` is the rail's width less its margins,
  so a long directory name no longer competes with a session row; `.dir-path` truncates
  from the left (`direction: rtl`) because the tail of a path is the informative part.
- `crates/web/src/app.js` — no logic change. `picker` (line ~304) already owned
  open/close/browse; only its trigger moved.
- Owned by `docs/design/web-surface.md` §1 and §4.

### 2.2 The transcript is left-aligned and full width

- `crates/web/src/style.css` — `.wrap` went from `max-width: 780px; margin: 0 auto` to
  `max-width: none; margin: 0; padding: 0 26px`. The padding is now the only measure, so
  a paragraph still stops short of the edge while a tool card spends the full width.
- Owned by `docs/design/web-surface.md` §4.

### 2.3 A thinking spinner and a Stop button

Front end — `crates/web/src/index.html`, `style.css`, `app.js`:

- `#thinking` (a spinner plus "Thinking…") and `#stop` live in the composer's row, next
  to `#send`. Both carry `hidden` and the CSS has an explicit `#thinking[hidden]`,
  `#stop[hidden] { display: none }` — without it the author `display` beats the user
  agent's `[hidden]` rule and they never go away. That mistake is easy to re-make.
- `setThinking(on)` replaces every direct `dom.send.disabled = …`: it drives the spinner,
  the Stop button and the spent Send button together, so the three cannot disagree.
- `stopTurn()` POSTs `/api/interrupt`. `Esc` calls it too, in `wireShortcuts`, but an
  open `@` picker or workspace picker claims `Esc` first.
- **The approval path now lights them as well.** Answering a parked approval resumes the
  turn, and that resumed request is where the reported 500 arrived; previously
  `state.running` was only set around `prompt.submit`, so the page sat there looking idle.
  It is also why the error read `approval.respond:` rather than `prompt.submit:`.

Back end — the part worth understanding before changing anything:

- A turn **owns** the session: the child is blocked inside `prompt.submit`, so any line the
  page sends waits until the turn it was meant to end has already ended. Stop therefore
  cannot be a protocol method. It is an HTTP route that signals the child process, exactly
  as the TUI's `esc` does.
- `crates/web/src/lib.rs` — `pub type StopSignal = Arc<dyn Fn(u32) + Send + Sync>` is
  injected into `serve_web`/`serve_web_with_models` rather than asked of the session, so
  the web crate keeps no copy of the platform code; the CLI passes
  `loong_tui::interrupt`, which makes the browser's Stop and the terminal's `esc` the same
  act. Also here: the `/api/interrupt` route, `read_request_body`, and a new defaulted
  trait method `Session::pid()`.
- `crates/web/src/app.rs` — `Child` gained `pid: Option<u32>`, read **once at creation**
  (asking later would block on the bridge the running turn holds); `busy_child()` finds the
  session that is busy and returns its pid.
- `crates/cli/src/web.rs` — `WebSession::pid()` returns `Some(self.transport.pid())`, and
  the listen call passes `Arc::new(loong_tui::interrupt)`.
- The route answers `{"interrupted": true}`, or `409` with a `reason` in words: "no turn is
  running", "this session has no process to signal". A signal sent to an idle child would
  land on whatever it did next, so refusing is the safe answer.
- The pid never reaches the browser: the page sends `session_id`, the server looks up the
  process. There is no route by which the page can name a pid.
- Owned by `docs/design/web-composer.md` §4 (new section; the mechanism lives there) and
  pointed to from `web-surface.md` §4 (the surface map).

### 2.4 A 500 is retried; a 400 is not

The 500 itself is the provider's (`request_id` came from them: the request arrived and
they failed). What needed fixing is that one bad minute on their side threw away a turn
whose tool had **already run** and whose approval had already been spent.

- `crates/providers/src/lib.rs` — `ModelError::is_transient()` splits failures in three:
  a **hiccup** (5xx, 429, connection reset/refused/closed, broken pipe, timeout, EOF), a
  **size refusal** (`is_context_overflow`), and a **refusal** (400/401/422, or any
  `Protocol` error). `http_status()` reads the `HTTP <code>` prefix the adapters write.
- `crates/kernel/src/turn/loop.rs` — `TRANSIENT_BACKOFF` (300 ms, 900 ms) and a retry
  branch in `complete_with_ladder`, before the `KernelError::Model` return. Two retries,
  then it fails. Each attempt goes to stderr, which both surfaces capture.
- The rule behind it, worth quoting when someone wants to widen it: **did anything get
  recorded?** A hiccup produced no answer, no tool call, no event, so asking again is
  asking once — and that is why the retry keeps the tool results the turn already paid
  for. A refusal would be refused again. A size refusal is a different fix (shed context).
- New document: `docs/design/model-faults.md` owns the three shapes and the reasoning.
  `kernel-loop.md` §7 summarises and points at it; `docs/design/README.md` lists it.

## 3. How it is tested

New tests, all of which pass:

| Test | Where | Proves |
|---|---|---|
| `a_hiccup_is_told_apart_from_a_refusal` | `crates/providers/src/lib.rs` | 5xx/429/socket errors are transient; 400/401/422 and `Protocol` are not |
| `a_hiccup_is_retried_and_the_turn_survives` | `crates/kernel/src/turn/tests.rs` | the reported 500 no longer ends the turn; the request was sent twice, identical |
| `a_hiccup_that_never_clears_still_fails_the_turn` | same | exactly three attempts, then an error — no unbounded retry |
| `stop_ends_the_running_turn` | `crates/web/src/app.rs` | nothing running → 409 with words, no signal; a busy turn → 200, the right pid, and the turn resolves afterwards |

`a_server_fault_still_fails_the_turn` changed from `HTTP 500` to `HTTP 401`, because a 500
is now retried and is no longer the shape that proves a fault stays fatal.

**Both new behaviours were shown failing before they passed**, which `AGENTS.md` requires:
forcing `is_transient` to return `false` fails the retry test; commenting out `stop(pid)`
in the route fails the stop test. Flip either back to see it.

## 4. What was verified, and what was not

Run and green:

```
cargo build --workspace                                  # ok
XDG_RUNTIME_DIR=$TMPDIR cargo test --workspace            # 149 binaries, 2227 passed, 21 ignored
python3 scripts/check-docs.py                            # OK
python3 scripts/check-modules.py                          # OK
```

**Not green: `./scripts/check.sh`'s last step.** It runs everything above and then
`( cd clients/vscode && npm ci && npm run compile && npm test )`, which needs the network;
this environment has none. Everything before that step passed. See §5 — I also emptied
that directory's `node_modules`, so it needs a network `npm ci` regardless of my changes.

**Not verified at all: the page in a browser.** The `/api/interrupt` route is tested at the
HTTP level and `app.js` passes `node --check`, but nobody has clicked the new Stop button,
watched the spinner, or looked at the widened transcript. Treat the three UI changes as
"written and type-checked", not "seen working". First thing to do on a networked machine:

```
cd clients/vscode && npm ci && npm run compile && npm test   # restores what I emptied
cargo run -p loongagent -- web                                # then click through §2.1–2.3
```

## 5. The tree is dirty, and one part is not mine

`git status` shows two unrelated bodies of work. Do not revert either by accident.

| Mine (this handover) | Someone else's (in progress, leave alone) |
|---|---|
| `README.md` (test count) | `crates/tui-app/src/lib.rs`, `crates/tui-app/src/tests.rs` |
| `crates/cli/src/web.rs` | `docs/design/surfaces.md` |
| `crates/kernel/src/turn/{loop,tests}.rs` | `docs/design/tui-selection.md` |
| `crates/providers/src/lib.rs` | |
| `crates/web/src/{lib,app}.rs`, `{app.js,index.html,style.css}` | |
| `docs/design/{README,kernel-loop,web-composer,web-surface}.md`, `docs/design/model-faults.md` (new) | |

The tui-app work is a draggable scroll bar on the TUI's page. It is unrelated, it does not
touch anything in this handover, and its tests pass in the run above.

## 6. Traps in this workspace

- **`/tmp` is not writable from the agent shell.** Some sandbox tests need a runtime dir;
  run tests as `XDG_RUNTIME_DIR=$TMPDIR cargo test --workspace` or they fail with
  `Permission denied` — on a clean `HEAD` too, so it is not a defect to chase.
- **`npm ci` cleans `node_modules` before it installs.** I ran it, it needed the network,
  it failed, and it left `clients/vscode/node_modules` empty. The directory is git-ignored,
  no TypeScript was touched, and `npm ci` restores it. But `npm run compile` will fail
  until someone does.
- **`README.md` states the test count** and `check-docs.py --check counts` compares it
  against the suite. Adding a test without updating both numbers fails the gate. It reads
  `2227 passed, 21 ignored` now.
- **`docs/design/web-surface.md` is at its 250-line budget.** Any addition there must be
  paid for by a cut. This is why the Stop mechanism lives in `web-composer.md` §4 and
  `web-surface.md` only points at it — keep doing that.
- **`docs/human-written/` is the person's own**, never edited by an agent, and the gate
  enforces it.
- `docs/superpowers/` is a record directory: no header fields, no index row, no line
  budget. That is why this file is here.

## 7. What is left, in the order I would do it

### 7.1 A failed turn leaves no trace (worth doing)

A turn interrupted by `esc` appends `turn.interrupted`. A turn killed by a model fault
appends **nothing** — `complete_with_ladder` returns `KernelError::Model` without an event
(`crates/kernel/src/turn/loop.rs`, the `return Err` at the end of the error branch). So
after a refresh the transcript shows a truncated turn with no reason, and the only evidence
is a line on the child's stderr. Needs a protocol event, so `crates/schema/protocol.toml`,
a regenerated `generated.rs`, and a field in the web transcript. That regeneration is why
it was left out of this batch.

### 7.2 Irreversible approvals are asked every single time (needs a decision first)

The person said the irreversible prompt fires too often. Cause: the page always sends
`scope: "once"` (`crates/web/src/app.js`, the `approval.respond` call). The protocol
already carries `action_class` and `project` scopes, but nothing widens the vote — the
scope is recorded in `approval.decided` and does not change later decisions — so today
there is no way to say "stop asking about this".

What I proposed and the person has not yet accepted: allow **by command shape, not by
action class** — remember `(command template, path prefix)` for the rest of the session,
inside the workspace only. `rm -rf target/*` then stops asking; `git push` still asks every
time. Widening by action class would mean "any deletion, ever", which is the thing the
class exists to prevent. This changes `docs/design/tools-permissions.md` and the boundary
ADR-0020 draws, so the documents come first.

### 7.3 Retry does not cover a mid-stream failure

`is_transient` reads the message the adapter produced. A connection that dies *after*
deltas have streamed may surface as a different string than the ones in the needle list.
If the reports keep coming, start there: the retry is cheap, the classification is the part
that can be wrong.

## 8. Reading order

1. `docs/design/web-surface.md` — what the page is, §4 for the layout rules.
2. `docs/design/web-composer.md` §4 — Stop, and why it is a signal and not a method.
3. `docs/design/model-faults.md` — the three failures, and the line between them.
4. `AGENTS.md` — the contract rules; every file you touch will be checked against them.
