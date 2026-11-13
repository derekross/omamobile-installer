// SPDX-License-Identifier: MIT
// The write preview, through the page's own code path, against a pretend
// phone behind a pretend WebUSB device (fake-usb.mjs). Run by
// web/preview-test.sh, in node: no browser, no USB.
//
// The path under test is the one the preview page takes: the module's
// chooser (`request_phone`), its WebUSB transport, the core's flow with the
// write switch on, and the page's walk of the release's files
// (web/preview/write-walk.js) through the session the flow opens. What node
// cannot run is the page's drawing (app.js, preview.js); the page's run in a
// headless browser covers that (page-run.mjs, the second part of
// web/preview-test.sh).
import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { zstdDecompressSync } from "node:zlib";
import { EP_IN, EP_OUT, FakePhone, INTERFACE, OTHER_SERIAL, PACKET, SENTINEL } from "./fake-usb.mjs";
import { begin, core, fakeTimers, fixture, GATE, good, nth, READS, reference, releaseFiles, screen, untilWaiting, usb, words } from "./harness.mjs";
import { checkFiles, match, walk, wanted } from "../write-walk.js";

// One install with nothing wrong: the measure for the rest
const whole = await reference();
const LOG = whole.phone.log;
// Where the session that writes sent its first command that is not a question
const FIRST_WRITE = LOG.findIndex((c) => !READS.test(c));
const at = (starts, n = 0) => nth(LOG, FIRST_WRITE, starts, n);

// Every pretend device a test used, for what holds after each of them
const devices = new Set();
const track = (run) => {
  devices.add(run.phone.usb);
  return run;
};
const started = async (...args) => track(await begin(...args));
track(whole);

afterEach(() => {
  for (const device of devices) {
    // What this installer does with a USB device, and nothing else of WebUSB
    const made = device.made().filter((m) => !["open", "close", "claimInterface", "transferOut", "transferIn"].includes(m));
    assert.deepEqual(made, [], "no control transfer, reset, clearHalt or other call");
    for (const call of device.made("transferOut")) {
      assert.equal(call.endpoint, EP_OUT);
      // Nothing but what the core's gate lists ever left as a command
      if (call.during === "command") assert.match(call.command, GATE);
    }
    for (const call of device.made("transferIn")) {
      assert.equal(call.endpoint, EP_IN);
      assert.equal(call.length, PACKET, "one answer's worth, in whole packets");
    }
    for (const call of device.made("claimInterface")) assert.equal(call.number, INTERFACE);
  }
  devices.clear();
  assert.equal(usb.listed, 0, "the page lists no devices: the chooser is the only way to one");
});

const stopped = (flow, id) => {
  const s = screen(flow);
  assert.equal(s.state, "stopped", words(flow));
  assert.equal(s.stop, id, words(flow));
};
const untouched = (phone, ...names) => {
  for (const name of names) assert.ok(phone.partitions.get(name).every((b) => b === SENTINEL), `${name} was written`);
};
// A walk that must end with the flow stopped
const stops = async (run) => assert.equal(await walk(run.flow, run.picked.again(), run.files), false);

test("a whole pretend install through the page's path writes slot B's images and userdata, and nothing else", () => {
  const { flow, phone } = whole;
  assert.equal(screen(flow).state, "installed", words(flow));
  assert.match(words(flow), /Every image was written, and the phone confirmed each one: boot_b, init_boot_b, vendor_boot_b, vendor_kernel_boot_b, dtbo_b, userdata\./);

  // The chooser: the browser's own, once, with the one filter
  assert.deepEqual(usb.requests.at(-1), { filters: [{ classCode: 0xff, subclassCode: 0x42, protocolCode: 0x03 }] });

  // What the phone received that is not a question, whole and in order
  const release = JSON.parse(flow.release());
  const sizes = release.boot_images.map((b) => `download:${b.size.toString(16).padStart(8, "0")}`);
  const sent = phone.notReads();
  const boots = release.boot_images.flatMap((b, i) => [sizes[i], `flash:${b.partition}`]);
  assert.deepEqual(sent.slice(0, boots.length), boots);
  const rest = sent.slice(boots.length);
  assert.deepEqual(rest.slice(-2), ["set_active:b", "reboot"]);
  const userdata = rest.slice(0, -2);
  assert.ok(userdata.length >= 2 && userdata.length % 2 === 0);
  userdata.forEach((command, i) => (i % 2 ? assert.equal(command, "flash:userdata") : assert.match(command, /^download:[0-9a-f]{8}$/)));
  // The dry run before it sent questions only
  assert.ok(LOG.slice(0, FIRST_WRITE).every((c) => READS.test(c)));
  // The serial number and the product, asked again before every pair and before set_active:b
  for (const [i, command] of LOG.entries()) {
    if (i >= FIRST_WRITE && (command.startsWith("download:") || command === "set_active:b")) {
      assert.deepEqual(LOG.slice(i - 2, i), ["getvar:serialno", "getvar:product"]);
    }
  }

  // What is on the phone: each image where it belongs, the root image unpacked
  for (const b of release.boot_images) {
    const image = fixture(b.source);
    assert.deepEqual(phone.partitions.get(b.partition).subarray(0, image.length), image, b.partition);
  }
  const raw = new Uint8Array(zstdDecompressSync(fixture("root.img.zst")));
  assert.deepEqual(phone.partitions.get("userdata").subarray(0, raw.length), raw);
  assert.deepEqual(phone.written().sort(), ["boot_b", "dtbo_b", "init_boot_b", "userdata", "vendor_boot_b", "vendor_kernel_boot_b"]);
  assert.equal(phone.activeSlot, "b");
  assert.ok(phone.rebooted);
  untouched(phone, "boot_a", "init_boot_a", "vbmeta_a", "vbmeta_b", "bootloader_a", "bootloader_b", "modem_a", "modem_b", "persist", "efs", "efs_backup", "devinfo", "misc");
});

test("every download is the size the core's session cut, sent in the core's pieces and in no others", async () => {
  // A phone that takes 4 MiB at a time: the root image is then one piece of
  // more than a megabyte, which the core hands to a transport in pieces of
  // 1 MiB (fastboot::SEND_CHUNK). The page's transport passes each on whole
  const phone = new FakePhone();
  phone.maxDownload = 4 << 20;
  const run = await started("write", { phone });
  const before = phone.usb.calls.length;
  assert.equal(await walk(run.flow, run.picked.again(), run.files), true);
  assert.equal(screen(run.flow).state, "installed");
  const calls = phone.usb.calls.slice(before).filter((c) => c.method === "transferOut");
  const downloads = [];
  for (const call of calls) {
    if (call.during === "command" && call.command.startsWith("download:")) downloads.push({ size: parseInt(call.command.slice(9), 16), data: [] });
    else if (call.during === "data") downloads.at(-1).data.push(call.length);
  }
  assert.equal(downloads.length, 6, "five boot images and the root image in one piece");
  for (const { size, data } of downloads) {
    assert.ok(size <= phone.maxDownload);
    // 1 MiB, 1 MiB, ..., the rest: the core's cut, with nothing re-cut by the page
    const expected = [];
    for (let left = size; left > 0; left -= 1 << 20) expected.push(Math.min(left, 1 << 20));
    assert.deepEqual(data, expected);
  }
  assert.ok(downloads.at(-1).data.length > 1, "the root image's piece took more than one transfer");
  const raw = new Uint8Array(zstdDecompressSync(fixture("root.img.zst")));
  assert.deepEqual(phone.partitions.get("userdata").subarray(0, raw.length), raw);
});

test("the session reports each piece of userdata, with the blocks its flash covered and how long it took", async () => {
  const run = await started("write");
  const events = [];
  await walk(run.flow, run.picked.again(), run.files, (e) => events.push(e));
  const last = events.filter((e) => e.phase === "userdata").at(-1);
  assert.equal(last.writes, true);
  assert.equal(last.blocks_done, last.blocks);
  assert.equal(last.pieces.length, run.phone.log.filter((c) => c === "flash:userdata").length - 1, "all but the piece the stream's end releases");
  for (const piece of last.pieces) {
    assert.ok(piece.blocks >= 1 && piece.blocks <= 65536);
    assert.ok(piece.bytes <= run.phone.maxDownload);
    assert.equal(typeof piece.flash_ms, "number");
  }
});

test("a phone whose serial number is not the chosen device's is not looked at further", async () => {
  // The browser reports one serial number for the device the person picked,
  // and the phone answers getvar:serialno with another
  const phone = new FakePhone();
  phone.usb.serialNumber = OTHER_SERIAL;
  const run = await started("phone", { phone });
  const picked = await core.request_phone();
  await assert.rejects(run.flow.read_phone(picked.again()), /the phone that answered is not the one that was chosen \(serial numbers differ\)/);
  assert.equal(screen(run.flow).state, "phone");
  assert.ok(phone.log.every((c) => READS.test(c)));
});

test("a device that reports no serial number cannot be pinned, and is asked nothing", async () => {
  const phone = new FakePhone();
  phone.usb.serialNumber = undefined;
  const run = await started("phone", { phone });
  const picked = await core.request_phone();
  await assert.rejects(run.flow.read_phone(picked.again()), /tells no serial number/);
  assert.deepEqual(phone.log, []);
  assert.deepEqual(phone.usb.made(), []);
});

test("another phone at the dry run stops the install, with nothing but questions sent", async () => {
  const run = await started("dry-run");
  run.phone.vars.set("serialno", OTHER_SERIAL);
  await stops(run);
  stopped(run.flow, "phone-changed");
  assert.deepEqual(run.phone.notReads(), []);
});

test("another phone after the codename is not written", async () => {
  const run = await started("write");
  run.phone.vars.set("serialno", OTHER_SERIAL);
  await stops(run);
  stopped(run.flow, "phone-changed");
  assert.deepEqual(run.phone.notReads(), []);
  assert.deepEqual(run.phone.written(), []);
});

test("a serial number that changes between two writes ends the install where it is", async () => {
  const run = await started("write");
  run.phone.swapSerialAfter = at("flash:init_boot_b");
  await stops(run);
  stopped(run.flow, "write-interrupted");
  assert.match(words(run.flow), /the phone changed during the install; nothing more is written/);
  assert.match(words(run.flow), /2 of 5 were written/);
  assert.deepEqual(run.phone.written(), ["boot_b", "init_boot_b"]);
  assert.equal(run.phone.activeSlot, "a");
});

test("a device that vanishes in the middle of a download is the core's stop, and nothing more is sent", async () => {
  const run = await started("write");
  // The cable is pulled 100000 bytes into the first piece of userdata
  run.phone.vanishInData = 100000;
  await stops(run);
  stopped(run.flow, "write-interrupted");
  assert.match(words(run.flow), /USB: sending: NotFoundError: The device was disconnected\./);
  assert.match(words(run.flow), /The connection to the phone was lost\./);
  assert.match(words(run.flow), /5 of 5 were written/);
  assert.match(words(run.flow), /userdata was not written/);
  untouched(run.phone, "userdata");
  assert.equal(run.phone.activeSlot, "a");
  assert.equal(run.phone.notReads().at(-1), "download:0003f060");
  // The link is over: the page hands the device nothing more
  const calls = run.phone.usb.calls.length;
  await assert.rejects(run.flow.session_boot(0, fixture("images/boot.img")), /no session is open/);
  assert.equal(run.phone.usb.calls.length, calls);
});

test("a device that vanishes while it flashes: the stop says the piece is in doubt", async () => {
  const run = await started("write");
  // The second piece of userdata is carried out, and its answer never arrives
  run.phone.loseAnswerAt = at("flash:userdata", 1);
  await stops(run);
  stopped(run.flow, "write-interrupted");
  assert.match(words(run.flow), /USB: receiving: NotFoundError: The device was disconnected\./);
  assert.match(words(run.flow), /it was writing userdata: slot B's boot images are written, and 1 pieces of the system image, with one more that may be partly written/);
  assert.match(words(run.flow), /What Android had stored on the phone is erased/);
  assert.equal(run.phone.activeSlot, "a");
  assert.ok(!run.phone.log.includes("set_active:b"));
});

test("a device that vanishes at each command of the write gives a stop, never an install", async () => {
  // The cable pulled before every command from the first write on, and with
  // the answer lost for every command that is not a question
  for (let index = FIRST_WRITE; index < LOG.length; index++) {
    for (const lose of [false, true]) {
      if (lose && READS.test(LOG[index])) continue;
      const run = await started("write");
      if (lose) run.phone.loseAnswerAt = index;
      else run.phone.unplugAt = index;
      await stops(run);
      const s = screen(run.flow);
      assert.equal(s.stop, "write-interrupted", `${LOG[index]} (${index}, answer lost: ${lose}): ${words(run.flow)}`);
      assert.match(words(run.flow), /The connection to the phone was lost\./);
      // Whatever the stop says, the phone was sent a prefix of the install
      assert.deepEqual(run.phone.log, LOG.slice(0, lose ? index + 1 : index));
    }
  }
});

test("a transfer that takes fewer bytes than it was handed ends the install", async () => {
  const run = await started("write");
  run.phone.takeAtMost = 1000;
  await stops(run);
  stopped(run.flow, "write-interrupted");
  assert.match(words(run.flow), /short download: 1000 of 16384 bytes accepted/);
  assert.match(words(run.flow), /No write reached the phone/);
  assert.deepEqual(run.phone.written(), []);
  assert.deepEqual(run.phone.notReads(), ["download:00004000"]);
});

test("a phone that offers to take fewer bytes than the download has is not sent them", async () => {
  const run = await started("write");
  run.phone.shortData = true;
  const before = run.phone.usb.made("transferOut").filter((c) => c.during === "data").length;
  await stops(run);
  stopped(run.flow, "write-interrupted");
  assert.match(words(run.flow), /short download: 8192 of 16384 bytes accepted/);
  assert.equal(run.phone.usb.made("transferOut").filter((c) => c.during === "data").length, before, "no byte of the image was sent");
  assert.deepEqual(run.phone.written(), []);
});

test("a FAIL answer is the core's stop, with the phone's words and how far the writes got", async () => {
  const run = await started("write");
  run.phone.failOn = ["flash:userdata"];
  await stops(run);
  stopped(run.flow, "write-interrupted");
  assert.match(words(run.flow), /the phone answered FAIL to 'flash:userdata': injected failure/);
  assert.match(words(run.flow), /slot B's boot images are written, and 0 pieces of the system image, with one more that may be partly written/);
  assert.equal(run.phone.notReads().at(-1), "flash:userdata");
  assert.equal(run.phone.activeSlot, "a");

  const first = await started("write");
  first.phone.failOn = ["flash:boot_b"];
  await stops(first);
  stopped(first.flow, "write-interrupted");
  assert.match(words(first.flow), /the phone answered FAIL to 'flash:boot_b'/);
  assert.deepEqual(first.phone.notReads(), ["download:00004000", "flash:boot_b"]);
});

test("an answer that never comes ends the install after the core's wait, and the device is closed", async () => {
  const run = await started("write");
  // The third piece of userdata is carried out; the phone stays on the bus
  // and says nothing
  run.phone.silentAt = at("flash:userdata", 2);
  const clock = fakeTimers();
  try {
    const walking = walk(run.flow, run.picked.again(), run.files);
    await untilWaiting(run.phone.usb);
    // One timer is waiting, and it is the core's 300 s (sparse::FLASH_WAIT_SECONDS)
    assert.deepEqual([...clock.timers.values()].map((t) => t.ms), [300000]);
    clock.fire();
    assert.equal(await walking, false);
    assert.equal(clock.timers.size, 0, "no timer is left behind");
  } finally {
    clock.restore();
  }
  stopped(run.flow, "write-interrupted");
  assert.match(words(run.flow), /USB: timed out waiting for the phone/);
  assert.match(words(run.flow), /2 pieces of the system image, with one more that may be partly written/);
  assert.equal(run.phone.usb.made("close").length, 1, "the pending transfer is ended by closing the device");
  assert.equal(run.phone.notReads().at(-1), "flash:userdata");
  assert.ok(!run.phone.log.includes("set_active:b"));
  assert.equal(run.phone.activeSlot, "a");
});

test("every wait for the phone has the core's limit, and none is left running", async () => {
  // A whole install with the timers counted: one per transfer, each of
  // 300 s, each cleared when its transfer ends
  const clock = fakeTimers();
  const set = globalThis.setTimeout;
  const asked = [];
  globalThis.setTimeout = (fn, ms, ...args) => {
    asked.push(ms);
    return set(fn, ms, ...args);
  };
  try {
    const run = await started("write");
    assert.equal(await walk(run.flow, run.picked.again(), run.files), true);
    const transfers = run.phone.usb.calls.filter((c) => ["open", "claimInterface", "transferOut", "transferIn"].includes(c.method)).length;
    assert.equal(asked.length, transfers);
    assert.ok(asked.every((ms) => ms === 300000));
    assert.equal(clock.timers.size, 0);
  } finally {
    clock.restore();
  }
});

test("a transfer the browser ends with stall, before a session is open, sends nothing that writes", async () => {
  const run = await started("write");
  run.phone.usb.failNext = "stall";
  await assert.rejects(walk(run.flow, run.picked.again(), run.files), /USB: sending: the transfer ended with stall/);
  // No session was opened: the flow is where it was, and the phone as it was
  assert.equal(screen(run.flow).state, "write");
  assert.deepEqual(run.phone.notReads(), []);
  assert.deepEqual(run.phone.written(), []);
});

test("an interface another program holds is said, and nothing is sent", async () => {
  const phone = new FakePhone();
  phone.usb.claimRefused = true;
  const run = await started("phone", { phone });
  const picked = await core.request_phone();
  await assert.rejects(run.flow.read_phone(picked.again()), /USB: claiming the fastboot interface: NetworkError: Unable to claim interface\./);
  assert.deepEqual(phone.log, []);
});

test("a chooser the person closes, and a browser without WebUSB", async () => {
  usb.phone = null;
  await assert.rejects(core.request_phone(), /no phone was chosen/);
  Object.defineProperty(globalThis.navigator, "usb", { value: undefined, configurable: true });
  try {
    await assert.rejects(core.request_phone(), /this browser has no WebUSB/);
  } finally {
    Object.defineProperty(globalThis.navigator, "usb", { value: usb, configurable: true });
  }
});

test("no command outside the gate can be sent: the module has no way to name one", () => {
  // Everything the module exports, and every step of the flow it offers.
  // None takes a command or a partition name; a new one fails this test
  // until someone has read it
  assert.deepEqual(
    Object.keys(core)
      .filter((n) => !["default", "initSync"].includes(n))
      .sort(),
    ["InstallFlow", "Phone", "Release", "RootCheck", "fake_phone", "fake_phone_scenarios", "request_phone", "selftest", "selftest_factory_zip", "selftest_verify_release", "verify_release", "web_write_preview"],
  );
  const steps = Object.getOwnPropertyNames(core.InstallFlow.prototype).filter((n) => !["constructor", "free", "__destroy_into_raw"].includes(n));
  assert.deepEqual(steps.sort(), [
    "accept_google_terms",
    "acknowledge_warning",
    "answers",
    "confirm_install",
    "decline_google_terms",
    "decline_warning",
    "download_begin",
    "download_body",
    "download_done",
    "download_progress",
    "download_response",
    "dry_run",
    "images_check_boot",
    "images_root_begin",
    "images_root_done",
    "images_root_update",
    "no_phone",
    "read_phone",
    "release",
    "screen",
    "session_abandon",
    "session_boot",
    "session_finish",
    "session_open",
    "session_progress",
    "session_root_feed",
    "zip_begin",
    "zip_done",
    "zip_update",
  ]);
  const makers = Object.getOwnPropertyNames(core.InstallFlow).filter((n) => !["length", "name", "prototype", "__wrap"].includes(n));
  assert.deepEqual(makers.sort(), ["begin", "begin_write_preview", "selftest", "selftest_write_preview"]);
  // Nothing states the other bootloader slot, and nothing sets the override
  // in a flow that writes
  const names = [...Object.keys(core), ...steps, ...makers].join(" ");
  assert.doesNotMatch(names, /slot|override|maintainer|unlock|command|partition|erase|oem|token/i);
});

test("the page's dry run of a flow that writes sends questions and nothing else", async () => {
  const run = await started("confirm");
  assert.equal(screen(run.flow).state, "confirm", words(run.flow));
  assert.ok(run.phone.log.length > 0 && run.phone.log.every((c) => READS.test(c)));
  assert.deepEqual(run.phone.written(), []);
  assert.equal(run.phone.usb.made("transferOut").filter((c) => c.during === "data").length, 0);
  // What the owner is asked to confirm lists the six partitions, in order
  const plan = screen(run.flow).plan;
  assert.deepEqual(
    plan.slice(0, 6).map((l) => l.split(":")[0]),
    ["boot_b", "init_boot_b", "vendor_boot_b", "vendor_kernel_boot_b", "dtbo_b", "userdata"],
  );
  assert.deepEqual(plan.slice(6), ["then: make slot B the active slot (set_active:b)", "then: restart the phone (reboot)"]);
});

test("the write steps out of order are refused, with nothing sent", async () => {
  // Before the dry run: no confirmation, and no session
  const early = await started("dry-run");
  const sent = early.phone.log.length;
  assert.throws(() => early.flow.confirm_install("cheetah"), /refused/);
  await assert.rejects(early.flow.session_boot(0, fixture("images/boot.img")), /no session is open/);
  await assert.rejects(early.flow.session_root_feed(fixture("root.img.zst")), /no session is open/);
  await assert.rejects(early.flow.session_finish(), /no session is open/);
  // A session is not opened for files that were not checked
  await assert.rejects(early.flow.session_open(early.picked.again()), /every file of the release is checked before a session is opened/);
  assert.equal(early.phone.log.length, sent);
  assert.equal(screen(early.flow).state, "dry-run");

  // At the confirmation: no session of either kind
  const confirm = await started("confirm");
  const asked = confirm.phone.log.length;
  await checkFiles(confirm.flow, confirm.files);
  await assert.rejects(confirm.flow.session_open(confirm.picked.again()), /neither its dry run nor its write/);
  assert.equal(confirm.phone.log.length, asked);

  // The page's own dry-run step, which walks no image, leads a flow that
  // writes to a stop and not to a confirmation
  const unwalked = await started("dry-run");
  await unwalked.flow.dry_run(unwalked.picked.again());
  stopped(unwalked.flow, "images-not-checked");
  assert.deepEqual(unwalked.phone.notReads(), []);

  // The checks are for one session: the session that writes needs them again
  const write = await started("write");
  const before = write.phone.log.length;
  await assert.rejects(write.flow.session_open(write.picked.again()), /every file of the release is checked before a session is opened/);
  assert.equal(write.phone.log.length, before);

  // Boot images out of the plan's order, in the session that writes
  const order = await started("write");
  await checkFiles(order.flow, order.files);
  assert.equal(await order.flow.session_open(order.picked.again()), true);
  assert.equal(await order.flow.session_boot(1, fixture("images/init_boot.img")), false);
  stopped(order.flow, "write-interrupted");
  assert.match(words(order.flow), /boot images are written in the manifest's order/);
  assert.deepEqual(order.phone.notReads(), []);
});

test("anything but the codename, as typed, stops the install with nothing written", async () => {
  for (const typed of ["Cheetah", "cheetah ", "", "yes", "panther"]) {
    const run = await started("confirm");
    run.flow.confirm_install(typed);
    stopped(run.flow, "not-confirmed");
    assert.deepEqual(run.phone.notReads(), []);
  }
});

test("a phone that tells one bootloader slot stops the page's flow, and the page has no way past it", async () => {
  const phone = new FakePhone();
  phone.vars.delete("version-bootloader-a");
  phone.vars.delete("version-bootloader-b");
  const run = await started("dry-run", { phone });
  stopped(run.flow, "bootloader-slots-unknown");
  assert.match(words(run.flow), /Starting the installer again as things are will stop here again\./);
  assert.ok(phone.log.every((c) => READS.test(c)));
  // Nothing more is accepted of a stopped flow
  assert.throws(() => run.flow.confirm_install("cheetah"), /refused/);
  await assert.rejects(run.flow.session_open(run.picked.again()), /checked before a session is opened|neither its dry run nor its write/);
});

test("a locked phone is shown the unlock command as text, and is sent only questions", async () => {
  const phone = new FakePhone();
  phone.vars.set("unlocked", "no");
  phone.unlockAbility = "get_unlock_ability: 1";
  const run = await started("dry-run", { phone });
  const s = screen(run.flow);
  assert.equal(s.state, "unlock");
  assert.deepEqual(
    s.steps.map((step) => step.command).filter(Boolean),
    ["fastboot flashing unlock"],
  );
  assert.match(s.notice, /Unlock it for this version only if the phone is one you can do without\./);
  assert.deepEqual(
    phone.log.filter((c) => !c.startsWith("getvar:")),
    ["flashing get_unlock_ability"],
  );
});

test("a phone on slot B is refused unless the run is said to be a reinstall", async () => {
  const phone = new FakePhone();
  phone.vars.set("current-slot", "b");
  const run = await started("dry-run", { phone });
  stopped(run.flow, "runs-from-slot-b");
  const again = new FakePhone();
  again.vars.set("current-slot", "b");
  const reinstall = await started("write", { phone: again, reinstall: true });
  assert.equal(await walk(reinstall.flow, reinstall.picked.again(), reinstall.files), true);
  assert.equal(screen(reinstall.flow).state, "installed");
});

test("a file that is not the release's is found before a session is opened", async () => {
  const flip = (bytes) => {
    const changed = bytes.slice();
    changed[changed.length >> 1] ^= 1;
    return changed;
  };
  // At the dry run: a boot image with one bit changed
  const run = await started("dry-run");
  const sent = run.phone.log.length;
  const bad = releaseFiles(run.flow, { "dtbo.img": flip });
  await assert.rejects(walk(run.flow, run.picked.again(), bad), /checksum/);
  assert.equal(run.phone.log.length, sent, "the phone was asked nothing for it");
  assert.equal(screen(run.flow).state, "dry-run");
  // The right files are then taken
  assert.equal(await walk(run.flow, run.picked.again(), run.files), true);
  assert.equal(screen(run.flow).state, "confirm");

  // After the codename: the root image changed on the disk meanwhile
  const late = await started("write");
  const before = late.phone.log.length;
  const name = JSON.parse(late.flow.release()).root_image.name;
  await assert.rejects(walk(late.flow, late.picked.again(), releaseFiles(late.flow, { [name]: flip })), /checksum/);
  assert.equal(late.phone.log.length, before);
  assert.deepEqual(late.phone.written(), []);
  assert.equal(screen(late.flow).state, "write");
});

test("files of another name or size are not read at all", async () => {
  const run = await started("dry-run");
  const release = JSON.parse(run.flow.release());
  const want = wanted(release);
  assert.deepEqual(
    want.boot.map((b) => b.name),
    ["boot.img", "init_boot.img", "vendor_boot.img", "vendor_kernel_boot.img", "dtbo.img"],
  );
  const short = new File([fixture("images/boot.img").subarray(0, 100)], "boot.img");
  const found = match([short], release);
  assert.equal(found.boot[0], null);
  assert.equal(found.problems.length, 6);
  assert.match(found.problems[0], /boot\.img is 100 bytes, and the release lists 16384/);
  assert.match(found.problems[5], /\.img\.zst is not among the files you picked/);
});

// A file whose bytes are other bytes the second time it is read, or that
// cannot be read any more
const fickle = (file, second) => {
  let reads = 0;
  return {
    name: file.name,
    size: file.size,
    slice(from, to) {
      const part = file.slice(from, to);
      return { arrayBuffer: () => (from === 0 && ++reads > 1 && second === "gone" ? Promise.reject(new DOMException("The requested file could not be read.", "NotReadableError")) : part.arrayBuffer().then((b) => (reads > 1 && second === "changed" && from === 0 ? flipFirst(b) : b))) };
    },
  };
};
const flipFirst = (buffer) => {
  const bytes = new Uint8Array(buffer.slice(0));
  bytes[bytes.length >> 1] ^= 1;
  return bytes.buffer;
};

test("a root image that changes between its check and its sending never makes slot B active", async () => {
  const run = await started("write");
  const files = { ...run.files, root: fickle(run.files.root, "changed") };
  await stops({ ...run, files });
  stopped(run.flow, "write-interrupted");
  assert.match(words(run.flow), /the root image that was sent to the phone is not the one that was checked before the first write/);
  assert.match(words(run.flow), /slot B is not made active/);
  assert.ok(!run.phone.log.includes("set_active:b") && !run.phone.log.includes("reboot"));
  assert.equal(run.phone.activeSlot, "a");
});

test("a file that can no longer be read ends the session through the flow", async () => {
  const run = await started("write");
  const files = { ...run.files, root: fickle(run.files.root, "gone") };
  await stops({ ...run, files });
  stopped(run.flow, "write-interrupted");
  assert.match(words(run.flow), /reading the release's files: The requested file could not be read\./);
  assert.match(words(run.flow), /5 of 5 were written/);
  assert.ok(!run.phone.log.includes("set_active:b"));
});

test("an install interrupted in userdata can be run again, and leaves what a clean run leaves", async () => {
  const run = await started("write");
  run.phone.loseAnswerAt = at("flash:userdata", 3);
  await stops(run);
  stopped(run.flow, "write-interrupted");
  // The owner plugs the phone back in and starts over: a new flow
  const again = await started("write", { phone: run.phone.replug() });
  assert.equal(await walk(again.flow, again.picked.again(), again.files), true);
  assert.equal(screen(again.flow).state, "installed");
  for (const [name, bytes] of whole.phone.partitions) assert.deepEqual(run.phone.partitions.get(name), bytes, name);
  assert.equal(run.phone.activeSlot, "b");
});

test("the answers are checked as on the page that ships, and the build says what it is", () => {
  assert.match(core.web_write_preview(), /this build can write to a phone/);
  assert.match(core.web_write_preview(), /It has never been run against a real phone\./);
  assert.equal(good.length, 7);
});
