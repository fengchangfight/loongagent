import { test } from "node:test";
import assert from "node:assert/strict";
import { fileChoices } from "../src/filePicker.ts";
import { matchingFiles } from "../src/mention.ts";

test("workspace files become sorted relative choices", () => {
  const choices = fileChoices(["/w/src/b.ts", "/w/a.ts", "/w/src/a.ts"], "/w");
  assert.deepEqual(choices.map((choice) => choice.label), ["a.ts", "src/a.ts", "src/b.ts"]);
  assert.deepEqual(choices.map((choice) => choice.path), ["/w/a.ts", "/w/src/a.ts", "/w/src/b.ts"]);
});

test("a path outside the workspace is not offered", () => {
  assert.deepEqual(fileChoices(["/w/a.ts", "/elsewhere/x.ts"], "/w").map((choice) => choice.label),
    ["a.ts"]);
});

test("matchingFiles is a case-insensitive prefix match", () => {
  const files = [
    { label: "src/chat.ts", path: "/w/src/chat.ts" },
    { label: "src/main.ts", path: "/w/src/main.ts" },
    { label: "other/src.ts", path: "/w/other/src.ts" },
    { label: "README.md", path: "/w/README.md" },
  ];
  assert.deepEqual(matchingFiles(files, "SRC").map((file) => file.label),
    ["src/chat.ts", "src/main.ts"]);
  assert.deepEqual(matchingFiles(files, "").map((file) => file.label),
    ["src/chat.ts", "src/main.ts", "other/src.ts", "README.md"]);
});
