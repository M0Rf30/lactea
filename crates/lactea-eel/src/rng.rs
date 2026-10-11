//! MT19937, bit-for-bit the generator behind projectm-eval's `rand()`
//! (`prjm_eval_genrand_int32` in TreeFunctions.c, MilkDrop's original implementation).
//!
//! projectm-eval keeps this state in a process-global static, lazily seeded with
//! [`PROJECTM_DEFAULT_SEED`] and never reseeded.  Here the state lives in
//! [`GlobalMemory`](crate::GlobalMemory) so every context of one engine shares one stream
//! (exactly like projectM, where all contexts share the single static stream), but separate
//! engines are independent and reproducible.

const N: usize = 624;
const M: usize = 397;
const MATRIX_A: u32 = 0x9908_b0df;
const UPPER_MASK: u32 = 0x8000_0000;
const LOWER_MASK: u32 = 0x7fff_ffff;

/// The seed projectm-eval initialises its Mersenne Twister with (`0x4141f00d`).
pub const PROJECTM_DEFAULT_SEED: u32 = 0x4141_f00d;

#[derive(Clone)]
pub(crate) struct Mt {
    mt: [u32; N],
    mti: usize,
}

impl Mt {
    pub(crate) fn new(seed: u32) -> Self {
        let mut m = Mt { mt: [0; N], mti: N };
        m.reseed(seed);
        m
    }

    pub(crate) fn reseed(&mut self, seed: u32) {
        self.mt[0] = seed;
        for i in 1..N {
            let prev = self.mt[i - 1];
            self.mt[i] = 1_812_433_253u32
                .wrapping_mul(prev ^ (prev >> 30))
                .wrapping_add(i as u32);
        }
        // projectm-eval leaves mti == N after initialisation, so the first call regenerates.
        self.mti = N;
    }

    pub(crate) fn next_u32(&mut self) -> u32 {
        let mag01 = [0u32, MATRIX_A];
        if self.mti >= N {
            let mut kk = 0;
            while kk < N - M {
                let y = (self.mt[kk] & UPPER_MASK) | (self.mt[kk + 1] & LOWER_MASK);
                self.mt[kk] = self.mt[kk + M] ^ (y >> 1) ^ mag01[(y & 1) as usize];
                kk += 1;
            }
            while kk < N - 1 {
                let y = (self.mt[kk] & UPPER_MASK) | (self.mt[kk + 1] & LOWER_MASK);
                self.mt[kk] = self.mt[kk + M - N] ^ (y >> 1) ^ mag01[(y & 1) as usize];
                kk += 1;
            }
            let y = (self.mt[N - 1] & UPPER_MASK) | (self.mt[0] & LOWER_MASK);
            self.mt[N - 1] = self.mt[M - 1] ^ (y >> 1) ^ mag01[(y & 1) as usize];
            self.mti = 0;
        }
        let mut y = self.mt[self.mti];
        self.mti += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^= y >> 18;
        y
    }
}
