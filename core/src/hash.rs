// SPDX-License-Identifier: MIT
//! SHA-256 helpers.
use sha2::{Digest, Sha256};

/// A SHA-256 digest as 64 lower-case hex digits.
pub fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 15) as usize] as char);
    }
    s
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

pub fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// SHA-256 over a stream, with the byte count.
#[derive(Default, Clone)]
pub struct StreamHasher {
    inner: Sha256,
    len: u64,
}

impl StreamHasher {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
        self.len += data.len() as u64;
    }
    pub fn len(&self) -> u64 {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// The digest in hex and the number of bytes hashed.
    pub fn finish(self) -> (String, u64) {
        (hex(&self.inner.finalize()), self.len)
    }
}
