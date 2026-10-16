#!/usr/bin/env python3
"""Checks over the repository's markdown, because nothing else reads it.

The documents are the part of this repository no compiler sees: a link can rot,
a file can drift from the rules it describes, and every build stays green. This
script is the document gate, wired into scripts/check.sh beside the others.

Usage:
    scripts/check-docs.py [--root DIR] [--check NAME] [--test-counts FILE]

    --root DIR          repository root (default: this script's parent)
    --check NAME        run one check: links, crate-map, orphans, citations,
                        counts or all (default: all)
    --test-counts FILE  parsed test results, for the counts check

The counts check arrives in a later task; the rest exist today behind the same
interface, which is why --check and --test-counts are already here. A check that
is asked for and cannot run is an error, never a silent skip.
"""
from __future__ import annotations

import argparse
import difflib
import os
import posixpath
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

# The first cell of a table row, which the index uses in two forms: a link
# (`| [crate-map.md](crate-map.md) |`) or a bare relative path (`| decisions/ |`).
# Both name something; only the first is a link.
TABLE_ROW = re.compile(r"^[ \t]*\|[ \t]*([^|\n]+?)[ \t]*(?=\|)", re.MULTILINE)

# The one document allowed to list the others, and so the only one the orphans
# check reads. It is itself the index: it must not need a row about itself, and
# a missing index is an error rather than a list of orphans it cannot judge.
INDEX = "docs/README.md"

# Checks the file knows how to run. A name outside this set is a usage error,
# so a typo in check.sh cannot turn a gate into a no-op.
CHECKS = ("links", "crate-map", "orphans", "citations", "counts")

# An ADR is named by a four-digit number and a topic by a two-digit one. Both
# acronyms are upper case in this repository, and both numbers are zero-padded,
# so the match is exact rather than a prefix of some longer name.
ADR_CITATION = re.compile(r"ADR-(\d{4})")
TOPIC_CITATION = re.compile(r"\bT-(\d{2})\b")

# An ADR file is named by the number it records, so the directory is its own
# list of the ADRs that exist.
ADR_FILENAME = re.compile(r"^(\d{4})-")

# A topic is a `## T-NN · …` heading; the title after the number is prose.
TOPIC_HEADING = re.compile(r"^## T-(\d{2})\b", re.MULTILINE)


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


def docs_markdown(root: str) -> list[str]:
    """Every .md under docs/ that git would carry, in git's order.

    Tracked *and* untracked-but-not-ignored: the orphan rule is about a
    document existing without a row in the index, and a writer who saves a new
    document sees that the moment it exists, not at the next commit. Ignored
    files are excluded, so a local scratch pile under docs/ is not a gate
    failure; the other checks keep to tracked files, which are the ones a
    committed link can actually rot.
    """
    listing = subprocess.run(
        ["git", "-C", root, "ls-files", "--cached", "--others",
         "--exclude-standard", "--", "docs"],
        capture_output=True,
        text=True,
    )
    if listing.returncode != 0:
        reason = " ".join(listing.stderr.split()) or "failed"
        raise CheckError(f"git ls-files -- docs in {root}: {reason}")
    return [name for name in listing.stdout.split() if name.endswith(".md")]


def indexed_targets(root: str) -> set[str]:
    """Every repository-relative path the index names, however it names it.

    Two ways of naming are read, because this index uses both. A markdown link
    target (`[crate-map.md](crate-map.md)`) is one. The other is the first cell
    of a table row that holds a bare relative path instead of a link, which is
    how the index lists the directory rows: `| decisions/ |`, `| research/ |`.
    Only table rows are read for the second form, so a directory merely
    *mentioned* in a sentence or in a later cell is a mention, not a row.
    """
    path = os.path.join(root, *INDEX.split("/"))
    try:
        with open(path, encoding="utf-8") as handle:
            text = handle.read()
    except OSError as error:
        raise CheckError(f"{INDEX}: {error.strerror or error}") from error
    try:
        # find_links raises on a fence left open in the index, which would hide
        # the rest of the rows; a target list that cannot be trusted is an
        # error, never a shorter list of indexed documents.
        links = find_links(text)
    except CheckError as error:
        raise CheckError(f"{INDEX}: {error}") from error

    targets: set[str] = {match.group(1).strip() for match in TABLE_ROW.finditer(text)}
    targets.update(target for _, target in links)

    indexed: set[str] = set()
    for target in targets:
        if target.startswith("<") and target.endswith(">"):
            target = target[1:-1].strip()
        if not target or target.startswith(("#",) + SKIPPED_SCHEMES):
            continue
        destination = target.split("#", 1)[0]
        if not destination:
            continue
        # Paths in a document are URLs, so they use "/" on every platform;
        # resolve them on POSIX rather than with the host's separator.
        own = posixpath.normpath(posixpath.join("docs", destination))
        if own == "docs" or own.startswith("docs/"):
            indexed.add(own)
    return indexed


def check_orphans(root: str) -> list[str]:
    """Every .md under docs/ must be reachable from the one index.

    The rule, exactly:

      * A document is indexed when its repository-relative path is named in
        docs/README.md — that is, the path is one of the index's link targets,
        or the bare relative path in the first cell of one of its table rows.
        docs/README.md itself is the index and is not required to list itself.
      * Otherwise the document is indexed when the first path segment below
        docs/ is named there. So a row naming `superpowers/` indexes every
        document anywhere under docs/superpowers/, at any depth, and a row
        naming `decisions/` indexes the 18 ADRs. This is the deliberate broad
        case: it is why one row covers a whole subtree, and why a new file in
        an already-indexed subtree does not need a new row.
      * A document at the root of docs/ has no such first directory, so
        docs/00-vision.md is indexed only by being named itself.

    Matching is a case-sensitive, exact comparison of normalised
    repository-relative paths: no prefix or substring matching, so `docs/research/`
    never covers `docs/research-notes/x.md`. The set is built from the index as
    it actually is — markdown link targets plus table row first cells — because
    this index writes directory rows bare (`| decisions/ |`) and file rows as
    links, and both name a document.
    """
    indexed = indexed_targets(root)
    problems = []
    for name in docs_markdown(root):
        if not name.startswith("docs/") or not name.endswith(".md"):
            continue
        if name == INDEX:
            continue
        if name in indexed:
            continue
        # The first directory below docs/: "superpowers" for
        # docs/superpowers/plans/x.md, None for a file at the root of docs/.
        rest = name[len("docs/"):]
        first = rest.split("/", 1)[0] if "/" in rest else None
        # A row may name that directory with or without its trailing slash.
        if first is not None and (
            f"docs/{first}/" in indexed or f"docs/{first}" in indexed
        ):
            continue
        if first is None:
            hint = f"add a row naming {name}"
        else:
            hint = (
                f"add a row naming it, or a row naming docs/{first}/, which "
                "covers everything under it"
            )
        problems.append(f"FAIL {name}: not indexed in {INDEX}; {hint}")
    return problems


def highest_adr(root: str) -> int:
    """The highest ADR number that exists, from the ADR filenames.

    The directory is read live, so a new `docs/decisions/NNNN-*.md` raises the
    limit by existing and no later task has to edit this check. A directory
    with nothing matching is an error: a bound that cannot be derived would
    wave every citation through, which is exactly the silent pass this gate
    must never produce.
    """
    directory = os.path.join(root, "docs", "decisions")
    try:
        names = os.listdir(directory)
    except OSError as error:
        raise CheckError(f"docs/decisions: {error.strerror or error}") from error
    numbers = [
        int(match.group(1))
        for name in names
        if (match := ADR_FILENAME.match(name)) and name.endswith(".md")
    ]
    if not numbers:
        raise CheckError(
            "docs/decisions: no NNNN-*.md file; the highest ADR cannot be derived"
        )
    return max(numbers)


def highest_topic(root: str) -> int:
    """The highest topic number in docs/02-topics.md, from its `## T-NN` headings.

    Read live for the same reason as the ADRs: the topics file will grow, and
    the limit has to grow with it without an edit here. The file is where
    topics are declared, so if it cannot be read, or declares none, the check
    cannot judge a citation and says so instead of passing.
    """
    path = os.path.join(root, "docs", "02-topics.md")
    try:
        with open(path, encoding="utf-8") as handle:
            text = handle.read()
    except OSError as error:
        raise CheckError(f"docs/02-topics.md: {error.strerror or error}") from error
    numbers = [int(match.group(1)) for match in TOPIC_HEADING.finditer(text)]
    if not numbers:
        raise CheckError(
            "docs/02-topics.md: no '## T-NN' heading; the highest topic cannot "
            "be derived"
        )
    return max(numbers)


def check_citations(root: str) -> list[str]:
    """No document may cite an ADR or a topic that does not exist.

    What this catches, exactly:

      * `ADR-NNNN` above the highest ADR file in docs/decisions/, and `T-NN`
        above the highest `## T-NN` heading in docs/02-topics.md.

    What it does not catch, by design: a *stale* citation. `ADR-0001…ADR-0014`
    passes even though 18 ADRs exist, because 14 is a real ADR; deciding which
    of the real ones a paragraph should have named instead is prose judgement,
    not a citation that points at nothing. This gate checks existence only.

    Scope is every tracked .md file, the same set the links check reads: a
    citation can rot in README.md or DEVELOPMENT.md just as easily as under
    docs/, and a gate that read only docs/** would miss them.

    Code is masked first with code_free, exactly as the links check does: a
    document may *show* a citation without owning it, and the plan that
    specifies this check spells out `ADR-0099` as its example. Masking keeps
    that plan and spec under the rule that governs every other document, so a
    bare ADR-0099 in the same prose still fails.
    """
    adr_limit = highest_adr(root)
    topic_limit = highest_topic(root)
    problems = []
    for name in tracked_markdown(root):
        path = os.path.join(root, name)
        with open(path, encoding="utf-8") as handle:
            text = handle.read()
        try:
            scan = code_free(text)
        except CheckError as error:
            # A document whose own markdown cannot be read is a failure, not a
            # licence to skip it.
            problems.append(f"FAIL {name}: {error}")
            continue
        limits = ((ADR_CITATION, adr_limit), (TOPIC_CITATION, topic_limit))
        for pattern, limit in limits:
            for match in pattern.finditer(scan):
                if int(match.group(1)) > limit:
                    line = scan.count("\n", 0, match.start()) + 1
                    problems.append(
                        f"FAIL {name}:{line}: {match.group(0)} does not exist "
                        f"(highest is {limit})"
                    )
    return problems


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
        if "orphans" in wanted:
            problems += check_orphans(root)
        if "citations" in wanted:
            problems += check_citations(root)
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
