#!/usr/bin/env python3
"""harw Update-Dienst (upd.harw.dev): Manifest, SSE-Ereignisstrom und APK-Download."""
import json, os, re, sys, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlsplit

ROOT = os.environ.get("HARW_UPD_ROOT", os.path.expanduser("~/harw-updates"))
LATEST = {"stable": "latest.json", "beta": "latest-beta.json"}
APK_DIR = os.path.join(ROOT, "apk")
NAME_RE = re.compile(r"^[A-Za-z0-9._-]{1,80}\.apk$")


def read_latest(channel):
    try:
        with open(os.path.join(ROOT, LATEST[channel]), "rb") as f:
            return f.read()
    except OSError:
        return None


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "harw-upd"

    def log_message(self, fmt, *a):
        sys.stderr.write("%s %s\n" % (self.address_string(), fmt % a))

    def _send(self, code, body=b"", ctype="text/plain; charset=utf-8", extra=()):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        for k, v in extra:
            self.send_header(k, v)
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    do_HEAD = lambda self: self.do_GET()

    def do_GET(self):
        request = urlsplit(self.path)
        path = request.path
        if path in ("/v1/latest", "/v1/events"):
            values = parse_qs(request.query, keep_blank_values=True).get("channel", ["stable"])
            if len(values) != 1 or values[0] not in LATEST:
                return self._send(400, b'{"error":"unknown channel"}', "application/json")
            channel = values[0]
        if path == "/v1/latest":
            data = read_latest(channel)
            if data is None:
                return self._send(404, b'{"error":"no release"}', "application/json")
            return self._send(200, data, "application/json")
        if path == "/v1/events":
            return self._events(channel)
        if path.startswith("/apk/"):
            return self._apk(path[5:])
        if path in ("/", "/healthz"):
            return self._send(200, b"harw update service\n")
        self._send(404, b"Nicht gefunden.\n")

    def _events(self, channel):
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache, no-transform")
        self.send_header("X-Accel-Buffering", "no")
        self.send_header("Connection", "keep-alive")
        self.end_headers()
        last = None
        idle = 0
        try:
            while True:
                data = read_latest(channel)
                if data is not None and data != last:
                    last = data
                    one = json.dumps(json.loads(data))  # eine Zeile
                    self.wfile.write(b"event: update\ndata: " + one.encode() + b"\n\n")
                    self.wfile.flush()
                    idle = 0
                time.sleep(2)
                idle += 2
                if idle >= 20:
                    self.wfile.write(b": ping\n\n")
                    self.wfile.flush()
                    idle = 0
        except OSError:
            pass
        self.close_connection = True

    def _apk(self, name):
        if not NAME_RE.match(name):
            return self._send(404, b"Nicht gefunden.\n")
        p = os.path.join(APK_DIR, name)
        try:
            size = os.path.getsize(p)
            f = open(p, "rb")
        except OSError:
            return self._send(404, b"Nicht gefunden.\n")
        with f:
            self.send_response(200)
            self.send_header("Content-Type", "application/vnd.android.package-archive")
            self.send_header("Content-Length", str(size))
            self.send_header("Content-Disposition", 'attachment; filename="%s"' % name)
            self.end_headers()
            if self.command == "HEAD":
                return
            while chunk := f.read(1 << 20):
                self.wfile.write(chunk)


if __name__ == "__main__":
    addr = os.environ.get("HARW_UPD_LISTEN", "127.0.0.1:7451")
    host, port = addr.rsplit(":", 1)
    ThreadingHTTPServer.daemon_threads = True
    ThreadingHTTPServer((host, int(port)), H).serve_forever()
