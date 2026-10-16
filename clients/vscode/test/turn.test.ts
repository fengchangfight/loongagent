import { test } from "node:test";
import assert from "node:assert/strict";
import { AgentSession } from "../src/agentSession.ts";
import { LoongSession } from "../src/session.ts";
import {
  ALLOW_ALWAYS,
  ALLOW_ONCE,
  answerFor,
  DENY,
  TurnBusyError,
  TurnController,
  TurnGate,
} from "../src/turn.ts";
import { FakeChild } from "./fakeChild.ts";
import type {
  ActionClass,
  ApprovalScope,
  EventEnvelope,
} from "../../ts/protocol.generated.ts";

function makeTurn(decide: (request: { request_id: string }) => Promise<{
  granted: boolean;
  scope: ApprovalScope;
  reason?: string;
}>) {
  const child = new FakeChild();
  const logs: string[] = [];
  const agent = new AgentSession(child, {
    onNotification: () => {},
    onLog: (line) => { logs.push(line); },
    onExit: () => {},
  });
  const session = new LoongSession(agent);
  const events: EventEnvelope[] = [];
  const deltas: string[] = [];
  const controller = new TurnController(session, {
    onEvent: (event) => { events.push(event); },
    onDelta: (text) => { deltas.push(text); },
    decide,
  });
  return { child, session, controller, events, deltas, logs };
}

function event(kind: string, data: unknown, seq = 1): EventEnvelope {
  return { seq, id: `e${seq}`, kind, ignorable: false, time_ms: 0, data: data as never };
}

function approvalRequested(requestId: string, actionClass: ActionClass, reason: string, seq = 1): EventEnvelope {
  return event("approval.requested", { request_id: requestId, action_class: actionClass, reason }, seq);
}

function notify(child: FakeChild, envelope: EventEnvelope): void {
  child.emitStdout(`${JSON.stringify({ jsonrpc: "2.0", method: "event", params: envelope })}\n`);
}

/** Answer the client's nth request with a result. */
function answer(child: FakeChild, index: number, result: unknown): void {
  child.emitStdout(`${JSON.stringify({ jsonrpc: "2.0", id: child.sent()[index].id, result })}\n`);
}

/** Answer the client's nth request with an error. */
function answerError(child: FakeChild, index: number, code: number, message: string): void {
  child.emitStdout(`${JSON.stringify({ jsonrpc: "2.0", id: child.sent()[index].id, error: { code, message } })}\n`);
}

/** Wait for something the controller does in a later microtask than the answer we just gave. */
async function until(predicate: () => boolean, label: string): Promise<void> {
  for (let attempt = 0; attempt < 200; attempt++) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 1));
  }
  throw new Error(`timed out waiting for ${label}`);
}

async function attach(child: FakeChild, session: LoongSession): Promise<void> {
  const pending = session.attach("/w");
  answer(child, 0, { session_id: "s1", schema_version: "0.1.0", existing_events: 0 });
  await pending;
}

test("one approval pause is answered and the turn finishes", async () => {
  const requests: string[] = [];
  const { child, session, controller } = makeTurn(async (request) => {
    requests.push(request.request_id);
    return { granted: true, scope: "once" };
  });
  await attach(child, session);

  const running = controller.run("write a file");
  assert.equal(child.sent()[1].method, "prompt.submit");
  assert.deepEqual(child.sent()[1].params, { session_id: "s1", text: "write a file" });

  notify(child, approvalRequested("appr-1", "write_workspace", "writes inside the workspace"));
  answer(child, 1, { turn_id: 1, accepted: true });

  await until(() => child.sent().length === 3, "approval.respond");
  assert.equal(child.sent()[2].method, "approval.respond");
  assert.deepEqual(child.sent()[2].params, {
    session_id: "s1",
    request_id: "appr-1",
    granted: true,
    scope: "once",
  });

  answer(child, 2, { turn_id: 1, accepted: true });
  assert.deepEqual(await running, { turn_id: 1, accepted: true });
  assert.deepEqual(requests, ["appr-1"]);
  assert.equal(controller.busy, false);
});

test("the pending approval is visible before prompt.submit resolves", async () => {
  const { child, session, controller } = makeTurn(async () => ({ granted: true, scope: "once" }));
  await attach(child, session);

  const running = controller.run("x");
  notify(child, approvalRequested("appr-9", "out_of_bounds", "network access"));
  await until(() => controller.pendingApproval !== null, "the pending approval");
  assert.equal(controller.pendingApproval?.request_id, "appr-9");
  assert.equal(controller.busy, true);

  answer(child, 1, { turn_id: 1, accepted: true });
  await until(() => child.sent().length === 3, "approval.respond");
  answer(child, 2, { turn_id: 1, accepted: true });
  await running;
});

test("two approvals in one turn are both answered, in order", async () => {
  const asked: string[] = [];
  const { child, session, controller } = makeTurn(async (request) => {
    asked.push(request.request_id);
    return { granted: true, scope: "once" };
  });
  await attach(child, session);

  const running = controller.run("x");
  notify(child, approvalRequested("appr-1", "write_workspace", "first"));
  answer(child, 1, { turn_id: 1, accepted: true });

  await until(() => child.sent().length === 3, "the first approval.respond");
  assert.equal(child.sent()[2].params && (child.sent()[2].params as { request_id: string }).request_id, "appr-1");

  notify(child, approvalRequested("appr-2", "write_workspace", "second"));
  answer(child, 2, { turn_id: 1, accepted: true });

  await until(() => child.sent().length === 4, "the second approval.respond");
  assert.equal((child.sent()[3].params as { request_id: string }).request_id, "appr-2");

  answer(child, 3, { turn_id: 1, accepted: true });
  await running;
  assert.deepEqual(asked, ["appr-1", "appr-2"]);
});

test("a denied approval is faithfully reported", async () => {
  const { child, session, controller } = makeTurn(async () => ({
    granted: false,
    scope: "once",
    reason: "denied in VS Code",
  }));
  await attach(child, session);

  const running = controller.run("x");
  notify(child, approvalRequested("appr-1", "out_of_bounds", "network access"));
  answer(child, 1, { turn_id: 1, accepted: true });
  await until(() => child.sent().length === 3, "approval.respond");

  assert.deepEqual(child.sent()[2].params, {
    session_id: "s1",
    request_id: "appr-1",
    granted: false,
    scope: "once",
    reason: "denied in VS Code",
  });
  answer(child, 2, { turn_id: 1, accepted: false });
  assert.equal((await running).accepted, false);
});

test("a second run while a turn is running is refused", async () => {
  const { child, session, controller } = makeTurn(async () => ({ granted: true, scope: "once" }));
  await attach(child, session);

  const running = controller.run("first");
  const before = child.sent().length;
  await assert.rejects(controller.run("second"), TurnBusyError);
  assert.equal(child.sent().length, before, "the refused run writes nothing");

  answer(child, 1, { turn_id: 1, accepted: true });
  await running;
});

test("a child that exits mid-turn rejects run with the exit code", async () => {
  const { child, session, controller } = makeTurn(async () => ({ granted: true, scope: "once" }));
  await attach(child, session);

  const running = controller.run("x");
  const check = assert.rejects(running, /7/);
  child.emitExit(7);
  await check;
  assert.equal(controller.busy, false, "the bus is released when the turn fails");
});

test("bus is released after a failed turn", async () => {
  const { child, session, controller } = makeTurn(async () => ({ granted: true, scope: "once" }));
  await attach(child, session);

  const failing = controller.run("one");
  const check = assert.rejects(failing, /boom/);
  answerError(child, 1, -32000, "boom: the turn failed");
  await check;
  assert.equal(controller.busy, false);

  const second = controller.run("two");
  assert.equal(child.sent()[2].method, "prompt.submit");
  answer(child, 2, { turn_id: 2, accepted: true });
  assert.deepEqual(await second, { turn_id: 2, accepted: true });
});

test("the controller forwards events and deltas to its hooks", async () => {
  const { child, session, controller, events, deltas } = makeTurn(async () => ({ granted: true, scope: "once" }));
  await attach(child, session);
  const running = controller.run("x");
  child.emitStdout(`{"jsonrpc":"2.0","method":"model.delta","params":{"turn_id":1,"text":"hi"}}\n`);
  notify(child, event("message.appended", { role: "assistant", text: "hi" }, 5));
  assert.deepEqual(deltas, ["hi"]);
  assert.deepEqual(events.map((e) => e.kind), ["message.appended"]);
  answer(child, 1, { turn_id: 1, accepted: true });
  await running;
});

function makeGate() {
  const child = new FakeChild();
  const agent = new AgentSession(child, {
    onNotification: () => {},
    onLog: () => {},
    onExit: () => {},
  });
  const session = new LoongSession(agent);
  return { child, session, gate: new TurnGate(session) };
}

function sinkInto(deltas: string[]): {
  onEvent: () => void;
  onDelta: (text: string) => void;
  decide: () => Promise<{ granted: boolean; scope: "once" }>;
} {
  return {
    onEvent: () => {},
    onDelta: (text) => { deltas.push(text); },
    decide: async () => ({ granted: true, scope: "once" }),
  };
}

// The two-tab failure the plan's Review Focus #1 names: with a controller per
// request, the second request both escaped the busy guard and stole the first
// turn's stream, because a LoongSession keeps one event and one delta observer.
test("a second request cannot take the running turn", async () => {
  const { child, session, gate } = makeGate();
  await attach(child, session);

  const controller = gate.begin(sinkInto([]));
  const running = controller.run("first");

  assert.throws(() => gate.begin(sinkInto([])), TurnBusyError);

  answer(child, 1, { turn_id: 1, accepted: true });
  await running;
  gate.end();
  assert.equal(gate.busy, false);
});

test("a refused request cannot steal the running turn's stream", async () => {
  const { child, session, gate } = makeGate();
  await attach(child, session);

  const first: string[] = [];
  const second: string[] = [];
  const controller = gate.begin(sinkInto(first));
  const running = controller.run("first");

  assert.throws(() => gate.begin(sinkInto(second)), TurnBusyError);
  child.emitStdout(`{"jsonrpc":"2.0","method":"model.delta","params":{"turn_id":1,"text":"answer-for-the-first"}}\n`);

  assert.deepEqual(first, ["answer-for-the-first"]);
  assert.deepEqual(second, [], "the refused request hears nothing of the running turn");

  answer(child, 1, { turn_id: 1, accepted: true });
  await running;
  gate.end();
});

test("the turn is available again once it ends", async () => {
  const { child, session, gate } = makeGate();
  await attach(child, session);

  const running = gate.begin(sinkInto([])).run("first");
  answer(child, 1, { turn_id: 1, accepted: true });
  await running;
  gate.end();

  const next = gate.begin(sinkInto([]));
  assert.equal(next.busy, false);
  const second = next.run("second");
  answer(child, 2, { turn_id: 2, accepted: true });
  assert.deepEqual(await second, { turn_id: 2, accepted: true });
});

test("allow once and allow always map to the protocol's scopes", () => {
  assert.deepEqual(answerFor(ALLOW_ONCE), { granted: true, scope: "once" });
  assert.deepEqual(answerFor(ALLOW_ALWAYS), { granted: true, scope: "action_class" });
});

test("a dismissed dialog is a denial, never an allow", () => {
  const denied = { granted: false, scope: "once", reason: "denied in VS Code" };
  assert.deepEqual(answerFor(undefined), denied);
  assert.deepEqual(answerFor(DENY), denied);
});
