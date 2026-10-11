//! Image-pair metrics and visual artefacts.
//!
//! Units: all absolute differences are in 8-bit code values (0..=255). Metrics ignore alpha
//! unless [`MetricOpts::include_alpha`] is set (then alpha counts as a fourth channel in
//! mean/RMSE/PSNR/fractions); SSIM is always computed on luma (Rec.601, from RGB only).

use crate::dump::Image;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug)]
/// Options for [`compare_images`].
pub struct MetricOpts {
    /// Count alpha as a fourth channel in mean/RMSE/PSNR/fractions.
    pub include_alpha: bool,
    /// Whether to compute SSIM (otherwise it is NaN).
    pub ssim: bool,
}

impl Default for MetricOpts {
    fn default() -> Self {
        Self {
            include_alpha: false,
            ssim: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
/// Difference metrics for one image pair (reference vs candidate).
pub struct ImageMetrics {
    /// Max |a-b| per R,G,B channel.
    pub max_abs: [u8; 3],
    /// Max |a-b| of alpha (informational, never part of the other metrics unless opted in).
    pub max_abs_alpha: u8,
    /// Mean absolute difference over the counted channels.
    pub mean_abs: f64,
    /// Root-mean-square difference over the counted channels.
    pub rmse: f64,
    /// 10*log10(255^2/MSE); `inf` when identical (serialised as `"inf"`).
    #[serde(with = "crate::util::jf64")]
    pub psnr: f64,
    /// Mean SSIM on luma; NaN (`"nan"`) if disabled.
    #[serde(with = "crate::util::jf64")]
    pub ssim: f64,
    /// Fraction of pixels with any-channel |diff| > 2 / 8 / 32.
    pub frac_gt2: f64,
    /// Fraction of pixels with any-channel difference above 8.
    pub frac_gt8: f64,
    /// Fraction of pixels with any-channel difference above 32.
    pub frac_gt32: f64,
}

impl ImageMetrics {
    /// Largest per-channel RGB difference.
    pub fn max_abs_any(&self) -> u8 {
        self.max_abs[0].max(self.max_abs[1]).max(self.max_abs[2])
    }
    /// Whether the RGB channels are exactly equal (alpha is ignored).
    pub fn identical(&self) -> bool {
        self.max_abs_any() == 0
    }
}

/// PSNR in dB for a mean squared error in 8-bit code values; infinite when `mse <= 0`.
pub fn psnr_from_mse(mse: f64) -> f64 {
    if mse <= 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("image size mismatch: {0}x{1} vs {2}x{3}")]
/// The two images passed to [`compare_images`] differ in size.
pub struct SizeMismatch(pub u32, pub u32, pub u32, pub u32);

/// Computes [`ImageMetrics`] for two same-sized images; `a` is the reference.
pub fn compare_images(
    a: &Image,
    b: &Image,
    opts: MetricOpts,
) -> Result<ImageMetrics, SizeMismatch> {
    if a.width != b.width || a.height != b.height {
        return Err(SizeMismatch(a.width, a.height, b.width, b.height));
    }
    let n = a.width as usize * a.height as usize;
    let nch = if opts.include_alpha { 4u64 } else { 3 };
    let (mut max_abs, mut max_a) = ([0u8; 3], 0u8);
    let (mut sum_abs, mut sum_sq) = (0u64, 0u64);
    let (mut c2, mut c8, mut c32) = (0u64, 0u64, 0u64);
    for (pa, pb) in a.rgba.chunks_exact(4).zip(b.rgba.chunks_exact(4)) {
        let mut px_max = 0u8;
        for c in 0..3 {
            let d = pa[c].abs_diff(pb[c]);
            max_abs[c] = max_abs[c].max(d);
            px_max = px_max.max(d);
            sum_abs += d as u64;
            sum_sq += (d as u64) * (d as u64);
        }
        let da = pa[3].abs_diff(pb[3]);
        max_a = max_a.max(da);
        if opts.include_alpha {
            px_max = px_max.max(da);
            sum_abs += da as u64;
            sum_sq += (da as u64) * (da as u64);
        }
        c2 += (px_max > 2) as u64;
        c8 += (px_max > 8) as u64;
        c32 += (px_max > 32) as u64;
    }
    let denom = (n as u64 * nch).max(1) as f64;
    let mse = sum_sq as f64 / denom;
    let nf = n.max(1) as f64;
    let rgb_identical = max_abs == [0, 0, 0];
    let ssim = if !opts.ssim {
        f64::NAN
    } else if rgb_identical {
        1.0
    } else {
        ssim_luma(a, b)
    };
    Ok(ImageMetrics {
        max_abs,
        max_abs_alpha: max_a,
        mean_abs: sum_abs as f64 / denom,
        rmse: mse.sqrt(),
        psnr: psnr_from_mse(mse),
        ssim,
        frac_gt2: c2 as f64 / nf,
        frac_gt8: c8 as f64 / nf,
        frac_gt32: c32 as f64 / nf,
    })
}

// --------------------------------------------------------------------------------------------
// SSIM
// --------------------------------------------------------------------------------------------

const SSIM_WINDOW: usize = 11;
const SSIM_SIGMA: f64 = 1.5;
const SSIM_K1: f64 = 0.01;
const SSIM_K2: f64 = 0.03;

fn luma(img: &Image) -> Vec<f64> {
    img.rgba
        .chunks_exact(4)
        .map(|p| 0.299 * p[0] as f64 + 0.587 * p[1] as f64 + 0.114 * p[2] as f64)
        .collect()
}

fn gaussian_kernel(size: usize, sigma: f64) -> Vec<f64> {
    let c = (size as f64 - 1.0) / 2.0;
    let mut k: Vec<f64> = (0..size)
        .map(|i| (-((i as f64 - c).powi(2)) / (2.0 * sigma * sigma)).exp())
        .collect();
    let s: f64 = k.iter().sum();
    k.iter_mut().for_each(|v| *v /= s);
    k
}

/// Separable "valid" filtering: output is (w-k+1) x (h-k+1).
fn filter_valid(src: &[f64], w: usize, h: usize, k: &[f64]) -> Vec<f64> {
    let kn = k.len();
    let ow = w - kn + 1;
    let oh = h - kn + 1;
    let mut tmp = vec![0.0f64; ow * h];
    for y in 0..h {
        let row = &src[y * w..(y + 1) * w];
        for x in 0..ow {
            let mut acc = 0.0;
            for (i, kv) in k.iter().enumerate() {
                acc += kv * row[x + i];
            }
            tmp[y * ow + x] = acc;
        }
    }
    let mut out = vec![0.0f64; ow * oh];
    for y in 0..oh {
        for (i, kv) in k.iter().enumerate() {
            let row = &tmp[(y + i) * ow..(y + i + 1) * ow];
            let o = &mut out[y * ow..(y + 1) * ow];
            for x in 0..ow {
                o[x] += kv * row[x];
            }
        }
    }
    out
}

/// Mean SSIM of the luma planes (Wang et al. 2004): 11x11 Gaussian window, sigma 1.5,
/// K1=0.01, K2=0.03, L=255, "valid" region only (no border padding). Images smaller than the
/// window use the largest odd window that fits. Panics if sizes differ.
pub fn ssim_luma(a: &Image, b: &Image) -> f64 {
    assert_eq!(
        (a.width, a.height),
        (b.width, b.height),
        "ssim: size mismatch"
    );
    let (w, h) = (a.width as usize, a.height as usize);
    if w == 0 || h == 0 {
        return 1.0;
    }
    let mut win = SSIM_WINDOW.min(w).min(h);
    if win % 2 == 0 {
        win -= 1;
    }
    let k = gaussian_kernel(win, SSIM_SIGMA);
    let x = luma(a);
    let y = luma(b);
    let xx: Vec<f64> = x.iter().map(|v| v * v).collect();
    let yy: Vec<f64> = y.iter().map(|v| v * v).collect();
    let xy: Vec<f64> = x.iter().zip(&y).map(|(a, b)| a * b).collect();
    let mu_x = filter_valid(&x, w, h, &k);
    let mu_y = filter_valid(&y, w, h, &k);
    let exx = filter_valid(&xx, w, h, &k);
    let eyy = filter_valid(&yy, w, h, &k);
    let exy = filter_valid(&xy, w, h, &k);
    let c1 = (SSIM_K1 * 255.0).powi(2);
    let c2 = (SSIM_K2 * 255.0).powi(2);
    let mut sum = 0.0;
    for i in 0..mu_x.len() {
        let (mx, my) = (mu_x[i], mu_y[i]);
        let vx = exx[i] - mx * mx;
        let vy = eyy[i] - my * my;
        let cxy = exy[i] - mx * my;
        sum +=
            ((2.0 * mx * my + c1) * (2.0 * cxy + c2)) / ((mx * mx + my * my + c1) * (vx + vy + c2));
    }
    sum / mu_x.len() as f64
}

// --------------------------------------------------------------------------------------------
// Visual artefacts
// --------------------------------------------------------------------------------------------

/// Colour ramp for a normalised value t in [0,1]: black -> blue -> cyan -> yellow -> red -> white.
fn ramp(t: f64) -> [u8; 3] {
    const STOPS: [(f64, [f64; 3]); 6] = [
        (0.0, [0.0, 0.0, 0.0]),
        (0.2, [0.0, 0.0, 255.0]),
        (0.4, [0.0, 255.0, 255.0]),
        (0.6, [255.0, 255.0, 0.0]),
        (0.8, [255.0, 0.0, 0.0]),
        (1.0, [255.0, 255.0, 255.0]),
    ];
    let t = t.clamp(0.0, 1.0);
    for w in STOPS.windows(2) {
        let (t0, c0) = w[0];
        let (t1, c1) = w[1];
        if t <= t1 {
            let f = (t - t0) / (t1 - t0);
            let mut o = [0u8; 3];
            for i in 0..3 {
                o[i] = (c0[i] + (c1[i] - c0[i]) * f).round() as u8;
            }
            return o;
        }
    }
    [255, 255, 255]
}

/// Diff heat-map: colour encodes sqrt(max-channel |diff| / 255); zero diff is pure black,
/// diff 1 is already visibly dark blue, 255 is white.
pub fn diff_heatmap(a: &Image, b: &Image) -> Image {
    let w = a.width.min(b.width);
    let h = a.height.min(b.height);
    let mut out = Image::solid(w, h, [0, 0, 0, 255]);
    for y in 0..h {
        for x in 0..w {
            let (pa, pb) = (a.pixel(x, y), b.pixel(x, y));
            let d = (0..3).map(|c| pa[c].abs_diff(pb[c])).max().unwrap_or(0);
            let rgb = ramp((d as f64 / 255.0).sqrt());
            out.set_pixel(x, y, [rgb[0], rgb[1], rgb[2], 255]);
        }
    }
    out
}

/// `ref | candidate | |diff| * amp` side by side with 4px separators; if sizes differ the
/// diff panel is computed on the common top-left region and the panels are padded.
pub fn triptych(a: &Image, b: &Image, amp: u32) -> Image {
    const SEP: u32 = 4;
    let h = a.height.max(b.height);
    let pw = a.width.max(b.width);
    let total_w = pw * 3 + SEP * 2;
    let mut out = Image::solid(total_w, h, [32, 32, 32, 255]);
    let cw = a.width.min(b.width);
    let ch = a.height.min(b.height);
    for y in 0..h {
        for x in 0..pw {
            if x < a.width && y < a.height {
                let p = a.pixel(x, y);
                out.set_pixel(x, y, [p[0], p[1], p[2], 255]);
            }
            if x < b.width && y < b.height {
                let p = b.pixel(x, y);
                out.set_pixel(pw + SEP + x, y, [p[0], p[1], p[2], 255]);
            }
            if x < cw && y < ch {
                let (pa, pb) = (a.pixel(x, y), b.pixel(x, y));
                let mut o = [0u8, 0, 0, 255];
                for c in 0..3 {
                    o[c] = (pa[c].abs_diff(pb[c]) as u32 * amp).min(255) as u8;
                }
                out.set_pixel(2 * (pw + SEP) + x, y, o);
            }
        }
    }
    out
}

/// Area-average (box) downscale to `tw` x `th` (each >= 1). Upscaling is not supported
/// (clamped to source size).
pub fn resize_box(src: &Image, tw: u32, th: u32) -> Image {
    let tw = tw.clamp(1, src.width.max(1));
    let th = th.clamp(1, src.height.max(1));
    let mut out = Image::solid(tw, th, [0, 0, 0, 255]);
    for ty in 0..th {
        let y0 = (ty as u64 * src.height as u64 / th as u64) as u32;
        let y1 = (((ty as u64 + 1) * src.height as u64 / th as u64) as u32)
            .max(y0 + 1)
            .min(src.height);
        for tx in 0..tw {
            let x0 = (tx as u64 * src.width as u64 / tw as u64) as u32;
            let x1 = (((tx as u64 + 1) * src.width as u64 / tw as u64) as u32)
                .max(x0 + 1)
                .min(src.width);
            let mut acc = [0u32; 4];
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = src.pixel(x, y);
                    for c in 0..4 {
                        acc[c] += p[c] as u32;
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)).max(1);
            out.set_pixel(
                tx,
                ty,
                [
                    (acc[0] / n) as u8,
                    (acc[1] / n) as u8,
                    (acc[2] / n) as u8,
                    (acc[3] / n) as u8,
                ],
            );
        }
    }
    out
}

// 3x5 digit font, rows top to bottom, bit 2 = left pixel.
const DIGITS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111],
    [0b010, 0b110, 0b010, 0b010, 0b111],
    [0b111, 0b001, 0b111, 0b100, 0b111],
    [0b111, 0b001, 0b111, 0b001, 0b111],
    [0b101, 0b101, 0b111, 0b001, 0b001],
    [0b111, 0b100, 0b111, 0b001, 0b111],
    [0b111, 0b100, 0b111, 0b101, 0b111],
    [0b111, 0b001, 0b001, 0b001, 0b001],
    [0b111, 0b101, 0b111, 0b101, 0b111],
    [0b111, 0b101, 0b111, 0b001, 0b111],
];

/// Draw a decimal number with a dark backing box at (x0,y0), pixel scale `s`.
pub(crate) fn draw_number(img: &mut Image, x0: u32, y0: u32, n: u32, s: u32) {
    let text = n.to_string();
    let bw = text.len() as u32 * 4 * s + s;
    let bh = 7 * s;
    for y in 0..bh {
        for x in 0..bw {
            if x0 + x < img.width && y0 + y < img.height {
                img.set_pixel(x0 + x, y0 + y, [0, 0, 0, 255]);
            }
        }
    }
    for (i, ch) in text.bytes().enumerate() {
        let glyph = &DIGITS[(ch - b'0') as usize];
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..3u32 {
                if bits & (1 << (2 - col)) != 0 {
                    for dy in 0..s {
                        for dx in 0..s {
                            let px = x0 + s + (i as u32 * 4 + col) * s + dx;
                            let py = y0 + s + row as u32 * s + dy;
                            if px < img.width && py < img.height {
                                img.set_pixel(px, py, [255, 255, 255, 255]);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Grid of labelled thumbnails. `items` = (frame number, image); every cell is `thumb_w` wide
/// (height from the first image's aspect ratio); `cols` columns; 2px gutters.
pub fn contact_sheet(items: &[(u32, Image)], cols: u32, thumb_w: u32) -> Image {
    const GUT: u32 = 2;
    if items.is_empty() {
        return Image::solid(1, 1, [0, 0, 0, 255]);
    }
    let cols = cols.clamp(1, items.len() as u32);
    let rows = (items.len() as u32).div_ceil(cols);
    let f = &items[0].1;
    let tw = thumb_w.clamp(1, f.width.max(1));
    let th = ((f.height as u64 * tw as u64 / f.width.max(1) as u64) as u32).max(1);
    let mut sheet = Image::solid(
        cols * (tw + GUT) + GUT,
        rows * (th + GUT) + GUT,
        [24, 24, 24, 255],
    );
    for (i, (frame, img)) in items.iter().enumerate() {
        let (cx, cy) = (i as u32 % cols, i as u32 / cols);
        let mut t = resize_box(img, tw, th);
        if t.width != tw || t.height != th {
            // differing source size: letterbox into the cell
            let mut cell = Image::solid(tw, th, [0, 0, 0, 255]);
            for y in 0..t.height.min(th) {
                for x in 0..t.width.min(tw) {
                    cell.set_pixel(x, y, t.pixel(x, y));
                }
            }
            t = cell;
        }
        let scale = if tw >= 240 { 2 } else { 1 };
        draw_number(&mut t, 0, 0, *frame, scale);
        let (ox, oy) = (GUT + cx * (tw + GUT), GUT + cy * (th + GUT));
        for y in 0..th {
            for x in 0..tw {
                sheet.set_pixel(ox + x, oy + y, t.pixel(x, y));
            }
        }
    }
    sheet
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng_image(w: u32, h: u32, mut s: u64) -> Image {
        let mut rgba = Vec::new();
        for _ in 0..w * h {
            for _ in 0..3 {
                s = s
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                rgba.push((s >> 56) as u8);
            }
            rgba.push(255);
        }
        Image::new(w, h, rgba)
    }

    #[test]
    fn psnr_analytic_uniform_offset() {
        // every channel differs by 10 -> MSE 100 -> PSNR = 10 log10(65025/100)
        let a = Image::solid(16, 16, [100, 100, 100, 255]);
        let b = Image::solid(16, 16, [110, 110, 110, 255]);
        let m = compare_images(&a, &b, MetricOpts::default()).unwrap();
        let expect = 10.0 * (65025.0f64 / 100.0).log10();
        assert!((m.psnr - expect).abs() < 1e-12, "{} vs {expect}", m.psnr);
        assert_eq!(m.max_abs, [10, 10, 10]);
        assert!((m.mean_abs - 10.0).abs() < 1e-12);
        assert!((m.rmse - 10.0).abs() < 1e-12);
        assert_eq!(m.frac_gt2, 1.0);
        assert_eq!(m.frac_gt8, 1.0);
        assert_eq!(m.frac_gt32, 0.0);
    }

    #[test]
    fn psnr_analytic_single_channel() {
        // only B differs by 16 everywhere: MSE = 256/3
        let a = Image::solid(8, 8, [0, 0, 0, 255]);
        let b = Image::solid(8, 8, [0, 0, 16, 255]);
        let m = compare_images(&a, &b, MetricOpts::default()).unwrap();
        let expect = 10.0 * (65025.0f64 / (256.0 / 3.0)).log10();
        assert!((m.psnr - expect).abs() < 1e-12);
        assert_eq!(m.max_abs, [0, 0, 16]);
        assert!((m.mean_abs - 16.0 / 3.0).abs() < 1e-12);
        assert_eq!(m.frac_gt8, 1.0);
        assert_eq!(m.frac_gt32, 0.0);
    }

    #[test]
    fn partial_fractions_and_alpha_ignored() {
        // 4x1 image: diffs 0, 3, 9, 40 on R; alpha differs everywhere (ignored by default)
        let a = Image::new(
            4,
            1,
            vec![10, 0, 0, 0, 10, 0, 0, 0, 10, 0, 0, 0, 10, 0, 0, 0],
        );
        let b = Image::new(
            4,
            1,
            vec![10, 0, 0, 255, 13, 0, 0, 255, 19, 0, 0, 255, 50, 0, 0, 255],
        );
        let m = compare_images(&a, &b, MetricOpts::default()).unwrap();
        assert_eq!(m.max_abs, [40, 0, 0]);
        assert_eq!(m.max_abs_alpha, 255);
        assert!((m.frac_gt2 - 0.75).abs() < 1e-12);
        assert!((m.frac_gt8 - 0.5).abs() < 1e-12);
        assert!((m.frac_gt32 - 0.25).abs() < 1e-12);
        let mse = (9.0 + 81.0 + 1600.0) / 12.0;
        assert!((m.psnr - psnr_from_mse(mse)).abs() < 1e-12);
        let ma = compare_images(
            &a,
            &b,
            MetricOpts {
                include_alpha: true,
                ssim: true,
            },
        )
        .unwrap();
        assert!(ma.psnr < m.psnr);
        assert_eq!(ma.frac_gt32, 1.0);
    }

    #[test]
    fn identical_is_inf_and_ssim_one() {
        let a = rng_image(40, 30, 7);
        let m = compare_images(&a, &a.clone(), MetricOpts::default()).unwrap();
        assert_eq!(m.psnr, f64::INFINITY);
        assert_eq!(m.ssim, 1.0);
        assert!(m.identical());
        // the SSIM kernel itself (not the shortcut) must also give 1
        assert!((ssim_luma(&a, &a) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn ssim_constant_images_closed_form() {
        // Zero variance everywhere: SSIM = (2 m1 m2 + C1) / (m1^2 + m2^2 + C1)
        let a = Image::solid(32, 32, [100, 100, 100, 255]);
        let b = Image::solid(32, 32, [120, 120, 120, 255]);
        let c1 = (0.01f64 * 255.0).powi(2);
        let expect = (2.0 * 100.0 * 120.0 + c1) / (100.0f64.powi(2) + 120.0f64.powi(2) + c1);
        assert!((expect - 0.983_610_925).abs() < 1e-8, "{expect}");
        let s = ssim_luma(&a, &b);
        assert!((s - expect).abs() < 1e-9, "{s} vs {expect}");
    }

    #[test]
    fn ssim_matches_numpy_scipy_reference() {
        // Reference value computed independently with numpy/scipy (valid-mode 11x11 Gaussian,
        // sigma 1.5, K1=0.01, K2=0.03, L=255):
        //   a[y,x] = (6x+4y) % 256 ; b[y,x] = (5x+9y) % 200 + 20 ; 40x30 gray images
        let (w, h) = (40u32, 30u32);
        let mk = |f: &dyn Fn(u32, u32) -> u32| {
            let mut img = Image::solid(w, h, [0, 0, 0, 255]);
            for y in 0..h {
                for x in 0..w {
                    let g = f(x, y) as u8;
                    img.set_pixel(x, y, [g, g, g, 255]);
                }
            }
            img
        };
        let a = mk(&|x, y| (6 * x + 4 * y) % 256);
        let b = mk(&|x, y| (5 * x + 9 * y) % 200 + 20);
        let s = ssim_luma(&a, &b);
        assert!((s - 0.252_399_001_067_414_75).abs() < 1e-9, "{s}");
        assert!((ssim_luma(&b, &a) - s).abs() < 1e-12, "symmetric");
    }

    #[test]
    fn ssim_drops_for_uncorrelated_noise() {
        let a = rng_image(48, 48, 1);
        let b = rng_image(48, 48, 2);
        let s = ssim_luma(&a, &b);
        assert!(s.abs() < 0.2, "{s}");
        // small perturbation stays high
        let mut c = a.clone();
        for p in c.rgba.chunks_exact_mut(4) {
            p[0] = p[0].saturating_add(1);
        }
        assert!(ssim_luma(&a, &c) > 0.99);
    }

    #[test]
    fn size_mismatch_is_error() {
        let a = Image::solid(4, 4, [0; 4]);
        let b = Image::solid(4, 5, [0; 4]);
        assert!(compare_images(&a, &b, MetricOpts::default()).is_err());
    }

    #[test]
    fn heatmap_and_triptych_shapes() {
        let a = Image::solid(10, 6, [10, 10, 10, 255]);
        let mut b = a.clone();
        b.set_pixel(3, 2, [20, 10, 10, 255]);
        let h = diff_heatmap(&a, &b);
        assert_eq!(h.pixel(0, 0), [0, 0, 0, 255]);
        assert_ne!(h.pixel(3, 2), [0, 0, 0, 255]);
        let t = triptych(&a, &b, 8);
        assert_eq!((t.width, t.height), (10 * 3 + 8, 6));
        assert_eq!(t.pixel(2 * 14 + 3, 2), [80, 0, 0, 255]);
        assert_eq!(t.pixel(2 * 14 + 4, 2), [0, 0, 0, 255]);
    }

    #[test]
    fn contact_sheet_layout() {
        let items: Vec<(u32, Image)> = (0..5)
            .map(|i| (i * 10, Image::solid(64, 32, [i as u8 * 40, 0, 0, 255])))
            .collect();
        let s = contact_sheet(&items, 3, 32);
        assert_eq!(s.width, 3 * 34 + 2);
        assert_eq!(s.height, 2 * (16 + 2) + 2);
        let g = resize_box(&Image::solid(64, 32, [9, 9, 9, 255]), 16, 8);
        assert_eq!(g.pixel(5, 5), [9, 9, 9, 255]);
    }
}
