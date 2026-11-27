import * as vscode from "vscode";
import { spawnAgent } from "./agentSession.ts";
import { withStoredSession } from "./resume.ts";
import { logMode } from "./questions.ts";
import { LoongSession } from "./session.ts";
import { SessionCore } from "./sessionCore.ts";
import type { AgentSession } from "./agentSession.ts";
import type { TurnController, TurnSink } from "./turn.ts";
import type { EventEnvelope } from "../../ts/protocol.generated.ts";
import { parseMode } from "./webviewProtocol.ts";
import type { PermissionMode } from "./webviewProtocol.ts";

/** Where this workspace's resumed session id is remembered between windows. */
const SESSION_KEY = "loongagent.sessionId";
/**
 * The mode this workspace last chose for a *new* session (ADR-0007 §5).
 *
 * `god` is deliberately not remembered: it is a decision about the turn in front
 * of you, not a property of the workspace, and a reload should not quietly come
 * back with every question silenced.
 */
const MODE_KEY = "loongagent.permissionMode";

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

  /** What the attach result said, for a session that has changed nothing yet. */
  private attachedAutoApprove: boolean | undefined;
  /** How many events the log already had at attach: everything before is history. */
  private attachedEventCount = 0;
  /** The mode the server last answered with, so the chip never lags its own click. */
  private answeredMode: PermissionMode | undefined;

  /**
   * What this session asks before it acts, as the server last recorded it.
   *
   * Only events that arrived *after* attach describe this kernel: a resumed log's
   * older `permissions.relaxed` belongs to a process that is gone, and a fresh
   * process starts from the config file's own default. `session.attach` reports how
   * many events the log already had, which is exactly the boundary. The answer to
   * our own `permissions.set` is the same authority, one round trip sooner.
   */
  get mode(): PermissionMode {
    if (this.answeredMode !== undefined) return this.answeredMode;
    const session = this.core.session;
    if (session === undefined) return "ask";
    return logMode(session.events.slice(this.attachedEventCount), this.attachedAutoApprove ?? false);
  }

  /** Change the mode, and remember the two that may outlive this session. */
  async setMode(mode: PermissionMode): Promise<PermissionMode> {
    const session = await this.ensure();
    const state = await session.setMode(mode);
    const applied = parseMode(state.mode) ?? (state.auto_approve ? "yolo" : "ask");
    this.answeredMode = applied;
    if (applied !== "god") await this.context.workspaceState.update(MODE_KEY, applied);
    return applied;
  }

  /** What a new session should start with: the workspace's answer, then the setting. */
  private preferredMode(): PermissionMode {
    const remembered = parseMode(this.context.workspaceState.get<string>(MODE_KEY));
    if (remembered !== undefined) return remembered;
    return this.configuredAutoApprove() ? "yolo" : "ask";
  }

  /** The `loongagent.autoApprove` setting, which predates modes and still means yolo. */
  private configuredAutoApprove(): boolean {
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
    this.attachedAutoApprove = attached.auto_approve;
    this.attachedEventCount = attached.existing_events;
    await this.context.workspaceState.update(SESSION_KEY, attached.session_id);
    this.output.appendLine(
      `ready: session ${attached.session_id} (schema ${attached.schema_version}, ` +
      `${attached.existing_events} events already)`,
    );
    if (attached.pending_approval !== undefined) {
      this.output.appendLine(
        `ready: the kernel still holds approval ${attached.pending_approval} — no turn is driving it`,
      );
    }
    // A new process starts from the config file, so the person's standing choice is
    // re-applied here — as a mode the server records, which is then what the panel
    // shows. Nothing is imposed when the choice is to ask: that is already the
    // default, and turning asking *on* is never the dangerous direction.
    const preferred = this.preferredMode();
    if (preferred !== "ask") {
      const applied = await session.setMode(preferred);
      this.answeredMode = parseMode(applied.mode) ?? (applied.auto_approve ? "yolo" : "ask");
      this.output.appendLine(
        `ready: this session starts in ${this.answeredMode} mode (permissions.relaxed)`,
      );
    }
    return session;
  }
}

function explain(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error);
}
