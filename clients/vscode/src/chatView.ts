import * as vscode from "vscode";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { basename, isAbsolute, relative, resolve } from "node:path";
import { ApprovalBook } from "./approval.ts";
import { previewEdit, previewWrite } from "./diff.ts";
import { DiffProvider } from "./diffProvider.ts";
import { fileChoices } from "./filePicker.ts";
import { refusalToStart } from "./guards.ts";
import { PreviewProvider } from "./previewProvider.ts";
import { promptText } from "./prompt.ts";
import { sameSelection, selectionAttachment } from "./selection.ts";
import { pathsInArgs, Renderer } from "./render.ts";
import { SessionAgent } from "./sessionAgent.ts";
import { FileSnapshots } from "./snapshots.ts";
import { TabManager } from "./tab.ts";
import { Transcript } from "./transcript.ts";
import { TurnBusyError } from "./turn.ts";
import { parseWebviewMessage } from "./webviewProtocol.ts";
import { EVENTS } from "../../ts/protocol.generated.ts";
import type { ApprovalRequested, ModelList, ToolCall, ToolResult } from "../../ts/protocol.generated.ts";
import type { DiffUris } from "./diffProvider.ts";
import type { Preview } from "./diff.ts";
import type { Attachment, BinaryStatus, HostMessage, UiPart } from "./webviewProtocol.ts";
import type { ApprovalAnswer } from "./turn.ts";
import type { SessionAgent as Agent } from "./sessionAgent.ts";
import type { LoongSession } from "./session.ts";

export type HostBinary = BinaryStatus;

const LEFT_SCHEME = "loongagent-left";
const PREVIEW_SCHEME = "loongagent-preview";
const RECENT_TURNS = 8;

interface TurnReview {
  token: string;
  snapshots: FileSnapshots;
}

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
  private readonly approvals = new ApprovalBook();
  private readonly turns = new Map<string, FileSnapshots>();
  private readonly approvalPreviews = new Map<string, { token: string; path: string; after: string }>();
  private readonly pendingAnswers = new Map<string, Promise<ApprovalAnswer>>();
  private readonly left: DiffProvider<vscode.Uri>;
  private readonly preview: PreviewProvider<vscode.Uri>;
  private view: vscode.WebviewView | undefined;
  private binary: HostBinary | undefined;
  private agent: Agent | undefined;
  private models: ModelList | undefined;
  private readonly attachments: Attachment[] = [];
  private selection: Attachment | undefined;
  private nextAttachment = 1;
  private ready = false;
  private running = false;
  private nextTurn = 1;
  private pendingTool: { callId: string; tool?: string; path?: string; preview?: Preview } | undefined;
  private readonly unreviewed = new Set<string>();

  constructor(options: {
    context: vscode.ExtensionContext;
    output: vscode.OutputChannel;
    binary?: HostBinary;
  }) {
    this.context = options.context;
    this.output = options.output;
    this.binary = options.binary;
    this.tabs.open({ id: "tab-1", title: "New session", status: "idle" });
    this.context.subscriptions.push(
      vscode.window.onDidChangeTextEditorSelection(() => this.refreshSelection()),
      vscode.window.onDidChangeActiveTextEditor(() => this.refreshSelection()),
      vscode.workspace.onDidChangeWorkspaceFolders(() => this.refreshSelection()),
    );
    this.refreshSelection();

    const leftUris: DiffUris<vscode.Uri> = {
      make: (token, path, label) =>
        vscode.Uri.from({ scheme: LEFT_SCHEME, path, query: `${token}|${label}` }),
      pathOf: (uri) => uri.path,
      tokenOf: (uri) => uri.query.split("|")[0] ?? "",
    };
    const previewUris: DiffUris<vscode.Uri> = {
      make: (token, path, label) =>
        vscode.Uri.from({ scheme: PREVIEW_SCHEME, path, query: `${token}|${label}` }),
      pathOf: (uri) => uri.path,
      tokenOf: (uri) => uri.query.split("|")[0] ?? "",
    };
    this.left = new DiffProvider(
      (token) => this.turns.get(token),
      (path) => this.gitShow(path),
      leftUris,
    );
    this.preview = new PreviewProvider(previewUris);
    this.context.subscriptions.push(
      vscode.workspace.registerTextDocumentContentProvider(LEFT_SCHEME, {
        provideTextDocumentContent: (uri) => this.left.provideTextDocumentContent(uri),
      }),
      vscode.workspace.registerTextDocumentContentProvider(PREVIEW_SCHEME, {
        provideTextDocumentContent: (uri) => this.preview.provideTextDocumentContent(uri),
      }),
    );
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
      this.denyPending("webview closed");
      this.view = undefined;
    });
  }

  setBinary(binary: HostBinary): void {
    this.binary = binary;
    this.postBootstrap();
    if (this.ready) void this.onReady();
  }

  post(message: HostMessage): void {
    void this.view?.webview.postMessage(message);
  }

  dispose(): void {
    this.denyPending("webview closed");
    this.agent?.dispose();
    this.agent = undefined;
    this.view = undefined;
  }

  private denyPending(reason: string): void {
    for (const tab of this.tabs.tabs) this.approvals.denyTab(tab.id, reason);
  }

  private closeTab(tabId: string): void {
    this.approvals.denyTab(tabId, "tab closed");
    if (this.tabs.activeId === tabId) {
      this.agent?.dispose();
      this.agent = undefined;
      this.transcript.reset();
      this.attachments.length = 0;
    }
    this.tabs.close(tabId);
    this.postBootstrap();
    this.postTabs();
  }

  /** Open the before/after diff for a turn's changed files; used by the command. */
  async reviewChanges(token?: string, paths?: string[]): Promise<void> {
    if (token === undefined || paths === undefined || paths.length === 0) {
      this.output.appendLine("review: nothing to review — ask for a change first");
      return;
    }
    const folder = vscode.workspace.workspaceFolders?.[0];
    for (const path of paths) {
      if (folder === undefined || !isInside(folder.uri.fsPath, path)) continue;
      await vscode.commands.executeCommand(
        "vscode.diff",
        this.left.uriFor(token, path, "before"),
        vscode.Uri.file(path),
        `${basename(path)} (before ↔ after)`,
      );
    }
  }

  private handle(raw: unknown): void {
    const message = parseWebviewMessage(raw);
    if (message === undefined) return;
    switch (message.type) {
      case "ready":
        void this.onReady();
        break;
      case "send":
        void this.send(message.text);
        break;
      case "cancel":
        this.cancel();
        break;
      case "approval":
        this.answerApproval(message.tabId, message.requestId, message.choice);
        break;
      case "review":
        void this.reviewChanges(message.token, message.paths);
        break;
      case "openDiff":
        void this.openApprovalDiff(message.token, message.path);
        break;
      case "selectModel":
        void this.selectModel(message.name);
        break;
      case "context":
        void this.showContext();
        break;
      case "pickFile":
        void this.pickFile();
        break;
      case "removeAttachment":
        this.removeAttachment(message.id);
        break;
      case "newTab":
        void this.newSession();
        break;
      case "yolo":
        void this.yolo(message.on);
        break;
      case "closeTab":
        this.closeTab(message.tabId);
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
      models: this.models?.models ?? [],
      ...(this.models?.selected === undefined ? {} : { model: this.models.selected }),
      ...(this.agent?.sessionId === undefined ? {} : { sessionId: this.agent.sessionId }),
      ...(this.binary === undefined ? {} : { binary: this.binary }),
      ...(this.selection === undefined ? {} : { selection: { ...this.selection } }),
    });
  }

  private postAttachments(): void {
    const tab = this.tabs.active();
    if (tab === undefined) return;
    this.post({
      type: "attachments",
      tabId: tab.id,
      items: this.attachments.map((attachment) => ({ ...attachment })),
    });
  }

  private postTabs(): void {
    this.post({ type: "tabs", tabs: [...this.tabs.tabs].map((tab) => ({ ...tab })) });
  }

  private async onReady(): Promise<void> {
    this.ready = true;
    this.postBootstrap();
    this.postAttachments();
    if (this.binary === undefined) return;
    try {
      const session = await this.ensureSession();
      this.models = await session.models();
      this.postBootstrap();
      const resume = this.agent?.takeResume();
      if (resume !== undefined) {
        for (const part of new Renderer({ cwd: this.workspace() })
          .resumed(resume.events, resume.sessionId)) {
          this.transcript.append(part);
          this.post({ type: "part", tabId: this.tabs.activeId ?? "tab-1", part });
        }
      }
      const exit = this.agent?.takeLastExit();
      if (exit !== undefined) {
        const part: UiPart = {
          kind: "notice",
          level: "warn",
          text: `The previous loongagent server exited (${exit.signal ?? `code ${exit.code}`}); a new one starts on the next send.`,
        };
        this.transcript.append(part);
        this.post({ type: "part", tabId: this.tabs.activeId ?? "tab-1", part });
      }
    } catch (error) {
      this.postError(startupAdvice(explain(error)));
      this.output.appendLine(`failed: ${explain(error)}`);
    }
  }

  private async ensureSession(): Promise<LoongSession> {
    if (this.binary === undefined) {
      throw new Error("the loongagent binary was not found; set loongagent.path or run Locate");
    }
    const folder = vscode.workspace.workspaceFolders?.[0];
    const refusal = refusalToStart({
      hasFolder: folder !== undefined,
      isTrusted: vscode.workspace.isTrusted,
    });
    if (refusal !== undefined || folder === undefined) {
      throw new Error(refusal ?? "open a folder first — the agent works on a workspace");
    }
    if (this.agent === undefined) {
      const agent = new SessionAgent({
        folder,
        output: this.output,
        context: this.context,
        binaryPath: this.binary.path,
      });
      agent.setExitObserver((code, signal) => {
        const reason = signal ?? `code ${code}`;
        this.denyPending(`server exited (${reason})`);
        const id = this.tabs.activeId;
        if (id !== undefined) {
          this.tabs.update(id, { status: "stopped" });
          this.postTabs();
        }
        this.postError(`The loongagent server exited (${reason}). The next send starts a new one.`);
      });
      this.agent = agent;
    }
    return this.agent.ensure();
  }

  private workspace(): string {
    return vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? "";
  }

  private async selectModel(name: string): Promise<void> {
    if (this.running) {
      this.postError("Stop the running turn before changing the model.");
      return;
    }
    try {
      const session = await this.ensureSession();
      this.models = await session.selectModel(name);
      this.postBootstrap();
    } catch (error) {
      this.postError(`I could not change the model: ${explain(error)}`);
      this.output.appendLine(`failed: ${explain(error)}`);
    }
  }

  private async showContext(): Promise<void> {
    try {
      const session = await this.ensureSession();
      const [part] = new Renderer({ cwd: this.workspace() }).context(await session.context());
      this.transcript.append(part);
      this.post({ type: "part", tabId: this.tabs.activeId ?? "tab-1", part });
    } catch (error) {
      this.postError(`I could not read the context: ${explain(error)}`);
      this.output.appendLine(`failed: ${explain(error)}`);
    }
  }

  private async pickFile(): Promise<void> {
    const folder = vscode.workspace.workspaceFolders?.[0];
    if (folder === undefined) {
      this.postError("Open a folder first — the agent works on a workspace.");
      return;
    }
    try {
      const files = await vscode.workspace.findFiles(
        "**/*",
        "**/{node_modules,.git}/**",
        2000,
      );
      const choices = fileChoices(files.map((uri) => uri.fsPath), folder.uri.fsPath);
      if (choices.length === 0) {
        this.postError("There are no workspace files to attach.");
        return;
      }
      const picked = await vscode.window.showQuickPick(choices, {
        placeHolder: "Attach a workspace file — type to filter",
        matchOnDescription: true,
      });
      if (picked === undefined) return;
      this.attachments.push({
        id: `att-${this.nextAttachment++}`,
        kind: "file",
        path: picked.path,
      });
      this.postAttachments();
    } catch (error) {
      this.postError(`I could not list the workspace files: ${explain(error)}`);
    }
  }

  private refreshSelection(): void {
    const folder = vscode.workspace.workspaceFolders?.[0];
    const editor = vscode.window.activeTextEditor;
    const next = folder === undefined || editor === undefined
      ? undefined
      : selectionAttachment({
        path: editor.document.uri.fsPath,
        startLine: editor.selection.start.line + 1,
        endLine: editor.selection.end.line + 1,
        text: editor.selection.isEmpty ? "" : editor.document.getText(editor.selection),
      }, folder.uri.fsPath, `att-${this.nextAttachment}`);
    if (sameSelection(this.selection, next)) return;
    if (next !== undefined) this.nextAttachment++;
    this.selection = next;
    this.postSelection();
  }

  private postSelection(): void {
    this.post({
      type: "selection",
      ...(this.selection === undefined ? {} : { selection: { ...this.selection } }),
    });
  }

  private removeAttachment(id: string): void {
    const index = this.attachments.findIndex((attachment) => attachment.id === id);
    if (index < 0) return;
    this.attachments.splice(index, 1);
    this.postAttachments();
  }

  private async newSession(): Promise<void> {
    if (this.running) {
      this.postError("Stop the running turn before starting a new session.");
      return;
    }
    try {
      await this.ensureSession();
      await this.agent?.startNew();
      const session = await this.ensureSession();
      this.transcript.reset();
      this.attachments.length = 0;
      this.models = await session.models();
      const tab = this.tabs.active();
      if (tab !== undefined) {
        this.tabs.update(tab.id, {
          sessionId: session.sessionId,
          title: "New session",
          status: "idle",
        });
      }
      this.postBootstrap();
      this.postAttachments();
      this.postTabs();
    } catch (error) {
      this.postError(`I could not start a new session: ${explain(error)}`);
      this.output.appendLine(`failed: ${explain(error)}`);
    }
  }

  private async yolo(on: boolean): Promise<void> {
    try {
      if (on) {
        const choice = await vscode.window.showWarningMessage(
          "Turn approval prompts off for this session? The sandbox stays in force.",
          { modal: true },
          "Turn prompts off",
          "Cancel",
        );
        if (choice !== "Turn prompts off") return;
      }
      const session = await this.ensureSession();
      const state = await session.setAutoApprove(on);
      await this.agent?.rememberAutoApprove(state);
      const part: UiPart = {
        kind: "notice",
        level: state ? "warn" : "info",
        text: state
          ? "Approval prompts are off for this session. The sandbox is unchanged."
          : "Approval prompts are on again for this session. The sandbox was never changed.",
      };
      this.transcript.append(part);
      this.post({ type: "part", tabId: this.tabs.activeId ?? "tab-1", part });
    } catch (error) {
      this.postError(`I could not change the approval setting: ${explain(error)}`);
      this.output.appendLine(`failed: ${explain(error)}`);
    }
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
    const prompt = promptText(text, this.selection === undefined
      ? this.attachments
      : [...this.attachments, this.selection]);
    this.attachments.length = 0;
    this.postAttachments();
    const renderer = new Renderer({ cwd: folder.uri.fsPath });
    renderer.beginTurn();
    const review = this.beginReview(folder);
    this.pendingTool = undefined;
    let failed = false;

    let controller;
    try {
      controller = await this.agent.beginTurn({
        onEvent: (event) => {
          if (event.kind === EVENTS.toolCall) {
            const call = event.data as unknown as ToolCall;
            review.snapshots.see(pathsInArgs(call.args, folder.uri.fsPath));
            this.recordPendingTool(call, review, folder.uri.fsPath);
          }
          if (event.kind === EVENTS.approvalRequested) {
            this.showApproval(tab.id, event.data as unknown as ApprovalRequested, review);
            return;
          }
          if (event.kind === EVENTS.toolResult) {
            const result = event.data as unknown as ToolResult;
            if (this.unreviewed.delete(result.call_id) && result.ok) {
              const auto = this.agent?.autoApprove === true;
              const notice: UiPart = {
                kind: "notice",
                level: "warn",
                text: auto
                  ? "write applied without review because approval prompts are off for this session"
                  : 'write applied without review — set [permissions] write_workspace = "ask" to review before it lands',
              };
              this.transcript.append(notice);
              this.post({ type: "part", tabId: tab.id, part: notice });
            }
          }
          for (const part of renderer.event(event)) {
            if (part.kind === "assistant") {
              this.transcript.append(part);
              this.post({ type: "assistantFinal", tabId: tab.id, text: part.text, html: part.html });
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
          const waiting = this.pendingAnswers.get(asked.request_id)
            ?? this.approvals.wait(tab.id, asked.request_id);
          this.pendingAnswers.delete(asked.request_id);
          const answer = await waiting;
          this.transcript.updateApproval(asked.request_id,
            { state: answer.granted ? "allowed" : "denied" });
          const updated = this.transcript.all().find((entry) =>
            entry.kind === "approval" && entry.requestId === asked.request_id);
          if (updated !== undefined) this.post({ type: "part", tabId: tab.id, part: updated });
          return answer;
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

    const previousExit = this.agent.takeLastExit();
    if (previousExit !== undefined) {
      const notice: UiPart = {
        kind: "notice",
        level: "warn",
        text: `The previous loongagent server exited (${previousExit.signal ?? `code ${previousExit.code}`}); a new one is running now.`,
      };
      this.transcript.append(notice);
      this.post({ type: "part", tabId: tab.id, part: notice });
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
      await controller.run(prompt);
      const changed = this.left.changedAmong(review.token, renderer.touchedFiles(), readNow)
        .filter((path) => isInside(folder.uri.fsPath, path));
      if (changed.length > 0) {
        const part: UiPart = { kind: "review", token: review.token, paths: changed };
        this.transcript.append(part);
        this.post({ type: "part", tabId: tab.id, part });
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
      this.pendingTool = undefined;
      this.pendingAnswers.clear();
      this.unreviewed.clear();
      this.postTabs();
    }
  }

  private answerApproval(
    tabId: string,
    requestId: string,
    choice: "once" | "always" | "deny",
  ): void {
    const answer = choice === "deny"
      ? { granted: false, scope: "once" as const, reason: "denied in VS Code" }
      : { granted: true, scope: choice === "once" ? "once" as const : "action_class" as const };
    if (!this.approvals.answer(tabId, requestId, answer)) return;
    this.tabs.update(tabId, { status: "running" });
    this.postTabs();
    this.transcript.updateApproval(requestId, { state: answer.granted ? "allowed" : "denied" });
    const updated = this.transcript.all().find((entry) =>
      entry.kind === "approval" && entry.requestId === requestId);
    if (updated !== undefined) this.post({ type: "part", tabId, part: updated });
  }

  private recordPendingTool(call: ToolCall, review: TurnReview, workspace: string): void {
    const args = call.args;
    this.pendingTool = { callId: call.call_id };
    if (args === null || typeof args !== "object" || Array.isArray(args)) return;
    const raw = (args as Record<string, unknown>).path;
    if (typeof raw !== "string") return;
    const path = isAbsolute(raw) ? resolve(raw) : resolve(workspace, raw);
    if (!isInside(workspace, path)) return;
    if (call.tool === "write_file" || call.tool === "edit") this.unreviewed.add(call.call_id);
    review.snapshots.see([path]);
    const snapshot = review.snapshots.left(path);
    if (snapshot !== undefined && snapshot.source !== "snapshot" && snapshot.note !== "new file") {
      this.pendingTool = { callId: call.call_id, tool: call.tool, path };
      return;
    }
    // The preview is against the file as it is now; the review's left side
    // stays the turn's first-mention snapshot.
    const before = readNow(path) ?? (snapshot?.source === "snapshot" ? snapshot.content : undefined);
    let preview: Preview | undefined;
    if (call.tool === "write_file") {
      const content = (args as Record<string, unknown>).content;
      if (typeof content === "string") preview = previewWrite(before, content);
    } else if (call.tool === "edit") {
      const oldString = (args as Record<string, unknown>).old_string;
      const newString = (args as Record<string, unknown>).new_string;
      const replaceAll = (args as Record<string, unknown>).replace_all === true;
      if (typeof oldString === "string" && typeof newString === "string") {
        preview = previewEdit(before, oldString, newString, replaceAll);
      }
    }
    this.pendingTool = { callId: call.call_id, tool: call.tool, path, preview };
  }

  private showApproval(tabId: string, asked: ApprovalRequested, review: TurnReview): void {
    // Register the waiter before the card is visible: a fast click must resolve
    // the same promise `decide` will await, not be lost between them.
    this.pendingAnswers.set(asked.request_id, this.approvals.wait(tabId, asked.request_id));
    this.tabs.update(tabId, { status: "waiting" });
    this.postTabs();
    const pending = this.pendingTool;
    if (pending !== undefined) this.unreviewed.delete(pending.callId);
    const path = pending?.path;
    const preview = pending?.preview;
    if (path !== undefined && preview?.ok === true) {
      this.approvalPreviews.set(asked.request_id, { token: review.token, path, after: preview.after });
    }
    if (preview !== undefined && !preview.ok) {
      const warning: UiPart = {
        kind: "notice",
        level: "warn",
        text: `preview unavailable: ${preview.reason} — the kernel will refuse this edit`,
      };
      this.transcript.append(warning);
      this.post({ type: "part", tabId, part: warning });
    } else if (path !== undefined && pending?.tool !== undefined
      && (pending.tool === "write_file" || pending.tool === "edit")) {
      const warning: UiPart = {
        kind: "notice",
        level: "warn",
        text: "preview unavailable: the file could not be read; the approval still applies",
      };
      this.transcript.append(warning);
      this.post({ type: "part", tabId, part: warning });
    }
    const part: UiPart = {
      kind: "approval",
      requestId: asked.request_id,
      ...(pending?.tool === undefined ? {} : { tool: pending.tool }),
      actionClass: asked.action_class,
      reason: asked.reason,
      paths: path === undefined ? [] : [path],
      ...(preview?.ok === true ? { diff: preview.diff } : {}),
      state: "pending",
    };
    this.transcript.append(part);
    this.post({ type: "part", tabId, part });
  }

  private async openApprovalDiff(requestId: string, path: string): Promise<void> {
    const preview = this.approvalPreviews.get(requestId);
    if (preview === undefined) {
      this.output.appendLine(`review: no preview is held for request ${requestId}`);
      return;
    }
    await vscode.commands.executeCommand(
      "vscode.diff",
      this.left.uriFor(preview.token, path, "before"),
      this.preview.uriFor(requestId, path, preview.after),
      `${basename(path)} (before ↔ proposed)`,
    );
  }

  private beginReview(folder: vscode.WorkspaceFolder): TurnReview {
    const token = String(this.nextTurn++);
    const snapshots = new FileSnapshots({ workspace: folder.uri.fsPath });
    this.turns.set(token, snapshots);
    while (this.turns.size > RECENT_TURNS) {
      const oldest = this.turns.keys().next();
      if (oldest.done === true) break;
      this.turns.delete(oldest.value);
    }
    return { token, snapshots };
  }

  private gitShow(path: string): string | undefined {
    const folder = vscode.workspace.workspaceFolders?.[0];
    if (folder === undefined) return undefined;
    const inside = relative(folder.uri.fsPath, path);
    if (inside === "" || inside.startsWith("..")) return undefined;
    try {
      return execFileSync("git", ["show", `HEAD:${inside}`], {
        cwd: folder.uri.fsPath,
        encoding: "utf8",
        stdio: ["ignore", "pipe", "ignore"],
      });
    } catch {
      return undefined;
    }
  }

  private cancel(): void {
    if (!this.running) return;
    this.output.appendLine("turn running: the person asked to stop");
    for (const tab of this.tabs.tabs) this.approvals.denyTab(tab.id, "stopped by the person");
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

function isInside(workspace: string, path: string): boolean {
  const inside = relative(workspace, path);
  return inside !== "" && !inside.startsWith("..") && !isAbsolute(inside);
}

function readNow(path: string): string | undefined {
  try {
    return readFileSync(path, "utf8");
  } catch {
    return undefined;
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
