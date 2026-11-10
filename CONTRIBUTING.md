# Contributing to the Omarchy Mobile installer

This repository holds the installer's shared core and its two front ends, a command-line tool and a web page. It is an early prototype (see [README.md](README.md) and [RECOMMENDATION.md](RECOMMENDATION.md)). It belongs to [Omarchy Mobile](https://github.com/derekross/omarchy-mobile), and that project's rules apply here: read its [CONTRIBUTING.md](https://github.com/derekross/omarchy-mobile/blob/main/CONTRIBUTING.md) and the safety rules in its [AGENTS.md](https://github.com/derekross/omarchy-mobile/blob/main/AGENTS.md) first.

This code will write to people's phones. The rules below exist because a phone holds calibration data that can't be replaced.

## Safety rules

- **Only slot B's boot images and `userdata` are ever written.** The partition names come from the release's signed `manifest.json`, checked against a fixed list in the core. Don't add a way to name a partition from anywhere else, and don't add commands the core doesn't have today: no `erase`, `format`, `oem`, `flashing` or caller-chosen partition.
- **Nothing is written before the release is verified:** the signature, then the checksums, then the manifest. Don't add a path that skips a step, and don't add support for unsigned bundles.
- **The phone is pinned by its serial number** for the whole install, and its codename is checked against the release. A phone that changes mid-install stops the install.
- **Real writes are switched off in both front ends for now.** Don't turn them on in a pull request. That is the maintainer's decision, after an attended run on a real phone.
- **Tests never touch USB.** Every fastboot test uses the fake phone in `core/tests/support/`. Don't add a test that opens or lists real USB devices, and don't run the command-line tool against a phone you care about.
- **The installer never sends `fastboot flashing unlock`** and never downloads or redistributes Google's firmware. It shows the unlock command for the owner to type, and takes a factory image the owner saved from Google's own page.

## Before you open a pull request

Run what CI runs:

```sh
tools/check-identifiers
cargo fmt --all --check
cargo clippy
cargo test
cargo check -p omamobile-installer-web --target wasm32-unknown-unknown
```

The toolchain is pinned in `rust-toolchain.toml`, so `cargo` installs the right one.

- **Never commit identifying data:** serial numbers, MAC or Bluetooth addresses, IMEIs, network names, account names. Use placeholders such as `<SERIAL>`. `fixtures/published/` holds the project's public release key and the small files of one published release; nothing else that names a person or a phone belongs in the repository.
- **Say what you ran and what you didn't.** If a change wasn't tried on a real phone, write that in the pull request. Only claim what you tested yourself.
- **Cite only properly licensed sources** in code and commit messages: Google's open-source documentation for the fastboot and sparse formats, crate documentation, and this project's own files. Don't name or compare other mobile Linux projects or their tools.
- **Commit messages** say why the change is needed. AI coding agents may add a `Co-Authored-By` line and never add `Signed-off-by`.

## Licence

MIT, as the rest of Omarchy Mobile. By contributing you agree your work is under it.
