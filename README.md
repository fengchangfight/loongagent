# loongagent

A firm-internal coding agent: a small kernel with a browser and a terminal in front
of it, where **the session log is the truth**, every permission decision is an
auditable event, and integrations arrive as MCP tools rather than as privileged
code.

The design is decided in [`docs/`](docs/) — 15 ADRs, a system overview, and a
parity checklist against Claude Code. This file is about running it.

## 30 seconds, no API key

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --release

./scripts/demo.sh          # a scripted model + the web UI, on http://127.0.0.1:8737
./scripts/demo.sh tui      # the same session, full-screen in the terminal
```

The scripted model reads a file and answers, so the tool call, the policy decision
and the event log are all real; only the model is fake.

## A real model

Any OpenAI-compatible endpoint works over `http://` or `https://` (vLLM, LM Studio,
llama.cpp, Ollama, DeepSeek, Moonshot/Kimi, OpenAI, an intranet gateway):

```bash
# DeepSeek
export LOONGAGENT_BASE_URL=https://api.deepseek.com
export LOONGAGENT_MODEL=deepseek-chat
export LOONGAGENT_API_KEY=sk-…
./target/release/loongagent web --cwd ~/some/project

# or keep it in a profile so the flags disappear (ADR-0008)
mkdir -p ~/.config/loongagent
cat > ~/.config/loongagent/profile.toml <<'TOML'
base_url = "https://api.moonshot.cn/v1"
model    = "kimi-k2-0905-preview"
api_key  = "env:MOONSHOT_API_KEY"     # secrets never live in config
budget_tokens = 200000
TOML
./target/release/loongagent web --cwd ~/some/project
```

Response streaming is not implemented: the answer appears when the turn finishes.

## Three surfaces, one brain

| Command | What it is |
|---|---|
| `loongagent web` | a browser UI on loopback; the URL carries a token in its fragment, and the socket refuses anything without it |
| `loongagent tui` | the same client, full-screen in a terminal (ratatui) |
| `loongagent repl` | line mode — works under a pipe, so CI and scripts are not left out |
| `loongagent run "…"` | headless: prints the turn and exits (exit 3 when a human is needed) |

All four speak JSON-RPC to the same local server, which streams every event of a
turn before it answers. The UI holds no agent logic, so a new surface cannot drift
from the kernel — and the web client is ~300 lines of page and a socket bridge.

## What to look at first

```bash
./target/release/loongagent config show     # every setting, and which layer set it
./target/release/loongagent skills list     # what /name resolves to here
./target/release/loongagent sandbox self-test   # the OS boundary, proven not assumed
cat .loongagent/session.jsonl               # the truth: one JSON line per event
```

In a session, try `/help`, then ask for something that writes a file — the policy
decision, the rule that fired, and the tool result are all in the stream and in the
log. Out-of-workspace writes stop for approval, and the *shell* tool's children run
under a Landlock policy that makes an escape from the workspace an `EACCES`.

## Guardrails you can show

- **Permissions are decisions with a rule**: `policy.decided {verdict, rule}` is in
  every session, so "why was this allowed?" has an answer: `builtin:investigate`,
  `profile:project:write_workspace`, `hook:policy-svc`.
- **Enforcement is at the OS**: Landlock confines every process a tool spawns;
  `sandbox self-test` runs a confined child that must fail to write outside the
  workspace, and the server refuses to start if that proof fails.
- **Hooks can only tighten**: shell, HTTP and MCP kinds behind one verdict
  contract; a hook that cannot answer fails closed. Repo-supplied hooks and MCP
  servers stay inert until `loongagent hooks trust` / `mcp trust`.
- **Context is explainable**: every block that reaches the model is a ledger entry
  with a category, a source and a token count; compaction clears stale tool output
  before it drops anything, and never splits a tool call from its result.

## Status, honestly

Working: the kernel loop, layered configuration, sessions and the JSONL log with a
derived SQLite index, the ledger, compaction, permissions + Landlock, hooks (three
kinds), MCP (tools and prompts, stdio), skills/commands, six built-in tools, and
the three surfaces above. 271 tests.

Not done, and not pretending otherwise: signed org packs (so the org config layer
is a plain admin-owned file, not tamper-resistant), response streaming, subagents,
an LSP-backed code tool, `.gitignore` awareness in `grep`/`glob`, HTTP transport for
MCP, multi-user sessions (one browser at a time drives the session), and a
model-made summary for compaction.

See [`DEVELOPMENT.md`](DEVELOPMENT.md) for the toolchain, the crate map and the
per-area detail.
