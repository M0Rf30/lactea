//! Audio file decoding with symphonia (wav, flac, mp3, ogg/vorbis, aac/m4a, alac, mkv, ...).
//! Output is always interleaved stereo f32.

use std::fs::File;
use std::path::{Path, PathBuf};

use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::{AudioDecoder as SymDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use super::to_stereo;

pub struct AudioDecoder {
    path: PathBuf,
    format: Box<dyn FormatReader>,
    decoder: Box<dyn SymDecoder>,
    track_id: u32,
    sample_rate: u32,
    channels: usize,
    total_frames: Option<u64>,
    scratch: Vec<f32>,
}

impl AudioDecoder {
    pub fn open(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let format = symphonia::default::get_probe()
            .probe(
                &hint,
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .map_err(|e| format!("{}: unsupported or corrupt file ({e})", path.display()))?;
        let track = format
            .default_track(TrackType::Audio)
            .ok_or_else(|| format!("{}: no decodable audio track", path.display()))?;
        let track_id = track.id;
        let total_frames = track.num_frames;
        let audio = match track.codec_params.as_ref() {
            Some(CodecParameters::Audio(audio)) => audio,
            _ => return Err(format!("{}: no audio codec parameters", path.display())),
        };
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(audio, &AudioDecoderOptions::default())
            .map_err(|e| format!("{}: no decoder ({e})", path.display()))?;
        // The decoder may output another rate than the container declares (HE-AAC).
        let sample_rate = decoder
            .codec_params()
            .sample_rate
            .or(audio.sample_rate)
            .ok_or_else(|| format!("{}: unknown sample rate", path.display()))?;
        let channels = audio
            .channels
            .as_ref()
            .map(|c| c.count())
            .unwrap_or(2)
            .max(1);
        Ok(Self {
            path: path.to_path_buf(),
            format,
            decoder,
            track_id,
            sample_rate,
            channels,
            total_frames,
            scratch: Vec::new(),
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn total_frames(&self) -> Option<u64> {
        self.total_frames
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Decode the next packet and append it as interleaved stereo. `Ok(false)` at end of stream.
    pub fn next_packet(&mut self, out: &mut Vec<f32>) -> Result<bool, String> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Ok(false),
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(false);
                }
                Err(SymError::ResetRequired) => return Ok(false),
                Err(e) => return Err(format!("{}: read error: {e}", self.path.display())),
            };
            if packet.track_id != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    self.channels = decoded.spec().channels().count().max(1);
                    decoded.copy_to_vec_interleaved(&mut self.scratch);
                    to_stereo(&self.scratch, self.channels, out);
                    return Ok(true);
                }
                // A corrupt packet is skipped, like every player does.
                Err(SymError::DecodeError(e)) => {
                    log::warn!("{}: skipping undecodable packet: {e}", self.path.display());
                }
                Err(e) => return Err(format!("{}: decode error: {e}", self.path.display())),
            }
        }
    }

    /// Decode the whole file into memory (interleaved stereo).
    pub fn decode_all(mut self) -> Result<(Vec<f32>, u32), String> {
        let mut out = Vec::new();
        while self.next_packet(&mut out)? {}
        Ok((out, self.sample_rate))
    }
}

/// Decode a whole file: `(interleaved stereo f32, sample_rate)`.
pub fn decode_file(path: &Path) -> Result<(Vec<f32>, u32), String> {
    AudioDecoder::open(path)?.decode_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn write_wav_f32(path: &Path, rate: u32, channels: u16, frames: &[Vec<f32>]) {
        let n = frames.len() as u32;
        let data_len = n * channels as u32 * 4;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data_len).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
        b.extend_from_slice(&channels.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * channels as u32 * 4).to_le_bytes());
        b.extend_from_slice(&(channels * 4).to_le_bytes());
        b.extend_from_slice(&32u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data_len.to_le_bytes());
        for f in frames {
            for s in f {
                b.extend_from_slice(&s.to_le_bytes());
            }
        }
        std::fs::write(path, b).unwrap();
    }

    #[test]
    fn decodes_stereo_and_mono_wav() {
        let dir = std::env::temp_dir().join(format!("lactea-dec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let st = dir.join("st.wav");
        let frames: Vec<Vec<f32>> = (0..1000)
            .map(|i| vec![i as f32 / 1000.0, -(i as f32) / 1000.0])
            .collect();
        write_wav_f32(&st, 44_100, 2, &frames);
        let (pcm, sr) = decode_file(&st).unwrap();
        assert_eq!(sr, 44_100);
        assert_eq!(pcm.len(), 2000);
        assert_eq!(pcm[2 * 500], 0.5);
        assert_eq!(pcm[2 * 500 + 1], -0.5);

        let mo = dir.join("mo.wav");
        let frames: Vec<Vec<f32>> = (0..100).map(|i| vec![i as f32 / 100.0]).collect();
        write_wav_f32(&mo, 22_050, 1, &frames);
        let (pcm, sr) = decode_file(&mo).unwrap();
        assert_eq!(sr, 22_050);
        assert_eq!(pcm.len(), 200);
        assert_eq!(pcm[2 * 50], 0.5);
        assert_eq!(pcm[2 * 50 + 1], 0.5);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_garbage() {
        let p = std::env::temp_dir().join(format!("lactea-garbage-{}.wav", std::process::id()));
        std::fs::write(&p, b"this is not audio at all, really").unwrap();
        assert!(AudioDecoder::open(&p).is_err());
        let _ = std::fs::remove_file(p);
        assert!(AudioDecoder::open(Path::new("/nonexistent/file.mp3")).is_err());
    }

    #[test]
    fn repo_fixture_decodes_to_expected_length() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/audio/synth128.wav");
        if !p.exists() {
            return;
        }
        let (pcm, sr) = decode_file(&p).unwrap();
        assert_eq!(sr, 44_100);
        assert_eq!(pcm.len(), 2 * 20 * 44_100);
    }
}
