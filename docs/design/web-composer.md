# The composer

- **Up:** [web-surface.md](web-surface.md)
- **Authority:** [ADR-0009](../decisions/0009-surfaces.md)
- **Code:** `crates/web/src/app.js`, `crates/web/src/files.rs`, `crates/web/src/index.html`, `crates/web/src/style.css`, `crates/web/src/lib.rs`
- **Verify:** `cargo test -p loong-web` · `./scripts/check.sh`

What the box at the bottom of the page does with a message before it becomes a turn:
which keystroke sends, how a workspace file is named, and what the composer
deliberately does not carry. [web-surface.md](web-surface.md) §4 draws the panel;
this document owns the input.

## 1. One box, and Enter sends it

`Enter` sends, `Shift+Enter` is a newline, and the box grows with the message to
180px and then scrolls. A message typed while a turn is running is not queued: the
composer is a way to say one thing, and a queue nobody can see is a promise the page
cannot keep. Sending with no session yet opens one first, so the first thing a person
types is not lost to a missing step.

Two keystrokes save a file and this one does not. The bar's `Ctrl` combinations belong
to the model row ([web-models.md](web-models.md)); the composer takes the plain keys
and hands the modified ones to the page.

## 2. `@` names a workspace file

Typing `@` at a word boundary — the start of the message or after whitespace — opens
a list of the workspace's files above the input, filtered as the mention is typed.
`↑`/`↓` move, `Tab`, `Enter` or a click takes a row, `Escape` closes. Taking a row
leaves `@relative/path ` in the message and closes the list: it inserts, it does not
send, and the rest of the message is untouched. A list with no rows takes no keys, so
`@` on its own never turns `Enter` into a dead key.

The mention ends at whitespace, and `@` inside a word (`a@b`) opens nothing: an address
is not a path, and a picker that guessed would be a picker that interrupts.

**The list is the server's.** The page asks `/api/files?token=…&seat=<n>&q=<query>&limit=8`
and draws what comes back; it never names a directory itself. The seat is the workspace
the tab sits on, resolved to a path in `app::State`, so the route cannot be pointed at
`/` — a page-local `dir=` would be a file browser for the whole machine behind a token
that already sits in the address bar. The route is token-gated like `/api/browse`.

Each answer is the workspace's file list, ranked: the file's own name first, then a
substring anywhere in the path, then an in-order subsequence, so `skv` finds
`src/kernel/view.rs` and `vks` finds nothing. The list is `git ls-files --cached
--others --exclude-standard`, or a bounded eight-deep walk for a workspace that is not
a repository, with `target`, `node_modules` and `.loongagent` never offered.

That rule is the TUI picker's (`crates/tui-app/src/mention.rs`), restated in
`crates/web/src/files.rs` rather than shared: ADR-0004's layering puts the terminal
surface and the portal on separate branches with no crate below both. The two are
deliberately identical — the same keystrokes should put the same first row on screen in
both surfaces — and each is pinned by its own tests.

The page asks at most one request per 120ms pause, and drops an answer that a later
keystroke has made stale, because the server runs `git` on each one.

**The mention is text.** `@src/main.rs` reaches the model as those characters; the
kernel reads the file with its own tools when the model asks. Nothing in the page
attaches a file to a turn, so there is no second path into a session and no way for the
page and the agent to disagree about what the file contains.

## 3. What the composer does not carry

- **No image or file attachment.** A turn's user message is text — `ChatMessage::User
  { text }` in `loong-providers`, and `text` is the whole of `prompt.submit`'s params on
  the wire — so a picture pasted into the box would be a promise nothing downstream can
  keep. Support
  is a change to the protocol and the kernel, in that order: an image field on
  `prompt.submit` and `message.appended` so a resumed session still has it, content
  parts in the provider's request body, an entry in the ledger's token estimate, and
  only then the page's chips and thumbnails. Until then the composer says nothing about
  pictures rather than accepting one and dropping it.
- **No prompt history.** `↑` with an empty box is left to the browser; the TUI's message
  history is not on the wire.
- **No slash commands.** `/model`, `/context` and `/yolo` are the TUI's; the page has the
  bar, the usage row and the switch instead.

## 4. Stopping a turn

While a turn runs, the composer's row carries a spinner that says it is thinking and a
**Stop** button; `Esc` does the same. It is the TUI's `esc`.

It cannot be a protocol method. A turn owns the session for as long as it runs — the
child is blocked inside `prompt.submit`, so a line sent now would sit unread until the
turn it was meant to end had already ended. The page therefore asks the listener to
signal the child the way the terminal does (ADR-0019's reasoning, `surfaces.md` §7):

```
POST /api/interrupt?token=<token>   {"session_id": "<id>"}
```

The route answers with `interrupted`, and with a `reason` in words when it is false —
"no turn is running" when there is nothing to stop, "this session has no process to
signal" when the session is not a child process. The id travels in the body rather than
the query so a stray link or an image tag cannot fire a signal at this surface, and the
pid is looked up **on the server**: the page never learns, and never names, a process.

The turn then ends the way any interrupted turn does: `turn.interrupted`, the
`prompt.submit` answer, the spinner gone. The session survives it. The signal goes to a
child that is busy *by this server's own bookkeeping* — a signal sent to an idle child
would land on whatever it did next.

The kernel reads the flag **between steps**, so a Stop pressed in the middle of a model
call lands when that call returns, exactly as the TUI's `esc` does. A stop is not a
socket teardown: the tool that was running finishes, and the turn ends after it.

The answer to a parked approval resumes the turn too, and that resumed turn is where a
model call can take as long as any other, so approving also shows the spinner and
leaves Stop available.
