import type { Attachment } from "./webviewProtocol.ts";

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
 * parameter and the person explicitly attached those lines.
 */
export function promptText(prompt: string, attachments: readonly Attachment[]): string {
  const files: string[] = [];
  for (const attachment of attachments) {
    if (attachment.kind !== "file") continue;
    if (!files.includes(attachment.path)) files.push(attachment.path);
  }

  let text = prompt;
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
