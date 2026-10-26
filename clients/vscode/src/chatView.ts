import * as vscode from "vscode";
import { TabManager } from "./tab.ts";
import { parseWebviewMessage } from "./webviewProtocol.ts";
import type { BinaryStatus, HostMessage, UiPart } from "./webviewProtocol.ts";

export type HostBinary = BinaryStatus;

/**
 * The sidebar's Webview shell.
 *
 * It owns the tabs and, later, the child agents and transcripts. For now it
 * proves the view opens, shows the resolved binary, and speaks the typed
 * message boundary; the chat arrives in the next task.
 */
export class ChatViewProvider implements vscode.WebviewViewProvider {
  static readonly viewType = "loongagent.chat";

  private readonly context: vscode.ExtensionContext;
  private readonly output: vscode.OutputChannel;
  private readonly tabs = new TabManager();
  private view: vscode.WebviewView | undefined;
  private binary: HostBinary | undefined;

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
    this.view = undefined;
  }

  private handle(raw: unknown): void {
    const message = parseWebviewMessage(raw);
    if (message === undefined) return;
    if (message.type === "ready") this.postBootstrap();
    else this.output.appendLine(`webview: ${message.type} arrives with the chat task`);
  }

  private postBootstrap(): void {
    const active = this.tabs.active();
    const transcript: UiPart[] = [];
    this.post({
      type: "bootstrap",
      tabs: [...this.tabs.tabs].map((tab) => ({ ...tab })),
      ...(this.tabs.activeId === undefined ? {} : { activeTabId: this.tabs.activeId }),
      activeTranscript: transcript,
      models: [],
      ...(active?.sessionId === undefined ? {} : { sessionId: active.sessionId }),
      ...(this.binary === undefined ? {} : { binary: this.binary }),
    });
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
