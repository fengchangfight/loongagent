import { test } from "node:test";
import assert from "node:assert/strict";
import { promptText } from "../src/prompt.ts";

test("a file attachment adds its absolute path", () => {
  assert.ok(promptText("look", [{ id: "a", kind: "file", path: "/w/a.ts" }]).includes("- /w/a.ts"));
});
test("a selection adds path, line range and text", () => {
  const text = promptText("explain", [{ id: "s", kind: "selection", path: "/w/a.ts",
    startLine: 10, endLine: 12, text: "const a = 1;" }]);
  assert.ok(text.includes("/w/a.ts"));
  assert.ok(text.includes("lines 10-12"));
  assert.ok(text.includes("const a = 1;"));
});
test("two file attachments are not repeated", () => {
  const text = promptText("look", [
    { id: "a", kind: "file", path: "/w/a.ts" },
    { id: "b", kind: "file", path: "/w/a.ts" },
  ]);
  assert.equal(text.split("- /w/a.ts").length - 1, 1);
});
test("no attachments leaves the prompt unchanged", () => {
  assert.equal(promptText("look", []), "look");
});
test("a selection containing backticks uses a longer fence", () => {
  const text = promptText("explain", [{ id: "s", kind: "selection", path: "/w/a.ts",
    startLine: 1, endLine: 1, text: "```" }]);
  assert.ok(text.includes("````"));
});
