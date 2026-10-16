import * as vscode from "vscode";
import { spawnAgent } from "./agentSession.ts";
import { LoongSession } from "./session.ts";
import { TurnController } from "./turn.ts";
import { EVENTS } from "../../ts/protocol.generated.ts";
import type { AgentSession } from "./agentSession.ts";
import type { Decider } from "./turn.ts";
import type { EventEnvelope } from "../../ts/protocol.generated.ts";

/** Where this workspace's session id is remembered between windows. */
const SESSION_KEY = "loongagent.sessionId";
/** Whether this workspace last chose to stop being asked (ADR-0007 §5). */
const AUTO_APPROVE_KEY = "loongagent.autoApprove";

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
  private announced = false;

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

  /** The last answer to "stop asking?", or nothing if this workspace never answered. */
  get autoApprove(): boolean | undefined {
    return this.context.workspaceState.get<boolean>(AUTO_APPROVE_KEY);
  }

  async rememberAutoApprove(on: boolean): Promise<void> {
    await this.context.workspaceState.update(AUTO_APPROVE_KEY, on);
  }

  /** What a new session should start with: the workspace's answer, then the setting. */
  private preferredAutoApprove(): boolean {
    const remembered = this.autoApprove;
    if (remembered !== undefined) return remembered;
    return vscode.workspace
      .getConfiguration("loongagent", this.folder.uri)
      .get<boolean>("autoApprove", false);
  }

  /**
   * Spawn if needed and attach; resolves to the live session.
   *
   * A session id stored from a previous window is resumed — and if it no longer
   * resolves (the log was deleted, or it belongs to another checkout), that is
   * said out loud and a new session is started **once**. Retrying a name the
   * server refuses is how a client turns a bad id into a loop.
   */
  async ensure(): Promise<LoongSession> {
    if (this.live !== undefined) return this.live;
    const stored = this.storedSessionId();
    try {
      return await this.open(stored);
    } catch (error) {
      if (stored === undefined) throw error;
      this.output.appendLine(
        `the stored session ${stored} could not be resumed (${explain(error)}); starting a new one`,
      );
      await this.context.workspaceState.update(SESSION_KEY, undefined);
      return this.open(undefined);
    }
  }

  /** A brand new session, remembered so the next window resumes it. */
  async startFresh(): Promise<string> {
    const session = await this.ensure();
    const id = await session.newSession();
    await this.context.workspaceState.update(SESSION_KEY, id);
    return id;
  }

  /**
   * The replayed events a reopened session should show, once per activation.
   *
   * Once, because the card says where you were: repeating it on every turn
   * would be the surface narrating the same history forever.
   */
  takeResumeEvents(): EventEnvelope[] | undefined {
    if (this.announced) return undefined;
    this.announced = true;
    const events = this.live?.events ?? [];
    return events.length === 0 ? undefined : events;
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

  private storedSessionId(): string | undefined {
    const stored = this.context.workspaceState.get<string>(SESSION_KEY);
    return stored === undefined || stored === "" ? undefined : stored;
  }

  private async open(stored: string | undefined): Promise<LoongSession> {
    const cwd = this.folder.uri.fsPath;
    const configuration = vscode.workspace.getConfiguration("loongagent", this.folder.uri);
    const command = configuration.get<string>("path", "loongagent");
    const model = configuration.get<string>("model", "");
    const extra = configuration.get<string[]>("extraArgs", []);
    // `--session <id>` resumes; `--new` says a fresh one is wanted on purpose,
    // rather than letting the CLI pick whatever is newest in this project.
    const args = ["server", "--stdio", "--cwd", cwd];
    if (model !== "") args.push("--model", model);
    args.push(...extra);
    if (stored === undefined) args.push("--new");
    else args.push("--session", stored);

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
    await this.context.workspaceState.update(SESSION_KEY, attached.session_id);
    this.output.appendLine(
      `ready: session ${attached.session_id} (schema ${attached.schema_version}, ` +
      `${attached.existing_events} events already)`,
    );
    // A workspace that chose to stop being asked comes back that way — and the
    // session records it, so the log still explains itself.
    if (this.preferredAutoApprove()) {
      await session.setAutoApprove(true);
      this.output.appendLine("ready: approval prompts are off for this session (permissions.relaxed)");
    }
    return session;
  }
}

function explain(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error);
}
