import { test } from "node:test";
import assert from "node:assert/strict";
import { sameSelection, selectionAttachment } from "../src/selection.ts";
import type { Attachment } from "../src/webviewProtocol.ts";

type Selection = Extract<Attachment, { kind: "selection" }>;

const workspace = "/work/project";
const input = {
  path: "/work/project/src/a.ts",
  startLine: 10,
  endLine: 12,
  text: "const value = 1;",
};

test("a non-empty selection inside the workspace becomes an attachment", () => {
  assert.deepEqual(selectionAttachment(input, workspace, "s1"), {
    id: "s1",
    kind: "selection",
    path: input.path,
    startLine: 10,
    endLine: 12,
    text: input.text,
  });
});

test("an empty selection is not attached", () => {
  assert.equal(selectionAttachment({ ...input, text: "" }, workspace, "s1"), undefined);
});

test("a selection outside the workspace is not attached", () => {
  assert.equal(selectionAttachment({ ...input, path: "/elsewhere/a.ts" }, workspace, "s1"), undefined);
});

test("sameSelection compares the range and the text", () => {
  const first = selectionAttachment(input, workspace, "s1") as Selection;
  const same = selectionAttachment(input, workspace, "s2") as Selection;
  assert.equal(sameSelection(first, same), true, "the id does not matter");
  assert.equal(sameSelection(first, { ...same, endLine: 13 }), false);
  assert.equal(sameSelection(first, { ...same, text: "changed" }), false);
});
