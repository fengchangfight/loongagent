# MCP client: servers, wire, tools and limits

- **Up:** [design/README.md](README.md) · [extension-model.md](extension-model.md)
- **Authority:** [ADR-0004](../decisions/0004-microkernel-extension-model.md) · [ADR-0007](../decisions/0007-permission-sandbox-engine.md)
- **Code:** `crates/mcp/src/`, `crates/tools/src/deferred.rs`, `crates/tools/src/lib.rs`, `crates/tools/src/search.rs`, `crates/kernel/src/turn/build.rs`, `crates/cli/src/mcp.rs`, `crates/http/src/lib.rs`
- **Verify:** `cargo test -p loong-mcp -p loong-tools -p loong-kernel` · `./scripts/check.sh`

[extension-model.md](extension-model.md) §3 says where MCP sits in the extension model and why
its tools pass the same gate as the built-ins. This document owns the client itself: what a
server entry may say, which parts of the protocol are implemented, how a tool result reaches
the model, and what is deliberately missing.

## 1. A server is a manifest entry, behind one trust gate

`McpSet::load` reads `<workspace>/.loongagent/mcp.toml`, and merges a second, optional source:
the reverse session's manifest, `<workspace>/.reverse/runs/<run>/mcp.toml`
([reverse.md](reverse.md) §3). That second path is not fixed: the reverse process points
`LOONGAGENT_REVERSE_DIR` at its own run directory for the session it starts, and
`McpSet::load_for_process` follows it; a session without that variable reads only the workspace
file, so a normal `loongagent` can never pick up reverse tools even while a reverse session runs
in the same project. Both files parse through the same validation, and each entry is stamped
with an origin (`workspace` / `reverse`) so `mcp list` can say where a server came from. A name
shared by the two files is a loud duplicate error, not a silent overwrite.

Trust is fail-closed per source: each manifest is checked against its own sibling record
(`mcp.trusted` beside it), neither record covers the other file, and writing or deleting one
pair never re-arms the other's question. A manifest whose bytes no longer match its record is
inert — its servers are parsed and listed but not run — and the load surfaces a reason that
names the path and the fix (`loongagent mcp trust`).

One `[[server]]` table is one server,
and it must name exactly one transport:

| Field | Meaning |
|---|---|
| `name` | Namespace for everything the server exposes (`mcp__<name>__…`). |
| `command`, `args`, `env` | The stdio transport: a child process this session starts and owns. |
| `url`, `headers` | The HTTP transport: an endpoint this session calls. `headers` values may use `${VAR}`. |
| `action_class` | The class of every tool on this server; default `out_of_bounds` (ask every time). |
| `tools.<tool> = "class"` | A per-tool override of the default class. |
| `timeout_ms` | The deadline on every reply; default 30 000, capped at 600 000. |
| `required` | `true` (default) makes an unusable server fatal; `false` turns it into a note. |
| `defer` | `auto` (default), `always` or `never` — see §6. |

Each manifest pair is inert until its bytes are recorded in its own `mcp.trusted` — the human
runs `loongagent mcp trust` for the workspace pair; the reverse binary records its own pair when
it writes it, inside its run directory. The record is a hash of the manifest's bytes: editing
one character, including a URL, revokes it. What trust
does **not** cover is unchanged and stated in [extension-model.md](extension-model.md) §3 — the
command, its PATH lookup and the files it reads are not hashed, and the process is outside the
tool sandbox.

The HTTP transport is a new egress path, so it is narrower than the stdio one:

- `https://` is required. A plain `http://` URL is accepted only for a loopback host
  (`127.0.0.1`, `::1`, `localhost`), so a token never crosses a wire in clear text.
- `${VAR}` in a header value is expanded from the environment at load; a missing variable is an
  error that names the variable and never prints the value.
- A redirect is a failure. Following one would let a trusted local endpoint hand the session to
  a host the manifest never named.

## 2. The wire: JSON-RPC 2.0, one message at a time

The client speaks JSON-RPC 2.0 and claims protocol version `2025-06-18`. Capabilities are taken
from what the server declared during `initialize`; a capability the server did not advertise is
never exercised, so a server without `prompts` is not asked for prompts.

Two transports carry the same messages, so everything above them is shared:

- **stdio.** Newline-delimited JSON. The child's stdin is the write side; a reader thread moves
  stdout lines into a channel, and every `recv` has a deadline. The child's stderr goes to
  `null`: a server that logs is not the session's log, and a full pipe would wedge it.
- **Streamable HTTP.** One POST per outgoing message to `url`. The request carries
  `Accept: application/json, text/event-stream`, `Content-Type: application/json` and, once the
  handshake has run, `MCP-Protocol-Version` and the `Mcp-Session-Id` the server returned during
  `initialize`. The reply body is either one JSON-RPC message or an SSE stream; both are read
  into one text buffer and the message whose `id` matches the request is taken from it. A
  notification is a POST that expects `202` and no body. A non-2xx status is an error carrying
  the status and a short excerpt of the body.

A POST is read to completion before the reply is used, which is why the HTTP transport keeps one
rule the spec does not require: **the server must close the response within `timeout_ms`.** A
server that keeps an SSE stream open after answering is a limit, not a hang — §9. There is no
`GET` stream and no `DELETE` teardown; a session ends by expiry.

Server-initiated messages are read where they appear, in the same loop that waits for a reply:

- a `notifications/tools/list_changed` sets a dirty flag (§6); `notifications/progress` and
  `notifications/cancelled` are ignored;
- a server-initiated **request** (`sampling/createMessage`, `roots/list`, `elicitation/create` or
  anything else) is answered with JSON-RPC error `-32601` naming the method, so the server learns
  the client does not serve it instead of waiting forever. Nothing else is implemented.

## 3. Discovery: every list is followed to its last page

`tools/list` and `prompts/list` are called with `{}` and then with each `nextCursor` the reply
carries, accumulating until the server stops sending one. The loop is bounded: at most 100 pages
and 10 000 items per list, and a cursor that repeats is an error. Without the loop a server with
many tools silently loses the tail — the worst kind of bug, because everything looks healthy.

## 4. Tools: one interface, one gate, honest results

Every listed tool becomes an `McpTool`: name `mcp__<server>__<tool>`, description and input
schema as declared, action class from the manifest (§1). It is registered in the same `Registry`
as the built-ins, so `Pdp::decide` runs before every model-facing call and the class is never
inferred from the arguments. A tool the manifest overrides keeps the override; a tool it does not
mention keeps the server's default.

A tool result is flattened for a text-only model, and nothing is dropped:

| Content block | What the model receives |
|---|---|
| `text` | The text itself. |
| `image`, `audio` | The bytes are written to a file under the media directory (§7) and the model gets `[image image/png → /tmp/…/….png (12.3 KiB)]`. |
| `resource` with `text` | The text, prefixed by its URI. |
| `resource` with `blob`, `resource_link` | The bytes to a file (or the link), described the same way. |
| anything else | `[<type> content omitted]` — named, so its absence is visible. |

`isError: true` becomes an ordinary tool error carrying the message. `structuredContent`, when
the server sends it, is appended as pretty-printed JSON so a model that only reads text can still
use it. A block larger than 5 MiB is not written to disk; the model is told its size instead.

## 5. Prompts become commands, bodies fetched on invoke

`prompts/list` fills the same catalogue skills and commands use: `mcp__<server>__<prompt>`, with
`<name>` for a required argument and `[name]` for an optional one. The body is fetched with
`prompts/get` at invoke time, so a server can change a prompt body without a restart. Argument
mapping is positional first, then `name=value` overrides; an undeclared `key=value` is treated as
positional rather than silently dropped. An MCP prompt is namespaced, so it can never shadow a
workspace skill.

Catalogue changes (`notifications/prompts/list_changed`) do not re-seat the skill list, which was
baked into the system prompt at build; §9 says why that is deliberate.

## 6. Deferred tools: search before you look (T-15)

MCP schemas are the largest thing a session puts into every request, and most are never called.
A server is *deferred* when `defer = "always"`, or when `defer = "auto"` and its tools are
expensive by either measure: more than 10 tools, or more than 8 KiB of input schema. A deferred
server's tools are **not** in the request; they are dispatchable, but the model does not see their
schema until it asks.

Asking is the `tool_search` tool, registered only when at least one server is deferred:

1. `tool_search` takes `{query, limit}` and matches the query's words against each deferred tool's
   name and description.
2. It returns the schema of up to `limit` matches (default 5, at most 20), and marks them active.
3. Active tools appear in the next step's request. Activation lasts for the session, so a second
   search is never needed for the same tool.
4. No match returns the first 20 names in the catalogue, so a wrong keyword costs one step rather
   than a dead end.

Dispatch never depends on activation: a model that knows a tool's name from a previous turn can
call it directly, and `Pdp::decide` still runs.

Refresh is by notification. A dirty server is re-listed once, at the start of the next turn
(`Kernel::begin_turn`), not in the middle of one: the request already sent cannot change, and
rebuilding it mid-step would cost the provider cache. Re-listing replaces that server's tools
wholesale — a tool the server dropped is removed, a new one is registered under the same defer
rule. A failed refresh keeps the previous set and appends a warning to the session's MCP note
rather than killing a turn that was already running.

## 7. Numbers this document pins

| Limit | Value | Why |
|---|---|---|
| Reply deadline | `timeout_ms`, default 30 000, max 600 000 | A wedged server fails the call, not the session. |
| Pages per list | 100 | Bounded loop; a buggy cursor cannot spin forever. |
| Items per list | 10 000 | Memory bound for `tools/list`. |
| Defer threshold | > 10 tools or > 8 KiB of schema | Roughly where a tool list starts to cost more than it earns. |
| `tool_search` results | 5 by default, 20 at most | Enough to work with, small enough to read. |
| Media block | 5 MiB | Above this the placeholder carries the size instead of the bytes. |
| Concurrent calls per server | 1 | One `Mutex` per client; §9 states the cost. |

The media directory is `$TMPDIR/loongagent-mcp/` (or the platform's temp directory), with the
server name, the process id and a counter in the file name, so two sessions never collide and a
reboot cleans up.

## 8. Shutdown

Dropping the last `Arc<Mutex<McpClient>>` ends the server. For stdio that is `kill(-pid)` on Unix
and `taskkill /T` on Windows — the whole process group, so the `npx`/`node` pair behind a server
does not outlive the session — then `wait`, so no zombie is left. For HTTP there is nothing to
kill: the session id simply expires on the server.

## 9. Limits

- **No sandbox.** The server process runs as an ordinary child of the session. The trust gate is
  the barrier, not confinement ([security-trust.md](security-trust.md) §3).
- **Trust covers `mcp.toml` only.** Replacing the script a trusted server names does not revoke
  trust. An HTTP server can change its behaviour at any time behind a trusted URL.
- **HTTP reads one response per POST.** A server that streams progress over SSE and keeps the
  stream open blocks until `timeout_ms`. Cancellation is not implemented in either transport.
- **One call at a time per server.** `McpTool::execute` holds the client's mutex, so two slow
  calls to the same server serialise; different servers do not.
- **No resources, sampling, roots or elicitation.** They are answered with `-32601` (§2); a
  server that needs them cannot be used fully.
- **The prompt catalogue is fixed at build.** A prompt body is live, a prompt name is not, because
  the catalogue is seated into the system prompt once (§5).
- **The trust gap in extension-model.md §3 still stands,** and this document does not narrow it.
