// SPDX-License-Identifier: MIT
// The write preview with a release that installs on the slot the phone runs
// from (manifest format 2, fixtures/selftest-running), through the page's
// own code path, against a pretend phone behind a pretend WebUSB device.
// Run by web/preview-test.sh, in node: no browser, no USB.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { FakePhone } from "./fake-usb.mjs";
import { core, good, screen, usb, words } from "./harness.mjs";
import { match, walk } from "../write-walk.js";

const root = new URL("../../..", import.meta.url).pathname;
const fixture = (name) => new Uint8Array(readFileSync(`${root}fixtures/selftest-running/${name}`));

function files(flow) {
  const release = JSON.parse(flow.release());
  assert.equal(release.format, 2);
  assert.deepEqual(release.accepted_slots, ["a"]);
  // No partition is named before the phone is looked at: the slot is its own
  assert.ok(release.boot_images.every((b) => b.partition === null && b.image));
  const picked = [...release.boot_images.map((b) => new File([fixture(b.source)], b.source.split("/").pop())), new File([fixture("root.img.zst")], release.root_image.name)];
  const found = match(picked, release);
  assert.deepEqual(found.problems, []);
  return found;
}

async function toThePhone(phone) {
  usb.phone = phone;
  const flow = core.InstallFlow.selftest_write_preview_running();
  flow.acknowledge_warning();
  const picked = await core.request_phone();
  await flow.read_phone(picked.again());
  return { flow, picked };
}

test("a first install from stock Android writes slot a and sends no set_active", async () => {
  const phone = new FakePhone().firstInstall("fake-1.0");
  const { flow, picked } = await toThePhone(phone);
  const s = screen(flow);
  assert.equal(s.state, "factory-image", words(flow));
  const zip = core.selftest_factory_zip();
  flow.zip_begin(s.file.name, zip.length);
  flow.zip_update(zip);
  flow.zip_done();
  flow.answers(...good);
  const found = files(flow);
  await walk(flow, picked.again(), found);
  const confirm = screen(flow);
  assert.equal(confirm.state, "confirm", words(flow));
  assert.ok(confirm.plan[0].startsWith("boot_a: "), confirm.plan.join("\n"));
  assert.ok(!confirm.plan.some((l) => l.includes("set_active")));
  assert.ok(confirm.footer.some((l) => l.startsWith("Android on this phone is replaced")));
  flow.confirm_install("cheetah");
  await walk(flow, picked.again(), found);
  assert.equal(screen(flow).state, "installed", words(flow));
  const changes = phone.log.filter((c) => !c.startsWith("getvar:") && !c.startsWith("download:"));
  assert.deepEqual(changes.slice(0, 5), ["flash:boot_a", "flash:init_boot_a", "flash:vendor_boot_a", "flash:vendor_kernel_boot_a", "flash:dtbo_a"]);
  assert.equal(changes.at(-1), "reboot");
  assert.ok(!phone.log.some((c) => c.startsWith("set_active") || c.startsWith("reboot-") || /^flash:.*_b$/.test(c)), phone.log.join("\n"));
  assert.equal(phone.activeSlot, "a");
});

test("a phone on slot b stops without the question of what slot b holds", async () => {
  const phone = new FakePhone().firstInstall("fake-1.0");
  phone.vars.set("current-slot", "b");
  phone.activeSlot = "b";
  phone.running = "b";
  const { flow } = await toThePhone(phone);
  const s = screen(flow);
  assert.equal(s.state, "stopped", words(flow));
  assert.equal(s.stop, "current-slot-not-accepted");
  assert.deepEqual(s.choices, []);
  assert.ok(phone.log.every((c) => c.startsWith("getvar:")), phone.log.join("\n"));
});
