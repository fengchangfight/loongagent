#!/usr/bin/env python3
"""Normalise a `cargo test -- --list` capture so two runs can be compared.

The compiled artifact name carries a content hash
(`target/debug/deps/loong_tui_app-4c9703ac1462149a`), which changes whenever the
code changes, so comparing raw captures fails for the wrong reason.  This reads
the merged stdout and stderr of

    cargo test --workspace --locked -- --list 2>&1

on stdin and writes the inventory on stdout:

* `Running ...` headers, with `target/debug/deps/<name>-<hash>` reduced to
  `<name>`.  The normalised name stays on its own line, which is what stops two
  same-named tests in different targets from cancelling out.
* `Doc-tests <name>` headers, unchanged.
* `<name>: test` lines, unchanged.

Cargo's own progress chatter (`Compiling`, `Finished`, blank lines) is dropped.
Duplicate lines are preserved -- this is a multiset, not a set -- and the result
is sorted, so `diff before after` is the whole comparison.

`--count` additionally writes the number of test lines to stderr, for the
plan's sanity check that the suite still lists the same 747 tests.
"""

from __future__ import annotations

import argparse
import re
import sys

# `target/debug/deps/loong_tui_app-4c9703ac1462149a` -> `loong_tui_app`
HASHED_TARGET = re.compile(r"target/debug/deps/([A-Za-z0-9_]+)-[0-9a-f]{8,}")


def normalise(capture: str) -> list[str]:
    """Return the sorted inventory lines of a merged `cargo test --list`."""
    lines: list[str] = []
    for line in capture.splitlines():
        stripped = line.strip()
        if stripped.startswith("Running "):
            lines.append(HASHED_TARGET.sub(r"\1", stripped))
        elif stripped.startswith("Doc-tests "):
            lines.append(stripped)
        elif stripped.endswith(": test"):
            lines.append(stripped)
    lines.sort()
    return lines


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="normalise_test_list.py",
        description="Normalise a merged `cargo test -- --list` capture on stdin.",
    )
    parser.add_argument(
        "--count",
        action="store_true",
        help="also write the number of test lines to stderr",
    )
    args = parser.parse_args(argv)

    lines = normalise(sys.stdin.read())
    for line in lines:
        print(line)
    if args.count:
        tests = sum(1 for line in lines if line.endswith(": test"))
        sys.stderr.write(f"{tests} test lines\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
