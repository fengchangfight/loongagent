#!/usr/bin/env python3
"""Repair a session log that two writers corrupted, by dropping the losing duplicate line.

Why this exists: two processes each kept their own `next_seq` on the same `.jsonl`, so the
second one wrote an event with a `seq` the file already had. `SessionLog::replay` is strict
and refuses the whole session, which makes it unopenable ("expected seq N, found N-1").
The code side is fixed — `SessionLog` now takes a write lock on `<log>.lock` — but a log
written before that fix stays broken until this runs.

The repair drops exactly one line and shifts nothing, so every surviving event keeps the
`seq` and `id` the derived databases (`<id>.sqlite`, `<id>.context.db`) already know. Which
of the duplicated lines to drop is decided by the index when it is there: the winner is the
event the index holds, the loser is the one whose insert failed.

Usage:
    python3 scripts/repair-dup-seq.py <log.jsonl>            # dry run: say what it would drop
    python3 scripts/repair-dup-seq.py <log.jsonl> --apply    # back up, then rewrite in place
"""

import json
import os
import sqlite3
import sys


def load(path):
    with open(path, "rb") as f:
        return f.readlines()


def seq_of(line):
    return json.loads(line)["seq"]


def check(rows):
    """Return the list of (line_number, seq, expected) places where seq does not advance by one.

    Adjacency, not the line number, is the invariant: `replay` walks the file and wants each
    event's `seq` to be the previous one plus one. A duplicated seq reports once, here, and the
    lines after it stay quiet even though each one now sits on a different line number.
    """
    bad = []
    prev = None
    for n, row in enumerate(rows, 1):
        wanted = 1 if prev is None else prev + 1
        if seq_of(row) != wanted:
            bad.append((n, seq_of(row), wanted))
        prev = seq_of(row)
    return bad


def read_one(db_path, sql, params=()):
    """Run one query and return its first row, or None if the database is not there.

    Read-only when SQLite allows it. A WAL database needs a writable `-shm` beside it, which a
    read-only handle cannot get, so an ordinary connection is the fallback.
    """
    if not os.path.exists(db_path):
        return None
    try:
        db = sqlite3.connect("file:%s?mode=ro" % db_path, uri=True)
        try:
            return db.execute(sql, params).fetchone()
        finally:
            db.close()
    except sqlite3.Error:
        pass
    db = sqlite3.connect(db_path)
    try:
        return db.execute(sql, params).fetchone()
    finally:
        db.close()


def index_row(sqlite_path, seq):
    """The (kind, time_ms) the index holds for this seq, or None.

    Both copies of a duplicate carry the same `id` — it is spelled from the `seq` — so the
    index can only be matched on `kind` and `time_ms`.
    """
    return read_one(sqlite_path, "select kind, time_ms from events where seq = ?", (seq,))


def context_cursor(context_path):
    """The byte offset the context index has already consumed, or None if it keeps none."""
    if not os.path.exists(context_path):
        return None
    try:
        row = read_one(context_path, "select value from meta where key = 'cursor_offset'")
    except sqlite3.Error:
        return None
    return int(row[0]) if row else None


def main(argv):
    if len(argv) < 2:
        print(__doc__)
        return 2
    log = argv[1]
    apply = "--apply" in argv[2:]

    rows = load(log)
    bad = check(rows)
    print("log:      %s" % log)
    print("lines:    %d" % len(rows))
    if not bad:
        print("ok: every line's seq is the previous one plus one; nothing to repair")
        return 0
    if len(bad) > 1:
        print("refusing: %d anomalies, this script only knows one" % len(bad))
        for n, seq, want in bad:
            print("  line %d: seq %d, expected %d" % (n, seq, want))
        return 1
    n, seq, want = bad[0]
    print("anomaly:  line %d has seq %d, expected %d" % (n, seq, want))

    # The line before the anomaly either repeats the seq (it is the other copy) or the file
    # has a hole this script cannot close.
    if seq != seq_of(rows[n - 2]):
        print("refusing: line %d has seq %d but line %d has seq %d — that is a gap, not a duplicate"
              % (n, seq, n - 1, seq_of(rows[n - 2])))
        return 1

    first, second = rows[n - 2], rows[n - 1]
    base = log[: -len(".jsonl")] if log.endswith(".jsonl") else log
    held = index_row(base + ".sqlite", seq)
    if held is not None:
        a, b = json.loads(first), json.loads(second)
        if (a["kind"], a["time_ms"]) == tuple(held):
            drop, keep = second, first
        elif (b["kind"], b["time_ms"]) == tuple(held):
            drop, keep = first, second
        else:
            print("refusing: the index holds %s at seq %d, matching neither copy" % (held, seq))
            return 1
        print("index:    seq %d holds %s, so that copy stays" % (seq, held))
    else:
        drop, keep = second, first
        print("index:    no row for seq %d (or no index), keeping the first copy" % seq)

    for label, row in (("drop", drop), ("keep", keep)):
        event = json.loads(row)
        text = event.get("data", {}).get("text", "")
        print("%-9s seq %d id %s kind %s time_ms %d %s"
              % (label + ":", event["seq"], event["id"], event["kind"], event["time_ms"],
                 text[:60].replace("\n", " ")))

    # The context index resumes from a byte offset. Dropping a line that starts before that
    # offset would hand it the wrong bytes, so say so instead of guessing.
    cursor = context_cursor(base + ".context.db")
    if cursor is not None:
        offset = sum(len(r) for r in rows[: (n - 2) if drop is first else (n - 1)])
        if offset < cursor:
            print("warning:  the dropped line starts at byte %d, before the context index's"
                  " cursor at %d — delete %s.context.db so it rebuilds"
                  % (offset, cursor, base))
        else:
            print("context:  cursor at %d is at or before the dropped line, nothing to redo" % cursor)

    if not apply:
        print("\ndry run; re-run with --apply to rewrite the file")
        return 0

    backup = log + ".dupseq.bak"
    if os.path.exists(backup):
        print("refusing: %s already exists" % backup)
        return 1
    with open(backup, "wb") as f:
        f.writelines(rows)
        f.flush()
        os.fsync(f.fileno())
    kept = [r for r in rows if r is not drop]
    tmp = log + ".repair.tmp"
    with open(tmp, "wb") as f:
        f.writelines(kept)
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, log)
    after = check(load(log))
    if after:
        print("still broken after the rewrite: %s" % after)
        print("the backup is at %s" % backup)
        return 1
    print("\nrepaired: %d lines, seq 1..%d contiguous; backup at %s"
          % (len(kept), len(kept), backup))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
