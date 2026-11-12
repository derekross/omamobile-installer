# The install flow: design

Status (2026-11-12): **built and tested against a pretend phone; never run against a real one.** The flow ends at the dry run. Nothing in this repository's front ends can write to a phone.

This is the design of the installer's flow: the steps of an install as one state machine in the core (`core/src/flow.rs`), which the command-line tool and the web page both drive. It follows Omarchy Mobile's [install experience plan](https://github.com/derekross/omarchy-mobile/blob/main/docs/INSTALL-EXPERIENCE-PLAN.md) (M3, M4) and the maintainer's decisions of 2026-11-10 and 2026-11-12, listed in §2. How the shell-script installer does each step today is in that project's `docs/INSTALL-GUIDED.md`.

## 1. The shape

```
front end                         core (no I/O of its own)
---------                         ------------------------
fetches the release's files  -->  Flow::begin          verifies, checks the build index
draws Flow::screen()         <--  Screen               title, text, steps, link, file, problem, plan, footer
hands in what the person did -->  acknowledge_warning, read_phone, accept_google_terms, factory_zip, answers, dry_run
lends a transport            -->  getvar and one read-only question reach the phone, nothing else
fetches Google's factory zip <--  FetchRequest         the address from the signed index, where to go on from, the size
hands in each answer, the bytes -->  FactoryDownload   every redirect, the size and the SHA-256 are decided here
```

The core decides the order of the steps, every refusal and every sentence a person reads about the phone. A front end draws and listens. That is what keeps the two front ends from drifting, and what a third one (the desktop window) gets for free.

## 2. The decisions the flow carries

Made by the maintainer on 2026-11-10:

1. **Reversed on 2026-11-12 by decision 11.** The rule was: "Google's factory image is the owner's download. The installer never downloads it and never shows Google's terms. It opens Google's page and takes the file the owner saved." What stays of it: the file is checked by size and SHA-256 against the release's signed build index, and the owner saving the file from Google's page themselves is still a way through the step.
2. **The match is exact, and tested.** The build whose bootloader *and* baseband equal the phone's, marked tested, or the install stops. There is no nearest build.
3. **The installer never sends `fastboot flashing unlock`.** It shows the command and the steps on the phone.
4. **One warning, before the unlock steps,** acknowledged once.
5. **No slot switching.** Reading the other bootloader slot by making it active is off for every device unless its descriptor says `BOOTLOADER_SLOT_PROBE=yes`. None does. Where fastboot reports only the current slot's version (the Pixel 7 Pro), the install stops and explains.
6. **Every step is tied to one phone,** pinned by its serial number in fastboot.
7. **Only slot B's boot images and `userdata` are ever written, nothing before the release is verified, and unsigned bundles are not supported.** The flow adds no write at all.

Made by the maintainer on 2026-11-12:

8. **One `flashing` command is allowed, and it only reads: `flashing get_unlock_ability`.** A locked phone that passes every stop is asked whether it would take the unlock command, and the unlock step says what it answered: "OEM unlocking" is already switched on, or it still has to be switched on in Developer options. A phone that doesn't answer, or answers something else than a plain yes or no, gets the steps for both cases, as before, and the install goes on. Nothing else of the `flashing` or `oem` families is sent, and decision 3 stands: the installer never sends the unlock command (§3, "The unlock question").
9. **The maintainer can let a dry run go on past a phone that reports one bootloader slot's version.** The override is for the maintainer's own first dry runs on such a phone (the Pixel 7 Pro). It is hidden, it works in a dry run only, it passes that one stop and no other, and every screen from then on says that the check was skipped and that the run is not evidence that an install would be allowed. Decision 5 stands for everyone else, and for every install (§6, "The maintainer's override").
10. **A build that is listed, not tested, with no newer build tested, is "not tested yet".** The owner is told that nothing they can install today changes that, and what they can do: wait, or ask in the issue tracker. They are not told to update Android. It still stops: decision 2 does not change. When a newer build was tested, "update Android to that build" stays (§4).
11. **The installer downloads Google's factory image for the owner, from Google's own server, after showing Google's terms for the owner to accept.** This reverses decision 1. A step of the flow shows what Google's download page asks a person to accept, in Google's words and marked as Google's, and nothing is downloaded until the owner accepts there. Then the file is fetched from the one address the release's signed build index gives for the phone's build, on `https://dl.google.com/`, and checked on this computer (size, then SHA-256 over the whole file) before anything of it is used. **The project hosts none of Google's files:** nothing of Google's is fetched from anywhere but that address, and nothing of it is passed on. An owner who does not accept, or whose download can't be made, saves the file from Google's page and hands it in, as under decision 1. Whether Omarchy Mobile may ship files extracted from a factory image itself is a separate question, and it is not decided (§3, "Google's terms" and "The download").

## 3. The states

| State (`Screen.state`) | What the person sees | What the front end hands in | Goes to | Reaches the phone |
| --- | --- | --- | --- | --- |
| (start) | | `Flow::begin(release files, build index, trusted keys, options)` | `warning`, or a stop | nothing |
| `warning` | The one warning: the phone is erased (by the unlock, if it is locked); Omarchy Mobile replaces Android and what this release lacks; the way back | `acknowledge_warning`, or `decline_warning` | `phone`, or a stop | nothing |
| `phone` | Connect the phone in Fastboot Mode (how, from the descriptor's `INSTALL_KEYS_FASTBOOT`) | `read_phone(transport, chosen)`, or `no_phone` | `unlock`, `google-terms` (`factory-image` when the owner has the file or the front end can't fetch from Google), or a stop | `getvar`; then, if the next screen is `unlock`, `flashing get_unlock_ability` |
| `unlock` | First, as the screen's notice: this version cannot install, and there is no reason to unlock for it. Then: the bootloader is locked; whether "OEM unlocking" is switched on, if the phone said; the steps on the phone and the command to type. The flow sends none of it | `read_phone` again, when the owner says it is done | `unlock` (still locked), `google-terms`, or a stop | `getvar`; then, if still locked, `flashing get_unlock_ability` |
| `google-terms` | What Google's download page asks a person to accept, as Google's words; whose the files are; Google's page as a link | `accept_google_terms(now)`, or `decline_google_terms` | `factory-image` (to download, or for the owner's own file) | nothing |
| `factory-image` | The build that matches, the acceptance if it was given, Google's page as a link, the file's name and size | Accepted: `factory_download(have)`, the fetch, `factory_download_done`. Always: `factory_zip(name, size)`, the file's bytes a piece at a time, `factory_zip_done` | `answers`; stays with why the file wasn't taken or the download ended early; or a stop | nothing |
| `answers` | User name, PIN or password, the phone's name, time zone, Wi-Fi | `answers(OwnerAnswers)` | `dry-run` | nothing |
| `dry-run` | What the dry run is | `dry_run(transport)`, then `dry_run_done(session)` or `dry_run_failed` | `done`, or a stop | `getvar` |
| `done` | The plan: what an install would write. Nothing was written | | (the end, in this version) | |
| `stopped` | Why, and what is true of the phone | | (the end: a new flow starts over) | |

A step handed in out of order is refused and changes nothing. Before the warning is accepted the flow refuses to look at a phone, so not even `getvar` is sent.

**Verifying comes first, and so does the build index.** `begin` checks the signature, the checksums, the manifest and the descriptor, and then the build index, before the warning. A release that can't be used stops there, not after the owner has erased a phone for it.

**Every stop comes before the unlock steps.** Each look at the phone judges it in one order: the model, the pinned phone, slot B, the two bootloader slots, the build index. Only a phone that passes all of them is shown how to unlock. This differs from the shell installer, which checks slots and the index after the unlock. It rests on a locked bootloader answering the same `getvar` questions as an unlocked one, which has not been checked on a phone (§9).

**A flow that ends at the dry run says so before the unlock steps.** Unlocking erases the phone, and this version installs nothing afterwards, so an owner who followed the unlock screen would erase a phone for a list of what an install would write. The core therefore puts this on the unlock screen as its notice (`Screen.notice`, which both front ends draw right under the title and set apart, before the body and the steps): "This version of the installer cannot install Omarchy Mobile yet. It only checks the phone and lists what an install would write. Unlocking the bootloader erases the phone, and there is no reason to unlock it for this version: do not unlock the phone for it." The steps stay, introduced as what a version that can install will ask, not for now. The sentence is tied to the flow's own state (`Flow::ends_at_dry_run`, which is true of every flow in this version) and not to a front end or a build: a version of the flow that writes answers that question from its state, and a flow that will install shows the steps without the notice. With the maintainer's override on a locked phone, the notice holds both sentences, this one first.

**The unlock question.** `flashing get_unlock_ability` asks a phone whether it would take an unlock command, which is what "OEM unlocking" in Android's Developer options decides. It reads; it changes nothing on the phone. It is the one command of the `flashing` family in the crate, as a named operation with a typed answer (`fastboot::unlock_ability`, giving `UnlockAbility::Allowed`, `NotAllowed` or `Unknown`), not a way to send a command of the caller's choosing. The command layer holds a closed list of what may leave (`getvar:`, `download:`, `flash:`, `set_active:b`, `reboot`, and this command compared whole), checked at the one place a command is sent; `flashing unlock`, `flashing lock`, `flashing unlock_critical` and every `oem` command are refused there, and a test tries each.

The flow asks it of a locked phone only, only once that phone has passed every stop and is about to be shown the unlock steps, and as the last question of that look. So a phone the install can't go on with is never asked, and neither is an unlocked one. The answer changes the unlock step's words and nothing else:

- yes (`get_unlock_ability: 1`): "The phone says that "OEM unlocking" is already switched on, so it will take the unlock command." The step that switches it on is left out.
- no (`get_unlock_ability: 0`): "The phone says that "OEM unlocking" is not switched on yet. Until it is, the phone refuses the unlock command, so switch it on in Android's Developer options first (step 1)."
- anything else (FAIL from a bootloader that doesn't know the command, another reply, both answers at once, no reply): nothing is concluded, and the screen gives the steps for both cases, as it did before this question existed. It is never an error and never a stop.

The form of the answer (an `INFO` line `get_unlock_ability: 0` or `1`) is the one Google's bootloaders are expected to give; it has not been checked on a phone (§9). An answer in another form falls back to the text for both cases, so a wrong guess here costs the sentence, not the install.

**Google's terms** (decision 11). The step comes after every stop and the unlock, when the phone's build is known, and before the factory image. It shows:

- Google's words, quoted and marked as Google's (`Screen.quote`, which both front ends set apart from the installer's own text): "Downloading of the system image and use of the device software is subject to the Google Terms of Service" and "By continuing, you agree to the Google Terms of Service and Privacy Policy", with links to `https://policies.google.com/terms` and `https://policies.google.com/privacy`; and that Google's page also says the download and use "may also be subject to certain third-party terms of service, which can be found in Settings > About phone > Legal information". These are the sentences the maintainer gave with the decision. They are constants in `core/src/flow.rs`; they have not been compared with Google's page by this code, and they have to be kept the same as the page (§9).
- In the installer's own words, what Google's page says of the files: they are for use only on the owner's own Pixel, and Google does not allow them to be redistributed. So the installer downloads them straight from Google to this computer, for this phone, and Omarchy Mobile does not host them or pass them on.
- Google's page for this build (the index's `page`), as a link.

Nothing happens until the owner answers. Accepting is recorded in the flow, with the build and the time (`Flow::google_terms_accepted`, a `TermsAccepted`; the front end passes its clock, since the core has none), and the next screen shows it: "You accepted Google's terms for build … on 2026-11-12 at 14:03 UTC." It is kept for that run only and is never sent anywhere. Not accepting is not a stop: the flow goes on to the factory image with the owner's own download, and downloads nothing for the rest of that run. An owner who says up front that they already have the file (`FlowOptions::owner_has_factory_zip`, the command line's `--factory-zip FILE`) is not shown the step, because the installer then downloads nothing: the terms were Google's to ask on its page.

**A front end that cannot fetch from Google says so at the start, and is not shown the step either** (`FlowOptions::cannot_fetch_from_google`). The web page is that front end: Google's server sends no `Access-Control-Allow-Origin` header (measured on 2026-11-12, §7), so a browser lets no page read the file, and asking the owner to accept Google's terms for a download that cannot be made would have them accept twice, here and on Google's page, for nothing. With the capability stated, a phone that passes every stop goes from `phone` (or `unlock`) straight to `factory-image` with the owner's own download (`OwnerFileWhy::CannotFetch`), `Flow::factory_download` is refused, and the screen says why in the core's words: "A web page is not allowed to read files from Google's server, so this page cannot download the image for you. The installer's desktop app and its command-line tool can download it for you. Here you download the image from Google's page yourself, where Google asks you to accept its terms, and the installer checks the file you saved." The command-line tool does not state it, and downloads. The decision is the core's: a front end states what it can't do and draws what it is given.

**The download.** The core does no I/O, so the front end fetches and the core decides (`core/src/download.rs`). `Flow::factory_download(have)` is refused unless the terms were accepted for this build in this flow. It gives a `FactoryDownload`, and from then on:

1. The front end feeds in the bytes it kept of an earlier try (`kept`), from the file's first byte. The core hashes them like the bytes that arrive later, and takes no answer of the server before they are all in.
2. It asks what to fetch (`request`): a `FetchRequest` with the address, the byte to go on from (a `Range: bytes=N-` header when that is above zero) and the file's size. A `FetchRequest` can only be made in the core, from the `url` of the build in the signed index or from a redirect that passed the same rule. No address comes from what a person typed, from a front end, or from a server.
3. It carries out one GET without following redirects, and hands the status and headers to `response`. The answer is `Fetch` (a redirect to follow), `Body { at }` (read the body, which belongs at that byte of the file) or `End` (read nothing).
4. It feeds the body in pieces to `body`, which counts and hashes them and says when to stop reading.
5. It hands everything to `Flow::factory_download_done`, with what kept it from carrying the request out, if anything did. The answer says what to do with the bytes it kept: `Verified`, `Kept` or `Discard`.

What holds, each with a test:

- **`https` and the host `dl.google.com`, and nothing else.** One rule, `build_index::is_download_url`, for the index entry (§5), again where a download starts, and for every redirect. A redirect to another host, to plain `http`, or to an address with a port, a user name, a query or `..` is not followed. A client that can't be told not to follow redirects (a browser) says where the answer came from in the end, and that is checked by the same rule; the page's Content-Security-Policy is what holds each hop in between to that host.
- **The size is the index's,** before the transfer (a `Content-Length`, or a `Content-Range` total, that says otherwise ends the try before a byte of the body is read) and during it (the piece that would pass the file's size is refused whole, and reading stops).
- **The SHA-256 is over the whole file** before anything of it is used: the kept bytes first, then what arrived. A server that sends the whole file although the rest was asked for is fine: what was kept is dropped, and its part of the hash with it.
- **A file that is not the listed one stops the install** (`factory-download-mismatch`, §4), and the front end is told to delete every byte it kept of it.
- **A download that can't be made is not a stop and says nothing against the file.** A redirect off Google's host, too many redirects (five), a status that is neither the file nor a redirect, an answer that does not fit the request, or a browser that got no answer it may read: the step falls back to the owner's own download, with the reason in a sentence, and the installer downloads no more in that run.
- **A download that broke off stays open.** The screen says how many bytes had arrived, and it can be tried again; a front end that kept the bytes goes on from them.
- **Progress** is `FactoryDownload::progress()`: bytes in hand, of the file's size.

The address is on no screen. A front end learns it only from a `FetchRequest`.

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
| `build-not-tested` | The build is listed, not marked tested | after an Android update, if a newer build was tested; else stops again ("not tested yet") |
| `factory-download-mismatch` | What the installer downloaded from Google's server is not the file the signed index describes: the server said another size, sent more bytes than the file has, or the SHA-256 of the whole file is another. What was downloaded is deleted | at any time (the file is downloaded afresh) |
| `locked-at-dry-run` | The bootloader is locked again at the dry run | after unlocking |
| `build-changed` | Other versions at the dry run than the factory image was chosen for | at any time |
| `dry-run-refused` | A check only the session makes (a partition's size, `max-download-size`), or the link was lost | stops again; after reconnecting |

A factory image the owner hands in that isn't taken (`ZipRefusal`: another name, another size, another SHA-256) is not a stop: the step stays open and says why. That differs from a download on purpose. A person can pick the wrong file, and picks another. A file the installer fetched itself from the one address it trusts, complete, with another SHA-256, means that Google's file and the release's list do not agree, and trying again in the same run would only fetch it again.

Not stops either: Google's terms not accepted, a download that can't be made (`OwnerFileWhy::Unavailable`: the step falls back to the owner's own download), and a download that broke off (`DownloadTrouble`: the step stays open). §3, "The download".

`factory-download-mismatch` says: "The downloaded file is not the one this release lists", then what was wrong ("… the file is not what the release's signed list of Google's builds says: its SHA-256 is another than the one the list records for build …"), then "The installer uses nothing of that file, and what it had downloaded of it is deleted from this computer.", then that starting again downloads the file afresh and whom to tell if it stops there again.

### What a stop says

The M1 reviews of the shell installer found stops that reassured wrongly: "Nothing is lost. Run this command again" after a half-written phone and after stops that a rerun can't pass, and a footer that called the wrong one of two phones "the phone". So here:

- **The closing lines are derived, never fixed.** `Stop::footer()` is built from two facts: `PhoneFact` (no phone was asked; a phone was only asked what it is; the same after the unlock steps were shown, with "if you unlocked its bootloader, that erased the phone"; two phones, each only asked) and `Retry` (at any time; once the message's fix is done; stops here again).
- **A stop that a retry can't pass says so,** and its message says what has to change or who to ask. The slot stop hands out no way around it.
- **Advice comes with where it leads.** "Update Android" is followed by: Android then runs from slot B, and the installer will stop for that.
- **No advice that can't work.** An untested build with no newer tested build in the index is "not tested yet" (decision 10, 2026-11-12): the stop's title says so, and its message says that nothing the owner can install today changes that, that an Android update does not help, and what they can do, which is to wait until the build has been tested or ask in the issue tracker. It is not told to update Android, and it still stops. "Newer" is the index's own order, newest first: a tested build listed before the phone's. When there is one, the stop names it and says to update Android to it, with where that leads.
- **When this flow gains writes, `PhoneFact` gains their cases** (a write began and did not finish; written), and every stop then says which. That is the type to extend, not the text.

## 5. The build index

`factory-builds.json`, format 1 (`core/src/build_index.rs`; the format is Omarchy Mobile's, from its `tools/factory-index`).

- **Every field is checked:** the build id, both versions, the SHA-256, a size above zero; the zip named `<codename>-<build>-factory-<first eight digits of its SHA-256>.zip`; `url` on `https://dl.google.com/` (and ending in the zip's name), with no character that could name another host; `page` Google's factory images page on `developers.google.com`; `tested` and `tested_with` together, the latter a release version; no build twice; the index's codename the release's. An unknown key is refused, as in the manifest's install plan: it could be a rule this version would ignore.
- **Trusted only through a verified release.** `VerifiedRelease::build_index` takes the bytes, checks them against the signed manifest's size and SHA-256 for `factory-builds.json`, then parses. Nothing else makes a `BuildIndex`, and the flow takes nothing else.
- **The match** is `BuildIndex::lookup(bootloader, baseband)`: `Tested` (used; other builds with the same pair are named, since fastboot can't tell carrier variants apart), `NotTested`, `NotListed`. Among builds with the same pair the tested one is used wherever it is listed.
- **`url` is the one address anything of Google's is fetched from** (decision 11), and only through `Flow::factory_download`, after the owner accepted Google's terms. The rule for it is `is_download_url`, which the download applies again and to every redirect. `page` is a link the person clicks.

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
4. **Say at the start if it cannot fetch from Google's server** (`FlowOptions::cannot_fetch_from_google`): the flow then asks for no terms and gives out no download. **Open Google's page only on the person's click,** as an ordinary link in a new tab. **Fetch one thing from Google, and only what the core asks for:** the `FetchRequest` of a `FactoryDownload`, which the flow gives out only after the owner accepted Google's terms. Don't build an address, change one, or follow a redirect the core did not answer with `Fetch`. Send Google no cookie and no referrer. Draw `Screen.quote` set apart, as someone else's words.
5. **Read the factory zip in pieces,** never whole: a file the owner hands in into `FactoryZipCheck::update` (the name and size first), a download into `FactoryDownload::body`. Bytes kept of an earlier try go into `FactoryDownload::kept` first, all of them. Do with the kept bytes what `factory_download_done` answers: on `Discard`, delete them.
6. **Keep the owner's answers in memory only.** `OwnerAnswers::check` applies the rules (those of Omarchy Mobile's `tools/install-phone`); the flow keeps a summary without the secrets or the network's name. A front end may add what only it knows (which time zones exist).
7. **Say what it is.** The page's first line says it is a preview that cannot write to a phone.
8. **Offer the pretend phone only in builds made for it** (the command-line tool's `fake-phone` feature, the page's `selftest` feature). Those are also the only builds that trust a test key.

How the two do it: `cli/src/main.rs` (`run_flow`) and `web/static/app.js` with `web/src/lib.rs` (`InstallFlow`).

### The download in each front end

**The command-line tool** fetches with `ureq` (a small blocking HTTP client; TLS by rustls with the webpki roots; no async runtime, which the tool does not otherwise have). It follows no redirect by itself and treats no status as an error: the core sees each answer. The file goes to `factory/<name>.part` under the work directory (`--work-dir DIR`; by default the user's cache directory, `omamobile-installer`), and is renamed to `<name>` once the core says `Verified`. A later run finds either file, reads it back into the core from its first byte and asks Google for the rest only. `--factory-zip FILE` hands in a file the owner already has, and nothing is downloaded. `flow` (the pretend phone) never reaches Google: its fetches go to a stand-in on this computer named with `--fake-google 127.0.0.1:PORT`, or nowhere; `install` refuses that option.

**The web page does not download it.** A page on another origin may read an answer only if the server sends `Access-Control-Allow-Origin` for it, and `dl.google.com` does not. So the page states `FlowOptions::cannot_fetch_from_google` at the start of every flow (`page_options` in `web/src/lib.rs`), the flow leaves the terms step out, and the owner saves the file from Google's page and picks it (§3, "Google's terms").

**What Google's server answers, measured on 2026-11-12** with one `HEAD` request and one 16-byte range request for the tested Pixel 7 Pro build (recorded in pull request #1; no code or test in this repository sends a request to Google):

| Question | Answer |
| --- | --- |
| Redirects? | No: `200` straight from `dl.google.com` |
| Size | A `Content-Length` equal to the size in the build index |
| Resume | `Accept-Ranges: bytes`; a range request is answered `206` |
| May a web page read it? | No: no `Access-Control-Allow-Origin` header, with an `Origin` sent |

So the command-line tool's download and its resume fit the real server, and a browser can never read the file. Google can change any of this; the page's capability is a statement about the server as measured, and it is the line to change if the server ever allows the read.

**The page's download code is kept** for a flow that does offer a download, which today only its self-test begins, against a stand-in. There it fetches with the browser's `fetch` (no cookie, no referrer), streams the body into the core and keeps none of it: there is no resume, and a try starts at the file's first byte. A browser follows redirects itself, so the page hands the core the address the answer came from in the end, and the site's Content-Security-Policy (`connect-src … https://dl.google.com`, as `web/serve.py` sends it) is what keeps every hop on that host. When `fetch` is rejected before any answer (which is all a page learns, of a refusal by the cross-origin rule and of a network error alike), the core is told `FetchFailure::NoAnswer` and the step falls back to the owner's own download, with the reason: "this browser got no answer from dl.google.com that a web page may read. A browser does not tell a page why: either there is no connection to Google's server, or Google's server does not let a web page read its files." The self-test runs both outcomes against a stand-in on another origin, one that sends the header and one that does not, and the page as it ships, which begins no download.

## 8. Pinning the phone

The first phone of the right model the flow looks at becomes the install's phone (`PhoneId`, the SHA-256 of its fastboot serial; the core holds the serial itself nowhere but in an open `Session`). Each later look and the dry run compare against it, and the dry run's `Session` reads the serial again before each (held back) write.

A later version writes, and then has steps that talk to the phone some other way (USB networking, the install link). A serial pinned in fastboot says nothing about who answers there. The shell installer's answer is to ask the phone for the digest of the serial its bootloader handed the kernel, before every such step, and to stop unless it equals the pinned one. `PhoneId` is that digest, so the same check fits; those steps must take a `PhoneId` and refuse without a match.

## 9. What is open

Not built, in the order a real install needs them:

- **A run on a real phone.** Unknown until then: how a Pixel's bootloader answers `getvar:all`; whether a locked bootloader answers `current-slot`, `version-baseband` and the per-slot versions (§3 depends on it); `max-download-size`; timing. The first run should be `install --dry-run`, which sends `getvar` and, to a locked phone, `flashing get_unlock_ability`. On a Pixel 7 Pro it is expected to stop at `bootloader-slots-unknown`; the maintainer's override (§6) takes a dry run past that stop, to see the rest.
- **How a real bootloader answers `flashing get_unlock_ability`.** The flow asks it since 2026-11-12 (§3). The pretend phone answers with an `INFO` line `get_unlock_ability: 0` or `1`; whether a Pixel's bootloader uses that form, and whether a locked one answers at all, is unknown until the run above. Any other answer gives the steps for both cases.
- **A whole download from Google's server by the tool** (decision 11). How the server answers was measured on 2026-11-12 (§7): no redirect, the index's size as `Content-Length`, `Range` honoured with `206`, and no `Access-Control-Allow-Origin`, so the web page downloads nothing and says so. What has not happened is a download by this code: every test uses a stand-in, and the first real one is an attended `install --dry-run`. If the server ever redirects off `dl.google.com` (to an edge host, say), the redirect is not followed and the step falls back to the owner's own download; following one would be a new decision, with the host named.
- **Google's words on the terms step** are the sentences given with the decision, as constants. Nothing checks them against Google's page, and Google can change its page. Someone has to compare them before a release, and whenever the page changes.
- **Shipping files extracted from a factory image ourselves** (so that an owner needs no factory image at all) is a separate question from decision 11, and it is not decided. Today the project hosts none of Google's files.
- **What the installer does with the factory image after checking it.** Nothing yet: the flow ends at the dry run. The page reads the file the owner picked a piece at a time and keeps none of it.
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
- `core/tests/factory_download.rs`: the terms step's words and that nothing is downloaded or taken before it is answered; the acceptance and its date; declining; a front end that cannot fetch from Google, which is shown no terms and given no download; and each property of the download against a stand-in that is a function in the test: the address is the signed index's, redirects on and off Google's host, the size before and during the transfer, the SHA-256, going on from kept bytes with the hash from the first byte, progress, and each fallback.
- `core/tests/build_index.rs`: every field's rule on the real cheetah index (`fixtures/index/`) and on changed copies; the match; trust.
- `cli/tests/flow_fake_phone.rs` (`--features fake-phone`): the built tool with typed input.
- `cli/tests/factory_download.rs` (the same feature): the built tool downloading from a stand-in for Google's server, a socket on 127.0.0.1 in the test: the terms before the first request, the file in the work directory, a download that broke off going on in a later run and in the same one with `Range`, a spoiled part found out and deleted, files that are not the listed one, redirects, and that a pretend run can't reach Google.
- `web/selftest.sh`: the page in a headless Chromium, clicked through every state against the pretend phone, under the strict Content-Security-Policy. The page as it ships states that it cannot fetch from Google, and the self-test checks that such a flow shows no terms step, offers no download and asks the stand-in nothing. The page's download code is run by flows the self-test begins without that capability; their downloads go to a stand-in that `web/serve.py --stand-in` runs on another port, which takes `dl.google.com`'s place in the policy, so the run can reach nothing of Google's: a server that lets the page read and one that does not, one that can't be reached, wrong bytes, too many, another size, redirects, a missing file and a transfer that breaks off. The browser is started with WebUSB off and, where bubblewrap is installed, without USB device nodes.

A check counts only if it fails when what it guards is removed. For this milestone that was tried by hand, one change at a time; the pull request lists the changes and what caught each.
