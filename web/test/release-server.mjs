// SPDX-License-Identifier: MIT
// A stand-in for the release server (pkgs.omamobile.org/releases/), on
// 127.0.0.1, for the tests of the page's download (web/test) and for the
// page's self-test in a headless browser (web/selftest.sh). It answers as
// omarchy-mobile's tools/server/releases-nginx.sh sets the real one up: any
// origin may read (Access-Control-Allow-Origin: *, the range headers
// exposed, an OPTIONS preflight answered 204), byte ranges with an ETag and
// If-Range, exact Content-Length, no compression.
//
// What it serves is a made-up release signed with a test key: by default
// fixtures/selftest (whose key's secret was never kept), or one handed in.
// Nothing of the real release key is involved.
//
// The first part of a path says how it answers, so a test picks the server's
// behaviour by the address it starts from (as web/serve.py's stand-in for
// Google's server does):
//
//   /<mode>/releases/<codename>/latest.json
//   /<mode>/releases/<codename>/<version>/<path>
//
// `mode` is a list joined by "+": `ok`; `norange` (a server without Range:
// every answer is the whole file); `cut` (the root image's first answer
// breaks off after half its body); `stall` (that answer stops sending after
// half, and holds the connection); `badsum` (one byte of the root image
// changed); `badboot` (one byte of boot.img changed); `badsig` (one byte of
// SHA256SUMS.sig changed); `longer` (the root image with bytes after it, and
// no length said); `badrange` (a range answer for another part than asked);
// `away` (latest.json points to another server); `nolatest` (no latest.json).
//
//   node web/test/release-server.mjs     serve on a free port; prints the origin
import { createReadStream, readFileSync, statSync } from "node:fs";
import { createServer } from "node:http";
import { createHash } from "node:crypto";

const root = new URL("../..", import.meta.url).pathname;
const fixture = (name) => readFileSync(`${root}fixtures/selftest/${name}`);

// The test release: path in the version directory -> bytes, or a file on disk
export function fixtureRelease() {
  const manifest = JSON.parse(fixture("manifest.json"));
  const files = new Map();
  for (const name of ["SHA256SUMS", "SHA256SUMS.sig", "manifest.json", "MANIFEST", "device.conf", "factory-builds.json"]) files.set(name, { bytes: fixture(name) });
  files.set("omamobile.gpg", { bytes: fixture("test-key.gpg") });
  for (const f of manifest.files.filter((f) => f.path.startsWith("images/"))) files.set(f.path, { bytes: fixture(f.path) });
  files.set(manifest.root_image.name, { bytes: fixture("root.img.zst") });
  return { codename: manifest.device.codename, version: manifest.version, label: manifest.label, root: manifest.root_image.name, files };
}

const sizeOf = (entry) => (entry.bytes ? entry.bytes.length : statSync(entry.path).size);
const etagOf = (entry) => {
  entry.etag ??= entry.bytes ? `"${createHash("sha256").update(entry.bytes).digest("hex").slice(0, 16)}"` : `"${statSync(entry.path).mtimeMs.toString(16)}-${sizeOf(entry).toString(16)}"`;
  return entry.etag;
};

export function startReleaseServer({ release = fixtureRelease(), port = 0 } = {}) {
  // What was asked, for the tests: one line per request
  const asked = [];
  // Which answers that break off were given already, so that the next try
  // of the same file gets the file
  const broken = new Set();
  const held = new Set();

  const server = createServer((req, res) => {
    const cors = {
      "Access-Control-Allow-Origin": "*",
      "Access-Control-Allow-Methods": "GET, HEAD, OPTIONS",
      "Access-Control-Allow-Headers": "Range, If-Range, If-None-Match, If-Modified-Since",
      "Access-Control-Expose-Headers": "Content-Length, Content-Range, Accept-Ranges, ETag",
      "Access-Control-Max-Age": "86400",
      "Cross-Origin-Resource-Policy": "cross-origin",
    };
    const url = new URL(req.url, "http://stand-in");
    // For the page's self-test in a browser: what was asked so far
    if (url.pathname === "/asked") {
      const body = JSON.stringify(asked);
      res.writeHead(200, { ...cors, "Content-Type": "application/json", "Cache-Control": "no-store" });
      return res.end(body);
    }
    const [mode = "", top, codename, ...rest] = url.pathname.slice(1).split("/");
    const modes = new Set(mode.split("+"));
    const path = rest.join("/");
    asked.push({ method: req.method, mode, path: rest.length ? path : url.pathname, range: req.headers.range ?? null, ifRange: req.headers["if-range"] ?? null });
    const send = (status, headers = {}, body = "") => {
      res.writeHead(status, { ...cors, "Cache-Control": "no-store", ...headers });
      res.end(body);
    };
    if (req.method === "OPTIONS") return send(204);
    if (req.method !== "GET" && req.method !== "HEAD") return send(405);
    if (top !== "releases" || codename !== release.codename) return send(404, { "Content-Length": "0" });

    if (rest.length === 1 && rest[0] === "latest.json") {
      if (modes.has("nolatest")) return send(404, { "Content-Length": "0" });
      const base = modes.has("away") ? `https://elsewhere.example/releases/${codename}/${release.version}/` : `${origin()}/${mode}/releases/${codename}/${release.version}/`;
      const manifest = release.files.get("manifest.json");
      const body = JSON.stringify({
        format: 1,
        codename,
        version: release.version,
        label: release.label,
        base_url: base,
        manifest: `${base}manifest.json`,
        manifest_sha256: createHash("sha256").update(manifest.bytes ?? readFileSync(manifest.path)).digest("hex"),
      });
      return send(200, { "Content-Type": "application/json", "Content-Length": String(Buffer.byteLength(body)) }, req.method === "HEAD" ? "" : body);
    }
    if (rest[0] !== release.version) return send(404, { "Content-Length": "0" });
    const name = rest.slice(1).join("/");
    const entry = release.files.get(name);
    if (!entry) return send(404, { "Content-Length": "0" });

    const size = sizeOf(entry);
    const etag = etagOf(entry);
    const isRoot = name === release.root;
    const flip = (modes.has("badsum") && isRoot) || (modes.has("badboot") && name === "images/boot.img") || (modes.has("badsig") && name === "SHA256SUMS.sig");
    let first = 0;
    let status = 200;
    const headers = { "Content-Type": "application/octet-stream", ETag: etag };
    if (!modes.has("norange")) headers["Accept-Ranges"] = "bytes";
    const range = /^bytes=([0-9]+)-([0-9]*)$/.exec(req.headers.range ?? "");
    const ifRange = req.headers["if-range"];
    if (range && !modes.has("norange") && (!ifRange || ifRange === etag)) {
      first = Number(range[1]);
      if (first >= size) return send(416, { "Content-Range": `bytes */${size}` });
      status = 206;
      headers["Content-Range"] = modes.has("badrange") ? `bytes ${first + 1}-${size - 1}/${size}` : `bytes ${first}-${size - 1}/${size}`;
    }
    const length = size - first;
    const longer = modes.has("longer") && isRoot;
    if (!longer) headers["Content-Length"] = String(length);
    res.writeHead(status, { ...cors, "Cache-Control": "public, max-age=31536000, immutable", ...headers });
    if (req.method === "HEAD") return res.end();

    // The body, from `first`
    const key = `${mode} ${name}`;
    const breaks = isRoot && first === 0 && !broken.has(key) && (modes.has("cut") || modes.has("stall"));
    const half = Math.floor(length / 2);
    if (entry.bytes) {
      let body = Buffer.from(entry.bytes.subarray(first));
      if (flip) {
        body = Buffer.from(body);
        body[Math.floor(body.length / 2)] ^= 1;
      }
      if (breaks) {
        broken.add(key);
        res.write(body.subarray(0, half));
        if (modes.has("stall")) {
          held.add(res);
          return;
        }
        return void setTimeout(() => res.destroy(), 20);
      }
      res.write(body);
      if (longer) res.write(Buffer.alloc(4096));
      return res.end();
    }
    // A file on disk, streamed (the measurements' large root image)
    const stream = createReadStream(entry.path, { start: first, highWaterMark: 1 << 20 });
    let sent = 0;
    if (breaks) broken.add(key);
    stream.on("data", (chunk) => {
      if (breaks && sent + chunk.length >= half) {
        res.write(chunk.subarray(0, half - sent));
        stream.destroy();
        if (modes.has("stall")) held.add(res);
        else setTimeout(() => res.destroy(), 20);
        return;
      }
      sent += chunk.length;
      if (!res.write(chunk)) {
        stream.pause();
        res.once("drain", () => stream.resume());
      }
    });
    stream.on("end", () => res.end());
  });

  let bound = null;
  const origin = () => `http://127.0.0.1:${bound}`;
  return new Promise((resolve) => {
    server.listen(port, "127.0.0.1", () => {
      bound = server.address().port;
      resolve({
        origin: origin(),
        // The page's base for releases, answering in `mode`
        base: (mode = "ok") => `${origin()}/${mode}/releases/`,
        asked,
        release,
        close: () =>
          new Promise((done) => {
            for (const res of held) res.destroy();
            server.closeAllConnections?.();
            server.close(done);
          }),
      });
    });
  });
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const s = await startReleaseServer();
  console.log(s.origin);
}
