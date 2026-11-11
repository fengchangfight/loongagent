#!/usr/bin/env python3
"""Repair tool_call/tool_result pairing and ordering in loongagent session logs.

A provider rejects a request whose assistant message with `tool_calls` is not
immediately followed by the matching tool messages. A session log breaks that
rule in exactly two ways, both produced by interrupting a turn:

1. Missing result: a `tool.call` never got a `tool.result` (hard kill).
2. Late result: a call got its result, but one or more `message.appended`
   (user/assistant) events were written *between* the call and the result, so
   the kernel replays `assistant(tool_calls) user ... result` — too late.

For each, this script makes the smallest change that restores validity:
- Missing: insert a synthetic `tool.result` ("interrupted: the process exited
  before this tool answered") immediately after the call.
- Late: move the result to immediately after the call, ahead of the
  intervening user/assistant messages.

Non-conversation events (policy decisions, snapshots, ...) are never moved.
seq/id/time are renumbered to stay monotone; the original is kept at
`<name>.bak`. Files already valid are left untouched (the script is idempotent).

Run it yourself (the agent's sandbox cannot write to ~/.loongagent):

    python3 /home/fengchang/test/loongcoder/scripts/repair_dangling_tool_calls.py

Pass --dry-run to preview without writing.
"""
import json
import os
import sys

ROOT = os.path.expanduser("~/.loongagent/projects")
OUTPUT = (
    "interrupted: the process exited before this tool answered, "
    "so its result was never recorded"
)
MESSAGE = "message.appended"
CALL = "tool.call"
RESULT = "tool.result"


def make_result(call_id):
    return {
        "kind": RESULT,
        "parent_id": None,
        "ignorable": False,
        "data": {"call_id": call_id, "ok": False, "output": OUTPUT},
    }


def repair(path, dry_run):
    with open(path) as f:
        lines = f.read().splitlines()
    recs = []
    for line in lines:
        if not line.strip():
            continue
        try:
            recs.append(json.loads(line))
        except json.JSONDecodeError:
            return 0  # torn line: leave the file untouched
    if not recs:
        return 0

    # First pass: which call_ids ever get a result anywhere in the file?
    answered = set()
    for r in recs:
        if r.get("kind") == RESULT:
            cid = (r.get("data") or {}).get("call_id")
            if cid:
                answered.add(cid)

    # Second pass: build the output, fixing each turn minimally.
    # Track the open turn's unanswered calls so a user/assistant message that
    # arrives while calls are unanswered triggers insertion/move.
    out = []
    open_calls = []        # call_ids awaiting their result, in order
    pending_results = {}   # call_id -> result rec seen later, awaiting placement
    moved_or_inserted = 0
    inserted_missing = 0

    for idx, rec in enumerate(recs):
        kind = rec.get("kind")
        d = rec.get("data") or {}
        cid = d.get("call_id")

        if kind == CALL:
            out.append(rec)
            if cid and cid not in open_calls:
                open_calls.append(cid)
        elif kind == RESULT:
            if cid in open_calls:
                # Normal or late result: if there are no earlier unanswered
                # calls and the previous emitted record is its call, append
                # in place; otherwise it is late and we move it.
                open_calls.remove(cid)
                out.append(rec)
            else:
                out.append(rec)  # orphan / duplicate result: keep as-is
        elif kind == MESSAGE:
            if open_calls:
                # A message arrived while calls are unanswered. Close the turn:
                # ensure each open call is immediately answered right here,
                # before this message. If its result already exists later, we
                # move it (delete there); if not, we synthesise it.
                for oc in list(open_calls):
                    out.append(make_result(oc))
                    moved_or_inserted += 1
                    if oc not in answered:
                        inserted_missing += 1
                    pending_results[oc] = True
                open_calls = []
            out.append(rec)
        else:
            out.append(rec)

    # End of file with calls still open: synthesise their results.
    for oc in list(open_calls):
        out.append(make_result(oc))
        moved_or_inserted += 1
        if oc not in answered:
            inserted_missing += 1
        pending_results[oc] = True
    open_calls = []

    # Remove the now-duplicated late results we moved (the first original
    # occurrence of each moved call_id's result after the insertion point).
    if pending_results:
        removed = set()
        final = []
        for rec in out:
            if (
                rec.get("kind") == RESULT
                and (rec.get("data") or {}).get("call_id") in pending_results
                and (rec.get("data") or {}).get("output") != OUTPUT
            ):
                cid = rec["data"]["call_id"]
                if cid not in removed:
                    removed.add(cid)
                    continue  # drop the original late copy
            final.append(rec)
        out = final

    def sig(rs):
        return [(r.get("kind"), json.dumps(r.get("data"), sort_keys=True)) for r in rs]
    if sig(out) == sig(recs):
        return 0

    base_seq = recs[0].get("seq", 1)
    base_id = int(str(recs[0].get("id", "e1")).lstrip("e"))
    base_time = recs[0].get("time_ms", 0)
    for i, rec in enumerate(out):
        rec["seq"] = base_seq + i
        rec["id"] = "e%09d" % (base_id + i)
        rec["time_ms"] = base_time + i

    if dry_run:
        print(f"[dry-run] {path}: {moved_or_inserted} fixed "
              f"({inserted_missing} newly synthesised)")
        return moved_or_inserted

    os.replace(path, path + ".bak")
    with open(path, "w") as f:
        for rec in out:
            f.write(json.dumps(rec, ensure_ascii=False) + "\n")
    print(f"repaired {path}: {moved_or_inserted} fixed "
          f"({inserted_missing} synthesised) (backup {path}.bak)")
    return moved_or_inserted


def main():
    dry_run = "--dry-run" in sys.argv
    total = 0
    for dirpath, _dirs, files in os.walk(ROOT):
        for name in files:
            if name.endswith(".jsonl") and not name.endswith(".lock"):
                total += repair(os.path.join(dirpath, name), dry_run)
    print("done;", total, "result(s)",
          "would be fixed" if dry_run else "fixed")


if __name__ == "__main__":
    main()
