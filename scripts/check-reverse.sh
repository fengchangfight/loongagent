#!/usr/bin/env bash
# Prove the vendored reverse surface still is the surface `docs/design/reverse.md` describes.
#
# Two checks, smallest first:
#   1. the MCP server answers `initialize` and lists the tools the design names, and none of the
#      bridges this build drops;
#   2. a real call works end to end on a file that is always present (`/bin/true`, or the running
#      Python's own binary when even that is missing), producing an ELF/dyld/PE record with a
#      subject, a summary and parsed sections.
#
# What this proves: the surface builds, lists what `docs/design/reverse.md` says, and carries a
# real result over stdio. What it does not prove: anything about a target it did not open. The
# behaviour tests are the workspace run in `scripts/check.sh` (which includes the vendored
# crates); this script is the transport-shaped half, and runs before them so a broken handshake
# fails fast. CI runs `scripts/check.sh`.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

CARGO="${CARGO:-cargo}"
TARGET="${CARGO_TARGET_DIR:-target}"
BIN="$TARGET/debug/loongagent-reverse"

echo "check-reverse: build the server"
"$CARGO" build --quiet -p loong-reverse-mcp --bin loongagent-reverse

echo "check-reverse: stdio handshake and tool list"
workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT
python3 - "$BIN" "$workdir" <<'PY'
import json
import os
import subprocess
import sys

binary, workdir = sys.argv[1], sys.argv[2]
server = subprocess.Popen(
    [binary, "--project", os.path.join(workdir, "project"), "--all-tools"],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    text=True,
    bufsize=1,
)
sent = 0


def call(method, params):
    """Send one request and return its response; the server answers concurrently, so match ids."""
    global sent
    sent += 1
    server.stdin.write(json.dumps({"jsonrpc": "2.0", "id": sent, "method": method, "params": params}) + "\n")
    server.stdin.flush()
    while True:
        line = server.stdout.readline()
        if not line:
            raise SystemExit(f"check-reverse: the server closed the stream during {method}\n{server.stderr.read()}")
        reply = json.loads(line)
        if reply.get("id") == sent:
            return reply


hello = call(
    "initialize",
    {
        "protocolVersion": "2025-06-18",
        "capabilities": {},
        "clientInfo": {"name": "check-reverse", "version": "0"},
    },
)["result"]
assert hello["serverInfo"]["name"] == "loongagent-reverse", hello["serverInfo"]

tools = call("tools/list", {})["result"]["tools"]
names = {tool["name"] for tool in tools}
required = {
    "core_target_open",
    "core_target_close",
    "native_functions_list",
    "native_function_disassemble",
    "native_function_pseudocode",
    "native_strings_browse",
    "web_site_open",
    "js_source_locate",
    "android_package_show",
    "core_archive_show",
    "core_report_write",
}
missing = required - names
assert not missing, f"check-reverse: the server does not list {sorted(missing)}"
dropped = [
    name
    for name in names
    if any(part in name for part in ("_jeb_", "engines_", "game_", "electron_", "_frida_", "rizin_", "cutter_", "gdb_"))
]
assert not dropped, f"check-reverse: bridges this build drops are listed: {dropped}"

target = "/bin/true"
if not os.path.isfile(target):
    target = sys.executable
opened = call("tools/call", {"name": "core_target_open", "arguments": {"path": target}})
assert "error" not in opened, opened
listed = call("tools/call", {"name": "native_sections_list", "arguments": {}})
assert "error" not in listed, listed
record = listed["result"]["structuredContent"]
assert record["operation"] == "native_sections_list", record["operation"]
assert record["subject"]["kind"] in {"elf", "macho", "pe", "unknown"}, record["subject"]
assert record["result"].get("sections") or record["result"].get("segments"), "nothing parsed"
assert record["summary"], "an evidence record must summarise itself"
print(f"check-reverse: {len(names)} tools, opened {target}, sections listed")
server.stdin.close()
server.wait(timeout=30)
PY

echo "check-reverse: OK"
