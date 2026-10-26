import { test } from "node:test";
import assert from "node:assert/strict";
import { Transcript } from "../src/transcript.ts";
import type { UiPart } from "../src/webviewProtocol.ts";

test("parts keep their order", () => {
  const t = new Transcript();
  t.append({ kind: "user", text: "u" });
  t.append({ kind: "notice", level: "info", text: "n" });
  assert.deepEqual(t.all().map((part) => part.kind), ["user", "notice"]);
});
test("a tool result updates the card with the same callId", () => {
  const t = new Transcript();
  t.append({ kind: "tool", callId: "c1", tool: "write_file",
    actionClass: "write_workspace", paths: [], status: "running" });
  t.updateTool("c1", { status: "ok", output: "done" });
  const [part] = t.all();
  assert.equal(t.all().length, 1);
  assert.equal(part.kind, "tool");
  if (part.kind !== "tool") throw new Error("expected tool part");
  assert.equal(part.status, "ok");
});
test("an unknown tool result does not invent a partial card", () => {
  const t = new Transcript();
  t.updateTool("c2", { status: "ok" });
  assert.equal(t.all().length, 0);
});
test("an approval can be marked allowed", () => {
  const t = new Transcript();
  t.append({ kind: "approval", requestId: "r1", actionClass: "write_workspace",
    reason: "ask", paths: [], state: "pending" });
  t.updateApproval("r1", { state: "allowed" });
  const [part] = t.all();
  assert.equal(part.kind, "approval");
  if (part.kind !== "approval") throw new Error("expected approval part");
  assert.equal(part.state, "allowed");
});
test("reset empties the transcript", () => {
  const t = new Transcript();
  t.append({ kind: "notice", level: "info", text: "n" });
  t.reset();
  assert.equal(t.all().length, 0);
});
test("all returns a copy, not the stored array", () => {
  const t = new Transcript();
  t.append({ kind: "user", text: "u" });
  const parts: UiPart[] = t.all();
  parts.length = 0;
  assert.equal(t.all().length, 1);
});
