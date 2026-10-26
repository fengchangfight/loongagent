import { test } from "node:test";
import assert from "node:assert/strict";
import { renderMarkdown } from "../src/markdown.ts";

test("raw HTML is escaped, never passed through", () => {
  const html = renderMarkdown('<img src=x onerror="alert(1)"><script>alert(1)</script>');
  assert.ok(!html.includes("<img"));
  assert.ok(!html.includes("<script"));
  assert.ok(html.includes("&lt;img"));
});
test("a fenced block keeps its text and does not close early", () => {
  const html = renderMarkdown("```ts\nconst a = '<b>';\n```");
  assert.ok(html.includes("<pre><code"));
  assert.ok(html.includes("&lt;b&gt;"));
});
test("inline code and bold are rendered", () => {
  const html = renderMarkdown("**bold** and `code`");
  assert.ok(html.includes("<strong>bold</strong>"));
  assert.ok(html.includes("<code>code</code>"));
});
test("only http and https links become anchors", () => {
  assert.ok(renderMarkdown("[x](https://e.com)").includes('href="https://e.com"'));
  assert.ok(!renderMarkdown("[x](javascript:alert(1))").includes("href="));
});
test("an unmatched fence is text, not a swallowed remainder", () => {
  const html = renderMarkdown("```\nstill text");
  assert.ok(html.includes("still text"));
  assert.ok(!html.includes("<pre>"));
});
