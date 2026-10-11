//! Deterministic random streams of the engine.
//!
//! The determinism contract (`docs/ARCHITECTURE.md`) says every RNG of the projectM reference build
//! and of lactea is seeded from the run seed. The reference build (`oracle/README.md`,
//! "Determinism") draws every random quantity of a preset from named streams; the engine
//! reproduces the same stream family so a free run sees the *same* `rand_frame`, `rand_preset`,
//! `rot_*`, hue offsets and random-texture picks as the instrumented projectM:
//!
//! ```text
//! state  = splitmix64(seed + γ) ^ fnv1a64(name)          γ = 0x9E3779B97F4A7C15
//! next64 : state += γ; return splitmix64_mix(state)
//! next32 = next64 >> 32
//! ```
//!
//! Streams used: `shader_warp` / `shader_comp` (per shader pass), `hue_offsets`, `randtex_<name>`.
//! (The expression engine's `rand()` is a separate Mersenne Twister owned by `lactea-eel`.)
//!
//! The `6.28` literals are projectM's own rounded constant and must stay as written for bit-identical
//! rotation matrices; the index loops mirror projectM's draw order over parallel arrays.
#![allow(clippy::approx_constant, clippy::needless_range_loop)]

use lactea_abi::NUM_ROT;
use lactea_render::PassRandom;
use lactea_render::uniforms::{drop_rot_translation, pack_float4x3, rot_matrix};

const GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn fnv1a64(s: &str) -> u64 {
    let mut h = 0xCBF2_9CE4_8422_2325u64;
    for &b in s.as_bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

/// One named stream.
#[derive(Clone, Debug)]
pub struct StreamRng {
    state: u64,
}

impl StreamRng {
    pub fn new(seed: u64, name: &str) -> Self {
        Self {
            state: mix(seed.wrapping_add(GAMMA)) ^ fnv1a64(name),
        }
    }

    pub fn next64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(GAMMA);
        mix(self.state)
    }

    pub fn next32(&mut self) -> u32 {
        (self.next64() >> 32) as u32
    }

    /// `rand() % 7381 / 7380.0f` of projectM's shader helper.
    pub fn shader_float(&mut self) -> f32 {
        (self.next32() % 7381) as f32 / 7380.0
    }

    /// `std::uniform_int_distribution<int>(0, INT32_MAX)` replacement.
    pub fn int31(&mut self) -> u32 {
        self.next32() >> 1
    }
}

/// The four per-preset hue-shader phases (`PresetState::hueRandomOffsets`).
pub fn hue_offsets(seed: u64) -> [f32; 4] {
    let mut r = StreamRng::new(seed, "hue_offsets");
    [
        (r.int31() % 64841) as f32 * 0.01,
        (r.int31() % 53751) as f32 * 0.01,
        (r.int31() % 42661) as f32 * 0.01,
        (r.int31() % 31571) as f32 * 0.01,
    ]
}

/// Random state of one shader pass (`MilkdropShader`): `rand_preset`, the 24 rotation
/// parameters drawn at construction, and the per-frame draws.
#[derive(Clone, Debug)]
pub struct ShaderStream {
    rng: StreamRng,
    pub rand_preset: [f32; 4],
    translation: [[f32; 3]; NUM_ROT],
    centers: [[f32; 3]; NUM_ROT],
    speeds: [[f32; 3]; NUM_ROT],
}

impl ShaderStream {
    /// `stream` is `"shader_warp"` or `"shader_comp"`.
    pub fn new(seed: u64, stream: &str) -> Self {
        let mut rng = StreamRng::new(seed, stream);
        let rand_preset = [
            rng.shader_float(),
            rng.shader_float(),
            rng.shader_float(),
            rng.shader_float(),
        ];
        let mut translation = [[0.0f32; 3]; NUM_ROT];
        let mut centers = [[0.0f32; 3]; NUM_ROT];
        let mut speeds = [[0.0f32; 3]; NUM_ROT];
        // projectM's arrays hold 20 entries (`rot_rand1..4` are drawn per frame instead): the
        // constructor consumes 20 * 9 draws after `rand_preset`.
        for index in 0..20 {
            let rot_mult = 0.9f32 * (index as f32 / 8.0f32).powf(3.2f32);
            for k in 0..3 {
                translation[index][k] = (rng.shader_float() * 2.0 - 1.0) * 1.0;
            }
            for k in 0..3 {
                centers[index][k] = rng.shader_float() * 6.28;
            }
            for k in 0..3 {
                speeds[index][k] = (rng.shader_float() * 2.0 - 1.0) * rot_mult;
            }
        }
        Self {
            rng,
            rand_preset,
            translation,
            centers,
            speeds,
        }
    }

    /// Draw the per-frame values (`MilkdropShader::LoadVariables`): `rand_frame` first, then the
    /// four fully random matrices; the 20 time-driven ones use `time` (float, like projectM).
    ///
    /// `drop_translation`: projectM uploads only the rotation columns of each matrix.
    pub fn frame(&mut self, time: f32, drop_translation: bool) -> PassRandom {
        let rand_frame = [
            self.rng.shader_float(),
            self.rng.shader_float(),
            self.rng.shader_float(),
            self.rng.shader_float(),
        ];
        let mut rot = Box::new([[[0.0f32; 4]; 3]; NUM_ROT]);
        for i in 0..20 {
            let ax = self.centers[i][0] + self.speeds[i][0] * time;
            let ay = self.centers[i][1] + self.speeds[i][1] * time;
            let az = self.centers[i][2] + self.speeds[i][2] * time;
            let m = rot_matrix(ax, ay, az, self.translation[i]);
            rot[i] = pack_float4x3(&m);
        }
        for i in 20..24 {
            let ax = self.rng.shader_float() * 6.28;
            let ay = self.rng.shader_float() * 6.28;
            let az = self.rng.shader_float() * 6.28;
            let t = [
                self.rng.shader_float(),
                self.rng.shader_float(),
                self.rng.shader_float(),
            ];
            let m = rot_matrix(ax, ay, az, t);
            rot[i] = pack_float4x3(&m);
        }
        if drop_translation {
            drop_rot_translation(&mut rot);
            // The reference dump (and its GLSL `mat3x4`) holds the transpose of the D3DX-ordered
            // rotation block `rot_matrix` + `pack_float4x3` produce (verified against `warp.rot_*`).
            for m in rot.iter_mut() {
                for c in 0..3 {
                    for r in (c + 1)..3 {
                        let t = m[c][r];
                        m[c][r] = m[r][c];
                        m[r][c] = t;
                    }
                }
            }
        }
        PassRandom {
            rand_frame,
            rand_preset: self.rand_preset,
            rot: Some(rot),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streams_are_deterministic_and_independent() {
        let mut a = StreamRng::new(1, "shader_warp");
        let mut b = StreamRng::new(1, "shader_warp");
        let mut c = StreamRng::new(1, "shader_comp");
        let (x, y, z) = (a.next64(), b.next64(), c.next64());
        assert_eq!(x, y);
        assert_ne!(x, z);
        assert_ne!(StreamRng::new(2, "shader_warp").next64(), x);
    }

    #[test]
    fn shader_float_range() {
        let mut r = StreamRng::new(7, "t");
        for _ in 0..1000 {
            let v = r.shader_float();
            assert!((0.0..=1.0).contains(&v));
        }
    }

    #[test]
    fn hue_offsets_in_range() {
        let h = hue_offsets(1);
        assert!(h[0] < 648.41 && h[1] < 537.51 && h[2] < 426.61 && h[3] < 315.71);
    }
}
