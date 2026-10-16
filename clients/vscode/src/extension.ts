import * as vscode from "vscode";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { basename, relative } from "node:path";
import { DiffProvider } from "./diffProvider.ts";
import { promptText } from "./prompt.ts";
import { pathsInArgs, Renderer } from "./render.ts";
import { FileSnapshots } from "./snapshots.ts";
import { TurnBusyError } from "./turn.ts";
import { WorkspaceAgent } from "./workspaceAgent.ts";
import { EVENTS } from "../../ts/protocol.generated.ts";
import type { DiffUris } from "./diffProvider.ts";
import type { UiPart } from "./render.ts";
import type { JsonValue } from "../../ts/protocol.generated.ts";
import type { ApprovalAnswer, ApprovalRequest } from "./turn.ts";
import type { LoongSession } from "./session.ts";
import type { TurnController } from "./turn.ts";

const PARTICIPANT_ID = "loongagent.chat";
const SMOKE_PROMPT = "Say hello in one sentence, then stop.";
const NOT_YET = "not implemented yet";
/** The private URI scheme whose documents are a turn's "before" side. */
const LEFT_SCHEME = "loongagent-left";

interface Review {
  workspace: string;
  snapshots: FileSnapshots;
  provider: DiffProvider<vscode.Uri>;
}

/**
 * The window's turn-review state.
 *
 * One per window rather than one per folder: the participant works on the first
 * workspace folder (spec §4.6), and the path inside the URI is what identifies a
 * left side, so there is nothing to keep per folder.
 */
let review: Review | undefined;

function reviewFor(folder: vscode.WorkspaceFolder): Review {
  if (review !== undefined && review.workspace === folder.uri.fsPath) return review;
  const snapshots = new FileSnapshots({ workspace: folder.uri.fsPath });
  const uris: DiffUris<vscode.Uri> = {
    make: (path, label) => vscode.Uri.from({ scheme: LEFT_SCHEME, path, query: label }),
    pathOf: (uri) => uri.path,
  };
  review = {
    workspace: folder.uri.fsPath,
    snapshots,
    provider: new DiffProvider(snapshots, gitShowIn(folder), uris),
  };
  return review;
}

/** `git show HEAD:<path>`, or nothing at all — no git, no HEAD or untracked are all fine. */
function gitShowIn(folder: vscode.WorkspaceFolder): (path: string) => string | undefined {
  return (path) => {
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
  };
}

function readNow(path: string): string | undefined {
  try {
    return readFileSync(path, "utf8");
  } catch {
    return undefined;
  }
}

async function openDiffs(paths: string[] | undefined, output: vscode.OutputChannel): Promise<void> {
  if (paths === undefined || paths.length === 0) {
    output.appendLine("review: nothing to review — ask for a change first, then use the button in the reply");
    return;
  }
  for (const path of paths) {
    const left = review === undefined ? vscode.Uri.file(path) : review.provider.uriFor(path, "before");
    await vscode.commands.executeCommand(
      "vscode.diff",
      left,
      vscode.Uri.file(path),
      `${basename(path)} (before ↔ after)`,
    );
  }
}

export function activate(context: vscode.ExtensionContext): void {
  const output = vscode.window.createOutputChannel("loongagent");
  context.subscriptions.push(output);
  output.appendLine("loongagent: activated");

  const agents = new Map<string, WorkspaceAgent>();
  const agentFor = (folder: vscode.WorkspaceFolder): WorkspaceAgent => {
    const key = folder.uri.toString();
    const existing = agents.get(key);
    if (existing !== undefined) return existing;
    const made = new WorkspaceAgent({ folder, output, context });
    agents.set(key, made);
    // The child dies with the extension: a window that closes must not leave a
    // server behind, and neither must a reload.
    context.subscriptions.push({ dispose: () => made.dispose() });
    return made;
  };

  const participant = vscode.chat.createChatParticipant(
    PARTICIPANT_ID,
    (request, _chat, stream, token) => handleRequest(request, stream, token, output, agentFor),
  );
  // Two, and only two: a menu nobody reads is worse than no menu. The diffs are
  // the button above, because a follow-up cannot carry the turn's file list.
  participant.followupProvider = {
    provideFollowups: () => [
      { prompt: "Explain what you just did, and why.", label: "Explain this" },
      { prompt: "Review the changes you just made and tell me what to check.", label: "Review the changes" },
    ],
  };
  context.subscriptions.push(participant);

  context.subscriptions.push(
    vscode.workspace.onDidGrantWorkspaceTrust(() =>
      output.appendLine("ready: the workspace is trusted now; the agent starts on the next request")),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("loongagent.smoke", () => runSmoke(output, context)),
  );

  // The left pane of every diff this window opens. Registered once; the path in
  // the URI says which file it is about.
  context.subscriptions.push(
    vscode.workspace.registerTextDocumentContentProvider(LEFT_SCHEME, {
      provideTextDocumentContent: (uri) =>
        review === undefined
          ? "[loongagent] no turn has run in this window yet\n"
          : review.provider.provideTextDocumentContent(uri),
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("loongagent.reviewChanges", (paths?: string[]) =>
      openDiffs(paths, output)),
  );
}

export function deactivate(): void {}

// ------------------------------------------------------------------ the participant

async function handleRequest(
  request: vscode.ChatRequest,
  stream: vscode.ChatResponseStream,
  token: vscode.CancellationToken,
  output: vscode.OutputChannel,
  agentFor: (folder: vscode.WorkspaceFolder) => WorkspaceAgent,
): Promise<void> {
  const folder = vscode.workspace.workspaceFolders?.[0];
  if (folder === undefined) {
    stream.markdown("Open a folder first — the agent works on a workspace.");
    return;
  }
  if (!vscode.workspace.isTrusted) {
    stream.markdown(
      "This workspace is not trusted, so I will not start the agent here. " +
      "Trust the folder (Workspaces: Manage Workspace Trust) and ask again.",
    );
    return;
  }

  const agent = agentFor(folder);
  switch (request.command) {
    case "new":
      return startFreshSession(agent, stream, output);
    case "yolo":
      return toggleAutoApprove(agent, stream, output, folder);
    // eslint-disable-next-line no-fallthrough
    case "model":
      return chooseModel(agent, stream, output);
    case "context":
      return showContext(agent, stream, output);
    default:
      return runTurn(request, stream, token, output, agent, folder);
  }
}

async function startFreshSession(
  agent: WorkspaceAgent,
  stream: vscode.ChatResponseStream,
  output: vscode.OutputChannel,
): Promise<void> {
  try {
    // Through the agent, so the new id is remembered for the next window.
    const id = await agent.startFresh();
    output.appendLine(`ready: new session ${id}`);
    stream.markdown(`Started a new session: \`${id}\`.`);
  } catch (error) {
    stream.markdown(`I could not start a new session: ${explain(error)}`);
    output.appendLine(`failed: ${explain(error)}`);
  }
}

/**
 * The human's answer to a policy question, in the protocol's own terms.
 *
 * A dismissed modal is a denial, never an allow: closing a dialog is not
 * consent, and reading it as consent is how a guard becomes decorative.
 *
 * `project` scope is deliberately not on the menu (spec §4.4, §8): three buttons
 * is already the limit of what a modal can carry honestly, and "the whole
 * project, forever" is not a thing to slip into a stray click.
 */
export async function decideFromModal(request: ApprovalRequest): Promise<ApprovalAnswer> {
  const answer = await vscode.window.showWarningMessage(
    `loongagent wants to act: ${request.action_class} — ${request.reason}`,
    { modal: true },
    "Allow once",
    "Allow always",
    "Deny",
  );
  if (answer === "Allow once") return { granted: true, scope: "once" };
  if (answer === "Allow always") return { granted: true, scope: "action_class" };
  return { granted: false, scope: "once", reason: "denied in VS Code" };
}

async function toggleAutoApprove(
  agent: WorkspaceAgent,
  stream: vscode.ChatResponseStream,
  output: vscode.OutputChannel,
  folder: vscode.WorkspaceFolder,
): Promise<void> {
  try {
    const session = await agent.ensure();
    const current = agent.autoApprove ?? vscode.workspace
      .getConfiguration("loongagent", folder.uri)
      .get<boolean>("autoApprove", false);
    const state = await session.setAutoApprove(!current);
    await agent.rememberAutoApprove(state);
    output.appendLine(`ready: approval prompts ${state ? "off" : "on"} for this session`);
    stream.markdown(state
      ? "Approval prompts are **off** for this session. The sandbox is untouched — only the *asking* "
        + "stopped, and the session records it (`permissions.relaxed`)."
      : "Approval prompts are **on** again for this session. The sandbox was never changed.");
  } catch (error) {
    stream.markdown(`I could not change the approval setting: ${explain(error)}`);
    output.appendLine(`failed: ${explain(error)}`);
  }
}

async function chooseModel(
  agent: WorkspaceAgent,
  stream: vscode.ChatResponseStream,
  output: vscode.OutputChannel,
): Promise<void> {
  try {
    const session = await agent.ensure();
    const before = await session.models();
    const picked = await vscode.window.showQuickPick(
      before.models.map((offering) => ({
        label: offering.name,
        description: offering.model,
        picked: offering.name === before.selected,
      })),
      { title: "Model for this session" },
    );
    if (picked === undefined) return;
    emit(stream, new Renderer({ cwd: "" }).models(await session.selectModel(picked.label)));
  } catch (error) {
    stream.markdown(`I could not change the model: ${explain(error)}`);
    output.appendLine(`failed: ${explain(error)}`);
  }
}

async function showContext(
  agent: WorkspaceAgent,
  stream: vscode.ChatResponseStream,
  output: vscode.OutputChannel,
): Promise<void> {
  try {
    const session = await agent.ensure();
    emit(stream, new Renderer({ cwd: "" }).context(await session.context()));
  } catch (error) {
    stream.markdown(`I could not read the context: ${explain(error)}`);
    output.appendLine(`failed: ${explain(error)}`);
  }
}

async function runTurn(
  request: vscode.ChatRequest,
  stream: vscode.ChatResponseStream,
  token: vscode.CancellationToken,
  output: vscode.OutputChannel,
  agent: WorkspaceAgent,
  folder: vscode.WorkspaceFolder,
): Promise<void> {
  let session: LoongSession;
  try {
    session = await agent.ensure();
  } catch (error) {
    stream.markdown(startupAdvice(explain(error)));
    output.appendLine(`failed: ${explain(error)}`);
    return;
  }

  const renderer = new Renderer({ cwd: folder.uri.fsPath });
  renderer.beginTurn();
  const { snapshots, provider } = reviewFor(folder);
  snapshots.clear();
  // A reopened session says where you were, once per activation: the chat's
  // transcript belongs to the window, the session belongs to the log.
  const resume = agent.takeResumeEvents();
  if (resume !== undefined) emit(stream, renderer.resumed(resume));
  const previous = agent.takeLastExit();
  if (previous !== undefined) {
    stream.markdown(
      `\n(The previous loongagent server exited: ${previous.signal ?? `code ${previous.code}`} ` +
      "— started a new one.)\n",
    );
  }
  const from = session.events.length;
  let controller: TurnController;
  try {
    controller = await agent.controller({
      onEvent: (event) => {
        // Before the tool runs: that timing is what makes the recorded left side
        // the pre-turn content even when the tree was already dirty (spec §4.5).
        if (event.kind === EVENTS.toolCall) {
          const args = (event.data as { args?: JsonValue }).args;
          if (args !== undefined) snapshots.see(pathsInArgs(args));
        }
        emit(stream, renderer.event(event));
      },
      onDelta: (text) => emit(stream, renderer.delta(text)),
      // The person answers, in the editor, while the turn waits.
      decide: decideFromModal,
    });
  } catch (error) {
    stream.markdown(startupAdvice(explain(error)));
    output.appendLine(`failed: ${explain(error)}`);
    return;
  }

  // Stop is the person's; it is a signal to the child, and the child survives it.
  const cancellation = token.onCancellationRequested(() => {
    output.appendLine("turn running: the person asked to stop");
    session.cancel();
  });

  output.appendLine("turn running");
  try {
    await controller.run(promptText(request.prompt, request.references));
    const changed = provider.changedAmong(renderer.touchedFiles(), readNow);
    if (changed.length > 0) {
      stream.button({
        command: "loongagent.reviewChanges",
        title: `Review ${changed.length} changed file${changed.length === 1 ? "" : "s"}`,
        arguments: [changed],
      });
    }
    const interrupted = session.events
      .slice(from)
      .filter((event) => event.kind === EVENTS.turnInterrupted)
      .pop();
    if (interrupted !== undefined) {
      const reason = (interrupted.data as { reason?: string }).reason ?? "no reason recorded";
      stream.markdown(`\nInterrupted — ${reason}.`);
    }
  } catch (error) {
    if (error instanceof TurnBusyError) {
      stream.markdown("A turn is already running. Wait for it to finish, or press Stop.");
    } else {
      stream.markdown(`The turn failed: ${explain(error)}\n\nSee the **loongagent** output channel for the detail.`);
      output.appendLine(`failed: ${explain(error)}`);
    }
  } finally {
    cancellation.dispose();
  }
}

function emit(stream: vscode.ChatResponseStream, parts: UiPart[]): void {
  for (const part of parts) {
    if (part.kind === "markdown") stream.markdown(part.text);
    else stream.progress(part.text);
  }
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

// ------------------------------------------------------------------ spec step 0

/**
 * Spec step 0: prove that this extension can reach the real kernel.
 *
 * It spawns the server the same way the participant does, attaches, runs one
 * throwaway turn that denies everything, and prints the stream. When the link
 * is broken, the output channel says where.
 */
async function runSmoke(output: vscode.OutputChannel, context: vscode.ExtensionContext): Promise<void> {
  const folder = vscode.workspace.workspaceFolders?.[0];
  if (folder === undefined) {
    output.appendLine("smoke: open a folder first — the agent works on a workspace");
    return;
  }
  const agent = new WorkspaceAgent({ folder, output, context });
  try {
    const session = await agent.ensure();
    const controller = await agent.controller({
      onEvent: (event) => output.appendLine(`event: ${event.kind}`),
      onDelta: (text) => output.append(text),
      decide: async (asked) => {
        output.appendLine(`smoke: the policy asked about ${asked.action_class} — ${asked.reason} (denied)`);
        return { granted: false, scope: "once", reason: "the smoke test denies everything" };
      },
    });
    const result = await controller.run(SMOKE_PROMPT);
    output.appendLine(`\nsmoke: turn ${result.turn_id} finished (accepted: ${result.accepted})`);
  } catch (error) {
    output.appendLine(`smoke failed: ${explain(error)}`);
  } finally {
    agent.dispose();
  }
}
