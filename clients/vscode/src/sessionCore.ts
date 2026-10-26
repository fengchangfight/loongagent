import { TurnGate } from "./turn.ts";
import type { LoongSession } from "./session.ts";

/**
 * The live session and the gate that owns its turn.
 *
 * Keeping them in one object is the fix for a dead child: clearing the session
 * must clear the gate too, or the next request runs on a process that is gone.
 */
export class SessionCore {
  private live: LoongSession | undefined;
  private gate: TurnGate | undefined;

  get session(): LoongSession | undefined {
    return this.live;
  }

  get turnGate(): TurnGate | undefined {
    return this.gate;
  }

  adopt(session: LoongSession): void {
    this.live = session;
    this.gate = new TurnGate(session);
  }

  forget(): void {
    this.live = undefined;
    this.gate = undefined;
  }

  alive(): boolean {
    return this.live !== undefined;
  }
}
