import { test } from "node:test";
import assert from "node:assert/strict";
import { SessionCore } from "../src/sessionCore.ts";

const fakeSession = () => ({
  onEvent: () => {},
  onDelta: () => {},
  submit: async () => ({ turn_id: 1, accepted: true }),
  respondApproval: async () => true,
  cancel: () => {},
}) as never;

test("adopting a session creates a gate", () => {
  const core = new SessionCore();
  core.adopt(fakeSession());
  assert.ok(core.turnGate);
  assert.ok(core.alive());
});
test("forgetting the dead child clears the gate", () => {
  const core = new SessionCore();
  core.adopt(fakeSession());
  core.forget();
  assert.equal(core.session, undefined);
  assert.equal(core.turnGate, undefined);
  assert.equal(core.alive(), false);
});
test("a new session gets a new gate, not the dead one", () => {
  const core = new SessionCore();
  core.adopt(fakeSession());
  const dead = core.turnGate;
  core.forget();
  core.adopt(fakeSession());
  assert.notEqual(core.turnGate, dead);
});
