//! Minimal WAV reader (float32 / int16 PCM, mono duplicated to stereo) — same rules as the
//! oracle's `refrender`.

use anyhow::{Context, Result, bail};

/// Interleaved stereo float audio.
pub struct Audio {
    pub data: Vec<f32>,
    pub frames: u64,
    pub rate: u32,
}

fn rd32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}
fn rd16(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}

pub fn load_wav(path: &std::path::Path) -> Result<Audio> {
    let buf =
        std::fs::read(path).with_context(|| format!("cannot open audio {}", path.display()))?;
    if buf.len() < 12 || &buf[..4] != b"RIFF" || &buf[8..12] != b"WAVE" {
        bail!("not a RIFF/WAVE file");
    }
    let (mut fmt, mut ch, mut bits, mut rate) = (0u16, 0u16, 0u16, 0u32);
    let mut data: Option<&[u8]> = None;
    let mut p = 12usize;
    while p + 8 <= buf.len() {
        let cl = rd32(&buf[p + 4..]) as usize;
        let c = &buf[p + 8..];
        match &buf[p..p + 4] {
            b"fmt " => {
                fmt = rd16(c);
                ch = rd16(&c[2..]);
                rate = rd32(&c[4..]);
                bits = rd16(&c[14..]);
                if fmt == 0xFFFE && cl >= 26 {
                    fmt = rd16(&c[24..]);
                }
            }
            b"data" => {
                data = Some(&c[..cl.min(c.len())]);
                break;
            }
            _ => {}
        }
        p += 8 + cl + (cl & 1);
    }
    let Some(data) = data else {
        bail!("WAV: missing fmt/data");
    };
    if ch < 1 {
        bail!("WAV: no channels");
    }
    if !((fmt == 3 && bits == 32) || (fmt == 1 && bits == 16)) {
        bail!("WAV: only float32 and int16 PCM supported");
    }
    let bytes = (bits / 8) as usize;
    let n = data.len() / (ch as usize * bytes);
    let mut out = Vec::with_capacity(n * 2);
    for i in 0..n {
        for c in 0..2usize {
            let cc = if c < ch as usize { c } else { 0 };
            let q = &data[(i * ch as usize + cc) * bytes..];
            out.push(if fmt == 3 {
                f32::from_bits(rd32(q))
            } else {
                f32::from(rd16(q) as i16) / 32768.0
            });
        }
    }
    Ok(Audio {
        data: out,
        frames: n as u64,
        rate,
    })
}
