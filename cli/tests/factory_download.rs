// SPDX-License-Identifier: MIT
//! The command-line tool downloading Google's factory image, against the
//! pretend phone and a stand-in for Google's server in this file (a socket
//! on 127.0.0.1). Nothing here reaches Google, and nothing touches USB.
//!
//!   cargo test -p omamobile-installer-cli --features fake-phone
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

const ZIP: &str = "cheetah-test.260101.001-factory-31c2322a.zip";
const ZIP_PATH: &str = "/dl/android/aosp/cheetah-test.260101.001-factory-31c2322a.zip";
const ANSWERS: &str = "alex\n4812\n4812\n\nEurope/Paris\n\n";
const TERMS: &str = "Google's terms for the factory image";
const MISMATCH: &str = "The downloaded file is not the one this release lists";
/// How long the stand-in holds a stalled connection open, in seconds.
const STALL_HELD: u64 = 45;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/selftest")
}
fn zip_file() -> PathBuf {
    fixture().join("factory").join(ZIP)
}
fn zip() -> Vec<u8> {
    std::fs::read(zip_file()).unwrap()
}

/// What the stand-in was asked.
#[derive(Debug, Clone)]
struct Asked {
    path: String,
    range: Option<String>,
}

/// What the stand-in answers. `length`: the `Content-Length` it claims;
/// without one the body ends when the connection closes.
struct Reply {
    status: u16,
    headers: Vec<(&'static str, String)>,
    length: Option<u64>,
    body: Vec<u8>,
    /// After the body: say nothing more and keep the connection open, as a
    /// transfer that stalls.
    stall: bool,
}

impl Reply {
    fn whole(body: &[u8]) -> Self {
        Reply {
            status: 200,
            headers: vec![],
            length: Some(body.len() as u64),
            body: body.to_vec(),
            stall: false,
        }
    }
    fn status(status: u16) -> Self {
        Reply {
            status,
            headers: vec![],
            length: Some(0),
            body: vec![],
            stall: false,
        }
    }
    fn redirect(to: &str) -> Self {
        Reply {
            headers: vec![("Location", to.to_string())],
            ..Reply::status(302)
        }
    }
    /// As a server that knows `Range` answers: the rest of `file`.
    fn ranged(file: &[u8], asked: &Asked) -> Self {
        let Some(range) = &asked.range else {
            return Reply::whole(file);
        };
        let from: usize = range
            .strip_prefix("bytes=")
            .and_then(|r| r.strip_suffix('-'))
            .and_then(|n| n.parse().ok())
            .expect("the tool asks for the rest, as bytes=N-");
        Reply {
            status: 206,
            headers: vec![(
                "Content-Range",
                format!("bytes {from}-{}/{}", file.len() - 1, file.len()),
            )],
            length: Some((file.len() - from) as u64),
            body: file[from..].to_vec(),
            stall: false,
        }
    }
}

/// A stand-in for Google's server: one answer per connection, decided by
/// `answer` from the request and how many came before it.
struct StandIn {
    at: SocketAddr,
    asked: Arc<Mutex<Vec<Asked>>>,
}

impl StandIn {
    fn start(answer: impl Fn(&Asked, usize) -> Reply + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let at = listener.local_addr().unwrap();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let log = asked.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    match stream.read(&mut byte) {
                        Ok(1) => head.push(byte[0]),
                        _ => break,
                    }
                }
                let head = String::from_utf8_lossy(&head).to_string();
                let mut lines = head.lines();
                let path = lines
                    .next()
                    .and_then(|l| l.split(' ').nth(1))
                    .unwrap_or_default()
                    .to_string();
                let range = lines
                    .filter_map(|l| l.split_once(": "))
                    .find(|(name, _)| name.eq_ignore_ascii_case("range"))
                    .map(|(_, value)| value.to_string());
                let request = Asked { path, range };
                let nth = {
                    let mut log = log.lock().unwrap();
                    log.push(request.clone());
                    log.len() - 1
                };
                let reply = answer(&request, nth);
                let mut out = format!(
                    "HTTP/1.1 {} Stand-in\r\nConnection: close\r\n",
                    reply.status
                );
                if let Some(length) = reply.length {
                    out.push_str(&format!("Content-Length: {length}\r\n"));
                }
                for (name, value) in &reply.headers {
                    out.push_str(&format!("{name}: {value}\r\n"));
                }
                out.push_str("\r\n");
                let _ = stream.write_all(out.as_bytes());
                let _ = stream.write_all(&reply.body);
                let _ = stream.flush();
                if reply.stall {
                    // Held open, silent, far longer than the tool waits
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_secs(STALL_HELD));
                        drop(stream);
                    });
                }
            }
        });
        StandIn { at, asked }
    }
    fn asked(&self) -> Vec<Asked> {
        self.asked.lock().unwrap().clone()
    }
}

/// An empty work directory of this test's own.
fn work(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("factory-download-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
/// What the work directory holds of the factory image, by name and size.
fn kept(work: &Path) -> Vec<(String, u64)> {
    let mut files: Vec<(String, u64)> = match std::fs::read_dir(work.join("factory")) {
        Ok(dir) => dir
            .map(|e| e.unwrap())
            .map(|e| {
                (
                    e.file_name().to_string_lossy().to_string(),
                    e.metadata().unwrap().len(),
                )
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    files.sort();
    files
}

/// Run the tool's flow on the ready pretend phone; (exit code, output, errors).
fn flow(
    work: &Path,
    google: Option<&StandIn>,
    extra: &[&str],
    typed: &str,
) -> (i32, String, String) {
    let dir = fixture();
    let at = google.map(|g| g.at.to_string());
    let mut args = vec![
        "flow",
        dir.to_str().unwrap(),
        "--fake-phone",
        "ready",
        "--work-dir",
        work.to_str().unwrap(),
    ];
    if let Some(at) = &at {
        args.extend(["--fake-google", at]);
    }
    args.extend_from_slice(extra);
    run(&args, typed)
}

/// The variables an HTTP client may take a proxy from.
const PROXY_VARS: [&str; 6] = [
    "http_proxy",
    "HTTP_PROXY",
    "https_proxy",
    "HTTPS_PROXY",
    "all_proxy",
    "ALL_PROXY",
];

fn run(args: &[&str], typed: &str) -> (i32, String, String) {
    run_with(args, typed, &[])
}

/// As `run`, with these variables set for the tool and no other proxy
/// variable of this computer's.
fn run_with(args: &[&str], typed: &str, env: &[(&str, &str)]) -> (i32, String, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_omamobile-install"));
    for var in PROXY_VARS {
        command.env_remove(var);
    }
    command.env_remove("no_proxy").env_remove("NO_PROXY");
    command.envs(env.iter().copied());
    let mut child = command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A tool that refuses its arguments exits without reading its input,
    // and writing to it then fails: that is not this test's failure
    match child.stdin.take().unwrap().write_all(typed.as_bytes()) {
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => panic!("{e}"),
        _ => {}
    }
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

fn flat(out: &str) -> String {
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn titles(out: &str) -> Vec<&str> {
    out.lines().filter_map(|l| l.strip_prefix("==> ")).collect()
}

#[test]
fn the_terms_are_shown_and_accepted_before_the_tool_downloads_and_checks_the_file() {
    let work = work("whole");
    let file = zip();
    let body = file.clone();
    let google = StandIn::start(move |_, _| Reply::whole(&body));
    // yes to the warning, yes to Google's terms, then the answers
    let typed = format!("yes\nyes\n{ANSWERS}");
    let (code, out, err) = flow(&work, Some(&google), &[], &typed);
    assert_eq!(code, 0, "{out}\n{err}");
    assert_eq!(
        titles(&out),
        [
            "Read this once",
            "Connect the Pixel 7 Pro",
            TERMS,
            "Google's factory image",
            "About you and the phone",
            "The dry run",
            "The dry run passed. Nothing was written"
        ]
    );
    // Google's words, marked as Google's and set apart, with the pages they name
    let text = flat(&out);
    assert!(text.contains("These are Google's words, not Omarchy Mobile's:"));
    assert!(text.contains("\"Downloading of the system image and use of the device software is subject to the Google Terms of Service\""));
    assert!(text.contains(
        "\"By continuing, you agree to the Google Terms of Service and Privacy Policy\""
    ));
    assert!(out.contains("\n        \"Downloading of the system image"));
    assert!(text.contains("\"may also be subject to certain third-party terms of service, which can be found in Settings > About phone > Legal information\""));
    assert!(out.contains("    Google Terms of Service: https://policies.google.com/terms\n"));
    assert!(out.contains("    Google Privacy Policy: https://policies.google.com/privacy\n"));
    assert!(out.contains("\n    https://developers.google.com/android/images#cheetah\n"));
    assert!(text.contains(
        "for use only on your own Pixel, and Google does not allow them to be redistributed"
    ));
    assert!(text.contains("Omarchy Mobile does not host them or pass them on"));
    // Nothing was fetched before the terms were answered
    let terms_at = out.find(TERMS).unwrap();
    let asked_at = out.find("Type yes to accept").unwrap();
    let download_at = out.find("downloading ").unwrap();
    assert!(terms_at < asked_at && asked_at < download_at);
    // The acceptance, with the build and the time, on the next screen
    assert!(text.contains("You accepted Google's terms for build TEST.260101.001 on 20"));
    // One request, for the signed index's path, whole; progress as bytes of the total
    let asked = google.asked();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0].path, ZIP_PATH);
    assert_eq!(asked[0].range, None);
    assert!(
        out.contains(&format!("    {0} of {0} bytes", file.len())),
        "{out}"
    );
    // The file is in the work directory under Google's name, and nothing else
    assert_eq!(kept(&work), [(ZIP.to_string(), file.len() as u64)]);
    assert_eq!(std::fs::read(work.join("factory").join(ZIP)).unwrap(), file);
    assert!(text.contains(&format!(
        "Google's factory image for build TEST.260101.001 is the right file ({ZIP})"
    )));
    assert!(out.contains("0 others; partitions written: 0"));

    // A later run finds the file there: nothing is fetched, and it is
    // still checked before it is used
    let (code, out, err) = flow(&work, Some(&google), &[], &typed);
    assert_eq!(code, 0, "{out}\n{err}");
    assert_eq!(google.asked().len(), 1);
    assert!(titles(&out).contains(&TERMS));
    // ... so a file that was changed since is found out, and deleted
    let mut changed = file.clone();
    changed[100] ^= 1;
    std::fs::write(work.join("factory").join(ZIP), &changed).unwrap();
    let (code, out, _) = flow(&work, Some(&google), &[], &typed);
    assert_eq!(code, 2, "{out}");
    assert_eq!(*titles(&out).last().unwrap(), MISMATCH);
    assert_eq!(google.asked().len(), 1);
    assert_eq!(kept(&work), []);
}

#[test]
fn not_accepting_or_handing_in_a_file_downloads_nothing() {
    let work = work("declined");
    let file = zip();
    let google = StandIn::start(move |_, _| Reply::whole(&file));
    // Enter at Google's terms: the owner's own download, as before
    let typed = format!("yes\n\n{}\n{ANSWERS}", zip_file().display());
    let (code, out, err) = flow(&work, Some(&google), &[], &typed);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(titles(&out).contains(&TERMS));
    let text = flat(&out);
    assert!(text
        .contains("You did not accept Google's terms in the installer, so it downloads nothing."));
    assert!(
        text.contains("1. Open Google's page (the link below) and accept Google's terms there.")
    );
    assert!(out.contains("Where did you save the file?"));
    assert!(!out.contains("downloading "));
    // "y" is not yes
    let typed = format!("yes\ny\n{}\n{ANSWERS}", zip_file().display());
    let (code, _, _) = flow(&work, Some(&google), &[], &typed);
    assert_eq!(code, 0);

    // A file handed in up front: no terms step, since nothing is downloaded
    let typed = format!("yes\n{ANSWERS}");
    let (code, out, err) = flow(
        &work,
        Some(&google),
        &["--factory-zip", zip_file().to_str().unwrap()],
        &typed,
    );
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(!titles(&out).contains(&TERMS));
    assert!(flat(&out).contains("The installer downloads nothing in this run"));

    assert_eq!(google.asked().len(), 0, "{:?}", google.asked());
    assert_eq!(kept(&work), []);
}

#[test]
fn a_download_that_broke_off_goes_on_in_a_later_run_and_in_the_same_one() {
    let work = work("resumed");
    let file = zip();
    let body = file.clone();
    // The first answer breaks off partway; later ones know Range
    let google = StandIn::start(move |asked, nth| {
        if nth == 0 {
            Reply {
                body: body[..20_000].to_vec(),
                ..Reply::whole(&body)
            }
        } else {
            Reply::ranged(&body, asked)
        }
    });
    // The input ends where the tool asks whether to try again
    let (code, out, err) = flow(&work, Some(&google), &[], "yes\nyes\n");
    assert_eq!(code, 1, "{out}\n{err}");
    assert!(err.contains("no answer"));
    assert!(flat(&out).contains("! The download from Google's server stopped before it was complete: 20000 of 49152 bytes had arrived"), "{out}");
    assert!(out.contains(
        "Press Enter to try the download again, or type where you saved the file yourself: "
    ));
    assert_eq!(*titles(&out).last().unwrap(), "Google's factory image");
    // What arrived is kept, as a part
    assert_eq!(kept(&work), [(format!("{ZIP}.part"), 20_000)]);
    assert_eq!(
        std::fs::read(work.join("factory").join(format!("{ZIP}.part"))).unwrap(),
        file[..20_000]
    );

    // A later run asks for the rest only, and the whole file passes its check
    let typed = format!("yes\nyes\n{ANSWERS}");
    let (code, out, err) = flow(&work, Some(&google), &[], &typed);
    assert_eq!(code, 0, "{out}\n{err}");
    let asked = google.asked();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert_eq!(asked[1].path, ZIP_PATH);
    assert_eq!(asked[1].range.as_deref(), Some("bytes=20000-"));
    assert_eq!(kept(&work), [(ZIP.to_string(), file.len() as u64)]);
    assert_eq!(std::fs::read(work.join("factory").join(ZIP)).unwrap(), file);

    // The same within one run: Enter at the question tries again
    let work = self::work("resumed-same-run");
    let body = file.clone();
    let google = StandIn::start(move |asked, nth| {
        if nth == 0 {
            Reply {
                body: body[..30_000].to_vec(),
                ..Reply::whole(&body)
            }
        } else {
            Reply::ranged(&body, asked)
        }
    });
    let typed = format!("yes\nyes\n\n{ANSWERS}");
    let (code, out, err) = flow(&work, Some(&google), &[], &typed);
    assert_eq!(code, 0, "{out}\n{err}");
    let asked = google.asked();
    assert_eq!(asked.len(), 2);
    assert_eq!(asked[1].range.as_deref(), Some("bytes=30000-"));
    assert_eq!(std::fs::read(work.join("factory").join(ZIP)).unwrap(), file);

    // A server that sends the whole file although the rest was asked for
    let work = self::work("resumed-whole");
    std::fs::create_dir_all(work.join("factory")).unwrap();
    std::fs::write(
        work.join("factory").join(format!("{ZIP}.part")),
        &file[..7000],
    )
    .unwrap();
    let body = file.clone();
    let google = StandIn::start(move |_, _| Reply::whole(&body));
    let typed = format!("yes\nyes\n{ANSWERS}");
    let (code, out, err) = flow(&work, Some(&google), &[], &typed);
    assert_eq!(code, 0, "{out}\n{err}");
    assert_eq!(google.asked()[0].range.as_deref(), Some("bytes=7000-"));
    assert_eq!(std::fs::read(work.join("factory").join(ZIP)).unwrap(), file);

    // At the question, the owner can hand in their own file instead
    let work = self::work("resumed-own-file");
    let body = file.clone();
    let google = StandIn::start(move |_, _| Reply {
        body: body[..100].to_vec(),
        ..Reply::whole(&body)
    });
    let typed = format!("yes\nyes\n{}\n{ANSWERS}", zip_file().display());
    let (code, out, err) = flow(&work, Some(&google), &[], &typed);
    assert_eq!(code, 0, "{out}\n{err}");
    assert_eq!(google.asked().len(), 1);
}

#[test]
fn a_download_that_stalls_ends_the_try_keeps_the_part_and_can_be_tried_again() {
    let file = zip();
    let half = file.len() / 2;
    // The whole length said, half of it sent, and then silence with the
    // connection held open. Later answers know Range
    let stalling = |file: Vec<u8>| {
        StandIn::start(move |asked, nth| {
            if nth == 0 {
                Reply {
                    body: file[..half].to_vec(),
                    stall: true,
                    ..Reply::whole(&file)
                }
            } else {
                Reply::ranged(&file, asked)
            }
        })
    };
    let said = format!("! The download from Google's server stopped before it was complete: {half} of {} bytes had arrived (Google's server sent nothing for 2 seconds). You can try again.", file.len());

    // The input ends where the tool asks whether to try again: the tool is
    // back long before the server lets go of the connection
    let work = work("stalled");
    let google = stalling(file.clone());
    let started = std::time::Instant::now();
    let (code, out, err) = flow(&work, Some(&google), &[], "yes\nyes\n");
    let took = started.elapsed().as_secs();
    assert!(
        took < STALL_HELD / 2,
        "the tool waited {took} s on a silent server"
    );
    assert_eq!(code, 1, "{out}\n{err}");
    assert!(flat(&out).contains(&said), "{out}");
    assert!(out.contains(
        "Press Enter to try the download again, or type where you saved the file yourself: "
    ));
    assert_eq!(*titles(&out).last().unwrap(), "Google's factory image");
    // What arrived is kept, as a part, and it is a true start of the file
    assert_eq!(kept(&work), [(format!("{ZIP}.part"), half as u64)]);
    assert_eq!(
        std::fs::read(work.join("factory").join(format!("{ZIP}.part"))).unwrap(),
        file[..half]
    );

    // In one run: Enter at the question goes on from the part
    let work = self::work("stalled-same-run");
    let google = stalling(file.clone());
    let typed = format!("yes\nyes\n\n{ANSWERS}");
    let started = std::time::Instant::now();
    let (code, out, err) = flow(&work, Some(&google), &[], &typed);
    assert!(started.elapsed().as_secs() < STALL_HELD / 2);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(flat(&out).contains(&said), "{out}");
    let asked = google.asked();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert_eq!(asked[1].range, Some(format!("bytes={half}-")));
    assert_eq!(kept(&work), [(ZIP.to_string(), file.len() as u64)]);
    assert_eq!(std::fs::read(work.join("factory").join(ZIP)).unwrap(), file);
}

#[test]
fn a_resumed_file_is_hashed_from_its_first_byte() {
    // A part left by an earlier run that is not the file's beginning: the
    // rest that arrives is right, the whole is not
    let work = work("spoiled-part");
    let file = zip();
    std::fs::create_dir_all(work.join("factory")).unwrap();
    let mut part = file[..20_000].to_vec();
    part[3] ^= 1;
    std::fs::write(work.join("factory").join(format!("{ZIP}.part")), &part).unwrap();
    let body = file.clone();
    let google = StandIn::start(move |asked, _| Reply::ranged(&body, asked));
    let typed = format!("yes\nyes\n{ANSWERS}");
    let (code, out, err) = flow(&work, Some(&google), &[], &typed);
    assert_eq!(code, 2, "{out}\n{err}");
    assert_eq!(google.asked()[0].range.as_deref(), Some("bytes=20000-"));
    assert_eq!(*titles(&out).last().unwrap(), MISMATCH);
    // The part is gone, so the next run starts the file afresh
    assert_eq!(kept(&work), []);
    let (code, _, _) = flow(&work, Some(&google), &[], &typed);
    assert_eq!(code, 0);
    assert_eq!(google.asked()[1].range, None);
    assert_eq!(std::fs::read(work.join("factory").join(ZIP)).unwrap(), file);
}

#[test]
fn a_file_that_is_not_the_listed_one_stops_the_tool_and_is_deleted() {
    let file = zip();
    let mut wrong = file.clone();
    wrong[file.len() / 2] ^= 1;
    let mut longer = file.clone();
    longer.extend_from_slice(&[0u8; 4096]);
    let deleted = "The installer uses nothing of that file, and what it had downloaded of it is deleted from this computer.";
    let nothing_taken = "The installer took nothing of what the server offered. Starting the installer again tries the download again.";
    let cases: [(&str, Reply, &str, &str); 4] = [
        (
            "another-sha256",
            Reply::whole(&wrong),
            "the file is not what the release's signed list of Google's builds says: its SHA-256 is another than the one the list records for build TEST.260101.001.",
            deleted,
        ),
        (
            // No length said, and more bytes than the file has
            "too-many-bytes",
            Reply {
                length: None,
                ..Reply::whole(&longer)
            },
            "Google's server sent more for cheetah-test.260101.001-factory-31c2322a.zip than the file has, so what it sent is not what the release's signed list of Google's builds says",
            deleted,
        ),
        (
            "another-size-said",
            Reply::whole(&longer),
            "Google's server offered a file of 53248 bytes as cheetah-test.260101.001-factory-31c2322a.zip, and that is not what the release's signed list of Google's builds says: the list records 49152 bytes",
            nothing_taken,
        ),
        (
            "a-smaller-size-said",
            Reply::whole(&file[..1000]),
            "Google's server offered a file of 1000 bytes",
            nothing_taken,
        ),
    ];
    for (name, reply, says, then) in cases {
        let work = work(name);
        let reply = Mutex::new(Some(reply));
        let google = StandIn::start(move |_, _| reply.lock().unwrap().take().unwrap());
        let typed = format!("yes\nyes\n{ANSWERS}");
        let (code, out, err) = flow(&work, Some(&google), &[], &typed);
        assert_eq!(code, 2, "{name}: {out}\n{err}");
        assert_eq!(*titles(&out).last().unwrap(), MISMATCH, "{name}");
        let text = flat(&out);
        assert!(text.contains(says), "{name}: {out}");
        assert!(text.contains(then), "{name}: {out}");
        assert!(text.contains("It has written nothing to it."));
        assert!(text.contains("You can start the installer again at any time."));
        // Nothing of it is left, and no later step was shown
        assert_eq!(kept(&work), [], "{name}");
        assert!(!titles(&out).contains(&"About you and the phone"));
        assert_eq!(google.asked().len(), 1);
        assert!(out.contains("0 others; partitions written: 0"));
    }
}

#[test]
fn another_size_said_by_the_server_leaves_a_good_part_on_disk() {
    let file = zip();
    let mut longer = file.clone();
    longer.push(0);
    // A true start of the file, left by an earlier run
    let with_part = |name: &str| {
        let work = work(name);
        std::fs::create_dir_all(work.join("factory")).unwrap();
        std::fs::write(
            work.join("factory").join(format!("{ZIP}.part")),
            &file[..20_000],
        )
        .unwrap();
        work
    };
    let cases: [(&str, Reply, u64); 3] = [
        // The whole file again, one byte too long
        ("part-kept-200-longer", Reply::whole(&longer), 49153),
        // A short page with status 200 in the file's place
        ("part-kept-200-page", Reply::whole(&[b'x'; 35]), 35),
        // The rest, of a file one byte too long
        (
            "part-kept-206-total",
            Reply {
                status: 206,
                headers: vec![("Content-Range", "bytes 20000-49152/49153".into())],
                ..Reply::whole(&longer[20_000..])
            },
            49153,
        ),
    ];
    for (name, reply, said) in cases {
        let work = with_part(name);
        let reply = Mutex::new(Some(reply));
        let good = file.clone();
        // The first answer names another size; later ones are a good server's
        let google = StandIn::start(move |asked, _| match reply.lock().unwrap().take() {
            Some(reply) => reply,
            None => Reply::ranged(&good, asked),
        });
        let typed = format!("yes\nyes\n{ANSWERS}");
        let (code, out, err) = flow(&work, Some(&google), &[], &typed);
        // It is not the listed file: the install stops, as before
        assert_eq!(code, 2, "{name}: {out}\n{err}");
        assert_eq!(*titles(&out).last().unwrap(), MISMATCH, "{name}");
        let text = flat(&out);
        assert!(
            text.contains(&format!("Google's server offered a file of {said} bytes")),
            "{name}: {out}"
        );
        assert!(text.contains("The installer took nothing of what the server offered. The 20000 bytes an earlier try downloaded stay on this computer: the server's answer says nothing about them, and they are checked from the first byte, together with the rest, before anything of the file is used."), "{name}: {out}");
        assert!(
            text.contains("Starting the installer again goes on from those bytes."),
            "{name}: {out}"
        );
        assert!(!text.contains("deleted"), "{name}: {out}");
        assert_eq!(google.asked()[0].range.as_deref(), Some("bytes=20000-"));
        // The part is there, byte for byte
        assert_eq!(kept(&work), [(format!("{ZIP}.part"), 20_000)], "{name}");
        assert_eq!(
            std::fs::read(work.join("factory").join(format!("{ZIP}.part"))).unwrap(),
            file[..20_000],
            "{name}"
        );

        // The next run goes on from it, and the whole file passes its check
        let (code, out, err) = flow(&work, Some(&google), &[], &typed);
        assert_eq!(code, 0, "{name}: {out}\n{err}");
        let asked = google.asked();
        assert_eq!(asked.len(), 2, "{name}: {asked:?}");
        assert_eq!(asked[1].range.as_deref(), Some("bytes=20000-"));
        assert_eq!(kept(&work), [(ZIP.to_string(), file.len() as u64)]);
        assert_eq!(std::fs::read(work.join("factory").join(ZIP)).unwrap(), file);
    }
}

/// Something that listens where a proxy would, counts who connects and
/// keeps the first line each one sends. It answers nothing.
struct Listening {
    at: SocketAddr,
    heard: Arc<Mutex<Vec<String>>>,
}

impl Listening {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let at = listener.local_addr().unwrap();
        let heard = Arc::new(Mutex::new(Vec::new()));
        let log = heard.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                let (mut line, mut byte) = (Vec::new(), [0u8; 1]);
                while !line.ends_with(b"\r\n") && line.len() < 512 {
                    match stream.read(&mut byte) {
                        Ok(1) => line.push(byte[0]),
                        _ => break,
                    }
                }
                log.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&line).trim().to_string());
            }
        });
        Listening { at, heard }
    }
    fn heard(&self) -> Vec<String> {
        self.heard.lock().unwrap().clone()
    }
}

#[test]
fn a_proxy_named_in_the_environment_is_not_used_and_one_named_with_proxy_is() {
    let file = zip();
    let dir = fixture();
    let dir = dir.to_str().unwrap();
    let typed = format!("yes\nyes\n{ANSWERS}");
    let proxy = Listening::start();
    let through = format!("http://{}", proxy.at);

    // Each variable alone, then all of them at once: the request goes
    // straight to the stand-in, and nothing connects to the proxy
    let mut sets: Vec<Vec<(&str, &str)>> =
        PROXY_VARS.iter().map(|v| vec![(*v, &*through)]).collect();
    sets.push(PROXY_VARS.iter().map(|v| (*v, &*through)).collect());
    for (n, env) in sets.iter().enumerate() {
        let work = work(&format!("proxy-in-environment-{n}"));
        let body = file.clone();
        let google = StandIn::start(move |_, _| Reply::whole(&body));
        let at = google.at.to_string();
        let args = [
            "flow",
            dir,
            "--fake-phone",
            "ready",
            "--work-dir",
            work.to_str().unwrap(),
            "--fake-google",
            &at,
        ];
        let (code, out, err) = run_with(&args, &typed, env);
        assert_eq!(code, 0, "{env:?}: {out}\n{err}");
        assert_eq!(google.asked().len(), 1, "{env:?}");
        assert_eq!(proxy.heard(), Vec::<String>::new(), "{env:?}");
        assert_eq!(std::fs::read(work.join("factory").join(ZIP)).unwrap(), file);
        assert!(!flat(&out).contains("through the proxy"), "{out}");
    }

    // Named on the command line, by the owner: used, and said
    let work = work("proxy-named");
    let body = file.clone();
    let google = StandIn::start(move |_, _| Reply::whole(&body));
    let at = google.at.to_string();
    let args = [
        "flow",
        dir,
        "--fake-phone",
        "ready",
        "--work-dir",
        work.to_str().unwrap(),
        "--fake-google",
        &at,
        "--proxy",
        &through,
    ];
    // The input ends where the tool asks whether to try again
    let (code, out, err) = run_with(&args, "yes\nyes\n", &[]);
    assert_eq!(code, 1, "{out}\n{err}");
    let heard = proxy.heard();
    assert_eq!(heard.len(), 1, "{heard:?}");
    // The proxy is asked for the stand-in, which is itself asked nothing
    assert!(heard[0].contains(&at), "{heard:?}");
    assert_eq!(google.asked().len(), 0);
    assert!(
        flat(&out).contains("through the proxy named with --proxy"),
        "{out}"
    );
    // Its address is not printed back: it may hold a password
    assert!(!out.contains(&through) && !err.contains(&through));
    assert_eq!(kept(&work), []);

    // Only an HTTP proxy, with a host; refused from the arguments alone
    for bad in [
        "",
        "proxy.example",
        "socks5://127.0.0.1:1080",
        "ftp://127.0.0.1:21",
        "http://",
    ] {
        let (code, out, err) = run_with(
            &["flow", dir, "--fake-phone", "ready", "--proxy", bad],
            "yes\nyes\n",
            &[],
        );
        assert_eq!(code, 1, "{bad}");
        assert!(
            err.contains("--proxy takes an address like http://HOST:PORT"),
            "{bad}: {err}"
        );
        assert!(out.is_empty(), "{bad}: {out}");
    }
}

#[test]
fn a_redirect_is_followed_on_googles_host_only() {
    // On the same host: followed, and the file is taken
    let work = work("redirect-same-host");
    let file = zip();
    let body = file.clone();
    let google = StandIn::start(move |asked, nth| match nth {
        0 => Reply::redirect("https://dl.google.com/edge/one/factory.zip"),
        1 => Reply::redirect("/edge/two/factory.zip"),
        _ => Reply::ranged(&body, asked),
    });
    let typed = format!("yes\nyes\n{ANSWERS}");
    let (code, out, err) = flow(&work, Some(&google), &[], &typed);
    assert_eq!(code, 0, "{out}\n{err}");
    let paths: Vec<String> = google.asked().into_iter().map(|a| a.path).collect();
    assert_eq!(
        paths,
        [ZIP_PATH, "/edge/one/factory.zip", "/edge/two/factory.zip"]
    );
    assert_eq!(std::fs::read(work.join("factory").join(ZIP)).unwrap(), file);

    // Anywhere else: not followed. The tool asks for nothing more, says
    // why, and takes the file the owner saves themselves
    for (name, to) in [
        (
            "redirect-other-host",
            "https://example.org/dl/android/aosp/factory.zip".to_string(),
        ),
        (
            "redirect-plain-http",
            format!("http://dl.google.com{ZIP_PATH}"),
        ),
        // The stand-in's own address is not Google's either
        (
            "redirect-stand-in",
            "http://127.0.0.1/factory.zip".to_string(),
        ),
    ] {
        let work = self::work(name);
        let google = StandIn::start(move |_, _| Reply::redirect(&to));
        let typed = format!("yes\nyes\n{}\n{ANSWERS}", zip_file().display());
        let (code, out, err) = flow(&work, Some(&google), &[], &typed);
        assert_eq!(code, 0, "{name}: {out}\n{err}");
        assert_eq!(google.asked().len(), 1, "{name}");
        let text = flat(&out);
        assert!(text.contains("Google's server sent the installer on to another address, and the installer fetches Google's files from https://dl.google.com and nowhere else, so it did not follow."), "{name}: {out}");
        assert!(
            text.contains("You can still go on: download the image from Google's page yourself")
        );
        assert!(!out.contains("example.org"));
        assert!(out.contains("Where did you save the file?"));
        assert_eq!(kept(&work), [], "{name}");
    }

    // Not the file and not a redirect
    let work = self::work("not-found");
    let google = StandIn::start(|_, _| Reply::status(404));
    let (code, out, _) = flow(&work, Some(&google), &[], "yes\nyes\n");
    assert_eq!(code, 1);
    assert!(
        flat(&out).contains("Google's server answered with HTTP status 404 instead of the file.")
    );
    assert!(out.contains("Where did you save the file?"));
}

#[test]
fn the_pretend_run_never_reaches_google_and_the_stand_in_is_for_it_alone() {
    // Without a stand-in, a pretend run fetches from nowhere
    let work = work("nowhere");
    let (code, out, err) = flow(&work, None, &[], "yes\nyes\n");
    assert_eq!(code, 1, "{out}\n{err}");
    assert!(flat(&out).contains("a pretend run fetches nothing from Google, and no stand-in was named with --fake-google"), "{out}");
    assert_eq!(*titles(&out).last().unwrap(), "Google's factory image");

    let dir = fixture();
    let dir = dir.to_str().unwrap();
    // A stand-in is an address on this computer
    for at in ["192.0.2.1:80", "dl.google.com:443", "example.org", ""] {
        let (code, out, err) = run(
            &["flow", dir, "--fake-phone", "ready", "--fake-google", at],
            "yes\nyes\n",
        );
        assert_eq!(code, 1, "{at}");
        assert!(err.contains("--fake-google takes an address"), "{err}");
        assert!(out.is_empty(), "{out}");
    }
    // And never for a real phone: refused from the arguments alone, before
    // any USB device is listed or opened
    let (code, out, err) = run(
        &[
            "install",
            dir,
            "--serial",
            "<SERIAL>",
            "--dry-run",
            "--fake-google",
            "127.0.0.1:9",
        ],
        "",
    );
    assert_eq!(code, 1);
    assert!(err.contains("for the pretend phone only"), "{err}");
    assert!(out.is_empty(), "{out}");
}
