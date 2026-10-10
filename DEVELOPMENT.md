# Development

The design lives in [`docs/`](docs/) — decisions in [`docs/decisions/`](docs/decisions/),
the current picture in [docs/04-system-overview.md](docs/04-system-overview.md), and
what to build against whom in [docs/05-parity-and-synthesis.md](docs/05-parity-and-synthesis.md).
This file is only about *running the code*.

## Toolchain

Rust stable. On this machine `static.rust-lang.org` is unreachable, so the
toolchain and crates come from the TUNA mirror:

```bash
# toolchain (already done here)
curl -sSfL -o /tmp/rustup-init \
  https://mirrors.tuna.tsinghua.edu.cn/rustup/rustup/dist/x86_64-unknown-linux-gnu/rustup-init
chmod +x /tmp/rustup-init
RUSTUP_DIST_SERVER=https://mirrors.tuna.tsinghua.edu.cn/rustup \
RUSTUP_UPDATE_ROOT=https://mirrors.tuna.tsinghua.edu.cn/rustup/rustup \
  /tmp/rustup-init -y --no-modify-path --profile minimal --default-toolchain stable

export PATH="$HOME/.cargo/bin:$PATH"   # add to ~/.bashrc to make it permanent
```

crates.io is mirrored in `~/.cargo/config.toml` (`[source.crates-io] replace-with
= "tuna"`); without it, dependency downloads fail here.

## Gates

One command runs exactly what CI runs, so "it passed here" means one thing:

```bash
./scripts/check.sh          # formatting, schema, layering, tests, release build
FAST=1 ./scripts/check.sh   # skip the release build (the slow one)
```

| Gate | Catches |
|---|---|
| `cargo fmt --check` | style drift; note that generated Rust is formatted **by the generator**, or the two would fight over the same bytes |
| `gen-schema.py --check` | generated types drifting from `crates/schema/protocol.toml` |
| `check-layering.sh` | a crate dependency pointing the wrong way (ADR-0004) |
| `cargo test --workspace --locked` | behaviour, including end-to-end tests against the real binary |
| `cargo build --release --locked` | code that compiles in debug and not with optimisations |

`.github/workflows/ci.yml` and `.gitea/workflows/ci.yml` call that script, so the
same gates run wherever the repository is hosted. Both install rustfmt (the
toolchain uses the `minimal` profile) and use the TUNA mirror, because this project
is built from behind one.

### The control plane (ADR-0012, ADR-0016)

Two binaries, one shared crate, and a deliberate division of labour:

| Binary | Who runs it | What it does |
|---|---|---|
| `loongagent` | every engineer | reads a signed pack, verifies it against a **pinned** key, runs the profile |
| `loongagent-control` | a **profile administrator** | `keygen`, `serve`, `publish`, `show`, `health` — distributes packs, and nothing else. `sign` exists for an air-gapped administrator |

Bringing one up (layout, key, tokens, systemd, troubleshooting):
**[docs/runbooks/control-plane.md](runbooks/control-plane.md)**, and
`scripts/control-plane-init.sh` writes the layout and prints the next commands.

The two programs keep their files in two roots, and a **draft pack is neither**: it
is versioned configuration, so it lives in the administrator's repository.

```bash
# once, on the control-plane host
./scripts/control-plane-init.sh --issuer acme-platform \
    --starter ~/config/loongagent/profiles.toml
#   /etc/loongagent-control/{control-key.pkcs8,client.token,admin.token}  mode 0600
#   /var/lib/loongagent-control/                                          the served pack

# the control plane holds the key and signs what administrators publish
loongagent-control serve --dir /var/lib/loongagent-control \
    --key /etc/loongagent-control/control-key.pkcs8 --issuer acme-platform \
    --client-token-file /etc/loongagent-control/client.token \
    --admin-token-file  /etc/loongagent-control/admin.token

# an administrator publishes the draft from their repository; the key stays on the server
loongagent-control publish --url https://control.acme.internal \
    --pack ~/config/loongagent/profiles.toml \
    --token-file /etc/loongagent-control/admin.token --version 2026.10.09-1

# on each machine
loongagent setup --url https://control.acme.internal --public-key <hex> --token env:CLIENT_TOKEN
loongagent profiles && loongagent use default && loongagent tui
```

Properties that are enforced, not documented:

- **One envelope, one crate.** `loong-pack` holds the envelope and its sign/verify
  path, and *both* binaries depend on it (ADR-0012 §1) — the two deploy
  independently, so a duplicated verifier would eventually disagree.
- **The signature covers everything but itself**: artifact, payload schema, version,
  issuer, scope, issue time, key id and payload. A pack cannot be replayed as
  another artifact or as another version.
- **The key is pinned out of band.** The public key travels with the install (image,
  MDM), never learned from the wire; a pack signed by another key is refused with
  both fingerprints printed. **The control plane holds the signing key** (owner
  decision, 2026-10-09) and signs the drafts administrators publish, so the private
  key never has to live on someone's workstation; started with `--public-key`, it
  refuses to run if the key does not derive it. A control plane without a key can
  still distribute, and says so in `GET /v1/health` (`can_sign`). The trade is
  explicit: a compromised control plane can mint packs.
- **Tokens are separated.** Reading needs the client token every install has;
  publishing needs the admin token. The client token cannot change policy.
- **The verified pack is the `control` layer**, ranked above the local org file and
  **locked**: project, user and session layers may narrow it, never rewrite it (an
  attempt is an error naming the pack). The one profile-related key a user owns is
  `control.profile` — the choice of bundle.
- **Offline is degraded, not dead, and the client decides**: at startup it refreshes the
  pack if the control plane answers within a few seconds, uses the last verified pack if
  it does not, and — if nothing has ever been synced — says what to do about it. There is
  no flag for this: it is a property of the client, not a mode someone has to know. A pack
  that *fails* verification never falls back to the cache. **A pack does not expire** (owner
  decision, 2026-10-09): a machine that never reconnects keeps running the pack it
  verified, so revocation has no teeth against it — an accepted trade, recorded in
  ADR-0016 §7.
- **Transport**: HTTPS everywhere except loopback; the client refuses to send a
  token in the clear to another machine.

**Not built, and deliberately** (ADR-0012 §5): identity/SSO, the model gateway
(provider keys, metering, shared prompt cache), rollup dashboards and the
audit/statistics uplink. The client only ever *reads*. Also not built: key rotation
and revocation policy, per-team scopes, and polling/push cadence — the envelope has
`key_id` and `version` for them, the policy does not exist yet.

### Sessions

A session **is** its log file, and the file's name is its id: eight characters from a
clock and a process, in an alphabet with the pairs people misread removed
(`23456789abcdefghjkmnpqrstuvwxyz`). No index to keep in step, `ls` is a valid way to
look, and `--session` takes the id, an unambiguous prefix, `latest`, or a path.

```
~/.loongagent/projects/<slug>/<id>.jsonl     the session
~/.loongagent/projects/<slug>/<id>.sqlite    its derived index (disposable)
```

- `loongagent tui` picks up the **newest** session in the project, and `--new` starts a
  fresh one instead. Because that default is invisible otherwise, the first line of the
  interface says which it was — `continuing session 36bxg8rd (17 events) — \`loongagent tui
  --new\` starts a fresh one`, `new session aatt5pww`, or `reopened session …` — and the
  usage text says the same thing where people look first;
- `loongagent sessions [--all] [--limit N]` lists ids with when, turns, events and the
  first prompt, marking the one a bare `tui` would pick up;
- `loongagent resume <id>` (or `resume latest`) reopens one;
- the first session ever written kept the file name `session.jsonl` and answers to the id
  `default`, so nothing had to be renamed to introduce ids;
- a session from another project can be reopened by id; the workspace stays where the
  command was run, which is worth knowing before editing.

**Forking is how a child session is made** (ADR-0015, level 2):

```bash
loongagent fork              # a child of the newest session here
loongagent fork <id> --at 42 # a child continuing from the parent's event 42
```

The child records its origin as its **first event** —
`session.forked { origin, parent_session, parent_seq }` — and **stores none of the
parent's events**: opening it replays the ancestors' prefix (up to their fork points,
oldest first) and then its own log. A child lives beside its parent, which is what makes
the reference a file name instead of a path that can rot; a missing ancestor refuses the
session and names the whole chain (ADR-0014: inoperable, not merely unexplained). The
listing's `from` column shows `parent@seq`.

**Deleting takes the branch** (ADR-0015: a child session and its descendants):

```bash
loongagent delete <id>            # refuses if that removes descendants
loongagent delete <id> --force    # the whole branch: the session, its children, theirs
```

The unit is the branch because a child without its ancestors cannot run: deleting a parent
alone would leave sessions that refuse to open. The command prints the branch first, so
the refusal is also the preview. Two guard rails, both from having been surprised before:

- a session **held open by another process** is refused and the pid is named. Deleting a
  file another process is appending to does not stop that process — it keeps writing to a
  name nobody can see, and the conversation is gone without a word. (Found by scanning
  `/proc/*/cmdline` for `--session <path>`; Linux-only, like the rest of this.)
- `--force` overrides both, and then the warning is printed rather than swallowed.

The derived `.sqlite` index goes with the log; it is disposable either way.

**Opening a session replays it.** The log is the record, so `Kernel::build` rebuilds the
conversation from it — ancestors first, then the session's own events — before any turn
runs. This was missing until 2026-10-09 and the symptom was quiet: a resumed session
appended to a conversation the model could not see, and answered "this is the first
message in our conversation" when asked what it had been told. Compaction replays from
the counts in its own event rather than re-deciding, so a changed policy cannot rewrite
what happened.

**Attaching replays the past.** `session.attach` reports how many events exist, and the
client then asks for them (`session.replay`) and renders them with the same code that
renders live events. Reopening a session used to give an empty screen that was quietly
appending to yesterday's conversation; the test that caught it is the one that reopens a
session and asserts the earlier prompt is on screen.

### Long output, timing, and getting things back

- A tool result is **bounded**: at most three lines and 100 columns each, then
  `… N more line(s), 8.2 KiB · /expand`. A chatty command must not turn the transcript
  into a wall of text. **`/expand`** asks the session's own log (through
  `session.replay`) for the most recent tool result in full, so the summary in the
  transcript is never the only copy of anything.
- Those markers are **reachable from the keyboard, with no mouse involved** (so
  left-drag selection and right-click copy keep working): `tab` / `shift-tab` walk the
  folds, the selected marker is drawn in reverse video and prefixed with `▸`, and `enter`
  opens that one. What opens is the log's own text — the transcript keeps the summary, so
  looking at something never rewrites the record. While it is open the output takes the
  transcript's place and `pgup`/`pgdn` walk through it, read the way a terminal reads a
  command's output: the end first, paging back towards the start. `esc` closes the output,
  `esc` again lets the fold go, and a prompt with text in it always sends on `enter` — a
  fold can never take the prompt's key away.
- A fold is named by **its place counting from the oldest**, which is the order the
  markers appear in on screen: `loong_tui::Client::expand_tool_result(at)` and
  `App::fold_at` are the two halves of one count, so no identifier has to be shown to
  anybody. From the oldest and not from the newest because the log only grows: a name that
  is asked for while a turn is running still means the same result when the answer comes
  back, where a count from the end would have quietly moved on to the next fold.
- **Opening a fold does not ask the log while a turn is running.** A turn owns the
  connection — `submit_with` is blocked reading the turn's own stream, the same fact that
  makes interrupting a signal rather than a message — so a request sent then would sit in
  the queue until the whole turn ended, which is exactly when nobody is reading anymore.
  It does not have to be sent: the full text of every tool result already came past on that
  stream, and the transcript is only a summary of it. The worker sends each fold's text
  with the summary line that stands for it (`FromWorker::Fold`), and the view keeps the
  newest `FOLD_KEEP` (32) of them, so `enter` is a local act. The folds a resumed session
  starts with are read once at startup (`Client::folded_results`) for the same reason. What
  the view does not hold — a fold older than the bound — is fetched from `session.replay`
  as before, which the running turn may delay; that is the only case where it does.
- Each tool result carries **how long the tool took** — `← ok all good  (2.4s)` — taken
  from the `time_ms` on the events rather than from a stopwatch in the interface, so it
  measures the work rather than the rendering. `loong_tui::Renderer` keeps the one piece
  of state that needs (the last call's time). When a turn ends, the transcript says how
  long the whole turn took as a `· turn took 42.1s` note.

### YOLO: opting out of the prompts, visibly

`/yolo` turns approval prompts off for the session. ADR-0007 §5 asks for exactly three
things, and all three are done:

- **it is an explicit opt-out** — a command someone types, not a config key. No layer may
  loosen the ladder (the narrow-only rule is tested), so this cannot arrive by inheritance
  from a project file or a control-plane pack;
- **opting out is itself an event** — `permissions.relaxed { auto_approve, reason }` is
  written before anything is allowed by it, and a ruling that would have asked is recorded
  as `verdict: allow, rule: "yolo:<class>"`, so a session that never asked says why;
- **it is visible in the UI** — the input box and the footer both carry `⚠ YOLO`, in red,
  for as long as it is on. The dangerous state is the one nobody notices.

**The sandbox is a different axis and is untouched**: YOLO removes the *question*, not the
*boundary*. A write outside the workspace still cannot happen — what changes is that the
tool runs and returns its refusal, instead of the turn stopping to ask.

### Stopping a turn

`Ctrl-C` (or `Esc`) while a turn is running **stops the turn**; with nothing running it
leaves, which is the habit people bring from every other tool, and the hint line says
which one it will be. Interrupting is cooperative and lands **at the next step boundary**
— after the tool that is already running finishes, before the next model call. A model
request already in flight is waited for; nothing is left half-done and nothing is
silently abandoned.

How it travels: the TUI owns the server process, so it sends **SIGINT** rather than a
protocol message. The client is blocked reading the turn's own stream, so a message could
not be sent from the same thread, and a process it spawned already has a way to say
"stop". The server installs one handler (`Server::catch_interrupts`) that sets an
`AtomicBool` the kernel reads — `Kernel::interruptor`/`use_interruptor` — and the turn
records **`turn.interrupted`** before returning, so the log says why a session has no
answer. The stop belongs to the turn it was asked for: the flag is cleared when the next
turn starts, and the process stays alive for it.

Verified end to end with a model that runs `sleep 4` as a tool: the log ends
`… tool.result, turn.interrupted`, the model is never asked a second time, and the
transcript shows `⏹ interrupted — stopped by you` in yellow — a stop the person asked for,
so neither an error nor noise.

### Colour

The transcript is painted by classification rather than by the sender: `classify` reads
the line's own vocabulary (the prefixes `loong-tui`'s renderer writes) and `style_for`
turns that into a style, so there is one palette for the whole surface.

| Line | Colour | Why |
|---|---|---|
| `you: …` | the label in cyan, the words plain | the label marks where a prompt began when scrolling back; the person's own words stay readable |
| `→ tool …` | teal | the agent is doing something |
| `← ok …` | green, dim | a result is evidence, not news |
| `← failed …` | red | it did not work |
| `! approval required …` | yellow, bold | the person has to answer |
| `! …` | red, bold | something went wrong |
| `· policy: …` | violet | a decision about permission, which is worth finding later |
| `· …` | the terminal's own foreground, dimmed | bookkeeping: skills, tokens, compaction |
| anything else | default | the model's own answer, left alone |

The palette names colours and never invents RGB, because a fixed tone is picked for one
background and unreadable on the other. The same rule is why the quiet lines are the
**terminal's own foreground, dimmed**, and not `DarkGray`: bright black *is* the background
on a dark theme, so the lines saying which skills loaded and which profile is in use were
drawn into the wall. Reported from a real screen, where scrolling at startup appeared to
grey them out. `MARK_QUIET` is that decision, and a test pins both the style and the cells
that reach the screen.

The notes strip follows the same idea by severity — a line containing `warning` is yellow,
`error` or `refused` is red, everything else quiet — and the streaming preview is quiet
italic, so text still being written is never mistaken for the settled answer.

Every other secondary tone in the surface is that one style — the panel's details, the
context panel's labels, the first-run form's own help line, and a cleared entry. `DarkGray`
appeared in eleven of them and was wrong in all eleven for the same reason. It was worst in
**the answers that were not chosen** in a decision menu: they were drawn in the background
colour too, so a three-answer menu read as a single answer. Those are `default` now — the
one thing marking the highlighted one is that it is coloured and bold, which only works if
the others can be seen.

A test renders real events through `loong_tui::render_event` and asserts that every
**marked** line is classified: only the model's prose is allowed to fall through to
`Answer`. Another reads the cell buffer and asserts the colours actually land (the text
helper throws styles away on purpose, so there is a second helper that does not).

### The mark

Three rows at the top when the terminal is at least 48 columns and 18 rows — the mark the
owner specified, kept verbatim:

```
╭───────༺ ᛭ ༻────────────╮
╞═══════【 龍 】════════════╡
╰───────༻ ᛭ ༺────────────╯
```

A frame with room to be imposing when the terminal has it — five rows, double rules, the
character centred with air around it, and the owner's ornaments on the top and bottom rules:

    ╔═══════════════༺ ᛭ ༻═══════════════╗
    ║                                   ║     loongagent
    ║               【 龍 】              ║
    ║                                   ║     ⟬ ᛭ 兴云吐雾 ⋄ 隐介藏形 ᛭ ⟭
    ╚═══════════════༻ ᛭ ༺═══════════════╝

The words are the owner's and unchanged; only the frame grew. The rules are **built** from the
interior width (`MARK_INNER`) with the ornaments measured at draw time, so the four sides line
up even when a rune or a Tibetan mark measures differently than expected on a given terminal.
`/logo` puts it away, and a terminal under 52 columns by 22 rows never gets it — five rows is
a real cost to the conversation, so it is only paid where there is room.

The inscription takes the first form that fits — with ornaments (28), without them (19), then
four characters (8) — and there is nothing on the bottom rule: the inscription carries the
thought, and a mark that explains itself twice talks too much.

**Colour comes from the theme, not from us.** `龍` and the wordmark are the terminal's own
foreground in bold; the frame is ANSI magenta; the transcript's accents are ANSI cyan and
magenta. Nothing in the palette is an absolute RGB or a literal white — the owner runs a
light background, where `Color::White` is invisible and a fixed pale teal is barely there.

### Watching a turn happen

A turn streams. The kernel calls a sink for **every event the moment it is written**
(`Kernel::set_event_sink`), and the server puts that event and the model's deltas on the
connection immediately, so a surface can show work while it is still happening rather than
when it is over. Two rules keep it honest:

- nothing is announced that was not written first — the event is durable before the sink
  sees it, so a client can never show a decision the log does not have (ADR-0009);
- a **delta** is still transient: written to the connection and nowhere else, because the
  log holds the finished message and a client that misses a fragment has missed nothing.

`Server::handle_line` keeps its old contract for tests and small programs — it collects
through an in-memory connection and hands back everything in order, response last — while
`handle_line_streaming(line, &Out)` writes as it goes and returns only the response.

In the TUI that shows up as the work appearing line by line (`→ read_file note.txt`,
`← ok …`) and an input title that says what is running and for how long (`⠹ working 12s ·
shell: cargo test`), because "working…" for two minutes tells nobody whether it is thinking
or stuck. Watching it work needs two things:

- **the mark turns.** The title carries a braille frame taken from the clock
  (`spinner_frame`), so a line whose words have not changed for ten seconds still proves the
  process is alive. Reading the frame from elapsed time rather than from a counter means a
  missed redraw cannot freeze it, and a test can name the frame it wants instead of racing a
  timer.
- **the answer being written is the end of the transcript, not a thing below it.** It used
  to be appended after a full window, so the box drew the *beginning* of the answer and
  clipped the newest lines off the bottom: a long answer appeared to stop moving after one
  screenful, which is the opposite of watching it be written. The window moves with the live
  lines now. The preview stays dim and italic, and the recorded message replaces it — the
  preview was never the record.

What a line is worth reading is bounded, not the event stream itself:

- **a call shows what it carries, not only where it points.** `→ write_file game.html` says
  nothing about the page, so the call's own text follows: the command's own lines, and for
  an edit or a write the text being put in, marked the way a diff marks it. It is bounded by
  `CALL_ROWS` (8) and `CALL_COLUMNS` (160), and its elision note deliberately does **not**
  look like a fold pointer — folds are the log's tool *results*, counted from the oldest, so
  a marker on a call would put the keyboard on a line the log cannot answer for.
- **a result keeps `RESULT_ROWS` (10) lines and `RESULT_COLUMNS` (160) columns** before
  folding, and the marker says which of the two happened. It was three lines at 100 columns,
  and a clipped *line* produced `… 0 more line(s)` — a pointer that opened nothing.

`run` and `repl` still print the turn when it finishes; handing them the same live callback
is a small change, deliberately left until the surface that was asked for works.

### What the TUI answers to

| Key / command | What it does |
|---|---|
| `enter` | send the line |
| *(long input)* | wraps at the box edge; the box grows to as many rows as needed (at most eight), and the caret stays on screen |
| `/exit`, `/quit`, `/q` | leave — handled by the **interface**, never sent to the agent |
| `/logo` | show or hide the mark at the top |
| `ctrl-c`, `ctrl-d` | leave |
| `f2` | the context panel |
| `pgup`/`pgdn`, mouse wheel | scroll the conversation; `ctrl-pgup`/`ctrl-pgdn` jump to the ends |
| `tab` / `shift-tab` | walk the folds (`… N more line(s)` markers); the selected one is drawn in reverse video behind `▸` |
| `enter` on a selected fold | open it in full, and again to put it away; a prompt with text still sends on `enter` |
| `esc` | close an open fold, then let the selection go — it only leaves when there is neither |
| `↑`/`↓` (or `k`/`j`), `enter` | choose an answer when a decision is waiting; `esc` denies, and `a`/`d` still work |
| `/yolo` | turn approval prompts **off** for this session; `/yolo` again turns them back on |
| `←`/`→`/`home`/`end`/`del`, `ctrl-a/e/u/k/w` | edit the line, by character so CJK works |

Scrolling stops where the transcript starts: `↑` can only reveal what is above the window,
so a conversation shorter than the window stays where it is instead of pushing its newest
lines off the end. The count is capped by the window at draw time, so the `[n/total]` hint
and the next key press say the same thing the screen does.

`/exit` is in that table rather than in the skills because quitting is the surface's
business: a command that means "stop this program" has no meaning to a model. Everything
else beginning with `/` — `/help`, workspace skills, MCP prompts — goes to the server,
where it is resolved, audited and recorded. The boundary is tested from both sides.

On the way out the session path is printed, so leaving is not a dead end.

### The debug panel (F2)

`F2` in the TUI opens a context panel on the right: the tokens the next request will
carry against the budget, `messages`/`tools`/`compactions`, a per-category breakdown
largest-first, and every ledger entry with its source and cost. `p` marks a pinned entry
(the system prompt, the tool descriptions, a decision), `c` one whose body was replaced
to free space (ADR-0006 §4).

Two properties worth keeping:

- **Reading it changes nothing.** `context.explain` is a question about the session, not
  a fact in it: no event is recorded, so looking cannot alter what you are looking at
  (asserted in the kernel and server tests).
- **It works offline.** The panel reads the kernel, not the control plane, so it looks
  the same with no network at all.

Opening the panel asks for the view immediately, and every finished turn refreshes it,
so it is never an empty box and never stale.

### One root for the client

Everything the local client keeps is under **`~/.loongagent/`** (`$LOONGAGENT_HOME`
moves it for CI and scripts), and the naming rule is short enough to remember:

- **files directly in it are configuration** — `profile.toml` (user layer),
  `control.toml` (control-plane address + pinned key), `env` (secrets, `0600`),
  `client.token` (a per-user copy of the read token);
- **its subdirectories are state** — `projects/<project>/session.jsonl`,
  `control/<origin>/profiles.json` (the verified pack cache).

The control *plane* is a service, so its files stay where a service's belong:
`/etc/loongagent-control` for what an administrator authors and the service must read
at boot, `/var/lib/loongagent-control` for what it writes.

### The draft is committed; the secret never is

A profile pack is configuration — reviewed, versioned, diffed — so it carries a
**placeholder** and the value lives on each machine:

```toml
api_key = "env:DEEPSEEK_API_KEY"        # what is committed
```

In a **pack** a literal is accepted, visibly (ADR-0008 §5 amendment, 2026-10-09): the
deployment can ship the credential until identity and the model gateway exist. It warns
at publish, in the publish response, in `GET /v1/health` (`plaintext_secrets`), and on
every client; `check --strict` fails on it. It lands in the draft, the control plane's
`profiles.json`, and each install's pack cache — never in the session log, the index, or
`control.toml`.

In a **local configuration file** (project, user, session, org) a literal is still
refused in three places, so a repository or a dotfile cannot carry a key: at the
administrator's own `publish --pack`, at the control plane when it validates a draft,
and when a client loads a pack.

`loongagent-control check --pack FILE` runs that validation offline — no network, no
signature — so it can sit in a pre-commit hook. A secret pasted into a draft that is
under version control is already in the history by the time `publish` says no.

The part after the prefix is checked too, because a prefix is not a reference:
`env:sk-4f06…` starts with `env:` and still puts the key in the pack, in every client's
cache, and in git as soon as the draft is committed. `env:NAME` must be a variable
*name* — `UPPER_SNAKE`, letters/digits/underscores, no long unbroken run of characters
that a secret would have — and `file:PATH#NAME` is checked the same way. A lower-case
variable name is therefore refused: use the convention. The reference travels
inside the signed envelope — `profiles.json` holds `env:DEEPSEEK_API_KEY`, never a
value — and the control plane never resolves it: the secret stays on the client.

The client reads environment files before resolving any reference:

| Order | File | Rule |
|---|---|---|
| 1 | whatever the shell exported | always wins; an env file never overrides it |
| 2 | `~/.loongagent/env` | the recommended home for the secret, mode `0600` (`NAME=value`, `NAME: value`, or a TOML `[keys]` table) |
| 3 | `<workspace>/.env` | convenient when the repository you work in has one |

The user's file wins over the workspace's, so a checked-out repository cannot replace
a credential the user chose. All three accept `NAME=value`, `NAME: value`,
`export NAME=value` and quoted values, and only names are ever reported
(`[config] environment from …: DEEPSEEK_API_KEY`).

**The signing key must never be in a repository**, and the tools enforce it: `keygen`
refuses to write a key inside a git work tree (`--force` writes it and prints the
`.gitignore` lines), `scripts/control-plane-init.sh` refuses the same way (`FORCE=1`),
and `publish --pack` refuses while a secret is *tracked* in the repository holding the
draft — naming the `git rm --cached` fix — while an untracked, unignored secret gets a
warning. Five integration tests in `crates/control/tests/` cover those paths.

`file:PATH` / `file:PATH#KEY` remain for a machine that keeps its secret elsewhere
(`NAME: value`, `NAME=value`, or a TOML `[keys]` table are all understood). The
control-plane **token** must be a reference too, for the same reason: `control.toml`
lives in `~/.config`, which dotfiles repositories do commit.

### Profiles: several models, and a router you can explain

A profile is a **named bundle** an administrator publishes (T-07), and the payload
of a signed pack is where it lives:

```toml
default_profile = "default"

[[profiles]]
name = "default"
description = "Balanced: a cheap model for the loop, escalating past 150k tokens."
  [[profiles.models]]
  name = "fast"
  base_url = "https://api.deepseek.com"
  model = "<the id from your provider's docs>"
  api_key = "env:DEEPSEEK_API_KEY"     # or gateway: / file:PATH#KEY; never a literal
  [profiles.router]
  default = "fast"
  [[profiles.router.rules]]
  when = "context_tokens > 150000"
  use = "long"
```

The same shape works as a local, unmanaged profile — one model, no control plane.
The format:

```toml
[[models]]
name = "fast"                 # what you and the router call it
kind = "openai-compat"        # the only adapter today
base_url = "https://api.deepseek.com"
model = "<the id from your provider's docs>"
api_key = "file:credentials.toml#DEEPSEEK_API_KEY"   # or env:NAME; never a literal
budget_tokens = 128000

[router]
default = "fast"
[[router.rules]]              # first match wins, per model call
when = "context_tokens > 150000"
use  = "long"
```

What a rule may read is deliberately small — `context_tokens` and `step` — because a
model choice nobody can explain is a model choice nobody can debug. The rule that
fired is written to the session log as `model.selected`, together with the estimate
it used, so "why this model?" has an answer afterwards.

Merge rules, in the spirit of ADR-0008 §2: a **project layer may not define models**
(a repository must not choose where prompts go), though it may route among models
that exist; redefining a model in a more local layer takes the endpoint but **not a
larger budget** (ceilings only narrow); `router.default` and `router.rules` are
free-override, with the project layer's choices checked against the models that exist.

Secrets are references: `env:NAME`, `file:PATH#KEY` (relative paths resolve beside the
profile in use), and `keychain:`/`gateway:` — which are recognised and refused with a
reason, because no credential store is wired yet (ADR-0008 §5). `loongagent setup`
stores a pasted key in `credentials.toml` with mode 0600 and writes only the
reference into the profile.

### Sessions live in the state directory

`~/.loongagent/projects/<project>/session.jsonl`, keyed by the project
path (readable slug plus a hash, so two paths cannot quietly share a session). Nobody
has to pass `--session`; doing so overrides it.

### The toolchain, and what is pinned where

`rust-toolchain.toml` pins the **channel** (`stable`) so a developer's machine needs
no network to start. CI pins the **exact version** (`RUST_VERSION` in the workflow),
because an artifact must be traceable to a compiler — and `scripts/release.sh`
records that version in the artifact's `BUILD` file.

Pinning the exact version in `rust-toolchain.toml` was tried and reverted: rustup
treats it as a separate toolchain and tried to download it (the mirror 404s that
tarball here). If your mirror serves versioned tarballs, pinning it in the file is
the better end state.

## Everyday commands

```bash
cargo build --workspace            # ~1 min cold, seconds warm
cargo test  --workspace            # 69 tests
./scripts/check-layering.sh        # asserts crate dependencies point down only
./scripts/gen-schema.py            # regenerate from crates/schema/protocol.toml
./scripts/gen-schema.py --check    # CI gate: generated files must be current
```

## Layout

Nine of the eleven crates are layers; dependencies may only point **down**.

| Crate | Layer | Owns |
|---|---|---|
| `loong-schema` | 0 | `protocol.toml` (the only hand-written wire definition) + generated Rust types |
| `loong-eventlog` | 1 | append-only JSONL truth; the `ignorable` envelope is enforced, not trusted |
| `loong-index` | 1 | derived, disposable SQLite (counts, search, event-tree children) |
| `loong-providers` | 1 | the `Model` trait + provider doubles |
| `loong-tools` | 1 | MCP-shaped tools; action class per **call**; path classification |
| `loong-sandbox` | 1 | the PDP: allow / ask / deny **plus the rule that fired**; narrow-only tightening |
| `loong-kernel` | 2 | the fixed kernel: turn loop, context ledger (the only write path), wiring |
| `loong-protocol` | 2 | JSON-RPC 2.0 envelopes and framing |
| `loong-server` | 3 | the local server: sessions live here, events are streamed |
| `loong-tui` | 3 | the terminal client: renders events, sends intents, zero agent logic |
| `loongagent` | 4 | the binary: `server` / `tui` / `run` |

## Running it

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo build

# headless, with a provider double (no keys, no network)
./target/debug/loongagent run "hello" --session /tmp/s.jsonl --cwd /tmp --model echo
./target/debug/loongagent run "write it" --session /tmp/w.jsonl --cwd /tmp --model demo-write
./target/debug/loongagent run "escape"  --session /tmp/e.jsonl --cwd /tmp --model demo-escape   # exits 3: needs a human

# full-screen: the TUI spawns its own server child and talks JSON-RPC over pipes
./target/debug/loongagent tui --session /tmp/s.jsonl --cwd /tmp --model demo-escape

# line mode: same client, no TTY needed (pipes, CI, scripts)
echo "hello" | ./target/debug/loongagent repl --session /tmp/s.jsonl --cwd /tmp --model echo

# the server alone, for scripting
./target/debug/loongagent server --stdio --session /tmp/s.jsonl --cwd /tmp --model echo

# a real provider: any OpenAI-compatible Chat Completions endpoint, http:// or https://
# (vLLM, LM Studio, llama.cpp --server, Ollama /v1, an intranet gateway)
LOONGAGENT_BASE_URL=http://127.0.0.1:8000/v1 LOONGAGENT_MODEL=qwen2.5-coder \
  ./target/debug/loongagent run "summarise README.md" --session /tmp/r.jsonl --cwd . --model openai-compat
```

For a hosted provider, set the base URL to its https endpoint:

```bash
LOONGAGENT_BASE_URL=https://api.deepseek.com LOONGAGENT_MODEL=deepseek-chat \
LOONGAGENT_API_KEY=sk-… ./target/debug/loongagent web --cwd .
```

TLS is rustls with the `ring` backend and the webpki root store; a certificate that
does not match the host is refused. Response streaming is not implemented, so the
answer appears when the turn completes.

`README.md` is the short version for someone trying it, and `scripts/demo.sh`
runs a scripted model with no API key at all (`./scripts/demo.sh [web|tui|run]`).

Sessions are plain JSONL — `cat` them. `LOONGAGENT_SERVER_BIN` overrides which
binary the TUI spawns as its server (used by the end-to-end test).

## What works today

- **Schema-first protocol**: one IDL generates the Rust types, the TypeScript
  types, sample payloads, and a round-trip conformance test per struct.
- **The event log is the truth**: append-only, sequence-checked, with the
  forward-compatibility envelope. Unknown *ignorable* kinds are skipped (and
  reported); unknown non-ignorable kinds fail loudly; a record whose marker
  contradicts the schema is rejected.
- **The loop**: user → model → tool call → PDP verdict → execute / ask / deny →
  results fed back, bounded by a step limit. Approvals stop the turn and resume
  from an event. A **step is a model completion**, so every tool round trip costs one: the
  default (`DEFAULT_MAX_STEPS`, 100) is a backstop against a model that *spins*, not a
  judgement about how large a task may be. It was 8, which is how an agent writing one page
  came to stop mid-edit — read, edit, run, fix is already four of them. `--max-steps N` sets
  it, `--max-steps 0` means no limit, and a config layer may still narrow `max_steps`
  (ADR-0008 §2). Cost is bounded by the token budget and compaction, which are the limits a
  person actually chooses. Other agent loops treat it the same way: kimi-cli's
  `loop_control.max_steps_per_turn` documents "unset or `0` means unlimited", and Claude
  Code's `CLAUDE_CODE_MAX_TURNS` applies only when no explicit limit is passed. Because the
  default comes from no layer, `config show` prints it with source `<defaults>` — a limit
  nobody can see is how a turn came to stop at a number no command mentioned.
- **The ledger**: every block that reaches the model is an entry with category,
  source, tokens and lifetime; eviction drops stale tool output before prose and
  never touches pinned entries; `explain()` answers "why is this in my context?".
- **Compaction** (ADR-0006 §4), driven before every model call: once the input would
  pass the ceiling, stale tool output is **cleared** first (its body replaced by a marker —
  the model can re-run a command, it cannot re-read a decision), and only if that
  is not enough are the oldest whole exchanges **dropped**. The newest window
  (`--keep-newest`, default 20k) always stays verbatim, the most recent entry is
  never cleared, and a tool result is never separated from the call that produced
  it — a conversation that splits them is invalid to every provider. Each
  compaction is an event with before/after tokens; a compaction that could free
  nothing is not an event at all.
- **Two ceilings, and the stricter wins**, because either one overflowing ends the same
  way. The **budget** is what a person or a pack chose — a cap on **one request's input**,
  which is why the name is about spending while the number is about size — and it fires at
  `compact_at_percent` (default 80). The **window** is what the model can physically hold:
  a model declares `max_context_size`, and the input may reach the window minus
  `reserved_context_size` (default 16 Ki), the room the answer needs. Neither is a
  cumulative quota: what is measured before every model call is the size of the prompt about
  to be sent (the kept history included), which compaction then brings back under the line.
  A session may spend far more than either number in total — nothing meters that. The reserve
  is the one
  narrow-only number whose strictness runs the other way: keeping *more* room is the
  conservative choice, so a layer may raise it and never shrink it (`stricter_for`).
  `--budget N` sets the budget (0 = no cap of your own; a configuration's still holds).
- **A budget written on a model counts too.** A pack may put `budget_tokens` on each model
  instead of on the session, and for a while that value was parsed, merged, and then read by
  nothing: the pack this repository ships declares `budget_tokens = 128000` on its only
  model, and the session it configured ran with *no* budget at all — compaction never ran,
  and the context grew until the provider refused it. `effective_budget` takes the strictest
  of the caller's request, the session key, and what the models declare, because a model the
  router may pick is a model the cap has to cover. When nothing bounds the context — no
  budget anywhere and no window on any model — the CLI says so at startup, since the
  alternative is discovering it as a provider error in a long session.
- **Six built-in tools**, all behind one interface: `read_file`, `write_file`,
  **`edit`**, **`grep`**, **`glob`**, `shell`. `edit` refuses a missing or
  ambiguous `old_string` (with the counts and line numbers) instead of guessing —
  a wrong guess silently corrupts a file. `grep` and `glob` are read-only by
  construction, so they classify as investigate actions and cost no approval;
  both bound their own output, skip `.git` / `target` / `node_modules` and binary
  files, because an agent that reads 40 MB of matches has spent its context on
  noise.
- **Two enforcement points**: the PDP decides, the tool runner refuses to write
  outside the workspace even if a call is allowed.
- **A real OS-level PEP, applied to tool children**: Landlock (ABI probe + a
  ruleset that allows reads everywhere and writes only under the workspace). It
  needs no privileges and is inherited by child processes. The kernel prepares
  the policy in the parent (`WorkspacePolicy`, allocation-free on the child's
  success path) and installs it with `pre_exec`, so the restriction lands in the
  forked child and can never leak into the server. `loongagent sandbox
  self-test` *proves* it by running a confined child that must fail to write
  outside and succeed inside — it never reports "available" without
  demonstrating denial. The server runs that self-test before its first tool
  call: a broken mechanism refuses startup, a missing one is a loud warning.
- **Layered configuration** (`loong-config`, ADR-0008): built-in defaults < org
  < project < user < session, merged **per key kind** rather than by "last writer
  wins":
  | kind | keys | rule |
  |---|---|---|
  | free-override | `model`, `base_url`, `profile` | the most local layer that sets it wins |
  | narrow-only | `budget_tokens`, `keep_newest_tokens`, `max_steps`, `compact_at_percent`, `max_context_size`, `reserved_context_size`, `[permissions]` | strictest wins; a lower-authority layer may only narrow |
  | add-only | `skill_dirs`, `disable_skills` | union across layers |
  | reference | `api_key` | names a source (`env:NAME`); a literal secret is refused |

  The built-in ladder is the *ceiling of permissiveness*, so a profile cannot make
  `out_of_bounds` free; the PDP names the layer that narrowed a rule
  (`profile:project:write_workspace`); every key answers "why is this the value?"
  via `loongagent config show` / `config explain <key>`. Two rules come from the
  threat model rather than the table: **the project layer may not set `model`,
  `base_url` or `api_key`** (a repository must not choose where prompts go), and a
  project layer's `skill_dirs` must stay inside the workspace. Layer files:
  `/etc/loongagent/profile.toml` (an administrator's file — the placeholder for
  ADR-0008's signed org pack), `<workspace>/.loongagent/profile.toml`,
  `~/.loongagent/profile.toml`, and `--profile FILE` as the session
  layer.
- **Skills and commands** (`loong-skills`, T-18): one concept with two
  spellings, `.loongagent/skills/<name>/SKILL.md` and
  `.loongagent/commands/<name>.md`. The **description** enters context (in the
  system prompt, with the file it came from); the **body** loads only on invoke —
  so a workspace can carry twenty workflows and pay for twenty lines. `/name args`
  expands `$ARGUMENTS` and `$1`…`$9`, and appends the arguments if the body uses no
  placeholder. Invocation parsing is strict on purpose: `/etc/hosts is wrong` is a
  sentence about a path, not a command, and an unknown `/reveiw` is **refused with
  a pointer to `/help`** rather than guessed at. The model loads a body by name
  through the `skill` tool. `/help` is always available and lists what exists.
  Every load and every invocation is an event naming its source file, so a
  surprising instruction is traceable to a path rather than being anonymous prompt.
  `loongagent skills list` shows the catalogue and each body's token cost.
- **A system prompt slot**: the first message is now a short frame plus the skill
  catalogue. It is a pinned `SystemPrompt` ledger entry, it never counts as
  compactable history, and compaction refuses to drop it.
- **Streaming answers**: the adapter speaks Server-Sent Events (chunked replies,
  reassembled across chunk boundaries — including tool-call arguments, which arrive
  in fragments), and the kernel hands each fragment to a surface as a `model.delta`
  **transient notification**: written to the client and nowhere else, because the
  finished `message.appended` is the record. A server that ignores `stream: true`
  and answers with plain JSON is still understood, and a model that cannot stream
  at all falls back to the trait's default (one delta carrying the whole answer).
- **A browser surface** (`loong-web`): `loongagent web` serves one self-contained
  page (embedded in the binary; no bundler, no CDN) and carries the *same* JSON-RPC
  lines over a WebSocket, so the browser speaks exactly what the TUI speaks. The
  page needs a **token** on the handshake — any tab can open `ws://127.0.0.1:…`, so
  a loopback bind is not authorisation — and the token travels in the URL fragment,
  which browsers never send to a server. A request/response maps onto one
  WebSocket message, which is what the server's "stream the events, then answer"
  design was for. One browser at a time drives the session (the connection is
  serialised behind the session); a second tab waits its turn.
- **Two terminal surfaces** (`loong-tui-app`, ADR-0009/0013): `loongagent tui` is
  a ratatui full-screen UI — a scrollable conversation, an input line, approval
  prompts that repurpose it, a diagnostics pane fed by the server process's stderr
  (so startup notes are drawn, never dumped over the UI), and a worker thread that
  owns the blocking client so a turn never freezes the screen. `loongagent repl`
  is the same client in line mode, which is what a pipe or CI needs; `tui` refuses
  a pipe with a message that names that way out. The UI's state machine and drawing
  are tested against ratatui's `TestBackend` (no TTY required), and the real
  terminal path was verified in a PTY.
- **MCP servers** (`loong-mcp`, T-15): a trusted `.loongagent/mcp.toml` names
  stdio servers to start; their tools join the **same registry** as the built-ins
  (`mcp__<server>__<tool>`), so they go through the same PDP verdict, emit the
  same `tool.call`/`tool.result` events, and appear in the same audit trail. Their
  **prompts become commands** (`mcp__<server>__<prompt>`) in the same catalogue as
  workspace skills, with their declared arguments as the hint; the body is fetched
  on invoke, so a server can offer twenty workflows at the cost of twenty lines.
  Capabilities are negotiated: a server that advertises no prompts is never sent
  `prompts/list`. Every
  read has a deadline (a reader thread plus a timeout, so a wedged server fails the
  call instead of hanging the session), and each tool's **action class is declared**
  in the manifest — per server, with per-tool overrides, defaulting to the cautious
  `out_of_bounds` (asks every time). A server entry names a command to run, so it
  sits behind the same trust gate as hooks: inert until `loongagent mcp trust`.
- **Lifecycle hooks** (`loong-hooks`): a hook answers one question at one moment
  — allow, ask, or deny — and nothing else. Six moments fire today:
  `session_start`, `user_prompt_submit`, `pre_tool`, `post_tool`,
  `permission_requested`, `permission_decided`. Three kinds behind **one verdict
  contract**: `shell` (a script receiving the event as JSON on stdin; exit 2
  denies with stderr as the reason, or print `{"verdict":"ask","reason":"…"}`),
  `http` (an endpoint, **off** until the manifest sets `allow_http_hooks = true`),
  and `mcp` (a tool on a trusted MCP server, answering with the same JSON — so an
  external policy service can gate a built-in tool). Verdicts aggregate
  monotonically — deny > ask > allow — so hooks can only tighten what the session
  may do, and a hook that cannot answer fails **closed** by default. Every answer
  is a `hook.fired` event, whether or not it changed the outcome.
  Repo-shipped hooks are treated as repository content: they stay inert until
  `loongagent hooks trust` records the exact bytes, and editing the manifest or a
  script re-arms the question (`loongagent hooks list` shows the state).
- **A shell tool with a process-tree teardown**: `shell` runs a command in the
  workspace, bounds each output stream (default 64 KiB), and kills the whole
  process group on timeout (killing only `sh` would leave `sleep 30` running and
  hold the output open). Its action class is computed from the *command*, so the
  same tool is investigate for `cat`, a workspace write for `touch`, and out of
  bounds for `curl` or `sudo` — the classifier is an autonomy heuristic, never a
  security boundary; the PEP is the boundary.
- **The derived index is wired into the session**: every append feeds it, a
  failure marks it stale (and queries then refuse rather than lie), and reopening
  rebuilds it from the log — deleting it is always safe.
- **One real provider adapter**: `openai-compat` translates our `Model` interface
  to the Chat Completions wire protocol (messages, tools, tool-call arguments as
  JSON strings, usage) and back. Six tests drive it against a local HTTP server,
  including the error paths.
- **The vertical slice**, covered by three end-to-end tests against the real
  binary: a tool writes inside the workspace and the streamed events equal the
  log; an out-of-bounds write asks first and a denial is recorded without
  touching anything; the TUI client drives a real server. A manual run against a
  fake OpenAI-compatible server exercises the whole chain — CLI → server → kernel
  → adapter → HTTP → tool → PDP → back — and the log holds all eight events.

## What is not done yet (honest list)

1. **Cost is not priced**: `usd_micros: 0` until pricing lands, and there is no
   tokenizer (the ledger estimates tokens at ~4 chars). Tool-call deltas are
   assembled but not shown live — a surface sees the call when it is complete.
2. **Network egress is not confined** — Landlock covers the filesystem only;
   blocking egress needs seccomp or a network namespace (neither is available
   unprivileged in this container: `unshare -Urn` is refused). The `shell`
   tool's classifier scores network commands as out-of-bounds, which is an
   autonomy decision, not enforcement. macOS seatbelt is not implemented.
   Caveat: a hardened deployment must not treat the classifier as a boundary.
3. **Compaction does not summarize yet** — it clears and drops, but a
   model-made summary of what it drops is the other half of ADR-0006 §4, and a
   session that must shrink a lot currently loses the oldest turns instead.
4. **The TUI is single-session and minimal** — no mouse, no copy mode, no split
   panes, no session picker, no syntax highlighting. `--budget` and friends are
   flags, not profiles.
5. **`grep`/`glob` do not honour `.gitignore` yet** — they use a small explicit
   skip list instead. Web tools (`web_fetch`, `web_search`) are not built, and
   `grep` reads whole files (no streaming) under a 4 MiB per-file cap.
6. **Skills are not yet gated or scoped**: their descriptions enter every
   session's context (an injection surface, mitigated by provenance events, not by
   a trust gate), and per-skill `allowed-tools` is parsed but not enforced. An
   MCP prompt's *description* likewise enters context as soon as its server is
   trusted — the server's manifest is gated, the prompt text is not reviewable in
   advance.
7. **MCP is stdio-only and eager**: no HTTP transport, no resources, no
   sampling, and server-initiated requests are refused rather than served. Tools
   are listed at session start and every schema is handed to the model.
8. **Post-tool hook verdicts are recorded but cannot undo anything**, and hooks
   deliberately cannot rewrite a tool call — the moment they could, the loop would
   stop being small enough to audit.
9. **The TUI has no in-app control-plane address form**: it prints the exact command
   (`loongagent setup --url …`) instead of asking for the address inside the UI. The
   remaining half of the earlier TUI request list.
10. **The TUI has no debug pane yet**: the context/ledger view (what the model is
   actually being sent, entry by entry, with its token categories) is requested and
   not built. `loongagent config show` and `cat .loongagent/session.jsonl` are the
   current substitutes.
11. **`cargo clippy` is not a gate yet**: the code has never had a lint sweep, and
   adding a gate that fails on the first run is worse than not having it. It is the
   next hygiene step after the formatter sweep.
11. **Configuration is layered but not yet distributed**: ADR-0008's signed pack
   envelope is not implemented, so the org layer is a plain file an administrator
   owns and nothing is tamper-resistant; `keychain:` and `gateway:` credential
   references are recognised and refused (no credential store is wired), and there
   is no `--set key=value` for ad-hoc session overrides.
12. **Not started**: session
   export/import, WASM extensions, the web client, compaction, the control plane.

## After changing code

`./scripts/install-local.sh` installs both binaries where PATH finds them and prints the
commit each was built from, next to the repository HEAD. `--version` names that commit,
because a stale `~/.cargo/bin` copy has quietly run old rules three times here — once
publishing with the wrong issuer, once not knowing a subcommand, once refusing a pack
the new code accepts — and a long-running control plane is the same trap in a process:
**restart it after installing.**

## Cutting a release

```bash
./scripts/release.sh      # refuses a dirty tree
```

It builds with `--locked --release` and writes, into `dist/`:

- `loongagent-<version>-<target>.tar.gz` — the binary, `README.md`, and a `BUILD`
  file naming the version, the commit, the host target and the exact toolchain;
- `SHA256SUMS` — checksums over the tarball.

Install it versioned, so an upgrade or a rollback is a symlink flip:

```bash
tar -xzf loongagent-0.0.1-x86_64-unknown-linux-gnu.tar.gz -C /opt
ln -sfn /opt/loongagent-0.0.1-x86_64-unknown-linux-gnu /opt/loongagent/current
ln -sfn /opt/loongagent/current/bin/loongagent /usr/local/bin/loongagent
```

Two limits worth knowing before distributing it:

- **The artifact is not signed.** ADR-0008 §6 makes an organisation's configuration
  layer tamper-resistant through a signature, and that is not implemented — so do
  not describe the org layer as enforced yet.
- **The binary is dynamically linked against glibc** (`ldd target/release/loongagent`),
  so it runs on distributions at least as new as the build host. For an older fleet,
  build in a container pinned to that glibc or add a musl target.

## What belongs in git, and what must not

`.loongagent/` holds two very different kinds of file, and the difference is a
security property rather than a preference:

| Commit it | Never commit it |
|---|---|
| `profile.toml`, `hooks.toml`, `mcp.toml` — the project layer (ADR-0008) | `session.jsonl` / `*.sqlite` — the session itself: prompts, file contents, tool output, approvals |
| `skills/`, `commands/` | `*.trusted` — "a human trusted these exact bytes". Committing one lets a repository ship a pre-trusted hook or MCP server, which is exactly what the gate exists to prevent |

`.gitignore` ships those ignores. If you copy the layout into another repository,
copy that part of the file too.

## Commits

The repository has no git identity configured; commits here use
`git -c user.name="loongagent" -c user.email="noreply@local" commit …`.
Docs changes go to `origin` (`ssh://git@git.fengchang.cc:10022/fengchang/loongagent.git`).
