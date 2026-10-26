import type { DiffUris, UriLike } from "./diffProvider.ts";

const MARKER = "[loongagent] ";

/**
 * The right-hand side of a review-before-write diff.
 *
 * The text is stored under `${requestId}\u0000${path}`; the URI only carries the
 * key. A request nobody holds any more gets a marker, never another request's
 * preview.
 */
export class PreviewProvider<U extends UriLike> {
  private readonly contents = new Map<string, string>();
  private readonly uris: DiffUris<U>;

  constructor(uris: DiffUris<U>) {
    this.uris = uris;
  }

  uriFor(requestId: string, path: string, after: string): U {
    this.contents.set(`${requestId}\u0000${path}`, after);
    return this.uris.make(requestId, path, "preview");
  }

  provideTextDocumentContent(uri: U): string {
    const key = `${this.uris.tokenOf(uri)}\u0000${this.uris.pathOf(uri)}`;
    return this.contents.get(key) ?? `${MARKER}preview unavailable\n`;
  }
}
