# The install flow: design

Status (2026-11-12): **built and tested against a pretend phone; never run against a real one.** The flow ends at the dry run. Nothing in this repository's front ends can write to a phone.

This is the design of the installer's flow: the steps of an install as one state machine in the core (`core/src/flow.rs`), which the command-line tool and the web page both drive. It follows Omarchy Mobile's [install experience plan](https://github.com/derekross/omarchy-mobile/blob/main/docs/INSTALL-EXPERIENCE-PLAN.md) (M3, M4) and the maintainer's decisions of 2026-11-10 and 2026-11-12, listed in §2. How the shell-script installer does each step today is in that project's `docs/INSTALL-GUIDED.md`.

## 1. The shape

```
front end                         core (no I/O of its own)
---------                         ------------------------
fetches the release's files  -->  Flow::begin          verifies, checks the build index
draws Flow::screen()         <--  Screen               title, text, steps, link, file, problem, plan, footer
hands in what the person did -->  acknowledge_warning, read_phone, factory_zip, answers, dry_run
lends a transport            -->  getvar and one read-only question reach the phone, nothing else
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

Made by the maintainer on 2026-11-12:

8. **One `flashing` command is allowed, and it only reads: `flashing get_unlock_ability`.** A locked phone that passes every stop is asked whether it would take the unlock command, and the unlock step says what it answered: "OEM unlocking" is already switched on, or it still has to be switched on in Developer options. A phone that doesn't answer, or answers something else than a plain yes or no, gets the steps for both cases, as before, and the install goes on. Nothing else of the `flashing` or `oem` families is sent, and decision 3 stands: the installer never sends the unlock command (§3, "The unlock question").
9. **The maintainer can let a dry run go on past a phone that reports one bootloader slot's version.** The override is for the maintainer's own first dry runs on such a phone (the Pixel 7 Pro). It is hidden, it works in a dry run only, it passes that one stop and no other, and every screen from then on says that the check was skipped and that the run is not evidence that an install would be allowed. Decision 5 stands for everyone else, and for every install (§6, "The maintainer's override").

## 3. The states

| State (`Screen.state`) | What the person sees | What the front end hands in | Goes to | Reaches the phone |
| --- | --- | --- | --- | --- |
| (start) | | `Flow::begin(release files, build index, trusted keys, options)` | `warning`, or a stop | nothing |
| `warning` | The one warning: the phone is erased (by the unlock, if it is locked); Omarchy Mobile replaces Android and what this release lacks; the way back | `acknowledge_warning`, or `decline_warning` | `phone`, or a stop | nothing |
| `phone` | Connect the phone in Fastboot Mode (how, from the descriptor's `INSTALL_KEYS_FASTBOOT`) | `read_phone(transport, chosen)`, or `no_phone` | `unlock`, `factory-image`, or a stop | `getvar`; then, if the next screen is `unlock`, `flashing get_unlock_ability` |
| `unlock` | The bootloader is locked: whether "OEM unlocking" is switched on, if the phone said; the steps on the phone and the command to type. The flow sends none of it | `read_phone` again, when the owner says it is done | `unlock` (still locked), `factory-image`, or a stop | `getvar`; then, if still locked, `flashing get_unlock_ability` |
| `factory-image` | The build that matches, Google's page as a link, the file's name and size | `factory_zip(name, size)`, the file's bytes a piece at a time, `factory_zip_done` | `answers`, or stays with why the file wasn't taken | nothing |
| `answers` | User name, PIN or password, the phone's name, time zone, Wi-Fi | `answers(OwnerAnswers)` | `dry-run` | nothing |
| `dry-run` | What the dry run is | `dry_run(transport)`, then `dry_run_done(session)` or `dry_run_failed` | `done`, or a stop | `getvar` |
| `done` | The plan: what an install would write. Nothing was written | | (the end, in this version) | |
| `stopped` | Why, and what is true of the phone | | (the end: a new flow starts over) | |

A step handed in out of order is refused and changes nothing. Before the warning is accepted the flow refuses to look at a phone, so not even `getvar` is sent.

**Verifying comes first, and so does the build index.** `begin` checks the signature, the checksums, the manifest and the descriptor, and then the build index, before the warning. A release that can't be used stops there, not after the owner has erased a phone for it.

**Every stop comes before the unlock steps.** Each look at the phone judges it in one order: the model, the pinned phone, slot B, the two bootloader slots, the build index. Only a phone that passes all of them is shown how to unlock. This differs from the shell installer, which checks slots and the index after the unlock. It rests on a locked bootloader answering the same `getvar` questions as an unlocked one, which has not been checked on a phone (§9).

**The unlock question.** `flashing get_unlock_ability` asks a phone whether it would take an unlock command, which is what "OEM unlocking" in Android's Developer options decides. It reads; it changes nothing on the phone. It is the one command of the `flashing` family in the crate, as a named operation with a typed answer (`fastboot::unlock_ability`, giving `UnlockAbility::Allowed`, `NotAllowed` or `Unknown`), not a way to send a command of the caller's choosing. The command layer holds a closed list of what may leave (`getvar:`, `download:`, `flash:`, `set_active:b`, `reboot`, and this command compared whole), checked at the one place a command is sent; `flashing unlock`, `flashing lock`, `flashing unlock_critical` and every `oem` command are refused there, and a test tries each.

The flow asks it of a locked phone only, only once that phone has passed every stop and is about to be shown the unlock steps, and as the last question of that look. So a phone the install can't go on with is never asked, and neither is an unlocked one. The answer changes the unlock step's words and nothing else:

- yes (`get_unlock_ability: 1`): "The phone says that "OEM unlocking" is already switched on, so it will take the unlock command." The step that switches it on is left out.
- no (`get_unlock_ability: 0`): "The phone says that "OEM unlocking" is not switched on yet. Until it is, the phone refuses the unlock command, so switch it on in Android's Developer options first (step 1)."
- anything else (FAIL from a bootloader that doesn't know the command, another reply, both answers at once, no reply): nothing is concluded, and the screen gives the steps for both cases, as it did before this question existed. It is never an error and never a stop.

The form of the answer (an `INFO` line `get_unlock_ability: 0` or `1`) is the one Google's bootloaders are expected to give; it has not been checked on a phone (§9). An answer in another form falls back to the text for both cases, so a wrong guess here costs the sentence, not the install.

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

### The maintainer's override

Decided on 2026-11-12, because the rule above stops the maintainer's own Pixel 7 Pro before its first dry run on real hardware can show anything else.

`FlowOptions::maintainer_skip_bootloader_slot_check` lets the flow go on past `bootloader-slots-unknown`. What holds it in:

- **A dry run only.** This flow ends at the dry run, so the override can't reach a write here. The session the dry run opens carries it (`PhoneOptions::maintainer_skip_bootloader_slot_check`), and `Session::connect` refuses it with a typed error, `Error::OverrideOutsideDryRun`, unless the session holds every write back: before a single question is sent to the phone, whatever the phone would have reported. A session whose check was skipped also refuses at the last gate before a write is sent. A version of the flow that writes must refuse the override the same way before its first write.
- **That stop and no other.** Two versions that are known to differ still stop (`bootloader-slots-differ`), at the first look and at the dry run, and so does everything else.
- **Hidden.** On the command line it is `--maintainer-skip-bootloader-slot-check`, for `install --dry-run` and `flow`; it is not in the usage text, and `install` without `--dry-run` refuses it from the arguments alone, before any USB device is looked for. The web page has no control for it and the shipping module no way to set it; only the page's self-test sets it, in a self-test build, against the pretend phone. No stop's text mentions it.
- **Said, in the core's words, on every screen from the moment a check was skipped** (`Screen.notice`, which both front ends draw right under the title and set apart): "The bootloader-slot check was skipped by a maintainer override. This phone tells the version of only one of its two bootloader copies, and the installer went on without knowing the other. This run is not evidence that an install would be allowed." The last screen's title is "The dry run finished with one check skipped. Nothing was written", not "The dry run passed", and the plan records it (`DryRunPlan::slot_check_skipped`). A locked phone's unlock screen says its second bootloader copy was not checked, where it otherwise says the two match.
- **Nothing is said when nothing was skipped.** On a phone that reports both slots the override changes nothing, and no screen mentions it.

It is not a way to install on such a phone. That still needs the probe below, or another way to know slot B's bootloader.

### The slot probe

`BOOTLOADER_SLOT_PROBE` in `device.conf` is parsed (`yes` or `no`; anything else fails verification) and carried as `VerifiedRelease::rules().bootloader_slot_probe`. When it is `yes` the stop says the release allows the probe and this version does not do it. **Nothing acts on the key in this version.** The core has no command that could: no `set_active:a`, no `reboot-bootloader`.

### What `manifest.json` would need to carry

For a later version to probe, the rule has to be in the signed install plan, not only in the descriptor:

- **`install.bootloader_slot_probe`** (true or false), written by the bundle tool from the descriptor's key. This version already reads it and refuses a release whose manifest and descriptor disagree. A version of this installer from before this change refuses a manifest with the key at all (the install plan is closed), so a release must not write it until the installers in use read it.
- **The probe's steps, named in the plan,** as `install.after` names `set-active b` and `reboot` today: for example an `install.before` of `["probe-other-bootloader-slot"]`. The plan is a closed list the core checks word for word, so the probe can't be an unlisted side effect.
- **What the probe leaves behind.** It makes the other slot active, restarts the bootloader, reads `version-bootloader`, and switches back. On a phone whose bootloader counts boot tries per slot or clears a slot's "successful" mark on `set_active` (Omarchy Mobile's AGENTS.md records both for the Pixel 9a), that costs something. The descriptor is where a device's maintainers say it is safe, which is why the default is off.

## 7. What a front end must do

1. **Draw the whole `Screen`:** title, the notice (right under the title, set apart), body, numbered steps, a step's `command` as text to copy, the link, the file wanted, the problem, the plan, the footer. Add no sentence of its own about the phone's state, and no reassurance.
2. **Never run a `command`.** It is for the person's terminal.
3. **Send nothing to a phone itself,** and offer no control for the maintainer's override in a page or a usage text (§6). It lends the flow a transport (`read_phone`, `dry_run`) and passes which device the person chose (`PhoneId::of_serial` of the USB serial number), so the phone that answers must be that one.
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

- **A run on a real phone.** Unknown until then: how a Pixel's bootloader answers `getvar:all`; whether a locked bootloader answers `current-slot`, `version-baseband` and the per-slot versions (§3 depends on it); `max-download-size`; timing. The first run should be `install --dry-run`, which sends `getvar` and, to a locked phone, `flashing get_unlock_ability`. On a Pixel 7 Pro it is expected to stop at `bootloader-slots-unknown`; the maintainer's override (§6) takes a dry run past that stop, to see the rest.
- **How a real bootloader answers `flashing get_unlock_ability`.** The flow asks it since 2026-11-12 (§3). The pretend phone answers with an `INFO` line `get_unlock_ability: 0` or `1`; whether a Pixel's bootloader uses that form, and whether a locked one answers at all, is unknown until the run above. Any other answer gives the steps for both cases.
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

- `core/tests/flow.rs`: each state and each stop against the pretend phone; each test also checks that only `getvar` reached it, and for a locked phone at the unlock steps `flashing get_unlock_ability`, counted. The override's properties, one by one: it passes the one-slot stop in a dry run and says so on every screen, it passes no other stop, and a session that could write refuses it before anything is sent.
- `core/src/fastboot.rs` (unit tests, where the private command layer is): of the `flashing` and `oem` families only the one question is sent, nothing off the list of commands is sent, and an answer is a plain yes or no or nothing is concluded.
- `core/tests/build_index.rs`: every field's rule on the real cheetah index (`fixtures/index/`) and on changed copies; the match; trust.
- `cli/tests/flow_fake_phone.rs` (`--features fake-phone`): the built tool with typed input.
- `web/selftest.sh`: the page in a headless Chromium, clicked through every state against the pretend phone, under the strict Content-Security-Policy. The browser is started with WebUSB off and, where bubblewrap is installed, without USB device nodes.

A check counts only if it fails when what it guards is removed. For this milestone that was tried by hand, one change at a time; the pull request lists the changes and what caught each.
