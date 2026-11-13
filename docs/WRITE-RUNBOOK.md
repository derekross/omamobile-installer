# The write path's first run on a real phone: runbook

Status: **not run.** Written offline, with no phone. Nothing in this file has been tried on hardware. Results go in the last section, with the date and the installer's commit.

This is the first attended run of `omamobile-install install --write` (DESIGN §11) on the maintainer's own Pixel 7 Pro: a test phone that may be erased, that runs a working Omarchy Mobile install from slot B today, and whose radio backup exists. The maintainer runs every step, or approves it, at the keyboard. Read-only steps come first; there is one step that writes.

No identifier goes into this file or into anything committed: no serial number, no IMEI, no `getvar all` output. The placeholders:

| Placeholder | What it is |
| --- | --- |
| `<SERIAL>` | the phone's serial number in fastboot. Kept in a shell variable (step 2), never typed into a file |
| `<RELEASE_DIR>` | a downloaded, signed release for `cheetah`, with its `images/` directory and its root image (`.img.zst`) beside it or in it |
| `<FACTORY_ZIP>` | Google's factory image for the phone's build, as `tools/install-guided` keeps it in `~/.cache/omamobile-install/factory/` |
| `<BOOTLOADER>` | the phone's bootloader version, as read from **both** of its slots (see "The statement about slot A's bootloader") |
| `<NV_BACKUP_DIR>` | the radio backup made with Omarchy Mobile's `tools/nv-backup`, outside every repository |
| `<OMARCHY_MOBILE>` | the omarchy-mobile checkout |

## What this version does not do, and what that means for the run

- **The release must keep places for the answers** (DESIGN §12, decided on 2026-11-13): its `manifest.json` names `root_image.placeholders`, which Omarchy Mobile's release images carry from the change that added them. The published releases before it have none, and `install --write` stops for such a release at its start (`no-place-for-answers`), before it asks a phone anything. So this run waits for the first signed release built with them. The phone the write leaves then sets up an account from the answers given in step 5 (made-up ones) at its first start.
- **No SSH key is in slot B's ramdisk or in the answers.** The terminal's `install --write` asks for none (the desktop app's run gives this computer's, app/docs/STATUS.md §7). So there is **no SSH after the write**: `tools/phone-ssh`, `tools/phone-smoke` and `tools/nv-backup` cannot reach the phone the installer leaves. Everything this run learns after the write is read from the installer's own output, from fastboot `getvar`, and from the screen.
- **Nothing after the restart:** no radio backup, none of Google's files, no check.
- **The installer restarts the phone itself,** straight after `set_active:b`. There is no pause between the two, so there is no moment to read `getvar` between them. Step 6 catches the phone in Fastboot Mode on its way up instead.

So the session does not end with a phone to use. **It ends by installing again with `tools/install-guided --start-over` from the omarchy-mobile checkout** (step 8), which is the installer that did a real install on this phone. Without `--start-over` that tool would find its record of the earlier install and write nothing.

## What could not be verified without a real phone, and the step that answers each

| Unknown | Answered by |
| --- | --- |
| How a real bootloader answers `getvar:all` and each `getvar` the flow asks: the per-slot bootloader versions (expected: absent on this phone), `max-download-size`, the `partition-size` form, `version-baseband` | step 3 (the dry run): it stops with the phone's words if one is not as expected |
| Whether the release, its build index and the phone's bootloader and baseband pair meet (a build marked tested) | steps 1 and 3 |
| Whether each boot image fits its `_b` partition, an AVB-footer image exactly | step 3; again by the writing session in step 5, before its first write |
| Whether the pinned serial is read back the same before every command pair | step 3 (once per image), step 5 |
| **Whether a real bootloader accepts the sparse pieces this splitter cuts** | step 5: each `flash:userdata` piece answered `OKAY`. Step 7: the phone mounts what was written and starts from it |
| **How long the phone takes over the `flash:` of the piece that covered the most blocks,** against the USB wait (300 s per answer). A piece is at most 64 MiB to send, and it is not the bytes sent that take the time: a piece of repeated blocks is a few bytes and can cover up to 65536 blocks, 256 MiB of `userdata`, which is the installer's bound (DESIGN §11, "How much one flash asks of the phone") | step 5: the tool prints, for each piece, the blocks its flash covered and how long the phone took to answer, and then names the piece that covered the most blocks and the flash that took longest. A wait that runs out is the stop "The install stopped partway" with a transport error, and a line `userdata, sent and not confirmed: piece N: … blocks` |
| **What `current-slot` reads right after `set_active:b`** | **not by this run as written.** The phone already runs from slot B (a reinstall), so `current-slot` reads `b` before and after. Step 6 records it all the same. A real answer needs a phone whose active slot is A at the write: see "Not answered by this run" |
| What the bootloader's slot marks read after a flash of slot B and `set_active:b` (`slot-successful:b`, `slot-retry-count:b`, `slot-unbootable:b`) | step 2 (before) and step 6 (after) |
| What the phone shows at its first start with no `firstboot.conf` | step 7 |
| Whether `reboot` is answered before the phone drops off the bus (the tool needs the `OKAY` to say "installed") | step 5: the last screen is "Omarchy Mobile is written, and the phone is restarting", or the stop for `SlotSet` ("restart it from its Fastboot Mode screen") |

## Before anything

- The phone is the maintainer's test Pixel 7 Pro and nothing on it is needed: `userdata` is erased in step 5.
- The battery is above half. Fastboot Mode charges slowly.
- One phone is connected, by a cable that has carried a `bin/omamobile push` or an install before.
- The installer is built from the commit under test, with no features:

  ```sh
  cargo build --release --locked -p omamobile-installer-cli
  tools/check-shipping --built
  ```

  Pass: every line `ok`. Record `git rev-parse --short HEAD`.
- `<OMARCHY_MOBILE>` is at a commit whose `tools/install-guided` installed this phone, and `<FACTORY_ZIP>` and `<NV_BACKUP_DIR>` are where that run left them.

### The statement about slot A's bootloader

The Pixel 7 Pro's bootloader tells the version of the slot in use only. `tools/install-phone` goes on past that with `--bootloader-slots-match VERSION`: the owner's statement that the other slot's version was read from the phone and is VERSION, which must equal the version the phone reports. `omamobile-install` applies the same rule with the same option, on the command line only (DESIGN §6). Since the maintainer's decision of 2026-11-13 a reinstall from slot B does not need the statement: the version the phone reports is then slot B's, the bootloader the install keeps it on (DESIGN §6, "A reinstall from slot B"). This runbook still gives it with `--reinstall`; it is compared with the phone and stops the run if it is another version, so the commands below hold either way.

The installer cannot check the statement. `<BOOTLOADER>` here is therefore a version the maintainer **has read from slot A of this phone** (`fastboot getvar version-bootloader` while slot A was the current slot, as `install-phone`'s own message says), and that equals what the phone reports from slot B today. If nobody has read slot A's version on this phone, **stop: this runbook does not go on without it,** and it does not switch slots to find out.

## Steps

Each step: the command, what passes, when to stop, the way back, the risk.

### 0. The radio backup still matches (read-only, the phone running Omarchy Mobile)

```sh
cd <OMARCHY_MOBILE>
tools/nv-backup --check <NV_BACKUP_DIR>
```

- **Pass:** every partition's checksum matches the backup.
- **Stop:** any mismatch, or no backup. Do not go on to a write with a backup that does not match the phone (omarchy-mobile AGENTS.md §1): find out what changed first.
- **Rollback:** none needed (reads over SSH; the partitions are read-only).
- **Risk:** none.

The installer never writes those partitions (the gate refuses every name but slot B's five images and `userdata`). The check is here because it is the last moment this session has SSH.

### 1. The release verifies, and carries a build index (no phone)

```sh
target/release/omamobile-install verify <RELEASE_DIR>
```

- **Pass:** signed by the release key; every file matches; the root image matches the signed manifest; a line that lists Google's builds; two lines "the owner's answers go into /etc/omamobile/firstboot.conf" and "... boot-update.conf", each with its offset.
- **Stop also:** "the root image keeps no place for the owner's answers": a release from before the placeholders (above). The run waits for one that has them.
- **Stop:** "this release carries no list of Google's builds (factory-builds.json)". The flow stops for such a release before it looks at a phone (the published 2026.11.08 is one). The run then waits for a release that ships the index; nothing here works around that.
- **Rollback:** none needed.
- **Risk:** none.

### 2. The phone in Fastboot Mode, and its state before (read-only)

Bring the phone to Fastboot Mode the way a reinstall does. With Omarchy Mobile running, stop the modem first (omarchy-mobile AGENTS.md §2), then restart into the bootloader, or use the keys (Volume Down + Power, held about 20 s):

```sh
cd <OMARCHY_MOBILE>
tools/phone-ssh 'systemctl stop omamobile-modem'
bin/omamobile fastboot
```

Then, with Android's `fastboot` tool, which is used in this runbook **for `getvar` only**:

```sh
serial=$(fastboot devices | awk '$2 == "fastboot" { print $1 }')   # one line, or stop
for v in product unlocked current-slot version-bootloader version-baseband \
         slot-successful:a slot-successful:b slot-unbootable:a slot-unbootable:b \
         slot-retry-count:a slot-retry-count:b max-download-size partition-size:userdata; do
  fastboot -s "$serial" getvar "$v" 2>&1 | head -1
done
target/release/omamobile-install devices | sed -E 's/serial .*/serial <SERIAL>/'
```

- **Pass:** one phone; `product: cheetah`; `unlocked: yes`; `current-slot: b`; `version-bootloader` equals `<BOOTLOADER>`; the installer's `devices` lists one phone in fastboot mode.
- **Stop:** more than one phone; `current-slot: a` (this is then not the reinstall this runbook describes: stop and decide again); `version-bootloader` is not `<BOOTLOADER>`; `unlocked: no`.
- **Rollback:** none needed. To leave: `fastboot -s "$serial" reboot` starts Omarchy Mobile again.
- **Risk:** none (`getvar` reads). Write the values down, without the serial: step 6 compares against them.

### 3. The dry run against the real phone (read-only)

```sh
target/release/omamobile-install install <RELEASE_DIR> --serial "$serial" --dry-run \
  --reinstall --bootloader-slots-match <BOOTLOADER> --factory-zip <FACTORY_ZIP> 2>&1 | tee dry-run.log
```

Type `yes` at the warning and give the answers it asks for: made-up ones, and the same ones again in step 5, since the phone will take those (the PIN is the one to unlock it with after step 7). In the dry run they are checked, and walked into the root image's placeholders with nothing written.

- **Expect:** the screens "Read this once", "Connect the Pixel 7 Pro", "Google's factory image", "About you and the phone", "The dry run", then **"The dry run passed. Nothing was written"**. From the look at the phone on, every screen carries the notice that the run rests on the statement about the other bootloader copy. The last screen lists `download … bytes, flash:boot_b`, then the same for `init_boot_b`, `vendor_boot_b`, `vendor_kernel_boot_b` and `dtbo_b`, then `flash userdata: the root image, in N pieces (erases userdata)`, `set_active:b`, `reboot`, and nothing else.
- **Pass:** exit status 0 and that last title; the five `_b` partitions and `userdata` only; the phone still in Fastboot Mode, and step 2's `getvar` values unchanged when read again.
- **Stop:** any stop screen (its id and words say which rule: `bootloader-slots-stated-other`, `build-not-listed`, `build-not-tested`, `runs-from-slot-b`, "The dry run stopped" with the phone's own answer). Any partition in the list that is not one of the six. "The release's images are not all on this computer". Do not go on to step 4 after a stop: fix the cause, or record it as the run's result.
- **Rollback:** none needed. A dry-run session's gate passes no `flash:`, `download:`, `set_active:b` or `reboot`.
- **Risk:** low. It is the first time this code talks to a real bootloader; it sends `getvar` only. If the phone stops answering, do not send more: `fastboot -s "$serial" getvar product` once, and if that is silent too, restart the phone into Fastboot Mode with the keys.

`dry-run.log` holds no serial (the tool prints none) but check before keeping it: `grep -c "$serial" dry-run.log` must print 0.

### 4. Go or no-go (no command)

The maintainer reads step 3's list against this, and says go:

- the six partitions, in that order, each image no larger than its partition;
- `userdata` will be erased, and the working install on the phone with it;
- after the write the phone sets up an account from the made-up answers at its first start, and has no SSH (no key anywhere) until step 8;
- the way back is step 8, and behind it slot A's stock Android (`fastboot --set-active=a`, which needs a factory reset now and writes the radio partitions when it starts: a fresh `nv-backup` after any stock start).

- **Stop:** any doubt about `<BOOTLOADER>`, the backup, or the release.

### 5. The write (the one step that writes)

Do not unplug the phone or press its keys from here until it restarts by itself.

```sh
script --quiet --log-timing write.timing write.log --command \
  "target/release/omamobile-install install <RELEASE_DIR> --serial $serial --write \
   --reinstall --bootloader-slots-match <BOOTLOADER> --factory-zip <FACTORY_ZIP>"
awk '$1 > max { max = $1 } END { print "longest wait between two outputs:", max, "s" }' write.timing
```

`script` keeps the terminal the tool asks its questions on (a pipe through a line-by-line timestamper would hold the "Codename:" question back until after its answer) and records when each piece of output came. `write.log` then holds what was typed too: the answers and the codename, no serial (`grep -c "$serial" write.log` must print 0 before it is kept). Neither file is committed.

It runs step 3 again first, then shows **"This is what the installer will write"**: each partition with the image's size and the partition's size on this phone, `set_active:b`, `reboot`, what is erased, the notice about the bootloader statement, and that the phone sets up the account from the answers at its first start. Read it. Then type `cheetah`.

- **Expect, after the codename:** "Writing to the phone"; a `download:`/`flash:` pair printed for each of `boot_b`, `init_boot_b`, `vendor_boot_b`, `vendor_kernel_boot_b`, `dtbo_b`; then a line for each piece of `userdata`, `flash:userdata (piece 1: B blocks, K KiB written on the phone, S bytes sent, answered in T s)`, up to the N of step 3 (the dry run cut the same pieces); then two lines that begin `userdata, the piece that covered the most blocks:` and `userdata, the flash the phone took longest over:`; then `set_active:b`; then `reboot`; then **"Omarchy Mobile is written, and the phone is restarting"**.
- **Pass:** exit status 0 and that title; no line names another partition; no piece covers more than 65536 blocks; no flash answered anywhere near 300 s. **Record the two `userdata,` lines whole**: they answer the timing question (the most blocks one flash covered, and how long the phone took; the longest any flash took). From them, the phone's write speed: KiB written divided by seconds, to set beside the 874 KiB/s the bound assumes at least. `write.timing` is the cross-check, and covers what those lines do not (the boot images, `set_active:b`): its longest wait also includes the time the root image takes to decompress and the time spent reading the confirmation.
- **Stop:** the confirmation lists anything but the six partitions: type nothing and press Ctrl-C (nothing was written). After the codename: any stop screen. It says how far the phone confirmed the writes ("The install stopped partway", with how many boot images, whether `userdata` was begun, whether slot B was made active). Leave the phone in Fastboot Mode, send it nothing else, copy the stop's words into the results, and go to step 8: `tools/install-guided` writes every partition again whole.
- **Rollback:** step 8. Slot A is not written by any step here.
- **Risk:** medium, as every install: `userdata` is erased, and slot B's boot partitions are replaced. New here: the sparse pieces and the long writes have only ever met the pretend phone. A piece the bootloader refuses is a `FAIL` and a stop, with `userdata` partly written; step 8 covers it.

### 6. The slot state after the write (read-only)

As `reboot` is printed, **hold Volume Down** until "Fastboot Mode" shows. If the phone starts Omarchy Mobile instead, go to step 7 first and come back here with the keys (Volume Down + Power, about 20 s).

```sh
for v in current-slot slot-successful:b slot-unbootable:b slot-retry-count:b \
         slot-successful:a slot-unbootable:a slot-retry-count:a unlocked version-bootloader; do
  fastboot -s "$serial" getvar "$v" 2>&1 | head -1
done
```

- **Expect:** `current-slot: b`; `slot-unbootable:b: no`; `unlocked: yes`; `version-bootloader` as before. `slot-successful:b` and `slot-retry-count:b` are what this step is for: record them beside step 2's (on the Pixel 9a a flash of slot B alone clears the successful mark; whether the 7 Pro does the same after this installer's commands is not known).
- **Stop:** `current-slot: a` (then `set_active:b` did not take, whatever the tool printed: record it and go to step 8); `slot-unbootable:b: yes`.
- **Rollback:** none needed.
- **Risk:** none. Then `fastboot -s "$serial" reboot` for step 7.

### 7. What the phone shows (no command)

**Keep the cable in, to a computer that is awake, from the restart on.** Slot B's initramfs has a USB safety net (omarchy-mobile `initramfs/init`, "Self-recovery for bring-up"): if no USB host has configured the phone's USB gadget 45 s after it starts, it prints why on the screen, waits 20 s more (and for the root file system to finish growing), and panics on purpose. That is about 65 s after the start; the panic restarts the phone, and each such start spends one of slot B's tries. Out of tries, the bootloader starts stock Android from slot A, which rewrites the radio partitions. A cable to a charger, or to a computer that is asleep, counts as no host. If the cable does come out, put it back and count the restart in the results.

Watch the screen for five minutes. Nothing can be sent to the phone and nothing is.

- **Expect (inferred from the root image's build and the first-start unit, never seen):** the boot logo; the root file system grows; `omamobile-firstboot` runs with the answers from step 5 (a few minutes); then the phone logs in to that account and locks, and the PIN from step 5 unlocks it. The phone may show up as a USB network device on the computer; SSH is refused (no key anywhere, root locked). If the screen stays on the logo or comes up with no account, record it: that is the placeholders' first test on a phone failing, and step 8 still applies.
- **Pass, for this run's purpose:** the phone gets past the bootloader and the kernel's logo into the system written in step 5, and does not fall back to slot A. That is the evidence that the pieces of `userdata` hold a file system.
- **Stop:** "omamobile: no USB host, crashing in 20 s" on the screen, or the phone restarting by itself about a minute after each start: the safety net above found no USB host. Check the cable and the computer before the next start; do not let it run through slot B's tries. Stock Android's recovery or its "Try again / Factory data reset" screen: slot B did not start and the bootloader fell back. **Choose neither.** Hold Volume Down + Power about 20 s for Fastboot Mode (omarchy-mobile AGENTS.md §4) and go to step 8. A stock start writes the radio partitions: take a fresh read-only `nv-backup` after step 8 and say so in the results.
- **Record:** what was on the screen at 30 s, 2 min and 5 min, in words. A photo only if it shows no network name.
- **Rollback:** step 8.
- **Risk:** none (watching).

### 8. Install again with the shell installer (ends the session)

Volume Down + Power, about 20 s, for Fastboot Mode. Then:

```sh
cd <OMARCHY_MOBILE>
tools/install-guided --status
tools/install-guided --start-over --dry-run --reinstall --bootloader-slots-match <BOOTLOADER>
tools/install-guided --start-over --reinstall --bootloader-slots-match <BOOTLOADER>
tools/install-guided --status
```

**`--start-over` is what makes this step do anything.** This computer holds `install-guided`'s record of the install that put Omarchy Mobile on this phone, and that record says it finished. Without `--start-over`, `install-guided` reads the record, prints "This phone was installed on …" and "To install again from the start …", and exits with status 0 **without writing anything**, for `--dry-run` as well: a recovery step that reports success and leaves the phone as step 5 left it. `--start-over` deletes that record and nothing else (the `state` file and `report.json` in `~/.local/state/omamobile-install/cheetah/`). The radio backup in `<NV_BACKUP_DIR>` and the factory image in `~/.cache/omamobile-install/factory/` stay where they are: its help says so, and the code removes only those two files. It is on both commands on purpose: a dry run records nothing, so the second one finds no record and its `--start-over` does nothing.

The reinstall takes its own radio backup into a new, dated folder (`~/omamobile-backup/cheetah-radio-backup-<date>-<time>`, or under `--backup-dir`); `<NV_BACKUP_DIR>` is not written.

- **Expect:** the first `--status` shows the earlier install, every step `done`. Each of the two runs then prints "The record of the earlier install is forgotten" (the first one) and goes through its steps by number: "Step 1 of 11: Checking this computer and the download" onwards. The second asks for the codename and prints "Step 5 of 11: Writing Omarchy Mobile to the phone" with `install-phone`'s own flash output under it.
- **Pass, all of:**
  - neither run printed "This phone was installed on";
  - the second run showed the write step doing the write (the codename typed, `install-phone`'s flashes of the five `_b` images and `userdata`), then its own steps to the end (the USB connection, the first start, `nv-backup`, `phone-provision`, the restart, `phone-smoke`);
  - the last `--status` reads `started` today, "Writing Omarchy Mobile to the phone" `done`, and every later step `done`;
  - the phone is the working install it was before step 5, with the account `install-guided` just asked for (the phone step 5 left had none: an account is itself the proof that this install ran);
  - `tools/nv-backup --check <NV_BACKUP_DIR>` matches, unless step 7 met a stock start.
- **Stop:** "This phone was installed on …" (a run without `--start-over`: nothing was written; run it again with the option). Otherwise as `docs/INSTALL-GUIDED.md` says for each of its steps.
- **Rollback:** `fastboot --set-active=a` and stock Android, as omarchy-mobile's INSTALL.md describes.
- **Risk:** medium (it erases `userdata` again, as every install).

## Not answered by this run

- **`current-slot` right after `set_active:b`, from slot A.** The rerun advice in DESIGN §11 ("after `set_active:b` a rerun is refused unless it is said to be a reinstall") rests on it. This phone is on slot B before the write, so the run cannot show it. Answering it needs slot A active at the write, without `--reinstall`. On this phone that means `fastboot --set-active=a` by hand first and no restart in between, since a restart with slot A active starts stock Android on a foreign `userdata`. That is a slot change this runbook does not make: it is the maintainer's decision, for a later run.
- **An interrupted write on real hardware.** The pretend phone had its cable pulled before every command; nobody pulls the real one on purpose in a first run.
- **A download of Google's factory image by this tool.** `--factory-zip` is given in every step so that the first real write is not also the first real download. That is its own attended `install --dry-run` (DESIGN §9).
- **A locked phone,** and the unlock question `flashing get_unlock_ability`: this phone is unlocked and is never locked for a test.
- **A phone that reports both bootloader slots.**

## Results

Not run yet. For each run: the date, the installer's commit (`git rev-parse --short HEAD`), the release's version, each step's outcome in a line, the number of `userdata` pieces and the two `userdata,` lines of step 5 (the piece that covered the most blocks with its flash time, and the slowest flash), the longest wait in `write.timing` after the codename, step 2's and step 6's `getvar` values side by side (no serial), and what step 7 saw.
