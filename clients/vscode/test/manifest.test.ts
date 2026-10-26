import { test } from "node:test";
import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";

const pkg = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8"));

test("the engine floor is the installed VS Code", () => {
  assert.equal(pkg.engines.vscode, "^1.138.0");
});
test("no proposed API is enabled", () => {
  assert.equal(pkg.enabledApiProposals, undefined);
});
test("the entry point is the bundled file", () => {
  assert.equal(pkg.main, "./dist/extension.js");
});
test("an untrusted workspace is only partly supported", () => {
  assert.equal(pkg.capabilities.untrustedWorkspaces.supported, "limited");
  assert.ok(pkg.capabilities.untrustedWorkspaces.description.length > 0);
});
test("the sidebar is contributed", () => {
  const [container] = pkg.contributes.viewsContainers.activitybar;
  assert.equal(container.id, "loongagent");
  assert.equal(container.icon, "media/loong-48.png");
  const [view] = pkg.contributes.views.loongagent;
  assert.equal(view.id, "loongagent.chat");
  assert.equal(view.type, "webview");
});
test("the native chat surface and placeholder model are gone", () => {
  assert.equal(pkg.contributes.chatParticipants, undefined);
  assert.equal(pkg.contributes.languageModelChatProviders, undefined);
});
test("the binary path is machine-scoped", () => {
  assert.equal(pkg.contributes.configuration.properties["loongagent.path"].scope, "machine");
  assert.equal(pkg.contributes.configuration.properties["loongagent.autoApprove"].scope, "machine");
  assert.equal(pkg.contributes.configuration.properties["loongagent.extraArgs"].scope, "machine");
});
test("the sidebar commands and selection menu are contributed", () => {
  const ids = pkg.contributes.commands.map((c: { command: string }) => c.command);
  for (const id of ["loongagent.openChat", "loongagent.locateBinary",
    "loongagent.attachSelection", "loongagent.reviewChanges"]) assert.ok(ids.includes(id));
  assert.equal(pkg.contributes.menus["editor/context"][0].command, "loongagent.attachSelection");
});
test("the derived icons exist", () => {
  for (const name of ["loong-24.png", "loong-48.png", "loong-128.png"]) {
    assert.ok(existsSync(new URL(`../media/${name}`, import.meta.url)));
  }
});
test("the binary, model and auto-approve settings exist", () => {
  const props = pkg.contributes.configuration.properties;
  assert.equal(props["loongagent.path"].default, "loongagent");
  assert.equal(props["loongagent.autoApprove"].default, false);
  assert.ok("loongagent.model" in props && "loongagent.extraArgs" in props);
});
