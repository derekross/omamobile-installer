// SPDX-License-Identifier: MIT
//! SHA-256, hex and the comparisons that must not tell an attacker how far
//! they got.
use sha2::{Digest, Sha256};

/// SHA-256 of `data`.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    sha256_parts(&[data])
}

/// SHA-256 over the parts, one after the other.
pub fn sha256_parts(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&h.finalize());
    out
}

/// A running SHA-256, for a shell that hashes a file in pieces.
#[derive(Clone, Default)]
pub struct Hasher(Sha256);

impl Hasher {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }
    pub fn finish(self) -> [u8; 32] {
        let mut out = [0u8; 32];
        out.copy_from_slice(&self.0.finalize());
        out
    }
}

/// Lower-case hex.
pub fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 15) as usize] as char);
    }
    s
}

/// 64 lower-case hex digits as 32 bytes. Upper case is refused: one digest
/// has one spelling on the wire.
pub fn from_hex32(s: &str) -> Option<[u8; 32]> {
    let b = s.as_bytes();
    if b.len() != 64 {
        return None;
    }
    let nib = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = (nib(b[2 * i])? << 4) | nib(b[2 * i + 1])?;
    }
    Some(out)
}

/// Equal or not, in a time that depends on the lengths only.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    // black_box keeps the compiler from turning the fold into an early exit
    core::hint::black_box(diff) == 0
}

/// The label of INSTALL-LINK-PLAN §5.3's serial proof.
pub const SERIAL_LABEL: &[u8] = b"omil-serial-v1";

/// SHA-256("omil-serial-v1" ‖ the 32 nonce bytes ‖ the serial's bytes).
pub fn serial_proof(nonce: &[u8; 32], serial: &[u8]) -> [u8; 32] {
    sha256_parts(&[SERIAL_LABEL, nonce, serial])
}
