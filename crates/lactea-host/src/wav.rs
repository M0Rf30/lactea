//! Minimal WAV reader (float32 / int16 PCM, mono duplicated to stereo): same rules as the oracle's
//! `refrender`.

use std::path::Path;

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

/// Read the file at `path`; see [`parse_wav`].
pub fn load_wav(path: &Path) -> Result<Audio, String> {
    let buf =
        std::fs::read(path).map_err(|e| format!("cannot open audio {}: {e}", path.display()))?;
    parse_wav(&buf)
}

/// Decode a RIFF/WAVE image: float32 or int16 PCM (also inside `WAVE_FORMAT_EXTENSIBLE`).
pub fn parse_wav(buf: &[u8]) -> Result<Audio, String> {
    if buf.len() < 12 || &buf[..4] != b"RIFF" || &buf[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".into());
    }
    let (mut fmt, mut ch, mut bits, mut rate) = (0u16, 0u16, 0u16, 0u32);
    let mut data: Option<&[u8]> = None;
    let mut p = 12usize;
    while p + 8 <= buf.len() {
        let cl = rd32(&buf[p + 4..]) as usize;
        let c = &buf[p + 8..];
        match &buf[p..p + 4] {
            b"fmt " => {
                // channels, rate and bits live in the first 16 bytes of the chunk
                if cl < 16 || c.len() < 16 {
                    return Err("WAV: fmt chunk shorter than 16 bytes".into());
                }
                fmt = rd16(c);
                ch = rd16(&c[2..]);
                rate = rd32(&c[4..]);
                bits = rd16(&c[14..]);
                if fmt == 0xFFFE {
                    // the sub-format GUID starts at byte 24 of the extensible chunk
                    if cl < 26 || c.len() < 26 {
                        return Err("WAV: extensible fmt chunk shorter than 26 bytes".into());
                    }
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
        return Err("WAV: missing fmt/data".into());
    };
    if ch < 1 {
        return Err("WAV: no channels".into());
    }
    if !((fmt == 3 && bits == 32) || (fmt == 1 && bits == 16)) {
        return Err("WAV: only float32 and int16 PCM supported".into());
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

#[cfg(test)]
mod tests {
    use super::*;

    fn wav_with_fmt(fmt: &[u8]) -> Vec<u8> {
        let mut b = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        b.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
        b.extend_from_slice(fmt);
        b
    }

    #[test]
    fn short_fmt_chunk_is_an_error_not_a_panic() {
        for len in [0usize, 2, 15] {
            let e = parse_wav(&wav_with_fmt(&vec![0u8; len]))
                .err()
                .expect("error");
            assert!(e.contains("fmt"), "{e}");
        }
        // extensible marker without the sub-format bytes
        let mut fmt = vec![0u8; 16];
        fmt[..2].copy_from_slice(&0xFFFEu16.to_le_bytes());
        let e = parse_wav(&wav_with_fmt(&fmt)).err().expect("error");
        assert!(e.contains("extensible"), "{e}");
    }

    #[test]
    fn int16_mono_is_duplicated_to_stereo() {
        let mut fmt = Vec::new();
        for v in [1u16, 1] {
            fmt.extend_from_slice(&v.to_le_bytes());
        }
        fmt.extend_from_slice(&8000u32.to_le_bytes());
        fmt.extend_from_slice(&16000u32.to_le_bytes());
        fmt.extend_from_slice(&2u16.to_le_bytes());
        fmt.extend_from_slice(&16u16.to_le_bytes());
        let mut b = wav_with_fmt(&fmt);
        b.extend_from_slice(b"data\x04\0\0\0");
        b.extend_from_slice(&16384i16.to_le_bytes());
        b.extend_from_slice(&(-32768i16).to_le_bytes());
        let a = parse_wav(&b).unwrap();
        assert_eq!((a.frames, a.rate), (2, 8000));
        assert_eq!(a.data, vec![0.5, 0.5, -1.0, -1.0]);
    }
}
