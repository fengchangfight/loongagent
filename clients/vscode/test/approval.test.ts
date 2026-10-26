import { test } from "node:test";
import assert from "node:assert/strict";
import { ApprovalBook } from "../src/approval.ts";

test("an answer resolves the waiting decision", async () => {
  const book = new ApprovalBook();
  const pending = book.wait("t1", "r1");
  assert.equal(book.answer("t1", "r1", { granted: true, scope: "once" }), true);
  assert.deepEqual(await pending, { granted: true, scope: "once" });
});
test("an unknown request is ignored", () => {
  const book = new ApprovalBook();
  assert.equal(book.answer("t1", "missing", { granted: true, scope: "once" }), false);
});
test("a second answer for one request is ignored", async () => {
  const book = new ApprovalBook();
  const pending = book.wait("t1", "r1");
  book.answer("t1", "r1", { granted: false, scope: "once", reason: "no" });
  assert.equal(book.answer("t1", "r1", { granted: true, scope: "once" }), false);
  assert.deepEqual(await pending, { granted: false, scope: "once", reason: "no" });
});
test("closing a tab denies every pending decision with the reason", async () => {
  const book = new ApprovalBook();
  const p = book.wait("t1", "r1");
  book.denyTab("t1", "tab closed");
  assert.deepEqual(await p, { granted: false, scope: "once", reason: "tab closed" });
});
test("denying one tab does not touch another", () => {
  const book = new ApprovalBook();
  book.wait("t1", "r1"); book.wait("t2", "r2");
  book.denyTab("t1", "tab closed");
  assert.equal(book.pending("t1"), 0);
  assert.equal(book.pending("t2"), 1);
});
