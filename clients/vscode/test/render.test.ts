import { test } from "node:test";
import assert from "node:assert/strict";
import { MAX_TOOL_OUTPUT_CHARS, Renderer, pathsInArgs } from "../src/render.ts";
import type { ContextExplain, EventEnvelope } from "../../ts/protocol.generated.ts";
import type { UiPart } from "../src/webviewProtocol.ts";

const CWD = "/w";

function event(kind: string, data: unknown, seq = 1): EventEnvelope {
  return { seq, id: `e${seq}`, kind, ignorable: false, time_ms: 0, data: data as never };
}

function textOf(parts: UiPart[]): string {
  return parts.map((part) => {
    switch (part.kind) {
      case "tool":
        return [part.tool, part.actionClass, ...part.paths, part.output ?? "",
          part.truncated === undefined ? "" : `truncated ${part.truncated}`].join("\n");
      case "approval":
        return `${part.actionClass} ${part.reason}`;
      case "review":
        return part.paths.join("\n");
      default:
        return part.text;
    }
  }).join("\n");
}

test("a relative write_file path is resolved against the workspace", () => {
  assert.deepEqual(pathsInArgs({ path: "demo.txt" }, CWD), ["/w/demo.txt"]);
});

test("a tool call names the tool, class and relative path", () => {
  const parts = new Renderer({ cwd: CWD }).event(event("tool.call", {
    call_id: "c1", tool: "write_file", action_class: "write_workspace", args: { path: "demo.txt" },
  }));
  assert.equal(parts[0].kind, "tool");
  if (parts[0].kind !== "tool") throw new Error("expected tool");
  assert.deepEqual(parts[0].paths, ["/w/demo.txt"]);
  assert.equal(parts[0].status, "running");
  assert.doesNotMatch(textOf(parts), /policy|allow|deny/);
});

test("a tool result is capped, names its loss and keeps the call id", () => {
  const [part] = new Renderer({ cwd: CWD }).event(event("tool.result", {
    call_id: "c1", ok: true, output: "x".repeat(10_000),
  }));
  assert.equal(part.kind, "tool");
  if (part.kind !== "tool") throw new Error("expected tool");
  assert.equal(part.callId, "c1");
  assert.equal(part.status, "ok");
  assert.equal(part.output?.length, MAX_TOOL_OUTPUT_CHARS);
  assert.equal(part.truncated, 6000);
});

test("an unknown event kind renders nothing", () => {
  assert.deepEqual(new Renderer({ cwd: CWD }).event(event("something.new", { anything: true })), []);
});

test("a policy decision names the rule that fired", () => {
  const text = textOf(new Renderer({ cwd: CWD }).event(event("policy.decided", {
    action_class: "write_workspace", verdict: "allow", rule: "builtin:write_workspace",
  })));
  assert.match(text, /builtin:write_workspace/);
  assert.match(text, /allow/);
});

test("an approval decision names the scope that was chosen", () => {
  const text = textOf(new Renderer({ cwd: CWD }).event(event("approval.decided", {
    request_id: "appr-1", granted: true, scope: "action_class",
  })));
  assert.match(text, /action_class/);
  assert.match(text, /granted/);
});

test("the extension does not repeat the user's own message", () => {
  assert.deepEqual(new Renderer({ cwd: CWD }).event(
    event("message.appended", { role: "user", text: "hello" })), []);
});

test("an assistant message is rendered once, with sanitized HTML", () => {
  const [part] = new Renderer({ cwd: CWD }).event(
    event("message.appended", { role: "assistant", text: "**hi** <script>x</script>" }));
  assert.equal(part.kind, "assistant");
  if (part.kind !== "assistant") throw new Error("expected assistant");
  assert.ok(part.html.includes("<strong>hi</strong>"));
  assert.ok(!part.html.includes("<script"));
});

test("pathsInArgs keeps absolute paths and ignores flags and urls", () => {
  assert.deepEqual(
    pathsInArgs({ path: "/w/a.txt", url: "https://x/y", flags: ["--verbose", "/w/b.txt"] }, CWD),
    ["/w/a.txt", "/w/b.txt"],
  );
});

test("content strings are not harvested as paths", () => {
  assert.deepEqual(pathsInArgs({ path: "/w/a.txt", content: "/w/not-a-path.txt" }, CWD),
    ["/w/a.txt"]);
});

test("pathsInArgs does not repeat a path it saw twice", () => {
  assert.deepEqual(pathsInArgs({ from: "/w/a.txt", to: "/w/a.txt" }, CWD), ["/w/a.txt"]);
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
  const parts = new Renderer({ cwd: CWD }).event(event("tool.call", {
    call_id: "c1", tool: "read_file", action_class: "out_of_bounds", args: { path: "/etc/hostname" },
  }));
  assert.match(textOf(parts), /\/etc\/hostname/);
});

test("a resumed session is summarised, not replayed in full", () => {
  const renderer = new Renderer({ cwd: CWD });
  const events: EventEnvelope[] = [];
  for (let i = 0; i < 20; i++) {
    events.push(event("message.appended",
      { role: i % 2 === 0 ? "user" : "assistant", text: `message ${i}` }, i + 1));
  }
  events.push(event("approval.decided", { request_id: "appr-1", granted: true, scope: "once" }, 21));
  const parts = renderer.resumed(events, "s7");
  assert.equal(parts[0].kind, "notice");
  if (parts[0].kind !== "notice") throw new Error("expected notice");
  const text = textOf(parts);
  assert.match(parts[0].text, /21/);
  assert.match(parts[0].text, /s7/);
  assert.match(text, /message 18/);
  assert.match(text, /message 19/);
  assert.doesNotMatch(text, /message 0\b/);
});

test("an empty replay says so instead of rendering nothing", () => {
  const parts = new Renderer({ cwd: CWD }).resumed([], "s7");
  assert.equal(parts.length, 1);
  assert.equal(parts[0].kind, "notice");
  if (parts[0].kind === "notice") assert.match(parts[0].text, /no events/);
});

test("a session that stopped asking is announced, not silently quieter", () => {
  const text = textOf(new Renderer({ cwd: CWD }).event(event("permissions.relaxed", {
    auto_approve: true, reason: "the person asked for it",
  })));
  assert.match(text, /approval prompts are off/);
  assert.match(text, /the person asked for it/);
});

test("prompts coming back are announced too", () => {
  const text = textOf(new Renderer({ cwd: CWD }).event(event("permissions.relaxed", {
    auto_approve: false, reason: "back to asking",
  })));
  assert.match(text, /approval prompts are on/);
});

test("an approval request carries its request id, class and reason", () => {
  const [part] = new Renderer({ cwd: CWD }).event(event("approval.requested", {
    request_id: "appr-1", action_class: "out_of_bounds", reason: "network access",
  }));
  assert.equal(part.kind, "approval");
  if (part.kind !== "approval") throw new Error("expected approval");
  assert.equal(part.requestId, "appr-1");
  assert.equal(part.state, "pending");
  assert.match(textOf([part]), /out_of_bounds/);
  assert.match(textOf([part]), /network access/);
});

function explain(overrides: Partial<ContextExplain> = {}): ContextExplain {
  return {
    context_tokens: 1200, budget_tokens: 8000, messages: 4, tools: 9, compactions: 0,
    entries: [], categories: [], memory_mode: "", memory_summarizer: "", memory_degraded: "",
    memory_layers: [],
    memory_worker: { state: "off", windows: 0, model_calls: 0, enriched: 0,
      failed: 0, pending: 0, deferrals: 0, last_error: "" },
    ...overrides,
  };
}

test("the model list names every model and marks the current one", () => {
  const text = textOf(new Renderer({ cwd: CWD }).models({
    models: [{ name: "a", model: "a-model" }, { name: "b", model: "b-model" }],
    selected: "b",
  }));
  assert.match(text, /`a` \(a-model\)/);
  assert.match(text, /`b` \(b-model\)/);
  assert.match(text, /`b` \(b-model\) — current/);
  assert.doesNotMatch(text, /`a` \(a-model\) — current/);
});

test("the context notice quotes the numbers it was given and computes nothing", () => {
  const text = textOf(new Renderer({ cwd: CWD }).context(explain()));
  for (const number of ["1200", "8000", "4", "9"]) {
    assert.match(text, new RegExp(`\\b${number}\\b`), `${number} appears as it was given`);
  }
  assert.doesNotMatch(text, /%/);
});
