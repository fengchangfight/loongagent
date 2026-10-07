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

# interactive: the TUI spawns its own server child and talks JSON-RPC over pipes
./target/debug/loongagent tui --session /tmp/s.jsonl --cwd /tmp --model demo-escape

# the server alone, for scripting
./target/debug/loongagent server --stdio --session /tmp/s.jsonl --cwd /tmp --model echo

# a real provider: any OpenAI-compatible Chat Completions endpoint over http://
# (vLLM, LM Studio, llama.cpp --server, Ollama /v1, an intranet gateway)
LOONGAGENT_BASE_URL=http://127.0.0.1:8000/v1 LOONGAGENT_MODEL=qwen2.5-coder \
  ./target/debug/loongagent run "summarise README.md" --session /tmp/r.jsonl --cwd . --model openai-compat
```

`https://` endpoints are refused with a clear message for now: TLS is a
dependency decision that has not been made, and self-hosted `http://` endpoints
are the firm-internal case this adapter was built for.

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
- **MCP servers** (`loong-mcp`, T-15): a trusted `.loongagent/mcp.toml` names
  stdio servers to start; their tools join the **same registry** as the built-ins
  (`mcp__<server>__<tool>`), so they go through the same PDP verdict, emit the
  same `tool.call`/`tool.result` events, and appear in the same audit trail. Every
  read has a deadline (a reader thread plus a timeout, so a wedged server fails the
  call instead of hanging the session), and each tool's **action class is declared**
  in the manifest — per server, with per-tool overrides, defaulting to the cautious
  `out_of_bounds` (asks every time). A server entry names a command to run, so it
  sits behind the same trust gate as hooks: inert until `loongagent mcp trust`.
- **Lifecycle hooks** (`loong-hooks`): a hook answers one question at one moment
  — allow, ask, or deny — and nothing else. Six moments fire today:
  `session_start`, `user_prompt_submit`, `pre_tool`, `post_tool`,
  `permission_requested`, `permission_decided`. Two kinds: `shell` (a script
  receiving the event as JSON on stdin; exit 2 denies with stderr as the reason,
  or print `{"verdict":"ask","reason":"…"}`) and `http` (an endpoint, **off**
  until the manifest sets `allow_http_hooks = true`). Verdicts aggregate
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

1. **The adapter is HTTP-only** — there is no TLS client and no streaming;
   cost is recorded as `usd_micros: 0` until pricing/profiles land, and there is
   no tokenizer (the ledger estimates tokens at ~4 chars).
2. **Network egress is not confined** — Landlock covers the filesystem only;
   blocking egress needs seccomp or a network namespace (neither is available
   unprivileged in this container: `unshare -Urn` is refused). The `shell`
   tool's classifier scores network commands as out-of-bounds, which is an
   autonomy decision, not enforcement. macOS seatbelt is not implemented.
   Caveat: a hardened deployment must not treat the classifier as a boundary.
3. **Compaction is not implemented** — the ledger can evict, but nothing drives
   the ADR-0006 §4 heuristic (clear tool output → summarize, keep the newest
   window verbatim) yet.
4. **The TUI is a line REPL** — ADR-0013 expects `ratatui` for the real surface.
5. **`grep`/`glob` do not honour `.gitignore` yet** — they use a small explicit
   skip list instead. Web tools (`web_fetch`, `web_search`) are not built, and
   `grep` reads whole files (no streaming) under a 4 MiB per-file cap.
6. **MCP is stdio-only and eager**: no HTTP transport, no resources/prompts, no
   sampling, and server-initiated requests are refused rather than served. Tools
   are listed at session start and every schema is handed to the model. The `mcp`
   *hook kind* is still refused at load time — the client now exists, so wiring it
   into `HookRunner` is the next step rather than a blocker.
7. **Post-tool hook verdicts are recorded but cannot undo anything**, and hooks
   deliberately cannot rewrite a tool call — the moment they could, the loop would
   stop being small enough to audit.
8. **Not started**: skills/commands, profiles/config packs, session
   export/import, WASM extensions, the web client, compaction, the control plane.

## Commits

The repository has no git identity configured; commits here use
`git -c user.name="loongagent" -c user.email="noreply@local" commit …`.
Docs changes go to `origin` (`ssh://git@git.fengchang.cc:10022/fengchang/loongagent.git`).
