import * as vscode from "vscode";
import { spawnAgent } from "./agentSession.ts";
import { withStoredSession } from "./resume.ts";
import { LoongSession } from "./session.ts";
import { TurnController, TurnGate } from "./turn.ts";
import { EVENTS } from "../../ts/protocol.generated.ts";
import type { AgentSession } from "./agentSession.ts";
import type { TurnSink } from "./turn.ts";
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
  private gateInstance: TurnGate | undefined;
  private opening: Promise<LoongSession> | undefined;
  private resumed = false;
  private agent: AgentSession | undefined;
  private live: LoongSession | undefined;
  private announced = false;
  private lastExit: { code: number | null; signal: string | null } | undefined;

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
    // One child per cold start: two chat tabs opened at once would otherwise each
    // spawn a server, attach to its own new session, and orphan one of them.
    if (this.opening !== undefined) return this.opening;
    this.opening = this.resumeOrStart();
    try {
      return await this.opening;
    } finally {
      this.opening = undefined;
    }
  }

  private async resumeOrStart(): Promise<LoongSession> {
    return withStoredSession(
      this.storedSessionId(),
      (id) => this.open(id),
      async (id, error) => {
        this.output.appendLine(
          `the stored session ${id} could not be resumed (${explain(error)}); starting a new one`,
        );
        await this.context.workspaceState.update(SESSION_KEY, undefined);
      },
    );
  }

  /** A brand new session, remembered so the next window resumes it. */
  async startFresh(): Promise<string> {
    const session = await this.ensure();
    const id = await session.newSession();
    await this.context.workspaceState.update(SESSION_KEY, id);
    return id;
  }

  /** How the previous child died, once, so the next reply can say why it restarted. */
  takeLastExit(): { code: number | null; signal: string | null } | undefined {
    const exit = this.lastExit;
    this.lastExit = undefined;
    return exit;
  }

  /**
   * Where a *reopened* session was, once per activation.
   *
   * Gated on having actually resumed (`--session`), not on the log being
   * non-empty: a brand-new session already carries its own first event, so
   * "resumed" would be a false claim on the first prompt of every session.
   */
  takeResume(): { sessionId: string; events: EventEnvelope[] } | undefined {
    if (this.announced || !this.resumed) return undefined;
    this.announced = true;
    const sessionId = this.live?.sessionId;
    const events = this.live?.events ?? [];
    if (sessionId === undefined || events.length === 0) return undefined;
    return { sessionId, events };
  }

  /**
   * Take the session's turn for this request, or throw `TurnBusyError`.
   *
   * One gate per session, so the busy check is about the session and not about
   * this call: two chat tabs cannot both hold the turn.
   */
  async beginTurn(sink: TurnSink): Promise<TurnController> {
    const session = await this.ensure();
    if (this.gateInstance === undefined) this.gateInstance = new TurnGate(session);
    this.output.appendLine("turn running");
    return this.gateInstance.begin(sink);
  }

  /** Give the turn back. */
  endTurn(): void {
    this.gateInstance?.end();
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
    this.resumed = stored !== undefined;

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
          this.lastExit = { code, signal };
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
