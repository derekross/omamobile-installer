# Omarchy Mobile installer

The installer for [Omarchy Mobile](https://github.com/derekross/omarchy-mobile): one core, written in Rust, behind a command-line tool, a web page and a desktop window. It is an early prototype.

**Nothing here was ever run against a real phone, and it cannot yet install a phone you can use.** The write path is built, and tested against a pretend phone only: it has never sent a command to a real one, and its first real use is the maintainer's own attended run ([docs/WRITE-RUNBOOK.md](docs/WRITE-RUNBOOK.md)). The command-line tool can write, with `--write` and the phone's codename typed; the web page cannot; the desktop window writes only through the tool, when it was started with `--write` itself. The owner's answers don't reach the phone yet, so what it installs starts with no user account. Every test uses a pretend phone in the test code.

- [docs/DESIGN.md](docs/DESIGN.md): the install flow, its states and stops, the build index, the bootloader-slot rule, what a front end must do, the write path and what happens when it is interrupted, and what is open.
- [docs/WRITE-RUNBOOK.md](docs/WRITE-RUNBOOK.md): the write path's first attended run on a real phone, step by step. Not run yet.
- [docs/APP.md](docs/APP.md): the desktop window (`app/`), and the protocol it speaks with the command-line tool, `omamobile-install json`, which is also the interface for agents and scripts. [app/docs/STATUS.md](app/docs/STATUS.md): what of the window works against the pretend phone, what has never run, and its first attended run.
- [docs/DESIGN.md](docs/DESIGN.md) §13: the install link after the write, on the installer's side: the phone's first start, the code, the owner's answers, the radio backup, the owner's factory image sent to the phone, the firmware, the restart, ready. Built against a pretend phone that speaks the link; never run on a phone.
- [docs/LINK.md](docs/LINK.md): the install link's protocol crate (`link/`): what the phone and the installer will say to each other over USB at the phone's first start, as the install link plan has it since its correction of 2026-11-12. Built and tested in memory; the installer drives its host machine after the write (DESIGN §13), and protocol 1 is not fixed before the crate's first tag.
- [RECOMMENDATION.md](RECOMMENDATION.md): why Rust and a web UI (the spike this grew from), with measurements.
- [CONTRIBUTING.md](CONTRIBUTING.md): the safety rules.

## What works, and what does not

Works, against the pretend phone and the fixtures:

- **Verifying a release:** the release key's signature, the checksums, `manifest.json` and `device.conf` against the installer's rules. The published Pixel 7 Pro release 2026.11.08 verifies, natively and in a browser.
- **The install flow up to the dry run,** the same in both front ends: the one warning; reading the phone's state; the unlock steps as text for the owner, with what the phone says about "OEM unlocking", under a notice that says what unlocking is for in this run (a dry run installs nothing and gives no reason to unlock; a run with `--write` installs a phone nobody can sign in to yet); looking the phone up in the release's signed build index; Google's terms for its factory image, shown for the owner to accept (by the command-line tool; the web page cannot download from Google, says so, and asks for no terms); the factory zip, downloaded from Google's server by the installer or saved by the owner and handed in, checked by size and SHA-256 either way; the owner's answers; and a dry run that lists what an install would write.
- **Every stop,** with words that are true for the phone at that point: another model, slot B, the bootloader slots, a build that isn't listed or tested, another phone than the first, a release or an index that fails its checks, a download that is not the file the release lists.
- **The write path, on the pretend phone only** (DESIGN §11). In the command-line tool, `install --write`: the same dry run first, then a list of each partition that will be written with its sizes and of what is erased, then the phone's codename, typed, then slot B's boot images and `userdata` (the root image decompressed and cut into sparse images as a stream), `set_active:b` and `reboot`. Each condition of a write has a test that takes it away and reads the pretend phone's record: a release or build index that did not verify, a phone that does not pass every stop, a locked phone, another phone, no typed codename, a dry run, the maintainer's override. An install interrupted before or after any command can be run again, and the stop says how far it got.

Does not work, or is not built:

- **An install on a real phone.** The write path has never run against one. Until the maintainer's attended run, treat `install --write` as untested code that erases a phone.
- **A phone you can use afterwards.** The owner's answers are not put on the phone: that needs writing into the root image's file system, which the core can't do yet (DESIGN §12 has the options). The phone starts Omarchy Mobile with no user account, and the installer says so before the confirmation. Nothing after the write exists either (the radio backup, firmware, the check).
- **A write to any phone but the Pixel 7 Pro.** A release for another phone stops a run that writes.
- **A download from Google's real server, by this code.** Since the maintainer's decision of 2026-11-12 the installer downloads Google's factory image for the owner, after showing Google's terms (DESIGN §2, decision 11). Every test of that uses a stand-in, and no code here has sent a request to `dl.google.com`. How the server answers was measured by hand on 2026-11-12 (DESIGN §7): `200` with no redirect, a `Content-Length` equal to the build index's size, `Range` honoured with `206`, and no `Access-Control-Allow-Origin` header.
- **A download in the web page.** Without that header a browser lets no page read the file. So the page tells the core that it cannot fetch from Google, and the flow there asks for no terms and goes straight to "save the file from Google's page and pick it here", saying why. The command-line tool downloads.
- **The published release in the flow.** 2026.11.08 carries no build index, so the flow stops and says so. It needs a release that ships `factory-builds.json`.
- **A first install on a Pixel 7 Pro without its owner's word for one bootloader slot.** Its bootloader reports one slot's version only, and on a phone that runs from slot A the flow stops there (DESIGN §6). A reinstall, on a phone that already runs from slot B, goes on with the version slot B reports, which is the bootloader the install keeps it on (decided on 2026-11-13; in the desktop window the core asks what slot B holds). On the command line the owner can state the other slot's version, as with Omarchy Mobile's `tools/install-phone`: `--bootloader-slots-match VERSION`, which must be the version the phone reports. The installer cannot check that statement and never switches slots to try; a run that rests on it says so on every screen. The web page has no such option. The maintainer also has a hidden override that takes a dry run, and nothing else, past that stop; such a run says on every screen that the check was skipped and that it is not evidence that an install would be allowed, and with `--write` it is refused.
- **Android to Fastboot Mode, and the unlock,** are the owner's: the installer shows the keys and the command.
- **An install through the desktop window on a real phone.** The window (`app/`, `omainstall`) goes through the whole flow against the pretend phone, the write path among it, and has never met a real one. On a Pixel 7 Pro it reinstalls, and stops a first install at the bootloader-slot stop: the owner's statement above is the command line's alone (app/docs/STATUS.md).
- **A release found or fetched by the tool.** `json` and `install` take a release's directory; `omainstall` without one has nothing to start from.
- **The web page's downloads of the release** (the images and the root image), **Windows and macOS.**

## Layout

| Path | What |
| --- | --- |
| `core/` | The shared core: the install flow with its write path (`flow.rs`), the build index, the download of Google's factory image as decisions on what a front end fetched (`download.rs`), the filter for what a phone or a server says before it is printed (`text.rs`), `manifest.json` and `device.conf` rules, the release signature and checksums, fastboot over a transport trait, Android sparse cutting, zstd, and the owner's answers as first-start files that nothing writes yet (`firstboot.rs`). No I/O of its own |
| `core/tests/` | 94 tests, 19 of them the write path's (`write_path.rs`), and 16 in the core's sources: 9 beside the command layer in `core/src/fastboot.rs`, 4 for `core/src/text.rs`, 3 in `firstboot.rs`. `support/fake.rs` is the pretend phone, which records every command and can have its cable pulled; `support/bundle.rs` signs small test bundles with a key made on the spot |
| `usb/` | The fastboot transport over USB (nusb): one file for Linux, Windows, macOS and WebUSB |
| `link/` | `omamobile-link`, the install link's protocol with no I/O and no dependency on the core: frames, every message, first boot's rules for the owner's answers, the phone's and the computer's state machines, and the rules by which the phone's daemon squares its state file with what it finds at its start. The phone's daemon in omarchy-mobile builds on the same crate (it is checked for `aarch64-unknown-linux-gnu` here, not run). `link/tests/` drives both machines through a loopback in memory, kills the stand-in daemon before every output of a whole install, and replaces the installer at every frame; `link/golden/` holds one frame of every kind as bytes and as hex text, the contract between the two repositories ([docs/LINK.md](docs/LINK.md)) |
| `cli/` | `omamobile-install`. `verify`, `sparse` and `help` need no phone; `flow --fake-phone` runs the install flow against the pretend phone (with `--write`, its write path too); `devices`, `install --dry-run`, `install --write` and `json` use USB; `install --write` writes, and so does `json --write`. `json` (`src/json.rs`) is the flow as JSON lines on standard input and output, for the desktop window, agents and scripts. It downloads Google's factory image with `ureq`, into `--work-dir`, through no proxy unless `--proxy` names one |
| `app/` | `omainstall`, the desktop window: C++ and Qt Quick, in the style of Omarchy's own apps. It starts `omamobile-install json` and draws the screens it is sent ([app/README.md](app/README.md), [docs/APP.md](docs/APP.md)) |
| `web/` | The same core as WebAssembly, a static page with a screen for each state of the flow, a local server that sends the strict headers (and, for the self-test, runs a stand-in for Google's server), and a self-test that clicks through the page in a headless browser |
| `fixtures/published/` | The small signed files of the published cheetah 2026.11.08 release, and the first 32 bytes of its root image. `omamobile.gpg` is the project's public key as published; its user ID holds the maintainer's address |
| `fixtures/index/` | `device/cheetah/factory-builds.json` as Omarchy Mobile carries it: public metadata about two of Google's builds |
| `fixtures/selftest/`, `fixtures/zstd/` | A signed test bundle (test key, build index, a stand-in factory zip of noise) for the front ends' pretend phone, and a file made by the `zstd` tool |
| `platform/linux/` | A fallback udev rule |
| `tools/` | `check-identifiers` (no identifier of a phone or a person in any tracked file) and `check-shipping` (what ships has one way to write, the command-line tool started with `--write`, as `install` or as `json`; the page has none, the window only passes its own `--write` on, and none of them holds the pretend phone). CI runs both |

## Build and test

The toolchain is pinned in `rust-toolchain.toml`.

```sh
cargo test                      # the core, against the pretend phone; no USB
cargo test -p omamobile-installer-cli --features fake-phone   # the built tool driving the flow, as text and as JSON lines, and downloading from a stand-in
cargo build --release           # target/release/omamobile-install
cargo check -p omamobile-installer-cli --target x86_64-pc-windows-msvc   # also -gnu, and the two apple-darwin targets

cargo install --locked wasm-bindgen-cli --version 0.2.130   # once; the version in Cargo.lock
web/build.sh                    # web/dist, the page as it would ship
web/build.sh selftest           # with the pretend phone built in
web/serve.py                    # http://127.0.0.1:8765/
web/serve.py --stand-in         # for a selftest build: ?pretend and ?selftest, with a stand-in for Google's server
web/selftest.sh                 # builds, serves, and runs ?selftest in a headless Chromium held to 127.0.0.1
tools/check-shipping            # one way to write (the tool's --write), none in the page, no pretend phone (--built: also the built tool, page and app)

app/bin/build                   # app/build/omainstall (needs Qt 6: qt6-base, qt6-declarative)
app/bin/test                    # the window's tests, offscreen, against a stand-in and against the real tool with its pretend phone
app/bin/screens                 # the same walk as pictures, into app/docs/screens/
```

Commands that touch no USB device:

```sh
omamobile-install verify BUNDLE_DIR             # a downloaded bundle: signature, rules, every file, the build index
omamobile-install sparse ROOT.img.zst OUT_DIR   # the sparse images a flash would send; check with simg2img

# The whole install flow against a pretend phone (a build with --features fake-phone):
cargo run --features fake-phone -p omamobile-installer-cli -- flow fixtures/selftest --fake-phone ready \
  --factory-zip fixtures/selftest/factory/cheetah-test.260101.001-factory-31c2322a.zip
# Without --factory-zip the flow shows Google's terms; accepted, it downloads from the stand-in named with
# --fake-google 127.0.0.1:PORT (a pretend run never reaches Google), into --work-dir DIR
# Other pretend phones: locked, locked-oem-off, other-model, slot-b, one-slot, slots-differ, unknown-build, untested-build, untested-old-build

# The write path against the pretend phone: add --write. It lists what it will write, asks for the
# codename (cheetah), and ends by printing every command the pretend phone received
cargo run --features fake-phone -p omamobile-installer-cli -- flow fixtures/selftest --fake-phone ready --write \
  --factory-zip fixtures/selftest/factory/cheetah-test.260101.001-factory-31c2322a.zip

omamobile-install help                          # the commands, and exactly what --write writes and erases
```

## For agents and scripts

`omamobile-install json [--release DIR] [--work-dir DIR] [--proxy http://HOST:PORT] [--write]` is the installer's machine interface, and the supported one: JSON lines on standard input and output, one object per line, specified in [docs/APP.md](docs/APP.md) §3 (protocol 3). Standard output carries the protocol and nothing else. The tool says `hello`; after `hello` and `start` from the other side, every message that is sent is answered by exactly one `screen` (the core's screen: what a person would read, the action it asks for, and on a stop whether starting again can help), with `progress`, `problems`, `phones` or `error` before it where there is something to say. The desktop window, `omainstall`, is built on it.

Without `--write` it ends at the dry run, and nothing that is sent to it can change that: whether the flow may write is the tool's own argument, never a message. With `--write` it goes on as `install --write` does, to the list of what will be written, the codename (`confirm`) and the writes (`write`), and says each command it sent. It has no argument and no message for a serial number, for the owner's statement about a bootloader slot, or for the maintainer's override.

An install driven to the dry run, against the pretend phone of a `fake-phone` build (no USB device is opened). The lines are taken in order, so a script that knows the way can write them all at once:

```sh
cargo build -p omamobile-installer-cli --features fake-phone
zip=$PWD/fixtures/selftest/factory/cheetah-test.260101.001-factory-31c2322a.zip
printf '%s\n' '{"type":"hello","protocol":3,"app":"a script","version":"0"}' \
  '{"type":"start","reinstall":false}' '{"type":"acknowledge"}' '{"type":"read-phone"}' \
  '{"type":"decline-terms"}' "{\"type\":\"take-file\",\"path\":\"$zip\"}" \
  '{"type":"answers","username":"alex","password":"2580","password_again":"2580","hostname":"","timezone":"Europe/Paris","wifi_ssid":"","wifi_psk":""}' \
  '{"type":"dry-run"}' |
  target/debug/omamobile-install json --release fixtures/selftest --fake-phone ready |
  jq -c 'select(.type == "screen").screen | {state, stop, retry, action, title}'
```

It prints one line for each screen, from `warning` to `done`; the last screen's `plan` lists what an install would write. A driver that decides as it goes reads each screen and answers the action it names: docs/APP.md §3.7 has one in Python. Other pretend phones show the stops (`--fake-phone slot-b`), a locked phone (`locked`), two phones (`ready,slot-b`) or none (`none`). With `--write` added to the tool's arguments, and `'{"type":"confirm","typed":"cheetah"}' '{"type":"write"}'` to the lines, the same run goes through the pretend phone's writes to `installed`.

On a real phone, the warning, Google's terms and the codename are the phone's owner's to answer, not a program's.

## Safety

- **`omamobile-install install DIR --serial S --write` erases a phone.** It writes slot B's boot partitions and `userdata`, makes slot B the active slot and restarts the phone. `userdata` is erased: everything Android stored is lost, and Android on slot A needs a factory reset if it is ever started again. It has never been run against a real phone. Don't run it on a phone you care about, and never without its owner at the keyboard: the run shows what it will write and waits for the phone's codename to be typed.
- **`omamobile-install json --write`, and `omainstall open --write`, which starts it, reach the same writes** once the owner has typed the codename in that run. Neither has run against a real phone. Without `--write`, `json` and the window end at the dry run, whatever is sent or clicked.
- `install` has no default: it needs `--dry-run` or `--write`. `omamobile-install devices`, `install --dry-run`, `json` without `--write` (from its first `read-phone` on, never before `start`) and the page's "Choose the phone" open USB devices and only read: they go through the install flow, which sends `getvar` and, to a locked phone it is about to show the unlock steps for, the one question `flashing get_unlock_ability`. Ask the phone's owner before any of them.
- Nothing is written unless all of this holds, checked together where the flow opens the one session that can write (`Flow::begin_write`): the release and its build index verified; this phone past every stop in this run, and again when asked at the write; unlocked; the same phone, by its serial number, before every write; the codename typed in this run; the run begun with `--write`; the maintainer's bootloader-slot override not used; and, for a phone that reports one bootloader slot's version, the owner's statement of the other's, equal to it. Then each image is checked against its partition's size on the phone, `userdata` included, before the first write. Every file of the release is checked against the signed manifest before the first write too, and the root image is hashed again as it is sent: if what was sent is not what was checked, slot B is not made active. One `flash:` of `userdata` covers at most 256 MiB, so no single answer the installer waits for stands for more work than that.
- The core can send `getvar:`, `download:`, `flash:` to slot B's boot images and `userdata`, `set_active:b`, `reboot` and the read-only question `flashing get_unlock_ability`. That list is closed and checked where a command is sent: no other `flashing` command (`flashing unlock` among them), no `oem`, no `erase`, no other slot. The check does not rest on the code above it: a `flash:` leaves only for a partition that is on the session's own list (the verified release's: the descriptor's `BOOT_IMAGES` with `_b`, and `userdata`) and on the gate's fixed one (so not `persist`, `efs`, `devinfo`, `modem`, `vbmeta` or anything on slot A), and a session that is a dry run, as every session is but the one `Flow::begin_write` opens, can send no `flash:`, `download:`, `set_active:` or `reboot` at all. Every command of the write path leaves through that one gate: there is no second list.
- **One function can open a session that writes, and the compiler holds that.** A session writes only if it holds a write token, a type only `Flow::begin_write` can make (its field is private to the flow's module) and that it makes after the checks above. Every other session, whoever opens it, is a dry run. Making a token anywhere else does not compile, which the core's doctests show (DESIGN §11).
- **The web page cannot write,** in any build. It begins every flow as a dry run and calls none of the flow's write steps, and `tools/check-shipping` checks that in its source and in the shipping build. Its older direct session, and the `enable-writes` feature that could have made it write, are removed.
- The unlock command is shown for the owner to type. The installer never sends it. The only slot command it has is the install's own `set_active:b`, after the last write: it never switches slots to look at one, and never makes slot A active.
- **Google's factory image** (the maintainer's decision of 2026-11-12, which reverses the one of 2026-11-10): the installer downloads it for the owner, from Google's own server, after showing Google's terms for the owner to accept, and checks it on this computer before it uses anything of it. The address comes only from the release's signed build index, on `https://dl.google.com/` and nowhere else, redirects included. **This project hosts none of Google's files** and passes none on. An owner can still save the file from Google's page and hand it in (`--factory-zip FILE`, or the page's file picker). Whether the project may ship files extracted from a factory image itself is a separate question, and it is not decided.
- **The download uses no proxy unless the owner names one** on the command line: `--proxy http://HOST:PORT`. A proxy named in the environment (`http_proxy`, `https_proxy`, `all_proxy`, in lower or upper case) is not used, whatever set it. Through a named proxy the connection is still TLS to `dl.google.com`, checked against the roots built into the tool, so the proxy can't change the file; what it learns is that this computer fetches a Pixel factory image, which is why using one is the owner's choice and not the environment's. The proxy's address is not printed back. An owner who needs a proxy and would rather not name it can save the file from Google's page with their browser and hand it in.
- `omamobile-install install` and `json` on a real phone therefore also reach the internet, once the owner accepts Google's terms: one file, from Google. `flow --fake-phone`, `json --fake-phone` and every test reach a stand-in on this computer, or nothing.
- No serial number is stored or printed by the core: it compares a digest. Tests use placeholders.
