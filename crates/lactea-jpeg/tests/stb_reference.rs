//! The decoder must reproduce `stbi_load(.., 4)` (stb_image 2.30, SSE2 kernels) byte for byte.
//!
//! The fixtures were encoded with libjpeg (Pillow) from synthetic gradients with noise, a saturated stripe and a
//! hard vertical step, in the layouts projectM texture packs use (4:2:0, 4:2:2, 4:4:4, grayscale, restart
//! intervals) plus progressive scans and degenerate sizes. The expected values are the FNV-1a 64 hash of the RGBA8
//! output of a C program that includes the `stb_image.h` vendored by projectM and calls `stbi_load(path, .., 4)`
//! (see `docs/render.md`, "User textures").
//!
//! `FD_JPEG_FUZZ=<n>` (default 60) sets how many mutated streams the robustness test feeds the decoder.

use lactea_jpeg::{decode_rgba, is_jpeg};

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

const CASES: &[(&str, u32, u32, u64)] = &[
    ("base_420_33x17", 33, 17, 0xb0b8_fb1d_91c8_404c),
    ("base_420_restart_64x48", 64, 48, 0xadb8_6c4f_281e_0e41),
    ("base_422_40x24", 40, 24, 0x7799_82e9_82c7_e1e8),
    ("base_444_21x35", 21, 35, 0x447a_0556_11ef_1ca3),
    ("gray_19x13", 19, 13, 0xc809_d1ff_e51d_e25a),
    ("prog_420_50x30", 50, 30, 0xf848_167b_0a36_43f2),
    ("prog_444_17x17", 17, 17, 0xdf53_c6b4_45e3_09ac),
    ("tiny_1x1", 1, 1, 0xd789_707b_f56e_11d6),
    ("tiny_9x2_420", 9, 2, 0x93f3_af95_0b61_c55d),
];

fn fixture(name: &str) -> Vec<u8> {
    let p = format!("{}/tests/fixtures/{name}.jpg", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

#[test]
fn decodes_exactly_like_stb_image() {
    for &(name, w, h, hash) in CASES {
        let data = fixture(name);
        assert!(is_jpeg(&data), "{name}");
        let img = decode_rgba(&data).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!((img.width, img.height), (w, h), "{name}");
        assert_eq!(img.rgba.len(), (w * h * 4) as usize, "{name}");
        assert!(
            img.rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 255),
            "{name}: alpha"
        );
        assert_eq!(
            fnv1a64(&img.rgba),
            hash,
            "{name}: differs from stb_image output"
        );
    }
}

/// Corrupt or truncated streams must fail or decode to *something*, never panic.
#[test]
fn damaged_streams_do_not_panic() {
    for &(name, ..) in CASES {
        let data = fixture(name);
        for cut in [
            0,
            1,
            2,
            5,
            20,
            data.len() / 3,
            data.len() / 2,
            data.len() - 1,
        ] {
            let _ = decode_rgba(&data[..cut.min(data.len())]);
        }
        let mut state = 0x1234_5678_9abc_def0u64 ^ name.len() as u64;
        // FD_JPEG_FUZZ=N for a longer run
        let iters: usize = std::env::var("FD_JPEG_FUZZ")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60);
        for _ in 0..iters {
            let mut d = data.clone();
            for _ in 0..3 {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let i = (state >> 33) as usize % d.len();
                d[i] ^= 1 << ((state >> 20) & 7);
            }
            let _ = decode_rgba(&d);
        }
    }
    assert!(decode_rgba(b"not a jpeg").is_err());
    assert!(!is_jpeg(b"\x89PNG"));
}

/// SOI, a frame header (`sof` = 0xC0 baseline / 0xC2 progressive) with `comps` components of 1x1 sampling, EOI.
fn header_only(sof: u8, width: u16, height: u16, comps: u8) -> Vec<u8> {
    let mut d = vec![0xff, 0xd8, 0xff, sof];
    d.extend_from_slice(&(8 + 3 * u16::from(comps)).to_be_bytes());
    d.push(8);
    d.extend_from_slice(&height.to_be_bytes());
    d.extend_from_slice(&width.to_be_bytes());
    d.push(comps);
    for id in 1..=comps {
        d.extend_from_slice(&[id, 0x11, 0]);
    }
    d.extend_from_slice(&[0xff, 0xd9]);
    d
}

/// A tiny header must not make the decoder allocate planes, coefficients and output of gigabytes.
#[test]
fn declared_size_beyond_the_memory_budget_is_rejected() {
    // output alone is exactly 1 GiB (accepted by the old check); with three progressive planes it is ~3.5 GiB
    let e = decode_rgba(&header_only(0xc2, 16384, 16384, 3)).unwrap_err();
    assert_eq!(e.reason(), "image too large");
}

#[test]
fn a_frame_without_any_scan_is_rejected() {
    let e = decode_rgba(&header_only(0xc0, 8, 8, 1)).unwrap_err();
    assert_eq!(e.reason(), "no SOS");
}
