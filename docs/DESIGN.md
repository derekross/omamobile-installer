# The install flow: design

Status (2026-11-12): **built and tested against a pretend phone only; never run against a real one.** That includes the write path (§11): it is built, every rule of it has a test that reads the pretend phone's record of what it was sent, and it has never sent a command to a real phone. Its first real use is the maintainer's own attended run. The command-line tool is the one front end that can write, and only with `--write` and the typed codename; the web page cannot. The owner's answers do not reach the phone yet (§12), so an install made by this version starts with no user account. A Pixel 7 Pro reaches a write only on its owner's statement about the bootloader slot it does not report, given on the command line (§6, "The owner's statement"); the maintainer's override that passes the same stop is for dry runs only. The first attended run is laid out in [WRITE-RUNBOOK.md](WRITE-RUNBOOK.md).

This is the design of the installer's flow: the steps of an install as one state machine in the core (`core/src/flow.rs`), which the command-line tool and the web page both drive. It follows Omarchy Mobile's [install experience plan](https://github.com/derekross/omarchy-mobile/blob/main/docs/INSTALL-EXPERIENCE-PLAN.md) (M3, M4) and the maintainer's decisions of 2026-11-10 and 2026-11-12, listed in §2. How the shell-script installer does each step today is in that project's `docs/INSTALL-GUIDED.md`.

## 1. The shape

```
front end                         core (no I/O of its own)
---------                         ------------------------
fetches the release's files  -->  Flow::begin          verifies, checks the build index
draws Flow::screen()         <--  Screen               title, text, steps, link, file, problem, plan, footer
hands in what the person did -->  acknowledge_warning, read_phone, accept_google_terms, factory_zip, answers, dry_run
lends a transport            -->  getvar and one read-only question reach the phone, nothing else
(a flow begun to write)      -->  confirm_install (what the owner typed), begin_write
sends the release's images   -->  the one Session that can write; write_done or write_failed
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
5. **No slot switching.** Reading the other bootloader slot by making it active is off for every device unless its descriptor says `BOOTLOADER_SLOT_PROBE=yes`. None does. Where fastboot reports only the current slot's version (the Pixel 7 Pro), the install stops and explains. (Changed in one respect with the write path, for the command line only, and **for the maintainer to confirm when that pull request is reviewed**: the stop can be passed by the owner's statement about the other slot, which is `tools/install-phone`'s rule. The installer still switches no slot. §6, "The owner's statement".)
6. **Every step is tied to one phone,** pinned by its serial number in fastboot.
7. **Only slot B's boot images and `userdata` are ever written, nothing before the release is verified, and unsigned bundles are not supported.** The flow adds no write at all. (That last sentence was true until the write path of §11. The rule itself is unchanged, and §11 is how the flow keeps it.)

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
| `dry-run` | What the dry run is | `dry_run(transport)`, then `dry_run_done(session)` or `dry_run_failed` | `done`; in a flow that writes, `confirm`; or a stop | `getvar` |
| `done` | The plan: what an install would write. Nothing was written | | (the end of a dry run) | |
| `confirm` | Only in a flow begun with `FlowOptions::write`: each partition that will be written, with the image's size and the partition's size on this phone, in order; then `set_active:b` and `reboot`; what is erased and what is not touched; that the answers do not reach the phone; and what to type | `confirm_install(typed)`: what the owner typed, as typed | `write` when it is the phone's codename; a stop for anything else | nothing |
| `write` | That the writes come now, and to leave the phone alone | `begin_write(transport)`; then the release's images through the session it gives; then `write_done(session)`, or `write_failed(session, error)` | `installed`, or a stop | `getvar`, then `download:`, `flash:` (slot B's boot images, `userdata`), `set_active:b`, `reboot` |
| `installed` | Every write was confirmed, slot B is active, the phone was told to restart; what the first start is, and that it has no user account | | (the end) | |
| `stopped` | Why, and what is true of the phone | | (the end: a new flow starts over) | |

A step handed in out of order is refused and changes nothing. Before the warning is accepted the flow refuses to look at a phone, so not even `getvar` is sent.

**Verifying comes first, and so does the build index.** `begin` checks the signature, the checksums, the manifest and the descriptor, and then the build index, before the warning. A release that can't be used stops there, not after the owner has erased a phone for it.

**Every stop comes before the unlock steps.** Each look at the phone judges it in one order: the model, the pinned phone, slot B, the two bootloader slots, the build index. Only a phone that passes all of them is shown how to unlock. This differs from the shell installer, which checks slots and the index after the unlock. It rests on a locked bootloader answering the same `getvar` questions as an unlocked one, which has not been checked on a phone (§9).

**A flow that ends at the dry run says so before the unlock steps.** Unlocking erases the phone, and this version installs nothing afterwards, so an owner who followed the unlock screen would erase a phone for a list of what an install would write. The core therefore puts this on the unlock screen as its notice (`Screen.notice`, which both front ends draw right under the title and set apart, before the body and the steps): "This version of the installer cannot install Omarchy Mobile yet. It only checks the phone and lists what an install would write. Unlocking the bootloader erases the phone, and there is no reason to unlock it for this version: do not unlock the phone for it." The steps stay, introduced as what a version that can install will ask, not for now. The sentence is tied to the flow's own state (`Flow::ends_at_dry_run`, which is true of every flow in this version) and not to a front end or a build: a version of the flow that writes answers that question from its state, and a flow that will install shows the steps without the notice. With the maintainer's override on a locked phone, the notice holds both sentences, this one first.

**The unlock question.** `flashing get_unlock_ability` asks a phone whether it would take an unlock command, which is what "OEM unlocking" in Android's Developer options decides. It reads; it changes nothing on the phone. It is the one command of the `flashing` family in the crate, as a named operation with a typed answer (`fastboot::unlock_ability`, giving `UnlockAbility::Allowed`, `NotAllowed` or `Unknown`), not a way to send a command of the caller's choosing. The command layer holds a closed list of what may leave (`getvar:`, `download:`, `flash:`, `set_active:b`, `reboot`, and this command compared whole), checked at the one place a command is sent; `flashing unlock`, `flashing lock`, `flashing unlock_critical` and every `oem` command are refused there, and a test tries each.

**The gate is closed by itself, not by the layer above it** (`allowed` in `core/src/fastboot.rs`). Each client carries the list of partitions it may flash, checked at the moment a command is sent:

- A client that only asks (`probe`, `unlock_ability`) has an empty list, and so has every dry-run session's. With an empty list nothing that writes or stages a write passes: no `flash:`, no `download:`, no `set_active:b`, no `reboot`. Every session a shipping front end opens is a dry run, so in a shipping build the gate passes `getvar:` and the one question, and nothing else.
- The session that may write (the one `Flow::begin_write` opens, with the token only it can make: §11, "One way to a session that writes") has the verified release's partitions as its list. A `flash:` passes only for a name that is on that list and also on the gate's own fixed one, slot B's boot images and `userdata`. So `flash:persist`, `flash:boot_a` or `flash:vbmeta_b` is refused at the gate even if the `Partition` type above it had let the name through; the type and the gate each hold without the other, and a test feeds the gate a list of such names.
- A `download:` carries exactly eight hex digits.

No option says whether a session writes. `Session::connect` and `connect_pinned` open dry runs and nothing else; `PhoneOptions` has no field for it.

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

**A front end that cannot fetch from Google says so at the start, and is not shown the step either** (`FlowOptions::cannot_fetch_from_google`). The web page is that front end: Google's server sends no `Access-Control-Allow-Origin` header (measured on 2026-11-12, §7), so a browser lets no page read the file, and asking the owner to accept Google's terms for a download that cannot be made would have them accept twice, here and on Google's page, for nothing. With the capability stated, a phone that passes every stop goes from `phone` (or `unlock`) straight to `factory-image` with the owner's own download (`OwnerFileWhy::CannotFetch`), `Flow::factory_download` is refused, and the screen says why in the core's words: "A web page is not allowed to read files from Google's server, so this page cannot download the image for you. The installer's command-line tool can download it for you. Here you download the image from Google's page yourself, where Google asks you to accept its terms, and the installer checks the file you saved." The command-line tool does not state it, and downloads. The decision is the core's: a front end states what it can't do and draws what it is given.

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
- **A file that is not the listed one stops the install** (`factory-download-mismatch`, §4).
- **Bytes kept on this computer are deleted only when they can't be part of the right file:** there are more of them than the file has (they are not used, and the file starts afresh), the file is complete and its SHA-256 is another, or more bytes arrived after them than the file has. Then the front end is told `Discard`. A size the server only announces is no such proof. An answer whose `Content-Length` or `Content-Range` total is not the index's still ends the try before a byte of its body is read, and still stops the install, but the answer is `Kept`: what an earlier try downloaded stays, and the next run goes on from it, hashing it from its first byte as always. Deleting it bought no safety and cost a download of several gigabytes for one odd answer.
- **A download that can't be made is not a stop and says nothing against the file.** A redirect off Google's host, too many redirects (five), a status that is neither the file nor a redirect, an answer that does not fit the request, or a browser that got no answer it may read: the step falls back to the owner's own download, with the reason in a sentence, and the installer downloads no more in that run.
- **A download that broke off stays open.** The screen says how many bytes had arrived, and it can be tried again; a front end that kept the bytes goes on from them.
- **A download that goes quiet is one that broke off.** A front end that fetches must give up on a body that stops arriving, or a server that sends half the file and then nothing holds it for good. It tells the core `FetchFailure::Stalled { seconds }`, and the step stays open like any interrupted download, with what arrived kept: "The download from Google's server stopped before it was complete: … of … bytes had arrived (Google's server sent nothing for 60 seconds). You can try again."
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
| `bootloader-slots-stated-other` | Only the current slot's version is reported, and the version the owner stated for the other slot is not that one | stops again (with the same statement) |
| `baseband-unknown` | No `version-baseband` | stops again |
| `build-not-listed` | No build has both the phone's versions | after an Android update |
| `build-not-tested` | The build is listed, not marked tested | after an Android update, if a newer build was tested; else stops again ("not tested yet") |
| `factory-download-mismatch` | What Google's server sent or offered is not the file the signed index describes: the server said another size, sent more bytes than the file has, or the SHA-256 of the whole file is another. What was downloaded is deleted, except after a size that was only said: then nothing was taken, and what an earlier try kept stays | at any time (the file is downloaded afresh, or goes on from what was kept) |
| `locked-at-dry-run` | The bootloader is locked again at the dry run | after unlocking |
| `build-changed` | Other versions at the dry run than the factory image was chosen for | at any time |
| `dry-run-refused` | A check only the session makes (a partition's size, `max-download-size`), or the link was lost | stops again; after reconnecting |
| `override-in-an-install` | A flow that writes was begun with the maintainer's override. Stopped in `begin`, before the release is read | without the override, or as a dry run |
| `writes-not-for-this-device` | A flow that writes, for a release whose phone is not in `WRITE_DEVICES` (today: the Pixel 7 Pro). Stopped in `begin` | stops again |
| `images-not-checked` | A flow that writes, and the front end did not walk the release's images through the dry run (they are not on this computer) | once they are fetched |
| `not-confirmed` | What was typed at the confirmation is not the phone's codename. Nothing was written | at any time |
| `locked-at-write` | The bootloader is locked again when the write is begun | after unlocking |
| `write-refused` | The checks the writing session starts with did not pass (a partition's size, the root image against `userdata`), or the link was lost, before any write | stops again; after reconnecting |
| `write-interrupted` | The writes began and did not run through: the link was lost, the phone answered FAIL, another phone answered, a file could not be read. The closing lines say how far the phone confirmed the writes (§11) | at any time; what a rerun does is said for each case |

`phone-changed`, `runs-from-slot-b`, both bootloader-slot stops, `baseband-unknown`, both build stops and `build-changed` can also come at `write`: `begin_write` judges the phone again.

A factory image the owner hands in that isn't taken (`ZipRefusal`: another name, another size, another SHA-256) is not a stop: the step stays open and says why. That differs from a download on purpose. A person can pick the wrong file, and picks another. A file the installer fetched itself from the one address it trusts, complete, with another SHA-256, means that Google's file and the release's list do not agree, and trying again in the same run would only fetch it again.

Not stops either: Google's terms not accepted, a download that can't be made (`OwnerFileWhy::Unavailable`: the step falls back to the owner's own download), and a download that broke off (`DownloadTrouble`: the step stays open). §3, "The download".

`factory-download-mismatch` says: "The downloaded file is not the one this release lists", then what was wrong ("… the file is not what the release's signed list of Google's builds says: its SHA-256 is another than the one the list records for build …"), then "The installer uses nothing of that file, and what it had downloaded of it is deleted from this computer.", then that starting again downloads the file afresh and whom to tell if it stops there again. After a size the server only said, the second sentence is instead "The installer took nothing of what the server offered.", followed, when an earlier try left bytes, by "The 20000 bytes an earlier try downloaded stay on this computer: the server's answer says nothing about them, and they are checked from the first byte, together with the rest, before anything of the file is used." and "Starting the installer again goes on from those bytes."

### What a stop says

The M1 reviews of the shell installer found stops that reassured wrongly: "Nothing is lost. Run this command again" after a half-written phone and after stops that a rerun can't pass, and a footer that called the wrong one of two phones "the phone". So here:

- **The closing lines are derived, never fixed.** `Stop::footer()` is built from two facts: `PhoneFact` (no phone was asked; a phone was only asked what it is; the same after the unlock steps were shown, with "if you unlocked its bootloader, that erased the phone"; two phones, each only asked) and `Retry` (at any time; once the message's fix is done; stops here again).
- **A stop that a retry can't pass says so,** and its message says what has to change or who to ask. The slot stop hands out no way around it.
- **Advice comes with where it leads.** "Update Android" is followed by: Android then runs from slot B, and the installer will stop for that.
- **No advice that can't work.** An untested build with no newer tested build in the index is "not tested yet" (decision 10, 2026-11-12): the stop's title says so, and its message says that nothing the owner can install today changes that, that an Android update does not help, and what they can do, which is to wait until the build has been tested or ask in the issue tracker. It is not told to update Android, and it still stops. "Newer" is the index's own order, newest first: a tested build listed before the phone's. When there is one, the stop names it and says to update Android to it, with where that leads.
- **A stop after the writes began says how far they got.** `PhoneFact::WriteStopped` carries a `WriteReach`, which the session keeps from what the phone confirmed, and whether the phone ran from slot B. The closing lines are built from the two: what is on the phone now, what the owner should do, and what starting again does (§11, "Interrupted"). A stop before the first write still says that nothing was written, and only then.

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
- only the current slot's reported, and nothing stated about the other: **stop** (`bootloader-slots-unknown`);
- only the current slot's reported, and the owner stated the other slot's version: go on if it is the version the phone reports, **stop** if it is not (`bootloader-slots-stated-other`). Below.

**Reported means a version, not a line.** A bootloader that answers `version-bootloader-a:` or `:b:` with nothing after the colon has not reported that slot, as `tools/install-phone` reads it (`[[ -n $bl_a && -n $bl_b ]]`). Two empty answers are therefore not "both reported and equal": such a phone is one that tells one slot, and takes the third or fourth line above. The rule is in the one function that reads a per-slot version (`slot_bootloader` in `core/src/fastboot.rs`), which the flow's judge and every session's `connect` both use. Tests: `a_slot_whose_bootloader_version_is_empty_is_a_slot_not_reported` (there), and `a_phone_whose_per_slot_bootloader_versions_are_empty_has_not_reported_them` (`core/tests/write_path.rs`).

The flow never switches slots, on any of these paths.

### The owner's statement

Added with the write path, because without it no Pixel 7 Pro can be written at all, and the write path's first real run is on one. It reverses, for the command line only, what this section said until then ("the flow never takes the owner's word for the other slot, and the command-line option that set it is gone"), so **it is the maintainer's to confirm at review.** Nothing was invented for it: it is the rule of Omarchy Mobile's `tools/install-phone`, the shell installer that installs this phone today.

That rule, read from `tools/install-phone` (its phone checks) and `tools/install-guided` (`slots_check`):

1. If the bootloader reports both slots' versions, they must be equal. The statement is not looked at.
2. Otherwise, with `--bootloader-slots-match VERSION`: VERSION must equal the phone's `version-bootloader` (the slot in use), or the install stops. `install-phone`'s help says what VERSION asserts: "you checked the other slot carries VERSION too". `install-guided` calls it "a statement that someone read it from the phone".
3. Otherwise the install stops.
4. `install-guided` compares the statement before the unlock steps as well as at the write, so that a wrong one stops a locked phone before it is erased, and its stop for case 3 prints no option and no command: the option is not handed out.

Here:

- `FlowOptions::bootloader_slots_match` is the statement. `Flow::judge` applies 1 to 3 at every look at the phone: the first one (so before the unlock steps), the dry run's and the write's. Each session is given it too (`PhoneOptions::bootloader_slots_confirmed`) and compares it again at `connect`, with the version it reads then.
- **The command line only.** `--bootloader-slots-match VERSION`, for `install --dry-run`, `install --write` and `flow`; said once, with a version, or the tool refuses its arguments before any USB device is looked for. `omamobile-install help` says what it asserts and that the installer cannot check it. The stop for a phone that reports one slot does not mention it (4 above).
- **Not the web page.** The page makes its flow's options in one function, which does not set it, and `tools/check-shipping` fails if the page's source or the shipping module names it. A statement like this is typed by the phone's owner at their own keyboard, and the page ends at a dry run anyway.
- **Said on every screen once it was used** (`Screen.notice`, `SLOTS_ON_THE_OWNERS_WORD`): "This phone tells the version of only one of its two bootloader copies. The installer has not read the other copy's version and cannot: it goes on because this run was started with the statement that someone read it from the phone and that it is the same version. If that is not so, the phone may not start from slot B." The confirmation before a write carries it, and a locked phone's unlock screen says that the second copy was not read, where it otherwise says the two match.
- **It passes that stop and no other,** and is not a way round two versions that are known to differ.
- **What it cannot do** is make the statement true. If slot A's and slot B's bootloaders differ on such a phone, the phone may not start from slot B, and the way back is the bootloader's own fallback to slot A. That is the risk `install-phone` carries today, no smaller here.

Test: `a_phone_that_tells_one_bootloader_slot_is_written_only_on_the_owners_statement` (`core/tests/write_path.rs`), and `a_phone_that_tells_one_bootloader_slot_is_written_on_the_owners_statement_only` in `cli/tests/flow_fake_phone.rs`.

### The maintainer's override

Decided on 2026-11-12, because the rule above stops the maintainer's own Pixel 7 Pro before its first dry run on real hardware can show anything else.

`FlowOptions::maintainer_skip_bootloader_slot_check` lets the flow go on past `bootloader-slots-unknown`. What holds it in:

- **A dry run only.** A flow begun to write and with the override stops in `Flow::begin`, before the release is read or a phone is asked anything (`override-in-an-install`); `dry_run_done` stops a flow that writes if a check was skipped anyway, and `begin_write` refuses with `Error::OverrideOutsideDryRun`. The session the dry run opens carries the override (`PhoneOptions::maintainer_skip_bootloader_slot_check`), and the session that writes refuses it with that same typed error, before a single question is sent to the phone, whatever the phone would have reported (a session opened with `Session::connect` holds every write back, and takes it). A session whose check was skipped also refuses at the last gate before a write is sent.
- **That stop and no other.** Two versions that are known to differ still stop (`bootloader-slots-differ`), at the first look and at the dry run, and so does everything else.
- **Hidden.** On the command line it is `--maintainer-skip-bootloader-slot-check`, for `install --dry-run` and `flow`; it is not in the usage or help text, and `install` without `--dry-run` (so also with `--write`) refuses it from the arguments alone, before any USB device is looked for. The web page has no control for it and the shipping module no way to set it; only the page's self-test sets it, in a self-test build, against the pretend phone. No stop's text mentions it.
- **Said, in the core's words, on every screen from the moment a check was skipped** (`Screen.notice`, which both front ends draw right under the title and set apart): "The bootloader-slot check was skipped by a maintainer override. This phone tells the version of only one of its two bootloader copies, and the installer went on without knowing the other. This run is not evidence that an install would be allowed." The last screen's title is "The dry run finished with one check skipped. Nothing was written", not "The dry run passed", and the plan records it (`DryRunPlan::slot_check_skipped`). A locked phone's unlock screen says its second bootloader copy was not checked, where it otherwise says the two match.
- **Nothing is said when nothing was skipped.** On a phone that reports both slots the override changes nothing, and no screen mentions it.

It is not a way to install on such a phone. That takes the owner's statement above, or the probe below.

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
9. **Write only through the flow, and only if it is a front end that may.** Today that is the command-line tool. It sets `FlowOptions::write` only for its explicit `--write`, never by default; shows the `confirm` screen whole; hands in what the owner typed without changing it; opens no session of its own (one it opened itself could not write: §11); and gives the session back with `write_done` or `write_failed`, so a stop can say how far the writes got. The web page sets `write: false`, calls none of these, and CI checks both in its source and in the shipping build.

How the two do it: `cli/src/main.rs` (`run_flow`) and `web/static/app.js` with `web/src/lib.rs` (`InstallFlow`).

### The download in each front end

**The command-line tool** fetches with `ureq` (a small blocking HTTP client; TLS by rustls with the webpki roots; no async runtime, which the tool does not otherwise have). It follows no redirect by itself and treats no status as an error: the core sees each answer. Its timeouts: 30 seconds to connect, 60 seconds for the answer's headers, and 60 seconds without a byte of the body, after which the try ends as an interrupted download and the part is kept (the body is read on a thread of its own, since the client has a limit only for a whole transfer, and no number fits that for a file of several gigabytes). The file goes to `factory/<name>.part` under the work directory (`--work-dir DIR`; by default the user's cache directory, `omamobile-installer`), and is renamed to `<name>` once the core says `Verified`. A later run finds either file, reads it back into the core from its first byte and asks Google for the rest only. `--factory-zip FILE` hands in a file the owner already has, and nothing is downloaded. **It uses no proxy unless the owner names one** with `--proxy http://HOST:PORT`: the client would otherwise take one from `http_proxy`, `https_proxy` or `all_proxy`, and whatever set such a variable would be told, unasked, that this computer fetches a Pixel factory image (it could not change the file: the tunnel is TLS to `dl.google.com` against the built-in roots). A proxy is a route, not an address, so the rule that no address comes from the environment did not cover it; now the route does not come from there either. `flow` (the pretend phone) never reaches Google: its fetches go to a stand-in on this computer named with `--fake-google 127.0.0.1:PORT`, or nowhere; `install` refuses that option.

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

### What a phone or a server says, on a terminal

The command-line tool prints the core's screens on a terminal, and a terminal acts on control characters: an escape sequence clears the screen or sets the window's title, a carriage return overwrites the line before it. A phone's `product`, its version strings and a `FAIL` message are the phone's to choose, and so is the text an HTTP client quotes from a server, and the name of a file on the owner's disk. So no such character leaves the core (`core/src/text.rs`):

- **What a phone says is made plain where it is read** (`fastboot`'s reply reader), before it is compared, kept in a report or put in an error. A tab or a line break becomes a space; every other control character, and the marks that turn the reading order round or hide in a word, is spelled out where it stood (`\u{1b}` for ESC). Nothing is dropped, so two answers that differ by such a character still differ: `cheetah` followed by a NUL is not the codename, and a serial number that carries one does not match the chosen device.
- **Every string of a `Screen` goes through the same filter where the screen is made,** whatever it was built from.

A front end that prints a string it got from a device itself (the tool's `devices`, from the USB descriptors) uses the same function.

## 8. Pinning the phone

The first phone of the right model the flow looks at becomes the install's phone (`PhoneId`, the SHA-256 of its fastboot serial; the core holds the serial itself nowhere but in an open `Session`). Each later look and the dry run compare against it, and the dry run's `Session` reads the serial again before each (held back) write.

The writing session is opened on the pinned `PhoneId` and asks the phone its serial number and product again before every `download:` and `flash:` pair and before `set_active:b`; another answer ends it with nothing more sent (§11).

A later version has steps after the write that talk to the phone some other way (USB networking, the install link). A serial pinned in fastboot says nothing about who answers there. The shell installer's answer is to ask the phone for the digest of the serial its bootloader handed the kernel, before every such step, and to stop unless it equals the pinned one. `PhoneId` is that digest, so the same check fits; those steps must take a `PhoneId` and refuse without a match.

## 9. What is open

Not built, in the order a real install needs them:

- **A run on a real phone.** Unknown until then: how a Pixel's bootloader answers `getvar:all`; whether a locked bootloader answers `current-slot`, `version-baseband` and the per-slot versions (§3 depends on it); `max-download-size`; timing. For the write path (§11), also: how long the phone takes over the `flash:` of the piece that covers the most blocks (the installer bounds it at 256 MiB of `userdata` and prints the time for each piece: §11, "How much one flash asks of the phone"), against the transport's 300 s wait; whether the phone takes the pieces this splitter cuts (they follow the layout of AOSP's own resparse, and only the pretend phone has parsed them); whether `current-slot` reads `b` right after `set_active:b` (the rerun advice covers both answers); and what the phone shows at its first start without `firstboot.conf`. The first run should be `install --dry-run`, which sends `getvar` and, to a locked phone, `flashing get_unlock_ability`. On a Pixel 7 Pro it is expected to stop at `bootloader-slots-unknown`; the maintainer's override (§6) takes a dry run past that stop, to see the rest.
- **How a real bootloader answers `flashing get_unlock_ability`.** The flow asks it since 2026-11-12 (§3). The pretend phone answers with an `INFO` line `get_unlock_ability: 0` or `1`; whether a Pixel's bootloader uses that form, and whether a locked one answers at all, is unknown until the run above. Any other answer gives the steps for both cases.
- **A whole download from Google's server by the tool** (decision 11). How the server answers was measured on 2026-11-12 (§7): no redirect, the index's size as `Content-Length`, `Range` honoured with `206`, and no `Access-Control-Allow-Origin`, so the web page downloads nothing and says so. What has not happened is a download by this code: every test uses a stand-in, and the first real one is an attended `install --dry-run`. If the server ever redirects off `dl.google.com` (to an edge host, say), the redirect is not followed and the step falls back to the owner's own download; following one would be a new decision, with the host named.
- **Google's words on the terms step** are the sentences given with the decision, as constants. Nothing checks them against Google's page, and Google can change its page. Someone has to compare them before a release, and whenever the page changes.
- **Shipping files extracted from a factory image ourselves** (so that an owner needs no factory image at all) is a separate question from decision 11, and it is not decided. Today the project hosts none of Google's files.
- **What the installer does with the factory image after checking it.** Nothing yet: the flow ends at the dry run. The page reads the file the owner picked a piece at a time and keeps none of it.
- **Restarting an Android phone into Fastboot Mode** (adb). The phone screen gives the key combination.
- **How the owner unlocks from the web page.** The command needs Android's fastboot tool on the computer, which the plan says a person should never have to install.
- **Knowing the other bootloader slot without anyone's word.** The Pixel 7 Pro, the one phone `WRITE_DEVICES` names, is written only on its owner's statement about the slot it does not report (§6). The probe, or another way to measure slot B's bootloader, is what would retire the statement; Omarchy Mobile's decision of 2026-11-10 (its plan's D4) is that `install-phone` will measure it on a first install.
- **Delivering the owner's answers** (`firstboot.conf`): §12. This document said "until then no front end may write". The command-line tool now can, because the write path was asked for before that question is settled; the confirmation says, before anything is written, that the phone will start with no user account. Whether `--write` should be held back until §12 is settled is the maintainer's call.
- **The install link** and every step after the write: the first start, the radio backup, firmware, the check.
- **A record of how far an install got**, so a second run goes on instead of starting over. Today a rerun writes everything again (§11).
- **The web page's downloads:** the release's images and root image, with resume and storage, so the dry run there can walk them.
- **The desktop window** (a web view around the same page, with the native core behind it), and **Windows and macOS** (type-checked only; drivers, signing).
- **The published build index** with a detached signature (§5).
- **Timeouts in the browser** for a phone that stops answering.

## 10. Tests

- `core/tests/flow.rs`: each state and each stop against the pretend phone; each test also checks that only `getvar` reached it, and for a locked phone at the unlock steps `flashing get_unlock_ability`, counted. The override's properties, one by one: it passes the one-slot stop in a dry run and says so on every screen, it passes no other stop, and a session that could write refuses it before anything is sent.
- `core/src/text.rs` (unit tests) and two tests in `core/tests/flow.rs`, one in `core/tests/factory_download.rs`: a phone's product, versions, `FAIL` message and unlock answer, an HTTP client's error and a file's name, each carrying ESC sequences, a carriage return, a NUL, a bell and a right-to-left override, reach no screen and no error as such.
- `core/src/fastboot.rs` (unit tests, where the private command layer is): of the `flashing` and `oem` families only the one question is sent, nothing off the list of commands is sent, a `flash:` leaves only for a partition on the session's list (slot A, the unsuffixed names, bootloader, radio, vbmeta, devinfo, persist, efs, modem and made-up names are each tried), the gate refuses a wrong name without the `Partition` type's help, a client that only asks and a dry-run session's client send nothing that writes, the default options say nothing and skip nothing, and an answer is a plain yes or no or nothing is concluded.
- `core/tests/write_path.rs`: the write path, rule by rule (§11 names the test for each). Every test reads the pretend phone's record of what it was sent.
- `core/src/flow/tests.rs` (unit tests, which can set the flow's private fields and make a write token): the checks in `begin_write` that no order of the public steps can reach (§11, rule 2), that a session writes only with a token, and that the session that writes refuses the maintainer's override before anything is sent. They borrow the pretend phone and the test release from `core/tests/support`.
- The core's doctests (`cargo test` runs them): making a `WriteToken` and asking `PhoneOptions` for a writing session do not compile, each beside the same lines in a form that does (§11, "One way to a session that writes").
- `core/tests/fastboot_install.rs`: whole installs and refused ones at the session's level. The session that writes is reached through the flow, as everywhere; the checks a session starts with are tried through dry runs, which share them.
- `core/src/firstboot.rs` (unit tests): sha512-crypt against the scheme's vector and `openssl passwd -6`, the first-start files line for line, the rules for the SSH key, name and email.
- `core/tests/factory_download.rs`: the terms step's words and that nothing is downloaded or taken before it is answered; the acceptance and its date; declining; a front end that cannot fetch from Google, which is shown no terms and given no download; and each property of the download against a stand-in that is a function in the test: the address is the signed index's, redirects on and off Google's host, the size before and during the transfer, the SHA-256, going on from kept bytes with the hash from the first byte, progress, and each fallback.
- `core/tests/build_index.rs`: every field's rule on the real cheetah index (`fixtures/index/`) and on changed copies; the match; trust.
- `cli/tests/flow_fake_phone.rs` (`--features fake-phone`): the built tool with typed input, `--write` against the pretend phone among it: what is shown before the confirmation, the commands the phone received, and that nothing is written without `--write` or without the codename. No test runs `install` far enough to look for a USB device.
- `cli/tests/factory_download.rs` (the same feature): the built tool downloading from a stand-in for Google's server, a socket on 127.0.0.1 in the test: the terms before the first request, the file in the work directory, a download that broke off going on in a later run and in the same one with `Range`, a server that goes silent mid-file with the connection held open, a spoiled part found out and deleted, files that are not the listed one, redirects, that a proxy named in the environment is not connected to while one named with `--proxy` is, and that a pretend run can't reach Google.
- `web/selftest.sh`: the page in a headless Chromium, clicked through every state against the pretend phone, under the strict Content-Security-Policy. Its own walk of an install against the pretend phone is a dry run: no build of the page's module can open a session that writes. The page as it ships states that it cannot fetch from Google, and the self-test checks that such a flow shows no terms step, offers no download and asks the stand-in nothing. The page's download code is run by flows the self-test begins without that capability; their downloads go to a stand-in that `web/serve.py --stand-in` runs on another port, which takes `dl.google.com`'s place in the policy, so the run can reach nothing of Google's: a server that lets the page read and one that does not, one that can't be reached, wrong bytes, too many, another size, redirects, a missing file and a transfer that breaks off. The browser is started with WebUSB off and, where bubblewrap is installed, without USB device nodes.

A check counts only if it fails when what it guards is removed. For this milestone that was tried by hand, one change at a time; the pull request lists the changes and what caught each.

## 11. The write path

What is written, in which order and after which checks is what Omarchy Mobile's `tools/install-phone` does. Where this differs, it says so (the last part of this section).

### What a successful install sends

After the questions (`getvar:`), and nothing else:

```
download:<size>, flash:<image>_b      once per boot image, in the descriptor's BOOT_IMAGES order
                                      (Pixel 7 Pro: boot_b, init_boot_b, vendor_boot_b, vendor_kernel_boot_b, dtbo_b)
download:<size>, flash:userdata       once per sparse image of the root image, each no larger than max-download-size
                                      (and than 64 MiB), and each covering at most 65536 blocks (256 MiB) of userdata
set_active:b                          what fastboot --set-active=b sends
reboot
```

Before each `download:` and `flash:` pair, and before `set_active:b`, the session asks `getvar:serialno` and `getvar:product` again.

### The rules, and the test for each

Tests are in `core/tests/write_path.rs` unless another file is named.

1. **Only slot B's boot images and `userdata`.** The partitions come from the verified release: `manifest.json`'s writes, which must be the descriptor's `BOOT_IMAGES` with `_b`, then `userdata` (`manifest.rs`). A session builds its list of what a `flash:` may name from those at `connect`, and the one place a command is sent compares against it whole (`fastboot.rs`, `allowed`): the gate that was closed on the main branch before the write path was put on it. That gate also holds a fixed list of its own (slot B's five images and `userdata`), lets a `download:` through only with eight hex digits, and gives a dry-run session an empty list, with which no `flash:`, `download:`, `set_active:b` or `reboot` passes at all. Every command of the write path leaves through it; there is no second list beside it. The only session with a list is the one `Flow::begin_write` opens (`Session::may_flash`), and that this is the only one is held by the compiler (below, "One way to a session that writes"). No function takes a partition name from a caller, a person or a phone. Tests: `a_flash_leaves_only_for_a_partition_on_the_sessions_list` (in `fastboot.rs`: each forbidden name and made-up ones, at the command layer), `the_flash_list_is_the_releases_descriptor_and_nothing_else` (a three-image descriptor writes three; a signed release naming any other partition is no release), `an_install_sends_these_commands_in_this_order_and_nothing_else`, `the_one_gate_has_the_releases_list_only_in_the_session_that_writes` (the dry run of a flow that will write has an empty list; the writing session has the release's six names, and no more); in `fastboot.rs` also `the_gate_holds_without_the_partition_type` and `a_dry_run_sessions_client_sends_no_command_that_writes`.
2. **Nothing is written unless all of these hold,** checked together in `Flow::begin_write`, the one place the flow opens a session that can write:
   - the release and its build index were verified: `nothing_is_written_for_a_release_or_an_index_that_did_not_verify`;
   - the phone passed every stop in this run, and passes them again when asked at the write: `nothing_is_written_to_a_phone_that_does_not_pass_every_stop`;
   - the phone is unlocked: `nothing_is_written_to_a_locked_phone`;
   - it is the pinned phone, at the write and before every command pair after it: `nothing_is_written_to_another_phone_than_the_pinned_one`;
   - the owner typed the codename in this run: `nothing_is_written_without_the_typed_codename`, and for `begin_write`'s own check `begin_write_itself_refuses_a_run_in_which_the_codename_was_not_typed`;
   - the flow is not a dry run: `a_dry_run_has_no_way_to_a_write`, and for `begin_write`'s own check `begin_write_itself_refuses_a_flow_that_was_not_begun_to_write`;
   - the maintainer's override was not used: `the_maintainers_override_never_reaches_a_write`;
   - a phone that reports one bootloader slot was stated to match by its owner, and the statement is that slot's version when asked at the write: `a_phone_that_tells_one_bootloader_slot_is_written_only_on_the_owners_statement`.

   The two tests named second are in `core/src/flow/tests.rs`, not in `core/tests`, and that is the point of them. The flow's public steps cannot take those two conditions away: the write step is reached only through `confirm_install` with the right codename, and the confirmation only in a flow begun to write. So the tests in `core/tests` show that the steps before `begin_write` hold them, and passed unchanged with either check deleted from `begin_write` (the safety review of this pull request tried it). The unit tests take a real flow to the write step against the pretend phone, clear the one private field, and call `begin_write`: it must refuse, with not one command sent, not even a question. A third runs the same flow unchanged, and gets the session. Each of the two fails when its check is removed.

   Also: the phone is one the write path was written for (`a_release_for_another_phone_model_is_not_written`), and every image was checked in this run (`a_write_needs_every_image_checked_in_this_run`).
3. **Sizes, before the first write.** Each boot image must fit its partition, an `AVB_FOOTERS` image exactly (its footer is read from the partition's end), and be no larger than `max-download-size`: checked by the dry run's session and again by the writing session at `connect`. The root image's unpacked size is checked against `userdata` by the dry run, and again by `Session::preflight_userdata` before the first write. Test: `every_image_is_checked_against_its_partition_before_the_first_write`.
4. **Order and interruption:** below. Test: `an_install_interrupted_anywhere_can_be_run_again`. **Every `flash:` is bounded in what it makes the phone write:** "How much one flash asks of the phone", below.
5. **The slot.** `set_active:b`, once, after the last write was confirmed, then `reboot`. That is all install-phone does about slots, and all the core has: no `set_active:a`, no command that marks a slot good or bad, no `reboot-bootloader`. (The phone's own `omamobile-boot-success` marks slot B good after a start; that is not the installer's.) Test: `an_install_sends_these_commands_in_this_order_and_nothing_else`; `nothing_off_the_list_is_sent` in `fastboot.rs`.
6. **The owner's answers:** not delivered. §12.
7. **After the last write:** `reboot`, and the `installed` screen. Nothing else is sent, and the installer does nothing after the restart. Test: the same as rule 5, which reads the last screen.
8. **The web page has no write path.** No build of its module can open a session that writes (below). `cli/tests/flow_fake_phone.rs` covers the tool's `--write`; `tools/check-shipping` covers the page, in its source and in the shipping build, and the tool's one switch: its write switch is set in one line of the command-line tool, from `--write`.
9. **The owner's words are the core's.** The `confirm` screen lists each partition with the image's size and the partition's size on this phone, the two commands that follow, what is erased and what is never written. Test: `the_confirmation_lists_every_partition_its_size_and_what_is_erased`, which compares the screen whole.
10. **The bytes that are sent are the bytes that were checked, and every file is checked before the first write.** A boot image is read into memory, checked against the signed manifest and sent from that memory (`VerifiedImage` borrows the bytes). The root image is too large for that: it is hashed, then read again to be cut and sent, and until the safety review of this pull request the tool opened its name a second time for the second reading, after slot B's boot images were already written. Two things changed, because each closes a different gap:
    - **In the tool (`walk_images`):** every boot image is read and checked, and the root image is hashed, before the first `flash:`; a file that is not the release's stops the install with the phone as it was. The root image is opened once: hashed from that handle, which is then rewound and streamed, so a name that points at another file meanwhile changes nothing. Each boot image is read and checked once more as it is written.
    - **In the core (`Session::userdata_streamed`, `finish_userdata`):** one open file can still change in place between the two readings, so the session hashes the compressed bytes as they are streamed to it (the helper in `install.rs` hands each buffer to the session's hash and to the splitter) and `finish_userdata` refuses unless that digest is the signed manifest's. A session that refused is over: `set_active:b` and `reboot` are never sent, the phone stays on the slot it was on, and the stop says `userdata` was begun and the install did not run through. A caller that hands the session pieces without the bytes they were cut from is refused the same way.

    Hashing while streaming alone would have been enough to keep slot B from being made active, and it is the part that proves what was sent. It was not chosen alone because it finds a wrong file only after `userdata` is erased; the hash before the first write is what keeps a wrong file from costing anything. Tests: `a_root_image_that_is_not_the_one_that_was_checked_never_makes_slot_b_active`, and in `cli/tests/flow_fake_phone.rs` `a_release_file_that_changed_after_the_confirmation_is_found_before_the_first_write` (one bit of the root image, then of the last boot image, changed in place while the tool waits for the codename: nothing but questions reaches the phone; with the old order five partitions were written first).

### One way to a session that writes

Rule 2 says the conditions are checked together in `Flow::begin_write`. That is worth something only if no session can write that `begin_write` did not open. Until the safety review of this pull request that rested on a search of the sources: `Session::connect` was public and `PhoneOptions` had a public `dry_run` field, so any crate could have written `dry_run: false`, opened a writing session, and skipped the typed codename, the verified build index, `WRITE_DEVICES` and the check of `userdata`'s size, none of which a session checks itself. The web crate did have such a path, behind a cargo feature nothing turned on.

Now the compiler holds it:

- **A session writes only if it holds a `flow::WriteToken`.** `Session` keeps the token it was opened with; without one it is a dry run, its gate's list is empty, and no `flash:`, `download:`, `set_active:b` or `reboot` passes (rule 1). `PhoneOptions` has no `dry_run` field any more: whether a session writes is not an option.
- **Only `Flow::begin_write` can make a token.** `WriteToken` has one field, private to the flow's module, no constructor, and no `Default`, `Clone` or `Copy`. Rust's privacy rule is the proof: a tuple struct with a private field cannot be built outside the module that declares it, by any crate. `begin_write` makes it on the line before it opens the session, after every check of rule 2.
- **The function that takes the token is not public** (`Session::connect_writing`, `pub(crate)`), so another crate could not call it even with a token. `Session::connect` and `connect_pinned`, which stay public for dry runs, cannot be handed one.
- **Proof that runs:** the doctests on `WriteToken` and `PhoneOptions` (`cargo test` runs them) try to make a token by its constructor and by `default()`, and to write `PhoneOptions { dry_run: false, .. }`; each must fail to compile, and each stands beside the same lines in a form that does compile, so a doctest that fails for an unrelated reason (a renamed path) is noticed. Making the field public, or deriving `Default`, fails them (tried). In `core/src/flow/tests.rs`, `a_session_writes_only_with_a_token` opens both kinds on the same phone with the same options and walks the release through each.
- **What the compiler does not hold, `tools/check-shipping` reads:** inside the flow's own module the field can be reached, so the check is that `WriteToken(())` is written once there, in `begin_write`, and `connect_writing` called once, in `begin_write`; that the two declarations are as above; and that the only other file that makes a token is the flow's unit tests, which are compiled only for tests. Three changes were tried against it: the field made public, `connect_writing` made public, and a second token made in `dry_run`. Each fails it.

What went with this: the web crate's `enable-writes` feature, its direct session (`connect`, `Install`) and `writes_enabled`. The page's self-test used to run a real install against the pretend phone through such a session; it walks the same images through a dry run now, and the writes are tested in the core and through the command-line tool. The core's own tests of the session (`core/tests/fastboot_install.rs`) reach the session that writes through the flow, like everything else.

### How much one flash asks of the phone

The transport waits 300 s for an answer (`sparse::FLASH_WAIT_SECONDS`, which the USB crate takes its wait from). A wait that runs out ends the install where it is, with the phone possibly still writing: the worst place is inside `userdata`. So the question is what one `flash:` can ask for.

A boot image is at most its partition, tens of megabytes. A piece of the root image was bounded only in its own size (`max-download-size`, and the tool's 64 MiB), and that says nothing about the work: the splitter turns every run of blocks that repeat one 32-bit word, zeros among them, into a FILL chunk, which is sixteen bytes however many blocks it covers. A root file system is mostly such blocks. One download of a few hundred bytes could tell the phone to write gigabytes, behind one answer.

Two rules were possible: make the wait for a `flash:` grow with the blocks it covers, or bound the blocks. **The blocks are bounded**, because that rule is the simpler one to hold and to test:

- It is a property of the bytes sent, checked where they are cut and again where they are sent, with no clock in it. A scaled wait would have to reach the transport, which does not know what a command is, and the browser's transport has no wait at all.
- It keeps the wait short. A longer wait is also a longer time a phone that hung is not noticed.
- A stop inside `userdata` can then say something bounded: the piece in doubt covers at most 256 MiB.

The rule: **no piece covers more than `sparse::MAX_PIECE_BLOCKS` = 65536 blocks of 4096 bytes, 256 MiB of `userdata`,** counting its RAW and FILL chunks. The splitter ends a piece there and the next goes on from that block. The session does not rely on the splitter: `flash_userdata_segment` reads every chunk header of the piece it is handed (`sparse::cover`), refuses anything that is not a sparse image as this crate cuts them, and refuses a piece that covers more than the cap, or nothing, before anything is sent.

**The stated minimum write speed** is what makes 256 MiB fit 300 s: 874 KiB/s (`sparse::MIN_WRITE_KIB_PER_SECOND`, computed from the two numbers). A Pixel's storage writes on the order of a hundred times faster, so the wait is a net for a phone that stopped, not a budget a healthy one comes near. A phone that writes slower than that is one this installer cannot wait for, and it says so with a stop inside `userdata` (below). What the real number is on a Pixel 7 Pro is not known until the first run (§9), which is why it is measured:

**What the session reports for each piece** (`Session::pieces`, a `PieceReport` each): the bytes downloaded, the blocks its flash covered, and how long the phone took from `flash:userdata` to its `OKAY`. The core has no clock, so the front end lends one (`Session::time_flashes_with`); the command-line tool does, when it writes. The piece is recorded before its commands are sent, so after a stop the last one reported is the piece in doubt, with its blocks and no time. The tool prints a line per piece and, at the end of `userdata` or at a stop inside it, which piece covered the most blocks, which flash took longest, and a piece that was sent and not confirmed. The runbook's timing question reads those lines.

Tests: `no_piece_covers_more_blocks_than_one_flash_may` and `a_piece_is_read_back_chunk_by_chunk_or_refused` (`core/tests/sparse_zstd.rs`; the first cuts a 261 MiB image of repeated blocks, which was one piece of about a megabyte covering all of it before the cap), `a_piece_that_covers_more_than_one_flash_may_is_refused_by_the_session` and `the_session_reports_each_piece_its_blocks_and_how_long_its_flash_took` (`core/tests/fastboot_install.rs`), and the tool's lines in `cli/tests/flow_fake_phone.rs`.

### Interrupted

The session records how far the phone confirmed the writes (`WriteReach`). A `download:` alone changes nothing on a phone, so only `flash:`, `set_active:b` and `reboot` count; a command that was sent and not answered counts as "in doubt". `Flow::write_failed` turns that into the stop's closing lines.

| The run stopped | The phone then | What the owner is told | A rerun |
| --- | --- | --- | --- |
| Before the first `flash:` (`Nothing`) | As it was | "No write reached the phone" | Any time; nothing differs |
| Among the boot images (`BootImages`) | Slot B's boot partitions partly new; the one in doubt may be half written. `userdata` and slot A untouched, slot A still active: Android starts with everything on it. (On a reinstall, where the phone runs from slot B, it may not start.) | How many of how many were written, that `userdata` was not, and which slot the phone starts from | Writes every image again from the first |
| In `userdata` (`Userdata`), or at its end because what was streamed was not the image that was checked (rule 10) | Boot images written. `userdata` partly written: what Android stored is gone. Slot A still active, so a restart starts Android, which finds no data and asks for a factory reset | That it is erased, and to bring the phone back to Fastboot Mode and start again | The same |
| After the last image, before or at `set_active:b` (`Written`, `SlotInDoubt`) | Everything written. Slot A active, or unknown if the answer was lost | That every image is written and the slot was not (or may not have been) made active | The same. If the slot did change, the installer then sees a phone on slot B and stops (`runs-from-slot-b`); said to be a reinstall, it runs through. The stop's text says so |
| After `set_active:b`, at `reboot` (`SlotSet`) | Complete. Slot B active; the phone may or may not have restarted | To restart it from its Fastboot Mode screen | Not needed; as a reinstall it writes everything again |

A rerun is a new flow: it verifies the release, judges the phone and asks for the codename again, then writes everything from the first image. Every write is of a whole partition image or a whole sparse image, so writing again over a partial write leaves the same bytes as one clean run. The test pulls the pretend phone's cable before every command from the first write on, and with the answer lost for every command that is not a question, checks that the stop's account matches what the phone received, replugs, runs again, and compares every partition.

**After `set_active:b` a rerun is refused unless it is said to be a reinstall.** That is `runs-from-slot-b` doing its job: from fastboot alone, a half-finished install and stock Android on slot B look the same, as install-phone's own comment says. It rests on the bootloader reporting the active slot as `current-slot` after `set_active`, which is not checked on a phone (§9).

### Where this differs from install-phone

- **Checks first, and more of them.** install-phone leaves the root image's size to `fastboot flash`, after slot B is already written; here it is checked before the first write. The slot and build-index checks come before the unlock (§3). `--bootloader-slots-match` is install-phone's, with its rule (§6); here every screen of a run that used it says so, the confirmation among them, where install-phone prints one line.
- **The dry run is part of the run that writes,** not a separate invocation, so what the owner confirms was computed on this phone minutes before.
- **Other phones are refused.** install-phone sends the same commands to every device with an `android-v4` descriptor. What `set_active:b` costs and what a failed start falls back to differ by phone (Omarchy Mobile's AGENTS.md records for the Pixel 9a that a flash of slot B clears its "successful" mark, and that a fallback to slot A cost `userdata`), so `WRITE_DEVICES` names the Pixel 7 Pro alone and a release for another phone stops a flow that writes. Adding a phone is a decision, with its notes read.
- **A stop says where it stopped.** install-phone says "the phone is still in fastboot. Slot A is untouched; fix the cause and rerun" wherever a command fails, including after `userdata` was partly written, when Android's data is gone.
- **The images are written as the release signed them.** install-phone flashes copies it changed: the root image with the owner's files, and the ramdisk image with the owner's SSH key appended. Here every byte written was checked against the signed manifest, the root image a second time as it is sent (rule 10), and the answers are not delivered (§12).
- **No hints after the restart** about the radio backup and firmware: those tools need SSH access to the phone, which comes with the answers.
- **A second switch.** install-phone writes unless told `--dry-run`. The tool here needs `--write` and has no default.

## 12. The owner's answers: decided, the install link (E)

**Decided by the maintainer on 2026-11-12: option E.** The answers reach the phone over the install link at its first start, the channel the install plan already chose for Google's files (its D1 and M2). The installer never edits an image: what it flashes stays byte for byte what the release signed, and options A to D below are not taken. Until the install link exists, on the phone and here, `--write` stays what it is today: a proof that the write itself works, for an attended test, and not an install anyone can use. `tools/install-phone` remains the way to install a phone.

What follows is the analysis that led there, kept as written.


install-phone puts the answers on the phone before the flash: it unpacks the root image to a local copy, writes `/etc/omamobile/firstboot.conf` (mode 0600), `/root/.ssh/authorized_keys` and `/etc/omamobile/boot-update.conf` (`AUTO_WRITE=yes`) into it with `debugfs -w`, checks the copy with `e2fsck`, turns it into a sparse image and flashes that. It also appends the SSH key to the initramfs inside the ramdisk image (`vendor_kernel_boot`) and flashes that copy. The phone's `omamobile-firstboot` creates the user from `firstboot.conf` and deletes it; without the file it does nothing.

**What is built:** everything up to writing into the image. `core/src/firstboot.rs` makes the exact bytes of `firstboot.conf` and `authorized_keys` from the flow's answers, with the password hashed as `openssl passwd -6` hashes it and install-phone's rules for the SSH key, full name and email. The root image is unpacked and cut into sparse images as a stream, without a local copy (`install.rs`, `sparse.rs`, `zstd_push.rs`). **What is not:** putting those bytes into the ext4 image. The core has no ext4 writer, and it runs no other program. Nothing calls `firstboot.rs` yet, and the flow does not ask for an SSH key, a full name or an email.

So today a phone installed by this installer starts Omarchy Mobile with no user account, no way to unlock it and no SSH access. The `answers` step, the `confirm` screen, the `installed` screen and `--write`'s help all say so. What exactly the phone shows at that first start has not been looked at on a phone.

The options, with what each costs:

| Option | What it takes | Cost and risk |
| --- | --- | --- |
| **A. An ext4 writer in the core,** our own, in Rust | Read the superblock, group descriptors, bitmaps, inode table and two directories; allocate inodes and blocks; add directory entries (and `/root/.ssh` if the image has none); update the counts and every checksum (`mkfs.ext4`'s defaults, which `rootfs/build-inside` uses, include `metadata_csum`). As a stream it needs two passes over the image (one to read the metadata, one to send it with the changed blocks swapped in), or a local unpacked copy like install-phone's | No dependency. On the order of a thousand lines of file-system code that runs on every install, has to follow whatever features the image is made with, and can produce a root file system that is subtly wrong. Its tests need real ext4 images and `e2fsck` as the judge (in CI, not in the core). The largest and riskiest piece of code in the installer |
| **B. An ext4 crate** | The same edits through someone else's library | A large dependency in the code that writes to phones, to be read and pinned. Not evaluated here: which crates can write, with `metadata_csum`, to WebAssembly as well |
| **C. `debugfs` from the command-line tool,** as install-phone | The tool unpacks to a local copy and runs e2fsprogs | Works today on Linux. Needs the image's size free on disk, e2fsprogs installed, and a front end doing what the core should decide; nothing for the web page, little for Windows. The core itself runs no other program, by rule |
| **D. A place for the answers, reserved by the release** | `build-rootfs` creates `firstboot.conf` (and `authorized_keys`) in the image as files of a fixed size filled with blank lines, and the signed manifest records which blocks hold them. The installer swaps those blocks in the stream, padding to the same size: no file-system structure changes, and data blocks carry no checksum | Small and streaming in the core (a block swap ahead of the sparse splitter). Costs a change on the other side: `build-rootfs`, the bundle's manifest (a new key in the closed install plan, so older installers refuse such a release), and `omamobile-firstboot` must take an all-blank file as "no answers". Needs a release made that way |
| **E. The install link** (the install plan's M2) | The phone asks for the answers over USB at its first start | No change to any image, and the same path later carries the factory image. All of it is still to build, on the phone and here |

D and E both keep "every byte written is the release's, or a block the release reserved". A, B and C write an image no signature covers.

Separate from all five, and needed for install-phone's whole result:

- **The SSH key in slot B's initramfs.** A cpio archive of one file, an LZ4 stream in the legacy format, and the vendor boot image's header and ramdisk table rewritten: small to write. The cost is the rule it bends: the session takes only bytes the signed manifest lists (`VerifiedImage`), and this image would be the release's plus bytes the installer made. It needs its own proof type, so that only "the signed image with exactly this appended" can be written. Without it the initramfs's rescue SSH has no owner key until the phone's first kernel update.
- **`boot-update.conf` with `AUTO_WRITE=yes`.** It lets the phone write slot B's boot images by itself on a kernel update. Omarchy Mobile's rules name install-phone as the one tool that writes it, so whether this installer does is a decision, not a detail. Without it the kernel package's updates do not reach slot B on a phone installed here.
- **The three questions the flow does not ask:** an SSH public key, and a full name and email for git. `firstboot.rs` takes them; the `answers` step and both front ends do not.
