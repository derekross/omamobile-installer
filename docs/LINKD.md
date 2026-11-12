# The install link's daemon

Status (2026-11-12): **built and tested on a laptop only, against a socket pair and a scratch directory. Nothing here has opened a USB gadget, FunctionFS, configfs or a phone.** This is stage L2 of Omarchy Mobile's [install link plan](https://github.com/derekross/omarchy-mobile/blob/main/docs/INSTALL-LINK-PLAN.md) ("the plan" below): `omamobile-linkd`, the shell around the phone machine of the protocol crate ([LINK.md](LINK.md)), and `omamobile-install-link status|reopen`.

`linkd/` is the crate `omamobile-linkd`. It applies no rule of the protocol: the frames, the messages, the phone's state machine and the start-up rules (`phone::reconcile`) are the crate's. The daemon reads the phone into what the machine asks for, carries out what the machine returns, in order, and moves the frames.

**Where it lives.** The plan has the daemon in omarchy-mobile's package `omamobile-install-link`, with a `Cargo.lock` that pins the protocol crate's tag (its §3.3, §11.1). There is no tag yet, so that cannot be built today; the daemon is a workspace crate here, next to the crate it is built on, with a path dependency. Two ways on from here: the package takes this repository's tag as its source, as the other Rust packages there take theirs; or `linkd/` moves there once the tag exists.

## 1. What is here

| Module | What |
| --- | --- |
| `daemon` | The one loop: an event in, the machine's list out, each item carried out in order. Every `Effect` of the crate |
| `found` | What is on the phone: `Found` for the start-up rules, `status`'s facts, the radio partitions with the checks `tools/nv-backup` makes |
| `durable` | A write that holds across a power cut: a temporary file in the target's own directory, synced, renamed, the directory synced (the plan's §6.5). The kill test's hook |
| `bus` | The link's two directions (§5.7): a thread each, at most 8 frames waiting, no read while 8 wait or before the frame before is dealt with, a time limit for each write |
| `sockets` | The two local sockets, each for one peer whose uid the kernel vouches for (`SO_PEERCRED`), each carrying fixed lines |
| `programs` | The fixed programs (§6.1) and systemd, with fixed command lines |
| `command` | `omamobile-install-link status|reopen` |

It has no `unsafe` (`#![forbid(unsafe_code)]`), no async runtime, and one dependency beside the protocol crate: `rustix`, for the peer's uid, free space, the kernel's random numbers and the kill test's signal.

```
omamobile-linkd [--root DIR] --transport stdio
omamobile-install-link status|reopen [--root DIR]
```

- **`--root DIR`** is a scratch directory in place of the phone's file system, as `OMAMOBILE_FIRSTBOOT_ROOT` and `NV_PHONE_ROOT` are for the scripts. The daemon then opens nothing outside it and runs the fixed programs from under it (`DIR/usr/bin/systemctl` and so on), which is where the tests put their stand-ins. Radio partitions under a scratch root are regular files.
- **`--transport stdio`**: the link is the `SOCK_SEQPACKET` socket on standard input, one frame per read as bulk transfers give. Two one-byte packets stand in for FunctionFS's events: 1 is `ENABLE`, 0 is `DISABLE`. Without it the daemon refuses to offer the link: FunctionFS is stage L3.
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

## 2. What is not here

| Not here | Whose |
| --- | --- |
| FunctionFS: the descriptors, the endpoint files, `io_cancel`, `ENABLE`/`DISABLE`/`SUSPEND` from ep0 | L3. `bus` is written against a socket; the rules it keeps are the ones L3 keeps |
| `omamobile-link-gadget`, `omamobile-install-screen`, the session agent's program, the units, the package | L3, L5, the runbook's R5. The daemon calls the gadget script at its fixed path and speaks the two sockets |
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

## 4. The tests

`cargo test -p omamobile-linkd`. Every integration test starts real `omamobile-linkd` processes on scratch roots under the system's temporary directory, with stand-ins for `systemctl`, `systemd-run`, `omamobile-firstboot`, `phone-smoke` and the gadget script, and drives them with the crate's own host machine over a socket pair. Both clocks run 50 times faster than the wall's.

| File | What it shows |
| --- | --- |
| `tests/rows.rs` | **`rows`**: every row of §6.5's three tables as a scratch root and what the daemon does to it; no gadget change whenever the link is not offered; the budget's fourth boot |
| `tests/kill.rs` | **`kill`**: a whole install with the daemon killed (SIGKILL) at each of its kill points in turn, about 205 of them: before each item of each list the machine returns, and between the single writes of each effect that makes more than one (a temporary file written and not yet renamed; renamed and the directory not yet synced; `boot-update.conf`, `firstboot.conf`, the unit; `done`, the clean-up, the marker). The test starts it again as systemd would while the marker is there. Every time: `finished`, `done` never early, first boot ran once with the answers that were sent, one restart, the gadget put back |
| `tests/flow.rs` | The whole install with nothing going wrong, and what is on disk after each step |
| `tests/daemon.rs` | `reopen` and the boot it ran in; closed for a boot; the agent's socket and lines, `locked` after twenty wrong codes; the backup's preconditions, each refused; first boot's objections and its failure; no space; a pulled "cable" after each of 24 frames of the backup and of the image; an installer replaced after each of 16 frames with the link left up; a host that never reads |
| `src/*` (unit tests) | The order of a durable write; the bus's rules; the fixed lines |

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

## 5. What stage L3 must still prove

- **Everything about real blocking.** A socket holds a writer back much as a bulk endpoint does, which is how `a_host_that_never_reads` works, but the cancel is not the same: here a write that timed out was never sent. On FunctionFS it is `io_cancel` on a request the controller may be half-way through: that it dequeues cleanly, and what the host then reads, is the plan's `flow` (a) to (d) on `dummy_hcd` and R4 on the phone.
- **One frame per read** from the OUT endpoint, the pad rule at 64, 512 and 1024, and the read buffer's size; `SOCK_SEQPACKET` gives boundaries for free.
- **The events.** `ENABLE`, `DISABLE`, `UNBIND`, `SUSPEND`, `RESUME` from ep0 in place of the two one-byte packets; that a host process killed with no unbind (F1, F2, fifty times) leaves the IN endpoint's frames in the order the tests here assume.
- **A daemon that dies with the function bound.** Here its end of the pair closes and the host sees the link go. What a host sees when FunctionFS loses its daemon, and that `remove` then `add` by the next start (E3) brings a clean function back.
- **The gadget script itself**, its hang marker, and that E3's record and the script's own marker agree.
- **The real programs**: `systemctl show`'s answers for the two units as the daemon reads them (`ActiveState`, and `ActiveEnterTimestampMonotonic` for the display manager), `systemd-run` with the worker's properties, first boot's real log.
- **Sizes.** The tests' frames carry 1024 bytes and the image is 9 KiB; nothing here has moved a megabyte frame or a 4 GB file, or measured the time a whole-file digest holds a worker thread.
