import type { TabState } from "./webviewProtocol.ts";

/** How many conversations one window may keep open at once. */
export const MAX_TABS = 5;

/** The sidebar's tabs as data: no child processes, no VS Code. */
export class TabManager {
  private list: TabState[] = [];
  private current: string | undefined;

  get tabs(): readonly TabState[] {
    return this.list;
  }

  get activeId(): string | undefined {
    return this.current;
  }

  active(): TabState | undefined {
    return this.list.find((tab) => tab.id === this.current);
  }

  /** Open a tab and make it active; a sixth tab is refused, never silently dropped. */
  open(tab: TabState): "opened" | "max" {
    const existing = this.list.find((entry) => entry.id === tab.id);
    if (existing !== undefined) {
      Object.assign(existing, tab);
      this.current = tab.id;
      return "opened";
    }
    if (this.list.length >= MAX_TABS) return "max";
    this.list.push({ ...tab });
    this.current = tab.id;
    return "opened";
  }

  /** Close a tab; an active one hands over to its next neighbour, then its previous. */
  close(id: string): void {
    const index = this.list.findIndex((tab) => tab.id === id);
    if (index < 0) return;
    this.list.splice(index, 1);
    if (this.current !== id) return;
    const next = this.list[index] ?? this.list[index - 1];
    this.current = next?.id;
  }

  activate(id: string): void {
    if (this.list.some((tab) => tab.id === id)) this.current = id;
  }

  find(sessionId: string): TabState | undefined {
    return this.list.find((tab) => tab.sessionId === sessionId);
  }

  update(id: string, patch: Partial<Omit<TabState, "id">>): void {
    const tab = this.list.find((entry) => entry.id === id);
    if (tab !== undefined) Object.assign(tab, patch);
  }

  snapshot(): { tabs: TabState[]; activeId?: string } {
    return {
      tabs: this.list.map((tab) => ({ ...tab })),
      ...(this.current === undefined ? {} : { activeId: this.current }),
    };
  }

  /** Replace the state from persistence; never exceed the ceiling or lose an active tab. */
  restore(tabs: TabState[], activeId?: string): void {
    this.list = tabs.slice(0, MAX_TABS).map((tab) => ({ ...tab }));
    this.current = activeId !== undefined && this.list.some((tab) => tab.id === activeId)
      ? activeId
      : this.list[0]?.id;
  }
}
