import { readFileSync } from "node:fs";
import { resolve, sep } from "node:path";

/** What a path held, and where that answer came from. */
export interface Snapshot {
  path: string;
  content: string | undefined;
  /** where the left side came from; "none" means nobody can show a left side */
  source: "snapshot" | "none";
  /** why there is no content, when there is none: "new file" | "unreadable (CODE)" | "outside the workspace" */
  note?: string;
}

/**
 * The left-hand side of a diff: a file's content at the moment a turn first
 * mentioned it.
 *
 * The timing is what makes this exact. The kernel records `tool.call` *before*
 * it runs the tool (and before it is approved), so the first mention of a path
 * always precedes its first write — and unlike `git show HEAD:<path>` this stays
 * correct when the tree was already dirty before the turn. The *harvest* of
 * paths is not exact (`pathsInArgs` reads opaque JSON), so the diff list can be
 * incomplete; it is never wrong about a file it does have.
 *
 * Nothing outside the workspace is ever read: a tool argument is data the model
 * chose, and it does not get to make this class open a file for it.
 */
export class FileSnapshots {
  private readonly workspace: string;
  private readonly kept = new Map<string, Snapshot>();

  constructor(options: { workspace: string }) {
    this.workspace = resolve(options.workspace);
  }

  /** Read and keep each path's current content. The first sighting wins. */
  see(paths: string[]): void {
    for (const path of paths) {
      if (this.kept.has(path)) continue;
      this.kept.set(path, this.read(path));
    }
  }

  left(path: string): Snapshot | undefined {
    return this.kept.get(path);
  }

  clear(): void {
    this.kept.clear();
  }

  private read(path: string): Snapshot {
    const resolved = resolve(path);
    if (resolved !== this.workspace && !resolved.startsWith(this.workspace + sep)) {
      return { path, content: undefined, source: "none", note: "outside the workspace" };
    }
    try {
      return { path, content: readFileSync(resolved, "utf8"), source: "snapshot" };
    } catch (error) {
      const code = (error as NodeJS.ErrnoException).code ?? "unknown";
      return {
        path,
        content: undefined,
        source: "none",
        note: code === "ENOENT" ? "new file" : `unreadable (${code})`,
      };
    }
  }
}
