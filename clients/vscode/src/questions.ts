import { EVENTS } from "../../ts/protocol.generated.ts";
import type { EventEnvelope, PermissionsRelaxed } from "../../ts/protocol.generated.ts";
import { parseMode } from "./webviewProtocol.ts";
import type { PermissionMode, UiPart } from "./webviewProtocol.ts";

/**
 * What a surface may do about a parked approval, and which mode a session is in.
 *
 * Both answers belong to the server: `session.attach` reports the request id its
 * kernel is holding in memory, and the log's last `permissions.relaxed` event
 * names the mode. A card drawn from the log alone is a button that may answer
 * nothing — a question whose owner is gone — so the two are compared here rather
 * than guessed at the point of drawing.
 */

/** Every card whose answer nobody has recorded yet, in transcript order. */
export function waitingQuestions(parts: UiPart[]): string[] {
  const waiting: string[] = [];
  for (const part of parts) {
    if (part.kind === "approval" && part.state === "pending") waiting.push(part.requestId);
  }
  return waiting;
}

/**
 * Split the transcript's parked cards by what the kernel says.
 *
 * `held` is `session.attach`'s `pending_approval`: the one request a surface can
 * still answer. Every other pending card came from a log whose process is gone,
 * and the person is told so instead of clicking a button that does nothing.
 * `keep` is the held request only when the transcript actually shows it: a
 * question the kernel holds but the log never showed is still cleared — by the
 * caller, which knows `held` — it is never answered on the person's behalf here.
 */
export function reconcileQuestions(
  parts: UiPart[],
  held: string | undefined,
): { keep?: string; retire: string[] } {
  const waiting = waitingQuestions(parts);
  if (held === undefined) return { retire: waiting };
  return {
    ...(waiting.includes(held) ? { keep: held } : {}),
    retire: waiting.filter((requestId) => requestId !== held),
  };
}

/**
 * The approval a refused prompt names, or nothing.
 *
 * The server answers a second prompt with `out of order: approval <id> is still
 * pending` and waits for an answer to that one. Reading the id out of the
 * refusal is the last line of defence for a question this surface lost track of.
 */
export function refusedForApproval(message: string): string | undefined {
  return /out of order: approval (\S+) is still pending/.exec(message)?.[1];
}

/**
 * The mode a session is in, from the log's last word plus the attach result.
 *
 * An event written before ADR-0022 carries no `mode`, and is read the way the
 * protocol says: `auto_approve ? yolo : ask`. With no such event at all, the
 * attach result is the only word there is.
 */
export function logMode(events: EventEnvelope[], autoApprove: boolean): PermissionMode {
  for (let index = events.length - 1; index >= 0; index--) {
    const event = events[index];
    if (event === undefined || event.kind !== EVENTS.permissionsRelaxed) continue;
    const relaxed = event.data as unknown as PermissionsRelaxed;
    return parseMode(relaxed.mode) ?? (relaxed.auto_approve ? "yolo" : "ask");
  }
  return autoApprove ? "yolo" : "ask";
}

/** What a mode switch should do, given what the tab is in the middle of. */
export type SwitchDecision =
  | "ignore"
  | "cancel-pending"
  | "queue"
  | "apply";

/**
 * Decide what a switch does, without waiting for anything.
 *
 * The server reads its next request only once a running turn ends — `prompt.submit`
 * runs the whole turn inside dispatch — so a switch asked during a turn cannot be
 * applied then. It is queued instead, and the panel shows it marked as pending; the
 * person never waits on a round trip that cannot come.
 *
 * Asking for the mode already in force cancels a queued switch: the last click is
 * the one that is meant.
 */
export function switchDecision(
  running: boolean,
  wanted: PermissionMode,
  current: PermissionMode,
  pending: PermissionMode | undefined,
): SwitchDecision {
  if (wanted === current) return pending === undefined ? "ignore" : "cancel-pending";
  return running ? "queue" : "apply";
}

/**
 * The mode `/yolo` and `/god` land on, by the TUI's own rule.
 *
 * A command naming the mode already on turns it off; `/yolo` from `god` comes
 * down to `yolo` rather than all the way to asking.
 */
export function toggledMode(command: "yolo" | "god", current: PermissionMode): PermissionMode {
  if (command === "god") return current === "god" ? "ask" : "god";
  return current === "yolo" ? "ask" : "yolo";
}

/**
 * The mode's mark, as ADR-0007 §5 requires: a relaxed session must look relaxed.
 *
 * The words are the mode's own names — `YOLO` and `GOD` — so the mark reads as the
 * command the person typed and not as a description of one. Asking carries no mark:
 * it is the default, and a badge for it would only add noise.
 */
export function modeBadge(mode: PermissionMode): string {
  if (mode === "yolo") return "⚠ YOLO";
  if (mode === "god") return "⚡ GOD";
  return "";
}

/**
 * What the transcript says when the mode changes.
 *
 * Each mode makes a different promise, so each gets its own sentence: YOLO's leaves
 * the sandbox alone and keeps asking about irreversible actions, and god's drops
 * those two questions while refusing two shapes outright (ADR-0020, ADR-0022).
 *
 * `deferred` is for a switch made while a turn runs. The server reads its next
 * request only once the turn ends, so the change cannot be in force yet — and a
 * person who has just clicked must not be told it is.
 */
export function modeNotice(mode: PermissionMode, options?: { deferred?: boolean }): string {
  const promise = mode === "god"
    ? "⚡ GOD — deletions and outward sends are no longer asked about; a deletion reaching "
      + "outside the workspace, and a secret leaving the machine, are refused rather than asked. "
      + "/god turns it off."
    : mode === "yolo"
      ? "⚠ YOLO — approval prompts are off for this session; the sandbox still applies and "
        + "irreversible actions are still asked about. /yolo turns it off."
      : "approval prompts are on again for this session — the sandbox was never changed.";
  return options?.deferred === true
    ? `${promise} It takes effect when this turn ends.`
    : promise;
}
