import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { DiffProvider } from "../src/diffProvider.ts";
import { FileSnapshots } from "../src/snapshots.ts";
import type { DiffUris, UriLike } from "../src/diffProvider.ts";

interface StubUri extends UriLike {
  path: string;
  label: string;
}

/** Stands in for `vscode.Uri`: the path is what carries the identity. */
const stubUris: DiffUris<StubUri> = {
  make: (path, label) => ({ path, label }),
  pathOf: (uri) => uri.path,
};

function tempDir(label: string): string {
  return mkdtempSync(join(tmpdir(), `loong-${label}-`));
}

test("a snapshotted path serves its snapshot, not git", () => {
  const dir = tempDir("snap");
  try {
    const file = join(dir, "a.txt");
    writeFileSync(file, "as it was");
    const snapshots = new FileSnapshots({ workspace: dir });
    snapshots.see([file]);
    let askedGit = false;
    const provider = new DiffProvider(snapshots, () => { askedGit = true; return "from git"; }, stubUris);

    const text = provider.provideTextDocumentContent(provider.uriFor(file, "before"));
    assert.equal(text, "as it was");
    assert.equal(askedGit, false, "git is not consulted when the snapshot has the answer");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("a path with no snapshot falls back to git and says so", () => {
  const dir = tempDir("git");
  try {
    const file = join(dir, "a.txt");
    const snapshots = new FileSnapshots({ workspace: dir });
    const provider = new DiffProvider(snapshots, (path) => (path === file ? "from HEAD" : undefined), stubUris);

    const text = provider.provideTextDocumentContent(provider.uriFor(file, "before"));
    assert.match(text, /git/);
    assert.match(text, /from HEAD/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("a path with neither yields an empty document plus a marker", () => {
  const dir = tempDir("none");
  try {
    const file = join(dir, "a.txt");
    const snapshots = new FileSnapshots({ workspace: dir });
    const provider = new DiffProvider(snapshots, () => undefined, stubUris);
    const text = provider.provideTextDocumentContent(provider.uriFor(file, "before"));
    assert.match(text, /no previous version/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("a new file's left side is empty and named as new", () => {
  const dir = tempDir("new");
  try {
    const file = join(dir, "new.txt");
    const snapshots = new FileSnapshots({ workspace: dir });
    snapshots.see([file]);
    const provider = new DiffProvider(snapshots, () => "should not be used", stubUris);
    const text = provider.provideTextDocumentContent(provider.uriFor(file, "before"));
    assert.match(text, /new file/);
    assert.doesNotMatch(text, /should not be used/, "the snapshot's 'new file' answer is final");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("the path survives the round trip through the URI", () => {
  const dir = tempDir("spaces");
  try {
    const file = join(dir, "a b.txt");
    writeFileSync(file, "spaced");
    const snapshots = new FileSnapshots({ workspace: dir });
    snapshots.see([file]);
    const provider = new DiffProvider(snapshots, () => undefined, stubUris);
    const text = provider.provideTextDocumentContent(provider.uriFor(file, "before"));
    assert.equal(text, "spaced");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("changedAmong lists only what actually differs", () => {
  const dir = tempDir("changed");
  try {
    const file = join(dir, "a.txt");
    writeFileSync(file, "one");
    const snapshots = new FileSnapshots({ workspace: dir });
    snapshots.see([file]);
    const provider = new DiffProvider(snapshots, () => undefined, stubUris);

    assert.deepEqual(provider.changedAmong([file], () => "one"), [], "unchanged is not listed");
    assert.deepEqual(provider.changedAmong([file], () => "two"), [file], "changed is listed");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("a path with no left side is listed rather than assumed unchanged", () => {
  const dir = tempDir("noleft");
  try {
    const outside = "/somewhere/else.txt";
    const snapshots = new FileSnapshots({ workspace: dir });
    const provider = new DiffProvider(snapshots, () => undefined, stubUris);
    assert.deepEqual(provider.changedAmong([outside], () => undefined), [outside]);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
