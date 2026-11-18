// SPDX-License-Identifier: MIT
// The write preview's page: what it adds to the page's own script (app.js)
// in a build made with `web/build.sh web-write-preview`. The page as it ships
// does not load this file, and its module has nothing this file could call
// (tools/check-shipping checks both).
//
// It adds three things, all drawn from the core's screens:
//   - every flow is begun as one that may write, by the module's own
//     constructor for that;
//   - the dry run walks the release's files, which the person picks here
//     (a flow that writes confirms nothing it has not checked);
//   - the confirmation (what the person typed goes to the core as typed)
//     and the write.
// It adds no sentence about the phone, no way round a stop, and no control
// for the other bootloader slot: a phone that tells one slot's version stops
// here as it does on the page that ships.
import { additions, page } from "./app.js";
import { match, walk, wanted } from "./write-walk.js";

const { core, $, el, act } = page;

// The release's files as the person picked them, kept from the dry run to
// the write: File objects, read again from the disk each time
let picked = [];
// Whether a session is open and being fed
let walking = false;

const hasPreview = () => typeof core.InstallFlow.begin_write_preview === "function" && typeof core.web_write_preview === "function";

additions.loaded = "loaded (a write preview: this build can write to a phone)";

additions.begin = (files, index, reinstall) => {
  if (!hasPreview()) {
    $("banner").textContent = "This page was built as a write preview, but its installer module has no write path, so it cannot write to a phone.";
    return core.InstallFlow.begin(...files, index, reinstall);
  }
  // The module's own statement of what this build is, and the status line
  // again, whatever order the scripts were loaded in
  $("banner").textContent = core.web_write_preview();
  $("core").textContent = additions.loaded;
  picked = [];
  return core.InstallFlow.begin_write_preview(...files, index, reinstall);
};

const line = (text) => {
  const p = $("walk-progress");
  if (p) p.textContent = text;
};

function report(event) {
  if (event.phase === "checking") {
    line(event.of ? `Checking ${event.file}: ${Math.floor((100 * event.done) / event.of)} %` : `Checking ${event.file}`);
  } else if (event.phase === "boot") {
    line(`${event.writes ? "Sent" : "Checked against the phone"}: ${event.file}`);
  } else {
    const last = event.pieces.at(-1);
    const took = last?.flash_ms == null ? "" : `, the last flash answered in ${(last.flash_ms / 1000).toFixed(1)} s`;
    line(`userdata: ${event.blocks_done} of ${event.blocks} blocks read, ${event.pieces.length} pieces ${event.writes ? "written" : "cut"}${took}`);
  }
}

// While the phone is written: ask the browser to keep the screen awake and
// to ask before the tab is closed. Neither can be relied on; docs/WEB-WRITE.md
// says what a sleeping tab does.
async function guarded(action) {
  const warn = (event) => event.preventDefault();
  addEventListener("beforeunload", warn);
  let lock = null;
  try {
    lock = await navigator.wakeLock?.request("screen");
  } catch {
    // Not allowed or not there: the write goes on without it
  }
  try {
    return await action();
  } finally {
    removeEventListener("beforeunload", warn);
    await lock?.release().catch(() => {});
  }
}

async function runWalk() {
  const release = JSON.parse(page.flow.release());
  const files = match(picked, release);
  if (files.problems.length) throw new Error(files.problems.join(" "));
  if (!page.phone) page.phone = await core.request_phone();
  walking = true;
  try {
    await walk(page.flow, page.phone.again(), files, report);
  } finally {
    walking = false;
  }
}

function filePicker() {
  const release = JSON.parse(page.flow.release());
  const want = wanted(release);
  const input = el("input", { type: "file", id: "release-files", multiple: true });
  const list = el("ul", { className: "plan", id: "release-wanted" });
  const draw = () => {
    const found = match(picked, release);
    list.replaceChildren(
      ...[...want.boot, want.root].map((w, n) => {
        const have = n < want.boot.length ? found.boot[n] : found.root;
        return el("li", { textContent: `${have ? "picked" : "wanted"}: ${w.name} (${w.size} bytes)` });
      }),
    );
  };
  input.addEventListener("change", () => {
    picked = [...input.files];
    draw();
  });
  draw();
  return el(
    "div",
    {},
    el("p", { textContent: "The release's files, from this computer: its boot images and its system image (.img.zst). They are read here and checked against the signed release; nothing of them is uploaded." }),
    list,
    el("label", {}, "The files: ", input),
  );
}

additions.controls = (s) => {
  if (!hasPreview() || !page.flow.release()) return null;
  const row = el("div", { className: "actions" });
  const progress = el("p", { id: "walk-progress", className: "pending" });
  const go = (id, label, action) => {
    const b = el("button", { type: "button", id, textContent: label, disabled: walking });
    b.addEventListener("click", () => act(action));
    return b;
  };
  switch (s.action) {
    case "dry-run":
      return el("div", {}, filePicker(), el("div", { className: "actions" }, go("dry", "Run the dry run", runWalk)), progress);
    case "confirm": {
      const typed = el("input", { id: "codename", autocomplete: "off", autocapitalize: "none", spellcheck: false });
      row.append(
        el("label", { htmlFor: "codename", textContent: "Codename: " }),
        typed,
        // As typed: the core compares it, and stops for anything else
        go("confirm", "Confirm", () => page.flow.confirm_install(typed.value)),
      );
      return row;
    }
    case "write":
      return el("div", {}, el("div", { className: "actions" }, go("write", "Write to the phone now", () => guarded(runWalk))), progress);
    default:
      return null;
  }
};
