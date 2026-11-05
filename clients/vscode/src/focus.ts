import { isAbsolute, relative } from "node:path";
import type { Focus } from "./webviewProtocol.ts";

/** The editor data the host reads before deciding what the person is looking at. */
export interface FocusInput {
  scheme: string;
  path: string;
  /** The cursor line as VS Code reports it, 0-based. */
  cursorLine: number;
}

function inside(workspace: string, path: string): boolean {
  const relativePath = relative(workspace, path);
  return relativePath !== "" && !relativePath.startsWith("..") && !isAbsolute(relativePath);
}

/**
 * The file the active editor is focused on, with its 1-based cursor line, or
 * undefined when it is not a workspace file. Nothing outside the workspace is
 * named to the agent, matching the selection rule.
 */
export function focusedFile(input: FocusInput, workspace: string): Focus | undefined {
  if (input.scheme !== "file") return undefined;
  if (input.cursorLine < 0) return undefined;
  if (!inside(workspace, input.path)) return undefined;
  return { path: input.path, line: input.cursorLine + 1 };
}

/** True when two focused files name the same path at the same cursor line. */
export function sameFocus(a: Focus | undefined, b: Focus | undefined): boolean {
  if (a === undefined || b === undefined) return a === b;
  return a.path === b.path && a.line === b.line;
}
