// SPDX-License-Identifier: MIT
// The write preview: the release's files through the session the core's flow
// opened. Not in the page as it ships (docs/WEB-WRITE.md).
//
// This is the page's counterpart of the command-line tool's walk of the
// images, and it has no document in it, so the tests run it as it is, in
// node, against a pretend phone at the USB level (web/preview/test).
//
// What it decides: nothing about the phone. It reads the files the person
// picked and hands their bytes to the wasm module, which checks each against
// the signed manifest, lets the core's flow open the session, and sends what
// the core's session cuts. How many bytes go to the phone at a time is the
// core's; READ_BYTES below is only how much of a file is read at a time.
// When the session returns an error the module hands it to the flow itself,
// and the functions here return false: the flow's screen then says what
// happened and how far the writes got.

// How much of a picked file is read into memory at a time. It says nothing
// about what is sent: the core cuts the root image into the pieces a phone
// is sent, and hands each to the transport in its own pieces.
const READ_BYTES = 1 << 20;

// The files a release is made of, by the names its verified manifest gives
// (the module's `release()`): the boot images in the plan's order, then the
// root image.
export function wanted(release) {
  return {
    boot: release.boot_images.map((image) => ({ name: image.source.split("/").pop(), size: image.size, partition: image.partition })),
    root: { name: release.root_image.name, size: release.root_image.size },
  };
}

// The picked files that are the release's, by name and size. `problems`:
// what is missing or is not the size the manifest lists, in words about the
// files (never about the phone). A file of another size is not read at all.
export function match(files, release) {
  const want = wanted(release);
  const problems = [];
  const find = ({ name, size }) => {
    const file = [...files].find((f) => f.name === name);
    if (!file) problems.push(`${name} is not among the files you picked.`);
    else if (file.size !== size) problems.push(`${name} is ${file.size} bytes, and the release lists ${size}: it is not the release's file, or it is not complete.`);
    else return file;
    return null;
  };
  const boot = want.boot.map(find);
  const root = find(want.root);
  return { boot, root, problems };
}

const whole = async (file) => new Uint8Array(await file.arrayBuffer());

// A file, a piece at a time, never whole in memory
async function* pieces(file) {
  for (let at = 0; at < file.size; at += READ_BYTES) {
    yield new Uint8Array(await file.slice(at, at + READ_BYTES).arrayBuffer());
  }
}

// Every file against the signed manifest, before a session is opened. Throws
// with the core's words for a file that is not the release's; nothing was
// sent to the phone for it.
export async function checkFiles(flow, files, report = () => {}) {
  for (const [index, file] of files.boot.entries()) {
    report({ phase: "checking", file: file.name });
    flow.images_check_boot(index, await whole(file));
  }
  flow.images_root_begin();
  let done = 0;
  for await (const chunk of pieces(files.root)) {
    flow.images_root_update(chunk);
    done += chunk.length;
    report({ phase: "checking", file: files.root.name, done, of: files.root.size });
  }
  flow.images_root_done();
}

// The step the flow is on, the dry run or the write: check every file, let
// the flow open its session on `phone`, and hand the files through it.
// `true`: the session ran through and is back with the flow. `false`: the
// flow stopped, and its screen says why. An exception: a file was not the
// release's or the page was used out of order, before anything was sent.
export async function walk(flow, phone, files, report = () => {}) {
  await checkFiles(flow, files, report);
  if (!(await flow.session_open(phone))) return false;
  // A file that can no longer be read ends the session through the flow, so
  // that its stop says how far the writes got
  const read = async (what) => {
    try {
      return await what();
    } catch (e) {
      flow.session_abandon(e.message ?? String(e));
      return null;
    }
  };
  const progress = () => JSON.parse(flow.session_progress());
  for (const [index, file] of files.boot.entries()) {
    const bytes = await read(() => whole(file));
    if (!bytes) return false;
    if (!(await flow.session_boot(index, bytes))) return false;
    report({ phase: "boot", file: file.name, ...progress() });
  }
  const chunks = pieces(files.root);
  for (;;) {
    const next = await read(() => chunks.next());
    if (!next) return false;
    if (next.done) break;
    // Awaited before the next read: the module sends every piece of
    // userdata this completes before it returns
    if (!(await flow.session_root_feed(next.value))) return false;
    report({ phase: "userdata", ...progress() });
  }
  return await flow.session_finish();
}
