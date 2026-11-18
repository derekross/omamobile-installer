// SPDX-License-Identifier: MIT
// The write preview's page, clicked through in a headless browser against the
// pretend phone at the USB level (fake-usb.mjs). Test code only: it is copied
// into a build made with `web/build.sh web-write-preview selftest`, where it
// takes the place of the page's own script tag, and into no other build.
//
// It is the part node cannot run: the page's own script and the preview's
// (app.js, preview.js), with their buttons, the file inputs and the field for
// the codename. It clicks what a person would click. The browser is started
// with WebUSB switched off (web/preview-test.sh), and this script refuses to
// run in one that has it, so the only "USB device" the page can be handed is
// the pretend one below.
// The page's own entry, as the preview's first page loads it
import "./preview.js";
import { additions, page } from "./app.js";
import { FakePhone, FakeUsb, SENTINEL } from "./fake-usb.mjs";

const result = {};
const failures = [];
const check = (name, ok, detail = "") => {
  result[name] = ok ? "pass" : `FAIL ${detail}`;
  if (!ok) failures.push(name);
};

async function run() {
  if ("usb" in navigator) throw new Error("this browser has WebUSB: the page's test only runs where no real device can be reached");
  const usb = new FakeUsb();
  Object.defineProperty(navigator, "usb", { value: usb, configurable: true });

  const { core, $ } = page;
  // The test bundle in the place of the published release, which has no
  // build index and no images here. Everything else is the preview's own
  const previews = additions.begin;
  additions.begin = (files, index, reinstall) => {
    previews(files, index, reinstall).free();
    return core.InstallFlow.selftest_write_preview(reinstall);
  };

  const ready = () => new Promise((resolve) => {
    const look = () => (document.documentElement.dataset.ready === "1" ? resolve() : setTimeout(look, 20));
    look();
  });
  await ready();

  const state = () => $("screen").dataset.state;
  const stop = () => $("screen").dataset.stop;
  const text = () => $("screen").textContent;
  const click = async (id) => {
    const b = $(id);
    if (!b) throw new Error(`there is no #${id} on the '${state()}' screen`);
    b.click();
    await page.busy;
  };
  const pick = async (id, files) => {
    const transfer = new DataTransfer();
    for (const file of files) transfer.items.add(file);
    $(id).files = transfer.files;
    $(id).dispatchEvent(new Event("change"));
    await page.busy;
  };
  const fetched = async (name) => new Uint8Array(await (await fetch(`test-release/${name}`)).arrayBuffer());
  const releaseFiles = async () => {
    const release = JSON.parse(page.flow.release());
    const names = [...release.boot_images.map((b) => b.source.split("/").pop()), "root.img.zst"];
    const files = await Promise.all(names.map(async (name) => new File([await fetched(name)], name === "root.img.zst" ? release.root_image.name : name)));
    return files;
  };
  // A new flow on a new pretend phone, up to the dry run's screen
  const toDryRun = async (phone = new FakePhone()) => {
    usb.phone = phone;
    await click("restart");
    await click("accept");
    await click("look");
    if (state() !== "factory-image") return phone;
    const zip = core.selftest_factory_zip();
    await pick("zip", [new File([zip], JSON.parse(page.flow.screen()).file.name)]);
    const form = $("answers");
    for (const [name, value] of Object.entries({ username: "alex", password: "4812", password_again: "4812", timezone: "Europe/Paris" })) form.elements[name].value = value;
    form.requestSubmit();
    await page.busy;
    return phone;
  };
  const notReads = (phone) => phone.notReads();

  // 1. What the page says it is
  check("the preview's first line says that this build can write to a phone and has never met a real one", $("banner").textContent === core.web_write_preview() && $("banner").textContent.includes("this build can write to a phone") && !document.body.textContent.includes("cannot write to a phone"), $("banner").textContent);
  check("the status line says so too", $("core").textContent.includes("this build can write to a phone"), $("core").textContent);

  // 2. A whole install, clicked through
  const phone = await toDryRun();
  check("the chooser was the browser's, with the fastboot filter and nothing else", JSON.stringify(usb.requests.at(-1)) === '{"filters":[{"classCode":255,"subclassCode":66,"protocolCode":3}]}' && usb.listed === 0, JSON.stringify(usb.requests.at(-1)));
  check("the dry run's screen asks for the release's files and lists them", state() === "dry-run" && !!$("release-files") && $("release-wanted").children.length === 6 && [...$("release-wanted").children].every((li) => li.textContent.startsWith("wanted: ")), text());
  await click("dry");
  check("without the files the dry run is not begun, and the page says which are missing", state() === "dry-run" && ($("failure")?.textContent ?? "").includes("boot.img is not among the files you picked") && notReads(phone).length === 0, $("failure")?.textContent);
  const files = await releaseFiles();
  await pick("release-files", files);
  check("picked files are marked", [...$("release-wanted").children].every((li) => li.textContent.startsWith("picked: ")), $("release-wanted").textContent);
  await click("dry");
  const plan = [...document.querySelectorAll("#screen .plan li")].map((li) => li.textContent);
  check("after the dry run the page lists what will be written, in the core's words, and asks for the codename", state() === "confirm" && plan.length === 8 && plan[0].startsWith("boot_b: ") && plan[5].startsWith("userdata: ") && plan[5].endsWith("ERASES userdata") && text().includes("To go ahead, type the phone's codename: cheetah") && !!$("codename") && !$("write"), `${state()} ${text()}`);
  check("up to here the phone was sent questions only", notReads(phone).length === 0 && phone.written().length === 0, notReads(phone).join(" "));
  $("codename").value = "cheetah";
  await click("confirm");
  check("the codename typed: the write's screen, and still nothing written", state() === "write" && text().includes("Don't unplug it and don't press its keys") && !!$("write") && notReads(phone).length === 0);
  await click("write");
  const sent = notReads(phone);
  check("the write ran through: the installed screen, in the core's words", state() === "installed" && text().includes("Every image was written, and the phone confirmed each one: boot_b, init_boot_b, vendor_boot_b, vendor_kernel_boot_b, dtbo_b, userdata."), `${state()} ${stop()} ${text()} ${$("failure")?.textContent ?? ""}`);
  check("the phone was sent slot B's five images, userdata, set_active:b and reboot, and nothing else", sent.slice(0, 10).filter((c, i) => i % 2).join(" ") === "flash:boot_b flash:init_boot_b flash:vendor_boot_b flash:vendor_kernel_boot_b flash:dtbo_b" && sent.slice(10, -2).every((c, i) => (i % 2 ? c === "flash:userdata" : /^download:[0-9a-f]{8}$/.test(c))) && sent.slice(-2).join(" ") === "set_active:b reboot", sent.join(" "));
  check("slot B's images and userdata are on the pretend phone, and no other partition was touched", phone.written().sort().join(" ") === "boot_b dtbo_b init_boot_b userdata vendor_boot_b vendor_kernel_boot_b" && phone.activeSlot === "b" && phone.rebooted && phone.partitions.get("persist").every((b) => b === SENTINEL));
  check("at the USB level the page opened, claimed and transferred, and did nothing else", phone.usb.made().sort().join(" ") === "claimInterface open transferIn transferOut", phone.usb.made().join(" "));

  // 3. Anything but the codename
  const second = await toDryRun();
  await pick("release-files", files);
  await click("dry");
  $("codename").value = "Cheetah";
  await click("confirm");
  check("anything but the codename stops the install, with nothing written", stop() === "not-confirmed" && notReads(second).length === 0 && !$("write") && !$("codename"), `${state()} ${stop()}`);

  // 4. The cable pulled while the phone flashes userdata
  const third = await toDryRun();
  await pick("release-files", files);
  await click("dry");
  $("codename").value = "cheetah";
  await click("confirm");
  // The same steps as the first install, so the same place in the log: its
  // second flash of userdata is carried out, and the answer never arrives
  third.loseAnswerAt = phone.log.flatMap((c, i) => (c === "flash:userdata" ? [i] : []))[1];
  await click("write");
  check("a phone that vanishes while it flashes: the core's stop is drawn, with how far the writes got", stop() === "write-interrupted" && text().includes("The connection to the phone was lost.") && text().includes("with one more that may be partly written") && third.activeSlot === "a" && !third.log.includes("set_active:b") && !$("write") && !$("failure"), `${state()} ${stop()} ${text()}`);

  // 5. A phone that tells one bootloader slot: the flow's stop, and no way round it
  const oneSlot = new FakePhone();
  oneSlot.vars.delete("version-bootloader-a");
  oneSlot.vars.delete("version-bootloader-b");
  await toDryRun(oneSlot);
  check("a phone that tells one bootloader slot is stopped, and the page offers nothing but to start over", stop() === "bootloader-slots-unknown" && text().includes("will stop here again") && !$("release-files") && !$("codename") && !$("write") && !$("dry") && [...document.querySelectorAll("#screen button")].map((b) => b.id).join(" ") === "again" && notReads(oneSlot).length === 0 && !document.querySelector("#screen input, #screen select, #screen a"), text());

  // 6. A locked phone: the command as text
  const locked = new FakePhone();
  locked.vars.set("unlocked", "no");
  locked.unlockAbility = "get_unlock_ability: 0";
  await toDryRun(locked);
  const commands = [...document.querySelectorAll("#screen .command")].map((c) => c.textContent);
  check("a locked phone is shown the unlock command as text, and was sent questions only", state() === "unlock" && commands.join("|") === "fastboot flashing unlock" && notReads(locked).length === 0 && locked.log.filter((c) => !c.startsWith("getvar:")).join("|") === "flashing get_unlock_ability", `${state()} ${commands.join("|")}`);
}

try {
  await run();
  result.failed = failures;
  result.pass = failures.length === 0;
} catch (e) {
  result.pass = false;
  result.error = e.message ?? String(e);
  result.on_screen = document.getElementById("screen")?.dataset.state;
}
const out = document.getElementById("selftest");
document.getElementById("selftest-section").hidden = false;
out.textContent = JSON.stringify(result, null, 1);
out.className = result.pass ? "done" : "bad";
// For the headless run: the server prints what it is sent
await fetch("selftest-result", { method: "POST", body: JSON.stringify(result) });
