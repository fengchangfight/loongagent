/** A `#file` reference as the chat hands it over; only `fsPath` is read. */
export interface Reference {
  value: unknown;
}

/**
 * The prompt the kernel receives.
 *
 * `#file` references become absolute paths and nothing else: the kernel's own
 * read tools fetch the contents, and pasting bodies here would be this surface
 * assembling context, which is the server's job (ADR-0009 §2).
 */
export function promptText(prompt: string, references: readonly Reference[] | undefined): string {
  if (references === undefined || references.length === 0) return prompt;
  const paths: string[] = [];
  for (const reference of references) {
    const path = pathOf(reference.value);
    if (path !== undefined && !paths.includes(path)) paths.push(path);
  }
  if (paths.length === 0) return prompt;
  return `${prompt}\n\nreferenced files:\n${paths.map((path) => `- ${path}`).join("\n")}`;
}

/**
 * Duck-typed on purpose: a `vscode.Uri` and a `vscode.Location` both carry a
 * `uri`/`fsPath`, and this module stays free of the `vscode` import so its rule
 * can be tested without an extension host.
 */
function pathOf(value: unknown): string | undefined {
  if (value === null || typeof value !== "object") return undefined;
  const direct = (value as { fsPath?: unknown }).fsPath;
  if (typeof direct === "string" && direct !== "") return direct;
  const nested = (value as { uri?: unknown }).uri;
  if (nested !== null && typeof nested === "object") {
    const fsPath = (nested as { fsPath?: unknown }).fsPath;
    if (typeof fsPath === "string" && fsPath !== "") return fsPath;
  }
  return undefined;
}
