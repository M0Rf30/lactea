// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tiny deterministic PRNG (SplitMix64). Every random decision the renderer makes
//! (noise textures, preset rotation parameters, per-frame random rotations, hue-shader
//! phases) is derived from `RenderConfig::seed` through this generator.

/// The SplitMix64 output mixer: a bijective scramble of `z`, usable on its own to hash a value into 64 random bits.
pub fn mix64(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The renderer's deterministic generator (SplitMix64).
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    /// A generator starting at `seed`.
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    /// Derive an independent stream: `Rng::new(seed).fork(tag)`.
    pub fn fork(&self, tag: u64) -> Rng {
        let mut r = Rng(self.0 ^ tag.wrapping_mul(0xD6E8_FEB8_6659_FD93));
        r.next_u64();
        r
    }

    /// Next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        mix64(self.0)
    }

    /// Uniform in `[0, 1)`; MD2's `FRAND` (plugin.cpp: `uniform_real_distribution<float>(0,1)`).
    pub fn frand(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / 16_777_216.0
    }

    /// Uniform integer in `[0, n)`.
    pub fn below(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        (((self.next_u64() >> 32) * n as u64) >> 32) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_in_range() {
        let mut a = Rng::new(1);
        let mut b = Rng::new(1);
        for _ in 0..1000 {
            let x = a.frand();
            assert_eq!(x, b.frand());
            assert!((0.0..1.0).contains(&x));
        }
        let mut c = Rng::new(2);
        assert_ne!(Rng::new(1).next_u64(), c.next_u64());
        for _ in 0..1000 {
            assert!(a.below(7) < 7);
        }
    }
}
