# The desktop app: design

Status (2026-11-13): **protocol 5 holds both the steps after the write with the release's tools (DESIGN §13) and the install link after the write (§3.9), each tested against a pretend phone only. The app and the tool's side of the protocol below (`omamobile-install json`, `cli/src/json.rs`) are both built, and tested against each other with the tool's pretend phone. Neither has been run with a real phone: the tool's USB path in this mode has never listed or opened a device.** Started as it is by default, the window goes to the dry run and no further. Started with `--write` it goes on through the flow's write path (DESIGN §11), which has not run on a real phone either, and then, for a release that ships the tools for them, through the steps after the write (DESIGN §13: the USB connection, the first start, the radio backup, the firmware, a restart and a check, with the release's own tools), which have run against stand-ins only; for a release whose root image offers the install link, through the link instead (§3.9), which has run against the pretend phone that speaks it only. What works, what has never run and the app's first attended run are in [app/docs/STATUS.md](../app/docs/STATUS.md).

`omainstall` (`app/`) is a Qt Quick window in the style of Omarchy's own apps. Its one job is to guide a person through installing Omarchy Mobile on a phone plugged into this computer. It is a third front end for the flow in [DESIGN.md](DESIGN.md), next to the command-line tool and the web page, and DESIGN §7 ("What a front end must do") applies to it.

This document is the app's design and, in §3, the protocol between the app and the tool. §3 is the specification of `omamobile-install json`, which is also the installer's interface for agents and scripts (§3.7).

## 1. What the app is, and is not

- **A thin front end.** The core decides the order of the steps, every refusal and every sentence a person reads about the phone. The app draws the `Screen` it is sent and hands in what the person did.
- **No install logic.** The app has no check of a phone, a release, a file or an answer. It does not open a USB device, does not fetch anything from the internet, does not hash a file and does not read one: it passes a path.
- **No sentence about the phone of its own.** The app's own words are the labels of its buttons and fields, the names of the five stages at the top, the shortcut list, the line that says it is a preview, and what it says when the tool itself fails (§6). Even the question "which of these phones?" is the tool's (§3.3, `phones`).
- **It writes only when it was started to.** `omainstall open --write` passes `--write` on to the tool, and that is the one way: no control in the window and no message of the protocol turns writing on (§3.6). Without it the window ends at the dry run. With it, the writes still wait for the list of what will be written and for the phone's codename, typed by the owner, as on the command line. The window says which kind of run it is from its first screen.
- **It cannot give the owner's statement about a bootloader slot, and it does reinstalls.** A phone that tells the version of one bootloader copy only (the Pixel 7 Pro) stops in the app on a first install, with or without `--write`: the statement that passes that stop is typed on the tool's own command line and nowhere else (DESIGN §6). A phone that runs from slot B is asked what slot B holds, in the core's words; answered "Omarchy Mobile", the run is a reinstall, and the bootloader-slot check passes with the version slot B reports, which the phone already runs (DESIGN §6, "A reinstall from slot B", decided on 2026-11-13). The app draws the question and the stops.

## 2. The shape

```
omainstall (Qt Quick)                         omamobile-install json           core
---------------------                         ----------------------           ----
Main.qml      draws backend.screen
Backend       QProcess, JSON lines   <-->     stdin / stdout, one object  -->  Flow (core/src/flow.rs)
                                              per line                    <--  Screen
FilePicker    xdg-desktop-portal              USB, the download, the clock,
                                              the files: all here
```

The app starts the tool as a child process and talks to it over the child's standard input and output. Everything that touches a phone, the network or a file is the tool's, as it is today in `cli/src/main.rs`.

**Finding the tool.** In this order:

1. the path in the environment variable `OMAINSTALL_TOOL`, for tests. A variable that names no program finds nothing: the app says so rather than fall through to another tool;
2. `omamobile-install` in the directory of the app's own binary;
3. `omamobile-install` on `PATH`.

**Starting it.** `omamobile-install json`, with `--write` when the app was started as `omainstall open --write` and `--release DIR` when it was given a directory. Those are all the arguments the app ever passes. `omainstall check` never passes `--write`.

**The mode's arguments are a closed list:** `--release DIR`; `--write` (the flow may write: §3.6); and two the app does not pass and a script may, `--work-dir DIR` (where a download of Google's factory image is kept; by default the user's cache directory, as for `install`) and `--proxy http://HOST:PORT` (the download's route, as for `install`). A build with the `fake-phone` feature also takes `--fake-phone STATES`, `--fake-google 127.0.0.1:PORT`, `--fake-unplug COMMAND:N` and `--fake-after DIR` (§10); a build that ships has none of them. Any other argument is refused before `hello`, with a `fatal` and exit status 1: there is no argument that names a phone (`--serial`), none for the owner's statement about a bootloader slot (`--bootloader-slots-match`), and none for the maintainer's override (§3.6).

**Without `--release`** the tool still says `hello`, so `omainstall check` works anywhere. `start` then fails with a `fatal`: this version can't find or fetch a release by itself (§9).

## 3. The protocol

Protocol number **5**. (Number 4 was two protocols, on two branches that were merged into this one: the one with the steps after the write and the one with the install link; 5 is both. Number 3 was the same protocol without either: no action `after` and no message of that type, the last screen of a write named no action; no `link` on the `installed` screen, none of the link's states, screens whose action is `wait`, the actions `link`, `pair`, `backup-dir` and `retry`, `start` with `link`, the three optional fields of `answers`, or the `pretend` message of a pretend build (§3.9). Number 2 was the same protocol without the question of what slot B holds: no `choices` in the screen, no `slot-b` action and message; a phone on slot B in a run not said to be a reinstall stopped at `runs-from-slot-b`. Number 1 was 2 without a write: no `--write`, no `writes` in the tool's hello, no `confirm` and `write`, no `sent`, `piece` and `account`. Sides that speak different numbers refuse each other, §3.2.)

### 3.1 Framing

- One JSON object per line, in each direction, UTF-8, ended by a line feed. No line feed inside a message. An empty line is skipped. The order of an object's keys means nothing (the tool writes them sorted).
- Every message has a `type`.
- A line to the tool is at most 1 MiB, and a last line that lacks its line feed still counts.
- **The tool takes the lines in the order they were written,** each after the one before it was answered. A script may therefore write several at once. While an action runs, the tool looks at the next line only to see whether it is a cancel or a quit (§3.4).
- The tool's standard output carries protocol messages and nothing else. What it has to say besides goes to standard error, as free text; the app shows the end of it only when the tool stops unexpectedly.
- The app never sends a message the tool did not ask for, with two exceptions: `download` with `"do": "cancel"`, and `quit`. It sends one message without a click: `write`, once, when the screen that answers `confirm` names it (§4).

### 3.2 The handshake, and versions

```
tool -> app   {"type":"hello","protocol":4,"tool":"omamobile-install","version":"0.0.1","writes":false}
app -> tool   {"type":"hello","protocol":4,"app":"omainstall","version":"0.1.0"}     (not answered)
app -> tool   {"type":"start","reinstall":false}
tool -> app   {"type":"screen","screen":{...}}
```

- The tool says `hello` first, at once, before it reads a release or looks at anything. **Before `start` it opens no USB device and fetches nothing.** `omainstall check` relies on that: it reads the hello and sends `quit`.
- `protocol` is one whole number. There is no negotiation. A side that reads another number than its own stops:
  - the app shows "The installer's tool speaks protocol N and this app speaks protocol M. Update whichever is older.", sends nothing more, and ends the tool;
  - the tool answers the app's `hello` with `fatal` and exits with status 1. A `hello` that names no whole number as its `protocol` is taken the same way. Of the app's `hello` the tool reads `protocol` and nothing else.
- **The tool's `hello` says whether this run's flow may write:** `writes`, true exactly when the tool was started with `--write`. The app compares it with what it was started for and goes on with neither a tool that writes when a dry run was asked for nor the reverse: it says so, sends nothing, and ends the tool.
- `start` comes after the app's `hello`, once. It carries what the person said up front (`FlowOptions`): `reinstall`, true or false, slot B already holds Omarchy Mobile. With `false` a phone on slot B is not stopped but asked about (`slot-b`, §3.4): this mode tells the flow that its front end can ask (`FlowOptions::asks_what_slot_b_holds`). **It does not carry the write switch**, and a `write` field in it, or in any message, is ignored: the switch is the tool's own argument. The tool then reads the release, calls `Flow::begin` and sends the first screen: the warning, or a stop if the release fails its checks. A release that can't be read at all (no directory was named, or a file of it is missing) is a `fatal`.
- **Before `start` there is no flow and so no screen.** What the tool can't take then (a line that is not a JSON object, a `start` before the app's `hello` or without `reinstall`, any other message but `quit`) is answered with `error` alone.
- A new message type, a new field the other side must understand, or a changed meaning takes a new protocol number. A new *optional* field does not: both sides ignore fields they do not know. A new value of `Screen.action` does not either: an app that does not know it shows the screen, offers no main action, and says "The installer asks for a step this app does not know (…). Update the app to go on."

### 3.3 From the tool

| `type` | Fields | Meaning |
| --- | --- | --- |
| `hello` | `protocol` (number), `tool` (text), `version` (text), `writes` (true or false) | §3.2 |
| `screen` | `screen`: the core's `Screen` | The flow's current screen. Ends the action in hand (§3.5) |
| `progress` | `what` (text), `done` (number), `total` (number) | Bytes done and bytes in all, for the action in hand. `what` is `download` (Google's factory image arriving), `check` (a file handed in being hashed, or the factory image hashed again before the firmware), `dry-run` (the release's images on disk being checked and walked through the dry run: each file twice, once to check it and once through the session; none is sent when the images are not there), and in a run that writes `verify` (every file of the release checked again, before the first write) and then `write` (the same files as they are sent to the phone). After the write (DESIGN §13), in seconds rather than bytes: `first-start` and `restart` (waited, of the bound), `backup` and `firmware` (run so far, `total` 0). Another word is shown as plain "Working". `total` 0 means the end is not known. A few a second at most |
| `phones` | `title` (text), `phones`: `[{"id": text, "label": text}]`, at least one | More than one phone in Fastboot Mode answered a `read-phone` that named none, and none of them is the phone the flow looked at before. `title` is the question, in the tool's words. `id` is a name the tool makes up for this run (a number, as text) and stays that phone's for the run; `label` is what the person can tell the phones apart by: the product name the phone gives over USB, and the USB bus and port it is on. **Neither is a serial number** (the core prints none, README "Safety") |
| `problems` | `problems`: `[{"field": text, "message": text}]` | The core's objections to the owner's answers, as `AnswerProblem` serialises: `field` is `username`, `password`, `hostname`, `timezone`, `wifi_ssid` or `wifi_psk`. The flow stays at `answers` |
| `sent` | `command` (text) | Only during `write`: a command that is not a question was sent by the session that writes, as the phone received it (`download:00004000`, `flash:boot_b`, `set_active:b`, `reboot`). The pieces of `userdata` are not said this way but as `piece` |
| `piece` | `number` (number, from 1), `blocks` (number), `bytes` (number), `seconds` (number, or null when the phone did not answer), `line` (text) | Only during `write`: one piece of `userdata` was sent. `blocks` is what its flash covered on the phone, `bytes` what was sent, `seconds` how long the phone took to answer. `line` is the same in the words the terminal prints (`flash:userdata (piece 1: … blocks, … KiB written on the phone, … bytes sent, answered in … s)`) |
| `account` | `lines`: `[text]` | Only during `write`, at most once, when `userdata` is over or the write stopped inside it: the piece that covered the most blocks, the flash the phone took longest over, and a piece that was sent and not confirmed (DESIGN §11, "How much one flash asks of the phone"). The lines the runbook reads |
| `error` | `message` (text) | What was sent did not go through and the flow did not move for it: a file that is not there, a phone that stopped answering, an action the screen does not name, a message of a type the tool does not know, a line that is not a JSON object. The tool's words, shown as they are. It repeats nothing of what was sent but the message's type and a phone's id |
| `fatal` | `message` (text) | The tool itself failed and is about to exit with a status other than 0: an argument it does not take (the one `fatal` that comes before `hello`), another protocol number, a release that can't be read, a bug (a panic is caught and said this way) |
| `bye` | | The answer to `quit`. The tool then exits with status 0 |

**`screen.screen` is exactly `omamobile_installer_core::flow::Screen` as serde serialises it**, the same JSON the web page reads from `InstallFlow::screen()`:

```
state     text          warning, phone, slot-b, unlock, google-terms, factory-image, answers, dry-run, done,
                        confirm, write, installed (those three only in a run that writes), stopped;
                        after the write (DESIGN §13, made by the tool): after-resume, network, first-start,
                        backup, firmware, restart, ready
                        or, for a release with the install link: first-start, pair, setup,
                        radio-backup, factory-send, provision, restart, check, ready (§3.9)
stop      text | null   for `stopped`: the stop's id (DESIGN §4)
retry     text | null   for `stopped`: whether starting the installer again can get past the stop:
                        whenever (at any time), after-fix (once what the message says is done),
                        stops-again (as things are it stops at the same place). Null on every other screen
title     text
notice    text | null   said before everything else, set apart
body      [text]
quote     null | { intro: text, lines: [text], after: [text], links: [{ label: text, url: text }] }
steps     [{ text: text, command: text | null }]
link      text | null
file      null | { name: text, size: number, build: text }
problem   text | null
plan      [text]
footer    [text]
choices   [{ answer: text, label: text }]
                        for `slot-b`: the answers to the core's question, in order; the label is the
                        core's words for the button, the answer what the app sends back. For `after`:
                        the owner's choices on a screen of the steps after the write. Empty on every
                        other screen
action    text          acknowledge, read-phone, slot-b, accept-terms, download, take-file, answers,
                        dry-run, confirm, write, after, none; link, wait, pair, backup-dir, retry (§3.9)
```

The app checks that every key is there and of that type. A screen that lacks one is malformed (§6): a missing `notice` must never be read as "no notice". Keys the app does not know are carried along and ignored.

`retry` is the stop's `Retry` in the core, the same fact its closing line says in words. It was added to `Screen` when the tool's side was built; no tool spoke protocol 1 before that, so it was part of protocol 1 from its first tool on, and it is unchanged in 2 and 3. A front end offers "start again" from it (§4) and never from the stop's sentences.

### 3.4 From the app

One message per action that `Screen.action` names. The app sends an action only while the screen on show names it, and only when no action is in hand. **The tool holds to the same rule itself:** an action the flow's screen does not name is refused with `error` before the flow or a phone is touched, whoever sent it.

| `Screen.action` | Message | The tool hands the flow |
| --- | --- | --- |
| `acknowledge` | `{"type":"acknowledge"}` | `acknowledge_warning` |
| `acknowledge` | `{"type":"decline"}` | `decline_warning` |
| `slot-b` | `{"type":"slot-b","answer":"<answer>"}`, the `answer` of one of the screen's `choices`: `omarchy-mobile`, `android` or `not-sure` | `answer_slot_b`: Omarchy Mobile makes the run a reinstall and the flow goes on from the look at the phone, with no new question sent to it; the other two end in the core's stop `runs-from-slot-b`. A missing or unknown `answer`: `error`, and the question stays |
| `read-phone` | `{"type":"read-phone"}` or `{"type":"read-phone","phone":"<id>"}` | No phone in Fastboot Mode: `no_phone`. One, or the one chosen: `read_phone`. Several and none chosen: the phone the flow looked at before, if it is among them; otherwise a `phones` message, and the flow is not touched. An `id` no `phones` message of this run gave, or a phone that is gone: `error` |
| `accept-terms` | `{"type":"accept-terms"}` | `accept_google_terms(now)`, by the tool's clock: the app sends no time |
| `accept-terms` | `{"type":"decline-terms"}` | `decline_google_terms` |
| `download` | `{"type":"download","do":"start"}` | `factory_download`, the fetch as the core asks for it, `factory_download_done`; `progress` with `what: "download"` on the way |
| `download` (while that runs) | `{"type":"download","do":"cancel"}` | The transfer is ended, also one that stands still, and handed to `factory_download_done` as one that broke off, so the bytes that arrived are kept and the core's screen says how far it came. The next `start` goes on from them. A cancel with no download running is ignored: it gets no answer at all. A `do` that is neither `start` nor `cancel`: `error` |
| `download`, `take-file` | `{"type":"take-file","path":"<absolute path>"}` | `factory_zip(name, size)`, the file's bytes in pieces, `factory_zip_done`; `progress` with `what: "check"` on the way. No such file, or a path that is not absolute: `error` |
| `answers` | `{"type":"answers","username":…,"password":…,"password_again":…,"hostname":…,"timezone":…,"wifi_ssid":…,"wifi_psk":…}` | `OwnerAnswers::check` on the seven texts of `AnswersInput`, as typed. Problems: a `problems` message. None: `answers`. A time zone this computer does not know is the tool's to refuse, as a problem of `timezone`. A field that is missing or is not text: `error`, which names the field and repeats nothing that was typed |
| `dry-run` | `{"type":"dry-run"}` | The phone the flow looked at (or the only one connected) is opened; then `dry_run`, the release's images walked through it when they are on disk, `dry_run_done` or `dry_run_failed`. No phone in Fastboot Mode: `error`, and the step stays open (the flow has no "no phone" at this step) |
| `confirm` | `{"type":"confirm","typed":"<what the owner typed>"}` | `confirm_install(typed)`, with the text as it is: the phone's codename takes the flow to `write`, anything else (an empty text among it) to the core's stop `not-confirmed`. A `typed` that is missing or is not text: `error`, and the step stays open |
| `write` | `{"type":"write"}` | The phone the flow looked at is opened; then `begin_write`, the one place a session that can write is opened; every file of the release checked, then sent through that session; `write_done` or `write_failed`. On the way `progress` (`verify`, then `write`), `sent`, `piece` and `account`. No phone in Fastboot Mode: `error`, and the step stays open. In a run that was not started with `--write` no screen ever names this action, so it is refused like any action out of turn |
| `after` | `{"type":"after","answer":"<answer>"}`, the `answer` of one of the screen's `choices` | No flow step: the tool's steps after the write (DESIGN §13). `installed` offers `begin`; `after-resume` `go-on` and `forget` (which answers with the flow's first screen); `network` `add` (the tool adds the NetworkManager connection) and `myself`; `first-start` `wait`; `backup` `backup`; `firmware` `firmware`; `restart` `restart`. Each runs its step, with `progress` on the way, and answers with the next step's screen, or the same one with its `problem`. A missing or unknown `answer`: `error`, and the screen stays. A quit behind a wait (`first-start`, `restart`) ends the wait; behind `backup` or `firmware` it waits for the tool to finish, as behind a write |
| any | `{"type":"quit"}` | Nothing. A download or the check of a file that is running is ended (a download as if cancelled, so its bytes are kept); a look at a phone or a dry run that is running is finished, since both only read and neither takes long. **A write that is running is finished: nothing sent to this mode cuts a write off, and neither does a reader that goes away.** A message the tool can no longer write (the app died) does not end it in the middle of a write; it leaves, with status 1, once the action in hand is over. The action in hand still gets its screen, and then the tool says `bye` |

Also from the app: `hello` and `start` (§3.2). The end of the app's output (the tool's standard input closes) means `quit`.

A `quit`, or the end of the input, takes its turn like every line (§3.1): it ends a running action only when it is the next line after that action's.

The fields of `answers` are those of `AnswersInput` in `core/src/flow.rs` today. All seven are always sent; an empty text means what it means there (the default phone name, no Wi-Fi, an open network).

### 3.5 What answers what

**Every action ends with exactly one `screen`**: the flow's screen after the action, changed or not. Before it the tool may send any number of `progress` messages, and at most one of `phones`, `problems` or `error`; a `write` also any number of `sent` and `piece`, and at most one `account`. From sending an action until that screen the app is busy: it shows what is running, offers no other action (but cancelling a download), and asks before it quits.

So a refused file is `screen` (the same step, with the core's `problem`); refused answers are `problems`, then `screen`; a file that is not there is `error`, then `screen`.

**The same holds for everything else that is sent after `start`,** so that a sender is never left waiting: a line the tool can't read, a message of an unknown type, an action out of turn, a second `hello` or `start` each get `error`, then the screen, unchanged. The exceptions are `quit` (answered by `bye`) and a cancel that finds no download (not answered).

After a screen whose `action` is `none` (a stop, or the end), nothing more is taken: an action sent then is answered with `error` and that same screen. The tool stays until `quit` or the end of its input; a front end must also take a tool that exits with status 0 by itself there for no failure.

### 3.6 What never crosses

- **No write that the tool was not started for.** Whether the flow may write is said once, on the tool's own command line (`json --write`), which the app passes on only when it was itself started with `--write`. No message turns it on: `start` carries only `reinstall`, the tool reads no message for the switch, and `tools/check-shipping` holds both to one line each. A run without `--write` ends at the dry run, and its tests send it every message that could ask for a write, `confirm` and `write` among them, and end by reading the pretend phone's own record: `getvar`, the one read-only question of a locked phone, and nothing else.
- **No write without the flow.** With `--write`, `write` is taken only when the flow's screen names it, which it does only after the dry run, the list of what will be written, and `confirm` with the phone's codename. The session that writes is the one `Flow::begin_write` opens (DESIGN §11); this mode opens none of its own. Which partitions are written is the core's: slot B's boot images and `userdata`.
- **No write that is cut off.** No message cancels a write, and a `quit` or the end of the input behind one waits for it.
- **Nothing after the write that the tool was not started for.** The steps after the write come only in a run started with `--write`, and only after its write or from its own record of one (`after-resume`); a run without `--write` makes no key, reads no record and runs none of the release's tools.
- **No statement about a bootloader slot.** `--bootloader-slots-match` is `install`'s (DESIGN §6): this mode refuses it like any argument it does not have and has no message for it, so a phone that reports one bootloader slot's version stops in this mode, in the app and for a script alike.
- **No command.** A `Screen` step's `command` is text for the person's own terminal. The app shows it and copies it to the clipboard on request. It has no way to run it, and no message sends it back.
- **No address.** The app never sees where Google's file is fetched from. `Screen.link` and the quote's links are pages for the person, opened in their browser on their click, and only if they are `https`.
- **No serial number**, in either direction.
- **No override.** There is no message, argument or control for the maintainer's bootloader-slot override (DESIGN §6): `json` refuses `--maintainer-skip-bootloader-slot-check` like any argument it does not have, so a phone that reports one bootloader slot's version stops in this mode, and only `install --dry-run` on a terminal takes the override. The core's `notice`, which says on every screen that a check was skipped, is drawn right under the title all the same, for a later version in which it can arrive.
- **The owner's answers** go to the tool once, over the pipe, when the form is handed in. The app keeps them only in the form's fields, which are gone with the step. It writes them nowhere, and logs nothing of what it sends.

### 3.7 For agents and scripts

`omamobile-install json` is the installer's interface for anything that is not a person at a terminal, and the supported one: the terminal's `install` and `flow` print for a person, and their words and layout may change. An agent or a script starts the tool the same way as the app and exchanges the same lines: it reads the `Screen`, decides, and sends one of the actions of §3.4. The app holds nothing an agent would need, and the rules are the same for both, because they are the core's. Standard output is the protocol and nothing else; what the tool has to say besides is on standard error.

Started without `--write` it ends at the dry run, and nothing sent to it can write to a phone (§3.6). On a real phone it then sends `getvar` and, to a locked phone, one read-only question, and it downloads Google's factory image once Google's terms were accepted with `accept-terms`. **An agent does not accept Google's terms, or the warning, for a person:** both are the phone's owner's to answer, and `decline-terms` with a file the owner saved is always a way through. **And an agent does not start this mode with `--write` against a real phone, or send `confirm`, for a person:** the codename is the owner's go-ahead for erasing their phone, typed by them. Against the pretend phone of a `fake-phone` build a script may do both, which is how the write path's tests run.

A driver reads each screen and answers what it asks for. In Python, against the pretend phone of a `fake-phone` build (no USB device is opened):

```python
import json, os, subprocess
tool = subprocess.Popen(["target/debug/omamobile-install", "json", "--release", "fixtures/selftest", "--fake-phone", "ready"],
                        stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
def send(**message):                    # one message; the screen that ends it
    tool.stdin.write(json.dumps(message) + "\n"); tool.stdin.flush()
    while (reply := json.loads(tool.stdout.readline()))["type"] != "screen":
        print(reply)                    # progress, phones, problems or error
    return reply["screen"]
assert json.loads(tool.stdout.readline())["protocol"] == 4
tool.stdin.write('{"type":"hello","protocol":4,"app":"a script","version":"0"}\n')
zip = os.path.abspath("fixtures/selftest/factory/cheetah-test.260101.001-factory-31c2322a.zip")
moves = {"acknowledge": {"type": "acknowledge"}, "read-phone": {"type": "read-phone"}, "accept-terms": {"type": "decline-terms"},
         "take-file": {"type": "take-file", "path": zip}, "dry-run": {"type": "dry-run"},
         "answers": dict(type="answers", username="alex", password="2580", password_again="2580", hostname="", timezone="Europe/Paris", wifi_ssid="", wifi_psk="")}
screen = send(type="start", reinstall=False)
while screen["action"] in moves:
    screen = send(**moves[screen["action"]])
print(screen["state"], screen["stop"], screen["retry"], screen["title"], *screen["plan"], sep="\n")
```

Since the lines are taken in order (§3.1), a script that knows the way can also write them all at once and read what comes back; the README has that as ten lines of shell. A stop on the way shows as a screen whose `state` is `stopped`, with the stop's id in `stop` and whether a new run can help in `retry`; the actions written after it are each answered with `error`.

The app has two commands of its own with no display, for the same readers: `omainstall check` (§7) and `omainstall open`.

### 3.8 An example

```
<- {"type":"hello","protocol":4,"tool":"omamobile-install","version":"0.0.1","writes":false}
-> {"type":"hello","protocol":4,"app":"omainstall","version":"0.1.0"}
-> {"type":"start","reinstall":false}
<- {"type":"screen","screen":{"state":"warning", ..., "action":"acknowledge"}}
-> {"type":"acknowledge"}
<- {"type":"screen","screen":{"state":"phone", ..., "action":"read-phone"}}
-> {"type":"read-phone"}
<- {"type":"screen","screen":{"state":"google-terms", ..., "action":"accept-terms"}}
-> {"type":"accept-terms"}
<- {"type":"screen","screen":{"state":"factory-image", ..., "action":"download"}}
-> {"type":"download","do":"start"}
<- {"type":"progress","what":"download","done":1048576,"total":3912345678}
<- ...
<- {"type":"screen","screen":{"state":"answers", ..., "action":"answers"}}
-> {"type":"answers","username":"Root","password":"…","password_again":"…","hostname":"","timezone":"Europe/Paris","wifi_ssid":"","wifi_psk":""}
<- {"type":"problems","problems":[{"field":"username","message":"Use lower-case letters, digits, - and _, starting with a letter (at most 32)."}]}
<- {"type":"screen","screen":{"state":"answers", ..., "action":"answers"}}
-> {"type":"answers","username":"alex", ...}
<- {"type":"screen","screen":{"state":"dry-run", ..., "action":"dry-run"}}
-> {"type":"dry-run"}
<- {"type":"screen","screen":{"state":"done", ..., "plan":[...], "action":"none"}}
-> {"type":"quit"}
<- {"type":"bye"}
```

In a run started with `--write` (the hello then says `"writes":true`) the dry run ends at the confirmation instead, and the rest is:

```
-> {"type":"dry-run"}
<- {"type":"screen","screen":{"state":"confirm", ..., "plan":[...], "action":"confirm"}}
-> {"type":"confirm","typed":"cheetah"}
<- {"type":"screen","screen":{"state":"write", ..., "action":"write"}}
-> {"type":"write"}
<- {"type":"progress","what":"verify","done":1642290,"total":1642290}
<- {"type":"progress","what":"write","done":0,"total":1642290}
<- {"type":"sent","command":"download:00004000"}
<- {"type":"sent","command":"flash:boot_b"}
<- ...
<- {"type":"piece","number":1,"blocks":131,"bytes":258144,"seconds":0.41,"line":"flash:userdata (piece 1: 131 blocks, ...)"}
<- ...
<- {"type":"account","lines":["userdata, the piece that covered the most blocks: ...", "userdata, the flash the phone took longest over: ..."]}
<- {"type":"sent","command":"set_active:b"}
<- {"type":"sent","command":"reboot"}
<- {"type":"screen","screen":{"state":"installed", ..., "action":"none"}}
```

### 3.9 After the write: the install link (protocol 5)

For a release whose `manifest.json` says `install_link` (Omarchy Mobile's INSTALL-LINK-PLAN, stage L6; DESIGN §13), a write that ran through does not end the run. The `installed` screen's action is `link`, and from `{"type":"link"}` on the screens are the install link's, made by the core's `LinkFlow` (`core/src/link.rs`) from the phone's first start to a phone that is ready. A release without `install_link` ends at `installed` (`none`), as in protocol 3.

| `state` | What the phone and the installer do | `action` |
| --- | --- | --- |
| `first-start` | The phone starts Omarchy Mobile for the first time and grows its system; the tool looks for the link's USB interface (§4 of the plan: by class, subclass, protocol and string). After ten minutes the screen's `problem` says what to check; it goes on looking | `wait` |
| `pair` | The phone shows a six-digit code. Or, after its own setup, it shows none until the owner taps "Setup is not finished" in its unlocked session; or too many wrong codes locked it | `pair`, or `wait` while the owner acts at the phone |
| `setup` | The answers go to the phone (the password as its sha512-crypt hash); first-boot setup runs. In a run that went on from a record before the phone had the answers, they are asked again. When first-boot setup failed: the steps, and `retry` | `wait`, `answers`, `retry` |
| `radio-backup` | The phone's radio partitions are copied to a folder on this computer, as `tools/nv-backup` writes one | `backup-dir` until a folder is chosen, then `wait` |
| `factory-send` | The owner's factory image is read from its file a piece at a time and sent; the phone checks it whole | `wait` |
| `provision` | The phone extracts and installs its firmware. When it failed, or the battery is too low: `retry` | `wait`, `retry` |
| `restart` | The planned restart; the tool waits for the link to come back | `wait` |
| `check` | The phone's own check report | `wait` |
| `ready` | Done. A camera step that failed is said ("Cameras: not set up") | `none` |
| `stopped` | One of the link's stops (`link-wrong-phone`, `link-version`, `link-protocol`, `link-computer`, `link-refused`, `backup-precondition`, `backup-mismatch`, `factory-mismatch`, `no-space`, `too-large`), with `retry` | `none` |

**A screen whose action is `wait` does not end an action.** While the phone works by itself the tool sends a `screen` each time it changes, and `progress` with `what` `link-wait`, `firstboot`, `backup`, `factory-send`, `factory-check` or `provision`. The action that was sent (`link`, `pair`, `backup-dir`, `retry`, `answers`) is answered by the first screen whose action is not `wait`. So §3.5's rule holds with "a screen" read as "a screen that asks for something". The app stays busy through the `wait` screens and draws each one. A `quit`, or the end of the input, leaves the link where it is: the record in the work directory (below) lets a new run go on.

| `Screen.action` | Message | The tool hands the link's flow |
| --- | --- | --- |
| `link` | `{"type":"link"}` | `Flow::link` with the answers it kept in memory and the factory image's path it checked; then runs the link. The app sends it by itself, as it sends `write` after `confirm` |
| `pair` | `{"type":"pair","code":"<as typed>"}` | `LinkFlow::pair`, the text as typed; the phone decides. A wrong code is the same screen with the core's `problem` (tries left) |
| `backup-dir` | `{"type":"backup-dir","path":"<absolute path>"}` | A new, empty folder (made mode 700, its files 600) for the radio backup. A folder that is not empty, or a path that is not absolute: `error`, and the step stays |
| `retry` | `{"type":"retry"}` | First-boot setup again, or provisioning again |
| `answers` | as §3.4 | Only in a run that went on from a record before the phone had the answers |

**`answers` carries three more fields, each text or left out:** `ssh_key` (one OpenSSH public key's line), `full_name` and `email` (for git on the phone). In a run that writes, for a release with the link, they and the seven others are checked by the link's own rules (first boot's, the protocol crate's `Answers::validate`) before anything is written, and an objection is an `error`.

**`start` with `"link": true`** goes on from the record a run before this one kept in the work directory (`--work-dir`, or the user's cache directory): `link/record.json`, mode 600, holding the serial the write pinned (for the serial proof: it never crosses the link), the session token, the factory image's path and the backup folder. It needs `--release DIR` too. The record holds no answer and no password hash; the answers are asked again if the phone has none yet.

**A pretend build** (`fake-phone`) says `{"type":"pretend","shows":"<code>"}` before a screen that asks for the code: what its pretend phone's screen shows, for the tests and the recorded pictures. A build that ships never says it.

## 4. The window

One window, comfortable from 720x560, resizable. Text can be selected, and a screen longer than the window scrolls, with the scroll bar always on show then.

From top to bottom:

- **What this is** (DESIGN §7, rule 7): the app's name and which kind of run the window was started for. "Dry run" and "This run cannot write to a phone. It ends at a dry run."; or, started with `--write`, "Install" in the warning colour and "This run can write to a phone. It writes only after it has listed what it will write and you have typed the phone's codename."
- **The stages** (warning, phone, factory image, about you, dry run, and in a run that writes a sixth, install, and a seventh, set up, for the steps after the write), the current one marked, from `Screen.state`. Hidden on a stop.
- **The screen**, in the order the command-line tool prints it:

  | `Screen` | Drawn as |
  | --- | --- |
  | `title` | The heading. On a stop, under the word "Stopped" |
  | `notice` | A bordered box right under the title, in the warning colour, bold |
  | `body` | Paragraphs |
  | `quote` | Set in behind a bar in the accent: the intro, each line in italics and quotation marks, what comes after, and each link as a button that opens the browser |
  | `steps` | Numbered. A step's `command` in a monospace box with "Copy" |
  | `choices` | One full-width button for each, labelled with the core's words, in the core's order; a click sends its `answer` (`slot-b`) |
  | `link` | "Open the page in the browser", with the address under it |
  | `file` | A box with the file's name, its size and the build |
  | `plan` | A numbered monospace list |
  | `problem` | A box in the warning colour. A screen that has one is scrolled so that it shows |
  | (the answers form) | When `action` is `answers` |
  | `footer` | Closing lines behind a grey bar |
  | (what was sent) | From the first `sent` of a write on, and kept on the screens that follow it: a box "Sent to the phone" with the tool's `sent` commands, each `piece`'s `line` and the `account`'s lines, in order. While the writes run, the newest eight and how many there are; afterwards all of them |

  All of it is plain text: nothing the tool sends is read as markup.
- **The bar at the bottom:** what is running, with bytes for a download, a check and a write; the tool's `error` or the file chooser's failure; the field for the phone's codename, when `action` is `confirm`; and the buttons.

**The buttons**, from `Screen.action`. The labels are the app's own and are translated:

| `action` | Main action (also Space and Enter) | Beside it |
| --- | --- | --- |
| `acknowledge` | I have read this. Go on | Stop here |
| `read-phone` | Look at the phone | |
| `slot-b` | (none: each of the core's `choices` is its own button, on the screen; none is set apart as the main one) | |
| `after` | (none: as `slot-b`, each choice is its own button on the screen; a click sends `after` with its answer. While `firmware` runs the window holds, as during a write) | |
| `accept-terms` | I accept. Download the file | I will download it myself |
| `download` | Download | Choose a file I saved…; while it runs: Cancel |
| `take-file` | Choose the file… | |
| `answers` | Go on | |
| `dry-run` | Run the dry run | |
| `confirm` | Erase the phone and install (disabled until something is typed in the field beside it) | Do not install |
| `write` | (none while it runs; "Look for the phone again" if the writes could not begin) | |
| `none` | Close | Start again, on a stop whose `retry` is `whenever` or `after-fix` |

**A stop and the last screen** keep the window open with the core's text and "Close". **A stop also has "Start again" beside it exactly when the core says a new run can get past it:** `Screen.retry` is `whenever` or `after-fix` (§3.3). It ends the tool and starts it anew, and so the flow. A stop whose `retry` is `stops-again`, a word for `retry` the app does not know, and the last screen of a dry run have "Close" alone. The app reads the field and never the stop's sentences.

**The confirmation** (`confirm`) is the core's screen: the list of what will be written, what is erased, and what to type. The app adds a field for the codename in the bar at the bottom, where it is in sight however long the list is. It does not take the focus by itself and is emptied with the step. What is typed is sent as typed; the core decides what it is worth. "Do not install" sends an empty text, for which the core stops with nothing written. With nothing typed the main button is disabled and Space and Enter do nothing, so the install is never confirmed, or refused, by a stray key.

**The writes follow the codename at once.** When the screen that answers `confirm` names `write`, the app sends `write` itself: the codename was the owner's go-ahead (DESIGN §3), and the app adds no second question, as the command line adds none. If that try finds no phone, the step stays open with the tool's words and is tried again only by a click.

**While the phone is written** the bar shows the progress and no button. Nothing cancels a write. Closing the window, or `Q`, does not ask whether to quit: the window says "The installer is writing to the phone. This window stays open until that has ended." and stays. If the app is ended all the same, it tells the tool to leave and waits for the write to end (up to thirty minutes) rather than ending the tool. The same holds when the tool sends something the app cannot read in the middle of a write (§6): the app stops reading and says so, but it does not end the tool. It closes the tool's input, so that the tool finishes the write and leaves, says that it has done so, and offers neither "Try again" nor a way to close the window until the tool has left. Then it says that the tool has left and that it does not know how the write ended: that was in what it could not read.

**The answers form** has the seven fields of `AnswersInput`. The time zone starts as this computer's. The form checks nothing: it sends what was typed, and the core's objections come back per field and are shown under the field they are about. What was typed stays.

**The file** is chosen through xdg-desktop-portal (`org.freedesktop.portal.FileChooser`, over D-Bus), behind an abstract picker so that the tests hand in a fake; never `QFileDialog`. A saved file can also be dropped on the window. Either way the app sends the path and reads nothing.

**More than one phone:** the tool's `title` and one button per `label`.

**Keys.** `?` shows the shortcuts and `Q` quits; the full list is in [app/README.md](../app/README.md). While a field has the focus the letters go to the field, and while a button has the focus Space and Enter press that button, so that a person who tabbed to "Stop here" cannot accept by accident.

**Quitting** while an action is in hand asks first ("A step is still running. Quit anyway?"), except while the phone is written (above). Quitting sends `quit` and ends the tool if it has not left a moment later.

## 5. Omarchy's conventions

The app follows the `omarchy-app` conventions: C++ and Qt Quick, `qmake6` and `make`, a flat `src/`, one `Backend` object handed to QML, the Material style in dark. The accent comes from the current Omarchy theme and the corner radius from Hyprland's `decoration:rounding`, both live; without Omarchy the accent is `#FFD60A` and the corners are sharp. The Wayland app id, the `.desktop` file, the icon and the binary are all `omainstall`.

The buttons are Material buttons with a plain face of the app's own (`ActionButton.qml`): the main action is filled with the accent and labelled in black or white, whichever reads on it.

## 6. When the tool itself fails

Said in the window, in place of the screen, with "Try again" (a new run of the tool, and so a new flow) and "Close". The first line is the app's; under it are the tool's own words where there are any. The app never goes on from a state it cannot read.

| What happened | What the app says | And does |
| --- | --- | --- |
| No tool (§2) | The installer's tool (omamobile-install) was not found. It belongs beside this app or on the PATH. | Starts nothing |
| It could not be started | The installer's tool could not be started. (the system's reason) | |
| No `hello` within 10 seconds | The installer's tool started and did not say hello, so the app stopped it. | Ends the tool |
| Another protocol number | The installer's tool speaks protocol N and this app speaks protocol M. Update whichever is older. | Sends nothing, ends the tool |
| A hello whose `writes` is not what the window was started for | This window was started for a dry run, and the installer's tool started a run that can write. The app stopped it. (or the reverse) | Sends nothing, ends the tool |
| A line that is not a JSON object, a message of an unknown type, a message or a screen that lacks a field, a line of more than 4 MiB | The installer's tool sent something this app cannot read, so the app stopped it rather than guess. (what was wrong, and the start of the line) | Ends the tool |
| `fatal` | The installer's tool reported that it failed: (its message) | |
| Any of the above while the tool was writing to a phone | The line for what happened, and under it: The installer's tool was writing to the phone when this happened. The app has not ended it: it finishes the write and then leaves. Keep the phone connected. | Closes the tool's input and does not end it; no "Try again" and no closing until it has left. The app says nothing about how the write ended: it could not read it |
| It exited without being asked, or crashed | The installer's tool stopped unexpectedly (exit status N). / The installer's tool crashed. If a screen was on show: What this window showed before may no longer be true. (the end of its standard error) | |

A file chooser that cannot be shown (no portal, no session bus) is said in the bar at the bottom, with the system's reason and the way that still works: dropping the file on the window.

## 7. Commands with no display

- `omainstall check [--json]` finds the tool, starts it, reads its `hello` and sends `quit`. It prints the tool's path, version and protocol number, and exits 0 when the tool is there and speaks this app's protocol; otherwise 1, with the reason on standard error (and in the JSON as `error_kind` and `error`). It asks nothing of a phone (§3.2).
- `omainstall open [--reinstall] [RELEASE_DIR]` is the window, for a dry run. `omainstall` alone is the same.
- `omainstall open --write [--reinstall] RELEASE_DIR` is the window for an install: the same flow, which after the dry run lists what it will write and writes once the phone's codename is typed. `--write` is never implied, and nothing in the window sets it.

There are no others. Driving an install without a window is the tool's `json` mode (§3.7).

## 8. Words and languages

The app's own strings go through the `Translator` of the Omarchy conventions: English is the base, a language is one flat JSON file in `app/src/i18n/`, and the window follows Omarchy's language live. German (`de.json`) is the working example; a test fails when a string of the app is missing from it or a line of it is no longer used.

**The core's sentences arrive in English and are shown as they are.** A translated window therefore has buttons in one language and the text about the phone in another. Translating the core's sentences is an open question for the core, not the app: they are composed there, from the phone's state, and a front end that translated them would be writing sentences about the phone. If the core gains languages, the natural seam is a language in `start`, and the screens arrive translated.

## 9. What is open

- **A run with a real phone.** `omamobile-install json` finds the phones in Fastboot Mode over USB as `install` does, and that part of it has never run: every test and every run so far used the pretend phone. Unknown until then, besides what DESIGN §9 lists: what a Pixel gives as its product name over USB (the `phones` label), and how two connected phones show.
- **Where the release comes from.** The tool takes a directory. `omainstall open DIR` passes one on; without it `start` ends in a `fatal` that says so, because the tool can't find or fetch a release itself yet.
- **The owner's statement about a bootloader slot, through the app.** Decided for now (2026-11-13): the command line only (DESIGN §6), and first installs stay there. A Pixel 7 Pro reaches a write in the app only as a reinstall from slot B (DESIGN §6, "A reinstall from slot B"); a first install stops at the bootloader-slot stop. The later goal is that the installer measures the other slot itself (DESIGN §6, "Measuring the other slot").
- **A write through the app on a real phone.** Not run. On a Pixel 7 Pro it is runnable as a reinstall only, for the reason above.
- **The maintainer's override through this mode.** `json` has no argument for it (§3.6), so the maintainer's own dry runs on a Pixel 7 Pro go through `install --dry-run` on a terminal, not through the app. Whether the app should ever show such a run is the maintainer's decision.
- **Windows and macOS.** The mode keeps standard output for the protocol by moving the descriptor aside, on Unix only; elsewhere it writes to standard output as it is. It was built and run on Linux only.
- **Saying that this is a reinstall.** The core now names the choice: a phone on slot B is asked what slot B holds (`slot-b`, with `Screen.choices`), and the app draws the core's three answers as buttons, with no main action of its own on that screen. `omainstall open --reinstall` still says it up front, and the question is then not asked.
- **Cancelling the check of a file handed in.** A factory image is several gigabytes and hashing it takes a while; the protocol can cancel a download and nothing else (a `quit` does end the check). The same goes for a dry run that walks a release's root image.
- **A signal.** Ctrl-C in the terminal the app was started from reaches the tool too, as it reaches `install --write`, and would cut a write off. The window's own close is held while the phone is written; a signal is not.
- **A download that has not begun to arrive.** A cancel takes effect once Google's server has answered; while the tool still waits for that answer (at most a minute and a half, by its timeouts) it is not seen.
- **The file chooser on a real session.** The portal code has run against no portal: the tests use a fake picker. The chooser is opened without a parent window handle, so it is not tied to the app's window.
- **A look on a real desktop.** Every picture of the window so far is an offscreen render with the software backend and the default accent (`app/docs/screens/`, written by `app/bin/screens`).
- **Languages for the core's sentences** (§8).
- **DESIGN.md §9** still describes the desktop window as "a web view around the same page". This app is another answer to that line; DESIGN.md was left as it is and should be brought in step when this lands.

## 10. Tests

**The tool** (`cli/tests/json_mode.rs`, `cargo test -p omamobile-installer-cli --features fake-phone`): the built tool driven over its standard input and output. The handshake and another protocol number; the whole flow to the dry run as a transcript, with a download from a stand-in for Google's server (a socket on 127.0.0.1 in the test); each kind of stop the pretend phone, a changed release and a wrong download can reach, with its `retry`; malformed lines before and after `start`; every action out of turn at every step; a download cancelled, ended by `quit` and ended by the input closing, and gone on with from the bytes that arrived; the input closing at each step; two phones and none; every line written at once; arguments that are refused; a panic. Every test ends by checking of the whole run: standard output held JSON lines of the protocol and nothing else, each action was answered by exactly one screen, and, in a run that was not started with `--write`, the pretend phones received `getvar` and the one unlock question and nothing else (their own record, which a pretend run writes to standard error) and the tool said no `sent`, `piece` or `account`. Whatever the run, a pretend phone received nothing that the write path may not send.

The runs that write (`--write`, against the pretend phone only): a whole install as a transcript, with the files all checked before the first command that writes, what the tool said it sent compared with what the phone received, and the six partitions written; anything but the codename, an empty text among it, stops with nothing written, and a `confirm` without `typed` or a `write` before the codename moves nothing; the pretend phone's cable pulled before the first write, among the boot images and inside `userdata`, each ending in the core's stop with slot B not made active; a `quit` written right behind a `write`, which waits for it; a phone that tells one bootloader slot, which stops with `--write` as without; and the hello's `writes`.

A `fake-phone` build takes `json --fake-phone STATES [--fake-google 127.0.0.1:PORT] [--fake-unplug COMMAND:N]`, as `flow --fake-phone` does (`--fake-unplug flash:userdata:2` pulls the pretend phone's cable as the second command that starts with `flash:userdata` is sent): `STATES` is one of the pretend phone's states (`ready`, `locked`, `slot-b`, …), several with commas between them for several phones connected at once (`ready,slot-b`), or `none`. Such a run trusts the test bundle's key, opens no USB device and reaches a stand-in for Google's server or nothing. A build that ships has none of these arguments (a unit test of the mode's argument parser, in every build).

**The steps after the write** (`cli/tests/after_write.rs`, the same feature): the built tool with `--write`, the release `fixtures/after` (the test bundle with stand-ins for the release's tools, signed with the test key), `--fake-after` naming stand-ins for `nmcli` and `ssh-keygen`, and a pretend phone that answers over "USB SSH" kept in a folder: the whole set-up step by step, a setup cut off and gone on with, one forgotten, another phone that answers, NetworkManager refusing, the key refused, a first start that runs out, a tool that is not the signed one (nothing runs), a backup that fails. `HOME` and the XDG folders are the test's own.

**The app.** `app/bin/test` builds and runs one Qt Test binary, offscreen. It needs no phone, no USB and nothing from the internet. It first builds the tool with the `fake-phone` feature (cargo); without cargo it says that the tests against the real tool are skipped, and they report as skipped.

- **The stand-in** (`app/tests/stand-in-tool`, Python) speaks the protocol and replays recorded screens. It can replay one screen, walk the whole flow (to the dry run, or through the write when it was started with `--write`), offer two phones, hold a download open until it is cancelled, hold a write open for two and a half seconds, end a write in the core's stop for a lost link, find no phone at the first try of a write, and fail in each way of §6, which the real tool can't be asked to do.
- **The recorded screens** (`app/tests/fixtures/screens/`, 60 files) are the core's own output: `Flow::screen()` serialised by serde, for every state and every kind of stop, driven against the core's pretend phone and test bundle. `app/tests/record-screens/run` writes them again from the core as it is; run it when the flow's words or the `Screen`'s fields change. They include stops the tool's pretend phone can't be brought to from outside (a phone that changes at the dry run or at the write, a release for another phone), every place a write can stop that the core's closing lines tell apart, and screens the app can't begin itself (a run on the owner's statement about a bootloader slot, with its notice).
- **Written by hand,** for the stand-in, from the flow's own sentences: the two `problems` it sends, and its `phones` and `error` messages.
- **Against the stand-in:** every recorded screen reaches QML equal to what was sent; each action and each button sends exactly its message, and an action the screen does not name is not sent; "Start again" is on a stop exactly when its `retry` says so, and starts a new run; malformed JSON, a screen that lacks a field, an unknown message, an unexpected exit, a crash, a protocol mismatch, a `fatal`, a tool started for the other kind of run, a silent tool and a missing tool each give the stated error; a run that writes: the switch passed on only when the window was started with it and never for a probe, the flow through the write with the tool's record of what it sent, the confirmation in the window (the field, the disabled button, "Do not install"), a window that stays while the phone is written and an app that waits for the write when it is ended, a write that finds no phone, and a write that stops partway; the file picker is the injected fake; and `Main.qml` is loaded for every recorded screen, compared with it text for text, and fails on any QML warning.
- **Against the real tool** (`omamobile-install json --fake-phone …`, started through a script the test writes, since the app itself passes only `json` and the release): the straight path to the dry run clicked in the window, with a download from a stand-in for Google's server in the test; eight stops, each drawn from the real screen with "Start again" where its `retry` allows, and a ninth for a download that is not the listed file; a locked phone; a reinstall on slot B; two phones in the chooser; a file handed in that is missing, wrong and right; a download that stands still, cancelled and gone on with; and the hello that `omainstall check` reads. Each ends by reading the pretend phones' record: reads only.
- **Against the real tool, in a run that writes** (`json --write --fake-phone …`): a complete install clicked in the window, with a download from the stand-in, ending with the pretend phone's record of slot B's five boot images, the pieces of `userdata`, `set_active:b` and `reboot`; an install the owner does not go ahead with, by the button and by a wrong word, with reads only; the pretend phone unplugged inside `userdata`, with the core's account and the tool's record on the window; a download that breaks off and goes on from what arrived, and one the server has no file for; and a phone that tells one bootloader slot, which stops with nothing the app sent naming a slot.
- **The pictures** (`app/bin/screens`, the test `theScreensOfAPretendInstall`): the same walk with one picture for each screen, saved to `app/docs/screens/`. A plain test run makes them in a scratch directory, so the walk is tested; they are looked at by a person.
