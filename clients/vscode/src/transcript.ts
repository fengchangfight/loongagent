import type { UiPart } from "./webviewProtocol.ts";

type ToolPart = Extract<UiPart, { kind: "tool" }>;
type ApprovalPart = Extract<UiPart, { kind: "approval" }>;

/**
 * One tab's ordered transcript.
 *
 * The Webview is a view over this list; it never reconstructs events. Tool
 * results and approval answers update the card they belong to instead of
 * appending a second one, because the person reading wants one card per action.
 */
export class Transcript {
  private readonly parts: UiPart[] = [];

  /** A copy, so a consumer cannot mutate the record by holding the array. */
  all(): UiPart[] {
    return [...this.parts];
  }

  append(part: UiPart): void {
    this.parts.push(part);
  }

  /**
   * Update the tool card for `callId`; do nothing when no call was recorded.
   *
   * A result arriving without its call is not a reason to invent a card with
   * missing fields; the host appends the complete result part in that case.
   */
  updateTool(callId: string, patch: Partial<ToolPart>): void {
    for (let index = this.parts.length - 1; index >= 0; index--) {
      const part = this.parts[index];
      if (part.kind === "tool" && part.callId === callId) {
        this.parts[index] = { ...part, ...patch, kind: "tool", callId };
        return;
      }
    }
  }

  /** Update the approval card for `requestId`; do nothing when it is unknown. */
  updateApproval(requestId: string, patch: Partial<ApprovalPart>): void {
    for (let index = this.parts.length - 1; index >= 0; index--) {
      const part = this.parts[index];
      if (part.kind === "approval" && part.requestId === requestId) {
        this.parts[index] = { ...part, ...patch, kind: "approval", requestId };
        return;
      }
    }
  }

  reset(): void {
    this.parts.length = 0;
  }
}
