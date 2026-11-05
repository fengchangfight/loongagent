import { test } from "node:test";
import assert from "node:assert/strict";
import { focusedFile, sameFocus } from "../src/focus.ts";

const workspace = "/work/project";
const file = {
  scheme: "file",
  path: "/work/project/src/a.ts",
  cursorLine: 11,
};

test("a file inside the workspace with the cursor becomes the focused file", () => {
  assert.deepEqual(focusedFile(file, workspace), { path: file.path, line: 12 });
});

test("the top line is reported as line 1, not 0", () => {
  assert.deepEqual(focusedFile({ ...file, cursorLine: 0 }, workspace), { path: file.path, line: 1 });
});

test("a file outside the workspace is not focused", () => {
  assert.equal(focusedFile({ ...file, path: "/elsewhere/a.ts" }, workspace), undefined);
});

test("an untitled document is not focused", () => {
  assert.equal(focusedFile({ ...file, scheme: "untitled" }, workspace), undefined);
});

test("sameFocus compares the path and the cursor line", () => {
  assert.equal(sameFocus({ path: "/w/a.ts", line: 3 }, { path: "/w/a.ts", line: 3 }), true);
  assert.equal(sameFocus({ path: "/w/a.ts", line: 3 }, { path: "/w/a.ts", line: 4 }), false);
  assert.equal(sameFocus(undefined, undefined), true);
  assert.equal(sameFocus({ path: "/w/a.ts", line: 3 }, undefined), false);
});
