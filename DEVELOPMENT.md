# Development

- **Up:** [docs/04-system-overview.md](docs/04-system-overview.md)
- **Authority:** [docs/README.md](docs/README.md) · [docs/design/README.md](docs/design/README.md)
- **Code:** `crates/`, `clients/vscode/`, `scripts/`, `Cargo.toml`
- **Verify:** `./scripts/check.sh`

This is the maintainer entry: build it, gate it, change it, release it. It is not
the status page — [`README.md`](README.md) §状态 is that — and it is not the
mechanism reference: [`docs/design/`](docs/design/README.md) owns each subsystem
and [`docs/04-system-overview.md`](docs/04-system-overview.md) is the one-read
picture. "Where the mechanisms are documented" at the end maps the sections this
file used to carry to those pages.

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

### Windows

The workspace builds for Windows with the same command as everywhere else, provided a
C compiler is present (bundled SQLite and `ring` both compile C, and neither is
optional):

```powershell
cargo build --release --locked -p loongagent     # -> target\release\loongagent.exe

# or, with the prerequisites checked first and the binary staged:
powershell -ExecutionPolicy Bypass -File scripts\build-windows.ps1
```

`x86_64-pc-windows-msvc` is the supported target; `-gnu` works with MinGW-w64, which
is also how this repository is cross-checked from Linux. The build command, the TUI
parity table and the gaps that are *not* closed yet are in
[docs/runbooks/windows-build.md](docs/runbooks/windows-build.md).

### What is pinned where

`rust-toolchain.toml` pins the **channel** (`stable`) so a developer's machine needs
no network to start. CI pins the **exact version** (`RUST_VERSION` in the workflow),
because an artifact must be traceable to a compiler — and `scripts/release.sh`
records that version in the artifact's `BUILD` file. Pinning the exact version in
`rust-toolchain.toml` was tried and reverted: rustup treats it as a separate
toolchain and tried to download it (the mirror 404s that tarball here).

## Gates

One command runs exactly what CI runs, so "it passed here" means one thing:

```bash
./scripts/check.sh          # formatting, schema, layering, human-written, documents, tests, release build
FAST=1 ./scripts/check.sh   # skip the release build (the slow one)
```

| Gate | Catches |
|---|---|
| `cargo fmt --check` | style drift; generated Rust is formatted **by the generator**, or the two would fight over the same bytes |
| `gen-schema.py --check` | generated types drifting from `crates/schema/protocol.toml` |
| `check-layering.sh` | a crate dependency pointing the wrong way (ADR-0004) |
| `check-human-written.sh` | a change under `docs/human-written/` — the human's notebook (`AGENTS.md`) |
| `check-docs.py` | a broken relative link, a [crate map](docs/crate-map.md) drifted from `scripts/layering_rules.py`, a document the index does not list or that is over budget, or an impossible ADR/topic citation |
| `cargo test --workspace --locked` | behaviour, including end-to-end tests against the real binary |
| `cargo build --release --locked` | code that compiles in debug and not with optimisations |

`.github/workflows/ci.yml` and `.gitea/workflows/ci.yml` call that script, so the
same gates run wherever the repository is hosted. Both install rustfmt (the
toolchain uses the `minimal` profile) and use the TUNA mirror.

**`docs/human-written/` is the human's notebook.** `AGENTS.md` forbids an agent
from creating, editing, moving, or deleting anything there, and the gate enforces
it: uncommitted changes fail, and a commit that touches the directory without the
human's `Human-Written: true` trailer fails. The human's own commits carry that
trailer; the docs checks skip the directory, so those notes follow no document
conventions.

### Focused commands

```bash
cargo build --workspace
cargo test --workspace --locked                # the totals README.md states
cargo test -p loong-kernel                     # one crate
cargo test -p loong-kernel a_reopened_session  # one test by name
./scripts/gen-schema.py                        # regenerate from crates/schema/protocol.toml
./scripts/gen-schema.py --check                # the CI gate
./scripts/check-layering.sh                    # crate dependencies point down only
```

## The change path: documents first

Before changing behaviour, name the design document that owns it. If none
exists, write or split one first, then the module plan, then the code — the rule
is in [`AGENTS.md`](AGENTS.md) §Contracts and navigation. A mechanism change
updates its owning document in the same change. For the four extension
procedures (a built-in tool, an event, a hook, a routing change), start at
[`docs/07-extending.md`](docs/07-extending.md).

## The control plane

Two binaries, one shared crate: `loongagent` reads a signed pack and runs a
profile; `loongagent-control` runs `keygen`, `serve`, `publish`, `show` and
`health` and distributes packs. The design is
[`docs/design/control-plane.md`](docs/design/control-plane.md); the doing is
[`docs/runbooks/control-plane.md`](docs/runbooks/control-plane.md) — layout, key,
tokens, `serve`, `publish`, systemd, migration, day two and troubleshooting.
`scripts/control-plane-init.sh` writes the layout and prints the next commands.

## Running it

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo build

# headless, with a provider double (no keys, no network)
./target/debug/loongagent run "hello" --session /tmp/s.jsonl --cwd /tmp --model echo
./target/debug/loongagent run "write it" --session /tmp/w.jsonl --cwd /tmp --model demo-write
./target/debug/loongagent run "escape"  --session /tmp/e.jsonl --cwd /tmp --model demo-escape   # exits 3: needs a human
./target/debug/loongagent tui --session /tmp/s.jsonl --cwd /tmp --model demo-escape
echo "hello" | ./target/debug/loongagent repl --session /tmp/s.jsonl --cwd /tmp --model echo

# a real provider: any OpenAI-compatible Chat Completions endpoint
LOONGAGENT_BASE_URL=https://api.deepseek.com LOONGAGENT_MODEL=deepseek-chat \
LOONGAGENT_API_KEY=sk-… ./target/debug/loongagent web --cwd .
```

`README.md` is the short version for someone trying it, and `scripts/demo.sh`
runs a scripted model with no API key at all (`./scripts/demo.sh [web|tui|run]`).
Sessions are plain JSONL — `cat` them. `LOONGAGENT_SERVER_BIN` overrides which
binary the TUI spawns as its server (used by the end-to-end test).

## The VS Code sidebar

The extension is TypeScript, not a Rust crate. It drives the same `loongagent server --stdio`
child and renders in its own Activity Bar Webview; see
[docs/design/vscode-surface.md](docs/design/vscode-surface.md) and
[clients/vscode/README.md](clients/vscode/README.md).

```bash
cd clients/vscode
npm ci
npm run compile            # esbuild: dist/extension.js + dist/webview.js, then tsc
npm test                   # node --test over the pure modules
code --extensionDevelopmentPath="$PWD" --new-window /tmp/loong-vscode-scratch
```

The binary is not bundled. Set `loongagent.path` (machine scope) to the installed binary, or
run **loongagent: Locate the binary**. Review-before-write needs
`[permissions] write_workspace = "ask"`; the extension never changes policy.

## TUI presentation contracts

The palette, the mark and the fold/colour rules are owned by
[`docs/design/surfaces.md`](docs/design/surfaces.md) §3; this section keeps the
day-to-day contracts, and `README.md` has the keys a user needs.

- **Folds are keyboard-first.** `tab`/`shift-tab` walk the `… N more line(s)`
  markers, `enter` opens one, and clippings never depend on a mouse. A fold is
  named by its place from the oldest, so a name still means the same result when
  the turn ends.
- **Opening a fold does not ask the log while a turn is running.** Each fold's
  text came past with the turn's stream; the view keeps the newest `FOLD_KEEP`
  (32), and only an older fold is fetched through `session.replay`.
- **Colour is classification, not sender.** `classify` reads the line's own
  vocabulary and `style_for` picks the style; the palette names ANSI colours and
  never invents RGB, and quiet lines are the terminal's own foreground dimmed —
  `DarkGray` is the background on a dark theme, so it drew bookkeeping into the
  wall (`MARK_QUIET`).
- **The mark only appears when there is room** (at least 48 columns by 18 rows;
  the five-row form needs 52 by 22), `/logo` puts it away, and the inscription
  takes the longest form that fits.
- **A decision panel sits below the transcript and above the input**, clips every
  detail to display columns (never wraps), and gives up details before answers.
- **The wire is not the log.** `loongagent tui --local --debug-model` (also
  `run`/`repl`/`web`) appends each exchange to `<session>.models.jsonl`;
  `finish_reason` (`stop` vs `tool_calls`) is the server's own verdict when a
  model prints a tool call as text. The dump is diagnostic data, never an event,
  and it contains the prompt — treat it like the session.

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

The artifact is **not signed**; the pack envelope is, and is verified against a
pinned key. The binary is dynamically linked against glibc
(`ldd target/release/loongagent`), so it runs on distributions at least as new as
the build host — or build in a container pinned to that glibc or add a musl target.

## After changing code

`./scripts/install-local.sh` installs both binaries where PATH finds them and prints
the commit each was built from, next to the repository HEAD. `--version` names that
commit, because a stale `~/.cargo/bin` copy has quietly run old rules three times
here — once publishing with the wrong issuer, once not knowing a subcommand, once
refusing a pack the new code accepts — and a long-running control plane is the same
trap in a process: **restart it after installing.**

## What belongs in git, and what must not

`.loongagent/` holds two very different kinds of file, and the difference is a
security property rather than a preference:

| Commit it | Never commit it |
|---|---|
| `profile.toml`, `hooks.toml`, `mcp.toml` — the project layer (ADR-0008) | `session.jsonl` / `*.sqlite` — prompts, file contents, tool output, approvals |
| `skills/`, `commands/` | `*.trusted` — "a human trusted these exact bytes"; committing one lets a repository ship a pre-trusted hook or MCP server |

`.gitignore` ships those ignores. If you copy the layout into another repository,
copy that part of the file too.

## Where the mechanisms are documented

| What this file used to describe | Owner |
|---|---|
| A turn, approvals, interruption, how a turn ends | [design/kernel-loop.md](docs/design/kernel-loop.md), [flows/turn-lifecycle.md](docs/design/flows/turn-lifecycle.md), [flows/tool-approval.md](docs/design/flows/tool-approval.md) |
| The context ledger, compaction, the F2 panel and `recall` | [design/context-ledger.md](docs/design/context-ledger.md), [context/](docs/context/README.md) |
| Tools, action classes, PDP, YOLO, sandbox | [design/tools-permissions.md](docs/design/tools-permissions.md), [flows/tool-approval.md](docs/design/flows/tool-approval.md) |
| Sessions, fork/delete/resume/retry, the state directory | [design/sessions-lineage.md](docs/design/sessions-lineage.md), [design/events-storage.md](docs/design/events-storage.md), [flows/resume-fork.md](docs/design/flows/resume-fork.md) |
| TUI, web, CLI and the JSON-RPC they share | [design/surfaces.md](docs/design/surfaces.md) |
| Config layers, profiles, secrets and environment files | [design/config-routing.md](docs/design/config-routing.md) |
| Control plane, packs, pinned key, local/managed/gateway modes | [design/control-plane.md](docs/design/control-plane.md), [design/config-routing.md](docs/design/config-routing.md) |
| Hooks, skills/commands, MCP, plugins | [design/extension-model.md](docs/design/extension-model.md) |
| Trust boundaries, signing key, what is trusted | [design/security-trust.md](docs/design/security-trust.md) |
| Event log, JSONL, SQLite index, schema | [design/events-storage.md](docs/design/events-storage.md) |
| Crate layout and the dependency edges allowed between crates | [docs/crate-map.md](docs/crate-map.md), generated from `scripts/layering_rules.py` |
| Toolchain, gates, build, release | this file |
| Status: what works and what does not | [`README.md`](README.md) §状态 |

## Commits

The repository has no git identity configured; commits here use
`git -c user.name="loongagent" -c user.email="noreply@local" commit …`.
Docs changes go to `origin` (`ssh://git@git.fengchang.cc:10022/fengchang/loongagent.git`).
