// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The kernel's random number generator.
//!
//! * [`Generator`]: a cryptographically secure generator in the style of
//!   Linux's: an entropy pool hashed with BLAKE2s (RFC 7693) feeds the key
//!   of a ChaCha20 stream (RFC 8439), and every request replaces the key
//!   with fresh keystream before returning ("fast key erasure"), so a later
//!   compromise of the state does not reveal earlier output.
//! * [`FastPool`]: a cheap pool for interrupt timing, folded into a
//!   [`Generator`].
//!
//! ChaCha20 and BLAKE2s are RustCrypto's (`chacha20`, `blake2`), which wipe
//! their state when it is dropped; on the soft-float targets of the loader
//! and kernel they run their portable code
//! (`.cargo/architectures/amd64/config.toml`). The crate has no platform
//! code: the kernel supplies the entropy (firmware seed, the processor's
//! random number generators, timing jitter, interrupt timing) and the
//! locking. It is `no_std` and does not allocate, so the kernel can use it.

#![no_std]

#[cfg(test)]
extern crate std;

use blake2::{Blake2s256, Digest};
use chacha20::ChaCha20;
use chacha20::cipher::{KeyIvInit, StreamCipher};

/// Bytes in a generator key.
pub const KEY_LEN: usize = 32;

/// A ChaCha20-based generator with an entropy pool.
///
/// Feed entropy with [`Generator::mix`], fold the pool into the key with
/// [`Generator::reseed`], and draw output with [`Generator::fill`]. The
/// caller decides when to reseed (the kernel does it at boot and then
/// periodically).
#[derive(Clone)]
pub struct Generator {
    key: [u8; KEY_LEN],
    /// BLAKE2s over every input so far (`None` until the first: a hash's
    /// state cannot be made in a `const`, as the kernel's static needs).
    pool: Option<Blake2s256>,
    /// Number of entropy inputs mixed into the pool since the last reseed.
    pending_inputs: u32,
    reseeds: u64,
}

impl Generator {
    /// A generator whose key and pool start from fixed values: it must be
    /// seeded with [`Generator::mix`] and [`Generator::reseed`] before its
    /// output is unpredictable.
    pub const fn new() -> Generator {
        Generator { key: [0; KEY_LEN], pool: None, pending_inputs: 0, reseeds: 0 }
    }

    /// Mixes `data` into the entropy pool. Inputs never reduce the
    /// unpredictability of the pool, so untrusted data may be mixed too.
    pub fn mix(&mut self, data: &[u8]) {
        // Length-prefix each input so concatenations cannot collide.
        let pool = self.pool.get_or_insert_with(Blake2s256::new);
        pool.update((data.len() as u64).to_le_bytes());
        pool.update(data);
        self.pending_inputs = self.pending_inputs.saturating_add(1);
    }

    /// Folds the entropy pool into the key: `key = BLAKE2s(key ‖ pool)`.
    /// The pool keeps accumulating (it is chained, not cleared).
    pub fn reseed(&mut self) {
        let pool = self.pool.get_or_insert_with(Blake2s256::new);
        let pool_digest = pool.clone().finalize();
        let mut h = Blake2s256::new();
        h.update(b"veda reseed");
        h.update(self.key);
        h.update(pool_digest);
        h.update(self.reseeds.to_le_bytes());
        self.key = h.finalize().into();
        // Chain the pool so the next digest depends on everything so far.
        pool.update(pool_digest);
        self.reseeds += 1;
        self.pending_inputs = 0;
    }

    /// How many times the generator has been reseeded.
    pub fn reseed_count(&self) -> u64 {
        self.reseeds
    }

    /// Entropy inputs mixed since the last reseed.
    pub fn pending_inputs(&self) -> u32 {
        self.pending_inputs
    }

    /// Fills `out` with random bytes (at most 256 GiB, ChaCha20's stream
    /// under one key and nonce).
    ///
    /// The key's ChaCha20 stream (a zero nonce, from block 0): its first 32
    /// bytes become the next key and are never output; the rest of it is
    /// the output.
    pub fn fill(&mut self, out: &mut [u8]) {
        let mut stream = ChaCha20::new(&self.key.into(), &[0; 12].into());
        let mut next = [0u8; KEY_LEN];
        stream.apply_keystream(&mut next);
        out.fill(0);
        stream.apply_keystream(out);
        self.key = next;
    }

    /// Returns a random `u64`.
    pub fn next_u64(&mut self) -> u64 {
        let mut b = [0u8; 8];
        self.fill(&mut b);
        u64::from_le_bytes(b)
    }
}

impl Default for Generator {
    fn default() -> Self {
        Generator::new()
    }
}

/// A small mixing pool for interrupt timestamps: cheap enough to run on
/// every interrupt, folded into a [`Generator`] at reseed time. It is not a
/// cryptographic hash; it only has to retain the timing variations until
/// BLAKE2s condenses them.
#[derive(Debug, Clone, Copy, Default)]
pub struct FastPool {
    lanes: [u64; 4],
    count: u32,
}

impl FastPool {
    pub const fn new() -> FastPool {
        FastPool { lanes: [0; 4], count: 0 }
    }

    /// Mixes one sample (for example a clock counter reading and an interrupt
    /// vector).
    pub fn add(&mut self, a: u64, b: u64) {
        // A SipHash-like round over the four lanes.
        let l = &mut self.lanes;
        l[0] ^= a;
        l[1] ^= b;
        l[0] = l[0].wrapping_add(l[1]);
        l[1] = l[1].rotate_left(13) ^ l[0];
        l[0] = l[0].rotate_left(32);
        l[2] = l[2].wrapping_add(l[3]);
        l[3] = l[3].rotate_left(16) ^ l[2];
        l[0] = l[0].wrapping_add(l[3]);
        l[3] = l[3].rotate_left(21) ^ l[0];
        l[2] = l[2].wrapping_add(l[1]);
        l[1] = l[1].rotate_left(17) ^ l[2];
        l[2] = l[2].rotate_left(32);
        self.count = self.count.wrapping_add(1);
    }

    /// Samples mixed since the pool was last drained.
    pub fn count(&self) -> u32 {
        self.count
    }

    /// Returns the pool contents and resets it.
    pub fn drain(&mut self) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, lane) in self.lanes.iter().enumerate() {
            out[i * 8..i * 8 + 8].copy_from_slice(&lane.to_le_bytes());
        }
        *self = FastPool::new();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded(seed: &[u8]) -> Generator {
        let mut g = Generator::new();
        g.mix(seed);
        g.reseed();
        g
    }

    #[test]
    fn output_depends_on_the_seed() {
        let mut a = seeded(b"seed one");
        let mut b = seeded(b"seed two");
        let (mut x, mut y) = ([0u8; 64], [0u8; 64]);
        a.fill(&mut x);
        b.fill(&mut y);
        assert_ne!(x, y);
        // Deterministic for a given seed (needed for reproducible tests).
        let mut c = seeded(b"seed one");
        let mut z = [0u8; 64];
        c.fill(&mut z);
        assert_eq!(x, z);
    }

    /// The generator's output for fixed inputs, as its first version (its
    /// own ChaCha20 and BLAKE2s) produced it: the output and keys are
    /// exactly what RFC 8439 and RFC 7693 make of the design.
    #[test]
    fn known_answers() {
        let hex = |b: &[u8]| b.iter().map(|x| std::format!("{x:02x}")).collect::<std::string::String>();
        let mut g = Generator::new();
        g.mix(b"known answer");
        g.mix(&[0xa5; 300]);
        g.reseed();
        let mut out = [0u8; 150];
        g.fill(&mut out);
        assert_eq!(
            hex(&out),
            "f3664c94a182ad064e77309d74ac3bcd40d9bdaa8ce16f2ca869bf9ac606917fcb69b76fdf97f38af59efcd1cb7a77a920d66398e4e3bab7301b8e7e38770d74650e918f2b7905ddd7cfe4403789519567412bbf3ea6d5303a0cca9eaf2edf063036a4dc6af7f463c81b8f40f3830e4d5cbc9f0e480545f0c729cef82b202fe29581fdeecf0efadec45ebacd325ddf513e252ba3df6b"
        );
        let mut more = [0u8; 40];
        g.fill(&mut more);
        assert_eq!(hex(&more), "3b4a3f425c07b7ec424cbf5354581b778746048c018acc0862ec899f55f5208367f3e4c84aa001b3");
        assert_eq!(hex(&g.key), "de1c4bd04d5fb046a0b0d0f789d700517019893dd001a7858de42b7be23dda50");
        // A pool nothing was mixed into.
        let mut fresh = Generator::new();
        fresh.reseed();
        assert_eq!(hex(&fresh.key), "ecf2211632b1dfd6722f937ad552f44bb0ef66fb778d45801f0ac2de5618bdf8");
    }

    #[test]
    fn consecutive_requests_differ_and_rekey() {
        let mut g = seeded(b"x");
        let k0 = g.key;
        let (mut a, mut b) = ([0u8; 100], [0u8; 100]);
        g.fill(&mut a);
        let k1 = g.key;
        g.fill(&mut b);
        assert_ne!(a, b);
        assert_ne!(k0, k1);
        assert_ne!(k1, g.key);
        // The new key is never part of the output.
        assert!(!a.windows(32).any(|w| w == k1));
    }

    #[test]
    fn output_spans_many_blocks_without_repeating() {
        let mut g = seeded(b"long");
        let mut big = std::vec![0u8; 64 * 40 + 7];
        g.fill(&mut big);
        let blocks: std::vec::Vec<&[u8]> = big.chunks(64).collect();
        for i in 0..blocks.len() {
            for j in i + 1..blocks.len() {
                assert_ne!(blocks[i], blocks[j]);
            }
        }
    }

    #[test]
    fn reseeding_changes_the_stream() {
        let mut a = seeded(b"same");
        let mut b = seeded(b"same");
        b.mix(b"extra entropy");
        b.reseed();
        let (mut x, mut y) = ([0u8; 32], [0u8; 32]);
        a.fill(&mut x);
        b.fill(&mut y);
        assert_ne!(x, y);
        assert_eq!(b.reseed_count(), 2);
    }

    #[test]
    fn small_requests_work() {
        let mut g = seeded(b"tiny");
        let mut one = [0u8; 1];
        g.fill(&mut one);
        let mut none = [0u8; 0];
        g.fill(&mut none);
        let _ = g.next_u64();
    }

    #[test]
    fn fast_pool_retains_variation() {
        let mut p = FastPool::new();
        let mut q = FastPool::new();
        for i in 0..100u64 {
            p.add(1000 + i * 7, 33);
            q.add(1000 + i * 7 + (i == 50) as u64, 33);
        }
        assert_eq!(p.count(), 100);
        assert_ne!(p.drain(), q.drain());
        assert_eq!(p.count(), 0);
    }
}
