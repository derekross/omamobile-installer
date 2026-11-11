# The install flow: design

Status (2026-11-10): **built and tested against a pretend phone; never run against a real one.** The flow ends at the dry run. Nothing in this repository's front ends can write to a phone.

This is the design of the installer's flow: the steps of an install as one state machine in the core (`core/src/flow.rs`), which the command-line tool and the web page both drive. It follows Omarchy Mobile's [install experience plan](https://github.com/derekross/omarchy-mobile/blob/main/docs/INSTALL-EXPERIENCE-PLAN.md) (M3, M4) and the maintainer's decisions of 2026-11-10, listed in §2. How the shell-script installer does each step today is in that project's `docs/INSTALL-GUIDED.md`.

## 1. The shape

```
front end                         core (no I/O of its own)
---------                         ------------------------
fetches the release's files  -->  Flow::begin          verifies, checks the build index
draws Flow::screen()         <--  Screen               title, text, steps, link, file, problem, plan, footer
hands in what the person did -->  acknowledge_warning, read_phone, factory_zip, answers, dry_run
lends a transport            -->  getvar, and nothing else, reaches the phone
```

The core decides the order of the steps, every refusal and every sentence a person reads about the phone. A front end draws and listens. That is what keeps the two front ends from drifting, and what a third one (the desktop window) gets for free.

## 2. The decisions the flow carries

Made by the maintainer on 2026-11-10:

1. **Google's factory image is the owner's download.** The installer never downloads it and never shows Google's terms. It opens Google's page and takes the file the owner saved, checked by size and SHA-256 against the release's signed build index.
2. **The match is exact, and tested.** The build whose bootloader *and* baseband equal the phone's, marked tested, or the install stops. There is no nearest build.
3. **The installer never sends `fastboot flashing unlock`.** It shows the command and the steps on the phone.
4. **One warning, before the unlock steps,** acknowledged once.
5. **No slot switching.** Reading the other bootloader slot by making it active is off for every device unless its descriptor says `BOOTLOADER_SLOT_PROBE=yes`. None does. Where fastboot reports only the current slot's version (the Pixel 7 Pro), the install stops and explains.
6. **Every step is tied to one phone,** pinned by its serial number in fastboot.
7. **Only slot B's boot images and `userdata` are ever written, nothing before the release is verified, and unsigned bundles are not supported.** The flow adds no write at all.

## 3. The states

| State (`Screen.state`) | What the person sees | What the front end hands in | Goes to | Reaches the phone |
| --- | --- | --- | --- | --- |
| (start) | | `Flow::begin(release files, build index, trusted keys, options)` | `warning`, or a stop | nothing |
| `warning` | The one warning: the phone is erased (by the unlock, if it is locked); Omarchy Mobile replaces Android and what this release lacks; the way back | `acknowledge_warning`, or `decline_warning` | `phone`, or a stop | nothing |
| `phone` | Connect the phone in Fastboot Mode (how, from the descriptor's `INSTALL_KEYS_FASTBOOT`) | `read_phone(transport, chosen)`, or `no_phone` | `unlock`, `factory-image`, or a stop | `getvar` |
| `unlock` | The bootloader is locked: the steps on the phone and the command to type. The flow sends none of it | `read_phone` again, when the owner says it is done | `unlock` (still locked), `factory-image`, or a stop | `getvar` |
| `factory-image` | The build that matches, Google's page as a link, the file's name and size | `factory_zip(name, size)`, the file's bytes a piece at a time, `factory_zip_done` | `answers`, or stays with why the file wasn't taken | nothing |
| `answers` | User name, PIN or password, the phone's name, time zone, Wi-Fi | `answers(OwnerAnswers)` | `dry-run` | nothing |
| `dry-run` | What the dry run is | `dry_run(transport)`, then `dry_run_done(session)` or `dry_run_failed` | `done`, or a stop | `getvar` |
| `done` | The plan: what an install would write. Nothing was written | | (the end, in this version) | |
| `stopped` | Why, and what is true of the phone | | (the end: a new flow starts over) | |

A step handed in out of order is refused and changes nothing. Before the warning is accepted the flow refuses to look at a phone, so not even `getvar` is sent.

**Verifying comes first, and so does the build index.** `begin` checks the signature, the checksums, the manifest and the descriptor, and then the build index, before the warning. A release that can't be used stops there, not after the owner has erased a phone for it.

**Every stop comes before the unlock steps.** Each look at the phone judges it in one order: the model, the pinned phone, slot B, the two bootloader slots, the build index. Only a phone that passes all of them is shown how to unlock. This differs from the shell installer, which checks slots and the index after the unlock. It rests on a locked bootloader answering the same `getvar` questions as an unlocked one, which has not been checked on a phone (§9).

**The dry run judges the phone again**, then opens a `Session` with every write held back, pinned to the same phone. A front end that has the release's images (the command-line tool) walks them through that session: each is checked against the signed manifest and the root image is cut into sparse images, all without a write. One that doesn't (the web page, for now) gets the signed plan's list, and the last screen says which it was.

## 4. The stops

Each is a `StopKind` with a short id (`Screen.stop`), a title and a message.

| Id | When | Starting again |
| --- | --- | --- |
| `release-not-verified` | The signature, a checksum, the manifest or the descriptor did not hold | after fetching the release again |
| `no-build-index` | The release has no `factory-builds.json` | stops again |
| `build-index-refused` | The index isn't the signed file, or breaks a rule | after a new fetch (not the signed bytes); stops again (a rule) |
| `warning-declined` | The owner did not accept | at any time |
| `wrong-model` | `product` is not the release's codename | with the right phone |
| `phone-changed` | Another phone than the first one this install looked at | with one phone |
| `runs-from-slot-b` | The current slot is B and this is not a reinstall | stops again |
| `bootloader-slots-unknown` | Only the current slot's bootloader version is reported | stops again |
| `bootloader-slots-differ` | Both reported, and different | stops again |
| `baseband-unknown` | No `version-baseband` | stops again |
| `build-not-listed` | No build has both the phone's versions | after an Android update |
| `build-not-tested` | The build is listed, not marked tested | after an update, if a newer build was tested; else stops again |
| `locked-at-dry-run` | The bootloader is locked again at the dry run | after unlocking |
| `build-changed` | Other versions at the dry run than the factory image was chosen for | at any time |
| `dry-run-refused` | A check only the session makes (a partition's size, `max-download-size`), or the link was lost | stops again; after reconnecting |

A factory image that isn't taken (`ZipRefusal`: another name, another size, another SHA-256) is not a stop: the step stays open and says why.

### What a stop says

The M1 reviews of the shell installer found stops that reassured wrongly: "Nothing is lost. Run this command again" after a half-written phone and after stops that a rerun can't pass, and a footer that called the wrong one of two phones "the phone". So here:

- **The closing lines are derived, never fixed.** `Stop::footer()` is built from two facts: `PhoneFact` (no phone was asked; a phone was only asked what it is; the same after the unlock steps were shown, with "if you unlocked its bootloader, that erased the phone"; two phones, each only asked) and `Retry` (at any time; once the message's fix is done; stops here again).
- **A stop that a retry can't pass says so,** and its message says what has to change or who to ask. The slot stop hands out no way around it.
- **Advice comes with where it leads.** "Update Android" is followed by: Android then runs from slot B, and the installer will stop for that.
- **No advice that can't work.** An untested build with no newer tested build in the index is not told to update Android.
- **When this flow gains writes, `PhoneFact` gains their cases** (a write began and did not finish; written), and every stop then says which. That is the type to extend, not the text.

## 5. The build index

`factory-builds.json`, format 1 (`core/src/build_index.rs`; the format is Omarchy Mobile's, from its `tools/factory-index`).

- **Every field is checked:** the build id, both versions, the SHA-256, a size above zero; the zip named `<codename>-<build>-factory-<first eight digits of its SHA-256>.zip`; `url` on `https://dl.google.com/` (and ending in the zip's name), with no character that could name another host; `page` Google's factory images page on `developers.google.com`; `tested` and `tested_with` together, the latter a release version; no build twice; the index's codename the release's. An unknown key is refused, as in the manifest's install plan: it could be a rule this version would ignore.
- **Trusted only through a verified release.** `VerifiedRelease::build_index` takes the bytes, checks them against the signed manifest's size and SHA-256 for `factory-builds.json`, then parses. Nothing else makes a `BuildIndex`, and the flow takes nothing else.
- **The match** is `BuildIndex::lookup(bootloader, baseband)`: `Tested` (used; other builds with the same pair are named, since fastboot can't tell carrier variants apart), `NotTested`, `NotListed`. Among builds with the same pair the tested one is used wherever it is listed.
- **`url` is never fetched.** `page` is a link the person clicks.

Not built: the newer copy published beside `latest.json` with a detached signature (INSTALL-GUIDED §5). `signature::verify_detached` can check one; what is open is binding it to a release and its replay (a signed index never expires).

## 6. The bootloader's two slots

Setting slot B active runs slot B's bootloader, so both slots' versions must match. The rule, in `Flow::judge`:

- both reported and equal: go on;
- both reported and different: stop;
- only the current slot's reported: **stop.** The flow never takes the owner's word for the other slot (`PhoneOptions::bootloader_slots_confirmed` is always `None` on this path, and the command-line option that set it is gone), and never switches slots.

`BOOTLOADER_SLOT_PROBE` in `device.conf` is parsed (`yes` or `no`; anything else fails verification) and carried as `VerifiedRelease::rules().bootloader_slot_probe`. When it is `yes` the stop says the release allows the probe and this version does not do it. **Nothing acts on the key in this version.** The core has no command that could: no `set_active:a`, no `reboot-bootloader`.

### What `manifest.json` would need to carry

For a later version to probe, the rule has to be in the signed install plan, not only in the descriptor:

- **`install.bootloader_slot_probe`** (true or false), written by the bundle tool from the descriptor's key. This version already reads it and refuses a release whose manifest and descriptor disagree. A version of this installer from before this change refuses a manifest with the key at all (the install plan is closed), so a release must not write it until the installers in use read it.
- **The probe's steps, named in the plan,** as `install.after` names `set-active b` and `reboot` today: for example an `install.before` of `["probe-other-bootloader-slot"]`. The plan is a closed list the core checks word for word, so the probe can't be an unlisted side effect.
- **What the probe leaves behind.** It makes the other slot active, restarts the bootloader, reads `version-bootloader`, and switches back. On a phone whose bootloader counts boot tries per slot or clears a slot's "successful" mark on `set_active` (Omarchy Mobile's AGENTS.md records both for the Pixel 9a), that costs something. The descriptor is where a device's maintainers say it is safe, which is why the default is off.

## 7. What a front end must do

1. **Draw the whole `Screen`:** title, body, numbered steps, a step's `command` as text to copy, the link, the file wanted, the problem, the plan, the footer. Add no sentence of its own about the phone's state, and no reassurance.
2. **Never run a `command`.** It is for the person's terminal.
3. **Send nothing to a phone itself.** It lends the flow a transport (`read_phone`, `dry_run`) and passes which device the person chose (`PhoneId::of_serial` of the USB serial number), so the phone that answers must be that one.
4. **Open Google's page only on the person's click,** as an ordinary link in a new tab. Fetch nothing from Google.
5. **Read the factory zip in pieces** into `FactoryZipCheck::update`; never hold it whole. Hand over the name and size first.
6. **Keep the owner's answers in memory only.** `OwnerAnswers::check` applies the rules (those of Omarchy Mobile's `tools/install-phone`); the flow keeps a summary without the secrets or the network's name. A front end may add what only it knows (which time zones exist).
7. **Say what it is.** The page's first line says it is a preview that cannot write to a phone.
8. **Offer the pretend phone only in builds made for it** (the command-line tool's `fake-phone` feature, the page's `selftest` feature). Those are also the only builds that trust a test key.

How the two do it: `cli/src/main.rs` (`run_flow`) and `web/static/app.js` with `web/src/lib.rs` (`InstallFlow`).

## 8. Pinning the phone

The first phone of the right model the flow looks at becomes the install's phone (`PhoneId`, the SHA-256 of its fastboot serial; the core holds the serial itself nowhere but in an open `Session`). Each later look and the dry run compare against it, and the dry run's `Session` reads the serial again before each (held back) write.

A later version writes, and then has steps that talk to the phone some other way (USB networking, the install link). A serial pinned in fastboot says nothing about who answers there. The shell installer's answer is to ask the phone for the digest of the serial its bootloader handed the kernel, before every such step, and to stop unless it equals the pinned one. `PhoneId` is that digest, so the same check fits; those steps must take a `PhoneId` and refuse without a match.

## 9. What is open

Not built, in the order a real install needs them:

- **A run on a real phone.** Unknown until then: how a Pixel's bootloader answers `getvar:all`; whether a locked bootloader answers `current-slot`, `version-baseband` and the per-slot versions (§3 depends on it); `max-download-size`; timing. The first run should be `install --dry-run`, which sends `getvar` only. On a Pixel 7 Pro it is expected to stop at `bootloader-slots-unknown`.
- **Whether a locked phone can be unlocked.** `fastboot flashing get_unlock_ability` would tell whether "OEM unlocking" is on, but it is a `flashing` command, and the core has none (CONTRIBUTING). So the unlock screen gives the steps for both cases.
- **Restarting an Android phone into Fastboot Mode** (adb). The phone screen gives the key combination.
- **How the owner unlocks from the web page.** The command needs Android's fastboot tool on the computer, which the plan says a person should never have to install.
- **Delivering the owner's answers** (`firstboot.conf`): over the install link (plan M2) or written into the image. Until then no front end may write.
- **The install link** and every step after the write: the first start, the radio backup, firmware, the restart, the check.
- **Real writes**, the confirmation before them, and a record of how far an install got, so a second run goes on instead of starting over.
- **The web page's downloads:** the release's images and root image, with resume and storage, so the dry run there can walk them.
- **The desktop window** (a web view around the same page, with the native core behind it), and **Windows and macOS** (type-checked only; drivers, signing).
- **The published build index** with a detached signature (§5).
- **Timeouts in the browser** for a phone that stops answering.

## 10. Tests

- `core/tests/flow.rs`: each state and each stop against the pretend phone; each test also checks that only `getvar` reached it.
- `core/tests/build_index.rs`: every field's rule on the real cheetah index (`fixtures/index/`) and on changed copies; the match; trust.
- `cli/tests/flow_fake_phone.rs` (`--features fake-phone`): the built tool with typed input.
- `web/selftest.sh`: the page in a headless Chromium, clicked through every state against the pretend phone, under the strict Content-Security-Policy. The browser is started with WebUSB off and, where bubblewrap is installed, without USB device nodes.

A check counts only if it fails when what it guards is removed. For this milestone that was tried by hand, one change at a time; the pull request lists the changes and what caught each.
