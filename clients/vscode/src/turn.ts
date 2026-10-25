import { EVENTS } from "../../ts/protocol.generated.ts";
import type {
  ActionClass,
  ApprovalDecided,
  ApprovalRequested,
  ApprovalScope,
  EventEnvelope,
} from "../../ts/protocol.generated.ts";
import type { LoongSession } from "./session.ts";

/** What the policy is asking about, quoted from the event rather than inferred. */
export interface ApprovalRequest {
  request_id: string;
  action_class: ActionClass;
  reason: string;
}

/** The human's answer, in the protocol's own terms. */
export interface ApprovalAnswer {
  granted: boolean;
  scope: ApprovalScope;
  reason?: string;
}

export type Decider = (request: ApprovalRequest) => Promise<ApprovalAnswer>;

/** A turn is already running; one writer, one turn. */
export class TurnBusyError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "TurnBusyError";
  }
}

/** The button labels, in one place, so the mapping below cannot drift from them. */
export const ALLOW_ONCE = "Allow once";
export const ALLOW_ALWAYS = "Allow always";
export const DENY = "Deny";

/**
 * What a person's answer means in the protocol's terms.
 *
 * A dismissed dialog is a **denial**, never an allow: closing a box is not
 * consent, and reading it as consent is how a guard becomes decorative.
 */
export function answerFor(choice: string | undefined): ApprovalAnswer {
  if (choice === ALLOW_ONCE) return { granted: true, scope: "once" };
  if (choice === ALLOW_ALWAYS) return { granted: true, scope: "action_class" };
  return { granted: false, scope: "once", reason: "denied in VS Code" };
}

export interface TurnHooks {
  onEvent(event: EventEnvelope): void;
  onDelta(text: string): void;
  decide: Decider;
}

/**
 * One turn, driven to its end.
 *
 * The protocol makes this a loop rather than a single request: the server stops
 * a turn when the policy asks (ADR-0007), returning from `prompt.submit` after
 * streaming `approval.requested`, and the turn resumes on `approval.respond`.
 * A turn can pause any number of times, so the controller answers each pause
 * and keeps going until none is pending.
 *
 * It also holds the one-writer rule: two turns on one session would interleave
 * two streams into one conversation.
 */
/** Where one running turn's parts and questions go. */
export type TurnSink = TurnHooks;

/**
 * One writer per session.
 *
 * A `TurnController`'s `running` flag only means anything if the *same*
 * controller is reused, so this owns the one instance and lends it to whoever
 * holds the turn. It also owns the sink, because the controller's hooks are
 * fixed when it is built — a controller per request cannot refuse the second
 * request (its flag is always false) and it *replaces* the session's observers,
 * which delivers the running turn's stream to the wrong chat.
 */
export class TurnGate {
  private readonly controller: TurnController;
  private sink: TurnSink | undefined;

  constructor(session: LoongSession) {
    this.controller = new TurnController(session, {
      onEvent: (event) => this.sink?.onEvent(event),
      onDelta: (text) => this.sink?.onDelta(text),
      decide: (request) => this.ask(request),
    });
  }

  get busy(): boolean {
    return this.controller.busy;
  }

  get pendingApproval(): ApprovalRequest | null {
    return this.controller.pendingApproval;
  }

  /** Take the turn, or refuse without touching the sink the running turn owns. */
  begin(sink: TurnSink): TurnController {
    if (this.controller.busy) {
      throw new TurnBusyError("a turn is already running; wait for it to finish or stop it");
    }
    this.sink = sink;
    return this.controller;
  }

  /** Give the turn back, whoever had it. */
  end(): void {
    this.sink = undefined;
  }

  private ask(request: ApprovalRequest): Promise<ApprovalAnswer> {
    const sink = this.sink;
    if (sink === undefined) {
      // Nothing is holding the turn: the safe answer to a policy question is no.
      return Promise.resolve({
        granted: false,
        scope: "once",
        reason: "no surface is holding this turn",
      });
    }
    return sink.decide(request);
  }
}

export class TurnController {
  private readonly session: LoongSession;
  private readonly hooks: TurnHooks;
  private running = false;
  private pending: ApprovalRequest | null = null;

  constructor(session: LoongSession, hooks: TurnHooks) {
    this.session = session;
    this.hooks = hooks;
    // Events and deltas arrive while a request is outstanding, so they are
    // observed as they land rather than collected after the fact.
    session.onEvent((event) => this.observe(event));
    session.onDelta((text) => this.hooks.onDelta(text));
  }

  get busy(): boolean {
    return this.running;
  }

  get pendingApproval(): ApprovalRequest | null {
    return this.pending;
  }

  async run(text: string): Promise<{ turn_id: number; accepted: boolean }> {
    if (this.running) {
      throw new TurnBusyError("a turn is already running; wait for it to finish or stop it");
    }
    this.running = true;
    try {
      const started = await this.session.submit(text);
      let accepted = started.accepted;
      while (this.pending !== null) {
        const request = this.pending;
        this.pending = null;
        const answer = await this.hooks.decide(request);
        accepted = await this.session.respondApproval(
          request.request_id,
          answer.granted,
          answer.scope,
          answer.reason,
        );
      }
      // The turn id is the server's own count for this turn; resuming an
      // approval does not start a new one, so the first answer's id stands.
      return { turn_id: started.turn_id, accepted };
    } finally {
      this.running = false;
      this.pending = null;
    }
  }

  private observe(event: EventEnvelope): void {
    if (event.kind === EVENTS.approvalRequested) {
      const asked = event.data as unknown as ApprovalRequested;
      this.pending = {
        request_id: asked.request_id,
        action_class: asked.action_class,
        reason: asked.reason,
      };
    } else if (event.kind === EVENTS.approvalDecided) {
      const decided = event.data as unknown as ApprovalDecided;
      if (this.pending !== null && this.pending.request_id === decided.request_id) {
        this.pending = null;
      }
    }
    this.hooks.onEvent(event);
  }
}
