import type { TabState } from "./webviewProtocol.ts";

/** The part of an open tab that survives a window reload. */
export interface StoredTab {
  id: string;
  sessionId?: string;
  title: string;
}

/** Persist only stable tab facts; status and unread are runtime state. */
export function encodeTabs(tabs: readonly TabState[]): StoredTab[] {
  return tabs.map((tab) => ({
    id: tab.id,
    ...(tab.sessionId === undefined ? {} : { sessionId: tab.sessionId }),
    title: tab.title,
  }));
}

/** Read untrusted workspace state back into tabs, dropping malformed and duplicate entries. */
export function decodeTabs(raw: unknown): StoredTab[] {
  if (!Array.isArray(raw)) return [];
  const seen = new Set<string>();
  const tabs: StoredTab[] = [];
  for (const entry of raw) {
    if (typeof entry !== "object" || entry === null) continue;
    const row = entry as Record<string, unknown>;
    if (typeof row.id !== "string" || row.id === "") continue;
    if (typeof row.title !== "string") continue;
    if (seen.has(row.id)) continue;
    seen.add(row.id);
    tabs.push({
      id: row.id,
      ...(typeof row.sessionId === "string" && row.sessionId !== ""
        ? { sessionId: row.sessionId }
        : {}),
      title: row.title,
    });
  }
  return tabs;
}
