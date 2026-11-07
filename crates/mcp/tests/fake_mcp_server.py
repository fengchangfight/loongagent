#!/usr/bin/env python3
"""A minimal MCP stdio server used as a test fixture.

Modes:
  normal     — handshake, tools and prompts, correct replies
  no-prompts — advertises no prompts capability (and errors if asked)
  silent     — handshake never answered (the deadline must fire)
  die        — exits immediately (a dead server must be reported, not hung on)
  paginated  — tools/list and prompts/list answer in pages with a nextCursor
  ask-back   — sends a sampling/createMessage request and reports the answer
  notify     — announces tools/list_changed, then lists one extra tool
  many       — twelve tools, enough to be deferred by the size rule
  media      — tools/call returns an image block and structured content
"""
import json
import sys

# How many times tools/list has been answered in "notify" mode: the tool list
# changes after the first answer, so a refresh must actually re-read it.
listed = 0


def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def paged(items, key, params):
    """One item per page: the client has to follow every cursor to see them all."""
    index = int(params.get("cursor") or 0)
    result = {key: [items[index]]}
    if index + 1 < len(items):
        result["nextCursor"] = str(index + 1)
    return result


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "normal"
    if mode == "die":
        return
    answer = None
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
        # A reply to the request this fixture sent (mode ask-back) carries no method.
        if method is None and msg.get("id") == 900:
            error = (msg.get("error") or {}).get("code")
            answer = f"answered {error}"
            continue
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
            params = msg.get("params") or {}
            if mode == "many":
                send({"jsonrpc": "2.0", "id": msg["id"], "result": {"tools": [
                    {"name": f"many_{i}", "description": f"tool number {i}",
                     "inputSchema": {"type": "object", "properties": {"arg": {"type": "string"}}}}
                    for i in range(12)
                ]}})
                continue
            if mode == "notify":
                global listed
                listed += 1
                tools = [
                    {"name": "ping", "description": "round-trip a message",
                     "inputSchema": {"type": "object"}},
                ]
                if listed > 1:
                    # A tool the server added after the first answer.
                    tools.append({"name": "pong_new", "description": "added later",
                                  "inputSchema": {"type": "object"}})
                send({"jsonrpc": "2.0", "id": msg["id"], "result": {"tools": tools}})
                continue
            if mode == "paginated":
                tools = [
                    {"name": "paged_a", "description": "first page", "inputSchema": {"type": "object"}},
                    {"name": "paged_b", "description": "second page", "inputSchema": {"type": "object"}},
                    {"name": "paged_c", "description": "third page", "inputSchema": {"type": "object"}},
                ]
                send({"jsonrpc": "2.0", "id": msg["id"],
                      "result": paged(tools, "tools", params)})
                continue
            send({"jsonrpc": "2.0", "id": msg["id"], "result": {"tools": [
                {"name": "ping", "description": "round-trip a message",
                 "inputSchema": {"type": "object", "properties": {"message": {"type": "string"}},
                                 "required": ["message"]}},
                {"name": "fail", "description": "always fails", "inputSchema": {"type": "object"}},
                {"name": "verdict_deny", "description": "answers deny (a policy service)",
                 "inputSchema": {"type": "object"}},
                {"name": "verdict_ask", "description": "answers ask (a policy service)",
                 "inputSchema": {"type": "object"}},
                {"name": "screenshot", "description": "returns an image and structured data",
                 "inputSchema": {"type": "object"}},
            ]}})
        elif method == "prompts/list":
            params = msg.get("params") or {}
            if mode == "no-prompts":
                send({"jsonrpc": "2.0", "id": msg["id"],
                      "error": {"code": -32601, "message": "prompts are not supported"}})
            elif mode == "paginated":
                prompts = [
                    {"name": "paged_review", "description": "page one", "arguments": []},
                    {"name": "paged_release", "description": "page two", "arguments": []},
                ]
                send({"jsonrpc": "2.0", "id": msg["id"],
                      "result": paged(prompts, "prompts", params)})
                continue
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
            if mode == "notify":
                # A server may say its tool list changed at any time; the client has
                # to notice it without being asked.
                send({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"})
            if mode == "ask-back":
                # A server-initiated request the client does not serve. It must be
                # answered (with an error) rather than silently dropped.
                send({"jsonrpc": "2.0", "id": 900, "method": "sampling/createMessage",
                      "params": {"messages": []}})
                # Give the client a moment to answer before the tool result.
                for line in sys.stdin:
                    line = line.strip()
                    if not line:
                        continue
                    reply = json.loads(line)
                    if reply.get("id") == 900:
                        error = (reply.get("error") or {}).get("code")
                        answer = f"answered {error}"
                    break
            if name == "ping":
                text = "pong: " + str(params.get("arguments", {}).get("message", ""))
                if answer:
                    text += f" ({answer})"
                send({"jsonrpc": "2.0", "id": msg["id"],
                      "result": {"content": [{"type": "text", "text": text}]}})
            elif mode == "media" and name == "screenshot":
                send({"jsonrpc": "2.0", "id": msg["id"], "result": {
                    "content": [{"type": "image", "mimeType": "image/png", "data": "aGVsbG8="}],
                    "structuredContent": {"width": 2, "height": 1}}})
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
