# Installer core spike

A working spike for decision D5 of Omarchy Mobile's install plan: one installer core for a web flasher and a desktop app. Read [RECOMMENDATION.md](RECOMMENDATION.md) for the answer and the evidence. This is not a product, and it is not part of the omarchy-mobile repository.

**Nothing here was run against a real phone.** Every fastboot test uses a fake device in the test code.

## Layout

| Path | What |
| --- | --- |
| `core/` | The shared core: `manifest.json` and `device.conf` rules, the release signature and checksums, fastboot over a transport trait, Android sparse cutting, zstd. No I/O of its own |
| `core/tests/` | 25 tests. `support/fake.rs` is the fake phone; `support/bundle.rs` signs small test bundles with a key made on the spot |
| `usb/` | The fastboot transport over USB (nusb): one file for Linux, Windows, macOS and WebUSB |
| `cli/` | `omamobile-install`: `verify` and `sparse` need no phone; `devices` and `install --dry-run` use USB |
| `web/` | The same core as WebAssembly, a static page, and a local server that sends the strict headers |
| `fixtures/published/` | The small signed files of the published cheetah 2026.11.08 release, and the first 32 bytes of its root image. `omamobile.gpg` is the project's public key as published; its user ID holds the maintainer's address |
| `fixtures/selftest/`, `fixtures/zstd/` | A signed test bundle for the browser self-test, and a file made by the `zstd` tool |
| `platform/linux/` | A fallback udev rule |

## Build and test

```sh
cargo test                      # core, against the fake phone; no USB
cargo build --release           # target/release/omamobile-install
cargo check -p omamobile-installer-cli --target x86_64-pc-windows-msvc   # also -gnu, and the two apple-darwin targets

web/build.sh                    # web/dist (needs wasm-bindgen-cli 0.2.130 and the wasm32 target)
web/build.sh selftest           # with the fake phone built in
web/serve.py                    # http://127.0.0.1:8765/ and http://127.0.0.1:8765/?selftest
```

Commands that touch no USB device:

```sh
omamobile-install verify BUNDLE_DIR             # a downloaded bundle: signature, rules, every file
omamobile-install sparse ROOT.img.zst OUT_DIR   # the sparse images a flash would send; check with simg2img
```

## Safety

- `omamobile-install devices` and `install` open USB devices. `install` runs only with `--dry-run`, which sends `getvar` and nothing else; the page's "Check my phone" is the same. Ask the phone's owner before either.
- The core can send `getvar:`, `download:`, `flash:` to slot B's boot images and `userdata`, `set_active:b` and `reboot`. It has no code for anything else. Those writes are reachable only from the tests and from a `web` build with `enable-writes`, which nothing here turns on.
- No serial number is stored or printed by the core. Tests use placeholders.
