# VS Code sidebar surface — M2: tabs, history and `session.list` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use subagent-driven-development (recommended) or executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the sidebar keep several conversations open at once: one tab per session, a history list backed by a new read-only `session.list` method, lazy restoration after reload, and per-tab approvals and transcripts.

**Architecture:** The extension host owns a `TabManager`, one `SessionAgent` (one child, one `LoongSession`, one `TurnGate`) per tab, one `Transcript` per tab, and one `ApprovalBook` for pending decisions. The Webview renders the active tab and keeps a per-tab cache; the host is the source of truth. `session.list` is a server capability injected by the CLI, so the surface never reads the session directory.

**Tech Stack:** M1's TypeScript Webview + host; Rust schema/server/CLI for `session.list`; `node --test`, `cargo test`.

**Spec:** [2026-10-26-vscode-webview-design.md](../specs/2026-10-26-vscode-webview-design.md). **Depends on:** [M1](2026-10-26-vscode-webview-m1.md). Complete M1 first; this plan assumes its `TabManager`, `SessionAgent`, `Transcript`, `ApprovalBook` and Webview shell exist.

## Global Constraints

- **M1 is green and committed.** Do not start M2 with a broken M1; the single-tab surface must work first.
- **One tab = one session = one child.** Never open the same session in two tabs of one window; focus the existing tab.
- **`MAX_TABS = 5`; history limit 50, clamped 1..200.**
- **`session.list` is read-only.** No event, no kernel behaviour change, no session-directory parsing in the extension.
- **The server crate must not depend on the CLI.** The lister is injected by `crates/cli/src/server.rs`.
- **Stable VS Code APIs only.** No proposed APIs.
- **Pure modules stay `vscode`-free:** `tab.ts`, `tabStore.ts`, `transcript.ts`, `approval.ts`, `webviewProtocol.ts`, everything under `webview/`.
- **Commit named paths only;** never `git add -A`. Do not edit `docs/human-written/`.
- **Test commands:** `cd clients/vscode && npm test`, `npm run typecheck`; `python3 scripts/gen-schema.py --check`; `cargo test -p loongagent -p loong-server -p loong-schema`.

## Review Focus

Five failure modes this milestone owns:

1. **The same session is opened twice** (history click, restored tab, double click): focus the existing tab, never spawn a second writer. → Task 4, with the `TabManager.find` test from M1 and a host-level dedupe check.
2. **A sixth tab is requested**: the view says "at most 5 tabs" and no child is spawned. → Task 4 (uses M1 `TabManager.open` returning `"max"`).
3. **A background tab has a pending approval and is closed**: the decision resolves to Deny with reason `tab closed`; it never hangs or allows. → Task 4 (uses M1 `ApprovalBook.denyTab`).
4. **Reload restores a tab whose session no longer exists**: the tab is dropped with a visible message, the rest load, and no retry loop starts. → Task 3 (`tabStore` decode) and Task 4 (restore).
5. **`session.list` is called on a server without a lister**: it returns an empty list, not an error or invented data. → Task 1.

---

### Task 1: `session.list` in the schema and the server

**Files:**
- Modify: `crates/schema/protocol.toml`
- Regenerate: `crates/schema/src/generated.rs`, `crates/schema/tests/generated_roundtrip.rs`, `clients/ts/protocol.generated.ts`, `clients/ts/protocol.samples.json`
- Modify: `crates/server/src/lib.rs`
- Test: `crates/server/src/lib.rs` (the inline `#[cfg(test)]` module)

**Interfaces:**
- Consumes: `loong_schema::generated` types.
- Produces:
  ```rust
  // generated
  pub struct SessionListParams { pub limit: Option<u32> }
  pub struct SessionInfo { pub id: String, pub title: String,
    pub updated_at: u64, pub events: u64, pub turns: u64 }
  pub struct SessionListResult { pub sessions: Vec<SessionInfo> }

  // server
  type SessionLister = Box<dyn Fn(u32) -> Vec<gen::SessionInfo>>;
  impl Server {
    pub fn with_session_lister(self, lister: SessionLister) -> Self;
  }
  ```
- The method is `session.list`; it records no event.

- [ ] **Step 1: Write the failing server test**

Add a test in `crates/server/src/lib.rs`:

```rust
#[test]
fn session_list_answers_from_the_injected_lister() {
    let mut server = Server::new(kernel(), "s1").with_session_lister(Box::new(|limit| {
        assert_eq!(limit, 5);
        vec![gen::SessionInfo { id: "a".into(), title: "first".into(),
            updated_at: 1, events: 2, turns: 1 }]
    }));
    let out = lines_of(&mut server, r#"{"jsonrpc":"2.0","id":1,"method":"session.list","params":{"limit":5}}"#);
    let response: Value = serde_json::from_str(out.last().unwrap()).unwrap();
    assert_eq!(response["result"]["sessions"][0]["id"], "a");
}
#[test]
fn session_list_without_a_lister_is_empty() {
    let mut server = Server::new(kernel(), "s1");
    let out = lines_of(&mut server,
        r#"{"jsonrpc":"2.0","id":1,"method":"session.list","params":{}}"#);
    let response: Value = serde_json::from_str(out.last().unwrap()).unwrap();
    assert_eq!(response["result"]["sessions"], serde_json::json!([]));
}
```

Use the existing `kernel()` helper in the server tests.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p loong-server session_list`
Expected: FAIL — unknown method / no `with_session_lister`.

- [ ] **Step 3: Add the schema types and method, then regenerate**

Add `SessionListParams` (`limit` optional), `SessionInfo` (five fields), `SessionListResult` (`sessions`), and the `session.list` method to `crates/schema/protocol.toml`, following the shape of `session.attach` and `model.list`. Run `python3 scripts/gen-schema.py`; then `python3 scripts/gen-schema.py --check`.

- [ ] **Step 4: Add the server lister and dispatch**

Add `session_lister: Option<SessionLister>` to `Server`, initialize `None` in `new`, add the builder, and add a `"session.list"` arm in `dispatch`: parse `SessionListParams`, clamp `limit` to 1..200 (default 50), call the lister or use `Vec::new()`, and respond with `SessionListResult`. Do not append an event.

- [ ] **Step 5: Run the Rust tests**

Run: `cargo test -p loong-server session_list && python3 scripts/gen-schema.py --check`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/schema/protocol.toml crates/schema/src/generated.rs \
  crates/schema/tests/generated_roundtrip.rs clients/ts/protocol.generated.ts \
  clients/ts/protocol.samples.json crates/server/src/lib.rs
git commit -m "feat(server): list a project's sessions without recording an event"
```

---

### Task 2: The CLI provides the real lister

**Files:**
- Modify: `crates/cli/src/server.rs`
- Modify: `crates/cli/src/session.rs`
- Test: `crates/cli/src/session.rs` (or the existing session test module)

**Interfaces:**
- Consumes: `SessionSummary` from `crates/cli/src/session.rs`; `gen::SessionInfo`.
- Produces:
  ```rust
  pub(crate) fn session_infos(cwd: &Path, limit: u32) -> Vec<loong_schema::generated::SessionInfo>;
  ```
- `serve` injects `Box::new(move |limit| session_infos(&cwd, limit))` into the server.

- [ ] **Step 1: Write the failing mapping test**

Use the existing temp-session helpers to write two logs with different first prompts; assert:
- `session_infos(&cwd, 10)` returns both, newest first (the existing summary order).
- `title` is the first prompt with whitespace collapsed and truncated to 80 characters; a log with no user message is `"New session"`.
- `updated_at` is a Unix second, `events`/`turns` are the summary counts.
- `session_infos(&cwd, 1)` returns one.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p loongagent session_infos`
Expected: FAIL — function missing.

- [ ] **Step 3: Implement the mapping and inject it**

Map `SessionSummary` to `gen::SessionInfo`. `modified.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)`. Title: `first_prompt`, split/join whitespace, take 80 chars; else `"New session"`. In `serve`, clone `args.cwd` into the closure and call `.with_session_lister(...)` after building the `Server`.

- [ ] **Step 4: Run the Rust tests**

Run: `cargo test -p loongagent -p loong-server`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cli/src/server.rs crates/cli/src/session.rs
git commit -m "feat(cli): answer session.list from the project's session summaries"
```

---

### Task 3: `listSessions` in the TypeScript session and persisted tab state

**Files:**
- Modify: `clients/vscode/src/session.ts`
- Create: `clients/vscode/src/tabStore.ts`
- Test: `clients/vscode/test/session.test.ts`
- Test: `clients/vscode/test/tabStore.test.ts`

**Interfaces:**
- Consumes: `METHODS.sessionList`; `SessionInfo` from `webviewProtocol.ts`; `TabState`.
- Produces:
  ```ts
  // session.ts
  async listSessions(limit = 50): Promise<SessionInfo[]>;

  // tabStore.ts
  export interface StoredTab { id: string; sessionId?: string; title: string; }
  export function encodeTabs(tabs: readonly TabState[]): StoredTab[];
  export function decodeTabs(raw: unknown): StoredTab[];
  ```
- `listSessions` sends `{limit}` only; it does not add `session_id` (the method is project-scoped).
- `decodeTabs` rejects non-arrays, entries without a string `id`, and duplicate ids; it preserves order and drops unknown fields.

- [ ] **Step 1: Write the failing tests**

```ts
test("listSessions sends only a limit and returns the list", async () => {
  const { child, session } = makeSession();
  await attachTo(child, session, "s1");
  const pending = session.listSessions(7);
  const request = child.sent()[1];
  assert.equal(request.method, "session.list");
  assert.deepEqual(request.params, { limit: 7 });
  answer(child, 1, { sessions: [{ id: "a", title: "A", updated_at: 1, events: 2, turns: 1 }] });
  assert.deepEqual(await pending, [{ id: "a", title: "A", updated_at: 1, events: 2, turns: 1 }]);
});
test("encode/decode round-trips open tabs", () => {
  const tabs: TabState[] = [{ id: "t1", sessionId: "s1", title: "A", status: "idle" }];
  assert.deepEqual(decodeTabs(encodeTabs(tabs)), [{ id: "t1", sessionId: "s1", title: "A" }]);
});
test("decode drops malformed and duplicate entries", () => {
  assert.deepEqual(decodeTabs([{ id: "t1", title: "A" }, { title: "no id" }, { id: "t1", title: "B" }]),
    [{ id: "t1", title: "A" }]);
  assert.deepEqual(decodeTabs("nope"), []);
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd clients/vscode && node --test test/session.test.ts test/tabStore.test.ts`
Expected: FAIL — method/module missing.

- [ ] **Step 3: Implement `listSessions` and `tabStore`**

Use `this.agent.request<{sessions: SessionInfo[]}>(METHODS.sessionList, {limit})`; map a session refusal like the other calls only if the server names a session. `encodeTabs` copies `{id, sessionId, title}`; `decodeTabs` validates.

- [ ] **Step 4: Run the tests and typecheck**

Run: `cd clients/vscode && node --test test/session.test.ts test/tabStore.test.ts && npm run typecheck`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add clients/vscode/src/session.ts clients/vscode/src/tabStore.ts \
  clients/vscode/test/session.test.ts clients/vscode/test/tabStore.test.ts
git commit -m "feat(vscode): list sessions and persist the open tabs"
```

---

### Task 4: Multi-tab routing in the host

**Files:**
- Modify: `clients/vscode/src/chatView.ts`
- Modify: `clients/vscode/src/extension.ts`
- Modify: `clients/vscode/src/sessionAgent.ts`
- Tests: M1's `tab.test.ts` (max, `find`) and `approval.test.ts` (`denyTab`) are the
  pure coverage; this task's host glue is verified manually because it imports `vscode`.

**Interfaces:**
- Consumes: M1 `TabManager`, `SessionAgent`, `Transcript`, `ApprovalBook`, `Renderer`, `FileSnapshots`, `PreviewProvider`; Task 3 `encodeTabs`/`decodeTabs`, `listSessions`.
- Produces:
  ```ts
  class ChatViewProvider {
    private tabs: TabManager;
    private agents: Map<string, SessionAgent>;
    private transcripts: Map<string, Transcript>;
    private approvals: ApprovalBook;
    private activeSession(): LoongSession | undefined;
    private postTab(tabId: string, message: HostMessage): void;
  }
  ```
- Workspace keys: `loongagent.tabs.v1` (StoredTab[]), `loongagent.activeTab.v1` (string).

- [ ] **Step 1: Refactor the host state**

Replace the single `agent`/`transcript` fields with the maps. On `ready`:
- restore `decodeTabs(workspaceState.get("loongagent.tabs.v1"))` into `TabManager`;
- map each `StoredTab` to a `TabState` with `status: "idle"`, restore the active id; if there are no tabs, create one with `newTab()` so the composer always has an active tab;
- post `bootstrap` with tabs, active id, active transcript, models (if a session is live), binary;
- a restored or history tab constructs `SessionAgent` with its `sessionId`; if `ensure()` throws, drop the tab with a visible message. It must not fall back to a new session.

- [ ] **Step 2: Implement the tab messages**

- `newTab`: if `TabManager.open` returns `"max"`, post `"At most 5 tabs"`; otherwise create a `SessionAgent({fresh:true})`, a `Transcript`, open the tab, activate, persist, post `tabs`, and ensure the agent (spawn `--new`).
- `selectTab`: activate, persist, ensure the agent (`--session` or restored), post `transcript` and models.
- `closeTab`: `approvals.denyTab(tabId, "tab closed")`, dispose the agent, remove transcript, close, persist, post tabs and the new active transcript.
- `openSession`: if `tabs.find(sessionId)` exists, focus it; otherwise create a tab with `sessionId` and a `SessionAgent({sessionId})` that spawns `--session <id>`; if it cannot attach, drop the tab with a message; no second writer and no silent new session.
- `history`: ensure a live session (create a tab if none), call `listSessions()`, post `sessions`.
- `send`/`approval`/`cancel`/`selectModel`/`context`/`pickFile`/`pickSelection`/`removeAttachment`/`review`/`openDiff`/`yolo`: route by `tabId`; an unknown tab is an error, never a fallback to the active tab.
- Background turns: post `part`/`delta` with the tab id; update the tab status/badge; the Webview caches them.
- Close a tab with a running turn: dispose the child (Stop) and say so.

- [ ] **Step 3: Persist and restore**

After every open/close/activate/session-id change, write `encodeTabs(tabs)` and the active id. On restore, drop a tab whose session cannot attach with a visible message; continue with the rest. Lazy tabs start their child on first activation.

- [ ] **Step 4: Run tests and the manual multi-tab check**

Run: `cd clients/vscode && npm test && npm run typecheck`
Manual:
- Open three tabs; two turns run at once; transcripts stay isolated.
- A pending approval in tab B gets a badge; switching shows the card; closing B denies it.
- Open the same session from History twice: the second click focuses the first tab.
- Reload: tabs and active transcript return; a deleted session is dropped with a message.

- [ ] **Step 5: Commit**

```bash
git add clients/vscode/src/chatView.ts clients/vscode/src/extension.ts \
  clients/vscode/src/sessionAgent.ts clients/vscode/test/tab.test.ts
git commit -m "feat(vscode): run one session per tab with isolated state"
```

---

### Task 5: The tab bar and history overlay

**Files:**
- Modify: `clients/vscode/webview/main.ts`
- Modify: `clients/vscode/webview/style.css`

**Interfaces:**
- Consumes: `HostMessage.tabs`/`sessions`/`transcript`/`part`/`delta`; `WebviewMessage.newTab`/`selectTab`/`closeTab`/`history`/`openSession`.
- Produces: the tab bar and history overlay described in spec §7.

- [ ] **Step 1: Implement the tab bar**

Render `[title ×]` per tab, active highlight, status dot for `running`/`waiting`, `+`, `History`. `+` sends `newTab`; clicking sends `selectTab`; `×` sends `closeTab`. The active tab's `transcript` replaces the rendered parts; background `part` messages update a `Map<tabId, UiPart[]>` cache.

- [ ] **Step 2: Implement the history overlay**

`History` sends `history`; `sessions` renders newest first with title, updated time and turns; clicking sends `openSession`. A close/backdrop dismisses the overlay. No delete/rename.

- [ ] **Step 3: Style and focus**

Tabs scroll horizontally at narrow widths; the active tab is visible; the overlay uses the editor background and a border; the composer stays focused on switch.

- [ ] **Step 4: Build and verify manually**

Run: `cd clients/vscode && npm run compile`
Manual: three tabs, switch, close, history opens an old session, reload restores, narrow width scrolls, badges appear for background approval.

- [ ] **Step 5: Commit**

```bash
git add clients/vscode/webview/main.ts clients/vscode/webview/style.css
git commit -m "feat(vscode): switch conversations from a tab bar and history"
```

---

### Task 6: Documentation, gate and acceptance

**Files:**
- Modify: `clients/vscode/README.md`
- Modify: `docs/design/surfaces.md`
- Modify: `docs/05-parity-and-synthesis.md`
- Modify: `scripts/check.sh`
- Modify: `.github/workflows/ci.yml`
- Modify: `.gitea/workflows/ci.yml`

- [ ] **Step 1: Document the multi-session behavior**

README: one tab = one session = one child; max 5; same session focuses; close stops; reload restores; one window per session (no cross-window write token); history limit. Update `docs/design/surfaces.md` (VS Code section, code paths, `Verify`) and the parity row. Mention `session.list` is read-only and injected by the CLI.

- [ ] **Step 2: Gate and CI**

`scripts/check.sh` must run `npm run compile && npm test` (M1 added this) and the Rust tests cover `session.list`. Both CI workflows use Node 24 (`actions/setup-node@v4`).

- [ ] **Step 3: Run the gates**

Run: `python3 scripts/gen-schema.py --check`
Run: `cargo test -p loongagent -p loong-server -p loong-schema`
Run: `cd clients/vscode && npm run compile && npm test`
Run: `python3 scripts/check-docs.py`
Run: `FAST=1 ./scripts/check.sh`
Expected: all green.

- [ ] **Step 4: Manual M2 acceptance**

From spec §14 items 6–7: three tabs, two concurrent turns, background approval badge, close denies, reload restores, deleted session drops, history opens/focuses, max-five message, killed child respawns in its tab.

- [ ] **Step 5: Commit**

```bash
git add clients/vscode/README.md docs/design/surfaces.md \
  docs/05-parity-and-synthesis.md scripts/check.sh \
  .github/workflows/ci.yml .gitea/workflows/ci.yml
git commit -m "docs(vscode): document multi-session tabs and the history surface"
```

---

## M2 Done When

- `session.list` is generated, served, injected by the CLI, and covered by Rust tests; a server without a lister returns an empty list.
- The sidebar has a tab bar and history; one tab is one session and one child.
- Same-session clicks focus; max five; close denies pending approvals; reload restores; deleted sessions are dropped visibly.
- Background turns and approvals are isolated per tab; the active transcript is correct after every switch.
- The full gate is green and the docs describe the surface and its limits.
