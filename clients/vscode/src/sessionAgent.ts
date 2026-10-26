import * as vscode from "vscode";
import { spawnAgent } from "./agentSession.ts";
import { withStoredSession } from "./resume.ts";
import { LoongSession } from "./session.ts";
import { SessionCore } from "./sessionCore.ts";
import type { AgentSession } from "./agentSession.ts";
import type { TurnController, TurnSink } from "./turn.ts";
import type { EventEnvelope } from "../../ts/protocol.generated.ts";

/** Where this workspace's resumed session id is remembered between windows. */
const SESSION_KEY = "loongagent.sessionId";
/** Whether this workspace last chose to stop being asked (ADR-0007 §5). */
const AUTO_APPROVE_KEY = "loongagent.autoApprove";

/**
 * One tab's agent: the child process, the session, and the turn gate.
 *
 * It holds no policy and renders nothing. `sessionId` attaches exactly that
 * session (a history or restored tab) and throws when it cannot; `fresh` starts
 * a new one; neither uses the workspace's remembered id with one fallback.
 */
export class SessionAgent {
  private readonly folder: vscode.WorkspaceFolder;
  private readonly output: vscode.OutputChannel;
  private readonly context: vscode.ExtensionContext;
  private readonly binaryPath: string;
  private readonly fixedSessionId: string | undefined;
  private readonly fresh: boolean;
  private readonly core = new SessionCore();
  private opening: Promise<LoongSession> | undefined;
  private resumed = false;
  private agent: AgentSession | undefined;
  private announced = false;
  private lastExit: { code: number | null; signal: string | null } | undefined;
  private exitObserver: ((code: number | null, signal: string | null) => void) | undefined;

  constructor(options: {
    folder: vscode.WorkspaceFolder;
    output: vscode.OutputChannel;
    context: vscode.ExtensionContext;
    binaryPath: string;
    sessionId?: string;
    fresh?: boolean;
  }) {
    this.folder = options.folder;
    this.output = options.output;
    this.context = options.context;
    this.binaryPath = options.binaryPath;
    this.fixedSessionId = options.sessionId;
    this.fresh = options.fresh === true;
  }

  get sessionId(): string | undefined {
    return this.core.session?.sessionId;
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
   * A fixed session id is attached exactly and a failure propagates. With
   * neither a fixed id nor a fresh request, the workspace's remembered id is
   * tried once and a refusal starts a new session with a word said out loud.
   */
  async ensure(): Promise<LoongSession> {
    const live = this.core.session;
    if (live !== undefined) return live;
    if (this.opening !== undefined) return this.opening;
    this.opening = this.openInitial();
    try {
      return await this.opening;
    } finally {
      this.opening = undefined;
    }
  }

  private async openInitial(): Promise<LoongSession> {
    if (this.fixedSessionId !== undefined) return this.open(this.fixedSessionId);
    if (this.fresh) return this.open(undefined);
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

  /** A brand new session in this tab. */
  async startNew(): Promise<string> {
    const agent = this.agent;
    this.agent = undefined;
    this.core.forget();
    if (agent !== undefined) await agent.dispose();
    this.opening = this.open(undefined);
    try {
      const session = await this.opening;
      return session.sessionId ?? "";
    } finally {
      this.opening = undefined;
    }
  }

  /** Called once when the live child exits; lets the host cancel a waiting approval. */
  setExitObserver(observer: (code: number | null, signal: string | null) => void): void {
    this.exitObserver = observer;
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
   * non-empty: a brand-new session already carries its own first event.
   */
  takeResume(): { sessionId: string; events: EventEnvelope[] } | undefined {
    if (this.announced || !this.resumed) return undefined;
    this.announced = true;
    const sessionId = this.core.session?.sessionId;
    const events = this.core.session?.events ?? [];
    if (sessionId === undefined || events.length === 0) return undefined;
    return { sessionId, events };
  }

  /** Take the session's turn, or throw `TurnBusyError` without touching the sink. */
  async beginTurn(sink: TurnSink): Promise<TurnController> {
    await this.ensure();
    const gate = this.core.turnGate;
    if (gate === undefined) throw new Error("the session has no turn gate");
    this.output.appendLine("turn running");
    return gate.begin(sink);
  }

  /** Give the turn back. */
  endTurn(): void {
    this.core.turnGate?.end();
  }

  /** Stop the running turn; the child survives. */
  cancel(): void {
    this.core.session?.cancel();
  }

  dispose(): void {
    const agent = this.agent;
    this.agent = undefined;
    this.core.forget();
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
    const model = configuration.get<string>("model", "");
    const extra = configuration.get<string[]>("extraArgs", []);
    const args = ["server", "--stdio", "--cwd", cwd];
    if (model !== "") args.push("--model", model);
    args.push(...extra);
    if (stored === undefined) args.push("--new");
    else args.push("--session", stored);
    this.resumed = stored !== undefined;

    this.output.appendLine(`spawning: ${this.binaryPath} ${args.join(" ")}`);
    const agent = spawnAgent(this.binaryPath, args, cwd, {
      onNotification: () => {},
      onLog: (line) => this.output.appendLine(`[server] ${line}`),
      onExit: (code, signal) => {
        this.output.appendLine(`stopped: the loongagent server exited (${signal ?? `code ${code}`})`);
        if (this.agent === agent) {
          this.agent = undefined;
          this.core.forget();
          this.lastExit = { code, signal };
          this.exitObserver?.(code, signal);
        }
      },
    });

    this.agent = agent;
    const session = new LoongSession(agent);
    let attached;
    try {
      attached = await session.attach(cwd);
    } catch (error) {
      if (this.agent === agent) this.agent = undefined;
      this.core.forget();
      await agent.dispose();
      throw error;
    }
    if (this.agent !== agent) throw new Error("the loongagent server exited before attach completed");
    this.core.adopt(session);
    await this.context.workspaceState.update(SESSION_KEY, attached.session_id);
    this.output.appendLine(
      `ready: session ${attached.session_id} (schema ${attached.schema_version}, ` +
      `${attached.existing_events} events already)`,
    );
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
