//! Fixed-step audio feed for reproducible runs (`--audio-wav F --deterministic`).
//!
//! Frame `n` is rendered at time `n / fps` and receives exactly the PCM sample frames
//! `lactea_abi::pcm_range(n, sample_rate, fps)` — the rule the oracle and the headless
//! renderer use — so a run is a pure function of (wav, fps, preset, seed).

use std::path::Path;

use super::decode::decode_file;

pub struct WavFeed {
    /// Interleaved stereo.
    pcm: Vec<f32>,
    sample_rate: u32,
    fps: u32,
}

impl WavFeed {
    pub fn load(path: &Path, fps: u32) -> Result<Self, String> {
        let (pcm, sample_rate) = decode_file(path)?;
        Ok(Self::from_pcm(pcm, sample_rate, fps))
    }

    pub fn from_pcm(pcm: Vec<f32>, sample_rate: u32, fps: u32) -> Self {
        Self {
            pcm,
            sample_rate,
            fps: fps.max(1),
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn fps(&self) -> u32 {
        self.fps
    }

    pub fn sample_frames(&self) -> u64 {
        (self.pcm.len() / 2) as u64
    }

    /// Number of video frames the whole file covers at `fps`.
    pub fn video_frames(&self) -> u64 {
        self.sample_frames() * self.fps as u64 / self.sample_rate as u64
    }

    /// PCM (interleaved stereo) to push before rendering frame `n`; empty after the end.
    pub fn frame_pcm(&self, n: u64) -> &[f32] {
        let (a, b) = lactea_abi::pcm_range(n, self.sample_rate as u64, self.fps as u64);
        let total = self.sample_frames();
        let (a, b) = (a.min(total), b.min(total));
        &self.pcm[(a * 2) as usize..(b * 2) as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slices_follow_pcm_range_and_tile_the_stream() {
        // 44100 Hz @ 60 fps: 735 sample frames per video frame.
        let pcm: Vec<f32> = (0..2 * 44_100).map(|i| i as f32).collect();
        let feed = WavFeed::from_pcm(pcm.clone(), 44_100, 60);
        assert_eq!(feed.frame_pcm(0).len(), 2 * 735);
        assert_eq!(feed.frame_pcm(1)[0], (2 * 735) as f32);
        let mut all = Vec::new();
        for n in 0..feed.video_frames() {
            all.extend_from_slice(feed.frame_pcm(n));
        }
        assert_eq!(all, pcm);
        assert!(feed.frame_pcm(feed.video_frames() + 10).is_empty());
    }

    #[test]
    fn non_integer_ratio_is_gapless() {
        // 44100 / 24 = 1837.5 -> alternating 1837 / 1838 sample frames, never a gap.
        let pcm: Vec<f32> = (0..2 * 44_100).map(|i| i as f32).collect();
        let feed = WavFeed::from_pcm(pcm.clone(), 44_100, 24);
        let mut all = Vec::new();
        for n in 0..24 {
            all.extend_from_slice(feed.frame_pcm(n));
        }
        assert_eq!(all, pcm);
    }

    #[test]
    fn loads_repo_fixture() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/audio/synth128.wav");
        if !p.exists() {
            return;
        }
        let feed = WavFeed::load(&p, 60).unwrap();
        assert_eq!(feed.sample_rate(), 44_100);
        assert_eq!(feed.video_frames(), 20 * 60);
    }
}
