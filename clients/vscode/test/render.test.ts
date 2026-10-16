import { test } from "node:test";
import assert from "node:assert/strict";
import { MAX_TOOL_OUTPUT_CHARS, Renderer, pathsInArgs } from "../src/render.ts";
import type { EventEnvelope } from "../../ts/protocol.generated.ts";

const CWD = "/w";

function event(kind: string, data: unknown, seq = 1): EventEnvelope {
  return { seq, id: `e${seq}`, kind, ignorable: false, time_ms: 0, data: data as never };
}

function textOf(parts: { text: string }[]): string {
  return parts.map((part) => part.text).join("\n");
}

test("a model delta becomes markdown, verbatim", () => {
  const renderer = new Renderer({ cwd: CWD });
  assert.deepEqual(renderer.delta("hi"), [{ kind: "markdown", text: "hi" }]);
  assert.deepEqual(renderer.delta(" wor"), [{ kind: "markdown", text: " wor" }]);
});

test("a tool call reports the tool, its action class and the path, and no verdict", () => {
  const renderer = new Renderer({ cwd: CWD });
  const parts = renderer.event(event("tool.call", {
    call_id: "c1",
    tool: "write_file",
    action_class: "write_workspace",
    args: { path: "/w/a.txt" },
  }));
  const text = textOf(parts);
  assert.match(text, /write_file/);
  assert.match(text, /write_workspace/);
  assert.match(text, /a\.txt/);
  assert.doesNotMatch(text, /policy|allow|deny/, "the event carries no verdict, so none is shown");
});

test("a tool result longer than the cap is truncated with a marker naming the loss", () => {
  const renderer = new Renderer({ cwd: CWD });
  const parts = renderer.event(event("tool.result", { call_id: "c1", ok: true, output: "x".repeat(10_000) }));
  const text = textOf(parts);
  assert.ok(text.length <= MAX_TOOL_OUTPUT_CHARS + 200, `rendered ${text.length} characters`);
  assert.match(text, /truncated/);
  assert.match(text, /6000/, "the marker says how much was dropped");
});

test("an unknown event kind renders nothing", () => {
  const renderer = new Renderer({ cwd: CWD });
  assert.deepEqual(renderer.event(event("something.new", { anything: true })), []);
});

test("a policy decision names the rule that fired", () => {
  const renderer = new Renderer({ cwd: CWD });
  const text = textOf(renderer.event(event("policy.decided", {
    action_class: "write_workspace",
    verdict: "allow",
    rule: "builtin:write_workspace",
  })));
  assert.match(text, /builtin:write_workspace/);
  assert.match(text, /allow/);
});

test("an approval decision names the scope that was chosen", () => {
  const renderer = new Renderer({ cwd: CWD });
  const text = textOf(renderer.event(event("approval.decided", {
    request_id: "appr-1",
    granted: true,
    scope: "action_class",
  })));
  assert.match(text, /action_class/);
  assert.match(text, /granted/);
});

test("the extension does not repeat the user's own message", () => {
  const renderer = new Renderer({ cwd: CWD });
  assert.deepEqual(renderer.event(event("message.appended", { role: "user", text: "hello" })), []);
});

test("an assistant message the turn already streamed is not repeated", () => {
  const renderer = new Renderer({ cwd: CWD });
  renderer.beginTurn();
  renderer.delta("done");
  assert.deepEqual(renderer.event(event("message.appended", { role: "assistant", text: "done" })), []);
});

test("an assistant message nothing streamed is rendered", () => {
  const renderer = new Renderer({ cwd: CWD });
  renderer.beginTurn();
  const text = textOf(renderer.event(event("message.appended", { role: "assistant", text: "done" })));
  assert.match(text, /done/);
});

test("pathsInArgs finds absolute paths and ignores flags and urls", () => {
  assert.deepEqual(
    pathsInArgs({ path: "/w/a.txt", url: "https://x/y", flags: ["--verbose", "/w/b.txt"] }),
    ["/w/a.txt", "/w/b.txt"],
  );
});

test("pathsInArgs does not repeat a path it saw twice", () => {
  assert.deepEqual(pathsInArgs({ from: "/w/a.txt", to: "/w/a.txt" }), ["/w/a.txt"]);
});

test("touchedFiles accumulates within a turn and resets on beginTurn", () => {
  const renderer = new Renderer({ cwd: CWD });
  renderer.beginTurn();
  renderer.event(event("tool.call", { call_id: "c1", tool: "read_file", action_class: "investigate", args: { path: "/w/a.txt" } }));
  renderer.event(event("tool.call", { call_id: "c2", tool: "write_file", action_class: "write_workspace", args: { path: "/w/b.txt" } }));
  assert.deepEqual(renderer.touchedFiles(), ["/w/a.txt", "/w/b.txt"]);
  renderer.beginTurn();
  assert.deepEqual(renderer.touchedFiles(), []);
});

test("a path outside the workspace stays absolute so it reads as outside", () => {
  const renderer = new Renderer({ cwd: CWD });
  const text = textOf(renderer.event(event("tool.call", {
    call_id: "c1",
    tool: "read_file",
    action_class: "out_of_bounds",
    args: { path: "/etc/hostname" },
  })));
  assert.match(text, /\/etc\/hostname/);
});

test("a resumed session is summarised, not replayed in full", () => {
  const renderer = new Renderer({ cwd: CWD });
  const events: EventEnvelope[] = [];
  for (let i = 0; i < 20; i++) {
    events.push(event(
      "message.appended",
      { role: i % 2 === 0 ? "user" : "assistant", text: `message ${i}` },
      i + 1,
    ));
  }
  events.push(event("approval.decided", { request_id: "appr-1", granted: true, scope: "once" }, 21));

  const parts = renderer.resumed(events);
  const text = textOf(parts);
  assert.match(parts[0].text, /21/, "the first part names the event count");
  assert.match(text, /message 18/, "the newest question is shown");
  assert.match(text, /message 19/, "the newest answer is shown");
  assert.doesNotMatch(text, /message 0\b/, "the whole history is not replayed");
});

test("an empty replay says so instead of rendering nothing", () => {
  const parts = new Renderer({ cwd: CWD }).resumed([]);
  assert.equal(parts.length, 1);
  assert.match(parts[0].text, /no events/);
});

test("a session that stopped asking is announced, not silently quieter", () => {
  const renderer = new Renderer({ cwd: CWD });
  const text = textOf(renderer.event(event("permissions.relaxed", {
    auto_approve: true,
    reason: "the person asked for it",
  })));
  assert.match(text, /approval prompts are off/);
  assert.match(text, /the person asked for it/);
});

test("prompts coming back are announced too", () => {
  const renderer = new Renderer({ cwd: CWD });
  const text = textOf(renderer.event(event("permissions.relaxed", {
    auto_approve: false,
    reason: "back to asking",
  })));
  assert.match(text, /approval prompts are on/);
});

test("an approval request names the action class and the reason the policy gave", () => {
  const renderer = new Renderer({ cwd: CWD });
  const text = textOf(renderer.event(event("approval.requested", {
    request_id: "appr-1",
    action_class: "out_of_bounds",
    reason: "network access",
  })));
  assert.match(text, /out_of_bounds/);
  assert.match(text, /network access/);
});
