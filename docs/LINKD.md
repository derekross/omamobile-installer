# The install link's daemon

Status (2026-11-13): **stage L2 built and tested on a laptop, against a socket pair and a scratch directory. Stage L3's code is written (the FunctionFS transport, `omamobile-link-gadget`, the host tests, `tools/l3-run-as-root`) and tested without root: against the socket pair, a fake configfs tree and the script's dry run. It has not yet run on FunctionFS: that is the maintainer's `sudo tools/l3-run-as-root` (§5). Nothing here has run on a phone.** These are stages L2 and L3 of Omarchy Mobile's [install link plan](https://github.com/derekross/omarchy-mobile/blob/main/docs/INSTALL-LINK-PLAN.md) ("the plan" below): `omamobile-linkd`, the shell around the phone machine of the protocol crate ([LINK.md](LINK.md)), and `omamobile-install-link status|reopen`.

`linkd/` is the crate `omamobile-linkd`. It applies no rule of the protocol: the frames, the messages, the phone's state machine and the start-up rules (`phone::reconcile`) are the crate's. The daemon reads the phone into what the machine asks for, carries out what the machine returns, in order, and moves the frames.

**Where it lives.** The plan has the daemon in omarchy-mobile's package `omamobile-install-link`, with a `Cargo.lock` that pins the protocol crate's tag (its §3.3, §11.1). There is no tag yet, so that cannot be built today; the daemon is a workspace crate here, next to the crate it is built on, with a path dependency. Two ways on from here: the package takes this repository's tag as its source, as the other Rust packages there take theirs; or `linkd/` moves there once the tag exists.

## 1. What is here

| Module | What |
| --- | --- |
| `daemon` | The one loop: an event in, the machine's list out, each item carried out in order. Every `Effect` of the crate |
| `found` | What is on the phone: `Found` for the start-up rules, `status`'s facts, the radio partitions with the checks `tools/nv-backup` makes |
| `durable` | A write that holds across a power cut: a temporary file in the target's own directory, synced, renamed, the directory synced (the plan's §6.5). The kill test's hook |
| `bus` | The link's two directions (§5.7): a thread each, at most 8 frames waiting, no read while 8 wait or before the frame before is dealt with, a time limit for each write. Two transports keep these rules: FunctionFS, and the socket pair of the tests |
| `ffs` | FunctionFS (§4): the descriptors (v2: full, high and super speed, and the Microsoft OS extended compatible ID `WINUSB`) and the string written to `ep0`, `ep0`'s events, the endpoint files |
| `aio` | Linux's native asynchronous I/O for the endpoint files: one request at a time, a wait with a time limit, and `io_cancel` |
| `sockets` | The two local sockets, each for one peer whose uid the kernel vouches for (`SO_PEERCRED`), each carrying fixed lines |
| `programs` | The fixed programs (§6.1) and systemd, with fixed command lines |
| `command` | `omamobile-install-link status|reopen` |

It has no async runtime, and two dependencies beside the protocol crate: `rustix`, for the peer's uid, free space, the kernel's random numbers, `poll` on `ep0` and the kill test's signal; and `libc`, for the five system calls of native asynchronous I/O (`io_setup`, `io_submit`, `io_getevents`, `io_cancel`, `io_destroy`), which neither std nor rustix names. Those five are the crate's only `unsafe` (`#![deny(unsafe_code)]`, allowed in `aio` alone): the request block and its buffer stay the module's from the submit until the kernel reports the request complete.

```
omamobile-linkd [--root DIR] --transport ffs|stdio
omamobile-install-link status|reopen [--root DIR]
```

- **`--root DIR`** is a scratch directory in place of the phone's file system, as `OMAMOBILE_FIRSTBOOT_ROOT` and `NV_PHONE_ROOT` are for the scripts. The daemon then opens nothing outside it and runs the fixed programs from under it (`DIR/usr/bin/systemctl` and so on), which is where the tests put their stand-ins. Radio partitions under a scratch root are regular files.
- **`--transport ffs`**: the link is FunctionFS, mounted by `omamobile-link-gadget add` at `/run/omamobile-install-link/ffs` (under the root). The daemon starts `add`, waits for `ep0`, writes the descriptors and the strings, and waits for the script to put the function on the bus (§1.1).
- **`--transport stdio`**: the link is the `SOCK_SEQPACKET` socket on standard input, one frame per read as bulk transfers give. Two one-byte packets stand in for FunctionFS's events: 1 is `ENABLE`, 0 is `DISABLE`. Without a transport the daemon refuses to offer the link.
- **The test options** (`--clock-scale`, `--poll-ms`, `--write-timeout-ms`, `--max-data`, `--ack-every`, `--window`, `--working-space`, `--die-file`) are refused without a scratch root.

### The files

| Path | What |
| --- | --- |
| `/etc/omamobile/install-link` | The marker. Without it the daemon does nothing |
| `/var/lib/omamobile/install-link/state` | The state file (mode 600), from the first start, with the boot id it was last written in |
| `.../done` | Why the link ended, and for `expired` what is missing: one line of words |
| `.../reopened` | The boot `reopen` ran in |
| `.../factory.zip`, `.../modem-nv.sha256`, `.../provision.result` | The image (mode 600), the pending checksums, the worker's result |
| `/run/omamobile-install-link/` (a tmpfs: gone at every boot) | `agent` and `screen` (the sockets), `firstboot.conf` (the candidate, mode 600), `closed` (closed for this boot), `offered` (the gadget is changed), `provision.progress` |

### The two sockets

| Socket | Peer | Peer to daemon | Daemon to peer |
| --- | --- | --- | --- |
| `agent` | The account first boot made: the peer's uid, compared with the uid of the name in first boot's `user` file | `show`, `hide` | `AgentLine::line()`: `open N S`, `code DIGITS S`, `locked`, `paired`, `closed N`, `ended REASON MISSING...` |
| `screen` | root | `key` (a volume key) | `code DIGITS`, `wrong-code LEFT`, `locked WAIT_S`, `paired`, `version`, `setup`, `stopped STEP`, `no-space`, `damaged`, `off`; the last one again to a screen program that connects later |

Anything but a peer's fixed lines ends its connection. The code is in these lines and nowhere else: not in a file, not in the journal. The plan names the agent's socket and lines; **the screen's socket is this crate's choice**: the plan says the screen program draws the code and reads the volume keys, and not how it and the daemon talk.

### 1.1 FunctionFS

**Putting the function on the bus** (the plan's §3.3) is one run of `omamobile-link-gadget add` with the daemon's part in the middle of it. The daemon writes its record on the tmpfs (`offered`, E3), starts the script and waits for `ep0`; the script records the gadget as it is, writes its hang marker, unbinds the UDC, creates `functions/ffs.link` and mounts its FunctionFS; the daemon writes the descriptors and the string to `ep0`, and the kernel makes `ep1` (OUT) and `ep2` (IN); the script sets `bcdDevice` 0x0200 and the OS descriptors (`os_desc`: `use`, vendor code 0xcd, `MSFT100`), links the functions again with the link last, binds the UDC and removes its marker. The daemon then opens the endpoints. Twenty seconds for all of it, or the start fails.

**The three threads.** `ep0`'s: `poll`, then the events (`BIND`, `UNBIND`, `ENABLE`, `DISABLE`, `SUSPEND`, `RESUME`; a `SETUP` for the function is stalled, the OS descriptors being the kernel's). `ENABLE` is the machine's `LinkUp`; `DISABLE` and `UNBIND` its `LinkDown`, once; the others are logged and change nothing (§4 of the plan). The reader's: one asynchronous read of the OUT endpoint at a time, of `frame::read_buffer_len(max_data)` bytes, asked for only while the function is enabled; one completed read is one frame. The writer's: one asynchronous write at a time; one the host has not taken in the write's time (30 s) is cancelled with `io_cancel` and waited for, and the session ends (§5.7). A write that completed before the cancel reached it counts as taken.

**Leaving.** The daemon cancels what its threads have under way and closes the function's files, then runs `omamobile-link-gadget remove`, which takes the function out, unmounts it and puts the gadget back from the record `add` made. A daemon that is killed closes the files too, by the kernel's hand: FunctionFS then takes the whole gadget off the bus (E12), and the next start's `remove` (E3) puts it back.

**`omamobile-link-gadget add|remove`** is in `linkd/gadget/` until omarchy-mobile's package `omamobile-install-link` exists; it moves there, to `/usr/lib/omamobile-install-link/`, with the daemon. Exit 0 done, 1 failed with the gadget put back, 2 usage, 3 this boot skipped. Its hang marker (`/var/lib/omamobile/link-gadget-started`) holds the step and the boot id: a marker of an earlier boot (a hang inside an unbind or a bind, then a reset) skips the change for the rest of that boot (`/run/omamobile-install-link/gadget-skipped`) and is removed, so the next boot tries again; a marker of this boot (a change cut short without a hang: a killed daemon, a failure) does not stop the next `add` or `remove`. An `EXIT` trap always links the functions again and binds the UDC; a signal (a start timeout's `SIGTERM`) keeps the marker, as `omamobile-usb-mac` does. Its paths can be pointed elsewhere for tests and for stage L3 (`LINK_CONFIGFS`, `LINK_GADGET_NAME`, `LINK_FFS_DIR`, `LINK_RUN_DIR`, `LINK_MARKER`, `LINK_BOOT_ID`, `LINK_FFS_OPTS`, `LINK_UDC_ALLOW`, `LINK_WAIT_S`); a phone needs none of them.

## 2. What is not here

| Not here | Whose |
| --- | --- |
| A run on FunctionFS | The maintainer's `sudo tools/l3-run-as-root` (§5); then the phone's runbook, R2 to R4 |
| `omamobile-install-screen`, the session agent's program, the units, the package | L5, the runbook's R5. The daemon calls the gadget script at its fixed path and speaks the two sockets |
| The sleep inhibitor (§3.3) | Not built: it needs logind over D-Bus, which a scratch root has not. The link's unit is the place to prove it |
| `omamobile-firstboot --check --porcelain` | omarchy-mobile's. The tests use a stand-in that prints what `--porcelain` is specified to print |
| The golden frames in omarchy-mobile, `tools/link-golden`, the crate's tag | omarchy-mobile's and the maintainer's |
| A stop by signal | A daemon that systemd stops is killed like any other: the next start in that boot puts the gadget back (below); the unit should also do it (`ExecStopPost=`) |

## 3. Where the daemon chose, or the plan did not hold

| | The plan | The daemon | Why |
| --- | --- | --- | --- |
| E1 | §6.5, `finish`: "the marker deleted, `done` written, the state deleted". §3.3: the unit starts on "the marker and not `done`" | Every end writes `done` first, then deletes what the end deletes, and the marker last. A start that finds `done` (row 1) finishes that clean-up | Killed between the marker and `done`, nothing would ever start the daemon again: a link that neither ended nor can be reopened. With `done` first and the marker last every kill leaves a marker, so one more start gets through and completes the end. **The unit's condition must then be the marker alone**; with "and not `done`" row 1 cannot run. The kill test found it |
| E2 | §6.5: placing the answers ends with "the unit started"; "the unit also starts by itself at the next boot" | A start that finds stage `firstboot`, a `firstboot.conf` and the unit not run in this boot starts the unit | A daemon killed between copying `firstboot.conf` and starting the unit left an installer waiting for a first boot that nothing starts before the next restart |
| E3 | §3.3: `omamobile-link-gadget remove` "when the daemon stops for any reason" | The daemon records on the tmpfs that it changed the gadget. A start that finds the record (a daemon died in this boot) puts the gadget back before anything else | A killed daemon runs nothing. Without the record the next start calls `add` on a gadget that already has the function, or, when that start ends the link, leaves the function on the bus with nothing behind it |
| E4 | §2.2: after ten unpaired minutes the link is closed "for this boot" | `Close` leaves a mark on the tmpfs; a start that finds it offers nothing | The machine counts a boot once, so a daemon started again in that boot would offer the link again |
| E5 | §2.8 (P6): `reopen` records the boot id it ran in | In its own file, checked before the start-up rules run; `reopen` refuses when the boot id cannot be read | In the state file it would make that boot "counted", and the start-up rules would then offer the link at once |
| E6 | §5.4: `firstboot.step` is "the step's name as `omamobile-firstboot` logs it", and `status` "names no user" | The account step is `user` | The script logs that step as "user NAME", and `status` is answered unpaired |
| E7 | §5.7: "ends the session" after a write the bus did not take | Frames the machine still sends before it hears of it are cancelled too | They would reach the host numbered after the cancelled ones |
| E8 | §6.5: "a result file" | One that cannot be read as a result counts as a run that was interrupted | Never as a success |
| E9 | Nothing on a phone with no serial in bootconfig | Nothing is offered and nothing written | The serial proof would be of nothing |
| E10 | §3.3: `omamobile-link-gadget add` mounts FunctionFS, "lets the daemon write the descriptors", then binds | The daemon starts `add` and writes the descriptors while the script waits for the endpoint files (§1.1) | The kernel makes the endpoint files only from the descriptors, and a function without them cannot be bound: the two halves have to meet inside one run of the script, under its one `EXIT` trap |
| E11 | §3.3: a start that finds the hang marker leaves the gadget for that boot | The marker holds the boot id. Only a marker of an earlier boot skips, and the skip is kept on the tmpfs for the rest of that boot; a marker of this boot does not stop the next change | Without the boot id, a daemon killed inside `add` (E3's case, no hang) would make every later start in that boot skip the link |
| E12 | §3.3: `remove` puts the gadget back "as the initramfs made it" | `add` records the gadget on the tmpfs (the UDC, the functions in order, `bcdDevice`, `os_desc`) and `remove` puts it back from that record, also when the gadget is unbound | When the daemon's files close with the function bound, FunctionFS takes the **whole gadget** off the bus (upstream `ffs_closed()` calls `unregister_gadget_item()`): the UDC file is then empty, and NCM and ACM are gone with the link until the next `remove`. So on the development phone a daemon that dies takes SSH with it until the unit starts it again or `ExecStopPost=` runs `remove`. The mount option `no_disconnect=1` would keep the gadget bound with the link deactivated instead; L3's "a dead daemon" line shows what this kernel does, and the plan should choose after it |
| E13 | §5.7: `io_cancel` dequeues a write | The endpoint files are opened non-blocking, and every read and write is an asynchronous request | On a blocking endpoint file a request made before `ENABLE` waits for it inside `io_submit`, where no time limit reaches; non-blocking, it fails at once and the bus waits for `ENABLE` itself |
| E14 | §11.1, L2: a socket pair stands in for the endpoints | The L2 tests keep to 1024-byte frames; L3's checks of its own logic on the socket pair use 64 KiB and a small send buffer | A local datagram must fit the sender's buffer (about 208 KiB by default), so a megabyte frame is refused there, and a socket takes many small frames where a bulk IN endpoint holds one. Sizes and blocking are USB's: L3 |

## 4. The tests

`cargo test -p omamobile-linkd`. Every integration test starts real `omamobile-linkd` processes on scratch roots under the system's temporary directory, with stand-ins for `systemctl`, `systemd-run`, `omamobile-firstboot`, `phone-smoke` and the gadget script, and drives them with the crate's own host machine over a socket pair. Both clocks run 50 times faster than the wall's.

| File | What it shows |
| --- | --- |
| `tests/rows.rs` | **`rows`**: every row of §6.5's three tables as a scratch root and what the daemon does to it; no gadget change whenever the link is not offered; the budget's fourth boot |
| `tests/kill.rs` | **`kill`**: a whole install with the daemon killed (SIGKILL) at each of its kill points in turn, about 205 of them: before each item of each list the machine returns, and between the single writes of each effect that makes more than one (a temporary file written and not yet renamed; renamed and the directory not yet synced; `boot-update.conf`, `firstboot.conf`, the unit; `done`, the clean-up, the marker). The test starts it again as systemd would while the marker is there. Every time: `finished`, `done` never early, first boot ran once with the answers that were sent, one restart, the gadget put back |
| `tests/flow.rs` | The whole install with nothing going wrong, and what is on disk after each step |
| `tests/daemon.rs` | `reopen` and the boot it ran in; closed for a boot; the agent's socket and lines, `locked` after twenty wrong codes; the backup's preconditions, each refused; first boot's objections and its failure; no space; a pulled "cable" after each of 24 frames of the backup and of the image; an installer replaced after each of 16 frames with the link left up; a host that never reads |
| `tests/gadget.rs` | `gadget/test`: `omamobile-link-gadget` against a fake configfs tree, 27 checks: `add` and `remove` and what each leaves; a gadget the kernel unbound; no descriptors in time; a signal; the hang marker of this boot and of an earlier one; a UDC that is not allowed; no gadget, an unbound one, no boot id |
| `tests/l3_script.rs` | `tools/l3-run-as-root` without root: wrong arguments, the refusal without sudo, and the dry run (every root command printed, its own gadget only, only a `dummy_udc`, nothing written) |
| `tests/l3.rs` | Stage L3 (§5). Skipped unless `LINKD_L3` is set; `LINKD_L3=socket` runs its scenarios against the socket pair, without root, to check the tests' own logic |
| `src/*` (unit tests) | The order of a durable write; the bus's rules; the fixed lines; the descriptors' and the strings' layout and `ep0`'s events; an asynchronous write and read |

A SIGKILL does not lose what the page cache holds, so no test here shows that a sync holds across a power cut: the unit test shows that each write asks for the syncs, in §6.5's order.

### Guards removed, one at a time

Each was taken out of the source alone and its test run; each time the test failed.

| The guard | Caught by |
| --- | --- |
| A write's file is synced before the rename; the directory after it; the temporary file is in the target's directory; a removal syncs the directory; a left-over temporary file does not lend its mode (5 guards) | `durable::tests::*` |
| E1: `done` first, the marker last | `kill` |
| E1: a start finishes the clean-up of an end a kill cut short | `kill` |
| E2: first boot's unit started by a start that finds it not run | `kill` |
| E3: the gadget put back by the start after a daemon that died | `kill` |
| §6.5: `firstboot.conf` is in place before the unit is started | `flow` |
| The candidate is mode 600 | `flow` |
| E4: a later start in a boot the link was closed in offers nothing | `daemon::ten_unpaired_minutes_close_the_link_for_this_boot_only` |
| E5: a start in the boot `reopen` ran in exits | `daemon::reopen_grants_one_boot_and_takes_effect_at_the_next` |
| `reopen` only after `expired`; not without this boot's id (2) | `daemon::reopen_only_after_an_expired_end` |
| The agent's peer must be the account first boot made | `daemon::the_agent_socket_takes_the_account_first_boot_made_and_nobody_else` |
| One peer at a time; anything but a fixed line ends the connection (2) | `daemon::the_agents_lines_and_a_code_only_on_the_owners_tap` |
| §5.7: no read while 8 frames wait; a write gets its time and no more; the next `hello` is answered after a write was given up (3) | `daemon::a_host_that_never_reads_cannot_hold_the_daemon` |
| E7; the next frame is read only when the one before is dealt with (2) | `bus::tests::*` |
| E6: the account step names no user | `daemon::first_boot_that_fails_is_told_by_its_steps_and_names_no_user` |
| The backup: `ro` 1; not mounted; found exactly once; the device there and of the right kind; the descriptor's board (5) | `daemon::the_backup_is_refused_unless_every_partition_is_found_read_only_and_unmounted` |
| The test options are for a scratch root only | `daemon::the_test_options_are_refused_without_a_scratch_root` |
| No marker: nothing is done | `rows::row_1_done_deletes_the_marker_and_exits` |
| E9: nothing offered without a serial and a codename | `rows::nothing_is_offered_and_nothing_written_without_a_serial_or_a_codename` |
| An end deletes the image; `finished` deletes the state and the token (2) | `rows::row_7_*`, `rows::row_6_*` |
| E8: a result file that is not one is not a success | `rows::table_3_provisioning_that_was_running` |

Not shown by any test: that a partition's next chunk is read only when the bus has taken the frame before (§5.7, item 4; a socket does not show it), and the syncs of the image's file.

## 5. Stage L3: FunctionFS over `dummy_hcd`

L3 runs the real daemon on real FunctionFS, with a host that talks to it over USB through the installer's own USB library (`nusb`), all on this computer: the kernel's virtual USB controller `dummy_hcd` joins a gadget to a host bus. The host tests open only a device whose sysfs path is under `dummy_hcd`; no real USB device and no phone is touched.

### 5.1 Running it

From your own account, in the checkout:

```
sudo tools/l3-run-as-root
```

`--speeds "high full super"` (the default, in that order) picks the bus speeds, `--big-bytes N` the streamed file's length (default 4 GiB + 1000; 0 leaves it out), `--only NAME` runs only the tests whose names contain NAME. `tools/l3-run-as-root --dry-run` prints every command it would run as root and runs none. It builds first (as you), then takes about 4 minutes at high speed plus the streamed file, 2 at super speed plus the file, and about 5 at full speed (12 Mbit/s: the sizes test is left out there). The log is `target/l3/l3-YYYYMMDD-HHMMSS.log`: plain text to paste, every root command in it with its time.

The streamed file is written by the daemon into a scratch root and deleted after; the script leaves it out when the scratch directory's file system has less than its length and 2 GiB free. Its contents are a pattern that compresses to almost nothing on a compressing file system.

### 5.2 What it does as root, in order

1. Refuses unless it runs as root through `sudo` from a named account; refuses while another run is going on; cleans up after an earlier run that was killed outright (`/run/omamobile-l3.state`).
2. Read-only checks: the four modules exist; usbguard is not active (it would block the virtual device); no other gadget is bound to a `dummy_udc`; whether `dummy_hcd` was already loaded (then only its speed runs, and it stays loaded).
3. Builds the daemon and the tests as you (`cargo test --release --no-run`).
4. From here an `EXIT` trap puts everything back. Mounts configfs if it is not mounted.
5. Writes one udev rule, `/run/udev/rules.d/99-omamobile-l3-PID.rules`: the device nodes of the virtual bus only (`KERNELS=="dummy_hcd.[0-9]*"`) owned by you, so that the host tests run without root. `udevadm control --reload`.
6. For each speed: loads `dummy_hcd` at that speed, `libcomposite`, `usb_f_fs` and `usb_f_ss_lb`, recording which modules each load added; creates its own gadget `omamobile-l3-PID` (`1d6b:0104`, `bcdDevice` 0x0100) with one function no host driver binds (`SourceSink`, in place of the phone's NCM and ACM, so this computer's network and modem managers see nothing) and binds it to `dummy_udc.0`, refusing any other UDC; starts the broker (below); runs the tests as you; stops the broker and any daemon left; removes the gadget.
7. The trap: the broker and the tests' daemons stopped; the gadget unbound and removed (a FunctionFS a test left mounted unmounted first); the udev rule removed; exactly the modules it loaded unloaded; configfs unmounted if it mounted it; its directories removed.

**The broker** is how the tests, which run as you, get `omamobile-link-gadget` run as root, as the daemon does on a phone. It reads one request at a time from a FIFO only you can write (`target/l3/broker-PID/req`): `add ROOT` or `remove ROOT` runs the real `linkd/gadget/omamobile-link-gadget` against the test gadget, with FunctionFS mounted at `ROOT/run/omamobile-install-link/ffs` (`uid` and `gid` yours, so the daemon opens the endpoints as you), the hang marker and boot id under `ROOT`, and `LINK_UDC_ALLOW='dummy_udc.*'`; `ROOT` must be one of the tests' scratch roots in the run's own directory, owned by you, with no symbolic links on the way. `unbind` and `bind` write the test gadget's `UDC`; `autosuspend` and `noautosuspend` set `power/control` of the virtual bus's test device. Nothing else is taken.

### 5.3 What each result line means

Each passing check prints `L3 ok - NAME (SPEED): WHAT WAS MEASURED`; a failure is cargo's `FAILED` with the reason. At the end: `high: PASS` (or `FAIL`) per speed.

| Line | What it shows | The plan |
| --- | --- | --- |
| `descriptors` | The device as a host reads it: `1d6b:0104`, `bcdDevice` 0x0200 with the link; the interface 0xff/0x6f/0x01 named "Omarchy Mobile install link"; one bulk OUT and one bulk IN of 64, 512 or 1024 bytes as the speed; SuperSpeed companions; the OS string at 0xEE and the extended compatible ID `WINUSB` for the link's interface | §4; the **[I]**s on `bcdDevice` and the OS descriptors' 1.0 form |
| `remove` | After the daemon ends and `remove` runs: the gadget as before, its `bcdDevice` back, no link interface, bound again | §3.3 |
| `one frame per read (OUT)` | A ping of every length around one, two and three packets and 1024 and 4096 bytes, each answered once and in order: FunctionFS completed one read per transfer, at the short packet | §5.1 D6's **[I]** |
| `the pad rule is needed` | A frame of a whole number of packets without its pad, then a ping: the phone read the two as one transfer and refused it | §5.1's pad |
| `one frame per read (IN) and the pad` | The radio backup in 1992-byte data frames (56 + 1992 = 2048, a multiple of every packet size, so each carries its pad), every read on the host one frame; then the image in such frames, verified | §5.1 |
| `flow (a)` and `flow (a), after` | A host that writes and never reads: its writes are held about 30 s, the daemon cancels its write (`io_cancel`) and ends the session, with no `DISABLE` and no new enumeration; the next `hello` is answered, how many stale frames it passed over, the stream resumed at an acknowledged offset, the image verified | §11.3 F6 (a); §5.7 item 3's **[I]** for this controller |
| `flow (b)` | Each `ack` read 5 s late, the window kept, the image verified, no write given up | F6 (b) |
| `flow (c)` | `abort` in a 64 MiB partition with the reader stopped 5 s: `aborted` | F6 (c) |
| `flow (d), F1, F2` | 50 installer processes killed (SIGKILL, no unbind): in the stream, and between writing `hello` and reading its answer; each next one paired within its first three frames with no wrong count; the last finished the image from where it was | F6 (d), F1, F2 |
| `reset` | A host reset: `DISABLE` then `ENABLE`; the next session resumed | §5.3 |
| `unbind and bind` | The UDC unbound: `UNBIND`; bound: `BIND`, `ENABLE` | §4, §5.3; R4 on the phone |
| `suspend and resume` | The bus suspended and resumed: `SUSPEND`, `RESUME`, nothing ended | §4 |
| `a dead daemon, the host's view` | What a host sees when the daemon is killed with the function bound, and what the gadget's `UDC` reads then (E12) | §3.3, E3 |
| `a dead daemon, the next start` | The next start finds E3's record, runs `remove` then `add`, and a clean function comes back; no hang marker left | E3 |
| `the hang marker` | A marker of an earlier boot: no link, the gadget bound as it was, the marker gone, the skip on the tmpfs, E3's record gone; the next boot: the link | §3.3, E11 |
| `megabyte frames (IN)` | A 64 MiB partition in 1 MiB data frames, digest equal | §5.3's `max_data` |
| `a streamed file` | 4 GiB + 1000 bytes in 1 MiB frames with the real 64 MiB `ack` and 128 MiB window; the rate; how long the phone's whole-file check held its worker; verified | §5.6, §6.4 |
| `systemctl show` | The daemon's reading of this computer's real `systemctl show` (read-only) | LINKD §5's "real programs", in part |

### 5.4 Expected output

The numbers are what a run measures; these show the form (from the socket-pair check of the tests, `LINKD_L3=socket`, where the times are shorter):

```
=== high speed ===
L3 ok - descriptors (high): 1d6b:0104 bcdDevice 0x0200, interface ff/6f/01 "Omarchy Mobile install link" #1, bulk 512 bytes, OS string vendor code 0xcd, WINUSB
L3 ok - remove (high): the gadget as before: bcdDevice 0x0100, no link interface, bound to dummy_udc.0
L3 ok - one frame per read (OUT) (high): 15 pings of 511..4097 bytes, 5 padded, each one pong, in order, at packet 512
L3 ok - the pad rule is needed (high): a 1024-byte frame without its pad and the next frame were read as one, and refused
L3 ok - flow (a) (high): the host wrote 40 MiB without reading; its writes were held 30.0s from 0.1s in; ...
...
test result: ok. 12 passed; 0 failed; ...
=== results ===
high: PASS
full: PASS
super: PASS
cleanup: done (modules unloaded: ...)
```

### 5.5 What only the root run can show, and what is left after it

The root run is the first time this code meets the kernel: the descriptors' acceptance by FunctionFS, the endpoint files' blocking and `io_cancel` on `dummy_hcd`'s requests, one frame per read at each packet size, the events as FunctionFS sends them, what FunctionFS does to the gadget when the daemon dies (E12), the gadget script on real configfs, and the sizes. Tests marked "USB only" run only there. What a host killed in the middle of an OUT transfer leaves in the phone's read (the partial transfer may join the next run's `hello`) is measured by `flow (d), F1, F2`: if a next run needs more than three frames, the line says so and the plan's F1 needs a host-side fix.

After L3, for the phone (the plan's runbook): the phone's own controller's dequeue (R4), the gadget script on the phone's gadget beside NCM and ACM (R2, R3), the unit and its `ExecStopPost=`, `systemd-run` with the worker's properties, first boot's real log, and the sleep inhibitor (§2).
