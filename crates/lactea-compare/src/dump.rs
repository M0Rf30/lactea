//! Dump reader/writer for the layout in [`lactea_abi::dump`].

use crate::{Error, Result};
use lactea_abi::dump::{self, DumpMeta, Stage};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// RGBA8 image, row 0 = top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// RGBA8 bytes, row-major, 4 per pixel.
    pub rgba: Vec<u8>,
}

impl Image {
    /// Wraps an RGBA8 buffer, row 0 = top.
    ///
    /// # Panics
    ///
    /// Panics if `rgba.len()` is not `width * height * 4`.
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        assert_eq!(
            rgba.len(),
            width as usize * height as usize * 4,
            "rgba length mismatch"
        );
        Self {
            width,
            height,
            rgba,
        }
    }
    /// Creates a `width` x `height` image filled with `px`.
    pub fn solid(width: u32, height: u32, px: [u8; 4]) -> Self {
        let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
        for _ in 0..width as usize * height as usize {
            rgba.extend_from_slice(&px);
        }
        Self {
            width,
            height,
            rgba,
        }
    }
    /// Returns the pixel at (`x`, `y`).
    ///
    /// # Panics
    ///
    /// Panics if the coordinates are outside the image.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [
            self.rgba[i],
            self.rgba[i + 1],
            self.rgba[i + 2],
            self.rgba[i + 3],
        ]
    }
    /// Overwrites the pixel at (`x`, `y`).
    ///
    /// # Panics
    ///
    /// Panics if the coordinates are outside the image.
    pub fn set_pixel(&mut self, x: u32, y: u32, px: [u8; 4]) {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        self.rgba[i..i + 4].copy_from_slice(&px);
    }

    /// Decodes a PNG file into RGBA8.
    pub fn load_png(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let img = image::ImageReader::open(path)
            .map_err(|e| Error::io(path, e))?
            .with_guessed_format()
            .map_err(|e| Error::io(path, e))?
            .decode()
            .map_err(|e| Error::fmt(path, e))?
            .to_rgba8();
        let (w, h) = img.dimensions();
        Ok(Self {
            width: w,
            height: h,
            rgba: img.into_raw(),
        })
    }

    /// Writes the image as an RGBA8 PNG, creating parent directories.
    pub fn save_png(&self, path: impl AsRef<Path>) -> Result<()> {
        write_png(path.as_ref(), self.width, self.height, &self.rgba)
    }
}

fn write_png(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<()> {
    if rgba.len() != w as usize * h as usize * 4 {
        return Err(Error::fmt(
            path,
            format!(
                "rgba buffer has {} bytes, expected {}x{}x4",
                rgba.len(),
                w,
                h
            ),
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    image::save_buffer_with_format(
        path,
        rgba,
        w,
        h,
        image::ColorType::Rgba8,
        image::ImageFormat::Png,
    )
    .map_err(|e| Error::fmt(path, e))
}

// --------------------------------------------------------------------------------------------
// Variable maps: JSON encoding of non-finite values
// --------------------------------------------------------------------------------------------

/// JSON encoding of one variable: finite -> number, NaN -> `"nan"`, +inf -> `"inf"`, -inf -> `"-inf"`.
pub fn encode_var(v: f64) -> Value {
    if v.is_nan() {
        Value::String("nan".into())
    } else if v == f64::INFINITY {
        Value::String("inf".into())
    } else if v == f64::NEG_INFINITY {
        Value::String("-inf".into())
    } else {
        serde_json::Number::from_f64(v)
            .map(Value::Number)
            .unwrap_or(Value::Null)
    }
}

/// Serialise a variable map (sorted keys) to pretty-less JSON text.
pub fn vars_to_json(vars: &BTreeMap<String, f64>) -> String {
    let obj: serde_json::Map<String, Value> = vars
        .iter()
        .map(|(k, v)| (k.clone(), encode_var(*v)))
        .collect();
    Value::Object(obj).to_string()
}

fn decode_var(v: &Value) -> std::result::Result<f64, String> {
    match v {
        Value::Number(n) => n
            .as_f64()
            .ok_or_else(|| format!("number {n} not representable as f64")),
        Value::Null => Ok(f64::NAN),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => s
            .trim()
            .parse::<f64>()
            .map_err(|_| format!("bad numeric string {s:?}")),
        other => Err(format!("unsupported value {other}")),
    }
}

/// Parse `vars/<frame>.json`. Accepts numbers, the strings `"nan"`, `"inf"`, `"-inf"`
/// (any case, also `"infinity"`), `null` (= NaN), and — as a tolerance for C++ writers using
/// printf — bare `nan`/`inf`/`-inf`/`-nan`/`Infinity` tokens outside strings.
pub fn parse_vars(text: &str) -> std::result::Result<BTreeMap<String, f64>, String> {
    let value: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(first) => {
            serde_json::from_str(&sanitize_bare_tokens(text)).map_err(|_| first.to_string())?
        }
    };
    let Value::Object(obj) = value else {
        return Err("vars JSON must be an object".into());
    };
    let mut out = BTreeMap::new();
    for (k, v) in obj {
        out.insert(
            k.clone(),
            decode_var(&v).map_err(|e| format!("var {k}: {e}"))?,
        );
    }
    Ok(out)
}

/// Quote bare non-JSON float tokens (`nan`, `inf`, `-inf`, `-nan`, `Infinity`, ...).
fn sanitize_bare_tokens(text: &str) -> String {
    let b: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 16);
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < b.len() {
                out.push(b[i + 1]);
                i += 1;
            } else if c == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_str = true;
            out.push(c);
            i += 1;
            continue;
        }
        let sign = (c == '-' || c == '+') && i + 1 < b.len() && b[i + 1].is_ascii_alphabetic();
        if c.is_ascii_alphabetic() || sign {
            let start = i;
            i += 1;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == '_') {
                i += 1;
            }
            let tok: String = b[start..i].iter().collect();
            let lower = tok.to_ascii_lowercase();
            let bare = lower.trim_start_matches(['-', '+']);
            if matches!(bare, "nan" | "inf" | "infinity") {
                let neg = lower.starts_with('-') && bare != "nan";
                out.push_str(match (bare, neg) {
                    ("nan", _) => "\"nan\"",
                    (_, true) => "\"-inf\"",
                    _ => "\"inf\"",
                });
            } else {
                out.push_str(&tok);
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

// --------------------------------------------------------------------------------------------
// Writer
// --------------------------------------------------------------------------------------------

/// Writes the dump layout. All `write_*` take `&self` and touch distinct files per call, so a
/// writer may be shared between threads.
#[derive(Debug)]
pub struct DumpWriter {
    dir: PathBuf,
}

impl DumpWriter {
    /// Create `dir` (and parents) and write `meta.json`.
    pub fn create(dir: impl AsRef<Path>, meta: DumpMeta) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        let p = dir.join(dump::META_FILE);
        let text = serde_json::to_string_pretty(&meta).map_err(|e| Error::fmt(&p, e))?;
        std::fs::write(&p, text).map_err(|e| Error::io(&p, e))?;
        Ok(Self { dir })
    }

    /// Root directory of the dump.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Path where the PNG for `stage` at `frame` is (or would be) written.
    pub fn stage_path(&self, stage: Stage, frame: u32) -> PathBuf {
        self.dir
            .join(stage.dir())
            .join(dump::frame_file(frame, "png"))
    }

    /// `rgba8` is `w*h*4` bytes, row 0 = top.
    pub fn write_stage(
        &self,
        stage: Stage,
        frame: u32,
        w: u32,
        h: u32,
        rgba8: &[u8],
    ) -> Result<()> {
        write_png(&self.stage_path(stage, frame), w, h, rgba8)
    }

    /// Writes `vars/<frame>.json` with non-finite values encoded as strings.
    pub fn write_vars(&self, frame: u32, vars: &BTreeMap<String, f64>) -> Result<()> {
        let d = self.dir.join(dump::VARS_DIR);
        std::fs::create_dir_all(&d).map_err(|e| Error::io(&d, e))?;
        let p = d.join(dump::frame_file(frame, "json"));
        std::fs::write(&p, vars_to_json(vars)).map_err(|e| Error::io(&p, e))
    }

    /// Static 2D texture `textures/<name>.png`.
    pub fn write_texture(&self, name: &str, w: u32, h: u32, rgba8: &[u8]) -> Result<()> {
        write_png(
            &self
                .dir
                .join(dump::TEXTURES_DIR)
                .join(format!("{name}.png")),
            w,
            h,
            rgba8,
        )
    }

    /// Raw binary texture (3D volumes) `textures/<name>.bin`.
    pub fn write_texture_bin(&self, name: &str, bytes: &[u8]) -> Result<()> {
        let d = self.dir.join(dump::TEXTURES_DIR);
        std::fs::create_dir_all(&d).map_err(|e| Error::io(&d, e))?;
        let p = d.join(format!("{name}.bin"));
        std::fs::write(&p, bytes).map_err(|e| Error::io(&p, e))
    }
}

// --------------------------------------------------------------------------------------------
// Reader
// --------------------------------------------------------------------------------------------

/// Reads a dump directory. Opening only lists files; PNG/JSON are decoded on demand.
#[derive(Debug)]
pub struct DumpReader {
    dir: PathBuf,
    meta: Option<DumpMeta>,
    meta_error: Option<String>,
    stages: Vec<(Stage, Vec<u32>)>,
    var_frames: Vec<u32>,
}

fn list_frames(dir: &Path, ext: &str) -> Vec<u32> {
    let mut v: Vec<u32> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let (stem, x) = name.rsplit_once('.')?;
                if x != ext {
                    return None;
                }
                stem.parse::<u32>().ok()
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    v.sort_unstable();
    v.dedup();
    v
}

impl DumpReader {
    /// Opens `dir`, listing stage frames and variable files without decoding them.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        if !dir.is_dir() {
            return Err(Error::Msg(format!("{}: not a directory", dir.display())));
        }
        let mp = dir.join(dump::META_FILE);
        let (meta, meta_error) = match std::fs::read_to_string(&mp) {
            Ok(t) => match serde_json::from_str::<DumpMeta>(&t) {
                Ok(m) => (Some(m), None),
                Err(e) => (None, Some(format!("{}: {e}", mp.display()))),
            },
            Err(e) => (None, Some(format!("{}: {e}", mp.display()))),
        };
        let mut stages = Vec::new();
        for s in Stage::ALL {
            let f = list_frames(&dir.join(s.dir()), "png");
            if !f.is_empty() {
                stages.push((s, f));
            }
        }
        let var_frames = list_frames(&dir.join(dump::VARS_DIR), "json");
        Ok(Self {
            dir,
            meta,
            meta_error,
            stages,
            var_frames,
        })
    }

    /// Root directory of the dump.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
    /// `None` if `meta.json` is missing or does not match [`DumpMeta`] (see [`Self::meta_error`]).
    pub fn meta(&self) -> Option<&DumpMeta> {
        self.meta.as_ref()
    }
    /// Why `meta.json` could not be loaded, if it could not.
    pub fn meta_error(&self) -> Option<&str> {
        self.meta_error.as_deref()
    }
    /// Stages that have at least one frame on disk, in `Stage::ALL` order.
    pub fn stages(&self) -> Vec<Stage> {
        self.stages.iter().map(|(s, _)| *s).collect()
    }
    /// Sorted frame numbers available for a stage (empty if none).
    pub fn frames(&self, stage: Stage) -> &[u32] {
        self.stages
            .iter()
            .find(|(s, _)| *s == stage)
            .map(|(_, f)| f.as_slice())
            .unwrap_or(&[])
    }
    /// Sorted frame numbers that have a variable file (empty if none).
    pub fn var_frames(&self) -> &[u32] {
        &self.var_frames
    }
    /// Whether any per-frame variable file exists.
    pub fn has_vars(&self) -> bool {
        !self.var_frames.is_empty()
    }

    /// Path of the PNG for `stage` at `frame` (the file may not exist).
    pub fn stage_path(&self, stage: Stage, frame: u32) -> PathBuf {
        self.dir
            .join(stage.dir())
            .join(dump::frame_file(frame, "png"))
    }

    /// Decodes the PNG of `stage` at `frame`.
    pub fn load_stage(&self, stage: Stage, frame: u32) -> Result<Image> {
        Image::load_png(self.stage_path(stage, frame))
    }

    /// Parses `vars/<frame>.json` into a name -> value map.
    pub fn load_vars(&self, frame: u32) -> Result<BTreeMap<String, f64>> {
        let p = self
            .dir
            .join(dump::VARS_DIR)
            .join(dump::frame_file(frame, "json"));
        let t = std::fs::read_to_string(&p).map_err(|e| Error::io(&p, e))?;
        parse_vars(&t).map_err(|e| Error::fmt(&p, e))
    }

    /// Names (without extension) of static textures, sorted, 2D `.png` and 3D `.bin`.
    pub fn texture_names(&self) -> Vec<String> {
        let mut v: Vec<String> = match std::fs::read_dir(self.dir.join(dump::TEXTURES_DIR)) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    let (stem, ext) = n.rsplit_once('.')?;
                    matches!(ext, "png" | "bin").then(|| stem.to_string())
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        v.sort();
        v.dedup();
        v
    }

    /// Decodes the 2D texture `textures/<name>.png`.
    pub fn load_texture(&self, name: &str) -> Result<Image> {
        Image::load_png(
            self.dir
                .join(dump::TEXTURES_DIR)
                .join(format!("{name}.png")),
        )
    }

    /// Raw bytes of `textures/<name>.bin`.
    pub fn load_texture_bin(&self, name: &str) -> Result<Vec<u8>> {
        let p = self
            .dir
            .join(dump::TEXTURES_DIR)
            .join(format!("{name}.bin"));
        std::fs::read(&p).map_err(|e| Error::io(&p, e))
    }
}

/// Parse a stage directory name.
pub fn parse_stage(s: &str) -> Option<Stage> {
    Stage::ALL.into_iter().find(|st| st.dir() == s.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lactea_abi::RunConfig;

    pub(crate) fn meta() -> DumpMeta {
        DumpMeta {
            producer: "test".into(),
            preset: "p.milk".into(),
            audio: "synth128".into(),
            config: RunConfig {
                width: 4,
                height: 3,
                frames: 2,
                ..RunConfig::default()
            },
            stages: vec![Stage::Final, Stage::Main],
            has_vars: true,
        }
    }

    #[test]
    fn roundtrip_with_nan_vars() {
        let td = tempfile::tempdir().unwrap();
        let w = DumpWriter::create(td.path().join("d"), meta()).unwrap();
        let rgba: Vec<u8> = (0..4 * 3 * 4).map(|i| (i * 5) as u8).collect();
        w.write_stage(Stage::Final, 0, 4, 3, &rgba).unwrap();
        w.write_stage(Stage::Final, 7, 4, 3, &rgba).unwrap();
        w.write_stage(Stage::Main, 7, 4, 3, &rgba).unwrap();
        let mut vars = BTreeMap::new();
        vars.insert("zoom".to_string(), 1.02);
        vars.insert("nan".to_string(), f64::NAN);
        vars.insert("pinf".to_string(), f64::INFINITY);
        vars.insert("ninf".to_string(), f64::NEG_INFINITY);
        vars.insert("tiny".to_string(), 1.234567890123456e-300);
        vars.insert("negzero".to_string(), -0.0);
        w.write_vars(7, &vars).unwrap();
        w.write_texture("noise_lq", 4, 3, &rgba).unwrap();
        w.write_texture_bin("noisevol_lq", &[1, 2, 3]).unwrap();

        let r = DumpReader::open(td.path().join("d")).unwrap();
        assert_eq!(r.meta(), Some(&meta()));
        assert_eq!(r.stages(), vec![Stage::Final, Stage::Main]);
        assert_eq!(r.frames(Stage::Final), &[0, 7]);
        assert_eq!(r.frames(Stage::Main), &[7]);
        assert!(r.frames(Stage::Blur1).is_empty());
        assert_eq!(r.var_frames(), &[7]);
        let img = r.load_stage(Stage::Final, 7).unwrap();
        assert_eq!((img.width, img.height), (4, 3));
        assert_eq!(img.rgba, rgba);
        let v = r.load_vars(7).unwrap();
        assert_eq!(v["zoom"], 1.02);
        assert!(v["nan"].is_nan());
        assert_eq!(v["pinf"], f64::INFINITY);
        assert_eq!(v["ninf"], f64::NEG_INFINITY);
        assert_eq!(v["tiny"], 1.234567890123456e-300);
        assert!(v["negzero"] == 0.0 && v["negzero"].is_sign_negative());
        assert_eq!(
            r.texture_names(),
            vec!["noise_lq".to_string(), "noisevol_lq".to_string()]
        );
        assert_eq!(r.load_texture("noise_lq").unwrap().rgba, rgba);
        assert_eq!(r.load_texture_bin("noisevol_lq").unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn vars_tolerant_parse() {
        let v = parse_vars(
            r#"{"a": "NaN", "b": null, "c": "-inf", "d": "Infinity", "e": 2.5, "f": 1e-3}"#,
        )
        .unwrap();
        assert!(v["a"].is_nan() && v["b"].is_nan());
        assert_eq!(v["c"], f64::NEG_INFINITY);
        assert_eq!(v["d"], f64::INFINITY);
        assert_eq!(v["e"], 2.5);
        // bare tokens as printf would emit them
        let v = parse_vars(r#"{"a": nan, "b": -nan, "c": inf, "d": -inf, "e": -1.5e+2, "nan": 1}"#)
            .unwrap();
        assert!(v["a"].is_nan() && v["b"].is_nan());
        assert_eq!(v["c"], f64::INFINITY);
        assert_eq!(v["d"], f64::NEG_INFINITY);
        assert_eq!(v["e"], -150.0);
        assert_eq!(v["nan"], 1.0);
        assert!(parse_vars("[1]").is_err());
        assert!(parse_vars("{\"a\": \"zzz\"}").is_err());
    }

    #[test]
    fn open_missing_meta_is_tolerated() {
        let td = tempfile::tempdir().unwrap();
        let r = DumpReader::open(td.path()).unwrap();
        assert!(r.meta().is_none() && r.meta_error().is_some());
        assert!(r.stages().is_empty());
        assert!(DumpReader::open(td.path().join("nope")).is_err());
    }
}
