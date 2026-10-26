import { isAbsolute, relative } from "node:path";
import type { Attachment } from "./webviewProtocol.ts";

/** The editor data the host reads before turning a selection into a prompt attachment. */
export interface SelectionInput {
  path: string;
  startLine: number;
  endLine: number;
  text: string;
}

function inside(workspace: string, path: string): boolean {
  const relativePath = relative(workspace, path);
  return relativePath !== "" && !relativePath.startsWith("..") && !isAbsolute(relativePath);
}

/**
 * A selection only becomes an attachment when it has text, a valid line range,
 * and a path inside the workspace. The id is assigned by the host so a
 * re-render can tell a new selection from the one already shown.
 */
export function selectionAttachment(
  input: SelectionInput,
  workspace: string,
  id: string,
): Attachment | undefined {
  if (input.text.length === 0) return undefined;
  if (input.startLine < 1 || input.endLine < input.startLine) return undefined;
  if (!inside(workspace, input.path)) return undefined;
  return {
    id,
    kind: "selection",
    path: input.path,
    startLine: input.startLine,
    endLine: input.endLine,
    text: input.text,
  };
}

/** True when two selections describe the same text and range, regardless of id. */
export function sameSelection(a: Attachment | undefined, b: Attachment | undefined): boolean {
  if (a === undefined || b === undefined) return a === b;
  if (a.kind !== "selection" || b.kind !== "selection") return false;
  return a.path === b.path
    && a.startLine === b.startLine
    && a.endLine === b.endLine
    && a.text === b.text;
}
