import type { Attachment, Focus } from "./webviewProtocol.ts";

function longestRunOf(text: string, character: string): number {
  let longest = 0;
  let current = 0;
  for (const letter of text) {
    current = letter === character ? current + 1 : 0;
    if (current > longest) longest = current;
  }
  return longest;
}

/**
 * The prompt the kernel receives.
 *
 * Files are paths only: the kernel's own read tools fetch their content.
 * A selection is the one inlined body, because `read_file` has no line-range
 * parameter and the person explicitly attached those lines. The focused file
 * is named when nothing is selected, so "the current file" has a referent;
 * a selection block already names its file, so the line is not repeated.
 */
export function promptText(
  prompt: string,
  attachments: readonly Attachment[],
  focus?: Focus,
): string {
  const files: string[] = [];
  for (const attachment of attachments) {
    if (attachment.kind !== "file") continue;
    if (!files.includes(attachment.path)) files.push(attachment.path);
  }

  let text = prompt;
  const hasSelection = attachments.some((attachment) => attachment.kind === "selection");
  if (focus !== undefined && !hasSelection) {
    text += `\n\ncurrent file: ${focus.path} (cursor at line ${focus.line})`;
  }
  if (files.length > 0) {
    text += `\n\nreferenced files:\n${files.map((path) => `- ${path}`).join("\n")}`;
  }
  for (const attachment of attachments) {
    if (attachment.kind !== "selection") continue;
    const fence = "`".repeat(Math.max(3, longestRunOf(attachment.text, "`") + 1));
    text += `\n\nselected code from ${attachment.path} (lines ${attachment.startLine}-${attachment.endLine}):\n`
      + `${fence}\n${attachment.text}\n${fence}`;
  }
  return text;
}
