import { test } from "node:test";
import assert from "node:assert/strict";
import { binaryName, candidatePaths, parseVersion, resolveBinary } from "../src/binary.ts";

const env = (over: Partial<Parameters<typeof resolveBinary>[0]> = {}) => ({
  configured: "loongagent",
  platform: "linux" as const,
  home: "/home/u",
  pathEnv: "/usr/bin:/home/u/.cargo/bin",
  exists: (p: string) => ["/home/u/.cargo/bin/loongagent"].includes(p),
  isExecutable: () => true,
  ...over,
});

test("a configured absolute path wins", () => {
  const found = resolveBinary(env({ configured: "/opt/l/loongagent",
    exists: (p) => p === "/opt/l/loongagent" }));
  assert.deepEqual(found, { path: "/opt/l/loongagent", source: "setting" });
});
test("a picked path beats PATH", () => {
  const found = resolveBinary(env({ picked: "/home/u/bin/loongagent",
    exists: (p) => p === "/home/u/bin/loongagent" }));
  assert.deepEqual(found, { path: "/home/u/bin/loongagent", source: "picked" });
});
test("a missing setting falls through to PATH", () => {
  assert.deepEqual(resolveBinary(env()), {
    path: "/home/u/.cargo/bin/loongagent", source: "path" });
});
test("common candidates are the last resort", () => {
  const found = resolveBinary(env({ pathEnv: "/usr/bin",
    exists: (p) => p === "/home/u/.local/bin/loongagent" }));
  assert.deepEqual(found, { path: "/home/u/.local/bin/loongagent", source: "candidate" });
});
test("Windows uses loongagent.exe", () => {
  assert.equal(binaryName("win32"), "loongagent.exe");
  assert.ok(candidatePaths("win32", "C:\\Users\\u")
    .every((p) => p.endsWith("loongagent.exe")));
});
test("parseVersion reads version, schema and commit", () => {
  assert.deepEqual(parseVersion("loongagent 0.0.1 (schema 0.1.0, commit abc+dirty)"),
    { version: "0.0.1", schema: "0.1.0", commit: "abc+dirty" });
  assert.equal(parseVersion("not a version"), undefined);
});
