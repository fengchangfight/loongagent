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
  fresh one instead;
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

### What the TUI answers to

| Key / command | What it does |
|---|---|
| `enter` | send the line |
| `/exit`, `/quit`, `/q` | leave — handled by the **interface**, never sent to the agent |
| `ctrl-c`, `ctrl-d` | leave |
| `f2` | the context panel |
| `pgup`/`pgdn`, mouse wheel | scroll the conversation; `ctrl-pgup`/`ctrl-pgdn` jump to the ends |
| `a`/`y`, `d`/`n`, `esc` | approve / deny a pending action |
| `←`/`→`/`home`/`end`/`del`, `ctrl-a/e/u/k/w` | edit the line, by character so CJK works |

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
  from an event.
- **The ledger**: every block that reaches the model is an entry with category,
  source, tokens and lifetime; eviction drops stale tool output before prose and
  never touches pinned entries; `explain()` answers "why is this in my context?".
- **Compaction** (ADR-0006 §4), driven before every model call: past 80% of the
  budget, stale tool output is **cleared** first (its body replaced by a marker —
  the model can re-run a command, it cannot re-read a decision), and only if that
  is not enough are the oldest whole exchanges **dropped**. The newest window
  (`--keep-newest`, default 20k) always stays verbatim, the most recent entry is
  never cleared, and a tool result is never separated from the call that produced
  it — a conversation that splits them is invalid to every provider. Each
  compaction is an event with before/after tokens; a compaction that could free
  nothing is not an event at all. `--budget N` sets the budget (0 = unbounded).
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
  | narrow-only | `budget_tokens`, `keep_newest_tokens`, `max_steps`, `compact_at_percent`, `[permissions]` | strictest wins; a lower-authority layer may only narrow |
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
