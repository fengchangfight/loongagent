import * as vscode from "vscode";
import { promptText } from "./prompt.ts";
import { Renderer } from "./render.ts";
import { TurnBusyError } from "./turn.ts";
import { WorkspaceAgent } from "./workspaceAgent.ts";
import { EVENTS } from "../../ts/protocol.generated.ts";
import type { UiPart } from "./render.ts";
import type { ApprovalAnswer, ApprovalRequest } from "./turn.ts";
import type { LoongSession } from "./session.ts";
import type { TurnController } from "./turn.ts";

const PARTICIPANT_ID = "loongagent.chat";
const SMOKE_PROMPT = "Say hello in one sentence, then stop.";
const NOT_YET = "not implemented yet";

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

  context.subscriptions.push(
    vscode.chat.createChatParticipant(PARTICIPANT_ID, (request, _chat, stream, token) =>
      handleRequest(request, stream, token, output, agentFor)),
  );

  context.subscriptions.push(
    vscode.workspace.onDidGrantWorkspaceTrust(() =>
      output.appendLine("ready: the workspace is trusted now; the agent starts on the next request")),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("loongagent.smoke", () => runSmoke(output, context)),
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
    case "model":
    case "context":
      stream.markdown(`/${request.command} is ${NOT_YET}.`);
      return;
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
  // A reopened session says where you were, once per activation: the chat's
  // transcript belongs to the window, the session belongs to the log.
  const resume = agent.takeResumeEvents();
  if (resume !== undefined) emit(stream, renderer.resumed(resume));
  const from = session.events.length;
  let controller: TurnController;
  try {
    controller = await agent.controller({
      onEvent: (event) => emit(stream, renderer.event(event)),
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
