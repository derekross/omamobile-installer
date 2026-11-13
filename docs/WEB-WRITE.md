# The web page's write path: a preview that does not ship

Status (2026-11-13): the page now downloads and checks the release itself (§6, "The download"), in the page that ships and in this preview. Status of the write path (2026-11-12): **built, and tested against a pretend phone only. It has never been run against a real phone, and never in a real browser with a real USB device.** Every test here runs in node or in a headless Chromium that is started with WebUSB switched off. The page that ships is unchanged in what it can do: it cannot write to a phone, and `tools/check-shipping` fails if anything described here is in it.

This document is the design of the page's write preview, how it is kept out of the page that ships, what tests it, the problem of getting a system image of several gigabytes through a browser tab, and the steps of a first attended run in a browser. [DESIGN.md](DESIGN.md) is the design of the flow it drives, and its §11 the write path itself; [WRITE-RUNBOOK.md](WRITE-RUNBOOK.md) is the command-line tool's first run, which comes before anything here is tried on a phone.

## 1. What it is, and what it is not

A second build of the web page, made with the cargo feature `web-write-preview`, that can do what `omamobile-install install --write` does: after the flow's dry run, the list of what will be written and the phone's codename typed by the owner, it writes slot B's boot images and `userdata` through the session `Flow::begin_write` opens, then `set_active:b` and `reboot`.

- **It is not a decision that the page writes.** DESIGN §7 says a front end writes "only if it is a front end that may", and today that is the command-line tool. This build exists so that the day the maintainer decides, the transport, the steps and their tests are there and have been reviewed. Until then it is a preview, and the files that say "the web page cannot write" now say "the web page as it ships".
- **It adds no second way to send a fastboot command.** The page's module has one way to a phone, the core's command layer, whose gate (`allowed(command, flashable)` in `core/src/fastboot.rs`) is the one the command-line tool's commands leave through. The module exports nothing that takes a command or a partition name, and a test lists every export.
- **It adds no second way to a session that writes.** It calls `Flow::begin_write`, like the tool. It cannot make a `WriteToken`, and it opens no session of its own.
- **It does not take the owner's statement about the other bootloader slot.** That statement is the command line's, by decision (DESIGN §6). The preview's options are the page's own with the write switch on. So **a Pixel 7 Pro stops the preview's flow at the first look** (`bootloader-slots-unknown`), and the Pixel 7 Pro is the only phone the write path accepts (`WRITE_DEVICES`). No write can be reached from a browser on a real phone today. §8 says what the first run in a browser can therefore show, and what would have to be decided before it can show more.
- **It never accepts Google's terms and never sends an unlock command.** It states, like the page that ships, that it cannot fetch from Google; a locked phone is shown the unlock command as text.
- **What it installs is what the tool installs:** a phone with no user account (DESIGN §12).

## 2. The shape

```
web/static/release-fetch.js          the page's download of the release, kept in the browser (§6); in every build
web/preview/index.html, preview.js   the preview's page: the field for the codename, the file picker until there is a release
web/preview/write-walk.js            reads the release's files (downloaded or picked), hands their bytes to the module
        |
web/src/write_preview.rs             the flow's write steps for the page (begin_write among them)
web/src/webusb.rs                    the page's own transport: core's Transport over WebUSB
        |
core: Flow -> Session -> Client      every rule, every stop, the one gate, the sizes
```

Both `.rs` files are compiled only with the feature. Both scripts, and the preview's first page, are copied only into a preview build. The page's own script (`web/static/app.js`) got an extension point for them and nothing else: an object `additions` (how a flow is begun, controls for a screen's action, the status line), which nothing in the page that ships sets.

### The steps the module offers (`write_preview.rs`)

| The page calls | The module does | The core's part |
| --- | --- | --- |
| `InstallFlow.begin_write_preview(release files, index, reinstall)` | begins a flow with the page's options and `write` on | `Flow::begin`: verifies, refuses a release for another phone than `WRITE_DEVICES` |
| `images_check_boot(n, bytes)`, `images_root_begin/update/done` | checks every file of the release against the signed manifest, before any session | `verify_boot_image`, `RootImageCheck` |
| `session_open(phone)` | refuses unless every file was checked; then asks the flow for the session of the step it is on | `Flow::dry_run` or, once the codename was typed, `Flow::begin_write` |
| `session_boot(n, bytes)` | checks the image again, hands it to the session | `Session::flash_boot` |
| `session_root_feed(chunk)` | feeds the compressed bytes to the session's hash and to the splitter; every piece that completes is sent before it returns | `install::UserdataFeed`, `Session::flash_userdata_segment` |
| `session_finish()` | the last piece, the session's own hash check, `set_active:b`, `reboot`, then the session back to the flow | `finish_userdata`, `activate_and_reboot`, `write_done` or `dry_run_done` |
| `confirm_install(typed)` | passes on what was typed, unchanged | compares it with the codename, or stops |
| `session_abandon(why)` | a file could not be read any more: the session goes back to the flow as failed | `write_failed` or `dry_run_failed` |
| `session_progress()` | the pieces so far: bytes, blocks, how long each flash took | `Session::pieces` |

When the session returns an error, the module hands the session back to the flow at once (`write_failed`, with the error as it came) and answers `false`. The page's script cannot forget to: it is not its job. The next screen is the flow's stop, which says how far the phone confirmed the writes.

One loop cuts and sends the root image, for both front ends: `install::UserdataFeed` is new in the core, and the reader-based function the tool calls (`flash_userdata_with_progress`) now reads into it. The cap on a piece that both front ends pass is one constant, `install::SEGMENT_CAP` (64 MiB, the tool's number until now).

## 3. The transport (`webusb.rs`)

It implements `fastboot::Transport`, the two functions the command-line tool's USB transport implements: `send` and `recv`.

- **The device** is the one the person picks in the browser's own chooser (`navigator.usb.requestDevice`), shown with one filter: class 0xff, subclass 0x42, protocol 0x03. `getDevices` is never called, so the page reaches no device from an earlier visit without the chooser.
- **Pinning** is the core's. The serial number the browser reports for the picked device becomes the `PhoneId` the flow is handed; the phone must answer `getvar:serialno` with the same one at every look, at `connect`, and before every `download:` and `flash:` pair (DESIGN §8). A device for which the browser reports no serial number is asked nothing.
- **Sizes are the core's.** One `send` is one `transferOut` of exactly the bytes handed in. The command layer hands a download over in pieces of `fastboot::SEND_CHUNK` (1 MiB); the download's own size is what the session's splitter cut, at most the phone's `max-download-size` and `SEGMENT_CAP`, covering at most `sparse::MAX_PIECE_BLOCKS` blocks. The transport's only number is 256, the most bytes a fastboot answer has, rounded up to whole packets of the IN endpoint.
- **Every wait ends.** Each promise the browser gives (`open`, `claimInterface`, `transferOut`, `transferIn`) is raced against a timer of `sparse::FLASH_WAIT_SECONDS` (300 s), the wait the tool's transport has. A wait that runs out is `Error::Transport("timed out waiting for the phone")`, the tool's words. WebUSB cannot cancel one transfer, so the device is closed, without waiting for that either.
- **A lost device** is a rejected transfer (Chromium: `NotFoundError`, "The device was disconnected."), and comes back as `Error::Transport` with the browser's words. So does a transfer that ends `stall` or `babble`. A transfer that took fewer bytes than it was handed is not the transport's to judge: it returns the count, and the core makes it `ShortDownload`.
- **Over is over.** After any of these the transport hands the device nothing more.
- The flow turns `Error::Transport` after `begin_write` into `write-interrupted`, with "The connection to the phone was lost" and the closing lines for how far the writes got; before a session is open it is an error on the screen with the flow where it was, as in the tool.

Why a transport of its own, when the page already reads phones through `omamobile-installer-usb` (nusb's WebUSB backend): that one has no wait in a browser (its comment says "a timeout there is the page's"), and a transport that can wait for ever must not carry a write. The preview build uses its own transport for every look at a phone, not only for the write, so one build has one transport.

## 4. Kept out of the page that ships

| What | Held by |
| --- | --- |
| `webusb.rs` and `write_preview.rs` are compiled only with `web-write-preview` | `#[cfg(feature = …)]` on both `mod` lines; `tools/check-shipping` reads them, and every other line of the module that names something of the two |
| The feature is no default | `tools/check-shipping` |
| A preview build never lands in `web/dist` | `web/build.sh web-write-preview` builds into `web/dist-preview`; with `selftest` (the tests' build, with the test key) into `web/dist-preview-test` |
| The preview's scripts are not in `web/static`, which every build copies | `tools/check-shipping`: three files there, one script tag, none of the preview's names |
| The shipping module holds none of it | `tools/check-shipping --built`: the module's bytes hold none of the preview's words or export names, its glue none of its functions, the module loaded in node exports none of them, and `web/dist` holds the page's three files and no other |
| The shipping page says it cannot write | `tools/check-shipping --built`, as before |
| The preview build does hold it, so the looks above can find it | `tools/check-shipping --preview`: `web/dist-preview` holds every one of those words and exports, says on its first page that it can write and has never met a real phone, and holds no pretend phone, no test key and none of its tests |
| The write switch | set in two lines in the whole repository: the tool's, from `--write`, and the preview's `options.write = true;`. `tools/check-shipping` reads both |

**The default build after this change.** The shipping module is the module the branch started from but for 13 bytes, which are line numbers (compared in §7), and its glue is the same file. The shipping page's script differs by the extension point above; the first status line now reads "loaded (this page cannot write to a phone)", because "no build of this page can write" stopped being true.

## 5. Tests

`web/preview-test.sh` builds `web-write-preview selftest` and runs two parts. Neither can reach a USB device: node has none, and the browser is started with WebUSB off, in an empty `/dev` where bubblewrap is installed; the page's test refuses to run in a browser that has WebUSB.

**The pretend phone** (`web/preview/test/fake-usb.mjs`) is the core's pretend phone (`core/tests/support/fake.rs`) written again in JavaScript, behind an object with the shape of a WebUSB `USBDevice`: the same partitions, the same parser for sparse images, the same switches (FAIL on a command, a short `DATA`, fewer bytes taken, another serial number, the cable pulled before a command or before its answer), and three a USB device adds: the cable pulled inside a download, an answer that never comes while the device stays on the bus, and a transfer the browser ends with `stall`. It records every call made on it.

**In node** (`web/preview/test/write.test.mjs`, 32 tests): the module's chooser and transport, the flow with the write switch on, and the page's own walk of the files (`write-walk.js`).

| Asked for | Test |
| --- | --- |
| A whole pretend install through the page's path | "a whole pretend install …": the commands in order, the partitions byte for byte (the root image unpacked by node's own zstd), slot B active, nothing else touched |
| A wrong serial | "a phone whose serial number is not the chosen device's …", "a device that reports no serial number …", "another phone at the dry run …", "another phone after the codename …", "a serial number that changes between two writes …" |
| A device that vanishes mid-flash | "… in the middle of a download …", "… while it flashes …", and "… at each command of the write …": the cable pulled before each command from the first `download:` on (47 of them, the questions between the writes included), and with the answer lost for each that is not a question |
| A short write | "a transfer that takes fewer bytes …", "a phone that offers to take fewer bytes …" |
| A FAIL answer | "a FAIL answer is the core's stop …" |
| An answer that never comes | "an answer that never comes …": the timer is the test's, it asks for 300000 ms, and when it runs out the install stops and the device is closed. "every wait for the phone has the core's limit …" counts one timer per transfer over a whole install |
| No command outside the gate | after every test, every command any pretend device received is matched against the gate's list written out again in the test; "no command outside the gate can be sent …" lists every export of the module; "the write steps out of order are refused …"; "anything but the codename …"; "a phone that tells one bootloader slot …" |
| Sizes are the core's | "every download is the size the core's session cut …": with a phone that takes 4 MiB, the root image's one piece arrives as 1 MiB, 1 MiB, the rest |
| Files | a changed boot image, a root image that changed before its check and one that changes after it (slot B is never made active), a file that can no longer be read |

Also after every test: nothing but `open`, `close`, `claimInterface`, `transferOut` and `transferIn` was called on the device (no control transfer, no `reset`), on the fastboot interface and its two endpoints, and `getDevices` never.

**In a headless Chromium** (`web/preview/test/page-run.mjs`, 17 checks): the preview's page as it is, with its scripts loaded as its first page loads them, clicked through: what its first line and status line say, the chooser's filter, the file picker, a whole install, anything but the codename, a phone that vanishes while it flashes, a phone that tells one bootloader slot (the flow's stop, and the page offers nothing but "Start over"), and a locked phone.

**A check counts only if it fails when what it guards is removed** (DESIGN §10). Tried for this branch, one change at a time, each put back:

| Change | Caught by |
| --- | --- |
| The wait halved | node: both timer tests |
| The transport sends at most 64 KiB of what it is handed | node: every install |
| The transfer is not raced against the timer | node: the timeout test never ends, and the run fails for it |
| The device is not closed after a wait ran out | node: the timeout test |
| The chooser's filter without the protocol | node: the whole install |
| The preview states the other bootloader slot | `check-shipping` (sources); node: the one-slot test |
| A session's error is not handed to the flow | node: every stop |
| A session is opened without the files checked | node: out of order |
| The walk skips the checks | node: every install |
| The feature made a default | `check-shipping` (sources), and `--built`: five lines, among them the module's bytes and its exports |
| A `mod` line without its `cfg` | `check-shipping` (sources) |
| The shipping page loads the preview's script; the shipping script confirms an install | `check-shipping` (sources) |
| The page tidies the codename before handing it in | the page's run |
| The preview's scripts copied into every build | `check-shipping --built`: three lines |

## 6. The streaming problem

### The numbers

From the published release's own files (`fixtures/published`, release 2026.11.08 for the Pixel 7 Pro): the root image is **3 412 028 613 bytes compressed** (zstd), and its frame header gives **12 GiB unpacked**, with a 4 MiB window. Slot B's five boot images are tens of megabytes each. The session cuts the root image into sparse images of at most 64 MiB to send and at most 256 MiB of `userdata` to write, so an install is some dozens of `download:` and `flash:` pairs at least.

### How the page gets the image

| Way | What it takes | State |
| --- | --- | --- |
| **The owner's files, through the File API.** The owner downloads the release (its `images/` and the `.img.zst`) with the browser or anything else, and picks the files in the page | A `File` is a handle; the browser reads it from disk when asked. Nothing is uploaded. A file that changed on disk after it was picked fails to read (`NotReadableError`), which ends the session through the flow | **Implemented.** Enough for an attended test; not what an owner should have to do |
| **A download by the page** from `pkgs.omamobile.org`, which sends the CORS header for it | The image is read **four times** in a run that writes (below), so it must be stored, not streamed: the Origin Private File System is the place, and it is subject to the origin's storage quota and to eviction unless the page asks for persistence. It needs resume (a 3.4 GB download that restarts on every hiccup is not an install path), and the SHA-256 over the whole file before any use | **Built (2026-11-13),** in the page that ships and in the preview: "The download" below. Tested against stand-ins in node and in headless Chromium; never against the real server for the large files |
| **A release saved on this computer,** picked in the page's release panel: its files, its folder, or its one-file `.tar` | The same check as a download; the files are read in place (a `.tar`'s members are slices of it) | **Built (2026-11-13)** |

Google's factory image is neither: a page cannot read it from Google's server at all (DESIGN §7), and this version does nothing with it after checking it.

### The download (`web/static/release-fetch.js`)

One script, in every build (the page that ships loads it from `app.js`; it holds nothing of the write preview, and `tools/check-shipping` reads that it reaches no phone and names one server). Everything it reaches outside itself is handed in, so the tests run it in node.

**Trust is the module's, as in the command-line tool.** The script finds the release and fetches bytes; the wasm module decides what they are worth, with the same code the tool runs (`core/src/bundle.rs`, rPGP, which builds for `wasm32-unknown-unknown` and was already in the module):

1. **The pointer.** `releases/<codename>/latest.json` (written by omarchy-mobile's `tools/release-upload --latest`) for the phone chosen at the top of the page. It is followed only when its `format` is 1, its codename is the one asked for, its version is a calendar version, and its `base_url` is exactly the directory beside it on the same server; a pointer anywhere else is refused before anything is fetched from there. It is only a pointer: it can name a version, and the signature decides the rest.
2. **The small files** (`SHA256SUMS`, `SHA256SUMS.sig`, `omamobile.gpg`, `manifest.json`, `MANIFEST`, `device.conf`, and `factory-builds.json` when there is one), at most 4 MiB each. `manifest.json`'s SHA-256 is compared with the pointer's, only to say early that the two disagree.
3. **The module's check** (`verify_release`, and the flow's own `InstallFlow.begin`): the release key's detached signature on `SHA256SUMS`, against the primary key's fingerprint built into the module (`OMAMOBILE_KEYS`, the release key `2056…4673` and the original package key), then the whole chain of DESIGN's bundle rules. The certificate itself comes from the release (`omamobile.gpg`); one with another fingerprint is ignored, so the pin is the fingerprint, as in the tool. A release that fails is the flow's `release-not-verified` stop, and no download is offered.
4. **The large files,** on the owner's click: each boot image, then the root image, from the version directory, each checked against the signed manifest by the module (`Release.check_file` for a boot image, whole; `Release.root_check` for the root image, a piece at a time as the bytes arrive). A file that fails is deleted.

**Storage.** The Origin Private File System, `omamobile-releases/<codename>/<version>/`. A large file is kept as numbered parts of at most 32 MiB (`<name>.NNNN.part`) and a record (`<name>.json`: the signed SHA-256 and size, the server's ETag, whether it is complete). Why parts: a writable in Chromium writes to a file of its own and replaces the file only when it is closed, so one file of 3.4 GB written in one go would be lost whole with the tab, and opened again with its data kept it is copied whole. A part is closed when it is full and whenever a transfer ends, broken or not; a tab that is closed or crashes loses at most the part it was writing. The parts are put back together as one `File` (`new File(parts, name)`), which the browser reads from disk when it is read: that `File` is what the write walk reads, a mebibyte at a time, so the image is never whole in memory. Other versions of the same phone are deleted when a new one is opened; the panel's "Delete the downloaded release" deletes every one.

**Before the first byte:** `navigator.storage.persist()` is asked for (a browser may evict an origin's files under pressure otherwise; a no is said on the page and does not stop the download, since everything is checked again before use), then `navigator.storage.estimate()`: what is still to download plus 256 MiB must fit in the quota left, or the page says how much the browser allows and how much is needed, and offers the saved release instead.

**Resume.** What is kept goes through the root image's check first, from its first byte (as the core does for Google's file), then the rest is asked for with `Range: bytes=N-` and `If-Range: <ETag>`. Only a `206` with `Content-Range: bytes N-(size-1)/size` is taken as the rest. A `200` is the whole file again (a server without Range, or a file that changed): what was kept is deleted, with its part of the hash, and the file starts over. Any other answer, a length other than the signed size, or one byte more than the signed size ends the file. A transfer that breaks, or sends no byte for 60 s, is tried again from where it stopped, five tries in all (2, 5, 15 and 30 s apart); then the panel says what happened and its button goes on from there. A request is sent with no cookie and no referrer, `cache: "no-store"` (the files are kept once, here), and `redirect: "error"`.

**A later visit.** The release's directory is found by version, and each file's record by the signed SHA-256; a file that is complete is not downloaded again. It is read back from disk through the same check before it is used, on every visit (12 s or so for the root image at the module's hashing speed), because bytes on a disk can change between visits and the check costs no download.

**A release saved on this computer.** The panel's file inputs take the release's files (any number, by name: every name the page needs is unique in a release), its folder, or its `.tar` (GNU tar, as `tools/lib/bundle-tar.sh` makes it: ustar headers with their checksum, GNU long names and pax headers read, base-256 sizes; its members become slices of the `.tar`, read in place). The small files go through the same check, and every large file is checked at once, the root image a piece at a time.

**The page that ships** downloads and checks the release, and then does what it did: its flow ends at the dry run, which sends no image to the phone. **The write preview** hands the checked files to its walk (`page.large`), which checks each again, as it checked the picked files before; its file picker on the dry run's screen stays for when there is no release in the panel.

**Each step waits for the one before it.** The panel's download button can be pressed only once the release's small files passed the module's check; the preview's "Run the dry run" only once every file of the release is there (downloaded and checked, or picked with the names and sizes the release lists). The core's flow orders the rest.

**The phone, picked again.** When an action fails because the browser lost the device (the phone restarted, the cable moved), the page forgets the device, and the next step's button says "Choose the phone and run the dry run": the chooser needs the owner's click, so the page cannot do it by itself. It adds no sentence of its own about the phone (DESIGN §7, rule 1); the error under the screen is the browser's.

**The preview's wake lock** is taken again whenever the tab is shown, while a write lasts: the browser lets go of it each time the tab is hidden.

**Its tests.** In node (`web/fetch-test.sh`, `web/test/release-fetch.test.mjs`, 22 tests), against a stand-in for the release server on 127.0.0.1 (`web/test/release-server.mjs`: the headers `tools/server/releases-nginx.sh` sets, ranges with an ETag and `If-Range`, an `OPTIONS` preflight, and a way to break each answer) and a stand-in for the browser's file system on disk with its commit-on-close (`web/test/opfs-stand-in.mjs`). The release is `fixtures/selftest`, signed with a test key whose secret was never kept, checked with that key by a function of the self-test builds only (`selftest_verify_release`; `check-shipping --built` reads that the shipping module has none of it), and refused by the built-in release keys.

| Asked for | Test |
| --- | --- |
| A whole download | "a whole download …": every file once, without a range, the bytes the release's, persistence asked; "a second visit downloads nothing …" |
| A resumed download after a cut | "a download that is cut goes on from the byte it reached …" (the second request's range is exactly the bytes that arrived, with the ETag; about the file's size written in all); "a transfer that goes quiet …"; "a tab closed in the middle …" (only closed parts are kept, and the next visit asks from their end) |
| A bad checksum | "a root image with one byte changed …", "a boot image with one byte changed …" (nothing after it is fetched), "more bytes than the signed size …"; both deleted |
| A bad signature | "a signature that does not verify …": the module refuses, the flow begun from the files stops at `release-not-verified`, nothing large is fetched |
| A server without Range | "a server without Range …": the cut file starts over, with its hash; "an answer for another range …" is refused |
| Too little quota | "too little room …": refused before the first large byte, with both numbers; "persistence refused …" goes on |
| The pointer | "a pointer to anywhere but beside itself is not followed …" |
| A saved release | its files, its `.tar` (made by GNU tar in the test), a changed root image and a missing signature |
| The preview's walk | "downloaded into the file system, the release goes through the write preview's walk …": a cut download, the dry run and the write to the pretend phone from the kept parts, slot B active |

A browser's `fetch` refuses to be called as a method of another object, and node's does not: the tests' `fetch` refuses the same way, after the page's first run in Chromium found such a call. Each guard was removed once to see a test fail: the range on resume (3 tests), the root image's check, the boot images' check, the quota check, the pointer's rule, the restart on a `200`, the `Content-Range` check (one test each), and `fetch` called as a method (16).

**In a headless Chromium** (`web/selftest.sh`, section 8 of the page's self-test, 10 more checks): the page that ships, built for its self-test, downloads from the same stand-in on another origin, so the browser's own cross-origin rules and its own Origin Private File System are the ones in use: a cut download resumed with a range, every file a `File` of that file system, a later visit that downloads nothing, a changed root image refused and deleted, a signature that does not verify (no download offered), a server without Range, and the delete button. The browser is held to 127.0.0.1 as before.

**Measured, with the stand-ins** (this computer, no phone, no network but the loopback):

| What | Where | Result |
| --- | --- | --- |
| A made-up release with a root image of 1 GiB of random bytes, signed with a key made for the run (`web/test/measure-download.mjs 1024`), downloaded whole | node 26, the stand-in server, the file-system stand-in on an SSD | 5.3 s: 194 MiB/s, bound by the module's SHA-256 (about 270 MiB/s, §6 "What was measured"); 32 parts; 1024 MiB written |
| The same, cut at half and gone on with | the same | 7.7 s in all: the kept 512 MiB hashed again (about 2.5 s), then the other 512 MiB asked with `Range: bytes=536870912-`; 1024 MiB written in all, nothing twice |
| A later visit | the same | 5.4 s, nothing downloaded and nothing written: the kept GiB read back and hashed |
| For the published root image (3.4 GB), from these | extrapolated | about 18 s of hashing for a download at loopback speed, the same again on each later visit; the network is the bound on a real download |
| The test release (1.6 MB) in headless Chromium 155, cut once | the page's self-test | 2.1 s, nearly all of it the first retry's 2 s wait; a later visit's check 0.1 s. Says that it works in a browser, not how fast |

### What only a real browser and the real server can answer about the download

1. **A whole download of 3.4 GB in a real tab:** the time, and whether the tab stays awake and in front long enough (whether Chrome freezes a hidden tab while it downloads, before any phone is attached, was not looked up or tried).
2. **The quota Chrome gives the origin on an ordinary disk,** and what `persist()` answers there without a prompt (Chrome decides by site engagement; Firefox asks). Headless Chromium answered no, with a quota of 10 GiB.
3. **Whether `new File(parts)` of OPFS files stays on disk** in a real tab, or the browser reads them into memory somewhere: the tab's memory while the root image is hashed and walked.
4. **What a real dropped connection looks like** (a laptop changing networks, a sleep): which error `fetch` gives and after how long, against the 60 s stall timer.
5. **The real server's answers to the page's range requests** from a browser: the `206`, `Content-Range` and `ETag` through nginx's CORS headers (the server's script exposes them; only `curl` has read them), and that its `no-cache` on `latest.json` and a year's cache on the version directory do what the page expects with `cache: "no-store"`.
6. **The site's own headers** where the page is served: its Content-Security-Policy must allow `connect-src https://pkgs.omamobile.org` (as `web/serve.py` now sends it), and its Permissions-Policy must allow `usb`.
7. **Eviction:** what a browser does with the 3.4 GB under storage pressure between two visits. The page checks everything again, so the cost is a new download, not a wrong file.

### Decompressing and cutting without holding it

Nothing is new here but the push-fed loop. The page reads the file in pieces of 1 MiB (`file.slice(…).arrayBuffer()`; this number is only how much is read at a time), and awaits `session_root_feed(piece)` before it reads the next. Inside:

```
compressed bytes -> the session's SHA-256 (userdata_streamed)
                 -> ZstdPush (ruzstd, one block at a time; the frame's window) -> SparseSplitter
                 -> a finished sparse image -> download:, the bytes in 1 MiB transfers, flash:userdata
```

`UserdataFeed::feed` hands the splitter at most 256 KiB of input at a time, whatever it is given, and sends every sparse image that completes before it takes more. The await is the back-pressure: the page reads no faster than the phone takes. What is in memory at once: the piece just read (1 MiB, in JavaScript and once more in the module), the zstd window (4 MiB for the published image; the core refuses more than 128 MiB), the sparse image being built and the one being sent (64 MiB each at most), and a copy of the megabyte in flight, which the browser keeps for the transfer.

**The image is read four times in a run that writes:** hashed before the dry run's session is opened; hashed again and cut through the dry run's session (which sends nothing; this is where the number of pieces on the confirmation comes from); hashed after the codename, before the session that writes is opened; and hashed again, cut and sent. The tool does the same. It is what "every file is checked before the first write" and "the bytes that are sent are the bytes that were checked" cost (DESIGN §11, rule 10), and nothing here shortens it.

### The browser's limits

- **Which browsers have WebUSB at all.** Chromium-based ones: Chrome, Edge, Opera, on Linux, macOS, Windows and ChromeOS, and Chrome on Android. Firefox and Safari do not implement it. So the page's write path can never be the only install path. On Linux the browser's user must be allowed to open the device node (`platform/linux/70-omamobile-fastboot.rules`, or systemd's own rule); a browser packaged in a sandbox that hides USB devices cannot. On Windows the fastboot interface needs a WinUSB driver bound to it before any browser can claim it. Neither was tried.
- **Where a page may use it.** A secure context (`https`, or `127.0.0.1`), a user's click for the chooser, and a Permissions-Policy that allows `usb` (`web/serve.py` sends `usb=(self)`; the real site must send the same, and the page must not be framed by another origin).
- **Memory.** A wasm32 module has 4 GiB of address space at most, and its memory never shrinks. The pipeline above needs on the order of 200 MiB, and that is why the 64 MiB cap is there: a Pixel offers a far larger `max-download-size`. The boot images are read whole, one at a time.
- **WebUSB transfer size.** Chromium has a limit of 32 MiB for one transfer's buffer (`kUsbTransferLengthLimit` in `third_party/blink/renderer/modules/webusb/usb_device.cc`, behind a feature switch). The core hands a transport 1 MiB at a time, so the page is far below it and has no arithmetic of its own to keep it there. What one `transferOut` of 1 MiB costs in round trips between the page and the browser's USB service, 3 400 times and more, is not known.
- **A tab put to sleep.** Three different things. *Timers* in a background tab are slowed, so the 300 s wait can end late there, never early. *Freezing* (Chrome on Energy Saver, since Chrome 133) stops the scripts of a tab that has been hidden for some minutes; Chrome's description of it ("Freezing on Energy Saver", developer.chrome.com) lists a page that controls a device through WebUSB among those it does not freeze. That is Chrome's statement, not something checked here. *Discarding* a tab, or closing it, or the computer going to sleep, ends the install where it is: the phone is left as the stop for that point describes it, with nobody shown the stop. The preview asks for a screen wake lock and asks before the tab is closed while a session is open; neither is a guarantee. **A write from a browser needs the tab kept in front and the computer kept awake, and the page cannot make either true.**
- **A module that stops.** A panic in the module (it is built with `panic = "abort"`) ends it with no stop on the screen. The phone is then as far as the last command it took.

### What is implemented

The release downloaded by the page and kept in the browser, with resume, or a saved release picked (above); the owner's files through the File API on the dry run's screen; the checks before each session; the push-fed loop in the core, shared with the tool; the transport with the core's wait; progress as the session reports it; the wake lock and the question before closing. Not implemented: anything of the install that survives a reload (the release's files do; a reload starts a new flow, and a rerun writes everything again: DESIGN §11, "Interrupted").

### What was measured, and in what

All on the computer this was written on, with no phone. None of it is a measurement of a browser talking to a USB device.

| What | Where | Result |
| --- | --- | --- |
| A whole pretend install through the page's path (the test bundle: five images of 4 to 16 KiB, a root image of 2 MiB cut into 6 pieces for a phone that takes 256 KiB) | node 26 (V8), the pretend device | 122 commands, 133 OUT transfers and 189 IN transfers over the whole run (the looks, the dry run and the write), about 50 ms for the write. It says the path works, and nothing about speed |
| The same with a phone that takes 4 MiB | the same | one piece of userdata, the largest OUT transfer exactly 1 048 576 bytes |
| SHA-256 in the module | node 26 (V8), 512 MiB through `RootCheck` | about 270 MiB/s: some 12 s for one pass over the published image, four passes in a run |
| zstd in the module | node 26 (V8), the 3 MiB pattern fixture 40 times | about 800 MiB/s, the same as the native tool on that fixture (809 MiB/s). The page's self-test times the same loop in headless Chromium 155 and printed 1846 MiB/s, with a millisecond clock over less than a tenth of a second, so read both as an order of magnitude. The fixture compresses far better than a file system does: an upper bound either way |
| The splitter's memory on a large image | the native tool's `sparse` on a made-up 4 GiB image (a quarter random, half zeros, a quarter text), 64 MiB cap | 33 pieces, the largest 67 105 280 bytes, peak resident memory 194 MiB. Native, not wasm: the same code and the same buffers, not the same allocator |
| The module's memory after hashing 512 MiB and the self-test's install | node 26 | 24 MiB: hashing holds nothing |
| The page in a browser | headless Chromium 155, WebUSB off, the pretend device | the 17 checks of §5. No timing was taken |

### What only a real phone and a real browser can answer

1. **Whether Chromium's WebUSB carries fastboot to a Pixel's bootloader at all** from this page: the chooser's filter matching the bootloader's interface, `open` and `claimInterface` on Linux, the IN request of 512 bytes answered by a short packet. The first look answers it (§8, step 4 and 5).
2. **How long `getvar:all` takes** with one `transferIn` per line, and whether the bootloader's answers arrive one packet per transfer as the transport assumes.
3. **The throughput of a download** in 1 MiB transfers through the browser's USB service, against the tool's on the same phone and cable.
4. **How long a `flash:` of the largest piece takes** against the 300 s wait. The tool's first run answers this for the phone (WRITE-RUNBOOK step 5); the browser adds only its own overhead.
5. **What the browser does to a pending `transferIn` when the device is closed after a timeout,** and whether an answer that arrives late is seen by a later session. The transport does not rely on either.
6. **What a real disconnect looks like to the page:** the name and words of the rejection, and whether it comes at once or after the transfer's own time.
7. **Whether the tab stays awake** through an install of many minutes with the window in front, on battery, with Energy Saver on.
8. **How fast the real image goes through the module in a real browser** (hashing at disk speed, zstd on real file-system data, the garbage the megabyte copies leave). This one needs no phone: a pretend device in a real browser with the real release's files would measure it. Not built.
9. **Memory in a real tab** over the whole install.
10. **Everything WRITE-RUNBOOK lists as unknown** (the sparse pieces accepted by a real bootloader, `current-slot` after `set_active:b`, the first start) is unknown here too, and is the tool's to answer first.

## 7. What the design did not cover, and what was decided here

Each is the maintainer's to confirm or change at review.

1. **A preview build at all.** DESIGN and CONTRIBUTING said the page gets no write path, in any build. This branch was asked for, and it changes those sentences to "the page as it ships". If the maintainer does not want a build of the page that can write to exist in the repository, the branch is not merged; nothing in it changes the page that ships.
2. **A transport of the page's own, not nusb's with a wait added** (§3). The preview build uses it for every look at the phone.
3. **The shipping script's extension point** (`additions`, `page` in `web/static/app.js`). The alternative was a second copy of the page's drawing code.
4. **The core's loop was refactored** into `install::UserdataFeed`, so that the page has no copy of it, and the tool's 64 MiB became `install::SEGMENT_CAP`. The tool's behaviour is the same; the core's and the tool's tests pass unchanged.
5. **Every file is checked before the session is opened,** not after as in the tool (which opens the writing session, then checks the files before the first `flash:`). Here a file that is not the release's is found before the phone is asked anything at that step, and the flow stays where it is, so the owner can pick the right file; the tool stops the install.
6. **One more click than the tool.** After the codename is taken, the write's screen is drawn with a button, "Write to the phone now". The codename is the core's condition; the click is the page's, and it is what lets the screen that says "don't unplug the phone" be read before the writes begin.
7. **The page's own sentences** are about files only ("boot.img is not among the files you picked"), never about the phone.
8. **A transport error before a session is open is not a stop.** `Flow::begin_write` returns such an error without stopping (the tool then exits). The page shows it under the screen, which stays on the write step, and the button can be pressed again. Nothing was written at that point.
9. **The wake lock and the question before closing the tab** (§6).
10. **Where the builds go** (`web/dist-preview`, `web/dist-preview-test`), and `web/serve.py --dir`.

Added with the page's download (2026-11-13):

11. **The phone is chosen on the page before anything is fetched** (a list of codenames the release server has releases for, the Pixel 7 Pro today), because the flow begins from a release and the phone is looked at only after the warning. A wrong choice ends at the flow's own stop for another model.
12. **The pin is the primary key's fingerprint, as in the tool,** and the certificate comes from the release. Pinning the whole certificate in the page would also pin the signing subkey, which expires on 2028-10-26 and would then need a new page; the fingerprint survives a renewed or replaced subkey.
13. **The large files are downloaded on the owner's click,** not when the page opens; the small files are fetched when it opens.
14. **Parts of 32 MiB,** five tries a file, a 60 s stall timer, 256 MiB of quota to spare: numbers chosen here, none measured in a real browser.
15. **Kept files are checked again on every visit,** not trusted from their record.
16. **A download is never trusted to `latest.json`:** it is followed only to the directory beside it, and its digest of `manifest.json` only gives an earlier message. The server could still point to an older signed release; nothing here refuses one older than the last seen (there is no signed index of versions to compare with).
17. **The shipping page no longer reads `release/`** from its own origin; that directory stays in the builds for the self-test's look at the published release. The attended run below takes the release from the server or from a saved release.
18. **Each step's button waits for the one before it** (the panel's download for the release's check, the preview's dry run for the files). The page's run in a browser checked that a press without the files gives a message; it now checks that the button cannot be pressed.
19. **The phone picked again** after the browser lost it, by the next step's button, without a sentence of the page's own about the phone (DESIGN §7, rule 1).

**The shipping module, compared.** `web/build.sh` was run at the commit this branch started from and at its head, on one computer with one toolchain. The glue (`omamobile_installer_web.js`) is the same file, by SHA-256. The module (`omamobile_installer_web_bg.wasm`) has the same size and differs in 13 bytes, all in the table of source positions it keeps for its panic messages: they are line numbers in `web/src/lib.rs`, which moved because lines were added above them (the `cfg` lines and the comment for the preview). No instruction and no string differs. It is not a claim of reproducible builds.

## 8. The first attended run in a browser, against a real Pixel 7 Pro: runbook

Status: **not run.** Written offline, with no phone and no real browser. Results go in §9, with the date and the commit.

**What this run can show, and what it cannot.** The Pixel 7 Pro's bootloader tells one slot's version, the page cannot take the owner's statement for the other, so the flow stops at the first look. Steps 1 to 6 are therefore the whole run that is possible today, and **all of them only read.** They answer whether WebUSB in a real browser carries fastboot to this phone through both transports. Steps 7 to 9 are written down so that they exist when a decision makes them reachable; **they cannot be reached with this build on this phone, and nothing in this runbook works around that.**

**Before this run:** the command-line tool's first run ([WRITE-RUNBOOK.md](WRITE-RUNBOOK.md)) has been made and its results recorded. A browser must not be the first thing that sends this code's commands to a real bootloader.

The phone is the maintainer's test Pixel 7 Pro, running Omarchy Mobile from slot B. The maintainer runs every step, or approves it, at the keyboard. No identifier goes into this file or anything committed: no serial number, no `getvar all` output, no screenshot of the browser's chooser.

| Placeholder | What it is |
| --- | --- |
| `<RELEASE_DIR>` | a signed release for `cheetah` that carries `factory-builds.json`, with its `images/` and its root image |
| `<OMARCHY_MOBILE>` | the omarchy-mobile checkout |
| `<NV_BACKUP_DIR>` | the radio backup, outside every repository |

### 0. Builds and checks (no phone)

```sh
cargo build --release --locked -p omamobile-installer-cli
web/build.sh
web/build.sh web-write-preview
tools/check-shipping --built --preview
web/preview-test.sh
web/fetch-test.sh
```

Both pages take the release from the release server (the latest for the chosen phone) or from `<RELEASE_DIR>`, picked in their release panel; nothing is copied into a build.

- **Pass:** every line of `check-shipping` is `ok`; the node tests and the page's run pass. Record `git rev-parse --short HEAD`.
- **Stop:** any `not ok`, any failing test.
- **Rollback:** none needed. The copies are in directories git ignores.
- **Risk:** none. Nothing here opens a USB device; the tests' browser has WebUSB off.

### 1. The radio backup still matches (read-only, the phone running Omarchy Mobile)

As WRITE-RUNBOOK step 0: `tools/nv-backup --check <NV_BACKUP_DIR>` in `<OMARCHY_MOBILE>`.

- **Pass:** every checksum matches. **Stop:** any mismatch, or no backup.
- **Rollback:** none needed. **Risk:** none.

No step of this run writes. The check is here because the rule is to make it before work on a phone's bootloader interface, and because this is the last moment with SSH.

### 2. Each page loads and verifies the release (no phone)

Two terminals, then Chromium (an ordinary install, not one packaged so that it cannot see USB devices), one tab each:

```sh
web/serve.py 8765                      # the page that ships:   http://127.0.0.1:8765/
web/serve.py 8766 --dir dist-preview   # the write preview:     http://127.0.0.1:8766/
```

For each tab: either the page's release panel shows the server's latest release, or pick `<RELEASE_DIR>`'s folder (or its `.tar`) in the panel. Optionally, on 8765, "Download and check the release" (3.4 GB into the browser; record the time, the quota the page reports, what `persist()` answered, and the tab's memory in the browser's task manager while the root image is checked), then reload the page and "Check the downloaded release": nothing is downloaded again.

- **Expect:** on 8765, the banner "This is a preview. It cannot write to a phone …", then "loaded (this page cannot write to a phone)" and the release's version, "signed by" the release key, and the screen "Read this once". On 8766, the banner "This is a write preview … this build can write to a phone … It has never been run against a real phone.", "loaded (a write preview: this build can write to a phone)", the same release line and the same first screen.
- **Stop:** a stop screen instead of the warning (`release-not-verified`, `no-build-index`, `build-index-refused`, `writes-not-for-this-device`): the release is not one this run can use. Either page saying what the other should.
- **Rollback:** none needed. **Risk:** none: before the warning is accepted the flow looks at no phone, and nothing is plugged in.

### 3. The phone in Fastboot Mode, and its state before (read-only)

As WRITE-RUNBOOK step 2, word for word: stop the modem, `bin/omamobile fastboot`, then the `getvar` loop with Android's `fastboot` tool, which this runbook uses for `getvar` only.

- **Pass:** one phone; `product: cheetah`; `unlocked: yes`; `current-slot: b`. Write the values down, without the serial number.
- **Stop:** more than one phone; anything else than those values.
- **Rollback:** `fastboot reboot` starts Omarchy Mobile again. **Risk:** none.

Close nothing and leave no `fastboot` command running: one program at a time can claim the interface.

### 4. The page that ships looks at the phone (read-only)

In the tab on 8765: "I have read this. Go on"; tick "slot B already holds Omarchy Mobile (a reinstall)", which starts the flow over, so accept the warning again; "Choose the phone"; in the browser's chooser pick the one entry and "Connect".

- **Expect:** the chooser lists exactly one device. Then the stop **"The installer can't check the phone's second bootloader copy"** (`bootloader-slots-unknown`), ending "The installer only asked the phone what it is. It has written nothing to it." and "Starting the installer again as things are will stop here again."
- **Pass:** that stop, within a few seconds. It means: the filter matched the bootloader's interface, the browser opened and claimed it, `getvar:serialno` equalled the serial number the browser reported, and every `getvar` including `getvar:all` was answered and read.
- **Stop, and record the words:** an empty chooser (the browser cannot see the device: permissions, or a sandboxed browser); a red line under the screen starting "USB:" (the transport failed: copy it); the page showing nothing for more than a minute (this transport has no wait: close the tab, and do not go on to step 5 before the phone answers `fastboot getvar product` again); "the phone that answered is not the one that was chosen"; any other stop (`runs-from-slot-b` means the box was not ticked; `wrong-model`, `baseband-unknown`, `build-not-listed`, `build-not-tested` are the flow's findings about this phone and this release: record them, they hold for the tool too). A screen that goes on past the phone step would mean this bootloader reports both slots: stop and record it, it changes what this document assumes.
- **Rollback:** none needed. Close the tab; the browser releases the device.
- **Risk:** low. The flow before a dry run has no session; its client's list of what may be flashed is empty, so the gate passes `getvar:` and the one unlock question (not asked of an unlocked phone) and nothing else. It is the first time this page talks to a real bootloader.

### 5. The write preview looks at the phone (read-only)

Close the tab of step 4 first. In the tab on 8766, the same clicks.

- **Expect and pass:** the same stop, with the same words. The page offers "Start over" and nothing else: no file picker, no field for the codename.
- **Stop:** as step 4. In addition: a line starting "USB: … timed out" (the transport's wait ran out: 300 s), or any difference from what step 4 showed (the two transports asked the same questions of the same phone).
- **Rollback:** none needed. Close the tab.
- **Risk:** low, and the same as step 4's for the same reason: this build can open a session that writes only from the write step, which comes after the dry run, the files and the codename, and this flow stopped at the first look. The preview's own transport meets a real device here for the first time.

Optional, and still read-only: "Start over", accept, and while the chooser is open pull the cable; the chooser empties; "Cancel" gives "no phone was chosen" under the screen. Put the cable back and look again: the same stop as before.

### 6. The phone's state after, and back to Omarchy Mobile (read-only)

The `getvar` loop of step 3 again, then `fastboot reboot`.

- **Pass:** every value as in step 3; Omarchy Mobile starts from slot B. `tools/nv-backup --check <NV_BACKUP_DIR>` matches.
- **Stop:** any value changed (nothing in steps 4 and 5 can change one: find out what did before anything else is run against this phone).
- **Rollback:** none needed. **Risk:** none.

**The run ends here today.** Record in §9: the browser and its version, the two stops, how long each look took, and anything the browser's own pages showed about the device.

### What stands between step 6 and step 7

A decision, which is the maintainer's and is not made here, on how a Pixel 7 Pro passes `bootloader-slots-unknown` in a browser. The ways there are: the owner's statement taken by the page (which reverses "command line only"); the other slot's bootloader measured, so that no statement is needed (Omarchy Mobile's plan, D4); or a phone that reports both slots, once the write path accepts one. With none of them, the steps below cannot be reached, and a build changed locally to reach them is not this runbook.

### 7. The dry run in the browser (read-only; not reachable today)

In the preview's tab, after the look: the file the owner saved from Google's page, the answers, then on "The dry run" pick the release's files (`<RELEASE_DIR>/images/*.img` and the `.img.zst`) and "Run the dry run".

- **Expect:** "Checking …" for each file (the root image takes its hashing time), then the pieces counted as they are cut, then **"This is what the installer will write"**: the six partitions with sizes, `set_active:b`, `reboot`, what is erased.
- **Pass:** that screen; the list equal, line for line, to the one the tool's dry run gave for the same release and phone (WRITE-RUNBOOK step 3), the number of pieces included; step 3's `getvar` values unchanged.
- **Stop:** any stop screen; a different list; the tab's memory growing without bound (the browser's task manager).
- **Rollback:** none needed: a dry-run session's gate passes no `download:`, `flash:`, `set_active:b` or `reboot`.
- **Risk:** low. Record how long the two readings of the root image took.

### 8. Go or no-go, and the write (the one step that writes; not reachable today)

Go or no-go as WRITE-RUNBOOK step 4. In addition: the computer on mains power, its sleep and the browser's Energy Saver off, the preview's tab in a window of its own, in front, and nothing else to do on the computer until the phone restarts.

Type `cheetah`, "Confirm", read the write's screen, "Write to the phone now". Do not unplug the phone, press its keys, switch tabs or lock the screen until it restarts by itself.

- **Expect:** the progress line naming each boot image, then "userdata: … blocks read, N pieces written, the last flash answered in T s", then **"Omarchy Mobile is written, and the phone is restarting"**.
- **Pass:** that screen. Record the number of pieces, the longest flash and the whole time, beside the tool's numbers for the same release.
- **Stop:** any stop screen: it says how far the phone confirmed the writes. Leave the phone in Fastboot Mode, send it nothing, copy the stop's words, and go to WRITE-RUNBOOK step 8. A page that shows nothing new for more than 300 s after its last progress line, or a tab that crashed or was closed: there is no stop to read; treat `userdata` as partly written and go to WRITE-RUNBOOK step 8.
- **Rollback:** WRITE-RUNBOOK step 8 (`tools/install-guided --start-over …`), which writes every partition again whole. Slot A is not written by any step.
- **Risk:** medium, as every install: `userdata` is erased, slot B's boot partitions replaced. New here beyond the tool's run: the browser between the installer and the phone, and a tab that can be put to sleep or closed.

### 9. After the write (not reachable today)

WRITE-RUNBOOK steps 6, 7 and 8, unchanged: the slot state, what the phone shows with the cable in, and the reinstall with the shell installer, which ends the session.

## 9. Results

Not run yet. For each run: the date, the commit, the browser and its version, the operating system, each step's outcome in a line, and for steps 4 and 5 the stop's id and how long the look took.
