// SPDX-License-Identifier: MIT
//! Framing, byte for byte as INSTALL-LINK-PLAN §5.1 lays it out, and a
//! decoder that takes any bytes: every wrong frame is an error that names
//! what is wrong, and no input panics.
use omamobile_link::digest::sha256;
use omamobile_link::frame::*;
use omamobile_link::sim::TestRng;

const MAX: u32 = DEFAULT_MAX_DATA;

#[test]
fn the_header_is_the_documents_layout() {
    let payload = br#"{"t":"ping","id":7}"#;
    let f = encode(Kind::Message, 0x0403_0201, 0, payload, MAX).unwrap();
    assert_eq!(&f[0..4], &[0x4f, 0x4d, 0x49, 0x4c], "magic OMIL");
    assert_eq!(&f[0..4], b"OMIL");
    assert_eq!(f[4], 1, "frame version");
    assert_eq!(f[5], 1, "kind: message");
    assert_eq!(&f[6..8], &[0, 0], "flags");
    assert_eq!(&f[8..12], &[1, 2, 3, 4], "seq, little-endian");
    assert_eq!(
        &f[12..16],
        &[payload.len() as u8, 0, 0, 0],
        "len, little-endian"
    );
    assert_eq!(&f[16..24], &[0; 8], "offset: 0 for a message");
    assert_eq!(&f[24..56], &sha256(payload), "SHA-256 of the payload");
    assert_eq!(&f[56..], payload);
    assert_eq!(f.len(), HEADER_LEN + payload.len());

    let d = encode(Kind::Data, 9, 0x0102_0304_0506_0708, &[0xaa; 3], MAX).unwrap();
    assert_eq!(d[5], 2, "kind: data");
    assert_eq!(
        &d[16..24],
        &[8, 7, 6, 5, 4, 3, 2, 1],
        "offset, little-endian"
    );
    assert_eq!(
        decode(&d, MAX).unwrap(),
        Frame {
            kind: Kind::Data,
            seq: 9,
            offset: 0x0102_0304_0506_0708,
            payload: &[0xaa; 3]
        }
    );
}

#[test]
fn an_empty_message_payload_hashes_zero_bytes() {
    let f = encode(Kind::Message, 0, 0, b"", MAX).unwrap();
    assert_eq!(&f[24..56], &sha256(b""));
    assert_eq!(decode(&f, MAX).unwrap().payload, b"");
}

#[test]
fn the_pad_byte_makes_every_transfer_end_in_a_short_packet() {
    // 56 + len a multiple of 64: flag bit 0, one zero byte, neither counted
    // in len nor hashed. So no frame is a whole number of packets at 64,
    // 512 or 1024 bytes.
    for len in 0..=4096usize {
        let payload = vec![0x5a; len];
        let f = encode(Kind::Message, 1, 0, &payload, MAX).unwrap();
        let padded = (HEADER_LEN + len).is_multiple_of(64);
        assert_eq!(needs_pad(len), padded);
        assert_eq!(f.len(), HEADER_LEN + len + padded as usize, "len {len}");
        assert_eq!(u16::from_le_bytes([f[6], f[7]]), padded as u16, "len {len}");
        assert_eq!(
            u32::from_le_bytes([f[12], f[13], f[14], f[15]]) as usize,
            len
        );
        assert_eq!(&f[24..56], &sha256(&payload));
        if padded {
            assert_eq!(f[f.len() - 1], 0);
        }
        for packet in [64, 512, 1024] {
            assert_ne!(
                f.len() % packet,
                0,
                "len {len}: a whole number of {packet}-byte packets"
            );
        }
        assert_eq!(decode(&f, MAX).unwrap().payload, &payload[..]);
    }
    assert!(needs_pad(8) && needs_pad(1048576 - 56) && !needs_pad(1048576));
    // At the limits too
    for len in [MAX_MESSAGE, 262144 - 56] {
        let f = encode(Kind::Message, 1, 0, &vec![b' '; len], MAX).unwrap();
        assert_ne!(f.len() % 64, 0);
    }
}

#[test]
fn the_read_buffer_holds_the_largest_frame() {
    for max in [MIN_MAX_DATA, 4096, DEFAULT_MAX_DATA, MAX_DATA_LIMIT] {
        let n = read_buffer_len(max);
        assert_eq!(n % 1024, 0);
        assert!(n > HEADER_LEN + max as usize);
        assert!(
            n > HEADER_LEN + MAX_MESSAGE,
            "a message can be longer than a small max_data"
        );
    }
    assert_eq!(read_buffer_len(DEFAULT_MAX_DATA), 1048576 + 1024);
}

#[test]
fn the_limits() {
    let big = vec![0u8; MAX_DATA_LIMIT as usize + 1];
    assert!(encode(Kind::Message, 0, 0, &big[..MAX_MESSAGE], MAX).is_ok());
    assert_eq!(
        encode(Kind::Message, 0, 0, &big[..MAX_MESSAGE + 1], MAX),
        Err(FrameError::TooLong {
            kind: Kind::Message,
            len: 262145,
            max: 262144
        })
    );
    assert!(encode(Kind::Data, 0, 0, &big[..MAX as usize], MAX).is_ok());
    assert!(matches!(
        encode(Kind::Data, 0, 0, &big[..MAX as usize + 1], MAX),
        Err(FrameError::TooLong { .. })
    ));
    // max_data is the session's: a frame the limit allows elsewhere is refused here
    let f = encode(Kind::Data, 0, 0, &big[..4096], MAX).unwrap();
    assert_eq!(
        decode(&f, 2048),
        Err(FrameError::TooLong {
            kind: Kind::Data,
            len: 4096,
            max: 2048
        })
    );
    // ... and nobody's max_data is over 4194304
    assert!(encode(Kind::Data, 0, 0, &big[..MAX_DATA_LIMIT as usize], u32::MAX).is_ok());
    assert!(matches!(
        encode(Kind::Data, 0, 0, &big, u32::MAX),
        Err(FrameError::TooLong { max: 4194304, .. })
    ));
    // What the encoder will not write
    assert_eq!(
        encode(Kind::Message, 0, 1, b"{}", MAX),
        Err(FrameError::MessageOffset(1))
    );
    assert_eq!(
        encode(Kind::Data, 0, 5, b"", MAX),
        Err(FrameError::DataRange { offset: 5, len: 0 })
    );
    assert_eq!(
        encode(Kind::Data, 0, u64::MAX, b"x", MAX),
        Err(FrameError::DataRange {
            offset: u64::MAX,
            len: 1
        })
    );
    assert!(encode(Kind::Data, 0, u64::MAX - 1, b"x", MAX).is_ok());
}

/// A valid frame with its header rewritten, digest and length left alone.
fn with(f: &[u8], at: usize, bytes: &[u8]) -> Vec<u8> {
    let mut v = f.to_vec();
    v[at..at + bytes.len()].copy_from_slice(bytes);
    v
}

#[test]
fn every_wrong_frame_is_named() {
    let payload = br#"{"t":"ping","id":1}"#;
    let m = encode(Kind::Message, 3, 0, payload, MAX).unwrap();
    let d = encode(Kind::Data, 3, 100, payload, MAX).unwrap();
    let padded = encode(Kind::Message, 3, 0, &[b' '; 8], MAX).unwrap();
    assert_eq!(padded.len(), 65);

    assert_eq!(decode(&[], MAX), Err(FrameError::Short { got: 0 }));
    assert_eq!(decode(&m[..55], MAX), Err(FrameError::Short { got: 55 }));
    assert_eq!(decode(&with(&m, 0, b"OMIX"), MAX), Err(FrameError::Magic));
    assert_eq!(decode(&with(&m, 4, &[2]), MAX), Err(FrameError::Version(2)));
    assert_eq!(decode(&with(&m, 4, &[0]), MAX), Err(FrameError::Version(0)));
    assert_eq!(decode(&with(&m, 5, &[0]), MAX), Err(FrameError::Kind(0)));
    assert_eq!(decode(&with(&m, 5, &[3]), MAX), Err(FrameError::Kind(3)));
    assert_eq!(
        decode(&with(&m, 6, &[2, 0]), MAX),
        Err(FrameError::Flags(2))
    );
    assert_eq!(
        decode(&with(&m, 6, &[0, 0x80]), MAX),
        Err(FrameError::Flags(0x8000))
    );
    assert_eq!(
        decode(&with(&m, 12, &262145u32.to_le_bytes()), MAX),
        Err(FrameError::TooLong {
            kind: Kind::Message,
            len: 262145,
            max: 262144
        })
    );
    assert_eq!(
        decode(&with(&d, 12, &(MAX + 1).to_le_bytes()), MAX),
        Err(FrameError::TooLong {
            kind: Kind::Data,
            len: MAX + 1,
            max: MAX
        })
    );
    // len is not the bytes that arrived
    assert_eq!(
        decode(&m[..m.len() - 1], MAX),
        Err(FrameError::Length {
            expected: 75,
            got: 74
        })
    );
    let mut long = m.clone();
    long.push(0);
    assert_eq!(
        decode(&long, MAX),
        Err(FrameError::Length {
            expected: 75,
            got: 76
        })
    );
    assert_eq!(
        decode(&with(&m, 12, &[18, 0, 0, 0]), MAX),
        Err(FrameError::Length {
            expected: 74,
            got: 75
        })
    );
    // The pad rule, both ways
    assert_eq!(
        decode(&with(&m, 6, &[1, 0]), MAX),
        Err(FrameError::PadFlag { len: 19, set: true })
    );
    assert_eq!(
        decode(&with(&padded, 6, &[0, 0]), MAX),
        Err(FrameError::PadFlag { len: 8, set: false })
    );
    assert_eq!(
        decode(&padded[..64], MAX),
        Err(FrameError::Length {
            expected: 65,
            got: 64
        })
    );
    assert_eq!(
        decode(&with(&padded, 64, &[1]), MAX),
        Err(FrameError::PadByte(1))
    );
    // Offsets
    assert_eq!(
        decode(&with(&m, 16, &[1]), MAX),
        Err(FrameError::MessageOffset(1))
    );
    assert_eq!(
        decode(&with(&d, 16, &u64::MAX.to_le_bytes()), MAX),
        Err(FrameError::DataRange {
            offset: u64::MAX,
            len: 19
        })
    );
    let mut empty = encode(Kind::Message, 0, 0, b"", MAX).unwrap();
    empty[5] = 2;
    assert_eq!(
        decode(&empty, MAX),
        Err(FrameError::DataRange { offset: 0, len: 0 })
    );
    // The digest: a changed payload, a changed digest
    assert_eq!(decode(&with(&m, 60, b"X"), MAX), Err(FrameError::Digest));
    assert_eq!(
        decode(&with(&m, 24, &[m[24] ^ 1]), MAX),
        Err(FrameError::Digest)
    );
    // Each error says what is wrong, in words
    for e in [
        FrameError::Short { got: 3 },
        FrameError::Magic,
        FrameError::Seq {
            expected: 4,
            got: 9,
        },
        FrameError::NoStream,
        FrameError::StreamOffset {
            expected: 0,
            got: 1024,
        },
        FrameError::StreamOverrun { size: 10, end: 11 },
        FrameError::PadFlag { len: 8, set: false },
    ] {
        assert!(e.to_string().len() > 10, "{e:?}");
    }
}

/// The sequence number and a data frame's offset are the session's to check;
/// this is the check the state machines make, for the fuzzing below.
fn accept(bytes: &[u8], seq: u32, offset: u64) -> bool {
    matches!(decode(bytes, MAX), Ok(f) if f.seq == seq && f.offset == offset)
}

#[test]
fn fuzz_truncations_at_every_length() {
    let mut rng = TestRng(1);
    for len in [0usize, 1, 7, 8, 9, 100, 1000] {
        for kind in [Kind::Message, Kind::Data] {
            if kind == Kind::Data && len == 0 {
                continue;
            }
            let offset = if kind == Kind::Data { 4096 } else { 0 };
            let f = encode(kind, 5, offset, &rng.bytes(len), MAX).unwrap();
            assert!(accept(&f, 5, offset));
            for cut in 0..f.len() {
                assert!(
                    decode(&f[..cut], MAX).is_err(),
                    "{kind:?} len {len} cut to {cut}"
                );
            }
            // ... and with anything appended
            let mut more = f.clone();
            more.push(0);
            assert!(decode(&more, MAX).is_err());
        }
    }
}

#[test]
fn fuzz_every_single_bit_flip_of_a_header() {
    // Every one of the 448 header bits, for a message, a padded message and
    // a data frame: no flipped frame is taken for the frame that was sent.
    let mut rng = TestRng(2);
    let frames = [
        encode(Kind::Message, 5, 0, br#"{"t":"ping","id":1}"#, MAX).unwrap(),
        encode(Kind::Message, 5, 0, &[b' '; 72], MAX).unwrap(),
        encode(Kind::Data, 5, 8192, &rng.bytes(300), MAX).unwrap(),
        encode(Kind::Data, 5, 8192, &rng.bytes(8), MAX).unwrap(),
    ];
    let mut refused = [0u32; 2];
    for f in &frames {
        let offset = if f[5] == 2 { 8192 } else { 0 };
        assert!(accept(f, 5, offset));
        for bit in 0..HEADER_LEN * 8 {
            let mut g = f.clone();
            g[bit / 8] ^= 1 << (bit % 8);
            let by_decoder = decode(&g, MAX).is_err();
            refused[by_decoder as usize] += 1;
            assert!(!accept(&g, 5, offset), "bit {bit} of {:?}", &f[..16]);
            // The decoder alone lets through only a changed seq, or a
            // changed offset of a data frame: the session's two checks
            if !by_decoder {
                let in_seq = (64..96).contains(&bit);
                let in_offset = (128..192).contains(&bit) && f[5] == 2;
                assert!(in_seq || in_offset, "bit {bit}");
            }
        }
        // Every bit of the payload and the pad too
        for bit in HEADER_LEN * 8..f.len() * 8 {
            let mut g = f.clone();
            g[bit / 8] ^= 1 << (bit % 8);
            assert!(decode(&g, MAX).is_err(), "payload bit {bit}");
        }
    }
    assert_eq!(
        refused[0],
        2 * 32 + 2 * (32 + 64),
        "seq bits, and the data frames' offset bits"
    );
}

#[test]
fn fuzz_random_buffers() {
    // Noise, noise behind a real header, and real frames with noise written
    // over a stretch: 200000 inputs, none panics, and none that differs
    // from a sent frame is taken for it.
    let mut rng = TestRng(3);
    let good = encode(Kind::Data, 1, 0, &rng.bytes(500), MAX).unwrap();
    let mut errors = std::collections::BTreeMap::<String, u32>::new();
    for i in 0..200_000u32 {
        let len = (rng.next_u64() % 700) as usize;
        let mut buf = rng.bytes(len);
        match i % 4 {
            1 if len >= 8 => {
                buf[..6].copy_from_slice(&[0x4f, 0x4d, 0x49, 0x4c, 1, 1 + (i % 8 == 1) as u8])
            }
            2 => {
                // a valid header for whatever follows it
                let mut f = encode(Kind::Message, 0, 0, &buf, MAX).unwrap();
                let at = (rng.next_u64() as usize) % f.len();
                f[at] = f[at].wrapping_add(1 + (rng.next_u64() % 255) as u8);
                buf = f;
            }
            3 => {
                buf = good.clone();
                let at = (rng.next_u64() as usize) % buf.len();
                let n = ((rng.next_u64() % 16) as usize).min(buf.len() - at);
                let noise = rng.bytes(n);
                buf[at..at + n].copy_from_slice(&noise);
            }
            _ => {}
        }
        match decode(&buf, MAX) {
            Ok(f) => {
                // Only a frame whose bytes all hold together decodes
                assert_eq!(
                    encode(f.kind, f.seq, f.offset, f.payload, MAX).unwrap(),
                    buf
                );
                assert!(i % 4 >= 2, "noise decoded as a frame");
            }
            Err(e) => {
                let name = format!("{e:?}");
                *errors
                    .entry(name.split([' ', '(', '{']).next().unwrap().to_string())
                    .or_default() += 1;
            }
        }
    }
    // The noise reached the decoder's every stage
    for kind in [
        "Short", "Magic", "Version", "Kind", "Flags", "TooLong", "Length", "PadFlag", "Digest",
    ] {
        assert!(errors.contains_key(kind), "no {kind} among {errors:?}");
    }
}

#[test]
fn fuzz_headers_built_field_by_field() {
    // Every combination of a few values per header field, the digest right
    // or wrong: decode agrees with the rules written out here.
    let payloads: [&[u8]; 4] = [b"", b"12345678", b"{}", &[7u8; 200]];
    let mut n = 0;
    for payload in payloads {
        for version in [0u8, 1, 2] {
            for kind in [0u8, 1, 2, 3] {
                for flags in [0u16, 1, 2, 3] {
                    for len_delta in [-1i64, 0, 1] {
                        for offset in [0u64, 1, u64::MAX] {
                            for pad in [None, Some(0u8), Some(9)] {
                                for good_digest in [true, false] {
                                    let len = (payload.len() as i64 + len_delta).max(0) as u32;
                                    let mut f = Vec::new();
                                    f.extend_from_slice(b"OMIL");
                                    f.push(version);
                                    f.push(kind);
                                    f.extend_from_slice(&flags.to_le_bytes());
                                    f.extend_from_slice(&77u32.to_le_bytes());
                                    f.extend_from_slice(&len.to_le_bytes());
                                    f.extend_from_slice(&offset.to_le_bytes());
                                    let mut digest = sha256(payload);
                                    digest[31] ^= !good_digest as u8;
                                    f.extend_from_slice(&digest);
                                    f.extend_from_slice(payload);
                                    f.extend(pad);
                                    let want_pad = (56 + payload.len()) % 64 == 0;
                                    let ok = version == 1
                                        && (kind == 1 || kind == 2)
                                        && flags == want_pad as u16
                                        && len as usize == payload.len()
                                        && pad == want_pad.then_some(0)
                                        && (kind != 1 || offset == 0)
                                        && (kind != 2
                                            || (!payload.is_empty() && offset != u64::MAX))
                                        && good_digest;
                                    assert_eq!(decode(&f, MAX).is_ok(), ok, "{f:?}");
                                    n += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(n, 4 * 3 * 4 * 4 * 3 * 3 * 3 * 2);
}
