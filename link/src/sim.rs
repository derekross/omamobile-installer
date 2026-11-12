// SPDX-License-Identifier: MIT
//! Stand-ins for tests. Behind the `sim` feature: a shipping build has none
//! of it. Nothing here opens a file or a device.
use crate::Entropy;

/// Numbers that look random and are the same every run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestRng(pub u64);

impl TestRng {
    pub fn next_u64(&mut self) -> u64 {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        let mut v = vec![0u8; n];
        self.fill(&mut v);
        v
    }
}

impl Entropy for TestRng {
    fn fill(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            let n = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&n[..chunk.len()]);
        }
    }
}
