// SPDX-License-Identifier: MIT
// The page's own script: no framework, no third-party code, nothing inline.
//
// It drives the core's install flow (core/src/flow.rs, through the wasm
// module's InstallFlow): it draws the screen the flow gives it and hands in
// what the person did. The order of the steps, every refusal and every
// sentence on a screen are the core's. What is the page's own: the buttons,
// the file picker, the form, and reading the picked file a piece at a time.
//
// Nothing is fetched from Google by the page as it ships: a browser lets no
// page read a file from Google's server (it sends no
// Access-Control-Allow-Origin header; measured on 2026-11-12). The wasm
// module tells the core so at the start of every flow, and the flow goes
// straight to the file the person saves from Google's page, with no terms
// step and no download on offer.
//
// The download code below is for a flow that does offer one (action
// "download"), which today only the self-test begins, against a stand-in:
// one file, only after the person accepted Google's terms on the flow's
// terms step. The address is the core's (from the release's signed build
// index), the page fetches it, and the core decides what the answer and the
// bytes are worth. A fetch the browser refuses falls back to the file the
// person saves from Google's page.
//
//   /            the published release's files, and a real phone over WebUSB
//   /?pretend    a pretend phone and a test bundle (self-test builds only)
//   /?selftest   the same, driven through every state by this script
import init, * as core from "./pkg/omamobile_installer_web.js";

const $ = (id) => document.getElementById(id);
const el = (tag, props = {}, ...children) => {
  const node = Object.assign(document.createElement(tag), props);
  node.append(...children);
  return node;
};
const RELEASE_FILES = ["SHA256SUMS", "SHA256SUMS.sig", "omamobile.gpg", "manifest.json", "MANIFEST", "device.conf"];
const query = new URLSearchParams(location.search);

let pretend = false; // a pretend phone and the built-in test bundle
let flow = null; // core.InstallFlow
let phone = null; // core.Phone: the one the person picked, or the pretend one
let busy = Promise.resolve(); // the action in progress, for the self-test to wait on
// Self-test builds only: a stand-in on this computer that answers in Google's
// place ({ origin, mode }), so that a pretend run never reaches Google
let standIn = null;
const GOOGLE = "https://dl.google.com/";

const show = (id, text, state) => {
  $(id).textContent = text;
  $(id).className = state;
};

async function bytes(url, optional = false) {
  const response = await fetch(url);
  if (optional && response.status === 404) return undefined;
  if (!response.ok) throw new Error(`${url}: HTTP ${response.status}`);
  return new Uint8Array(await response.arrayBuffer());
}

// ---- Starting a flow ------------------------------------------------------

// `slotOverride` is the maintainer's override for the bootloader-slot stop.
// Nothing on the page sets it: only the self-test does, on the pretend phone.
// `fetches` begins a flow that offers the download from Google, which the
// page as it ships never does: only the self-test sets it, with a stand-in.
async function start(tamper = "", slotOverride = false, fetches = false) {
  phone = null;
  if (pretend) {
    if (!standIn) {
      // Served by web/serve.py --stand-in; without it a pretend run fetches nothing
      const config = await fetch("google-stand-in.json");
      if (config.ok) standIn = { origin: (await config.json()).origin, mode: "allowed" };
    }
    flow = core.InstallFlow.selftest($("reinstall").checked, tamper, slotOverride, fetches);
    phone = core.fake_phone($("scenario").value);
  } else {
    const files = await Promise.all(RELEASE_FILES.map((name) => bytes("release/" + name)));
    // The list of Google's builds, when the release has one
    const index = await bytes("release/factory-builds.json", true);
    flow = core.InstallFlow.begin(...files, index, $("reinstall").checked);
  }
  const text = flow.release();
  if (text) {
    const d = JSON.parse(text);
    const who = pretend ? "a test bundle, signed by a test key" : `signed by ${d.signed_by}`;
    show("release", `Omarchy Mobile ${d.version} (${d.label}) for ${d.device} (${d.codename}), ${who}`, "ok");
  } else {
    show("release", "the release did not pass its checks", "bad");
  }
  render();
}

// ---- Actions --------------------------------------------------------------

// Run one action, then draw whatever screen the flow is on. An error that
// isn't one of the flow's own stops (the chooser was closed, the cable was
// pulled) is shown on the screen, which stays as it was.
function act(action) {
  busy = (async () => {
    let failure = "";
    try {
      await action();
    } catch (e) {
      failure = e.message ?? String(e);
    }
    render();
    if (failure) $("screen").append(el("p", { className: "problem", id: "failure", textContent: failure }));
  })();
  return busy;
}

async function lookAtPhone() {
  if (!pretend) {
    // The browser's chooser, each time: a phone that restarted is a new device
    try {
      phone = await core.request_phone();
    } catch (e) {
      flow.no_phone();
      throw e;
    }
  }
  await flow.read_phone(phone.again());
}

async function takeFile(file) {
  if (!file) return;
  // The name and the size first: a wrong file is refused before it is read
  if (!flow.zip_begin(file.name, file.size)) return;
  const bar = $("zip-progress");
  if (bar) bar.hidden = false;
  // A piece at a time: the file is never whole in memory
  const reader = file.stream().getReader();
  let done = 0;
  for (;;) {
    const { value, done: end } = await reader.read();
    if (end) break;
    flow.zip_update(value);
    done += value.length;
    if (bar) bar.value = done / Math.max(file.size, 1);
  }
  flow.zip_done();
}

// Fetch Google's factory image: what the core asks for, and nothing else.
// The core is told the answer and given the bytes as they arrive; it counts
// and hashes them, and its next screen says how it went. The page keeps no
// byte of the file.
async function download() {
  const request = JSON.parse(flow.download_begin());
  const bar = $("zip-progress");
  if (bar) bar.hidden = false;
  let failure = "";
  let detail = "";
  // In a pretend run the stand-in answers in Google's place, or nothing does
  const target = !pretend ? request.url : standIn ? request.url.replace(GOOGLE, `${standIn.origin}/${standIn.mode}/`) : null;
  const cameFrom = (url) => (pretend && standIn ? url.replace(`${standIn.origin}/${standIn.mode}/`, GOOGLE) : url);
  let response = null;
  if (!target) {
    failure = "network";
    detail = "a pretend run fetches nothing from Google, and this page was served without a stand-in (web/serve.py --stand-in)";
  } else {
    try {
      // No cookie and no referrer go to Google. The browser follows a
      // redirect itself; the core is told where the answer came from
      response = await fetch(target, { mode: "cors", credentials: "omit", referrerPolicy: "no-referrer", cache: "no-store" });
    } catch {
      // All a page learns: no connection, or a server that does not allow
      // a page on another origin to read its files
      failure = "no-answer";
    }
  }
  if (response) {
    // A length is the file's only when the body is sent as it is
    const length = response.headers.get("content-encoding") ? null : response.headers.get("content-length");
    if (flow.download_response(response.status, cameFrom(response.url), length === null ? undefined : Number(length))) {
      const reader = response.body.getReader();
      try {
        for (;;) {
          const { value, done } = await reader.read();
          if (done) break;
          // The core takes no byte past the file's size
          if (!flow.download_body(value)) {
            await reader.cancel();
            break;
          }
          const [have, total] = flow.download_progress();
          if (bar) bar.value = have / Math.max(total, 1);
        }
      } catch (e) {
        failure = "network";
        detail = e.message ?? String(e);
      }
    } else {
      await response.body?.cancel();
    }
  }
  flow.download_done(failure, detail);
}

// Hand the form's fields to the flow. Returns whether they were taken; when
// not, each problem is put under its field, which keeps what was typed.
function submitAnswers(form) {
  const value = (name) => form.elements[name].value;
  const problems = JSON.parse(
    flow.answers(value("username"), value("password"), value("password_again"), value("hostname"), value("timezone"), value("wifi_ssid"), value("wifi_psk")),
  );
  for (const p of problems) {
    form.elements[p.field].after(el("p", { className: "field-problem", textContent: p.message }));
  }
  return problems.length === 0;
}

// ---- Drawing --------------------------------------------------------------

const button = (id, label, action) => {
  const b = el("button", { type: "button", id, textContent: label });
  b.addEventListener("click", () => act(action));
  return b;
};

function answersForm() {
  const zones = typeof Intl.supportedValuesOf === "function" ? Intl.supportedValuesOf("timeZone") : [];
  const field = (name, label, props = {}) => [
    el("label", { htmlFor: `a-${name}`, textContent: label }),
    el("input", { name, id: `a-${name}`, autocomplete: "off", ...props }),
  ];
  const form = el(
    "form",
    { className: "answers", id: "answers" },
    ...field("username", "Your user name on the phone", { placeholder: "alex", autocapitalize: "none", spellcheck: false }),
    ...field("password", "A PIN or password", { type: "password", autocomplete: "new-password" }),
    ...field("password_again", "The same again", { type: "password", autocomplete: "new-password" }),
    ...field("hostname", "The phone's name", { placeholder: "<user name>-phone", autocapitalize: "none", spellcheck: false }),
    ...field("timezone", "Time zone", { value: Intl.DateTimeFormat().resolvedOptions().timeZone ?? "", spellcheck: false }),
    ...field("wifi_ssid", "Wi-Fi network (optional)", { spellcheck: false }),
    ...field("wifi_psk", "Wi-Fi password", { type: "password" }),
    el("div", { className: "actions" }, el("button", { type: "submit", id: "answers-go", textContent: "Go on" })),
  );
  if (zones.length) {
    form.elements.timezone.setAttribute("list", "zones");
    form.append(el("datalist", { id: "zones" }, ...zones.map((z) => el("option", { value: z }))));
  }
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    form.querySelectorAll(".field-problem").forEach((p) => p.remove());
    // The core checks a zone's shape; the browser knows which zones exist
    const zone = form.elements.timezone;
    if (zones.length && zone.value && zone.value !== "UTC" && !zones.includes(zone.value)) {
      zone.after(el("p", { className: "field-problem", textContent: "That isn't a time zone this browser knows (they look like Europe/Paris)." }));
      busy = Promise.resolve();
      return;
    }
    // Drawn again only when the answers were taken, so nothing typed is lost
    busy = (async () => {
      try {
        if (submitAnswers(form)) render();
      } catch (e) {
        form.append(el("p", { className: "field-problem", textContent: e.message ?? String(e) }));
      }
    })();
  });
  return form;
}

function controls(s) {
  const row = el("div", { className: "actions" });
  switch (s.action) {
    case "acknowledge":
      row.append(
        button("accept", "I have read this. Go on", () => flow.acknowledge_warning()),
        button("decline", "Stop here", () => flow.decline_warning()),
      );
      break;
    case "read-phone":
      if (pretend) {
        row.append(button("look", "Look at the pretend phone", lookAtPhone));
        if (s.state === "unlock") {
          row.append(button("pretend-unlock", "Pretend its owner unlocked it", () => phone.fake_set("unlocked", "yes")));
        }
      } else if ("usb" in navigator) {
        row.append(button("look", s.state === "unlock" ? "Look at the phone again" : "Choose the phone", lookAtPhone));
      } else {
        row.append(el("p", { className: "problem", textContent: "This browser has no WebUSB, so it can't reach a phone. A Chromium-based browser is needed." }));
      }
      break;
    case "accept-terms":
      row.append(
        button("accept-terms", "I accept. Download the file from Google", () => flow.accept_google_terms(Date.now())),
        button("decline-terms", "I will download it myself", () => flow.decline_google_terms()),
        el("a", { id: "google", className: "button", href: s.link, target: "_blank", rel: "noopener noreferrer", textContent: "Open Google's page" }),
      );
      break;
    case "download":
    case "take-file": {
      const input = el("input", { type: "file", id: "zip", accept: ".zip" });
      input.addEventListener("change", () => act(() => takeFile(input.files[0])));
      if (s.action === "download") row.append(button("download", "Download from Google", download));
      row.append(
        el("a", { id: "google", className: "button", href: s.link, target: "_blank", rel: "noopener noreferrer", textContent: "Open Google's page" }),
        el("label", {}, s.action === "download" ? "Or the file you saved: " : "The file you saved: ", input),
      );
      if (pretend) {
        row.append(button("pretend-zip", "Use the pretend factory image", () => takeFile(new File([core.selftest_factory_zip()], s.file.name))));
      }
      row.append(el("progress", { id: "zip-progress", max: 1, value: 0, hidden: true }));
      break;
    }
    case "answers":
      return answersForm();
    case "dry-run":
      row.append(
        button("dry", "Run the dry run", async () => {
          if (!phone) phone = await core.request_phone();
          await flow.dry_run(phone.again());
        }),
      );
      break;
    default:
      row.append(button("again", "Start over", () => start()));
  }
  return row;
}

function render() {
  const s = JSON.parse(flow.screen());
  const box = $("screen");
  box.hidden = false;
  box.className = s.state;
  box.dataset.state = s.state;
  box.dataset.stop = s.stop ?? "";
  // Where the install is, among its steps
  const steps = [...$("progress").children];
  const at = steps.findIndex((li) => li.dataset.state.split(" ").includes(s.state));
  $("progress").hidden = at < 0;
  steps.forEach((li, n) => {
    li.className = n === at ? "now" : n < at ? "past" : "";
  });
  const parts = [el("h2", { textContent: s.title })];
  // A check that was skipped: before everything else, and set apart
  if (s.notice) parts.push(el("p", { className: "notice", id: "notice", textContent: s.notice }));
  parts.push(...s.body.map((text) => el("p", { textContent: text })));
  // Someone else's words: set apart as theirs, with the pages they name
  if (s.quote) {
    const link = (l) => el("li", {}, el("a", { href: l.url, target: "_blank", rel: "noopener noreferrer", textContent: l.label }));
    parts.push(
      el(
        "div",
        { className: "quote", id: "quote" },
        el("p", { textContent: s.quote.intro }),
        el("blockquote", {}, ...s.quote.lines.map((text) => el("p", { textContent: text }))),
        ...s.quote.after.map((text) => el("p", { textContent: text })),
        el("ul", { className: "links" }, ...s.quote.links.map(link)),
      ),
    );
  }
  if (s.steps.length) {
    parts.push(
      el(
        "ol",
        { className: "steps" },
        ...s.steps.map((step) => {
          const li = el("li", { textContent: step.text });
          // A command for the person to type in a terminal: text, never run
          if (step.command) li.append(el("code", { className: "command", textContent: step.command }));
          return li;
        }),
      ),
    );
  }
  if (s.plan.length) parts.push(el("ul", { className: "plan" }, ...s.plan.map((line) => el("li", { textContent: line }))));
  if (s.problem) parts.push(el("p", { className: "problem", id: "problem", textContent: s.problem }));
  if (s.footer.length) parts.push(el("div", { className: "footer" }, ...s.footer.map((text) => el("p", { textContent: text }))));
  parts.push(controls(s));
  box.replaceChildren(...parts);
}

// ---- The self-test --------------------------------------------------------

// Goes through the page as a person would: it clicks the buttons the screens
// show, picks files in the file input and fills the form. Everything runs
// against the pretend phone in the wasm module; no USB device is touched.
async function selftest() {
  const result = {};
  const failures = [];
  const check = (name, ok, detail = "") => {
    result[name] = ok ? "pass" : `FAIL ${detail}`;
    if (!ok) failures.push(name);
  };
  const state = () => $("screen").dataset.state;
  const stop = () => $("screen").dataset.stop;
  const text = () => $("screen").textContent;
  const problem = () => $("problem")?.textContent ?? "";
  const click = async (id) => {
    const b = $(id);
    if (!b) throw new Error(`there is no #${id} on the '${state()}' screen`);
    b.click();
    await busy;
  };
  // `fetches`: a flow that offers the download, as the page's download code
  // needs it. The page as it ships begins none (section 1c)
  const begin = async (scenario, { reinstall = false, tamper = "", slotOverride = false, fetches = true } = {}) => {
    $("scenario").value = scenario;
    $("reinstall").checked = reinstall;
    await start(tamper, slotOverride, fetches);
  };
  const sent = () => JSON.parse(phone.fake_report());
  // getvar and nothing else; a locked phone at the unlock steps is also
  // asked `flashing get_unlock_ability`, `questions` times
  const onlyGetvar = (questions = 0) => {
    const r = sent();
    return r.commands > 0 && r.unlock_questions === questions && r.not_reads.length === 0 && r.written.length === 0 && r.active_slot === "a";
  };
  const notice = () => $("notice")?.textContent ?? "";
  const stepCount = () => document.querySelectorAll("#screen .steps li").length;
  const pick = async (file) => {
    const transfer = new DataTransfer();
    transfer.items.add(file);
    $("zip").files = transfer.files;
    $("zip").dispatchEvent(new Event("change"));
    await busy;
  };
  const answer = async (values) => {
    const form = $("answers");
    for (const [name, value] of Object.entries(values)) form.elements[name].value = value;
    form.requestSubmit();
    await busy;
  };
  // What the stand-in for Google's server was asked so far, and how it
  // answers from now on
  const asked = async () => (await (await fetch(`${standIn.origin}/asked`)).json()).length;
  const google = (mode) => {
    standIn.mode = mode;
  };
  // A ready pretend phone, taken to the step after Google's terms
  const toFactoryImage = async (accept, scenario = "ready") => {
    await begin(scenario);
    await click("accept");
    await click("look");
    await click(accept ? "accept-terms" : "decline-terms");
  };
  const marked = () => $("answers").querySelectorAll(".field-problem").length;
  const good = { username: "alex", password: "4812", password_again: "4812", hostname: "", timezone: "Europe/Paris", wifi_ssid: "", wifi_psk: "" };

  // 1. The whole flow, with every way the factory image and the answers are refused
  await begin("ready");
  const seen = [state()];
  check("the page says it is a preview that cannot write", $("banner").textContent.includes("cannot write to a phone") && !core.writes_enabled());
  check("the warning comes first, and nothing is asked of the phone before it", state() === "warning" && sent().commands === 0 && !$("look"));
  await click("accept");
  seen.push(state());
  await click("look");
  seen.push(state());
  // Google's terms, before anything is downloaded
  check("the page was served with a stand-in for Google's server, and a pretend run can reach nothing else of Google's", !!standIn && standIn.origin.startsWith("http://127.0.0.1:") && standIn.origin !== location.origin);
  const before = await asked();
  const quoted = [...document.querySelectorAll("#quote blockquote p")].map((p) => p.textContent);
  check(
    "the terms step quotes Google's words as Google's, apart from the page's own",
    state() === "google-terms" &&
      quoted.length === 2 &&
      quoted[0] === "Downloading of the system image and use of the device software is subject to the Google Terms of Service" &&
      quoted[1] === "By continuing, you agree to the Google Terms of Service and Privacy Policy" &&
      $("quote").textContent.includes("These are Google's words, not Omarchy Mobile's") &&
      $("quote").textContent.includes('"may also be subject to certain third-party terms of service, which can be found in Settings > About phone > Legal information"'),
    text(),
  );
  const termsLinks = [...document.querySelectorAll("#quote .links a")].map((a) => a.href);
  check("the terms step links Google's terms, its privacy policy and its page for this build", termsLinks.join(" ") === "https://policies.google.com/terms https://policies.google.com/privacy" && $("google").href === "https://developers.google.com/android/images#cheetah" && $("google").target === "_blank", termsLinks.join(" "));
  check("the terms step says whose the files are and that nobody passes them on", text().includes("for use only on your own Pixel, and Google does not allow them to be redistributed") && text().includes("Omarchy Mobile does not host them or pass them on") && text().includes("downloads nothing until you accept"));
  check("nothing is fetched and no file is taken before the terms are answered", !$("download") && !$("zip") && (await asked()) === before);
  await click("decline-terms");
  seen.push(state());
  check("not accepted: the page offers no download, says so, and takes the file the person saved", state() === "factory-image" && !$("download") && !!$("zip") && text().includes("You did not accept Google's terms in the installer, so it downloads nothing") && (await asked()) === before, text());
  const zip = core.selftest_factory_zip();
  const name = JSON.parse(flow.screen()).file.name;
  check("Google's page is a link to developers.google.com, opened by the person", $("google").href.startsWith("https://developers.google.com/android/images") && $("google").target === "_blank");
  await pick(new File([zip], "some-other-file.zip"));
  check("a file with another name is not taken", state() === "factory-image" && problem().includes("is not the file for this phone"), problem());
  await pick(new File([zip.slice(0, 1000)], name));
  check("a file of the wrong size is not taken", state() === "factory-image" && problem().includes("isn't complete"), problem());
  const flipped = zip.slice();
  flipped[flipped.length >> 1] ^= 1;
  await pick(new File([flipped], name));
  check("a file with another SHA-256 is not taken", state() === "factory-image" && problem().includes("SHA-256 differs"), problem());
  await pick(new File([zip], name));
  seen.push(state());
  await answer({ ...good, username: "Root", password_again: "4813" });
  check("answers outside the rules are marked, and what was typed is kept", state() === "answers" && marked() === 2 && $("answers").elements.username.value === "Root", String(marked()));
  await answer({ ...good, timezone: "Mars/Olympus" });
  check("a time zone the browser doesn't know is refused", state() === "answers" && marked() === 1);
  await answer(good);
  seen.push(state());
  await click("dry");
  seen.push(state());
  check("the steps come in the flow's order", seen.join(" ") === "warning phone google-terms factory-image answers dry-run done", seen.join(" "));
  const plan = [...document.querySelectorAll("#screen .plan li")].map((li) => li.textContent);
  check("the dry run lists slot B's images, userdata, set_active and reboot", plan.length === 8 && plan[0].endsWith("flash:boot_b") && plan[5].startsWith("flash userdata") && plan[7] === "reboot", plan.join(" | "));
  check("the last screen says nothing was written and that the flow ends there", text().includes("Nothing was written") && text().includes("no step that writes to a phone"));
  check("the whole flow sent getvar and nothing else", onlyGetvar(), JSON.stringify(sent()));
  result.commands_in_a_whole_flow = sent().commands;

  // 1b. Google's terms accepted: the page fetches the file. Each answer the
  // stand-in can give, and what the browser makes of it
  await toFactoryImage(true);
  const first = await asked();
  check("accepted: the next screen says so, with the build, and offers the download", state() === "factory-image" && /You accepted Google's terms for build TEST\.260101\.001 on 20\d\d-\d\d-\d\d at \d\d:\d\d UTC/.test(text()) && !!$("download") && !!$("zip") && (await asked()) === first, text());
  google("allowed");
  await click("download");
  check("a server that lets the page read: the file is fetched, checked, and the flow goes on", state() === "answers" && (await asked()) === first + 1, `${state()} ${$("failure")?.textContent ?? ""}`);
  google("refused");
  await toFactoryImage(true);
  await click("download");
  check(
    "a server that does not let the page read: the refusal is noticed, and the page falls back to the person's own download",
    state() === "factory-image" && !$("download") && !!$("zip") && !$("failure") && text().includes("this browser got no answer from dl.google.com that a web page may read") && text().includes("download the image from Google's page yourself") && stepCount() === 3 && $("google").href.startsWith("https://developers.google.com/android/images"),
    text(),
  );
  await pick(new File([zip], name));
  check("after the fallback, the file the person saved is taken", state() === "answers");
  google("unreachable");
  await toFactoryImage(true);
  await click("download");
  check("a server that can't be reached falls back the same way", state() === "factory-image" && !$("download") && !!$("zip") && text().includes("this browser got no answer from dl.google.com"), text());
  for (const [mode, says] of [
    ["wrong", "its SHA-256 is another than the one the list records"],
    ["longer", "sent more for"],
    ["size", "offered a file of"],
  ]) {
    google(mode);
    await toFactoryImage(true);
    await click("download");
    check(`a download that is not the listed file stops (${mode})`, stop() === "factory-download-mismatch" && text().includes(says) && text().includes("not what the release's signed list of Google's builds says") && text().includes("It has written nothing to it.") && !$("download") && !$("zip"), `${state()} ${stop()} ${text()}`);
  }
  google("moved");
  await toFactoryImage(true);
  await click("download");
  check("an answer that came from another address in the end is not taken", state() === "factory-image" && !$("download") && text().includes("sent the installer on to another address"), text());
  google("moved-same");
  await toFactoryImage(true);
  await click("download");
  check("a redirect that stays on the same host is fine", state() === "answers", `${state()} ${text()}`);
  google("missing");
  await toFactoryImage(true);
  await click("download");
  check("a server that has no such file: said, with the person's own download", state() === "factory-image" && !$("download") && text().includes("HTTP status 404 instead of the file"), text());
  google("broken");
  await toFactoryImage(true);
  await click("download");
  check("a transfer that breaks off leaves the download open and says how far it came", state() === "factory-image" && !!$("download") && /stopped before it was complete: \d+ of 49152 bytes had arrived/.test(problem()), `${problem()} ${text()}`);
  google("allowed");
  await click("download");
  check("and it can be tried again", state() === "answers");
  await toFactoryImage(true);
  await pick(new File([zip], name));
  check("with the download on offer, a file the person saved is taken as well", state() === "answers");
  google("allowed");

  // 1c. The page as it ships: it tells the core that it cannot fetch from
  // Google, and the flow asks for no terms and offers no download
  await begin("ready", { fetches: false });
  await click("accept");
  const unasked = await asked();
  await click("look");
  check(
    "the page as it ships is not asked for Google's terms: it goes straight to the person's own download and says why",
    state() === "factory-image" &&
      !$("accept-terms") &&
      !$("decline-terms") &&
      !$("download") &&
      !$("quote") &&
      !!$("zip") &&
      text().includes("A web page is not allowed to read files from Google's server, so this page cannot download the image for you.") &&
      text().includes("The installer's desktop app and its command-line tool can download it for you.") &&
      !text().includes("got no answer") &&
      stepCount() === 3 &&
      $("google").href.startsWith("https://developers.google.com/android/images") &&
      (await asked()) === unasked,
    text(),
  );
  await pick(new File([zip], name));
  check("the page as it ships takes the file the person saved, and nothing was fetched", state() === "answers" && (await asked()) === unasked);
  await begin("locked", { fetches: false });
  await click("accept");
  await click("look");
  await click("pretend-unlock");
  await click("look");
  check("the page as it ships: an unlocked phone comes to the same step, with no terms", state() === "factory-image" && !$("download") && !$("accept-terms"), state());

  // 2. A locked phone: the command is shown, nothing is sent
  await begin("locked");
  await click("accept");
  await click("look");
  const commands = [...document.querySelectorAll("#screen .command")].map((c) => c.textContent);
  check("a locked phone gets the unlock steps, with the command as text", state() === "unlock" && commands.length === 1 && commands[0] === "fastboot flashing unlock" && text().includes("does not send the unlock command"));
  check(
    "a locked phone is told first, right under the title, that this version cannot install and not to unlock for it",
    $("screen").children[1] === $("notice") &&
      notice() === "This version of the installer cannot install Omarchy Mobile yet. It only checks the phone and lists what an install would write. Unlocking the bootloader erases the phone, and there is no reason to unlock it for this version: do not unlock the phone for it." &&
      text().includes("This version gives you no reason to take them."),
    notice(),
  );
  check("a locked phone was sent getvar and the one unlock question, and nothing else", onlyGetvar(1), JSON.stringify(sent()));
  check("a locked phone with OEM unlocking on is told so, without the Developer options step", text().includes('"OEM unlocking" is already switched on') && !text().includes("Developer options") && stepCount() === 3, text());
  await click("look");
  check("still locked: the same screen", state() === "unlock");
  await click("pretend-unlock");
  await click("look");
  check("unlocked by its owner: on to Google's terms for the factory image", state() === "google-terms");

  await begin("locked-oem-off");
  await click("accept");
  await click("look");
  check("a locked phone with OEM unlocking off is told to switch it on first", state() === "unlock" && text().includes('"OEM unlocking" is not switched on yet') && text().includes('Switch on "OEM unlocking"') && stepCount() === 4 && onlyGetvar(1), text());
  await begin("locked");
  phone.fake_unlock_answer("something this installer doesn't know");
  await click("accept");
  await click("look");
  check("a phone that doesn't answer the unlock question gets the steps for both cases", state() === "unlock" && !text().includes("The phone says") && text().includes('If "OEM unlocking" is not switched on yet') && stepCount() === 4 && onlyGetvar(1), text());

  // 3. Another phone comes back after the unlock steps
  await begin("locked");
  await click("accept");
  await click("look");
  phone.fake_swap();
  await click("look");
  check("another phone than the first stops the install", stop() === "phone-changed" && text().includes("written nothing to either") && text().includes("erased that phone"), text());

  // 4. Each stop, with what it says of the phone
  for (const [scenario, id] of [
    ["other-model", "wrong-model"],
    ["slot-b", "runs-from-slot-b"],
    ["one-slot", "bootloader-slots-unknown"],
    ["slots-differ", "bootloader-slots-differ"],
    ["unknown-build", "build-not-listed"],
    ["untested-build", "build-not-tested"],
    ["untested-old-build", "build-not-tested"],
  ]) {
    await begin(scenario);
    await click("accept");
    await click("look");
    check(`${scenario} stops (${id})`, state() === "stopped" && stop() === id && text().includes("It has written nothing to it.") && onlyGetvar() && !$("look") && !$("zip"), `${state()} ${stop()}`);
  }
  await begin("one-slot");
  await click("accept");
  await click("look");
  check("the slot stop hands out no way around and says a retry stops again", text().includes("will stop here again") && !text().includes("--") && !text().includes("override") && !notice() && sent().not_reads.length === 0);
  await begin("untested-old-build");
  await click("accept");
  await click("look");
  check("not tested, and a newer build is: update Android to it", text().includes("If Android offers an update to it, install it") && text().includes("once that is done"), text());
  await begin("untested-build");
  await click("accept");
  await click("look");
  check(
    "not tested, and nothing newer is: told to wait, not to update Android",
    $("screen").querySelector("h2").textContent.endsWith("has not been tested with Omarchy Mobile yet") && text().includes("Nothing you can install today changes that") && text().includes("What you can do is wait") && text().includes("/issues") && !text().includes("System update") && text().includes("will stop here again"),
    text(),
  );

  // 4b. The maintainer's override for the bootloader-slot stop: the page has
  // no control for it, and when it is set the core's notice is on every screen
  const skipped = "The bootloader-slot check was skipped by a maintainer override.";
  const notEvidence = "This run is not evidence that an install would be allowed.";
  check("the page offers no control for the override", !document.querySelector("main").innerHTML.includes("override") && !document.querySelector("[id*=override], [name*=override], [id*=maintainer]"));
  await begin("one-slot", { slotOverride: true });
  check("nothing is said before a check was skipped", !notice());
  await click("accept");
  await click("look");
  const noticed = [state() === "google-terms" && notice().startsWith(skipped) && notice().endsWith(notEvidence)];
  await click("decline-terms");
  noticed.push(state() === "factory-image" && notice().endsWith(notEvidence));
  await click("pretend-zip");
  noticed.push(state() === "answers" && notice().endsWith(notEvidence));
  await answer(good);
  noticed.push(state() === "dry-run" && notice().endsWith(notEvidence));
  await click("dry");
  noticed.push(state() === "done" && notice().startsWith(skipped) && notice().endsWith(notEvidence));
  check("with the override, every screen after the look says the check was skipped and the run is no evidence", noticed.every(Boolean), noticed.join(" "));
  check("with the override, the notice comes right under the title, and the title doesn't say passed", $("screen").children[1] === $("notice") && $("screen").children[0].textContent === "The dry run finished with one check skipped. Nothing was written", $("screen").children[0].textContent);
  check("with the override, the dry run still sent getvar and nothing else", onlyGetvar(), JSON.stringify(sent()));
  await begin("slots-differ", { slotOverride: true });
  await click("accept");
  await click("look");
  check("with the override, two bootloader versions that differ still stop", stop() === "bootloader-slots-differ" && !notice());
  await begin("ready", { slotOverride: true });
  await click("accept");
  await click("look");
  check("with the override, a phone that reports both slots gets no notice", state() === "google-terms" && !notice());
  await begin("slot-b", { reinstall: true });
  await click("accept");
  await click("look");
  check("slot B, said to be a reinstall, goes on", state() === "google-terms");

  // 5. The warning declined; a release and an index that fail their checks
  await begin("ready");
  await click("decline");
  check("declining the warning ends the install with nothing sent", stop() === "warning-declined" && sent().commands === 0 && text().includes("Nothing was sent to a phone."));
  await begin("ready", { tamper: "manifest" });
  check("a changed manifest stops before the warning", stop() === "release-not-verified" && !$("accept") && $("release").className === "bad");
  await begin("ready", { tamper: "index" });
  check("a changed build index stops before the warning", stop() === "build-index-refused" && !$("accept"));

  // 6. The published release's own files, with the release keys
  const published = await Promise.all(RELEASE_FILES.map((n) => bytes("release/" + n)));
  const real = core.InstallFlow.begin(...published, undefined, false);
  check("the published release verifies with the built-in release keys", !!real.release() && JSON.parse(real.release()).version === "2026.11.08");
  check("the published release has no build index, and the flow says so", JSON.parse(real.screen()).stop === "no-build-index", JSON.parse(real.screen()).stop);
  published[3][100] ^= 1;
  check("the published release with one changed byte is refused", JSON.parse(core.InstallFlow.begin(...published, undefined, false).screen()).stop === "release-not-verified");

  // 7. The write path against the pretend phone, and zstd (as before)
  const install = JSON.parse(await core.selftest());
  check(
    "an install against the pretend phone writes slot B and userdata only, and the release keys refuse the test bundle",
    install.install_done && install.userdata_matches && install.boot_b_matches && install.partitions_written.length === 6 && install.test_key_refused_by_release_keys && install.wrong_phone_refused,
    JSON.stringify(install),
  );
  result.zstd_mib_per_s = Math.round(install.zstd_mib_per_s);

  result.failed = failures;
  result.pass = failures.length === 0;
  await begin("ready", { fetches: false });
  return result;
}

// ---- Start ----------------------------------------------------------------

async function main() {
  try {
    await init();
    show("core", `loaded${core.writes_enabled() ? "" : " (this build can't write to a phone)"}`, "ok");
  } catch (e) {
    show("core", `the installer did not load: ${e.message ?? e}`, "bad");
    return;
  }
  if (core.writes_enabled()) $("banner").textContent = "This build can write to a phone.";
  const selftesting = query.has("selftest");
  if (query.has("pretend") || selftesting) {
    if (!core.fake_phone) {
      show("release", "this build has no pretend phone (web/build.sh selftest makes one that has)", "bad");
      return;
    }
    pretend = true;
    $("scenario").append(...core.fake_phone_scenarios().map((name) => el("option", { value: name, textContent: name })));
    $("scenario-label").hidden = false;
  }
  $("options").hidden = false;
  $("restart").addEventListener("click", () => act(() => start()));
  $("scenario").addEventListener("change", () => act(() => start()));
  $("reinstall").addEventListener("change", () => act(() => start()));
  try {
    await start();
  } catch (e) {
    show("release", e.message ?? String(e), "bad");
  }
  if (selftesting) {
    $("selftest-section").hidden = false;
    let result;
    try {
      result = await selftest();
    } catch (e) {
      result = { pass: false, error: e.message ?? String(e), on_screen: $("screen").dataset.state };
    }
    show("selftest", JSON.stringify(result, null, 1), result.pass ? "done" : "bad");
    // For a headless run: the demo server prints what it is sent
    await fetch("selftest-result", { method: "POST", body: JSON.stringify(result) });
  }
  document.documentElement.dataset.ready = "1";
}

main();
