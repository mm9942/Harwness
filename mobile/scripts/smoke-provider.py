"""Deterministic loopback-only Chat Completions fixture for mobile-bootstrap."""

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import sys
import time


MODEL, REPLY = sys.argv[1:]


class Handler(BaseHTTPRequestHandler):
    def log_message(self, format, *args):
        # Never log requests or their contents.
        pass

    def do_POST(self):
        if self.path != "/v1/chat/completions":
            self.send_error(404)
            return
        try:
            length = int(self.headers.get("Content-Length", "0"))
            if not 0 < length <= 1_048_576:
                raise ValueError("invalid request length")
            request = json.loads(self.rfile.read(length))
            if not isinstance(request, dict) or request.get("model") != MODEL:
                raise ValueError("unexpected model")
        except ValueError:
            self.send_error(400)
            return

        common = {"id": "chatcmpl-mobile-smoke", "created": 0, "model": MODEL}
        if not request.get("stream"):
            payload = {
                **common,
                "object": "chat.completion",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": REPLY},
                    "finish_reason": "stop",
                }],
                "usage": {"prompt_tokens": 1, "completion_tokens": 4, "total_tokens": 5},
            }
            body = json.dumps(payload).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return

        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.end_headers()
        first, _, rest = REPLY.partition(" ")
        for delta, finish_reason in [
            ({"role": "assistant", "content": first + " "}, None),
            ({"content": rest}, None),
            ({}, "stop"),
        ]:
            payload = {
                **common,
                "object": "chat.completion.chunk",
                "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason}],
            }
            self.wfile.write(f"data: {json.dumps(payload)}\n\n".encode())
            self.wfile.flush()
            if finish_reason is None:
                time.sleep(0.2)
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()


with ThreadingHTTPServer(("127.0.0.1", 0), Handler) as server:
    print(server.server_port, flush=True)
    server.serve_forever()
