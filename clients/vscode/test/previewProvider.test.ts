import { test } from "node:test";
import assert from "node:assert/strict";
import { PreviewProvider } from "../src/previewProvider.ts";
import type { DiffUris } from "../src/diffProvider.ts";

interface StubUri { path: string; query: string; }

const uris: DiffUris<StubUri> = {
  make: (token, path, label) => ({ path, query: `${token}|${label}` }),
  pathOf: (uri) => uri.path,
  tokenOf: (uri) => uri.query.split("|")[0] ?? "",
};

test("a preview URI serves the stored after text", () => {
  const provider = new PreviewProvider(uris);
  const uri = provider.uriFor("r1", "/w/a b.txt", "new content");
  assert.equal(provider.provideTextDocumentContent(uri), "new content");
  assert.equal(uris.pathOf(uri), "/w/a b.txt");
  assert.equal(uris.tokenOf(uri), "r1");
});
test("an unknown preview says so instead of showing another turn's text", () => {
  const provider = new PreviewProvider(uris);
  assert.match(provider.provideTextDocumentContent({ path: "/w/a", query: "missing|preview" }),
    /preview unavailable/);
});
