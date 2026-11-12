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
}

impl Reply {
    fn whole(body: &[u8]) -> Self {
        Reply {
            status: 200,
            headers: vec![],
            length: Some(body.len() as u64),
            body: body.to_vec(),
        }
    }
    fn status(status: u16) -> Self {
        Reply {
            status,
            headers: vec![],
            length: Some(0),
            body: vec![],
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

fn run(args: &[&str], typed: &str) -> (i32, String, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_omamobile-install"));
    // No proxy of this computer's between the tool and the stand-in
    for var in ["http_proxy", "https_proxy", "all_proxy"] {
        command.env_remove(var).env_remove(var.to_uppercase());
    }
    let mut child = command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(typed.as_bytes())
        .unwrap();
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
    let cases: [(&str, Reply, &str); 4] = [
        (
            "another-sha256",
            Reply::whole(&wrong),
            "the file is not what the release's signed list of Google's builds says: its SHA-256 is another than the one the list records for build TEST.260101.001.",
        ),
        (
            // No length said, and more bytes than the file has
            "too-many-bytes",
            Reply {
                length: None,
                ..Reply::whole(&longer)
            },
            "Google's server sent more for cheetah-test.260101.001-factory-31c2322a.zip than the file has, so what it sent is not what the release's signed list of Google's builds says",
        ),
        (
            "another-size-said",
            Reply::whole(&longer),
            "Google's server offered a file of 53248 bytes as cheetah-test.260101.001-factory-31c2322a.zip, and that is not what the release's signed list of Google's builds says: the list records 49152 bytes",
        ),
        (
            "a-smaller-size-said",
            Reply::whole(&file[..1000]),
            "Google's server offered a file of 1000 bytes",
        ),
    ];
    for (name, reply, says) in cases {
        let work = work(name);
        let reply = Mutex::new(Some(reply));
        let google = StandIn::start(move |_, _| reply.lock().unwrap().take().unwrap());
        let typed = format!("yes\nyes\n{ANSWERS}");
        let (code, out, err) = flow(&work, Some(&google), &[], &typed);
        assert_eq!(code, 2, "{name}: {out}\n{err}");
        assert_eq!(*titles(&out).last().unwrap(), MISMATCH, "{name}");
        let text = flat(&out);
        assert!(text.contains(says), "{name}: {out}");
        assert!(text.contains("The installer uses nothing of that file, and what it had downloaded of it is deleted from this computer."));
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
