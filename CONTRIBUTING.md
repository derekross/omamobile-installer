# Contributing to the Omarchy Mobile installer

This repository holds the installer's shared core and its two front ends, a command-line tool and a web page. It is an early prototype (see [README.md](README.md), [docs/DESIGN.md](docs/DESIGN.md) and [RECOMMENDATION.md](RECOMMENDATION.md)). It belongs to [Omarchy Mobile](https://github.com/derekross/omarchy-mobile), and that project's rules apply here: read its [CONTRIBUTING.md](https://github.com/derekross/omarchy-mobile/blob/main/CONTRIBUTING.md) and the safety rules in its [AGENTS.md](https://github.com/derekross/omarchy-mobile/blob/main/AGENTS.md) first.

This code will write to people's phones. The rules below exist because a phone holds calibration data that can't be replaced.

## Safety rules

- **Only slot B's boot images and `userdata` are ever written.** The partition names come from the release's signed `manifest.json`, checked against a fixed list in the core, and a session may flash only those: its list is built from the verified release and compared where a command is sent. Don't add a way to name a partition from anywhere else (a person, a front end, a phone's answer), and don't add commands the core doesn't have today: no `erase`, `format`, `oem` or caller-chosen partition, no `set_active:a`, nothing that marks a slot, and of the `flashing` family only the read-only question `flashing get_unlock_ability` (the maintainer's decision of 2026-11-12). The list of commands that may leave is closed, in `core/src/fastboot.rs`: don't turn it into a way to send a command of the caller's choosing.
- **Nothing is written before the release is verified:** the signature, then the checksums, then the manifest. Don't add a path that skips a step, and don't add support for unsigned bundles.
- **The phone is pinned by its serial number** for the whole install, and its codename is checked against the release. A phone that changes between steps or mid-install stops the install. A step added after a write must take the pinned phone (`PhoneId`) and refuse another one, on whatever link it talks to it.
- **The install flow is the core's** (`core/src/flow.rs`): its order, its refusals and its words. A front end draws the screen it is given and hands in what the person did. Don't add a check, a step or a sentence about the phone's state in a front end; add it to the flow, with a test.
- **The one warning comes before anything is asked of a phone,** and before the unlock steps. Don't add a path to a phone that goes round it.
- **A stop says what is true.** Its closing lines come from what was done to the phone and whether starting again can help (`PhoneFact`, `Retry`). Don't add a fixed reassurance, and don't hand out a way around a stop in its text.
- **The write path is the flow's, and the command-line tool's alone** (DESIGN §11). It has never run against a real phone; its first real use is the maintainer's own attended run.
  - `Flow::begin_write` is the one place a front end gets a session that can write. Don't open one any other way in a front end, and don't take a condition out of it: the release and its build index verified, the phone past every stop in this run and judged again there, unlocked, the pinned phone, the codename typed in this run, the run begun to write, the maintainer's override not used. Each has a test in `core/tests/write_path.rs` that takes it away and reads the pretend phone's record; a new condition gets one too.
  - `--write` stays explicit and never the default, and nothing implies it. The confirmation stays the phone's codename, typed, after the list of what will be written.
  - A stop after the writes began says how far they got (`WriteReach`), from what the phone confirmed. Don't word one by hand, and don't write "nothing was written" where a write may have reached the phone.
  - The order is `tools/install-phone`'s: slot B's boot images, `userdata`, `set_active:b`, `reboot`. Don't add a step between or after them without the maintainer.
  - Adding a phone to `WRITE_DEVICES` is the maintainer's decision, with that phone's notes on what `set_active` costs and what a failed start falls back to.
  - **The web page gets no write path.** Don't set `FlowOptions::write` there, don't call the flow's write steps from it, and don't build it with `enable-writes`. CI checks all three.
  - The images are written as the release signed them. Don't add a step that changes one (to put the owner's answers in, say) without the design for it: DESIGN §12 lists the options, and choosing one is the maintainer's.
- **Tests never touch USB.** Every fastboot test uses the fake phone in `core/tests/support/`. Don't add a test that opens or lists real USB devices, and don't run the command-line tool against a phone you care about. `install` with `--serial` and `--dry-run` or `--write` looks for a USB device, so no test runs it that far: the tool's tests use `flow --fake-phone`, and only the refusals `install` makes from its arguments alone.
- **The installer never sends `fastboot flashing unlock`.** It shows the unlock command for the owner to type, and asks a locked phone only whether it would take that command (to say whether "OEM unlocking" is switched on; a phone that doesn't answer is not stopped for it).
- **Google's factory image comes from Google, to the owner's computer, and from nowhere else** (the maintainer's decision of 2026-11-12, which reverses the one of 2026-11-10 that the installer never downloads it). The installer downloads it for the owner after showing Google's terms for the owner to accept; an owner can still save it from Google's page and hand it in. This project hosts none of Google's files and never redistributes them: don't add a mirror, a cache on a server, a bundle that carries them, or a test fixture made from them. Whether the project may ship files extracted from a factory image itself is a separate question that is not decided; don't build towards it in a pull request. The rules of the download are the core's (`core/src/download.rs`), each with a test:
  - Nothing is downloaded before the owner accepted Google's terms on the flow's step for it. Don't add a path round that step, and don't reword what it quotes: those are Google's sentences, marked as Google's.
  - The address comes only from the build index of a verified release. Only `https` and only the host `dl.google.com`, checked on the index entry and on every redirect. Don't take an address from the person, a front end, an environment variable or a server, and don't add a host.
  - The size is the index's before and during the transfer, and the SHA-256 is checked over the whole file, from its first byte, before anything of it is used, also when a download went on from bytes kept earlier. A file that isn't the listed one stops the install and is deleted.
  - The acceptance is kept in the flow for that run. It is never sent anywhere, and neither is anything about the download.
  - Tests never fetch from Google. They use a stand-in: a function in the core's tests, a socket on 127.0.0.1 in the tool's, `web/serve.py --stand-in` for the page. A run against the pretend phone (`flow`, the page's `?pretend`) can reach a stand-in only.
- **The build that is used is the exact, tested one.** A factory image, downloaded or handed in, is checked by size and SHA-256 against the release's signed build index. The match is the build with both the phone's bootloader and its baseband, marked tested, or the install stops: don't add a nearest-build fallback. A build that is listed and not tested is told to update Android only when a newer build was tested; otherwise it is "not tested yet", and the owner is told to wait.
- **The installer never switches slots to look at the other one.** A phone that reports one slot's bootloader version stops the install, and nobody's word is taken for the other slot. `BOOTLOADER_SLOT_PROBE` is parsed and nothing acts on it; acting on it is the maintainer's decision, per device. The one way past that stop is the maintainer's override (DESIGN §6, 2026-11-12), for a dry run only: don't list it in a usage text or give the page a control for it, don't let it pass any other stop, don't let it reach a session that could write, and don't take away the notice that says the check was skipped and the run is not evidence.
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

If you changed the flow or the page, also run `web/selftest.sh` (a headless Chromium clicks through the page against the pretend phone and a stand-in for Google's server; CI doesn't run it).

The toolchain is pinned in `rust-toolchain.toml`, so `cargo` installs the right one.

- **Never commit identifying data:** serial numbers, MAC or Bluetooth addresses, IMEIs, network names, account names. Use placeholders such as `<SERIAL>`. `fixtures/published/` holds the project's public release key and the small files of one published release; nothing else that names a person or a phone belongs in the repository.
- **A new check must fail when what it guards is removed.** Try it: take the guard out, see the test fail, put it back, and say in the pull request which you tried.
- **Say what you ran and what you didn't.** If a change wasn't tried on a real phone, write that in the pull request. Only claim what you tested yourself.
- **Cite only properly licensed sources** in code and commit messages: Google's open-source documentation for the fastboot and sparse formats, crate documentation, and this project's own files. Don't name or compare other mobile Linux projects or their tools.
- **Commit messages** say why the change is needed. AI coding agents may add a `Co-Authored-By` line and never add `Signed-off-by`.

## Licence

MIT, as the rest of Omarchy Mobile. By contributing you agree your work is under it.
