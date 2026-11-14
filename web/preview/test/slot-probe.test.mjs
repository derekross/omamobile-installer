// SPDX-License-Identifier: MIT
// The write preview's measurement of the other bootloader slot
// (docs/DESIGN.md §6), through the page's own code path (slot-probe.js),
// against a pretend phone behind a pretend WebUSB device that restarts: it
// leaves the bus and comes back as a new device, each as the browser's
// event. Run by web/preview-test.sh, in node: no browser, no USB.
import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { FakePhone, EP_OUT } from "./fake-usb.mjs";
import { begin, core, GATE, PROBE, screen, usb, words } from "./harness.mjs";
import { runProbe } from "../slot-probe.js";
import { walk } from "../write-walk.js";
import { releaseFiles } from "./harness.mjs";

// A "second" of the core's limits, in milliseconds: the pretend phone
// restarts in a few
const SECOND = 5;

const devices = [];
const firstInstall = (slotB) => {
  const phone = new FakePhone().firstInstall(slotB);
  return phone;
};

afterEach(() => {
  for (const device of devices.splice(0)) {
    for (const call of device.made("transferOut")) {
      assert.equal(call.endpoint, EP_OUT);
      if (call.during === "command") assert.ok(GATE.test(call.command) || PROBE.test(call.command), call.command);
    }
    const made = device.made().filter((m) => !["open", "close", "claimInterface", "transferOut", "transferIn"].includes(m));
    assert.deepEqual(made, [], "no control transfer, reset or other call");
  }
  assert.equal(usb.listed, 0, "the page lists no devices: the browser's events hand it the phone again");
});

// What the measurement asks of the page, as the page does it; the record
// kept as the page keeps it
async function measure(run, answer = "measure") {
  const kept = [];
  const lines = [];
  assert.equal(run.flow.slot_probe_answer(answer), true);
  devices.push(run.phone.usb);
  const phone = await runProbe(run.flow, core, usb, run.picked, {
    say: (l) => lines.push(l),
    keep: (k) => kept.push(k),
    second: SECOND,
  });
  devices.push(run.phone.usb);
  return { phone, lines, kept: kept.at(-1) };
}

const notReads = (phone) => phone.notReads();
const ROUND_TRIP = ["set_active:b", "reboot-bootloader", "set_active:a", "reboot-bootloader"];

test("a first install measures slot B through the browser's events and goes on", async () => {
  const run = await begin("phone-looked", { phone: firstInstall(), probe: true });
  const s = screen(run.flow);
  assert.equal(s.state, "slot-probe", words(run.flow));
  assert.deepEqual(
    s.choices.map((c) => c.answer),
    ["measure", "stop"],
  );
  // An answer the screen does not offer is refused, and nothing is sent
  assert.throws(() => run.flow.slot_probe_answer("put-back"));
  assert.deepEqual(notReads(run.phone), []);
  const { phone, lines, kept } = await measure(run);
  assert.deepEqual(notReads(run.phone), ROUND_TRIP);
  assert.equal(run.phone.activeSlot, "a");
  assert.equal(run.phone.restarts, 2);
  assert.ok(lines.includes("The phone left the bus and came back in Fastboot Mode."));
  assert.equal(kept, undefined, "nothing to keep: slot A is back");
  assert.equal(screen(run.flow).state, "factory-image");
  // The phone the browser handed back is the one that goes on: through the
  // whole install, to set_active:b
  run.picked = phone;
  const zip = core.selftest_factory_zip();
  run.flow.zip_begin(screen(run.flow).file.name, zip.length);
  run.flow.zip_update(zip);
  run.flow.zip_done();
  run.flow.answers("alex", "4812", "4812", "", "Europe/Paris", "", "");
  run.files = releaseFiles(run.flow);
  assert.equal(await walk(run.flow, run.picked.again(), run.files), true);
  assert.match(words(run.flow), /Bootloader fake-1\.0 on both copies: the installer measured slot B's bootloader/);
  run.flow.confirm_install("cheetah");
  assert.equal(await walk(run.flow, run.picked.again(), run.files), true);
  devices.push(run.phone.usb);
  assert.equal(screen(run.flow).state, "installed", words(run.flow));
  const sent = notReads(run.phone);
  assert.deepEqual(sent.slice(0, 4), ROUND_TRIP);
  assert.deepEqual(sent.slice(-2), ["set_active:b", "reboot"]);
});

test("copies that differ stop with slot A active", async () => {
  const run = await begin("phone-looked", { phone: firstInstall("fake-0.9"), probe: true });
  await measure(run);
  const s = screen(run.flow);
  assert.equal(s.stop, "bootloader-slots-differ", words(run.flow));
  assert.match(words(run.flow), /slot B's is fake-0\.9/);
  assert.equal(run.phone.activeSlot, "a");
  assert.deepEqual(notReads(run.phone), ROUND_TRIP);
});

test("a phone that never leaves the bus is set back and not measured", async () => {
  const phone = firstInstall();
  phone.restart = "never-leaves";
  const run = await begin("phone-looked", { phone, probe: true });
  const { lines } = await measure(run);
  assert.ok(lines.includes("The phone did not leave the bus."));
  assert.equal(screen(run.flow).stop, "slot-probe-stopped");
  assert.deepEqual(notReads(run.phone), ["set_active:b", "reboot-bootloader", "set_active:a"]);
  assert.equal(run.phone.activeSlot, "a");
});

test("a phone that starts an OS from slot B is put back by the next run, from the record", async () => {
  const phone = firstInstall();
  phone.restart = "boots-os-on-b";
  const run = await begin("phone-looked", { phone, probe: true });
  const { kept, phone: after } = await measure(run);
  assert.equal(after, null, "the page has no phone once it did not come back");
  assert.equal(screen(run.flow).stop, "slot-probe-stopped");
  assert.equal(screen(run.flow).retry, "after-fix");
  assert.match(words(run.flow), /choose nothing/);
  assert.equal(run.phone.activeSlot, "b");
  assert.match(kept, /^omamobile-slot-probe 1\n/);
  // The owner's buttons; then a new run with what the page kept
  phone.restart = "normal";
  phone.ownerButtons();
  const next = await begin("phone-looked", { phone, probe: true, record: kept });
  assert.equal(screen(next.flow).state, "slot-probe-put-back", words(next.flow));
  const before = notReads(phone).length;
  const put = await measure(next, "put-back");
  assert.deepEqual(notReads(phone).slice(before), ["set_active:a", "reboot-bootloader"]);
  assert.equal(put.kept, undefined);
  assert.equal(phone.activeSlot, "a");
  assert.equal(screen(next.flow).state, "phone");
});

test("a release that does not allow the measurement stops as before", async () => {
  const run = await begin("phone-looked", { phone: firstInstall(), probe: false });
  assert.equal(screen(run.flow).stop, "bootloader-slots-unknown");
  assert.deepEqual(notReads(run.phone), []);
  devices.push(run.phone.usb);
});
