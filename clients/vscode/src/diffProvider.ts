import type { FileSnapshots } from "./snapshots.ts";

/** The little of a URI this needs: an identity that survives being handed back. */
export interface UriLike {
  /** The decoded path of the resource, as `vscode.Uri.path` is. */
  path: string;
}

/** How to make and read the surface's URI type, so this file needs no `vscode`. */
export interface DiffUris<U extends UriLike> {
  make(path: string, label: string): U;
  pathOf(uri: U): string;
}

/** Marks text that is about the diff rather than part of the file. */
const MARKER = "[loongagent] ";

/**
 * The left-hand side of a `vscode.diff`: what a path held when the turn first
 * mentioned it.
 *
 * The path travels inside the URI, so nothing has to be remembered between
 * "open this diff" and "what do I put in the left pane" — a diff tab outlives
 * the turn that opened it.
 */
export class DiffProvider<U extends UriLike> {
  private readonly snapshots: FileSnapshots;
  private readonly gitShow: (path: string) => string | undefined;
  private readonly uris: DiffUris<U>;

  constructor(
    snapshots: FileSnapshots,
    gitShow: (path: string) => string | undefined,
    uris: DiffUris<U>,
  ) {
    this.snapshots = snapshots;
    this.gitShow = gitShow;
    this.uris = uris;
  }

  /** A URI carrying the absolute path whose left side this document serves. */
  uriFor(path: string, label: string): U {
    return this.uris.make(path, label);
  }

  provideTextDocumentContent(uri: U): string {
    const path = this.uris.pathOf(uri);
    const snapshot = this.snapshots.left(path);
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
  changedAmong(paths: string[], read: (path: string) => string | undefined): string[] {
    return paths.filter((path) => {
      const left = this.snapshots.left(path);
      if (left === undefined || left.source !== "snapshot") return true;
      return read(path) !== left.content;
    });
  }
}
