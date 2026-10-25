import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const pkg = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8"));

test("the engine floor is the installed VS Code", () => {
  assert.equal(pkg.engines.vscode, "^1.138.0");
});
test("no proposed API is enabled", () => {
  assert.equal(pkg.enabledApiProposals, undefined);
});
test("the chat participant is @loong and sticky", () => {
  const [p] = pkg.contributes.chatParticipants;
  assert.equal(p.id, "loongagent.chat");
  assert.equal(p.name, "loong");
  assert.equal(p.isSticky, true);
  assert.deepEqual(p.commands.map((c: { name: string }) => c.name).sort(),
    ["context", "model", "new", "yolo"]);
});
test("the entry point is the bundled file", () => {
  assert.equal(pkg.main, "./dist/extension.js");
});
test("an untrusted workspace is only partly supported", () => {
  assert.equal(pkg.capabilities.untrustedWorkspaces.supported, "limited");
  assert.ok(pkg.capabilities.untrustedWorkspaces.description.length > 0);
});
test("the binary path, model and auto-approve settings exist", () => {
  const props = pkg.contributes.configuration.properties;
  assert.equal(props["loongagent.path"].default, "loongagent");
  assert.equal(props["loongagent.autoApprove"].default, false);
  assert.ok("loongagent.model" in props && "loongagent.extraArgs" in props);
});
