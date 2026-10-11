// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-FileCopyrightText: 2005-2013 Nullsoft, Inc.
// SPDX-License-Identifier: (MIT OR Apache-2.0) AND BSD-3-Clause

//! MD2 blur chain (milkdropfs.cpp `BlurPasses` / `GetSafeBlurMinMax`, plugin.cpp blur texture
//! creation): 6 textures, alternating long-horizontal / short-vertical passes, progressive
//! scale & bias so each level uses the full 8-bit range of its `[min..max]` window.

pub const NUM_BLUR_TEX: usize = 6;

/// Blur kernel weights `w[8]` (hard-coded in MD2: "user can specify these").
pub const WEIGHTS: [f32; 8] = [4.0, 3.8, 3.5, 2.9, 1.9, 1.2, 0.7, 0.3];

/// Sizes of the six blur textures for a `w`x`h` internal image (plugin.cpp):
///
/// ```text
/// for i in 0..6: if !(i&1) || i<2 { w = max(16, w/2); h = max(16, h/2) }
///                size_i = (((w+3)/16)*16, ((h+3)/4)*4)
/// ```
/// i.e. 1/2, 1/4, 1/8, 1/8, 1/16, 1/16 of the image. Blur level `k` (1..=3) is texture `2k-1`.
pub fn blur_tex_sizes(w: u32, h: u32) -> [(u32, u32); NUM_BLUR_TEX] {
    let (mut cw, mut ch) = (w, h);
    let mut out = [(0, 0); NUM_BLUR_TEX];
    for (i, o) in out.iter_mut().enumerate() {
        if i & 1 == 0 || i < 2 {
            cw = (cw / 2).max(16);
            ch = (ch / 2).max(16);
        }
        *o = (((cw + 3) / 16) * 16, ch.div_ceil(4) * 4);
    }
    out
}

/// `GetSafeBlurMinMax`: keep each level's window inside the previous one and at least 0.1 wide.
///
/// Ported literally: both MD2 (BeatDrop copy) and projectM set `max = avg - 0.05` in the "push
/// apart" branches (a typo that collapses the window to zero width and yields an infinite
/// scale); there is no divergence between the two, so there is no compat switch.
pub fn safe_blur_min_max(min_in: [f32; 3], max_in: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    let mut mn = min_in;
    let mut mx = max_in;
    const MIN_DIST: f32 = 0.1;
    if mx[0] - mn[0] < MIN_DIST {
        let avg = (mn[0] + mx[0]) * 0.5;
        mn[0] = avg - MIN_DIST * 0.5;
        mx[0] = avg - MIN_DIST * 0.5;
    }
    mx[1] = mx[0].min(mx[1]);
    mn[1] = mn[0].max(mn[1]);
    if mx[1] - mn[1] < MIN_DIST {
        let avg = (mn[1] + mx[1]) * 0.5;
        mn[1] = avg - MIN_DIST * 0.5;
        mx[1] = avg - MIN_DIST * 0.5;
    }
    mx[2] = mx[1].min(mx[2]);
    mn[2] = mn[1].max(mn[2]);
    if mx[2] - mn[2] < MIN_DIST {
        let avg = (mn[2] + mx[2]) * 0.5;
        mn[2] = avg - MIN_DIST * 0.5;
        mx[2] = avg - MIN_DIST * 0.5;
    }
    (mn, mx)
}

/// Progressive `(scale, bias)` applied by the horizontal pass of blur level `k`
/// (so that `value*scale+bias` expands the level's window to `[0,1]`).
pub fn scale_bias(mn: [f32; 3], mx: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    let mut s = [0.0; 3];
    let mut b = [0.0; 3];
    s[0] = 1.0 / (mx[0] - mn[0]);
    b[0] = -mn[0] * s[0];
    let mut temp_min = (mn[1] - mn[0]) / (mx[0] - mn[0]);
    let mut temp_max = (mx[1] - mn[0]) / (mx[0] - mn[0]);
    s[1] = 1.0 / (temp_max - temp_min);
    b[1] = -temp_min * s[1];
    temp_min = (mn[2] - mn[1]) / (mx[1] - mn[1]);
    temp_max = (mx[2] - mn[1]) / (mx[1] - mn[1]);
    s[2] = 1.0 / (temp_max - temp_min);
    b[2] = -temp_min * s[2];
    (s, b)
}

/// Constants of the horizontal pass: `_c1 = (w1..w4)`, `_c2 = (d1..d4)`, `w_div`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HorizontalTaps {
    pub w: [f32; 4],
    pub d: [f32; 4],
    pub w_div: f32,
}

pub fn horizontal_taps() -> HorizontalTaps {
    let w = WEIGHTS;
    let w1 = w[0] + w[1];
    let w2 = w[2] + w[3];
    let w3 = w[4] + w[5];
    let w4 = w[6] + w[7];
    HorizontalTaps {
        w: [w1, w2, w3, w4],
        d: [
            0.0 + 2.0 * w[1] / w1,
            2.0 + 2.0 * w[3] / w2,
            4.0 + 2.0 * w[5] / w3,
            6.0 + 2.0 * w[7] / w4,
        ],
        w_div: 0.5 / (w1 + w2 + w3 + w4),
    }
}

/// Constants of the vertical pass: `_c5 = (w1, w2, d1, d2)`, `w_div`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VerticalTaps {
    pub w: [f32; 2],
    pub d: [f32; 2],
    pub w_div: f32,
}

pub fn vertical_taps() -> VerticalTaps {
    let w = WEIGHTS;
    let w1 = w[0] + w[1] + w[2] + w[3];
    let w2 = w[4] + w[5] + w[6] + w[7];
    VerticalTaps {
        w: [w1, w2],
        d: [
            0.0 + 2.0 * ((w[2] + w[3]) / w1),
            2.0 + 2.0 * ((w[6] + w[7]) / w2),
        ],
        w_div: 1.0 / ((w1 + w2) * 2.0),
    }
}

/// Number of blur passes that must run: `min(6, highest_level_used * 2)`.
pub fn pass_count(highest_level_used: u8) -> usize {
    (NUM_BLUR_TEX).min(highest_level_used as usize * 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_640x360() {
        let s = blur_tex_sizes(640, 360);
        // w: 320,160,80,80,40,40 -> ((w+3)/16)*16 ; h: 180,90,45,45,22->?,..
        assert_eq!(s[0], (320, 180));
        assert_eq!(s[1], (160, 92)); // h=90 -> (93/4)*4 = 92
        assert_eq!(s[2], (80, 48)); // h=45 -> (48/4)*4 = 48
        assert_eq!(s[3], (80, 48));
        assert_eq!(s[4], (32, 24)); // w=40 -> (43/16)*16 = 32 ; h=22 -> (25/4)*4 = 24
        assert_eq!(s[5], (32, 24));
    }

    #[test]
    fn sizes_have_min_16() {
        let s = blur_tex_sizes(64, 64);
        for (w, h) in s {
            assert!(w >= 16 && h >= 16);
        }
    }

    #[test]
    fn horizontal_taps_hand_computed() {
        let t = horizontal_taps();
        assert_eq!(t.w, [7.8, 6.4, 3.1, 1.0]);
        let d1 = 2.0 * 3.8 / 7.8;
        let d2 = 2.0 + 2.0 * 2.9 / 6.4;
        assert!((t.d[0] - d1).abs() < 1e-6 && (t.d[1] - d2).abs() < 1e-6);
        assert!((t.w_div - 0.5 / 18.3).abs() < 1e-7);
        // the kernel is normalised: 2 * sum(w_k) * w_div == 1
        assert!((2.0 * t.w.iter().sum::<f32>() * t.w_div - 1.0).abs() < 1e-6);
    }

    #[test]
    fn vertical_taps_hand_computed() {
        let t = vertical_taps();
        assert!((t.w[0] - 14.2).abs() < 1e-5 && (t.w[1] - 4.1).abs() < 1e-5);
        assert!((t.d[0] - 2.0 * (6.4 / 14.2)).abs() < 1e-6);
        assert!((t.d[1] - (2.0 + 2.0 * (1.0 / 4.1))).abs() < 1e-6);
        assert!((2.0 * (t.w[0] + t.w[1]) * t.w_div - 1.0).abs() < 1e-6);
    }

    #[test]
    fn scale_bias_progression() {
        let (mn, mx) = safe_blur_min_max([0.0; 3], [1.0; 3]);
        let (s, b) = scale_bias(mn, mx);
        assert_eq!(s, [1.0, 1.0, 1.0]);
        assert_eq!(b, [0.0, 0.0, 0.0]);
        // level1 window [0.2, 0.8]: scale 1/0.6, bias -0.2/0.6
        let (mn, mx) = safe_blur_min_max([0.2, 0.0, 0.0], [0.8, 1.0, 1.0]);
        let (s, b) = scale_bias(mn, mx);
        assert!((s[0] - 1.0 / 0.6).abs() < 1e-5 && (b[0] + 0.2 / 0.6).abs() < 1e-5);
        // level 2 asks for [0,1] but is clamped into level 1's window -> identity progression
        assert!((s[1] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn min_max_push_apart_and_nesting() {
        let (mn, mx) = safe_blur_min_max([0.5, 0.0, 0.0], [0.52, 1.0, 1.0]);
        // literal MD2/projectM typo: both end up at avg - 0.05
        assert!((mx[0] - mn[0]).abs() < 1e-6 && (mn[0] - 0.46).abs() < 1e-6);
        // later levels are clamped into the previous window *before* being pushed apart
        assert!(mx[1] <= 0.5 && mx[2] <= 0.5);
    }

    #[test]
    fn pass_counts() {
        assert_eq!(pass_count(0), 0);
        assert_eq!(pass_count(1), 2);
        assert_eq!(pass_count(2), 4);
        assert_eq!(pass_count(3), 6);
    }
}
