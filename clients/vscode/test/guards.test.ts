import { test } from "node:test";
import assert from "node:assert/strict";
import { refusalToStart } from "../src/guards.ts";

test("no folder is refused with something to do about it", () => {
  const refusal = refusalToStart({ hasFolder: false, isTrusted: true });
  assert.match(refusal ?? "", /Open a folder/);
});

test("an untrusted workspace is refused", () => {
  const refusal = refusalToStart({ hasFolder: true, isTrusted: false });
  assert.match(refusal ?? "", /not trusted/);
  assert.match(refusal ?? "", /Manage Workspace Trust/, "the way out is named");
});

test("a trusted workspace with a folder is not refused", () => {
  assert.equal(refusalToStart({ hasFolder: true, isTrusted: true }), undefined);
});

test("no folder is refused even when the workspace is untrusted", () => {
  assert.notEqual(refusalToStart({ hasFolder: false, isTrusted: false }), undefined);
});
