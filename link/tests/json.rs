// SPDX-License-Identifier: MIT
//! The crate's own JSON reader: strict where the protocol needs it, in step
//! with a general reader everywhere else, and safe on any bytes.
use omamobile_link::json::{parse, Value, MAX_DEPTH, MAX_NUMBER};
use omamobile_link::sim::TestRng;

/// This crate's value as the general reader's, to compare.
fn as_serde(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => (*b).into(),
        Value::Num(n) => serde_json::from_str(n).expect("a number this crate took"),
        Value::Str(s) => s.as_str().into(),
        Value::Arr(a) => a.iter().map(as_serde).collect::<Vec<_>>().into(),
        Value::Obj(o) => o
            .iter()
            .map(|(k, v)| (k.clone(), as_serde(v)))
            .collect::<serde_json::Map<_, _>>()
            .into(),
    }
}

#[test]
fn what_json_allows_is_read() {
    for text in [
        "null",
        " true ",
        "false",
        "0",
        "-0",
        "12.5e-3",
        "1E+9",
        r#""""#,
        r#""a\"b\\c\/d\b\f\n\r\t\u00e9\ud83d\ude00 é 😀""#,
        "[]",
        "[1,[2,[3]],{}]",
        r#"{"a":1,"b":{"c":[true,null]},"":"empty key"}"#,
        "\t\r\n {\"t\" : \"ping\" , \"id\" : 1 } \n",
    ] {
        let v = parse(text.as_bytes()).unwrap_or_else(|e| panic!("{text}: {e}"));
        let want: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(as_serde(&v), want, "{text}");
        // Written and read again, it is the same value
        assert_eq!(parse(v.to_json().as_bytes()).unwrap(), v, "{text}");
    }
}

#[test]
fn what_json_does_not_allow_is_refused_with_its_place() {
    for (text, at) in [
        ("", 0),
        ("nul", 0),
        ("tru", 0),
        ("01", 1),
        ("1.", 2),
        (".5", 0),
        ("+1", 0),
        ("1e", 2),
        ("-", 1),
        ("0x10", 1),
        ("NaN", 0),
        ("'a'", 0),
        ("\"a", 2),
        ("\"\\x\"", 2),
        ("\"\\u12\"", 3),
        ("\"\\ud800\"", 7),
        ("\"\\ud800\\u0041\"", 13),
        ("\"\\udc00\"", 7),
        ("\"a\nb\"", 2),
        ("[1,]", 3),
        ("[1 2]", 3),
        ("{\"a\"}", 4),
        ("{\"a\":1,}", 7),
        ("{a:1}", 1),
        ("{} {}", 3),
        ("[1]]", 3),
    ] {
        let e = parse(text.as_bytes()).expect_err(text);
        assert_eq!(e.at, at, "{text}: {e}");
    }
    assert_eq!(parse(&[b'"', 0xff, b'"']).unwrap_err().what, "not UTF-8");
    assert_eq!(
        parse(&[0xef, 0xbb, 0xbf, b'1']).unwrap_err().at,
        0,
        "a byte order mark is not JSON"
    );
}

#[test]
fn a_key_given_twice_is_refused() {
    // A general reader keeps the last one; two readers that disagree about
    // which is how a checked value and a used value come apart.
    let text = br#"{"t":"pair","id":1,"code":"000000","code":"123456"}"#;
    assert!(serde_json::from_slice::<serde_json::Value>(text).is_ok());
    let e = parse(text).unwrap_err();
    assert_eq!((e.what, e.at), ("a key given twice", 35));
    assert!(parse(br#"{"a":{"b":1,"b":2}}"#).is_err());
    assert!(
        parse(br#"{"a":1,"\u0061":2}"#).is_err(),
        "the same key, spelled with an escape"
    );
    assert!(
        parse(br#"[{"a":1},{"a":2}]"#).is_ok(),
        "two objects may share a key"
    );
}

#[test]
fn nesting_has_a_limit() {
    let deep = |n: usize| format!("{}{}", "[".repeat(n), "]".repeat(n));
    assert!(parse(deep(MAX_DEPTH).as_bytes()).is_ok());
    assert_eq!(
        parse(deep(MAX_DEPTH + 1).as_bytes()).unwrap_err().what,
        "nested too deep"
    );
    // A quarter of a megabyte of brackets does not overflow the stack
    assert!(parse("[".repeat(262144).as_bytes()).is_err());
    assert!(parse("{\"a\":".repeat(50000).as_bytes()).is_err());
}

#[test]
fn the_protocols_numbers_are_whole_and_at_most_2_to_the_53_less_one() {
    let n = |s: &str| parse(s.as_bytes()).unwrap().as_u64();
    assert_eq!(n("0"), Some(0));
    assert_eq!(n("8589934592"), Some(8 << 30));
    assert_eq!(n("9007199254740991"), Some(MAX_NUMBER));
    for no in [
        "9007199254740992",
        "18446744073709551616",
        "-1",
        "-0",
        "1.0",
        "1e3",
        "\"1\"",
        "true",
    ] {
        assert_eq!(n(no), None, "{no}");
    }
    // Other numbers pass through as they were written (the smoke report)
    let v = parse(b"[-1.50e+10,1E400]").unwrap();
    assert_eq!(v.to_json(), "[-1.50e+10,1E400]");
}

#[test]
fn strings_are_written_one_way() {
    let v = Value::Str("a\"b\\c\n\u{1}\u{7f}é😀/".into());
    assert_eq!(v.to_json(), "\"a\\\"b\\\\c\\u000a\\u0001\u{7f}é😀/\"");
    assert_eq!(parse(v.to_json().as_bytes()).unwrap(), v);
    let o = Value::Obj(vec![
        ("b".into(), Value::Num("1".into())),
        ("a".into(), Value::Null),
    ]);
    assert_eq!(
        o.to_json(),
        r#"{"b":1,"a":null}"#,
        "keys stay in the order they were given"
    );
}

/// A random JSON text: mostly valid, often broken somewhere.
fn random_text(rng: &mut TestRng, depth: u32) -> String {
    match rng.next_u64() % if depth > 4 { 5 } else { 8 } {
        0 => "null".into(),
        1 => ["true", "false"][(rng.next_u64() % 2) as usize].into(),
        2 => ["0", "7", "-3", "1.5", "2e9", "9007199254740991", "01", "1."]
            [(rng.next_u64() % 8) as usize]
            .into(),
        3 | 4 => {
            let parts = [
                "a",
                "é",
                "\\n",
                "\\u0041",
                "\\ud83d\\ude00",
                "\\\"",
                " ",
                "\\ud800",
                "\u{1}",
                "😀",
            ];
            let n = rng.next_u64() % 4;
            let s: String = (0..n)
                .map(|_| parts[(rng.next_u64() % parts.len() as u64) as usize])
                .collect();
            format!("\"{s}\"")
        }
        5 | 6 => {
            let n = rng.next_u64() % 4;
            let items: Vec<String> = (0..n).map(|_| random_text(rng, depth + 1)).collect();
            format!(
                "[{}]",
                items.join([",", " , ", ",,"][(rng.next_u64() % 9 / 4) as usize])
            )
        }
        _ => {
            let n = rng.next_u64() % 4;
            let items: Vec<String> = (0..n)
                .map(|_| {
                    format!(
                        "\"k{}\":{}",
                        rng.next_u64() % 5,
                        random_text(rng, depth + 1)
                    )
                })
                .collect();
            format!("{{{}}}", items.join(","))
        }
    }
}

#[test]
fn fuzz_against_a_general_reader() {
    // Whatever this crate takes, the general reader takes, as the same
    // value; what it refuses and the general one takes is a key given twice
    // and nothing else.
    let mut rng = TestRng(4);
    let (mut taken, mut refused, mut twice) = (0, 0, 0);
    for _ in 0..50_000 {
        let text = random_text(&mut rng, 0);
        let theirs = serde_json::from_str::<serde_json::Value>(&text);
        match parse(text.as_bytes()) {
            Ok(v) => {
                taken += 1;
                assert_eq!(
                    as_serde(&v),
                    theirs.unwrap_or_else(|e| panic!("{text}: {e}")),
                    "{text}"
                );
                assert_eq!(parse(v.to_json().as_bytes()).unwrap(), v, "{text}");
            }
            Err(e) => {
                refused += 1;
                if theirs.is_ok() {
                    twice += 1;
                    assert_eq!(e.what, "a key given twice", "{text}");
                }
            }
        }
    }
    assert!(
        taken > 10_000 && refused > 5_000 && twice > 500,
        "{taken} {refused} {twice}"
    );
}

#[test]
fn fuzz_any_bytes() {
    // Noise, and valid text with a byte changed, cut short or repeated:
    // 300000 inputs, none panics.
    let mut rng = TestRng(5);
    let seed = br#"{"t":"status","re":3,"stage":"waiting","list":[1,2.5,"x\u00e9\ud83d\ude00"],"o":{"a":null,"b":true}}"#;
    for i in 0..300_000u32 {
        let buf = match i % 3 {
            0 => {
                let len = (rng.next_u64() % 64) as usize;
                rng.bytes(len)
            }
            1 => {
                let mut b = seed.to_vec();
                let at = (rng.next_u64() as usize) % b.len();
                b[at] = rng.next_u64() as u8;
                b
            }
            _ => {
                let mut b = seed.to_vec();
                let at = (rng.next_u64() as usize) % b.len();
                if i % 2 == 0 {
                    b.truncate(at);
                } else {
                    let tail = b[at..].to_vec();
                    b.extend_from_slice(&tail);
                }
                b
            }
        };
        if let Ok(v) = parse(&buf) {
            assert_eq!(parse(v.to_json().as_bytes()).unwrap(), v);
        }
    }
}
