import { test } from "node:test";
import assert from "node:assert/strict";
import { parseWebviewMessage } from "../src/webviewProtocol.ts";

test("a send is parsed", () => {
  assert.deepEqual(parseWebviewMessage({ type: "send", tabId: "t1", text: "hi" }),
    { type: "send", tabId: "t1", text: "hi" });
});
test("an approval choice is validated", () => {
  assert.equal(parseWebviewMessage({ type: "approval", tabId: "t1", requestId: "r", choice: "always" })?.type, "approval");
  assert.equal(parseWebviewMessage({ type: "approval", tabId: "t1", requestId: "r", choice: "maybe" }), undefined);
});
test("unknown and malformed messages are ignored", () => {
  assert.equal(parseWebviewMessage({ type: "nope" }), undefined);
  assert.equal(parseWebviewMessage({ type: "send", text: "hi" }), undefined);
  assert.equal(parseWebviewMessage(null), undefined);
});
