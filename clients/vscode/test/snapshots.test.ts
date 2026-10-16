import { test } from "node:test";
import assert from "node:assert/strict";
import { chmodSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { FileSnapshots } from "../src/snapshots.ts";

function tempDir(label: string): string {
  return mkdtempSync(join(tmpdir(), `loong-${label}-`));
}

test("the first sighting is the one kept", () => {
  const dir = tempDir("first");
  try {
    const file = join(dir, "a.txt");
    writeFileSync(file, "one");
    const snapshots = new FileSnapshots({ workspace: dir });
    snapshots.see([file]);
    writeFileSync(file, "two");
    snapshots.see([file]);
    const left = snapshots.left(file);
    assert.equal(left?.content, "one");
    assert.equal(left?.source, "snapshot");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("a path outside the workspace is never read", () => {
  const workspace = tempDir("in");
  const outside = tempDir("out");
  try {
    const secret = join(outside, "secret.txt");
    writeFileSync(secret, "secret");
    const snapshots = new FileSnapshots({ workspace });
    snapshots.see([secret]);
    const left = snapshots.left(secret);
    assert.equal(left?.content, undefined, "the bytes outside the workspace are not taken");
    assert.equal(left?.source, "none");
    assert.equal(left?.note, "outside the workspace");

    // A path outside that does not exist is still "outside", not "new file":
    // containment is decided before anything is read.
    const missing = join(outside, "missing.txt");
    snapshots.see([missing]);
    assert.equal(snapshots.left(missing)?.note, "outside the workspace");
  } finally {
    rmSync(workspace, { recursive: true, force: true });
    rmSync(outside, { recursive: true, force: true });
  }
});

test("a file that does not exist yet is a new file", () => {
  const dir = tempDir("new");
  try {
    const snapshots = new FileSnapshots({ workspace: dir });
    const file = join(dir, "new.txt");
    snapshots.see([file]);
    const left = snapshots.left(file);
    assert.equal(left?.content, undefined);
    assert.equal(left?.note, "new file");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("an unreadable file records why instead of throwing", (t) => {
  if (typeof process.getuid === "function" && process.getuid() === 0) {
    t.skip("running as root, where a file mode grants nothing and denies nothing");
    return;
  }
  const dir = tempDir("locked");
  try {
    const file = join(dir, "locked.txt");
    writeFileSync(file, "closed");
    chmodSync(file, 0o000);
    const snapshots = new FileSnapshots({ workspace: dir });
    snapshots.see([file]);
    const left = snapshots.left(file);
    assert.equal(left?.content, undefined);
    assert.match(left?.note ?? "", /^unreadable/);
    assert.match(left?.note ?? "", /EACCES/, "the reason is named, not guessed");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("clear forgets everything", () => {
  const dir = tempDir("clear");
  try {
    const file = join(dir, "a.txt");
    writeFileSync(file, "one");
    const snapshots = new FileSnapshots({ workspace: dir });
    snapshots.see([file]);
    assert.notEqual(snapshots.left(file), undefined);
    snapshots.clear();
    assert.equal(snapshots.left(file), undefined);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
