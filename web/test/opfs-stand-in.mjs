// SPDX-License-Identifier: MIT
// A stand-in for the browser's Origin Private File System and
// `navigator.storage`, for the tests of the page's download in node, which
// has neither. It keeps files in a directory on disk and has the part of the
// API the page uses (web/static/release-fetch.js): directory handles with
// `getDirectoryHandle`, `getFileHandle`, `removeEntry` and `entries`, file
// handles with `getFile` and `createWritable`.
//
// As in Chromium, a writable writes to a file of its own and replaces the
// file only on `close()`; `abort()` drops it. `crash()` is a tab that is
// closed or crashes: every open writable is dropped and every later call on
// one fails, so what was not closed is lost, as it would be.
import { mkdirSync, mkdtempSync, openAsBlob, readdirSync, renameSync, rmSync, statSync, writeFileSync, appendFileSync, unlinkSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const notFound = (name) => Object.assign(new Error(`${name}: not found`), { name: "NotFoundError" });

export class StandInStorage {
  constructor({ quota = 64 * 2 ** 30, persist = true, dir = mkdtempSync(join(tmpdir(), "opfs-stand-in-")) } = {}) {
    this.path = dir;
    this.quota = quota;
    this.allowPersist = persist;
    this.persistAsked = 0;
    this.crashed = false;
    this.open = new Set();
    this.bytesWritten = 0;
  }
  async getDirectory() {
    return new Dir(this, this.path, "");
  }
  async persist() {
    this.persistAsked++;
    return this.allowPersist;
  }
  async persisted() {
    return false;
  }
  async estimate() {
    return { quota: this.quota, usage: used(this.path) };
  }
  // The tab is gone: nothing that was not closed is kept
  crash() {
    this.crashed = true;
    for (const w of this.open) w.drop();
    this.open.clear();
  }
  // A new visit to the page, over the same files
  reopen() {
    this.crashed = false;
  }
  remove() {
    rmSync(this.path, { recursive: true, force: true });
  }
}

function used(path) {
  let n = 0;
  for (const e of readdirSync(path, { withFileTypes: true })) {
    const p = join(path, e.name);
    n += e.isDirectory() ? used(p) : statSync(p).size;
  }
  return n;
}

class Dir {
  constructor(storage, path, name) {
    this.storage = storage;
    this.path = path;
    this.name = name;
    this.kind = "directory";
  }
  async getDirectoryHandle(name, { create = false } = {}) {
    const p = join(this.path, name);
    if (!existsSync(p)) {
      if (!create) throw notFound(name);
      mkdirSync(p);
    }
    return new Dir(this.storage, p, name);
  }
  async getFileHandle(name, { create = false } = {}) {
    const p = join(this.path, name);
    if (!existsSync(p)) {
      if (!create) throw notFound(name);
      writeFileSync(p, "");
    }
    return new FileHandle(this.storage, p, name);
  }
  async removeEntry(name, { recursive = false } = {}) {
    const p = join(this.path, name);
    if (!existsSync(p)) throw notFound(name);
    rmSync(p, { recursive });
  }
  async *entries() {
    for (const e of readdirSync(this.path, { withFileTypes: true })) {
      if (e.name.includes(".swap-")) continue;
      const p = join(this.path, e.name);
      yield [e.name, e.isDirectory() ? new Dir(this.storage, p, e.name) : new FileHandle(this.storage, p, e.name)];
    }
  }
}

let swaps = 0;
class FileHandle {
  constructor(storage, path, name) {
    this.storage = storage;
    this.path = path;
    this.name = name;
    this.kind = "file";
  }
  // A File read from disk when it is read
  async getFile() {
    if (!existsSync(this.path)) throw notFound(this.name);
    return new File([await openAsBlob(this.path)], this.name);
  }
  async createWritable({ keepExistingData = false } = {}) {
    if (this.storage.crashed) throw new Error("the page is gone");
    const swap = `${this.path}.swap-${++swaps}`;
    writeFileSync(swap, "");
    if (keepExistingData) writeFileSync(swap, await (await this.getFile()).bytes());
    const storage = this.storage;
    const target = this.path;
    let over = false;
    const w = {
      async write(data) {
        if (over || storage.crashed) throw new Error("the writable is closed");
        const bytes = typeof data === "string" ? Buffer.from(data) : Buffer.from(data.buffer ?? data, data.byteOffset ?? 0, data.byteLength ?? data.length);
        if (used(storage.path) + bytes.length > storage.quota) throw Object.assign(new Error("over the quota"), { name: "QuotaExceededError" });
        appendFileSync(swap, bytes);
        storage.bytesWritten += bytes.length;
      },
      async close() {
        if (over || storage.crashed) throw new Error("the writable is closed");
        over = true;
        storage.open.delete(w);
        renameSync(swap, target);
      },
      async abort() {
        w.drop();
      },
      drop() {
        over = true;
        storage.open.delete(w);
        if (existsSync(swap)) unlinkSync(swap);
      },
    };
    storage.open.add(w);
    return w;
  }
}
