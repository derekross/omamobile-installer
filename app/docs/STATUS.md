# omainstall: where it stands

Status (2026-11-13): **the app and the tool's `json` mode it drives have run against the pretend phone only. Neither has ever listed, opened or written a real USB device.** On 2026-11-13 the app was used outside these tests to install a real Pixel 7 Pro as a reinstall (slot B and `userdata` written, the phone started); after the write it stopped, and that phone had no firmware. Since then a window started with `--write` goes on after the write with the steps `tools/install-guided` takes (DESIGN §13): those have run against stand-ins only, and their first attended run is section 7. The write path they now reach is the core's (docs/DESIGN.md §11), which has not run on a real phone either: its first attended run is laid out in [docs/WRITE-RUNBOOK.md](../../docs/WRITE-RUNBOOK.md), with the command-line tool.

This file is the gap list for the desktop app: what works against the pretend phone, what has never met a real phone, what is left out of the app on purpose, and the first attended run of the app against a real Pixel 7 Pro. The protocol and the window's design are in [docs/APP.md](../../docs/APP.md).

**Read this first: on a Pixel 7 Pro the app does reinstalls, and stops a first install before anything is written, by design.** That phone's bootloader tells the version of one of its two copies only. On a first install (the phone runs from slot A) the flow stops there (`bootloader-slots-unknown`): what passes that stop is the owner's statement on the tool's own command line (`install --bootloader-slots-match VERSION`, DESIGN §6), and the statement is the command line's alone. The app has no control for it, the `json` mode no argument and no message, and `tools/check-shipping` fails if either gains one. On a reinstall (the phone runs from slot B) the core asks what slot B holds; answered "Omarchy Mobile", the bootloader-slot check passes with the version slot B reports, the bootloader the phone already runs, and the install switches no slot (decided by the maintainer on 2026-11-13, DESIGN §6, "A reinstall from slot B"). So the app can take a real Pixel 7 Pro to a write as a reinstall; the write path's attended run (WRITE-RUNBOOK) still comes first (below, "What stands between the app and a real install").

**After the write (protocol 4):** for a release with the install link, the window goes on by itself to the phone's first start and draws the link's screens: the code from the phone's screen, a folder for the radio backup (the portal's folder chooser, never tried on a desktop), the factory image sent, the firmware, the restart, ready (DESIGN §13, APP.md §3.9; pictures 40 to 51 in `app/docs/screens`). Run only against the tool's pretend phone that speaks the link (`theScreensOfTheInstallLink`); never against a phone or its daemon.

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

- **The steps after the write** (DESIGN §13), in a window started with `--write`, for a release that ships the tools for them (`fixtures/after`, with stand-ins for the release's tools, `nmcli` and `ssh-keygen`, and a pretend phone that answers over "USB SSH" kept in a folder): the last screen of the write with "Set up the phone"; the USB connection explained, its command shown, added on the click, or refused by NetworkManager and shown for the owner to run; the first start waited for, and run out of its bound; the radio backup into a folder the screen names, with the IMEI warning; the firmware (the window holds while it runs, as for a write); the restart and the check in plain words; a setup cut off and offered again when the window is opened again, gone on with or forgotten; another phone answering, which is asked which it is and nothing else; the phone refusing this computer's key; a tool of the release that is not the signed one, which never runs. `theRealToolSetsUpThePhoneAfterTheWrite` in the app, eleven tests in `cli/tests/after_write.rs`.
- **One run from the write to a working phone** (DESIGN §12 and §13 together): the write puts the answers and this computer's SSH key into the placeholder `firstboot.conf`; the pretend phone's first start (a stand-in for `omamobile-firstboot`, reading that block out of the `userdata` the pretend phone received) refuses the key until it has given root the answers' key, then takes it and no other; then the backup, the firmware, the restart and the check. A key file changed after the write is refused to the end of the wait, said plainly, and the run goes on once the right one is back. The key is in nothing the tool says.
- **The package** (`app/pkgbuild/PKGBUILD`): built once on 2026-11-13 with `makepkg --nodeps -f` (its `check()` ran the shipping tool's unit tests). It holds `/usr/bin/omainstall`, `/usr/bin/omamobile-install` (a shipping build: no pretend phone in it), the `.desktop` file and the icon; `omainstall check` from the built package found the tool beside it and read protocol 4. It was not installed (`makepkg -si` needs a password), and no dependency was installed for it.

The pictures in [screens/](screens/) are that walk: `app/bin/screens` writes them again. Pictures 39 to 50 are the run that goes on after the write: 39 its confirmation, with this computer's key among the answers, and 50 a key that is not the one in the answers.

- **The owner's answers into the system image** (DESIGN §12, decided on 2026-11-13). The answers form is the same; at the confirmation and after the install the core's screens now say that the phone sets up the account from them at its first start, and the unlock notice of a run that writes says that this version's writes have met only the pretend phone. The core puts the answers into the two placeholder blocks the release's root image keeps for them as it streams the image: the pretend phone's `userdata` holds the image with exactly those two blocks changed, and a test installs a real 8 MiB ext4 image that way and finds it clean with `e2fsck -fn` and the answers in the files with `debugfs` (`core/tests/answers.rs`). A release without placeholders stops a run that writes before it asks a phone anything (`stop-no-place-for-answers`, recorded and drawn). Never run against a real phone or a full root image.

## 2. What has never run against a real phone, or a real desktop

- **The `json` mode's USB path**: listing the phones in Fastboot Mode, labelling them (the product name a Pixel gives over USB, and the bus and port), opening one, pinning it by its serial number. It follows what `install` does and has never run. Nobody knows what the `phones` label reads for a Pixel, or how two connected phones show.
- **Everything DESIGN §9 lists as unknown for the core**: how a real bootloader answers each `getvar`, whether a locked one answers them, `flashing get_unlock_ability`, `max-download-size`, timing.
- **The write path** (DESIGN §11): the sparse pieces, the time a `flash:` takes against the 300 s wait, the slot marks afterwards, `reboot` answered or not. Its first run is WRITE-RUNBOOK's, on the command line. The app adds nothing to what is sent and takes nothing away: `json --write` goes through the same `walk_images` and the same session as `install --write`.
- **A download from Google's real server.** Every test uses a stand-in. `dl.google.com` was measured once by hand (DESIGN §7), not fetched by this code.
- **The window on a real desktop.** Every picture is an offscreen render with Qt's software backend, the fallback accent (`#FFD60A`) and sharp corners. Not seen: the window under Hyprland, the accent and rounding followed live from a running Omarchy, the icon and the Wayland app id in the launcher, a real keyboard (the one key test is skipped where the platform gives the window no focus; it ran here).
- **The file chooser.** `PortalFilePicker` (xdg-desktop-portal over D-Bus) has run against no portal: the tests hand in a fake. Dropping a file on the window was not tried with a real drag either.
- **The Arch package, installed.** It was built (section 1) and not installed: the launcher entry, the icon and the dependencies it names have not been seen on a desktop that installed it.
- **The steps after the write on a real phone** (DESIGN §13): NetworkManager adding the connection (and whether polkit asks for a password, or refuses), the release's real `tools/phone-ssh`, `tools/nv-backup` and `tools/phone-provision` run by the tool, the first start's marker, the restart and `phone-smoke`.
- **This computer's key reaching a real phone's system through the answers** (DESIGN §12, "This computer's SSH key"): `SSH_KEY` in the placeholder `firstboot.conf`, set up for root and the account by the real `omamobile-firstboot` 0.1.6 at a real first start, and how long into the first start the key is first taken. Only the stand-in first start has read it (section 7, step 4).
- **The SSH key in slot B's initramfs on a real kernel.** Checked with the system's `lz4` and `cpio` once by hand; whether the phone's kernel unpacks the second stream has not been seen (section 7, step 8).
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
| The cameras' two Python modules | `tools/install-guided` fetches them from pypi.org by pinned version and SHA-256; the release bundle does not ship that list, so the tool has nothing signed to pin them by. `tools/phone-provision` uses them when they are already in `~/.cache/omamobile-venv` (DESIGN §13) |
| Removing the USB connection | It stays saved, as install-guided leaves it; the screens give `nmcli connection delete omamobile-phone` |
| The steps after the write in a dry-run window | They put files on the phone and restart it. A window started without `--write` makes no key, reads no record and runs none of the release's tools |
| The core's sentences in another language | They are composed in the core, in English (APP §8). The app's own strings have German as the working example |
| Cancelling the check of a file handed in, or a dry run | The protocol cancels a download and nothing else; `quit` ends a file check |

## 4. What stands between the app and a real install

In the order they bind:

1. **The write path's first attended run** (WRITE-RUNBOOK, with the command-line tool). Until it has run, treat `omainstall open --write` like `install --write`: untested code that erases a phone.
2. **A first install through the app.** Decided on 2026-11-13: first installs stay on the command line for now, and the app does reinstalls (a Pixel 7 Pro that runs from slot B, DESIGN §6). A first install on a Pixel 7 Pro needs the owner's statement, which the app does not take; the goal is that the installer measures the other slot itself (DESIGN §6, "Measuring the other slot: the later goal", a design with its attended test and no code yet).
3. **A release that carries the build index.** The published 2026.11.08 has none, and the flow stops for it before it looks at a phone.
4. **A phone one can use afterwards, on a real phone**: the owner's answers and this computer's SSH key reach the phone in the release image's placeholders (DESIGN §12, decided on 2026-11-13), but only from a release built with them: none is published yet, and a release without them stops a run that writes (`no-place-for-answers`).
5. **The combined run's first attended run** (section 7).
6. **`tools/phone-smoke` and `tools/camera-requirements.txt` in the release bundle** (Omarchy Mobile's `tools/make-bundle`): without them the check is `systemctl is-system-running` alone and the cameras get no data. A change on the other repository's side.

## 5. Tools this work did not have

None was missing for what was run. `wasm-bindgen` is not on `PATH` on the machine this was written on; `web/build.sh` was run with the copy in the user's cargo directory first on `PATH` (the version `Cargo.lock` names). `makepkg` was run once, with `--nodeps` and without installing (section 1). `web/selftest.sh` was not run by this work: the page was not changed by it, beyond what the two branches it was built from already held.

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

## 7. The first attended run from the write to a working phone: runbook

Status: **not run.** Written offline on 2026-11-13, with no phone. Nothing in this section has been tried on hardware.

What it tests: the whole run as an owner meets it, in one window started with `--write`: the write with the owner's answers and this computer's SSH key in the release image's placeholders (DESIGN §12), the phone's first start setting up the account and the key from them, then the steps after the write with the release's tools (DESIGN §13): the USB connection, the radio backup, the firmware, the restart and the check. It comes after section 6 and after WRITE-RUNBOOK, and its step 3 is a write (a reinstall from slot B): **every step that writes, restarts or changes this computer is the maintainer's to approve at the keyboard.** Omarchy Mobile's AGENTS.md section 1 applies throughout: a fresh radio-backup check before radio work, the modem stopped before a restart the installer does not make, and nothing sent to a phone that stopped answering.

**What it needs:** a signed release for `cheetah` built with the placeholders (Omarchy Mobile's `rootfs/placeholders`, RELEASES §6.1: `root_image.placeholders` names both files) and with `omamobile-firstboot` 0.1.6 or later in its image (it reads a placeholder and its `SSH_KEY`), with `factory-builds.json` and `tools/`. No such release is published yet; a release without placeholders stops at `start` (`no-place-for-answers`) with nothing asked of the phone, and this runbook has nothing to test with it.

Placeholders: `<RELEASE_DIR>` (that release), `<OMARCHY_MOBILE>` (the omarchy-mobile checkout), `<NV_BACKUP_DIR>` (the off-repo radio backup), `<FACTORY_ZIP>` (the factory image the release's index lists for the phone), `<USER>` (the user name typed at the answers step).

### 0. What is under test (no phone)

```sh
cargo build --release --locked -p omamobile-installer-cli
app/bin/build && app/bin/test
tools/check-shipping --built
target/release/omamobile-install verify <RELEASE_DIR> | grep -i placeholder
ls <RELEASE_DIR>/tools/          # phone-ssh, nv-backup, phone-provision and the extract-* tools
```

- **Pass:** `0 failed, 0 skipped`; every `check-shipping` line `ok`; `verify` lists both placeholders (`/etc/omamobile/firstboot.conf`, `/etc/omamobile/boot-update.conf`); the release's `tools/` holds `phone-ssh`, `nv-backup` and `phone-provision`.
- **Stop:** anything else.
- **Rollback:** none. **Risk:** none.

### 1. This computer before (read-only)

```sh
ls ~/.ssh/id_ed25519.pub ~/.local/share/omamobile-installer/ssh/ 2>&1
wc -c ~/.ssh/id_ed25519.pub 2>/dev/null
nmcli -t -f NAME,TYPE connection show | grep -c omamobile-phone
ls ~/.local/state/omamobile-installer/cheetah/ 2>&1
```

- **Expect:** whether an owner key exists (then it is used; otherwise the run makes one in `~/.local/share/omamobile-installer/ssh/`), and its size well under 3539 bytes (an Ed25519 key is about 100; a longer one is refused at `start`, plainly); whether a connection `omamobile-phone` is saved (then the run adds none; note its `ipv4.method`); no `after.json` (or the run starts at `after-resume`).
- **Pass:** recorded. **Stop:** an `after.json` nobody can explain. **Rollback:** none. **Risk:** none.

### 2. The phone's radio partitions match the backup (read-only)

With the phone running Omarchy Mobile from slot B, as AGENTS.md section 1 asks before radio work:

```sh
cd <OMARCHY_MOBILE>
tools/nv-backup --check <NV_BACKUP_DIR>
```

- **Pass:** every NV partition `SAME`.
- **Stop:** any `DIFFERENT` or unreadable partition: take no new step until it is understood.
- **Rollback:** none. **Risk:** none (`nv-backup` reads).

### 3. The write, as a reinstall (writes slot B and `userdata`: the maintainer's go)

```sh
tools/phone-ssh 'systemctl stop omamobile-modem'
bin/omamobile fastboot
OMAINSTALL_TOOL=$PWD/target/release/omamobile-install app/build/omainstall open --write --reinstall <RELEASE_DIR>
```

In the window, as section 6's step 5 to the confirmation; hand in `<FACTORY_ZIP>`; give real answers (a user name, a PIN, a phone name, a time zone, the Wi-Fi network if wanted); then type `cheetah`.

- **Expect:** the confirmation's body says "Your answers: user `<USER>` (PIN), …, and this computer's SSH public key", and "vendor_kernel_boot_b is written with this computer's SSH public key added to the initramfs it carries"; its footer says that the key goes with the answers, for root and the account, and only its public half. Then the writes, and **"Omarchy Mobile is written, and the phone is restarting"**, whose body says that the account comes from the answers at the first start, that the steps come next, and that the key went to the phone with the answers (it names the key's file); one button, "Set up the phone: its USB connection, radio backup and firmware".
- **Pass:** that screen. **Stop:** any stop of the core: WRITE-RUNBOOK's steps for it. Do not click "Set up" if the write did not end at `installed`.
- **Rollback:** WRITE-RUNBOOK step 8 (reinstall with `tools/install-guided --start-over`). **Risk:** high: it erases `userdata`, as every reinstall does.

### 4. The USB connection and the first start (may add a NetworkManager connection: the maintainer's go)

Click "Set up the phone". Watch the phone's screen meanwhile: it should grow its file system, run its first-start setup and come up locked.

- **Expect:** if the phone already answers or `omamobile-phone` is saved, no question and the wait begins; otherwise "The USB connection to the phone" with the command. "Add the connection" runs it; polkit may ask for a password; a refusal shows the command for a terminal. Then "Waiting for the phone's first start", up to 20 minutes. Early in the wait the phone answers and refuses the key (the initramfs, then the system before `omamobile-firstboot`'s SSH step): the window says nothing of it and keeps waiting. Then **"Back up the phone's radio partitions"**.
- **Pass:** the backup screen, and these, read before going on (the first reads of the running system with the key that came with the answers):

  ```sh
  cd <OMARCHY_MOBILE>
  tools/phone-ssh 'cat /var/lib/omamobile/firstboot/done; ls -l /etc/omamobile/; grep -v "^#" /etc/omamobile/boot-update.conf'
  tools/phone-ssh 'stat -c "%a %U %n" /root/.ssh /root/.ssh/authorized_keys /home/<USER>/.ssh /home/<USER>/.ssh/authorized_keys'
  tools/phone-ssh 'grep -c . /root/.ssh/authorized_keys /home/<USER>/.ssh/authorized_keys'
  tools/phone-ssh 'grep -cF "$(cut -d" " -f2 < /dev/stdin)" /var/log/omamobile/firstboot.log; journalctl -b -u omamobile-firstboot | grep -c ssh-' < ~/.ssh/id_ed25519.pub
  tools/phone-ssh 'hostnamectl --static; readlink /etc/localtime; id <USER>'
  ```

  The done marker exists; `firstboot.conf` is gone; `boot-update.conf` says `AUTO_WRITE=yes` and nothing else; `/root/.ssh` and `/home/<USER>/.ssh` are `700`, each `authorized_keys` `600`, owned by root and by `<USER>`, one line each; the key's text is in neither the first-start log nor its journal (both counts `0`; use the key file the run used); the phone's name, time zone and account are the answers'. On the phone: it is locked, and the PIN unlocks it.
- **Stop:** "still does not take this computer's SSH key" after the wait's bound: look at the phone's screen. Locked: the first start finished and the key differs (compare step 1's key file with the one the window named); dark or stuck: the first start stopped before its SSH step; record its screen, and do not restart it by hand before reading AGENTS.md's restart rules. "Another phone answers over the USB cable". The first start runs out with the key taken: the marker is missing; read `/var/log/omamobile/firstboot.log` over SSH and record its last lines (they hold no secret). Any of the checks above failing, above all a key in a log.
- **Rollback:** `nmcli connection delete omamobile-phone` if the run added it and it is not wanted.
- **Risk:** low: a saved connection on this computer, and reads of the phone over SSH.
- **Also try once here:** close the window during the wait (it asks first), open it again with the same command: **"An install on this computer is not finished"**, "Go on with this phone's setup", and the wait goes on.

### 5. The radio backup (reads the phone)

Click "Back up the radio partitions".

- **Expect:** a folder `~/omamobile-backup/cheetah-radio-backup-<date>-<time>` with `SHA256SUMS`, mode 700; then "Add the phone's firmware".
- **Pass:** `cd <OMARCHY_MOBILE> && tools/nv-backup --check <that folder>` says `SAME` for every NV partition, and the new folder's NV checksums equal `<NV_BACKUP_DIR>`'s.
- **Stop:** a failure whose last lines name a partition (not found, writable, mounted): ask, do not work around it. Any NV checksum that differs from `<NV_BACKUP_DIR>`.
- **Rollback:** none (it reads); the folder is the owner's, private, never committed. **Risk:** low.

### 6. The firmware (puts files on the phone: the maintainer's go)

Click "Add the firmware".

- **Expect:** the factory image hashed again; `tools/phone-provision --first-install --nv-backup <folder> <FACTORY_ZIP>` running for several minutes (its lines on the tool's standard error); the window holds if closed; then "Restart the phone and check it".
- **Pass:** that screen, and `tools/phone-provision`'s own "Done" in its output.
- **Stop:** a failure: read its last lines on the screen; try again once at most.
- **Rollback:** what phone-provision puts on the phone is on `userdata`; the way back is a reinstall (step 3's rollback). **Risk:** medium.

### 7. The restart and the check (restarts the phone: announce it)

Click "Restart the phone and check it". The phone's screen goes dark.

- **Expect:** the modem stopped, `reboot-to omamobile`, the phone back within 10 minutes, then **"The phone is set up"** with what works, what does not and what was not checked; the backup's folder and the report's path in the footer.
- **Pass:** that screen; `~/.local/state/omamobile-installer/cheetah/after.json` says `"check": "passed"` and `"finished"`; after the restart the phone came up locked, into the account, and the PIN unlocks it; if a Wi-Fi network was given, it is connected (`tools/phone-ssh 'nmcli -t -f NAME,DEVICE connection show --active'`, read and not recorded: it names the network).
- **Stop:** the phone not back within 10 minutes (wait one more; then the phone's keys, as AGENTS.md says); "Another phone answers"; the key refused after the restart (the first start's key did not stay).
- **Rollback:** as step 6. **Risk:** low.

### 8. After (read-only)

```sh
cd <OMARCHY_MOBILE>
tools/phone-ssh 'dmesg | grep -i initramfs'
tools/phone-ssh 'ls /etc/omamobile/firstboot.conf 2>&1; grep -cE "PASSWORD_HASH|WIFI_PSK|\$6\$" /var/log/omamobile/firstboot.log'
tools/phone-smoke
tools/nv-backup --check <NV_BACKUP_DIR>
```

- **Pass:** no "Initramfs unpacking failed" (the key's second LZ4 stream was taken); `firstboot.conf` gone and no hash or Wi-Fi password in the first-start log (count `0`); `phone-smoke` as the window said; every NV partition `SAME`.
- **Stop:** an initramfs unpacking error (record it: the key's archive needs install-phone's one-stream form); a secret in the log; any NV partition `DIFFERENT`.
- **Rollback:** none. **Risk:** none.
- **Once, if the maintainer wants the rescue path seen:** the initramfs's SSH takes the same key (it is reached only when the system does not start; not part of a normal run).

### Results

Not run yet. For each run: the date, the commit, the release's version, the kernel release (`uname -r`), each step's outcome in a line, the title of each screen where it stopped, and step 1's and step 5's notes. No serial number, no IMEI, no backup contents, no network name.
