import { JsonRpcError } from "./agentSession.ts";
import type { AgentSession, AgentSessionOptions } from "./agentSession.ts";
import { METHODS, NOTIFICATIONS } from "../../ts/protocol.generated.ts";
import type {
  ApprovalScope,
  ContextExplain,
  EventEnvelope,
  ModelList,
  SessionAttachResult,
} from "../../ts/protocol.generated.ts";
import type { SessionInfo } from "./webviewProtocol.ts";

/**
 * The server refuses a request that names a session it is not on rather than
 * acting on the wrong log. The caller's move is to re-attach, never to retry.
 */
export class SessionRefusedError extends Error {
  /** The session this client believed it was on when it was refused. */
  readonly sessionId: string;

  // No parameter property: `node --test` strips types rather than compiling them.
  constructor(sessionId: string, message: string) {
    super(message);
    this.name = "SessionRefusedError";
    this.sessionId = sessionId;
  }
}

/**
 * The one notification the schema has no constant for: the transient list in
 * `crates/schema/protocol.toml` names only `model.delta`, while the server
 * announces every logged event as `event` with the envelope itself as params
 * (`crates/protocol/src/lib.rs`, `EVENT_NOTIFICATION`).
 */
const EVENT_NOTIFICATION = "event";

/**
 * The protocol as the rest of the extension uses it: method names, the session
 * every request has to name, and the events seen so far.
 *
 * This is the only file that spells a method name or a param field, so a schema
 * change lands here and nowhere else.
 */
export class LoongSession {
  private readonly agent: AgentSession;
  private current: string | undefined;
  private readonly seen: EventEnvelope[] = [];
  private eventObserver: ((event: EventEnvelope) => void) | undefined;
  private deltaObserver: ((text: string) => void) | undefined;

  constructor(agent: AgentSession) {
    this.agent = agent;
    // The wrapper is built after the transport that feeds it, so it takes the
    // notification stream over instead of being passed into the constructor.
    agent.setNotificationHandler((method, params) => this.receive(method, params));
  }

  get sessionId(): string | undefined {
    return this.current;
  }

  /**
   * Everything heard so far.
   *
   * The server drains the session's log as events *before* it answers
   * `session.attach`, so this is already the history at attach time; `replay()`
   * would add it a second time and is deliberately not called on the live path.
   */
  get events(): EventEnvelope[] {
    return this.seen;
  }

  async attach(cwd: string): Promise<SessionAttachResult> {
    const result = await this.call<SessionAttachResult>(METHODS.sessionAttach, {
      cwd,
      client: "vscode",
    });
    this.current = result.session_id;
    return result;
  }

  async newSession(): Promise<string> {
    const result = await this.call<{ session_id: string }>(METHODS.sessionNew, this.mine());
    this.current = result.session_id;
    return result.session_id;
  }

  /** List this project's sessions for the history surface; the method records nothing. */
  async listSessions(limit = 50): Promise<SessionInfo[]> {
    const result = await this.call<{ sessions: SessionInfo[] }>(METHODS.sessionList, { limit });
    return result.sessions;
  }

  async replay(): Promise<EventEnvelope[]> {
    const result = await this.call<{ events: EventEnvelope[] }>(METHODS.sessionReplay, this.mine());
    this.seen.push(...result.events);
    return result.events;
  }

  /**
   * Submit a turn.
   *
   * It resolves at the turn's first **pause**, not at its end: when the policy
   * asks, the server streams `approval.requested` and returns, and the turn
   * continues on `respondApproval`. Callers that want the whole turn loop.
   */
  async submit(text: string): Promise<{ turn_id: number; accepted: boolean }> {
    return this.call<{ turn_id: number; accepted: boolean }>(
      METHODS.promptSubmit,
      this.mine({ text }),
      { idle: true },
    );
  }

  /** Called for every logged event as it arrives. Replaces any earlier observer. */
  onEvent(handler: (event: EventEnvelope) => void): void {
    this.eventObserver = handler;
  }

  /** Called for each fragment of the answer, in the order the model wrote them. */
  onDelta(handler: (text: string) => void): void {
    this.deltaObserver = handler;
  }

  async models(): Promise<ModelList> {
    return this.call<ModelList>(METHODS.modelList, this.mine());
  }

  async selectModel(name: string): Promise<ModelList> {
    return this.call<ModelList>(METHODS.modelSelect, this.mine({ name }));
  }

  async context(): Promise<ContextExplain> {
    return this.call<ContextExplain>(METHODS.contextExplain, this.mine());
  }

  /** Opting out of prompts is a session event; the new state is what comes back. */
  async setAutoApprove(on: boolean): Promise<boolean> {
    const state = await this.call<{ auto_approve: boolean }>(
      METHODS.permissionsSet,
      this.mine({ auto_approve: on }),
    );
    return state.auto_approve;
  }

  async respondApproval(
    requestId: string,
    granted: boolean,
    scope: ApprovalScope,
    reason?: string,
  ): Promise<boolean> {
    const result = await this.call<{ accepted: boolean }>(
      METHODS.approvalRespond,
      this.mine({
        request_id: requestId,
        granted,
        scope,
        ...(reason === undefined ? {} : { reason }),
      }),
      { idle: true },
    );
    return result.accepted;
  }

  /** Stop the running turn. The server keeps the session and waits for the next one. */
  cancel(): void {
    this.agent.cancel();
  }

  private receive(method: string, params: unknown): void {
    if (params === null || typeof params !== "object") return;
    if (method === EVENT_NOTIFICATION) {
      const event = params as EventEnvelope;
      this.seen.push(event);
      this.eventObserver?.(event);
      return;
    }
    if (method === NOTIFICATIONS.modelDelta) {
      const fragment = params as { text?: unknown };
      if (typeof fragment.text === "string") this.deltaObserver?.(fragment.text);
    }
  }

  private async call<T>(
    method: string,
    params: Record<string, unknown>,
    options: AgentSessionOptions = {},
  ): Promise<T> {
    try {
      return await this.agent.request<T>(method, params, options);
    } catch (error) {
      // The server says which session it is on; that is a refusal, not a failure
      // of the request itself, and it is the caller's cue to start over.
      if (error instanceof JsonRpcError && /session/i.test(error.message)) {
        throw new SessionRefusedError(this.current ?? "", error.message);
      }
      throw error;
    }
  }

  /**
   * Every method but `session.attach` names the session it is on, and the server
   * refuses a name it does not recognise. Sending one before there is a session
   * to name is a programming error here, not a round trip.
   */
  private mine(params: Record<string, unknown> = {}): Record<string, unknown> {
    if (this.current === undefined) {
      throw new Error("this session is not attached yet; attach before sending anything");
    }
    return { session_id: this.current, ...params };
  }
}
