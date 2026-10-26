import type { FileChoice } from "./webviewProtocol.ts";

/** How many files the inline @ picker shows at once. */
export const MAX_MENTION_FILES = 50;

/** The files whose relative label starts with the query, case-insensitively. */
export function matchingFiles(
  files: readonly FileChoice[],
  query: string,
): FileChoice[] {
  const prefix = query.toLowerCase();
  return files
    .filter((file) => file.label.toLowerCase().startsWith(prefix))
    .slice(0, MAX_MENTION_FILES);
}


/** Where the caret is inside an `@` mention, or nothing when it is literal text. */
export function mentionAt(value: string, caret: number): { start: number; query: string } | undefined {
  const before = value.slice(0, caret);
  const match = /(^|\s)@([^\s@]*)$/.exec(before);
  if (match === null) return undefined;
  const query = match[2];
  return { start: caret - query.length - 1, query };
}
