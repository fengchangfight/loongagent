import { test } from "node:test";
import assert from "node:assert/strict";
import { AgentSession } from "../src/agentSession.ts";
import { LoongSession, SessionRefusedError } from "../src/session.ts";
import { FakeChild } from "./fakeChild.ts";
import type { EventEnvelope } from "../../ts/protocol.generated.ts";

function makeSession() {
  const child = new FakeChild();
  const logs: string[] = [];
  const agent = new AgentSession(child, {
    onNotification: () => {},
    onLog: (line) => { logs.push(line); },
    onExit: () => {},
  });
  const session = new LoongSession(agent);
  return { child, agent, session, logs };
}

function envelope(seq: number): EventEnvelope {
  return {
    seq,
    id: `e${seq}`,
    kind: "message.appended",
    ignorable: false,
    time_ms: 0,
    data: { role: "assistant", text: `m${seq}` },
  };
}

/** Answer the client's nth request with a result. */
function answer(child: FakeChild, index: number, result: unknown): void {
  const request = child.sent()[index];
  child.emitStdout(`${JSON.stringify({ jsonrpc: "2.0", id: request.id, result })}\n`);
}

/** Answer the client's nth request with an error. */
function answerError(child: FakeChild, index: number, code: number, message: string): void {
  const request = child.sent()[index];
  child.emitStdout(`${JSON.stringify({ jsonrpc: "2.0", id: request.id, error: { code, message } })}\n`);
}

/** Notify the client of an event. */
function notifyEvent(child: FakeChild, event: EventEnvelope): void {
  child.emitStdout(`${JSON.stringify({ jsonrpc: "2.0", method: "event", params: event })}\n`);
}

async function attachTo(child: FakeChild, session: LoongSession, id: string): Promise<void> {
  const pending = session.attach("/w");
  answer(child, 0, { session_id: id, schema_version: "0.1.0", existing_events: 0 });
  await pending;
}

test("attach records the session id and asks as vscode", async () => {
  const { child, session } = makeSession();
  const pending = session.attach("/w");
  const request = child.sent()[0];
  assert.equal(request.method, "session.attach");
  assert.deepEqual(request.params, { cwd: "/w", client: "vscode" });
  answer(child, 0, { session_id: "s1", schema_version: "0.1.0", existing_events: 0 });
  const result = await pending;
  assert.equal(result.session_id, "s1");
  assert.equal(session.sessionId, "s1");
});

test("a request before attach says to attach first", async () => {
  const { session } = makeSession();
  await assert.rejects(session.replay(), /attach/);
});

test("a refusal names the session it is not on", async () => {
  const { child, session } = makeSession();
  await attachTo(child, session, "s1");
  const pending = session.newSession();
  const check = assert.rejects(pending, (error: unknown) => {
    assert.ok(error instanceof SessionRefusedError, "rejects with a SessionRefusedError");
    assert.equal(error.sessionId, "s1");
    assert.match(error.message, /s1/);
    return true;
  });
  answerError(child, 1, -32000, 'this server is on session "s1" and cannot begin another');
  await check;
});

test("newSession names the current session and adopts the new id", async () => {
  const { child, session } = makeSession();
  await attachTo(child, session, "s1");
  const pending = session.newSession();
  assert.deepEqual(child.sent()[1].params, { session_id: "s1" });
  answer(child, 1, { session_id: "s2" });
  assert.equal(await pending, "s2");
  assert.equal(session.sessionId, "s2");
});

test("replay returns the events and live events accumulate", async () => {
  const { child, session } = makeSession();
  await attachTo(child, session, "s1");
  const pending = session.replay();
  answer(child, 1, { events: [envelope(1), envelope(2)] });
  const events = await pending;
  assert.equal(events.length, 2);
  assert.equal(session.events.length, 2);
  notifyEvent(child, envelope(3));
  assert.equal(session.events.length, 3);
  assert.equal(session.events[2].seq, 3);
});

test("respondApproval sends the scope it was given", async () => {
  const { child, session } = makeSession();
  await attachTo(child, session, "s1");
  const pending = session.respondApproval("appr-1", false, "once", "denied in VS Code");
  assert.equal(child.sent()[1].method, "approval.respond");
  assert.deepEqual(child.sent()[1].params, {
    session_id: "s1",
    request_id: "appr-1",
    granted: false,
    scope: "once",
    reason: "denied in VS Code",
  });
  answer(child, 1, { turn_id: 1, accepted: false });
  assert.equal(await pending, false);
});

test("setAutoApprove sends the flag and reports the new state", async () => {
  const { child, session } = makeSession();
  await attachTo(child, session, "s1");
  const pending = session.setAutoApprove(true);
  assert.deepEqual(child.sent()[1].params, { session_id: "s1", auto_approve: true });
  answer(child, 1, { auto_approve: true });
  assert.equal(await pending, true);
});

test("cancel reaches the child", () => {
  const { child, session } = makeSession();
  session.cancel();
  assert.deepEqual(child.signals, ["SIGINT"]);
});

test("submit names the session and returns the turn", async () => {
  const { child, session } = makeSession();
  await attachTo(child, session, "s1");
  const pending = session.submit("hello");
  assert.equal(child.sent()[1].method, "prompt.submit");
  assert.deepEqual(child.sent()[1].params, { session_id: "s1", text: "hello" });
  answer(child, 1, { turn_id: 1, accepted: true });
  assert.deepEqual(await pending, { turn_id: 1, accepted: true });
});

test("an event observer hears events as they arrive", () => {
  const { child, session } = makeSession();
  const heard: number[] = [];
  session.onEvent((event) => { heard.push(event.seq); });
  notifyEvent(child, envelope(1));
  notifyEvent(child, envelope(2));
  assert.deepEqual(heard, [1, 2]);
  assert.equal(session.events.length, 2, "observing does not stop the events accumulating");
});

test("a delta observer hears each fragment", () => {
  const { child, session } = makeSession();
  const fragments: string[] = [];
  session.onDelta((text) => { fragments.push(text); });
  child.emitStdout(`{"jsonrpc":"2.0","method":"model.delta","params":{"turn_id":1,"text":"he"}}\n`);
  child.emitStdout(`{"jsonrpc":"2.0","method":"model.delta","params":{"turn_id":1,"text":"llo"}}\n`);
  assert.deepEqual(fragments, ["he", "llo"]);
});
