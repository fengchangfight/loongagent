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
  pending: new Map(),    // id → {resolve, reject}
  token: null,

  workspaces: [],        // from session.tree: each carries its sessions
  seat: 0,               // the workspace this tab sits on
  sessionId: null,       // the session the chat shows
  models: [],            // from model.list
  activeModel: null,     // the lit one

  running: false,        // a turn is in flight
  draft: null,           // the streaming assistant message element
  process: null,         // the open process panel element
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
  form: el("composer-form"),
  status: el("status"),
  modelbar: el("modelbar"),
  modelSelect: el("model-select"),
  sessionList: el("session-list"),
  newSession: el("new-session"),
  workspaceBtn: el("workspace-btn"),
  picker: el("picker"),
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

function timeAgo(epochSeconds) {
  if (!epochSeconds) return "";
  const delta = Date.now() / 1000 - epochSeconds;
  if (delta < 60) return "just now";
  if (delta < 3600) return `${Math.floor(delta / 60)}m ago`;
  if (delta < 86400) return `${Math.floor(delta / 3600)}h ago`;
  return `${Math.floor(delta / 86400)}d ago`;
}

/* ── The socket ───────────────────────────────────────────────────────── */

function connect() {
  const token = (location.hash.match(/token=([0-9a-f]+)/) || [])[1];
  if (!token) {
    setStatus("no token in the URL — open the URL `loongagent web` printed", "bad");
    return;
  }
  state.token = token;
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  const ws = new WebSocket(`${proto}//${location.host}/ws?token=${token}`);
  state.ws = ws;

  ws.onopen = () => {
    setStatus("connected", "ok");
    boot();
  };
  ws.onclose = () => {
    setStatus("the server closed the connection — refresh to retry", "bad");
  };
  ws.onerror = () => {
    setStatus("cannot reach the server — is `loongagent web` still running?", "bad");
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
  } catch (e) {
    notice("error", `boot failed: ${e.message}`);
  }
}

async function refreshSessions() {
  // One call returns the whole two-level tree: every workspace with its sessions,
  // each session flagged live/busy (web-surface.md §2).
  const result = await sendRequest("session.tree", {});
  state.workspaces = result.workspaces || [];
  if (typeof result.seat === "number") state.seat = result.seat;
  renderWorkspace();
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

/* Fold a ModelList result into state and redraw the picker. */
function applyModelList(list) {
  state.models = (list && list.models) || [];
  state.activeModel = state.models.find((m) => m.active)
    ? state.models.find((m) => m.active).name
    : null;
  renderModels();
}

/* ── The banner: workspace ────────────────────────────────────────────── */

function renderWorkspace() {
  const ws = state.workspaces[state.seat];
  if (!ws) return;
  const dir = ws.dir || "";
  const name = dir.split("/").filter(Boolean).pop() || "/";
  dom.workspaceBtn.querySelector(".dir-name").textContent = name;
  dom.workspaceBtn.querySelector(".dir-path").textContent = dir;
  dom.workspaceBtn.title = dir;
}

/* The picker: list directories, open one as a workspace. */
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
      notice("error", body.error || "cannot list that directory");
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
      list.innerHTML = '<div class="empty">No subdirectories here.</div>';
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
      renderWorkspace();
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

/* ── The model picker ─────────────────────────────────────────────────── */

function renderModels() {
  const select = dom.modelSelect;
  const bar = dom.modelbar;
  const hint = bar.querySelector(".hint");
  select.innerHTML = "";

  if (!state.sessionId) {
    select.disabled = true;
    const o = document.createElement("option");
    o.textContent = "no session — start one to pick a model";
    select.appendChild(o);
    hint.textContent = "";
    return;
  }
  if (state.models.length === 0) {
    select.disabled = true;
    const o = document.createElement("option");
    o.textContent = "no models configured";
    select.appendChild(o);
    hint.textContent = "";
    return;
  }

  select.disabled = state.running;
  for (const m of state.models) {
    const o = document.createElement("option");
    o.value = m.name;
    o.textContent = m.context_window
      ? `${m.name} · ctx ${m.context_window}`
      : m.name;
    if (m.active) o.selected = true;
    select.appendChild(o);
  }
  const active = state.models.find((m) => m.active);
  state.activeModel = active ? active.name : null;
  hint.textContent = state.activeModel ? `model: ${state.activeModel}` : "";
}

async function chooseModel(name) {
  if (state.running || !state.sessionId) return;
  try {
    const result = await sendRequest("model.select", { session_id: state.sessionId, name });
    applyModelList(result);
    notice("info", `model → ${name}`);
  } catch (e) {
    notice("error", `model.select: ${e.message}`);
    renderModels(); // snap the select back to the real active model
  }
}

dom.modelSelect.onchange = () => chooseModel(dom.modelSelect.value);

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
    list.innerHTML = '<div class="empty">No workspace yet.</div>';
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
    head.title = (w.dir || "") + (collapsed ? " — click to expand" : " — click to collapse");
    head.onclick = () => toggleCollapsed(w);
    list.appendChild(head);

    if (collapsed) continue;

    // Level two: that workspace's sessions, indented under it.
    if (sessions.length === 0) {
      const empty = document.createElement("div");
      empty.className = "empty nested";
      empty.textContent = w.index === state.seat ? "No sessions — start one with ＋ New." : "No sessions.";
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
        (s.busy ? "running · " : s.running ? "live · " : "") +
        `${s.turns || 0} turns · ${timeAgo(Math.round(spokenMs / 1000))}`;
      row.appendChild(meta);

      const del = document.createElement("span");
      del.className = "delete";
      del.textContent = "✕";
      del.title = "Delete this session (its log is removed for good)";
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

async function newSession() {
  try {
    const result = await sendRequest("session.new", {});
    state.sessionId = result.session_id;
    clearChat();
    await refreshSessions();
    await refreshModels();
    setStatus(`session ${state.sessionId}`, "ok");
  } catch (e) {
    notice("error", `session.new: ${e.message}`);
  }
}

async function openSession(id, workspaceIndex) {
  if (state.running) return;
  try {
    // The surface binds a child to this session (starting one if none is live) and
    // attaches the tab; there is no "current session" to name, so none is sent.
    const params = { open: id };
    if (typeof workspaceIndex === "number") params.workspace = workspaceIndex;
    const result = await sendRequest("session.open", params);
    if (typeof workspaceIndex === "number") state.seat = workspaceIndex;
    state.sessionId = result.session_id;
    clearChat();
    renderWorkspace();
    await refreshSessions();
    await replay(result.session_id);
    await refreshModels();
    setStatus(`session ${state.sessionId}`, "ok");
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
      clearChat();
    }
    await refreshSessions();
  } catch (e) {
    notice("error", `session.delete: ${e.message}`);
  }
}

dom.newSession.onclick = () => newSession();

/* ── The transcript ───────────────────────────────────────────────────── */

function clearChat() {
  dom.messages.innerHTML = "";
  state.draft = null;
  state.process = null;
  resetUsage();
}

function message(who, text) {
  const div = document.createElement("div");
  div.className = `msg ${who}`;
  const label = document.createElement("div");
  label.className = "who";
  label.textContent = who === "user" ? "You" : who === "assistant" ? "loongagent" : who;
  const body = document.createElement("div");
  body.className = "text";
  body.textContent = text;
  div.appendChild(label);
  div.appendChild(body);
  dom.messages.appendChild(div);
  scrollDown();
  return body;
}

/* Replay a session's log into the chat (web-surface.md §3). */
async function replay(id) {
  const result = await sendRequest("session.replay", { session_id: id, from_seq: 0 });
  for (const env of result.events || []) {
    onEvent(env, true);
  }
  scrollDown();
}

/* ── The process panel: the turn's machinery as it happens ──────────── */

function ensureProcessPanel() {
  if (state.process) return state.process;
  const panel = document.createElement("details");
  panel.className = "process";
  panel.open = true;
  const summary = document.createElement("summary");
  summary.innerHTML =
    '<span class="spinner"></span><span class="label">Working…</span><span class="count"></span>';
  const steps = document.createElement("div");
  steps.className = "steps";
  panel.appendChild(summary);
  panel.appendChild(steps);
  dom.messages.appendChild(panel);
  state.process = panel;
  scrollDown();
  return panel;
}

function processStep(kind, badge, headline, detail) {
  const panel = state.process;
  if (!panel) return;
  const step = document.createElement("div");
  step.className = `step ${kind}`;
  const b = document.createElement("span");
  b.className = "badge";
  b.textContent = badge;
  const body = document.createElement("div");
  body.className = "body";
  const h = document.createElement("div");
  h.className = "headline";
  h.textContent = headline;
  body.appendChild(h);
  if (detail) {
    const d = document.createElement("div");
    d.className = "detail";
    d.style.display = "none";
    d.textContent = detail;
    const t = document.createElement("button");
    t.className = "toggle";
    t.textContent = "detail ▸";
    t.onclick = () => {
      const open = d.style.display !== "none";
      d.style.display = open ? "none" : "block";
      t.textContent = open ? "detail ▸" : "detail ▾";
    };
    body.appendChild(t);
    body.appendChild(d);
  }
  step.appendChild(b);
  step.appendChild(body);
  panel.querySelector(".steps").appendChild(step);
  const count = panel.querySelector(".count");
  count.textContent = `${panel.querySelectorAll(".step").length} steps`;
  scrollDown();
  return step;
}

function finishProcessPanel() {
  const panel = state.process;
  if (!panel) return;
  const spinner = panel.querySelector(".spinner");
  if (spinner) spinner.remove();
  const label = panel.querySelector(".label");
  if (label) label.textContent = "Process";
  panel.open = false; // the turn is done; the machinery folds away
  state.process = null;
}

/* ── The draft: the assistant's streaming words ───────────────────────── */

function appendDraftText(delta) {
  if (!delta) return;
  if (!state.draft) {
    // The assistant's words begin after its machinery: the draft message follows
    // the process panel.
    state.draft = message("assistant", "");
  }
  state.draft.textContent += delta;
  scrollDown();
}

/* ── Events: one handler for live and replayed ones ───────────────────── */

function onEvent(env, replayed) {
  const kind = env.kind;
  const data = env.data || {};
  switch (kind) {
    case "message.appended": {
      const role = data.role;
      const text = data.text || "";
      if (role === "user") {
        message("user", text);
      } else if (role === "assistant") {
        if (state.draft && state.draft.textContent === "") {
          state.draft.textContent = text;
          state.draft = null;
        } else if (state.draft && text.startsWith(state.draft.textContent)) {
          // The final text supersedes the streamed draft.
          state.draft.textContent = text;
          state.draft = null;
        } else {
          state.draft = null;
          message("assistant", text);
        }
      }
      break;
    }
    case "tool.called":
      ensureProcessPanel();
      processStep("tool", data.tool || "tool", data.command || data.tool || "", "");
      break;
    case "decision.made":
      ensureProcessPanel();
      processStep("decision", "decision", data.command || "", data.reason || "");
      break;
    case "decision.synthesized":
      ensureProcessPanel();
      processStep("decision", "rule+", data.command || "", data.reason || "");
      break;
    case "tool.result": {
      ensureProcessPanel();
      const lines = typeof data.lines === "number" ? `${data.lines} lines` : "";
      processStep("tool", "result", `${data.tool || "tool"} — ${data.status || "ok"} ${lines}`, "");
      break;
    }
    case "tool.intercepted":
      ensureProcessPanel();
      processStep("tool", "redirect", data.tool || "tool", data.instruction || "");
      break;
    case "approval.requested":
      showApproval(env);
      break;
    case "turn.started":
      if (!replayed) {
        state.running = true;
        renderModels();
        dom.send.disabled = true;
      }
      break;
    case "turn.completed":
      if (!replayed) {
        state.running = false;
        renderModels();
        dom.send.disabled = false;
        finishProcessPanel();
        state.draft = null;
      }
      break;
    case "cost.recorded":
      recordCost(data);
      break;
    case "model.switched":
      state.activeModel = data.model;
      state.models.forEach((m) => (m.active = m.name === data.model));
      if (state.models.length > 0) renderModels();
      else refreshModels();
      break;
    case "session.resumed":
      if (!replayed) notice("info", `session ${data.id || ""} — ${data.events || 0} events`);
      break;
    case "client.error":
      notice("error", data.message || "client error");
      break;
    default:
      break;
  }
}

/* ── Approvals ────────────────────────────────────────────────────────── */

function showApproval(env) {
  const data = env.data || {};
  const box = document.createElement("div");
  box.className = "approval";
  const what = document.createElement("div");
  what.className = "what";
  const label = document.createElement("div");
  label.textContent = data.risk ? `Approval — ${data.risk} risk` : "Approval requested";
  const command = document.createElement("span");
  command.className = "command";
  command.textContent = data.command || "";
  what.appendChild(label);
  if (data.command) what.appendChild(command);

  const approve = document.createElement("button");
  approve.className = "approve";
  approve.textContent = "Approve";
  const refuse = document.createElement("button");
  refuse.className = "refuse";
  refuse.textContent = "Refuse";

  const answer = (granted) => {
    box.classList.add("answered");
    approve.textContent = granted ? "Approved ✓" : "Approve";
    refuse.textContent = granted ? "Refuse" : "Refused ✕";
    sendRequest("approval.respond", {
      session_id: state.sessionId || "default",
      request_id: env.id,
      granted,
      scope: "once",
    }).catch((e) => notice("error", `approval.respond: ${e.message}`));
  };
  approve.onclick = () => answer(true);
  refuse.onclick = () => answer(false);

  box.appendChild(what);
  box.appendChild(approve);
  box.appendChild(refuse);
  dom.messages.appendChild(box);
  scrollDown();
}

/* ── Usage: the TUI's own five numbers, in the browser ────────────────── */

function recordCost(data) {
  const u = state.usage;
  u.calls += 1;
  u.in += data.tokens_in || 0;
  u.out += data.tokens_out || 0;
  u.cached += data.cached_tokens || 0;
  u.cost += data.usd || 0;
  u.ctx = data.context_tokens || u.ctx;
  u.win = data.context_window || u.win;
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

dom.form.onsubmit = (e) => {
  e.preventDefault();
  submit();
};

dom.prompt.onkeydown = (e) => {
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    submit();
  }
};

dom.prompt.oninput = () => {
  dom.prompt.style.height = "auto";
  dom.prompt.style.height = Math.min(dom.prompt.scrollHeight, 180) + "px";
};

async function submit() {
  const text = dom.prompt.value.trim();
  if (!text || state.running) return;
  if (!state.sessionId) {
    await newSession();
    if (!state.sessionId) return;
  }
  dom.prompt.value = "";
  dom.prompt.style.height = "auto";
  state.running = true;
  renderModels();
  dom.send.disabled = true;
  message("user", text);
  ensureProcessPanel();
  try {
    await sendRequest("prompt.submit", { session_id: state.sessionId, text });
  } catch (e) {
    state.running = false;
    renderModels();
    dom.send.disabled = false;
    finishProcessPanel();
    notice("error", `prompt.submit: ${e.message}`);
  }
}

/* ── Go ───────────────────────────────────────────────────────────────── */

loadCollapsed();
connect();
