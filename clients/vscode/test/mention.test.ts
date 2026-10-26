import { test } from "node:test";
import assert from "node:assert/strict";
import { insertMention, mentionAt } from "../src/mention.ts";

test("an @ at the start of a word opens the mention", () => {
  assert.deepEqual(mentionAt("@src", 4), { start: 0, query: "src" });
  assert.deepEqual(mentionAt("explain @src", 12), { start: 8, query: "src" });
});

test("an @ inside a word or followed by a space is literal", () => {
  assert.equal(mentionAt("a@b", 3), undefined);
  assert.equal(mentionAt("@foo bar", 8), undefined);
  assert.equal(mentionAt("plain text", 10), undefined);
});

test("choosing a file leaves its @path in the input", () => {
  assert.deepEqual(insertMention("update @doc", 7, "doc", "docs/a.md"), {
    value: "update @docs/a.md ",
    caret: 18,
  });
  assert.deepEqual(insertMention("@src", 0, "src", "src/a.ts"), {
    value: "@src/a.ts ",
    caret: 10,
  });
});
