import { test } from "node:test";
import assert from "node:assert/strict";
import { previewEdit, previewWrite, unifiedDiff } from "../src/diff.ts";

test("a new file previews as all additions", () => {
  const preview = previewWrite(undefined, "a\nb");
  assert.equal(preview.ok, true);
  if (!preview.ok) return;
  assert.equal(preview.after, "a\nb");
  assert.equal(preview.diff.added, 2);
});
test("a write previews a replacement", () => {
  const preview = previewWrite("a", "b");
  assert.equal(preview.ok, true);
  if (!preview.ok) return;
  assert.ok(preview.diff.unified.includes("-a"));
  assert.ok(preview.diff.unified.includes("+b"));
});
test("edit replaces one occurrence", () => {
  const preview = previewEdit("x old y", "old", "new");
  assert.equal(preview.ok, true);
  if (preview.ok) assert.equal(preview.after, "x new y");
});
test("edit with replace_all replaces every occurrence", () => {
  const preview = previewEdit("a a", "a", "x", true);
  assert.equal(preview.ok, true);
  if (preview.ok) assert.equal(preview.after, "x x");
});
test("edit with an absent old_string refuses to preview", () => {
  assert.deepEqual(previewEdit("a", "missing", "x"), { ok: false, reason: "old_string was not found" });
});
test("edit with an ambiguous old_string refuses to preview", () => {
  assert.equal(previewEdit("a a", "a", "x").ok, false);
});
test("a large diff is truncated and says so", () => {
  const preview = previewWrite("a\nb\nc", "a\nb\nc\nd", 2);
  assert.equal(preview.ok, true);
  if (preview.ok) assert.equal(preview.diff.truncated, true);
});
test("an unchanged file has no diff and is not truncated", () => {
  const diff = unifiedDiff("a\nb", "a\nb");
  assert.deepEqual(diff, { unified: "", added: 0, removed: 0, truncated: false });
});
