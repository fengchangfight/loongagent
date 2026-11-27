import { test } from "node:test";
import assert from "node:assert/strict";
import { EVENTS } from "../../ts/protocol.generated.ts";
import type { EventEnvelope } from "../../ts/protocol.generated.ts";
import {
  logMode,
  modeBadge,
  modeNotice,
  reconcileQuestions,
  refusedForApproval,
  switchDecision,
  toggledMode,
  waitingQuestions,
} from "../src/questions.ts";
import { parseMode } from "../src/webviewProtocol.ts";
import type { UiPart } from "../src/webviewProtocol.ts";

function card(requestId: string, state: "pending" | "allowed" | "denied" | "expired"): UiPart {
  return { kind: "approval", requestId, actionClass: "shell", reason: "r", paths: [], state };
}

function permissions(autoApprove: boolean, mode?: string): EventEnvelope {
  return {
    seq: 1,
    id: "e1",
    kind: EVENTS.permissionsRelaxed,
    ignorable: false,
    time_ms: 0,
    data: { auto_approve: autoApprove, reason: "asked", ...(mode === undefined ? {} : { mode }) },
  } as unknown as EventEnvelope;
}

test("only unanswered cards count as waiting", () => {
  const parts = [card("a", "pending"), card("b", "allowed"), card("c", "pending"), card("d", "expired")];
  assert.deepEqual(waitingQuestions(parts), ["a", "c"]);
});

test("a question the kernel still holds keeps its buttons", () => {
  const parts = [card("appr-1", "pending"), card("appr-2", "pending")];
  assert.deepEqual(reconcileQuestions(parts, "appr-1"), { keep: "appr-1", retire: ["appr-2"] });
});

test("with no question held, every parked card is retired", () => {
  const parts = [card("appr-1", "pending"), card("appr-2", "pending")];
  assert.deepEqual(reconcileQuestions(parts, undefined), { retire: ["appr-1", "appr-2"] });
});

test("a held question the log never showed keeps nothing and retires the rest", () => {
  assert.deepEqual(reconcileQuestions([card("appr-1", "pending")], "appr-9"),
    { retire: ["appr-1"] });
});

test("the exit from out of order names the approval to clear", () => {
  assert.equal(refusedForApproval(
    "out of order: approval appr-4 is still pending — answer it (approve or deny) "
    + "before sending another prompt"), "appr-4");
});

test("an unrelated failure names nothing", () => {
  assert.equal(refusedForApproval("the server went away"), undefined);
  assert.equal(refusedForApproval("a turn is already running"), undefined);
});

test("the log's last permissions event names the mode", () => {
  assert.equal(logMode([permissions(true, "god")], false), "god");
  assert.equal(logMode([permissions(true, "god"), permissions(false, "ask")], true), "ask");
});

test("an event from before modes existed is read by its boolean", () => {
  assert.equal(logMode([permissions(true)], false), "yolo");
  assert.equal(logMode([permissions(false)], true), "ask");
});

test("with no permissions event the attach result decides", () => {
  assert.equal(logMode([], true), "yolo");
  assert.equal(logMode([], false), "ask");
});

test("a mode is only one of the three", () => {
  assert.equal(parseMode("god"), "god");
  assert.equal(parseMode("yolo"), "yolo");
  assert.equal(parseMode("ask"), "ask");
  assert.equal(parseMode("off"), undefined);
  assert.equal(parseMode(undefined), undefined);
});

test("/yolo and /god toggle the way the TUI toggles them", () => {
  assert.equal(toggledMode("yolo", "ask"), "yolo");
  assert.equal(toggledMode("yolo", "yolo"), "ask");
  assert.equal(toggledMode("yolo", "god"), "yolo");
  assert.equal(toggledMode("god", "ask"), "god");
  assert.equal(toggledMode("god", "yolo"), "god");
  assert.equal(toggledMode("god", "god"), "ask");
});

test("the header marks a relaxed session by the mode's own name", () => {
  assert.equal(modeBadge("ask"), "");
  assert.equal(modeBadge("yolo"), "⚠ YOLO", "yolo is named yolo, the command that sets it");
  assert.equal(modeBadge("god"), "⚡ GOD");
  assert.notEqual(modeBadge("yolo"), modeBadge("god"), "the two relaxed modes never look alike");
});

test("each mode change makes that mode's own promise", () => {
  assert.ok(modeNotice("yolo").includes("sandbox still applies"), "yolo leaves the sandbox alone");
  assert.ok(modeNotice("yolo").includes("irreversible"), "yolo still asks about irreversible actions");
  assert.ok(modeNotice("god").includes("outside the workspace"));
  assert.ok(modeNotice("god").includes("secret"));
  assert.ok(modeNotice("ask").includes("on again"));
});

test("a switch never waits on a running turn", () => {
  assert.equal(switchDecision(true, "god", "ask", undefined), "queue",
    "a running turn cannot read the request, so the switch is queued rather than awaited");
  assert.equal(switchDecision(false, "god", "ask", undefined), "apply");
});

test("asking for the mode already in force does nothing, or cancels a queued switch", () => {
  assert.equal(switchDecision(false, "ask", "ask", undefined), "ignore");
  assert.equal(switchDecision(true, "ask", "ask", "god"), "cancel-pending",
    "clicking back to the current mode withdraws the switch still waiting");
  assert.equal(switchDecision(true, "yolo", "ask", "god"), "queue",
    "a second choice during the turn replaces the first");
});

test("a deferred switch says both what it promises and when it lands", () => {
  const deferred = modeNotice("god", { deferred: true });
  assert.ok(deferred.includes("outside the workspace"), "the promise is not lost by deferring");
  assert.ok(deferred.includes("when this turn ends"), "the person is told it is not in force yet");
  assert.ok(!modeNotice("god").includes("when this turn ends"),
    "an applied switch does not pretend to be waiting");
});
