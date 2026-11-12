// SPDX-License-Identifier: MIT
//! The golden frames: one file per frame under `link/golden/`, the bytes of
//! one bulk transfer each, with a description beside it. They are the
//! contract the phone's daemon in omarchy-mobile is checked against (the
//! same files are kept there, INSTALL-LINK-PLAN §12 Q8), and this test
//! fails when the encoding changes by a single byte.
//!
//! A deliberate change of the wire is a new protocol number. To write the
//! files anew: `OMIL_GOLDEN_WRITE=1 cargo test -p omamobile-link --test golden`.
mod common;
use common::*;
use omamobile_link::digest::{sha256, to_hex};
use omamobile_link::frame::{
    self, Kind, DEFAULT_MAX_DATA, HEADER_LEN, MAX_DATA_LIMIT, MAX_MESSAGE,
};
use omamobile_link::json::Value;
use omamobile_link::msg::*;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Frames longer than this are kept as their SHA-256 and the rule that
/// makes them, not as bytes.
const KEEP_BYTES_UP_TO: usize = 300_000;

struct Golden {
    name: String,
    direction: &'static str,
    about: String,
    payload_text: String,
    bytes: Vec<u8>,
}

/// The payload of the large data frames: byte i is (i * 31 + 7) mod 251.
fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| ((i * 31 + 7) % 251) as u8).collect()
}

fn message(name: String, direction: &'static str, seq: u32, json: String, about: &str) -> Golden {
    let bytes = frame::encode(Kind::Message, seq, 0, json.as_bytes(), DEFAULT_MAX_DATA).unwrap();
    let payload_text = if json.len() > 2000 {
        format!("{} ... ({} bytes)", &json[..200], json.len())
    } else {
        json
    };
    Golden {
        name,
        direction,
        about: about.to_string(),
        payload_text,
        bytes,
    }
}

fn data(
    name: &str,
    direction: &'static str,
    seq: u32,
    offset: u64,
    payload: &[u8],
    max: u32,
    about: &str,
) -> Golden {
    let bytes = frame::encode(Kind::Data, seq, offset, payload, max).unwrap();
    let payload_text = if payload.len() <= 16 {
        format!("{} bytes, hex {}", payload.len(), to_hex(payload))
    } else {
        format!("{} bytes: byte i is (i * 31 + 7) mod 251", payload.len())
    };
    Golden {
        name: name.to_string(),
        direction,
        about: about.to_string(),
        payload_text,
        bytes,
    }
}

fn all() -> Vec<Golden> {
    let mut v = Vec::new();
    // Every message the host sends, numbered as one session would
    for (i, (name, f)) in host_samples().into_iter().enumerate() {
        let about = format!("{}, id {}", f.msg.t(), f.id);
        v.push(message(
            format!("host-{:02}-{name}", i + 1),
            "host to phone",
            i as u32,
            f.to_json().unwrap(),
            &about,
        ));
    }
    // Every message the phone sends, every error code among them
    for (i, (name, f)) in phone_samples().into_iter().enumerate() {
        let about = match f.re {
            Some(re) => format!("{}, answering id {re}", f.msg.t()),
            None => format!("{}, sent by itself (no re)", f.msg.t()),
        };
        v.push(message(
            format!("phone-{:02}-{name}", i + 1),
            "phone to host",
            i as u32,
            f.to_json().unwrap(),
            &about,
        ));
    }
    // The pad byte: 56 + len a multiple of 64
    let padded = (0..64)
        .map(|n| HostFrame {
            id: 5,
            msg: HostMsg::FactoryBegin(FactoryBegin {
                size: 1,
                sha256: digest(0x30),
                build: "b".into(),
                name: "n".repeat(n + 1),
            }),
        })
        .find(|f| frame::needs_pad(f.to_json().unwrap().len()))
        .unwrap();
    v.push(message(
        "pad-message".into(),
        "host to phone",
        4,
        padded.to_json().unwrap(),
        "a message with the pad byte: flag bit 0 set, one zero byte after the payload",
    ));
    v.push(data(
        "pad-data-8",
        "phone to host",
        9,
        0,
        &[1, 2, 3, 4, 5, 6, 7, 8],
        DEFAULT_MAX_DATA,
        "the shortest data frame with the pad byte: 56 + 8 = 64",
    ));
    v.push(data(
        "data-3",
        "host to phone",
        2,
        0,
        &[0xaa, 0xbb, 0xcc],
        DEFAULT_MAX_DATA,
        "a short data frame at the stream's start",
    ));
    v.push(data("data-offset-64-bit", "host to phone", 0xffff_ffff, 8 * (1u64 << 30) - 5, &[9, 8, 7, 6, 5], DEFAULT_MAX_DATA, "the last five bytes of an 8 GiB stream, with the largest seq: every header field little-endian"));
    v.push(data(
        "data-1024",
        "phone to host",
        7,
        2048,
        &pattern(1024),
        DEFAULT_MAX_DATA,
        "one frame of the smallest max_data",
    ));
    // The size limits
    let fill = |len: usize| {
        let empty = PhoneFrame {
            re: Some(19),
            msg: PhoneMsg::SmokeReport {
                report: Value::Str(String::new()),
            },
        };
        let over = empty.to_json().unwrap().len();
        let f = PhoneFrame {
            re: Some(19),
            msg: PhoneMsg::SmokeReport {
                report: Value::Str("x".repeat(len - over)),
            },
        };
        f.to_json().unwrap()
    };
    v.push(message(
        "limit-message-262144".into(),
        "phone to host",
        30,
        fill(MAX_MESSAGE),
        "the longest message: a smoke-report whose report is a string of x that fills 262144 bytes",
    ));
    v.push(message(
        "limit-message-262088-padded".into(),
        "phone to host",
        31,
        fill(MAX_MESSAGE - 56),
        "the longest message that takes the pad byte: 56 + 262088 = 262144",
    ));
    v.push(data(
        "limit-data-1048576",
        "host to phone",
        100,
        67108864,
        &pattern(1 << 20),
        DEFAULT_MAX_DATA,
        "a full data frame at the default max_data",
    ));
    v.push(data(
        "limit-data-1048520-padded",
        "host to phone",
        101,
        0,
        &pattern((1 << 20) - 56),
        DEFAULT_MAX_DATA,
        "a data frame that fills 1048576 bytes with its header, so it takes the pad byte",
    ));
    v.push(data(
        "limit-data-4194304",
        "host to phone",
        102,
        0,
        &pattern(4 << 20),
        MAX_DATA_LIMIT,
        "a full data frame at the largest max_data",
    ));
    v
}

fn describe(g: &Golden) -> String {
    let f = frame::decode(&g.bytes, MAX_DATA_LIMIT).unwrap();
    let pad = g.bytes.len() - HEADER_LEN - f.payload.len();
    format!(
        "frame: {}\n{}\ndirection: {}\nkind: {}\nflags: {}\nseq: {}\nlen: {}\noffset: {}\npayload sha256: {}\npad byte: {}\nframe bytes: {}\nframe sha256: {}\npayload: {}\n",
        g.name,
        g.about,
        g.direction,
        if f.kind == Kind::Message { "1 (message)" } else { "2 (data)" },
        pad,
        f.seq,
        f.payload.len(),
        f.offset,
        to_hex(&sha256(f.payload)),
        if pad == 1 { "yes" } else { "no" },
        g.bytes.len(),
        to_hex(&sha256(&g.bytes)),
        g.payload_text,
    )
}

/// The files the golden frames are: name to content.
fn files() -> BTreeMap<String, Vec<u8>> {
    let mut m = BTreeMap::new();
    for g in all() {
        assert!(
            m.insert(format!("{}.txt", g.name), describe(&g).into_bytes())
                .is_none(),
            "{} twice",
            g.name
        );
        if g.bytes.len() <= KEEP_BYTES_UP_TO {
            m.insert(format!("{}.bin", g.name), g.bytes);
        }
    }
    m
}

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("golden")
}

#[test]
fn the_encoding_is_the_golden_frames() {
    let want = files();
    if std::env::var_os("OMIL_GOLDEN_WRITE").is_some() {
        std::fs::create_dir_all(dir()).unwrap();
        for entry in std::fs::read_dir(dir()).unwrap() {
            std::fs::remove_file(entry.unwrap().path()).unwrap();
        }
        for (name, content) in &want {
            std::fs::write(dir().join(name), content).unwrap();
        }
        eprintln!("wrote {} files to {}", want.len(), dir().display());
        return;
    }
    let mut have = BTreeMap::new();
    for entry in std::fs::read_dir(dir()).expect("link/golden") {
        let path = entry.unwrap().path();
        have.insert(
            path.file_name().unwrap().to_string_lossy().to_string(),
            std::fs::read(&path).unwrap(),
        );
    }
    let names = |m: &BTreeMap<String, Vec<u8>>| m.keys().cloned().collect::<Vec<_>>();
    assert_eq!(names(&have), names(&want), "the set of golden files");
    for (name, content) in &want {
        assert!(
            have[name] == *content,
            "{name} is not what this crate encodes now. The wire changed: that is a new protocol number, not a new golden file."
        );
    }
}

#[test]
fn the_golden_files_read_back_as_what_they_say() {
    // The other way round, from the files alone: what the daemon's own
    // test does with its copy.
    let hosts: BTreeMap<String, HostFrame> = host_samples()
        .into_iter()
        .map(|(n, f)| (n.to_string(), f))
        .collect();
    let phones: BTreeMap<String, PhoneFrame> = phone_samples().into_iter().collect();
    let (mut messages, mut datas, mut padded) = (0, 0, 0);
    for entry in std::fs::read_dir(dir()).expect("link/golden") {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "txt") {
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(text.is_ascii() || text.contains("payload: {"), "{path:?}");
            continue;
        }
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let bytes = std::fs::read(&path).unwrap();
        let f = frame::decode(&bytes, MAX_DATA_LIMIT).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_ne!(bytes.len() % 64, 0, "{name}: ends in a short packet");
        padded += (bytes.len() - HEADER_LEN - f.payload.len()) as u32;
        if f.kind == Kind::Data {
            datas += 1;
            continue;
        }
        messages += 1;
        if let Some(sample) = name.strip_prefix("host-").and_then(|n| hosts.get(&n[3..])) {
            assert_eq!(&HostFrame::parse(f.payload).unwrap(), sample, "{name}");
        } else if let Some(sample) = name
            .strip_prefix("phone-")
            .and_then(|n| phones.get(&n[3..]))
        {
            assert_eq!(&PhoneFrame::parse(f.payload).unwrap(), sample, "{name}");
        } else {
            assert!(
                HostFrame::parse(f.payload).is_ok() || PhoneFrame::parse(f.payload).is_ok(),
                "{name}"
            );
        }
    }
    assert_eq!(
        messages,
        20 + 42 + 3,
        "every message of the host's and the phone's, the padded one, the two at the limit"
    );
    assert_eq!(datas, 4);
    assert!(
        padded >= 3,
        "pad-message, pad-data-8 and the padded message at the limit, at least"
    );
}

#[test]
fn every_kind_of_frame_and_message_has_a_golden_file() {
    let all = all();
    let named = |prefix: &str| all.iter().filter(|g| g.name.starts_with(prefix)).count();
    assert_eq!(named("host-"), 20);
    assert_eq!(named("phone-"), 23 + 19);
    for t in HOST_TYPES {
        assert!(
            all.iter()
                .any(|g| g.direction == "host to phone" && g.about.starts_with(&format!("{t},"))),
            "{t}"
        );
    }
    for t in PHONE_TYPES {
        assert!(
            all.iter()
                .any(|g| g.direction == "phone to host" && g.about.starts_with(&format!("{t},"))),
            "{t}"
        );
    }
    for code in error_codes() {
        assert!(all
            .iter()
            .any(|g| g.name.ends_with(&format!("error-{}", code.as_str()))));
    }
    // The large ones are kept by digest: check the rule gives what the description says
    let big: Vec<&Golden> = all
        .iter()
        .filter(|g| g.bytes.len() > KEEP_BYTES_UP_TO)
        .collect();
    assert_eq!(
        big.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(),
        [
            "limit-data-1048576",
            "limit-data-1048520-padded",
            "limit-data-4194304"
        ]
    );
    assert_eq!(big[1].bytes.len(), 1048576 + 1);
    assert_eq!(big[2].bytes.len(), 56 + 4194304);
}
