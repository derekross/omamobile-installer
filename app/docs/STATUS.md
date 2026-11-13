# omainstall: where it stands

Status (2026-11-13): **the app and the tool's `json` mode it drives have run against the pretend phone only. Neither has ever listed, opened or written a real USB device.** The write path they now reach is the core's (docs/DESIGN.md §11), which has not run on a real phone either: its first attended run is laid out in [docs/WRITE-RUNBOOK.md](../../docs/WRITE-RUNBOOK.md), with the command-line tool.

This file is the gap list for the desktop app: what works against the pretend phone, what has never met a real phone, what is left out of the app on purpose, and the first attended run of the app against a real Pixel 7 Pro. The protocol and the window's design are in [docs/APP.md](../../docs/APP.md).

**Read this first: on a Pixel 7 Pro the app does reinstalls, and stops a first install before anything is written, by design.** That phone's bootloader tells the version of one of its two copies only. On a first install (the phone runs from slot A) the flow stops there (`bootloader-slots-unknown`): what passes that stop is the owner's statement on the tool's own command line (`install --bootloader-slots-match VERSION`, DESIGN §6), and the statement is the command line's alone. The app has no control for it, the `json` mode no argument and no message, and `tools/check-shipping` fails if either gains one. On a reinstall (the phone runs from slot B) the core asks what slot B holds; answered "Omarchy Mobile", the bootloader-slot check passes with the version slot B reports, the bootloader the phone already runs, and the install switches no slot (decided by the maintainer on 2026-11-13, DESIGN §6, "A reinstall from slot B"). So the app can take a real Pixel 7 Pro to a write as a reinstall; the write path's attended run (WRITE-RUNBOOK) still comes first (below, "What stands between the app and a real install").

## 1. What works, against the pretend phone

Each line was run on 2026-11-13 by `app/bin/test` (offscreen, Qt 6.12) or by `cargo test -p omamobile-installer-cli --features fake-phone`. The phone is the core's pretend phone held in the tool's memory (`--fake-phone`, in builds with the `fake-phone` feature only); Google's server is a socket on 127.0.0.1 in the test.

- **Every screen the core can emit is drawn.** One recorded screen for each state (`warning`, `phone`, `slot-b`, `unlock`, `google-terms`, `factory-image`, `answers`, `dry-run`, `done`, `confirm`, `write`, `installed`, `stopped`) and for each of the 24 kinds of stop of DESIGN §4, 60 files in `app/tests/fixtures/screens/`, each compared with the window text for text: title, notice, body, quote, steps and their commands, link, file, plan, problem, footer. The list of stop ids the test holds is DESIGN §4's, checked both ways against the recorded screens; a stop added to the core has to be added to the recorder (`app/tests/record-screens`) and to that list by hand, and nothing fails until someone does.
- **The whole flow to the dry run**, in a window started without `--write`: the warning, the look at the phone, Google's terms accepted or declined, the download with its bytes, a file handed in, the answers with the core's objections per field, the dry run, the plan.
- **A complete pretend install**, in a window started with `--write`: all of the above, then the list of what will be written, the codename typed, the writes, the last screen. The pretend phone's own record is read at the end: slot B's five boot images, the pieces of `userdata`, `set_active:b`, `reboot`, and nothing else; six partitions written.
- **An install the owner does not go ahead with**: "Do not install", or anything typed that is not the codename, ends in the core's stop `not-confirmed`, and the pretend phone received questions only.
- **A phone unplugged mid-write** (the pretend phone's cable pulled as the second piece of `userdata` is sent): the core's stop `write-interrupted` with its account of how far the writes got, the tool's record of what was sent and what was not confirmed, "Start again" because the core says a new run can help. No `set_active:b`, no `reboot`.
- **A download that fails**: a connection that breaks after 20000 bytes leaves the step open with the core's sentence; the next try asks the stand-in for the rest only (`Range: bytes=20000-`) and ends with the right file. A server that has no such file: the step falls back to the owner's own file, with the core's reason.
- **A download cancelled** and gone on with from what arrived; a download that is not the listed file (the core's stop).
- **The stops a phone can cause at the first look**, each from a pretend phone in that state: another model, slot B, one bootloader slot reported, two that differ, a build that is not listed, not tested, not tested with a newer one tested.
- **A phone that tells one bootloader slot stops also in a run that writes**, and nothing the app sent names a slot or a version.
- **A phone that runs from slot B is asked what slot B holds**: the core's question with its three answers as buttons, each answer's message, the stop for "Android" and for "not sure", and a reinstall of a phone that tells one bootloader copy (the Pixel 7 Pro's case) through the confirmation, the codename and the writes, against the real tool with its pretend phone: six partitions written, `set_active:b`, no `set_active:a`, and nothing the app sent names a bootloader version.
- **A locked phone**: the unlock steps, the command shown and never sent, and the phone looked at again.
- **Two phones**: the tool's question, one button per phone, no serial number in either.
- **A write is never cut off by the window.** While the phone is being written there is no cancel; closing the window (or `Q`) does not ask, it says that the window stays; and if the app is ended anyway it tells the tool to leave and waits for the write to end (up to 30 minutes) instead of ending the tool. Tested against the stand-in, which holds a write open. The same when the tool says something the app cannot read mid-write: the app stops reading, does not end the tool, and offers no new run until the tool has left; then it says that it does not know how the write ended. And if the app dies mid-write, the tool finishes the write before it leaves (a test of the tool closes its output just before the writes).
- **A write that finds no phone** (the tool's `error`) keeps the step open and is tried again only by the person's click.
- **When the tool itself fails**: no tool, a tool that can't be started, no hello, another protocol number, a tool started for the other kind of run (it writes when a dry run was asked for, or the reverse), malformed lines, a crash.
- **The tool's side** (`cli/tests/json_mode.rs`, 23 tests): the protocol message by message, a whole install as a transcript with what the tool said it sent compared with what the phone received, the cable pulled before the first write, among the boot images and inside `userdata`, a quit written behind a write, and that a run not started with `--write` sends nothing but reads whatever it is sent.

The pictures in [screens/](screens/) are that walk: `app/bin/screens` writes them again.

## 2. What has never run against a real phone, or a real desktop

- **The `json` mode's USB path**: listing the phones in Fastboot Mode, labelling them (the product name a Pixel gives over USB, and the bus and port), opening one, pinning it by its serial number. It follows what `install` does and has never run. Nobody knows what the `phones` label reads for a Pixel, or how two connected phones show.
- **Everything DESIGN §9 lists as unknown for the core**: how a real bootloader answers each `getvar`, whether a locked one answers them, `flashing get_unlock_ability`, `max-download-size`, timing.
- **The write path** (DESIGN §11): the sparse pieces, the time a `flash:` takes against the 300 s wait, the slot marks afterwards, `reboot` answered or not. Its first run is WRITE-RUNBOOK's, on the command line. The app adds nothing to what is sent and takes nothing away: `json --write` goes through the same `walk_images` and the same session as `install --write`.
- **A download from Google's real server.** Every test uses a stand-in. `dl.google.com` was measured once by hand (DESIGN §7), not fetched by this code.
- **The window on a real desktop.** Every picture is an offscreen render with Qt's software backend, the fallback accent (`#FFD60A`) and sharp corners. Not seen: the window under Hyprland, the accent and rounding followed live from a running Omarchy, the icon and the Wayland app id in the launcher, a real keyboard (the one key test is skipped where the platform gives the window no focus; it ran here).
- **The file chooser.** `PortalFilePicker` (xdg-desktop-portal over D-Bus) has run against no portal: the tests hand in a fake. Dropping a file on the window was not tried with a real drag either.
- **The Arch package.** `app/pkgbuild/PKGBUILD` was not built in this work (`makepkg` was not run), and it does not package the tool: the app looks for `omamobile-install` beside itself or on `PATH`.
- **Opening a link.** The tests replace the browser with a counter.
- **A long write.** The pretend phone answers in microseconds: the progress bar, the "last 8 of N" list and the wait at shutdown have not been watched over minutes. A real root image is cut into hundreds of pieces, not six.
- **Ctrl-C in the terminal the app was started from.** It reaches the app and the tool alike, as it reaches `install --write`: a write would be cut off. The window's own close is held; a signal is not.

## 3. What is deliberately not in the app

| Not in the app | Why |
| --- | --- |
| The owner's statement about a bootloader slot the phone does not report (`--bootloader-slots-match`) | Decided: the command line only (DESIGN §6). It is typed by the owner at their own keyboard, with the version read from the phone. So a Pixel 7 Pro stops in the app at `bootloader-slots-unknown`, and the app shows that stop as the core words it, with "Close" alone |
| The maintainer's override for that stop | Hidden, dry runs only, the terminal's `install --dry-run` only (DESIGN §6). The window draws the notice such a run carries, should one ever arrive |
| A control in the window that turns writing on | The write switch is the app's own `--write`, passed on as the tool's `--write`: said once, when the window is started, and shown as "Install" or "Dry run" in the header from the first screen. A checkbox would be a second way to it, one click from an erased phone. `tools/check-shipping` holds the switch to those two lines |
| A second "are you sure" after the codename | The codename is the go-ahead, by design (DESIGN §3, `confirm`). The writes follow at once, as on the command line. The button that hands the codename in is disabled until something is typed, and "Do not install" is beside it |
| Cancelling a write | A write that is cut off leaves a phone half written. The tool has no message for it and the window no button |
| A serial number, anywhere | The tool pins the phone and never says its serial; the app is told a label (APP §3.3) |
| A way to run a command | A step's `command` is shown and copied, never run (DESIGN §7, rule 2) |
| A proxy, a work directory, a factory image named up front | The tool's `json` mode takes `--proxy` and `--work-dir` for scripts; the app passes neither, and the owner hands a saved file in at the step |
| A control that says "reinstall" up front | Not needed: a phone on slot B is asked about with the core's question and the core's answers (`slot-b`). `omainstall open --reinstall` still says it up front, and the question is then not asked |
| Finding or fetching a release | The tool takes a directory. `omainstall open DIR` passes it on |
| The owner's answers reaching the phone | Not built in the core (DESIGN §12, decided: the install link). The form is drawn, the core checks it, and the core's screens say three times that the phone starts with no account |
| Anything after the restart | Not built: no radio backup, no firmware, no check (DESIGN §9) |
| The core's sentences in another language | They are composed in the core, in English (APP §8). The app's own strings have German as the working example |
| Cancelling the check of a file handed in, or a dry run | The protocol cancels a download and nothing else; `quit` ends a file check |

## 4. What stands between the app and a real install

In the order they bind:

1. **The write path's first attended run** (WRITE-RUNBOOK, with the command-line tool). Until it has run, treat `omainstall open --write` like `install --write`: untested code that erases a phone.
2. **A first install through the app.** Decided on 2026-11-13: first installs stay on the command line for now, and the app does reinstalls (a Pixel 7 Pro that runs from slot B, DESIGN §6). A first install on a Pixel 7 Pro needs the owner's statement, which the app does not take; the goal is that the installer measures the other slot itself (DESIGN §6, "Measuring the other slot: the later goal", a design with its attended test and no code yet).
3. **A release that carries the build index.** The published 2026.11.08 has none, and the flow stops for it before it looks at a phone.
4. **A phone one can use afterwards**: the owner's answers (DESIGN §12) and every step after the restart.
5. **The tool beside the app**: a package for `omamobile-install`, or the app's package carrying it.

## 5. Tools this work did not have

None was missing for what was run. `wasm-bindgen` is not on `PATH` on the machine this was written on; `web/build.sh` was run with the copy in the user's cargo directory first on `PATH` (the version `Cargo.lock` names). `makepkg` was not run. `web/selftest.sh` was not run by this work: the page was not changed by it, beyond what the two branches it was built from already held.

## 6. The first attended run of the app against a real Pixel 7 Pro: runbook

Status: **not run.** Written offline, with no phone. Nothing in this section has been tried on hardware.

This run is read-only from its first step to its last. Without saying it is a reinstall the window asks what slot B holds, and "I am not sure" ends it there; said to be a reinstall, it goes to the dry run's end, and in a window started with `--write` to the confirmation, where "Do not install" ends it with nothing written. Its purpose is what only a real phone and a real desktop can show: that the `json` mode finds and opens a phone over USB, that the window draws a real phone's stop, and what the app looks like under Omarchy. **It does not write, and it is not the write path's first run.** That is WRITE-RUNBOOK, and it comes first: this run should not be the first time this code talks to a real bootloader.

The maintainer runs every step, or approves it, at the keyboard. No identifier goes into this file or into anything committed: no serial number, no `getvar all` output, no picture that shows one. Placeholders are WRITE-RUNBOOK's: `<RELEASE_DIR>` (a signed release for `cheetah` that carries `factory-builds.json`), `<OMARCHY_MOBILE>` (the omarchy-mobile checkout), `<NV_BACKUP_DIR>`.

### Before anything

- WRITE-RUNBOOK's steps 0 to 3 have passed on this phone with this commit (the radio backup matches, the release verifies, the phone's state was recorded, and `install --dry-run` ran against it). If its step 3 stopped, this run waits for that to be understood.
- One phone is connected. A second phone in Fastboot Mode would bring up the chooser, which is worth seeing only on purpose.
- The phone is the maintainer's test Pixel 7 Pro, running Omarchy Mobile from slot B.

### 0. What is under test (no phone)

```sh
cargo build --release --locked -p omamobile-installer-cli
web/build.sh
app/bin/build
app/bin/test
tools/check-shipping --built
git rev-parse --short HEAD
```

- **Pass:** the tests end with `0 failed, 0 skipped`; every line of `check-shipping` is `ok`, among them "the built app holds nothing of the pretend phone". Record the commit.
- **Stop:** any `not ok`, any failed or skipped test.
- **Rollback:** none needed.
- **Risk:** none. No USB device is listed or opened: the tests name a pretend phone, and `check-shipping` reads files.

### 1. The app finds the tool (no phone is asked anything)

```sh
OMAINSTALL_TOOL=$PWD/target/release/omamobile-install app/build/omainstall check
```

- **Expect:** three lines: the tool's path, its version, `protocol: 3 (this app speaks 3)`.
- **Pass:** exit status 0.
- **Stop:** anything else. The reason is on standard error.
- **Rollback:** none needed.
- **Risk:** none. `check` reads the tool's hello and sends `quit`; before `start` the tool opens no USB device (APP §3.2).

### 2. The phone in Fastboot Mode, and its state before (read-only)

As WRITE-RUNBOOK's step 2: stop the modem, restart into the bootloader, and read the same `getvar` values with Android's `fastboot`, used for `getvar` only.

```sh
cd <OMARCHY_MOBILE>
tools/phone-ssh 'systemctl stop omamobile-modem'
bin/omamobile fastboot
serial=$(fastboot devices | awk '$2 == "fastboot" { print $1 }')   # one line, or stop
for v in product unlocked current-slot version-bootloader version-baseband \
         slot-successful:b slot-unbootable:b slot-retry-count:b; do
  fastboot -s "$serial" getvar "$v" 2>&1 | head -1
done
```

- **Pass:** one phone; `product: cheetah`; `current-slot: b`; `unlocked: yes`.
- **Stop:** more than one phone; another product; `current-slot: a` (then this is not the phone this runbook describes).
- **Rollback:** none needed. To leave: `fastboot -s "$serial" reboot` starts Omarchy Mobile again.
- **Risk:** none (`getvar` reads). Write the values down without the serial: step 6 compares against them.

### 3. The window for a dry run, without saying it is a reinstall (read-only)

```sh
OMAINSTALL_TOOL=$PWD/target/release/omamobile-install app/build/omainstall open <RELEASE_DIR>
```

In the window: read the warning, "I have read this. Go on", then "Look at the phone". At the question, "I am not sure".

- **Expect:** the header says "Dry run" and "This run cannot write to a phone." The warning ("Read this once"), "Connect the Pixel 7 Pro", and after the look the question **"What is on this phone's slot B?"** with three buttons and no main action. After "I am not sure", the stop **"Find out what is on slot B first"** under the word "Stopped", with "Close" and "Start again".
- **Pass:** the question, then that stop. It is the first time the `json` mode listed and opened a USB device, and the phone answered `getvar`. The answer sent nothing to the phone.
- **Stop:** "The installer did not run" (the tool failed: copy its words into the results); a chooser with more than one phone; any other screen. A stop with another title is a result, not a failure: record its title and its closing lines, and do not go on to step 4 until it is understood.
- **Rollback:** none needed. "Close" ends the tool.
- **Risk:** low. The tool sends `getvar` and nothing else: a run without `--write` cannot reach a write whatever is clicked (its tests send every message that could ask for one). If the phone stops answering, send it nothing more; `fastboot -s "$serial" getvar product` once, and if that is silent too, restart it into Fastboot Mode with the keys.

### 4. The window for a dry run, as a reinstall (read-only)

```sh
OMAINSTALL_TOOL=$PWD/target/release/omamobile-install app/build/omainstall open --reinstall <RELEASE_DIR>
```

The same two clicks. Then, at Google's terms, "I will download it myself", and hand in `<FACTORY_ZIP>` (the file WRITE-RUNBOOK's run used): this run downloads nothing. The answers: made-up ones. Then "Run the dry run".

- **Expect:** no question (the run was said to be a reinstall); "Google's terms for the factory image"; the file taken; the dry run; **"The dry run passed. Nothing was written"**, whose body has the line "Bootloader `<BOOTLOADER>`: slot B's bootloader, which the phone already runs; the install switches no slot." and whose plan ends with `set_active:b`.
- **Pass:** that screen. It is the first real phone through the app's dry run, and the first run of the reinstall rule of 2026-11-13 on a real bootloader.
- **Stop:** as step 3; **the stop "The installer can't check the phone's second bootloader copy"** (then the phone did not report `current-slot: b` to the tool, though step 2 read it: record both); and anything that would download (a click on "I accept" is not part of this run).
- **Rollback:** none needed.
- **Risk:** low, as step 3: a run without `--write` reads, whatever is clicked.

### 5. The window for an install (the flow's write switch is on; nothing is written)

Only after steps 3 and 4 ended at their stops.

```sh
OMAINSTALL_TOOL=$PWD/target/release/omamobile-install app/build/omainstall open --write --reinstall <RELEASE_DIR>
```

The same two clicks.

The same steps as step 4, to the confirmation. **Type nothing in the codename field.** Click "Do not install".

- **Expect:** the header says "Install", in red, and "This run can write to a phone. It writes only after it has listed what it will write and you have typed the phone's codename." Six stages, the last one "Install". After the dry run, **"This is what the installer will write"**: the same bootloader line as step 4, the six partitions with their sizes, `set_active:b`, `reboot`, what is erased, and the field for the codename. After "Do not install", the stop **"Not confirmed"**.
- **Pass:** that confirmation, then that stop; nothing was typed.
- **Stop:** **anything else. If the window shows "Writing to the phone", it was not this runbook's run: do not touch the phone or the cable, and let the write end** (a write that is cut off leaves a phone half written; the window holds itself open). A codename typed here is followed by real writes through a path that has not had its attended run with the app; that run is its own decision.
- **Rollback:** none: nothing was written, and step 6 checks it.
- **Risk:** medium: a tool started with `--write` talking to a real phone, one typed word from an erase. What keeps it from a write is the typed codename, and behind it the flow's checks in `begin_write` and the session's gate.

### 6. The phone's state after, and leaving (read-only)

```sh
for v in product unlocked current-slot version-bootloader version-baseband \
         slot-successful:b slot-unbootable:b slot-retry-count:b; do
  fastboot -s "$serial" getvar "$v" 2>&1 | head -1
done
fastboot -s "$serial" reboot
```

- **Pass:** every value as in step 2; the phone starts Omarchy Mobile from slot B as before, with its account, and the modem comes up at boot as usual.
- **Stop:** any value that changed (record both, and treat the phone as WRITE-RUNBOOK's step 8 treats one after a write: reinstall with `tools/install-guided --start-over`). A phone that does not start Omarchy Mobile.
- **Rollback:** WRITE-RUNBOOK's step 8.
- **Risk:** none (`getvar` reads; `reboot` is the ordinary way out of Fastboot Mode).

### 7. What the window looked like (no command)

- **Record, in words:** whether the accent and the corner radius were the running theme's; whether the launcher showed the icon after `app/bin/install` (optional, and it asks for a password); what `Tab`, `Space`, `?` and `Q` did; whether the text could be selected and copied into an issue.
- A picture only if it shows no serial number and no network name: a stop's screen shows neither.

### Not answered by this run

- **A real download, and a first install**, through the app. The download is its own attended run; a first install on a Pixel 7 Pro stops at the bootloader-slot stop (section 4, point 2).
- **A write through the app on real hardware.** Not attempted here on purpose. When there is a phone for it, its runbook is WRITE-RUNBOOK's with the window in place of the terminal, and it adds what only the app has: that the window stays while the phone is written, and that a quit behind a write waits for it.
- **Two phones connected**, and a phone with no serial number.
- **A locked phone.**

### Results

Not run yet. For each run: the date, the commit, the release's version, each step's outcome in a line, the title of each stop as shown, step 2's and step 6's `getvar` values side by side (no serial), and step 7's notes.
