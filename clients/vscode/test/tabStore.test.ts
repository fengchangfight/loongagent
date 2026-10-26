import { test } from "node:test";
import assert from "node:assert/strict";
import { decodeTabs, encodeTabs } from "../src/tabStore.ts";
import type { TabState } from "../src/webviewProtocol.ts";

const tabs: TabState[] = [
  { id: "t1", sessionId: "s1", title: "A", status: "idle" },
  { id: "t2", title: "New session", status: "running" },
];

test("open tabs round-trip through the stored form", () => {
  assert.deepEqual(decodeTabs(encodeTabs(tabs)), [
    { id: "t1", sessionId: "s1", title: "A" },
    { id: "t2", title: "New session" },
  ]);
});

test("malformed and duplicate stored tabs are dropped", () => {
  assert.deepEqual(decodeTabs([
    { id: "t1", title: "A" },
    { title: "no id" },
    { id: "t1", title: "B" },
  ]), [{ id: "t1", title: "A" }]);
  assert.deepEqual(decodeTabs("nope"), []);
});
