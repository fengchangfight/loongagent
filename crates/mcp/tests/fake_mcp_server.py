#!/usr/bin/env python3
"""A minimal MCP stdio server used as a test fixture.

Modes:
  normal  — handshake, two tools (ping, fail), correct replies
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
            send({"jsonrpc": "2.0", "id": msg["id"], "result": {
                "protocolVersion": "2025-06-18",
                "capabilities": {"tools": {}},
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
