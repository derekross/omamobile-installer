#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Serve web/dist on 127.0.0.1 with the headers the real site would send.

The policy allows only this origin's own files: no inline script or style, no
eval, no third-party origin. 'wasm-unsafe-eval' is what lets a page compile
WebAssembly under a script-src policy; it does not allow JavaScript eval.
"""
import functools
import http.server
import pathlib
import sys

CSP = "; ".join([
    "default-src 'none'",
    "script-src 'self' 'wasm-unsafe-eval'",
    "style-src 'self'",
    "connect-src 'self'",          # the real page adds https://pkgs.omamobile.org
    "img-src 'self'",
    "base-uri 'none'",
    "form-action 'none'",
    "frame-ancestors 'none'",
    "require-trusted-types-for 'script'",
    "trusted-types 'none'",
])


class Handler(http.server.SimpleHTTPRequestHandler):
    extensions_map = {**http.server.SimpleHTTPRequestHandler.extensions_map, ".wasm": "application/wasm", ".js": "text/javascript"}

    def end_headers(self):
        self.send_header("Content-Security-Policy", CSP)
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Referrer-Policy", "no-referrer")
        self.send_header("Cross-Origin-Opener-Policy", "same-origin")
        self.send_header("Permissions-Policy", "usb=(self)")
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def do_POST(self):
        # Only for a headless self-test run: print the page's result
        if self.path != "/selftest-result":
            return self.send_error(404)
        body = self.rfile.read(min(int(self.headers.get("Content-Length", 0)), 1 << 16))
        print("selftest-result", body.decode("utf-8", "replace"), flush=True)
        self.send_response(204)
        self.end_headers()


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8765
    root = pathlib.Path(__file__).resolve().parent / "dist"
    handler = functools.partial(Handler, directory=str(root))
    with http.server.ThreadingHTTPServer(("127.0.0.1", port), handler) as server:
        print(f"http://127.0.0.1:{port}/", flush=True)
        server.serve_forever()
