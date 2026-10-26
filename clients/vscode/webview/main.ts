import type { Attachment, BinaryStatus, HostMessage, TabState, UiPart } from "../src/webviewProtocol.ts";

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

const composer = document.createElement("footer");
composer.className = "composer";
const attachmentsBar = document.createElement("div");
attachmentsBar.className = "attachments";
const input = document.createElement("textarea");
input.className = "input";
input.rows = 3;
input.placeholder = "Ask loongagent…";
input.setAttribute("aria-label", "Message loongagent");
const controls = document.createElement("div");
controls.className = "controls";
const fileButton = document.createElement("button");
fileButton.className = "secondary";
fileButton.textContent = "@ file";
fileButton.setAttribute("aria-label", "Attach a workspace file");
const selectionButton = document.createElement("button");
selectionButton.className = "secondary";
selectionButton.textContent = "selection";
selectionButton.setAttribute("aria-label", "Attach the editor selection");
const send = document.createElement("button");
send.className = "send";
send.textContent = "Send";
send.setAttribute("aria-label", "Send the message");
const hint = document.createElement("span");
hint.className = "hint";
hint.textContent = "Enter sends · Shift+Enter newline";
controls.append(fileButton, selectionButton, hint, send);
composer.append(attachmentsBar, input, controls);
shell.append(header, transcript, composer);
root.append(shell);

interface State {
  parts: UiPart[];
  live: string;
  tabs: TabState[];
  activeTabId?: string;
  binary?: BinaryStatus;
  models: Models;
  model?: string;
  attachments: Attachment[];
  busy: boolean;
  yolo: boolean;
}

const state: State = {
  parts: [], live: "", tabs: [], models: [], attachments: [], busy: false, yolo: false,
};

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
    "Ask for a change in this workspace. Files and selections you attach become part of the prompt."));
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
        `approval — ${part.actionClass}${part.state === "pending" ? "" : ` (${part.state})`}`));
      card.append(element("div", "reason", part.reason));
      if (part.diff !== undefined) {
        card.append(element("div", "paths",
          `${part.diff.added} added · ${part.diff.removed} removed${part.diff.truncated ? " · truncated" : ""}`));
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

function upsert(part: UiPart): void {
  if (part.kind === "tool") {
    const index = state.parts.findIndex((entry) => entry.kind === "tool" && entry.callId === part.callId);
    if (index >= 0) {
      state.parts[index] = part;
      return;
    }
  }
  if (part.kind === "approval") {
    const index = state.parts.findIndex((entry) =>
      entry.kind === "approval" && entry.requestId === part.requestId);
    if (index >= 0) {
      state.parts[index] = part;
      return;
    }
  }
  state.parts.push(part);
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
  api.postMessage({ type: "send", tabId: state.activeTabId, text });
  input.value = "";
  setBusy(true);
  render();
}

stop.addEventListener("click", () => {
  if (state.activeTabId === undefined) return;
  api.postMessage({ type: "cancel", tabId: state.activeTabId });
});
send.addEventListener("click", submit);
fileButton.addEventListener("click", () => {
  if (state.activeTabId === undefined) return;
  api.postMessage({ type: "pickFile", tabId: state.activeTabId });
});
selectionButton.addEventListener("click", () => {
  if (state.activeTabId === undefined) return;
  api.postMessage({ type: "pickSelection", tabId: state.activeTabId });
});
modelSelect.addEventListener("change", () => {
  if (state.activeTabId === undefined || state.busy || modelSelect.value === "") return;
  api.postMessage({ type: "selectModel", tabId: state.activeTabId, name: modelSelect.value });
});
input.addEventListener("keydown", (event) => {
  if (event.key === "Enter" && !event.shiftKey) {
    event.preventDefault();
    submit();
  }
});

window.addEventListener("message", (event: MessageEvent<HostMessage>) => {
  const message = event.data;
  switch (message.type) {
    case "bootstrap":
      state.tabs = message.tabs;
      state.activeTabId = message.activeTabId ?? message.tabs[0]?.id;
      state.parts = message.activeTranscript;
      state.binary = message.binary;
      state.models = message.models;
      state.model = message.model;
      state.live = "";
      state.busy = activeTab()?.status === "running" || activeTab()?.status === "waiting";
      setBusy(state.busy);
      renderModels();
      renderAttachments();
      render();
      break;
    case "tabs":
      state.tabs = message.tabs;
      state.busy = activeTab()?.status === "running" || activeTab()?.status === "waiting";
      setBusy(state.busy);
      render();
      break;
    case "delta":
      if (message.tabId !== state.activeTabId) break;
      state.live += message.text;
      render();
      break;
    case "part":
      if (message.tabId !== state.activeTabId) break;
      upsert(message.part);
      render();
      break;
    case "assistantFinal":
      if (message.tabId !== state.activeTabId) break;
      state.live = "";
      state.parts.push({ kind: "assistant", text: message.text, html: message.html });
      render();
      break;
    case "error": {
      if (message.tabId !== undefined && message.tabId !== state.activeTabId) break;
      const text = message.detail === undefined ? message.message : `${message.message}\n${message.detail}`;
      state.parts.push({ kind: "notice", level: "error", text });
      setBusy(false);
      render();
      break;
    }
    case "transcript":
      if (message.tabId !== state.activeTabId) break;
      state.parts = message.parts;
      render();
      break;
    case "attachments":
      if (message.tabId !== state.activeTabId) break;
      state.attachments = message.items;
      renderAttachments();
      break;
    default:
      break;
  }
});

api.postMessage({ type: "ready" });
render();
