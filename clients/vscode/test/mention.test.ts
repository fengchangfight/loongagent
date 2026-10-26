import { test } from "node:test";
import assert from "node:assert/strict";
import { mentionAt } from "../src/mention.ts";

test("an @ at the start of a word opens the mention", () => {
  assert.deepEqual(mentionAt("@src", 4), { start: 0, query: "src" });
  assert.deepEqual(mentionAt("explain @src", 12), { start: 8, query: "src" });
});

test("an @ inside a word or followed by a space is literal", () => {
  assert.equal(mentionAt("a@b", 3), undefined);
  assert.equal(mentionAt("@foo bar", 8), undefined);
  assert.equal(mentionAt("plain text", 10), undefined);
});
