# VS Code Surface Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use subagent-driven-development (recommended) or executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A personal-use VS Code extension that drives the existing `loongagent server` over its existing stdio JSON-RPC wire and renders the agent in VS Code's native Chat view as `@loong`.

**Architecture:** The extension host spawns `loongagent server --stdio --cwd <folder>` as a child and speaks newline-delimited JSON-RPC 2.0 to it — the same wire the TUI and the web surface already speak. Three modules with no `vscode` import carry the logic (`agentSession.ts` transport, `turn.ts` the approval pause/resume state machine, `render.ts` events→UI parts) so they are testable with `node --test` and a fake child; `extension.ts` is the only file that touches the VS Code API, and it contains no agent logic.

**Tech Stack:** TypeScript (typecheck only, `tsc --noEmit`), esbuild (bundle to `dist/extension.js`, `vscode` external), Node's built-in test runner (Node 24 runs `.ts` test files natively — verified), VS Code stable APIs only.

**Spec:** `docs/superpowers/specs/2026-10-16-vscode-surface-design.md` — read it with this plan; the plan argues from it and every "spec §" reference below points into it.

## Global Constraints

- **Engine:** `engines.vscode` is exactly `^1.138.0` (the installed VS Code). Do not raise it.
- **Stable APIs only.** No `enabledApiProposals` key anywhere in `package.json`; no `--enable-proposed-api`; no proposed-only types (`ChatResponseMultiDiffPart`, `ChatResponseConfirmationPart`, `ChatToolInvocationPart`, `chatSessionsProvider`, …).
- **No agent logic in the extension** (spec §4.7, ADR-0009 §2): no routing, no context assembly, no policy evaluation, no verdicts of our own. Every decision line shown comes from `policy.decided`, `approval.requested`, or `approval.decided`.
- **The extension writes nothing into the workspace.** Its only writes are `context.workspaceState` and the `loongagent` output channel.
- **Approvals default off:** `loongagent.autoApprove` defaults to `false`.
- **Types come from the generated file, never copied:** import from `clients/ts/protocol.generated.ts` with the `.ts` extension. Use its `METHODS`, `EVENTS`, `NOTIFICATIONS` constants; never a bare method-kind string literal.
- **Mutation scope:** `clients/vscode/**`, plus exactly one doc-only line in `crates/schema/protocol.toml` (Task 3) and one line in `scripts/check.sh` (Task 11). No kernel, no server, no other crate.
- **`.gitignore` facts:** the repo ignores `.vscode/`, `node_modules/` and `dist/` at any depth. So: no committed launch config (run the dev host from the command line), and no `.gitignore` inside `clients/vscode`.
- **Concurrent edits:** this branch (`navigability`) is being edited by other work in the same working tree. Create an isolated worktree first (see the `using-git-worktrees` skill), and in every commit `git add` only the paths the task names — never `-A`.
- **TypeScript rules:** relative imports carry the `.ts` extension; type-only imports use `import type` (`verbatimModuleSyntax` is on); `strict` is on.
- **Windows:** cancel is a POSIX signal (`SIGINT`) only. Do not add a Windows path; the spec records it as unsupported.

## Review Focus

Five failure modes the spec implies but no single task owns by default. Each has its test pinned in the task named here.

1. **A second prompt while a turn is running** (two chat tabs, or a fast second Enter). Expected: the second is refused with "a turn is already running", never interleaved into the first turn's stream. → Task 4.
2. **The child dying mid-turn** (crash, OOM, killed by hand). Expected: the open request rejects with the exit code, the transcript says so, the next prompt respawns. → Tasks 2 and 4.
3. **Tool output that is huge or not text** (a 5 MB log, a binary dump). Expected: truncated at a fixed cap with a visible marker, the chat stays responsive. → Task 5.
4. **A tool argument naming a path outside the workspace** (`/etc/shadow`, `~/.ssh/id_rsa`). Expected: the extension never reads it; it may name it in the transcript, and it must not snapshot it. → Task 6.
5. **A session id that no longer resolves** (log deleted, another checkout's id). Expected: an actionable message naming the id, and a new session started — never a silent failure or a retry loop. → Tasks 3 and 8.

---

### Task 1: Extension scaffold that activates

**Files:**
- Create: `clients/vscode/package.json`
- Create: `clients/vscode/tsconfig.json`
- Create: `clients/vscode/esbuild.mjs`
- Create: `clients/vscode/src/extension.ts`
- Test: `clients/vscode/test/manifest.test.ts`

**Interfaces:**
- Consumes: nothing.
- Produces: an extension whose `activate()` creates the output channel named `loongagent` and logs `loongagent: activated`; npm scripts `compile` (esbuild + typecheck), `typecheck`, `test`; the four slash commands (`new`, `model`, `context`, `yolo`) and the `chatParticipants` contribution that later tasks fill in.

- [ ] **Step 1: Write the failing test**

`test/manifest.test.ts` — reads `package.json` and asserts the contribution shape, so a typo in the manifest fails `npm test` rather than a chat window:

```ts
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const pkg = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8"));

test("the engine floor is the installed VS Code", () => {
  assert.equal(pkg.engines.vscode, "^1.138.0");
});
test("no proposed API is enabled", () => {
  assert.equal(pkg.enabledApiProposals, undefined);
});
test("the chat participant is @loong and sticky", () => {
  const [p] = pkg.contributes.chatParticipants;
  assert.equal(p.id, "loongagent.chat");
  assert.equal(p.name, "loong");
  assert.equal(p.isSticky, true);
  assert.deepEqual(p.commands.map((c: { name: string }) => c.name).sort(),
    ["context", "model", "new", "yolo"]);
});
test("the entry point is the bundled file", () => {
  assert.equal(pkg.main, "./dist/extension.js");
});
test("an untrusted workspace is only partly supported", () => {
  assert.equal(pkg.capabilities.untrustedWorkspaces.supported, "limited");
  assert.ok(pkg.capabilities.untrustedWorkspaces.description.length > 0);
});
test("the binary path, model and auto-approve settings exist", () => {
  const props = pkg.contributes.configuration.properties;
  assert.equal(props["loongagent.path"].default, "loongagent");
  assert.equal(props["loongagent.autoApprove"].default, false);
  assert.ok("loongagent.model" in props && "loongagent.extraArgs" in props);
});
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd clients/vscode && node --test test/*.test.ts`
Expected: FAIL — `ENOENT … package.json`.

- [ ] **Step 3: Write the manifest, the two build files, and a minimal `activate`**

`package.json` (exact; `private` because this is never published, spec §3):

```json
{
  "name": "loongagent-vscode",
  "displayName": "loongagent",
  "description": "loongagent in the editor: the local agent in VS Code's own chat view.",
  "private": true,
  "version": "0.0.1",
  "publisher": "loongagent",
  "engines": { "vscode": "^1.138.0" },
  "categories": ["AI", "Other"],
  "main": "./dist/extension.js",
  "capabilities": {
    "untrustedWorkspaces": {
      "supported": "limited",
      "description": "The agent works on this workspace and runs its own tools under its own sandbox and approval rules."
    }
  },
  "contributes": {
    "chatParticipants": [
      {
        "id": "loongagent.chat",
        "name": "loong",
        "fullName": "loongagent",
        "description": "Ask the local loongagent",
        "isSticky": true,
        "commands": [
          { "name": "new", "description": "Start a new session" },
          { "name": "model", "description": "Choose the model for this session" },
          { "name": "context", "description": "Explain what the next request will carry" },
          { "name": "yolo", "description": "Turn approval prompts off for this session" }
        ]
      }
    ],
    "configuration": {
      "title": "loongagent",
      "properties": {
        "loongagent.path": { "type": "string", "default": "loongagent", "description": "The loongagent binary." },
        "loongagent.model": { "type": "string", "default": "", "description": "Passed as --model when the server starts." },
        "loongagent.autoApprove": { "type": "boolean", "default": false, "description": "Turn approval prompts off for this session (recorded as permissions.relaxed)." },
        "loongagent.extraArgs": { "type": "array", "items": { "type": "string" }, "default": [], "description": "Extra arguments for loongagent server." }
      }
    }
  },
  "scripts": {
    "compile": "node esbuild.mjs && tsc --noEmit",
    "watch": "node esbuild.mjs --watch",
    "typecheck": "tsc --noEmit",
    "test": "node --test test/*.test.ts"
  },
  "devDependencies": {
    "@types/node": "^24.0.0",
    "@types/vscode": "^1.138.0",
    "esbuild": "^0.25.0",
    "typescript": "^5.6.0"
  }
}
```

`tsconfig.json` — `moduleResolution: "Bundler"` is what lets a `.ts` extension in an import specifier typecheck; `noEmit` because esbuild emits.

```json
{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ESNext",
    "moduleResolution": "Bundler",
    "lib": ["ES2022"],
    "types": ["node", "vscode"],
    "strict": true,
    "noEmit": true,
    "allowImportingTsExtensions": true,
    "verbatimModuleSyntax": true,
    "skipLibCheck": true,
    "forceConsistentCasingInFileNames": true
  },
  "include": ["src/**/*.ts", "test/**/*.ts", "../ts/protocol.generated.ts"]
}
```

`esbuild.mjs` — bundle `src/extension.ts` to `dist/extension.js`, `vscode` external, CJS (the extension host loads CommonJS), Node 20 as the floor (VS Code 1.138's host is newer):

```js
import { build, context } from "esbuild";

const options = {
  entryPoints: ["src/extension.ts"],
  bundle: true,
  platform: "node",
  format: "cjs",
  target: "node20",
  external: ["vscode"],
  outfile: "dist/extension.js",
  sourcemap: true,
  logLevel: "info",
};

if (process.argv.includes("--watch")) {
  const ctx = await context(options);
  await ctx.watch();
} else {
  await build(options);
}
```

`src/extension.ts` — minimal activation only:

```ts
import * as vscode from "vscode";

export function activate(context: vscode.ExtensionContext): void {
  const output = vscode.window.createOutputChannel("loongagent");
  context.subscriptions.push(output);
  output.appendLine("loongagent: activated");
}

export function deactivate(): void {}
```

- [ ] **Step 4: Install the dev dependencies**

Run: `cd clients/vscode && npm install`
Then confirm the types version exists and matches the engine: `npm view @types/vscode versions --json | tail -20`; if `1.138.x` is not published, install the highest published version at or below it and say so in the commit message — do not raise `engines.vscode`.

- [ ] **Step 5: Run the tests and the typecheck to verify they pass**

Run: `cd clients/vscode && npm test`
Expected: PASS — 6 tests.
Run: `cd clients/vscode && npm run compile`
Expected: `dist/extension.js` written, `tsc --noEmit` silent, exit 0.

- [ ] **Step 6: Verify activation in a real VS Code window**

Run: `cd clients/vscode && npm run compile && code --extensionDevelopmentPath="$PWD" --new-window /tmp/loong-vscode-scratch`
(mkdir the scratch folder first; a workspace folder is needed from Task 7 on.)
Expected: a new window titled `[Extension Development Host]`; Output → `loongagent` shows `loongagent: activated`. Also confirm the Chat view's participant picker offers `@loong`.

- [ ] **Step 7: Commit**

```bash
git add clients/vscode/package.json clients/vscode/tsconfig.json clients/vscode/esbuild.mjs clients/vscode/src/extension.ts clients/vscode/test/manifest.test.ts clients/vscode/package-lock.json
git commit -m "feat(vscode): the extension scaffolds, activates, and contributes @loong"
```

---

### Task 2: The transport — framing, correlation, failure

**Files:**
- Create: `clients/vscode/src/agentSession.ts`
- Create: `clients/vscode/test/fakeChild.ts`
- Test: `clients/vscode/test/agentSession.test.ts`

**Interfaces:**
- Consumes: nothing (Node built-ins only).
- Produces: `AgentSession` (one JSON-RPC 2.0 connection over a newline-delimited stream) and `spawnAgent`. Later tasks use exactly: `new AgentSession(child, hooks)`, `session.request<T>(method, params): Promise<T>`, `session.notify(method, params): void`, `session.cancel(): void`, `session.dispose(): Promise<void>`, `spawnAgent(command: string, args: string[], cwd: string, hooks: AgentSessionHooks): AgentSession`.

```ts
// src/agentSession.ts
import type { ChildProcess } from "node:child_process";

export interface ChildLike {
  stdin: { write(chunk: string): unknown } | null;
  stdout: { on(event: "data", listener: (chunk: Buffer | string) => void): unknown } | null;
  stderr: { on(event: "data", listener: (chunk: Buffer | string) => void): unknown } | null;
  on(event: "exit", listener: (code: number | null, signal: string | null) => void): unknown;
  on(event: "error", listener: (error: Error) => void): unknown;
  kill(signal?: NodeJS.Signals): unknown;
}

export interface AgentSessionHooks {
  /** Every JSON-RPC notification the server sends (method + params, verbatim). */
  onNotification(method: string, params: unknown): void;
  /** Anything the transport cannot turn into a message: child stderr, malformed lines. */
  onLog(line: string): void;
  onExit(code: number | null, signal: string | null): void;
}

export class JsonRpcError extends Error {
  constructor(readonly code: number, message: string, readonly data?: unknown);
}

export class AgentSession {
  constructor(child: ChildLike, hooks: AgentSessionHooks);
  request<T = unknown>(method: string, params: Record<string, unknown>): Promise<T>;
  cancel(): void;
  dispose(): Promise<void>;
}

export function spawnAgent(
  command: string, args: string[], cwd: string, hooks: AgentSessionHooks,
): AgentSession;
```

- [ ] **Step 1: Write the fake child and the failing tests**

`test/fakeChild.ts` — an `EventEmitter`-backed stand-in that records what was written and can be fed raw bytes:

```ts
import { EventEmitter } from "node:events";
import type { ChildLike } from "../src/agentSession.ts";

export class FakeChild implements ChildLike {
  readonly written: string[] = [];
  readonly signals: (NodeJS.Signals | undefined)[] = [];
  readonly stdout = new EventEmitter();
  readonly stderr = new EventEmitter();
  private readonly events = new EventEmitter();
  readonly stdin = { write: (chunk: string) => { this.written.push(chunk); return true; } };
  on(event: "exit" | "error", listener: (...args: never[]) => void): this {
    this.events.on(event, listener as (...args: unknown[]) => void); return this;
  }
  kill(signal?: NodeJS.Signals): boolean { this.signals.push(signal); return true; }
  /** Feed raw bytes as the child's stdout. */
  emitStdout(bytes: string): void { this.stdout.emit("data", Buffer.from(bytes, "utf8")); }
  emitExit(code: number | null, signal: NodeJS.Signals | null = null): void {
    this.events.emit("exit", code, signal);
  }
  /** The JSON objects the session has written so far. */
  sent(): { id?: number; method: string; params: unknown }[] {
    return this.written.join("").split("\n").filter(Boolean).map((l) => JSON.parse(l));
  }
}
```

`test/agentSession.test.ts` — the behaviours that matter, each a named test:

```ts
// a response split across chunks is still matched
//   emit '{"jsonrpc":"2.0","id":1,"resu' then 'lt":{"session_id":"s1"}}\n'
//   → await session.request(...) resolves to { session_id: "s1" }
// two messages in one chunk are both handled
//   one chunk carrying a response for id 1 and a notification; both observed
// the id is what correlates, not the order
//   two requests outstanding; answer id 2 first; both resolve to their own results
// a JSON-RPC error response rejects with its code and message
//   → rejects JsonRpcError, err.code === -32601, err.message contains "unknown method"
// a malformed line is logged and does not stop the reader
//   emit "not json\n" then a valid response; onLog got the line, the request still resolves
// stderr lines reach onLog
//   emit on stderr → onLog called with the text
// a child that exits rejects every pending request with the exit code
//   two outstanding requests; emitExit(3) → BOTH reject, message names code 3
// cancel sends SIGINT to the child
//   session.cancel() → child.signals equals ["SIGINT"]
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd clients/vscode && npm test`
Expected: FAIL — `Cannot find module '../src/agentSession.ts'`.

- [ ] **Step 3: Implement `AgentSession` in `src/agentSession.ts`**

Requirements the tests pin, so the body is determined:

- One id counter starting at 1; `Map<number, {resolve, reject}>` for outstanding requests.
- Writes `JSON.stringify(message) + "\n"`; nothing else ever reaches stdout.
- Decode stdout through a single `StringDecoder` (multi-byte characters can split across chunks), keep a string buffer, split on `\n`, ignore empty lines.
- A parsed object with a `method` and no `id` is a notification → `hooks.onNotification(method, params)`.
- A parsed object with an `id` and an `error` → reject that entry with `JsonRpcError(code, message, data)`.
- A parsed object with an `id` and a `result` → resolve that entry.
- JSON that does not parse, or parses to a non-object, or has neither `method` nor `id` → `hooks.onLog("unparsable line: …")` and continue reading.
- On `exit` and on `error`: reject every outstanding request with a message naming the code/signal or the error, clear the map, call `hooks.onExit`.
- `cancel()` sends `SIGINT`; `dispose()` removes listeners and kills the child if it is still alive (no `SIGTERM` escalation — the server exits on stdin EOF).
- `spawnAgent` uses `child_process.spawn(command, args, { cwd, stdio: ["pipe","pipe","pipe"] })` and never touches `process.cwd()` (spec §4.1: the extension host's cwd is not documented to be the workspace root); a spawn `error` (ENOENT) is reported through `hooks.onExit(null, null)` after `hooks.onLog` names the command.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd clients/vscode && npm test && npm run typecheck`
Expected: PASS — all Task 1 and Task 2 tests; typecheck silent.

- [ ] **Step 5: Commit**

```bash
git add clients/vscode/src/agentSession.ts clients/vscode/test/fakeChild.ts clients/vscode/test/agentSession.test.ts
git commit -m "feat(vscode): the transport correlates requests, so an approval can be answered mid-turn"
```

---

### Task 3: Protocol semantics — the typed session wrapper

**Files:**
- Create: `clients/vscode/src/session.ts`
- Modify: `crates/schema/protocol.toml` (the `client` doc string, one line)
- Regenerate: `crates/schema/src/generated.rs`, `crates/schema/tests/generated_roundtrip.rs`, `clients/ts/protocol.generated.ts`, `clients/ts/protocol.samples.json` (by script, never by hand)
- Test: `clients/vscode/test/session.test.ts`

**Interfaces:**
- Consumes: `AgentSession` (Task 2), and the generated types/constants from `../../ts/protocol.generated.ts`.
- Produces: `LoongSession`, the only place protocol method names and their result shapes appear. Later tasks use exactly:

```ts
// src/session.ts
import type { AgentSession } from "./agentSession.ts";
import type {
  ContextExplain, EventEnvelope, ModelList, SessionAttachResult,
} from "../../ts/protocol.generated.ts";
import type { ApprovalScope } from "../../ts/protocol.generated.ts";

export class SessionRefusedError extends Error {
  constructor(readonly sessionId: string, message: string);
}

export class LoongSession {
  constructor(agent: AgentSession);
  get sessionId(): string | undefined;
  attach(cwd: string): Promise<SessionAttachResult>;   // sends client: "vscode"
  newSession(): Promise<string>;                        // returns the new session id
  replay(): Promise<EventEnvelope[]>;
  models(): Promise<ModelList>;
  selectModel(name: string): Promise<ModelList>;
  context(): Promise<ContextExplain>;
  setAutoApprove(on: boolean): Promise<boolean>;
  respondApproval(requestId: string, granted: boolean, scope: ApprovalScope, reason?: string): Promise<boolean>;
  cancel(): void;
  readonly events: EventEnvelope[];                     // everything seen so far (replay + live)
}
```

- [ ] **Step 1: Write the failing tests** (`test/session.test.ts`, driving `FakeChild` through `AgentSession`)

```ts
// attach records the session id and asks as "vscode"
//   answer session.attach with {session_id:"s1",schema_version:"0.1.0",existing_events:0}
//   → await attach("/w") resolves; session.sessionId === "s1"
//   → the written request's params.client === "vscode" and params.cwd === "/w"
// a refusal names the session it is not on
//   answer with error {code:-32000,message:"this server is on session \"s1\""}
//   → rejects SessionRefusedError and err.message contains "s1"
// newSession updates the id from the result
//   → session.new → {session_id:"s2"} → sessionId === "s2"
// replay returns the events and they accumulate in .events
//   answer session.replay with two message.appended events → length 2
//   a later live 'event' notification appends → length 3
// respondApproval sends the scope it was given
//   → the written params are {session_id, request_id, granted:false, scope:"once", reason:"denied in VS Code"}
// setAutoApprove sends the flag and reports the new state
// cancel reaches the child
//   → session.cancel() → child.signals equals ["SIGINT"]
```

- [ ] **Step 1: Run the tests to verify they fail**

Run: `cd clients/vscode && npm test`
Expected: FAIL — `Cannot find module '../src/session.ts'`.

- [ ] **Step 2: Implement `LoongSession` in `src/session.ts`**

Determined by the tests and the generated schema:
- Every request body carries the current `session_id` where the schema's params type has one (all of them except `session.attach`/`session.new`); a request sent before any id exists throws with a message saying to attach first.
- Subscribe to the `event` notification in the constructor: its method string is exactly `"event"` (`crates/protocol/src/lib.rs:24`, `EVENT_NOTIFICATION`) and its params **are** the `EventEnvelope`, not a wrapper (note that in a comment: `NOTIFICATIONS` in the generated file names only `model.delta`, so the `event` kind is not in any generated constant).
- Map a refusal (`error.data` naming a different session, or a message containing `session`) onto `SessionRefusedError` so callers re-attach instead of retrying blindly (Review Focus #5).
- Method names come from `METHODS.*`; event kinds are compared against `EVENTS.*`.

- [ ] **Step 3: Widen the `client` doc string and regenerate**

In `crates/schema/protocol.toml`, `SessionAttachParams.client`'s doc currently reads `tui | web | acp`; it becomes `tui | web | vscode | acp`. This is a doc-only change: no field, type, or wire change.
Run: `python3 scripts/gen-schema.py`
Expected: four generated files rewritten; `python3 scripts/gen-schema.py --check` exits 0.

- [ ] **Step 4: Run the tests, the typecheck, and the schema check**

Run: `cd clients/vscode && npm test && npm run typecheck`
Run: `python3 scripts/gen-schema.py --check`
Expected: PASS; schema check silent.

- [ ] **Step 5: Commit**

```bash
git add clients/vscode/src/session.ts clients/vscode/test/session.test.ts crates/schema/protocol.toml crates/schema/src/generated.rs crates/schema/tests/generated_roundtrip.rs clients/ts/protocol.generated.ts clients/ts/protocol.samples.json
git commit -m "feat(vscode): the session wrapper speaks the named protocol, and the schema stops omitting vscode"
```

---

### Task 4: `TurnController` — the approval pause/resume loop, and the link proven

This task delivers **spec step 0** and the spec's hardest invariant (§4.4): a turn pauses several times, and each pause is a request the extension must answer while the previous one is still outstanding.

**Files:**
- Create: `clients/vscode/src/turn.ts`
- Test: `clients/vscode/test/turn.test.ts`
- Modify: `clients/vscode/src/extension.ts` (register `loongagent.smoke`), `clients/vscode/package.json` (the `loongagent.smoke` command)

**Interfaces:**
- Consumes: `LoongSession` (Task 3).
- Produces:

```ts
// src/turn.ts
import type { LoongSession } from "./session.ts";
import type { ActionClass, ApprovalScope, EventEnvelope } from "../../ts/protocol.generated.ts";

export interface ApprovalRequest {
  request_id: string;
  action_class: ActionClass;
  reason: string;
}
export interface ApprovalAnswer {
  granted: boolean;
  scope: ApprovalScope;
  reason?: string;
}
export type Decider = (request: ApprovalRequest) => Promise<ApprovalAnswer>;

export class TurnBusyError extends Error {}

export interface TurnHooks {
  onEvent(event: EventEnvelope): void;
  onDelta(text: string): void;
  decide: Decider;
}

export class TurnController {
  constructor(session: LoongSession, hooks: TurnHooks);
  get busy(): boolean;
  get pendingApproval(): ApprovalRequest | null;
  run(text: string): Promise<{ turn_id: number; accepted: boolean }>;
}
```

- [ ] **Step 1: Write the failing tests** (`test/turn.test.ts`, answering `FakeChild` by method)

```ts
// one approval pause is answered and the turn finishes
//   prompt.submit gets no answer until the test, having emitted approval.requested
//   decide returns {granted:true,scope:"once"}; then answer prompt.submit
//   → run resolves; the child received approval.respond with the request_id; decide called once
// the pending approval is visible before prompt.submit resolves
//   emit approval.requested, do NOT answer prompt.submit
//   → controller.pendingApproval.request_id is set (the server streams events before its response)
// two approvals in one turn are both answered, in order
//   first approval.respond's result is answered only after a second approval.requested arrives
//   → decide called twice, with the two different request_ids in order; run resolves once
// a denied approval is faithfully reported
//   decide returns {granted:false,scope:"once",reason:"denied in VS Code"}
//   → the written approval.respond params.granted === false and reason matches
// a second run while a turn is running is refused   [Review Focus 1]
//   start run() without finishing it; the second run() rejects TurnBusyError and writes nothing
// a child that exits mid-turn rejects run with the exit code   [Review Focus 2]
//   → rejects with a message naming code 7; busy becomes false again
// bus is released after a failed turn
//   after the rejection above, a new run() with an answered prompt.submit resolves
```

- [ ] **Step 1: Run the tests to verify they fail**

Run: `cd clients/vscode && npm test`
Expected: FAIL — `Cannot find module '../src/turn.ts'`.

- [ ] **Step 2: Implement `TurnController` in `src/turn.ts`**

The loop, exactly as the protocol forces it (spec §4.4):

1. `run` sets `busy`, registers the `event` handler, then `await session.request(METHODS.promptSubmit, {session_id, text})`.
2. It records `approval.requested` events as `pendingApproval` (there is at most one at a time).
3. When the prompt request resolves: while `pendingApproval` is set — take it, clear it, `await hooks.decide(request)`, then `await session.respondApproval(...)` with the answer, and repeat.
4. Clear `busy` in a `finally`, whichever way it ended.

`model.delta` notifications go to `hooks.onDelta`; everything else goes through `session.events` to `hooks.onEvent`. `approval.decided` clears `pendingApproval` if it matches.

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cd clients/vscode && npm test && npm run typecheck`
Expected: PASS.

- [ ] **Step 4: Register the smoke command and prove the link (spec step 0)**

Add `loongagent.smoke` to `package.json`'s `contributes.commands` and register it in `extension.ts`: resolve `loongagent.path`, spawn `server --stdio --cwd <first workspace folder>` plus `--model`/`extraArgs` when configured, `attach`, run one fixed prompt (`"Say hello in one sentence, then stop."`) with a decider that always denies, and print every delta and every event kind to the output channel. Pipe the child's stderr into the same channel.

Build, then verify by hand:
```bash
cd clients/vscode && npm run compile
code --extensionDevelopmentPath="$PWD" --new-window /tmp/loong-vscode-scratch
```
Run `loongagent: Smoke test` from the Command Palette, and confirm, in the `loongagent` output channel: an attached session id, a streamed answer arriving in pieces, and at least one `tool.call` or `model.selected` event line. Then find that session's log and confirm it is the same session:
```bash
ls -t ~/.loongagent/projects/*/<id>.jsonl | head -1
```
Expected: the streamed text matches the `message.appended` in the log. This is the step-0 acceptance in the spec §5.

- [ ] **Step 5: Commit**

```bash
git add clients/vscode/src/turn.ts clients/vscode/test/turn.test.ts clients/vscode/src/extension.ts clients/vscode/package.json
git commit -m "feat(vscode): a turn pauses at each approval and resumes, and the link is proven end to end"
```

---

### Task 5: `Renderer` — events to UI parts

**Files:**
- Create: `clients/vscode/src/render.ts`
- Test: `clients/vscode/test/render.test.ts`

**Interfaces:**
- Consumes: `EventEnvelope`, `JsonValue`, `EVENTS` (generated).
- Produces:

```ts
// src/render.ts
import type { EventEnvelope, JsonValue } from "../../ts/protocol.generated.ts";

export type UiPart =
  | { kind: "markdown"; text: string }
  | { kind: "progress"; text: string };

export const MAX_TOOL_OUTPUT_CHARS = 4000;

export class Renderer {
  constructor(options: { cwd: string });
  beginTurn(): void;
  delta(text: string): UiPart[];
  event(envelope: EventEnvelope): UiPart[];
  touchedFiles(): string[];
}

/** Absolute paths that appear as string values in a tool call's arguments. */
export function pathsInArgs(args: JsonValue): string[];
```

- [ ] **Step 1: Write the failing tests** (`test/render.test.ts`)

```ts
// a model delta becomes markdown, verbatim
//   delta("hi") → [{kind:"markdown",text:"hi"}]      (no trimming: chunks are mid-word)
// a tool call reports its action class
//   event tool.call {tool:"write_file",action_class:"write_workspace",args:{path:"/w/a.txt"}}
//   → one progress part naming the tool, and no verdict word of our own
// a tool result longer than the cap is truncated with a marker   [Review Focus 3]
//   output of 10_000 chars → the markdown part is ≤ MAX_TOOL_OUTPUT_CHARS + 200
//   and its text contains "truncated"
// an unknown event kind renders nothing
//   event kind "something.new" → []
// a policy decision names the rule that fired
//   policy.decided {verdict:"allow",rule:"builtin:write_workspace"} → text contains the rule
// an approval decision names the scope that was chosen
//   approval.decided {granted:true,scope:"action_class"} → text contains "action_class"
// the extension does not repeat the user's own message
//   message.appended {role:"user"} → []
// pathsInArgs finds absolute paths and ignores flags and urls
//   {path:"/w/a.txt", url:"https://x/y", flags:["--verbose","/w/b.txt"]}
//   → ["/w/a.txt","/w/b.txt"]
// touchedFiles accumulates within a turn and resets on beginTurn
```

- [ ] **Step 1: Run the tests to verify they fail**

Run: `cd clients/vscode && npm test`
Expected: FAIL — `Cannot find module '../src/render.ts'`.

- [ ] **Step 2: Implement `Renderer` and `pathsInArgs` in `src/render.ts`**

Rules: compare `envelope.kind` against `EVENTS.*`; never invent a verdict, rule, or scope value — quote what the event carries. The tool-call line prints each touched path relative to `options.cwd` when it is inside it, and absolute when it is not (a path outside the workspace must stay legible as outside). Truncation keeps the head and appends a marker naming how much was dropped. `pathsInArgs` walks the JSON, collects string values that start with `/` (and, on a non-POSIX path, with a drive letter), rejects values containing `://`, deduplicates, and does **not** touch the filesystem.

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cd clients/vscode && npm test && npm run typecheck`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add clients/vscode/src/render.ts clients/vscode/test/render.test.ts
git commit -m "feat(vscode): the renderer quotes events instead of deciding what they mean"
```

---

### Task 6: `FileSnapshots` — the diff's left side

**Files:**
- Create: `clients/vscode/src/snapshots.ts`
- Test: `clients/vscode/test/snapshots.test.ts`

**Interfaces:**
- Consumes: nothing (Node `fs` only).
- Produces:

```ts
// src/snapshots.ts
export interface Snapshot {
  path: string;
  content: string | undefined;
  /** where the left side came from; "none" means nobody can show a left side */
  source: "snapshot" | "none";
  /** why there is no content, when there is none: "new file" | "unreadable" | "outside the workspace" */
  note?: string;
}

export class FileSnapshots {
  constructor(options: { workspace: string });
  /** Read and keep each path's current content; the first sighting wins. */
  see(paths: string[]): void;
  left(path: string): Snapshot | undefined;
  clear(): void;
}
```

- [ ] **Step 1: Write the failing tests** (`test/snapshots.test.ts`, using `node:fs` and a temp dir under `os.tmpdir()`)

```ts
// the first sighting is the one kept
//   write "one", see([p]), write "two", see([p])
//   → left(p).content === "one" and source === "snapshot"
// a path outside the workspace is never read   [Review Focus 4]
//   see(["/etc/hostname"]) with workspace = the temp dir
//   → left(...).content === undefined, source === "none", note === "outside the workspace"
//   and the file's bytes were not read (assert by using a path that does not exist but is outside
//   the workspace: the note must still be "outside the workspace", not "unreadable")
// a file that does not exist yet is a new file
//   see([tempDir + "/new.txt"]) → content === undefined, note === "new file"
// an unreadable file records why instead of throwing
//   chmod 000 a file (skip this assertion when running as root) → note === "unreadable"
// clear forgets everything
//   see then clear then left → undefined
```

- [ ] **Step 1: Run the tests to verify they fail**

Run: `cd clients/vscode && npm test`
Expected: FAIL — `Cannot find module '../src/snapshots.ts'`.

- [ ] **Step 2: Implement `FileSnapshots` in `src/snapshots.ts`**

Containment check before any read: `path.resolve(candidate)` must equal the workspace or start with `workspace + path.sep`. Reads are `readFileSync(path, "utf8")` inside a try/catch; an `ENOENT` is `"new file"`, anything else is `"unreadable"` (with the error's code appended to the note). The class never writes.

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cd clients/vscode && npm test && npm run typecheck`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add clients/vscode/src/snapshots.ts clients/vscode/test/snapshots.test.ts
git commit -m "feat(vscode): the diff's left side is the file at first mention, contained to the workspace"
```

---

### Task 7: The chat participant — `@loong` end to end

This task delivers **spec step 1** (minus resume, which is Task 8).

**Files:**
- Create: `clients/vscode/src/workspaceAgent.ts`
- Modify: `clients/vscode/src/extension.ts`
- Modify: `clients/vscode/package.json` (nothing new is needed; the participant is already contributed)

**Interfaces:**
- Consumes: `spawnAgent`/`AgentSession` (2), `LoongSession` (3), `TurnController` (4), `Renderer` (5), `FileSnapshots` (6).
- Produces:

```ts
// src/workspaceAgent.ts
import * as vscode from "vscode";
import type { TurnController } from "./turn.ts";
import type { LoongSession } from "./session.ts";

export class WorkspaceAgent {
  constructor(options: {
    folder: vscode.WorkspaceFolder;
    output: vscode.OutputChannel;
    context: vscode.ExtensionContext;
  });
  /** Spawn if needed and attach; resolves to the live session. */
  ensure(): Promise<LoongSession>;
  /** A controller bound to this agent's event stream and the caller's decider. */
  controller(hooks: { onEvent: (event: EventEnvelope) => void; onDelta: (text: string) => void; decide: Decider }): Promise<TurnController>;
  get sessionId(): string | undefined;
  dispose(): void;
}
```

- [ ] **Step 1: Implement `WorkspaceAgent`**

Responsibilities: resolve `loongagent.path` (plus `--model`, `extraArgs` when configured); spawn with `server --stdio --cwd <folder>` plus `--session <stored id>` or `--new` (Task 8 adds the stored id — for now always `--new`); `attach`; keep one `LoongSession`; kill the child in `dispose()`; report an unexpected exit to the output channel and clear the session so the next `ensure()` respawns. No policy, no rendering.

- [ ] **Step 2: Implement the participant in `extension.ts`**

- Register with `vscode.chat.createChatParticipant("loongagent.chat", handler)` and push it onto `context.subscriptions`.
- Gate on trust: if `!vscode.workspace.isTrusted`, reply with the reason and do not spawn; subscribe to `vscode.workspace.onDidGrantWorkspaceTrust` to start afterwards.
- Require exactly one workspace folder (the first) and say so plainly when there is none.
- `request.command` dispatch: `new` → `LoongSession.newSession()` and say the new id; `model`, `context`, `yolo` → reply `"/<cmd> is not implemented yet"` (Tasks 9 and 11 fill them in).
- Otherwise: `stream.progress("thinking")`, then `controller.run(promptText(request))` with a decider that **denies with the fixed reason** until Task 9 lands (`{granted:false, scope:"once", reason:"approvals are not implemented yet"}`), forwarding `onDelta` to `stream.markdown` and `onEvent` parts to `stream.markdown`/`stream.progress` per `UiPart.kind`.
- `#file` references (spec §4.3): `promptText(request)` returns `request.prompt` unchanged when `request.references` is empty, and otherwise appends a block listing each reference's absolute path — `referenced files:\n- <path>` — and nothing else. The kernel's own read tools fetch the contents; inlining bodies is out of scope (spec §3), so do not read those files here.
- Stop: `token.onCancellationRequested(() => session.cancel())`, registered before the turn starts and disposed when it ends. The reply afterwards says the turn was interrupted and, if the event arrived, quotes `turn.interrupted`'s `reason`.
- Lifecycle reporting: the output channel gets one line per transition, using exactly these words — `spawning`, `ready`, `turn running`, `awaiting approval`, `stopped` — so "is it stuck or is it waiting for me?" is answerable from the channel.
- Errors: `TurnBusyError` → a markdown line saying a turn is already running (Review Focus 1); anything else → the message plus a line pointing at the `loongagent` output channel.
- Register `context.subscriptions` disposal so the child dies with the extension; kill the child in `deactivate()`.

- [ ] **Step 3: Verify by hand (spec step 1 acceptance)**

```bash
cd clients/vscode && npm run compile
code --extensionDevelopmentPath="$PWD" --new-window /tmp/loong-vscode-scratch
```
Expected, in the Extension Development Host: open Chat, type `@loong say hello and tell me which model you are`; the answer streams in word by word. Then `/new` reports a new session id, and the output channel shows the matching log path. Then attach a file with `#file:` and ask a question about it: the prompt the child receives (visible in the channel's start line, or by asking the model to repeat the block) names the path, and the extension did not paste the file's contents. While a turn is running, press Stop in the chat: the transcript says the turn was interrupted and the child process is still alive (`pgrep -f "loongagent server"`). Cross-check the session log:
```bash
ls -t ~/.loongagent/projects/*/*.jsonl | head -1
```
Expected: the same session id appears in the log path, and its `message.appended` matches what the chat shows.

- [ ] **Step 4: Commit**

```bash
git add clients/vscode/src/workspaceAgent.ts clients/vscode/src/extension.ts
git commit -m "feat(vscode): @loong streams the agent's answer into the native chat view"
```

---

### Task 8: Resume and replay across a window reload

**Files:**
- Modify: `clients/vscode/src/workspaceAgent.ts`
- Test: `clients/vscode/test/render.test.ts` (the resumed-session summary is a renderer rule)

**Interfaces:**
- Consumes: Task 7's `WorkspaceAgent`.
- Produces: `Renderer.resumed(events: EventEnvelope[]): UiPart[]` — the compact "resumed session" card (spec §4.6).

- [ ] **Step 1: Write the failing test**

```ts
// a resumed session is summarised, not replayed in full
//   twenty message.appended events and one approval.decided
//   → the first part names the session's event count, and the markdown contains the last user
//     message text and the last assistant message text, and does NOT contain the first message's text
// an empty replay says so instead of rendering nothing
//   resumed([]) → exactly one markdown part containing "no events"
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd clients/vscode && npm test`
Expected: FAIL — `renderer.resumed is not a function`.

- [ ] **Step 3: Implement `resumed` and the persistence**

- `Renderer.resumed`: name the event count, quote the newest user message and the newest assistant message, and nothing older.
- `WorkspaceAgent`: read the session id from `context.workspaceState.get<string>("loongagent.sessionId")`; pass `--session <id>` when present and otherwise `--new`; store the id after every successful `attach`/`newSession`. When the stored id fails to resolve, print the failure, say plainly that a new session is starting, and retry once with `--new` (Review Focus 5) — never loop.
- The participant, on the first request of an activation with a resumed session, emits `renderer.resumed(session.events)` before the answer.

- [ ] **Step 4: Verify by hand (spec §2 criterion 4)**

In the Extension Development Host: ask one question, then run `Developer: Reload Window`, then ask another.
Expected: after the reload the same session id is re-attached; the first answer of the new chat names the session and its event count and shows the previous exchange; the `--session` argument is visible in the output channel's start line.

- [ ] **Step 5: Run the tests and commit**

Run: `cd clients/vscode && npm test && npm run typecheck`
Expected: PASS.

```bash
git add clients/vscode/src/render.ts clients/vscode/src/workspaceAgent.ts clients/vscode/test/render.test.ts
git commit -m "feat(vscode): a reload re-attaches to the same session and says where you were"
```

---

### Task 9: Approvals in the chat

This task delivers **spec step 2**.

**Files:**
- Modify: `clients/vscode/src/extension.ts`
- Modify: `clients/vscode/src/render.ts` (the `permissions.relaxed` line already exists in spirit — make it explicit)
- Test: `clients/vscode/test/render.test.ts`

**Interfaces:**
- Consumes: `Decider`, `ApprovalRequest`, `ApprovalAnswer` (Task 4).
- Produces: `export function decideFromModal(request: ApprovalRequest): Promise<ApprovalAnswer>` in `extension.ts`, mapping the three buttons onto the protocol's scopes; and the `/yolo` command handling.

- [ ] **Step 1: Write the failing test**

```ts
// a relaxed session is announced, not silently quieter   [spec §4.4, ADR-0007 §5]
//   permissions.relaxed {auto_approve:true,reason:"…"}
//   → one markdown part containing "approval prompts are off"
// an approval request names the action class and the reason the policy gave
//   approval.requested {action_class:"out_of_bounds",reason:"network access"}
//   → text contains "out_of_bounds" and "network access"
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd clients/vscode && npm test`
Expected: FAIL — no part contains the relaxed-session text, and the approval line lacks the action class.

- [ ] **Step 3: Implement the two renderer rules**

Quote the event's own `action_class`, `reason`, and `auto_approve`; compute nothing.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd clients/vscode && npm test`
Expected: PASS.

- [ ] **Step 5: Implement the modal decider and `/yolo`**

- `decideFromModal`: `vscode.window.showWarningMessage(<the policy's reason and the action class>, { modal: true }, "Allow once", "Allow always", "Deny")` → `once` / `action_class` / `{granted:false, scope:"once", reason:"denied in VS Code"}`. A dismissed modal is a denial, never an allow. The `project` scope is deliberately not offered (spec §4.4 and §8) — leave a comment saying so.
- Replace the placeholder decider from Task 7.
- `/yolo`: read the current state with `models`-style defensive coding — there is no "read permissions" method, so keep the extension's own last-known value in `workspaceState` (default from `loongagent.autoApprove`), call `setAutoApprove`, and let the resulting `permissions.relaxed` event do the announcing. State plainly in the reply that the sandbox is untouched (spec §4.4, ADR-0007 §5).

- [ ] **Step 6: Verify by hand (spec §5 criterion 2)**

In the Extension Development Host: `@loong create a file named smoke.txt containing the word hello`.
Expected: a modal appears naming the action class and the policy's reason; **Deny** leaves no file and the transcript shows the denial, and the log has `approval.decided {granted:false}`; on a second run **Allow once** creates the file and the log records `approval.decided {granted:true,scope:"once"}`. Then `/yolo` and a third write: no modal, and the transcript carries the `permissions.relaxed` line.

- [ ] **Step 7: Commit**

```bash
git add clients/vscode/src/extension.ts clients/vscode/src/render.ts clients/vscode/test/render.test.ts
git commit -m "feat(vscode): the human answers approvals in the editor and the log says which scope they chose"
```

---

### Task 10: Tool-call rendering and the changed-file review

This task delivers **spec step 3**.

**Files:**
- Modify: `clients/vscode/src/extension.ts`
- Modify: `clients/vscode/package.json` (`loongagent.reviewChanges` command)
- Create: `clients/vscode/src/diffProvider.ts`
- Test: `clients/vscode/test/diffProvider.test.ts`

**Interfaces:**
- Consumes: `FileSnapshots` (6), `pathsInArgs` (5), `TurnController` (4).
- Produces:

```ts
// src/diffProvider.ts
/** The left-hand side of a vscode.diff: what a path held when the turn first mentioned it. */
export class DiffProvider {
  constructor(snapshots: FileSnapshots, gitShow: (path: string) => string | undefined);
  /** A URI carrying the absolute path whose left side this document serves. */
  uriFor(path: string, label: string): vscode.Uri;
  provideTextDocumentContent(uri: vscode.Uri): string;
}
```

- [ ] **Step 1: Write the failing tests** (`test/diffProvider.test.ts`, no VS Code: pass a stub `Uri` factory)

```ts
// a snapshotted path serves its snapshot, not git
//   snapshots.left(p).source === "snapshot" → provideTextDocumentContent(uriFor(p,"before")) returns the snapshot text
//   and the stub gitShow was never called
// a path with no snapshot falls back to git and says so
//   left(p) === undefined → content comes from gitShow(p), and a marker line names git as the source
// a path with neither yields an empty document plus a marker
//   gitShow returns undefined → content contains "no previous version"
// a new file's left side is empty and named as new
//   left(p).note === "new file" → content is "" and the marker says "new file"
// the path survives the round trip through the URI
//   uriFor("/w/a b.txt") → provideTextDocumentContent resolves back to exactly that path (spaces included)
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd clients/vscode && npm test`
Expected: FAIL — `Cannot find module '../src/diffProvider.ts'`.

- [ ] **Step 3: Implement `DiffProvider` and wire the review command**

- Register the provider once per activation under a private scheme (`loongagent-left`) with `vscode.workspace.registerTextDocumentContentProvider`; the provider reads the path back out of the URI (`Uri.path`, decoded), so no state travels outside the URI.
- During a turn, `pathsInArgs` from every `tool.call`'s args goes to `snapshots.see(...)`; the session's `session.events` are the source (no second event handler).
- At the end of a turn, if any file was touched, `stream.button({ command: "loongagent.reviewChanges", title: "Review N changed files" })`; the command opens `vscode.diff(leftUri, vscode.Uri.file(path), "<name> (before ↔ after)")` per file, side by side.
- The `gitShow` implementation shells out to `git show HEAD:<relative path>` with the workspace as cwd, and returns `undefined` on any failure — the diff must still open when git is absent, dirty, or the file is untracked.

- [ ] **Step 4: Verify by hand (spec §5 criterion 3)**

In the Extension Development Host: `@loong add a comment line to the top of two files in this workspace`, approve, then click the review button.
Expected: one diff tab per file opens; each left side equals what the file held before the turn (verify by making the files dirty *before* the prompt — the left side must show the dirty content, not HEAD's). The transcript names the source of each left side.

- [ ] **Step 5: Commit**

```bash
git add clients/vscode/src/diffProvider.ts clients/vscode/test/diffProvider.test.ts clients/vscode/src/extension.ts clients/vscode/package.json
git commit -m "feat(vscode): a turn's changes open as diffs, with the file as it was when first touched"
```

---

### Task 11: The daily-use threshold

This task delivers **spec step 4**.

**Files:**
- Modify: `clients/vscode/src/extension.ts` (the `model`, `context`, `new` commands' final form; error paths)
- Modify: `scripts/check.sh` (one line: the extension's typecheck and tests)
- Test: `clients/vscode/test/render.test.ts` (the model and context renderings)

**Interfaces:**
- Consumes: `LoongSession.models/selectModel/context` (Task 3).
- Produces: `Renderer.models(list: ModelList): UiPart[]` and `Renderer.context(explain: ContextExplain): UiPart[]` (both pure, both testable without VS Code); `/model` (quick pick over `models()`, then `selectModel`) and `/context` (a markdown table from `context()`).

- [ ] **Step 1: Write the failing tests**

```ts
// the model list names the current model
//   models() → {models:[{name:"a",…},{name:"b",…}],selected:"b"}
//   → the rendered text contains "a" and "b" and marks "b" as current
// the context table quotes the numbers it was given
//   context {context_tokens:1200,budget_tokens:8000,messages:4,tools:9,compactions:0,entries:[],categories:[]}
//   → text contains "1200", "8000", "4" and "9", and does not compute a percentage of its own
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cd clients/vscode && npm test`
Expected: FAIL — `renderer.models is not a function` / `renderer.context is not a function`.

- [ ] **Step 3: Implement the two renderer rules and the two command handlers**

`/model` lists `models().models` in a `showQuickPick` (marking `models().selected`), then calls `selectModel` with the chosen `name`. `/context` renders `context()` as a markdown table, quoting the numbers it was given and computing no percentage of its own.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd clients/vscode && npm test`
Expected: PASS.

- [ ] **Step 5: Finish the error paths and the follow-ups**

- Binary not found (spawn `ENOENT`): name `loongagent.path` and the command it tried.
- The child exits unexpectedly: report the exit code in the channel and in the next reply; respawn on the next request.
- A request that never gets a response: `AgentSession.request` rejects after 120 s with "no response to <method> after 120s" (spec §4.9). Add this timeout to Task 2's `request` and one test for it in `test/agentSession.test.ts`.
- Follow-ups (spec §4.3): set `participant.followupProvider` so a finished turn offers "Review changes" (Task 10's command) and "Explain this". Keep the labels to those two — a menu nobody reads is worse than no menu.

- [ ] **Step 6: Add the extension to the repo gate**

Append to `scripts/check.sh`, after the existing cargo work, so a broken extension fails the same gate as a broken crate:

```bash
# The VS Code surface: types and unit tests (no VS Code needed — the logic modules have no vscode import).
( cd clients/vscode && npm ci --silent && npm run typecheck && npm test )
```

Run: `bash scripts/check.sh`
Expected: the existing checks unchanged and the extension's typecheck + tests green.

- [ ] **Step 7: Verify the whole thing by hand (spec §5, step 4)**

In the Extension Development Host: `/model` switches the session's model and the log gains `model.selected`; `/context` shows a table whose token count matches what the TUI shows for the same session; `/new` starts a fresh session and the chat says so. Then use it for real work for an afternoon without reloading.

- [ ] **Step 8: Commit**

```bash
git add clients/vscode/src/extension.ts clients/vscode/src/agentSession.ts clients/vscode/test/render.test.ts clients/vscode/test/agentSession.test.ts scripts/check.sh
git commit -m "feat(vscode): model, context and the error paths land, and the gate typechecks the extension"
```

---

## After the plan

Spec step 5 (the ACP adapter) is deliberately **not** in this plan: it is a different subsystem (`crates/acp` plus a `loongagent acp` CLI arm and a cancel entry point in the server), it changes no extension file except a second implementation behind `agentSession.ts`'s interface, and the spec's §8 records when to reconsider. If step 3's UX proves too flat, §8's escalation (proposed `chatParticipantAdditions`, or a `secondarySidebar` webview view) is likewise a new spec, not a task here.
