# The desktop app: design

Status (2026-11-12): **the app and the tool's side of the protocol below (`omamobile-install json`, `cli/src/json.rs`) are both built, and tested against each other with the tool's pretend phone. Neither has been run with a real phone: the tool's USB path in this mode has never listed or opened a device.** Like the web preview, this version goes to the dry run and no further.

`omainstall` (`app/`) is a Qt Quick window in the style of Omarchy's own apps. Its one job is to guide a person through installing Omarchy Mobile on a phone plugged into this computer. It is a third front end for the flow in [DESIGN.md](DESIGN.md), next to the command-line tool and the web page, and DESIGN §7 ("What a front end must do") applies to it.

This document is the app's design and, in §3, the protocol between the app and the tool. §3 is the specification of `omamobile-install json`, which is also the installer's interface for agents and scripts (§3.7).

## 1. What the app is, and is not

- **A thin front end.** The core decides the order of the steps, every refusal and every sentence a person reads about the phone. The app draws the `Screen` it is sent and hands in what the person did.
- **No install logic.** The app has no check of a phone, a release, a file or an answer. It does not open a USB device, does not fetch anything from the internet, does not hash a file and does not read one: it passes a path.
- **No sentence about the phone of its own.** The app's own words are the labels of its buttons and fields, the names of the five stages at the top, the shortcut list, the line that says it is a preview, and what it says when the tool itself fails (§6). Even the question "which of these phones?" is the tool's (§3.3, `phones`).
- **It cannot write to a phone.** The app passes the tool one fixed argument (§2) and has no message that asks for a write (§3.6).

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

**Starting it.** `omamobile-install json`, or `omamobile-install json --release DIR` when the app was started as `omainstall open DIR`. Those are all the arguments the app ever passes.

**The mode's arguments are a closed list:** `--release DIR`, and `--work-dir DIR` (where a download of Google's factory image is kept; by default the user's cache directory, as for `install`), which the app does not pass and a script may. A build with the `fake-phone` feature also takes `--fake-phone STATES` and `--fake-google 127.0.0.1:PORT` (§10); a build that ships has neither. Any other argument is refused before `hello`, with a `fatal` and exit status 1: there is no argument that writes to a phone, none that names one (`--serial`), and none for the maintainer's override (§3.6).

**Without `--release`** the tool still says `hello`, so `omainstall check` works anywhere. `start` then fails with a `fatal`: this version can't find or fetch a release by itself (§9).

## 3. The protocol

Protocol number **1**.

### 3.1 Framing

- One JSON object per line, in each direction, UTF-8, ended by a line feed. No line feed inside a message. An empty line is skipped. The order of an object's keys means nothing (the tool writes them sorted).
- Every message has a `type`.
- A line to the tool is at most 1 MiB, and a last line that lacks its line feed still counts.
- **The tool takes the lines in the order they were written,** each after the one before it was answered. A script may therefore write several at once. While an action runs, the tool looks at the next line only to see whether it is a cancel or a quit (§3.4).
- The tool's standard output carries protocol messages and nothing else. What it has to say besides goes to standard error, as free text; the app shows the end of it only when the tool stops unexpectedly.
- The app never sends a message the tool did not ask for, with two exceptions: `download` with `"do": "cancel"`, and `quit`.

### 3.2 The handshake, and versions

```
tool -> app   {"type":"hello","protocol":1,"tool":"omamobile-install","version":"0.0.1"}
app -> tool   {"type":"hello","protocol":1,"app":"omainstall","version":"0.1.0"}     (not answered)
app -> tool   {"type":"start","reinstall":false}
tool -> app   {"type":"screen","screen":{...}}
```

- The tool says `hello` first, at once, before it reads a release or looks at anything. **Before `start` it opens no USB device and fetches nothing.** `omainstall check` relies on that: it reads the hello and sends `quit`.
- `protocol` is one whole number. There is no negotiation. A side that reads another number than its own stops:
  - the app shows "The installer's tool speaks protocol N and this app speaks protocol M. Update whichever is older.", sends nothing more, and ends the tool;
  - the tool answers the app's `hello` with `fatal` and exits with status 1. A `hello` that names no whole number as its `protocol` is taken the same way. Of the app's `hello` the tool reads `protocol` and nothing else.
- `start` comes after the app's `hello`, once. It carries what the person said up front (`FlowOptions`): `reinstall`, true or false, slot B already holds Omarchy Mobile. The tool then reads the release, calls `Flow::begin` and sends the first screen: the warning, or a stop if the release fails its checks. A release that can't be read at all (no directory was named, or a file of it is missing) is a `fatal`.
- **Before `start` there is no flow and so no screen.** What the tool can't take then (a line that is not a JSON object, a `start` before the app's `hello` or without `reinstall`, any other message but `quit`) is answered with `error` alone.
- A new message type, a new field the other side must understand, or a changed meaning takes a new protocol number. A new *optional* field does not: both sides ignore fields they do not know. A new value of `Screen.action` does not either: an app that does not know it shows the screen, offers no main action, and says "The installer asks for a step this app does not know (…). Update the app to go on."

### 3.3 From the tool

| `type` | Fields | Meaning |
| --- | --- | --- |
| `hello` | `protocol` (number), `tool` (text), `version` (text) | §3.2 |
| `screen` | `screen`: the core's `Screen` | The flow's current screen. Ends the action in hand (§3.5) |
| `progress` | `what` (text), `done` (number), `total` (number) | Bytes done and bytes in all, for the action in hand. `what` is `download` (Google's factory image arriving), `check` (a file handed in being hashed) or `dry-run` (the release's images on disk being checked and walked through the dry run: each boot image once and the root image twice; none is sent when the images are not there). Another word is shown as plain "Working". `total` 0 means the end is not known. A few a second at most |
| `phones` | `title` (text), `phones`: `[{"id": text, "label": text}]`, at least one | More than one phone in Fastboot Mode answered a `read-phone` that named none, and none of them is the phone the flow looked at before. `title` is the question, in the tool's words. `id` is a name the tool makes up for this run (a number, as text) and stays that phone's for the run; `label` is what the person can tell the phones apart by: the product name the phone gives over USB, and the USB bus and port it is on. **Neither is a serial number** (the core prints none, README "Safety") |
| `problems` | `problems`: `[{"field": text, "message": text}]` | The core's objections to the owner's answers, as `AnswerProblem` serialises: `field` is `username`, `password`, `hostname`, `timezone`, `wifi_ssid` or `wifi_psk`. The flow stays at `answers` |
| `error` | `message` (text) | What was sent did not go through and the flow did not move for it: a file that is not there, a phone that stopped answering, an action the screen does not name, a message of a type the tool does not know, a line that is not a JSON object. The tool's words, shown as they are. It repeats nothing of what was sent but the message's type and a phone's id |
| `fatal` | `message` (text) | The tool itself failed and is about to exit with a status other than 0: an argument it does not take (the one `fatal` that comes before `hello`), another protocol number, a release that can't be read, a bug (a panic is caught and said this way) |
| `bye` | | The answer to `quit`. The tool then exits with status 0 |

**`screen.screen` is exactly `omamobile_installer_core::flow::Screen` as serde serialises it**, the same JSON the web page reads from `InstallFlow::screen()`:

```
state     text          warning, phone, unlock, google-terms, factory-image, answers, dry-run, done, stopped
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
action    text          acknowledge, read-phone, accept-terms, download, take-file, answers, dry-run, none
```

The app checks that every key is there and of that type. A screen that lacks one is malformed (§6): a missing `notice` must never be read as "no notice". Keys the app does not know are carried along and ignored.

`retry` is the stop's `Retry` in the core, the same fact its closing line says in words. It was added to `Screen` when the tool's side was built; no tool spoke protocol 1 before that, so it is part of protocol 1 and the number did not change. A front end offers "start again" from it (§4) and never from the stop's sentences.

### 3.4 From the app

One message per action that `Screen.action` names. The app sends an action only while the screen on show names it, and only when no action is in hand. **The tool holds to the same rule itself:** an action the flow's screen does not name is refused with `error` before the flow or a phone is touched, whoever sent it.

| `Screen.action` | Message | The tool hands the flow |
| --- | --- | --- |
| `acknowledge` | `{"type":"acknowledge"}` | `acknowledge_warning` |
| `acknowledge` | `{"type":"decline"}` | `decline_warning` |
| `read-phone` | `{"type":"read-phone"}` or `{"type":"read-phone","phone":"<id>"}` | No phone in Fastboot Mode: `no_phone`. One, or the one chosen: `read_phone`. Several and none chosen: the phone the flow looked at before, if it is among them; otherwise a `phones` message, and the flow is not touched. An `id` no `phones` message of this run gave, or a phone that is gone: `error` |
| `accept-terms` | `{"type":"accept-terms"}` | `accept_google_terms(now)`, by the tool's clock: the app sends no time |
| `accept-terms` | `{"type":"decline-terms"}` | `decline_google_terms` |
| `download` | `{"type":"download","do":"start"}` | `factory_download`, the fetch as the core asks for it, `factory_download_done`; `progress` with `what: "download"` on the way |
| `download` (while that runs) | `{"type":"download","do":"cancel"}` | The transfer is ended, also one that stands still, and handed to `factory_download_done` as one that broke off, so the bytes that arrived are kept and the core's screen says how far it came. The next `start` goes on from them. A cancel with no download running is ignored: it gets no answer at all. A `do` that is neither `start` nor `cancel`: `error` |
| `download`, `take-file` | `{"type":"take-file","path":"<absolute path>"}` | `factory_zip(name, size)`, the file's bytes in pieces, `factory_zip_done`; `progress` with `what: "check"` on the way. No such file, or a path that is not absolute: `error` |
| `answers` | `{"type":"answers","username":…,"password":…,"password_again":…,"hostname":…,"timezone":…,"wifi_ssid":…,"wifi_psk":…}` | `OwnerAnswers::check` on the seven texts of `AnswersInput`, as typed. Problems: a `problems` message. None: `answers`. A time zone this computer does not know is the tool's to refuse, as a problem of `timezone`. A field that is missing or is not text: `error`, which names the field and repeats nothing that was typed |
| `dry-run` | `{"type":"dry-run"}` | The phone the flow looked at (or the only one connected) is opened; then `dry_run`, the release's images walked through it when they are on disk, `dry_run_done` or `dry_run_failed`. No phone in Fastboot Mode: `error`, and the step stays open (the flow has no "no phone" at this step) |
| any | `{"type":"quit"}` | Nothing. A download or the check of a file that is running is ended (a download as if cancelled, so its bytes are kept); a look at a phone or a dry run that is running is finished, since both only read and neither takes long. The action in hand still gets its screen, and then the tool says `bye` |

Also from the app: `hello` and `start` (§3.2). The end of the app's output (the tool's standard input closes) means `quit`.

A `quit`, or the end of the input, takes its turn like every line (§3.1): it ends a running action only when it is the next line after that action's.

The fields of `answers` are those of `AnswersInput` in `core/src/flow.rs` today. All seven are always sent; an empty text means what it means there (the default phone name, no Wi-Fi, an open network).

### 3.5 What answers what

**Every action ends with exactly one `screen`**: the flow's screen after the action, changed or not. Before it the tool may send any number of `progress` messages, and at most one of `phones`, `problems` or `error`. From sending an action until that screen the app is busy: it shows what is running, offers no other action (but cancelling a download), and asks before it quits.

So a refused file is `screen` (the same step, with the core's `problem`); refused answers are `problems`, then `screen`; a file that is not there is `error`, then `screen`.

**The same holds for everything else that is sent after `start`,** so that a sender is never left waiting: a line the tool can't read, a message of an unknown type, an action out of turn, a second `hello` or `start` each get `error`, then the screen, unchanged. The exceptions are `quit` (answered by `bye`) and a cancel that finds no download (not answered).

After a screen whose `action` is `none` (a stop, or the end), nothing more is taken: an action sent then is answered with `error` and that same screen. The tool stays until `quit` or the end of its input; a front end must also take a tool that exits with status 0 by itself there for no failure.

### 3.6 What never crosses

- **No write.** The protocol has no message that writes to a phone, and the mode has no flag for it: its arguments are a closed list (§2), so a flag that another command of the tool takes for a write, now or later, stops this mode before it says `hello`. This version's flow ends at the dry run. A version that writes needs a new message, and with it a new protocol number, so an app of today can never ask a later tool for a write. The tool's tests end every run by reading the pretend phone's own record of what it received: `getvar`, the one read-only question of a locked phone, and nothing else.
- **No command.** A `Screen` step's `command` is text for the person's own terminal. The app shows it and copies it to the clipboard on request. It has no way to run it, and no message sends it back.
- **No address.** The app never sees where Google's file is fetched from. `Screen.link` and the quote's links are pages for the person, opened in their browser on their click, and only if they are `https`.
- **No serial number**, in either direction.
- **No override.** There is no message, argument or control for the maintainer's bootloader-slot override (DESIGN §6): `json` refuses `--maintainer-skip-bootloader-slot-check` like any argument it does not have, so a phone that reports one bootloader slot's version stops in this mode, and only `install --dry-run` on a terminal takes the override. The core's `notice`, which says on every screen that a check was skipped, is drawn right under the title all the same, for a later version in which it can arrive.
- **The owner's answers** go to the tool once, over the pipe, when the form is handed in. The app keeps them only in the form's fields, which are gone with the step. It writes them nowhere, and logs nothing of what it sends.

### 3.7 For agents and scripts

`omamobile-install json` is the installer's interface for anything that is not a person at a terminal, and the supported one: the terminal's `install` and `flow` print for a person, and their words and layout may change. An agent or a script starts the tool the same way as the app and exchanges the same lines: it reads the `Screen`, decides, and sends one of the actions of §3.4. The app holds nothing an agent would need, and the rules are the same for both, because they are the core's. Standard output is the protocol and nothing else; what the tool has to say besides is on standard error.

It ends at the dry run, like the app: nothing sent to this mode can write to a phone (§3.6). On a real phone it sends `getvar` and, to a locked phone, one read-only question, and it downloads Google's factory image once Google's terms were accepted with `accept-terms`. **An agent does not accept Google's terms, or the warning, for a person:** both are the phone's owner's to answer, and `decline-terms` with a file the owner saved is always a way through.

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
assert json.loads(tool.stdout.readline())["protocol"] == 1
tool.stdin.write('{"type":"hello","protocol":1,"app":"a script","version":"0"}\n')
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
<- {"type":"hello","protocol":1,"tool":"omamobile-install","version":"0.0.1"}
-> {"type":"hello","protocol":1,"app":"omainstall","version":"0.1.0"}
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

## 4. The window

One window, comfortable from 720x560, resizable. Text can be selected, and a screen longer than the window scrolls, with the scroll bar always on show then.

From top to bottom:

- **What this is:** the app's name, "Preview", and "This version cannot write to a phone. It ends at a dry run." (DESIGN §7, rule 7).
- **The five stages** (warning, phone, factory image, about you, dry run), the current one marked, from `Screen.state`. Hidden on a stop.
- **The screen**, in the order the command-line tool prints it:

  | `Screen` | Drawn as |
  | --- | --- |
  | `title` | The heading. On a stop, under the word "Stopped" |
  | `notice` | A bordered box right under the title, in the warning colour, bold |
  | `body` | Paragraphs |
  | `quote` | Set in behind a bar in the accent: the intro, each line in italics and quotation marks, what comes after, and each link as a button that opens the browser |
  | `steps` | Numbered. A step's `command` in a monospace box with "Copy" |
  | `link` | "Open the page in the browser", with the address under it |
  | `file` | A box with the file's name, its size and the build |
  | `plan` | A numbered monospace list |
  | `problem` | A box in the warning colour. A screen that has one is scrolled so that it shows |
  | (the answers form) | When `action` is `answers` |
  | `footer` | Closing lines behind a grey bar |

  All of it is plain text: nothing the tool sends is read as markup.
- **The bar at the bottom:** what is running, with bytes for a download; the tool's `error` or the file chooser's failure; and the buttons.

**The buttons**, from `Screen.action`. The labels are the app's own and are translated:

| `action` | Main action (also Space and Enter) | Beside it |
| --- | --- | --- |
| `acknowledge` | I have read this. Go on | Stop here |
| `read-phone` | Look at the phone | |
| `accept-terms` | I accept. Download the file | I will download it myself |
| `download` | Download | Choose a file I saved…; while it runs: Cancel |
| `take-file` | Choose the file… | |
| `answers` | Go on | |
| `dry-run` | Run the dry run | |
| `none` | Close | Start again, on a stop whose `retry` is `whenever` or `after-fix` |

**A stop and the last screen** keep the window open with the core's text and "Close". **A stop also has "Start again" beside it exactly when the core says a new run can get past it:** `Screen.retry` is `whenever` or `after-fix` (§3.3). It ends the tool and starts it anew, and so the flow. A stop whose `retry` is `stops-again`, a word for `retry` the app does not know, and the last screen of a dry run have "Close" alone. The app reads the field and never the stop's sentences.

**The answers form** has the seven fields of `AnswersInput`. The time zone starts as this computer's. The form checks nothing: it sends what was typed, and the core's objections come back per field and are shown under the field they are about. What was typed stays.

**The file** is chosen through xdg-desktop-portal (`org.freedesktop.portal.FileChooser`, over D-Bus), behind an abstract picker so that the tests hand in a fake; never `QFileDialog`. A saved file can also be dropped on the window. Either way the app sends the path and reads nothing.

**More than one phone:** the tool's `title` and one button per `label`.

**Keys.** `?` shows the shortcuts and `Q` quits; the full list is in [app/README.md](../app/README.md). While a field has the focus the letters go to the field, and while a button has the focus Space and Enter press that button, so that a person who tabbed to "Stop here" cannot accept by accident.

**Quitting** while an action is in hand asks first ("A step is still running. Quit anyway?"). Quitting sends `quit` and ends the tool if it has not left a moment later.

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
| A line that is not a JSON object, a message of an unknown type, a message or a screen that lacks a field, a line of more than 4 MiB | The installer's tool sent something this app cannot read, so the app stopped it rather than guess. (what was wrong, and the start of the line) | Ends the tool |
| `fatal` | The installer's tool reported that it failed: (its message) | |
| It exited without being asked, or crashed | The installer's tool stopped unexpectedly (exit status N). / The installer's tool crashed. If a screen was on show: What this window showed before may no longer be true. (the end of its standard error) | |

A file chooser that cannot be shown (no portal, no session bus) is said in the bar at the bottom, with the system's reason and the way that still works: dropping the file on the window.

## 7. Commands with no display

- `omainstall check [--json]` finds the tool, starts it, reads its `hello` and sends `quit`. It prints the tool's path, version and protocol number, and exits 0 when the tool is there and speaks this app's protocol; otherwise 1, with the reason on standard error (and in the JSON as `error_kind` and `error`). It asks nothing of a phone (§3.2).
- `omainstall open [--reinstall] [RELEASE_DIR]` is the window. `omainstall` alone is the same.

There are no others. Driving an install without a window is the tool's `json` mode (§3.7).

## 8. Words and languages

The app's own strings go through the `Translator` of the Omarchy conventions: English is the base, a language is one flat JSON file in `app/src/i18n/`, and the window follows Omarchy's language live. German (`de.json`) is the working example; a test fails when a string of the app is missing from it or a line of it is no longer used.

**The core's sentences arrive in English and are shown as they are.** A translated window therefore has buttons in one language and the text about the phone in another. Translating the core's sentences is an open question for the core, not the app: they are composed there, from the phone's state, and a front end that translated them would be writing sentences about the phone. If the core gains languages, the natural seam is a language in `start`, and the screens arrive translated.

## 9. What is open

- **A run with a real phone.** `omamobile-install json` finds the phones in Fastboot Mode over USB as `install` does, and that part of it has never run: every test and every run so far used the pretend phone. Unknown until then, besides what DESIGN §9 lists: what a Pixel gives as its product name over USB (the `phones` label), and how two connected phones show.
- **Where the release comes from.** The tool takes a directory. `omainstall open DIR` passes one on; without it `start` ends in a `fatal` that says so, because the tool can't find or fetch a release itself yet.
- **The maintainer's override through this mode.** `json` has no argument for it (§3.6), so the maintainer's own dry runs on a Pixel 7 Pro go through `install --dry-run` on a terminal, not through the app. Whether the app should ever show such a run is the maintainer's decision.
- **Windows and macOS.** The mode keeps standard output for the protocol by moving the descriptor aside, on Unix only; elsewhere it writes to standard output as it is. It was built and run on Linux only.
- **Saying that this is a reinstall.** The slot B stop tells the person to start the installer again and say so. The app takes it as `omainstall open --reinstall` and offers no control in the window, because the sentence beside that control would be about the phone. If the core named the choice (a field of the stop's screen, or an action), the app would draw it.
- **Cancelling the check of a file handed in.** A factory image is several gigabytes and hashing it takes a while; protocol 1 can cancel a download and nothing else (a `quit` does end the check). The same goes for a dry run that walks a release's root image.
- **A download that has not begun to arrive.** A cancel takes effect once Google's server has answered; while the tool still waits for that answer (at most a minute and a half, by its timeouts) it is not seen.
- **The file chooser on a real session.** The portal code has run against no portal: the tests use a fake picker. The chooser is opened without a parent window handle, so it is not tied to the app's window.
- **A look on a real desktop.** Every picture of the window so far is an offscreen render with the software backend and the default accent.
- **Languages for the core's sentences** (§8).
- **DESIGN.md §9** still describes the desktop window as "a web view around the same page". This app is another answer to that line; DESIGN.md was left as it is and should be brought in step when this lands.

## 10. Tests

**The tool** (`cli/tests/json_mode.rs`, `cargo test -p omamobile-installer-cli --features fake-phone`): the built tool driven over its standard input and output. The handshake and another protocol number; the whole flow to the dry run as a transcript, with a download from a stand-in for Google's server (a socket on 127.0.0.1 in the test); each kind of stop the pretend phone, a changed release and a wrong download can reach, with its `retry`; malformed lines before and after `start`; every action out of turn at every step; a download cancelled, ended by `quit` and ended by the input closing, and gone on with from the bytes that arrived; the input closing at each step; two phones and none; every line written at once; arguments that are refused; a panic. Every test ends by checking three things of the whole run: standard output held JSON lines of the protocol and nothing else, each action was answered by exactly one screen, and the pretend phones received `getvar` and the one unlock question and nothing else (their own record, which a pretend run writes to standard error).

A `fake-phone` build takes `json --fake-phone STATES [--fake-google 127.0.0.1:PORT]`, as `flow --fake-phone` does: `STATES` is one of the pretend phone's states (`ready`, `locked`, `slot-b`, …), several with commas between them for several phones connected at once (`ready,slot-b`), or `none`. Such a run trusts the test bundle's key, opens no USB device and reaches a stand-in for Google's server or nothing. A build that ships has neither argument (a unit test of the mode's argument parser, in every build).

**The app.** `app/bin/test` builds and runs one Qt Test binary, offscreen. It needs no phone, no USB and nothing from the internet. It first builds the tool with the `fake-phone` feature (cargo); without cargo it says that the tests against the real tool are skipped, and they report as skipped.

- **The stand-in** (`app/tests/stand-in-tool`, Python) speaks the protocol and replays recorded screens. It can replay one screen, walk the whole flow, offer two phones, hold a download open until it is cancelled, and fail in each way of §6, which the real tool can't be asked to do.
- **The recorded screens** (`app/tests/fixtures/screens/`, 35 files) are the core's own output: `Flow::screen()` serialised by serde, for every state and every kind of stop, driven against the core's pretend phone and test bundle. `app/tests/record-screens/run` writes them again from the core as it is; run it when the flow's words or the `Screen`'s fields change. They include stops the tool's pretend phone can't be brought to from outside (a phone that changes at the dry run).
- **Written by hand,** for the stand-in, from the flow's own sentences: the two `problems` it sends, and its `phones` and `error` messages.
- **Against the stand-in:** every recorded screen reaches QML equal to what was sent; each action and each button sends exactly its message, and an action the screen does not name is not sent; "Start again" is on a stop exactly when its `retry` says so, and starts a new run; malformed JSON, a screen that lacks a field, an unknown message, an unexpected exit, a crash, a protocol mismatch, a `fatal`, a silent tool and a missing tool each give the stated error; the file picker is the injected fake; and `Main.qml` is loaded for every recorded screen, compared with it text for text, and fails on any QML warning.
- **Against the real tool** (`omamobile-install json --fake-phone …`, started through a script the test writes, since the app itself passes only `json` and the release): the straight path to the dry run clicked in the window, with a download from a stand-in for Google's server in the test; eight stops, each drawn from the real screen with "Start again" where its `retry` allows, and a ninth for a download that is not the listed file; a locked phone; a reinstall on slot B; two phones in the chooser; a file handed in that is missing, wrong and right; a download that stands still, cancelled and gone on with; and the hello that `omainstall check` reads. Each ends by reading the pretend phones' record: reads only.
