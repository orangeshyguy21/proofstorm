"""Opt-in real-CLI contract fixture. Loopback model and MCP, no external accounts."""
import http.server
import json
import pathlib
import sys

mode, directory = sys.argv[1:3]
work = pathlib.Path(directory)


def append(name, value):
    with (work / name).open("a") as stream:
        stream.write(json.dumps(value) + "\n")


if mode == "mcp":
    for line in sys.stdin:
        request = json.loads(line)
        if "id" not in request:
            continue
        method = request.get("method")
        if method == "initialize":
            (work / "proxy-ready.json").write_text("{}")
            result = {"protocolVersion": "2025-11-25", "capabilities": {"tools": {}},
                      "serverInfo": {"name": "fixture", "version": "1"}}
        elif method == "tools/list":
            result = {"tools": [{"name": "catalog_list", "description": "Fixture tool",
                      "inputSchema": {"type": "object", "properties": {}, "additionalProperties": False}}]}
        elif method == "tools/call":
            params = request["params"]
            append("events.jsonl", {"kind": "start", "id": 1, "tool": params["name"], "arguments": params.get("arguments", {})})
            append("events.jsonl", {"kind": "end", "id": 1, "success": True, "elapsed_ms": 1})
            result = {"content": [{"type": "text", "text": "fixture ok"}], "isError": False}
        else:
            result = {}
        print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
else:
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            request["fixture_plugin"] = self.headers.get("x-proofstorm-fixture")
            append("requests.jsonl", request)
            self.server.calls += 1
            call = self.server.calls
            if call == 1:
                delta = {"role": "assistant", "tool_calls": [{"index": 0, "id": "fixture-call", "type": "function", "function": {"name": "proofstorm_catalog_list", "arguments": "{}"}}]}
                finish = "tool_calls"
            elif call == 2 and mode == "denied":
                delta = {"role": "assistant", "tool_calls": [{"index": 0, "id": "forbidden-call", "type": "function", "function": {"name": "shell", "arguments": "{\"command\":\"echo forbidden\"}"}}]}
                finish = "tool_calls"
            else:
                delta = {"role": "assistant", "content": "fixture complete"}
                finish = "stop"
            chunks = [{"id": f"fixture-{call}", "object": "chat.completion.chunk", "model": "model", "choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
                      {"id": f"fixture-{call}", "object": "chat.completion.chunk", "model": "model", "choices": [{"index": 0, "delta": {}, "finish_reason": finish}], "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}}]
            payload = ("".join("data: " + json.dumps(c) + "\n\n" for c in chunks) + "data: [DONE]\n\n").encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
    server.calls = 0
    print(server.server_port, flush=True)
    server.serve_forever()
