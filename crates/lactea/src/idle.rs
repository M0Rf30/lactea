//! The idle preset: what an engine shows when the host has not loaded anything yet.
//!
//! projectM loads its built-in idle preset (`idle://`, the "M" logo with headphones) whenever no
//! preset is active. lactea does not render anything until the host loads a preset (an engine with no
//! preset draws black), so hosts that want the same "something is alive" behaviour call
//! [`Engine::load_idle_preset`]. The preset here is original (a slow feedback swirl with a breathing
//! ring and an audio-reactive line), not a copy of projectM's: it needs no textures and no shaders, so
//! it loads on every backend and is bit-for-bit deterministic like any other preset.

use crate::{Engine, EngineError, PresetReport};

/// Display name the idle preset is loaded under (`Engine::preset_name`).
pub const IDLE_PRESET_NAME: &str = "idle://lactea";

/// `.milk` source of the idle preset.
pub const IDLE_PRESET: &str = "[preset00]
fRating=2.000000
fGammaAdj=1.300000
fDecay=0.960000
fVideoEchoZoom=1.020000
fVideoEchoAlpha=0.150000
nVideoEchoOrientation=1
nWaveMode=6
bAdditiveWaves=1
bWaveDots=0
bWaveThick=1
bModWaveAlphaByVolume=0
bMaximizeWaveColor=1
bTexWrap=1
bDarkenCenter=0
bRedBlueStereo=0
bBrighten=0
bDarken=0
bSolarize=0
bInvert=0
fWaveAlpha=1.500000
fWaveScale=1.200000
fWaveSmoothing=0.750000
fWaveParam=0.000000
fModWaveAlphaStart=0.750000
fModWaveAlphaEnd=0.950000
fWarpAnimSpeed=1.000000
fWarpScale=1.300000
fZoomExponent=1.000000
fShader=0.000000
zoom=1.006000
rot=0.012000
cx=0.500000
cy=0.500000
dx=0.000000
dy=0.000000
warp=0.250000
sx=1.000000
sy=1.000000
wave_r=0.700000
wave_g=0.800000
wave_b=1.000000
wave_x=0.500000
wave_y=0.500000
ob_size=0.005000
ob_r=0.100000
ob_g=0.100000
ob_b=0.300000
ob_a=1.000000
ib_size=0.000000
ib_r=0.000000
ib_g=0.000000
ib_b=0.000000
ib_a=0.000000
nMotionVectorsX=0.000000
nMotionVectorsY=0.000000
mv_dx=0.000000
mv_dy=0.000000
mv_l=0.000000
mv_r=1.000000
mv_g=1.000000
mv_b=1.000000
mv_a=0.000000
shapecode_0_enabled=1
shapecode_0_sides=48
shapecode_0_additive=1
shapecode_0_thickOutline=1
shapecode_0_textured=0
shapecode_0_num_inst=1
shapecode_0_x=0.500000
shapecode_0_y=0.500000
shapecode_0_rad=0.200000
shapecode_0_ang=0.000000
shapecode_0_tex_ang=0.000000
shapecode_0_tex_zoom=1.000000
shapecode_0_r=0.000000
shapecode_0_g=0.000000
shapecode_0_b=0.000000
shapecode_0_a=0.000000
shapecode_0_r2=0.000000
shapecode_0_g2=0.000000
shapecode_0_b2=0.000000
shapecode_0_a2=0.000000
shapecode_0_border_r=0.800000
shapecode_0_border_g=0.900000
shapecode_0_border_b=1.000000
shapecode_0_border_a=0.800000
shape_0_per_frame1=rad = 0.18 + 0.04 * sin(time * 1.3) + 0.05 * bass_att;
shape_0_per_frame2=border_r = 0.5 + 0.5 * sin(time * 0.6);
shape_0_per_frame3=border_g = 0.5 + 0.5 * sin(time * 0.8 + 2.0);
shape_0_per_frame4=border_b = 0.5 + 0.5 * sin(time * 1.0 + 4.0);
per_frame_1=wave_r = 0.6 + 0.4 * sin(time * 0.7);
per_frame_2=wave_g = 0.6 + 0.4 * sin(time * 0.9 + 2.1);
per_frame_3=wave_b = 0.6 + 0.4 * sin(time * 1.1 + 4.2);
per_frame_4=rot = 0.02 * sin(time * 0.23);
per_frame_5=zoom = 1.004 + 0.008 * bass_att;
per_pixel_1=zoom = zoom + 0.012 * rad * sin(time * 0.5);
";

impl Engine {
    /// Loads [`IDLE_PRESET`] (a hard cut, like any [`Engine::load_preset`]).
    pub fn load_idle_preset(&mut self) -> Result<PresetReport, EngineError> {
        self.load_preset(
            IDLE_PRESET.as_bytes(),
            IDLE_PRESET_NAME,
            crate::Transition::Cut,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lactea_preset::Preset;

    #[test]
    fn idle_preset_parses_with_its_code_and_shape() {
        let p = Preset::parse(IDLE_PRESET.as_bytes()).expect("parse");
        assert_eq!(p.shapes[0].enabled, 1);
        assert_eq!(p.base.wave_mode, 6);
        assert!(!p.per_frame.is_empty() && !p.per_pixel.is_empty());
        assert!(
            p.warp_shader.is_none() && p.comp_shader.is_none(),
            "no shaders: loads everywhere"
        );
    }
}
