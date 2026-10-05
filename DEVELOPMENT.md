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
```

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
- **Two enforcement points**: the PDP decides, the tool runner refuses to write
  outside the workspace even if a call is allowed.
- **The vertical slice**, covered by three end-to-end tests against the real
  binary: a tool writes inside the workspace and the streamed events equal the
  log; an out-of-bounds write asks first and a denial is recorded without
  touching anything; the TUI client drives a real server.

## What is not done yet (honest list)

1. **The derived index is implemented and tested but not yet wired into a
   session** — nothing calls `loong-index` from the kernel/server yet, so search
   and tree queries are not exposed.
2. **No real provider adapter** — only `EchoModel` and the scripted doubles;
   cost is recorded as `usd_micros: 0` until pricing/profiles land.
3. **No OS sandbox PEP** — the PDP and the tool-level refusal are the only
   enforcement; seatbelt / Landlock+bubblewrap / Job Objects are not started.
4. **Compaction is not implemented** — the ledger can evict, but nothing drives
   the ADR-0006 §4 heuristic (clear tool output → summarize, keep the newest
   window verbatim) yet.
5. **The TUI is a line REPL** — ADR-0013 expects `ratatui` for the real surface.
6. **Not started**: hooks (three kinds + the event set), skills/commands, MCP,
   profiles/config packs, session export/import, WASM extensions, the web
   client, the control plane.

## Commits

The repository has no git identity configured; commits here use
`git -c user.name="loongagent" -c user.email="noreply@local" commit …`.
Docs changes go to `origin` (`ssh://git@git.fengchang.cc:10022/fengchang/loongagent.git`).
