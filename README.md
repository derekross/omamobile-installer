# Omarchy Mobile installer

The installer for [Omarchy Mobile](https://github.com/derekross/omarchy-mobile): one core, written in Rust, behind a command-line tool and a web page. It is an early prototype.

**It cannot install anything yet, and nothing here was ever run against a real phone.** The install flow ends at a dry run, real writes are switched off in both front ends, and every test uses a pretend phone in the test code.

- [docs/DESIGN.md](docs/DESIGN.md): the install flow, its states and stops, the build index, the bootloader-slot rule, what a front end must do, and what is open.
- [RECOMMENDATION.md](RECOMMENDATION.md): why Rust and a web UI (the spike this grew from), with measurements.
- [CONTRIBUTING.md](CONTRIBUTING.md): the safety rules.

## What works, and what does not

Works, against the pretend phone and the fixtures:

- **Verifying a release:** the release key's signature, the checksums, `manifest.json` and `device.conf` against the installer's rules. The published Pixel 7 Pro release 2026.11.08 verifies, natively and in a browser.
- **The install flow up to the dry run,** the same in both front ends: the one warning; reading the phone's state; the unlock steps as text for the owner, with what the phone says about "OEM unlocking", under a notice that this version cannot install and gives no reason to unlock; looking the phone up in the release's signed build index; Google's terms for its factory image, shown for the owner to accept (by the command-line tool; the web page cannot download from Google, says so, and asks for no terms); the factory zip, downloaded from Google's server by the installer or saved by the owner and handed in, checked by size and SHA-256 either way; the owner's answers; and a dry run that lists what an install would write.
- **Every stop,** with words that are true for the phone at that point: another model, slot B, the bootloader slots, a build that isn't listed or tested, another phone than the first, a release or an index that fails its checks, a download that is not the file the release lists.
- **The write path, on the pretend phone only:** slot B's boot images and `userdata`, the root image decompressed and cut into sparse images.

Does not work, or is not built:

- **Installing.** No front end can write to a phone. The owner's answers can't be delivered to the phone yet, and nothing after the write exists (the first start, the radio backup, firmware, the restart, the check).
- **A download from Google's real server, by this code.** Since the maintainer's decision of 2026-11-12 the installer downloads Google's factory image for the owner, after showing Google's terms (DESIGN §2, decision 11). Every test of that uses a stand-in, and no code here has sent a request to `dl.google.com`. How the server answers was measured by hand on 2026-11-12 (DESIGN §7): `200` with no redirect, a `Content-Length` equal to the build index's size, `Range` honoured with `206`, and no `Access-Control-Allow-Origin` header.
- **A download in the web page.** Without that header a browser lets no page read the file. So the page tells the core that it cannot fetch from Google, and the flow there asks for no terms and goes straight to "save the file from Google's page and pick it here", saying why. The command-line tool downloads.
- **The published release in the flow.** 2026.11.08 carries no build index, so the flow stops and says so. It needs a release that ships `factory-builds.json`.
- **A Pixel 7 Pro, even then.** Its bootloader reports one slot's version only, and the flow stops there by decision (DESIGN §6). The maintainer has a hidden override that takes a dry run, and nothing else, past that stop; such a run says on every screen that the check was skipped and that it is not evidence that an install would be allowed.
- **Android to Fastboot Mode, and the unlock,** are the owner's: the installer shows the keys and the command.
- **The web page's downloads of the release** (the images and the root image), **the desktop window, Windows and macOS.**

## Layout

| Path | What |
| --- | --- |
| `core/` | The shared core: the install flow (`flow.rs`), the build index, the download of Google's factory image as decisions on what a front end fetched (`download.rs`), `manifest.json` and `device.conf` rules, the release signature and checksums, fastboot over a transport trait, Android sparse cutting, zstd. No I/O of its own |
| `core/tests/` | 68 tests, and 3 beside the command layer in `core/src/fastboot.rs`. `support/fake.rs` is the pretend phone; `support/bundle.rs` signs small test bundles with a key made on the spot |
| `usb/` | The fastboot transport over USB (nusb): one file for Linux, Windows, macOS and WebUSB |
| `cli/` | `omamobile-install`. `verify` and `sparse` need no phone; `flow --fake-phone` runs the install flow against the pretend phone; `devices` and `install --dry-run` use USB. It downloads Google's factory image with `ureq`, into `--work-dir`, through no proxy unless `--proxy` names one |
| `web/` | The same core as WebAssembly, a static page with a screen for each state of the flow, a local server that sends the strict headers (and, for the self-test, runs a stand-in for Google's server), and a self-test that clicks through the page in a headless browser |
| `fixtures/published/` | The small signed files of the published cheetah 2026.11.08 release, and the first 32 bytes of its root image. `omamobile.gpg` is the project's public key as published; its user ID holds the maintainer's address |
| `fixtures/index/` | `device/cheetah/factory-builds.json` as Omarchy Mobile carries it: public metadata about two of Google's builds |
| `fixtures/selftest/`, `fixtures/zstd/` | A signed test bundle (test key, build index, a stand-in factory zip of noise) for the front ends' pretend phone, and a file made by the `zstd` tool |
| `platform/linux/` | A fallback udev rule |

## Build and test

The toolchain is pinned in `rust-toolchain.toml`.

```sh
cargo test                      # the core, against the pretend phone; no USB
cargo test -p omamobile-installer-cli --features fake-phone   # the built tool driving the flow, and downloading from a stand-in
cargo build --release           # target/release/omamobile-install
cargo check -p omamobile-installer-cli --target x86_64-pc-windows-msvc   # also -gnu, and the two apple-darwin targets

cargo install --locked wasm-bindgen-cli --version 0.2.130   # once; the version in Cargo.lock
web/build.sh                    # web/dist, the page as it would ship
web/build.sh selftest           # with the pretend phone built in
web/serve.py                    # http://127.0.0.1:8765/
web/serve.py --stand-in         # for a selftest build: ?pretend and ?selftest, with a stand-in for Google's server
web/selftest.sh                 # builds, serves, and runs ?selftest in a headless Chromium
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
```

## Safety

- `omamobile-install devices` and `install` open USB devices, and so does the page's "Choose the phone". `install` runs only with `--dry-run`. Both go through the install flow, which sends `getvar` and, to a locked phone it is about to show the unlock steps for, the one question `flashing get_unlock_ability`. Both read, and nothing else is sent. Ask the phone's owner before either.
- The core can send `getvar:`, `download:`, `flash:` to slot B's boot images and `userdata`, `set_active:b`, `reboot` and the read-only question `flashing get_unlock_ability`. That list is closed and checked where a command is sent: no other `flashing` command (`flashing unlock` among them), no `oem`, no `erase`, no other slot. The check does not rest on the code above it: a `flash:` leaves only for a partition that is on the session's own list and on the gate's fixed one (so not `persist`, `efs`, `devinfo`, `modem`, `vbmeta` or anything on slot A), and a session that is a dry run, as every session of a shipping front end is, can send no `flash:`, `download:`, `set_active:` or `reboot` at all. The default session options are a dry run. The writes are reachable only from the tests and from a `web` build with `enable-writes`, which nothing here turns on.
- The unlock command is shown for the owner to type. The installer never sends it and never switches slots.
- **Google's factory image** (the maintainer's decision of 2026-11-12, which reverses the one of 2026-11-10): the installer downloads it for the owner, from Google's own server, after showing Google's terms for the owner to accept, and checks it on this computer before it uses anything of it. The address comes only from the release's signed build index, on `https://dl.google.com/` and nowhere else, redirects included. **This project hosts none of Google's files** and passes none on. An owner can still save the file from Google's page and hand it in (`--factory-zip FILE`, or the page's file picker). Whether the project may ship files extracted from a factory image itself is a separate question, and it is not decided.
- **The download uses no proxy unless the owner names one** on the command line: `--proxy http://HOST:PORT`. A proxy named in the environment (`http_proxy`, `https_proxy`, `all_proxy`, in lower or upper case) is not used, whatever set it. Through a named proxy the connection is still TLS to `dl.google.com`, checked against the roots built into the tool, so the proxy can't change the file; what it learns is that this computer fetches a Pixel factory image, which is why using one is the owner's choice and not the environment's. The proxy's address is not printed back. An owner who needs a proxy and would rather not name it can save the file from Google's page with their browser and hand it in.
- `omamobile-install install --dry-run` on a real phone therefore also reaches the internet, once the owner accepts Google's terms: one file, from Google. `flow --fake-phone` and every test reach a stand-in on this computer, or nothing.
- No serial number is stored or printed by the core: it compares a digest. Tests use placeholders.
