import * as vscode from "vscode";
import { spawnAgent } from "./agentSession.ts";
import { LoongSession } from "./session.ts";
import { TurnController } from "./turn.ts";

const SMOKE_PROMPT = "Say hello in one sentence, then stop.";

export function activate(context: vscode.ExtensionContext): void {
  const output = vscode.window.createOutputChannel("loongagent");
  context.subscriptions.push(output);
  output.appendLine("loongagent: activated");

  context.subscriptions.push(
    vscode.commands.registerCommand("loongagent.smoke", () => runSmoke(output)),
  );
}

export function deactivate(): void {}

/**
 * Spec step 0: prove that this extension can reach the real kernel.
 *
 * It spawns the server the same way the participant will, attaches, runs one
 * throwaway turn that denies everything, and prints the stream. When the link
 * is broken, the output channel says where.
 */
async function runSmoke(output: vscode.OutputChannel): Promise<void> {
  const folder = vscode.workspace.workspaceFolders?.[0];
  if (folder === undefined) {
    output.appendLine("smoke: open a folder first — the agent works on a workspace");
    return;
  }
  const configuration = vscode.workspace.getConfiguration("loongagent");
  const command = configuration.get<string>("path", "loongagent");
  const model = configuration.get<string>("model", "");
  const extra = configuration.get<string[]>("extraArgs", []);
  const args = ["server", "--stdio", "--cwd", folder.uri.fsPath];
  if (model !== "") args.push("--model", model);
  args.push(...extra);
  output.appendLine(`smoke: ${command} ${args.join(" ")}`);

  const agent = spawnAgent(command, args, folder.uri.fsPath, {
    onNotification: () => {},
    onLog: (line) => output.appendLine(`[server] ${line}`),
    onExit: (code, signal) =>
      output.appendLine(`smoke: the server exited (${signal ?? `code ${code}`})`),
  });

  try {
    const session = new LoongSession(agent);
    const attached = await session.attach(folder.uri.fsPath);
    output.appendLine(
      `smoke: attached to session ${attached.session_id} ` +
      `(schema ${attached.schema_version}, ${attached.existing_events} events already)`,
    );

    const controller = new TurnController(session, {
      onEvent: (event) => output.appendLine(`event: ${event.kind}`),
      onDelta: (text) => output.append(text),
      decide: async (request) => {
        output.appendLine(
          `smoke: the policy asked about ${request.action_class} — ${request.reason} (denied)`,
        );
        return { granted: false, scope: "once", reason: "the smoke test denies everything" };
      },
    });

    const result = await controller.run(SMOKE_PROMPT);
    output.appendLine(`\nsmoke: turn ${result.turn_id} finished (accepted: ${result.accepted})`);
  } catch (error) {
    output.appendLine(`smoke failed: ${explain(error)}`);
  } finally {
    await agent.dispose();
  }
}

function explain(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error);
}
