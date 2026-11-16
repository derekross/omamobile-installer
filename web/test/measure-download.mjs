// SPDX-License-Identifier: MIT
// How fast the page's download code goes, in node, with no network in the
// way: a made-up release with a root image of SIZE MiB of random bytes,
// signed with a key gpg makes for this run alone (in a directory under
// target/, deleted afterwards), served by the stand-in for the release server
// on 127.0.0.1, downloaded into the stand-in for the browser's file system
// on this computer's disk, and checked by the module as the page checks it.
// Three runs: a whole download, one cut at half and gone on with, and a
// later visit that downloads nothing and checks what was kept.
//
// It says what the page's own code costs (the module's SHA-256, the parts,
// the writes). It says nothing about a browser's file system or a real
// network: docs/WEB-WRITE.md §6 lists what only a real browser can answer.
//
//   web/build.sh web-write-preview selftest && node web/test/measure-download.mjs [SIZE]
import { execFileSync } from "node:child_process";
import { createHash, randomFillSync } from "node:crypto";
import { closeSync, mkdirSync, openSync, readFileSync, rmSync, writeSync } from "node:fs";
import { StandInStorage } from "./opfs-stand-in.mjs";
import { fixtureRelease, startReleaseServer } from "./release-server.mjs";
import { downloadRelease, fetchSmall, findLatest, largeFiles, Store } from "../static/release-fetch.js";

const root = new URL("../..", import.meta.url).pathname;
const pkg = `${root}web/dist-preview-test/pkg/`;
const core = await import(`${pkg}omamobile_installer_web.js`);
core.initSync({ module: readFileSync(`${pkg}omamobile_installer_web_bg.wasm`) });

const mib = Number(process.argv[2] ?? 1024);
const work = `${root}target/measure-download`;
rmSync(work, { recursive: true, force: true });
mkdirSync(`${work}/gnupg`, { recursive: true, mode: 0o700 });

// The root image: random bytes, written a MiB at a time
const rootPath = `${work}/root.img.zst`;
const hash = createHash("sha256");
const fd = openSync(rootPath, "w");
const block = Buffer.alloc(1 << 20);
for (let i = 0; i < mib; i++) {
  randomFillSync(block);
  hash.update(block);
  writeSync(fd, block);
}
closeSync(fd);
const rootSha = hash.digest("hex");

// The release: the test release's files, with this root image, this key
const gpg = (...args) => execFileSync("gpg", ["--homedir", `${work}/gnupg`, "--batch", "--quiet", "--pinentry-mode", "loopback", "--passphrase", "", ...args]);
gpg("--quick-gen-key", "made-up release key for a measurement <measure@example.invalid>", "ed25519", "sign", "never");
const fingerprint = /^fpr:+([0-9A-F]{40}):/m.exec(gpg("--with-colons", "--fingerprint").toString())[1];
const release = fixtureRelease();
const name = release.root;
const manifest = JSON.parse(release.files.get("manifest.json").bytes);
manifest.root_image = { ...manifest.root_image, sha256: rootSha, size: mib << 20 };
const text = release.files
  .get("MANIFEST")
  .bytes.toString()
  .replace(/^root image: .*$/m, `root image: ${name} (${rootSha})`);
release.files.set("MANIFEST", { bytes: Buffer.from(text) });
release.files.set("omamobile.gpg", { bytes: gpg("--export", fingerprint) });
const sha = (b) => createHash("sha256").update(b).digest("hex");
for (const f of manifest.files) {
  const b = release.files.get(f.path).bytes;
  Object.assign(f, { sha256: sha(b), size: b.length });
}
release.files.set("manifest.json", { bytes: Buffer.from(JSON.stringify(manifest, null, 2) + "\n") });
const covered = [...manifest.files.map((f) => f.path), "manifest.json"].sort();
const sums = covered.map((p) => `${sha(release.files.get(p).bytes)}  ${p}\n`).join("");
release.files.set("SHA256SUMS", { bytes: Buffer.from(sums) });
execFileSync("gpg", ["--homedir", `${work}/gnupg`, "--batch", "--quiet", "--digest-algo", "SHA256", "--detach-sign", "--output", `${work}/SHA256SUMS.sig`, "-"], { input: sums });
release.files.set("SHA256SUMS.sig", { bytes: readFileSync(`${work}/SHA256SUMS.sig`) });
release.files.set(name, { path: rootPath });

const server = await startReleaseServer({ release });
mkdirSync(`${work}/opfs`);
const storage = new StandInStorage({ dir: `${work}/opfs` });
const out = { root_image_mib: mib, part_mib: 32 };
async function run(mode, label) {
  const pointer = await findLatest("cheetah", { base: server.base(mode) });
  const small = await fetchSmall(pointer);
  const verified = core.selftest_verify_release(fingerprint, ...small.files);
  const files = largeFiles(JSON.parse(verified.describe()), small.files[3]);
  const store = await Store.open(await storage.getDirectory(), "cheetah", pointer.version);
  const written = storage.bytesWritten;
  const t = performance.now();
  const got = await downloadRelease({ release: verified, files, base_url: pointer.base_url, store, storage, retryMs: [0] });
  const s = (performance.now() - t) / 1000;
  out[`${label}_s`] = Number(s.toFixed(2));
  out[`${label}_mib_per_s`] = Math.round(mib / s);
  out[`${label}_written_mib`] = Math.round((storage.bytesWritten - written) / (1 << 20));
  out[`${label}_root_parts`] = (await store.parts(name)).length;
  return got;
}
try {
  await run("ok", "whole");
  await Store.clear(await storage.getDirectory());
  await run("cut", "cut_at_half");
  await run("ok", "later_visit");
  out.root_gets = server.asked.filter((a) => a.method === "GET" && a.path.endsWith(name)).map((a) => `${a.mode} ${a.range ?? "whole"}`);
  console.log(JSON.stringify(out, null, 1));
} finally {
  await server.close();
  rmSync(work, { recursive: true, force: true });
}
