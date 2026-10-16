#!/usr/bin/env python3
"""Checks over the repository's markdown, because nothing else reads it.

The documents are the part of this repository no compiler sees: a link can rot,
a file can drift from the rules it describes, and every build stays green. This
script is the document gate, wired into scripts/check.sh beside the others.

Usage:
    scripts/check-docs.py [--root DIR] [--check NAME] [--test-counts FILE]

    --root DIR          repository root (default: this script's parent)
    --check NAME        run one check: links, counts or all (default: all)
    --test-counts FILE  parsed test results, for the counts check

Only --check links exists today; later tasks add checks behind the same
interface, which is why --check and --test-counts are already here. A check
that is asked for and cannot run is an error, never a silent skip.
"""
from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys

# The three URL forms a markdown link may use without naming a file here, and
# the fragment form, which points inside the current document.
SKIPPED_SCHEMES = ("http:", "https:", "mailto:")

# [text](target) and ![alt](target). Targets never contain an unbalanced
# bracket in this repository; the plan and the craft documents do not use
# reference-style links.
LINK = re.compile(r"!?\[[^\]]*\]\(([^)]*)\)")

# Checks the file knows how to run. A name outside this set is a usage error,
# so a typo in check.sh cannot turn a gate into a no-op.
CHECKS = ("links", "counts")


def fail(*lines: str) -> int:
    """Report one reason the gate could not run or did not pass."""
    print("check-docs: error:", lines[0], file=sys.stderr)
    for line in lines[1:]:
        print("  " + line, file=sys.stderr)
    return 1


def tracked_markdown(root: str) -> list[str]:
    """Every tracked .md file, relative to root, in git's order.

    Tracked, not walked: scratch piles and generated trees at the repository
    root are gitignored or untracked and must never enter the gate.
    """
    listing = subprocess.run(
        ["git", "-C", root, "ls-files", "*.md"],
        capture_output=True,
        text=True,
    )
    if listing.returncode != 0:
        raise RuntimeError(listing.stderr.strip() or "git ls-files failed")
    return listing.stdout.split()


def _blank(text: str) -> str:
    """Same length and newlines as text, every other character a space."""
    return "".join(c if c == "\n" else " " for c in text)


def code_free(text: str) -> str:
    """A copy of text with fenced blocks and inline code spans blanked out.

    A document is allowed to *show* a link without owning it — the plan for
    this very check contains `[text](target)` inside code spans — so code is
    removed before any target is looked for. Blanks keep every line number.
    """
    lines = text.split("\n")
    out: list[str] = []
    fence: str | None = None
    for line in lines:
        stripped = line.lstrip(" ")
        indent = len(line) - len(stripped)
        # The opening fence is at most three spaces in and is a run of one
        # character; the closing one only has to be at least as long.
        if fence is None and indent <= 3 and stripped[:3] in ("```", "~~~"):
            fence = stripped[0] * (len(stripped) - len(stripped.lstrip(stripped[0])))
            out.append(_blank(line))
            continue
        if fence is None:
            out.append(line)
        else:
            # A closing fence is the same character, at least as long.
            if stripped.startswith(fence) and stripped[len(fence) :].strip() == "":
                fence = None
            out.append(_blank(line))

    # Inline spans, run by run: `x`, ``x``, and so on. A run is closed by a run
    # of the same length, which is what CommonMark does.
    joined = "\n".join(out)
    chars = list(joined)
    i = 0
    while i < len(joined):
        if joined[i] != "`":
            i += 1
            continue
        run = 1
        while i + run < len(joined) and joined[i + run] == "`":
            run += 1
        closer = joined.find("`" * run, i + run)
        if closer == -1:
            i += run
            continue
        for j in range(i, closer + run):
            if chars[j] != "\n":
                chars[j] = " "
        i = closer + run
    return "".join(chars)


def find_links(text: str) -> list[tuple[int, str]]:
    """(1-based line, target) for every link outside code, in file order."""
    scan = code_free(text)
    found = []
    for match in LINK.finditer(scan):
        found.append((scan.count("\n", 0, match.start()) + 1, match.group(1).strip()))
    return found


def check_links(root: str) -> list[str]:
    """Every relative link in every tracked document must resolve."""
    broken = []
    for name in tracked_markdown(root):
        path = os.path.join(root, name)
        with open(path, encoding="utf-8") as handle:
            text = handle.read()
        for line, target in find_links(text):
            # Markdown may wrap a target in angle brackets; titles are not used
            # in this repository, and a bare space in a target is a typo.
            if target.startswith("<") and target.endswith(">"):
                target = target[1:-1].strip()
            if not target or target.startswith(("#",) + SKIPPED_SCHEMES):
                continue
            # A fragment points inside a document this link already names.
            destination = target.split("#", 1)[0]
            if not destination:
                continue
            resolved = os.path.normpath(os.path.join(os.path.dirname(path), destination))
            if not os.path.exists(resolved):
                broken.append(f"FAIL {name}:{line}: {target} does not exist")
    return broken


def check_counts(counts_file: str | None, *, requested: bool) -> list[str]:
    """Not written yet; the parameter is the interface a later task fills in.

    Asked for by name without --test-counts it must be an error, not a skip: a
    check that quietly does nothing is worse than no check. Asked for as part
    of "all" it simply cannot run here, and "all" means all that can.
    """
    if requested and counts_file is None:
        return [
            "FAIL --check counts needs --test-counts FILE; "
            "a check that cannot read the counts must not pass silently"
        ]
    return []


def main() -> int:
    repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    parser = argparse.ArgumentParser(
        prog="check-docs.py",
        description="Check the repository's markdown (run from scripts/check.sh).",
    )
    parser.add_argument("--root", default=repo, help="repository root (default: this repository)")
    parser.add_argument(
        "--check",
        default=None,
        metavar="NAME",
        help="one check to run: " + ", ".join(CHECKS) + ", or all (default: all)",
    )
    parser.add_argument(
        "--test-counts",
        default=None,
        metavar="FILE",
        help="cargo test output for the counts check",
    )
    args = parser.parse_args()

    if args.check is None or args.check == "all":
        # "all" is every check that can run in this invocation. counts needs
        # --test-counts, so it is not part of a bare run; naming it explicitly
        # is what makes it mandatory (see check_counts).
        wanted = [name for name in CHECKS if name != "counts" or args.test_counts]
    elif args.check in CHECKS:
        wanted = [args.check]
    else:
        return fail(f"unknown check {args.check!r}", "known checks: " + ", ".join(CHECKS) + ", all")

    root = os.path.normpath(os.path.abspath(os.path.join(os.getcwd(), args.root)))
    if not os.path.isdir(root):
        return fail(f"--root {args.root}: not a directory")

    problems = []
    try:
        if "links" in wanted:
            problems += check_links(root)
        if "counts" in wanted:
            problems += check_counts(args.test_counts, requested=args.check == "counts")
    except OSError as error:
        return fail(str(error))

    if problems:
        print(f"document check FAILED ({len(problems)} problem(s)):", file=sys.stderr)
        for problem in problems:
            print("  " + problem, file=sys.stderr)
        return 1

    ran = ", ".join(wanted)
    print(f"check-docs: {ran}: OK", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
