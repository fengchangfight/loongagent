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
