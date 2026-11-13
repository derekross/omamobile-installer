# Installer core: Rust or TypeScript, and which UI (decision D5)

A spike for Omarchy Mobile's [INSTALL-EXPERIENCE-PLAN](https://github.com/derekross/omarchy-mobile/blob/main/docs/INSTALL-EXPERIENCE-PLAN.md) M3 and M4, built on 2026-11-10 with rustc 1.98.1 on x86_64 Linux. No command was sent to a real phone: every fastboot test runs against a fake device.

This is the spike's report, kept as it was written. What has been built since (the install flow, the build index, the unlock guidance) and what is open now are in [docs/DESIGN.md](docs/DESIGN.md); the sizes and test counts below are the spike's. One thing the spike's front ends did is gone: they take nobody's word for the other bootloader slot any more (the `--bootloader-slots-match` option).

Each fact carries its source:

- **[B]** built or run here;
- **[R]** read in the crate's current source or documentation, or in a file on this machine;
- **[M]** from memory, not checked.

## Recommendation

1. **Write the core in Rust.** One crate holds the manifest rules, the signature and checksum checks, fastboot, the sparse splitter and zstd. The same source built as a Linux binary, type-checked for Windows and macOS, and ran as WebAssembly in Chromium under a strict content security policy **[B]**.
2. **Use one web UI for the page and the desktop app,** in a native window (Tauri 2, or wry and tao beneath it). The UI talks to a small installer API with two back ends: the WebAssembly core in the browser, the native core in the app. Nothing of the desktop app was built in this spike.
3. **Keep OpenPGP for the release signature, checked with rPGP.** It verified the published 2026.11.08 release natively and in the browser **[B]**. It is also 86 % of the WebAssembly module, which is the one cost worth a decision (see "The signature").

## What the spike shows

| Question | Result | Source |
| --- | --- | --- |
| Does one core serve the browser and the desktop? | Yes. `core` has no I/O; `usb` is one transport for Linux, Windows, macOS and WebUSB (nusb); `cli` and `web` are thin front ends | B |
| Does it read the real release? | The CLI checked the published cheetah 2026.11.08 bundle: signature by the release key's subkey, 19 files against the signed manifest (root image not fetched) | B |
| Does the browser build work under a strict policy? | Headless Chromium loaded it with `default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; connect-src 'self'` plus Trusted Types, verified the published release, and ran a whole install against the fake phone. No inline script, no third-party origin | B |
| Is the sparse output right? | A 2 GiB image cut into 6 and into 43 sparse images; AOSP's `simg2img` rebuilt the image from each set, byte for byte | B |
| Is memory bounded? | 39 MiB peak with a 32 MiB segment cap, 263 MiB with 256 MiB segments (native). The first version held 400 MiB: a megabyte of compressed text is hundreds of megabytes of image, so the pipeline now decodes one zstd block per step | B |
| Which targets compile? | Linux x86_64: built and run. `cargo check` passes for `x86_64-pc-windows-gnu`, `x86_64-pc-windows-msvc`, `aarch64-apple-darwin`, `x86_64-apple-darwin`, and `wasm32-unknown-unknown` builds | B |

Measured sizes **[B]**:

| Artifact | Bytes | Compressed |
| --- | --- | --- |
| CLI, Linux x86_64, release, stripped (links only libc and libgcc_s) | 1,812,160 | |
| `.wasm`: core, rPGP, nusb's WebUSB | 1,463,202 | 464,298 gzip, 346,837 brotli |
| `.wasm` without OpenPGP (can't install; built to measure) | 207,083 | 68,689 brotli |
| JavaScript glue (generated) | 38,478 | 6,249 brotli |

`wasm-opt -Oz` took the module to 1,375,447 bytes and made its brotli size slightly larger, so it isn't worth a build step.

zstd speed **[B]**: 870 to 970 MiB/s of raw image natively through decompression and sparse cutting (the 2 GiB synthetic image; the `zstd` tool decompresses the same file about three times faster). In Chromium the decoder ran at about 1,300 MiB/s on a small, very compressible fixture, which is not a root image. Both are far above what USB carries, but neither was measured on the real root image.

## Why Rust and not TypeScript

- **The desktop app needs native USB either way.** WebKitGTK and WKWebView, the system web views on Linux and macOS, have no WebUSB **[M]**. A TypeScript core in a native window would need a native USB helper beside it (the `usb` package, MIT, binds libusb **[R: npm metadata]**) or a bundled Chromium. With Rust the native USB code is the core's own transport, the same file the browser uses **[B]**.
- **The safety rules sit in types.** A `Partition` can only be made by the manifest parser, from a fixed list of slot B images and `userdata`. `Session` takes bytes only as a `VerifiedImage` or under a `VerifiedRootImage`, which only a signature-checked release can produce. The command layer is private. A front end can't send `flash:vbmeta_b` because there is no function that takes a partition name. TypeScript can express this by convention, not by construction.
- **One binary, no runtime** for the CLI and the app's back end: 1.8 MB against a Node or Electron runtime.
- **Licences are simple.** Every Rust dependency used is MIT, Apache-2.0 or BSD. The usual OpenPGP library for JavaScript, `openpgp`, is LGPL-3.0-or-later **[R: npm metadata]**, which needs thought for a bundled web page.

What TypeScript would do better: one language with the website, no WebAssembly boundary to debug, and a faster edit loop for UI people. The core is about 1,900 lines; the boundary cost is real but small. If nobody on the project will maintain Rust, that outweighs the points above.

## The signature

rPGP (`pgp` 0.21.0, MIT OR Apache-2.0, pure Rust) works natively and on `wasm32-unknown-unknown` (its `wasm` feature) **[B]**. It checks the certificate's self-signatures, the subkey binding and its back-signature; the core adds the trusted primary fingerprints, a refusal of revoked keys, and validity at the signature's own time.

Its cost **[B]**: 1.26 MB of the 1.46 MB module, and 168 of the workspace's 205 crates (RSA, DSA and ciphers the installer never uses; it has no feature to leave them out **[R]**). Compressed, that is about 280 KB before a 2.8 GB download, so size alone doesn't argue against it. The dependency count does a little.

Two alternatives, neither built:

- a hand-written verifier for exactly what the release key uses (v4 packets, Ed25519, SHA-512) on `ed25519-dalek` (BSD-3-Clause). Small, but it is new signature-parsing code that somebody has to own;
- a second, simpler signature beside the OpenPGP one, made at release time. That changes the release process and the key layout (RELEASES §10).

I would ship rPGP first and revisit only if the dependency count becomes a review burden.

A policy point for the maintainer: the release key's signing subkey expires two years after it was made **[B: read from the published key]**. The core judges validity at the signature's creation time, so an old release stays installable after the subkey expires; `gpg` compares with today's date. The front end gets the expiry and can do either.

## The desktop UI

**A web UI in a native window.** The page and the app then share markup, styles and behaviour, which is what M3 asks for.

- The UI calls one installer API (verify, pick a phone, check, flash with progress). In the browser it is the WebAssembly module. In the app it is the native core behind the window's message channel, which also gives the app what a browser lacks: plain files, resumable downloads without a storage quota, and USB without a chooser.
- Tauri 2 (2.12.5, Apache-2.0 OR MIT, released 2026-10-21) or wry 0.57 directly **[R: crates.io]**. `webkit2gtk-4.1` 2.54 is installed on this machine **[R]**; nothing was compiled against it.
- A native toolkit instead (Omarchy's own apps are Qt Quick) would look most at home on Omarchy, but it means a second UI to keep in step with the web page and a C++ boundary. The core doesn't rule it out later.

Not verified: the look of a web view UI under Omarchy's themes, start-up time, and packaging on each platform.

## What each platform needs

- **Linux.** systemd 262's `70-uaccess.rules` already grants the logged-in user any USB device with a fastboot interface (`ff4203` sets `ID_DEBUG_APPLIANCE`, which gets `uaccess`) **[R: the file on this machine]**. So a current Arch or Omarchy needs no rule; which systemd version first carried it I didn't check. `platform/linux/70-omamobile-fastboot.rules` is a fallback for older ones, untested. Chromium needs the same node access for WebUSB.
- **Windows.** nusb needs the interface bound to WinUSB **[R: nusb docs]**, and so does WebUSB **[M]**. Whether a Pixel's bootloader gets WinUSB without help, and whether Google's USB driver is enough for both, is not verified; the plan already marks it inferred. Linking a Windows binary here stopped at a missing MinGW `dlltool`; a Windows machine or CI runner settles it. Code signing is open (the plan's M3).
- **macOS.** No driver or permission setup; nusb uses IOKit **[R: nusb docs]**. nusb's docs say a vendor-class device may need `set_configuration` after opening, which the transport doesn't do yet. Linking needs Apple's SDK, so only `cargo check` ran. Signing and notarisation are open.
- **Browser.** WebUSB exists only in Chromium-based browsers **[M, as the plan says]**. `web-sys` treats its WebUSB bindings as unstable: the build sets `--cfg=web_sys_unstable_apis` **[B]**, and a web-sys update can change them.

## Crates

| Crate | Use | Licence | Latest stable | Notes |
| --- | --- | --- | --- | --- |
| `nusb` 0.2.9 | USB on four platforms | Apache-2.0 OR MIT | 2026-10-24 | Pure Rust, no libusb. First release 2023, still 0.x: the youngest dependency, and the one to test hardest |
| `pgp` 0.21.0 | Signature | MIT OR Apache-2.0 | 2026-10-03 | Since 2017, active. Large (above) |
| `ruzstd` 0.9.1 | zstd decoding | MIT | 2026-10-11 | Pure Rust, so no C compiler for any target. About 3x slower than the C library natively |
| `sha2` 0.11.1 | SHA-256 | MIT OR Apache-2.0 | 2026-10-12 | RustCrypto |
| `serde`, `serde_json` | manifest.json | MIT OR Apache-2.0 | 2026-10-11 | |
| `wasm-bindgen` 0.2.130, `web-sys`, `js-sys`, `wasm-bindgen-futures` | Browser bindings | MIT OR Apache-2.0 | 2026-10-30 | The CLI tool's version must equal the crate's |
| `pollster` 1.0.1 | Runs futures in the CLI | Apache-2.0 OR MIT | 2026-07-10 | nusb needs no async runtime |

Dates and licences are from crates.io on 2026-11-10 **[R]**. The `zstd` crate (BSD-3-Clause, the C library) was left out on purpose: it needs a C compiler for every target, WebAssembly included.

## Risks and what the spike did not verify

- **No real phone.** The protocol code follows AOSP's fastboot README and passes against a fake that I also wrote, so a shared misreading would pass. Unknown on real hardware: how the Pixel bootloader answers `getvar:all`, whether it reports per-slot bootloader versions (D4 is open), its `max-download-size`, how it treats many `flash:userdata` commands in a row, and timing. The first real run should be the CLI's `--dry-run`, which sends only `getvar`.
- **FILL chunks for zero blocks.** The splitter writes zero blocks as FILL, as `img2simg` does, so the phone writes them. The published root image is 12 GiB raw; how long the bootloader takes over the zero runs is unmeasured. Skipping them would be faster and would leave stale data inside the image's extent.
- **Segment size on the web.** Each sparse image is held in memory before it is sent. A cap (32 or 64 MiB) keeps the browser small but means a few hundred flash commands for the real image; the per-command overhead on the phone is unknown.
- **The owner's answers.** Neither front end could put `firstboot.conf` on the phone: that needed the install link (M2) or an ext4 writer. (Since 2026-11-13 they go into placeholders the release image keeps for them, DESIGN §12.)
- **Verify before write.** The root image's SHA-256 is known only once all of it has been read, so the web flasher must store the download, hash it, then stream it. The core enforces the order with a proof token, but can't tie the token to the bytes streamed afterwards. Browser storage for 3.4 GB is unverified (the plan's M4).
- **The manifest lacks the raw image size.** The core reads it from the zstd frame header, which the published image has **[B: first 32 bytes fetched]**. `root_image.raw_size` in `manifest.json` would make it signed data.
- **Stricter than `install-phone`** in one place: an unknown key inside `install` or a write is refused, not ignored. A format 1 manifest that later gains such a key would need a new installer, which seems right for a plan, but it is a choice.
- **Not built:** the desktop window, adb (the reboot into fastboot), the build index, the install link, downloads with resume, timeouts in the browser, unlock guidance.
- **Unsigned bundles** are not supported by the core at all. Development needs a decision: a separate build, or a test key.
