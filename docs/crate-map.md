# Crate map

Generated from the `ALLOWED` edges in `scripts/layering_rules.py`, which
`scripts/check-layering.sh` enforces in CI. Regenerate it with:

```
python3 scripts/layering_rules.py --emit-map . > docs/crate-map.md
```

Dependencies may only point **down**: the schema depends on nothing, the
kernel never depends on a surface, and a client never reaches back into the
kernel. The edges below are every internal dependency allowed; anything not
listed is a violation. The middle column is each crate's own `//!` header.

| Crate | What it is | Allowed dependencies |
|---|---|---|
| `loong-config` | Configuration and profile layering (ADR-0008). | `loong-schema` |
| `loong-context` | Layered, reversible context memory (ADR-0017). | `loong-eventlog`, `loong-http`, `loong-schema` |
| `loong-control` | Talking to the control plane: fetching the signed profile pack, verifying it, | `loong-config`, `loong-http`, `loong-pack` |
| `loong-eventlog` | Write and read the append-only JSONL event log that is one session's source of truth. | `loong-schema` |
| `loong-hooks` | Lifecycle hooks: the deterministic enforcement layer (ADR-0007 §7). | `loong-http`, `loong-manifest`, `loong-proc`, `loong-schema` |
| `loong-http` | Carry one JSON request over HTTP/1.1, plain or TLS, with bounded timeouts and a streamed reply. | — |
| `loong-index` | Answer tree, search and count queries from a disposable SQLite copy of the JSONL log. | `loong-eventlog`, `loong-schema` |
| `loong-kernel` | The fixed kernel (ADR-0004): the model+tool loop, the context ledger that is | `loong-config`, `loong-context`, `loong-eventlog`, `loong-hooks`, `loong-index`, `loong-mcp`, `loong-providers`, `loong-sandbox`, `loong-schema`, `loong-skills`, `loong-tools` |
| `loong-manifest` | Gate repository manifests behind a human trust decision and a change-detection hash. | — |
| `loong-mcp` | MCP (T-15): external tools behind the same interface as the built-ins. | `loong-manifest`, `loong-protocol`, `loong-schema`, `loong-tools` |
| `loong-pack` | Sign and verify the one envelope every distributed artifact travels in. | — |
| `loong-plugin` | Plugins: a bundle of skills you install **by name**, instead of hand-copying | `loong-config`, `loong-skills` |
| `loong-proc` | Run a child process with bounded output, a hard timeout and teardown of its whole tree. | — |
| `loong-protocol` | Define the JSON-RPC 2.0 envelopes and one-line framing that carry schema messages between surfaces and the server. | `loong-schema` |
| `loong-providers` | Model providers behind one `Model` interface (ADR-0004). | `loong-http`, `loong-schema` |
| `loong-sandbox` | Policy decision point (ADR-0007): the kernel decides, the OS enforces. | `loong-schema` |
| `loong-schema` | Own the single source of truth for every message on the wire, generated once for Rust and TypeScript. | — |
| `loong-server` | The local agent server (ADR-0001): sessions live here, clients attach. | `loong-eventlog`, `loong-kernel`, `loong-protocol`, `loong-schema` |
| `loong-skills` | Skills and commands — **one concept** (T-18, T-16 merged in 2026-10-05). | `loong-schema`, `loong-tools` |
| `loong-tools` | Tools behind one uniform, MCP-shaped interface — built-ins included | `loong-proc`, `loong-schema` |
| `loong-tui` | Terminal client (ADR-0009): renders the event stream and sends intents. | `loong-protocol`, `loong-schema` |
| `loong-tui-app` | The full-screen terminal surface (ADR-0009, ADR-0013): a ratatui view over a | `loong-schema`, `loong-tui` |
| `loong-web` | Serve the embedded browser page and bridge WebSocket lines to the same JSON-RPC server. | — |
| `loongagent` | loongagent — one binary, many faces (ADR-0009). | `loong-config`, `loong-context`, `loong-control`, `loong-eventlog`, `loong-hooks`, `loong-http`, `loong-index`, `loong-kernel`, `loong-manifest`, `loong-mcp`, `loong-pack`, `loong-plugin`, `loong-proc`, `loong-protocol`, `loong-providers`, `loong-sandbox`, `loong-schema`, `loong-server`, `loong-skills`, `loong-tools`, `loong-tui`, `loong-tui-app`, `loong-web` |
