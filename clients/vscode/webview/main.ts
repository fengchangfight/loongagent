import type { Attachment, BinaryStatus, FileChoice, HostMessage, SessionInfo, TabState, UiPart } from "../src/webviewProtocol.ts";
import { insertMention, matchingFiles, mentionAt } from "../src/mention.ts";

type Models = Extract<HostMessage, { type: "bootstrap" }>["models"];

declare function acquireVsCodeApi(): {
  postMessage(message: unknown): void;
  getState(): unknown;
  setState(state: unknown): void;
};

const api = acquireVsCodeApi();
const root = document.getElementById("app");
if (root === null) throw new Error("the sidebar has no #app element");

const shell = document.createElement("div");
shell.className = "shell";
const header = document.createElement("header");
header.className = "header";
const tabbar = document.createElement("div");
tabbar.className = "tabbar";
const brand = document.createElement("span");
brand.className = "brand";
brand.textContent = "loongagent";
const status = document.createElement("span");
status.className = "status";
const modelSelect = document.createElement("select");
modelSelect.className = "model";
modelSelect.setAttribute("aria-label", "Model for this session");
const stop = document.createElement("button");
stop.className = "stop";
stop.textContent = "Stop";
stop.setAttribute("aria-label", "Stop the running turn");
stop.disabled = true;
header.append(brand, status, modelSelect, stop);

const transcript = document.createElement("div");
transcript.className = "transcript";
transcript.setAttribute("role", "log");
transcript.setAttribute("aria-live", "polite");

const historyOverlay = document.createElement("div");
historyOverlay.className = "history-overlay";
historyOverlay.hidden = true;
const composer = document.createElement("footer");
composer.className = "composer";
const attachmentsBar = document.createElement("div");
attachmentsBar.className = "attachments";
const mentionBox = document.createElement("div");
mentionBox.className = "mention";
mentionBox.hidden = true;
mentionBox.setAttribute("role", "listbox");
const input = document.createElement("textarea");
input.className = "input";
input.rows = 3;
input.placeholder = "Ask loongagent…";
input.setAttribute("aria-label", "Message loongagent");
const controls = document.createElement("div");
controls.className = "controls";
const send = document.createElement("button");
send.className = "send";
send.textContent = "Send";
send.setAttribute("aria-label", "Send the message");
const hint = document.createElement("span");
hint.className = "hint";
hint.textContent = "Type @ to attach a file · Enter sends · Shift+Enter newline";
controls.append(hint, send);
composer.append(attachmentsBar, mentionBox, input, controls);
shell.append(header, tabbar, transcript, composer, historyOverlay);
root.append(shell);

interface State {
  parts: UiPart[];
  live: string;
  tabs: TabState[];
  activeTabId?: string;
  sessions: SessionInfo[];
  historyOpen: boolean;
  binary?: BinaryStatus;
  models: Models;
  model?: string;
  attachments: Attachment[];
  selection?: Attachment;
  files: FileChoice[];
  filesLoaded: boolean;
  mention?: { start: number; query: string; index: number };
  busy: boolean;
  yolo: boolean;
}

const state: State = {
  parts: [], live: "", tabs: [], activeTabId: undefined, sessions: [], historyOpen: false,
  models: [], attachments: [], selection: undefined, files: [],
  filesLoaded: false, mention: undefined, busy: false, yolo: false,
};

const cachedParts = new Map<string, UiPart[]>();
const cachedLive = new Map<string, string>();

function partsFor(tabId: string): UiPart[] {
  const existing = cachedParts.get(tabId);
  if (existing !== undefined) return existing;
  const created: UiPart[] = [];
  cachedParts.set(tabId, created);
  return created;
}

function renderTabs(): void {
  tabbar.replaceChildren();
  for (const tab of state.tabs) {
    const button = element("button", `tab${tab.id === state.activeTabId ? " active" : ""}`);
    button.setAttribute("type", "button");
    button.setAttribute("role", "tab");
    button.setAttribute("aria-selected", tab.id === state.activeTabId ? "true" : "false");
    if (tab.status === "running" || tab.status === "waiting") {
      button.append(element("span", `tab-dot ${tab.status}`));
    }
    button.append(element("span", "tab-title", tab.title));
    button.addEventListener("click", () => {
      if (tab.id === state.activeTabId) return;
      state.activeTabId = tab.id;
      state.parts = partsFor(tab.id);
      state.live = cachedLive.get(tab.id) ?? "";
      state.busy = tab.status === "running" || tab.status === "waiting";
      setBusy(state.busy);
      renderTabs();
      render();
      input.focus();
      api.postMessage({ type: "selectTab", tabId: tab.id });
    });
    const close = element("button", "tab-close", "×");
    close.setAttribute("type", "button");
    close.setAttribute("aria-label", `Close ${tab.title}`);
    close.addEventListener("click", (event) => {
      event.stopPropagation();
      api.postMessage({ type: "closeTab", tabId: tab.id });
    });
    button.append(close);
    tabbar.append(button);
  }
  const add = element("button", "tab-action", "+");
  add.setAttribute("type", "button");
  add.setAttribute("aria-label", "New tab");
  add.addEventListener("click", () => api.postMessage({ type: "newTab" }));
  const history = element("button", "tab-action", "History");
  history.setAttribute("type", "button");
  history.addEventListener("click", () => {
    state.historyOpen = true;
    renderHistory();
    api.postMessage({ type: "history" });
  });
  tabbar.append(add, history);
}

historyOverlay.addEventListener("click", (event) => {
  if (event.target !== historyOverlay) return;
  state.historyOpen = false;
  renderHistory();
});

function renderHistory(): void {
  historyOverlay.hidden = !state.historyOpen;
  historyOverlay.replaceChildren();
  if (!state.historyOpen) return;
  const card = element("div", "history-card");
  const head = element("div", "history-head");
  head.append(element("strong", "", "Sessions"));
  const close = element("button", "tab-action", "Close");
  close.setAttribute("type", "button");
  close.addEventListener("click", () => { state.historyOpen = false; renderHistory(); });
  head.append(close);
  card.append(head);
  if (state.sessions.length === 0) {
    card.append(element("div", "history-empty", "No sessions yet."));
  }
  for (const session of state.sessions) {
    const row = element("button", "history-row");
    row.setAttribute("type", "button");
    row.append(element("span", "history-title", session.title));
    row.append(element("span", "history-meta",
      `${new Date(session.updated_at * 1000).toLocaleString()} · ${session.turns} turns`));
    row.addEventListener("click", () => {
      state.historyOpen = false;
      renderHistory();
      api.postMessage({ type: "openSession", sessionId: session.id });
    });
    card.append(row);
  }
  historyOverlay.append(card);
}

function upsertInto(parts: UiPart[], part: UiPart): void {
  if (part.kind === "tool") {
    const index = parts.findIndex((entry) => entry.kind === "tool" && entry.callId === part.callId);
    if (index >= 0) { parts[index] = part; return; }
  }
  if (part.kind === "approval") {
    const index = parts.findIndex((entry) =>
      entry.kind === "approval" && entry.requestId === part.requestId);
    if (index >= 0) { parts[index] = part; return; }
  }
  parts.push(part);
}

function element(tag: string, className: string, text?: string): HTMLElement {
  const node = document.createElement(tag);
  node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function activeTab(): TabState | undefined {
  return state.tabs.find((tab) => tab.id === state.activeTabId);
}

function setBusy(value: boolean): void {
  state.busy = value;
  stop.disabled = !value;
  send.disabled = value;
  modelSelect.disabled = value || state.models.length === 0;
  renderStatus();
}

function renderModels(): void {
  modelSelect.replaceChildren();
  if (state.models.length === 0) {
    const option = document.createElement("option");
    option.textContent = "No models";
    option.value = "";
    modelSelect.append(option);
    modelSelect.disabled = true;
    return;
  }
  for (const offering of state.models) {
    const option = document.createElement("option");
    option.value = offering.name;
    option.textContent = offering.name;
    option.selected = offering.name === state.model;
    modelSelect.append(option);
  }
  modelSelect.disabled = state.busy;
}

function renderAttachments(): void {
  attachmentsBar.replaceChildren();
  if (state.selection !== undefined && state.selection.kind === "selection") {
    const chip = element("span", "chip selection");
    const label = `selection · ${state.selection.path}:${state.selection.startLine}-${state.selection.endLine}`;
    chip.append(element("span", "chip-label", label));
    chip.title = "From the active editor selection; deselect to remove";
    attachmentsBar.append(chip);
  }
  for (const attachment of state.attachments) {
    const chip = element("span", "chip");
    const label = attachment.kind === "file"
      ? attachment.path
      : `${attachment.path}:${attachment.startLine}-${attachment.endLine}`;
    chip.append(element("span", "chip-label", label));
    const remove = element("button", "chip-remove", "×");
    remove.setAttribute("aria-label", `Remove attachment ${label}`);
    remove.addEventListener("click", () => {
      if (state.activeTabId === undefined) return;
      api.postMessage({ type: "removeAttachment", tabId: state.activeTabId, id: attachment.id });
    });
    chip.append(remove);
    attachmentsBar.append(chip);
  }
}

function mentionMatches(): FileChoice[] {
  return state.mention === undefined ? [] : matchingFiles(state.files, state.mention.query);
}

function renderMention(): void {
  const mention = state.mention;
  if (mention === undefined) {
    mentionBox.hidden = true;
    mentionBox.replaceChildren();
    return;
  }
  const matches = mentionMatches();
  if (matches.length === 0) mention.index = 0;
  else if (mention.index >= matches.length) mention.index = matches.length - 1;
  mentionBox.hidden = false;
  mentionBox.replaceChildren();
  if (matches.length === 0) {
    mentionBox.append(element("div", "mention-empty",
      state.filesLoaded ? "No matching files" : "Loading workspace files…"));
    return;
  }
  matches.forEach((file, index) => {
    const item = element("button", "mention-item", file.label);
    item.setAttribute("type", "button");
    item.setAttribute("role", "option");
    item.setAttribute("aria-selected", index === mention.index ? "true" : "false");
    if (index === mention.index) item.classList.add("active");
    item.addEventListener("mousedown", (event) => {
      event.preventDefault();
      chooseMention(file);
    });
    mentionBox.append(item);
  });
}

function openMention(start: number, query: string): void {
  state.mention = { start, query, index: 0 };
  renderMention();
}

function closeMention(): void {
  state.mention = undefined;
  renderMention();
}

function chooseMention(file: FileChoice): void {
  const mention = state.mention;
  if (mention === undefined) return;
  const inserted = insertMention(input.value, mention.start, mention.query, file.label);
  input.value = inserted.value;
  input.setSelectionRange(inserted.caret, inserted.caret);
  closeMention();
  if (state.activeTabId !== undefined) {
    api.postMessage({ type: "attachFile", tabId: state.activeTabId, path: file.path });
  }
  input.focus();
}

function renderStatus(): void {
  const tab = activeTab();
  status.textContent = state.busy
    ? tab?.status === "waiting" ? "waiting for approval…" : "running…"
    : tab?.status === "stopped" ? "stopped" : "idle";
  status.className = `status ${state.busy ? "busy" : ""}`;
}

function renderEmpty(): void {
  const empty = element("div", "empty");
  empty.append(element("h1", "empty-title", "loongagent"));
  empty.append(element("p", "empty-copy",
    "Ask for a change in this workspace. Files you attach and the editor selection become part of the prompt."));
  if (state.busy) {
    empty.append(element("p", "empty-meta", "Thinking…"));
  } else if (state.binary === undefined) {
    empty.append(element("p", "empty-warn",
      "The loongagent binary was not found. Set loongagent.path or run \"loongagent: Locate the binary\"."));
  } else {
    empty.append(element("p", "empty-meta",
      `${state.binary.path} · version ${state.binary.version} · schema ${state.binary.schema}`));
  }
  transcript.append(empty);
}

function renderPart(part: UiPart): HTMLElement | undefined {
  switch (part.kind) {
    case "user": {
      const row = element("div", "message user");
      row.append(element("div", "label", "you"), element("div", "body", part.text));
      return row;
    }
    case "assistant": {
      const row = element("div", "message assistant");
      row.append(element("div", "label", "loongagent"));
      const body = element("div", "body markdown");
      body.innerHTML = part.html;
      row.append(body);
      return row;
    }
    case "progress":
      return element("div", "progress", part.text);
    case "notice": {
      const row = element("div", `notice ${part.level}`);
      row.textContent = part.text;
      return row;
    }
    case "tool": {
      const card = element("div", "card tool");
      const title = element("div", "card-title",
        `${part.tool} (${part.actionClass}) ${part.status}`);
      card.append(title);
      if (part.paths.length > 0) card.append(element("div", "paths", part.paths.join(" · ")));
      if (part.output !== undefined) {
        const output = element("pre", "output", part.output);
        card.append(output);
        if (part.truncated !== undefined) {
          card.append(element("div", "truncated", `truncated ${part.truncated} characters`));
        }
      }
      return card;
    }
    case "approval": {
      const card = element("div", `card approval ${part.state}`);
      card.append(element("div", "card-title",
        `approval — ${part.tool ?? part.actionClass}${part.state === "pending" ? "" : ` (${part.state})`}`));
      if (part.paths.length > 0) card.append(element("div", "paths", part.paths.join(" · ")));
      card.append(element("div", "reason", part.reason));
      if (part.diff !== undefined) {
        card.append(element("div", "paths",
          `${part.diff.added} added · ${part.diff.removed} removed${part.diff.approximate ? " (approximate)" : ""}${part.diff.truncated ? " · truncated" : ""}`));
        card.append(element("pre", "diff", part.diff.unified));
      }
      const actions = element("div", "actions");
      if (part.diff !== undefined && part.paths[0] !== undefined) {
        const open = element("button", "secondary", "Open full diff");
        open.addEventListener("click", () => {
          if (state.activeTabId !== undefined) {
            api.postMessage({ type: "openDiff", tabId: state.activeTabId,
              token: part.requestId, path: part.paths[0] });
          }
        });
        actions.append(open);
      }
      if (part.state === "pending") {
        const answer = (choice: "once" | "always" | "deny") => {
          if (state.activeTabId === undefined) return;
          api.postMessage({ type: "approval", tabId: state.activeTabId,
            requestId: part.requestId, choice });
          actions.querySelectorAll("button").forEach((button) => { button.disabled = true; });
        };
        const once = element("button", "yes", "Allow once");
        once.addEventListener("click", () => answer("once"));
        const always = element("button", "yes", "Allow always");
        always.addEventListener("click", () => answer("always"));
        const deny = element("button", "no", "Deny");
        deny.addEventListener("click", () => answer("deny"));
        actions.append(once, always, deny);
      }
      if (actions.childElementCount > 0) card.append(actions);
      return card;
    }
    case "review": {
      const card = element("div", "card review");
      card.append(element("div", "card-title", `Review ${part.paths.length} changed files`));
      card.append(element("div", "paths", part.paths.join(" · ")));
      const open = element("button", "secondary", "Open diffs");
      open.addEventListener("click", () => {
        if (state.activeTabId !== undefined) {
          api.postMessage({ type: "review", tabId: state.activeTabId,
            token: part.token, paths: part.paths });
        }
      });
      card.append(open);
      return card;
    }
  }
}

function render(): void {
  transcript.replaceChildren();
  if (state.parts.length === 0 && state.live === "") {
    renderEmpty();
  } else {
    for (const part of state.parts) {
      const node = renderPart(part);
      if (node !== undefined) transcript.append(node);
    }
    if (state.live !== "") {
      const live = element("div", "message assistant live");
      live.append(element("div", "label", "loongagent"), element("div", "body", state.live));
      transcript.append(live);
    }
  }
  renderStatus();
  transcript.scrollTop = transcript.scrollHeight;
}


function submit(): void {
  const text = input.value.trim();
  if (text === "" || state.busy || state.activeTabId === undefined) return;
  if (text.startsWith("/")) {
    const command = text.slice(1).split(/\s+/)[0];
    if (command === "new") {
      api.postMessage({ type: "newTab" });
      input.value = "";
      return;
    }
    if (command === "model") {
      modelSelect.focus();
      input.value = "";
      return;
    }
    if (command === "context") {
      api.postMessage({ type: "context", tabId: state.activeTabId });
      input.value = "";
      return;
    }
    if (command === "yolo") {
      state.yolo = !state.yolo;
      api.postMessage({ type: "yolo", tabId: state.activeTabId, on: state.yolo });
      input.value = "";
      return;
    }
  }
  closeMention();
  state.live = "";
  api.postMessage({ type: "send", tabId: state.activeTabId, text });
  input.value = "";
  setBusy(true);
  render();
}

stop.addEventListener("click", () => {
  if (state.activeTabId === undefined) return;
  state.live = "";
  api.postMessage({ type: "cancel", tabId: state.activeTabId });
  render();
});
send.addEventListener("click", submit);
modelSelect.addEventListener("change", () => {
  if (state.activeTabId === undefined || state.busy || modelSelect.value === "") return;
  api.postMessage({ type: "selectModel", tabId: state.activeTabId, name: modelSelect.value });
});
input.addEventListener("input", () => {
  const found = mentionAt(input.value, input.selectionStart ?? 0);
  if (found === undefined) closeMention();
  else openMention(found.start, found.query);
});
input.addEventListener("keydown", (event) => {
  if (state.mention !== undefined) {
    const matches = mentionMatches();
    if (event.key === "ArrowDown") {
      event.preventDefault();
      if (matches.length > 0) {
        state.mention.index = (state.mention.index + 1) % matches.length;
        renderMention();
      }
      return;
    }
    if (event.key === "ArrowUp") {
      event.preventDefault();
      if (matches.length > 0) {
        state.mention.index = (state.mention.index - 1 + matches.length) % matches.length;
        renderMention();
      }
      return;
    }
    if ((event.key === "Enter" || event.key === "Tab") && matches.length > 0) {
      event.preventDefault();
      chooseMention(matches[state.mention.index] ?? matches[0]);
      return;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      closeMention();
      return;
    }
  }
  if (event.key === "Enter" && !event.shiftKey) {
    event.preventDefault();
    submit();
  }
});
document.addEventListener("mousedown", (event) => {
  if (state.mention === undefined) return;
  const target = event.target as Node | null;
  if (target === null || target === input || mentionBox.contains(target)) return;
  closeMention();
});

window.addEventListener("message", (event: MessageEvent<HostMessage>) => {
  const message = event.data;
  switch (message.type) {
    case "bootstrap":
      state.tabs = message.tabs;
      state.activeTabId = message.activeTabId ?? message.tabs[0]?.id;
      if (state.activeTabId !== undefined) {
        cachedParts.set(state.activeTabId, message.activeTranscript);
      }
      state.parts = message.activeTranscript;
      state.binary = message.binary;
      state.models = message.models;
      state.model = message.model;
      state.selection = message.selection;
      // The file list is host state. A second bootstrap (models ready) must
      // not erase a list the earlier `files` message already delivered.
      state.mention = undefined;
      state.live = state.activeTabId === undefined
        ? ""
        : cachedLive.get(state.activeTabId) ?? "";
      state.busy = activeTab()?.status === "running" || activeTab()?.status === "waiting";
      setBusy(state.busy);
      renderModels();
      renderAttachments();
      renderMention();
      renderTabs();
      render();
      break;
    case "tabs":
      state.tabs = message.tabs;
      if (state.activeTabId === undefined || !state.tabs.some((tab) => tab.id === state.activeTabId)) {
        state.activeTabId = state.tabs[0]?.id;
        state.parts = state.activeTabId === undefined ? [] : partsFor(state.activeTabId);
      }
      state.busy = activeTab()?.status === "running" || activeTab()?.status === "waiting";
      if (!state.busy) state.live = "";
      setBusy(state.busy);
      renderTabs();
      render();
      break;
    case "delta":
      cachedLive.set(message.tabId, (cachedLive.get(message.tabId) ?? "") + message.text);
      if (message.tabId !== state.activeTabId) break;
      state.live = cachedLive.get(message.tabId) ?? "";
      render();
      break;
    case "part": {
      const parts = partsFor(message.tabId);
      upsertInto(parts, message.part);
      if (message.tabId !== state.activeTabId) break;
      state.parts = parts;
      render();
      break;
    }
    case "assistantFinal": {
      const parts = partsFor(message.tabId);
      cachedLive.set(message.tabId, "");
      parts.push({ kind: "assistant", text: message.text, html: message.html });
      if (message.tabId !== state.activeTabId) break;
      state.parts = parts;
      state.live = "";
      render();
      break;
    }
    case "error": {
      const text = message.detail === undefined ? message.message : `${message.message}\n${message.detail}`;
      const tabId = message.tabId ?? state.activeTabId;
      if (message.tabId !== undefined && message.tabId !== state.activeTabId) {
        if (tabId !== undefined) {
          partsFor(tabId).push({ kind: "notice", level: "error", text });
        }
        break;
      }
      state.live = "";
      state.parts.push({ kind: "notice", level: "error", text });
      setBusy(false);
      render();
      break;
    }
    case "transcript":
      cachedParts.set(message.tabId, message.parts);
      if (message.tabId !== state.activeTabId) break;
      state.parts = message.parts;
      render();
      break;
    case "attachments":
      if (message.tabId !== state.activeTabId) break;
      state.attachments = message.items;
      renderAttachments();
      break;
    case "selection":
      state.selection = message.selection;
      renderAttachments();
      break;
    case "files":
      state.files = message.files;
      state.filesLoaded = true;
      if (state.mention !== undefined) renderMention();
      break;
    case "sessions":
      state.sessions = message.sessions;
      state.historyOpen = true;
      renderHistory();
      break;
    default:
      break;
  }
});

api.postMessage({ type: "ready" });
render();
