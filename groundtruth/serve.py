#!/usr/bin/env python3
"""Tiny local server for the ground-truth labelling tool (PLAN 17.1).

Serves groundtruth/ over http://127.0.0.1:8765 and accepts POST /save?v=<video>
to write <video>.csv next to this file. Local-only, no dependencies.
"""
import http.server, socketserver, os, urllib.parse

ROOT = os.path.dirname(os.path.abspath(__file__))
PORT = 8765


class Handler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *a, **kw):
        super().__init__(*a, directory=ROOT, **kw)

    def do_POST(self):
        parsed = urllib.parse.urlparse(self.path)
        if parsed.path != "/save":
            self.send_error(404)
            return
        qs = urllib.parse.parse_qs(parsed.query)
        vid = (qs.get("v") or ["out"])[0]
        if not vid.isalnum():
            self.send_error(400, "bad video id")
            return
        body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        path = os.path.join(ROOT, f"{vid}.csv")
        with open(path, "wb") as fh:
            fh.write(body)
        print(f"saved {path} ({len(body)} bytes)")
        self.send_response(200)
        self.send_header("Content-Length", "2")
        self.end_headers()
        self.wfile.write(b"ok")

    def log_message(self, fmt, *args):
        # Quiet the per-asset GET spam; keep saves and errors visible.
        first = str(args[0]) if args else ""
        if "save" in first or "GET" not in first:
            super().log_message(fmt, *args)


socketserver.TCPServer.allow_reuse_address = True
with socketserver.TCPServer(("127.0.0.1", PORT), Handler) as httpd:
    print(f"labelling tool: http://127.0.0.1:{PORT}/label.html?v=v3")
    httpd.serve_forever()
