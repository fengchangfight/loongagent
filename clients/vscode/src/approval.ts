import type { ApprovalAnswer } from "./turn.ts";

interface Waiting {
  tabId: string;
  resolve(answer: ApprovalAnswer): void;
}

function keyOf(tabId: string, requestId: string): string {
  return `${tabId}\u0000${requestId}`;
}

/**
 * Pending decisions, one per tab and request.
 *
 * A closed tab must not leave the server waiting or, worse, turn a missing
 * answer into an allow. `denyTab` is the only cleanup path and it always denies.
 */
export class ApprovalBook {
  private readonly waiting = new Map<string, Waiting>();

  wait(tabId: string, requestId: string): Promise<ApprovalAnswer> {
    return new Promise((resolve) => {
      this.waiting.set(keyOf(tabId, requestId), { tabId, resolve });
    });
  }

  /** Answer one request; false when it is unknown or already answered. */
  answer(tabId: string, requestId: string, answer: ApprovalAnswer): boolean {
    const key = keyOf(tabId, requestId);
    const entry = this.waiting.get(key);
    if (entry === undefined) return false;
    this.waiting.delete(key);
    entry.resolve(answer);
    return true;
  }

  /** Deny every pending decision in one tab, with a reason the log can quote. */
  denyTab(tabId: string, reason: string): void {
    for (const [key, entry] of [...this.waiting.entries()]) {
      if (entry.tabId !== tabId) continue;
      this.waiting.delete(key);
      entry.resolve({ granted: false, scope: "once", reason });
    }
  }

  pending(tabId: string): number {
    let count = 0;
    for (const entry of this.waiting.values()) {
      if (entry.tabId === tabId) count++;
    }
    return count;
  }
}
