#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Serve web/dist on 127.0.0.1 with the headers the real site would send.

The policy allows only this origin's own files: no inline script or style, no
eval, no third-party origin. 'wasm-unsafe-eval' is what lets a page compile
WebAssembly under a script-src policy; it does not allow JavaScript eval.

The one other origin the page may fetch from is https://dl.google.com, for
Google's factory image, once the person has accepted Google's terms.

  web/serve.py [PORT]             as above
  web/serve.py [PORT] --dir NAME  another build than web/dist: its directory
                                  beside this file (the write preview's
                                  dist-preview, or its test build's)
  web/serve.py [PORT] --stand-in  for a self-test build: a stand-in for
                                  Google's server on another port of this
                                  computer takes dl.google.com's place in the
                                  policy, so the page can reach nothing of
                                  Google's. It serves fixtures/selftest/factory.
"""
import functools
import http.server
import json
import pathlib
import sys
import threading

HERE = pathlib.Path(__file__).resolve().parent


def csp(fetch_from):
    return "; ".join([
        "default-src 'none'",
        "script-src 'self' 'wasm-unsafe-eval'",
        "style-src 'self'",
        f"connect-src 'self' {fetch_from}",   # the real page adds https://pkgs.omamobile.org
        "img-src 'self'",
        "base-uri 'none'",
        "form-action 'none'",
        "frame-ancestors 'none'",
        "require-trusted-types-for 'script'",
        "trusted-types 'none'",
    ])


class Handler(http.server.SimpleHTTPRequestHandler):
    extensions_map = {**http.server.SimpleHTTPRequestHandler.extensions_map, ".wasm": "application/wasm", ".js": "text/javascript"}
    fetch_from = "https://dl.google.com"
    stand_in = None  # its origin, when there is one

    def end_headers(self):
        self.send_header("Content-Security-Policy", csp(self.fetch_from))
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Referrer-Policy", "no-referrer")
        self.send_header("Cross-Origin-Opener-Policy", "same-origin")
        self.send_header("Permissions-Policy", "usb=(self)")
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def do_GET(self):
        # Where a self-test build's page finds the stand-in for Google's server
        if self.path == "/google-stand-in.json" and self.stand_in:
            body = json.dumps({"origin": self.stand_in}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        super().do_GET()

    def do_POST(self):
        # Only for a headless self-test run: print the page's result
        if self.path != "/selftest-result":
            return self.send_error(404)
        body = self.rfile.read(min(int(self.headers.get("Content-Length", 0)), 1 << 16))
        print("selftest-result", body.decode("utf-8", "replace"), flush=True)
        self.send_response(204)
        self.end_headers()


class StandIn(http.server.BaseHTTPRequestHandler):
    """In Google's place for the page's self-test, on another origin.

    The first part of the path says how it answers; the last is the file's
    name in fixtures/selftest/factory. `allowed` is a server that lets a page
    on another origin read its files, `refused` one that does not. Google's
    server is the second kind (no Access-Control-Allow-Origin header, measured
    on 2026-11-12), so the page as it ships begins no download at all.
    """
    files = HERE.parent / "fixtures" / "selftest" / "factory"
    asked = []

    def log_message(self, *args):
        pass

    def answer(self, status, body=b"", length=None, cors=True, headers=()):
        self.send_response(status)
        if cors:
            self.send_header("Access-Control-Allow-Origin", "*")
        if length is not None:
            self.send_header("Content-Length", str(length))
        for name, value in headers:
            self.send_header(name, value)
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/asked":
            body = json.dumps(self.asked).encode()
            return self.answer(200, body, len(body))
        self.asked.append(self.path)
        parts = self.path.strip("/").split("/")
        mode, name = parts[0], parts[-1]
        file = self.files / name
        # Only a file of that one directory, by its name
        if name not in [f.name for f in self.files.iterdir()] or len(parts) < 2:
            return self.answer(404, length=0)
        data = file.read_bytes()
        if mode == "allowed" or mode == "elsewhere" or (mode == "moved-same" and "edge" in parts):
            self.answer(200, data, len(data))
        elif mode == "refused":
            # The file, without the header that lets another origin read it
            self.answer(200, data, len(data), cors=False)
        elif mode == "unreachable":
            self.close_connection = True
            self.connection.close()
        elif mode == "wrong":
            changed = bytearray(data)
            changed[len(changed) // 2] ^= 1
            self.answer(200, bytes(changed), len(changed))
        elif mode == "longer":
            # No length said, and more bytes than the file has
            self.answer(200, data + bytes(4096))
        elif mode == "size":
            self.answer(200, data + bytes(4096), len(data) + 4096)
        elif mode == "moved":
            self.answer(302, length=0, headers=[("Location", f"/elsewhere/{name}")])
        elif mode == "moved-same":
            self.answer(302, length=0, headers=[("Location", f"/moved-same/edge/{name}")])
        elif mode == "broken":
            # The whole length said, half of it sent
            self.answer(200, data[: len(data) // 2], len(data))
        else:
            self.answer(404, length=0)


if __name__ == "__main__":
    args = [a for a in sys.argv[1:] if a != "--stand-in"]
    served = "dist"
    if "--dir" in args:
        at = args.index("--dir")
        served = args[at + 1]
        if pathlib.PurePath(served).name != served:
            sys.exit("--dir takes the name of a directory beside this file")
        del args[at:at + 2]
    port = int(args[0]) if args else 8765
    if "--stand-in" in sys.argv[1:]:
        google = http.server.ThreadingHTTPServer(("127.0.0.1", 0), StandIn)
        Handler.stand_in = f"http://127.0.0.1:{google.server_address[1]}"
        Handler.fetch_from = Handler.stand_in
        threading.Thread(target=google.serve_forever, daemon=True).start()
    handler = functools.partial(Handler, directory=str(HERE / served))
    with http.server.ThreadingHTTPServer(("127.0.0.1", port), handler) as server:
        print(f"http://127.0.0.1:{port}/", flush=True)
        server.serve_forever()
