// SPDX-License-Identifier: MIT
//! Every message of protocol 1 written and read back, and the reader's
//! strictness: a key it does not know, a key that is missing, another type,
//! a string too long or of another form, a number out of range, a list too
//! long are each refused, with the field named.
mod common;
use common::*;
use omamobile_link::json::{parse, Value};
use omamobile_link::msg::*;

fn host(json: &str) -> Result<HostFrame, MsgError> {
    HostFrame::parse(json.as_bytes())
}
fn phone(json: &str) -> Result<PhoneFrame, MsgError> {
    PhoneFrame::parse(json.as_bytes())
}
fn why<T: core::fmt::Debug>(r: Result<T, MsgError>) -> (String, Why) {
    match r {
        Err(MsgError::Field { field, why, .. }) => (field, why),
        other => panic!("not a field error: {other:?}"),
    }
}

#[test]
fn every_message_is_written_and_read_back() {
    let h = host_samples();
    let mut types: Vec<&str> = h.iter().map(|(_, f)| f.msg.t()).collect();
    types.dedup();
    assert_eq!(
        types, HOST_TYPES,
        "one of every host message, in the plan's order"
    );
    for (name, f) in &h {
        let json = f.to_json().unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(&host(&json).unwrap(), f, "{name}");
        assert!(
            json.starts_with(&format!("{{\"t\":\"{}\",\"id\":{}", f.msg.t(), f.id)),
            "{json}"
        );
    }
    let p = phone_samples();
    let mut types: Vec<&str> = p.iter().map(|(_, f)| f.msg.t()).collect();
    types.dedup();
    assert_eq!(
        types, PHONE_TYPES,
        "one of every phone message, in the plan's order"
    );
    for (name, f) in &p {
        let json = f.to_json().unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(&phone(&json).unwrap(), f, "{name}");
    }
    let mut codes: Vec<&str> = error_codes().iter().map(|c| c.as_str()).collect();
    codes.sort();
    codes.dedup();
    assert_eq!(codes.len(), 19, "every error code of §5.2");
}

#[test]
fn the_writing_is_the_same_every_time_and_has_no_spaces() {
    let (_, f) = &host_samples()[0];
    assert_eq!(
        f.to_json().unwrap(),
        r#"{"t":"hello","id":1,"proto":[1],"installer":{"name":"omamobile-install","version":"0.0.1"},"max_data":1048576}"#
    );
    let ack = PhoneFrame {
        re: None,
        msg: PhoneMsg::Ack { offset: 67108864 },
    };
    assert_eq!(ack.to_json().unwrap(), r#"{"t":"ack","offset":67108864}"#);
    let e = PhoneFrame {
        re: Some(4),
        msg: PhoneMsg::Error(ErrorMsg::new(
            ErrorCode::BadCode { left: 3 },
            Stage::Waiting,
        )),
    };
    assert_eq!(
        e.to_json().unwrap(),
        r#"{"t":"error","re":4,"code":"bad-code","message":"wrong code","stage":"waiting","left":3}"#
    );
}

/// A sample's JSON with one thing changed.
fn mutate(json: &str, f: impl FnOnce(&mut Vec<(String, Value)>)) -> String {
    let Value::Obj(mut items) = parse(json.as_bytes()).unwrap() else {
        panic!()
    };
    f(&mut items);
    Value::Obj(items).to_json()
}

fn other_type(v: &Value) -> Value {
    match v {
        Value::Str(_) => Value::Num("1".into()),
        Value::Num(_) => Value::Str("1".into()),
        Value::Bool(_) => Value::Num("1".into()),
        Value::Arr(_) => Value::Obj(vec![]),
        Value::Obj(_) => Value::Arr(vec![]),
        Value::Null => Value::Bool(true),
    }
}

/// Keys a message may leave out.
const OPTIONAL: [&str; 8] = [
    "credential",
    "hostname",
    "timezone",
    "wifi_ssid",
    "wifi_psk",
    "full_name",
    "email",
    "token",
];

/// Run the same four checks over one message's JSON with either reader.
fn strict(
    name: &str,
    json: &str,
    read: &dyn Fn(&str) -> Result<(), MsgError>,
    lenient: bool,
    any_value: &[&str],
) {
    read(json).unwrap_or_else(|e| panic!("{name}: {e}"));
    // A key the message does not have
    let more = mutate(json, |o| o.push(("zz".into(), Value::Num("1".into()))));
    match read(&more) {
        Ok(()) => assert!(lenient, "{name}: an unknown key passed"),
        Err(e) => {
            assert!(!lenient, "{name}: {e}");
            assert_eq!(
                why::<()>(Err(e)),
                ("zz".to_string(), Why::Unknown),
                "{name}"
            );
        }
    }
    let Value::Obj(items) = parse(json.as_bytes()).unwrap() else {
        panic!()
    };
    for (i, (key, value)) in items.iter().enumerate() {
        // A key that is missing. `re` is not optional by itself: which
        // messages carry it is fixed, and tested on its own.
        if !OPTIONAL.contains(&key.as_str()) && key != "re" {
            let less = mutate(json, |o| {
                o.remove(i);
            });
            assert!(read(&less).is_err(), "{name}: read without {key}");
        }
        // A value of another type
        if !any_value.contains(&key.as_str()) {
            let wrong = mutate(json, |o| o[i].1 = other_type(value));
            assert!(
                read(&wrong).is_err(),
                "{name}: {key} read as {:?}",
                other_type(value)
            );
            let null = mutate(json, |o| o[i].1 = Value::Null);
            assert!(read(&null).is_err(), "{name}: {key} read as null");
        }
        // An object inside: the same for its keys
        let inner = match value {
            Value::Obj(inner) => Some(inner.clone()),
            Value::Arr(a) => a.first().and_then(|v| match v {
                Value::Obj(inner) => Some(inner.clone()),
                _ => None,
            }),
            _ => None,
        };
        let Some(inner) = inner.filter(|_| !any_value.contains(&key.as_str())) else {
            continue;
        };
        let put = |inner: Vec<(String, Value)>| {
            mutate(json, |o| {
                o[i].1 = match &o[i].1 {
                    Value::Arr(_) => Value::Arr(vec![Value::Obj(inner)]),
                    _ => Value::Obj(inner),
                }
            })
        };
        let mut more = inner.clone();
        more.push(("zz".into(), Value::Bool(true)));
        assert_eq!(
            read(&put(more)).is_ok(),
            lenient,
            "{name}: an unknown key inside {key}"
        );
        for j in 0..inner.len() {
            let mut less = inner.clone();
            let (k, v) = less.remove(j);
            assert!(read(&put(less)).is_err(), "{name}: read without {key}.{k}");
            let mut wrong = inner.clone();
            wrong[j].1 = other_type(&v);
            assert!(
                read(&put(wrong)).is_err(),
                "{name}: {key}.{k} read as another type"
            );
        }
    }
}

#[test]
fn every_message_is_read_strictly() {
    let mut n = 0;
    for (name, f) in host_samples() {
        let json = f.to_json().unwrap();
        strict(
            name,
            &json,
            &|s| host(s).map(|_| ()),
            f.msg.t() == "hello",
            &[],
        );
        n += 1;
    }
    for (name, f) in phone_samples() {
        let json = f.to_json().unwrap();
        let lenient = f.msg.t() == "hello" || name == "error-version";
        // `report` is the smoke test's own JSON: any value
        strict(
            &name,
            &json,
            &|s| phone(s).map(|_| ()),
            lenient,
            &["report"],
        );
        n += 1;
    }
    assert_eq!(n, 20 + 23 + 19);
}

#[test]
fn hello_and_the_version_error_pass_keys_from_a_later_protocol() {
    // Two sides that speak different numbers must still be able to say so
    let h = host(
        r#"{"t":"hello","id":1,"proto":[1,2],"installer":{"name":"x","version":"2","build":"b"},"max_data":4096,"features":["a"]}"#,
    );
    assert!(
        matches!(h, Ok(HostFrame { msg: HostMsg::Hello(ref x), .. }) if x.proto == [1, 2]),
        "{h:?}"
    );
    let e = phone(
        r#"{"t":"error","re":1,"code":"version","message":"m","stage":"waiting","proto":[2,3],"min":2}"#,
    );
    assert!(
        matches!(e, Ok(PhoneFrame { msg: PhoneMsg::Error(ref x), .. }) if x.code == ErrorCode::Version { proto: vec![2, 3] })
    );
    // What they do know stays strict
    assert_eq!(why(host(r#"{"t":"hello","id":1,"proto":[1],"installer":{"name":"x","version":"2"},"max_data":"4096"}"#)).1, Why::Type);
    // ... and no other message is lenient
    assert_eq!(
        why(host(r#"{"t":"ping","id":1,"features":[]}"#)),
        ("features".to_string(), Why::Unknown)
    );
    assert_eq!(
        why(phone(
            r#"{"t":"error","re":1,"code":"busy","message":"m","stage":"waiting","more":1}"#
        )),
        ("more".to_string(), Why::Unknown)
    );
}

#[test]
fn what_is_not_a_message() {
    assert!(matches!(host("not json"), Err(MsgError::Json(_))));
    assert!(
        matches!(
            host(r#"{"t":"ping","id":1,"id":2}"#),
            Err(MsgError::Json(_))
        ),
        "a key twice"
    );
    assert_eq!(host("[1]"), Err(MsgError::NotObject));
    assert_eq!(host("{}"), Err(MsgError::NoType));
    assert_eq!(host(r#"{"t":5,"id":1}"#), Err(MsgError::NoType));
    // An unknown type keeps its id, for the `unknown` reply
    assert_eq!(
        host(r#"{"t":"shell","id":9,"cmd":"id"}"#),
        Err(MsgError::UnknownType { id: Some(9) })
    );
    assert_eq!(
        host(r#"{"t":"shell"}"#),
        Err(MsgError::UnknownType { id: None })
    );
    // The phone's messages are not the host's, and the other way round
    assert_eq!(
        host(r#"{"t":"pong","id":1}"#),
        Err(MsgError::UnknownType { id: Some(1) })
    );
    assert!(matches!(
        phone(r#"{"t":"ping","re":1}"#),
        Err(MsgError::UnknownType { .. })
    ));
    assert!(
        matches!(
            host(r#"{"t":"Ping","id":1}"#),
            Err(MsgError::UnknownType { .. })
        ),
        "types are spelled one way"
    );
    // Not UTF-8
    assert!(matches!(
        HostFrame::parse(&[b'{', 0xff, b'}']),
        Err(MsgError::Json(_))
    ));
}

#[test]
fn id_and_re() {
    assert_eq!(
        why(host(r#"{"t":"ping"}"#)),
        ("id".to_string(), Why::Missing)
    );
    assert_eq!(
        why(host(r#"{"t":"ping","id":-1}"#)),
        ("id".to_string(), Why::Range)
    );
    assert_eq!(
        why(host(r#"{"t":"ping","id":1.0}"#)),
        ("id".to_string(), Why::Range)
    );
    assert_eq!(
        why(host(r#"{"t":"ping","id":"1"}"#)),
        ("id".to_string(), Why::Type)
    );
    assert_eq!(
        why(host(r#"{"t":"ping","id":9007199254740992}"#)),
        ("id".to_string(), Why::Range)
    );
    assert!(host(r#"{"t":"ping","id":9007199254740991}"#).is_ok());
    // An answer has `re`; what comes by itself has none; `error` is either
    assert_eq!(
        why(phone(r#"{"t":"pong"}"#)),
        ("re".to_string(), Why::Missing)
    );
    assert_eq!(
        why(phone(r#"{"t":"ack","re":1,"offset":0}"#)),
        ("re".to_string(), Why::Unknown)
    );
    for t in ["progress", "ack", "data-end", "provision-done"] {
        assert_eq!(PhoneMsg::re_rule(t), Re::Event);
    }
    assert!(phone(r#"{"t":"error","code":"protocol","message":"m","stage":"waiting"}"#).is_ok());
    assert!(phone(r#"{"t":"error","re":3,"code":"busy","message":"m","stage":"waiting"}"#).is_ok());
}

#[test]
fn limits_and_forms_of_the_hosts_fields() {
    let hello = |proto: &str, max: &str| {
        host(&format!(
            r#"{{"t":"hello","id":1,"proto":{proto},"installer":{{"name":"n","version":"v"}},"max_data":{max}}}"#
        ))
    };
    assert!(hello("[1]", "1024").is_ok() && hello("[1]", "4194304").is_ok());
    assert_eq!(
        why(hello("[1]", "4194305")),
        ("max_data".to_string(), Why::Range)
    );
    assert_eq!(
        why(hello("[1]", "1023")),
        ("max_data".to_string(), Why::Range)
    );
    assert_eq!(
        why(hello("[]", "4096")),
        ("proto".to_string(), Why::Missing)
    );
    assert_eq!(why(hello("[0]", "4096")), ("proto".to_string(), Why::Range));
    assert_eq!(
        why(hello("[65536]", "4096")),
        ("proto".to_string(), Why::Range)
    );
    assert_eq!(
        why(hello("[1,1]", "4096")),
        ("proto".to_string(), Why::Format)
    );
    assert_eq!(
        why(hello("[\"1\"]", "4096")),
        ("proto".to_string(), Why::Type)
    );
    let many: Vec<String> = (1..=17).map(|n| n.to_string()).collect();
    assert_eq!(
        why(hello(&format!("[{}]", many.join(",")), "4096")),
        ("proto".to_string(), Why::TooMany)
    );
    let named = |name: &str| {
        host(&format!(
            r#"{{"t":"hello","id":1,"proto":[1],"installer":{{"name":"{name}","version":"v"}},"max_data":4096}}"#
        ))
    };
    assert_eq!(
        why(named(&"n".repeat(65))),
        ("installer.name".to_string(), Why::TooLong)
    );
    assert_eq!(why(named("")), ("installer.name".to_string(), Why::Format));
    assert_eq!(
        why(named("caf\u{e9}")),
        ("installer.name".to_string(), Why::Format)
    );

    let pair = |code: &str| host(&format!(r#"{{"t":"pair","id":1,"code":"{code}"}}"#));
    assert!(pair("000000").is_ok());
    for bad in ["12345", "1234567", "12345a", "12 456", "１２３４５６"] {
        assert_eq!(why(pair(bad)), ("code".to_string(), Why::Format), "{bad}");
    }
    assert_eq!(
        why(host(r#"{"t":"pair","id":1,"code":123456}"#)),
        ("code".to_string(), Why::Type)
    );

    let resume = |token: &str| host(&format!(r#"{{"t":"resume","id":1,"token":"{token}"}}"#));
    assert!(resume(&"0f".repeat(32)).is_ok());
    assert_eq!(
        why(resume(&"0F".repeat(32))).1,
        Why::Format,
        "one digest, one spelling: lower case"
    );
    assert_eq!(why(resume(&"0f".repeat(31))).1, Why::Format);
    assert_eq!(why(resume(&"0f".repeat(33))).1, Why::TooLong);
    assert_eq!(why(resume(&"0g".repeat(32))).1, Why::Format);

    let read = |name: &str, offset: &str| {
        host(&format!(
            r#"{{"t":"backup-read","id":1,"name":"{name}","offset":{offset}}}"#
        ))
    };
    assert!(read("modem_userdata", "0").is_ok() && read(&"a".repeat(36), "1048576").is_ok());
    assert_eq!(
        why(read(&"a".repeat(37), "0")),
        ("name".to_string(), Why::TooLong)
    );
    // No message carries a path: a partition's name has no / and no dot
    for bad in [
        "",
        "../efs",
        "sda/1",
        "efs.img",
        "e fs",
        "efs\\u0000",
        "/dev/sda1",
    ] {
        assert_eq!(
            why(read(bad, "0")),
            ("name".to_string(), Why::Format),
            "{bad:?}"
        );
    }
    assert_eq!(why(read("efs", "-1")), ("offset".to_string(), Why::Range));

    let begin = |size: &str, build: &str, name: &str| {
        host(&format!(
            r#"{{"t":"factory-begin","id":1,"size":{size},"sha256":"{}","build":"{build}","name":"{name}"}}"#,
            "ab".repeat(32)
        ))
    };
    assert!(begin("1", "b", "f.zip").is_ok());
    // Over 8 GiB is read: the phone answers `too-large`, in words the installer can show
    assert!(begin("8589934593", "b", "f.zip").is_ok());
    assert_eq!(
        why(begin("0", "b", "f.zip")),
        ("size".to_string(), Why::Range)
    );
    assert_eq!(
        why(begin("9007199254740992", "b", "f.zip")),
        ("size".to_string(), Why::Range)
    );
    assert_eq!(
        why(begin("1", "", "f.zip")),
        ("build".to_string(), Why::Format)
    );
    assert_eq!(
        why(begin("1", &"b".repeat(65), "f.zip")),
        ("build".to_string(), Why::TooLong)
    );
    assert_eq!(
        why(begin("1", "b", &"f".repeat(256))),
        ("name".to_string(), Why::TooLong)
    );
    assert_eq!(
        why(begin("1", "b", "a\\nb")),
        ("name".to_string(), Why::Format)
    );
    assert!(
        begin("1", "b", "caf\u{e9} (1).zip").is_ok(),
        "shown only: any text without control characters"
    );
}

#[test]
fn limits_and_forms_of_the_phones_fields() {
    let set = |name: &str, key: &str, value: &str| {
        let (_, f) = phone_samples()
            .into_iter()
            .find(|(n, _)| n == name)
            .unwrap();
        let v = parse(value.as_bytes()).unwrap();
        mutate(&f.to_json().unwrap(), |o| {
            o.iter_mut().find(|(k, _)| k == key).unwrap().1 = v
        })
    };
    let field = |name: &str, key: &str, value: &str| why(phone(&set(name, key, value)));
    assert_eq!(field("hello", "proto", "0").1, Why::Range);
    assert_eq!(field("hello", "max_data", "4194305").1, Why::Range);
    assert_eq!(field("hello", "nonce", "\"abcd\"").1, Why::Format);
    assert_eq!(field("hello", "stage", "\"ready\"").1, Why::Format);
    assert_eq!(field("hello", "pair", "\"none\"").1, Why::Format);
    assert_eq!(field("hello", "device", "\"Cheetah\"").1, Why::Format);
    assert_eq!(field("hello", "wait_s", "4294967296").1, Why::Range);
    assert_eq!(field("status", "boot", "0").1, Why::Range);
    assert_eq!(field("status", "boot", "3").1, Why::Range);
    assert_eq!(field("status", "battery", "101").1, Why::Range);
    assert_eq!(
        field("status", "kernel", &format!("\"{}\"", "k".repeat(129))).1,
        Why::TooLong
    );
    assert_eq!(field("status", "release", "\"a\\tb\"").1, Why::Format);
    assert_eq!(field("progress", "what", "\"download\"").1, Why::Format);
    assert_eq!(
        field("progress", "step", &format!("\"{}\"", "s".repeat(65))).1,
        Why::TooLong
    );
    assert_eq!(
        field("paired", "serial_proof", &format!("\"{}\"", "A".repeat(64))).1,
        Why::Format
    );
    assert_eq!(field("paired", "token", "\"short\"").1, Why::Format);
    assert_eq!(field("error-bad-code", "left", "21").1, Why::Range);
    assert_eq!(field("error-busy", "code", "\"tired\"").1, Why::Format);
    assert_eq!(
        field("error-busy", "message", &format!("\"{}\"", "m".repeat(257))).1,
        Why::TooLong
    );
    let parts = |n: usize| {
        format!(
            "[{}]",
            vec![r#"{"name":"efs","size":1,"checked":true}"#; n].join(",")
        )
    };
    assert!(phone(&set("backup-parts", "parts", &parts(32))).is_ok());
    assert_eq!(field("backup-parts", "parts", &parts(33)).1, Why::TooMany);
    assert_eq!(
        field(
            "backup-parts",
            "parts",
            r#"[{"name":"a/b","size":1,"checked":true}]"#
        ),
        ("parts.name".to_string(), Why::Format)
    );
    assert_eq!(
        field(
            "error-problems",
            "problems",
            r#"[{"field":"shell","reason":"x"}]"#
        )
        .0,
        "problems.field"
    );
    assert_eq!(
        field(
            "error-problems",
            "problems",
            r#"[{"field":"username","reason":"Not A Word"}]"#
        )
        .0,
        "problems.reason"
    );
    // What an error carries belongs to its code
    assert_eq!(
        why(phone(
            r#"{"t":"error","re":1,"code":"busy","message":"m","stage":"waiting","left":1}"#
        ))
        .1,
        Why::Unknown
    );
    assert_eq!(
        why(phone(
            r#"{"t":"error","re":1,"code":"bad-code","message":"m","stage":"waiting"}"#
        )),
        ("left".to_string(), Why::Missing)
    );
}

#[test]
fn nothing_is_written_that_would_not_be_read() {
    let bad = [
        HostMsg::BackupRead {
            name: "../efs".into(),
            offset: 0,
        },
        HostMsg::Hello(HostHello {
            proto: vec![],
            installer_name: "n".into(),
            installer_version: "v".into(),
            max_data: 4096,
        }),
        HostMsg::Hello(HostHello {
            proto: vec![1],
            installer_name: "n".into(),
            installer_version: "v".into(),
            max_data: 5_000_000,
        }),
        HostMsg::FactoryBegin(FactoryBegin {
            size: 0,
            sha256: [0; 32],
            build: "b".into(),
            name: "n".into(),
        }),
    ];
    for msg in bad {
        assert!(
            HostFrame {
                id: 1,
                msg: msg.clone()
            }
            .to_json()
            .is_err(),
            "{msg:?}"
        );
    }
    let bad = [
        (Some(1), PhoneMsg::DataEnd { name: "efs".into() }),
        (None, PhoneMsg::Pong),
        (
            Some(1),
            PhoneMsg::Progress(Progress {
                what: What::Smoke,
                step: "é".into(),
                done: 0,
                total: 0,
            }),
        ),
        (None, PhoneMsg::Ack { offset: u64::MAX }),
    ];
    for (re, msg) in bad {
        assert!(
            PhoneFrame {
                re,
                msg: msg.clone()
            }
            .to_json()
            .is_err(),
            "{msg:?}"
        );
    }
    // A smoke report too large for one message is refused, not cut
    let report = Value::Str("x".repeat(omamobile_link::frame::MAX_MESSAGE));
    let e = PhoneFrame {
        re: Some(1),
        msg: PhoneMsg::SmokeReport { report },
    }
    .to_json();
    assert!(
        matches!(
            e,
            Err(MsgError::TooLong {
                t: "smoke-report",
                ..
            })
        ),
        "{e:?}"
    );
}

#[test]
fn what_must_not_reach_a_log_is_not_in_debug_output() {
    let code = Code::parse("314159").unwrap();
    let token = Token([0xab; 32]);
    let a = answers();
    let shown = format!(
        "{code:?} {token:?} {a:?} {:?} {:?}",
        HostMsg::Pair { code },
        HostMsg::Resume { token }
    );
    for secret in [
        "314159",
        "abab",
        "saltsalt",
        "a passphrase",
        "<SSID>",
        "owner@",
        "An Owner",
    ] {
        assert!(!shown.contains(secret), "{secret} in {shown}");
    }
    // A log line about a message: its type and its length
    let json = HostFrame {
        id: 3,
        msg: HostMsg::Pair { code },
    }
    .to_json()
    .unwrap();
    assert_eq!(
        log_type(json.as_bytes()).as_deref(),
        Some("pair (35 bytes)")
    );
    assert_eq!(
        log_type(b"{\"t\":\"<script>\"}"),
        None,
        "a peer's own text never reaches a log"
    );
}

#[test]
fn fuzz_messages_never_panic() {
    // Every sample with bytes changed, cut and spliced: 200000 inputs to
    // both readers. Nothing panics, and what is read is written the same.
    let mut rng = omamobile_link::sim::TestRng(6);
    let mut seeds: Vec<Vec<u8>> = host_samples()
        .iter()
        .map(|(_, f)| f.to_json().unwrap().into_bytes())
        .collect();
    seeds.extend(
        phone_samples()
            .iter()
            .map(|(_, f)| f.to_json().unwrap().into_bytes()),
    );
    let (mut read_host, mut read_phone) = (0u32, 0u32);
    for i in 0..200_000usize {
        let mut b = seeds[i % seeds.len()].clone();
        for _ in 0..1 + rng.next_u64() % 3 {
            let at = (rng.next_u64() as usize) % b.len();
            match rng.next_u64() % 5 {
                0 => b[at] = rng.next_u64() as u8,
                1 => b.truncate(at),
                2 => {
                    let other = &seeds[(rng.next_u64() as usize) % seeds.len()];
                    let from = (rng.next_u64() as usize) % other.len();
                    b.splice(at.., other[from..].iter().copied());
                }
                3 => b[at] = b"0123456789\"{}[],:-"[(rng.next_u64() % 18) as usize],
                _ => {
                    b.insert(at, b"\"\\{[9"[(rng.next_u64() % 5) as usize]);
                }
            }
            if b.is_empty() {
                break;
            }
        }
        if let Ok(f) = HostFrame::parse(&b) {
            read_host += 1;
            assert_eq!(
                HostFrame::parse(f.to_json().unwrap().as_bytes()).unwrap(),
                f
            );
        }
        if let Ok(f) = PhoneFrame::parse(&b) {
            read_phone += 1;
            assert_eq!(
                PhoneFrame::parse(f.to_json().unwrap().as_bytes()).unwrap(),
                f
            );
        }
        let _ = log_type(&b);
    }
    assert!(
        read_host > 1000 && read_phone > 1000,
        "{read_host} {read_phone}"
    );
}
