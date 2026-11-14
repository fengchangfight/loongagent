/* Purpose: the web surface's behaviour — the socket, the transcript, the process panel.
 *
 * Serves: `crates/web/src/index.html`.
 * Design: `docs/design/web-surface.md` §§3–4 — the browser speaks the same JSON-RPC
 * the TUI speaks (ADR-0002); the surface adds the workspace and session catalogue.
 * Up: the page's script tag.
 * Down: the WebSocket to `/ws`, the DOM of `index.html`.
 * Verify: opened against `cargo test -p loong-web`'s server; every state transition
 * here mirrors one the TUI makes over the same methods.
 */

"use strict";

/* ── State ────────────────────────────────────────────────────────────── */

const state = {
  ws: null,              // the socket to the surface
  seq: 0,                // next request id
  drawnTo: 0,            // the highest event seq this transcript already holds
  pending: new Map(),    // id → {resolve, reject}
  token: null,

  workspaces: [],        // from session.tree: each carries its sessions
  seat: 0,               // the workspace this tab sits on
  sessionId: null,       // the session the chat shows
  models: [],            // from model.list, each with the derived `active`

  // Whether this session is in YOLO mode. The log is the record: the last
  // `permissions.relaxed` says what is true, and `permissions.set`'s answer says
  // what the flip to it became (ADR-0007 §5, web-surface.md §4).
  autoApprove: false,

  // The models the profile declares, asked at boot before any session exists, and the
  // provider catalogue the add-model panel lists (web-surface.md §6).
  configured: [],
  providers: [],
  // Every model the panel is showing: `{provider, name, context_window, configured,
  // checked}`. The list *is* the selection — what is ticked is what is on — and
  // `provider` is null for a model hand-written into `local.toml`, which this panel
  // shows but must not rewrite (web-surface.md §6).
  modelRows: [],
  // The key typed for a provider in this dialog, by provider id: it is what the save
  // sends, and what proves the key was already checked when the list was fetched.
  providerKeys: {},

  running: false,        // a turn is in flight
  turn: null,            // the open turn block {panel, summary, body, steps, tools, startedAt}
  lastTurn: null,        // the turn just closed: a late event joins it instead of opening a new card
  draft: null,           // the element the streamed assistant text is landing in
  draftArticle: null,    // its bubble, so a delta joins the one draft instead of adding another
  draftRaw: "",          // what has streamed into it, re-rendered once the text is final
  calls: new Map(),      // tool call_id → {card, out, outcome}, so a result joins its call
  replaying: false,      // a replay is running: its turns carry no stopwatch
  usage: { in: 0, out: 0, cached: 0, calls: 0, ctx: 0, win: 0, cost: 0 },

  // Which workspaces are collapsed in the rail, by dir. Only entries the user
  // toggled by hand are stored; the rest follow the default (non-seat collapsed).
  collapsed: {},
};

/* ── DOM handles ──────────────────────────────────────────────────────── */

const el = (id) => document.getElementById(id);
const dom = {
  messages: el("messages"),
  transcript: el("transcript"),
  prompt: el("prompt"),
  send: el("send"),
  stop: el("stop"),
  thinking: el("thinking"),
  form: el("composer-form"),
  status: el("status"),
  modelbar: el("modelbar"),
  modelSlots: el("model-slots"),
  yolo: el("yolo"),
  addModel: el("add-model"),
  noModels: el("no-models"),
  noModelsAdd: el("no-models-add"),
  addModelDialog: el("add-model-dialog"),
  addProvider: el("add-provider"),
  addKey: el("add-key"),
  addKeyLabel: el("add-key-label"),
  addProbe: el("add-probe"),
  addHint: el("add-hint"),
  addStatus: el("add-status"),
  pickList: el("pick-list"),
  pickFilter: el("pick-filter"),
  pickAll: el("pick-all"),
  pickNone: el("pick-none"),
  pickCount: el("pick-count"),
  addCancel: el("add-cancel"),
  addSubmit: el("add-submit"),
  sessionList: el("session-list"),
  workspaceBtn: el("workspace-btn"),
  picker: el("picker"),
  filePicker: el("file-picker"),
};

/* ── Small helpers ────────────────────────────────────────────────────── */

function scrollDown() {
  dom.transcript.scrollTop = dom.transcript.scrollHeight;
}

function notice(kind, text) {
  const n = document.createElement("div");
  n.className = `notice ${kind}`;
  n.textContent = text;
  dom.messages.appendChild(n);
  scrollDown();
}

function setStatus(text, kind) {
  dom.status.textContent = text;
  dom.status.className = kind || "";
}

/* A turn in flight: the spinner says it is thinking, Stop says it can be ended, and
 * the send button is spent. One function drives all three so they can never disagree
 * (web-surface.md §4). */
function setThinking(on) {
  dom.thinking.hidden = !on;
  dom.stop.hidden = !on;
  dom.stop.disabled = false;
  dom.send.disabled = on;
}

/* Stop the running turn: the TUI's esc.
 *
 * It cannot be a protocol method. A turn owns the session for as long as it runs — the
 * child is blocked inside `prompt.submit` — so nothing the page sent over the socket
 * would ever be read. The page therefore asks the listener to signal that child the way
 * the terminal does, and the turn ends the way any interrupted turn does: with
 * `turn.interrupted` and the answer to `prompt.submit` (web-surface.md §4). */
async function stopTurn() {
  if (!state.running) return;
  dom.stop.disabled = true;
  setStatus("正在中断…");
  try {
    const answer = await fetch(
      `/api/interrupt?token=${encodeURIComponent(state.token)}`,
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ session_id: state.sessionId }),
      },
    ).then((res) => res.json());
    if (!answer.interrupted) {
      // Nothing was running after all, or there is no process to signal: say so
      // rather than leave a button that appears not to work.
      setStatus(answer.reason || "没有可中断的内容", "bad");
      dom.stop.disabled = false;
    }
  } catch (e) {
    setStatus(`停止失败：${e.message}`, "bad");
    dom.stop.disabled = false;
  }
}

function timeAgo(epochSeconds) {
  if (!epochSeconds) return "";
  const delta = Date.now() / 1000 - epochSeconds;
  if (delta < 60) return "刚刚";
  if (delta < 3600) return `${Math.floor(delta / 60)} 分钟前`;
  if (delta < 86400) return `${Math.floor(delta / 3600)} 小时前`;
  return `${Math.floor(delta / 86400)} 天前`;
}

/* The wire is gone: fail everything it owes, and drop the turn's own indicators.
 *
 * A request the socket can no longer answer must not sit pending: its promise would
 * never settle, so the turn's spinner (and Stop) would keep saying a turn is running
 * that nothing can finish — the page only looked right after a reload, which threw
 * the stuck state away. Failing the request lets the code waiting on it finish the
 * usual way, through its own `finally` (web-composer.md §5). */
function linkLost(why) {
  setStatus(why, "bad");
  const inFlight = [...state.pending.values()];
  state.pending.clear();
  for (const request of inFlight) request.reject(new Error("连接已断开"));
  if (state.running) {
    state.running = false;
    renderModels();
    setThinking(false);
    closeTurn();
  }
}

/* ── The socket ───────────────────────────────────────────────────────── */

function connect() {
  const token = (location.hash.match(/token=([0-9a-f]+)/) || [])[1];
  if (!token) {
    setStatus("网址里没有 token——请用 `loongagent web` 打印的那个网址打开", "bad");
    return;
  }
  state.token = token;
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  const ws = new WebSocket(`${proto}//${location.host}/ws?token=${token}`);
  state.ws = ws;

  ws.onopen = () => {
    setStatus("已连接", "ok");
    boot();
  };
  ws.onclose = () => {
    linkLost("服务器关闭了连接——刷新页面重试");
  };
  ws.onerror = () => {
    linkLost("连不上服务器——`loongagent web` 还在运行吗？");
  };
  ws.onmessage = (message) => onLine(message.data);
}

function sendRequest(method, params) {
  return new Promise((resolve, reject) => {
    const id = ++state.seq;
    state.pending.set(id, { resolve, reject });
    state.ws.send(JSON.stringify({ jsonrpc: "2.0", id, method, params: params || {} }));
  });
}

function onLine(raw) {
  let msg;
  try {
    msg = JSON.parse(raw);
  } catch {
    return;
  }
  if (msg.id !== undefined && msg.id !== null && state.pending.has(msg.id)) {
    const { resolve, reject } = state.pending.get(msg.id);
    state.pending.delete(msg.id);
    if (msg.error) reject(new Error(msg.error.message || "request failed"));
    else resolve(msg.result);
    return;
  }
  // Every forwarded line names the session that wrote it. A turn left running in one
  // session goes on writing while the tab shows another, and its events must not land in
  // this chat: the transcript on screen is one session's (web-surface.md §3).
  if (msg.session_id && state.sessionId && msg.session_id !== state.sessionId) return;
  // A notification: a session event or a model delta.
  if (msg.method === "event" && msg.params) {
    onEvent(msg.params);
  } else if (msg.method === "model.delta" && msg.params) {
    appendDraftText(msg.params.text || "");
  }
}

/* ── Boot: workspaces, sessions, models ───────────────────────────────── */

async function boot() {
  try {
    // session.tree carries the workspaces and the seat, so it is the single source.
    await refreshSessions();
    renderModels();
    await refreshModels();
    // Before anything else the page can do: does this machine have a model at all?
    // The panel below depends on this answer, and so does the warning (web-surface.md §6).
    await checkModels();
  } catch (e) {
    notice("error", `启动失败：${e.message}`);
  }
}

async function refreshSessions() {
  // One call returns the whole two-level tree: every workspace with its sessions,
  // each session flagged live/busy (web-surface.md §2).
  const result = await sendRequest("session.tree", {});
  state.workspaces = result.workspaces || [];
  if (typeof result.seat === "number") state.seat = result.seat;
  renderSessions();
}

async function refreshModels() {
  if (!state.sessionId) {
    state.models = [];
    renderModels();
    return;
  }
  try {
    const result = await sendRequest("model.list", { session_id: state.sessionId });
    applyModelList(result);
  } catch {
    /* the session may not be up yet; the bar fills in after the first prompt */
  }
}

/* Fold a ModelList result into state and redraw the picker.
 *
 * The wire names the current model in `ModelList.selected`; a `ModelOffering` carries only
 * what the profile declared — name, model id, provider, window (crates/schema/protocol.toml).
 * `active` is therefore *derived here and nowhere else*: the page used to look for it on each
 * offering, which the server never sends, so the row lit nothing and the chip claimed no model
 * was chosen however many models were listed. */
function markSelected(list) {
  const models = (list && list.models) || [];
  const selected = (list && list.selected) || "";
  for (const m of models) m.active = m.name === selected;
  return models;
}

function applyModelList(list) {
  state.models = markSelected(list);
  const active = state.models.find((m) => m.active);
  // The context window is a property of the model, not of any event: read it from the
  // list so the usage row can show the share of it in use.
  if (active && active.context_window) state.usage.win = active.context_window;
  renderModels();
  renderUsage();
}

/* ── The rail's "add workspace" button ────────────────────────────────── */

/* The button only opens the picker; the tree below is what shows which workspace
   the chat sits in. The picker: list directories, open one as a workspace. */
const picker = {
  cwd: null,
  async open() {
    dom.picker.classList.add("open");
    await this.go(null); // null → the surface's home
  },
  close() {
    dom.picker.classList.remove("open");
  },
  async go(dir) {
    const url = dir
      ? `/api/browse?token=${state.token}&dir=${encodeURIComponent(dir)}`
      : `/api/browse?token=${state.token}`;
    const res = await fetch(url);
    const body = await res.json();
    if (!res.ok) {
      notice("error", body.error || "无法列出那个目录");
      return;
    }
    this.cwd = body.cwd;
    this.render(body);
  },
  render(body) {
    dom.picker.querySelector(".crumbs .path").textContent = body.cwd;
    dom.picker.querySelector(".panel-foot .current").textContent = body.cwd;
    const list = dom.picker.querySelector(".dirs");
    list.innerHTML = "";
    const dirs = body.dirs || [];
    if (dirs.length === 0) {
      list.innerHTML = '<div class="empty">这里没有子目录。</div>';
    }
    for (const d of dirs) {
      const b = document.createElement("button");
      b.className = "dir";
      b.innerHTML = '<span class="icon">📁</span>';
      b.appendChild(document.createTextNode(d.name));
      b.onclick = () => this.go(d.path);
      list.appendChild(b);
    }
    const up = dom.picker.querySelector(".crumbs .up");
    up.disabled = !body.parent;
    up.style.opacity = body.parent ? "1" : "0.4";
    up.onclick = () => body.parent && this.go(body.parent);
    dom.picker.querySelector(".choose").onclick = () => this.choose();
  },
  async choose() {
    try {
      const result = await sendRequest("workspace.add", { dir: this.cwd });
      state.seat = result.index;
      state.sessionId = null;
      clearChat();
      await refreshSessions();
      this.close();
      await refreshModels();
    } catch (e) {
      notice("error", e.message);
    }
  },
};

dom.workspaceBtn.onclick = () => picker.open();
dom.picker.querySelector(".close").onclick = () => picker.close();
dom.picker.onclick = (e) => {
  if (e.target === dom.picker) picker.close();
};

/* ── The model bar: slots on screen, not a dropdown ───────────────────── */

/* Every model is on screen at once, the TUI's own bar in a browser (§3 of
 * `docs/design/surfaces.md`): switching is one keypress, no menu to open and
 * read, and the session's current model is never a thing to go and look up. The
 * lit slot is the whole answer — a filled dot, an accent fill, a bold name and
 * `aria-current` — and the row scrolls to keep it in sight, so the name is not
 * printed a second time beside it (web-surface.md §4). */

function renderModels() {
  const wrap = dom.modelSlots;
  wrap.innerHTML = "";

  if (!state.sessionId || state.models.length === 0) {
    renderYolo();
    return;
  }

  for (const m of state.models) {
    const slot = document.createElement("button");
    slot.type = "button";
    slot.className = "slot" + (m.active ? " current" : "");
    slot.dataset.name = m.name;
    slot.disabled = state.running;
    if (m.active) slot.setAttribute("aria-current", "true");
    slot.title = [m.name, m.context_window ? `ctx ${m.context_window}` : ""]
      .filter(Boolean)
      .join(" · ");

    // Always present, so lighting one up never shifts the row by a character.
    const dot = document.createElement("span");
    dot.className = "dot";
    dot.textContent = "●";
    slot.appendChild(dot);

    const name = document.createElement("span");
    name.className = "name";
    name.textContent = m.name;
    slot.appendChild(name);
    wrap.appendChild(slot);
  }

  renderYolo();
  // Keep the lit slot on screen even when the row is longer than the bar.
  const lit = wrap.querySelector(".slot.current");
  if (lit) lit.scrollIntoView({ block: "nearest", inline: "nearest" });
}

/* Delegated, so re-rendering the row never leaves a listener behind. */
dom.modelSlots.onclick = (e) => {
  const slot = e.target.closest(".slot");
  if (slot && !state.running) chooseModel(slot.dataset.name);
};

async function chooseModel(name) {
  if (state.running || !state.sessionId || !name) return;
  try {
    const result = await sendRequest("model.select", { session_id: state.sessionId, name });
    applyModelList(result);
  } catch (e) {
    notice("error", `model.select: ${e.message}`);
    renderModels(); // snap the bar back to the real active model
  }
}

/* Wrapping step through `model.list`: the last model is one press from the first. */
function stepModel(delta) {
  if (state.running || state.models.length === 0) return;
  const at = state.models.findIndex((m) => m.active);
  const next = ((at < 0 ? 0 : at) + delta + state.models.length) % state.models.length;
  chooseModel(state.models[next].name);
}

/* Move the *current* model one slot. Not a switch: the lit model stays lit and the row
 * rearranges around it, which is why this is a different call and a different chord.
 *
 * The surface owns the order: it reads the session's list, swaps the slot, writes
 * `local.toml` when this is a local profile, and remembers the arrangement so a later
 * refresh does not snap the bar back. The page only draws what comes back and says what
 * happened — a move that reached the file deserves to be said out loud, because it
 * changes the default model of every session after this one (web-surface.md §4). */
async function moveModel(delta) {
  if (state.running || !state.sessionId || state.models.length < 2) return;
  try {
    const result = await sendRequest("model.reorder", { session_id: state.sessionId, delta });
    applyModelList(result);
    const moved = result.moved || {};
    const where = result.persisted
      ? "written to local.toml — new sessions start with this order"
      : "this session only";
    notice("info", `已把 ${moved.name} 移到槽位 ${moved.to}（${where}）`);
    if (result.warning) notice("warn", `顺序没能写回配置：${result.warning}`);
  } catch (e) {
    notice("error", `model.reorder: ${e.message}`);
  }
}

/* Two chords, two acts, and neither takes a key the browser is using.
 *
 * `alt+n` / `alt+p` switch models, wrapping; Ctrl+N ("new window") and Ctrl+P ("print")
 * are the browser's own and no page may keep them. `ctrl+←` / `ctrl+→` reorder the
 * current model, because Alt+←/→ are the browser's back and forward. The bar prints
 * both pairs rather than hiding them in a tooltip (web-surface.md §4). */
function modelActionForKey(e) {
  if (e.metaKey || e.isComposing) return null;
  const key = (e.key || "").toLowerCase();
  const code = e.code || "";
  if (e.altKey && !e.ctrlKey) {
    if (key === "n" || code === "KeyN") return { act: "switch", delta: 1 };
    if (key === "p" || code === "KeyP") return { act: "switch", delta: -1 };
    return null;
  }
  if (e.ctrlKey && !e.altKey) {
    if (key === "arrowright" || code === "ArrowRight") return { act: "order", delta: 1 };
    if (key === "arrowleft" || code === "ArrowLeft") return { act: "order", delta: -1 };
  }
  return null;
}

function wireShortcuts() {
  document.addEventListener("keydown", (e) => {
    // Esc ends a running turn, the TUI's own key (web-surface.md §4). A list that is
    // open claims it first: the `@` picker and the workspace picker both close on Esc
    // and mark the event handled, and a turn must not die because someone meant to
    // dismiss a list.
    if (e.key === "Escape") {
      if (e.defaultPrevented) return;
      if (dom.picker.classList.contains("open")) {
        e.preventDefault();
        picker.close();
        return;
      }
      if (state.running) {
        e.preventDefault();
        stopTurn();
      }
      return;
    }
    const action = modelActionForKey(e);
    if (!action) return;
    if (state.models.length === 0) return;
    e.preventDefault();
    // Reordering needs two models to have an order at all.
    if (action.act === "order" && state.models.length < 2) return;
    if (action.act === "order") moveModel(action.delta);
    else stepModel(action.delta);
  });
}

/* Whether the session on screen has a turn in flight.
 *
 * "Running" belongs to a session, not to the page: a tab that switches away from a
 * working session must not carry its spinner into an idle one, where Stop would end
 * somebody else's turn — and switching back must show the spinner again, because that
 * turn really is still going. The surface keeps the flag per child, and `session.tree`
 * is where the rail already reads it from (web-surface.md §3). */
function busySession(id) {
  if (!id) return false;
  for (const w of state.workspaces) {
    for (const s of w.sessions || []) {
      if (s.id === id) return !!s.busy;
    }
  }
  return false;
}

/* ── YOLO: the session's approval prompts, off or on ──────────────────── */

/* ADR-0007 §5: opting out is itself shown. The switch says its own state in
 * words — `YOLO OFF` quiet and outlined, `YOLO ON` filled amber — because a hue
 * alone is not a label, and this one changes what the agent may do unattended.
 * It draws what the session says, never its own guess: `permissions.relaxed` is
 * the record and `permissions.set`'s answer is the confirmation, so opening an
 * old session shows the opt-out that session really has (web-surface.md §4,
 * ADR-0020). A turn owns the session, but the flip no longer waits for it to end: the
 * surface reaches a running turn over the same private control channel the TUI uses
 * (ADR-0019), and answers at once. The answer says when it lands — `queued: false` means
 * the running turn applies it at its next step, and its own `permissions.relaxed` is the
 * record — so the switch never claims a state that is not in the log yet. On a platform
 * with no channel the surface forwards the request and the flip lands when the turn ends,
 * which is what the switch says then (web-surface.md §4). */

function renderYolo() {
  const on = state.autoApprove;
  dom.yolo.classList.toggle("on", on);
  dom.yolo.setAttribute("aria-pressed", on ? "true" : "false");
  dom.yolo.disabled = !state.sessionId;
  // The same gate the slots carry: adopting a model reloads the live children, and the
  // surface refuses that outright while a turn is running (web-surface.md §6).
  dom.addModel.disabled = state.running;
  dom.yolo.querySelector(".state").textContent = on ? "开" : "关";
  dom.yolo.title = on
    ? "这个会话的审批提示已关闭：工具调用不再询问（沙箱仍然生效）。点击恢复询问。"
    : "审批提示已开启：每次工具调用都会先询问。点击让这个会话不再询问——沙箱仍然生效。";
}

async function setYolo(on) {
  if (!state.sessionId || on === state.autoApprove) return;
  state.autoApprove = on; // the round trip only; the answer below is what stays
  renderYolo();
  try {
    const result = await sendRequest("permissions.set", {
      session_id: state.sessionId,
      auto_approve: on,
    });
    state.autoApprove = !!result.auto_approve;
    renderYolo();
    // A flip that reached the running turn applies at its next step, not before: say
    // when, rather than let the switch look like it did nothing for a while.
    const later = result.queued === false;
    const when = later ? "（正在跑的回合会在下一步应用）" : "";
    notice(
      state.autoApprove ? "warn" : "info",
      (state.autoApprove
        ? "这个会话的审批提示已关闭——沙箱仍然生效"
        : "这个会话的审批提示已重新开启") + when,
    );
  } catch (e) {
    state.autoApprove = !on; // nothing was recorded, so nothing changed
    renderYolo();
    notice("error", `permissions.set: ${e.message}`);
  }
}

dom.yolo.onclick = () => setYolo(!state.autoApprove);

/* ── Models: one list, ticked, saved ──────────────────────────────────── */

/* Two things bring someone here: they have no model at all (the banner under the bar,
 * asked at boot before any session exists) or they want to change which models this
 * machine offers (the button beside the row they are switching in). Both open the same
 * panel, which is one list of models with a box beside each (web-surface.md §6).
 *
 * The list *is* the selection. It opens as what is configured — that is how a model is
 * turned off — and "list its models" adds the rest of a provider, unticked: pasting a key
 * says something about the credentials, not about wanting three hundred models somebody
 * else published. Nothing is ticked on the person's behalf except what is already on.
 *
 * Nothing about config is decided here. The surface proves the key, writes it and the
 * `[[models]]` blocks, and answers with the set it wrote — the page only shows what came
 * back. That is deliberate: `local.toml` has one set of rules and one writer. */

function renderNoModels() {
  dom.noModels.hidden = state.configured.length > 0;
}

/* Asked at boot, from the profile rather than from a session: a brand-new browser has
 * no tab, no child and no `model.list`, and the warning still has to be right. */
async function checkModels() {
  try {
    const result = await sendRequest("model.configured", {});
    state.configured = result.models || [];
  } catch {
    // A surface that cannot answer is not a surface that has no models: say nothing
    // rather than send somebody to configure what is already configured.
    state.configured = [{ name: "unknown" }];
  }
  renderNoModels();
}

/* What the chosen provider wants: a loopback one takes no key at all, and asking for
 * one would be asking for a password that cannot exist (config-routing.md §8). */
function renderAddHint() {
  const id = dom.addProvider.value;
  const provider = state.providers.find((p) => p.id === id);
  if (!provider) {
    dom.addKey.disabled = false;
    dom.addKey.placeholder = "把密钥粘贴到这里";
    dom.addKeyLabel.textContent = "API 密钥";
    dom.addHint.textContent =
      "密钥本身带着服务商的名字（sk-…、sk-ant-…），粘贴即可";
    return;
  }
  dom.addKey.disabled = !provider.needs_key;
  dom.addKey.placeholder = provider.needs_key
    ? `把密钥粘贴到这里（它会写入 ${provider.key_env}）`
    : "不需要密钥——这个跑在你自己的机器上";
  dom.addKeyLabel.textContent = provider.needs_key ? "API 密钥" : "API 密钥（不需要）";
  dom.addHint.textContent = provider.needs_key
    ? `${provider.label} 会先校验密钥，再写入任何东西`
    : `${provider.label} 跑在本地，列出它只需要它的地址能应答`;
}

/* The provider a model row belongs to, or "" for a model this panel must not rewrite:
 * hand-written, or named after an endpoint rather than a catalogue entry. */
const rowProvider = (row) => row.provider || "";

/* The rows the filter leaves visible. Ticking acts on these, so "select all" means all of
 * what is on screen — which is the only useful meaning when a provider has hundreds. */
function visibleRows() {
  const filter = dom.pickFilter.value.trim().toLowerCase();
  return state.modelRows.filter(
    (row) => !filter || row.name.toLowerCase().includes(filter),
  );
}

function renderPickCount() {
  const ticked = state.modelRows.filter((row) => row.checked).length;
  dom.pickCount.textContent = `已选 ${ticked} / ${state.modelRows.length}`;
  dom.addSubmit.textContent = ticked === 1 ? "保存 1 个模型" : `保存 ${ticked} 个模型`;
}

function renderModelRows() {
  const rows = visibleRows();
  dom.pickList.innerHTML = "";
  if (rows.length === 0) {
    const empty = document.createElement("div");
    empty.className = "empty";
    empty.textContent = state.modelRows.length
      ? "没有模型匹配这个筛选"
      : "还没有模型——先在上方选一个服务商并列出它的模型";
    dom.pickList.appendChild(empty);
    renderPickCount();
    return;
  }
  // Grouped by provider: which provider a model belongs to decides which `[[models]]`
  // block it lands in, so a flat list would be hiding the one thing that matters.
  const groups = new Map();
  for (const row of rows) {
    const id = rowProvider(row);
    if (!groups.has(id)) groups.set(id, []);
    groups.get(id).push(row);
  }
  for (const [id, group] of groups) {
    const head = document.createElement("div");
    head.className = "pick-group";
    const label = state.providers.find((p) => p.id === id);
    head.textContent = id ? label?.label || id : "hand-written — edit local.toml";
    dom.pickList.appendChild(head);
    for (const row of group) {
      const line = document.createElement("label");
      line.className = row.provider ? "pick-row" : "pick-row readonly";
      if (row.provider) {
        const box = document.createElement("input");
        box.type = "checkbox";
        box.checked = !!row.checked;
        box.onchange = () => {
          row.checked = box.checked;
          renderPickCount();
        };
        line.appendChild(box);
      }
      const name = document.createElement("span");
      name.className = "name";
      name.textContent = row.name;
      line.appendChild(name);
      const meta = document.createElement("span");
      meta.className = "meta";
      meta.textContent = row.provider
        ? row.context_window
          ? `${Math.round(row.context_window / 1024)}k ctx`
          : ""
        : "no provider field";
      line.appendChild(meta);
      dom.pickList.appendChild(line);
    }
  }
  renderPickCount();
}

async function openAddModel() {
  dom.addStatus.textContent = "";
  dom.addStatus.className = "status";
  dom.addKey.value = "";
  dom.pickFilter.value = "";
  dom.addSubmit.disabled = false;
  dom.addProbe.disabled = false;
  state.providerKeys = {};
  try {
    const result = await sendRequest("model.providers", {});
    const list = result.providers || [];
    state.providers = list;
    dom.addProvider.innerHTML = "";
    const guess = document.createElement("option");
    guess.value = "";
    guess.textContent = "— 从密钥判断 —";
    dom.addProvider.appendChild(guess);
    for (const provider of list) {
      const option = document.createElement("option");
      option.value = provider.id;
      option.textContent = provider.label;
      dom.addProvider.appendChild(option);
    }
  } catch (e) {
    dom.addStatus.textContent = `列不出服务商：${e.message}`;
    dom.addStatus.className = "status bad";
  }
  // The list starts from the file, not from a provider: what is configured now is what
  // the person can turn off (web-surface.md §6).
  await checkModels();
  state.modelRows = state.configured
    .filter((model) => model && model.name)
    .map((model) => ({
      provider: model.provider || null,
      name: model.name,
      context_window: model.context_window || null,
      checked: !!model.provider,
    }));
  renderAddHint();
  renderModelRows();
  dom.addModelDialog.showModal();
  dom.addKey.focus();
}

/* Bring in everything one provider offers, merged rather than replacing: what is
 * configured stays ticked, and the rest arrives unticked — listing must not clear a
 * selection, and it must not select one either. */
function mergeOffered(offered) {
  const id = offered.provider;
  for (const model of offered.models || []) {
    const row = state.modelRows.find(
      (r) => r.provider === id && r.name === model.name,
    );
    if (row) {
      row.context_window = model.context_window ?? row.context_window;
      continue;
    }
    state.modelRows.push({
      provider: id,
      name: model.name,
      context_window: model.context_window ?? null,
      checked: !!model.configured,
    });
  }
}

async function listProviderModels() {
  const provider = dom.addProvider.value;
  const key = dom.addKey.value.trim();
  const chosen = state.providers.find((p) => p.id === provider);
  if (chosen && chosen.needs_key && !key && !state.providerKeys[provider]) {
    dom.addStatus.textContent = "先粘贴密钥";
    dom.addStatus.className = "status bad";
    dom.addKey.focus();
    return;
  }
  dom.addProbe.disabled = true;
  dom.addStatus.textContent = "正在问服务商它提供哪些模型…";
  dom.addStatus.className = "status";
  try {
    const offered = await sendRequest("model.probe", { provider, key });
    // Which key this list is under: the save sends it, so a key that proved out is the
    // key that gets written, and a key is never asked for twice for the same list.
    state.providerKeys[offered.provider] = key;
    mergeOffered(offered);
    renderModelRows();
    const count = (offered.models || []).length;
    dom.addStatus.textContent =
      offered.summary ||
      `${offered.label}：${count} 个模型${offered.curated ? "（来自目录）" : ""}`;
    dom.addStatus.className = "status ok";
  } catch (e) {
    dom.addStatus.textContent = e.message;
    dom.addStatus.className = "status bad";
  }
  dom.addProbe.disabled = false;
}

/* The same names, sorted, for "did this provider's set change". */
const nameList = (rows) => rows.map((row) => row.name).sort();

/* What to write, per provider: the ticked models, and only for the providers whose set
 * actually changed (or whose key was just pasted). A provider whose set did not change
 * does not need a write, and every write drops the idle children so the next turn reads
 * the new config — so a no-op is not free.
 *
 * A provider with nothing ticked that had models is a real write with an empty list: that
 * is what turning them off means. Its key stays, so ticking one again brings it back
 * without asking for the key (web-surface.md §6). */
function plannedWrites() {
  const writes = [];
  const providers = new Set(state.modelRows.map(rowProvider));
  for (const id of providers) {
    if (!id) continue; // hand-written rows are not this panel's to rewrite
    const rows = state.modelRows.filter((row) => row.provider === id);
    const before = nameList(state.configured.filter((m) => m && m.provider === id));
    const after = nameList(rows.filter((row) => row.checked));
    const key = state.providerKeys[id] || "";
    const same = before.length === after.length && before.every((n, i) => n === after[i]);
    if (same && !key) continue;
    writes.push({
      provider: id,
      key,
      models: rows
        .filter((row) => row.checked)
        .map((row) => ({ name: row.name, context_window: row.context_window ?? null })),
    });
  }
  return writes;
}

async function saveModels() {
  const writes = plannedWrites();
  if (writes.length === 0) {
    dom.addStatus.textContent = "没有改动";
    dom.addStatus.className = "status";
    return;
  }
  dom.addSubmit.disabled = true;
  const done = [];
  try {
    for (const write of writes) {
      const label =
        state.providers.find((p) => p.id === write.provider)?.label || write.provider;
      dom.addStatus.textContent = `正在保存 ${label}…`;
      dom.addStatus.className = "status";
      const result = await sendRequest("model.add", write);
      done.push(`${result.label || label}：${(result.models || []).length} 个模型`);
    }
    // The surface reloaded every idle child, so this tab's own child is gone: open the
    // session again to get one that has read the new config, or start the first one.
    await checkModels();
    if (state.sessionId) await openSession(state.sessionId, state.seat);
    else await newSession();
    dom.addModelDialog.close();
    setStatus(`已保存——${done.join("；")}`, "ok");
  } catch (e) {
    dom.addStatus.textContent = done.length
      ? `${done.join("；")}——然后：${e.message}`
      : e.message;
    dom.addStatus.className = "status bad";
    dom.addSubmit.disabled = false;
    // Whatever did land is in the file now, so the list has to say so rather than keep
    // offering to write things that are already written.
    await checkModels();
  }
}

dom.addModel.onclick = () => openAddModel();
dom.noModelsAdd.onclick = () => openAddModel();
dom.addCancel.onclick = () => dom.addModelDialog.close();
dom.addSubmit.onclick = () => saveModels();
dom.addProbe.onclick = () => listProviderModels();
dom.addProvider.onchange = () => renderAddHint();
dom.pickFilter.oninput = () => renderModelRows();
dom.pickAll.onclick = () => {
  for (const row of visibleRows()) if (row.provider) row.checked = true;
  renderModelRows();
};
dom.pickNone.onclick = () => {
  for (const row of visibleRows()) if (row.provider) row.checked = false;
  renderModelRows();
};
dom.addKey.onkeydown = (e) => {
  if (e.key === "Enter") {
    e.preventDefault();
    listProviderModels();
  }
};

/* ── The session rail ─────────────────────────────────────────────────── */

/* The rail remembers which workspaces the user folded, so a refresh keeps them. */
const COLLAPSE_KEY = "loong.rail.collapsed";

function loadCollapsed() {
  try {
    state.collapsed = JSON.parse(localStorage.getItem(COLLAPSE_KEY) || "{}") || {};
  } catch {
    state.collapsed = {};
  }
}

function saveCollapsed() {
  try {
    localStorage.setItem(COLLAPSE_KEY, JSON.stringify(state.collapsed));
  } catch {
    /* storage may be unavailable; the rail just won't remember folds */
  }
}

/* A workspace is collapsed when the user folded it, or — never touched — when it
 * is not the one this tab sits on (so several workspaces don't flood the rail). */
function isCollapsed(w) {
  if (Object.prototype.hasOwnProperty.call(state.collapsed, w.dir)) {
    return !!state.collapsed[w.dir];
  }
  return w.index !== state.seat;
}

function toggleCollapsed(w) {
  state.collapsed[w.dir] = !isCollapsed(w);
  saveCollapsed();
  renderSessions();
}

function renderSessions() {
  const list = dom.sessionList;
  list.innerHTML = "";
  const workspaces = state.workspaces || [];
  if (workspaces.length === 0) {
    list.innerHTML = '<div class="empty">还没有工作区。</div>';
    return;
  }
  for (const w of workspaces) {
    const collapsed = isCollapsed(w);
    const sessions = w.sessions || [];
    // Level one: the workspace header. Clicking it folds/unfolds its sessions.
    const head = document.createElement("button");
    head.className =
      "workspace-row" + (w.index === state.seat ? " active" : "") +
      (collapsed ? " collapsed" : "");
    const chevron = document.createElement("span");
    chevron.className = "chevron";
    chevron.textContent = collapsed ? "▸" : "▾";
    head.appendChild(chevron);
    const icon = document.createElement("span");
    icon.className = "icon";
    icon.textContent = w.home ? "⌂" : "📁";
    head.appendChild(icon);
    const label = document.createElement("span");
    label.className = "title";
    label.textContent = w.name || (w.dir || "").split("/").filter(Boolean).pop() || "/";
    head.appendChild(label);
    // A collapsed header shows how many sessions hide under it.
    if (collapsed && sessions.length > 0) {
      const count = document.createElement("span");
      count.className = "count";
      count.textContent = String(sessions.length);
      head.appendChild(count);
    }
    head.title = (w.dir || "") + (collapsed ? " — 点击展开" : " — 点击折叠");
    head.onclick = () => toggleCollapsed(w);
    // The row's own `＋`: a session started here belongs to *this* workspace, so the
    // button names its target by where it sits rather than by a label (web-surface.md §2,
    // §4). Same shape as the copy/delete chips below: a `span` inside the row's button,
    // stopping the click so it does not also fold the workspace.
    const add = document.createElement("span");
    add.className = "add";
    add.textContent = "＋";
    add.title = `在「${label.textContent}」新建会话`;
    add.setAttribute("aria-label", add.title);
    add.onclick = (e) => {
      e.stopPropagation();
      newSession(w);
    };
    head.appendChild(add);
    list.appendChild(head);

    if (collapsed) continue;

    // Level two: that workspace's sessions, indented under it.
    if (sessions.length === 0) {
      const empty = document.createElement("div");
      empty.className = "empty nested";
      empty.textContent = "还没有会话——点这一行的 ＋ 开始一个。";
      list.appendChild(empty);
      continue;
    }
    for (const s of sessions) {
      const row = document.createElement("button");
      row.className =
        "session-row nested" +
        (s.id === state.sessionId && w.index === state.seat ? " active" : "") +
        (s.running ? " live" : "");

      const title = document.createElement("span");
      title.className = "title";
      title.textContent = s.title || s.id;
      row.appendChild(title);

      const meta = document.createElement("span");
      meta.className = "meta";
      // `last_ms` is when the conversation last moved; `modified` is the file's mtime,
      // which a resume refreshes — hence the fallback only for logs that lack the clock.
      const spokenMs = s.last_ms || (s.modified ? s.modified * 1000 : 0);
      meta.textContent =
        (s.busy ? "运行中 · " : s.running ? "在线 · " : "") +
        `${s.turns || 0} 轮 · ${timeAgo(Math.round(spokenMs / 1000))}`;
      row.appendChild(meta);

      const copy = document.createElement("span");
      copy.className = "copy";
      copy.textContent = "⧉";
      copy.title = `复制会话 id（${s.id}）`;
      copy.onclick = (e) => {
        e.stopPropagation();
        copySessionId(s.id, copy);
      };
      row.appendChild(copy);

      const del = document.createElement("span");
      del.className = "delete";
      del.textContent = "✕";
      del.title = "删除这个会话（它的日志会被彻底移除）";
      del.onclick = (e) => {
        e.stopPropagation();
        deleteSession(s.id, s.title, w.index);
      };
      row.appendChild(del);

      row.onclick = () => openSession(s.id, w.index);
      list.appendChild(row);
    }
  }
}

/* The session id, on the clipboard. A session's id is how you name it to every other
 * surface — `loongagent --resume <id>` — and it is not otherwise visible on the row. */
async function copySessionId(id, chip) {
  try {
    await navigator.clipboard.writeText(id);
  } catch {
    // No clipboard API (or it was refused): a selected textarea is the old way.
    const host = document.body || document.createElement("div");
    const area = document.createElement("textarea");
    area.value = id;
    host.appendChild(area);
    area.select();
    try {
      document.execCommand("copy");
    } catch {
      // Nothing more to try; the status line still shows the id.
    }
    area.remove();
  }
  setStatus(`会话 id ${id} —— 已复制`, "ok");
  // Confirm on the button itself, where the eye already is.
  chip.textContent = "✓";
  chip.classList.add("copied");
  setTimeout(() => {
    chip.textContent = "⧉";
    chip.classList.remove("copied");
  }, 1200);
}

/* A fresh session in one workspace: `workspace` is the rail row whose `＋` was clicked,
 * and its index travels with `session.new` so the surface starts the session there
 * instead of in the tab's current seat (web-surface.md §2). */
async function newSession(workspace) {
  try {
    const params = {};
    if (workspace && typeof workspace.index === "number") {
      params.workspace = workspace.index;
      // The new row has to be seen: unfold the workspace it lands in, even when the
      // user had folded it.
      state.collapsed[workspace.dir] = false;
      saveCollapsed();
    }
    const result = await sendRequest("session.new", params);
    if (typeof params.workspace === "number") state.seat = params.workspace;
    state.sessionId = result.session_id;
    // A session starts with prompts on; the replay after `session.open` is what
    // re-applies a `permissions.relaxed` this page never saw (web-surface.md §4).
    state.autoApprove = false;
    state.running = false;
    clearChat();
    await refreshSessions();
    await refreshModels();
    setThinking(false);
    setStatus(`会话 ${state.sessionId}`, "ok");
  } catch (e) {
    notice("error", `session.new: ${e.message}`);
  }
}

async function openSession(id, workspaceIndex) {
  // Switching is not a prompt and does not touch the session that is working: the tab
  // moves, the turn it left behind keeps going on its own child (web-surface.md §3).
  try {
    // The surface binds a child to this session (starting one if none is live) and
    // attaches the tab; there is no "current session" to name, so none is sent.
    const params = { open: id };
    if (typeof workspaceIndex === "number") params.workspace = workspaceIndex;
    const result = await sendRequest("session.open", params);
    if (typeof workspaceIndex === "number") state.seat = workspaceIndex;
    state.sessionId = result.session_id;
    // Whatever the previous session was, this one's record decides: the replay
    // below carries any `permissions.relaxed` it already has (web-surface.md §4).
    state.autoApprove = false;
    state.running = false;
    clearChat();
    await refreshSessions();
    // The session on screen decides the indicators: if it is the one still working, say so.
    state.running = busySession(state.sessionId);
    setThinking(state.running);
    renderModels();
    await replay(result.session_id);
    await refreshModels();
    setStatus(`会话 ${state.sessionId}`, "ok");
  } catch (e) {
    notice("error", `session.open: ${e.message}`);
  }
}

async function deleteSession(id, title, workspaceIndex) {
  const ok = window.confirm(
    `Delete session “${title || id}”?\n\nIts log and derived index are removed for good.`
  );
  if (!ok) return;
  try {
    if (typeof workspaceIndex === "number" && workspaceIndex !== state.seat) {
      await sendRequest("workspace.switch", { index: workspaceIndex });
      state.seat = workspaceIndex;
    }
    await sendRequest("session.delete", { session_id: id });
    if (state.sessionId === id) {
      state.sessionId = null;
      state.autoApprove = false; // no session, no prompts to be off (web-surface.md §4)
      clearChat();
      renderModels();
    }
    await refreshSessions();
  } catch (e) {
    notice("error", `session.delete: ${e.message}`);
  }
}

/* ── The transcript ───────────────────────────────────────────────────── */

/* One element, with a class and optional text. Everything below builds the page this
 * way rather than by assigning innerHTML: the text being rendered is the model's own
 * output, and markup in it must stay text. */
function node(tag, className, text) {
  const n = document.createElement(tag);
  if (className) n.className = className;
  if (text !== undefined && text !== null) n.textContent = text;
  return n;
}

/* The assistant's words, rendered: fenced blocks become code, `inline` becomes <code>,
 * **this** becomes bold, and blank lines separate paragraphs. Small on purpose — a full
 * markdown engine is not what makes a transcript readable, and every node here is built,
 * never parsed from the model's output into markup. */
function renderWords(container, text) {
  container.innerHTML = "";
  // Rendered words are paragraphs, so the element stops honouring the raw newlines the
  // streaming draft relies on.
  container.classList.add("rendered");
  const chunks = String(text === undefined || text === null ? "" : text).split("```");
  chunks.forEach((chunk, index) => {
    if (index % 2 === 1) {
      // A fenced block: an optional language on the first line, then its body verbatim.
      const lines = chunk.split("\n");
      const language = (lines[0] || "").trim();
      const fenced = language && !/\s/.test(language);
      const body = (fenced ? lines.slice(1).join("\n") : chunk).replace(/\n$/, "");
      const pre = node("pre", "code-block");
      if (fenced) pre.dataset.lang = language;
      pre.appendChild(node("code", null, body));
      container.appendChild(pre);
      return;
    }
    for (const para of chunk.split(/\n{2,}/)) {
      if (!para.trim()) continue;
      const p = node("p");
      for (const part of para.split(/(`[^`\n]+`|\*\*[^*\n]+\*\*)/)) {
        if (!part) continue;
        if (part.length > 2 && part.startsWith("`") && part.endsWith("`")) {
          p.appendChild(node("code", null, part.slice(1, -1)));
        } else if (part.length > 4 && part.startsWith("**") && part.endsWith("**")) {
          p.appendChild(node("strong", null, part.slice(2, -2)));
        } else {
          part.split("\n").forEach((line, at) => {
            if (at > 0) p.appendChild(document.createElement("br"));
            p.appendChild(document.createTextNode(line));
          });
        }
      }
      container.appendChild(p);
    }
  });
}

function clearChat() {
  dom.messages.innerHTML = "";
  // A cleared transcript holds no events, so nothing is a duplicate of it any more.
  state.drawnTo = 0;
  state.turn = null;
  state.lastTurn = null;
  state.draft = null;
  state.draftArticle = null;
  state.draftRaw = "";
  state.calls.clear();
  resetUsage();
}

/* One spoken line in the transcript. `before` puts it above an element already on
 * screen — the turn card opened when the prompt was sent. */
function message(who, text, before) {
  const article = node("div", `msg ${who}`);
  article.appendChild(
    node("div", "who", who === "user" ? "你" : who === "assistant" ? "loongagent" : who),
  );
  const body = node("div", "text");
  if (who === "assistant") renderWords(body, text);
  else body.textContent = text;
  article.appendChild(body);
  if (before && before.parentElement === dom.messages) dom.messages.insertBefore(article, before);
  else dom.messages.appendChild(article);
  scrollDown();
  return body;
}

/* ── Turns ────────────────────────────────────────────────────────────── */

/* A turn is one block holding the turn's machinery and its words, in the order they
 * happened — the tool calls, the policy decisions and the model's own sentences
 * interleaved, because that is what happened. A turn the reader watched stays open
 * when it ends; only a replayed one (history from an earlier sitting) folds shut, so
 * a long resumed session reads as a stack of summaries that open on demand. */

function openTurn(opts) {
  const quiet = !!(opts && opts.quiet);
  const pending = !!(opts && opts.pending);
  if (state.turn) closeTurn();
  const panel = node("details", "turn");
  panel.open = true;
  const summary = node("summary");
  // A quiet turn is one that arrived outside a prompt: it has nothing to wait for,
  // so it gets no spinner to leave running.
  if (!quiet) summary.appendChild(node("span", "spinner"));
  summary.appendChild(node("span", "label", quiet ? "轮次" : "正在工作…"));
  summary.appendChild(node("span", "meta", ""));
  panel.appendChild(summary);
  const body = node("div", "turn-body");
  panel.appendChild(body);
  dom.messages.appendChild(panel);
  state.turn = {
    panel,
    summary,
    body,
    steps: 0,
    tools: 0,
    startedAt: Date.now(),
    // A replayed turn happened in the past: a stopwatch over it means nothing.
    replayed: !!state.replaying,
    // Opened by `submit` before the first event: the user's own message belongs
    // above this card, not below it.
    pending,
    quiet,
  };
  scrollDown();
  return state.turn;
}

/* The turn an event belongs in.
 *
 * A turn is opened by a prompt in flight or by a replay, never by an event that
 * merely shows up: a line that arrives after the answer came back belongs to the
 * turn just finished. Opening a card for it left a "Working…" spinner nothing
 * would ever stop, under a turn that had already closed. */
function ensureTurn() {
  if (state.turn) return state.turn;
  if (state.running || state.replaying) return openTurn();
  if (state.lastTurn) return state.lastTurn;
  return openTurn({ quiet: true });
}

function bumpTurn(turn) {
  const shown = turn || state.turn;
  if (!shown) return;
  const meta = shown.summary.querySelector(".meta");
  const bits = [];
  if (shown.steps) bits.push(`${shown.steps} step${shown.steps === 1 ? "" : "s"}`);
  if (shown.tools) bits.push(`${shown.tools} tool${shown.tools === 1 ? "" : "s"}`);
  if (meta) meta.textContent = bits.join(" · ");
}

/* The turn is over: drop the spinner, state what happened, and leave the machinery
 * readable — a turn the reader was watching stays open, because folding it away the
 * moment the answer lands hides the very words it was waiting for. History folds. */
function closeTurn() {
  const turn = state.turn;
  if (!turn) return;
  // Render what streamed before anything below can drop the draft: the finished
  // message only re-renders a draft that is still open, so a close that dropped it
  // first would leave raw markdown on screen forever.
  finishDraft();
  const spinner = turn.summary.querySelector(".spinner");
  if (spinner) spinner.remove();
  const meta = turn.summary.querySelector(".meta");
  const bits = [];
  if (turn.steps) bits.push(`${turn.steps} step${turn.steps === 1 ? "" : "s"}`);
  if (turn.tools) bits.push(`${turn.tools} tool${turn.tools === 1 ? "" : "s"}`);
  if (!turn.replayed) {
    bits.push(`${((Date.now() - turn.startedAt) / 1000).toFixed(1)}s`);
  }
  if (meta) meta.textContent = bits.join(" · ");
  const label = turn.summary.querySelector(".label");
  if (label) label.textContent = "轮次";
  // An exchange that produced nothing at all — a prompt that failed before the
  // session said anything — is not worth a card.
  if (turn.pending && turn.steps === 0 && turn.tools === 0 && turn.body.children.length === 0) {
    turn.panel.remove();
    state.turn = null;
    state.draft = null;
    state.draftArticle = null;
    state.draftRaw = "";
    return;
  }
  // A replayed turn folds shut as a record — unless it still holds a question no decision
  // has settled: that question is the turn's live content, and folding it would hide the
  // buttons a refreshed page has just made clickable again (web-surface.md §4). A retired
  // question is a record like the rest, so it does not hold the turn open.
  if (turn.replayed && !turn.body.querySelector(".approval:not(.answered):not(.expired)")) {
    turn.panel.open = false;
  }
  state.turn = null;
  // Where the turn an out-of-prompt event belongs: it joins this card instead of
  // opening a new one.
  state.lastTurn = turn;
}

/* A step: one line of machinery inside the open turn. */
function addStep(className, badgeText, badgeClass) {
  const turn = ensureTurn();
  const step = node("div", `step ${className}`);
  step.appendChild(node("span", `badge ${badgeClass || ""}`.trim(), badgeText));
  turn.body.appendChild(step);
  turn.steps += 1;
  bumpTurn(turn);
  scrollDown();
  return step;
}

/* The arguments of a tool call, short enough to sit on the summary line. */
function argsInline(args) {
  if (!args || typeof args !== "object") return "";
  const parts = [];
  for (const [key, value] of Object.entries(args)) {
    let shown;
    if (typeof value === "string") shown = value.replace(/\s+/g, " ").trim();
    else shown = JSON.stringify(value);
    if (shown === undefined) continue;
    if (shown.length > 60) shown = `${shown.slice(0, 57)}…`;
    parts.push(`${key}=${shown}`);
  }
  return parts.join(" ");
}

function prettyJson(value) {
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

/* A token count, short: 23628 reads as 23.6k. */
function tokens(n) {
  if (typeof n !== "number") return "?";
  return n >= 1000 ? `${(n / 1000).toFixed(1)}k` : String(n);
}

/* A tool call: what ran, with what arguments, and — once its result arrives — what came
 * back. The output is what a reader actually wants to scan, so it gets a monospaced
 * block, the first lines always visible, and the whole thing one click away. */
function renderToolCall(data) {
  const turn = ensureTurn();
  const callId = data.call_id || "";
  const card = node("details", "step tool");
  const summary = node("summary");
  summary.appendChild(node("span", "badge tool", "工具"));
  summary.appendChild(node("strong", "tool-name", data.tool || "工具"));
  const inline = argsInline(data.args);
  if (inline) summary.appendChild(node("span", "args-inline", inline));
  const outcome = node("span", "outcome", "");
  summary.appendChild(outcome);
  card.appendChild(summary);
  const args = node("pre", "args");
  args.textContent = prettyJson(data.args === undefined ? {} : data.args);
  card.appendChild(args);
  const result = node("div", "result");
  card.appendChild(result);
  turn.body.appendChild(card);
  turn.tools += 1;
  turn.steps += 1;
  bumpTurn(turn);
  if (callId) state.calls.set(callId, { card, out: result, outcome });
  scrollDown();
}

/* A tool's answer, attached to the call it belongs to rather than shown as its own
 * disconnected card. */
function renderToolResult(data) {
  const turn = ensureTurn();
  const ok = data.ok !== false;
  const text = data.output === undefined || data.output === null ? "" : String(data.output);
  const lines = text.split("\n");
  const limit = 12;
  const known = data.call_id ? state.calls.get(data.call_id) : null;
  let host;
  if (known) {
    known.card.classList.add(ok ? "ok" : "failed");
    known.outcome.textContent = ok ? "✓" : "✕";
    known.outcome.className = `outcome ${ok ? "ok" : "failed"}`;
    host = known.out;
  } else {
    const card = node("details", "step tool");
    const summary = node("summary");
    summary.appendChild(node("span", `badge ${ok ? "ok" : "failed"}`, ok ? "结果" : "失败"));
    card.appendChild(summary);
    host = node("div", "result");
    card.appendChild(host);
    turn.body.appendChild(card);
    turn.steps += 1;
  }
  if (!text) {
    host.appendChild(node("div", "empty", ok ? "没有输出" : "失败了，且没有输出"));
  } else {
    const pre = node("pre", "output");
    pre.textContent = lines.slice(0, limit).join("\n");
    host.appendChild(pre);
    if (lines.length > limit) {
      const more = node("button", "toggle", `显示全部 ${lines.length} 行 ▾`);
      more.onclick = () => {
        pre.textContent = text;
        more.remove();
      };
      host.appendChild(more);
    }
  }
  bumpTurn(turn);
  scrollDown();
}

/* The assistant's streaming words. Appending text during the stream keeps the cost
 * linear; the markdown-ish rendering happens once, when the finished message arrives. */
function appendDraftText(delta) {
  if (!delta) return;
  const turn = ensureTurn();
  if (!state.draft || !state.draftArticle || state.draftArticle.parentElement !== turn.body) {
    const article = node("div", "msg assistant in-turn");
    article.appendChild(node("div", "who", "loongagent"));
    const body = node("div", "text");
    article.appendChild(body);
    turn.body.appendChild(article);
    state.draft = body;
    state.draftArticle = article;
    state.draftRaw = "";
  }
  state.draftRaw += delta;
  state.draft.textContent += delta;
  scrollDown();
}

/* The one place the streaming draft becomes rendered words. The finished
 * `message.appended` hands its text over; a turn that closes without one — an
 * interrupt, an error, a line the bridge held back — hands over what streamed
 * instead, so the bubble never stays raw markdown. Either way the draft is
 * cleared, and an empty draft renders nothing. */
function finishDraft(text) {
  if (!state.draft) return;
  const draft = state.draft;
  const words = typeof text === "string" && text.trim() ? text : state.draftRaw;
  state.draft = null;
  state.draftArticle = null;
  state.draftRaw = "";
  if (words.trim()) renderWords(draft, words);
}

/* Replay a session's log into the chat (web-surface.md §3). */
async function replay(id) {
  const result = await sendRequest("session.replay", { session_id: id, from_seq: 0 });
  state.replaying = true;
  try {
    for (const env of result.events || []) {
      onEvent(env, true);
    }
  } finally {
    state.replaying = false;
  }
  // Asked *after* the log is drawn, because `session.attach` reports the kernel's state
  // now: `pending_approval` is the one question this page is allowed to make answerable,
  // and every other open one is a record of a process that is gone. It is a read:
  // nothing is recorded and nothing is streamed (sessions-lineage.md §5).
  let live = null;
  let known = false;
  try {
    // `cwd` is the directory this tab stands in; the surface does not read it, but the
    // call carries it so a log read from a script and from the page look the same.
    const seat = state.workspaces.find((w) => w.index === state.seat);
    const attached = await sendRequest("session.attach", {
      cwd: (seat && seat.dir) || "",
      client: "web",
    });
    live = attached.pending_approval || null;
    known = true;
  } catch (e) {
    // Without the kernel's word nothing is retired: a question drawn `held` is already
    // out of reach, and inventing "the process is gone" for one would be the same guess
    // that made this wrong before.
    notice(
      "warn",
      `session.attach: ${e.message} —— 没能确认这个会话还在等哪个问题，刷新页面再看一次`,
    );
  }
  // The log has no "turn over" event: the last turn ends where the log does — after the
  // dead questions are put away, so a turn whose only question is a record folds shut.
  if (known) retireQuestions(live);
  closeTurn();
  scrollDown();
}

/* Answering a question is one rule, and the kernel is the only one who can say yes.
 *
 * The log carries `approval.requested` and, if the person answered, `approval.decided`.
 * It carries no "and now nobody is waiting": that fact lives in `Kernel.pending`, in
 * memory, in the process that asked. So the log alone can tell a *record* from an *open
 * question*, never an *answerable* one from a dead one — and a page that guesses gets it
 * wrong in both directions. It has done, twice: every replayed question was drawn
 * clickable and every click was doomed, then every replayed question was greyed and a
 * page refreshed on a live question could never answer it.
 *
 * So the page does not guess. A question is drawn *held* (buttons there, out of reach),
 * and `retireQuestions` — one pass, after the log is drawn — splits them three ways by
 * what is actually known:
 *   - `answered`: the log holds its decision. A record, greys, stays.
 *   - `live`: `session.attach` named it — the kernel is waiting on this one right now.
 *     Its buttons come into reach. A question arriving on the live stream is live by
 *     construction, so it is let through the same door (`openBox`).
 *   - everything else open: the process that asked it is gone, so a click could only
 *     return `no approval is pending`. Retired with one line saying so.
 * The pass walks the transcript and matches ids there, so one box is one row of the
 * transcript and no second register can drift out of step with it. */
function retireQuestions(live) {
  for (const box of document.querySelectorAll("#chat .approval")) {
    if (box.classList.contains("answered")) continue;
    if (box.dataset.request && box.dataset.request === live) {
      openBox(box);
      continue;
    }
    if (!box.classList.contains("held")) continue; /* already spent by its own click */
    retireBox(box, RETIRED);
  }
}

/* Put a question out of reach where it sits, with the line that says why. Dashed and
 * greyed rather than marked answered: nobody decided it. */
function retireBox(box, why) {
  box.classList.remove("held");
  box.classList.add("expired");
  for (const button of box.querySelectorAll("button")) {
    button.disabled = true;
  }
  const what = box.querySelector(".what");
  if (what && !box.querySelector(".expired-note")) {
    what.appendChild(node("span", "expired-note", why));
  }
}

/* The one line a dead question carries. */
const RETIRED = "这个问题是之前那个进程问的，那个进程已经不在了，所以现在没法再答 —— 重新发一次提问即可";

/* ── Events: one handler for live and replayed ones ───────────────────── */

function onEvent(env, replayed) {
  // A log line and the live stream can carry the same event: switching away from a busy
  // session and back replays what was written while the tab looked elsewhere, and lines
  // already queued for this socket may still arrive afterwards. `seq` is the log's own
  // order and only grows, so the highest one drawn says what is already on screen — the
  // replayed line and the streamed line for one event are the same event, drawn once
  // (web-surface.md §5). A surface-made notice carries `seq: 0` and is never a duplicate.
  const seq = typeof env.seq === "number" ? env.seq : 0;
  if (seq > 0) {
    if (seq <= state.drawnTo) return;
    state.drawnTo = seq;
  }
  const kind = env.kind;
  const data = env.data || {};
  switch (kind) {
    case "message.appended": {
      const role = data.role;
      const text = data.text === undefined || data.text === null ? "" : String(data.text);
      if (role === "user") {
        // A user message is the seam between turns: it closes the turn before it —
        // except the one opened for this very prompt, which is the turn it belongs
        // to and is still waiting for the first event. It sits above that card.
        const pending = state.turn && state.turn.pending ? state.turn : null;
        if (pending) pending.pending = false;
        else closeTurn();
        message("user", text, pending ? pending.panel : null);
      } else if (role === "assistant") {
        // The finished text supersedes what streamed: render it once, properly.
        // finishDraft renders into the open draft bubble when there is one; without
        // a draft the message needs a bubble of its own.
        const streamed = !!state.draft;
        finishDraft(text);
        if (!streamed && text.trim()) {
          const turn = ensureTurn();
          const article = node("div", "msg assistant in-turn");
          article.appendChild(node("div", "who", "loongagent"));
          const body = node("div", "text");
          renderWords(body, text);
          article.appendChild(body);
          turn.body.appendChild(article);
        }
        // An empty assistant message is a tool-only turn: the steps already say what
        // happened, so an empty bubble would only be noise.
        scrollDown();
      }
      break;
    }
    case "tool.call":
      renderToolCall(data);
      break;
    case "tool.result":
      renderToolResult(data);
      break;
    case "policy.decided": {
      // The most numerous event by far: a quiet one-line chip, not a card.
      const verdict = data.verdict || "allow";
      const chip = addStep(`策略 ${verdict}`, verdict, verdict);
      if (data.action_class) chip.appendChild(node("span", "class", data.action_class));
      if (data.rule) chip.appendChild(node("span", "rule", data.rule));
      break;
    }
    case "approval.requested":
      showApproval(data, replayed);
      break;
    case "approval.decided": {
      // The decision that settles a question: this tab's, another tab's, or one replayed
      // from the log after a refresh. The box is found by the request it answers, so a
      // replayed log greys exactly the questions that were answered.
      decideQuestions(data.request_id, !!data.granted, data.scope);
      const chip = addStep("审批", data.granted ? "已批准" : "已拒绝", data.granted ? "ok" : "deny");
      if (data.reason) chip.appendChild(node("span", "rule", data.reason));
      break;
    }
    case "skill.invoked": {
      const chip = addStep("技能", "skill", "skill");
      chip.appendChild(node("strong", "tool-name", data.name || "skill"));
      if (data.source) chip.appendChild(node("span", "rule", data.source));
      if (data.error) chip.appendChild(node("span", "rule deny", data.error));
      break;
    }
    case "compaction.applied": {
      const chip = addStep("压缩", "compaction", "info");
      const before = data.tokens_before;
      const after = data.tokens_after;
      if (typeof before === "number" && typeof after === "number") {
        chip.appendChild(node("span", "class", `${tokens(before)} → ${tokens(after)} tokens`));
      }
      if (data.summary) chip.appendChild(node("span", "rule", data.summary));
      break;
    }
    case "context.degraded": {
      const chip = addStep("警告", "上下文", "ask");
      chip.appendChild(node("span", "class", data.reason || "degraded"));
      break;
    }
    case "turn.interrupted": {
      const chip = addStep("警告", "已中断", "deny");
      if (data.reason) chip.appendChild(node("span", "rule", data.reason));
      break;
    }
    case "cost.recorded":
      recordCost(data);
      break;
    case "model.selected": {
      if (data.model) {
        state.models.forEach((m) => (m.active = m.name === data.model));
        if (state.models.length > 0) renderModels();
        else refreshModels();
      }
      if (typeof data.context_tokens === "number") {
        state.usage.ctx = data.context_tokens;
        renderUsage();
      }
      break;
    }
    case "permissions.relaxed":
      // The record of the flip, whether replayed or live. The button draws this
      // and nothing else, so a page that lost the answer still tells the truth.
      state.autoApprove = !!data.auto_approve;
      renderYolo();
      break;
    case "client.error":
      notice("error", data.message || "client error");
      break;
    default:
      // Bookkeeping the transcript does not draw: skills.loaded, profile.selected,
      // hook.fired, control.synced, session.forked.
      break;
  }
}

/* ── Approvals ────────────────────────────────────────────────────────── */

/* `approval.requested` carries the id the answer must quote: `data.request_id`, not the
 * event's own id — an event id is the log's, and `approval.respond` would not recognise
 * it.
 *
 * A question goes grey when its decision arrives, and only then — `decideBox`, which the
 * click, a replayed log and another tab's answer all come through. Answerability is a
 * different fact and has its own one door: see `retireQuestions`. */
function showApproval(data, replayed) {
  const turn = ensureTurn();
  const box = node("div", "approval");
  // The request id travels with the box: the retirement pass and each decision find boxes
  // by it in the transcript, so one box is one row and there is no second register.
  box.dataset.request = data.request_id || "";
  const what = node("div", "what");
  what.appendChild(node("div", "headline", "请求审批"));
  if (data.action_class) what.appendChild(node("span", "class", data.action_class));
  if (data.reason) what.appendChild(node("span", "reason", data.reason));
  const approve = node("button", "approve", "批准");
  const approveSession = node("button", "approve", "本会话内都批准");
  const refuse = node("button", "refuse", "拒绝");
  const answer = (granted, scope) => {
    // The click spends the buttons at once — one question takes one answer — but the
    // decision is drawn only once the kernel has taken it. Grey "已批准 ✓" laid down on a
    // guess is the same lie as a replayed question drawn answered.
    [approve, approveSession, refuse].forEach((button) => {
      button.disabled = true;
    });
    // Answering resumes the turn, and the resumed turn makes model calls that can take as
    // long as any other: the spinner says so and Stop can end it (web-surface.md §4). The
    // server runs that resume in front of this connection exactly as it runs a new turn,
    // so `approval.decided` greys this box the moment it is recorded and the rest of the
    // turn streams into the transcript as it happens — not one silent block at the end.
    state.running = true;
    renderModels();
    setThinking(true);
    sendRequest("approval.respond", {
      session_id: state.sessionId || "default",
      request_id: data.request_id,
      granted,
      // "session" is the scope the kernel remembers: it stores the command's
      // irreversible shape for the rest of the session, so the same shape does
      // not ask again. "once" runs this call and forgets.
      scope,
    })
      // The decision itself arrives as the event the resume streams; this is the same
      // fact in the answer, for the case where the event was lost on the way.
      .then(() => decideBox(box, granted, scope))
      .catch((e) => {
        notice("error", `approval.respond: ${e.message}`);
        if (String(e.message).includes("no approval is pending")) {
          // The kernel does not hold this question, so it was never ours to answer: the
          // box becomes the record it always was, and the line says why.
          retireBox(box, RETIRED);
        } else {
          // Anything else — a broken socket, a locked session — leaves the question open,
          // so the buttons come back and the user can try again.
          openBox(box);
        }
      })
      .finally(() => {
        state.running = false;
        renderModels();
        setThinking(false);
        // The answer's response is the resumed turn's end, exactly as `submit`'s response
        // is a new turn's: the events of the resume belong to a card, and that card ends
        // here. A question the resume parks on opens its own box in it.
        closeTurn();
      });
  };
  approve.onclick = () => answer(true, "once");
  approveSession.onclick = () => answer(true, "session");
  refuse.onclick = () => answer(false, "once");
  box.appendChild(what);
  box.appendChild(approve);
  box.appendChild(approveSession);
  box.appendChild(refuse);
  turn.body.appendChild(box);
  // Held means "there are buttons here and the kernel has not said this one is yours":
  // a question replayed from the log is not answerable until `retireQuestions` asks, and
  // a question arriving on the live stream is — the kernel just asked it.
  if (replayed) hold(box);
  else openBox(box);
  turn.panel.open = true;
  scrollDown();
}

/* The record of a decision, wherever it was made: greyed by `answered`, and the button
 * that was taken says so. */
function decideBox(box, granted, scope) {
  box.classList.remove("held");
  box.classList.remove("expired");
  // A decision outranks the page's earlier guess: this question was put away as dead and
  // was not — another tab answered it, or the kernel recorded it late.
  const note = box.querySelector(".expired-note");
  if (note && note.parentElement) note.parentElement.removeChild(note);
  box.classList.add("answered");
  const buttons = box.querySelectorAll("button");
  if (buttons[0]) buttons[0].textContent = granted && scope === "once" ? "已批准 ✓" : "批准";
  if (buttons[1]) {
    buttons[1].textContent = granted && scope === "session" ? "已批准 ✓" : "本会话内都批准";
  }
  if (buttons[2]) buttons[2].textContent = granted ? "拒绝" : "已拒绝 ✕";
  for (const button of buttons) button.disabled = true;
}

/* A decision finds its question in the transcript, by the id the question carried — the
 * same road for this tab's click, a decision replayed from the log, and one made in
 * another tab on the same session. */
function decideQuestions(requestId, granted, scope) {
  if (!requestId) return;
  for (const box of document.querySelectorAll("#chat .approval")) {
    if (box.dataset.request === requestId) decideBox(box, granted, scope);
  }
}

/* A question with buttons that reach nothing yet: drawn, not answerable. */
function hold(box) {
  box.classList.add("held");
  for (const button of box.querySelectorAll("button")) button.disabled = true;
}

/* The kernel named this question: its buttons are the answer it waits for. */
function openBox(box) {
  box.classList.remove("held");
  for (const button of box.querySelectorAll("button")) button.disabled = false;
}

/* ── Usage: the TUI's own five numbers, in the browser ────────────────── */

function recordCost(data) {
  const u = state.usage;
  u.calls += 1;
  u.in += data.tokens_in || 0;
  u.out += data.tokens_out || 0;
  // `cache_hit_tokens` and `usd_micros` are what `cost.recorded` carries; the context
  // share is not on this event at all — `model.selected` reports it.
  u.cached += data.cache_hit_tokens || 0;
  u.cost += (data.usd_micros || 0) / 1e6;
  renderUsage();
}

function resetUsage() {
  state.usage = { in: 0, out: 0, cached: 0, calls: 0, ctx: 0, win: 0, cost: 0 };
  renderUsage();
}

function renderUsage() {
  const u = state.usage;
  const fmt = (n) => (n >= 1000 ? `${(n / 1000).toFixed(1)}k` : String(n));
  el("u-in").textContent = fmt(u.in);
  el("u-out").textContent = fmt(u.out);
  el("u-cached").textContent = fmt(u.cached);
  el("u-rate").textContent = u.in > 0 ? `${Math.round((u.cached / u.in) * 100)}%` : "—";
  el("u-calls").textContent = String(u.calls);
  const ctx = el("u-ctx");
  if (u.win > 0) {
    const share = (u.ctx / u.win) * 100;
    ctx.textContent = `${share.toFixed(1)}%`;
    ctx.className = share > 80 ? "warn" : "";
  } else {
    ctx.textContent = "—";
  }
  el("u-cost").textContent = `$${u.cost.toFixed(4)}`;
}

/* ── The composer ─────────────────────────────────────────────────────── */

/* `@` at a word boundary opens the workspace's files above the input and filters
 * them as they are typed; Tab, Enter or a click leaves `@path ` in the message and
 * closes it (web-composer.md §2). The list is the server's: it knows the seat's
 * directory and ranks the matches, so the browser never names a path itself.
 * What the message ends up carrying is text, exactly as if it had been typed by
 * hand — the kernel reads the file with its own tools, and no second way of
 * putting a file in a turn exists. */
const filePicker = {
  open: false,
  rows: [],            // the ranked paths, best first
  index: 0,            // the row Tab/Enter would take
  start: -1,           // where the `@` sits in the textarea
  query: null,         // what has been typed after it, or null when nothing asked
  timer: null,
  asked: 0,            // the newest request's number, so an older answer is dropped
};

/* The word under the caret, when it is an `@` word; null otherwise. A mention
 * begins at a word boundary, so an e-mail address or `a@b` never opens the list. */
function mentionAtCaret() {
  const caret = dom.prompt.selectionStart;
  const before = dom.prompt.value.slice(0, caret);
  const at = before.lastIndexOf("@");
  if (at === -1) return null;
  const boundary = at === 0 || /\s/.test(before[at - 1]);
  if (!boundary) return null;
  const word = before.slice(at + 1);
  // A space ends the mention: the picker is about a path, not a sentence.
  if (/\s/.test(word)) return null;
  return { start: at, query: word };
}

function closeFilePicker() {
  filePicker.open = false;
  filePicker.query = null;
  filePicker.start = -1;
  dom.filePicker.hidden = true;
  dom.filePicker.innerHTML = "";
}

async function askFiles(query) {
  const asked = ++filePicker.asked;
  const url = `/api/files?token=${state.token}&seat=${state.seat}` +
    `&q=${encodeURIComponent(query)}&limit=8`;
  let body;
  try {
    const res = await fetch(url);
    body = await res.json();
    if (!res.ok) return;
  } catch {
    return;
  }
  // A later keystroke has been asked since; its answer is the one that counts.
  if (asked !== filePicker.asked || !filePicker.open) return;
  filePicker.rows = body.files || [];
  filePicker.index = 0;
  renderFilePicker();
}

function renderFilePicker() {
  if (!filePicker.open || filePicker.rows.length === 0) {
    dom.filePicker.hidden = true;
    dom.filePicker.innerHTML = "";
    return;
  }
  dom.filePicker.innerHTML = "";
  filePicker.rows.forEach((path, i) => {
    const row = document.createElement("div");
    row.className = "file-row" + (i === filePicker.index ? " picked" : "");
    row.setAttribute("role", "option");
    row.setAttribute("aria-selected", i === filePicker.index ? "true" : "false");
    // The name first: the directory is context, and a long path must not push it out.
    const name = document.createElement("span");
    name.className = "file-name";
    name.textContent = path.split("/").pop();
    const dir = document.createElement("span");
    dir.className = "file-dir";
    dir.textContent = path.slice(0, path.length - name.textContent.length).replace(/\/$/, "");
    row.appendChild(name);
    if (dir.textContent) row.appendChild(dir);
    row.onmousedown = (e) => {
      // mousedown, not click: the textarea must not lose the caret first.
      e.preventDefault();
      takeFile(i);
    };
    dom.filePicker.appendChild(row);
  });
  dom.filePicker.hidden = false;
}

/* Take the chosen path: the `@` word becomes `@path `, and the rest of the
 * message is left alone — a mention is inserted, not a message replaced. */
function takeFile(index) {
  const path = filePicker.rows[index];
  if (path === undefined) return;
  const value = dom.prompt.value;
  const caret = dom.prompt.selectionStart;
  const head = value.slice(0, filePicker.start);
  const tail = value.slice(caret);
  const inserted = `@${path} `;
  dom.prompt.value = head + inserted + tail;
  const at = head.length + inserted.length;
  dom.prompt.setSelectionRange(at, at);
  closeFilePicker();
  dom.prompt.focus();
  dom.prompt.oninput();
}

function moveFilePicker(delta) {
  if (filePicker.rows.length === 0) return;
  filePicker.index =
    (filePicker.index + delta + filePicker.rows.length) % filePicker.rows.length;
  renderFilePicker();
}

function syncFilePicker() {
  const mention = mentionAtCaret();
  if (!mention) {
    closeFilePicker();
    return;
  }
  filePicker.open = true;
  filePicker.start = mention.start;
  if (mention.query === filePicker.query) return;
  filePicker.query = mention.query;
  // One request per pause, not per keystroke: the server runs `git ls-files`.
  clearTimeout(filePicker.timer);
  filePicker.timer = setTimeout(() => askFiles(mention.query), 120);
}

dom.form.onsubmit = (e) => {
  e.preventDefault();
  submit();
};

dom.stop.onclick = () => stopTurn();

dom.prompt.onkeydown = (e) => {
  // Only keys the list can answer are taken from the input: with no rows on
  // screen, Enter still sends and Tab still leaves the field.
  if (filePicker.open && filePicker.rows.length > 0) {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      moveFilePicker(e.key === "ArrowDown" ? 1 : -1);
      return;
    }
    if ((e.key === "Enter" || e.key === "Tab") && !e.shiftKey) {
      e.preventDefault();
      takeFile(filePicker.index);
      return;
    }
  }
  if (filePicker.open && e.key === "Escape") {
    e.preventDefault();
    closeFilePicker();
    return;
  }
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    submit();
  }
};

dom.prompt.oninput = () => {
  dom.prompt.style.height = "auto";
  dom.prompt.style.height = Math.min(dom.prompt.scrollHeight, 180) + "px";
  syncFilePicker();
};

dom.prompt.onblur = () => closeFilePicker();

async function submit() {
  const text = dom.prompt.value.trim();
  if (!text || state.running) return;
  if (!state.sessionId) {
    await newSession();
    if (!state.sessionId) return;
  }
  dom.prompt.value = "";
  dom.prompt.style.height = "auto";
  // The answer to this prompt may arrive after the tab has moved to another session,
  // and it ends *that* turn, not whatever is on screen by then (web-surface.md §3).
  const asked = state.sessionId;
  state.running = true;
  renderModels();
  setThinking(true);
  // The card and its spinner start now, not when the first event arrives: the model
  // may think for ten seconds before it says anything, and that silence must not
  // read as a stall. The user's own message is placed above this card when it arrives.
  openTurn({ pending: true });
  // The transcript is drawn from the log alone: the user's own words come back as a
  // `message.appended` event, so drawing them here as well would show them twice.
  // The turn runs while this request is open, and its answer is the turn's end —
  // there is no turn-started or turn-finished event on the wire.
  try {
    await sendRequest("prompt.submit", { session_id: state.sessionId, text });
  } catch (e) {
    notice("error", `prompt.submit: ${e.message}`);
  } finally {
    if (asked === state.sessionId) {
      state.running = false;
      renderModels();
      setThinking(false);
      closeTurn();
    }
  }
}

/* ── Go ───────────────────────────────────────────────────────────────── */

loadCollapsed();
wireShortcuts();
connect();
