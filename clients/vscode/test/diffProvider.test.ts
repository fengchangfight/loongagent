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
  token: string;
}

/** Stands in for `vscode.Uri`: the path AND the turn travel inside it. */
const stubUris: DiffUris<StubUri> = {
  make: (token, path, label) => ({ token, path, label }),
  pathOf: (uri) => uri.path,
  tokenOf: (uri) => uri.token,
};

/** A store that only knows the tokens it is given, like the window's turn map. */
function stores(known: Record<string, FileSnapshots>) {
  return (token: string): FileSnapshots | undefined => known[token];
}

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
    const provider = new DiffProvider(stores({ t1: snapshots }), () => { askedGit = true; return "from git"; }, stubUris);

    const text = provider.provideTextDocumentContent(provider.uriFor("t1", file, "before"));
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
    const provider = new DiffProvider(stores({ t1: snapshots }), (path) => (path === file ? "from HEAD" : undefined), stubUris);

    const text = provider.provideTextDocumentContent(provider.uriFor("t1", file, "before"));
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
    const provider = new DiffProvider(stores({ t1: snapshots }), () => undefined, stubUris);
    const text = provider.provideTextDocumentContent(provider.uriFor("t1", file, "before"));
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
    const provider = new DiffProvider(stores({ t1: snapshots }), () => "should not be used", stubUris);
    const text = provider.provideTextDocumentContent(provider.uriFor("t1", file, "before"));
    assert.match(text, /new file/);
    assert.doesNotMatch(text, /should not be used/, "the snapshot's 'new file' answer is final");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("a denied new-file write is not listed as changed", () => {
  const dir = tempDir("denied-new");
  try {
    const file = join(dir, "new.txt");
    const snapshots = new FileSnapshots({ workspace: dir });
    snapshots.see([file]);
    const provider = new DiffProvider(stores({ t1: snapshots }), () => undefined, stubUris);
    assert.deepEqual(provider.changedAmong("t1", [file], () => undefined), []);
    assert.deepEqual(provider.changedAmong("t1", [file], () => "made"), [file]);
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
    const provider = new DiffProvider(stores({ t1: snapshots }), () => undefined, stubUris);
    const text = provider.provideTextDocumentContent(provider.uriFor("t1", file, "before"));
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
    const provider = new DiffProvider(stores({ t1: snapshots }), () => undefined, stubUris);

    assert.deepEqual(provider.changedAmong("t1", [file], () => "one"), [], "unchanged is not listed");
    assert.deepEqual(provider.changedAmong("t1", [file], () => "two"), [file], "changed is listed");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("a path with no left side is listed only when it now exists", () => {
  const dir = tempDir("noleft");
  try {
    const outside = "/somewhere/else.txt";
    const snapshots = new FileSnapshots({ workspace: dir });
    const provider = new DiffProvider(stores({ t1: snapshots }), () => undefined, stubUris);
    assert.deepEqual(provider.changedAmong("t1", [outside], () => undefined), [],
      "a guessed missing path is not a change");
    assert.deepEqual(provider.changedAmong("t1", [outside], () => "now here"), [outside]);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("a forgotten turn falls back to git rather than serving another turn's snapshot", () => {
  const dir = tempDir("forgot");
  try {
    const file = join(dir, "a.txt");
    writeFileSync(file, "this turn's before");
    const live = new FileSnapshots({ workspace: dir });
    live.see([file]);
    let askedGit = false;
    const provider = new DiffProvider(stores({ live }), () => { askedGit = true; return "from git"; }, stubUris);

    const text = provider.provideTextDocumentContent(provider.uriFor("gone", file, "before"));
    assert.equal(askedGit, true, "an unknown turn is not answered from the live one");
    assert.match(text, /git/);
    assert.doesNotMatch(text, /this turn's before/, "and never from the wrong turn");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("two turns of the same file each keep their own before", () => {
  const dir = tempDir("turns");
  try {
    const file = join(dir, "a.txt");
    writeFileSync(file, "turn one");
    const first = new FileSnapshots({ workspace: dir });
    first.see([file]);
    writeFileSync(file, "turn two");
    const second = new FileSnapshots({ workspace: dir });
    second.see([file]);
    const provider = new DiffProvider(stores({ one: first, two: second }), () => undefined, stubUris);

    assert.equal(provider.provideTextDocumentContent(provider.uriFor("one", file, "before")), "turn one");
    assert.equal(provider.provideTextDocumentContent(provider.uriFor("two", file, "before")), "turn two");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
