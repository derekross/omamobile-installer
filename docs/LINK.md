# The install link's protocol crate

Status (2026-11-13): **built and tested in memory only. Nothing here has opened a USB device or run on a phone. The installer uses the host machine after the write (DESIGN §14), against the `sim` stand-in for the daemon.** This is stage L1 of Omarchy Mobile's [install link plan](https://github.com/derekross/omarchy-mobile/blob/main/docs/INSTALL-LINK-PLAN.md) ("the plan" below): the protocol a freshly installed phone and the installer speak over USB at the phone's first start, as a library both sides build on.

`link/` is the crate `omamobile-link`. It lives in this repository as its own crate with no dependency on the core (the plan's decision Q8); the phone's daemon in omarchy-mobile will pin a tag of it. The plan's §5 is the specification, as corrected on 2026-11-12 for what this crate's first version found (its §15 and §16, decisions N1 to N4). **This is the second version: it implements the corrected plan, the fixes of the seven flaws F1 to F7 among them (§7 below).** Where it still differs from the plan, or chose where the plan is silent, §6 says so.

Protocol 1 is not fixed yet. It becomes fixed at this crate's first tag (decision N4); the golden frames were written anew once for this version.

## 1. What it is

| Module | What |
| --- | --- |
| `frame` | The 56-byte header, the pad byte, the limits: `encode`, and `decode`, which takes any bytes and names what is wrong (`FrameError`) |
| `json` | The crate's own JSON reader and writer: one spelling, a key given twice refused, numbers kept as their digits |
| `msg` | Every message of protocol 1 as a type (`HostFrame`, `PhoneFrame`), written one way and read strictly (`MsgError` names the message and the field) |
| `answers` | The owner's answers, checked by `omamobile-firstboot`'s rules (`Answers::validate`), and written as the `firstboot.conf` candidate (`ValidAnswers::firstboot_conf`) |
| `phone` | The phone's state machine (`Phone`), its state file (`Persistent`), what the daemon found at its start (`Found`) and the start-up rules that square the two (`reconcile`), what the machine asks of the daemon (`Effect`), and `reopen` |
| `host` | The computer's state machine (`Host`), what the install flow asks of it (`Command`), what it asks of the installer (`HostEffect`) and what it tells the flow (`Event`) |
| `digest` | SHA-256, hex, the serial proof, a comparison whose time does not depend on where two values differ |
| `sim` (feature `sim`, off in a shipping build) | For tests: both shells with their worlds in memory (`SimPhone`, with the files a start of the daemon looks at; `SimHost`), and a loopback between the two machines that can drop, repeat, swap or damage a frame, cut the link, stop the host's reads, or pause so a machine can be replaced (`Loopback`) |

What it is not:

- **No I/O.** It opens nothing, reads no clock and draws no random number. The caller moves the bytes, tells the time (`now`, milliseconds on a clock that does not go back and does not follow the wall clock) and supplies randomness (`Entropy`, to both machines), so every test is the same every run.
- **No `unsafe`** (`#![forbid(unsafe_code)]`), no async runtime, no thread.
- **One dependency:** `sha2` 0.11, which the workspace already uses, for the frame digest, the token's stored form and the serial proof. The tests add `serde_json` (to check this crate's JSON reader against a general one) and the crate itself with `sim` turned on.
- **No way to send bytes of a caller's choosing.** A host command is one of a closed list (`Command`), and each becomes typed messages (the plan's §7.1).

It builds for the host, for `wasm32-unknown-unknown` (the web page), for **`aarch64-unknown-linux-gnu` (the phone)** and for the Windows and macOS targets the toolchain here has. `cargo check -p omamobile-link --target aarch64-unknown-linux-gnu` passes and CI runs it, beside the check for the web page. That is a check, not a run: the tests have run on x86-64 only. The crate uses no operating-system interface, so nothing is expected to differ; the plan has the daemon's package run them natively in the aarch64 build container (its §11.1).

## 2. How the phone's daemon uses it

```
FunctionFS                         omamobile-linkd (the shell)                    Phone (no I/O)
----------                         ---------------------------                    --------------
                                   reads what is on the phone into Found    -->   Phone::start(cfg, &found, now, rng)
                                   does each output in order; Effect::Offer <--     squares the state file with it (§6.5),
                                     is "omamobile-link-gadget add"                 or ends the link before any gadget change
ENABLE / DISABLE, UNBIND     -->   PhoneInput::LinkUp / LinkDown            -->
one completed read from ep OUT -->  PhoneInput::Frame(bytes)                 -->   handle(now, input, rng)
a timer at phone.deadline()  -->   PhoneInput::Tick                         -->
the session agent's socket   -->   PhoneInput::AgentUp / AgentShow / ...    -->
a volume key                 -->   PhoneInput::Key                          -->
                                   does each output in order:               <--   Vec<PhoneOutput>
ep IN, one write per frame   <--     Send(bytes)
                                     Do(Effect): the screen, the agent, the
                                     state file, first boot, the partitions ...
                                   an effect's answer, when it has one      -->   PhoneInput::Done(ticket, Outcome)
```

1. **Start.** Fill in a `Found`: plain facts, each read without changing anything (this boot's `boot_id`; whether `done`, the marker, `firstboot.conf`, first boot's `done`, `boot-update.conf`, the pending checksums, a working directory, a candidate exist; the state file through `StateFile::read`; whether an account exists and whether it is the one first boot's `user` file names; first boot's unit in this boot; the image file's length; the provisioning unit and its result file). Make a `PhoneConfig` (the daemon's version, the codename, `androidboot.serialno`, the descriptor's `PROVISION_OPTIONAL`) and call `Phone::start`. Carry out what it returns like any other output list, in order. It ends in `Effect::Offer` when this boot offers the link; when it does not, the list says why (`Finish`, `Expire`, `Refuse`, `DeleteMarker`), `phone.stopped()` is true and the gadget is never touched. **The plan's §6.5 is `phone::reconcile`: the daemon applies no table of its own.**
2. **The loop.** Feed one input, get a list, carry it out *in order*, then feed the next. A `Send` is one write to the IN endpoint. An effect without a ticket (`Screen`, `Persist`, `FactoryWrite`, ...) must succeed before the next item is touched: if it cannot, stop working through the list and feed `PhoneInput::Fault`. That order is what makes the plan's "recorded on disk before the reply that announces it" true, and it is §6.5's write order: `Persist` always comes before the `Send` of the reply; the state says `provisioned` before the image is deleted; it says `finished` before `finished` is sent.
3. **Effects with a ticket** (`ReadFacts`, `CheckAnswers`, `StartFirstboot`, `BackupList`, `BackupDigest`, `FactoryDigest`, `Smoke`, ...) have an answer, which comes back as `PhoneInput::Done(ticket, outcome)`, at once or minutes later. The machine goes on answering `ping`, `status` and a new `hello` meanwhile, sends `progress` every five seconds to the session that waits, and tells every other step `busy`. An answer for a session that has ended is applied where it changed something (a verified partition, a started first boot) and is not sent to the next session.
4. **Jobs that outlive a session** report by themselves: `PhoneInput::FirstbootEnded`, `Progress`, and `ProvisionEnded { result }`, which carries the worker's result file as it is (`None`: it left none). The machine judges it: a run succeeds when every required step is `ok`; the optional steps that failed are kept for `status`.
5. **The owner at the phone.** `PhoneInput::Key`: a volume key while the first-start screen is up. `AgentUp` / `AgentDown`: the session agent connected to the daemon's socket, or left. `AgentShow` / `AgentHide`: its `show` and `hide`. `SessionUp`: three minutes after the display manager started with no agent yet. The machine answers with `Effect::Agent(AgentLine)`, whose `line()` is the text the socket carries (`open 2 600`, `code DIGITS 600`, `locked`, `paired`, `closed 2`, `ended finished`). The line with the code is for the socket and nothing else.
6. **The clock.** `phone.deadline()` is when the next `Tick` is due (a lock ending, a session or a stream gone quiet, `progress` owed, the session's code window, the ten unpaired minutes of the plan's §2.2). No tick is needed before it. `Effect::Close` closes the link for this boot.
7. **Read buffer.** `frame::read_buffer_len(config.max_data)`; one frame per completed read.
8. **Flow control is the shell's (the plan's §5.7).** The machine has no notion of a write that blocks. The daemon must: read the OUT endpoint independently of its writes to the IN endpoint; keep at most 8 frames queued and stop reading while 8 wait; give a write 30 s and then cancel what it queued and feed `PhoneInput::LinkDown`; and on `Effect::DropUnsent` (a new `hello` ended a session) cancel that session's frames the bus has not taken, before it writes the answer.
9. **Never log** a payload, `Debug` output of a message, an `AgentLine`, or an `Effect` as a whole. `Effect::Note` carries what the journal may hold: the type of a message, or what was wrong with a frame. `msg::log_type(payload)` gives "type (length)" for anything else.

What each effect expects is in its documentation in `link/src/phone.rs`. Those that carry rules of the plan:

- `CheckAnswers { conf }`: the text is the `firstboot.conf` candidate. Write it mode 600 on a tmpfs, run `omamobile-firstboot --check --porcelain` on it, and answer with `answers::parse_porcelain` of what it printed (`Outcome::Failed` if that is `None`).
- `StartFirstboot { retry: false }`: in §6.5's order, `boot-update.conf` from the descriptor (Q3), the candidate copied to `/etc/omamobile/firstboot.conf`, the unit started. The machine records stage `firstboot` only after the answer `Done`, and sends `answers-ok` after that. A daemon killed anywhere in between is put right by the next start (§7, F7).
- `BackupList`: the checks of `tools/nv-backup`. The answer names each partition's kernel device (`PartFound::dev`); the machine compares the listing at `backup-commit` with the first and refuses a partition that moved.
- `FactoryWrite { offset, data, sync }`: with `sync`, the file is synced before the list goes on. The `Persist` and the `ack` behind it rely on that.
- `Finish`, `Expire { missing }`, `Refuse { reason }`: the three ways the link ends; each says what it deletes and what `done` gets. `Expire` keeps the state file as it was just written (the stage, no token) and the pending checksums, which is what `omamobile-install-link reopen` starts from: `phone::reopen(&state)` gives the state file that grants one more setup boot.

## 3. How the installer uses it

```
install flow (core)                the installer's shell                          Host (no I/O)
-------------------                ---------------------                          -------------
"pair with this code", "send the   HostInput::Command(..)                   -->
 answers", "back up", ...                                                         handle(now, input, rng)
the link's interface found, gone   HostInput::LinkUp / LinkDown             -->
one completed read from ep IN      HostInput::Frame(bytes)                  -->
a timer at host.deadline()         HostInput::Tick                          -->
                                   does each output in order:               <--   Vec<HostOutput>
ep OUT, one write per frame  <--     Send(bytes)
                                     Do(HostEffect): the backup's files,
                                     the image's bytes, the token
screens, stops               <--     Event(..)
                                   an effect's answer                       -->   HostInput::Done(ticket, HostOutcome)
```

- **`HostConfig`** carries the serial the install pinned in fastboot (compared through the serial proof, never sent) and a token from an earlier run if the work directory has one. `rng` is the operating system's source (`crypto.getRandomValues` in the page).
- **A run begins with `hello`, cleanly, whatever came before.** `hello` is frame 0 and carries a nonce drawn anew each time. Until a frame echoes that nonce the machine passes over everything it reads, whatever its `seq`, `re` or digest, and counts none of it as an error (`host.discarded()` says how many, for a log). So a second run of the installer needs no replug, and an answer the first run left on the bus is never taken for the second's.
- **Pairing is the machine's.** With a token it sends `resume`, whatever `hello` says about `pair` (a lock included: `resume` is never locked). Without one: `Event::NeedCode` when a code is showing, and it waits for `Command::Pair`; otherwise it tells the flow once what the owner can do (`NeedTap`: "Unlock your phone and tap Setup is not finished"; `Locked { wait_s }`, with 0: a volume key, or a tap on the notice; `NeedSession`: the owner's session is not up yet) and says `hello` again every five seconds until a code is there. It checks the serial proof, which is bound to both sides' nonces, before anything else is said, and stops for good when it differs (`Stop::WrongPhone`). A token the phone no longer takes is forgotten (`ForgetToken`) and a code asked for.
- **One step at a time.** `Command::Answers`, `AwaitFirstboot`, `Backup`, `Factory`, `Provision`, `Restart`, `Smoke`, `Finish` each run to one closing event (`AnswersAccepted`, `BackupDone`, `FactoryVerified`, ..., or `Refused { step, error }` with the phone's code, or `Failed { step }` when the shell could not do its part).
- **A lost link is the machine's too.** After a cut, a minute of silence while it waits for the phone, or a session the phone ended, it emits `Event::LinkLost`, says `hello` again when the link is there, pairs with the token and takes the step up from where the phone says it is: the answers go again only at stage `waiting`; a partition is read on from the length of the copy on disk; the image from the offset the phone answers; `provision` and `restart` look at `status` first. Three sessions in a row that the phone ended over the protocol stop it (`Stop::Protocol`).
- **The backup's files** are the shell's: `BackupOpen` (how long is the copy), `BackupWrite`, `BackupDigest` (read the whole copy back from disk and hash it: the digest the phone is asked to confirm is of what is stored, not of what passed by), `BackupDiscard`, and `BackupFinish` (write and sync `SHA256SUMS`, `partitions.txt` and `NV-BACKUP` as `tools/nv-backup` does). **`BackupFinish` has an answer, and `backup-commit` is sent only after it:** a backup the phone has recorded as done is whole on the computer.
- **The image's window.** `factory-ready` says `ack_every` and `window`. The machine never asks the shell for bytes that would end beyond the last `ack` it has *read* plus the window; at the edge it stops and waits, and a minute of that is a lost link. An `ack` that goes back or is past what was sent ends the session. A stream the phone suspended (it answers one late data frame `not-allowed`, with no `re`) is begun again with `factory-begin`, from the offset the phone answers, with no strike.
- **`Event::ProvisionDone { steps, optional_failed }`**: `optional_failed` is filled whether the event was read or its end was learned from `status` in a later run, so "Cameras: not set up" does not depend on having been there.
- **What stays the shell's (the plan's §5.7, §7.1):** keep one read pending on the IN endpoint whenever a session is open, also while writing; give each write 30 s, and when one is not taken cancel it and feed `LinkDown`, then `LinkUp`; keep the token in the work directory (mode 600) natively, in memory only in the page.

## 4. The messages

Every message of the plan's §5, and the type that is it. `id` (host, from 1) and `re` (phone) are fields of `HostFrame` and `PhoneFrame`.

### Host to phone (`HostMsg`)

| Message | Type | Fields | Allowed (as the phone machine enforces it) |
| --- | --- | --- | --- |
| `hello` | `Hello(HostHello)` | `proto` 1 to 16 numbers of 1 to 65535, no number twice; `installer` {`name`, `version`}: 1 to 64 printable ASCII; `max_data` 1024 to 4194304; `nonce` 64 lower-case hex digits, new for every `hello` | Any time; always frame 0; ends the session before it |
| `ping` | `Ping` | | In a session |
| `pair` | `Pair { code }` | `code`: six ASCII digits (`Code`) | Unpaired, a code is shown, not locked |
| `resume` | `Resume { token }` | `token`: 64 lower-case hex digits (`Token`) | Unpaired, the phone holds a token. Never locked |
| `bye` | `Bye` | | In a session |
| `status` | `Status` | | In a session, paired or not |
| `answers` | `Answers(Answers)` | `username`, `password_hash`, `ssh_keys` (a list, may be empty); optional `credential`, `hostname`, `timezone`, `wifi_ssid`, `wifi_psk`, `full_name`, `email`. Values are judged by `Answers::validate` | Paired, stage `waiting` |
| `firstboot-retry` | `FirstbootRetry` | | Paired, stage `firstboot-failed` |
| `backup-list` | `BackupList` | | Paired, stage before `provisioned` |
| `backup-read` | `BackupRead { name, offset }` | `name`: 1 to 36 of `A-Z a-z 0-9 _ -`; `offset`: a multiple of the session's `max_data`, at most the size | After `backup-parts`, a listed name |
| `backup-verify` | `BackupVerify { name, sha256 }` | `sha256`: 64 lower-case hex digits | After `data-end` for that name |
| `backup-commit` | `BackupCommit` | | Every listed partition verified |
| `abort` | `Abort` | | A stream is open |
| `factory-begin` | `FactoryBegin(FactoryBegin)` | `size` 1 to 2^53-1 (over 8 GiB is answered `too-large`); `sha256`; `build` 1 to 64 printable ASCII; `name` 1 to 255 bytes, no control character | Paired, stage before `provisioned`, no stream open, provisioning not running |
| `factory-end` | `FactoryEnd` | | The image's stream open and every byte sent |
| `provision` | `Provision` | | Paired, stage `firstboot-done`, `backup-done`, image verified, on the charger or battery at least 30 % |
| `restart` | `Restart` | | Paired, stage `provisioned`, `boot` 1, once |
| `smoke` | `Smoke` | | Paired, stage `provisioned`, `boot` 2 |
| `finish` | `Finish` | | Paired, stage `provisioned` |

### Phone to host (`PhoneMsg`)

| Message | Type | `re` | Fields |
| --- | --- | --- | --- |
| `hello` | `Hello(PhoneHello)` | yes | `proto`, `link`, `device`, `max_data`, `nonce` (64 hex, new per session), `host_nonce` (the host's, echoed), `stage`, `pair` (`PairMode`: `code`, `ask`, `token`, `locked`), `wait_s` |
| `pong` | `Pong` | yes | |
| `paired` | `Paired { token, serial_proof }` | yes | `token` after `pair` only; `serial_proof` (64 hex): SHA-256("omil-serial-v1" ‖ the host's nonce ‖ the phone's nonce ‖ the serial) |
| `bye` | `Bye` | yes | |
| `status` | `Status(Box<Status>)` | yes | `stage`, `boot` (1 or 2), `release`, `kernel`, `device`, `firstboot` {`state`, `step`, `n`, `of`, `failed`}, `backup` {`state`, `parts`: [{`name`, `size`, `verified`}]}, `factory` {`state`, `have`, `size`}, `provision` {`state`, `step`, `n`, `of`, `optional_failed`}, `link` {`boots_left`, `closes_s`}, `battery`, `charging`, `free` |
| `answers-ok` | `AnswersOk` | yes | |
| `backup-parts` | `BackupParts { parts }` | yes | `parts`: at most 32 of {`name`, `size`, `checked`} (`BackupPart`) |
| `data-begin` | `DataBegin { name, size, offset }` | yes | |
| `data-end` | `DataEnd { name }` | no | |
| `backup-verified` | `BackupVerified` | yes | |
| `backup-done` | `BackupDone` | yes | |
| `aborted` | `Aborted` | yes | |
| `factory-ready` | `FactoryReady { offset, ack_every, window }` | yes | `ack_every` at least 1; `window` at least `ack_every` |
| `ack` | `Ack { offset }` | no | |
| `factory-verified` | `FactoryVerified` | yes | |
| `provision-started` | `ProvisionStarted` | yes | |
| `progress` | `Progress(Progress)` | no | `what` (`What`: `factory-check`, `provision`, `backup-verify`, `smoke`, `working`), `step`, `done`, `total` |
| `provision-done` | `ProvisionDone { steps }` | no | `steps`: at most 64 of {`step`, `state`: `ok`, `failed`, `skipped`} (`StepResult`) |
| `restarting` | `Restarting` | yes | |
| `smoke-report` | `SmokeReport { report }` | yes | `report`: any JSON value, as the smoke test wrote it |
| `finished` | `Finished` | yes | |
| `error` | `Error(ErrorMsg)` | either | `code`, `message`, `stage`, and what the code carries |

`error.code` is `ErrorCode`; with their extra fields: `protocol`, `version` {`proto`, `host_nonce`}, `unknown`, `not-allowed`, `not-paired`, `bad-code` {`left`}, `locked` {`wait_s`}, `bad-token`, `problems` {`problems`: [{`field`, `reason`}]}, `busy`, `backup-precondition` {`parts`}, `backup-mismatch`, `no-space` {`need`, `free`}, `too-large`, `factory-mismatch`, `provision-failed` {`step`}, `firstboot-failed` {`steps`}, `battery`, `internal`. `ErrorCode::text()` is the fixed English text for a log. An `error` has no `re` when it ends a session (`protocol`, `internal` after a fault), ends a long step (`provision-failed`) or answers a data frame that met no open stream (`not-allowed`).

Data frames are not messages: host to phone they are the factory image (`Effect::FactoryWrite`), phone to host a radio partition (`HostEffect::BackupWrite`).

What is counted toward the pairing lock: a wrong code, and nothing else (the plan's §2.5). The fifth, tenth and fifteenth replace the code and lock `pair` for 30, 60 and 120 s; the twentieth locks it until someone acts at the phone (`Key`, or `AgentShow`), which sets the count to 0.

## 5. The tests, and the golden frames

Everything runs with `cargo test`, in memory. No test opens a USB device or a file outside `link/golden/`, except the one named below when it is asked to.

| File | What it shows |
| --- | --- |
| `tests/frames.rs` | The header byte for byte; the pad rule at every length to 4096 and at the limits; every frame error by name. Fuzzing: every truncation of 13 frames; every single-bit flip of the header (448 bits) and of the payload of four frames; 200000 random and half-random buffers; 10368 headers built field by field against the rules written out again |
| `tests/json.rs` | The reader against `serde_json` on 50000 generated texts (what this crate takes, the general reader takes as the same value; what only the general one takes is a key given twice); 300000 damaged inputs |
| `tests/messages.rs` | Every message written and read back; for every message and every field: an unknown key, a missing key, another type, `null`, also inside nested objects; limits and forms field by field; 200000 damaged messages through both readers |
| `tests/answers.rs` | 204 cases of first boot's rules, each with the line it comes from. With `OMAMOBILE_FIRSTBOOT_BIN=/path/to/omamobile-firstboot` set, every case is also given to the real script (`--check`, in a scratch root) and the verdicts compared |
| `tests/turns.rs` | The phone's machine by hand: all 19 host messages in 17 states against a table written from the plan's "Allowed" column, with "nothing changed" checked for each refusal; 23 kinds of wrong frame in four states, each ending the session and counting nothing; a data frame with no open stream; the image's stream; timeouts; the state file across a daemon restart |
| `tests/host_turns.rs` | The host's machine by hand: every phone message, with the right `re`, a wrong one and none, in ten states; what answers `hello` (the nonce, not `re` or `seq`); the window and its acknowledgements; a suspended stream; the backup's files before the commit; silence; `busy`; waiting for the owner |
| `tests/flow.rs` | Both machines through the loopback: the straight path from the code to `finished`; a cut before every frame of the image's transfer, each way, and before every frame of a whole install; a dropped, repeated, swapped, shortened and bit-flipped frame each way; twenty wrong codes and a volume key; a wrong image digest; a wrong partition checksum; a partition that moved; version mismatch each way |
| `tests/fixes.rs` | The proofs the plan's §11.3 names, below: `rerun`, `stale`, `pair-again`, `rights`, `count`, the camera-only failure, the window |
| `tests/startup.rs` | `rows`, `budget` and `kill`, below |
| `tests/walk.rs` | Both machines walked at random, 180000 and about 156000 steps, with a shell that sometimes answers anything to anything and restarts that find the files of §6.5's third table as chance left them: no panic, every frame sent is one the other reader takes, the stage never goes back, every stage and every phone message is reached, and what `hello` says can pair is so |
| `tests/golden.rs` | The golden frames, and their text form |

**No input found makes the decoder, the JSON reader, the message readers, the start-up rules or either machine panic.** That is a statement about the inputs above, not a proof.

### The seven flaws: what proves each fix here, and what is left

The plan's §11.3 names a test for each fix at stage L2, against the daemon. These are the same tests against the machines, with the stand-in phone's files in memory where L2 has a scratch root.

| | The proof here (`cargo test -p omamobile-link`) | Left for L2 (a daemon), L3 (real FunctionFS) or the phone |
| --- | --- | --- |
| F1 | `fixes::rerun_an_installer_killed_at_any_frame_pairs_again_with_the_cable_untouched`: an install is stopped after each of its frames in turn (61 points, from the first `hello` to `provision`), the host's machine is replaced by a new one with the link left up and the frames under way left under way; the new one is paired within two frames (`hello` and `resume`, or `hello` and `pair` where it died before it had a token), neither side sees an error, no wrong try is counted, and the install goes on to its end. Also `turns::hello_starts_the_numbering_again_both_ways` | L3: the same with the host *process* killed and no unbind, 50 times. R6, R9 |
| F2 | `fixes::stale_answers_of_another_run_are_passed_over_and_the_proof_holds`: the link holds the answer to another run's `hello` with the same `id` 1, and three later frames, before the new `hello` is answered: four frames passed over, the proof equal, with a token and with a code. `fixes::stale_the_serial_proof_is_bound_to_both_nonces`: the proof computed from the plan's formula, not from the crate's function. `host_turns::only_the_frame_that_echoes_the_nonce_answers_hello` | L3: the host killed between writing `hello` and reading, 50 times: that a real IN endpoint hands such frames over in the order this test assumes |
| F3 | `fixes::pair_again_after_first_boot_with_the_token_lost`: at each stage from `firstboot` on, the host loses its token; `token` or `ask`, the tap, the code in the agent's line and nowhere else, a new token, the old one `bad-token` and forgotten. `fixes::rights_of_a_paired_host_at_every_stage_with_an_account`: every host message at five stages (both values of `boot`) against a table written from §2.7, paired and unpaired, `restart` once. `startup::budget_*`: ten unpaired minutes close a boot, to the millisecond, and never while a job runs or a step owes an answer; three setup boots, the fourth start ends as `expired` or `finished` with no `Offer`; `reopen` gives one boot and only after `expired` | L2: the agent, its socket and `SO_PEERCRED`; `omamobile-install-link status\|reopen`; that the unit's condition keeps a link that ended from starting. R5b (the lock screen), R9, R10 |
| F4 | `fixes::count_only_wrong_codes`: 200 protocol errors, 100 wrong tokens, 3 mismatched images and a data frame after 121 s leave the count at 0 and the right code pairs. `fixes::count_twenty_wrong_codes_lock_until_an_act_and_never_lock_resume`: 30, 60 and 120 s and then the lock, at the first-start screen and in the session; `resume` pairs under each lock; the right act clears it and the wrong one does not | R5 (the volume keys can be read), R9 |
| F5 | `fixes::a_camera_only_failure_is_a_success_with_a_note`; in `flow.rs`, a later run and a later boot still get `optional_failed` from `status`; `provision` at `provisioned` is `not-allowed` | L4: the worker's own one retry of the camera step, and the image really gone. L6: the installer's line |
| F6 | `host_turns::the_host_keeps_to_the_window`: the host machine stops exactly at the last acknowledgement read plus the window, goes on by exactly what an `ack` says, and takes a minute at the edge as a lost link. `fixes::window_a_host_that_does_not_read_stalls_itself_and_goes_on_when_it_reads`: through the loopback with the host's reads stopped, the transfer stalls at the window with two frames waiting on the phone's side, finishes when the host reads five seconds late, and resumes from the acknowledged offset after a lost link | **Everything about blocking.** An in-memory link never holds a writer back, so it cannot show the deadlock F6 is about, nor that the fix ends it: that the phone's write of `ack` blocks while the host does not read; the bound of 8 queued frames and the stop of reads behind it; the 30 s per write; `io_cancel` dequeuing cleanly; `abort` during a partition's stream with the host's reader stopped. That is L3's `flow` (a) to (d) on `dummy_hcd`, and R4 on the phone's own controller |
| F7 | `startup::rows_*`: every row of §6.5's three tables as files and what the machine has the daemon do to them; `rows_squared_once_is_squared`: 20000 phones with files at random, each squared in one pass. `startup::kill_the_daemon_after_every_single_output_of_a_whole_install`: the stand-in daemon is killed before each of the 163 outputs of a whole install in turn (each effect and each frame written), started again from the files, and the install reaches `finished` every time, with the answers taken once, one restart, and `done` never written early. `startup::kill_between_placing_the_answers_and_recording_the_stage`: the case itself | L2 `rows` and `kill` against the real daemon in a scratch root: that it fills in `Found` rightly, and that each write is a temporary file, synced and renamed. R9's forced restart during first-boot setup |

### Guards removed, one at a time

After the change, each of these 37 guards was taken out of the source alone and the tests run (`cargo test -p omamobile-link --tests`); each time a test failed. One survived at first (the serial proof computed without the host's nonce: both sides use the same function, so neither noticed) and got its own test.

| | The guard | Caught by |
| --- | --- | --- |
| F1 | The phone's `hello` answer is its frame 0 | `fixes::count_only_wrong_codes` and 3 more |
| F1 | A `hello` is taken in an open session, not only as its first frame | `fixes::stale_...`, `count_twenty_...` |
| F2 | The host takes only a `hello` that echoes its nonce | `fixes::stale_...`, `rerun_...` |
| F2 | ... and only as frame 0 | `host_turns::only_the_frame_that_echoes_the_nonce_answers_hello` |
| F2 | The host checks the serial proof | `fixes::stale_...` |
| F2 | The proof covers the host's nonce | `fixes::stale_the_serial_proof_is_bound_to_both_nonces` |
| F3 | `restart` only at `boot` 1 | `fixes::rights_...` |
| F3 | `restart` once | `fixes::rights_...` |
| F3 | `answers` at `waiting` only | `fixes::rights_...` |
| F3 | The code window opens only for the agent, off the first-start screen, unpaired | `fixes::the_sessions_code_lasts_...` |
| F3 | The ten minutes do not run while a host is paired | `startup::budget_the_ten_minutes_...` |
| F3 | ... nor while a job runs | the same |
| F3 | ... nor while a step owes an answer | the same |
| F3 | ... nor before an account exists | the same |
| F3 | The budget of setup boots ends the link | `startup::budget_three_setup_boots_then_the_end`, `budget_reopen_...` |
| F3 | A boot is counted once, not at every start of the daemon | `startup::budget_three_...` and 2 more |
| F3 | `reopen` only when the budget is spent | `startup::budget_reopen_gives_one_more_setup_boot` |
| F4 | `pair` is refused while locked | `fixes::count_twenty_...` |
| F4 | The twentieth wrong code locks until an act | the same |
| P4 | The agent is told `locked`, not `open`, after the twentieth | the same |
| F4 | A late data frame is answered once | `turns::a_data_frame_with_no_stream_...` |
| F4 | A volume key counts only at the first-start screen | `fixes::count_twenty_...` |
| F5 | A required step that is not `ok` fails the run | `fixes::a_camera_only_failure_...` |
| F5 | A run that left an empty result is not a success | `startup::rows_provisioning_that_was_running` |
| F6 | The host sends nothing beyond the window | `fixes::window_a_host_that_does_not_read_...` |
| F6 | An `ack` moves the window only forward, never past what was sent | `host_turns::the_host_keeps_to_the_window` |
| F6 | A window that cannot be left is refused by the host | the same |
| F6 | ... and by the reader (`window` at least `ack_every`) | `messages::limits_and_forms_of_the_phones_fields` |
| F6 | The phone never announces such a window | `fixes::window_is_what_factory_ready_says_...` |
| F7 | Row 1: `done` is the end | `startup::rows_1_done_is_the_end` and 2 more |
| F7 | Row 3: no state file and something of first boot's is not a first start | `startup::rows_3_...` |
| F7 | Row 5: an unreadable state file with an account ends the link | `startup::rows_4_and_5_...` |
| F7 | A `firstboot.conf` with the state at `waiting` is `firstboot` (the flaw itself) | `startup::kill_between_...`, `rows_what_first_boot_left`, `kill_the_daemon_...` |
| F7 | An image file shorter than `synced` is emptied | `startup::rows_the_image_and_its_record` |
| F7 | A pending checksum file with no `backup-done` is deleted | `startup::rows_the_backups_mark_...` |
| F7 | A new boot is known from the boot id in the state file | `fixes::rights_...` |
| §16 | `backup-commit` waits for the backup's files | `host_turns::the_backups_files_are_written_before_the_commit` |

### The golden frames

`link/golden/` holds, for each frame, `NAME.bin` (the bytes of one bulk transfer), `NAME.hex` (the same bytes as text) and `NAME.txt` (which message it is, each header field, the payload, and the SHA-256 of the frame's bytes). There is one of every host message (20, `answers` twice), one of every phone message (44: `hello` three times, `status` twice, every error code), frames with the pad byte, a data frame with a 64-bit offset and the largest `seq`, and the longest message: 71 frames kept as bytes. The three data frames at the megabyte limits are kept as a description with the frame's SHA-256 and the rule that makes the payload, not as bytes.

**The text form is for omarchy-mobile**, whose pull-request gate refuses binary files: `NAME.hex` is lower-case hex, 32 bytes a line, each line ended by a newline, and nothing else, so one frame has one text. It is the form the plan's `tools/link-golden` writes (its §11.1), so that repository's copy of `NAME.hex` and `NAME.txt` can be compared with these files byte for byte. `tests/golden.rs` fails if a file is missing, extra or differs by a byte; reads every `.bin` back from disk as what its name says; turns every `.hex` back into bytes, compares them with the `.bin` beside it and with the digest in the `.txt`; and refuses a text form in any other spelling.

They are the contract the phone's daemon is checked against. **They were written anew once for this version, under the same protocol number (the plan's decision N4): no tag, release or phone has spoken the first version.** From the crate's first tag on, a change that makes this test fail has changed the wire, and that is a new protocol number, not a new golden file. `OMIL_GOLDEN_WRITE=1 cargo test -p omamobile-link --test golden` writes them.

## 6. Differences from INSTALL-LINK-PLAN

The first version listed twenty-six (D1 to D26). The plan adopted twenty-five of them and sent one clause back (`resume` under the lock), which this version follows; its §15 has the list. What is left is where the corrected plan is silent or, in two places (P2, P3), where building it as written would do something it does not mean. Section numbers are the plan's.

| | The plan | The crate | Why |
| --- | --- | --- | --- |
| P1 | §6.5 counts setup boots and sets `boot` to 2 "from the first boot that begins at `provisioned`", and does not say how a start knows that it is the first of a boot | The state file keeps the boot it was last squared in (`boot_id`, the kernel's `/proc/sys/kernel/random/boot_id` as `Found` gives it). A start that finds another one is a new boot. An empty one counts as new every time | "This boot was counted" and the count must be one write. With a marker of the daemon's own (under `/run`, say), a daemon killed between the marker and the state file loses the boot: after the planned restart `boot` stays 1 and `restart` is taken a second time, and the boot is not counted against the three. The `kill` test found exactly that. The id is a random number of the kernel's and never crosses the link |
| P2 | §6.5, third table, "at any stage before `finished`": "`backup-done`, no pending checksum file: takes the mark away: the backup is taken again" | Only before `provisioned` | Provisioning takes the pending file ("`/etc/omamobile/modem-nv.sha256` from the pending file", §6.3). If the worker moves it, every start at `provisioned` would take the mark away on a phone whose backup is whole and on the computer, where no backup can be taken again; `status` would then say `backup` `none` to the installer's final list. The plan should say that the worker copies, or stop the row at `provisioned` |
| P3 | §2.2: the ten minutes "run from the latest of: the owner's session being up, the end of the last paired session, and the end of first-boot setup or of provisioning" | They do not run before the session is up in this boot. At `firstboot-failed` the display manager is held back (§3.3), so no session comes, and the link stays open for the boot, with the code on the first-start screen; the three setup boots still end it | With no session there is no notice to say "paused", and the first-start screen would be left saying "Setup stopped at" on a phone whose link is gone and whose display manager never starts. The plan should say what a boot at `firstboot-failed` does |
| P4 | §6.5, row 4: a state file that cannot be read and no account: "As row 2" | As row 2, and then the tables like any other start: a `firstboot.conf` found there makes the stage `firstboot` | Row 2's own condition has no `firstboot.conf`. One on a phone with a (damaged) state file is the link's, and first boot is about to make an account from it |
| P5 | §5.3 and §16: the host says `hello` again every 5 s "while `pair` is `ask`, or `locked` with no token" | Also while it is `token` and the host has no token (`Event::NeedSession`) | A page reloaded after the planned restart, before the owner has logged in, sees `token`: the session is not up. Stopping there would tell an owner that nothing can pair when unlocking the phone is all it takes |
| P6 | §5.7: `window` is "twice that, 134217728" | The same number. But a reader refuses a `window` under `ack_every`, the host refuses one under `ack_every` plus one frame, and a phone configured with one raises it to that | With less, a host that keeps to the window can never reach the next acknowledgement. The plan's numbers are far above it |
| P7 | §5.6: `ack` {`offset`}; nothing on what a host does with a wrong one. §5.1: the host "sends `factory-begin` again" after the late-data `not-allowed` | An `ack` that does not go forward, or is past what was sent, ends the session as a protocol error. A suspended stream is begun again at most five times without an `ack` in between, then the step fails (`Event::Failed`), with no strike; `factory-end` that met a suspended stream begins again the same way | The acknowledgement is the window's base and the resume point: a wrong one must not move either. A phone that suspends every transfer at once is not a protocol error, and not something to retry for ever |
| P8 | §5.6, §6.5: a run "succeeds when every required step is `ok`"; a result file | A result that lists no step, or none at all, is a failure at `interrupted`. A required step that is `skipped` fails the run at that step | Read as written. The plan does not say what `skipped` is for |
| P9 | §5.3: "The daemon cancels the old session's frames that the bus has not yet taken" | The machine says when: `Effect::DropUnsent`, before the answer to a `hello` that ended a session | The daemon does not read messages; without the effect it would have to |

Smaller choices, each where the plan says nothing: `id` and `re` are from 1, and an unknown message with `id` 0 is answered with no `re`. A `pair` answered `not-allowed` (the code went away between `hello` and `pair`) makes the host say `hello` again, with no strike. The agent's `show` is passed over while a host is paired, and a host that pairs, by a code or by `resume`, closes the window. "Verified, and the file's length is not `size`" takes the mark away from a file that is too long too, before it is cut. `restart` is "once" in memory: a daemon that is started again in `boot` 1 before the phone restarted takes it again, which is what lets a daemon killed between `restarting` and the restart finish the job.

## 7. What became of the seven flaws

The first version reported seven places where the plan as written led somewhere it did not mean to go. The corrected plan fixes each (its §11.3); this is where each fix is in the crate. §5 has the tests.

| | The flaw | The fix, here |
| --- | --- | --- |
| F1 | The numbering could not start again without the cable | `Phone::on_frame`: a `hello` is taken as frame 0 at any time, ends the session before it and restarts both directions' numbering. `Host::say_hello` |
| F2 | An old `hello` answer could be taken for a new one | `HostHello::nonce`, `PhoneHello::host_nonce`, `ErrorCode::Version::host_nonce`; `Host::on_frame` passes over everything until its nonce comes back; `digest::serial_proof` takes both nonces. `HostConfig::first_id` is gone |
| F3 | After first boot a lost token could not be replaced, and the link never ended | `PairMode::Ask`; `PhoneInput::Agent*` and `Effect::Agent`; a new token on every right code. `Persistent::setup_boots` and `boots_allowed`, counted in `reconcile`; `Phone::close_at` (the ten minutes) and `Effect::Close`; `Effect::Finish` and `Effect::Expire` as different ends; `phone::reopen`. `restart` at `boot` 1 once. `Status::link_*`. The host's `NeedTap`, `NeedSession` and five-second `hello` |
| F4 | A wrong try was counted for things an honest installer does | `Phone::wrong_code` is called for a wrong code and nothing else; `resume` has no lock; a data frame with no stream is `Note::LateData` and `not-allowed` once; `Lock::Act` and `PhoneInput::Key` / `AgentShow` |
| F5 | `provision` could not be run again for the camera step | `PhoneConfig::optional`, `Persistent::optional_failed`, `Status::provision_optional_failed`, `Event::ProvisionDone::optional_failed` |
| F6 | The acknowledgement could stop the transfer on real USB | `FactoryReady { ack_every, window }`; `Phase::FWait` in the host. The rest is the two shells' (§2 item 8, §3) |
| F7 | A crash between writing `firstboot.conf` and recording the stage ended the link for good | `phone::Found` and `phone::reconcile`, called by `Phone::start`; the state file from the first start; `Effect::Persist` before `Effect::Offer` |

Two more things in the corrected plan look wrong beside P1 to P3 above; neither is the crate's to settle:

- **The agent's socket had no line for a lock.** §2.8 now has one (the plan's P4, accepted): after the twentieth wrong code the crate sends `locked` (`AgentLine::Locked`) where it sent `open`: the window closes, the notice says why, and a tap (an act at the phone) shows a new code. An agent that connects while that lock holds is told `locked` too. After the fifth the window gets the new code, and the installer says the wait.
- **`reopen` "takes effect at the next boot"** (§2.8) is the unit's to keep, not the state file's. If the daemon is started in the boot `reopen` ran in, it finds a boot it has not counted, counts it, and offers the link at once: one boot is still all that is granted, but the gadget changes in a running session.

## 8. What stage L2 will need that is not here

- **The tag.** The crate is at the workspace's 0.0.1 with no tag. The plan has L2 start from `omamobile-link-1.0.0`, which fixes protocol 1.
- **The golden frames there.** `link/golden/*.hex` and `*.txt` are the files to copy; `tools/link-golden` and the `GOLDEN` index (the tag, its commit, each frame's SHA-256) are omarchy-mobile's. Each `.txt` already carries its frame's SHA-256.
- **`omamobile-firstboot --check --porcelain`** printing the reason words (`answers::reason`). `answers::parse_porcelain` reads it. The 204 cases run against the script with `OMAMOBILE_FIRSTBOOT_BIN`.
- **`Found`, filled in rightly.** The start-up rules are in the crate; what they are given is the daemon's: each field's documentation in `link/src/phone.rs` names the file or the fact. Three need care: `account` (a uid of 1000 or more, compared with the name in first boot's `user` file); `firstboot_unit` (`NotRun`, `Running` or `Failed` with the steps its log marks `FAILED`, *in this boot*); `boot_id` (never empty if it can be helped: an empty one spends a setup boot at every start).
- **Every write as §6.5 says:** a temporary file in the target's own directory, synced, renamed, the directory synced. The machine gives the order; the daemon gives the durability.
- **The I/O rules of §5.7** (§2 item 8 above), the sleep inhibitor (held from `Effect::Offer` until `Close`, `Finish` or the daemon's end), and FunctionFS's events: `ENABLE` is `LinkUp`, `DISABLE` and `UNBIND` are `LinkDown`, `SUSPEND` and `RESUME` nothing.
- **What `StartFirstboot { retry: false }` does**, in order: `boot-update.conf` (Q3), the candidate copied (it is on a tmpfs: not renamed), the unit started.
- **Numbers the plan leaves to the runbook:** the working space `FactoryOpen` checks (§6.4, R1), which the shell adds to the bytes still to come.
- **The session agent and `omamobile-install-link status|reopen`.** The agent's lines are `AgentLine::line()`. `reopen` is `phone::reopen` on the state file an `expired` end left, after checking that `done` says `expired`; it then deletes `done` and puts the marker back.
- **A transport for its tests.** A socket pair that keeps message boundaries (`SOCK_SEQPACKET`) stands in for bulk transfers as they are: one frame per read. It does not block as USB does.
- **The words.** `Effect::Screen` names what to show; the sentences are the screen program's. `Effect::Note` names what happened; the journal's lines are the daemon's. From stage `firstboot-done` on the first-start screen is gone (`Screen::Off`) and the daemon passes `Screen` effects over.

For stage L6 (the installer): a `Link` transport that yields one frame per read, keeps a read pending while it writes and gives each write 30 s; the flow's states mapped onto `Command` and `Event`, `NeedTap` and `NeedSession` among them; the operating system's randomness as `Entropy`; the token's place in the work directory; and `BackupFinish` as a step that can fail.
