# The desktop app: design

Status (2026-11-12): **the app is built and tested against a stand-in for the command-line tool. The tool's side of the protocol below does not exist yet, so the app has never driven the real core, and it has never been near a phone.** Like the web preview, this version goes to the dry run and no further.

`omainstall` (`app/`) is a Qt Quick window in the style of Omarchy's own apps. Its one job is to guide a person through installing Omarchy Mobile on a phone plugged into this computer. It is a third front end for the flow in [DESIGN.md](DESIGN.md), next to the command-line tool and the web page, and DESIGN §7 ("What a front end must do") applies to it.

This document is the app's design and, in §3, the protocol between the app and the tool, written so that the tool's side can be built from it.

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

**Starting it.** `omamobile-install json`, or `omamobile-install json --release DIR` when the app was started as `omainstall open DIR`. Those are all the arguments the app ever passes. Where the tool finds a release when no directory is given is the tool's to decide and is open (§9).

## 3. The protocol

Protocol number **1**.

### 3.1 Framing

- One JSON object per line, in each direction, UTF-8, ended by a line feed. No line feed inside a message. An empty line is skipped.
- Every message has a `type`.
- The tool's standard output carries protocol messages and nothing else. What it has to say besides goes to standard error, as free text; the app shows the end of it only when the tool stops unexpectedly.
- The app never sends a message the tool did not ask for, with two exceptions: `download` with `"do": "cancel"`, and `quit`.

### 3.2 The handshake, and versions

```
tool -> app   {"type":"hello","protocol":1,"tool":"omamobile-install","version":"0.0.1"}
app -> tool   {"type":"hello","protocol":1,"app":"omainstall","version":"0.1.0"}
app -> tool   {"type":"start","reinstall":false}
tool -> app   {"type":"screen","screen":{...}}
```

- The tool says `hello` first, at once, before it reads a release or looks at anything. **Before `start` it opens no USB device and fetches nothing.** `omainstall check` relies on that: it reads the hello and sends `quit`.
- `protocol` is one whole number. There is no negotiation. A side that reads another number than its own stops:
  - the app shows "The installer's tool speaks protocol N and this app speaks protocol M. Update whichever is older.", sends nothing more, and ends the tool;
  - the tool answers the app's `hello` with `fatal` and exits.
- `start` carries what the person said up front (`FlowOptions`): `reinstall`, slot B already holds Omarchy Mobile. The tool then calls `Flow::begin` and sends the first screen: the warning, or a stop if the release fails its checks.
- A new message type, a new field the other side must understand, or a changed meaning takes a new protocol number. A new *optional* field does not: both sides ignore fields they do not know. A new value of `Screen.action` does not either: an app that does not know it shows the screen, offers no main action, and says "The installer asks for a step this app does not know (…). Update the app to go on."

### 3.3 From the tool

| `type` | Fields | Meaning |
| --- | --- | --- |
| `hello` | `protocol` (number), `tool` (text), `version` (text) | §3.2 |
| `screen` | `screen`: the core's `Screen` | The flow's current screen. Ends the action in hand (§3.5) |
| `progress` | `what` (text), `done` (number), `total` (number) | Bytes done and bytes in all, for the action in hand. `what` is `download` (Google's factory image arriving), `check` (a file handed in being hashed) or `dry-run`. Another word is shown as plain "Working". `total` 0 means the end is not known. A few a second at most |
| `phones` | `title` (text), `phones`: `[{"id": text, "label": text}]`, at least one | More than one phone in Fastboot Mode answered a `read-phone` that named none. `title` is the question, in the tool's words. `id` is a name the tool makes up for this run; `label` is what the person can tell the phones apart by. **Neither is a serial number** (the core prints none, README "Safety") |
| `problems` | `problems`: `[{"field": text, "message": text}]` | The core's objections to the owner's answers, as `AnswerProblem` serialises: `field` is `username`, `password`, `hostname`, `timezone`, `wifi_ssid` or `wifi_psk`. The flow stays at `answers` |
| `error` | `message` (text) | The action in hand did not go through and the flow did not move for it: a file that is not there, a phone that stopped answering, a step handed in out of order (`Error::Refused`). The tool's words, shown as they are |
| `fatal` | `message` (text) | The tool itself failed and is about to exit with a status other than 0 |
| `bye` | | The answer to `quit`. The tool then exits with status 0 |

**`screen.screen` is exactly `omamobile_installer_core::flow::Screen` as serde serialises it**, the same JSON the web page reads from `InstallFlow::screen()`:

```
state     text          warning, phone, unlock, google-terms, factory-image, answers, dry-run, done, stopped
stop      text | null   for `stopped`: the stop's id (DESIGN §4)
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

### 3.4 From the app

One message per action that `Screen.action` names. The app sends an action only while the screen on show names it, and only when no action is in hand.

| `Screen.action` | Message | The tool hands the flow |
| --- | --- | --- |
| `acknowledge` | `{"type":"acknowledge"}` | `acknowledge_warning` |
| `acknowledge` | `{"type":"decline"}` | `decline_warning` |
| `read-phone` | `{"type":"read-phone"}` or `{"type":"read-phone","phone":"<id>"}` | No phone in Fastboot Mode: `no_phone`. One, or the one chosen: `read_phone`. Several and none chosen: a `phones` message, and the flow is not touched |
| `accept-terms` | `{"type":"accept-terms"}` | `accept_google_terms(now)`, by the tool's clock: the app sends no time |
| `accept-terms` | `{"type":"decline-terms"}` | `decline_google_terms` |
| `download` | `{"type":"download","do":"start"}` | `factory_download`, the fetch as the core asks for it, `factory_download_done`; `progress` with `what: "download"` on the way |
| `download` (while that runs) | `{"type":"download","do":"cancel"}` | The transfer is ended and handed to `factory_download_done` as one that broke off, so the bytes that arrived are kept and the core's screen says how far it came. A cancel with no download running is ignored |
| `download`, `take-file` | `{"type":"take-file","path":"<absolute path>"}` | `factory_zip(name, size)`, the file's bytes in pieces, `factory_zip_done`; `progress` with `what: "check"` on the way. No such file: `error` |
| `answers` | `{"type":"answers","username":…,"password":…,"password_again":…,"hostname":…,"timezone":…,"wifi_ssid":…,"wifi_psk":…}` | `OwnerAnswers::check` on the seven texts of `AnswersInput`, as typed. Problems: a `problems` message. None: `answers`. A time zone this computer does not know is the tool's to refuse, as a problem of `timezone` (the tool does that today) |
| `dry-run` | `{"type":"dry-run"}` | `dry_run`, the release's images walked through it when they are on disk, `dry_run_done` or `dry_run_failed` |
| any | `{"type":"quit"}` | Nothing. Whatever is running is abandoned (a download as if cancelled), and the tool says `bye` |

Also from the app: `hello` and `start` (§3.2). The end of the app's output (the tool's standard input closes) means `quit`.

The fields of `answers` are those of `AnswersInput` in `core/src/flow.rs` today. All seven are always sent; an empty text means what it means there (the default phone name, no Wi-Fi, an open network).

### 3.5 What answers what

**Every action ends with exactly one `screen`**: the flow's screen after the action, changed or not. Before it the tool may send any number of `progress` messages, and at most one of `phones`, `problems` or `error`. From sending an action until that screen the app is busy: it shows what is running, offers no other action (but cancelling a download), and asks before it quits.

So a refused file is `screen` (the same step, with the core's `problem`); refused answers are `problems`, then `screen`; a file that is not there is `error`, then `screen`.

After a screen whose `action` is `none` (a stop, or the end), nothing more is taken. The tool may stay until `quit` or exit with status 0 by itself; the app takes neither for a failure.

### 3.6 What never crosses

- **No write.** The protocol has no message that writes to a phone, and the mode has no flag for it. This version's flow ends at the dry run. A version that writes needs a new message, and with it a new protocol number, so an app of today can never ask a later tool for a write.
- **No command.** A `Screen` step's `command` is text for the person's own terminal. The app shows it and copies it to the clipboard on request. It has no way to run it, and no message sends it back.
- **No address.** The app never sees where Google's file is fetched from. `Screen.link` and the quote's links are pages for the person, opened in their browser on their click, and only if they are `https`.
- **No serial number**, in either direction.
- **No override.** There is no message, argument or control for the maintainer's bootloader-slot override (DESIGN §6). When a run was started with it some other way, the core's `notice` says so on every screen, and the app draws it right under the title.
- **The owner's answers** go to the tool once, over the pipe, when the form is handed in. The app keeps them only in the form's fields, which are gone with the step. It writes them nowhere, and logs nothing of what it sends.

### 3.7 For agents and scripts

`omamobile-install json` is the installer's interface for anything that is not a person at a terminal. An agent or a script starts the tool the same way and exchanges the same lines: it reads the `Screen`, decides, and sends one of the actions of §3.4. The app holds nothing an agent would need, and the rules are the same for both, because they are the core's.

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
| `none` | Close | |

**A stop and the last screen** keep the window open with the core's text and a single "Close". There is no "Start again": whether starting again helps is in the stop's closing lines as words (`Retry`), and the `Screen` carries no field for it. The app does not read sentences to decide what to offer (§9).

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

- **The tool's `json` mode.** Not built. Until it is, the app runs against `app/tests/stand-in-tool` only.
- **Where the release comes from.** The tool takes a directory today. `omainstall open DIR` passes one on; without it, the tool has to find or fetch a release itself, and that does not exist.
- **"Start again" on a stop.** The `Screen` would need to carry the stop's `Retry` (at any time, after the fix, stops again) for a front end to offer it where it helps and not where it cannot.
- **Saying that this is a reinstall.** The slot B stop tells the person to start the installer again and say so. The app takes it as `omainstall open --reinstall` and offers no control in the window, because the sentence beside that control would be about the phone. If the core named the choice (a field of the stop's screen, or an action), the app would draw it.
- **Cancelling the check of a file handed in.** A factory image is several gigabytes and hashing it takes a while; protocol 1 can cancel a download and nothing else.
- **The file chooser on a real session.** The portal code has run against no portal: the tests use a fake picker. The chooser is opened without a parent window handle, so it is not tied to the app's window.
- **A look on a real desktop.** Every picture of the window so far is an offscreen render with the software backend and the default accent.
- **Languages for the core's sentences** (§8).
- **DESIGN.md §9** still describes the desktop window as "a web view around the same page". This app is another answer to that line; DESIGN.md was left as it is and should be brought in step when this lands.

## 10. Tests

`app/bin/test` builds and runs one Qt Test binary, offscreen. It needs no Rust build, no phone, no USB and no network.

- **The stand-in** (`app/tests/stand-in-tool`, Python) speaks the protocol and replays recorded screens. It can replay one screen, walk the whole flow, offer two phones, hold a download open until it is cancelled, and fail in each way of §6.
- **The recorded screens** (`app/tests/fixtures/screens/`, 35 files) are the core's own output: `Flow::screen()` serialised by serde, for every state and every kind of stop, driven against the core's pretend phone and test bundle. `app/tests/record-screens/run` writes them again from the core as it is; run it when the flow's words change. The command-line tool cannot print a `Screen` as JSON today, which is why a small program beside the tests does it.
- **Written by hand,** from the flow's own sentences: the two `problems` the stand-in sends, and its `phones` and `error` messages. Nothing in the core produces those as JSON yet.
- **What is tested:** every recorded screen reaches QML equal to what was sent; each action and each button sends exactly its message, and an action the screen does not name is not sent; malformed JSON, a screen that lacks a field, an unknown message, an unexpected exit, a crash, a protocol mismatch, a `fatal`, a silent tool and a missing tool each give the stated error; the file picker is the injected fake; and `Main.qml` is loaded for every recorded screen, compared with it text for text, and fails on any QML warning.
