#!/usr/bin/env python3
"""Item-hash evidence for a code move that must not change any behaviour.

A structural move (splitting an oversized Rust file into sibling modules) is
supposed to reorder text, never to change it.  This script extracts every item
-- `fn`, `impl`, `struct`, `enum`, `union`, `trait`, `type`, `const`, `static`,
`mod`, `use` and `macro_rules!` -- from a set of files by brace matching, hashes
each item's entire canonical token stream (attributes, visibility, signature and
body together), and prints one line per item as

    sha256<TAB>kind<TAB>name<TAB>vis<TAB>path

sorted.  Comparing the sorted multisets of two revisions is the evidence that
the move changed nothing: every item that existed before still exists, with the
same code, whether it stayed in the parent file or left for a new one.

    move_evidence.py items (--rev REV | --worktree) --path PATH [--path PATH ...] --min N

The second subcommand asserts the other half of the standard -- that the only
byte difference a split may introduce is rustfmt's own reflow of the moved
block:

    move_evidence.py reflow --rev REV --from-path OLD --start A --end B \
        --dedent N --to-path NEW [--edition ED]

It takes lines A-B of OLD at REV, removes the N columns of indentation they had
where they lived, formats the result with rustfmt, and requires the output to be
byte-for-byte what NEW contains.  If it is, then every difference between the
original block and the committed file is rustfmt's output, which is the whole of
what this standard accepts.

Hashing whole items rather than bodies is the point.  A moved signature (a
parameter renamed or retyped, a `&mut` added), a dropped `pub`, a lost
`#[test]`/`#[cfg]`, or an entire `struct` can change and still compile, pass
every test and leave the manifest untouched, while every function body stays
byte-identical.  The `kind` and `vis` columns are printed as well as hashed, so
a visibility or kind change is visible in the comparison instead of merely
implied by it.

Three things the extractor is built to get right, each a way the proof could be
vacuous or wrong:

1.  `--min N` is mandatory and the command exits 1 when fewer than N items were
    extracted.  Two empty sets are equal, so without a floor a broken extractor
    reports success by finding nothing.
2.  Braces inside string, char and raw-string literals and inside comments are
    blanked out before brace matching.  Otherwise an item that contains `"}"` or
    `'}'` truncates silently and the comparison compares the wrong text.
3.  `--rev` and `--worktree` read the same bytes through different paths, so
    for an unchanged file they print identical output.  Passing both at once
    runs both and exits 1 if they disagree.

Two deliberate normalisations, because a module move cannot be byte-identical:
when code leaves a `mod foo { ... }` for its own file, every line loses the
module's indentation, and rustfmt -- which the gate runs with `--check` -- not
only dedents but also *reflows*: a construct that was split at the old column
can fit on one line at the new one.  An item is therefore hashed over its
sequence of Rust tokens rather than its bytes, so inter-token whitespace is
insignificant; and a trailing comma immediately before a closing delimiter is
dropped, because Rust's grammar makes it optional and rustfmt adds and removes
it.  A trailing comma inside a macro token tree is *not* dropped, because a
macro can observe it.  The one exception inside a literal is the whitespace
after a line-continuation backslash in a string, which Rust itself discards and
rustfmt reindents.  Everything that can change what the code means is still
hashed: identifiers, punctuation, numbers, the contents of every string, char
and raw-string literal, and every comment inside the item (whose own internal
whitespace is collapsed, since rustfmt may reindent a comment but not reword
it).  Comments *before* an item are not part of it; only `#[...]` attributes
are.

`mod` is the one kind hashed without its body: a module's body is a list of
items that appear in the multiset in their own right, so an inline
`mod tests { ... }` becoming `mod tests;` plus a file would otherwise compare
the module's body against nothing.  The declaration -- attributes, visibility,
name -- is still hashed, so `mod` becoming `pub mod` or losing `#[cfg(test)]`
is still caught.
"""

from __future__ import annotations

import argparse
import difflib
import hashlib
import os
import re
import subprocess
import sys
import tempfile

# Every item kind this script knows, and the keywords that introduce them.
_ITEM_KEYWORDS = (
    "fn",
    "impl",
    "struct",
    "enum",
    "union",
    "trait",
    "type",
    "const",
    "static",
    "mod",
    "use",
    "macro_rules",
)
_KEYWORD_RE = re.compile(r"\b(" + "|".join(_ITEM_KEYWORDS) + r")\b")

# Keywords that can precede an item without being one: `pub unsafe fn`,
# `pub(crate) async fn`, `pub const fn`, `pub extern "C" fn`.
_MODIFIERS = frozenset(("pub", "unsafe", "async", "const", "extern", "default"))

# A real item follows one of these, or the start of the file.  `-> impl Trait`
# and `x: impl Trait` follow `>`/`:`/`<`/`,`/`(` instead, which is what keeps
# type-position keywords from being read as items.
_ITEM_PRECEDERS = frozenset(";}{]")

# A trailing comma before one of these is optional in Rust's grammar.
_TRAILING_CLOSERS = frozenset(")]}>")

_IDENT = re.compile(r"(?:r#)?[A-Za-z_][A-Za-z0-9_]*")
_PUB = re.compile(r"\bpub\b\s*(\(([^)]*)\))?")

_DELIMITER_PAIRS = {"(": ")", "[": "]", "{": "}"}


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


def _skip_ws(text: str, start: int) -> int:
    while start < len(text) and text[start] in " \t\r\n":
        start += 1
    return start


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
    """Return the text's significant tokens.

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


def _canonical(text: str) -> str:
    """The item's canonical token stream (see the module docstring)."""
    tokens = tokenize(text)
    kept: list[str] = []
    macro_groups: list[bool] = []
    macro_depth = 0
    for index, token in enumerate(tokens):
        inside_macro = macro_depth > 0
        if token in "([{":
            opens_macro = False
            if not inside_macro:
                if kept and kept[-1] == "!":
                    opens_macro = True
                elif len(kept) >= 3 and kept[-2] == "!" and kept[-3] == "macro_rules":
                    opens_macro = True
            entry = opens_macro or inside_macro
            macro_groups.append(entry)
            if entry:
                macro_depth += 1
            kept.append(token)
            continue
        if token in ")]}":
            if macro_groups and macro_groups.pop():
                macro_depth -= 1
            kept.append(token)
            continue
        if (
            token == ","
            and not inside_macro
            and index + 1 < len(tokens)
            and tokens[index + 1] in _TRAILING_CLOSERS
        ):
            continue
        kept.append(token)
    return " ".join(kept)


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


def _head_start(sanitized: str, keyword_start: int) -> int:
    """Walk left over the item's visibility and modifiers."""
    position = keyword_start
    while True:
        probe = position
        while probe > 0 and sanitized[probe - 1] in " \t\r\n":
            probe -= 1
        if probe == 0:
            return position
        char = sanitized[probe - 1]

        if char == ")":
            depth = 1
            cursor = probe - 1
            while cursor > 0 and depth:
                cursor -= 1
                if sanitized[cursor] == ")":
                    depth += 1
                elif sanitized[cursor] == "(":
                    depth -= 1
            if cursor > 0:
                word = cursor
                while word > 0 and (
                    sanitized[word - 1].isalnum() or sanitized[word - 1] == "_"
                ):
                    word -= 1
                if sanitized[word:cursor] == "pub":
                    position = word
                    continue
            return position

        if char.isalnum() or char == "_":
            word = probe
            while word > 0 and (
                sanitized[word - 1].isalnum() or sanitized[word - 1] == "_"
            ):
                word -= 1
            if sanitized[word:probe] in _MODIFIERS:
                position = word
                continue
        return position


def _attribute_start(sanitized: str, head_start: int) -> int:
    """Walk left over the `#[...]` attributes that belong to the item."""
    position = head_start
    while True:
        probe = position
        while probe > 0 and sanitized[probe - 1] in " \t\r\n":
            probe -= 1
        if probe == 0 or sanitized[probe - 1] != "]":
            return position
        depth = 1
        cursor = probe - 1
        while cursor > 0 and depth:
            cursor -= 1
            if sanitized[cursor] == "]":
                depth += 1
            elif sanitized[cursor] == "[":
                depth -= 1
        if cursor > 0 and sanitized[cursor - 1] == "#":
            position = cursor - 1
            continue
        return position


def _at_item_position(sanitized: str, keyword_start: int) -> bool:
    head = _head_start(sanitized, keyword_start)
    probe = head
    while probe > 0 and sanitized[probe - 1] in " \t\r\n":
        probe -= 1
    if probe == 0:
        return True
    return sanitized[probe - 1] in _ITEM_PRECEDERS


def _visibility(segment: str) -> str:
    match = _PUB.search(segment)
    if not match:
        return "private"
    if match.group(1):
        return "pub(" + " ".join(match.group(2).split()) + ")"
    return "pub"


def _find_body(sanitized: str, start: int) -> tuple[str | None, int | None]:
    """Find `{` or `;` at depth zero after an item header."""
    parens = 0
    brackets = 0
    index = start
    while index < len(sanitized):
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
            return ("brace", index)
        elif char == ";" and parens == 0 and brackets == 0:
            return ("semi", index)
        elif char == "}" and parens == 0 and brackets == 0:
            return (None, None)
        index += 1
    return (None, None)


def _find_semicolon(sanitized: str, start: int) -> int | None:
    """Find the `;` that ends a `type`/`const`/`static`/`use`, at depth zero."""
    parens = 0
    brackets = 0
    braces = 0
    index = start
    while index < len(sanitized):
        char = sanitized[index]
        if char == "(":
            parens += 1
        elif char == ")":
            parens -= 1
        elif char == "[":
            brackets += 1
        elif char == "]":
            brackets -= 1
        elif char == "{":
            braces += 1
        elif char == "}":
            braces -= 1
        elif char == ";" and parens == 0 and brackets == 0 and braces == 0:
            return index
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


def _match_delimiter(sanitized: str, opening: int) -> int | None:
    opener = sanitized[opening]
    closer = _DELIMITER_PAIRS[opener]
    depth = 0
    for index in range(opening, len(sanitized)):
        char = sanitized[index]
        if char == opener:
            depth += 1
        elif char == closer:
            depth -= 1
            if depth == 0:
                return index
    return None


def extract_items(text: str) -> list[tuple[str, str, str, str]]:
    """Return (sha256, kind, name, vis) for every item in `text`."""
    sanitized = sanitize(text)
    items: list[tuple[str, str, str, str]] = []
    for match in _KEYWORD_RE.finditer(sanitized):
        keyword = match.group(1)
        if not _at_item_position(sanitized, match.start()):
            continue
        head = _head_start(sanitized, match.start())
        item_start = _attribute_start(sanitized, head)
        visibility = _visibility(text[head:match.start()])
        after = _skip_ws(sanitized, match.end())

        if keyword == "macro_rules":
            if after >= len(sanitized) or sanitized[after] != "!":
                continue
            name_match = _IDENT.match(sanitized, _skip_ws(sanitized, after + 1))
            if not name_match:
                continue
            name = name_match.group(0)
            opener = _skip_ws(sanitized, name_match.end())
            if opener >= len(sanitized) or sanitized[opener] not in "([{":
                continue
            closer = _match_delimiter(sanitized, opener)
            if closer is None:
                continue
            end = closer + 1
            kind = "macro_rules"

        elif keyword == "impl":
            found, body = _find_body(sanitized, match.end())
            if found != "brace" or body is None:
                continue
            closer = _match_brace(sanitized, body)
            if closer is None:
                continue
            name = " ".join(tokenize(text[match.end():body]))
            end = closer + 1
            kind = "impl"

        elif keyword in ("type", "const", "static", "use"):
            if keyword == "const":
                # `const fn`, `const unsafe fn`, `const extern fn`: a modifier,
                # not a const item.  A const item's name is an identifier.
                name_match = _IDENT.match(sanitized, after)
                if name_match and name_match.group(0) in ("fn", "unsafe", "async", "extern", "move"):
                    continue
            semicolon = _find_semicolon(sanitized, match.end())
            if semicolon is None:
                continue
            if keyword == "use":
                name = " ".join(tokenize(text[match.end():semicolon]))
            else:
                name_match = _IDENT.match(sanitized, after)
                if not name_match:
                    continue
                name = name_match.group(0)
            end = semicolon + 1
            kind = keyword

        else:  # fn, struct, enum, union, trait, mod
            found, body = _find_body(sanitized, match.end())
            if found is None or body is None:
                continue
            name_match = _IDENT.match(sanitized, after)
            if not name_match:
                continue
            name = name_match.group(0)
            if keyword == "mod":
                # The declaration only.  The module's body is a list of items
                # that are in the multiset individually; an inline module
                # becoming a file must not compare that body against nothing.
                end = body
            elif found == "brace":
                closer = _match_brace(sanitized, body)
                if closer is None:
                    continue
                end = closer + 1
            else:
                end = body + 1
            kind = keyword

        digest = hashlib.sha256(
            _canonical(text[item_start:end]).encode("utf-8")
        ).hexdigest()
        items.append((digest, kind, name, visibility))
    return items


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
        for digest, kind, name, visibility in extract_items(text):
            lines.append(f"{digest}\t{kind}\t{name}\t{visibility}\t{path}")
    if len(lines) < minimum:
        source = f"--rev {rev}" if rev is not None else "--worktree"
        raise SystemExit(
            f"move_evidence: extracted {len(lines)} items from {len(paths)} "
            f"path(s) ({source}), fewer than --min {minimum}"
        )
    lines.sort()
    return lines


def _dedent(lines: list[str], amount: int) -> list[str]:
    """Remove the indentation a block had where it used to live.

    Only the uniform shift a move causes is removed: a line that has the
    indentation loses it, a blank line becomes empty, and anything else (a
    multi-line literal whose content starts at column zero) is left alone,
    because rustfmt does not reindent literal contents.
    """
    if amount <= 0:
        return ["" if line.strip() == "" else line for line in lines]
    prefix = " " * amount
    out = []
    for line in lines:
        if line.startswith(prefix):
            out.append(line[amount:])
        elif line.strip() == "":
            out.append("")
        else:
            out.append(line)
    return out


def _reflow(
    root: str,
    rev: str,
    from_path: str,
    start: int,
    end: int,
    amount: int,
    to_path: str,
    edition: str,
) -> int:
    """Assert `rustfmt(dedent(old block))` equals the committed new file.

    This is the whole of what a split is allowed to change: if the new file is
    exactly what rustfmt makes of the old block with its old indentation
    removed, then every difference between the two is rustfmt's own reflow.
    """
    old_lines = _read_source(root, rev, from_path).split("\n")
    if not 1 <= start <= end <= len(old_lines):
        sys.stderr.write(
            f"move_evidence: {from_path} at {rev} has {len(old_lines)} lines; "
            f"--start {start} --end {end} is not a range within it\n"
        )
        return 1
    candidate = "\n".join(_dedent(old_lines[start - 1:end], amount)) + "\n"

    handle = tempfile.NamedTemporaryFile(
        "w", suffix=".rs", delete=False, encoding="utf-8"
    )
    try:
        handle.write(candidate)
        handle.close()
        proc = subprocess.run(
            ["rustfmt", "--edition", edition, handle.name],
            capture_output=True,
            text=True,
            check=False,
        )
        if proc.returncode != 0:
            sys.stderr.write("move_evidence: rustfmt failed:\n" + proc.stderr)
            return 1
        with open(handle.name, "r", encoding="utf-8") as formatted_handle:
            formatted = formatted_handle.read()
    finally:
        os.unlink(handle.name)

    committed = _read_source(root, None, to_path)
    if formatted == committed:
        print(
            f"reflow: rustfmt(dedent({from_path}:{start}-{end})) == {to_path}"
        )
        return 0
    sys.stderr.write(
        f"move_evidence: reflow check failed: rustfmt(dedent({from_path}:"
        f"{start}-{end})) is not {to_path}\n"
    )
    diff = difflib.unified_diff(
        formatted.split("\n"),
        committed.split("\n"),
        fromfile=f"rustfmt(dedent({from_path}:{start}-{end}))",
        tofile=to_path,
        lineterm="",
    )
    for line in diff:
        sys.stderr.write(line + "\n")
    return 1


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="move_evidence.py",
        description="Hash every item so a move can be shown to change nothing.",
    )
    subparsers = parser.add_subparsers(dest="command", required=True)

    items = subparsers.add_parser("items", help="print one line per Rust item")
    items.add_argument("--rev", metavar="REV", help="read the files from a git revision")
    items.add_argument("--worktree", action="store_true", help="read the files from disk")
    # `--rev` and `--worktree` are alternatives, but may be given together to
    # check that the two agree (they must, for an unchanged file).
    items.add_argument(
        "--path",
        metavar="PATH",
        action="append",
        required=True,
        help="a file to extract items from (repeatable); paths are repo-relative",
    )
    items.add_argument(
        "--min",
        metavar="N",
        type=int,
        required=True,
        help="fail unless at least N items were extracted",
    )

    reflow = subparsers.add_parser(
        "reflow",
        help="assert rustfmt(dedent(old block)) equals the committed new file",
    )
    reflow.add_argument("--rev", metavar="REV", required=True)
    reflow.add_argument("--from-path", metavar="PATH", required=True)
    reflow.add_argument("--start", metavar="LINE", type=int, required=True,
                        help="first line of the old block (1-based, inclusive)")
    reflow.add_argument("--end", metavar="LINE", type=int, required=True,
                        help="last line of the old block (1-based, inclusive)")
    reflow.add_argument("--dedent", metavar="N", type=int, default=0,
                        help="columns of indentation the block had where it lived")
    reflow.add_argument("--to-path", metavar="PATH", required=True)
    reflow.add_argument("--edition", metavar="ED", default="2021")

    args = parser.parse_args(argv)

    root = _repo_root()
    if args.command == "items":
        if args.min < 1:
            parser.error("--min must be at least 1")
        if args.rev is None and not args.worktree:
            parser.error("one of --rev or --worktree is required")
        if args.rev is not None and args.worktree:
            rev_lines = _collect(root, args.rev, args.path, args.min)
            worktree_lines = _collect(root, None, args.path, args.min)
            if rev_lines != worktree_lines:
                rev_set = set(rev_lines)
                worktree_set = set(worktree_lines)
                sys.stderr.write(
                    "move_evidence: --rev and --worktree disagree for "
                    f"{', '.join(args.path)}\n"
                )
                for line in [item for item in rev_lines if item not in worktree_set][:10]:
                    sys.stderr.write(f"  only in {args.rev}: {line}\n")
                for line in [
                    item for item in worktree_lines if item not in rev_set
                ][:10]:
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
    if args.command == "reflow":
        return _reflow(
            root,
            args.rev,
            args.from_path,
            args.start,
            args.end,
            args.dedent,
            args.to_path,
            args.edition,
        )
    return 2


if __name__ == "__main__":
    sys.exit(main())
