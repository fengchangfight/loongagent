import { test } from "node:test";
import assert from "node:assert/strict";
import { MAX_TABS, TabManager } from "../src/tab.ts";
import type { TabState } from "../src/webviewProtocol.ts";

const tab = (id: string, sessionId?: string): TabState => ({
  id, sessionId, title: id, status: "idle" });

test("open appends and activates the first tab", () => {
  const m = new TabManager();
  assert.equal(m.open(tab("t1")), "opened");
  assert.equal(m.active()?.id, "t1");
});
test("a sixth tab is refused", () => {
  const m = new TabManager();
  for (let i = 1; i <= MAX_TABS; i++) assert.equal(m.open(tab(`t${i}`)), "opened");
  assert.equal(m.open(tab("t6")), "max");
  assert.equal(m.tabs.length, MAX_TABS);
});
test("closing the active tab selects a neighbour", () => {
  const m = new TabManager();
  m.open(tab("t1")); m.open(tab("t2")); m.open(tab("t3"));
  m.close("t2");
  assert.equal(m.active()?.id, "t3");
  m.close("t3");
  assert.equal(m.active()?.id, "t1");
});
test("closing the last tab leaves no active tab", () => {
  const m = new TabManager();
  m.open(tab("t1"));
  m.close("t1");
  assert.equal(m.active(), undefined);
  assert.equal(m.tabs.length, 0);
});
test("find locates an open session and never duplicates it", () => {
  const m = new TabManager();
  m.open(tab("t1", "s1"));
  assert.equal(m.find("s1")?.id, "t1");
  assert.equal(m.find("missing"), undefined);
});
test("snapshot and restore round-trip", () => {
  const m = new TabManager();
  m.open(tab("t1", "s1"));
  const saved = m.snapshot();
  const other = new TabManager();
  other.restore(saved.tabs, saved.activeId);
  assert.deepEqual(other.snapshot(), saved);
});
test("restore clamps to the maximum and falls back to the first tab", () => {
  const m = new TabManager();
  m.restore([tab("t1"), tab("t2"), tab("t3"), tab("t4"), tab("t5"), tab("t6")], "missing");
  assert.equal(m.tabs.length, MAX_TABS);
  assert.equal(m.active()?.id, "t1");
});
