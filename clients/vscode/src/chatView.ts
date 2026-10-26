import * as vscode from "vscode";
import { refusalToStart } from "./guards.ts";
import { promptText } from "./prompt.ts";
import { Renderer } from "./render.ts";
import { SessionAgent } from "./sessionAgent.ts";
import { TabManager } from "./tab.ts";
import { Transcript } from "./transcript.ts";
import { TurnBusyError } from "./turn.ts";
import { parseWebviewMessage } from "./webviewProtocol.ts";
import type { BinaryStatus, HostMessage, UiPart } from "./webviewProtocol.ts";
import type { SessionAgent as Agent } from "./sessionAgent.ts";

export type HostBinary = BinaryStatus;

/**
 * The sidebar's Webview provider: tabs, one session agent, one transcript.
 *
 * The host owns every fact the view needs; the Webview only renders `UiPart`s
 * and sends intents. A turn's streamed fragments are not stored as parts; the
 * final `message.appended` is the record that goes into the transcript.
 */
export class ChatViewProvider implements vscode.WebviewViewProvider {
  static readonly viewType = "loongagent.chat";

  private readonly context: vscode.ExtensionContext;
  private readonly output: vscode.OutputChannel;
  private readonly tabs = new TabManager();
  private readonly transcript = new Transcript();
  private view: vscode.WebviewView | undefined;
  private binary: HostBinary | undefined;
  private agent: Agent | undefined;
  private running = false;

  constructor(options: {
    context: vscode.ExtensionContext;
    output: vscode.OutputChannel;
    binary?: HostBinary;
  }) {
    this.context = options.context;
    this.output = options.output;
    this.binary = options.binary;
    this.tabs.open({ id: "tab-1", title: "New session", status: "idle" });
  }

  resolveWebviewView(view: vscode.WebviewView): void {
    this.view = view;
    view.webview.options = {
      enableScripts: true,
      localResourceRoots: [
        this.context.extensionUri,
        vscode.Uri.joinPath(this.context.extensionUri, "dist"),
        vscode.Uri.joinPath(this.context.extensionUri, "webview"),
        vscode.Uri.joinPath(this.context.extensionUri, "media"),
      ],
    };
    view.webview.html = this.html(view.webview);
    view.webview.onDidReceiveMessage((raw) => this.handle(raw));
    view.onDidDispose(() => {
      this.view = undefined;
    });
  }

  setBinary(binary: HostBinary): void {
    this.binary = binary;
    this.postBootstrap();
  }

  post(message: HostMessage): void {
    void this.view?.webview.postMessage(message);
  }

  dispose(): void {
    this.agent?.dispose();
    this.agent = undefined;
    this.view = undefined;
  }

  private handle(raw: unknown): void {
    const message = parseWebviewMessage(raw);
    if (message === undefined) return;
    switch (message.type) {
      case "ready":
        this.postBootstrap();
        break;
      case "send":
        void this.send(message.text);
        break;
      case "cancel":
        this.cancel();
        break;
      default:
        this.output.appendLine(`webview: ${message.type} arrives with the next task`);
    }
  }

  private postBootstrap(): void {
    this.post({
      type: "bootstrap",
      tabs: [...this.tabs.tabs].map((tab) => ({ ...tab })),
      ...(this.tabs.activeId === undefined ? {} : { activeTabId: this.tabs.activeId }),
      activeTranscript: this.transcript.all(),
      models: [],
      ...(this.agent?.sessionId === undefined ? {} : { sessionId: this.agent.sessionId }),
      ...(this.binary === undefined ? {} : { binary: this.binary }),
    });
  }

  private postTabs(): void {
    this.post({ type: "tabs", tabs: [...this.tabs.tabs].map((tab) => ({ ...tab })) });
  }

  private async send(text: string): Promise<void> {
    if (this.running) {
      this.postError("A turn is already running. Wait for it to finish, or press Stop.");
      return;
    }
    const tab = this.tabs.active();
    if (tab === undefined) {
      this.postError("Open a folder and start a chat first.");
      return;
    }
    const folder = vscode.workspace.workspaceFolders?.[0];
    const refusal = refusalToStart({
      hasFolder: folder !== undefined,
      isTrusted: vscode.workspace.isTrusted,
    });
    if (refusal !== undefined || folder === undefined) {
      this.postError(refusal ?? "Open a folder first — the agent works on a workspace.");
      return;
    }
    if (this.binary === undefined) {
      this.postError("The loongagent binary was not found. Set loongagent.path or run \"loongagent: Locate the binary\".");
      return;
    }

    this.agent ??= new SessionAgent({
      folder,
      output: this.output,
      context: this.context,
      binaryPath: this.binary.path,
    });
    const renderer = new Renderer({ cwd: folder.uri.fsPath });
    renderer.beginTurn();
    let lastAssistant: { text: string; html: string } | undefined;
    let failed = false;

    let controller;
    try {
      controller = await this.agent.beginTurn({
        onEvent: (event) => {
          for (const part of renderer.event(event)) {
            if (part.kind === "assistant") {
              lastAssistant = { text: part.text, html: part.html };
              continue;
            }
            if (part.kind === "tool" && part.status !== "running") {
              const exists = this.transcript.all().some((entry) =>
                entry.kind === "tool" && entry.callId === part.callId);
              if (exists) this.transcript.updateTool(part.callId, part);
              else this.transcript.append(part);
            } else {
              this.transcript.append(part);
            }
            this.post({ type: "part", tabId: tab.id, part });
          }
        },
        onDelta: (delta) => this.post({ type: "delta", tabId: tab.id, text: delta }),
        decide: async (asked) => {
          this.output.appendLine(`awaiting approval: ${asked.action_class} — ${asked.reason}`);
          return {
            granted: false,
            scope: "once",
            reason: "approvals arrive in the next task",
          };
        },
      });
    } catch (error) {
      if (error instanceof TurnBusyError) {
        this.postError("A turn is already running in this session.");
        return;
      }
      this.postError(startupAdvice(explain(error)));
      this.output.appendLine(`failed: ${explain(error)}`);
      return;
    }

    this.running = true;
    this.tabs.update(tab.id, { status: "running" });
    if (tab.title === "New session") {
      this.tabs.update(tab.id, { title: titleFrom(text) });
    }
    const user: UiPart = { kind: "user", text };
    this.transcript.append(user);
    this.post({ type: "part", tabId: tab.id, part: user });
    this.postTabs();

    try {
      await controller.run(promptText(text, []));
      if (lastAssistant !== undefined) {
        const assistant: UiPart = { kind: "assistant", ...lastAssistant };
        this.transcript.append(assistant);
        this.post({ type: "assistantFinal", tabId: tab.id, ...lastAssistant });
      }
    } catch (error) {
      failed = true;
      const message = `The turn failed: ${explain(error)} See the **loongagent** output channel.`;
      const notice: UiPart = { kind: "notice", level: "error", text: message };
      this.transcript.append(notice);
      this.post({ type: "part", tabId: tab.id, part: notice });
      this.output.appendLine(`failed: ${explain(error)}`);
    } finally {
      this.running = false;
      this.tabs.update(tab.id, { status: failed ? "stopped" : "idle" });
      this.agent?.endTurn();
      this.postTabs();
    }
  }

  private cancel(): void {
    if (!this.running) return;
    this.output.appendLine("turn running: the person asked to stop");
    this.agent?.cancel();
  }

  private postError(message: string): void {
    this.post({ type: "error", message });
  }

  private html(webview: vscode.Webview): string {
    const script = webview.asWebviewUri(
      vscode.Uri.joinPath(this.context.extensionUri, "dist", "webview.js"),
    );
    const style = webview.asWebviewUri(
      vscode.Uri.joinPath(this.context.extensionUri, "webview", "style.css"),
    );
    const icon = webview.asWebviewUri(
      vscode.Uri.joinPath(this.context.extensionUri, "media", "loong-128.png"),
    );
    const csp = [
      "default-src 'none'",
      `img-src ${webview.cspSource} data:`,
      `style-src ${webview.cspSource}`,
      `script-src ${webview.cspSource}`,
    ].join("; ");
    return `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="${csp}">
<meta name="viewport" content="width=device-width, initial-scale=1">
<link rel="icon" type="image/png" href="${icon}">
<link rel="stylesheet" href="${style}">
</head>
<body>
<div id="app"></div>
<script src="${script}"></script>
</body>
</html>`;
  }
}

function titleFrom(text: string): string {
  const collapsed = text.trim().replace(/\s+/g, " ");
  return collapsed.length <= 60 ? collapsed : `${collapsed.slice(0, 60)}…`;
}

function startupAdvice(message: string): string {
  if (/ENOENT|not found/i.test(message)) {
    return `I could not find the loongagent binary (${message}). Set \`loongagent.path\` to it.`;
  }
  return `I could not start loongagent: ${message}\n\nSee the **loongagent** output channel.`;
}

function explain(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error);
}
