# Contributing to the Omarchy Mobile installer

This repository holds the installer's shared core and its two front ends, a command-line tool and a web page. It is an early prototype (see [README.md](README.md), [docs/DESIGN.md](docs/DESIGN.md) and [RECOMMENDATION.md](RECOMMENDATION.md)). It belongs to [Omarchy Mobile](https://github.com/derekross/omarchy-mobile), and that project's rules apply here: read its [CONTRIBUTING.md](https://github.com/derekross/omarchy-mobile/blob/main/CONTRIBUTING.md) and the safety rules in its [AGENTS.md](https://github.com/derekross/omarchy-mobile/blob/main/AGENTS.md) first.

This code will write to people's phones. The rules below exist because a phone holds calibration data that can't be replaced.

## Safety rules

- **Only slot B's boot images and `userdata` are ever written.** The partition names come from the release's signed `manifest.json`, checked against a fixed list in the core. Don't add a way to name a partition from anywhere else, and don't add commands the core doesn't have today: no `erase`, `format`, `oem` or caller-chosen partition, and of the `flashing` family only the read-only question `flashing get_unlock_ability` (the maintainer's decision of 2026-11-12). The list of commands that may leave is closed, in `core/src/fastboot.rs`: don't turn it into a way to send a command of the caller's choosing.
- **Nothing is written before the release is verified:** the signature, then the checksums, then the manifest. Don't add a path that skips a step, and don't add support for unsigned bundles.
- **The phone is pinned by its serial number** for the whole install, and its codename is checked against the release. A phone that changes between steps or mid-install stops the install. A step added after a write must take the pinned phone (`PhoneId`) and refuse another one, on whatever link it talks to it.
- **The install flow is the core's** (`core/src/flow.rs`): its order, its refusals and its words. A front end draws the screen it is given and hands in what the person did. Don't add a check, a step or a sentence about the phone's state in a front end; add it to the flow, with a test.
- **The one warning comes before anything is asked of a phone,** and before the unlock steps. Don't add a path to a phone that goes round it.
- **A stop says what is true.** Its closing lines come from what was done to the phone and whether starting again can help (`PhoneFact`, `Retry`). Don't add a fixed reassurance, and don't hand out a way around a stop in its text.
- **Real writes are switched off in both front ends for now.** Don't turn them on in a pull request. That is the maintainer's decision, after an attended run on a real phone.
- **Tests never touch USB.** Every fastboot test uses the fake phone in `core/tests/support/`. Don't add a test that opens or lists real USB devices, and don't run the command-line tool against a phone you care about.
- **The installer never sends `fastboot flashing unlock`** and never downloads or redistributes Google's firmware. It shows the unlock command for the owner to type, asks a locked phone only whether it would take that command (to say whether "OEM unlocking" is switched on; a phone that doesn't answer is not stopped for it), and takes a factory image the owner saved from Google's own page, checked by size and SHA-256 against the release's signed build index. The match is the build with both the phone's bootloader and its baseband, marked tested, or the install stops: don't add a nearest-build fallback.
- **The installer never switches slots to look at the other one.** A phone that reports one slot's bootloader version stops the install, and nobody's word is taken for the other slot. `BOOTLOADER_SLOT_PROBE` is parsed and nothing acts on it; acting on it is the maintainer's decision, per device.
- **The pretend phone and the test key stay behind their features** (`fake-phone` in `cli`, `selftest` in `web`). A default build must have no path that trusts a key found beside a bundle.

## Before you open a pull request

Run what CI runs:

```sh
tools/check-identifiers
cargo fmt --all --check
cargo clippy
cargo test
cargo test -p omamobile-installer-cli --features fake-phone
web/build.sh selftest && web/build.sh     # needs wasm-bindgen-cli at the version in Cargo.lock
```

If you changed the flow or the page, also run `web/selftest.sh` (a headless Chromium clicks through the page against the pretend phone; CI doesn't run it).

The toolchain is pinned in `rust-toolchain.toml`, so `cargo` installs the right one.

- **Never commit identifying data:** serial numbers, MAC or Bluetooth addresses, IMEIs, network names, account names. Use placeholders such as `<SERIAL>`. `fixtures/published/` holds the project's public release key and the small files of one published release; nothing else that names a person or a phone belongs in the repository.
- **A new check must fail when what it guards is removed.** Try it: take the guard out, see the test fail, put it back, and say in the pull request which you tried.
- **Say what you ran and what you didn't.** If a change wasn't tried on a real phone, write that in the pull request. Only claim what you tested yourself.
- **Cite only properly licensed sources** in code and commit messages: Google's open-source documentation for the fastboot and sparse formats, crate documentation, and this project's own files. Don't name or compare other mobile Linux projects or their tools.
- **Commit messages** say why the change is needed. AI coding agents may add a `Co-Authored-By` line and never add `Signed-off-by`.

## Licence

MIT, as the rest of Omarchy Mobile. By contributing you agree your work is under it.
