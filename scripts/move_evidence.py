#!/usr/bin/env python3
"""Body-hash evidence for a code move that must not change any behaviour.

A structural move (splitting an oversized Rust file into sibling modules) is
supposed to reorder text, never to change it.  This script extracts every `fn`
and `impl` body from a set of files by brace matching, hashes each body, and
prints one line per body as

    sha256<TAB>name<TAB>path

sorted.  Comparing the sorted multisets of two revisions is the evidence that
the move changed nothing: every body that existed before still exists, with the
same code, whether it stayed in the parent file or left for a new one.

    move_evidence.py bodies (--rev REV | --worktree) --path PATH [--path PATH ...] --min N

Three things the extractor is built to get right, each a way the proof could be
vacuous or wrong:

1.  `--min N` is mandatory and the command exits 1 when fewer than N bodies
    were extracted.  Two empty sets are equal, so without a floor a broken
    extractor reports success by finding nothing.
2.  Braces inside string, char and raw-string literals and inside comments are
    blanked out before brace matching.  Otherwise a body that contains `"}"` or
    `'}'` truncates silently and the comparison compares the wrong text.
3.  `--rev` and `--worktree` read the same bytes through different paths, so
    for an unchanged file they print identical output.  Passing both at once
    runs both and exits 1 if they disagree.

One deliberate normalisation, because a module move cannot be byte-identical:
when code leaves a `mod foo { ... }` for its own file, every line loses the
module's indentation, and rustfmt -- which the gate runs with `--check` -- not
only dedents but also *reflows*: a construct that was split at the old column
can fit on one line at the new one.  A body is therefore hashed over its
sequence of Rust tokens rather than its bytes, so that whitespace (and only
whitespace) between tokens is insignificant.  Everything that can change what
the code means is still hashed: identifiers, punctuation, numbers, the contents
of every string, char and raw-string literal, and every comment (whose own
internal whitespace is collapsed, since rustfmt may reindent a comment but not
reword it).  The one exception inside a literal is the whitespace after a
line-continuation backslash in a string, which Rust itself discards and rustfmt
reindents.  Two bodies hash equal exactly when they are the same tokens with the
same literal values; `let x` and `letx` do not collide, and an edited, added or
dropped token always changes the hash.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import re
import subprocess
import sys

# Characters that can immediately precede an item in real code (as opposed to a
# type position).  `-> impl Trait` and `x: impl Trait` are types, not items, and
# this is what tells them apart: a real `impl` follows `;`, `}`, `{`, `]` (an
# attribute) or `)` or the start of the file.
_ITEM_PRECEDERS = frozenset(";}{])")

_IDENT = re.compile(r"(?:r#)?[A-Za-z_][A-Za-z0-9_]*")


def _blank(chars: list[str], start: int, end: int) -> None:
    """Replace [start, end) with spaces, keeping newlines so offsets survive."""
    for index in range(start, end):
        if chars[index] != "\n":
            chars[index] = " "


def _scan_string(text: str, start: int) -> int:
    """`start` is the opening quote; return the index just past the close."""
    index = start + 1
    length = len(text)
    while index < length:
        char = text[index]
        if char == "\\":
            index += 2
        elif char == '"':
            return index + 1
        else:
            index += 1
    return length


def _scan_char(text: str, start: int) -> int | None:
    """`start` is a quote; return the index just past a char literal, or None.

    None means the quote starts a lifetime (`'a`, `'static`, `'_`), which is
    code, not a literal.
    """
    length = len(text)
    if start + 1 >= length:
        return None
    second = text[start + 1]
    if second == "\\":
        index = start + 2
        if index < length and text[index] == "u" and index + 1 < length and text[index + 1] == "{":
            close = text.find("}", index + 2)
            if close == -1:
                return None
            index = close + 1
        elif index < length and text[index] == "x":
            index += 3
        else:
            index += 1
        if index < length and text[index] == "'":
            return index + 1
        return None
    if second == "'":
        return None
    if start + 2 < length and text[start + 2] == "'":
        return start + 3
    return None


def _scan_raw(text: str, start: int) -> int:
    """`start` is the `r` of a raw string (optionally after `b`/`c`)."""
    index = start + 1
    hashes = 0
    while index < len(text) and text[index] == "#":
        hashes += 1
        index += 1
    if index >= len(text) or text[index] != '"':
        return start + 1  # not actually a raw string; consume the `r` only
    terminator = '"' + "#" * hashes
    close = text.find(terminator, index + 1)
    if close == -1:
        return len(text)
    return close + len(terminator)


def _is_ident_char(char: str) -> bool:
    return char.isalnum() or char == "_"


def _collapse_comment(raw: str) -> str:
    return " ".join(raw.split())


def _canon_escaped_string(raw: str) -> str:
    """Canonicalise a non-raw string/byte-string/C-string token.

    A `\\` immediately before a newline is Rust's line-continuation escape: it
    removes the newline and all leading whitespace on the next line, so rustfmt
    is free to reindent that whitespace without changing the value.  The escape
    and the newline are kept; the insignificant indentation after it is
    collapsed.  A `\\\\` is an escaped backslash and never a continuation, which
    is why this walks the escapes instead of matching a regular expression over
    the raw text.
    """
    quote = raw.index('"')
    out = [raw[: quote + 1]]
    index = quote + 1
    length = len(raw)
    while index < length:
        char = raw[index]
        if char == "\\" and index + 1 < length:
            following = raw[index + 1]
            if following == "\n":
                out.append("\\\n")
                index += 2
                while index < length and raw[index] in " \t\r":
                    index += 1
                continue
            if following == "\r" and index + 2 < length and raw[index + 2] == "\n":
                out.append("\\\n")
                index += 3
                while index < length and raw[index] in " \t\r":
                    index += 1
                continue
            out.append(raw[index:index + 2])
            index += 2
            continue
        out.append(char)
        index += 1
    return "".join(out)


def tokenize(text: str) -> list[str]:
    """Return the body's significant tokens.

    Whitespace between tokens is dropped, so a move that only reindents or
    reflows the code cannot be seen.  Literals and comments are kept as single
    tokens: a literal's bytes (including its internal whitespace) are preserved
    exactly, and a comment is collapsed to single spaces.
    """
    tokens: list[str] = []
    index = 0
    length = len(text)
    while index < length:
        char = text[index]

        if char in " \t\r\n":
            index += 1
            continue

        if char == "/" and index + 1 < length and text[index + 1] == "/":
            end = text.find("\n", index)
            end = length if end == -1 else end
            tokens.append(_collapse_comment(text[index:end]))
            index = end
            continue

        if char == "/" and index + 1 < length and text[index + 1] == "*":
            depth = 1
            cursor = index + 2
            while cursor < length and depth:
                if text.startswith("/*", cursor):
                    depth += 1
                    cursor += 2
                elif text.startswith("*/", cursor):
                    depth -= 1
                    cursor += 2
                else:
                    cursor += 1
            tokens.append(_collapse_comment(text[index:cursor]))
            index = cursor
            continue

        at_prefix = not (index > 0 and _is_ident_char(text[index - 1]))
        if at_prefix and char == "r" and index + 1 < length and text[index + 1] in '#"':
            end = _scan_raw(text, index)
            tokens.append(text[index:end])
            index = end
            continue
        if at_prefix and char in "bc":
            if index + 1 < length and text[index + 1] == "r" and (
                index + 2 < length and text[index + 2] in '#"'
            ):
                end = _scan_raw(text, index + 1)
                tokens.append(text[index:end])
                index = end
                continue
            if index + 1 < length and text[index + 1] == '"':
                end = _scan_string(text, index + 1)
                tokens.append(_canon_escaped_string(text[index:end]))
                index = end
                continue
            if index + 1 < length and text[index + 1] == "'":
                end = _scan_char(text, index + 1)
                if end is not None:
                    tokens.append(text[index:end])
                    index = end
                    continue

        if char == '"':
            end = _scan_string(text, index)
            tokens.append(_canon_escaped_string(text[index:end]))
            index = end
            continue

        if char == "'":
            end = _scan_char(text, index)
            if end is not None:
                tokens.append(text[index:end])
                index = end
                continue
            name = _IDENT.match(text, index + 1)
            if name:
                tokens.append(text[index:name.end()])
                index = name.end()
                continue
            tokens.append(char)
            index += 1
            continue

        name = _IDENT.match(text, index)
        if name:
            tokens.append(name.group(0))
            index = name.end()
            continue

        tokens.append(char)
        index += 1
    return tokens


def _canonical(body: str) -> str:
    return " ".join(tokenize(body))


def sanitize(text: str) -> str:
    """Blank comments and literals, preserving every offset and newline."""
    chars = list(text)
    length = len(text)
    index = 0
    while index < length:
        char = text[index]

        if char == "/" and index + 1 < length and text[index + 1] == "/":
            end = text.find("\n", index)
            end = length if end == -1 else end
            _blank(chars, index, end)
            index = end
            continue

        if char == "/" and index + 1 < length and text[index + 1] == "*":
            depth = 1
            cursor = index + 2
            while cursor < length and depth:
                if text.startswith("/*", cursor):
                    depth += 1
                    cursor += 2
                elif text.startswith("*/", cursor):
                    depth -= 1
                    cursor += 2
                else:
                    cursor += 1
            _blank(chars, index, cursor)
            index = cursor
            continue

        at_prefix = (char in "bcr") and not (
            index > 0 and _is_ident_char(text[index - 1])
        )
        if at_prefix and char == "r" and index + 1 < length and text[index + 1] in '#"':
            end = _scan_raw(text, index)
            _blank(chars, index, end)
            index = end
            continue

        if at_prefix and char in "bc" and index + 1 < length and text[index + 1] == "r":
            if index + 2 < length and text[index + 2] in '#"':
                end = _scan_raw(text, index + 1)
                _blank(chars, index, end)
                index = end
                continue

        if char == '"':
            end = _scan_string(text, index)
            _blank(chars, index, end)
            index = end
            continue

        if char in "bc" and index + 1 < length and text[index + 1] == '"' and at_prefix:
            end = _scan_string(text, index + 1)
            _blank(chars, index, end)
            index = end
            continue

        if char == "'":
            end = _scan_char(text, index)
            if end is not None:
                _blank(chars, index, end)
                index = end
                continue
            index += 1
            continue

        if char == "b" and index + 1 < length and text[index + 1] == "'" and at_prefix:
            end = _scan_char(text, index + 1)
            if end is not None:
                _blank(chars, index, end)
                index = end
                continue

        index += 1
    return "".join(chars)


def _find_open_brace(sanitized: str, start: int) -> int | None:
    """First `{` after an item header, skipping `()`/`[]`; None if `;` ends it.

    Angle brackets are deliberately not tracked: they are ambiguous with `<`
    and `>`, and a const-generic brace in a signature (`impl Foo<{N}>`) would
    then be read as the body.  No such signature exists in the files this
    script runs on; every `{` in the four split targets' signatures sits inside
    a literal that `sanitize` has already blanked.
    """
    parens = 0
    brackets = 0
    index = start
    length = len(sanitized)
    while index < length:
        char = sanitized[index]
        if char == "(":
            parens += 1
        elif char == ")":
            parens -= 1
        elif char == "[":
            brackets += 1
        elif char == "]":
            brackets -= 1
        elif char == "{" and parens == 0 and brackets == 0:
            return index
        elif char == ";" and parens == 0 and brackets == 0:
            return None
        elif char == "}" and parens == 0 and brackets == 0:
            return None
        index += 1
    return None


def _match_brace(sanitized: str, opening: int) -> int | None:
    depth = 0
    for index in range(opening, len(sanitized)):
        char = sanitized[index]
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return index
    return None


def extract_bodies(text: str) -> list[tuple[str, str]]:
    """Return (sha256, name) for every `fn` and `impl` body in `text`."""
    sanitized = sanitize(text)
    bodies: list[tuple[str, str]] = []
    for match in re.finditer(r"\b(fn|impl)\b", sanitized):
        keyword = match.group(1)
        probe = match.start() - 1
        while probe >= 0 and sanitized[probe] in " \t\r\n":
            probe -= 1
        preceding = sanitized[probe] if probe >= 0 else ""
        if keyword == "impl" and preceding and preceding not in _ITEM_PRECEDERS:
            continue

        cursor = match.end()
        if keyword == "fn":
            while cursor < len(sanitized) and sanitized[cursor] in " \t\r\n":
                cursor += 1
            name_match = _IDENT.match(sanitized, cursor)
            if not name_match:
                continue
            name = "fn " + name_match.group(0)
            header_end = name_match.end()
        else:
            header_end = cursor

        opening = _find_open_brace(sanitized, header_end)
        if opening is None:
            continue
        closing = _match_brace(sanitized, opening)
        if closing is None:
            continue

        if keyword == "impl":
            header = " ".join(text[match.end():opening].split())
            name = "impl " + header

        body = text[opening + 1:closing]
        digest = hashlib.sha256(_canonical(body).encode("utf-8")).hexdigest()
        bodies.append((digest, name))
    return bodies


def _repo_root() -> str:
    out = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True,
        text=True,
        check=False,
    )
    if out.returncode == 0:
        return out.stdout.strip()
    return os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def _read_source(root: str, rev: str | None, path: str) -> str:
    if rev is None:
        with open(os.path.join(root, path), "r", encoding="utf-8") as handle:
            return handle.read()
    out = subprocess.run(
        ["git", "show", f"{rev}:{path}"],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    if out.returncode != 0:
        raise SystemExit(
            f"move_evidence: cannot read {path} at {rev}: {out.stderr.strip()}"
        )
    return out.stdout


def _collect(root: str, rev: str | None, paths: list[str], minimum: int) -> list[str]:
    lines: list[str] = []
    for path in paths:
        text = _read_source(root, rev, path)
        for digest, name in extract_bodies(text):
            lines.append(f"{digest}\t{name}\t{path}")
    if len(lines) < minimum:
        source = f"--rev {rev}" if rev is not None else "--worktree"
        raise SystemExit(
            f"move_evidence: extracted {len(lines)} bodies from {len(paths)} "
            f"path(s) ({source}), fewer than --min {minimum}"
        )
    lines.sort()
    return lines


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="move_evidence.py",
        description="Hash every fn/impl body so a move can be shown to change nothing.",
    )
    subparsers = parser.add_subparsers(dest="command", required=True)

    bodies = subparsers.add_parser("bodies", help="print one line per fn/impl body")
    bodies.add_argument("--rev", metavar="REV", help="read the files from a git revision")
    bodies.add_argument("--worktree", action="store_true", help="read the files from disk")
    # `--rev` and `--worktree` are alternatives, but may be given together to
    # check the two agree (they must, for an unchanged file).
    bodies.add_argument(
        "--path",
        metavar="PATH",
        action="append",
        required=True,
        help="a file to extract bodies from (repeatable); paths are repo-relative",
    )
    bodies.add_argument(
        "--min",
        metavar="N",
        type=int,
        required=True,
        help="fail unless at least N bodies were extracted",
    )
    args = parser.parse_args(argv)

    root = _repo_root()
    if args.command == "bodies":
        if args.min < 1:
            parser.error("--min must be at least 1")
        if args.rev is None and not args.worktree:
            parser.error("one of --rev or --worktree is required")
        if args.rev is not None and args.worktree:
            rev_lines = _collect(root, args.rev, args.path, args.min)
            worktree_lines = _collect(root, None, args.path, args.min)
            if rev_lines != worktree_lines:
                only_rev = [line for line in rev_lines if line not in set(worktree_lines)]
                only_worktree = [
                    line for line in worktree_lines if line not in set(rev_lines)
                ]
                sys.stderr.write(
                    "move_evidence: --rev and --worktree disagree for "
                    f"{', '.join(args.path)}\n"
                )
                for line in only_rev[:10]:
                    sys.stderr.write(f"  only in {args.rev}: {line}\n")
                for line in only_worktree[:10]:
                    sys.stderr.write(f"  only in worktree: {line}\n")
                return 1
            lines = worktree_lines
        elif args.worktree:
            lines = _collect(root, None, args.path, args.min)
        else:
            lines = _collect(root, args.rev, args.path, args.min)
        for line in lines:
            print(line)
        return 0
    return 2


if __name__ == "__main__":
    sys.exit(main())
