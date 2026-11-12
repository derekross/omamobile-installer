# The install link's protocol crate

Status (2026-11-12): **built and tested in memory only. Nothing here has opened a USB device, run on a phone or been used by the installer yet.** This is stage L1 of Omarchy Mobile's [install link plan](https://github.com/derekross/omarchy-mobile/blob/main/docs/INSTALL-LINK-PLAN.md) ("the plan" below): the protocol a freshly installed phone and the installer speak over USB at the phone's first start, as a library both sides build on.

`link/` is the crate `omamobile-link`. It lives in this repository as its own crate with no dependency on the core (the plan's decision Q8); the phone's daemon in omarchy-mobile will pin a tag of it. The plan's §5 is the specification. Where the crate had to differ from it, or choose where it was silent, §6 below says so, point by point, so the plan can be corrected.

## 1. What it is

| Module | What |
| --- | --- |
| `frame` | The 56-byte header, the pad byte, the limits: `encode`, and `decode`, which takes any bytes and names what is wrong (`FrameError`) |
| `json` | The crate's own JSON reader and writer: one spelling, a key given twice refused, numbers kept as their digits |
| `msg` | Every message of protocol 1 as a type (`HostFrame`, `PhoneFrame`), written one way and read strictly (`MsgError` names the message and the field) |
| `answers` | The owner's answers, checked by `omamobile-firstboot`'s rules (`Answers::validate`), and written as the `firstboot.conf` candidate (`ValidAnswers::firstboot_conf`) |
| `phone` | The phone's state machine (`Phone`), its state file (`Persistent`) and what it asks of the daemon (`Effect`) |
| `host` | The computer's state machine (`Host`), what the install flow asks of it (`Command`), what it asks of the installer (`HostEffect`) and what it tells the flow (`Event`) |
| `digest` | SHA-256, hex, the serial proof, a comparison whose time does not depend on where two values differ |
| `sim` (feature `sim`, off in a shipping build) | For tests: both shells with their worlds in memory (`SimPhone`, `SimHost`), and a loopback between the two machines that can drop, repeat, swap or damage a frame or cut the link (`Loopback`) |

What it is not:

- **No I/O.** It opens nothing, reads no clock and draws no random number. The caller moves the bytes, tells the time (`now`, milliseconds on a clock that does not go back) and supplies randomness (`Entropy`), so every test is the same every run.
- **No `unsafe`** (`#![forbid(unsafe_code)]`), no async runtime, no thread.
- **One dependency:** `sha2` 0.11, which the workspace already uses, for the frame digest, the token's stored form and the serial proof. The tests add `serde_json` (to check this crate's JSON reader against a general one) and the crate itself with `sim` turned on.
- **No way to send bytes of a caller's choosing.** A host command is one of a closed list (`Command`), and each becomes typed messages (the plan's §7.1).

It builds for the host, for `wasm32-unknown-unknown` (the web page) and for the Windows and macOS targets the toolchain here has. **`aarch64-unknown-linux-gnu`, the phone's target, was not checked: its standard library is not installed on the machine this was written on** (`aarch64-apple-darwin` was). The crate uses no operating-system interface, so nothing is expected to differ; stage L2 builds it for the phone and finds out.

## 2. How the phone's daemon uses it

```
FunctionFS                         omamobile-linkd (the shell)                    Phone (no I/O)
----------                         ---------------------------                    --------------
ENABLE / DISABLE, UNBIND     -->   PhoneInput::LinkUp / LinkDown            -->
one completed read from ep OUT -->  PhoneInput::Frame(bytes)                 -->   handle(now, input, rng)
a timer at phone.deadline()  -->   PhoneInput::Tick                         -->
                                   does each output in order:               <--   Vec<PhoneOutput>
ep IN, one write per frame   <--     Send(bytes)
                                     Do(Effect): the screen, the state file,
                                     first boot, the partitions, the image ...
                                   an effect's answer, when it has one      -->   PhoneInput::Done(ticket, Outcome)
```

1. **Start.** Read the state file (`Persistent::from_json`; none on a first start: `Persistent::default()`), make a `PhoneConfig` (the daemon's version, the codename, `androidboot.serialno`, whether the code can be drawn now) and call `Phone::start`. `new_boot` is false when the daemon starts again within one boot. Carry out what `start` returns like any other output list. A state file at stage `finished`, or a fourth boot at `provisioned`, gives `Effect::Finish` at once.
2. **The loop.** Feed one input, get a list, carry it out *in order*, then feed the next. A `Send` is one write to the IN endpoint. An effect without a ticket (`Screen`, `Persist`, `FactoryWrite`, ...) must succeed before the next item is touched: if it cannot, stop working through the list and feed `PhoneInput::Fault`. That order is what makes the plan's "recorded on disk before the reply that announces it" true: `Persist` always comes before the `Send` of the reply.
3. **Effects with a ticket** (`ReadFacts`, `CheckAnswers`, `BackupList`, `BackupDigest`, `FactoryDigest`, `Smoke`, ...) have an answer, which comes back as `PhoneInput::Done(ticket, outcome)`, at once or minutes later. The machine goes on answering `ping`, `status` and a new `hello` meanwhile, sends `progress` every five seconds to the session that waits, and tells every other step `busy`. An answer for a session that has ended is applied where it changed something (a verified partition, a started first boot) and is not sent to the next session.
4. **Jobs that outlive a session** report by themselves: `PhoneInput::FirstbootEnded`, `Progress`, `ProvisionEnded`, `DesktopUp`, `CodeScreen`.
5. **The clock.** `phone.deadline()` is when the next `Tick` is due (a lock ending, a session or a stream gone quiet, `progress` owed, the ten minutes of the plan's §2.2). No tick is needed before it.
6. **Read buffer.** `frame::read_buffer_len(config.max_data)`; one frame per completed read.
7. **Never log** a payload, `Debug` output of a message, or an `Effect` as a whole. `Effect::Note` carries what the journal may hold: the type of a message, or what was wrong with a frame. `msg::log_type(payload)` gives "type (length)" for anything else.

What each effect expects is in its documentation in `link/src/phone.rs`. Three that carry rules of the plan:

- `CheckAnswers { conf }`: the text is the `firstboot.conf` candidate. Write it mode 600 on a tmpfs, run `omamobile-firstboot --check --porcelain` on it, and answer with `answers::parse_porcelain` of what it printed (`Outcome::Failed` if that is `None`).
- `BackupList`: the checks of `tools/nv-backup`. The answer names each partition's kernel device (`PartFound::dev`); the machine compares the listing at `backup-commit` with the first and refuses a partition that moved.
- `FactoryWrite { offset, data, sync }`: with `sync`, the file is synced before the list goes on. The `Persist` and the `ack` behind it rely on that.

## 3. How the installer uses it

```
install flow (core)                the installer's shell                          Host (no I/O)
-------------------                ---------------------                          -------------
"pair with this code", "send the   HostInput::Command(..)                   -->
 answers", "back up", ...                                                         handle(now, input)
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

- **`HostConfig`** carries the serial the install pinned in fastboot (compared through the serial proof, never sent), a token from an earlier run if the work directory has one, and `first_id`, which should differ from run to run (§6, D3).
- **Pairing is the machine's.** At `LinkUp` it says `hello`; with a token it sends `resume`, without one it emits `Event::NeedCode` and waits for `Command::Pair`. It checks the serial proof before anything else is said, and stops for good when it differs (`Stop::WrongPhone`).
- **One step at a time.** `Command::Answers`, `AwaitFirstboot`, `Backup`, `Factory`, `Provision`, `Restart`, `Smoke`, `Finish` each run to one closing event (`AnswersAccepted`, `BackupDone`, `FactoryVerified`, ..., or `Refused { step, error }` with the phone's code, or `Failed { step }` when the shell could not do its part).
- **A lost link is the machine's too.** After a cut, a minute of silence or a session the phone ended, it emits `Event::LinkLost`, says `hello` again when the link is there, pairs with the token and takes the step up from where the phone says it is: the answers go again only at stage `waiting`; a partition is read on from the length of the copy on disk; the image from the offset the phone answers; `provision` and `restart` look at `status` first. Three sessions in a row that the phone ended over the protocol stop it (`Stop::Protocol`).
- **The backup's files** are the shell's: `BackupOpen` (how long is the copy), `BackupWrite`, `BackupDigest` (read the whole copy back from disk and hash it: the digest the phone is asked to confirm is of what is stored, not of what passed by), `BackupDiscard`, `BackupFinish` (write `SHA256SUMS`, `partitions.txt` and `NV-BACKUP` as `tools/nv-backup` does).
- **Both shells must read while they write.** The phone sends `ack` while the host is sending the image, and the host may send `abort` while the phone is sending a partition. A shell that writes a whole stream before it reads will stop at the first acknowledgement (§7, F6).

## 4. The messages

Every message of the plan's §5, and the type that is it. `id` (host) and `re` (phone) are fields of `HostFrame` and `PhoneFrame`.

### Host to phone (`HostMsg`)

| Message | Type | Fields | Allowed (as the phone machine enforces it) |
| --- | --- | --- | --- |
| `hello` | `Hello(HostHello)` | `proto` 1 to 16 numbers of 1 to 65535, no number twice; `installer` {`name`, `version`}: 1 to 64 printable ASCII; `max_data` 1024 to 4194304 | Any time; always frame 0 |
| `ping` | `Ping` | | In a session |
| `pair` | `Pair { code }` | `code`: six ASCII digits (`Code`) | Unpaired, a code is shown, not locked |
| `resume` | `Resume { token }` | `token`: 64 lower-case hex digits (`Token`) | Unpaired, a token exists, not locked |
| `bye` | `Bye` | | In a session |
| `status` | `Status` | | In a session, paired or not |
| `answers` | `Answers(Answers)` | `username`, `password_hash`; optional `credential`, `hostname`, `timezone`, `wifi_ssid`, `wifi_psk`, `full_name`, `email`; `ssh_keys` (a list, may be empty). Values are judged by `Answers::validate` | Paired, stage `waiting` |
| `firstboot-retry` | `FirstbootRetry` | | Paired, stage `firstboot-failed` |
| `backup-list` | `BackupList` | | Paired, stage before `provisioned` |
| `backup-read` | `BackupRead { name, offset }` | `name`: 1 to 36 of `A-Z a-z 0-9 _ -`; `offset`: a multiple of the session's `max_data`, at most the size | After `backup-parts`, a listed name |
| `backup-verify` | `BackupVerify { name, sha256 }` | `sha256`: 64 lower-case hex digits | After `data-end` for that name |
| `backup-commit` | `BackupCommit` | | Every listed partition verified |
| `abort` | `Abort` | | A stream is open |
| `factory-begin` | `FactoryBegin(FactoryBegin)` | `size` 1 to 2^53-1 (over 8 GiB is answered `too-large`); `sha256`; `build` 1 to 64 printable ASCII; `name` 1 to 255 bytes, no control character | Paired, stage before `provisioned`, no stream open, provisioning not running |
| `factory-end` | `FactoryEnd` | | The image's stream open and every byte sent |
| `provision` | `Provision` | | Paired, stage `firstboot-done`, `backup-done`, image verified, on the charger or battery at least 30 % |
| `restart` | `Restart` | | Paired, stage `provisioned` |
| `smoke` | `Smoke` | | Paired, stage `provisioned`, boot 2 |
| `finish` | `Finish` | | Paired, stage `provisioned` |

### Phone to host (`PhoneMsg`)

| Message | Type | `re` | Fields |
| --- | --- | --- | --- |
| `hello` | `Hello(PhoneHello)` | yes | `proto`, `link`, `device`, `max_data`, `nonce` (64 hex), `stage`, `pair` (`PairMode`: `code`, `token`, `locked`), `wait_s` |
| `pong` | `Pong` | yes | |
| `paired` | `Paired { token, serial_proof }` | yes | `token` after `pair` only; `serial_proof` (64 hex) |
| `bye` | `Bye` | yes | |
| `status` | `Status(Box<Status>)` | yes | `stage`, `boot` (1 or 2), `release`, `kernel`, `device`, `firstboot` {`state`, `step`, `n`, `of`, `failed`}, `backup` {`state`, `parts`: [{`name`, `size`, `verified`}]}, `factory` {`state`, `have`, `size`}, `provision` {`state`, `step`, `n`, `of`}, `battery`, `charging`, `free` |
| `answers-ok` | `AnswersOk` | yes | |
| `backup-parts` | `BackupParts { parts }` | yes | `parts`: at most 32 of {`name`, `size`, `checked`} (`BackupPart`) |
| `data-begin` | `DataBegin { name, size, offset }` | yes | |
| `data-end` | `DataEnd { name }` | no | |
| `backup-verified` | `BackupVerified` | yes | |
| `backup-done` | `BackupDone` | yes | |
| `aborted` | `Aborted` | yes | |
| `factory-ready` | `FactoryReady { offset }` | yes | |
| `ack` | `Ack { offset }` | no | |
| `factory-verified` | `FactoryVerified` | yes | |
| `provision-started` | `ProvisionStarted` | yes | |
| `progress` | `Progress(Progress)` | no | `what` (`What`: `factory-check`, `provision`, `backup-verify`, `smoke`, `working`), `step`, `done`, `total` |
| `provision-done` | `ProvisionDone { steps }` | no | `steps`: at most 64 of {`step`, `state`: `ok`, `failed`, `skipped`} (`StepResult`) |
| `restarting` | `Restarting` | yes | |
| `smoke-report` | `SmokeReport { report }` | yes | `report`: any JSON value, as the smoke test wrote it |
| `finished` | `Finished` | yes | |
| `error` | `Error(ErrorMsg)` | either | `code`, `message`, `stage`, and what the code carries |

`error.code` is `ErrorCode`; with their extra fields: `protocol`, `version` {`proto`}, `unknown`, `not-allowed`, `not-paired`, `bad-code` {`left`}, `locked` {`wait_s`}, `bad-token`, `problems` {`problems`: [{`field`, `reason`}]}, `busy`, `backup-precondition` {`parts`}, `backup-mismatch`, `no-space` {`need`, `free`}, `too-large`, `factory-mismatch`, `provision-failed` {`step`}, `firstboot-failed` {`steps`}, `battery`, `internal`. `ErrorCode::text()` is the fixed English text for a log.

Data frames are not messages: host to phone they are the factory image (`Effect::FactoryWrite`), phone to host a radio partition (`HostEffect::BackupWrite`).

## 5. The tests, and the golden frames

Everything runs with `cargo test`, in memory. No test opens a USB device or a file outside `link/golden/`, except the one named below when it is asked to.

| File | What it shows |
| --- | --- |
| `tests/frames.rs` | The header byte for byte; the pad rule at every length to 4096 and at the limits; every frame error by name. Fuzzing: every truncation of 13 frames; every single-bit flip of the header (448 bits) and of the payload of four frames; 200000 random and half-random buffers; 10368 headers built field by field against the rules written out again |
| `tests/json.rs` | The reader against `serde_json` on 50000 generated texts (what this crate takes, the general reader takes as the same value; what only the general one takes is a key given twice); 300000 damaged inputs |
| `tests/messages.rs` | Every message written and read back; for every message and every field: an unknown key, a missing key, another type, `null`, also inside nested objects; limits and forms field by field; 200000 damaged messages through both readers |
| `tests/answers.rs` | 204 cases of first boot's rules, each with the line it comes from. With `OMAMOBILE_FIRSTBOOT_BIN=/path/to/omamobile-firstboot` set, every case is also given to the real script (`--check`, in a scratch root) and the verdicts compared |
| `tests/turns.rs` | The phone's machine by hand: all 19 host messages in 17 states against a table written from the plan's "Allowed" column, with "nothing changed" checked for each refusal; 24 kinds of wrong frame in four states; the image's stream; timeouts; the state file across a daemon restart |
| `tests/host_turns.rs` | The host's machine by hand: every phone message, with the right `re`, a wrong one and none, in ten states; wrong frames; silence; `busy` and `locked` |
| `tests/flow.rs` | Both machines through the loopback: the straight path from the code to `finished`; a cut before every frame of the image's transfer, each way, and before every frame of a whole install; a dropped, repeated, swapped, shortened and bit-flipped frame each way; twenty wrong codes; a wrong image digest; a wrong partition checksum; a partition that moved; version mismatch each way; the link finishing itself |
| `tests/walk.rs` | Both machines walked at random, 180000 and about 170000 steps, with a shell that sometimes answers anything to anything: no panic, every frame sent is one the other reader takes, the stage never goes back, every stage and every phone message is reached |
| `tests/golden.rs` | The golden frames |

**No input found makes the decoder, the JSON reader, the message readers or either machine panic.** That is a statement about the inputs above, not a proof.

**The golden frames** are `link/golden/`: for each frame `NAME.bin`, the bytes of one bulk transfer, and `NAME.txt`, which says which message it is, each header field, and the payload. There is one of every host message (20, `answers` twice), one of every phone message (42, every error code among them), frames with the pad byte, a data frame with a 64-bit offset and the largest `seq`, and the longest message. The three data frames at the megabyte limits are kept as a description with the frame's SHA-256 and the rule that makes the payload, not as bytes. `tests/golden.rs` fails if a file is missing, extra or differs by a byte, and reads every file back from disk as what its name says.

They are the contract the phone's daemon is checked against: omarchy-mobile keeps a copy of the directory beside its package and its tests decode and re-encode every file with the crate version it pins. A change that makes this test fail has changed the wire, and that is a new protocol number, not a new golden file. `OMIL_GOLDEN_WRITE=1 cargo test -p omamobile-link --test golden` writes them anew.

## 6. Differences from INSTALL-LINK-PLAN

Each point: what the plan says, what the crate does, and why. Section numbers are the plan's.

| | The plan | The crate | Why |
| --- | --- | --- | --- |
| D1 | §5.2: "Unknown keys are ignored." | Refused (`Why::Unknown`), in every message except `hello` (both ways) and the `version` error, which pass them | A message that silently loses a key the sender meant is how two sides come to disagree about what was said (an answer the owner typed, dropped). The protocol number is how the wire changes; `hello` and the `version` error are the two messages that must still be readable across numbers |
| D2 | §5.1: `seq` is "0 for the first frame a side sends after `ENABLE`, then +1 per frame". §5.3: `hello` may come "again at any time" | A `hello` is always frame 0 and starts the numbering again in both directions; the phone's `hello` in answer is its frame 0. A `hello` with another number is a frame error | Without it a session can only begin after `ENABLE`. An installer that was killed and started again, or either side after a frame error, has no way to agree on the numbers again short of pulling the cable (§7, F1) |
| D3 | §5.2: `id` is "a whole number that goes up by one" | The phone echoes `id` and does not check its order. The host counts up from `HostConfig::first_id`, which a shell should set differently for each run | An answer to the last run's `hello`, still under way on the bus, has the same `id` 1 as this run's and would be taken for its answer: wrong nonce, and the serial proof then fails for the right phone (§7, F2). All numbers are at most 2^53 - 1 |
| D4 | §5.2: "The phone answers every message exactly once". §5.1: a wrong frame gets `error`, ends the session and counts a wrong try | With no session open (before the first `hello`, after `bye`, a frame error or ten quiet minutes) a frame that is not a valid `hello` is dropped: not answered, not counted | After one frame error the frames already under way would each be answered and counted; five lock pairing. Nothing can be guessed without a session, so nothing is lost by not counting |
| D5 | §5.1 lists what makes a frame wrong; a payload that is not a message of protocol 1 is not in the list | A message frame whose payload is not JSON, not an object, has no `t`, or has a known `t` with a wrong, missing or unknown field is a protocol error like a wrong frame: `error` `protocol`, the session ends, a wrong try is counted. An unknown `t` is `unknown` and changes nothing, as the plan says | "A malformed frame counts as a wrong try" (§2.5); a peer that speaks protocol 1 never sends one |
| D6 | §5.1: a read buffer of "at least 56 + `max_data` + 1 bytes" | `frame::read_buffer_len`: 56 + the larger of `max_data` and 262144 + 1, rounded up to 1024 | A message may be 262144 bytes whatever `max_data` is, and `max_data` may be less |
| D7 | §5.3: `max_data` "at most 4194304", "1048576 by default" | At least 1024. Required in the host's `hello`; 1048576 is the phone's own unless its shell says otherwise. `backup-read`'s offset is a multiple of the *session's* `max_data` | No lower limit was given; "by default" could be either side's. A copy begun in a session with another `max_data` is read on from the multiple below its length |
| D8 | §5.1 says when the pad byte is sent | The reader also refuses: the pad flag set when no pad is due, a pad byte that is not zero, bytes after the frame, a data frame with no payload, an offset whose end does not fit 64 bits | One frame has one encoding |
| D9 | §5.3: `hello` carries `pair` (`code`, `token` or `locked`) and `wait_s`; `locked` errors carry `wait_s`. "20 wrong tries ... lock it until a restart" has no value | `wait_s` is always present: 0 when not locked; with `locked`, 0 means until the phone restarts and a timed lock is at least 1. When no code can be shown and no token exists, `pair` is `locked` with 0 | The fourth case and the endless lock needed a spelling |
| D10 | §2.5: "Five wrong codes replace the code and lock pairing for 30 s, doubling to 10 minutes; 20 wrong tries in one boot lock it until a restart" | Wrong tries are counted per boot and never reset, also not by a right code. Every fifth locks (30, 60, 120 s) and replaces the code; the twentieth locks until a restart. The answer to the fifth wrong code is `locked`, not `bad-code`. While locked, `pair` and `resume` are answered `locked` and not counted | As written, the ten-minute cap is never reached: the fourth lock is the twentieth try. Whether a right code resets the count was not said |
| D11 | §5.4 names `status`'s fields but not the values of `backup.state` and `factory.state`, nor which fields may be absent | `backup.state`: `none`, `listed`, `done`. `factory.state`: `none`, `partial`, `verified`. `firstboot.state` and `provision.state`: `none`, `running`, `done`, `failed`. Every field is always there (`step` empty, numbers 0 when there is nothing to say) | Strict reading needs closed lists |
| D12 | §5.6's `provision` row: `progress` {`what: "provision"`, `step`, `n`, `of`}; below the table: "`progress` is {`what`, `step`, `done`, `total`}" | The second form for all. `what` is one of `factory-check`, `provision`, `backup-verify`, `smoke`, `working` | The two contradict each other. The plan names two values of `what`; the phone also owes `progress` during the second read of a partition, the wait for the smoke test and any slow check |
| D13 | §5.5 `answers`: ten fields with lengths, `wifi_ssid` and `wifi_psk` marked optional | First boot's rules win (`omamobile-firstboot` lines 95-153; `link/src/answers.rs` cites each): only `username` and `password_hash` are required; `credential`, `hostname`, `timezone`, `full_name`, `email` may be left out, and an empty `credential`, `hostname` or `timezone` is first boot's default (`pin`, `omamobile`, `UTC`); a control character in any value is refused (line 103); `wifi_psk` is 8 to 63 printable ASCII or 64 hex digits, never "at most 64" of anything; a `wifi_ssid` that is given is 1 to 32 bytes. The lengths the plan adds where first boot has none (`timezone` 64, `full_name` 128, `email` 254, eight keys of 8192) are kept as the link's own. Every value problem is answered `problems` with the field; only a wrong JSON type, a missing key or an unknown one is a protocol error | The plan says the rules are first boot's own. A length first boot does not have must still be reportable under its field, so it is a `problems` reason (`too-long`, `too-many`), not a frame error |
| D14 | §5.5: the phone runs `omamobile-firstboot --check --porcelain`, which prints `KEY<TAB>reason-code` | The reason words are fixed here (`answers::reason`): `required`, `format`, `reserved`, `control-character`, `too-long`, `too-many`, `length`, `without-ssid`, and for what only the phone can check `system-account`, `exists`, `no-such-zone`. A reason is 1 to 32 of `a-z 0-9 -` | `--porcelain` does not exist yet and the plan names no words. The script's version of it should print these |
| D15 | §5.4: the backup and the image are "sub-states: any order, any time after pairing" | With the image's stream open, the phone takes `factory-end`, `abort`, `ping`, `status`, `bye` and `hello`; any other message is `not-allowed`. `factory-begin` is `not-allowed` while provisioning runs. One step that owes an answer at a time: others are `busy` | One stream and one step at a time keeps both machines small enough to test in every state. A host that wants the backup during the transfer sends `abort` first, and the image goes on from its last acknowledgement |
| D16 | §5.6: `ack` "after each 64 MiB is written and synced" | Also once when the last byte is written. An acknowledgement's offset is the end of the frame that crossed a 64 MiB line, so a multiple of 64 MiB only when `max_data` divides it. A transfer goes on from the last acknowledged offset; bytes after it are sent again | The tail must be synced before `factory-end` is answered, and the host's record should see it |
| D17 | §5.6: `factory-begin` answers "the last synced offset of a kept partial file with the same `size` and `sha256`" | The same when the kept file was already verified: the answer is its size, the verified mark is taken away, and `factory-end` checks the file again | Not said. Checking again costs a read and cannot be wrong |
| D18 | §5.2 lists the code `firstboot-failed` (with `steps`) and no message it answers; no message tells the host that first boot ended | `firstboot-failed` answers `provision` at stage `firstboot-failed`. The host learns how first boot ended from `status`, which `Command::AwaitFirstboot` asks every two seconds | A choice was needed |
| D19 | §8, "The camera step alone fails": "`provision` again before `finish`" | `provision` at stage `provisioned` is `not-allowed` | §5.6 makes a run whose camera step alone failed a success: the stage is `provisioned`, the image is deleted, and `factory-begin` is not allowed at that stage. There is nothing to provision again from (§7, F5) |
| D20 | §5.5: `backup-read` "After `backup-parts`", `backup-verify` "After `data-end` for that name", `backup-commit` "Every part verified" | The listing and its marks live for one boot, across sessions; only `backup-done` is in the state file. A second `backup-list` with the same answer keeps the marks; another answer replaces them. `backup-read` takes a verified mark away again; after `backup-mismatch` a partition must be read again before it is verified again. An offset equal to the size answers `data-begin` and `data-end` at once | §2.6 has the backup go on after a cable pull with `resume` and `backup-read`, so the listing must outlive a session |
| D21 | §5.2: `error` is {`t`, `re?`, `code`, `message`, `stage`}; some codes "with" a value | The values are fields named `left`, `wait_s`, `problems`, `need`, `free`, `step`, `steps`, and for `version` and `backup-precondition`, where the plan names none, `proto` and `parts`. A field belongs to its code and to no other. `error` has no `re` when it ends a session (`protocol`, `internal` after a fault) or a long step (`provision-failed`) | Strict reading |
| D22 | §5.3: "The phone answers a message within 10 s, or sends `progress` at least every 5 s until it does"; "The host takes 60 s of silence during a step as a lost link" | The phone sends `progress` after every 5 s in which it sent nothing for the step. The host counts silence only while it waits for the phone, not while its own shell is reading a file or the flow has asked for nothing | The simpler rule covers the plan's; a host that is not waiting cannot be kept waiting |
| D23 | §2.2: "the link is offered for 10 minutes after the session is up in each of at most three boots, then finishes itself" | The ten minutes run from `PhoneInput::DesktopUp` in a boot that began at `provisioned`; the fourth such boot finishes at the daemon's start. The boot in which provisioning ended has no limit. The end is put off while a step owes an answer. `boot` is 2 from the first boot that begins at `provisioned`, whatever restarted the phone | The installer asks for the restart right after `provision-done`; ending the link under it would lose the report for nothing |
| D24 | §2.5: the token after a right code | A right code always makes a new token and the old one stops working. `resume` when no token exists is `not-allowed` and not counted | Not said |
| D25 | §5 gives few limits beyond `answers`' | `link/src/msg.rs` fixes the rest: partition names 1 to 36 of `A-Z a-z 0-9 _ -`; the codename 1 to 32 of `a-z 0-9 _ -`; versions, releases, builds and step names at most 64 printable ASCII; `kernel` 128; `error.message` 256; lists of at most 16 protocol numbers, 32 partitions, 64 steps, 32 problems; digests, tokens and the nonce 64 *lower-case* hex digits; JSON nested at most 32 deep, a key at most once per object | Every string and list needs a bound to be read strictly, and one value needs one spelling |
| D26 | §5.6: `smoke-report` {`report`} | `report` is any JSON value, passed on as written. A report that does not fit one message (262144 bytes) is answered `internal` | Its shape is `phone-smoke`'s, not this protocol's |

## 7. What looks like a flaw in the protocol

Not ambiguities: places where the plan as written leads somewhere it does not mean to go. Each with the case that shows it.

- **F1. The numbering cannot start again without the cable.** `seq` restarts at `ENABLE` only, and `hello` is allowed at any time. An owner's installer crashes during the radio backup and is started again (the plan's own drill R9): the new process sends `hello` as its frame 0, the phone expects frame 41, answers `protocol` and counts a wrong try; so it goes for every later frame, and the fifth locks pairing. Only a replug helps, and nothing tells the owner so. The crate's D2 is the fix; the plan's §5.1 should say it.
- **F2. An old `hello` answer can be taken for a new one.** The same restart: the first run's `hello` (`id` 1) was answered, and the answer is still waiting in the phone's IN endpoint when the run dies. The second run sends its own `hello` (`id` 1 again), reads the old answer, keeps its nonce, pairs, and computes the serial proof with a nonce the phone has since replaced: "This is not the phone that was just installed", for the right phone. D3 (`first_id`) makes the collision unlikely, not impossible. A nonce of the host's in `hello`, echoed in the answer, would close it.
- **F3. After first boot, a lost token cannot be replaced, and the link then never ends.** §3.3 lets the display manager start from stage `firstboot-done`, so no code can be shown from then on; §7.2 says a reloaded web page "in boot 1 ... types the code again". An owner reloads the page during the radio backup: the token was in memory only (Q12), the lock screen owns the display, `hello` says `pair: "token"`, and nothing can pair. §2.2's limits begin at `provisioned`, so this phone offers the link at every boot from then on and never finishes itself; in the release composition (§3.3, the link alone) it also has no USB network function in any of those boots. The plan needs either a way to show a code after first boot or a limit for a phone that stopped before `provisioned`.
- **F4. A wrong try is counted for things an honest installer does.** A transfer that pauses for more than 120 s (a laptop that slept) has its stream suspended; the next data frame is "a data frame with no open stream": a protocol error and a wrong try. A corrupted download counts one per `factory-mismatch`. Twenty in a boot lock pairing until a restart, and after the planned restart that costs the smoke report. Any host on the cable can also spend the twenty in four minutes without knowing anything. Counting only wrong codes and wrong tokens toward the lock, and answering a late data frame `not-allowed`, would keep the limit for what it is for.
- **F5. `provision` cannot be run again for the camera step** (D19): the success that leaves the camera step failed deletes the image and moves to a stage where no image may be sent.
- **F6. The acknowledgement can stop the transfer on real USB.** The phone writes `ack` to its IN endpoint after 64 MiB and "reads the next data frame only after it has written the previous one"; a FunctionFS write waits for the host to read. A host that sends the image in a loop and reads afterwards never reads: the phone waits in its write, the host in its next write, for good, at the first 64 MiB. The same holds the other way for `abort` during a partition's stream. The loopback here cannot show it. Both shells must keep a read pending while they write, and stage L3 should test exactly this.
- **F7. A crash between writing `firstboot.conf` and recording the stage ends the link for good.** §5.5 renames the candidate to `/etc/omamobile/firstboot.conf` and then records stage `firstboot`. Power is lost between the two. At the next boot the marker and a `firstboot.conf` are both present, and §2.2's fourth row has the daemon delete the marker, write `done` and exit: a phone with an account and no radio backup, no firmware and no link. That row should apply only when the link's own state file does not exist; with one at stage `waiting`, a `firstboot.conf` means the answers were taken.

## 8. What stage L2 will need that is not here

- **A tag to pin.** The crate's version is the workspace's 0.0.1. Stage L2 needs a tag on this repository and a way to bring `link/golden/` into omarchy-mobile and keep the copy in step (a script, and the test there that reads the files).
- **`omamobile-firstboot --check --porcelain`** printing D14's words. `answers::parse_porcelain` reads it.
- **What the daemon finds at its start.** `Phone::start` takes the state file and nothing else. The daemon has to square it with the phone first: a `firstboot.conf` with the state at `waiting` (F7); a state that says provisioning runs when no unit does (feed `ProvisionEnded` with a failed step); first boot finished while the daemon was down (feed `FirstbootEnded`); an image file shorter than `synced` (the answer to `FactoryOpen` says how long it really is). These rules are not in the crate.
- **`boot-update.conf`** (the plan's Q3) has no effect of its own: it belongs in what the daemon does for `Effect::StartFirstboot { retry: false }`.
- **Numbers the plan leaves to the runbook:** the working space `FactoryOpen` checks (§6.4, R1), which the shell adds to the bytes still to come.
- **A transport for its tests.** A socket pair that keeps message boundaries (`SOCK_SEQPACKET`) stands in for bulk transfers as they are: one frame per read. Over a byte stream the daemon would need to find a frame's end from its header; `frame` has no function for that yet.
- **The words.** `Effect::Screen` names what to show; the sentences are the screen program's. `Effect::Note` names what happened; the journal's lines are the daemon's.
- **FunctionFS events:** `ENABLE` is `LinkUp`, `DISABLE` and `UNBIND` are `LinkDown`. `SUSPEND` and `RESUME` have no input; if a suspended bus should end a session, that is a new input or a `LinkDown`.

For stage L6 (the installer): a `Link` transport that yields one frame per read and keeps a read pending while it writes; the flow's states mapped onto `Command` and `Event`; `HostConfig::first_id` from the clock; and the token's place in the work directory.
