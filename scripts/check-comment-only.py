#!/usr/bin/env python3
"""Proves that a migration changed comments and nothing else.

The modular-contracts migration annotates every Rust file but must not change
behaviour. A compiler cannot tell an annotation commit from a bug fix, so this
script does: for each changed `.rs` file it compares the working tree with
`--base` after comments and whitespace outside literals are removed. Literal
text is kept, because `"a b"` and `"ab"` are different code.

Usage:
    scripts/check-comment-only.py [--root DIR] [--base REV]

    --root DIR      repository root (default: this repository)
    --base REV      revision to compare against (default: HEAD)

The scope is every tracked `.rs` file changed since `--base` (a deleted file is
a failure, not silently skipped) plus every untracked, non-ignored `.rs` file
under `crates/`. A new file is compared against an empty base, so a file that
holds only comments passes. The exit code is 0 when every file is comment-only
and 1 otherwise; a mismatch prints the first difference with both line numbers.

This is a migration tool, not a permanent gate: it is deliberately absent from
scripts/check.sh. Once the annotation migration is over it has no work to do.

The scanner reuses the literal boundaries from scripts/check-modules.py, loaded
by path because that file name is hyphenated. One scanner for both tools means a
raw string or a nested block comment cannot be read two different ways.
"""
from __future__ import annotations

import argparse
import importlib.util
import os
import subprocess
import sys

# The crate tree is the only place a new Rust file belongs; a scratch file at
# the repository root must not enter the comparison.
CRATES = "crates"

# The other gate's scanner, loaded by path: `check-modules.py` is not a valid
# module name, and copying its literal rules here would let the two drift.
MODULES_PATH = os.path.join(
    os.path.dirname(os.path.abspath(__file__)), "check-modules.py"
)

_GATE_MODULE = None


class CheckError(Exception):
    """The comparison cannot run at all; main turns this into a one-line failure."""


# -- git -------------------------------------------------------------------


def run_git(root: str, *args: str) -> str:
    """Run git in the root and return its stdout, or fail with one clear line."""
    result = subprocess.run(
        ["git", "-C", root, *args],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        reason = " ".join(result.stderr.split()) or f"exit {result.returncode}"
        raise CheckError(f"git {args[0]}: {reason}")
    return result.stdout


def git_root(root: str) -> str:
    """The repository top level containing `root`, so relative paths agree."""
    try:
        top = run_git(root, "rev-parse", "--show-toplevel").strip()
    except CheckError as error:
        raise CheckError(f"{root}: not a git work tree ({error})") from error
    if not top or not os.path.isdir(top):
        raise CheckError(f"{root}: git reported no usable work tree root")
    return os.path.normpath(top)


def resolve_base(root: str, revision: str) -> str:
    """Check that `revision` names a commit; a typo must not compare nothing."""
    result = subprocess.run(
        [
            "git",
            "-C",
            root,
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            f"{revision}^{{commit}}",
        ],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0 or not result.stdout.strip():
        raise CheckError(f"base revision does not exist: {revision}")
    return revision


def base_tree(root: str, base: str) -> set[str]:
    """Every path in the base commit, so a new file is not mistaken for a edit."""
    listing = run_git(root, "ls-tree", "-r", "--name-only", "-z", base)
    return {path for path in listing.split("\0") if path}


def tracked_changes(root: str, base: str) -> list[str]:
    """Every tracked `.rs` path whose content differs from `base`.

    Rename detection is off on purpose: a rename is a delete plus an add, and
    the old path must be reported as deleted rather than quietly dropped.
    `git diff <commit>` compares the commit with the working tree, index
    included, which is the tree the migration edits.
    """
    listing = run_git(
        root,
        "diff",
        "--name-only",
        "--no-renames",
        "-z",
        base,
        "--",
        "*.rs",
    )
    return [path for path in listing.split("\0") if path]


def untracked_rust(root: str) -> list[str]:
    """Non-ignored `.rs` files under crates/, in git order."""
    listing = run_git(
        root,
        "ls-files",
        "--others",
        "--exclude-standard",
        "-z",
        "--",
        CRATES,
    )
    return [path for path in listing.split("\0") if path.endswith(".rs")]


# -- comparison ------------------------------------------------------------


# The literal boundaries this script borrows from the other gate. They are
# private names there, so a rename must fail here with a line, not an
# AttributeError halfway through a file.
SCANNER_HELPERS = ("_string_end", "_char_end", "_raw_string_span")


def gate_module():
    """scripts/check-modules.py, imported by path because the name is hyphenated."""
    global _GATE_MODULE
    if _GATE_MODULE is None:
        spec = importlib.util.spec_from_file_location(
            "check_comment_only_gate_modules", MODULES_PATH
        )
        if spec is None or spec.loader is None:
            raise CheckError(f"cannot load {MODULES_PATH}")
        module = importlib.util.module_from_spec(spec)
        try:
            spec.loader.exec_module(module)
        except Exception as error:  # noqa: BLE001 - a broken helper is a gate error
            raise CheckError(f"cannot load {MODULES_PATH}: {error}") from error
        missing = [
            name for name in SCANNER_HELPERS if not hasattr(module, name)
        ]
        if missing:
            raise CheckError(
                f"{MODULES_PATH}: missing scanner helper(s): "
                + ", ".join(missing)
            )
        _GATE_MODULE = module
    return _GATE_MODULE


def _emit_literal(
    text: list[str], lines: list[int], chunk: str, line: int
) -> int:
    """Append literal text verbatim, recording each character's source line."""
    for char in chunk:
        text.append(char)
        lines.append(line)
        if char == "\n":
            line += 1
    return line


def canonical(source: str) -> tuple[str, list[int]]:
    """Strip comments and whitespace outside literals, keeping code and literals.

    `lines[index]` is the 1-based source line of `text[index]`, so a mismatch
    can name a line in each revision even though the compared text no longer
    carries layout. Quotes, escapes and literal contents are copied verbatim:
    whitespace inside a string is behaviour, so it must never be deleted.
    """
    gate = gate_module()
    text: list[str] = []
    lines: list[int] = []
    index = 0
    line = 1
    length = len(source)
    while index < length:
        char = source[index]
        if char == "/" and index + 1 < length:
            nxt = source[index + 1]
            if nxt == "/":
                # A line comment ends at the newline, which stays for counting.
                end = source.find("\n", index)
                if end == -1:
                    end = length
                line += source.count("\n", index, end)
                index = end
                continue
            if nxt == "*":
                # Rust nests block comments; close at the matching `*/`.
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
                line += source.count("\n", index, end)
                index = end
                continue
        raw = gate._raw_string_span(source, index)
        if raw is not None:
            start, end = raw
            line = _emit_literal(text, lines, source[start:end], line)
            index = end
            continue
        if char == '"':
            end = gate._string_end(source, index)
            line = _emit_literal(text, lines, source[index:end], line)
            index = end
            continue
        if char == "'":
            # `'a'` is a char literal and keeps its contents; `'a` is a
            # lifetime and is ordinary code.
            end = gate._char_end(source, index)
            if end is not None:
                line = _emit_literal(text, lines, source[index:end], line)
                index = end
                continue
        if char.isspace():
            if char == "\n":
                line += 1
            index += 1
            continue
        text.append(char)
        lines.append(line)
        index += 1
    return "".join(text), lines


def first_difference(base: str, work: str) -> int:
    """The offset of the first differing character, or the shorter length."""
    limit = min(len(base), len(work))
    for index in range(limit):
        if base[index] != work[index]:
            return index
    return limit


def line_at(lines: list[int], index: int) -> int:
    """The source line of `index`, clamping to the last line when one side ends."""
    if not lines:
        return 1
    if index >= len(lines):
        return lines[-1]
    return lines[index]


def snippet(text: str, index: int, width: int = 30) -> str:
    """A short window around the first difference, escaped onto one line."""
    start = max(0, index - width)
    end = min(len(text), index + width)
    window = (
        text[start:end]
        .replace("\\", "\\\\")
        .replace("\n", "\\n")
        .replace("\t", "\\t")
    )
    if start > 0:
        window = "..." + window
    if end < len(text):
        window += "..."
    return window


def compare_file(
    root: str,
    base: str,
    in_base: bool,
    relative: str,
) -> list[str]:
    """Failure lines for one file; an empty list means it is comment-only."""
    path = os.path.join(root, *relative.split("/"))
    if not os.path.isfile(path):
        return [f"{relative}: deleted from the working tree; cannot compare"]
    try:
        with open(path, encoding="utf-8") as handle:
            work_source = handle.read()
    except (OSError, UnicodeDecodeError) as error:
        return [f"{relative}: cannot read: {error}"]
    if in_base:
        try:
            base_source = run_git(root, "show", f"{base}:{relative}")
        except CheckError as error:
            return [f"{relative}: cannot read from {base}: {error}"]
    else:
        # A new file has no earlier version, so it is compared with nothing.
        base_source = ""
    base_text, base_lines = canonical(base_source)
    work_text, work_lines = canonical(work_source)
    if base_text == work_text:
        return []
    index = first_difference(base_text, work_text)
    work_line = line_at(work_lines, index)
    base_line = line_at(base_lines, index)
    return [
        f"{relative}: differs from {base}: worktree line {work_line}, "
        f"base line {base_line}",
        f"    worktree: {snippet(work_text, index)}",
        f"    base:     {snippet(base_text, index)}",
    ]


# -- main ------------------------------------------------------------------


def fail(message: str) -> int:
    """Report one reason the proof could not run or did not hold."""
    print(f"comment-only: error: {message}", file=sys.stderr)
    return 1


def main() -> int:
    repository = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    parser = argparse.ArgumentParser(
        prog="check-comment-only.py",
        description="Prove that changed Rust files only changed comments.",
    )
    parser.add_argument(
        "--root",
        default=repository,
        help="repository root (default: this repository)",
    )
    parser.add_argument(
        "--base",
        default="HEAD",
        help="revision to compare against (default: HEAD)",
    )
    args = parser.parse_args()

    root = os.path.normpath(os.path.abspath(os.path.join(os.getcwd(), args.root)))
    if not os.path.isdir(root):
        return fail(f"--root {args.root}: not a directory")

    try:
        root = git_root(root)
        base = resolve_base(root, args.base)
        files = tracked_changes(root, base)
        known = base_tree(root, base)
        for path in untracked_rust(root):
            if path not in files:
                files.append(path)

        failures: list[str] = []
        for relative in files:
            failures.extend(
                compare_file(root, base, relative in known, relative)
            )
        if failures:
            for problem in failures:
                print(f"comment-only: {problem}", file=sys.stderr)
            return 1

        print(f"comment-only: OK ({len(files)} files)")
        return 0
    except CheckError as error:
        return fail(str(error))
    except (OSError, UnicodeDecodeError) as error:
        return fail(str(error))


if __name__ == "__main__":
    sys.exit(main())
