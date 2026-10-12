//! `.wave` / `.shape` import and export on the loaded preset (MD3 3.37, docs/md3.md
//! "`.wave` / `.shape` files and the MD3 `.milk` writer").
//!
//! Import edits the source [`Preset`] of the preset on screen and rebuilds its whole runtime
//! (expression contexts, init code, q/t, waves, shapes, shaders), which is what MD3 does after an
//! import. It is not a preset change: no soft cut, the clock keeps running, and the preset's own
//! renderer keeps its feedback image, so the picture is not blanked.

use super::{Engine, LastFrame, Slot};
use crate::{EngineError, PresetReport};
use lactea_preset::{
    ParseError, Preset, export_shape_file, export_wave_file, import_shape_file, import_wave_file,
};

impl Engine {
    /// Import a `.wave` file into custom-wave slot `slot` of the loaded preset and recompile the
    /// preset (init code re-runs, q/t reset). MD3 sends a dropped `.wave` file to slot 8.
    ///
    /// Keys missing from the file keep the slot's values; its code is replaced
    /// (see [`lactea_preset::import_wave_file`]). The target is the incoming preset during a soft
    /// cut, else the active one. On error (no preset loaded, bad slot, unparsable input, code that
    /// does not compile) the preset is left as it was.
    ///
    /// The rebuild goes through the regular load path with the modified preset serialised by the
    /// lossless [`Preset::to_milk`]; compile or shader problems are reported in the returned
    /// [`PresetReport`] exactly like [`Engine::load_preset`].
    pub fn import_custom_wave(
        &mut self,
        slot: usize,
        bytes: &[u8],
    ) -> Result<PresetReport, EngineError> {
        self.import_custom(|p| import_wave_file(p, slot, bytes))
    }

    /// Import a `.shape` file into custom-shape slot `slot`; see [`Engine::import_custom_wave`].
    /// MD3 sends a dropped `.shape` file to slot 8.
    pub fn import_custom_shape(
        &mut self,
        slot: usize,
        bytes: &[u8],
    ) -> Result<PresetReport, EngineError> {
        self.import_custom(|p| import_shape_file(p, slot, bytes))
    }

    /// The `.wave` file text of custom-wave slot `slot` of the loaded preset (the incoming one
    /// during a soft cut); `None` when nothing is loaded or `slot >= 16`.
    pub fn export_custom_wave(&self, slot: usize) -> Option<String> {
        export_wave_file(&self.shown_slot()?.loaded.preset, slot)
    }

    /// The `.shape` file text of custom-shape slot `slot`; see [`Engine::export_custom_wave`].
    pub fn export_custom_shape(&self, slot: usize) -> Option<String> {
        export_shape_file(&self.shown_slot()?.loaded.preset, slot)
    }

    fn shown_slot(&self) -> Option<&Slot> {
        self.incoming.as_ref().or(self.active.as_ref())
    }

    fn import_custom(
        &mut self,
        apply: impl FnOnce(&mut Preset) -> Result<(), ParseError>,
    ) -> Result<PresetReport, EngineError> {
        let into_incoming = self.incoming.is_some();
        let target = if into_incoming {
            &mut self.incoming
        } else {
            &mut self.active
        };
        let mut slot = target.take().ok_or(EngineError::NoPreset)?;
        let result = self.rebuild(&mut slot, apply);
        // the build path never touches `active` / `incoming`, so putting the slot back restores the engine
        let target = if into_incoming {
            &mut self.incoming
        } else {
            &mut self.active
        };
        *target = Some(slot);
        result
    }

    fn rebuild(
        &mut self,
        slot: &mut Slot,
        apply: impl FnOnce(&mut Preset) -> Result<(), ParseError>,
    ) -> Result<PresetReport, EngineError> {
        let mut preset = slot.loaded.preset.clone();
        apply(&mut preset)?;
        let text = preset.to_milk();
        let name = slot.loaded.name.clone();
        let (loaded, report) =
            self.build_loaded(text.as_bytes(), &name, &mut slot.renderer, true)?;
        slot.loaded = loaded;
        slot.last = LastFrame::default();
        Ok(report)
    }
}
