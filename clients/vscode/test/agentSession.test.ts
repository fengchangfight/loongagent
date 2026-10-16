import { test } from "node:test";
import assert from "node:assert/strict";
import { AgentSession, JsonRpcError } from "../src/agentSession.ts";
import { FakeChild } from "./fakeChild.ts";

function makeSession() {
  const child = new FakeChild();
  const notifications: { method: string; params: unknown }[] = [];
  const logs: string[] = [];
  const exits: (number | null)[] = [];
  const session = new AgentSession(child, {
    onNotification: (method, params) => { notifications.push({ method, params }); },
    onLog: (line) => { logs.push(line); },
    onExit: (code) => { exits.push(code); },
  });
  return { child, session, notifications, logs, exits };
}

test("a response split across chunks is still matched", async () => {
  const { child, session } = makeSession();
  const pending = session.request("session.attach", { cwd: "/w", client: "vscode" });
  const id = child.sent()[0].id;
  child.emitStdout(`{"jsonrpc":"2.0","id":${id},"resu`);
  child.emitStdout(`lt":{"session_id":"s1"}}\n`);
  assert.deepEqual(await pending, { session_id: "s1" });
});

test("two messages in one chunk are both handled", async () => {
  const { child, session, notifications } = makeSession();
  const pending = session.request("session.new", { session_id: "s1" });
  const id = child.sent()[0].id;
  child.emitStdout(
    `{"jsonrpc":"2.0","id":${id},"result":{"session_id":"s2"}}\n` +
    `{"jsonrpc":"2.0","method":"model.delta","params":{"turn_id":1,"text":"hi"}}\n`,
  );
  assert.deepEqual(await pending, { session_id: "s2" });
  assert.deepEqual(notifications, [{ method: "model.delta", params: { turn_id: 1, text: "hi" } }]);
});

test("the id is what correlates, not the order", async () => {
  const { child, session } = makeSession();
  const first = session.request("session.replay", { session_id: "s1" });
  const firstId = child.sent()[0].id;
  const second = session.request("model.list", { session_id: "s1" });
  const secondId = child.sent()[1].id;
  assert.notEqual(firstId, secondId);
  child.emitStdout(`{"jsonrpc":"2.0","id":${secondId},"result":{"who":"second"}}\n`);
  child.emitStdout(`{"jsonrpc":"2.0","id":${firstId},"result":{"who":"first"}}\n`);
  assert.deepEqual(await first, { who: "first" });
  assert.deepEqual(await second, { who: "second" });
});

test("a JSON-RPC error response rejects with its code and message", async () => {
  const { child, session } = makeSession();
  const pending = session.request("nope.method", {});
  const id = child.sent()[0].id;
  // Attach the handler before the rejection can fire: a rejected promise with no
  // handler yet is an unhandled rejection, which the test runner reports as a failure.
  const check = assert.rejects(pending, (error: unknown) => {
    assert.ok(error instanceof JsonRpcError, "rejects with a JsonRpcError");
    assert.equal(error.code, -32601);
    assert.match(error.message, /unknown method/);
    return true;
  });
  child.emitStdout(
    `{"jsonrpc":"2.0","id":${id},"error":{"code":-32601,"message":"unknown method \\"nope.method\\""}}\n`,
  );
  await check;
});

test("a malformed line is logged and does not stop the reader", async () => {
  const { child, session, logs } = makeSession();
  const pending = session.request("session.replay", { session_id: "s1" });
  const id = child.sent()[0].id;
  child.emitStdout("not json\n");
  child.emitStdout(`{"jsonrpc":"2.0","id":${id},"result":{"events":[]}}\n`);
  assert.deepEqual(await pending, { events: [] });
  assert.equal(logs.length, 1);
  assert.match(logs[0], /not json/);
});

test("stderr lines reach onLog", () => {
  const { child, logs } = makeSession();
  child.stderr.emit("data", Buffer.from("warning: no config\n"));
  assert.deepEqual(logs, ["warning: no config"]);
});

test("a child that exits rejects every pending request with the exit code", async () => {
  const { child, session, exits } = makeSession();
  const first = session.request("session.replay", { session_id: "s1" });
  const second = session.request("model.list", { session_id: "s1" });
  const checks = [assert.rejects(first, /3/), assert.rejects(second, /3/)];
  child.emitExit(3);
  await Promise.all(checks);
  assert.deepEqual(exits, [3]);
});

test("cancel sends SIGINT to the child", () => {
  const { child, session } = makeSession();
  session.cancel();
  assert.deepEqual(child.signals, ["SIGINT"]);
});

test("a handler installed later receives later notifications", () => {
  const { child, session, notifications } = makeSession();
  const swapped: { method: string; params: unknown }[] = [];
  session.setNotificationHandler((method, params) => { swapped.push({ method, params }); });
  child.emitStdout(`{"jsonrpc":"2.0","method":"model.delta","params":{"turn_id":1,"text":"later"}}\n`);
  assert.deepEqual(swapped, [{ method: "model.delta", params: { turn_id: 1, text: "later" } }]);
  assert.deepEqual(notifications, [], "the original handler is replaced, not also called");
});
