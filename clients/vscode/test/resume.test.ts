import { test } from "node:test";
import assert from "node:assert/strict";
import { withStoredSession } from "../src/resume.ts";

test("a stored session that opens is used and nothing else happens", async () => {
  const tried: (string | undefined)[] = [];
  const seen: string[] = [];
  const result = await withStoredSession(
    "s7",
    async (id) => { tried.push(id); return `opened ${id}`; },
    (id) => { seen.push(id); },
  );
  assert.equal(result, "opened s7");
  assert.deepEqual(tried, ["s7"], "no fallback was attempted");
  assert.deepEqual(seen, []);
});

test("a stored session the server refuses is started over once, and said out loud", async () => {
  const tried: (string | undefined)[] = [];
  const announced: string[] = [];
  const result = await withStoredSession(
    "gone",
    async (id) => {
      tried.push(id);
      if (id === "gone") throw new Error('no session "gone"');
      return "a fresh one";
    },
    (id) => { announced.push(id); },
  );
  assert.equal(result, "a fresh one");
  assert.deepEqual(tried, ["gone", undefined], "tried the stored id, then started fresh");
  assert.deepEqual(announced, ["gone"], "and said which id it could not resume");
});

test("a failure with nothing stored propagates instead of being retried", async () => {
  let attempts = 0;
  await assert.rejects(
    withStoredSession(
      undefined,
      async () => { attempts += 1; throw new Error("the binary is missing"); },
      () => {},
    ),
    /binary is missing/,
  );
  assert.equal(attempts, 1, "no id means nothing to fall back from");
});

test("a fresh start that also fails propagates rather than looping", async () => {
  let attempts = 0;
  await assert.rejects(
    withStoredSession(
      "gone",
      async () => { attempts += 1; throw new Error("still broken"); },
      () => {},
    ),
    /still broken/,
  );
  assert.equal(attempts, 2, "the stored attempt and exactly one fresh one");
});
