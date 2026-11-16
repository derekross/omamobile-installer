// SPDX-License-Identifier: MIT
// The release, fetched and checked by the page itself (docs/WEB-WRITE.md §6):
// the latest release for a phone from the release server's index, its small
// files, then its boot images and its root image, kept in the browser's
// Origin Private File System so that an interrupted download goes on where it
// stopped and a finished one is not downloaded again. Or the same files from
// a release the owner saved on this computer: its files, or its one-file .tar.
//
// What it decides: nothing about the release's trust. The small files go to
// the wasm module, which checks the release key's signature on SHA256SUMS
// with the key's fingerprint built into it, and the rest of the chain
// (core/src/bundle.rs), exactly as the command-line tool does. Every large
// file is then checked by the module against the signed manifest: a boot
// image whole (`Release.check_file`), the root image as its bytes arrive
// (`Release.root_check`). `latest.json` is only a pointer: it can name a
// version, and only one on the server it came from.
//
// Everything this file reaches outside itself is handed in (`fetch`, the
// storage, the clock), so that the tests run it in node against a stand-in
// server and a stand-in file system (web/test). It holds no rule about a
// phone and sends nothing to one.

// Where releases are published (omarchy-mobile docs/RELEASES.md §6). The
// server sends CORS headers for every page and allows Range requests.
export const RELEASES = "https://pkgs.omamobile.org/releases/";

// The phones a release is published for, as the release server names them.
// The core's flow checks the phone against the release it is handed, so a
// wrong choice here ends at the flow's own stop for another model.
export const DEVICES = [{ codename: "cheetah", name: "Pixel 7 Pro" }];

// A release's small files, in the order the flow takes them (app.js), and
// the build index, which a release may lack
export const SMALL_FILES = ["SHA256SUMS", "SHA256SUMS.sig", "omamobile.gpg", "manifest.json", "MANIFEST", "device.conf"];
export const INDEX_FILE = "factory-builds.json";

const MiB = 1 << 20;
export const DEFAULTS = {
  // A large file is kept as numbered parts of at most this size. A part is
  // written in one go and committed when it is closed, so a tab that is
  // closed or crashes loses at most the part it was writing; a connection
  // that breaks loses nothing, since the part is closed then.
  partBytes: 32 * MiB,
  // No byte of the body for this long: the try ends, as a broken connection
  stallMs: 60_000,
  // Tries of one file before the page asks the owner to try again
  tries: 5,
  retryMs: [2_000, 5_000, 15_000, 30_000],
  // Room left over in the origin's quota, beyond what the files need
  marginBytes: 256 * MiB,
  // More than this is no small file of a release
  smallLimit: 4 * MiB,
  // How much of a kept file is read back at a time
  readBytes: 4 * MiB,
};

// Every way this file gives up, by kind: `no-release` (the server has no
// release for this phone), `server` (an answer that is not the file),
// `pointer` (latest.json is not one this page follows), `quota`, `storage`,
// `network`, `stalled`, `mismatch` (bytes that are not the signed release's:
// they are deleted), `bundle` (the files the owner picked)
export class ReleaseError extends Error {
  constructor(kind, message) {
    super(message);
    this.kind = kind;
  }
}
const fail = (kind, message) => new ReleaseError(kind, message);

// No cookie and no referrer go to the server, and nothing is kept in the
// browser's HTTP cache: the files are kept here, once
const PLAIN = { mode: "cors", credentials: "omit", referrerPolicy: "no-referrer", cache: "no-store", redirect: "error" };

const VERSION = /^20[0-9]{2}\.[0-9]{2}\.[0-9]{2}(\.[0-9]+)?$/;
const HEX64 = /^[0-9a-f]{64}$/;

export const basename = (path) => path.split("/").pop();

export async function sha256hex(bytes) {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  return [...digest].map((b) => b.toString(16).padStart(2, "0")).join("");
}

export function size(bytes) {
  if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(2)} GB`;
  if (bytes >= 1e6) return `${(bytes / 1e6).toFixed(1)} MB`;
  return `${Math.ceil(bytes / 1e3)} kB`;
}

// ---- The server's index and the small files --------------------------------

// The server's pointer to the latest release for `codename`
// (`releases/<codename>/latest.json`, written by tools/release-upload
// --latest). Followed only to a version directory beside it.
export async function findLatest(codename, { base = RELEASES, fetch: get = fetch } = {}) {
  if (!/^[a-z0-9]+$/.test(codename)) throw fail("pointer", `${codename} is not a phone's codename`);
  const url = `${base}${codename}/latest.json`;
  let response;
  try {
    response = await get(url, PLAIN);
  } catch {
    throw fail("network", `the release server (${new URL(base).host}) did not answer`);
  }
  if (response.status === 404) throw fail("no-release", `the release server has no release for ${codename} yet`);
  if (!response.ok) throw fail("server", `the release server answered HTTP ${response.status} for ${codename}'s latest release`);
  let p;
  try {
    p = JSON.parse(new TextDecoder().decode(await limited(response, DEFAULTS.smallLimit)));
  } catch {
    throw fail("pointer", "the release server's latest.json is not one this page can read");
  }
  const expected = `${base}${codename}/${p?.version}/`;
  if (p?.format !== 1 || p.codename !== codename || typeof p.version !== "string" || !VERSION.test(p.version)) {
    throw fail("pointer", "the release server's latest.json is not one this page can read");
  }
  if (p.base_url !== expected) throw fail("pointer", `latest.json points to ${p.base_url}, not beside itself (${expected})`);
  if (typeof p.manifest_sha256 !== "string" || !HEX64.test(p.manifest_sha256)) throw fail("pointer", "latest.json names no manifest digest");
  return { codename, version: p.version, label: String(p.label ?? ""), base_url: p.base_url, manifest_sha256: p.manifest_sha256 };
}

// A body of at most `limit` bytes, whole
async function limited(response, limit) {
  const length = Number(response.headers.get("content-length") ?? 0);
  if (length > limit) throw fail("server", "the server sent a small file larger than any release has");
  const bytes = new Uint8Array(await response.arrayBuffer());
  if (bytes.length > limit) throw fail("server", "the server sent a small file larger than any release has");
  return bytes;
}

// The release's small files from its version directory. The manifest's
// digest is compared with the pointer's, only to say early that the two
// disagree: trust is the module's check of the signature, which comes next.
export async function fetchSmall(pointer, { fetch: get = fetch } = {}) {
  const one = async (name, optional = false) => {
    let response;
    try {
      response = await get(pointer.base_url + name, PLAIN);
    } catch {
      throw fail("network", `${name} could not be fetched from the release server`);
    }
    if (optional && response.status === 404) return undefined;
    if (!response.ok) throw fail("server", `${name}: the release server answered HTTP ${response.status}`);
    return limited(response, DEFAULTS.smallLimit);
  };
  const files = await Promise.all(SMALL_FILES.map((name) => one(name)));
  if ((await sha256hex(files[3])) !== pointer.manifest_sha256) {
    throw fail("pointer", `the release server's latest.json and its manifest.json for ${pointer.version} disagree`);
  }
  return { files, index: await one(INDEX_FILE, true) };
}

// The large files of a verified release: `described` is the module's
// `describe()` of it (names and sizes, from the signed manifest), `manifest`
// the signed manifest.json's bytes (for each file's SHA-256, which keys what
// is kept)
export function largeFiles(described, manifest) {
  const m = JSON.parse(new TextDecoder().decode(manifest));
  const digest = (path) => m.files.find((f) => f.path === path)?.sha256;
  const boot = described.boot_images.map((b) => ({ kind: "boot", path: b.source, name: basename(b.source), size: b.size, sha256: digest(b.source) }));
  const r = described.root_image;
  const root = { kind: "root", path: r.name, name: r.name, size: r.size, sha256: m.root_image.sha256 };
  return [...boot, root];
}

// ---- Where the files are kept ------------------------------------------------

// A release's files in the Origin Private File System:
//   omamobile-releases/<codename>/<version>/<name>.<NNNN>.part   the bytes
//   omamobile-releases/<codename>/<version>/<name>.json          its record
// `root` is `await navigator.storage.getDirectory()`, or the tests' stand-in.
export class Store {
  static TOP = "omamobile-releases";

  constructor(dir) {
    this.dir = dir;
  }

  // The directory for one version. Other versions of the same phone are
  // deleted: a release directory never changes, and one is enough to keep.
  static async open(root, codename, version) {
    const top = await root.getDirectoryHandle(Store.TOP, { create: true });
    const phone = await top.getDirectoryHandle(codename, { create: true });
    const removed = [];
    for await (const [name] of phone.entries()) {
      if (name !== version) {
        await phone.removeEntry(name, { recursive: true });
        removed.push(name);
      }
    }
    return Object.assign(new Store(await phone.getDirectoryHandle(version, { create: true })), { removed });
  }

  // Every release this page has kept, for the button that deletes them
  static async clear(root) {
    try {
      await root.removeEntry(Store.TOP, { recursive: true });
    } catch (e) {
      if (e?.name !== "NotFoundError") throw e;
    }
  }

  static part(name, n) {
    return `${name}.${String(n).padStart(4, "0")}.part`;
  }

  // The kept parts of a file, in order from the first; parts after a gap
  // can't be placed and are deleted
  async parts(name) {
    const found = new Map();
    for await (const [entry, handle] of this.dir.entries()) {
      const m = entry.startsWith(`${name}.`) && /\.([0-9]{4})\.part$/.exec(entry);
      if (m && entry === Store.part(name, Number(m[1]))) found.set(Number(m[1]), handle);
    }
    const out = [];
    for (let n = 0; found.has(n); n++) {
      const file = await found.get(n).getFile();
      out.push({ n, file });
      found.delete(n);
    }
    for (const n of found.keys()) await this.dir.removeEntry(Store.part(name, n));
    return out;
  }

  async writer(name, n) {
    const handle = await this.dir.getFileHandle(Store.part(name, n), { create: true });
    return handle.createWritable({ keepExistingData: false });
  }

  // The kept file as one File, made of its parts (read from disk when read)
  async file(name) {
    const parts = await this.parts(name);
    return new File(
      parts.map((p) => p.file),
      name,
    );
  }

  async drop(name) {
    for (const p of await this.parts(name)) await this.dir.removeEntry(Store.part(name, p.n));
    await this.dir.removeEntry(`${name}.json`).catch(() => {});
  }

  async record(name) {
    try {
      const file = await (await this.dir.getFileHandle(`${name}.json`)).getFile();
      return JSON.parse(await file.text());
    } catch {
      return null;
    }
  }

  async setRecord(name, record) {
    const w = await (await this.dir.getFileHandle(`${name}.json`, { create: true })).createWritable();
    await w.write(JSON.stringify(record));
    await w.close();
  }
}

// ---- Downloading one large file ---------------------------------------------

// `bytes FIRST-LAST/TOTAL`
function contentRange(value) {
  const m = /^bytes ([0-9]+)-([0-9]+)\/([0-9]+)$/.exec(value ?? "");
  return m ? m.slice(1).map(Number) : null;
}

const sleeper = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

// One file, into `store`, going on from what is kept. Retries a broken or
// stalled transfer by itself, from where it broke. `check()` makes a fresh
// streaming check ({update, done}) that every byte goes through, kept bytes
// first; `done()` throws for a file that is not the signed release's, and
// the file is then deleted. Returns the kept file.
export async function fetchFile(want, o) {
  const opt = { ...DEFAULTS, sleep: sleeper, ...o };
  for (let tries = 1; ; tries++) {
    try {
      return await oneTry(want, opt);
    } catch (e) {
      const again = e instanceof ReleaseError && (e.kind === "network" || e.kind === "stalled");
      if (!again || tries >= opt.tries || opt.signal?.aborted) throw e;
      opt.onEvent?.({ type: "retry", name: want.name, tries, why: e.message });
      await opt.sleep(opt.retryMs[Math.min(tries - 1, opt.retryMs.length - 1)]);
    }
  }
}

async function oneTry(want, o) {
  const { store, url } = o;
  const event = (e) => o.onEvent?.({ name: want.name, size: want.size, ...e });
  let parts = await store.parts(want.name);
  let have = parts.reduce((n, p) => n + p.file.size, 0);
  const record = await store.record(want.name);
  // Kept bytes of another file (a release directory never changes, so this
  // is a page from before a change of rules) or more of them than the file has
  if (have > want.size || (record && record.sha256 !== want.sha256)) {
    await store.drop(want.name);
    parts = [];
    have = 0;
  }
  let etag = record?.etag ?? null;
  let check = o.check?.();
  // What was kept goes through the check first, from the first byte
  if (check) {
    let fed = 0;
    for (const p of parts) {
      for (let at = 0; at < p.file.size; at += o.readBytes) {
        const piece = new Uint8Array(await p.file.slice(at, at + o.readBytes).arrayBuffer());
        check.update(piece);
        fed += piece.length;
        event({ type: "kept", done: fed });
      }
    }
  }
  const finish = async () => {
    if (check) {
      try {
        check.done();
      } catch (e) {
        await store.drop(want.name);
        throw fail("mismatch", `${want.name} as downloaded is not the signed release's file (${e.message ?? e}); it was deleted`);
      }
    }
    await store.setRecord(want.name, { sha256: want.sha256, size: want.size, etag, complete: true });
    return store.file(want.name);
  };
  if (have === want.size) return finish();

  // The request: the rest of the file, if any of it is kept
  const controller = new AbortController();
  const outer = () => controller.abort();
  o.signal?.addEventListener("abort", outer);
  let stalled = false;
  let timer = null;
  const arm = () => {
    clearTimeout(timer);
    timer = setTimeout(() => {
      stalled = true;
      controller.abort();
    }, o.stallMs);
  };
  const headers = {};
  if (have > 0) {
    headers.Range = `bytes=${have}-`;
    // A file that changed on the server since is sent whole (200)
    if (record?.etag) headers["If-Range"] = record.etag;
  }
  let writer = null;
  try {
    arm();
    let response;
    try {
      // Called on its own: a browser's fetch refuses to be a method of
      // another object ("Illegal invocation")
      const get = o.fetch;
      response = await get(url, { ...PLAIN, headers, signal: controller.signal });
    } catch {
      throw stalled ? fail("stalled", `the release server did not answer for ${o.stallMs / 1000} s`) : fail("network", `${want.name}: no answer from the release server`);
    }
    let n = parts.length;
    if (response.status === 200) {
      const length = response.headers.get("content-encoding") ? null : response.headers.get("content-length");
      if (length !== null && Number(length) !== want.size) {
        await response.body?.cancel();
        throw fail("server", `the release server offers ${want.name} with ${length} bytes, and the signed release lists ${want.size}`);
      }
      if (have > 0) {
        // The whole file, whatever was asked for: a server without Range,
        // or a file that changed. What was kept goes, and so does its hash.
        event({ type: "restart", why: "the server sent the whole file again" });
        await store.drop(want.name);
        have = 0;
        n = 0;
        check = o.check?.();
      }
    } else if (response.status === 206 && have > 0) {
      const range = contentRange(response.headers.get("content-range"));
      if (!range || range[0] !== have || range[1] !== want.size - 1 || range[2] !== want.size) {
        await response.body?.cancel();
        throw fail("server", `the release server sent another part of ${want.name} than the one asked for`);
      }
    } else {
      await response.body?.cancel();
      throw fail("server", `${want.name}: the release server answered HTTP ${response.status}`);
    }
    etag = response.headers.get("etag");
    await store.setRecord(want.name, { sha256: want.sha256, size: want.size, etag, complete: false });

    let inPart = 0;
    const reader = response.body.getReader();
    for (;;) {
      let next;
      try {
        next = await reader.read();
      } catch {
        throw stalled ? fail("stalled", `${want.name}: no byte arrived for ${o.stallMs / 1000} s`) : fail("network", `${want.name}: the connection broke`);
      }
      if (next.done) break;
      arm();
      const chunk = next.value;
      if (have + chunk.length > want.size) {
        await reader.cancel().catch(() => {});
        await writer?.close();
        writer = null;
        await store.drop(want.name);
        throw fail("mismatch", `the release server sent more than the ${want.size} bytes of ${want.name}; what arrived was deleted`);
      }
      for (let at = 0; at < chunk.length; ) {
        if (!writer) {
          writer = await o.store.writer(want.name, n);
          inPart = 0;
        }
        const piece = chunk.subarray(at, at + Math.min(o.partBytes - inPart, chunk.length - at));
        await writer.write(piece);
        check?.update(piece);
        inPart += piece.length;
        have += piece.length;
        at += piece.length;
        if (inPart === o.partBytes) {
          await writer.close();
          writer = null;
          n++;
        }
      }
      event({ type: "download", done: have });
    }
    if (writer) {
      await writer.close();
      writer = null;
    }
    if (have < want.size) throw fail("network", `${want.name}: the server ended the file after ${have} of ${want.size} bytes`);
    return await finish();
  } catch (e) {
    // What arrived stays for the next try: the part being written is closed,
    // which commits it
    if (writer) await writer.close().catch(() => {});
    if (e instanceof ReleaseError) throw e;
    if (e?.name === "QuotaExceededError") throw fail("storage", "the browser has no more room for the release on this computer");
    throw fail("storage", `${want.name} could not be kept on this computer (${e?.message ?? e})`);
  } finally {
    clearTimeout(timer);
    o.signal?.removeEventListener("abort", outer);
  }
}

// ---- The whole release --------------------------------------------------------

// Room for what is still to come, asked before the first byte. `storage` is
// `navigator.storage`. Persistence is asked for (a browser may evict an
// origin's files under pressure otherwise); the answer is returned, and a no
// does not stop the download.
export async function prepare(files, store, storage, { marginBytes = DEFAULTS.marginBytes } = {}) {
  let kept = 0;
  for (const f of files) {
    const record = await store.record(f.name);
    if (record && record.sha256 !== f.sha256) continue;
    kept += (await store.parts(f.name)).reduce((n, p) => n + p.file.size, 0);
  }
  const total = files.reduce((n, f) => n + f.size, 0);
    const persisted = (await storage.persisted?.()) || ((await storage.persist?.()) ?? false);
  const { quota = 0, usage = 0 } = (await storage.estimate?.()) ?? {};
  const needed = Math.max(0, total - kept) + marginBytes;
  if (quota - usage < needed) {
    throw fail(
      "quota",
      `the browser allows this page ${size(Math.max(0, quota - usage))} more on this computer, and the release needs ${size(needed)} (${size(total - kept)} still to download, and room to spare). Free some disk space, or pick a release saved on this computer instead.`,
    );
  }
  return { total, kept, persisted, quota, usage };
}

// Download what is missing of a verified release and check every file
// against it. `release` is the module's `Release` (verify_release), `files`
// the list `largeFiles` made, `base_url` the release's directory. Returns
// the files as File objects, boot images first, in the plan's order.
export async function downloadRelease({ release, files, base_url, store, storage, fetch: get = fetch, onEvent, ...options }) {
  const ready = await prepare(files, store, storage, options);
  onEvent?.({ type: "prepared", ...ready });
  const out = [];
  for (const f of files) {
    const file = await fetchFile(f, {
      ...options,
      store,
      url: base_url + f.path,
      fetch: get,
      onEvent,
      check: f.kind === "root" ? () => release.root_check() : null,
    });
    if (f.kind === "boot") await checkBoot(release, f, file, store);
    onEvent?.({ type: "checked", name: f.name, size: f.size });
    out.push(file);
  }
  return out;
}

async function checkBoot(release, f, file, store) {
  try {
    release.check_file(f.path, new Uint8Array(await file.arrayBuffer()));
  } catch (e) {
    await store?.drop(f.name);
    throw fail("mismatch", `${f.name} is not the signed release's file (${e.message ?? e})${store ? "; it was deleted" : ""}`);
  }
}

// ---- A release saved on this computer -------------------------------------------

// The files the owner picked: the release's files (any of them, from its
// folder or loose), or its one-file .tar. Each is found by its name, which is
// unique in a release. Nothing is read but the small files and, for a .tar,
// its headers.
export async function fromPicked(list) {
  const picked = [...list];
  let byName;
  if (picked.length === 1 && picked[0].name.endsWith(".tar")) {
    byName = await readTar(picked[0]);
  } else {
    byName = new Map();
    for (const f of picked) {
      if (byName.has(f.name)) throw fail("bundle", `two of the files you picked are called ${f.name}`);
      byName.set(f.name, f);
    }
  }
  const small = async (name, optional = false) => {
    const f = byName.get(name);
    if (!f) {
      if (optional) return undefined;
      throw fail("bundle", `${name} is not among the files you picked: pick the release's folder, all of its files, or its .tar`);
    }
    if (f.size > DEFAULTS.smallLimit) throw fail("bundle", `${name} is larger than any release's ${name}`);
    return new Uint8Array(await f.arrayBuffer());
  };
  const files = [];
  for (const name of SMALL_FILES) files.push(await small(name));
  return { files, index: await small(INDEX_FILE, true), byName };
}

// The large files of a verified release among the picked ones, each checked
// against the signed manifest (the root image a piece at a time, never whole).
export async function checkPicked(release, files, byName, { onEvent, readBytes = DEFAULTS.readBytes } = {}) {
  const out = [];
  for (const f of files) {
    const file = byName.get(f.name);
    if (!file) throw fail("bundle", `${f.name} is not among the files you picked`);
    if (file.size !== f.size) throw fail("bundle", `${f.name} is ${file.size} bytes, and the release lists ${f.size}: it is not the release's file, or it is not complete`);
    if (f.kind === "boot") {
      await checkBoot(release, f, file, null);
    } else {
      const check = release.root_check();
      for (let at = 0; at < file.size; at += readBytes) {
        check.update(new Uint8Array(await file.slice(at, at + readBytes).arrayBuffer()));
        onEvent?.({ type: "kept", name: f.name, size: f.size, done: Math.min(at + readBytes, file.size) });
      }
      try {
        check.done();
      } catch (e) {
        throw fail("mismatch", `${f.name} is not the signed release's file (${e.message ?? e})`);
      }
    }
    onEvent?.({ type: "checked", name: f.name, size: f.size });
    out.push(file);
  }
  return out;
}

// A tar's members, by name: File objects that are slices of the
// tar, read from disk only when read. The release's .tar is GNU tar, made by
// omarchy-mobile's tools/lib/bundle-tar.sh: ustar headers, GNU long names
// ('L'), pax headers ('x') read for a path and a size.
export async function readTar(tar) {
  const out = new Map();
  const text = (b, from, to) => {
    const s = new TextDecoder().decode(b.subarray(from, to));
    const nul = s.indexOf("\0");
    return nul < 0 ? s : s.slice(0, nul);
  };
  const octal = (b, from, to) => {
    if (b[from] & 0x80) {
      // base-256, for a member of 8 GiB or more
      let n = 0;
      for (let i = from + 1; i < to; i++) n = n * 256 + b[i];
      return n;
    }
    const s = text(b, from, to).trim();
    if (!/^[0-7]*$/.test(s)) throw fail("bundle", `${tar.name} is not a tar file this page can read`);
    return s ? parseInt(s, 8) : 0;
  };
  let at = 0;
  let longName = null;
  let pax = {};
  while (at + 512 <= tar.size) {
    const h = new Uint8Array(await tar.slice(at, at + 512).arrayBuffer());
    if (h.every((b) => b === 0)) break;
    let sum = 0;
    for (let i = 0; i < 512; i++) sum += i >= 148 && i < 156 ? 32 : h[i];
    if (sum !== octal(h, 148, 156)) throw fail("bundle", `${tar.name} is not a tar file this page can read`);
    const type = String.fromCharCode(h[156] || 48);
    const length = pax.size !== undefined ? Number(pax.size) : octal(h, 124, 136);
    const data = at + 512;
    const prefix = text(h, 345, 500);
    let name = longName ?? pax.path ?? (prefix ? `${prefix}/${text(h, 0, 100)}` : text(h, 0, 100));
    if (type === "L") {
      longName = text(new Uint8Array(await tar.slice(data, data + length).arrayBuffer()), 0, length);
    } else if (type === "x") {
      const body = new TextDecoder().decode(await tar.slice(data, data + length).arrayBuffer());
      pax = {};
      for (const line of body.split("\n")) {
        const m = /^[0-9]+ ([^=]+)=(.*)$/.exec(line);
        if (m) pax[m[1]] = m[2];
      }
    } else {
      if (type === "0") {
        const base = basename(name.replace(/\/+$/, ""));
        if (out.has(base)) throw fail("bundle", `${tar.name} holds two files called ${base}`);
        out.set(base, new File([tar.slice(data, data + length)], base));
      }
      longName = null;
      pax = {};
    }
    at = data + Math.ceil(length / 512) * 512;
    name = null;
  }
  return out;
}
