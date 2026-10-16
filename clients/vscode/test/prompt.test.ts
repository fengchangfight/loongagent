import { test } from "node:test";
import assert from "node:assert/strict";
import { promptText } from "../src/prompt.ts";

test("a prompt with no references is passed through unchanged", () => {
  assert.equal(promptText("hello", []), "hello");
  assert.equal(promptText("hello", undefined), "hello");
});

test("a reference with an fsPath becomes one path line", () => {
  const text = promptText("what is in here?", [{ value: { fsPath: "/w/a.ts" } }]);
  assert.equal(text, "what is in here?\n\nreferenced files:\n- /w/a.ts");
});

test("a reference that wraps its uri is read too", () => {
  const text = promptText("look", [{ value: { uri: { fsPath: "/w/b.ts" } } }]);
  assert.match(text, /- \/w\/b\.ts/);
});

test("a reference with nothing readable contributes nothing", () => {
  assert.equal(promptText("hello", [{ value: "just a string" }, { value: 7 }]), "hello");
});

test("the same path twice appears once", () => {
  const text = promptText("look", [
    { value: { fsPath: "/w/a.ts" } },
    { value: { uri: { fsPath: "/w/a.ts" } } },
  ]);
  assert.equal(text.match(/\/w\/a\.ts/g)?.length, 1);
});

test("the block is only a path list — no file contents are pasted", () => {
  const text = promptText("look", [{ value: { fsPath: "/w/a.ts" } }]);
  assert.equal(text.split("referenced files:\n")[1], "- /w/a.ts");
});
