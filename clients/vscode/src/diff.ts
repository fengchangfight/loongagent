export interface DiffResult {
  unified: string;
  added: number;
  removed: number;
  truncated: boolean;
}

export type Preview =
  | { ok: true; after: string; diff: DiffResult }
  | { ok: false; reason: string };

const DEFAULT_MAX_LINES = 200;

type Op = { kind: "same" | "add" | "del"; text: string };

function lines(text: string): string[] {
  return text === "" ? [] : text.replace(/\r\n?/g, "\n").split("\n");
}

/** Common prefix/suffix, all middle replaced: a coarse but linear diff. */
function coarse(before: string[], after: string[]): Op[] {
  let prefix = 0;
  while (prefix < before.length && prefix < after.length && before[prefix] === after[prefix]) {
    prefix++;
  }
  let suffix = 0;
  while (suffix < before.length - prefix && suffix < after.length - prefix
    && before[before.length - 1 - suffix] === after[after.length - 1 - suffix]) {
    suffix++;
  }
  const ops: Op[] = [];
  for (let index = 0; index < prefix; index++) ops.push({ kind: "same", text: before[index] });
  for (let index = prefix; index < before.length - suffix; index++) {
    ops.push({ kind: "del", text: before[index] });
  }
  for (let index = prefix; index < after.length - suffix; index++) {
    ops.push({ kind: "add", text: after[index] });
  }
  for (let index = before.length - suffix; index < before.length; index++) {
    ops.push({ kind: "same", text: before[index] });
  }
  return ops;
}

/** A line-level LCS. The matrix is bounded; larger inputs use the coarse form. */
function operations(before: string[], after: string[]): Op[] {
  if (before.length * after.length > 1_000_000) return coarse(before, after);
  const width = after.length + 1;
  const table = Array.from({ length: before.length + 1 }, () => new Uint32Array(width));
  for (let i = before.length - 1; i >= 0; i--) {
    for (let j = after.length - 1; j >= 0; j--) {
      table[i][j] = before[i] === after[j]
        ? table[i + 1][j + 1] + 1
        : Math.max(table[i + 1][j], table[i][j + 1]);
    }
  }
  const ops: Op[] = [];
  let i = 0;
  let j = 0;
  while (i < before.length && j < after.length) {
    if (before[i] === after[j]) {
      ops.push({ kind: "same", text: before[i] });
      i++;
      j++;
    } else if (table[i + 1][j] >= table[i][j + 1]) {
      ops.push({ kind: "del", text: before[i] });
      i++;
    } else {
      ops.push({ kind: "add", text: after[j] });
      j++;
    }
  }
  while (i < before.length) ops.push({ kind: "del", text: before[i++] });
  while (j < after.length) ops.push({ kind: "add", text: after[j++] });
  return ops;
}

type Entry = { op: Op; oldNo: number; newNo: number };

function numbered(ops: Op[]): Entry[] {
  let oldNo = 1;
  let newNo = 1;
  return ops.map((op) => {
    const entry = { op, oldNo, newNo };
    if (op.kind !== "add") oldNo++;
    if (op.kind !== "del") newNo++;
    return entry;
  });
}

/** A unified diff with three context lines and the counts a card shows. */
export function unifiedDiff(before: string, after: string, maxLines = DEFAULT_MAX_LINES): DiffResult {
  const ops = operations(lines(before), lines(after));
  const added = ops.filter((op) => op.kind === "add").length;
  const removed = ops.filter((op) => op.kind === "del").length;
  if (added === 0 && removed === 0) {
    return { unified: "", added, removed, truncated: false };
  }

  const entries = numbered(ops);
  const show = new Array<boolean>(entries.length).fill(false);
  entries.forEach((entry, index) => {
    if (entry.op.kind === "same") return;
    for (let cursor = Math.max(0, index - 3); cursor <= Math.min(entries.length - 1, index + 3); cursor++) {
      show[cursor] = true;
    }
  });

  const hunks: string[] = [];
  let index = 0;
  while (index < entries.length) {
    if (!show[index]) {
      index++;
      continue;
    }
    const start = index;
    while (index < entries.length && show[index]) index++;
    const slice = entries.slice(start, index);
    const oldCount = slice.filter((entry) => entry.op.kind !== "add").length;
    const newCount = slice.filter((entry) => entry.op.kind !== "del").length;
    const oldStart = oldCount === 0 ? 0 : slice[0].oldNo;
    const newStart = newCount === 0 ? 0 : slice[0].newNo;
    const body = slice.map((entry) => {
      const prefix = entry.op.kind === "same" ? " " : entry.op.kind === "add" ? "+" : "-";
      return `${prefix}${entry.op.text}`;
    });
    hunks.push(`@@ -${oldStart},${oldCount} +${newStart},${newCount} @@`);
    hunks.push(...body);
  }

  let unified = hunks.join("\n");
  let truncated = false;
  if (maxLines > 0) {
    const all = unified.split("\n");
    if (all.length > maxLines) {
      unified = `${all.slice(0, maxLines).join("\n")}\n… truncated ${all.length - maxLines} lines`;
      truncated = true;
    }
  }
  return { unified, added, removed, truncated };
}

/** Preview a whole-file write; `undefined` before means a new file. */
export function previewWrite(before: string | undefined, content: string, maxLines?: number): Preview {
  return { ok: true, after: content, diff: unifiedDiff(before ?? "", content, maxLines) };
}

/** Preview the edit tool's exact-string replacement, or say why it cannot. */
export function previewEdit(
  before: string | undefined,
  oldString: string,
  newString: string,
  replaceAll = false,
  maxLines?: number,
): Preview {
  if (before === undefined) return { ok: false, reason: "the file does not exist" };
  if (oldString === "") return { ok: false, reason: "old_string is empty" };
  if (oldString === newString) return { ok: false, reason: "nothing to do" };
  const occurrences = before.split(oldString).length - 1;
  if (occurrences === 0) return { ok: false, reason: "old_string was not found" };
  if (occurrences > 1 && !replaceAll) return { ok: false, reason: "old_string is not unique" };
  const after = replaceAll
    ? before.split(oldString).join(newString)
    : before.replace(oldString, newString);
  return { ok: true, after, diff: unifiedDiff(before, after, maxLines) };
}
