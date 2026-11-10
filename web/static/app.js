// SPDX-License-Identifier: MIT
// The page's own script: no framework, no third-party code, nothing inline.
// It fetches bytes and shows text; every decision is the wasm core's.
import init, * as core from "./pkg/omamobile_installer_web.js";

const $ = (id) => document.getElementById(id);
const show = (id, text, state) => {
  $(id).textContent = text;
  $(id).className = state;
};
const list = (id, lines) => {
  $(id).replaceChildren(...lines.map((line) => Object.assign(document.createElement("li"), { textContent: line })));
};
const bytes = async (url) => {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`${url}: HTTP ${response.status}`);
  return new Uint8Array(await response.arrayBuffer());
};

async function loadRelease(base) {
  const names = ["SHA256SUMS", "SHA256SUMS.sig", "omamobile.gpg", "manifest.json", "MANIFEST", "device.conf"];
  const files = await Promise.all(names.map((name) => bytes(base + name)));
  // Throws unless the signature, the checksums and every rule hold
  return core.verify_release(...files);
}

function describe(release) {
  const d = JSON.parse(release.describe());
  show("release", `Omarchy Mobile ${d.version} (${d.label}) for ${d.device} (${d.codename}), signed by ${d.signed_by}`, "ok");
  list("plan", [
    ...d.boot_images.map((w) => `flash ${w.partition} ${w.source} (${w.size} bytes)`),
    `flash ${d.root_image.partition} ${d.root_image.name} (erases userdata)`,
    "set_active b",
    "reboot",
  ]);
}

async function checkPhone() {
  try {
    // The chooser needs this click; the release is checked again while it is open
    const phone = await core.request_phone();
    show("phone", "checking", "pending");
    const release = await loadRelease("release/");
    const install = await core.connect(release, phone, $("reinstall").checked, undefined);
    const f = JSON.parse(install.facts());
    show("phone", `${f.product}, unlocked, slot ${f.current_slot}, bootloader ${f.bootloader}, max-download-size ${f.max_download_size}`, "ok");
    list("log", install.plan().map((line) => `would: ${line}`));
  } catch (e) {
    show("phone", e.message ?? String(e), "bad");
  }
}

async function main() {
  try {
    await init();
    show("core", `loaded${core.writes_enabled() ? "" : " (this build can't write to a phone)"}`, "ok");
  } catch (e) {
    show("core", `the core did not load: ${e.message ?? e}`, "bad");
    return;
  }
  try {
    describe(await loadRelease("release/"));
  } catch (e) {
    show("release", e.message ?? String(e), "bad");
  }
  if ("usb" in navigator) {
    $("check").disabled = false;
    $("check").addEventListener("click", checkPhone);
  } else {
    show("phone", "this browser has no WebUSB: a Chromium-based browser is needed", "bad");
  }
  if (new URLSearchParams(location.search).has("selftest") && core.selftest) {
    $("selftest-section").hidden = false;
    try {
      // A changed file must be refused here too
      const broken = await Promise.all(["SHA256SUMS", "SHA256SUMS.sig", "omamobile.gpg", "manifest.json", "MANIFEST", "device.conf"].map((n) => bytes("release/" + n)));
      broken[3][100] ^= 1;
      let tampered = "accepted";
      try {
        core.verify_release(...broken);
      } catch (e) {
        tampered = `refused (${e.message})`;
      }
      const result = JSON.parse(await core.selftest());
      result.tampered_manifest = tampered;
      show("selftest", JSON.stringify(result, null, 1), "done");
      // For a headless run: the demo server prints what it is sent
      await fetch("selftest-result", { method: "POST", body: JSON.stringify(result) });
    } catch (e) {
      show("selftest", e.message ?? String(e), "bad");
    }
  }
  document.documentElement.dataset.ready = "1";
}

main();
