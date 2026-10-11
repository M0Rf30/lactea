//! `decode IN.jpg OUT.bin`: writes `i32 width, i32 height` (little endian) followed by the RGBA8 pixels, the
//! layout of the stb_image reference tool that `tests/stb_reference.rs` is pinned against.
use std::process::exit;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let [_, input, output] = a.as_slice() else {
        eprintln!("usage: decode IN.jpg OUT.bin");
        exit(2);
    };
    let data = std::fs::read(input).unwrap_or_else(|e| {
        eprintln!("{input}: {e}");
        exit(1);
    });
    let img = lactea_jpeg::decode_rgba(&data).unwrap_or_else(|e| {
        eprintln!("{input}: {e}");
        exit(1);
    });
    let mut out = Vec::with_capacity(img.rgba.len() + 8);
    out.extend_from_slice(&(img.width as i32).to_le_bytes());
    out.extend_from_slice(&(img.height as i32).to_le_bytes());
    out.extend_from_slice(&img.rgba);
    if let Err(e) = std::fs::write(output, out) {
        eprintln!("{output}: {e}");
        exit(1);
    }
}
