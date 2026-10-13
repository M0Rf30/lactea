//! Host-side helpers shared by the lactea binaries and the C API. The engine crates never touch the
//! filesystem or open a GPU; the hosts do, and they all need the same pieces: a directory-backed
//! [`lactea::TextureProvider`], image decoding, headless GPU setup and a WAV reader.

pub mod gpu;
pub mod presets;
pub mod textures;
pub mod wav;

pub use presets::{PlaylistFs, is_preset_file, scan_preset_files, scan_presets};
pub use textures::{DirTextures, TEXTURE_EXTENSIONS, decode_file, decode_image, decode_media};
