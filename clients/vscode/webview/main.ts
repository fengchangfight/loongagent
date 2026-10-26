import type { HostMessage } from "../src/webviewProtocol.ts";

declare function acquireVsCodeApi(): {
  postMessage(message: unknown): void;
  getState(): unknown;
  setState(state: unknown): void;
};

const api = acquireVsCodeApi();
const app = document.getElementById("app");
if (app === null) throw new Error("the sidebar has no #app element");

function element(tag: string, className: string, text?: string): HTMLElement {
  const node = document.createElement(tag);
  node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function renderBootstrap(message: Extract<HostMessage, { type: "bootstrap" }>): void {
  if (app === null) return;
  app.replaceChildren();
  const empty = element("div", "empty");
  empty.append(element("h1", "empty-title", "loongagent"));
  empty.append(element("p", "empty-copy",
    "Ask for a change in this workspace. Files and selections you attach become part of the prompt."));
  if (message.binary === undefined) {
    empty.append(element("p", "empty-warn",
      "The loongagent binary was not found. Set loongagent.path or run \"loongagent: Locate the binary\"."));
  } else {
    empty.append(element("p", "empty-meta",
      `${message.binary.path} · version ${message.binary.version} · schema ${message.binary.schema}`));
  }
  app.append(empty);
}

window.addEventListener("message", (event: MessageEvent<HostMessage>) => {
  const message = event.data;
  if (message.type === "bootstrap") renderBootstrap(message);
});

api.postMessage({ type: "ready" });
