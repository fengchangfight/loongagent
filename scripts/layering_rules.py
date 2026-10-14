#!/usr/bin/env python3
"""Dependency-direction rules for the loongagent workspace (ADR-0004).

Allowed internal edges. Anything not listed is a violation, and a crate that
depends on something *above* it in the stack is the violation we care about most:
the kernel must never depend on the server, the schema must never depend on
anything, and a client must never reach back into the kernel.
"""
import json
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
        "loong-sandbox", "loong-tools", "loong-hooks", "loong-mcp", "loong-skills", "loong-config", "loong-index", "loong-context", "loong-kernel", "loong-server", "loong-tui", "loong-tui-app", "loong-web", "loong-control", "loong-pack",
    },
}


def main() -> int:
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
