// SPDX-License-Identifier: MIT
//! The sparse splitter and the zstd push decoder, away from any phone.
mod support;
use omamobile_installer_core::hash::sha256_hex;
use omamobile_installer_core::install::UserdataStream;
use omamobile_installer_core::sparse::{SparseSplitter, BLOCK, MIN_SEGMENT};
use omamobile_installer_core::zstd_push::ZstdPush;
use omamobile_installer_core::Error;
use support::bundle::{noise, root_image, zstd_store};
use support::fake::unsparse;

/// 3 MiB compressed by the zstd tool (`zstd -19 --check`): compressed
/// blocks, a single-segment header and a content checksum.
const ZSTD_FIXTURE: &[u8] = include_bytes!("../../fixtures/zstd/pattern.img.zst");
const ZSTD_FIXTURE_SHA256: &str =
    "13a0315c55637cf3966d471b995931b0820a675f649d92ff961fa4b69625c2e2";

fn split(image: &[u8], limit: u64, piece: usize) -> Vec<Vec<u8>> {
    let mut s = SparseSplitter::new(image.len() as u64, limit).unwrap();
    let mut out = Vec::new();
    for chunk in image.chunks(piece) {
        s.push(chunk, &mut out).unwrap();
    }
    s.finish(&mut out).unwrap();
    out
}

#[test]
fn segments_fit_the_limit_and_rebuild_the_image() {
    let image = root_image();
    for (limit, piece) in [
        (MIN_SEGMENT, 4096),
        (10_000, 1),
        (64 << 10, 4095),
        (100_000, 4097),
        (256 << 10, 1 << 20),
        (1 << 30, 12345),
    ] {
        let segments = split(&image, limit, piece);
        // The partition starts as 0xa5 everywhere: every byte must be written
        let mut target = vec![0xa5u8; image.len()];
        for seg in &segments {
            assert!(
                seg.len() as u64 <= limit,
                "a {}-byte segment over the limit {limit}",
                seg.len()
            );
            unsparse(seg, &mut target).unwrap();
        }
        assert!(
            target == image,
            "limit {limit}, pieces of {piece}: the image differs"
        );
        if limit == 1 << 30 {
            assert_eq!(segments.len(), 1);
            // 2 MiB with 164 zero blocks and 4 fill blocks: smaller than raw
            assert!(segments[0].len() < image.len() - 150 * BLOCK);
        } else {
            assert!(segments.len() > 1);
        }
    }
}

#[test]
fn a_short_or_long_image_is_refused() {
    let image = noise(7, 8 * BLOCK);
    let mut out = Vec::new();
    let mut s = SparseSplitter::new(image.len() as u64, 1 << 20).unwrap();
    s.push(&image[..image.len() - 1], &mut out).unwrap();
    assert!(matches!(s.finish(&mut out), Err(Error::Sparse(_))));
    let mut s = SparseSplitter::new(image.len() as u64, 1 << 20).unwrap();
    s.push(&image, &mut out).unwrap();
    assert!(matches!(
        s.push(&image[..BLOCK], &mut out),
        Err(Error::Sparse(_))
    ));
    assert!(SparseSplitter::new(4097, 1 << 20).is_err());
    assert!(SparseSplitter::new(4096, 100).is_err());
}

fn decode(stream: &[u8], piece: usize) -> Result<Vec<u8>, Error> {
    let mut z = ZstdPush::new();
    let mut out = Vec::new();
    for chunk in stream.chunks(piece) {
        z.push(chunk, &mut |d| {
            out.extend_from_slice(d);
            Ok(())
        })?;
    }
    z.finish()?;
    Ok(out)
}

#[test]
fn zstd_made_by_the_zstd_tool_decodes_in_any_piece_size() {
    for piece in [1, 3, 17, 4096, 100_000, usize::MAX] {
        let out = decode(ZSTD_FIXTURE, piece).unwrap();
        assert_eq!(out.len(), 3 << 20);
        assert_eq!(sha256_hex(&out), ZSTD_FIXTURE_SHA256, "pieces of {piece}");
    }
}

#[test]
fn damaged_zstd_is_refused() {
    // Truncated
    assert!(matches!(
        decode(&ZSTD_FIXTURE[..ZSTD_FIXTURE.len() - 1], 4096),
        Err(Error::Zstd(_))
    ));
    assert!(matches!(
        decode(&ZSTD_FIXTURE[..1000], 4096),
        Err(Error::Zstd(_))
    ));
    // The checksum, then a byte in the middle
    let mut bad = ZSTD_FIXTURE.to_vec();
    *bad.last_mut().unwrap() ^= 1;
    assert!(matches!(decode(&bad, 4096), Err(Error::Zstd(_))));
    let mut bad = ZSTD_FIXTURE.to_vec();
    bad[ZSTD_FIXTURE.len() / 2] ^= 0x40;
    assert!(matches!(decode(&bad, 4096), Err(Error::Zstd(_))));
    // Trailing bytes, and something that isn't zstd
    let mut bad = ZSTD_FIXTURE.to_vec();
    bad.extend_from_slice(b"more");
    assert!(matches!(decode(&bad, 4096), Err(Error::Zstd(_))));
    assert!(matches!(
        decode(b"not zstd at all", 4096),
        Err(Error::Zstd(_))
    ));
}

#[test]
fn compressed_bytes_in_sparse_images_out() {
    let image = root_image();
    let stream = zstd_store(&image, true);
    for piece in [7, 5000, 1 << 20] {
        let mut u = UserdataStream::new(128 << 10);
        let mut out = Vec::new();
        for chunk in stream.chunks(piece) {
            u.feed(chunk).unwrap();
            while let Some(seg) = u.next_segment().unwrap() {
                out.push(seg);
            }
        }
        assert_eq!(u.image_len(), Some(image.len() as u64));
        assert_eq!(u.end().unwrap(), image.len() as u64);
        while let Some(seg) = u.next_segment().unwrap() {
            out.push(seg);
        }
        assert!(u.end().is_err());
        let mut target = vec![0xa5u8; image.len()];
        for seg in &out {
            assert!(seg.len() <= 128 << 10);
            unsparse(seg, &mut target).unwrap();
        }
        assert!(target == image);
    }
    // The zstd-made fixture too (3 MiB is 768 blocks)
    let mut u = UserdataStream::new(64 << 10);
    let mut out = Vec::new();
    u.feed(ZSTD_FIXTURE).unwrap();
    while let Some(seg) = u.next_segment().unwrap() {
        out.push(seg);
    }
    u.end().unwrap();
    out.extend(u.next_segment().unwrap());
    assert!(u.next_segment().unwrap().is_none());
    let mut target = vec![0xa5u8; 3 << 20];
    out.iter()
        .for_each(|seg| unsparse(seg, &mut target).unwrap());
    assert_eq!(sha256_hex(&target), ZSTD_FIXTURE_SHA256);
}

#[test]
fn a_stream_that_does_not_state_its_size_is_refused() {
    let stream = zstd_store(&root_image(), false);
    // It is valid zstd
    assert_eq!(decode(&stream, 4096).unwrap().len(), 2 << 20);
    let mut u = UserdataStream::new(128 << 10);
    assert!(matches!(u.feed(&stream), Err(Error::Zstd(_))));

    // A stream cut short is refused at the end, not taken as complete
    let stream = zstd_store(&root_image(), true);
    let mut u = UserdataStream::new(128 << 10);
    u.feed(&stream[..stream.len() / 2]).unwrap();
    while u.next_segment().unwrap().is_some() {}
    assert!(matches!(u.end(), Err(Error::Zstd(_))));
}
