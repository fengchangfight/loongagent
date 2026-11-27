import { test } from "node:test";
import assert from "node:assert/strict";
import {
  effectivePathEnv,
  expandVariables,
  mergePathEntries,
} from "../src/windowsPath.ts";

test("expandVariables substitutes known names and leaves unknown ones", () => {
  assert.equal(
    expandVariables("%USERPROFILE%\\bin;%LOCALAPPDATA%\\x", { USERPROFILE: "C:\\Users\\u" }),
    "C:\\Users\\u\\bin;%LOCALAPPDATA%\\x",
  );
});
test("expandVariables does not loop on a self reference", () => {
  assert.equal(expandVariables("%PATH%", { PATH: "%PATH%" }), "%PATH%");
});
test("expandVariables expands a variable that references another", () => {
  assert.equal(
    expandVariables("%A%", { A: "%B%", B: "done" }),
    "done",
  );
});

test("mergePathEntries keeps process entries first and appends new ones", () => {
  assert.equal(
    mergePathEntries("C:\\old", "C:\\machine;C:\\old", "C:\\user"),
    "C:\\old;C:\\machine;C:\\user",
  );
});
test("mergePathEntries drops blanks and duplicates", () => {
  assert.equal(
    mergePathEntries("C:\\a;;C:\\a", "C:\\b;", undefined),
    "C:\\a;C:\\b",
  );
});

test("effectivePathEnv is the process PATH off Windows", () => {
  assert.equal(
    effectivePathEnv("linux", "/usr/bin", () => ({ machine: undefined, user: undefined }), {}),
    "/usr/bin",
  );
});
test("effectivePathEnv falls back to the process PATH when the read fails", () => {
  assert.equal(
    effectivePathEnv("win32", "C:\\proc", () => { throw new Error("boom"); }, {}),
    "C:\\proc",
  );
});
test("effectivePathEnv on Windows appends the registry halves to the process PATH", () => {
  const env = { USERPROFILE: "C:\\Users\\u" };
  const path = effectivePathEnv(
    "win32",
    "C:\\proc",
    () => ({ machine: "C:\\Windows;C:\\proc", user: "%USERPROFILE%\\.cargo\\bin" }),
    env,
  );
  assert.equal(path, "C:\\proc;C:\\Windows;C:\\Users\\u\\.cargo\\bin");
});
test("effectivePathEnv on Windows adds the dist dir that a stale process PATH missed", () => {
  // The reported case: loongagent.exe sits in a directory on the machine PATH
  // that the extension host's inherited snapshot does not have.
  const path = effectivePathEnv(
    "win32",
    "C:\\Windows\\system32",
    () => ({
      machine: "C:\\Windows\\system32;C:\\repo\\dist\\windows",
      user: "C:\\Users\\u\\.cargo\\bin",
    }),
    {},
  );
  assert.equal(
    path,
    "C:\\Windows\\system32;C:\\repo\\dist\\windows;C:\\Users\\u\\.cargo\\bin",
  );
});
