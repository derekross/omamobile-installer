// SPDX-License-Identifier: MIT
// The page's download of a release (web/static/release-fetch.js), in node,
// against a stand-in for the release server on 127.0.0.1
// (release-server.mjs: CORS and Range as the real server sends them) and a
// stand-in for the browser's Origin Private File System (opfs-stand-in.mjs).
// The release is fixtures/selftest, a made-up release signed with a test key
// whose secret was never kept; the module checks it with that key's
// fingerprint (`selftest_verify_release`, self-test builds only), and the
// release keys built into the module refuse it.
//
// Run by web/fetch-test.sh. No browser, no USB, nothing but 127.0.0.1.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import { StandInStorage } from "./opfs-stand-in.mjs";
import { startReleaseServer } from "./release-server.mjs";
import { checkPicked, DEFAULTS, downloadRelease, fetchSmall, findLatest, fromPicked, largeFiles, readTar, ReleaseError, Store } from "../static/release-fetch.js";
// The write preview's own test harness: the module of the preview's test
// build, a pretend phone behind a pretend WebUSB device, the flow
import { begin, core, screen, words } from "../preview/test/harness.mjs";
import { match, walk } from "../preview/write-walk.js";

const root = new URL("../..", import.meta.url).pathname;
const fixture = (name) => new Uint8Array(readFileSync(`${root}fixtures/selftest/${name}`));
const server = await startReleaseServer();
const storages = [];
after(async () => {
  await server.close();
  for (const s of storages) s.remove();
});
const newStorage = (options) => {
  const s = new StandInStorage(options);
  storages.push(s);
  return s;
};
// A browser's fetch refuses to be called as a method of another object
// ("Illegal invocation"); node's does not care. This one refuses as a
// browser's does, so that the tests catch such a call
function browserFetch(...args) {
  if (this !== undefined && this !== globalThis) throw new TypeError("Illegal invocation");
  return fetch(...args);
}
// Small parts, so that the test root image (1.6 MB) spans many of them
const PART = 64 << 10;
const quick = { partBytes: PART, stallMs: 300, retryMs: [5], sleep: (ms) => new Promise((r) => setTimeout(r, ms)) };

// The release as the page gets it: the pointer, the small files, the
// module's check, the list of large files
async function release(mode = "ok") {
  const pointer = await findLatest("cheetah", { base: server.base(mode), fetch: browserFetch });
  const small = await fetchSmall(pointer, { fetch: browserFetch });
  const verified = core.selftest_verify_release(undefined, ...small.files);
  const files = largeFiles(JSON.parse(verified.describe()), small.files[3]);
  return { pointer, small, verified, files };
}
const expected = (f) => (f.kind === "root" ? fixture("root.img.zst") : fixture(f.path));
async function download(mode, storage, extra = {}) {
  const r = await release(mode);
  const store = await Store.open(await storage.getDirectory(), "cheetah", r.pointer.version);
  const events = [];
  const got = await downloadRelease({ release: r.verified, files: r.files, base_url: r.pointer.base_url, store, storage, fetch: browserFetch, onEvent: (e) => events.push(e), ...quick, ...extra });
  return { ...r, store, got, events };
}
const asked = (mode, path) => server.asked.filter((a) => a.mode === mode && a.path === path && a.method === "GET");
const rootName = () => server.release.root;
async function same(got, files) {
  assert.equal(got.length, files.length);
  for (const [i, f] of files.entries()) {
    assert.equal(got[i].name, f.name);
    assert.deepEqual(new Uint8Array(await got[i].arrayBuffer()), expected(f), `${f.name} is the release's file`);
  }
}
const refuses = async (promise, kind, words) => {
  await assert.rejects(promise, (e) => {
    assert.ok(e instanceof ReleaseError, `a ReleaseError, not ${e}`);
    assert.equal(e.kind, kind, e.message);
    if (words) assert.match(e.message, words);
    return true;
  });
};

test("the stand-in answers as the real server is set up: CORS for any page, the range headers exposed, a preflight, byte ranges", async () => {
  const url = `${server.base("probe")}cheetah/${server.release.version}/${rootName()}`;
  const pre = await fetch(url, { method: "OPTIONS" });
  assert.equal(pre.status, 204);
  assert.match(pre.headers.get("access-control-allow-headers"), /Range/);
  const part = await fetch(url, { headers: { Range: "bytes=10-" } });
  assert.equal(part.status, 206);
  assert.equal(part.headers.get("access-control-allow-origin"), "*");
  assert.match(part.headers.get("access-control-expose-headers"), /Content-Range/);
  assert.equal(part.headers.get("content-range"), `bytes 10-${fixture("root.img.zst").length - 1}/${fixture("root.img.zst").length}`);
  await part.body.cancel();
});

test("the latest release is found from the server's index, and its signed small files pass the module's check", async () => {
  const r = await release();
  assert.equal(r.pointer.version, "2026.01.02");
  assert.equal(r.pointer.base_url, `${server.base("ok")}cheetah/2026.01.02/`);
  assert.equal(JSON.parse(r.verified.describe()).version, "2026.01.02");
  assert.ok(r.small.index, "the release's build index came with it");
  assert.deepEqual(
    r.files.map((f) => f.name),
    ["boot.img", "init_boot.img", "vendor_boot.img", "vendor_kernel_boot.img", "dtbo.img", rootName()],
  );
  // The release keys built into the module refuse the test key
  assert.throws(() => core.verify_release(...r.small.files), /signature/);
});

test("a pointer to anywhere but beside itself is not followed, and a phone with no release is said so", async () => {
  await refuses(findLatest("cheetah", { base: server.base("away") }), "pointer", /not beside itself/);
  assert.ok(!server.asked.some((a) => a.mode === "away" && a.path !== "latest.json" && a.path.includes("2026")), "nothing was fetched from where it pointed");
  await refuses(findLatest("cheetah", { base: server.base("nolatest") }), "no-release", /no release for cheetah/);
  await refuses(findLatest("../cheetah", { base: server.base("ok") }), "pointer");
});

test("a whole download: every file once, without a range, kept, checked, and the same bytes as the release's", async () => {
  const storage = newStorage();
  const d = await download("ok", storage);
  await same(d.got, d.files);
  for (const f of d.files) {
    const gets = asked("ok", `${d.pointer.version}/${f.path}`);
    assert.equal(gets.length, 1, f.name);
    assert.equal(gets[0].range, null);
  }
  assert.equal(storage.persistAsked, 1, "persistence was asked for");
  const root = d.files.at(-1);
  assert.equal((await d.store.parts(root.name)).length, Math.ceil(root.size / PART));
  assert.deepEqual((await d.store.record(root.name)).complete, true);
  assert.ok(d.events.some((e) => e.type === "prepared" && e.persisted === true));
});

test("a second visit downloads nothing: what was kept is checked again from the disk and used", async () => {
  const storage = newStorage();
  const first = await download("ok", storage);
  const before = server.asked.length;
  const again = await download("ok", storage);
  await same(again.got, again.files);
  const large = server.asked.slice(before).filter((a) => a.path.includes("/images/") || a.path.endsWith(rootName()));
  assert.deepEqual(large, [], "no large file was fetched again");
  assert.ok(again.events.some((e) => e.type === "kept" && e.name === rootName()), "the kept root image went through the check");
  assert.equal(first.pointer.version, again.pointer.version);
});

test("a download that is cut goes on from the byte it reached, with a range and the file's ETag, and downloads the file once in all", async () => {
  const storage = newStorage();
  const d = await download("cut", storage);
  await same(d.got, d.files);
  const gets = asked("cut", `${d.pointer.version}/${rootName()}`);
  assert.equal(gets.length, 2);
  assert.equal(gets[0].range, null);
  const from = Number(/^bytes=([0-9]+)-$/.exec(gets[1].range)[1]);
  const size = fixture("root.img.zst").length;
  assert.equal(from, Math.floor(size / 2), "it went on from exactly the bytes that arrived");
  assert.match(gets[1].ifRange, /^"/);
  assert.ok(d.events.some((e) => e.type === "retry" && e.name === rootName()));
  assert.ok(storage.bytesWritten < size + 200000, `about the file's size was written (${storage.bytesWritten})`);
});

test("a transfer that goes quiet ends after the page's wait and goes on the same way", async () => {
  const storage = newStorage();
  const d = await download("stall", storage);
  await same(d.got, d.files);
  const gets = asked("stall", `${d.pointer.version}/${rootName()}`);
  assert.equal(gets.length, 2);
  assert.ok(d.events.some((e) => e.type === "retry" && /no byte arrived/.test(e.why)), JSON.stringify(d.events.filter((e) => e.type === "retry")));
});

test("a tab closed in the middle: the next visit goes on from the last part that was closed", async () => {
  const storage = newStorage();
  const r = await release("ok");
  const store = await Store.open(await storage.getDirectory(), "cheetah", r.pointer.version);
  const root = r.files.at(-1);
  const at = Math.floor(root.size * 0.6);
  await refuses(
    downloadRelease({
      release: r.verified,
      files: r.files,
      base_url: r.pointer.base_url,
      store,
      storage,
      ...quick,
      onEvent: (e) => {
        if (e.type === "download" && e.name === root.name && e.done >= at) storage.crash();
      },
    }),
    "storage",
  );
  storage.reopen();
  const kept = (await store.parts(root.name)).reduce((n, p) => n + p.file.size, 0);
  assert.ok(kept > 0 && kept % PART === 0 && kept <= at, `only whole closed parts are kept (${kept})`);
  const before = server.asked.length;
  const d = await download("ok", storage);
  await same(d.got, d.files);
  const resumed = server.asked.slice(before).filter((a) => a.path.endsWith(rootName()));
  assert.equal(resumed.length, 1);
  assert.equal(resumed[0].range, `bytes=${kept}-`);
});

test("a root image with one byte changed is refused by the signed checksum and deleted", async () => {
  const storage = newStorage();
  await refuses(download("badsum", storage), "mismatch", /is not the signed release's file/);
  const store = await Store.open(await storage.getDirectory(), "cheetah", "2026.01.02");
  assert.deepEqual(await store.parts(rootName()), [], "nothing of it is kept");
});

test("a boot image with one byte changed is refused and deleted, and nothing after it is fetched", async () => {
  const storage = newStorage();
  await refuses(download("badboot", storage), "mismatch", /boot\.img is not the signed release's file/);
  const store = await Store.open(await storage.getDirectory(), "cheetah", "2026.01.02");
  assert.deepEqual(await store.parts("boot.img"), []);
  assert.equal(asked("badboot", `2026.01.02/${rootName()}`).length, 0);
});

test("a signature that does not verify: the module refuses the release, and nothing large is fetched", async () => {
  const r = await findLatest("cheetah", { base: server.base("badsig") });
  const small = await fetchSmall(r);
  assert.throws(() => core.selftest_verify_release(undefined, ...small.files), /signature/);
  // As the page begins its flow from them: the flow's own stop
  const flow = core.InstallFlow.begin(...small.files, small.index, false);
  assert.equal(screen(flow).stop, "release-not-verified", words(flow));
  assert.equal(server.asked.filter((a) => a.mode === "badsig" && (a.path.includes("images/") || a.path.endsWith(rootName()))).length, 0);
});

test("a server without Range: a cut download starts the file over, and the hash with it", async () => {
  const storage = newStorage();
  const d = await download("norange+cut", storage);
  await same(d.got, d.files);
  const gets = asked("norange+cut", `${d.pointer.version}/${rootName()}`);
  assert.equal(gets.length, 2);
  assert.ok(gets[1].range, "the page asked for the rest");
  assert.ok(d.events.some((e) => e.type === "restart" && e.name === rootName()), "and was sent the whole file, and took it as such");
});

test("an answer for another range than the one asked is not taken", async () => {
  const storage = newStorage();
  await refuses(download("badrange+cut", storage, { tries: 2 }), "server", /another part/);
});

test("more bytes than the signed size: stopped and deleted", async () => {
  const storage = newStorage();
  await refuses(download("longer", storage), "mismatch", /more than the/);
  const store = await Store.open(await storage.getDirectory(), "cheetah", "2026.01.02");
  assert.deepEqual(await store.parts(rootName()), []);
});

test("too little room: refused before the first large byte, with the numbers", async () => {
  const storage = newStorage({ quota: 1 << 20 });
  const before = server.asked.length;
  await refuses(download("ok", storage), "quota", /allows this page .* more on this computer, and the release needs/);
  assert.deepEqual(
    server.asked.slice(before).filter((a) => a.path.includes("images/") || a.path.endsWith(rootName())),
    [],
  );
});

test("persistence refused: said, and the download goes on", async () => {
  const storage = newStorage({ persist: false });
  const d = await download("ok", storage);
  assert.ok(d.events.some((e) => e.type === "prepared" && e.persisted === false));
  await same(d.got, d.files);
});

test("a new version replaces the kept one", async () => {
  const storage = newStorage();
  const dir = await storage.getDirectory();
  const old = await Store.open(dir, "cheetah", "2025.12.31");
  await old.setRecord("x", { sha256: "0", size: 1 });
  const store = await Store.open(dir, "cheetah", "2026.01.02");
  assert.deepEqual(store.removed, ["2025.12.31"]);
  await Store.clear(dir);
  await assert.rejects(dir.getDirectoryHandle(Store.TOP), { name: "NotFoundError" });
});

// ---- A release saved on this computer ----------------------------------------

function savedRelease(change = {}) {
  const dir = mkdtempSync(join(tmpdir(), "saved-release-"));
  const bundle = join(dir, "omamobile-cheetah-2026.01.02");
  mkdirSync(join(bundle, "images"), { recursive: true });
  for (const [path, entry] of server.release.files) {
    const target = path === rootName() ? join(dir, path) : join(bundle, path);
    writeFileSync(target, change[path]?.(entry.bytes) ?? entry.bytes);
  }
  return dir;
}
const filesIn = (dir) =>
  execFileSync("find", [dir, "-type", "f"], { encoding: "utf8" })
    .trim()
    .split("\n")
    .map((p) => new File([readFileSync(p)], p.split("/").pop()));

test("a saved release's files, picked: found by name, the small files verify, every large file is checked", async () => {
  const dir = savedRelease();
  try {
    const picked = await fromPicked(filesIn(dir));
    const verified = core.selftest_verify_release(undefined, ...picked.files);
    const files = largeFiles(JSON.parse(verified.describe()), picked.files[3]);
    const got = await checkPicked(verified, files, picked.byName, { readBytes: PART });
    await same(got, files);
  } finally {
    rmSync(dir, { recursive: true });
  }
});

test("a saved release's .tar, as the release publishes it (GNU tar): its members are read in place", async () => {
  const dir = savedRelease();
  try {
    const tar = join(dir, "omamobile-cheetah-2026.01.02.tar");
    execFileSync("tar", ["--format=gnu", "--sort=name", "--owner=0", "--group=0", "-C", dir, "-cf", tar, "omamobile-cheetah-2026.01.02", rootName()]);
    const file = new File([readFileSync(tar)], "omamobile-cheetah-2026.01.02.tar");
    const members = await readTar(file);
    assert.ok(members.has("SHA256SUMS") && members.has("boot.img") && members.has(rootName()));
    const picked = await fromPicked([file]);
    const verified = core.selftest_verify_release(undefined, ...picked.files);
    const files = largeFiles(JSON.parse(verified.describe()), picked.files[3]);
    await same(await checkPicked(verified, files, picked.byName), files);
    await refuses(readTar(new File([new Uint8Array(2048).fill(7)], "x.tar")), "bundle", /not a tar file/);
  } finally {
    rmSync(dir, { recursive: true });
  }
});

test("a saved release with a changed root image, or without its signature, is refused", async () => {
  const flipped = savedRelease({ [rootName()]: (b) => Buffer.from(b).fill(1, 100, 101) });
  try {
    const picked = await fromPicked(filesIn(flipped));
    const verified = core.selftest_verify_release(undefined, ...picked.files);
    const files = largeFiles(JSON.parse(verified.describe()), picked.files[3]);
    await refuses(checkPicked(verified, files, picked.byName), "mismatch");
    await refuses(fromPicked(filesIn(flipped).filter((f) => f.name !== "SHA256SUMS.sig")), "bundle", /SHA256SUMS\.sig is not among/);
  } finally {
    rmSync(flipped, { recursive: true });
  }
});

// ---- Into the write preview's walk ------------------------------------------

test("downloaded into the file system, the release goes through the write preview's walk to a pretend phone, read from the kept parts", async () => {
  const storage = newStorage();
  const d = await download("cut", storage);
  const run = await begin("dry-run");
  const files = match(d.got, JSON.parse(run.flow.release()));
  assert.deepEqual(files.problems, []);
  assert.equal(await walk(run.flow, run.picked.again(), files), true, words(run.flow));
  assert.equal(screen(run.flow).state, "confirm", words(run.flow));
  run.flow.confirm_install("cheetah");
  assert.equal(await walk(run.flow, run.picked.again(), files), true, words(run.flow));
  assert.equal(screen(run.flow).state, "installed", words(run.flow));
  const flashed = run.phone.notReads().filter((c) => c.startsWith("flash:"));
  assert.deepEqual(flashed.slice(0, 5), ["flash:boot_b", "flash:init_boot_b", "flash:vendor_boot_b", "flash:vendor_kernel_boot_b", "flash:dtbo_b"]);
  assert.ok(flashed.length > 5 && flashed.slice(5).every((c) => c === "flash:userdata"));
  assert.equal(run.phone.activeSlot, "b");
  // userdata holds the root image the stand-in served, unpacked
  assert.deepEqual(new Uint8Array(await d.got.at(-1).arrayBuffer()), fixture("root.img.zst"));
});

test("the defaults are what docs/WEB-WRITE.md says", () => {
  assert.equal(DEFAULTS.partBytes, 32 << 20);
  assert.equal(DEFAULTS.stallMs, 60000);
  assert.equal(DEFAULTS.tries, 5);
});
