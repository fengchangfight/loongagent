# VS Code sidebar surface — M1: single-tab Webview Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use subagent-driven-development (recommended) or executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the Copilot-chat participant with an independent Activity Bar Webview that chats with the real loongagent server, streams answers, shows tool cards, reviews writes with a diff, and works without Copilot sign-in.

**Architecture:** The extension host owns the child process, session, turn controller, transcript and diff snapshots. A `WebviewViewProvider` bridges typed JSON messages to a vanilla TypeScript/CSS Webview. All logic that can run without VS Code is pure and unit-tested; the provider is thin glue.

**Tech Stack:** TypeScript (strict, `tsc --noEmit`), esbuild (two entries: extension CJS and Webview IIFE), Node's built-in test runner, VS Code stable APIs only.

**Spec:** [2026-10-26-vscode-webview-design.md](../specs/2026-10-26-vscode-webview-design.md). Read it with this plan; every "spec §" below points there. M2 (tabs + history + `session.list`) is a separate plan that depends on this one.

## Global Constraints

- **Start from `main` at the spec-approval commit.** The uncommitted placeholder-model workaround (if it is still in the working tree) is not part of this plan: discard `clients/vscode/src/modelProvider.ts`, restore the three files it touched, then start. Do not commit unrelated changes.
- **Stable VS Code APIs only.** No `enabledApiProposals`, no proposed-only parts (`ChatResponseMultiDiffPart`, `ChatToolInvocationPart`, `chatSessionsProvider`, …).
- **No binary is bundled.** `BinaryResolver` finds the separately installed `loongagent`.
- **`loongagent.path`, `loongagent.autoApprove` and `loongagent.extraArgs` are machine-scoped.** A workspace setting must not choose the executable or relax approvals. `loongagent.model` may stay resource-scoped.
- **The extension never changes policy.** Review-before-write relies on the user's `[permissions] write_workspace = "ask"`; with `allow`, the UI says "written unreviewed".
- **Pure modules have no `vscode` import:** `binary.ts`, `tab.ts`, `transcript.ts`, `webviewProtocol.ts`, `diff.ts`, `markdown.ts`, `approval.ts`, `sessionCore.ts`, `prompt.ts`, and everything under `webview/`.
- **TypeScript rules:** relative imports carry `.ts`; type-only imports use `import type`; `strict` and `verbatimModuleSyntax` are on. Do not add runtime dependencies.
- **Limits:** `MAX_TABS = 5`; history limit 50 (M2); request timeout 120 s; tool output cap 4000 chars.
- **`favicon.png` stays untracked.** Derive `media/loong-24.png`, `media/loong-48.png`, `media/loong-128.png`; commit only the derived files.
- **Every commit names its paths.** `git add <paths>`; never `git add -A`.
- **Do not edit `docs/human-written/`.**
- **Test commands:** `cd clients/vscode && npm test` and `npm run typecheck` (or `npm run compile`). Node 24 is the floor for `.ts` tests; the CI plan adds a Node pin.

## Review Focus

Five failure modes the spec implies, each pinned by a test in the task that owns the code:

1. **A model writes raw HTML/script** (`<img src=x onerror=…>`, `<script>`): the transcript must show it as text, never execute it. → Task 4.
2. **A tool names a relative path** (`write_file {path:"demo.txt"}`): the review list must resolve it against the workspace; absolute-only harvesting silently hides real changes. → Task 7.
3. **`edit` names an absent or ambiguous `old_string`**: the approval card must say the kernel will refuse, not show a fabricated diff. → Task 5.
4. **The child exits unexpectedly**: the next send must get a fresh session and a fresh turn gate, not the dead session's gate. → Task 8.
5. **A tab closes while an approval is pending**: the pending decision must resolve to Deny with reason `tab closed`, never hang or silently allow. → Task 10.

---

### Task 1: BinaryResolver

**Files:**
- Create: `clients/vscode/src/binary.ts`
- Test: `clients/vscode/test/binary.test.ts`

**Interfaces:**
- Consumes: nothing.
- Produces:
  ```ts
  export type BinarySource = "setting" | "picked" | "path" | "candidate";
  export interface ResolvedBinary { path: string; source: BinarySource; }
  export interface BinaryEnv {
    configured: string;
    picked?: string;
    platform: NodeJS.Platform;
    home: string;
    pathEnv: string | undefined;
    exists(path: string): boolean;
    isExecutable(path: string): boolean;
  }
  export function binaryName(platform: NodeJS.Platform): string;
  export function candidatePaths(platform: NodeJS.Platform, home: string): string[];
  export function resolveBinary(env: BinaryEnv): ResolvedBinary | undefined;
  export function parseVersion(stdout: string): { version: string; schema: string; commit?: string } | undefined;
  ```

- [ ] **Step 1: Write the failing test**

```ts
import { test } from "node:test";
import assert from "node:assert/strict";
import { binaryName, candidatePaths, parseVersion, resolveBinary } from "../src/binary.ts";

const env = (over: Partial<Parameters<typeof resolveBinary>[0]> = {}) => ({
  configured: "loongagent",
  platform: "linux" as const,
  home: "/home/u",
  pathEnv: "/usr/bin:/home/u/.cargo/bin",
  exists: (p: string) => ["/home/u/.cargo/bin/loongagent"].includes(p),
  isExecutable: () => true,
  ...over,
});

test("a configured absolute path wins", () => {
  const found = resolveBinary(env({ configured: "/opt/l/loongagent",
    exists: (p) => p === "/opt/l/loongagent" }));
  assert.deepEqual(found, { path: "/opt/l/loongagent", source: "setting" });
});
test("a picked path beats PATH", () => {
  const found = resolveBinary(env({ picked: "/home/u/bin/loongagent",
    exists: (p) => p === "/home/u/bin/loongagent" }));
  assert.deepEqual(found, { path: "/home/u/bin/loongagent", source: "picked" });
});
test("a missing setting falls through to PATH", () => {
  assert.deepEqual(resolveBinary(env()), {
    path: "/home/u/.cargo/bin/loongagent", source: "path" });
});
test("common candidates are the last resort", () => {
  const found = resolveBinary(env({ pathEnv: "/usr/bin",
    exists: (p) => p === "/home/u/.local/bin/loongagent" }));
  assert.deepEqual(found, { path: "/home/u/.local/bin/loongagent", source: "candidate" });
});
test("Windows uses loongagent.exe", () => {
  assert.equal(binaryName("win32"), "loongagent.exe");
  assert.ok(candidatePaths("win32", "C:\\Users\\u")
    .every((p) => p.endsWith("loongagent.exe")));
});
test("parseVersion reads version, schema and commit", () => {
  assert.deepEqual(parseVersion("loongagent 0.0.1 (schema 0.1.0, commit abc+dirty)"),
    { version: "0.0.1", schema: "0.1.0", commit: "abc+dirty" });
  assert.equal(parseVersion("not a version"), undefined);
});
```

The tests above pin the resolution order, Windows naming and version parsing.

- [ ] **Step 2: Run test to verify it fails**

Run: `cd clients/vscode && node --test test/binary.test.ts`
Expected: FAIL — `Cannot find module '../src/binary.ts'`.

- [ ] **Step 3: Implement `src/binary.ts`**

Resolution order: a path-like `configured` that exists; `picked`; PATH for `configured` (or `binaryName` when empty); `candidatePaths`. PATH splitting is `;` on Windows, `:` elsewhere. A path is executable when `isExecutable` says so; on Windows callers may pass `exists`. Candidate order: `~/.cargo/bin`, `~/.local/bin`, `/usr/local/bin`, `/opt/homebrew/bin`; on Windows `%USERPROFILE%\\.cargo\\bin\\loongagent.exe`.

- [ ] **Step 4: Run tests and typecheck to verify they pass**

Run: `cd clients/vscode && node --test test/binary.test.ts && npm run typecheck`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add clients/vscode/src/binary.ts clients/vscode/test/binary.test.ts
git commit -m "feat(vscode): resolve the loongagent binary without bundling it"
```

---

### Task 2: Webview protocol

**Files:**
- Create: `clients/vscode/src/webviewProtocol.ts`
- Test: `clients/vscode/test/webviewProtocol.test.ts`

**Interfaces:**
- Consumes: nothing.
- Produces: `TabState`, `UiPart`, `DiffPreview`, `Attachment`, `SessionInfo`, `HostMessage`, `WebviewMessage`, `BinaryStatus`, and `parseWebviewMessage(raw: unknown): WebviewMessage | undefined` exactly as the spec §8 defines them.
- `UiPart` includes `user`, `assistant` (`text` + sanitized `html`), `progress`, `tool`, `approval`, `review`, `notice`.
- `Attachment` is `{id, kind:"file", path}` or `{id, kind:"selection", path, startLine, endLine, text}`.

- [ ] **Step 1: Write the failing test**

```ts
import { test } from "node:test";
import assert from "node:assert/strict";
import { parseWebviewMessage } from "../src/webviewProtocol.ts";

test("a send is parsed", () => {
  assert.deepEqual(parseWebviewMessage({ type: "send", tabId: "t1", text: "hi" }),
    { type: "send", tabId: "t1", text: "hi" });
});
test("an approval choice is validated", () => {
  assert.equal(parseWebviewMessage({ type: "approval", tabId: "t1", requestId: "r", choice: "always" })?.type, "approval");
  assert.equal(parseWebviewMessage({ type: "approval", tabId: "t1", requestId: "r", choice: "maybe" }), undefined);
});
test("unknown and malformed messages are ignored", () => {
  assert.equal(parseWebviewMessage({ type: "nope" }), undefined);
  assert.equal(parseWebviewMessage({ type: "send", text: "hi" }), undefined);
  assert.equal(parseWebviewMessage(null), undefined);
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd clients/vscode && node --test test/webviewProtocol.test.ts`
Expected: FAIL — module missing.

- [ ] **Step 3: Implement the types and `parseWebviewMessage`**

Use a switch on `raw.type`; check every required field's type; return a fresh object. Unknown types and arrays return `undefined`.

- [ ] **Step 4: Run tests and typecheck**

Run: `cd clients/vscode && node --test test/webviewProtocol.test.ts && npm run typecheck`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add clients/vscode/src/webviewProtocol.ts clients/vscode/test/webviewProtocol.test.ts
git commit -m "feat(vscode): type the host-webview message boundary"
```

---

### Task 3: Transcript

**Files:**
- Create: `clients/vscode/src/transcript.ts`
- Test: `clients/vscode/test/transcript.test.ts`

**Interfaces:**
- Consumes: `UiPart` from `webviewProtocol.ts`.
- Produces:
  ```ts
  export class Transcript {
    all(): UiPart[];
    append(part: UiPart): void;
    updateTool(callId: string, patch: Partial<Extract<UiPart, { kind: "tool" }>>): void;
    updateApproval(requestId: string, patch: Partial<Extract<UiPart, { kind: "approval" }>>): void;
    reset(): void;
  }
  ```

- [ ] **Step 1: Write the failing test**

```ts
test("parts keep their order", () => {
  const t = new Transcript();
  t.append({ kind: "user", text: "u" });
  t.append({ kind: "notice", level: "info", text: "n" });
  assert.deepEqual(t.all().map((part) => part.kind), ["user", "notice"]);
});
test("a tool result updates the card with the same callId", () => {
  const t = new Transcript();
  t.append({ kind: "tool", callId: "c1", tool: "write_file",
    actionClass: "write_workspace", paths: [], status: "running" });
  t.updateTool("c1", { status: "ok", output: "done" });
  const [part] = t.all();
  assert.equal(t.all().length, 1);
  assert.equal(part.kind, "tool");
  assert.equal(part.status, "ok");
});
test("an unknown tool result does not invent a partial card", () => {
  const t = new Transcript();
  t.updateTool("c2", { status: "ok" });
  assert.equal(t.all().length, 0);
});
test("an approval can be marked allowed", () => {
  const t = new Transcript();
  t.append({ kind: "approval", requestId: "r1", actionClass: "write_workspace",
    reason: "ask", paths: [], state: "pending" });
  t.updateApproval("r1", { state: "allowed" });
  const [part] = t.all();
  assert.equal(part.kind, "approval");
  assert.equal(part.state, "allowed");
});
test("reset empties the transcript", () => {
  const t = new Transcript();
  t.append({ kind: "notice", level: "info", text: "n" });
  t.reset();
  assert.equal(t.all().length, 0);
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd clients/vscode && node --test test/transcript.test.ts`
Expected: FAIL — module missing.

- [ ] **Step 3: Implement `Transcript`**

Search from the end for `kind:"tool"` + `callId`, and for `kind:"approval"` + `requestId`; update in place and do nothing when absent. The host appends a complete result part when a result arrives without a call. Return a copy from `all()` so callers cannot mutate the stored array.

- [ ] **Step 4: Run tests**

Run: `cd clients/vscode && node --test test/transcript.test.ts`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add clients/vscode/src/transcript.ts clients/vscode/test/transcript.test.ts
git commit -m "feat(vscode): keep each tab's transcript as data"
```

---

### Task 4: Safe Markdown renderer

**Files:**
- Create: `clients/vscode/src/markdown.ts`
- Test: `clients/vscode/test/markdown.test.ts`

**Interfaces:**
- Consumes: nothing.
- Produces: `export function renderMarkdown(text: string): string`.
- Supports fenced code, inline code, bold, italic, unordered/ordered lists and http/https links. No raw HTML, tables, images or syntax highlighting.

- [ ] **Step 1: Write the failing test**

```ts
import { test } from "node:test";
import assert from "node:assert/strict";
import { renderMarkdown } from "../src/markdown.ts";

test("raw HTML is escaped, never passed through", () => {
  const html = renderMarkdown('<img src=x onerror="alert(1)"><script>alert(1)</script>');
  assert.ok(!html.includes("<img"));
  assert.ok(!html.includes("<script"));
  assert.ok(html.includes("&lt;img"));
});
test("a fenced block keeps its text and does not close early", () => {
  const html = renderMarkdown("```ts\nconst a = '<b>';\n```");
  assert.ok(html.includes("<pre><code"));
  assert.ok(html.includes("&lt;b&gt;"));
});
test("inline code and bold are rendered", () => { /* <code>, <strong> */ });
test("only http and https links become anchors", () => {
  assert.ok(renderMarkdown("[x](https://e.com)").includes('href="https://e.com"'));
  assert.ok(!renderMarkdown("[x](javascript:alert(1))").includes("href="));
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd clients/vscode && node --test test/markdown.test.ts`
Expected: FAIL — module missing.

- [ ] **Step 3: Implement `renderMarkdown`**

Escape first, then recognize only the supported constructs. Fences are matched on lines; an unmatched fence is text. Links are validated with `new URL` and only `http:`/`https:`; add `rel="noreferrer"`.

- [ ] **Step 4: Run tests**

Run: `cd clients/vscode && node --test test/markdown.test.ts`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add clients/vscode/src/markdown.ts clients/vscode/test/markdown.test.ts
git commit -m "feat(vscode): render a safe Markdown subset in the host"
```

---

### Task 5: Write/edit preview and unified diff

**Files:**
- Create: `clients/vscode/src/diff.ts`
- Test: `clients/vscode/test/diff.test.ts`

**Interfaces:**
- Consumes: nothing.
- Produces:
  ```ts
  export interface DiffResult { unified: string; added: number; removed: number; truncated: boolean; }
  export type Preview = { ok: true; after: string; diff: DiffResult } | { ok: false; reason: string };
  export function unifiedDiff(before: string, after: string, maxLines?: number): DiffResult;
  export function previewWrite(before: string | undefined, content: string, maxLines?: number): Preview;
  export function previewEdit(before: string | undefined, oldString: string, newString: string, replaceAll?: boolean, maxLines?: number): Preview;
  ```
- Default cap is 200 diff lines.

- [ ] **Step 1: Write the failing test**

```ts
test("a new file previews as all additions", () => {
  const preview = previewWrite(undefined, "a\nb");
  assert.equal(preview.ok, true);
  if (!preview.ok) return;
  assert.equal(preview.after, "a\nb");
  assert.equal(preview.diff.added, 2);
});
test("a write previews a replacement", () => {
  const preview = previewWrite("a", "b");
  assert.equal(preview.ok, true);
  if (!preview.ok) return;
  assert.ok(preview.diff.unified.includes("-a"));
  assert.ok(preview.diff.unified.includes("+b"));
});
test("edit replaces one occurrence", () => {
  const preview = previewEdit("x old y", "old", "new");
  assert.equal(preview.ok, true);
  if (preview.ok) assert.equal(preview.after, "x new y");
});
test("edit with replace_all replaces every occurrence", () => {
  const preview = previewEdit("a a", "a", "x", true);
  assert.equal(preview.ok, true);
  if (preview.ok) assert.equal(preview.after, "x x");
});
test("edit with an absent old_string refuses to preview", () => {
  assert.deepEqual(previewEdit("a", "missing", "x"), { ok: false, reason: "old_string was not found" });
});
test("edit with an ambiguous old_string refuses to preview", () => {
  assert.equal(previewEdit("a a", "a", "x").ok, false);
});
test("a large diff is truncated and says so", () => {
  const preview = previewWrite("a\nb\nc", "a\nb\nc\nd", 2);
  assert.equal(preview.ok, true);
  if (preview.ok) assert.equal(preview.diff.truncated, true);
});
```

Use the exact `reason` strings the test asserts; keep them stable.

- [ ] **Step 2: Run test to verify it fails**

Run: `cd clients/vscode && node --test test/diff.test.ts`
Expected: FAIL — module missing.

- [ ] **Step 3: Implement `diff.ts`**

`unifiedDiff` is a line-based LCS diff; emit `-`/`+` lines and `@@` hunks. `previewWrite` uses `before ?? ""`. `previewEdit` counts occurrences; `old === new` is `{ok:false, reason:"nothing to do"}`. Never return `ok:true` when the kernel would refuse.

- [ ] **Step 4: Run tests**

Run: `cd clients/vscode && node --test test/diff.test.ts`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add clients/vscode/src/diff.ts clients/vscode/test/diff.test.ts
git commit -m "feat(vscode): preview writes and edits before approval"
```

---

### Task 6: TabManager

**Files:**
- Create: `clients/vscode/src/tab.ts`
- Test: `clients/vscode/test/tab.test.ts`

**Interfaces:**
- Consumes: `TabState` from `webviewProtocol.ts`.
- Produces:
  ```ts
  export const MAX_TABS = 5;
  export class TabManager {
    get tabs(): readonly TabState[];
    get activeId(): string | undefined;
    active(): TabState | undefined;
    open(tab: TabState): "opened" | "max";
    close(id: string): void;
    activate(id: string): void;
    find(sessionId: string): TabState | undefined;
    update(id: string, patch: Partial<Omit<TabState, "id">>): void;
    snapshot(): { tabs: TabState[]; activeId?: string };
    restore(tabs: TabState[], activeId?: string): void;
  }
  ```

- [ ] **Step 1: Write the failing test**

```ts
const tab = (id: string, sessionId?: string): TabState => ({
  id, sessionId, title: id, status: "idle" });

test("open appends and activates the first tab", () => {
  const m = new TabManager();
  assert.equal(m.open(tab("t1")), "opened");
  assert.equal(m.active()?.id, "t1");
});
test("a sixth tab is refused", () => {
  const m = new TabManager();
  for (let i = 1; i <= 5; i++) assert.equal(m.open(tab(`t${i}`)), "opened");
  assert.equal(m.open(tab("t6")), "max");
  assert.equal(m.tabs.length, 5);
});
test("closing the active tab selects a neighbour", () => {
  const m = new TabManager();
  m.open(tab("t1")); m.open(tab("t2")); m.open(tab("t3"));
  m.close("t2");
  assert.equal(m.active()?.id, "t3");
  m.close("t3");
  assert.equal(m.active()?.id, "t1");
});
test("closing the last tab leaves no active tab", () => {
  const m = new TabManager();
  m.open(tab("t1"));
  m.close("t1");
  assert.equal(m.active(), undefined);
  assert.equal(m.tabs.length, 0);
});
test("find locates an open session and never duplicates it", () => {
  const m = new TabManager();
  m.open(tab("t1", "s1"));
  assert.equal(m.find("s1")?.id, "t1");
  assert.equal(m.find("missing"), undefined);
});
test("snapshot and restore round-trip", () => {
  const m = new TabManager();
  m.open(tab("t1", "s1"));
  const saved = m.snapshot();
  const other = new TabManager();
  other.restore(saved.tabs, saved.activeId);
  assert.deepEqual(other.snapshot(), saved);
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd clients/vscode && node --test test/tab.test.ts`
Expected: FAIL — module missing.

- [ ] **Step 3: Implement `TabManager`**

Keep one array and an active id. `close` chooses the next surviving tab, then the previous. `snapshot` copies the array. `restore` replaces state and clamps to `MAX_TABS`.

- [ ] **Step 4: Run tests**

Run: `cd clients/vscode && node --test test/tab.test.ts`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add clients/vscode/src/tab.ts clients/vscode/test/tab.test.ts
git commit -m "feat(vscode): model the sidebar's tabs as pure state"
```

---

### Task 7: Renderer events and path normalization

**Files:**
- Modify: `clients/vscode/src/render.ts`
- Modify: `clients/vscode/test/render.test.ts`

**Interfaces:**
- Consumes: `UiPart`, `DiffPreview` from `webviewProtocol.ts`; `EventEnvelope`, `JsonValue` from generated schema; `renderMarkdown`.
- Produces:
  - `Renderer.event(envelope): UiPart[]` with tool, approval, review-ready notices.
  - `pathsInArgs(args: JsonValue, cwd: string): string[]`.
  - `Renderer.touchedFiles(): string[]`.
  - `Renderer.models(list)`, `Renderer.context(explain)`, `Renderer.resumed(events, sessionId)` retained, returning `notice`/`assistant` parts.
- `pathsInArgs` resolves `args.path` for `read_file`, `write_file` and `edit` against `cwd`; other absolute-looking strings are kept; URLs and flags are ignored.

- [ ] **Step 1: Write the failing test**

```ts
test("a relative write_file path is resolved against the workspace", () => {
  assert.deepEqual(pathsInArgs({ path: "demo.txt" }, "/w"), ["/w/demo.txt"]);
});
test("a tool call names the tool, class and relative path", () => {
  const parts = renderer.event({ kind: "tool.call", seq: 1, data: {
    call_id: "c1", tool: "write_file", action_class: "write_workspace", args: { path: "demo.txt" } } });
  assert.equal(parts[0].kind, "tool");
  assert.deepEqual(parts[0].paths, ["/w/demo.txt"]);
});
test("an assistant message is markdown-rendered once", () => {
  const [part] = renderer.event({ kind: "message.appended", seq: 1, data: {
    role: "assistant", text: "**hi**" } } as never);
  assert.equal(part.kind, "assistant");
  if (part.kind === "assistant") assert.ok(part.html.includes("<strong>hi</strong>"));
});
test("an approval request carries its request id", () => {
  const [part] = renderer.event({ kind: "approval.requested", seq: 1, data: {
    request_id: "r1", action_class: "write_workspace", reason: "ask" } } as never);
  assert.equal(part.kind, "approval");
  if (part.kind === "approval") {
    assert.equal(part.requestId, "r1");
    assert.equal(part.state, "pending");
  }
});
```

Keep the existing tests for truncation, policy, approval scope, unknown events and display, updating only the expected `UiPart` shapes.

- [ ] **Step 2: Run test to verify it fails**

Run: `cd clients/vscode && node --test test/render.test.ts`
Expected: FAIL — new paths/shapes are not produced.

- [ ] **Step 3: Implement the new `UiPart` mapping**

- `tool.call` → one `tool` part with `status:"running"` and normalized paths.
- `tool.result` → one `tool` part with the same `callId`, `status`, capped `output` and `truncated`.
- `policy.decided`/`permissions.relaxed`/`turn.interrupted` → `notice`.
- `approval.requested` → `approval` with `state:"pending"` (the host adds `diff`).
- `approval.decided` → `notice`.
- `message.appended` assistant → `assistant` with `renderMarkdown`; user → `[]`.
- `resumed` → one `notice` naming the session and event count, plus the newest user/assistant text.

- [ ] **Step 4: Run tests and typecheck**

Run: `cd clients/vscode && node --test test/render.test.ts && npm run typecheck`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add clients/vscode/src/render.ts clients/vscode/test/render.test.ts
git commit -m "feat(vscode): render events as structured parts and resolve relative paths"
```

---

### Task 8: SessionCore and the per-tab SessionAgent

**Files:**
- Create: `clients/vscode/src/sessionCore.ts`
- Create: `clients/vscode/src/sessionAgent.ts` (from `src/workspaceAgent.ts`)
- Delete: `clients/vscode/src/workspaceAgent.ts`
- Test: `clients/vscode/test/sessionCore.test.ts`

**Interfaces:**
- Consumes: `LoongSession`, `TurnGate`, `HostBinary` (from `binary.ts`? use `{path:string}`), `AgentSession`.
- Produces:
  ```ts
  export class SessionCore {
    get session(): LoongSession | undefined;
    get turnGate(): TurnGate | undefined;
    adopt(session: LoongSession): void;
    forget(): void;
    alive(): boolean;
  }
  export class SessionAgent {
    constructor(options: { folder: vscode.WorkspaceFolder; output: vscode.OutputChannel;
      context: vscode.ExtensionContext; binaryPath: string; sessionId?: string;
      fresh?: boolean; });
    ensure(): Promise<LoongSession>;
    beginTurn(sink: TurnSink): Promise<TurnController>;
    endTurn(): void;
    startNew(): Promise<string>;
    takeResume(): { sessionId: string; events: EventEnvelope[] } | undefined;
    takeLastExit(): { code: number | null; signal: string | null } | undefined;
    get sessionId(): string | undefined;
    dispose(): void;
  }
  ```
- `SessionCore.adopt` creates a new `TurnGate`; `forget` clears both.

- [ ] **Step 1: Write the failing test**

```ts
import { test } from "node:test";
import assert from "node:assert/strict";
import { SessionCore } from "../src/sessionCore.ts";

const fakeSession = () => ({ onEvent: () => {}, onDelta: () => {}, submit: async () => ({ turn_id: 1, accepted: true }), respondApproval: async () => true, cancel: () => {} }) as never;

test("adopting a session creates a gate", () => {
  const core = new SessionCore();
  core.adopt(fakeSession());
  assert.ok(core.turnGate);
  assert.ok(core.alive());
});
test("forgetting the dead child clears the gate", () => {
  const core = new SessionCore();
  core.adopt(fakeSession());
  core.forget();
  assert.equal(core.session, undefined);
  assert.equal(core.turnGate, undefined);
  assert.equal(core.alive(), false);
});
test("a new session gets a new gate, not the dead one", () => {
  const core = new SessionCore();
  core.adopt(fakeSession());
  const dead = core.turnGate;
  core.forget();
  core.adopt(fakeSession());
  assert.notEqual(core.turnGate, dead);
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd clients/vscode && node --test test/sessionCore.test.ts`
Expected: FAIL — module missing.

- [ ] **Step 3: Implement `SessionCore` and refactor `SessionAgent`**

`SessionAgent` keeps the old `WorkspaceAgent` behavior (stored id, resume-once, auto-approve, output lines) but:
- takes `binaryPath` and spawns that exact path;
- accepts `sessionId` (attach exactly that session; throw if it cannot, so the host drops the tab), `fresh` (spawn `--new` and do not resume), or neither (the old stored-id-with-one-fallback path, used by M1);
- holds a `SessionCore`; `onExit` calls `core.forget()`;
- `beginTurn` uses `core.turnGate` (created by `adopt`), never a stale gate;
- `startNew` disposes the current child, spawns `--new`, attaches and adopts.
- Delete `workspaceAgent.ts`; update imports.

- [ ] **Step 4: Run tests and typecheck**

Run: `cd clients/vscode && node --test test/sessionCore.test.ts && npm run typecheck`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add clients/vscode/src/sessionCore.ts clients/vscode/src/sessionAgent.ts clients/vscode/test/sessionCore.test.ts
git rm clients/vscode/src/workspaceAgent.ts
git commit -m "fix(vscode): give a respawned child a fresh turn gate"
```

---

### Task 9: Prompt with file and selection attachments

**Files:**
- Modify: `clients/vscode/src/prompt.ts`
- Modify: `clients/vscode/test/prompt.test.ts`

**Interfaces:**
- Consumes: `Attachment` from `webviewProtocol.ts`.
- Produces: `export function promptText(prompt: string, attachments: readonly Attachment[]): string`.
- Files append `referenced files:\n- <absolute path>`; selections append a fenced block with path and `lines N-M`, and a fence longer than any run in the text.

- [ ] **Step 1: Write the failing test**

```ts
test("a file attachment adds its absolute path", () => {
  assert.ok(promptText("look", [{ id: "a", kind: "file", path: "/w/a.ts" }]).includes("- /w/a.ts"));
});
test("a selection adds path, line range and text", () => {
  const text = promptText("explain", [{ id: "s", kind: "selection", path: "/w/a.ts",
    startLine: 10, endLine: 12, text: "const a = 1;" }]);
  assert.ok(text.includes("/w/a.ts"));
  assert.ok(text.includes("lines 10-12"));
  assert.ok(text.includes("const a = 1;"));
});
test("two file attachments are not repeated", () => {
  const text = promptText("look", [
    { id: "a", kind: "file", path: "/w/a.ts" },
    { id: "b", kind: "file", path: "/w/a.ts" },
  ]);
  assert.equal(text.split("- /w/a.ts").length - 1, 1);
});
test("no attachments leaves the prompt unchanged", () => {
  assert.equal(promptText("look", []), "look");
});
test("a selection containing backticks uses a longer fence", () => {
  const text = promptText("explain", [{ id: "s", kind: "selection", path: "/w/a.ts",
    startLine: 1, endLine: 1, text: "```" }]);
  assert.ok(text.includes("````"));
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd clients/vscode && node --test test/prompt.test.ts`
Expected: FAIL — signature/behavior mismatch.

- [ ] **Step 3: Implement `promptText`**

Keep the old pure module free of `vscode`. No file contents are read.

- [ ] **Step 4: Run tests**

Run: `cd clients/vscode && node --test test/prompt.test.ts`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add clients/vscode/src/prompt.ts clients/vscode/test/prompt.test.ts
git commit -m "feat(vscode): carry files and selections into the prompt"
```

---

### Task 10: ApprovalBook

**Files:**
- Create: `clients/vscode/src/approval.ts`
- Test: `clients/vscode/test/approval.test.ts`

**Interfaces:**
- Consumes: `ApprovalAnswer` from `turn.ts`.
- Produces:
  ```ts
  export class ApprovalBook {
    wait(tabId: string, requestId: string): Promise<ApprovalAnswer>;
    answer(tabId: string, requestId: string, answer: ApprovalAnswer): boolean;
    denyTab(tabId: string, reason: string): void;
    pending(tabId: string): number;
  }
  ```

- [ ] **Step 1: Write the failing test**

```ts
test("an answer resolves the waiting decision", async () => {
  const book = new ApprovalBook();
  const pending = book.wait("t1", "r1");
  assert.equal(book.answer("t1", "r1", { granted: true, scope: "once" }), true);
  assert.deepEqual(await pending, { granted: true, scope: "once" });
});
test("an unknown request is ignored", () => {
  const book = new ApprovalBook();
  assert.equal(book.answer("t1", "missing", { granted: true, scope: "once" }), false);
});
test("a second answer for one request is ignored", async () => {
  const book = new ApprovalBook();
  const pending = book.wait("t1", "r1");
  book.answer("t1", "r1", { granted: false, scope: "once", reason: "no" });
  assert.equal(book.answer("t1", "r1", { granted: true, scope: "once" }), false);
  assert.deepEqual(await pending, { granted: false, scope: "once", reason: "no" });
});
test("closing a tab denies every pending decision with the reason", async () => {
  const p = book.wait("t1", "r1");
  book.denyTab("t1", "tab closed");
  assert.deepEqual(await p, { granted: false, scope: "once", reason: "tab closed" });
});
test("denying one tab does not touch another", () => {
  const book = new ApprovalBook();
  book.wait("t1", "r1"); book.wait("t2", "r2");
  book.denyTab("t1", "tab closed");
  assert.equal(book.pending("t1"), 0);
  assert.equal(book.pending("t2"), 1);
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd clients/vscode && node --test test/approval.test.ts`
Expected: FAIL — module missing.

- [ ] **Step 3: Implement `ApprovalBook`**

Key entries by `${tabId}\u0000${requestId}`; remove on answer. `denyTab` resolves all entries for that tab and removes them. Never auto-allow.

- [ ] **Step 4: Run tests**

Run: `cd clients/vscode && node --test test/approval.test.ts`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add clients/vscode/src/approval.ts clients/vscode/test/approval.test.ts
git commit -m "feat(vscode): never leave an approval pending across a closed tab"
```

---

### Task 11: Manifest, assets, build and an empty sidebar view

**Files:**
- Modify: `clients/vscode/package.json`
- Modify: `clients/vscode/esbuild.mjs`
- Modify: `clients/vscode/src/extension.ts`
- Create: `clients/vscode/src/chatView.ts` (shell)
- Create: `clients/vscode/src/binaryHost.ts`
- Create: `clients/vscode/webview/main.ts` (empty state)
- Create: `clients/vscode/webview/style.css`
- Create: `clients/vscode/media/loong-24.png`, `loong-48.png`, `loong-128.png`
- Test: `clients/vscode/test/manifest.test.ts`

**Interfaces:**
- Consumes: `BinaryResolver` (Task 1), `TabManager` (Task 6), `webviewProtocol` (Task 2).
- Produces:
  ```ts
  export interface HostBinary { path: string; version: string; schema: string; }
  export async function resolveHostBinary(context: vscode.ExtensionContext,
    output: vscode.OutputChannel): Promise<HostBinary | undefined>;
  export async function pickBinary(): Promise<string | undefined>;
  export class ChatViewProvider implements vscode.WebviewViewProvider {
    static readonly viewType = "loongagent.chat";
    constructor(options: { context: vscode.ExtensionContext; output: vscode.OutputChannel;
      binary?: HostBinary });
    resolveWebviewView(view: vscode.WebviewView): void;
    setBinary(binary: HostBinary): void;
    post(message: HostMessage): void;
  }
  ```

Manifest:

```json
"viewsContainers": { "activitybar": [
  { "id": "loongagent", "title": "loongagent", "icon": "media/loong-48.png" } ] },
"views": { "loongagent": [
  { "id": "loongagent.chat", "name": "Chat", "type": "webview" } ] }
```

Remove `chatParticipants`, `languageModelChatProviders` and `activationEvents`; keep the existing configuration; set `"scope": "machine"` on `loongagent.path`, `loongagent.autoApprove` and `loongagent.extraArgs`. Add commands `loongagent.openChat`, `loongagent.locateBinary`, `loongagent.attachSelection`, `loongagent.reviewChanges`, and a menu `editor/context` entry for `loongagent.attachSelection` with `when: "editorHasSelection"`.

- [ ] **Step 1: Write the failing manifest test**

```ts
test("the sidebar is contributed", () => {
  const [c] = pkg.contributes.viewsContainers.activitybar;
  assert.equal(c.id, "loongagent");
  assert.equal(pkg.contributes.views.loongagent[0].id, "loongagent.chat");
  assert.equal(pkg.contributes.views.loongagent[0].type, "webview");
});
test("the native chat surface and placeholder model are gone", () => {
  assert.equal(pkg.contributes.chatParticipants, undefined);
  assert.equal(pkg.contributes.languageModelChatProviders, undefined);
});
test("the binary path is machine-scoped", () => {
  assert.equal(pkg.contributes.configuration.properties["loongagent.path"].scope, "machine");
});
test("the sidebar commands and selection menu are contributed", () => {
  const ids = pkg.contributes.commands.map((c: { command: string }) => c.command);
  for (const id of ["loongagent.openChat", "loongagent.locateBinary",
    "loongagent.attachSelection", "loongagent.reviewChanges"]) assert.ok(ids.includes(id));
  assert.equal(pkg.contributes.menus["editor/context"][0].command, "loongagent.attachSelection");
});
test("the derived icons exist", () => {
  for (const p of ["loong-24.png", "loong-48.png", "loong-128.png"]) {
    assert.ok(existsSync(new URL(`../media/${p}`, import.meta.url)));
  }
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd clients/vscode && node --test test/manifest.test.ts`
Expected: FAIL — manifest still contributes `chatParticipants`, no views.

- [ ] **Step 3: Implement the manifest, assets, build and shell**

- Derive the icons with ffmpeg (available on the owner's machine):
  ```bash
  mkdir -p clients/vscode/media
  for size in 24 48 128; do
    ffmpeg -y -v error -i favicon.png -vf \
      "scale=${size}:${size}:force_original_aspect_ratio=decrease,pad=${size}:${size}:(ow-iw)/2:(oh-ih)/2:color=black@0,format=rgba" \
      -frames:v 1 clients/vscode/media/loong-${size}.png
  done
  ```
  The source is the owner's untracked `favicon.png` at the repository root; if you
  are in a fresh worktree without it, copy it from the owner's checkout. Do not commit
  `favicon.png`.
- `esbuild.mjs`: keep the extension build; add `webview/main.ts` → `dist/webview.js` (`format:"iife"`, `platform:"browser"`, no `vscode` external). Copy nothing else; `webview/style.css` is loaded directly.
- `binaryHost.ts`: read `loongagent.path`, the `globalState` key `loongagent.binaryPath`, `process.env.PATH`, `os.homedir()`; call `resolveBinary`; run `--version` with `execFile` and a 5 s timeout; compare `SCHEMA_VERSION`; return `undefined` with an actionable output line when absent.
- `chatView.ts` shell: `resolveWebviewView` sets `webview.options = { enableScripts: true, localResourceRoots: [extensionUri, dist, webview, media] }`, builds HTML with a CSP, loads `webview.js`, `style.css` and `media/loong-128.png`, registers `onDidReceiveMessage` (ignore until Task 12), and posts a bootstrap with one empty tab. `setBinary` replaces the resolved binary and posts the new status so a Locate click takes effect on the next spawn.
- `extension.ts`: remove the participant, model provider, smoke command and old chat functions; resolve the binary once (a missing binary leaves `binary` undefined and shows the Locate error, never a crash); create `ChatViewProvider`; register it; register `loongagent.openChat` (`workbench.view.extension.loongagent`), `loongagent.locateBinary` (file dialog → `globalState` → message that a new tab uses it), `loongagent.attachSelection` (Task 14 fills it), and `loongagent.reviewChanges` (Task 13 fills it). Dispose the provider and child.
- `webview/main.ts` shell: acquire the API, listen for messages, render the empty state and binary path, send `{type:"ready"}`.
- Delete `src/modelProvider.ts` if present.

- [ ] **Step 4: Run tests and compile**

Run: `cd clients/vscode && npm test && npm run compile`
Expected: manifest tests pass; both bundles build; `tsc` clean.

Manual:
```bash
mkdir -p /tmp/loong-vscode-scratch
code --extensionDevelopmentPath="$PWD" --new-window /tmp/loong-vscode-scratch
```
Expected: the Activity Bar shows the loongagent icon; the view shows the empty state and the resolved binary/schema; Output → loongagent has `loongagent: activated` and the binary path.

- [ ] **Step 5: Commit**

```bash
git add clients/vscode/package.json clients/vscode/esbuild.mjs clients/vscode/src/extension.ts \
  clients/vscode/src/chatView.ts clients/vscode/src/binaryHost.ts \
  clients/vscode/webview/main.ts clients/vscode/webview/style.css \
  clients/vscode/media/loong-24.png clients/vscode/media/loong-48.png \
  clients/vscode/media/loong-128.png clients/vscode/test/manifest.test.ts
git commit -m "feat(vscode): open an independent loongagent sidebar"
```

---

### Task 12: Chat, stream and Stop in the sidebar

**Files:**
- Modify: `clients/vscode/src/chatView.ts`
- Modify: `clients/vscode/webview/main.ts`
- Modify: `clients/vscode/webview/style.css`

**Interfaces:**
- Consumes: `SessionAgent` (Task 8), `Transcript` (Task 3), `Renderer` (Task 7), `TabManager` (Task 6), `ApprovalBook` (Task 10).
- Produces: the `send`, `cancel`, `ready` and `bootstrap` paths in `ChatViewProvider`; `post(message: HostMessage)`.

- [ ] **Step 1: Implement the host turn path**

- On `ready`: `post({type:"bootstrap", tabs, activeTabId, activeTranscript, models, sessionId, binary})`; call `agent.ensure()` lazily on the first send, not on ready.
- On `send`: validate the active tab; if no binary is set, post an error card with the Locate action and stop; add a `user` part to the transcript; if the tab title is still `New session`, set it to the prompt with whitespace collapsed and truncated to 60 characters, and post the updated tabs; `agent.beginTurn` with a sink:
  - `onDelta` → `post({type:"delta", tabId, text})`; mark tab `running`.
  - `onEvent` → `renderer.event`; append to `Transcript`; post `part`; for `approval.requested`, use a temporary Deny decision with reason `approvals arrive in the next task` (Task 13 replaces this).
  - on completion → post `assistantFinal` from the latest assistant event; status `idle`; add the review card if files changed (Task 13 computes it; for now omit).
  - on error → `notice`/`error`; status `stopped`.
- On `cancel` → `agent`/`session.cancel()`; status `stopped`.
- Busy: a second `send` while running posts an error and does not touch the running sink.

- [ ] **Step 2: Implement the Webview transcript and composer**

- `main.ts`: maintain `parts: UiPart[]`; `render()` replaces the transcript DOM from parts; `assistant` uses `html`; `tool`/`approval`/`review`/`notice` render cards; tool output uses `textContent`.
- `delta` appends to a live bubble with `textContent`; `assistantFinal` replaces the live bubble and adds the part.
- Composer: Enter sends, Shift+Enter newline; Stop visible while running; `aria-label`s.
- CSS: VS Code variables; no fixed colors; scrollable transcript.

- [ ] **Step 3: Build and verify manually**

Run: `cd clients/vscode && npm run compile`
In the dev host: send `hello` with a working binary/model (e.g. `--model echo` via `loongagent.extraArgs` or a configured model). Expected: user message, streaming answer, Stop works, a second send after completion works; a second send while running is refused.

- [ ] **Step 4: Run tests and typecheck**

Run: `cd clients/vscode && npm test && npm run typecheck`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add clients/vscode/src/chatView.ts clients/vscode/webview/main.ts clients/vscode/webview/style.css
git commit -m "feat(vscode): stream a real turn into the sidebar"
```

---

### Task 13: Approval cards, diff preview and Review

**Files:**
- Modify: `clients/vscode/src/chatView.ts`
- Modify: `clients/vscode/src/extension.ts`
- Modify: `clients/vscode/webview/main.ts`
- Modify: `clients/vscode/webview/style.css`
- Modify: `clients/vscode/src/snapshots.ts` (absolute-path normalization)
- Create: `clients/vscode/src/previewProvider.ts`
- Test: `clients/vscode/test/previewProvider.test.ts`
- Test: `clients/vscode/test/snapshots.test.ts` (add a relative-path case)

**Interfaces:**
- Consumes: `previewWrite`/`previewEdit` (Task 5), `ApprovalBook` (Task 10), `FileSnapshots`, `DiffProvider`.
- Produces:
  ```ts
  export class PreviewProvider<U extends UriLike> {
    constructor(uris: DiffUris<U>);
    uriFor(requestId: string, path: string, after: string): U;
    provideTextDocumentContent(uri: U): string;
  }
  ```

- [ ] **Step 1: Write the failing tests**

`previewProvider.test.ts`: a preview URI round-trips its request id and path; the provider serves the stored after text; an unknown URI is empty. `snapshots.test.ts`: `see(["demo.txt"])` with a workspace `/w` and a file `/w/demo.txt` reads the workspace file (relative paths are resolved against the workspace, not `process.cwd()`).

- [ ] **Step 2: Run them to verify they fail**

Run: `cd clients/vscode && node --test test/previewProvider.test.ts test/snapshots.test.ts`
Expected: FAIL — module missing/relative path not resolved.

- [ ] **Step 3: Implement the approval and review path**

- `ChatViewProvider`:
  - On `tool.call` with `write_file`/`edit`, resolve `args.path` against the workspace, `snapshots.see([absolute])`; for `edit`, call `previewEdit` with the current file; for `write_file`, `previewWrite`.
  - On `approval.requested`, create an approval part with the diff (or `{ok:false}` reason) and `book.wait(tabId, requestId)`; post it.
  - On `approval` message: `book.answer`; call `session.respondApproval` with the mapped scope; update the approval part; post.
  - On `closeTab`/provider dispose: `book.denyTab(tabId, "tab closed")`.
  - On turn end: normalize `renderer.touchedFiles()`, `changedAmong`, add a `review` part with the turn token and paths.
  - On `review`/`openDiff`: open `vscode.diff` with the snapshot left and `PreviewProvider` right (approval), or the snapshot/current (review).
- `snapshots.ts`: `resolveWorkspacePath(workspace, path)` before read; keep keys absolute; reject outside.
- `previewProvider.ts`: map `${requestId}\u0000${path}` → after text; `uriFor` encodes both in the URI query; `provideTextDocumentContent` returns the map value or `"[loongagent] preview unavailable"`.
- Webview: approval card shows `-N +M`, a folded `<pre>` diff, `Open full diff`, three buttons; review card has a button; buttons disabled after answer.
- `extension.ts`: `loongagent.reviewChanges` forwards to the provider; `loongagent.smoke` remains removed.

- [ ] **Step 4: Run tests and manual acceptance**

Run: `cd clients/vscode && npm test && npm run compile`
Manual with `[permissions] write_workspace = "ask"`:
- Ask for a file write; the card shows a diff; Deny leaves the file unchanged; Allow writes it.
- Ask for two file changes; Review opens both diffs with pre-turn left sides.
- With `allow`, the card does not appear; the tool result says "written unreviewed".

- [ ] **Step 5: Commit**

```bash
git add clients/vscode/src/chatView.ts clients/vscode/src/extension.ts \
  clients/vscode/src/previewProvider.ts clients/vscode/src/snapshots.ts \
  clients/vscode/webview/main.ts clients/vscode/webview/style.css \
  clients/vscode/test/previewProvider.test.ts clients/vscode/test/snapshots.test.ts
git commit -m "feat(vscode): review a write before it lands"
```

---

### Task 14: Model, context, attachments, resume and error paths

**Files:**
- Modify: `clients/vscode/src/chatView.ts`
- Modify: `clients/vscode/src/extension.ts`
- Modify: `clients/vscode/webview/main.ts`
- Modify: `clients/vscode/webview/style.css`
- Modify: `clients/vscode/src/sessionAgent.ts`
- Test: `clients/vscode/test/prompt.test.ts` (already covers the prompt shape)

**Interfaces:**
- Consumes: `LoongSession.models/selectModel/context/setAutoApprove`, `promptText`, `FileSnapshots`.
- Produces: the remaining `WebviewMessage` handlers: `selectModel`, `context`, `pickFile`, `pickSelection`, `removeAttachment`, `newTab` (reset the single tab to a new session in M1), `yolo`; the `attachSelection` command body; the resume card on first open.

- [ ] **Step 1: Implement model and context**

- `ready` posts `models()`; `selectModel` calls `selectModel(name)` and posts the new list/current; running disables the dropdown in the Webview.
- `/context` calls `context()` and posts a `notice` with the quoted numbers; no percentage is computed.

- [ ] **Step 2: Implement attachments**

- `pickFile`: `vscode.window.showOpenDialog` over the workspace; append `{id, kind:"file", path}`; post `attachments`.
- `pickSelection`: read `activeTextEditor`; append `{id, kind:"selection", path, startLine, endLine, text}` (1-based lines); post.
- `attachSelection` command: reveal the view, wait for `ready`, call `pickSelection`.
- `removeAttachment`: remove by id; post.
- On send: `promptText(text, attachments)`, then clear the attachments.
- No file contents are read except the selected range.

- [ ] **Step 3: Implement resume, `/new`, `/yolo` and errors**

- On first `ready` after a stored session, `takeResume()` posts a `notice` with the session id, event count and last messages.
- `/new` (and `newTab` in M1) calls `agent.startNew()`, resets the transcript and posts a fresh state.
- `/yolo` asks `vscode.window.showWarningMessage(..., {modal:true}, "Turn prompts off", "Cancel")`; on confirm, `setAutoApprove(!current)`, post a notice; the `permissions.relaxed` event is rendered.
- Binary missing: `bootstrap`/error card names `loongagent.path` and offers Locate; Locate stores the path, calls `provider.setBinary`, and the next send spawns with it. Child exit: error card + next send respawns; stalled request: 120 s error.
- Stop: `session.cancel()`.

- [ ] **Step 4: Run tests and manual acceptance**

Run: `cd clients/vscode && npm test && npm run typecheck`
Manual: model dropdown, `/context`, `@` file, selection from the editor, remove attachment, reload, `/new`, `/yolo`, missing binary (set a bad path), kill the child then send again.

- [ ] **Step 5: Commit**

```bash
git add clients/vscode/src/chatView.ts clients/vscode/src/extension.ts \
  clients/vscode/src/sessionAgent.ts clients/vscode/webview/main.ts \
  clients/vscode/webview/style.css
git commit -m "feat(vscode): model, context, attachments and error paths in the sidebar"
```

---

### Task 15: Documentation, gate and CI Node pin

**Files:**
- Create: `clients/vscode/README.md`
- Modify: `DEVELOPMENT.md`
- Modify: `README.md`
- Modify: `docs/design/surfaces.md`
- Modify: `docs/05-parity-and-synthesis.md`
- Modify: `scripts/check.sh`
- Modify: `.github/workflows/ci.yml`
- Modify: `.gitea/workflows/ci.yml`

**Interfaces:** none; this task records how to run the surface and makes the gate build it.

- [ ] **Step 1: Write the README and the design-doc updates**

`clients/vscode/README.md`: install/build the binary, `loongagent.path` (machine scope), Locate, `write_workspace = "ask"`, the sidebar features and known limits (no shell diff, no per-hunk, one window per session, Windows no Stop). Update `docs/design/surfaces.md` with a VS Code Webview section (code paths, `Verify`), `docs/05-parity-and-synthesis.md` with the shipped status, and the root run instructions.

- [ ] **Step 2: Make the gate build the extension**

In `scripts/check.sh`, change the extension step to:
```bash
( cd clients/vscode && npm ci --silent && npm run compile && npm test )
```
Add `actions/setup-node@v4` with `node-version: 24` to both CI workflows before the gates step.

- [ ] **Step 3: Run the gates**

Run: `cd clients/vscode && npm run compile && npm test`
Run: `python3 scripts/check-docs.py`
Run: `FAST=1 ./scripts/check.sh` (or the full script when the Rust work is unaffected)
Expected: all green.

- [ ] **Step 4: Manual acceptance and commit**

Run the M1 checklist from spec §14 items 1–5 and 7–8. Record the observed result in the commit message or the final report.

```bash
git add clients/vscode/README.md DEVELOPMENT.md README.md docs/design/surfaces.md \
  docs/05-parity-and-synthesis.md scripts/check.sh .github/workflows/ci.yml \
  .gitea/workflows/ci.yml
git commit -m "docs(vscode): document the sidebar and build it in the gate"
```

---

## M1 Done When

- `npm run compile` produces `dist/extension.js` and `dist/webview.js`.
- `npm test` passes; `tsc` is clean.
- The Activity Bar view opens without Copilot sign-in, shows the binary/schema, streams a prompt, stops a turn, attaches a selection/file, switches models, shows a write diff, denies/allows a write, reviews changed files, reloads, and respawns after a child exit.
- `scripts/check.sh` includes the extension compile and test.
- The old `@loong` participant and placeholder model are gone.
