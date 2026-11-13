// SPDX-License-Identifier: MIT
// For the write preview's tests in node: the page's wasm module as
// `web/build.sh web-write-preview selftest` built it, a pretend
// `navigator.usb`, the signed test bundle's files as the files a person
// would pick, and the steps of the page up to the dry run. No USB, no
// browser: node has neither.
import { readFileSync } from "node:fs";
import { FakePhone, FakeUsb } from "./fake-usb.mjs";
import { match, walk } from "../write-walk.js";

const root = new URL("../../..", import.meta.url).pathname;
const pkg = `${root}web/dist-preview-test/pkg/`;
export const core = await import(`${pkg}omamobile_installer_web.js`);
core.initSync({ module: readFileSync(`${pkg}omamobile_installer_web_bg.wasm`) });

export const fixture = (name) => new Uint8Array(readFileSync(`${root}fixtures/selftest/${name}`));

// `navigator.usb` for the module: the chooser in which the pretend owner
// picks the pretend phone
export const usb = new FakeUsb();
Object.defineProperty(globalThis.navigator, "usb", { value: usb, configurable: true });

export const screen = (flow) => JSON.parse(flow.screen());
export const words = (flow) => {
  const s = screen(flow);
  return [s.title, s.notice ?? "", ...s.body, ...s.plan, s.problem ?? "", ...s.footer].join("\n");
};

// The release's files as a person would pick them: File objects, by the
// names the verified release gives
export function releaseFiles(flow, change = {}) {
  const release = JSON.parse(flow.release());
  const file = (source, name) => new File([change[name]?.(fixture(source)) ?? fixture(source)], name);
  const files = [...release.boot_images.map((b) => file(b.source, b.source.split("/").pop())), file("root.img.zst", release.root_image.name)];
  const found = match(files, release);
  if (found.problems.length) throw new Error(found.problems.join(" "));
  return found;
}

export const good = ["alex", "4812", "4812", "", "Europe/Paris", "", ""];

// A new pretend phone on the cable and a flow that may write, taken as the
// page takes it to the step named: "phone" (the warning accepted), "dry-run"
// (looked at, factory image and answers in), "confirm" (the dry run walked)
// or "write" (the codename typed).
export async function begin(until = "dry-run", { phone = new FakePhone(), reinstall = false } = {}) {
  usb.phone = phone;
  const flow = core.InstallFlow.selftest_write_preview(reinstall);
  const run = { flow, phone, picked: null, files: null };
  flow.acknowledge_warning();
  if (until === "phone") return run;
  run.picked = await core.request_phone();
  await flow.read_phone(run.picked.again());
  if (screen(flow).state !== "factory-image") return run;
  const zip = core.selftest_factory_zip();
  flow.zip_begin(screen(flow).file.name, zip.length);
  flow.zip_update(zip);
  flow.zip_done();
  flow.answers(...good);
  run.files = releaseFiles(flow);
  if (until === "dry-run") return run;
  await walk(flow, run.picked.again(), run.files);
  if (until === "confirm") return run;
  flow.confirm_install("cheetah");
  return run;
}

// A whole install on a phone nothing goes wrong with: what it was sent, as
// the measure for the tests that break one command of it
export async function reference() {
  const run = await begin("write");
  await walk(run.flow, run.picked.again(), run.files);
  return run;
}

// The place in the phone's log of the nth (from 0) command that starts so,
// counted from the first command of the session that writes
export function nth(log, from, starts, n = 0) {
  let seen = 0;
  for (let i = from; i < log.length; i++) {
    if (log[i].startsWith(starts) && seen++ === n) return i;
  }
  throw new Error(`the reference install sent no ${starts} number ${n}`);
}

// Everything this installer may put on the wire, as the core's gate has it
// (core/src/fastboot.rs, `allowed`), written out again here on its own
export const GATE = /^(getvar:[\x20-\x7e]+|flashing get_unlock_ability|download:[0-9a-f]{8}|flash:(boot_b|init_boot_b|vendor_boot_b|vendor_kernel_boot_b|dtbo_b|userdata)|set_active:b|reboot)$/;
export const READS = /^(getvar:[\x20-\x7e]+|flashing get_unlock_ability)$/;

// The `setTimeout` the module finds, replaced by one the test fires by hand,
// so that a wait of minutes takes no time. `restore()` puts the real one back.
export function fakeTimers() {
  const real = { setTimeout: globalThis.setTimeout, clearTimeout: globalThis.clearTimeout };
  const timers = new Map();
  let next = 1;
  globalThis.setTimeout = (fn, ms, ...args) => {
    const id = next++;
    timers.set(id, { fn, ms, args });
    return id;
  };
  globalThis.clearTimeout = (id) => timers.delete(id);
  return {
    timers,
    asked: [],
    // Let the one timer that is waiting run out
    fire() {
      const [[id, timer]] = [...timers];
      timers.delete(id);
      timer.fn(...timer.args);
      return timer.ms;
    },
    restore() {
      Object.assign(globalThis, real);
    },
  };
}

// Until the pretend device holds an IN request nothing answers
export async function untilWaiting(device) {
  for (let i = 0; i < 10000; i++) {
    if (device.pendingIn.length) return;
    await new Promise((resolve) => setImmediate(resolve));
  }
  throw new Error("the page never waited for an answer");
}
