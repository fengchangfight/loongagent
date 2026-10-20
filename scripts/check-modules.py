#!/usr/bin/env python3
"""Checks that every Rust file and item carries its contract.

The compiler reads the code, not the comments above it, so a file can lose its
design anchor or an item can lose its doc block and every build stays green.
This script is the code contract gate, wired into scripts/check.sh beside the
document gate: a `//!` block on the file and a doc block on every `fn`, `struct`,
`enum`, `trait`, `union` and `type`.

Usage:
    scripts/check-modules.py [--root DIR] [--paths P1,P2] [--baseline FILE]
                             [--emit-baseline FILE] [--list]

    --root DIR          repository root (default: this script's parent)
    --paths P1,P2       check only these files, relative to the root; the
                        baseline is ignored, which is how a seeded defect or a
                        scratch file is inspected
    --baseline FILE     files allowed to fail for now (default:
                        scripts/code-contract-baseline.txt, which is deleted; a
                        missing file means nothing is baselined)
    --emit-baseline FILE
                        write the currently failing paths there, one per line,
                        and exit 0; refuses to write when nothing fails
    --list              print each in-scope file, its item count and status; the
                        extra output does not change the exit code

The baseline is a ratchet, not an ignore list. If one of its entries names a
missing file or a file that now passes, the gate fails: the list may only
shrink, so a migration removes its own entries and never leaves one behind. No
baseline exists now — `scripts/code-contract-baseline.txt` was deleted after the
code audit — so the gate checks the whole tracked tree. A missing baseline file
means "nothing is baselined", never a silent pass; a future migration that needs
one recreates it explicitly.

Exemptions are different from the baseline. A file that cannot carry a contract
is listed in scripts/contract-exemptions.txt as `path | reason`; a single item
may be exempted as `path#item | reason` or with a `// Contract-exempt: <reason>`
line above it. The reason must be at least 10 characters, and a stale exemption
fails. The file-level list is closed to generated code and the two crate-root
Cargo build scripts; item-level exemptions are not limited by that set.

The scanner masks line comments, nested block comments, string literals, char
literals and raw strings before it looks for items, preserving newlines so the
reported line numbers stay true. A `fn` inside a string or a comment is not an
item, and must not be counted as one.
"""
from __future__ import annotations

import argparse
import bisect
import glob
import os
import re
import subprocess
import sys

DEFAULT_BASELINE = "scripts/code-contract-baseline.txt"
DEFAULT_EXEMPTIONS = "scripts/contract-exemptions.txt"

# The file contract names both directions of the edge; the item contract uses
# the shorter Up/Down, because inside a file the neighbours are the file's.
MODULE_LABELS = ("Serves", "Design", "Upstream", "Downstream", "Verify")
ITEM_LABELS = ("Serves", "Design", "Up", "Down", "Verify")

# A private function is compact when the signature line through its closing
# brace is at most this many lines. The limit is deliberately small: the compact
# form is for one-line helpers, not for hiding a real function's contract.
TRIVIAL_SPAN = 12

# A reason shorter than this says nothing, so it does not exempt anything.
MIN_REASON = 10

# The file-level exemption list is closed. Only code that cannot carry a
# contract belongs here; an entry outside this list means either a real contract is being
# avoided or a generated file appeared without a rule, and both need a person.
# Item-level `path#item` exemptions are not limited by this set.
ALLOWED_FILE_EXEMPTIONS = frozenset(
    {
        "crates/schema/src/generated.rs",
        "crates/schema/tests/generated_roundtrip.rs",
        "crates/cli/build.rs",
        "crates/control/build.rs",
    }
)

# Indentation, visibility, modifiers, the keyword and the name. The masked text
# has already lost comments and strings, so a `fn` in either cannot match. `mod`
# is not in the alternation: the file contract covers the modules it declares.
ITEM = re.compile(
    r"^(?P<indent>[ \t]*)"
    r"(?P<vis>pub(?:\s*\([^)]*\))?\s+)?"
    r"(?P<mods>(?:(?:async|unsafe|const|default|extern)\s+)*)"
    r"(?P<kind>fn|struct|enum|trait|union|type)\s+"
    r"(?P<name>[A-Za-z_][A-Za-z0-9_]*)"
)

# A label may be plain (`Serves:`) or markdown bold (`**Serves:**`). The names
# are case-sensitive: they are a contract's field names, not prose.
FIELD_PATTERNS: dict[str, re.Pattern[str]] = {}

# `///` is an outer doc line; `////` is an ordinary comment and is not one.
DOC_LINE = re.compile(r"^///(?!/)")

# The single-line item escape allowed by AGENTS.md, in addition to the file of
# exemptions. It sits above the item (with its attributes and doc block) and
# covers that one item only.
INLINE_EXEMPT = re.compile(r"^//\s*Contract-exempt:\s*(.*)$")

# `#[test]` and `#[tokio::test]` are how this repository marks a test function.
TEST_ATTRIBUTE = re.compile(r"#\[\s*(?:tokio::)?test\b")

# A backticked path is how a contract names its anchors, so only that form is
# resolved; loose prose does not count as a link.
BACKTICKED = re.compile(r"`([^`\n]+)`")
ADR = re.compile(r"ADR-(\d{4})")

# The commands that prove something in this repository. "test" alone is not a
# command: prose such as "not tested" must not pass the Verify check, so only a
# backticked command or one of these bare tokens counts.
VERIFY_TOKENS = ("cargo", "./scripts/", "python3")

BASELINE_COMMENT = """\
# Files whose code contract is not complete yet. The gate skips these and fails
# on an entry that is missing or now passes, so this list only shrinks.
# Regenerate with: python3 scripts/check-modules.py --emit-baseline scripts/code-contract-baseline.txt
"""


class CheckError(Exception):
    """The check cannot run at all; main turns this into a one-line failure."""


class Item:
    """One annotatable declaration found in the masked source."""

    __slots__ = (
        "kind",
        "name",
        "line",
        "public",
        "declaration",
        "span",
        "docs",
        "attributes",
        "inline_exempt",
    )

    def __init__(
        self,
        kind: str,
        name: str,
        line: int,
        public: bool,
        declaration: bool,
        span: int | None,
        docs: str,
        attributes: str,
        inline_exempt: str | None,
    ) -> None:
        self.kind = kind
        self.name = name
        self.line = line
        self.public = public
        self.declaration = declaration
        self.span = span
        self.docs = docs
        self.attributes = attributes
        self.inline_exempt = inline_exempt

    @property
    def is_test(self) -> bool:
        return self.kind == "fn" and bool(TEST_ATTRIBUTE.search(self.attributes))

    @property
    def label(self) -> str:
        return f"{self.kind} `{self.name}`"


def fail(message: str) -> int:
    """Report a reason the gate could not run or did not pass."""
    print("check-modules: error:", message, file=sys.stderr)
    return 1


def field_value(text: str, name: str) -> str | None:
    """The text after `name:` on its line, or None when the label is absent.

    Only the rest of that line is the value: a contract field is one line in
    this repository, and reading further would let a missing field borrow the
    value of the next one.
    """
    pattern = FIELD_PATTERNS.get(name)
    if pattern is None:
        pattern = re.compile(rf"(?:\*\*)?{re.escape(name)}(?:\*\*)?\s*:")
        FIELD_PATTERNS[name] = pattern
    for line in text.splitlines():
        match = pattern.search(line)
        if match:
            return line[match.end() :].strip()
    return None


def design_resolves(value: str, root: str) -> bool:
    """True when a `Design:` value names a real document or ADR.

    The anchor is either a backticked `docs/...md` path that exists or an
    `ADR-NNNN` whose `docs/decisions/NNNN-*.md` file exists. Prose alone does
    not count: an anchor that cannot be followed is the drift this gate exists
    to catch.
    """
    for match in BACKTICKED.finditer(value):
        candidate = match.group(1).strip().split("#", 1)[0]
        if not (candidate.startswith("docs/") and candidate.endswith(".md")):
            continue
        path = os.path.join(root, *candidate.split("/"))
        if os.path.isfile(path):
            return True
    for match in ADR.finditer(value):
        number = match.group(1)
        pattern = os.path.join(root, "docs", "decisions", f"{number}-*.md")
        if glob.glob(pattern):
            return True
    return False


def verify_resolves(value: str) -> bool:
    """True when a `Verify:` value names a command this repository can run.

    A backticked span is the reliable form, and a bare `cargo`, `./scripts/` or
    `python3` is accepted for the contracts that write the command without
    marking it up. A prose sentence containing the word "test" is not a
    command and deliberately does not pass.
    """
    if any(token in value for token in VERIFY_TOKENS):
        return True
    return any(match.group(1).strip() for match in BACKTICKED.finditer(value))


# -- masking ---------------------------------------------------------------


def _mask_span(masked: list[str], start: int, end: int) -> None:
    """Blank a span but keep its newlines, so line numbers stay true."""
    for index in range(start, min(end, len(masked))):
        if masked[index] != "\n":
            masked[index] = " "


def _string_end(source: str, start: int) -> int:
    """The index just past a `"..."` literal, escapes included."""
    index = start + 1
    while index < len(source):
        char = source[index]
        if char == "\\":
            index += 2
        elif char == '"':
            return index + 1
        else:
            index += 1
    return len(source)


def _char_end(source: str, start: int) -> int | None:
    """The index just past a char literal, or None when this is a lifetime.

    `'a'` is a char and gets masked; `'a` is a lifetime and stays, because
    masking it would hide the code after it. A char literal has a closing quote
    after one character or one escape.
    """
    index = start + 1
    if index >= len(source):
        return None
    if source[index] == "\\":
        if source.startswith("\\u{", index):
            close = source.find("}", index + 3)
            if close == -1:
                return None
            index = close + 1
        elif source.startswith("\\x", index):
            index += 4
        else:
            index += 2
    else:
        index += 1
    if index < len(source) and source[index] == "'":
        return index + 1
    return None


def _raw_string_span(source: str, start: int) -> tuple[int, int] | None:
    """The span of a raw string starting here, or None.

    Raw strings (`r"..."`, `r#"..."#`, `br#"..."#`) are where a `fn` in a test
    fixture usually hides; they have no escapes, so the closing quote and hash
    count are the only end marker.
    """
    if start > 0 and (source[start - 1].isalnum() or source[start - 1] == "_"):
        return None
    if source.startswith("br", start):
        index = start + 2
    elif source.startswith("r", start):
        index = start + 1
    else:
        return None
    hashes = 0
    while index < len(source) and source[index] == "#":
        hashes += 1
        index += 1
    if index >= len(source) or source[index] != '"':
        return None
    index += 1
    closing = '"' + "#" * hashes
    close = source.find(closing, index)
    if close == -1:
        return (start, len(source))
    return (start, close + len(closing))


def mask(source: str) -> str:
    """Blank comments, strings, chars and raw strings, preserving newlines.

    One left-to-right pass is enough: whichever construct starts first wins, so
    a `//` inside a string is part of the string and a quote inside a comment is
    part of the comment. Nested block comments are counted rather than closed at
    the first `*/`, because Rust nests them and a nested comment is exactly the
    place a decoy `fn` would sit.
    """
    masked = list(source)
    index = 0
    length = len(source)
    while index < length:
        char = source[index]
        if char == "/" and index + 1 < length:
            nxt = source[index + 1]
            if nxt == "/":
                end = source.find("\n", index)
                if end == -1:
                    end = length
                _mask_span(masked, index, end)
                index = end
                continue
            if nxt == "*":
                depth = 1
                end = index + 2
                while end < length and depth:
                    if source.startswith("/*", end):
                        depth += 1
                        end += 2
                    elif source.startswith("*/", end):
                        depth -= 1
                        end += 2
                    else:
                        end += 1
                _mask_span(masked, index, end)
                index = end
                continue
        raw = _raw_string_span(source, index)
        if raw is not None:
            _mask_span(masked, raw[0], raw[1])
            index = raw[1]
            continue
        if char == '"':
            end = _string_end(source, index)
            _mask_span(masked, index, end)
            index = end
            continue
        if char == "'":
            end = _char_end(source, index)
            if end is not None:
                _mask_span(masked, index, end)
                index = end
                continue
        index += 1
    return "".join(masked)


# -- headers and items -----------------------------------------------------


def _attribute_start(masked_lines: list[str], end: int) -> int | None:
    """The first line of the attribute block ending at `end`, if there is one.

    Walking the bracket balance upward handles a multi-line attribute such as a
    formatted `#[derive(...)]`; a single-line one is just the same walk of one
    line. The check that the start begins with `#[` keeps a code line ending in
    `]` from being mistaken for an attribute.
    """
    if not masked_lines[end].strip().endswith("]"):
        return None
    depth = 0
    index = end
    while index >= 0:
        depth += masked_lines[index].count("[") - masked_lines[index].count("]")
        if depth == 0:
            return index if masked_lines[index].strip().startswith("#[") else None
        index -= 1
    return None


def _block_doc_start(raw_lines: list[str], end: int) -> int | None:
    """The first line of the `/** ... */` doc block ending at `end`, or None."""
    index = end - 1
    while index >= 0:
        stripped = raw_lines[index].strip()
        if stripped.startswith("/**") and not stripped.startswith("/**/"):
            return index
        index -= 1
    return None


def header_info(
    raw_lines: list[str], masked_lines: list[str], index: int
) -> tuple[str, str, str | None]:
    """The doc block above an item, its attributes, and any inline exemption.

    The scan goes upward and skips blank lines and whole attributes, including
    multi-line ones, so a blank line or a formatted `#[derive(...)]` between the
    doc block and the item does not hide the contract. It stops at the first
    line that is neither docs, an attribute, nor an exemption: that line is the
    previous item, and its docs are not this item's.
    """
    docs: list[str] = []
    attributes: list[str] = []
    inline_exempt = None
    cursor = index - 1
    while cursor >= 0:
        raw = raw_lines[cursor]
        stripped = raw.strip()
        if not stripped:
            cursor -= 1
            continue
        match = INLINE_EXEMPT.match(stripped)
        if match:
            if inline_exempt is None:
                inline_exempt = match.group(1).strip()
            cursor -= 1
            continue
        start = _attribute_start(masked_lines, cursor)
        if start is not None:
            attributes.append("\n".join(raw_lines[start : cursor + 1]))
            cursor = start - 1
            continue
        if DOC_LINE.match(stripped):
            docs.append(raw)
            cursor -= 1
            continue
        if stripped.startswith("/**") and not stripped.startswith("/**/"):
            docs.append(raw)
            cursor -= 1
            continue
        if stripped.endswith("*/"):
            start = _block_doc_start(raw_lines, cursor + 1)
            if start is not None:
                docs.extend(raw_lines[start : cursor + 1])
                cursor = start - 1
                continue
        break
    docs.reverse()
    return "\n".join(docs), "\n".join(attributes), inline_exempt


def _body_span(masked: str, offsets: list[int], index: int) -> int | None:
    """Line span from the signature line to the closing brace, or None.

    A declaration (a trait method ending in `;`) has no body and returns None.
    Parentheses and square brackets are tracked while looking for the opening
    brace, so an array length like `[u8; 4]` is not mistaken for a declaration
    and the `{` of a const generic is not mistaken for the body.
    """
    length = len(masked)
    cursor = offsets[index]
    parens = 0
    brackets = 0
    while cursor < length:
        char = masked[cursor]
        if char == "(":
            parens += 1
        elif char == ")":
            parens = max(0, parens - 1)
        elif char == "[":
            brackets += 1
        elif char == "]":
            brackets = max(0, brackets - 1)
        elif parens == 0 and brackets == 0:
            if char == ";":
                return None
            if char == "{":
                break
        cursor += 1
    else:
        return None

    depth = 0
    while cursor < length:
        char = masked[cursor]
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                close_line = bisect.bisect_right(offsets, cursor) - 1
                return close_line - index + 1
        cursor += 1
    return None


def find_items(raw_lines: list[str], masked_lines: list[str], masked: str) -> list[Item]:
    """Every annotatable item in the file, in source order."""
    offsets = []
    position = 0
    for line in masked_lines:
        offsets.append(position)
        position += len(line) + 1
    items = []
    for index, line in enumerate(masked_lines):
        match = ITEM.match(line)
        if not match:
            continue
        docs, attributes, inline_exempt = header_info(raw_lines, masked_lines, index)
        span = _body_span(masked, offsets, index)
        items.append(
            Item(
                kind=match.group("kind"),
                name=match.group("name"),
                line=index + 1,
                public=match.group("vis") is not None,
                declaration=span is None,
                span=span,
                docs=docs,
                attributes=attributes,
                inline_exempt=inline_exempt,
            )
        )
    return items


def leading_module_block(
    raw_lines: list[str], masked_lines: list[str]
) -> tuple[str | None, int]:
    """The leading `//!` block and the line it starts on, or (None, 1).

    Inner attributes (`#![...]`) may sit above the block; blank lines between
    paragraphs of the block are part of it. The first line that is not `//!` or
    blank ends it. `//!` is deliberately not accepted as an item doc: a file
    contract is not an item contract.
    """
    cursor = 0
    while cursor < len(raw_lines) and not raw_lines[cursor].strip():
        cursor += 1
    while cursor < len(raw_lines):
        stripped = raw_lines[cursor].strip()
        if not stripped.startswith("#!["):
            break
        depth = 0
        while cursor < len(raw_lines):
            depth += masked_lines[cursor].count("[") - masked_lines[cursor].count("]")
            cursor += 1
            if depth == 0:
                break
        while cursor < len(raw_lines) and not raw_lines[cursor].strip():
            cursor += 1

    if cursor >= len(raw_lines) or not raw_lines[cursor].strip().startswith("//!"):
        return None, 1

    start = cursor
    block: list[str] = []
    while cursor < len(raw_lines):
        stripped = raw_lines[cursor].strip()
        if stripped.startswith("//!"):
            block.append(stripped[3:])
            cursor += 1
        elif not stripped:
            block.append("")
            cursor += 1
        else:
            break
    return "\n".join(block), start + 1


# -- checks ----------------------------------------------------------------


def check_module(relative: str, block: str, line: int, root: str) -> list[str]:
    """Every problem with this file's module contract, one per line."""
    problems = []
    prefix = f"{relative}:{line}:"
    if field_value(block, "Purpose") is None:
        problems.append(f"{prefix} module contract: missing Purpose:")
    for label in MODULE_LABELS:
        value = field_value(block, label)
        if value is None:
            problems.append(f"{prefix} module contract: missing {label}:")
        elif label == "Design" and not design_resolves(value, root):
            problems.append(
                f"{prefix} module contract: Design: names no existing "
                "`docs/...md` file or ADR-NNNN"
            )
        elif label == "Verify":
            if not value:
                problems.append(f"{prefix} module contract: Verify: is empty")
            elif not verify_resolves(value):
                problems.append(
                    f"{prefix} module contract: Verify: must name a command "
                    "(cargo, test, ./scripts/ or python3)"
                )
    return problems


def _full_item_problems(
    relative: str, item: Item, text: str, root: str
) -> list[str]:
    problems = []
    prefix = f"{relative}:{item.line}: {item.label}:"
    for label in ITEM_LABELS:
        value = field_value(text, label)
        if value is None:
            problems.append(f"{prefix} missing {label}:")
        elif label == "Design" and not design_resolves(value, root):
            problems.append(
                f"{prefix} Design: names no existing `docs/...md` file or ADR-NNNN"
            )
        elif label in ("Up", "Down") and not value:
            problems.append(f"{prefix} {label}: is empty")
        elif label == "Verify":
            if not value:
                problems.append(f"{prefix} Verify: is empty")
            elif not verify_resolves(value):
                problems.append(
                    f"{prefix} Verify: must name a command "
                    "(cargo, test, ./scripts/ or python3)"
                )
    return problems


def check_item(relative: str, item: Item, root: str) -> list[str]:
    """Every problem with one item's contract, one per line."""
    prefix = f"{relative}:{item.line}: {item.label}:"

    if item.inline_exempt is not None:
        if len(item.inline_exempt) < MIN_REASON:
            return [
                f"{prefix} Contract-exempt reason must be at least "
                f"{MIN_REASON} characters"
            ]
        return []

    if not item.docs:
        return [f"{prefix} no doc block (expected `///` above the item)"]

    text = item.docs
    problems = []
    if field_value(text, "Purpose") is None:
        problems.append(f"{prefix} missing Purpose:")

    # A test function's target is named by the file contract. A declaration has
    # no body on which to hang Up/Down, so one Purpose line is enough; that
    # ruling covers trait methods and also body-less functions outside a trait,
    # such as a signature in an `extern "C"` block.
    if item.kind == "fn" and (item.is_test or item.declaration):
        return problems

    # Types are contracts even when private, and a public item is one by
    # definition: no compact form for either.
    if item.public or item.kind != "fn":
        problems.extend(_full_item_problems(relative, item, text, root))
        return problems

    # A private function. The compact form needs Purpose plus `Contract:`; the
    # full form is always acceptable. Only a trivial body may use the compact
    # form, so a long private function cannot hide behind one line.
    contract = field_value(text, "Contract")
    has_all_labels = all(field_value(text, label) is not None for label in ITEM_LABELS)
    if contract and (item.span is not None and item.span <= TRIVIAL_SPAN):
        return problems
    if has_all_labels:
        problems.extend(_full_item_problems(relative, item, text, root))
        return problems

    span = "unknown" if item.span is None else f"{item.span} lines"
    if contract:
        problems.append(
            f"{prefix} private function is non-trivial ({span}); needs the five "
            "labels"
        )
    elif item.span is not None and item.span <= TRIVIAL_SPAN:
        problems.append(
            f"{prefix} trivial private function needs `Contract:` (or the five "
            "labels)"
        )
    else:
        problems.append(
            f"{prefix} private function is non-trivial ({span}); needs the five "
            "labels"
        )
    return problems


# -- exemptions and baseline ----------------------------------------------


def read_exemptions(root: str) -> tuple[dict[str, str], dict[str, dict[str, str]], list[str]]:
    """Parse the exemption file into file-level and item-level maps.

    A malformed line and a too-short reason are reported here rather than
    silently dropping the exemption: an exemption that does not parse would
    otherwise look like it worked while the file stayed unexempted.
    """
    relative = DEFAULT_EXEMPTIONS
    path = os.path.join(root, *relative.split("/"))
    file_level: dict[str, str] = {}
    item_level: dict[str, dict[str, str]] = {}
    problems: list[str] = []
    if not os.path.isfile(path):
        return file_level, item_level, problems
    with open(path, encoding="utf-8") as handle:
        for number, raw in enumerate(handle, 1):
            stripped = raw.strip()
            if not stripped or stripped.startswith("#"):
                continue
            if "|" not in stripped:
                problems.append(
                    f"{relative}:{number}: expected `path | reason`"
                )
                continue
            key, reason = (part.strip() for part in stripped.split("|", 1))
            if len(reason) < MIN_REASON:
                problems.append(
                    f"{relative}:{number}: exemption reason must be at least "
                    f"{MIN_REASON} characters: {key}"
                )
                continue
            if "#" in key:
                item_path, item_name = key.split("#", 1)
                item_path = item_path.strip()
                item_name = item_name.strip()
                if not item_path or not item_name:
                    problems.append(
                        f"{relative}:{number}: expected `path#item | reason`"
                    )
                    continue
                item_level.setdefault(item_path, {})[item_name] = reason
            elif key not in ALLOWED_FILE_EXEMPTIONS:
                problems.append(
                    f"{relative}:{number}: file exemption is not allowed: "
                    f"{key} (the list is closed to the generated schema files "
                    "and the two crate-root build scripts)"
                )
            else:
                file_level[key] = reason
    return file_level, item_level, problems


def _read_paths(path: str) -> list[str]:
    """Paths from a file, ignoring blank and comment lines."""
    if not os.path.isfile(path):
        return []
    entries = []
    with open(path, encoding="utf-8") as handle:
        for raw in handle:
            stripped = raw.strip()
            if not stripped or stripped.startswith("#"):
                continue
            entries.append(stripped)
    return entries


def tracked_rust(root: str) -> list[str]:
    """Every tracked Rust file under crates, relative to the root, in git order.

    Tracked, not walked: a scratch file at the root or a generated tree is not
    part of the tree the contracts describe. `--paths` exists for the cases
    where a specific, possibly untracked, file should be checked.
    """
    listing = subprocess.run(
        ["git", "-C", root, "ls-files", "crates/**/*.rs"],
        capture_output=True,
        text=True,
    )
    if listing.returncode != 0:
        reason = " ".join(listing.stderr.split()) or "failed"
        raise CheckError(f"git ls-files in {root}: {reason}")
    return [name for name in listing.stdout.splitlines() if name.endswith(".rs")]


def relative_to_root(root: str, value: str) -> str:
    """A `--paths` value, normalised and relative to the root.

    `--paths` is documented as root-relative, so a relative value is joined to
    the root rather than to the shell's current directory; only an absolute
    value is taken as given.
    """
    absolute = value if os.path.isabs(value) else os.path.join(root, value)
    relative = os.path.relpath(os.path.abspath(absolute), root)
    if relative == ".." or relative.startswith(".." + os.sep):
        raise CheckError(f"--paths {value}: outside the root {root}")
    return relative.replace(os.sep, "/")


def resolve_file_arg(value: str) -> str:
    """An explicit file argument, relative to the current directory."""
    return os.path.normpath(os.path.abspath(value))


# -- main ------------------------------------------------------------------


def main() -> int:
    repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    parser = argparse.ArgumentParser(
        prog="check-modules.py",
        description="Check the code contracts (run from scripts/check.sh).",
    )
    parser.add_argument(
        "--root", default=repo, help="repository root (default: this repository)"
    )
    parser.add_argument(
        "--paths",
        default=None,
        metavar="P1,P2",
        help="check only these files, relative to the root; ignores the baseline",
    )
    parser.add_argument(
        "--baseline",
        default=None,
        metavar="FILE",
        help=f"baselined failures (default: {DEFAULT_BASELINE}, deleted; a "
        "missing file means nothing is baselined)",
    )
    parser.add_argument(
        "--emit-baseline",
        default=None,
        metavar="FILE",
        help="write the currently failing paths there and exit 0",
    )
    parser.add_argument(
        "--list",
        action="store_true",
        dest="list_files",
        help="print each in-scope file, its item count and status",
    )
    args = parser.parse_args()

    root = os.path.normpath(os.path.abspath(os.path.join(os.getcwd(), args.root)))
    if not os.path.isdir(root):
        return fail(f"--root {args.root}: not a directory")

    try:
        if args.paths is not None:
            files = []
            for value in args.paths.split(","):
                value = value.strip()
                if not value:
                    continue
                relative = relative_to_root(root, value)
                if not os.path.isfile(os.path.join(root, *relative.split("/"))):
                    raise CheckError(f"--paths {value}: no such file under {root}")
                if relative not in files:
                    files.append(relative)
            if not files:
                raise CheckError("--paths: no paths given")
            baseline_entries: set[str] = set()
            baseline_read = False
        else:
            files = tracked_rust(root)
            if not files:
                raise CheckError(
                    f"no tracked `crates/**/*.rs` files under {root}; "
                    "nothing was checked"
                )
            baseline_read = True
            baseline_label = DEFAULT_BASELINE
            if args.emit_baseline is not None:
                baseline_entries = set()
            else:
                baseline_path = (
                    resolve_file_arg(args.baseline)
                    if args.baseline is not None
                    else os.path.join(root, *DEFAULT_BASELINE.split("/"))
                )
                # Name the file that was actually read, not the default, when
                # --baseline points somewhere else.
                relative_baseline = os.path.relpath(baseline_path, root)
                if relative_baseline != ".." and not relative_baseline.startswith(
                    ".." + os.sep
                ):
                    baseline_label = relative_baseline.replace(os.sep, "/")
                else:
                    baseline_label = baseline_path
                baseline_entries = {
                    entry.replace(os.sep, "/") for entry in _read_paths(baseline_path)
                }

        file_exemptions, item_exemptions, problems = read_exemptions(root)

        # A file exemption names a file that must still exist; an item exemption
        # names an item that must still be there. Either kind going stale means
        # the list has drifted from the tree, so it fails like a stale baseline.
        for relative in sorted(file_exemptions):
            if not os.path.isfile(os.path.join(root, *relative.split("/"))):
                problems.append(
                    f"scripts/contract-exemptions.txt: stale exemption: "
                    f"{relative} (file is missing)"
                )

        results: list[tuple[str, list[str], list[Item], bool]] = []
        for relative in files:
            path = os.path.join(root, *relative.split("/"))
            try:
                with open(path, encoding="utf-8") as handle:
                    source = handle.read()
            except (OSError, UnicodeDecodeError) as error:
                results.append((relative, [f"{relative}: cannot read: {error}"], [], False))
                continue
            masked = mask(source)
            raw_lines = source.split("\n")
            masked_lines = masked.split("\n")
            items = find_items(raw_lines, masked_lines, masked)
            exempted = relative in file_exemptions
            if exempted:
                results.append((relative, [], items, True))
                continue
            block, block_line = leading_module_block(raw_lines, masked_lines)
            if block is None:
                issues = [
                    f"{relative}:1: module contract: no leading `//!` block"
                ]
            else:
                issues = check_module(relative, block, block_line, root)
            for item in items:
                if item.name in item_exemptions.get(relative, {}):
                    continue
                issues.extend(check_item(relative, item, root))
            results.append((relative, issues, items, False))

        # Stale item exemptions: in the default run the tracked set is the
        # scope, so a path that is not in it names a missing file. Under
        # --paths the scope is deliberately smaller, so an out-of-scope entry is
        # skipped rather than called stale for not having been read.
        default_scope = args.paths is None
        parsed = {relative for relative, _, _, _ in results}
        for relative, names in sorted(item_exemptions.items()):
            if relative not in parsed:
                if default_scope:
                    for name in sorted(names):
                        problems.append(
                            f"scripts/contract-exemptions.txt: stale exemption: "
                            f"{relative}#{name} (file is missing)"
                        )
                continue
            known: set[str] = set()
            for checked, _, items, _ in results:
                if checked == relative:
                    known = {item.name for item in items}
                    break
            for name in sorted(names):
                if name not in known:
                    problems.append(
                        f"scripts/contract-exemptions.txt: stale exemption: "
                        f"{relative}#{name} (item is missing)"
                    )

        # The baseline is a ratchet. A path that now passes is stale, and so is
        # one whose file is gone; both mean the list did not shrink when it
        # could have. Files listed there are not reported for now.
        failing = {
            relative
            for relative, issues, _, exempted in results
            if issues and not exempted
        }
        if baseline_read:
            for entry in sorted(baseline_entries):
                if entry not in failing:
                    problems.append(
                        f"{baseline_label}: stale baseline entry: {entry} "
                        "(file is missing or now passes)"
                    )

        if args.emit_baseline is not None:
            target = resolve_file_arg(args.emit_baseline)
            if not failing:
                # A comment-only baseline would silently turn every future
                # failure into a green run; refuse to write one.
                raise CheckError(
                    f"refusing to write an empty baseline to {target}: "
                    "no in-scope file fails"
                )
            os.makedirs(os.path.dirname(target) or ".", exist_ok=True)
            with open(target, "w", encoding="utf-8") as handle:
                handle.write(BASELINE_COMMENT)
                for relative in sorted(failing):
                    handle.write(relative + "\n")
            # emit is a migration tool: it reports what it would baseline, and
            # the stale problems it saw are warnings, not a reason to fail it.
            for problem in problems:
                print(f"check-modules: warning: {problem}", file=sys.stderr)
            print(
                f"check-modules: wrote {len(failing)} failing path(s) to "
                f"{os.path.relpath(target, root)}",
                file=sys.stderr,
            )
            return 0

        # Summarise before filtering, so the numbers describe the tree rather
        # than only the files that are still being reported.
        item_count = sum(len(items) for _, _, items, _ in results)
        documented = sum(
            1 for _, _, items, _ in results for item in items if item.docs
        )
        full = 0
        compact = 0
        inline_exemptions = 0
        for _, _, items, exempted in results:
            if exempted:
                continue
            for item in items:
                if item.inline_exempt is not None:
                    inline_exemptions += 1
                text = item.docs
                labels = all(
                    field_value(text, label) is not None for label in ITEM_LABELS
                )
                if field_value(text, "Purpose") is not None and labels:
                    full += 1
                elif (
                    not item.public
                    and item.kind == "fn"
                    and field_value(text, "Purpose") is not None
                    and field_value(text, "Contract")
                ):
                    compact += 1

        listed = []
        if args.list_files:
            for relative, issues, items, exempted in results:
                if exempted:
                    status = "EXEMPT"
                elif relative in baseline_entries:
                    status = "BASELINE"
                elif issues:
                    status = "FAIL"
                else:
                    status = "PASS"
                listed.append(f"{relative}: {len(items)} item(s) {status}")
            for line in listed:
                print(line)

        reportable = []
        for relative, issues, _, exempted in results:
            if exempted or relative in baseline_entries:
                continue
            reportable.extend(issues)
        reportable.extend(problems)

        if reportable:
            print(
                f"check-modules: FAILED ({len(reportable)} problem(s)):",
                file=sys.stderr,
            )
            for problem in reportable:
                print("  " + problem, file=sys.stderr)
            print(
                f"check-modules: {len(results)} files, {item_count} items, "
                f"{documented} documented, {full} full, {compact} compact, "
                f"{len(file_exemptions)} file exemption(s), "
                f"{sum(len(names) for names in item_exemptions.values())} item "
                f"exemption(s), {inline_exemptions} inline, "
                f"{len(baseline_entries)} baseline",
                file=sys.stderr,
            )
            return 1

        print(
            f"check-modules: {len(results)} files, {item_count} items, "
            f"{documented} documented, {full} full, {compact} compact, "
            f"{len(file_exemptions)} file exemption(s), "
            f"{sum(len(names) for names in item_exemptions.values())} item "
            f"exemption(s), {inline_exemptions} inline, "
            f"{len(baseline_entries)} baseline: OK",
            file=sys.stderr,
        )
        return 0
    except CheckError as error:
        return fail(str(error))
    except OSError as error:
        return fail(str(error))


if __name__ == "__main__":
    sys.exit(main())
