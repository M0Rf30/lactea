//! MT19937, bit-for-bit the generator behind projectm-eval's `rand()`
//! (`prjm_eval_genrand_int32` in TreeFunctions.c, MilkDrop's original implementation).
//!
//! projectm-eval keeps this state in a process-global static, lazily seeded with
//! [`PROJECTM_DEFAULT_SEED`] and never reseeded.  Here the state lives in
//! [`GlobalMemory`](crate::GlobalMemory) so every context of one engine shares one stream
//! (exactly like projectM, where all contexts share the single static stream), but separate
//! engines are independent and reproducible.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering::Relaxed};

const N: usize = 624;
const M: usize = 397;
const MATRIX_A: u32 = 0x9908_b0df;
const UPPER_MASK: u32 = 0x8000_0000;
const LOWER_MASK: u32 = 0x7fff_ffff;

/// The seed projectm-eval initialises its Mersenne Twister with (`0x4141f00d`).
pub const PROJECTM_DEFAULT_SEED: u32 = 0x4141_f00d;

/// The generator state as relaxed atomics: a draw is plain loads and stores, no lock and no read-modify-write (a
/// `Mutex` per `rand()` call was 9% of the render thread on a preset with thousands of shape instances).
///
/// One stream is drawn from by one thread at a time (the contexts of an engine run one after another).  Drawing from
/// several threads at the same moment is memory-safe (every index stays below `N`) but the values are unspecified,
/// like projectm-eval's own unsynchronised static.
pub(crate) struct Mt {
    mt: [AtomicU32; N],
    mti: AtomicUsize,
}

impl Mt {
    pub(crate) fn new(seed: u32) -> Self {
        let m = Mt {
            mt: std::array::from_fn(|_| AtomicU32::new(0)),
            mti: AtomicUsize::new(N),
        };
        m.reseed(seed);
        m
    }

    pub(crate) fn reseed(&self, seed: u32) {
        self.mt[0].store(seed, Relaxed);
        for i in 1..N {
            let prev = self.mt[i - 1].load(Relaxed);
            self.mt[i].store(
                1_812_433_253u32
                    .wrapping_mul(prev ^ (prev >> 30))
                    .wrapping_add(i as u32),
                Relaxed,
            );
        }
        // projectm-eval leaves mti == N after initialisation, so the first call regenerates.
        self.mti.store(N, Relaxed);
    }

    pub(crate) fn next_u32(&self) -> u32 {
        let mag01 = [0u32, MATRIX_A];
        let mut idx = self.mti.load(Relaxed);
        if idx >= N {
            let mt = |i: usize| self.mt[i].load(Relaxed);
            let mut kk = 0;
            while kk < N - M {
                let y = (mt(kk) & UPPER_MASK) | (mt(kk + 1) & LOWER_MASK);
                self.mt[kk].store(mt(kk + M) ^ (y >> 1) ^ mag01[(y & 1) as usize], Relaxed);
                kk += 1;
            }
            while kk < N - 1 {
                let y = (mt(kk) & UPPER_MASK) | (mt(kk + 1) & LOWER_MASK);
                self.mt[kk].store(mt(kk + M - N) ^ (y >> 1) ^ mag01[(y & 1) as usize], Relaxed);
                kk += 1;
            }
            let y = (mt(N - 1) & UPPER_MASK) | (mt(0) & LOWER_MASK);
            self.mt[N - 1].store(mt(M - 1) ^ (y >> 1) ^ mag01[(y & 1) as usize], Relaxed);
            idx = 0;
        }
        let mut y = self.mt[idx].load(Relaxed);
        self.mti.store(idx + 1, Relaxed);
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^= y >> 18;
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The C++ standard fixes the 10000th output of `std::mt19937` (default seed 5489): the draw crosses the
    /// state regeneration 16 times.
    #[test]
    fn matches_the_standard_mt19937_sequence() {
        let m = Mt::new(5489);
        let mut last = 0;
        for _ in 0..10_000 {
            last = m.next_u32();
        }
        assert_eq!(last, 4_123_659_995);
    }
}
