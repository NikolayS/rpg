#!/usr/bin/env python3
"""Static file server for rpg WASM demo with correct MIME types and COOP/COEP headers."""
import http.server
import os

MIME = {
    ".html": "text/html",
    ".js":   "application/javascript",
    ".wasm": "application/wasm",
    ".ts":   "text/plain",
    ".json": "application/json",
}

class Handler(http.server.SimpleHTTPRequestHandler):
    def end_headers(self):
        self.send_header("Cross-Origin-Opener-Policy",   "same-origin")
        self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
        super().end_headers()

    def guess_type(self, path):
        ext = os.path.splitext(path)[1]
        return MIME.get(ext, "application/octet-stream")

    def log_message(self, fmt, *args):
        pass  # suppress request noise

os.chdir(os.path.dirname(os.path.abspath(__file__)))
server = http.server.HTTPServer(("127.0.0.1", 8080), Handler)
print(f"serving wasm/ on http://127.0.0.1:8080", flush=True)
server.serve_forever()
