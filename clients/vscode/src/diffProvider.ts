import type { FileSnapshots } from "./snapshots.ts";

/** The little of a URI this needs: an identity that survives being handed back. */
export interface UriLike {
  /** The decoded path of the resource, as `vscode.Uri.path` is. */
  path: string;
}

/** How to make and read the surface's URI type, so this file needs no `vscode`. */
export interface DiffUris<U extends UriLike> {
  /** The token identifies the turn whose "before" this URI serves. */
  make(token: string, path: string, label: string): U;
  pathOf(uri: U): string;
  tokenOf(uri: U): string;
}

/** Marks text that is about the diff rather than part of the file. */
const MARKER = "[loongagent] ";

/**
 * The left-hand side of a `vscode.diff`: what a path held when the turn first
 * mentioned it.
 *
 * The turn **and** the path travel inside the URI, so a diff tab opened from an
 * older reply keeps showing that turn's "before" rather than whatever the newest
 * turn recorded. A turn nobody holds any more falls back to git and says so —
 * it never silently serves a different turn's snapshot.
 */
export class DiffProvider<U extends UriLike> {
  private readonly turnOf: (token: string) => FileSnapshots | undefined;
  private readonly gitShow: (path: string) => string | undefined;
  private readonly uris: DiffUris<U>;

  constructor(
    turnOf: (token: string) => FileSnapshots | undefined,
    gitShow: (path: string) => string | undefined,
    uris: DiffUris<U>,
  ) {
    this.turnOf = turnOf;
    this.gitShow = gitShow;
    this.uris = uris;
  }

  /** A URI carrying the turn and the absolute path whose left side it serves. */
  uriFor(token: string, path: string, label: string): U {
    return this.uris.make(token, path, label);
  }

  provideTextDocumentContent(uri: U): string {
    const path = this.uris.pathOf(uri);
    const snapshot = this.turnOf(this.uris.tokenOf(uri))?.left(path);
    if (snapshot !== undefined && snapshot.source === "snapshot") {
      // No marker: this pane is the file, and a marker would be a lie about its
      // contents. The other answers below are about the diff, so they say so.
      return snapshot.content ?? "";
    }
    if (snapshot !== undefined && snapshot.note === "new file") {
      return `${MARKER}new file — nothing was here before\n`;
    }
    if (snapshot !== undefined && snapshot.content === undefined) {
      return `${MARKER}${snapshot.note ?? "no left side"} — showing nothing\n`;
    }
    const fromGit = this.gitShow(path);
    if (fromGit !== undefined) {
      return `${MARKER}from git HEAD — this turn did not record a snapshot\n${fromGit}`;
    }
    return `${MARKER}no previous version — the file did not exist and git has no copy\n`;
  }

  /**
   * Which of these paths actually differ from the left side that would be shown.
   *
   * A path with no left side is listed rather than assumed unchanged: not being
   * able to prove a file is untouched is not the same as knowing it is.
   */
  changedAmong(token: string, paths: string[], read: (path: string) => string | undefined): string[] {
    const turn = this.turnOf(token);
    return paths.filter((path) => {
      const left = turn?.left(path);
      if (left === undefined || left.source !== "snapshot") return true;
      return read(path) !== left.content;
    });
  }
}
