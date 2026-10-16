import * as vscode from "vscode";
import { spawnAgent } from "./agentSession.ts";
import { LoongSession } from "./session.ts";
import { TurnController } from "./turn.ts";
import { EVENTS } from "../../ts/protocol.generated.ts";
import type { AgentSession } from "./agentSession.ts";
import type { Decider } from "./turn.ts";
import type { EventEnvelope } from "../../ts/protocol.generated.ts";

/**
 * The agent for one workspace folder: the child process, the session, and the
 * process's lifecycle as the output channel tells it.
 *
 * It holds no policy and renders nothing. It spawns the same binary the TUI and
 * the browser talk to, in the same way, and gives the participant something to
 * run turns on.
 */
export class WorkspaceAgent {
  private readonly folder: vscode.WorkspaceFolder;
  private readonly output: vscode.OutputChannel;
  private readonly context: vscode.ExtensionContext;
  private agent: AgentSession | undefined;
  private live: LoongSession | undefined;

  constructor(options: {
    folder: vscode.WorkspaceFolder;
    output: vscode.OutputChannel;
    context: vscode.ExtensionContext;
  }) {
    this.folder = options.folder;
    this.output = options.output;
    this.context = options.context;
  }

  get sessionId(): string | undefined {
    return this.live?.sessionId;
  }

  /** Spawn if needed and attach; resolves to the live session. */
  async ensure(): Promise<LoongSession> {
    if (this.live !== undefined) return this.live;
    const cwd = this.folder.uri.fsPath;
    const configuration = vscode.workspace.getConfiguration("loongagent", this.folder.uri);
    const command = configuration.get<string>("path", "loongagent");
    const model = configuration.get<string>("model", "");
    const extra = configuration.get<string[]>("extraArgs", []);
    // Task 8 adds `--session <stored id>` here; until then every window starts fresh.
    const args = ["server", "--stdio", "--cwd", cwd];
    if (model !== "") args.push("--model", model);
    args.push(...extra);

    this.output.appendLine(`spawning: ${command} ${args.join(" ")}`);
    const agent = spawnAgent(command, args, cwd, {
      onNotification: () => {},
      onLog: (line) => this.output.appendLine(`[server] ${line}`),
      onExit: (code, signal) => {
        this.output.appendLine(`stopped: the loongagent server exited (${signal ?? `code ${code}`})`);
        // Only clear what this child owned: a respawn must not be forgotten by
        // the exit of the process it replaced.
        if (this.agent === agent) {
          this.agent = undefined;
          this.live = undefined;
        }
      },
    });

    const session = new LoongSession(agent);
    let attached;
    try {
      attached = await session.attach(cwd);
    } catch (error) {
      // A child that never attached is of no use to anyone; do not keep it.
      await agent.dispose();
      throw error;
    }
    this.agent = agent;
    this.live = session;
    this.output.appendLine(
      `ready: session ${attached.session_id} (schema ${attached.schema_version}, ` +
      `${attached.existing_events} events already)`,
    );
    return session;
  }

  /** A controller bound to this agent's session and the caller's hooks. */
  async controller(hooks: {
    onEvent: (event: EventEnvelope) => void;
    onDelta: (text: string) => void;
    decide: Decider;
  }): Promise<TurnController> {
    const session = await this.ensure();
    return new TurnController(session, {
      onEvent: (event) => {
        if (event.kind === EVENTS.approvalRequested) this.output.appendLine("awaiting approval");
        hooks.onEvent(event);
      },
      onDelta: hooks.onDelta,
      decide: hooks.decide,
    });
  }

  dispose(): void {
    const agent = this.agent;
    this.agent = undefined;
    this.live = undefined;
    if (agent !== undefined) {
      this.output.appendLine("stopped: disposing the loongagent server");
      void agent.dispose();
    }
  }
}
