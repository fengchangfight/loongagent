#!/usr/bin/env python3
"""A minimal MCP stdio server used as a test fixture.

Modes:
  normal  — handshake, tools and prompts, correct replies
  no-prompts — advertises no prompts capability (and errors if asked)
  silent  — handshake never answered (the deadline must fire)
  die     — exits immediately (a dead server must be reported, not hung on)
"""
import json
import sys


def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "normal"
    if mode == "die":
        return
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if mode == "silent":
            continue
        method = msg.get("method")
        if method == "initialize":
            capabilities = {"tools": {}}
            if mode != "no-prompts":
                capabilities["prompts"] = {}
            send({"jsonrpc": "2.0", "id": msg["id"], "result": {
                "protocolVersion": "2025-06-18",
                "capabilities": capabilities,
                "serverInfo": {"name": "fake", "version": "0.0.1"},
            }})
        elif method == "notifications/initialized":
            pass  # a notification: no reply
        elif method == "tools/list":
            send({"jsonrpc": "2.0", "id": msg["id"], "result": {"tools": [
                {"name": "ping", "description": "round-trip a message",
                 "inputSchema": {"type": "object", "properties": {"message": {"type": "string"}},
                                 "required": ["message"]}},
                {"name": "fail", "description": "always fails", "inputSchema": {"type": "object"}},
                {"name": "verdict_deny", "description": "answers deny (a policy service)",
                 "inputSchema": {"type": "object"}},
                {"name": "verdict_ask", "description": "answers ask (a policy service)",
                 "inputSchema": {"type": "object"}},
            ]}})
        elif method == "prompts/list":
            if mode == "no-prompts":
                send({"jsonrpc": "2.0", "id": msg["id"],
                      "error": {"code": -32601, "message": "prompts are not supported"}})
            else:
                send({"jsonrpc": "2.0", "id": msg["id"], "result": {"prompts": [
                    {"name": "review", "description": "Review a diff and report only real bugs",
                     "arguments": [{"name": "ref", "description": "the git ref to review",
                                    "required": True}]},
                    {"name": "release", "description": "Walk the release checklist",
                     "arguments": []},
                    {"name": "broken", "description": "Advertised, but fails to load",
                     "arguments": []},
                ]}})
        elif method == "prompts/get":
            params = msg.get("params", {})
            name = params.get("name")
            arguments = params.get("arguments") or {}
            if name == "review":
                ref = arguments.get("ref", "HEAD")
                messages = [{"role": "user", "content": {"type": "text",
                             "text": f"Review {ref} and report ONLY real bugs, each with file:line."}}]
            elif name == "broken":
                send({"jsonrpc": "2.0", "id": msg["id"],
                      "error": {"code": -32603, "message": "the prompt handler is broken"}})
                continue
            elif name == "release":
                messages = [{"role": "user", "content": {"type": "text", "text": "Walk the release checklist."}},
                            {"role": "assistant", "content": {"type": "text", "text": "Understood."}}]
            else:
                send({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32602, "message": "unknown prompt"}})
                continue
            send({"jsonrpc": "2.0", "id": msg["id"],
                  "result": {"description": "from the fake server", "messages": messages}})
        elif method == "tools/call":
            params = msg.get("params", {})
            name = params.get("name")
            if name == "ping":
                text = "pong: " + str(params.get("arguments", {}).get("message", ""))
                send({"jsonrpc": "2.0", "id": msg["id"],
                      "result": {"content": [{"type": "text", "text": text}]}})
            elif name in ("verdict_deny", "verdict_ask"):
                verdict = "deny" if name == "verdict_deny" else "ask"
                text = json.dumps({"verdict": verdict, "reason": f"policy service says {verdict}"})
                send({"jsonrpc": "2.0", "id": msg["id"],
                      "result": {"content": [{"type": "text", "text": text}]}})
            elif name == "fail":
                send({"jsonrpc": "2.0", "id": msg["id"], "result": {
                    "isError": True,
                    "content": [{"type": "text", "text": "this tool always fails"}]}})
            else:
                send({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32602, "message": "unknown tool"}})
        elif msg.get("id") is not None:
            send({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32601, "message": "method not found"}})


if __name__ == "__main__":
    main()
