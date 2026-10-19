#!/usr/bin/env python3
"""Checks over the repository's markdown, because nothing else reads it.

The documents are the part of this repository no compiler sees: a link can rot,
a file can drift from the rules it describes, and every build stays green. This
script is the document gate, wired into scripts/check.sh beside the others.

Usage:
    scripts/check-docs.py [--root DIR] [--check NAME] [--test-counts FILE]
                          [--emit-baseline FILE]

    --root DIR          repository root (default: this script's parent)
    --check NAME        run one check: links, crate-map, orphans, citations,
                        counts, headers, lengths, code-paths, reachable or all
                        (default: all)
    --test-counts FILE  parsed test results, for the counts check
    --emit-baseline FILE
                        write the maintained documents that fail the four
                        navigation checks, one per line, and exit 0

The four navigation checks judge the maintained documents — the ones this
repository keeps current, as opposed to records such as research notes. They
are `headers` (Up, Authority, Code, Verify), `lengths` (the per-class budgets),
`code-paths` (the files Code names exist) and `reachable` (the Up chain reaches
the top of the document tree). The counts check compares the test totals
README.md states with the totals a test run actually printed, so the number is
checked rather than retyped; because it needs that capture it is the one check
that is not part of a bare run. A check that is asked for and cannot run is an
error, never a silent skip.

The document baseline was deleted after the document-tree audit, so a bare run
checks every maintained document. A future migration may create
scripts/doc-contract-baseline.txt explicitly; while it exists, the four checks
skip the documents it lists and fail on an entry whose file is missing or which
now passes every one of them, so the list is a ratchet that only shrinks.
"""
from __future__ import annotations

import argparse
import difflib
import glob
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

# The human's notebook. It is input to the project, not a document this gate
# maintains: link, orphan and citation rules would judge notes that follow no
# conventions, so the directory is exempt from all three. AGENTS.md forbids an
# agent from writing there at all, and scripts/check-human-written.sh enforces
# that separately.
HUMAN_ONLY = "docs/human-written/"

# Records: history and raw input. They are indexed and link-checked, but nobody
# keeps them current, so a navigation header or a line budget would be noise.
RECORD_DIRS = ("docs/research/", "docs/archive/", "docs/superpowers/", HUMAN_ONLY)

# A generated document is a product, like a record: the `crate-map` check owns
# its content byte for byte, so asking a person to add a header (or to trim it
# to a budget) would only be overwritten by the next regeneration.
GENERATED_DOCUMENTS = ("docs/crate-map.md",)

# The documents the navigation checks judge: the entries plus everything under
# docs/ that is kept current. tracked_markdown has already dropped the human's
# notebook; the record and generated classes are dropped here.
MAINTAINED_NAMES = ("README.md", "DEVELOPMENT.md")

# Four fields open every maintained document. Their order in a header is not
# checked; their presence is. A line that names a field may be plain or bold;
# only the bold form the schema documents is read.
HEADER_FIELDS = ("Up", "Authority", "Code", "Verify")
HEADER_FIELD = re.compile(r"^- \*\*(?P<name>[A-Za-z]+):\*\*\s*(?P<value>.*)$")

# The title is the first ATX level-one heading; the header follows it.
TITLE = re.compile(r"^# +\S")

# A backticked path is how Code names its anchors, so only that form is
# resolved. A token is a path when it contains "/" or ends in one of the
# repository's file extensions; that catches README.md, scripts/x.sh and
# crates/y alike, while a bare label such as `cargo` or `ADR-0004` is not a
# path claim. Commands, globs and prose with spaces are filtered out below.
BACKTICKED = re.compile(r"`([^`\n]+)`")
CODE_EXTENSIONS = (".md", ".rs", ".toml", ".sh", ".json", ".py", ".sql", ".yml", ".yaml")

# The commands that prove something here. "test" alone is not a command: prose
# such as "not tested" must not pass, so only a backticked span or one of these
# bare tokens counts. This is the same rule scripts/check-modules.py uses.
VERIFY_TOKENS = ("cargo", "./scripts/", "python3")

# The top of the document tree. README.md is the entry, docs/README.md is the
# index every maintained document is listed in, docs/04 the one-read
# architecture and docs/design/README.md the subsystem map. A document may be
# the root itself. Design documents must reach both design roots, because their
# path up has to pass through the subsystem map as well as the architecture.
ROOTS = (
    "README.md",
    "docs/README.md",
    "docs/04-system-overview.md",
    "docs/design/README.md",
)
DESIGN_ROOT = "docs/design/README.md"
DESIGN_ROOTS = (DESIGN_ROOT, "docs/04-system-overview.md")

# The navigation checks. They share the baseline ratchet, and each name is a
# value `--check` accepts.
CONTRACT_CHECKS = ("headers", "lengths", "code-paths", "reachable")

# Line budgets by class. The exact names come first, because a file budget is
# more specific than its directory's; a maintained document outside every named
# class gets the same 250 as the design/context/topics classes, so a new root
# file cannot slip in uncapped. Records and generated documents are exempt and
# never reach this table.
EXACT_BUDGETS = {
    "README.md": 250,
    "DEVELOPMENT.md": 300,
    "docs/README.md": 150,
    "docs/00-vision.md": 150,
    "docs/02-topics.md": 150,
    "docs/03-architecture.md": 250,
    "docs/04-system-overview.md": 180,
    "docs/05-parity-and-synthesis.md": 250,
    "docs/06-context-engineering.md": 60,
    "docs/07-extending.md": 60,
}
PREFIX_BUDGETS = (
    ("docs/design/", 250),
    ("docs/context/", 250),
    ("docs/topics/", 250),
    ("docs/extending/", 150),
    ("docs/runbooks/", 300),
    ("docs/decisions/", 300),
)
DEFAULT_BUDGET = 250

# The baseline, relative to the root. It is a ratchet, not an ignore list: a
# missing file means nothing is baselined, and an entry that no longer fails
# every one of the four checks is a failure. The file was deleted after the
# document-tree audit, so the gate normally checks the whole tree; a future
# migration recreates it explicitly.
DEFAULT_BASELINE = "scripts/doc-contract-baseline.txt"
BASELINE_COMMENT = """\
# Documents whose navigation contract is not complete yet. The gate skips these
# and fails on an entry that is missing or now passes, so this list only shrinks.
# Regenerate with: python3 scripts/check-docs.py --emit-baseline scripts/doc-contract-baseline.txt
"""

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
CHECKS = (
    "links",
    "crate-map",
    "orphans",
    "citations",
    "counts",
    "headers",
    "lengths",
    "code-paths",
    "reachable",
)

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

# One `test result:` line per test binary, and the only place the number of
# tests that actually ran is stated. The counts check sums these lines: with
# many binaries in the workspace, one binary's numbers are not the suite's.
TEST_RESULT = re.compile(r"^test result:.*$", re.MULTILINE)
PASSED_TESTS = re.compile(r"(\d+) passed")
IGNORED_TESTS = re.compile(r"(\d+) ignored")

# The one machine-readable line README.md carries, on its own line beneath the
# Chinese sentence a reader sees. The format is fixed by the plan; the check
# compares these two numbers and never tries to read the prose around them.
COUNTS_COMMENT = re.compile(
    r"^<!-- test-counts: (\d+) passed, (\d+) ignored -->$", re.MULTILINE
)


def fail(*lines: str) -> int:
    """Report one reason the gate could not run or did not pass."""
    print("check-docs: error:", lines[0], file=sys.stderr)
    for line in lines[1:]:
        print("  " + line, file=sys.stderr)
    return 1


def tracked_markdown(root: str) -> list[str]:
    """Every tracked .md file, relative to root, in git's order.

    Tracked, not walked: scratch piles and generated trees at the repository
    root are gitignored or untracked and must never enter the gate. The human's
    notebook is excluded: it is input, not a document this gate maintains.
    """
    listing = subprocess.run(
        ["git", "-C", root, "ls-files", "*.md"],
        capture_output=True,
        text=True,
    )
    if listing.returncode != 0:
        reason = " ".join(listing.stderr.split()) or "failed"
        raise CheckError(f"git ls-files in {root}: {reason}")
    return [name for name in listing.stdout.split() if not name.startswith(HUMAN_ONLY)]


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


def masked_document(root: str, name: str) -> tuple[str, str | None]:
    """One tracked document's text with code blanked out, or why it cannot be read.

    Every check that reads a document needs the same two things and the same
    two decisions, so they live here once rather than in each caller:

      * the text is masked with code_free, because a document may *show* a
        target, a link or a citation without owning it;
      * a document whose own markdown cannot be read is a failure for that
        document, not a licence to skip it.

    Returns (masked text, failure line or None). A mask that cannot be trusted
    is the failure; the caller reports it and keeps over the rest of the walk.
    """
    path = os.path.join(root, name)
    with open(path, encoding="utf-8") as handle:
        text = handle.read()
    try:
        return code_free(text), None
    except CheckError as error:
        return "", f"FAIL {name}: {error}"


def judged_documents(root: str) -> list[str]:
    """The tracked documents the reading checks judge; never an empty list.

    tracked_markdown is the raw walk. This is its entry point for the checks
    that *read* what they find, and it owns the empty case once, for all of
    them: a walk that lists no document would make the caller loop zero times
    and report OK while judging nothing, which is the silent pass this gate
    exists to refuse. A tree with no tracked markdown is an error, not a green
    run.
    """
    names = tracked_markdown(root)
    if not names:
        raise CheckError(
            "no tracked .md file to check; the walk is empty, so a green run "
            "here would have judged nothing"
        )
    return names


def is_record(name: str) -> bool:
    """True for history, research and the human's notebook: never maintained."""
    return any(name.startswith(prefix) for prefix in RECORD_DIRS)


def maintained_documents(root: str) -> list[str]:
    """The documents the navigation checks judge; never an empty list.

    The maintained set is the two root entries plus everything under docs/
    except the records and the generated crate map. It is deliberately
    narrower than tracked_markdown: a research note is history and the crate
    map is a product, so a header or a budget would be judged against a file
    nobody is allowed to write by hand. The empty case is an error for the same
    reason judged_documents owns it: four loops over nothing would print OK
    while checking nothing.
    """
    names = [
        name
        for name in tracked_markdown(root)
        if name in MAINTAINED_NAMES or name.startswith("docs/")
    ]
    names = [
        name
        for name in names
        if not is_record(name) and name not in GENERATED_DOCUMENTS
    ]
    if not names:
        raise CheckError(
            "no maintained document to check; every tracked .md is a record or "
            "generated, so a green run here would have judged nothing"
        )
    return names


def document_text(root: str, name: str) -> tuple[str, str | None]:
    """One maintained document's raw text, or why it cannot be read.

    Raw, not code-masked: the header's Code field is made of inline code spans,
    and masking would erase exactly what this check has to read. A document
    that cannot be read fails rather than disappearing from the walk.
    """
    try:
        with open(os.path.join(root, *name.split("/")), encoding="utf-8") as handle:
            return handle.read(), None
    except (OSError, UnicodeDecodeError) as error:
        return "", f"FAIL {name}:1: cannot read: {error}"


def header_of(text: str) -> tuple[int, dict[str, tuple[int, str]]] | None:
    """(title line, {field: (line, value)}) for a document, or None if no title.

    A header field is the first `- **Field:** value` line within the first 20
    lines after the title. 20 is enough for four fields, a blank line and a
    wrapped value, and small enough that a field mentioned in prose far below
    can never satisfy the contract. Only the first occurrence of each field is
    read: a later duplicate is prose or a mistake, not a second header.
    """
    lines = text.splitlines()
    title = next((index for index, line in enumerate(lines) if TITLE.match(line)), None)
    if title is None:
        return None
    fields: dict[str, tuple[int, str]] = {}
    for index in range(title + 1, min(len(lines), title + 21)):
        match = HEADER_FIELD.match(lines[index])
        if not match or match.group("name") not in HEADER_FIELDS:
            continue
        field = match.group("name")
        if field not in fields:
            fields[field] = (index + 1, match.group("value").strip())
    return title + 1, fields


def up_targets(name: str, value: str) -> list[str]:
    """Repository-relative paths a header's Up value links to.

    `name` is the document's own repository-relative path; a markdown link is
    resolved against the document's directory, as a reader's browser would.
    Fragment-only and external targets are not parent edges; the links check
    already owns whether a target exists.
    """
    found: list[str] = []
    for _, target in find_links(value):
        if target.startswith("<") and target.endswith(">"):
            target = target[1:-1].strip()
        if not target or target.startswith(("#",) + SKIPPED_SCHEMES):
            continue
        destination = target.split("#", 1)[0]
        if not destination:
            continue
        found.append(
            posixpath.normpath(
                posixpath.join(posixpath.dirname(name), destination)
            )
        )
    return found


def verify_resolves(value: str) -> bool:
    """True when a header's Verify value names a command this repository runs.

    The same rule as scripts/check-modules.py: a backticked span is the reliable
    form, and a bare `cargo`, `./scripts/` or `python3` is accepted for a field
    written without markup. A prose sentence containing the word "test" is not
    a command and deliberately does not pass.
    """
    if any(token in value for token in VERIFY_TOKENS):
        return True
    return any(match.group(1).strip() for match in BACKTICKED.finditer(value))


def required_header_fields(name: str) -> tuple[str, ...]:
    """The fields this class of document must carry.

    README.md is the entry root: only its Up is optional, because the entry may
    be the top; Authority, Code and Verify are still required, so a headerless
    or half-written entry cannot pass once it leaves the baseline. docs/README.md
    is the index root: Code and Verify only. ADRs are records of decisions and
    gain only Up; Status and the rest of their body stay theirs.
    """
    if name == "README.md":
        return ("Authority", "Code", "Verify")
    if name == INDEX:
        return ("Code", "Verify")
    if name.startswith("docs/decisions/") and name.endswith(".md"):
        return ("Up",)
    return HEADER_FIELDS


def check_headers(root: str) -> list[tuple[str, str]]:
    """Every maintained document must open with its navigation header.

    The header is how a reader walks up from a detail document to the design
    that owns it, so its absence is invisible to every other check: a document
    with no Up still renders and still compiles. A document whose title or
    header cannot be read fails here; it is never skipped.
    """
    problems: list[tuple[str, str]] = []
    for name in maintained_documents(root):
        text, failure = document_text(root, name)
        if failure:
            problems.append((name, failure))
            continue
        header = header_of(text)
        if header is None:
            problems.append(
                (name, f"FAIL {name}:1: no '# ' title, so the header cannot be read")
            )
            continue
        title, fields = header
        required = required_header_fields(name)
        for field in required:
            if field not in fields:
                problems.append(
                    (
                        name,
                        f"FAIL {name}:{title}: header has no '{field}:' field",
                    )
                )
                continue
            line, value = fields[field]
            if not value:
                problems.append(
                    (name, f"FAIL {name}:{line}: '{field}:' is empty")
                )
                continue
            if field == "Up" and not up_targets(name, value):
                problems.append(
                    (
                        name,
                        f"FAIL {name}:{line}: 'Up:' must name a relative "
                        "markdown link",
                    )
                )
            elif field == "Verify" and not verify_resolves(value):
                problems.append(
                    (
                        name,
                        f"FAIL {name}:{line}: 'Verify:' must name a command "
                        "(cargo, ./scripts/ or python3)",
                    )
                )
        # Up is optional only for the roots, but a root that writes one must
        # still write a real parent edge rather than a sentence.
        if "Up" in fields and "Up" not in required:
            line, value = fields["Up"]
            if not up_targets(name, value):
                problems.append(
                    (
                        name,
                        f"FAIL {name}:{line}: 'Up:' must name a relative "
                        "markdown link",
                    )
                )
    return problems


def budget_for(name: str) -> int:
    """The line budget for a maintained document."""
    if name in EXACT_BUDGETS:
        return EXACT_BUDGETS[name]
    for prefix, budget in PREFIX_BUDGETS:
        if name.startswith(prefix):
            return budget
    return DEFAULT_BUDGET


def check_lengths(root: str) -> list[tuple[str, str]]:
    """Every maintained document must stay within its class's line budget.

    Length is a proxy for the real rule: a document that can be read in one
    sitting, and that links down instead of restating its children. The budget
    is by file or directory class, so a detail document has a tighter limit
    than the entry it hangs from.
    """
    problems: list[tuple[str, str]] = []
    for name in maintained_documents(root):
        text, failure = document_text(root, name)
        if failure:
            problems.append((name, failure))
            continue
        count = len(text.splitlines())
        budget = budget_for(name)
        if count > budget:
            problems.append((name, f"FAIL {name}: {count} > {budget}"))
    return problems


def check_code_paths(root: str) -> list[tuple[str, str]]:
    """Every Code path a maintained document names must exist.

    A backticked token is a path claim when it contains "/" or ends in one of
    the repository's file extensions, so `README.md`, `scripts/check.sh` and
    `crates/kernel/src/lib.rs` are all resolved, not only the crates/ and docs/
    prefixes. Prose in the same field, a command such as
    `cargo test -p X crates/y`, and a glob or an ellipsis are not a claim that
    one file exists, so they are not read as paths. A fragment is an anchor
    inside the file the token already names, not part of the filename. The
    check is separate from headers because a path can rot long after the
    header was written and the failure should say which path.
    """
    problems: list[tuple[str, str]] = []
    for name in maintained_documents(root):
        text, failure = document_text(root, name)
        if failure:
            problems.append((name, failure))
            continue
        header = header_of(text)
        if header is None:
            continue  # headers reports the unreadable header
        fields = header[1]
        if "Code" not in fields:
            continue  # headers reports the missing field
        line, value = fields["Code"]
        for token in BACKTICKED.findall(value):
            token = token.strip()
            if " " in token or any(character in token for character in "*?[]"):
                continue
            if token.endswith("...") or "/..." in token:
                continue
            candidate = token.split("#", 1)[0].rstrip("/")
            if not candidate:
                continue
            if "/" not in candidate and not candidate.lower().endswith(CODE_EXTENSIONS):
                continue
            if os.path.exists(os.path.join(root, *candidate.split("/"))):
                continue
            problems.append(
                (name, f"FAIL {name}:{line}: Code: {token} does not exist")
            )
    return problems


def reached_roots(
    start: str, edges: dict[str, list[str]], roots: tuple[str, ...]
) -> set[str]:
    """The roots reachable from start by following Up edges; cycles are fine.

    A visited set is what makes a cycle terminate. The traversal does not stop
    at the first root: a design document has to reach two, and stopping early
    would turn "reaches 04" into a pass even when the subsystem map is missing
    from its chain.
    """
    seen: set[str] = set()
    stack = [start]
    found: set[str] = set()
    while stack:
        node = stack.pop()
        if node in seen:
            continue
        seen.add(node)
        if node in roots:
            found.add(node)
        for target in edges.get(node, ()):
            if target not in seen:
                stack.append(target)
    return found


def check_reachable(root: str) -> list[tuple[str, str]]:
    """Every maintained document must have a checked path back to the top.

    The graph is the maintained documents and their Up links. README.md,
    docs/README.md (the index), docs/04-system-overview.md and
    docs/design/README.md are the roots. A document must reach at least one of
    them; a document under docs/design/ must reach both docs/design/README.md
    and docs/04, because its path up runs through the subsystem map as well as
    the architecture. A link to a missing file is already a `links` failure;
    here it simply cannot help the chain.

    One line per document with no path, not one per broken link: the reader
    needs to know which document is stranded, and the links check names the
    target.
    """
    names = maintained_documents(root)
    edges: dict[str, list[str]] = {}
    lines: dict[str, int] = {}
    for name in names:
        text, failure = document_text(root, name)
        header = header_of(text) if not failure else None
        if failure or header is None:
            edges[name] = []
            lines[name] = 1
            continue
        title, fields = header
        if "Up" in fields:
            line, value = fields["Up"]
            edges[name] = up_targets(name, value)
            lines[name] = line
        else:
            edges[name] = []
            lines[name] = title

    problems: list[tuple[str, str]] = []
    for name in names:
        found = reached_roots(name, edges, ROOTS)
        if name.startswith("docs/design/"):
            missing = [root_name for root_name in DESIGN_ROOTS if root_name not in found]
            if missing:
                problems.append(
                    (
                        name,
                        f"FAIL {name}:{lines[name]}: Up chain does not reach "
                        + " or ".join(missing),
                    )
                )
        elif not found:
            problems.append(
                (
                    name,
                    f"FAIL {name}:{lines[name]}: Up chain reaches none of the "
                    "roots: " + ", ".join(ROOTS),
                )
            )
    return problems


def contract_results(root: str) -> dict[str, list[tuple[str, str]]]:
    """Every navigation check's failures, keyed by check name.

    All four are always evaluated, even when `--check` asks for one: the
    baseline's staleness is a property of the whole contract, so a document
    that still fails lengths must not make its baseline entry stale just
    because it was examined by the headers check alone.
    """
    return {
        "headers": check_headers(root),
        "lengths": check_lengths(root),
        "code-paths": check_code_paths(root),
        "reachable": check_reachable(root),
    }


def read_baseline(root: str) -> set[str]:
    """Paths in the baseline; a missing file means nothing is baselined.

    Missing is the first run, before the file exists — not an error and not a
    licence to pass anything: the checks still run over every maintained
    document.
    """
    path = os.path.join(root, *DEFAULT_BASELINE.split("/"))
    entries: set[str] = set()
    if not os.path.isfile(path):
        return entries
    try:
        with open(path, encoding="utf-8") as handle:
            for raw in handle:
                stripped = raw.strip()
                if not stripped or stripped.startswith("#"):
                    continue
                entries.add(stripped.replace(os.sep, "/"))
    except OSError as error:
        raise CheckError(f"{DEFAULT_BASELINE}: {error.strerror or error}") from error
    return entries


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
    for name in judged_documents(root):
        scan, failure = masked_document(root, name)
        if failure:
            broken.append(failure)
            continue
        # find_links masks again; code_free is idempotent on masked text
        # (verified over every tracked document), and it owns that step for
        # its other caller, indexed_targets, which passes raw text.
        for line, target in find_links(scan):
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
    committed link can actually rot. The human's notebook is excluded: its
    notes are input, not documents in the index (AGENTS.md).
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
    return [
        name
        for name in listing.stdout.split()
        if name.endswith(".md") and not name.startswith(HUMAN_ONLY)
    ]


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
    """The highest topic number across every place topics are declared.

    During the migration the T-NN headings move from docs/02-topics.md into
    docs/topics/*.md, so both locations are read until the move is finished:
    a gate that read only one would either reject real citations now or accept
    stale ones later. The files are read live, so a new topic raises the limit
    by existing and no later task has to edit this check. If neither location
    declares a heading, the check cannot judge a citation and says so instead
    of passing.
    """
    sources = []
    entry = os.path.join(root, "docs", "02-topics.md")
    if os.path.isfile(entry):
        sources.append(entry)
    sources += sorted(glob.glob(os.path.join(root, "docs", "topics", "*.md")))
    numbers: list[int] = []
    for source in sources:
        try:
            with open(source, encoding="utf-8") as handle:
                text = handle.read()
        except OSError as error:
            relative = os.path.relpath(source, root).replace(os.sep, "/")
            raise CheckError(f"{relative}: {error.strerror or error}") from error
        numbers += [int(match.group(1)) for match in TOPIC_HEADING.finditer(text)]
    if not numbers:
        raise CheckError(
            "docs/02-topics.md and docs/topics/*.md: no '## T-NN' heading; the "
            "highest topic cannot be derived"
        )
    return max(numbers)


def check_citations(root: str) -> list[str]:
    """No document may cite an ADR or a topic that does not exist.

    What this catches, exactly:

      * `ADR-NNNN` above the highest ADR file in docs/decisions/, and `T-NN`
        above the highest `## T-NN` heading in docs/02-topics.md or
        docs/topics/*.md.

    What it does not catch, by design: a *stale* citation. `ADR-0001…ADR-0014`
    passes even though 18 ADRs exist, because 14 is a real ADR; deciding which
    of the real ones a paragraph should have named instead is prose judgement,
    not a citation that points at nothing. This gate checks existence only.

    Scope is every tracked .md file, the same set the links check reads: a
    citation can rot in README.md or DEVELOPMENT.md just as easily as under
    docs/, and a gate that read only docs/** would miss them.

    Code is blanked out first, through the same masked_document helper the
    links check reads through: a document may *show* a citation without owning
    it, and the plan that specifies this check spells out `ADR-0099` as its
    example. Sharing that step keeps the plan and spec under the rule that
    governs every other document, so a bare ADR-0099 in the same prose still
    fails.
    """
    adr_limit = highest_adr(root)
    topic_limit = highest_topic(root)
    problems = []
    for name in judged_documents(root):
        scan, failure = masked_document(root, name)
        if failure:
            problems.append(failure)
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


def reported_totals(counts_text: str, source: str) -> tuple[int, int]:
    """(passed, ignored) summed over every `test result:` line in counts_text.

    A `test result:` line is what one test binary prints once for the whole
    binary, and there are many binaries in this workspace, so the suite's total
    is the *sum* over all of them. A file with no such line is an error: a
    counts check that parsed nothing and reported OK would be the exact silent
    pass this gate exists to refuse.
    """
    lines = TEST_RESULT.findall(counts_text)
    if not lines:
        raise CheckError(
            f"{source}: no 'test result:' line; there is nothing to compare, so "
            "a green run here would have checked nothing"
        )
    passed = ignored = 0
    for line in lines:
        found_passed = PASSED_TESTS.search(line)
        found_ignored = IGNORED_TESTS.search(line)
        if found_passed is None or found_ignored is None:
            raise CheckError(f"{source}: cannot read passed/ignored from {line!r}")
        passed += int(found_passed.group(1))
        ignored += int(found_ignored.group(1))
    return passed, ignored


def check_counts(root: str, counts_file: str | None, *, requested: bool) -> list[str]:
    """README.md's test numbers must equal what the suite itself reported.

    The number that started this gate was hand-typed and wrong; it drifted
    three times without any build noticing. So the two fields of the README's
    `test-counts` comment are compared with the `passed` and `ignored` totals
    in the captured test output, and nothing else in the file is read.

    Why the capture is parsed rather than the source counted: counting the
    tree finds 748 `#[test]`/`#[tokio::test]` attributes and 13 occurrences of
    `#[ignore`, two of which are prose in doc comments — and neither number is
    what the harness runs. A test behind a `cfg`, a macro-generated test, or an
    ignored-but-listed test makes the source count a different number again.
    The suite's own totals are the authority for how many tests the suite has;
    anything else here is a guess wearing a number.
    """
    if requested and counts_file is None:
        return [
            "FAIL --check counts needs --test-counts FILE; "
            "a check that cannot read the counts must not pass silently"
        ]
    if counts_file is None:
        return []

    try:
        with open(counts_file, encoding="utf-8", errors="replace") as handle:
            counts_text = handle.read()
    except OSError as error:
        raise CheckError(f"{counts_file}: {error.strerror or error}") from error
    passed, ignored = reported_totals(counts_text, counts_file)

    path = os.path.join(root, "README.md")
    try:
        with open(path, encoding="utf-8") as handle:
            readme = handle.read()
    except OSError as error:
        raise CheckError(f"README.md: {error.strerror or error}") from error

    stated = COUNTS_COMMENT.findall(readme)
    if not stated:
        raise CheckError(
            "README.md: no '<!-- test-counts: N passed, M ignored -->' line; "
            "the counts it states cannot be read"
        )
    if len(stated) > 1:
        raise CheckError(
            f"README.md: {len(stated)} 'test-counts' comments; which one states "
            "the counts is ambiguous"
        )
    said_passed, said_ignored = (int(value) for value in stated[0])

    problems = []
    if said_passed != passed:
        problems.append(
            f"FAIL README.md: says {said_passed} passed, the suite reports {passed}"
        )
    if said_ignored != ignored:
        problems.append(
            f"FAIL README.md: says {said_ignored} ignored, the suite reports {ignored}"
        )
    return problems


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
    parser.add_argument(
        "--emit-baseline",
        default=None,
        metavar="FILE",
        help="write the maintained documents that fail the four navigation "
        "checks there and exit 0",
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

    if args.emit_baseline is not None:
        try:
            results = contract_results(root)
        except (CheckError, OSError) as error:
            return fail(str(error))
        failing = sorted(
            {name for pairs in results.values() for name, _ in pairs}
        )
        target = os.path.normpath(os.path.abspath(args.emit_baseline))
        try:
            os.makedirs(os.path.dirname(target) or ".", exist_ok=True)
            with open(target, "w", encoding="utf-8") as handle:
                handle.write(BASELINE_COMMENT)
                for name in failing:
                    handle.write(name + "\n")
        except OSError as error:
            return fail(f"{args.emit_baseline}: {error.strerror or error}")
        # emit is a migration tool: it records what would fail now and exits 0,
        # even when the tree still has failures.
        print(
            f"check-docs: wrote {len(failing)} document(s) to "
            f"{os.path.relpath(target, os.getcwd())}",
            file=sys.stderr,
        )
        return 0

    # All four navigation checks are evaluated whenever any of them is wanted,
    # because a baseline entry is stale only when the document now passes the
    # whole contract, not just the one check being asked for.
    contract = [name for name in CONTRACT_CHECKS if name in wanted]
    problems: list[str] = []
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
            problems += check_counts(
                root, args.test_counts, requested=args.check == "counts"
            )
        if contract:
            results = contract_results(root)
            baseline = read_baseline(root)
            failing = {name for pairs in results.values() for name, _ in pairs}
            for entry in sorted(baseline):
                if entry not in failing:
                    problems.append(
                        f"FAIL {DEFAULT_BASELINE}: stale baseline entry: "
                        f"{entry} (file is missing or now passes)"
                    )
            for name in CONTRACT_CHECKS:
                if name not in contract:
                    continue
                for document, message in results[name]:
                    if document in baseline:
                        continue
                    problems.append(message)
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
