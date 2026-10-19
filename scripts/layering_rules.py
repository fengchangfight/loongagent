#!/usr/bin/env python3
"""Dependency-direction rules for the loongagent workspace (ADR-0004).

Allowed internal edges. Anything not listed is a violation, and a crate that
depends on something *above* it in the stack is the violation we care about most:
the kernel must never depend on the server, the schema must never depend on
anything, and a client must never reach back into the kernel.

With stdin it validates cargo metadata (what scripts/check-layering.sh pipes in).
With --emit-map it prints docs/crate-map.md instead, so the human-readable map is
a product of these same rules rather than a copy of them that can rot.
"""
import argparse
import json
import os
import sys

ALLOWED = {
    # the protocol's single source of truth depends on nothing of ours
    "loong-schema": set(),
    # the signed envelope, shared by the local agent and the control plane
    # (ADR-0012 §1: the two deploy independently, so this code cannot be copied)
    "loong-pack": set(),
    # pack distribution: reads the envelope, speaks HTTP to the control plane
    "loong-control": {"loong-pack", "loong-http", "loong-config"},
    # edges (pluggable, language-neutral, no kernel knowledge)
    "loong-protocol": {"loong-schema"},
    "loong-eventlog": {"loong-schema"},
    "loong-http": set(),
    "loong-proc": set(),
    "loong-manifest": set(),
    "loong-providers": {"loong-schema", "loong-http"},
    "loong-sandbox": {"loong-schema"},
    "loong-tools": {"loong-schema", "loong-proc"},
    "loong-hooks": {"loong-schema", "loong-http", "loong-proc", "loong-manifest"},
    "loong-mcp": {"loong-schema", "loong-protocol", "loong-tools", "loong-manifest"},
    "loong-skills": {"loong-schema", "loong-tools"},
    "loong-config": {"loong-schema"},
    # plugins are content bundles: they install a repository's skills and read the
    # paths configuration defines, but know nothing about the kernel (ADR-0018)
    "loong-plugin": {"loong-config", "loong-skills"},
    # the derived index reads the log; it is disposable and rebuildable
    "loong-index": {"loong-schema", "loong-eventlog"},
    # the layered context memory is derived from the log too, and is equally
    # disposable; it must never learn about the kernel (ADR-0017)
    "loong-context": {"loong-schema", "loong-eventlog", "loong-http"},
    # the fixed kernel calls its edges through traits; it never knows adapters
    "loong-kernel": {"loong-schema", "loong-eventlog", "loong-index", "loong-context", "loong-providers", "loong-sandbox", "loong-tools", "loong-hooks", "loong-mcp", "loong-skills", "loong-config"},
    # surfaces
    "loong-server": {"loong-schema", "loong-protocol", "loong-eventlog", "loong-kernel"},
    "loong-tui": {"loong-schema", "loong-protocol"},
    "loong-tui-app": {"loong-schema", "loong-tui"},
    "loong-web": set(),
    # the one binary that ties faces together
    "loongagent": {
        "loong-schema", "loong-protocol", "loong-eventlog", "loong-http", "loong-proc", "loong-manifest", "loong-providers",
        "loong-sandbox", "loong-tools", "loong-hooks", "loong-mcp", "loong-skills", "loong-plugin", "loong-config", "loong-index", "loong-context", "loong-kernel", "loong-server", "loong-tui", "loong-tui-app", "loong-web", "loong-control", "loong-pack",
    },
}


# The package whose crate directory is not its own name minus the `loong-` prefix.
# Every other package follows the mechanical rule.
DIRECTORY_EXCEPTIONS = {"loongagent": "cli"}


def crate_directory(name: str) -> str:
    """The directory under crates/ that holds this package."""
    if name in DIRECTORY_EXCEPTIONS:
        return DIRECTORY_EXCEPTIONS[name]
    if not name.startswith("loong-"):
        raise SystemExit(
            f"layering_rules.py: {name}: no loong- prefix and no directory in "
            "DIRECTORY_EXCEPTIONS, so its header cannot be found"
        )
    return name[len("loong-") :]


def header_line(root: str, name: str) -> str:
    """The first line of the crate's `//!` module header, marker removed.

    The header is the narrative the hand-written crate table used to carry, so a
    crate without one is a hard failure: an empty cell would silently drop it.
    """
    source = "main.rs" if name in DIRECTORY_EXCEPTIONS else "lib.rs"
    path = os.path.join(root, "crates", crate_directory(name), "src", source)
    try:
        with open(path, encoding="utf-8") as handle:
            for line in handle:
                if line.startswith("//!"):
                    text = line[3:].strip()
                    if text:
                        # A module contract opens with `Purpose:`; the map
                        # carries the sentence, not the field label.
                        for label in ("Purpose:", "**Purpose:**"):
                            if text.startswith(label):
                                text = text[len(label) :].strip()
                                break
                        if text:
                            return text
                    break
    except OSError as error:
        raise SystemExit(
            f"layering_rules.py: {name}: cannot read {path}: {error.strerror or error}"
        )
    raise SystemExit(
        f"layering_rules.py: {name}: {path} has no //! header line, so the crate "
        "map would carry a blank cell"
    )


def emit_map(root: str) -> int:
    """Print docs/crate-map.md to stdout: one line per crate, byte-stable.

    Sorted by crate name and by edge, written as UTF-8 bytes, ending in exactly
    one newline, so check-docs.py can regenerate and compare it verbatim.
    """
    lines = [
        "# Crate map",
        "",
        "Generated from the `ALLOWED` edges in `scripts/layering_rules.py`, which",
        "`scripts/check-layering.sh` enforces in CI. Regenerate it with:",
        "",
        "```",
        "python3 scripts/layering_rules.py --emit-map . > docs/crate-map.md",
        "```",
        "",
        "Dependencies may only point **down**: the schema depends on nothing, the",
        "kernel never depends on a surface, and a client never reaches back into the",
        "kernel. The edges below are every internal dependency allowed; anything not",
        "listed is a violation. The middle column is each crate's own `//!` header.",
        "",
        "| Crate | What it is | Allowed dependencies |",
        "|---|---|---|",
    ]
    for name in sorted(ALLOWED):
        edges = ALLOWED[name]
        allowed = ", ".join(f"`{edge}`" for edge in sorted(edges)) if edges else "—"
        lines.append(f"| `{name}` | {header_line(root, name)} | {allowed} |")
    lines.append("")
    sys.stdout.buffer.write("\n".join(lines).encode("utf-8"))
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(
        prog="layering_rules.py",
        description="Check the crate dependency direction (reads cargo metadata on stdin), "
        "or emit the generated crate map.",
    )
    parser.add_argument(
        "--emit-map",
        metavar="ROOT",
        default=None,
        help="print docs/crate-map.md for the workspace rooted at ROOT, instead of "
        "reading metadata on stdin",
    )
    args = parser.parse_args()
    if args.emit_map is not None:
        return emit_map(args.emit_map)

    meta = json.load(sys.stdin)
    members = {p["name"]: p for p in meta["packages"]}
    problems = []
    for name, pkg in sorted(members.items()):
        if name not in ALLOWED:
            problems.append(f"{name}: not classified in scripts/layering_rules.py")
            continue
        # Only normal dependencies are governed: a dev-dependency is a test
        # affordance (a kernel test may reach for a scripted model), not part of
        # the shipped dependency direction.
        internal = {
            d["name"]
            for d in pkg.get("dependencies", [])
            if d["name"] in members and d.get("kind") in (None, "normal")
        }
        extra = internal - ALLOWED[name]
        for dep in sorted(extra):
            problems.append(
                f"{name} -> {dep}: not allowed (see scripts/layering_rules.py)"
            )
    if problems:
        print("layering check FAILED:", file=sys.stderr)
        for p in problems:
            print(f"  - {p}", file=sys.stderr)
        return 1
    print(f"layering check OK ({len(members)} crates, edges point down only)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
