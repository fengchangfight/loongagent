import { isAbsolute, relative } from "node:path";
import type { FileChoice } from "./webviewProtocol.ts";

export type { FileChoice } from "./webviewProtocol.ts";

function inside(workspace: string, path: string): boolean {
  const relativePath = relative(workspace, path);
  return relativePath !== "" && !relativePath.startsWith("..") && !isAbsolute(relativePath);
}

/**
 * Turn `findFiles` results into sorted choices for the inline @ picker.
 *
 * Only files inside the workspace are offered; anything else would be a path
 * the agent cannot read. The label is relative so the list stays readable, and
 * the absolute path is what the attachment keeps.
 */
export function fileChoices(paths: readonly string[], workspace: string): FileChoice[] {
  const choices: FileChoice[] = [];
  for (const path of paths) {
    if (!inside(workspace, path)) continue;
    const label = relative(workspace, path).split(/[\\/]/).join("/");
    choices.push({ label, path });
  }
  return choices.sort((a, b) => a.label.localeCompare(b.label));
}
