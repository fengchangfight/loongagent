import { test } from "node:test";
import assert from "node:assert/strict";
import { fileChoices } from "../src/filePicker.ts";

test("workspace files become sorted relative choices", () => {
  const choices = fileChoices(["/w/src/b.ts", "/w/a.ts", "/w/src/a.ts"], "/w");
  assert.deepEqual(choices.map((choice) => choice.label), ["a.ts", "src/a.ts", "src/b.ts"]);
  assert.deepEqual(choices.map((choice) => choice.path), ["/w/a.ts", "/w/src/a.ts", "/w/src/b.ts"]);
});

test("a path outside the workspace is not offered", () => {
  assert.deepEqual(fileChoices(["/w/a.ts", "/elsewhere/x.ts"], "/w").map((choice) => choice.label),
    ["a.ts"]);
});
