#!/usr/bin/env bash
# A demo that needs no API key: a scripted OpenAI-compatible model plus a real
# session. The model is fake; the tool call, the policy decision, the OS boundary
# and the event log are all real.
#
#   ./scripts/demo.sh          # the web UI
#   ./scripts/demo.sh tui      # the full-screen terminal UI
set -euo pipefail

surface="${1:-web}"
root="$(cd "$(dirname "$0")/.." && pwd)"
bin="${LOONGAGENT_BIN:-$root/target/debug/loongagent}"
work="$(mktemp -d)"
port="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')"

cleanup() { kill "${model_pid:-}" "${agent_pid:-}" 2>/dev/null || true; rm -rf "$work"; }
trap cleanup EXIT INT TERM

cat > "$work/model.py" <<'PY'
import http.server, json, sys
class Handler(http.server.BaseHTTPRequestHandler):
    """A model that reads a file once, then answers — enough to exercise the loop."""
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))))
        if any(m.get("role") == "tool" for m in body.get("messages", [])):
            reply = {"choices": [{"message": {"role": "assistant",
                     "content": "The file says: " + next((m["content"] for m in reversed(body["messages"])
                        if m.get("role") == "tool"), "")[:60]}}],
                     "usage": {"prompt_tokens": 40, "completion_tokens": 12}}
        else:
            reply = {"choices": [{"message": {"role": "assistant", "content": None,
                     "tool_calls": [{"id": "c1", "type": "function",
                        "function": {"name": "read_file", "arguments": json.dumps({"path": "note.txt"})}}]},
                     "finish_reason": "tool_calls"}],
                     "usage": {"prompt_tokens": 40, "completion_tokens": 8}}
        out = json.dumps(reply).encode()
        self.send_response(200); self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(out))); self.end_headers(); self.wfile.write(out)
    def log_message(self, *args): pass
http.server.HTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
PY

printf 'hello from the workspace\n' > "$work/note.txt"
python3 "$work/model.py" "$port" & model_pid=$!
sleep 0.5
export LOONGAGENT_BASE_URL="http://127.0.0.1:$port/v1"
export LOONGAGENT_MODEL="scripted"

if [ ! -x "$bin" ]; then
  echo "no binary at $bin — run: cargo build" >&2
  exit 1
fi

echo "workspace: $work"
echo "the scripted model is on $LOONGAGENT_BASE_URL (Ctrl-C to stop)"
echo
case "$surface" in
  tui) "$bin" tui --cwd "$work" --session "$work/session.jsonl" --model openai-compat ;;
  run) "$bin" run "read note.txt and tell me what it says" --cwd "$work" \
         --session "$work/session.jsonl" --model openai-compat ;;
  *)   "$bin" web --cwd "$work" --session "$work/session.jsonl" --model openai-compat ;;
esac
