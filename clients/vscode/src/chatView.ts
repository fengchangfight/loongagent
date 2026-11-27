import * as vscode from "vscode";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { basename, isAbsolute, relative, resolve } from "node:path";
import { ApprovalBook } from "./approval.ts";
import { previewEdit, previewWrite } from "./diff.ts";
import { DiffProvider } from "./diffProvider.ts";
import { fileChoices } from "./filePicker.ts";
import { focusedFile, sameFocus } from "./focus.ts";
import { refusalToStart } from "./guards.ts";
import { PreviewProvider } from "./previewProvider.ts";
import { promptText } from "./prompt.ts";
import { modeNotice, reconcileQuestions, refusedForApproval, switchDecision } from "./questions.ts";
import { sameSelection, selectionAttachment } from "./selection.ts";
import { pathsInArgs, Renderer } from "./render.ts";
import { SessionAgent } from "./sessionAgent.ts";
import { decodeTabs, encodeTabs } from "./tabStore.ts";
import { FileSnapshots } from "./snapshots.ts";
import { TabManager } from "./tab.ts";
import { Transcript } from "./transcript.ts";
import { TurnBusyError } from "./turn.ts";
import { parseWebviewMessage } from "./webviewProtocol.ts";
import { EVENTS } from "../../ts/protocol.generated.ts";
import type { ApprovalRequested, ModelList, ToolCall, ToolResult } from "../../ts/protocol.generated.ts";
import type { DiffUris } from "./diffProvider.ts";
import type { Preview } from "./diff.ts";
import type { Attachment, BinaryStatus, Focus, HostMessage, PermissionMode, UiPart } from "./webviewProtocol.ts";
import type { ApprovalAnswer, TurnController } from "./turn.ts";
import type { SessionAgent as Agent } from "./sessionAgent.ts";
import type { LoongSession } from "./session.ts";

export type HostBinary = BinaryStatus;

const LEFT_SCHEME = "loongagent-left";
const PREVIEW_SCHEME = "loongagent-preview";
const RECENT_TURNS = 8;
const TABS_KEY = "loongagent.tabs.v1";
const ACTIVE_KEY = "loongagent.activeTab.v1";

interface TurnReview {
  token: string;
  snapshots: FileSnapshots;
}

interface PendingTool {
  callId: string;
  tool?: string;
  path?: string;
  preview?: Preview;
}

/** Everything one tab owns; swapping tabs swaps this object, not the agent's identity. */
interface TabRuntime {
  transcript: Transcript;
  agent?: Agent;
  models?: ModelList;
  attachments: Attachment[];
  running: boolean;
  pendingTool?: PendingTool;
  /**
   * The request this tab is asking the person to answer, if any.
   *
   * It is what Stop checks: answering the question is what ends a parked turn, so
   * interrupting the server at the same moment drops the answer on the floor and
   * leaves the kernel holding a question nobody can answer.
   */
  question?: string;
  /**
   * The mode this tab is waiting for, when a switch was asked during a turn.
   *
   * The server reads its next request only once the turn ends — `prompt.submit` runs
   * the whole turn inside `dispatch`, so a `permissions.set` written meanwhile waits
   * in the pipe. Rather than make the person wait for a round trip that cannot come,
   * the choice is shown at once and applied the moment the turn ends. Last one wins,
   * which is what clicking twice should mean.
   */
  pendingMode?: PermissionMode;
  unreviewed: Set<string>;
}

/**
 * The chat's Webview host: one editor panel, tabs, one session agent, one transcript.
 *
 * The host owns every fact the view needs; the Webview only renders `UiPart`s
 * and sends intents. A turn's streamed fragments are not stored as parts; the
 * final `message.appended` is the record that goes into the transcript.
 */
export class ChatViewProvider {
  static readonly viewType = "loongagent.chat";

  private readonly context: vscode.ExtensionContext;
  private readonly output: vscode.OutputChannel;
  private readonly tabs = new TabManager();
  private readonly approvals = new ApprovalBook();
  private readonly turns = new Map<string, FileSnapshots>();
  private readonly approvalPreviews = new Map<string, { token: string; path: string; after: string }>();
  private readonly pendingAnswers = new Map<string, Promise<ApprovalAnswer>>();
  private readonly left: DiffProvider<vscode.Uri>;
  private readonly preview: PreviewProvider<vscode.Uri>;
  private panel: vscode.WebviewPanel | undefined;
  private binary: HostBinary | undefined;
  private readonly runtime = new Map<string, TabRuntime>();
  private selection: Attachment | undefined;
  private focus: Focus | undefined;
  private nextAttachment = 1;
  private ready = false;
  private restored = false;
  private nextTurn = 1;
  private nextTab = 1;

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
      vscode.window.onDidChangeTextEditorSelection((event) =>
        this.captureEditor(event.textEditor)),
      vscode.window.onDidChangeActiveTextEditor((editor) => this.captureEditor(editor)),
      vscode.workspace.onDidChangeWorkspaceFolders(() => {
        this.captureEditor(vscode.window.activeTextEditor);
        void this.refreshFiles();
      }),
    );
    this.captureEditor(vscode.window.activeTextEditor);

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

  /**
   * Open the chat as an editor tab. Once the panel is already open, the same
   * action starts another conversation in a new internal tab; that is what the
   * title icon means after the first click.
   */
  openPanel(): void {
    if (this.panel !== undefined) {
      this.panel.reveal();
      void this.newTab();
      return;
    }
    const panel = vscode.window.createWebviewPanel(
      ChatViewProvider.viewType,
      "loongagent",
      vscode.ViewColumn.Beside,
      {
        enableScripts: true,
        retainContextWhenHidden: true,
        localResourceRoots: [
          this.context.extensionUri,
          vscode.Uri.joinPath(this.context.extensionUri, "dist"),
          vscode.Uri.joinPath(this.context.extensionUri, "webview"),
          vscode.Uri.joinPath(this.context.extensionUri, "media"),
        ],
      },
    );
    this.panel = panel;
    panel.iconPath = vscode.Uri.joinPath(this.context.extensionUri, "media", "loong-48.png");
    panel.webview.html = this.html(panel.webview);
    panel.webview.onDidReceiveMessage((raw) => this.handle(raw));
    panel.onDidDispose(() => {
      this.denyPending("tab closed");
      this.panel = undefined;
      this.ready = false;
    });
  }

  private runtimeFor(tabId: string): TabRuntime {
    const existing = this.runtime.get(tabId);
    if (existing !== undefined) return existing;
    const created: TabRuntime = {
      transcript: new Transcript(),
      attachments: [],
      running: false,
      unreviewed: new Set(),
    };
    this.runtime.set(tabId, created);
    return created;
  }

  private activeRuntime(): TabRuntime {
    return this.runtimeFor(this.tabs.activeId ?? "tab-1");
  }

  private get transcript(): Transcript { return this.activeRuntime().transcript; }
  private get agent(): Agent | undefined { return this.activeRuntime().agent; }
  private set agent(value: Agent | undefined) { this.activeRuntime().agent = value; }
  private get models(): ModelList | undefined { return this.activeRuntime().models; }
  private set models(value: ModelList | undefined) { this.activeRuntime().models = value; }
  private get attachments(): Attachment[] { return this.activeRuntime().attachments; }
  private get running(): boolean { return this.activeRuntime().running; }
  private set running(value: boolean) { this.activeRuntime().running = value; }
  private get pendingTool(): PendingTool | undefined { return this.activeRuntime().pendingTool; }
  private set pendingTool(value: PendingTool | undefined) { this.activeRuntime().pendingTool = value; }
  private get unreviewed(): Set<string> { return this.activeRuntime().unreviewed; }

  setBinary(binary: HostBinary): void {
    this.binary = binary;
    this.postBootstrap();
    if (this.ready) void this.onReady();
  }

  post(message: HostMessage): void {
    void this.panel?.webview.postMessage(message);
  }

  dispose(): void {
    this.denyPending("webview closed");
    this.panel?.dispose();
    this.panel = undefined;
    this.ready = false;
    for (const runtime of this.runtime.values()) runtime.agent?.dispose();
    this.runtime.clear();
  }

  private denyPending(reason: string): void {
    for (const tab of this.tabs.tabs) this.approvals.denyTab(tab.id, reason);
  }

  private closeTab(tabId: string): void {
    this.approvals.denyTab(tabId, "tab closed");
    const runtime = this.runtime.get(tabId);
    runtime?.agent?.dispose();
    this.runtime.delete(tabId);
    this.tabs.close(tabId);
    if (this.tabs.tabs.length === 0) {
      this.tabs.open({ id: this.newTabId(), title: "New session", status: "idle" });
    }
    this.persistTabs();
    this.postTabs();
    this.postBootstrap();
    this.postAttachments();
    void this.activateActive();
  }

  private async selectTab(tabId: string): Promise<void> {
    if (!this.tabs.tabs.some((tab) => tab.id === tabId)) {
      this.postError("That tab is no longer open.");
      return;
    }
    this.tabs.activate(tabId);
    this.persistTabs();
    this.postBootstrap();
    this.postAttachments(tabId);
    this.postTabs();
    try {
      await this.ensureAgent(tabId);
      if (this.tabs.activeId === tabId) this.postBootstrap();
    } catch (error) {
      this.dropTab(tabId, error);
    }
  }

  private async history(): Promise<void> {
    const tabId = this.tabs.activeId;
    if (tabId === undefined) {
      this.postError("Open a folder and start a chat first.");
      return;
    }
    try {
      const agent = await this.ensureAgent(tabId);
      const session = await agent.ensure();
      this.post({ type: "sessions", sessions: await session.listSessions() });
    } catch (error) {
      this.post({ type: "error", tabId,
        message: `I could not list the sessions: ${explain(error)}` });
    }
  }

  private async openSession(sessionId: string): Promise<void> {
    const existing = this.tabs.find(sessionId);
    if (existing !== undefined) {
      await this.selectTab(existing.id);
      return;
    }
    const id = this.newTabId();
    const opened = this.tabs.open({
      id,
      sessionId,
      title: sessionId,
      status: "idle",
    });
    if (opened === "max") {
      this.postError("At most 5 tabs can be open at once.");
      return;
    }
    this.runtimeFor(id);
    this.persistTabs();
    this.postTabs();
    this.postBootstrap();
    try {
      await this.ensureAgent(id);
      if (this.tabs.activeId === id) this.postBootstrap();
    } catch (error) {
      this.dropTab(id, error);
    }
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
        void this.send(message.tabId, message.text);
        break;
      case "cancel":
        this.cancel(message.tabId);
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
        void this.selectModel(message.tabId, message.name);
        break;
      case "context":
        void this.showContext(message.tabId);
        break;
      case "attachFile":
        this.attachFile(message.tabId, message.path);
        break;
      case "removeAttachment":
        this.removeAttachment(message.tabId, message.id);
        break;
      case "newTab":
        void this.newTab();
        break;
      case "selectTab":
        void this.selectTab(message.tabId);
        break;
      case "history":
        void this.history();
        break;
      case "openSession":
        void this.openSession(message.sessionId);
        break;
      case "mode":
        void this.setMode(message.tabId, message.mode);
        break;
      case "closeTab":
        this.closeTab(message.tabId);
        break;
      default:
        this.output.appendLine("webview: unknown message ignored");
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
      ...(this.focus === undefined ? {} : { focus: { ...this.focus } }),
      ...(this.agent === undefined ? {} : { mode: this.agent.mode }),
    });
  }

  private postAttachments(tabId = this.tabs.activeId): void {
    if (tabId === undefined) return;
    const tab = this.tabs.tabs.find((entry) => entry.id === tabId);
    if (tab === undefined) return;
    this.post({
      type: "attachments",
      tabId,
      items: this.runtimeFor(tabId).attachments.map((attachment) => ({ ...attachment })),
    });
  }

  private postTabs(): void {
    this.post({ type: "tabs", tabs: [...this.tabs.tabs].map((tab) => ({ ...tab })) });
  }

  private async onReady(): Promise<void> {
    this.ready = true;
    if (!this.restored) {
      this.restored = true;
      const stored = decodeTabs(this.context.workspaceState.get(TABS_KEY));
      if (stored.length > 0) {
        this.tabs.restore(
          stored.map((tab) => ({
            id: tab.id,
            ...(tab.sessionId === undefined ? {} : { sessionId: tab.sessionId }),
            title: tab.title,
            status: "idle" as const,
          })),
          this.context.workspaceState.get<string>(ACTIVE_KEY),
        );
      }
      if (this.tabs.tabs.length === 0) {
        this.tabs.open({ id: "tab-1", title: "New session", status: "idle" });
      }
      for (const tab of this.tabs.tabs) this.runtimeFor(tab.id);
      this.persistTabs();
    }
    this.postBootstrap();
    this.postAttachments();
    this.postTabs();
    void this.refreshFiles();
    if (this.binary === undefined) return;
    await this.activateActive();
  }

  /** Run the active tab's child and publish its transcript, or drop a dead restored tab. */
  private async activateActive(): Promise<void> {
    const tabId = this.tabs.activeId;
    if (tabId === undefined) return;
    try {
      await this.ensureAgent(tabId);
    } catch (error) {
      this.dropTab(tabId, error);
      return;
    }
    this.postBootstrap();
    this.postAttachments(tabId);
    this.postTabs();
  }

  /** The child and session for one tab, created lazily on first activation. */
  private async ensureAgent(tabId: string): Promise<Agent> {
    const tab = this.tabs.tabs.find((entry) => entry.id === tabId);
    if (tab === undefined) throw new Error("that tab is no longer open");
    const runtime = this.runtimeFor(tabId);
    if (runtime.agent !== undefined) {
      await runtime.agent.ensure();
      return runtime.agent;
    }
    const folder = vscode.workspace.workspaceFolders?.[0];
    const refusal = refusalToStart({
      hasFolder: folder !== undefined,
      isTrusted: vscode.workspace.isTrusted,
    });
    if (refusal !== undefined || folder === undefined) {
      throw new Error(refusal ?? "open a folder first — the agent works on a workspace");
    }
    if (this.binary === undefined) {
      throw new Error("the loongagent binary was not found; set loongagent.path or run Locate");
    }
    const agent = new SessionAgent({
      folder,
      output: this.output,
      context: this.context,
      binaryPath: this.binary.path,
      ...(tab.sessionId === undefined ? { fresh: true } : { sessionId: tab.sessionId }),
    });
    runtime.agent = agent;
    agent.setExitObserver((code, signal) => {
      const reason = signal ?? `code ${code}`;
      this.approvals.denyTab(tabId, `server exited (${reason})`);
      const current = this.runtimeFor(tabId);
      if (current.agent !== agent) return;
      current.agent = undefined;
      current.running = false;
      this.tabs.update(tabId, { status: "stopped" });
      this.postTabs();
      this.post({ type: "error", tabId,
        message: `The loongagent server exited (${reason}). The next send starts a new one.` });
    });
    try {
      const session = await agent.ensure();
      tab.sessionId = agent.sessionId;
      this.persistTabs();
      const resume = agent.takeResume();
      if (resume !== undefined) {
        const parts = new Renderer({ cwd: folder.uri.fsPath })
          .resumed(resume.events, resume.sessionId);
        for (const part of parts) {
          runtime.transcript.append(part);
          this.post({ type: "part", tabId, part });
        }
      }
      runtime.models = await session.models();
      await this.reconcileParkedQuestions(tabId, session);
      this.post({ type: "permissions", tabId, mode: agent.mode });
      return agent;
    } catch (error) {
      if (runtime.agent === agent) runtime.agent = undefined;
      agent.dispose();
      throw error;
    }
  }

  /**
   * Sort the transcript's parked questions by what the kernel actually holds.
   *
   * A resumed session replays its log, and the log may end on a question — but a
   * parked approval lives in the kernel's memory, so the replay's last word can be
   * a question whose owner is gone. `session.attach` is the only authority: it
   * names the request its kernel holds, and everything else is retired out loud
   * rather than left as a button that answers nothing.
   *
   * When the kernel *does* still hold one, no turn is driving it (nothing is
   * running in this window), so it is answered the way `TurnGate` answers a turn
   * no surface holds — with a denial — which is what lets the next prompt through
   * instead of being refused as out of order.
   */
  private async reconcileParkedQuestions(tabId: string, session: LoongSession): Promise<void> {
    const runtime = this.runtimeFor(tabId);
    const held = session.pendingApproval;
    const { retire } = reconcileQuestions(runtime.transcript.all(), held);
    for (const requestId of retire) {
      this.retireQuestion(tabId, requestId,
        "the process that asked it is gone, and the kernel is not holding it");
    }
    if (held === undefined) return;
    if (runtime.running) return;
    try {
      await session.respondApproval(held, false, "once", "no surface is holding this turn");
      session.clearPendingApproval();
      this.output.appendLine(
        `retired: ${held} — the kernel held it with no turn driving it, so it was denied`,
      );
    } catch (error) {
      this.output.appendLine(`retired: ${held} — could not deny it: ${explain(error)}`);
      return;
    }
    this.retireQuestion(tabId, held,
      "it was still waiting when this window reopened, and no turn was driving it");
  }

  private dropTab(tabId: string, error: unknown): void {
    if (this.tabs.tabs.length <= 1) {
      this.post({ type: "error", tabId,
        message: `I could not open that session: ${explain(error)}` });
      return;
    }
    this.closeTab(tabId);
    this.post({ type: "error", tabId,
      message: `I could not open that session: ${explain(error)}` });
  }

  private persistTabs(): void {
    void this.context.workspaceState.update(TABS_KEY, encodeTabs(this.tabs.tabs));
    void this.context.workspaceState.update(ACTIVE_KEY, this.tabs.activeId);
  }

  private newTabId(): string {
    let id = `tab-${this.nextTab++}`;
    while (this.tabs.tabs.some((tab) => tab.id === id)) id = `tab-${this.nextTab++}`;
    return id;
  }

  private workspace(): string {
    return vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? "";
  }

  private async selectModel(tabId: string, name: string): Promise<void> {
    const runtime = this.runtimeFor(tabId);
    if (runtime.running) {
      this.post({ type: "error", tabId, message: "Stop the running turn before changing the model." });
      return;
    }
    try {
      const agent = await this.ensureAgent(tabId);
      const session = await agent.ensure();
      runtime.models = await session.selectModel(name);
      if (this.tabs.activeId === tabId) this.postBootstrap();
    } catch (error) {
      this.post({ type: "error", tabId, message: `I could not change the model: ${explain(error)}` });
      this.output.appendLine(`failed: ${explain(error)}`);
    }
  }

  private async showContext(tabId: string): Promise<void> {
    try {
      const agent = await this.ensureAgent(tabId);
      const session = await agent.ensure();
      const [part] = new Renderer({ cwd: this.workspace() }).context(await session.context());
      this.runtimeFor(tabId).transcript.append(part);
      this.post({ type: "part", tabId, part });
    } catch (error) {
      this.post({ type: "error", tabId, message: `I could not read the context: ${explain(error)}` });
      this.output.appendLine(`failed: ${explain(error)}`);
    }
  }

  private async refreshFiles(): Promise<void> {
    const folder = vscode.workspace.workspaceFolders?.[0];
    try {
      const files = folder === undefined
        ? []
        : fileChoices(
          (await vscode.workspace.findFiles("**/*", "**/{node_modules,.git}/**", 2000))
            .map((uri) => uri.fsPath),
          folder.uri.fsPath,
        );
      this.post({ type: "files", files });
    } catch (error) {
      this.post({ type: "files", files: [] });
      this.output.appendLine(`files: could not list the workspace: ${explain(error)}`);
    }
  }

  private attachFile(tabId: string, path: string): void {
    const folder = vscode.workspace.workspaceFolders?.[0];
    if (folder === undefined || !isInside(folder.uri.fsPath, path)) {
      this.post({ type: "error", tabId, message: "That file is outside the workspace." });
      return;
    }
    this.runtimeFor(tabId).attachments.push({
      id: `att-${this.nextAttachment++}`,
      kind: "file",
      path,
    });
    this.postAttachments(tabId);
  }

  /**
   * Capture the file the active editor shows and its cursor line, independently
   * of the selection. Like the selection, the last file survives focus moving to
   * the chat (the active editor becomes `undefined`); switching to another text
   * editor replaces it, and a non-workspace or untitled editor clears it.
   */
  private captureEditor(editor: vscode.TextEditor | undefined): void {
    if (editor === undefined) return;
    const folder = vscode.workspace.workspaceFolders?.[0];
    const next = folder === undefined
      ? undefined
      : focusedFile({
        scheme: editor.document.uri.scheme,
        path: editor.document.uri.fsPath,
        cursorLine: editor.selection.active.line,
      }, folder.uri.fsPath);
    if (!sameFocus(this.focus, next)) {
      this.focus = next;
      this.postFocus();
    }
    this.captureSelection(editor);
  }

  /**
   * Capture a selection, but do not clear it when focus leaves the editor.
   *
   * Clicking the chat webview changes the active editor to `undefined`; treating
   * that as an empty selection made the chip disappear exactly when the person
   * switched to the chat to use it. An actual empty/collapsed selection in a
   * text editor, or a switch to another text editor, still clears it.
   */
  private captureSelection(editor: vscode.TextEditor | undefined): void {
    if (editor === undefined) return;
    const folder = vscode.workspace.workspaceFolders?.[0];
    const next = folder === undefined
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

  private postFocus(): void {
    this.post({
      type: "focus",
      ...(this.focus === undefined ? {} : { focus: { ...this.focus } }),
    });
  }

  private removeAttachment(tabId: string, id: string): void {
    const attachments = this.runtimeFor(tabId).attachments;
    const index = attachments.findIndex((attachment) => attachment.id === id);
    if (index < 0) return;
    attachments.splice(index, 1);
    this.postAttachments(tabId);
  }

  private async newTab(): Promise<void> {
    const id = this.newTabId();
    const opened = this.tabs.open({ id, title: "New session", status: "idle" });
    if (opened === "max") {
      this.postError("At most 5 tabs can be open at once.");
      return;
    }
    this.runtimeFor(id);
    this.persistTabs();
    this.postTabs();
    this.postBootstrap();
    this.postAttachments(id);
    try {
      await this.ensureAgent(id);
      if (this.tabs.activeId === id) this.postBootstrap();
    } catch (error) {
      this.dropTab(id, error);
    }
  }

  /**
   * Ask the server for a different mode — at once, or as soon as a turn allows.
   *
   * No confirmation step: the TUI switches on one keystroke too, and ADR-0007 §5
   * asks that a relaxed session *look* relaxed rather than that changing it be
   * laborious. The mark in the header is what carries the warning, which is why it
   * must never lag the mode.
   *
   * A running turn is not a reason to block. `prompt.submit` runs the whole turn
   * inside the server's dispatch, and the connection is read one line at a time, so
   * a `permissions.set` sent now would not be *read* until the turn ended: the button
   * would hang for minutes. The header flips instead, marked as pending, and the
   * switch lands when the turn does — which is when it takes effect anyway.
   */
  private async setMode(tabId: string, mode: PermissionMode): Promise<void> {
    let agent: Agent;
    try {
      agent = await this.ensureAgent(tabId);
    } catch (error) {
      this.post({ type: "error", tabId, message: startupAdvice(explain(error)) });
      this.output.appendLine(`failed: ${explain(error)}`);
      return;
    }
    const runtime = this.runtimeFor(tabId);
    const current = agent.mode;
    switch (switchDecision(runtime.running, mode, current, runtime.pendingMode)) {
      case "ignore":
        return;
      case "cancel-pending":
        // Asking for the mode already in force also cancels a switch still waiting to
        // land: the last click is the one the person means.
        runtime.pendingMode = undefined;
        this.post({ type: "permissions", tabId, mode: current });
        this.output.appendLine(`mode: ${current} — the pending switch was cancelled`);
        return;
      case "queue": {
        runtime.pendingMode = mode;
        this.post({ type: "permissions", tabId, mode, pending: true });
        this.appendNotice(tabId, modeNotice(mode, { deferred: true }),
          mode === "ask" ? "info" : "warn");
        this.output.appendLine(`mode: ${mode} queued — the running turn is read before any request`);
        return;
      }
      case "apply":
        await this.applyMode(tabId, agent, mode, { quiet: false });
    }
  }

  /**
   * Put a mode into force, and say what the server answered.
   *
   * `quiet` is for a switch that already said its piece when it was queued: the
   * promise was made then, so landing it again would only add noise. A failure is
   * never quiet — the header is put back to what the server really records.
   */
  private async applyMode(
    tabId: string,
    agent: Agent,
    mode: PermissionMode,
    options: { quiet: boolean },
  ): Promise<void> {
    try {
      const applied = await agent.setMode(mode);
      if (!options.quiet) {
        this.appendNotice(tabId, modeNotice(applied), applied === "ask" ? "info" : "warn");
      }
      this.post({ type: "permissions", tabId, mode: applied });
      this.output.appendLine(`mode: ${applied} (permissions.relaxed)`);
    } catch (error) {
      this.post({ type: "permissions", tabId, mode: agent.mode });
      this.post({ type: "error", tabId,
        message: `I could not change the approval setting: ${explain(error)}` });
      this.output.appendLine(`failed: ${explain(error)}`);
    }
  }

  /** Add one notice to the transcript and put it on screen. */
  private appendNotice(tabId: string, text: string, level: "info" | "warn" | "error"): void {
    const part: UiPart = { kind: "notice", level, text };
    this.runtimeFor(tabId).transcript.append(part);
    this.post({ type: "part", tabId, part });
  }

  private async send(tabId: string, text: string): Promise<void> {
    const tab = this.tabs.tabs.find((entry) => entry.id === tabId);
    if (tab === undefined) {
      this.postError("That tab is no longer open.");
      return;
    }
    const runtime = this.runtimeFor(tabId);
    if (runtime.running) {
      this.post({ type: "error", tabId,
        message: "A turn is already running. Wait for it to finish, or press Stop." });
      return;
    }
    const folder = vscode.workspace.workspaceFolders?.[0];
    const refusal = refusalToStart({
      hasFolder: folder !== undefined,
      isTrusted: vscode.workspace.isTrusted,
    });
    if (refusal !== undefined || folder === undefined) {
      this.post({ type: "error", tabId,
        message: refusal ?? "Open a folder first — the agent works on a workspace." });
      return;
    }
    if (this.binary === undefined) {
      this.post({ type: "error", tabId,
        message: "The loongagent binary was not found. Set loongagent.path or run \"loongagent: Locate the binary\"." });
      return;
    }

    let agent: Agent;
    try {
      agent = await this.ensureAgent(tabId);
    } catch (error) {
      this.post({ type: "error", tabId, message: startupAdvice(explain(error)) });
      this.output.appendLine(`failed: ${explain(error)}`);
      return;
    }

    const prompt = promptText(text, this.selection === undefined
      ? runtime.attachments
      : [...runtime.attachments, this.selection], this.focus);
    runtime.attachments.length = 0;
    this.postAttachments(tabId);
    const renderer = new Renderer({ cwd: folder.uri.fsPath });
    renderer.beginTurn();
    const review = this.beginReview(folder);
    runtime.pendingTool = undefined;
    let failed = false;

    let controller;
    try {
      controller = await agent.beginTurn({
        onEvent: (event) => {
          if (event.kind === EVENTS.toolCall) {
            const call = event.data as unknown as ToolCall;
            review.snapshots.see(pathsInArgs(call.args, folder.uri.fsPath));
            this.recordPendingTool(tabId, call, review, folder.uri.fsPath);
          }
          if (event.kind === EVENTS.approvalRequested) {
            this.showApproval(tabId, event.data as unknown as ApprovalRequested, review);
            return;
          }
          if (event.kind === EVENTS.toolResult) {
            const result = event.data as unknown as ToolResult;
            if (runtime.unreviewed.delete(result.call_id) && result.ok) {
              const auto = agent.mode !== "ask";
              const notice: UiPart = {
                kind: "notice",
                level: "warn",
                text: auto
                  ? "write applied without review because approval prompts are off for this session"
                  : 'write applied without review — set [permissions] write_workspace = "ask" to review before it lands',
              };
              runtime.transcript.append(notice);
              this.post({ type: "part", tabId, part: notice });
            }
          }
          for (const part of renderer.event(event)) {
            if (part.kind === "assistant") {
              runtime.transcript.append(part);
              this.post({ type: "assistantFinal", tabId, text: part.text, html: part.html });
              continue;
            }
            if (part.kind === "tool" && part.status !== "running") {
              const exists = runtime.transcript.all().some((entry) =>
                entry.kind === "tool" && entry.callId === part.callId);
              if (exists) runtime.transcript.updateTool(part.callId, part);
              else runtime.transcript.append(part);
            } else {
              runtime.transcript.append(part);
            }
            this.post({ type: "part", tabId, part });
          }
        },
        onDelta: (delta) => this.post({ type: "delta", tabId, text: delta }),
        decide: async (asked) => {
          this.output.appendLine(`awaiting approval: ${asked.action_class} — ${asked.reason}`);
          const key = `${tabId}:${asked.request_id}`;
          const waiting = this.pendingAnswers.get(key)
            ?? this.approvals.wait(tabId, asked.request_id);
          this.pendingAnswers.delete(key);
          const answer = await waiting;
          runtime.question = undefined;
          runtime.transcript.updateApproval(asked.request_id,
            { state: answer.granted ? "allowed" : "denied" });
          const updated = runtime.transcript.all().find((entry) =>
            entry.kind === "approval" && entry.requestId === asked.request_id);
          if (updated !== undefined) this.post({ type: "part", tabId, part: updated });
          return answer;
        },
      });
    } catch (error) {
      if (error instanceof TurnBusyError) {
        this.post({ type: "error", tabId,
          message: "A turn is already running in this session." });
        return;
      }
      this.post({ type: "error", tabId, message: startupAdvice(explain(error)) });
      this.output.appendLine(`failed: ${explain(error)}`);
      return;
    }

    const previousExit = agent.takeLastExit();
    if (previousExit !== undefined) {
      const notice: UiPart = {
        kind: "notice",
        level: "warn",
        text: `The previous loongagent server exited (${previousExit.signal ?? `code ${previousExit.code}`}); a new one is running now.`,
      };
      runtime.transcript.append(notice);
      this.post({ type: "part", tabId, part: notice });
    }
    runtime.running = true;
    this.tabs.update(tabId, { status: "running" });
    if (tab.title === "New session") {
      this.tabs.update(tabId, { title: titleFrom(text) });
    }
    const user: UiPart = { kind: "user", text };
    runtime.transcript.append(user);
    this.post({ type: "part", tabId, part: user });
    this.postTabs();
    this.persistTabs();

    try {
      await this.driveTurn(tabId, agent, controller, prompt);
      const changed = this.left.changedAmong(review.token, renderer.touchedFiles(), readNow)
        .filter((path) => isInside(folder.uri.fsPath, path));
      if (changed.length > 0) {
        const part: UiPart = { kind: "review", token: review.token, paths: changed };
        runtime.transcript.append(part);
        this.post({ type: "part", tabId, part });
      }
    } catch (error) {
      failed = true;
      const message = `The turn failed: ${explain(error)} See the **loongagent** output channel.`;
      const notice: UiPart = { kind: "notice", level: "error", text: message };
      runtime.transcript.append(notice);
      this.post({ type: "part", tabId, part: notice });
      this.output.appendLine(`failed: ${explain(error)}`);
    } finally {
      runtime.running = false;
      this.tabs.update(tabId, { status: failed ? "stopped" : "idle" });
      agent.endTurn();
      runtime.pendingTool = undefined;
      runtime.question = undefined;
      for (const key of [...this.pendingAnswers.keys()]) {
        if (key.startsWith(`${tabId}:`)) this.pendingAnswers.delete(key);
      }
      runtime.unreviewed.clear();
      this.postTabs();
      // The turn is over, so the connection is being read again: whatever was asked
      // for while it ran can now reach the server. Denials and approvals already woke
      // it; a mode switch is the one request that only ever waited.
      const deferred = runtime.pendingMode;
      runtime.pendingMode = undefined;
      if (deferred !== undefined) await this.applyMode(tabId, agent, deferred, { quiet: true });
    }
  }

  /**
   * Run the turn, and step out of the one state that leaves a session unusable.
   *
   * The server refuses a prompt while it still holds an unanswered question from
   * an earlier turn (`out of order: approval … is still pending`). That state has
   * no card left to click and nothing for Stop to stop, so the refusal is treated
   * as what it is — a question to be cleared — and the prompt is offered once
   * more. Denying is the safe direction: nothing runs that the person has not
   * just been asked about.
   */
  private async driveTurn(
    tabId: string,
    agent: Agent,
    controller: TurnController,
    prompt: string,
  ): Promise<void> {
    try {
      await controller.run(prompt);
    } catch (error) {
      const stuck = refusedForApproval(explain(error));
      if (stuck === undefined) throw error;
      const session = await agent.ensure();
      this.output.appendLine(`refused: ${stuck} is still pending — denying it and asking again`);
      const notice: UiPart = {
        kind: "notice",
        level: "warn",
        text: "One step from an earlier turn was still waiting with nothing able to answer it, "
          + "so it is denied now and your prompt is sent again.",
      };
      const runtime = this.runtimeFor(tabId);
      runtime.transcript.append(notice);
      this.post({ type: "part", tabId, part: notice });
      await session.respondApproval(stuck, false, "once", "cleared a question nothing could answer");
      session.clearPendingApproval();
      this.retireQuestion(tabId, stuck, "it outlived the turn that asked it");
      await controller.run(prompt);
    }
  }

  /**
   * Take the person's answer to a card, and never swallow it.
   *
   * A card can outlive the turn that asked it: the transcript is replayed from the
   * log, so a question whose process is gone still draws buttons. Silently
   * returning leaves the person clicking at a wall, so the card is retired and the
   * reason said out loud instead.
   */
  private answerApproval(
    tabId: string,
    requestId: string,
    choice: "once" | "always" | "deny",
  ): void {
    const answer = choice === "deny"
      ? { granted: false, scope: "once" as const, reason: "denied in VS Code" }
      : { granted: true, scope: choice === "once" ? "once" as const : "action_class" as const };
    if (!this.approvals.answer(tabId, requestId, answer)) {
      this.retireQuestion(tabId, requestId,
        "that question belongs to a turn whose process is gone, so this answer cannot reach it");
      return;
    }
    this.tabs.update(tabId, { status: "running" });
    this.postTabs();
    const runtime = this.runtimeFor(tabId);
    runtime.question = undefined;
    runtime.transcript.updateApproval(requestId, { state: answer.granted ? "allowed" : "denied" });
    const updated = runtime.transcript.all().find((entry) =>
      entry.kind === "approval" && entry.requestId === requestId);
    if (updated !== undefined) this.post({ type: "part", tabId, part: updated });
  }

  /**
   * Retire a card nothing is listening for, and say so once.
   *
   * Retiring is not answering: the card keeps its place in the record, drops its
   * buttons, and the transcript carries one line. This is the rule the TUI already
   * follows for a question the kernel does not confirm — and like the TUI, the
   * notice is advice rather than a protocol id, which belongs in the output channel.
   */
  private retireQuestion(tabId: string, requestId: string, why: string): void {
    const runtime = this.runtimeFor(tabId);
    const part = runtime.transcript.all().find((entry) =>
      entry.kind === "approval" && entry.requestId === requestId);
    if (part === undefined || part.kind !== "approval" || part.state === "expired") return;
    runtime.transcript.updateApproval(requestId, { state: "expired" });
    const updated = runtime.transcript.all().find((entry) =>
      entry.kind === "approval" && entry.requestId === requestId);
    if (updated !== undefined) this.post({ type: "part", tabId, part: updated });
    const notice: UiPart = {
      kind: "notice",
      level: "warn",
      text: "A question from an earlier turn can no longer be answered, so it is marked "
        + "expired. Send your prompt again and it will be asked afresh.",
    };
    runtime.transcript.append(notice);
    this.post({ type: "part", tabId, part: notice });
    this.output.appendLine(`retired: ${requestId} — ${why}`);
  }

  private recordPendingTool(
    tabId: string,
    call: ToolCall,
    review: TurnReview,
    workspace: string,
  ): void {
    const runtime = this.runtimeFor(tabId);
    const args = call.args;
    runtime.pendingTool = { callId: call.call_id };
    if (args === null || typeof args !== "object" || Array.isArray(args)) return;
    const raw = (args as Record<string, unknown>).path;
    if (typeof raw !== "string") return;
    const path = isAbsolute(raw) ? resolve(raw) : resolve(workspace, raw);
    if (!isInside(workspace, path)) return;
    if (call.tool === "write_file" || call.tool === "edit") runtime.unreviewed.add(call.call_id);
    review.snapshots.see([path]);
    const snapshot = review.snapshots.left(path);
    if (snapshot !== undefined && snapshot.source !== "snapshot" && snapshot.note !== "new file") {
      runtime.pendingTool = { callId: call.call_id, tool: call.tool, path };
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
    runtime.pendingTool = { callId: call.call_id, tool: call.tool, path, preview };
  }

  private showApproval(tabId: string, asked: ApprovalRequested, review: TurnReview): void {
    // Register the waiter before the card is visible: a fast click must resolve
    // the same promise `decide` will await, not be lost between them.
    this.pendingAnswers.set(`${tabId}:${asked.request_id}`, this.approvals.wait(tabId, asked.request_id));
    this.runtimeFor(tabId).question = asked.request_id;
    this.tabs.update(tabId, { status: "waiting" });
    this.postTabs();
    const runtime = this.runtimeFor(tabId);
    const pending = runtime.pendingTool;
    if (pending !== undefined) runtime.unreviewed.delete(pending.callId);
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
      runtime.transcript.append(warning);
      this.post({ type: "part", tabId, part: warning });
    } else if (path !== undefined && pending?.tool !== undefined
      && (pending.tool === "write_file" || pending.tool === "edit")) {
      const warning: UiPart = {
        kind: "notice",
        level: "warn",
        text: "preview unavailable: the file could not be read; the approval still applies",
      };
      runtime.transcript.append(warning);
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
    runtime.transcript.append(part);
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

  /**
   * Stop the turn — by answering the question first, when there is one.
   *
   * A turn parked on an approval is ended by the *answer*, not by the interrupt:
   * the kernel's `SIGINT` only sets a flag the loop reads, and a parked loop is
   * not reading anything. Denying and interrupting at once therefore races, and
   * when the interrupt wins the kernel keeps the question in memory for good —
   * every later prompt is refused with "approval … is still pending", there is no
   * card left to click, and Stop has nothing to stop. So: answer, don't interrupt.
   */
  private cancel(tabId: string): void {
    const runtime = this.runtimeFor(tabId);
    const question = runtime.question;
    if (question !== undefined) {
      this.approvals.denyTab(tabId, "stopped by the person");
      this.output.appendLine(`turn stop: answered ${question} with deny — the answer ends the turn`);
      return;
    }
    if (!runtime.running) {
      this.output.appendLine("turn stop: nothing is running in this tab");
      return;
    }
    this.output.appendLine("turn running: the person asked to stop");
    this.approvals.denyTab(tabId, "stopped by the person");
    runtime.agent?.cancel();
  }

  private postError(message: string, tabId?: string): void {
    this.post(tabId === undefined
      ? { type: "error", message }
      : { type: "error", tabId, message });
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
