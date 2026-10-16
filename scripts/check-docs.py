#!/usr/bin/env python3
"""Checks over the repository's markdown, because nothing else reads it.

The documents are the part of this repository no compiler sees: a link can rot,
a file can drift from the rules it describes, and every build stays green. This
script is the document gate, wired into scripts/check.sh beside the others.

Usage:
    scripts/check-docs.py [--root DIR] [--check NAME] [--test-counts FILE]

    --root DIR          repository root (default: this script's parent)
    --check NAME        run one check: links, crate-map, counts or all (default: all)
    --test-counts FILE  parsed test results, for the counts check

Only --check links and --check crate-map exist today; later tasks add checks
behind the same interface, which is why --check and --test-counts are already
here. A check that is asked for and cannot run is an error, never a silent skip.
"""
from __future__ import annotations

import argparse
import difflib
import os
import re
import subprocess
import sys


class CheckError(Exception):
    """The check cannot run at all; main turns this into a one-line failure."""


# The three URL forms a markdown link may use without naming a file here, and
# the fragment form, which points inside the current document.
SKIPPED_SCHEMES = ("http:", "https:", "mailto:")

# A blank line — any run of spaces or tabs on it — ends a paragraph.
BLANK_LINE = re.compile(r"\n[ \t]*\n")

# [text](target) and ![alt](target). Targets never contain an unbalanced
# bracket in this repository; the plan and the craft documents do not use
# reference-style links.
LINK = re.compile(r"!?\[[^\]]*\]\(([^)]*)\)")

# Checks the file knows how to run. A name outside this set is a usage error,
# so a typo in check.sh cannot turn a gate into a no-op.
CHECKS = ("links", "crate-map", "counts")


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
        reason = " ".join(listing.stderr.split()) or "failed"
        raise CheckError(f"git ls-files in {root}: {reason}")
    return listing.stdout.split()


def _blank(text: str) -> str:
    """Same length and newlines as text, every other character a space."""
    return "".join(c if c == "\n" else " " for c in text)


def _opening_fence(line: str) -> tuple[str, int] | None:
    """(character, run length) if line opens a fenced block, else None."""
    stripped = line.lstrip(" ")
    if len(line) - len(stripped) > 3 or stripped[:3] not in ("```", "~~~"):
        return None
    return stripped[0], _run(stripped, stripped[0])


def _run(text: str, character: str) -> int:
    """How many times character starts text."""
    return len(text) - len(text.lstrip(character))


def _closing_fence(line: str, fence: tuple[str, int]) -> bool:
    """At least as long a run of the same character, and nothing else on it."""
    character, length = fence
    stripped = line.lstrip(" ")
    run = _run(stripped, character)
    return run >= length and stripped[run:].strip() == ""


def _fence_name(fence: tuple[str, int]) -> str:
    """A fence named unambiguously: a run of a character, spelled out."""
    character, length = fence
    return f"{length} x {character!r}"


def code_free(text: str) -> str:
    """A copy of text with fenced blocks and inline code spans blanked out.

    A document is allowed to *show* a link without owning it — the plan for
    this very check contains `[text](target)` inside code spans — so code is
    removed before any target is looked for. Blanks keep every line number, so
    masking can never *hide* a link silently: a fence left open at end of file
    is an error rather than a licence to blank the rest of the document.
    """
    lines = text.split("\n")
    out: list[str] = []
    fence: tuple[str, int] | None = None
    fence_line = 0
    for number, line in enumerate(lines, 1):
        if fence is None:
            fence = _opening_fence(line)
            if fence is not None:
                fence_line = number
                out.append(_blank(line))
                continue
            out.append(line)
        else:
            if _closing_fence(line, fence):
                fence = None
            out.append(_blank(line))
    if fence is not None:
        raise CheckError(
            f"line {fence_line} opens a fence of {_fence_name(fence)} and it is never "
            "closed; everything after it would be ignored."
        )

    # Inline spans, run by run: `x`, ``x``, and so on. A run is closed by a run
    # of the same length inside the same paragraph — CommonMark forbids an
    # inline span from crossing a blank line, and without that bound one stray
    # backtick would swallow every link down to the next one.
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
        # A blank line, not the end of the document, is where a span must stop.
        paragraph_break = BLANK_LINE.search(joined, i)
        bound = paragraph_break.start() + 1 if paragraph_break else len(joined)
        # The closer run must have the same length, so skip past longer runs.
        closer = i + run
        while True:
            closer = joined.find("`" * run, closer, bound)
            if closer == -1:
                break
            after = closer + run
            if after >= len(joined) or joined[after] != "`":
                break
            closer = after
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


def check_one_link(root: str, name: str, line: int, target: str) -> str | None:
    """A failure line if this target names nothing that exists, else None."""
    # Markdown may wrap a target in angle brackets; titles are not used in this
    # repository, and a bare space in a target is a typo.
    if target.startswith("<") and target.endswith(">"):
        target = target[1:-1].strip()
    if not target or target.startswith(("#",) + SKIPPED_SCHEMES):
        return None
    # A fragment points inside a document this link already names.
    destination = target.split("#", 1)[0]
    if not destination:
        return None
    resolved = os.path.normpath(os.path.join(os.path.dirname(os.path.join(root, name)), destination))
    if os.path.exists(resolved):
        return None
    return f"FAIL {name}:{line}: {target} does not exist"


def check_links(root: str) -> list[str]:
    """Every relative link in every tracked document must resolve."""
    broken = []
    for name in tracked_markdown(root):
        path = os.path.join(root, name)
        with open(path, encoding="utf-8") as handle:
            text = handle.read()
        try:
            links = find_links(text)
        except CheckError as error:
            # A document whose own markdown cannot be read is a failure, not a
            # licence to skip it.
            broken.append(f"FAIL {name}: {error}")
            continue
        for line, target in links:
            problem = check_one_link(root, name, line, target)
            if problem:
                broken.append(problem)
    return broken


def check_crate_map(root: str) -> list[str]:
    """docs/crate-map.md must equal what scripts/layering_rules.py generates.

    Regenerate-and-compare, the same shape as gen-schema.py --check: the map the
    reader sees is a product of the rules CI enforces, so the two cannot drift.
    A generator that cannot run is a failure, never an OK over a stale file.
    """
    generator = os.path.join(root, "scripts", "layering_rules.py")
    generated = subprocess.run(
        [sys.executable, generator, "--emit-map", root],
        capture_output=True,
    )
    if generated.returncode != 0:
        reason = generated.stderr.decode("utf-8", "replace").strip().splitlines()
        raise CheckError(
            "cannot generate the crate map (scripts/layering_rules.py --emit-map): "
            + (reason[0] if reason else f"exit {generated.returncode}")
        )

    path = os.path.join(root, "docs", "crate-map.md")
    try:
        with open(path, "rb") as handle:
            current = handle.read()
    except OSError as error:
        raise CheckError(f"docs/crate-map.md: {error.strerror or error}")
    if current == generated.stdout:
        return []

    diff = list(
        difflib.unified_diff(
            current.decode("utf-8", "replace").splitlines(),
            generated.stdout.decode("utf-8", "replace").splitlines(),
            fromfile="docs/crate-map.md (on disk)",
            tofile="docs/crate-map.md (generated)",
            lineterm="",
        )
    )
    if not diff:
        diff = ["(the two differ only in line endings or encoding)"]
    return [
        "FAIL docs/crate-map.md: does not match what scripts/layering_rules.py "
        "generates; regenerate with "
        "`python3 scripts/layering_rules.py --emit-map . > docs/crate-map.md`\n"
        + "\n".join("    " + line for line in diff)
    ]


def check_counts(counts_file: str | None, *, requested: bool) -> list[str]:
    """Not implemented yet; the parameter is the interface a later task fills.

    Until that task lands there is no counts logic at all, so even with a file
    in hand the check must say so instead of printing OK. Asked for by name
    without --test-counts it is an error too: a check that quietly does nothing
    is worse than no check.
    """
    if requested and counts_file is None:
        return [
            "FAIL --check counts needs --test-counts FILE; "
            "a check that cannot read the counts must not pass silently"
        ]
    if counts_file is not None:
        return [
            "FAIL --check counts: not implemented yet; it must not report OK "
            "while it checks nothing"
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
        if "crate-map" in wanted:
            problems += check_crate_map(root)
        if "counts" in wanted:
            problems += check_counts(args.test_counts, requested=args.check == "counts")
    except (CheckError, OSError) as error:
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
