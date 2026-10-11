//! MilkDrop HLSL (ps_2/ps_3 dialect + `include.fx`) to WGSL translator.
//!
//! The translator reproduces exactly how MilkDrop 2 turns preset shader text into a compilable
//! pixel shader ([`md2_wrapped_hlsl`]: `include.fx` prepend, per-pass `#define`s for
//! `uv`/`uv_orig`/`rad`/`ang`/`hue_shader`, `shader_body` -> `void PS(...)`, `float3 ret = 0;`
//! and the final `_return_value = float4(ret.xyz, _vDiffuse.w)`), then lexes, preprocesses,
//! parses, type-checks and emits WGSL against the frozen `lactea-abi` interface.
//!
//! ## Semantics decisions
//! * `%` on floats is truncated (`fmod`), `frac(x) = x - floor(x)`, `pow(x, y)` is `pow(abs(x), y)` and
//!   `sqrt/log/log2/log10/rsqrt` take `abs(arg)` (projectM's hlslparser output; D3D9 semantics, no NaN);
//!   `rsqrt` is `inverseSqrt` and `log10(x)` is `log(x) / log(10)`.
//! * ints are floats (ps_2/ps_3 hardware): `int(x)` truncates, `int/int` is a float division.
//! * `tex2D` is `textureSample`; the module disables the WGSL `derivative_uniformity`
//!   diagnostic (D3D9 `ps_2_x` evaluates both sides of branches; MD2 textures that are mip-mapped
//!   — user/noise textures — therefore need real implicit derivatives).
//! * `fft` / `wave` samplers resolve to user textures of that name; the host supplies them or a
//!   placeholder.
//!
//! See `docs/hlsl.md` in the repository (<https://github.com/M0Rf30/lactea>) for the full list.
#![warn(missing_docs)]

mod ast;
mod emit;
#[cfg(feature = "testing")]
#[doc(hidden)]
pub mod harness;
mod iface;
mod lex;
mod parse;
mod pp;
mod wrap;

use lactea_abi::{PassKind, ShaderInterface, TexDim, TexSource};
use std::fmt;

pub use wrap::{default_comp_shader, default_warp_shader, md2_wrapped_hlsl};

/// Why a shader could not be translated. The messages are for humans; [`TranslateError::category`] is stable
/// enough to bucket failures.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum TranslateError {
    /// `shader_body` surgery failed (MD2 would reject the shader too).
    Wrap(String),
    /// Invalid token (bad literal, stray character).
    Lex(String),
    /// Malformed or unsupported preprocessor input (`#if` expression, macro call, directive order, ...).
    Preprocess(String),
    /// Syntax error.
    Parse(String),
    /// Valid HLSL that this translator does not support.
    Unsupported(String),
    /// Type/semantic error (undefined identifier, impossible conversion, ...).
    Type(String),
}

impl TranslateError {
    /// Short lowercase name of the failing stage (`"wrap"`, `"lex"`, `"preprocess"`, `"parse"`,
    /// `"unsupported"`, `"type"`).
    pub fn category(&self) -> &'static str {
        match self {
            TranslateError::Wrap(_) => "wrap",
            TranslateError::Lex(_) => "lex",
            TranslateError::Preprocess(_) => "preprocess",
            TranslateError::Parse(_) => "parse",
            TranslateError::Unsupported(_) => "unsupported",
            TranslateError::Type(_) => "type",
        }
    }
    /// The human-readable detail without the category prefix.
    pub fn message(&self) -> &str {
        match self {
            TranslateError::Wrap(m)
            | TranslateError::Lex(m)
            | TranslateError::Preprocess(m)
            | TranslateError::Parse(m)
            | TranslateError::Unsupported(m)
            | TranslateError::Type(m) => m,
        }
    }
}

impl fmt::Display for TranslateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} error: {}", self.category(), self.message())
    }
}

impl std::error::Error for TranslateError {}

/// Translation switches. Start from [`Options::default`] and set the fields you need.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Options {
    /// Emit `diagnostic(off, derivative_uniformity);` so `textureSample`/derivatives are legal in
    /// non-uniform control flow (default true; see crate docs).
    pub derivative_uniformity_off: bool,
    /// Float `%` and `fmod` use GLSL `mod` semantics, `x - y * floor(x / y)` (result takes the sign of the
    /// divisor). projectM's hlslparser lowers both to GLSL `mod`, so a negative dividend gives a positive result
    /// there; HLSL/D3D (MilkDrop 2) truncates (`x - y * trunc(x / y)`). Integer `%` is unaffected. Default `false`
    /// (HLSL semantics).
    pub floor_mod: bool,
    /// Reproduce projectM's hlslparser + GLSL generator where they differ from the D3DX compiler MilkDrop 2 uses:
    /// the parser's priority reset after `(float2(..)..)`, casts swallowing the rest of the expression and
    /// left-associative `?:`; `asin` / `acos` replaced by DX9-like polynomials outside `[-1, 1]` (instead of NaN);
    /// float literals printed with 6 significant digits (`ostream << float`). Default `false` (HLSL semantics).
    pub projectm_flavor: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            derivative_uniformity_off: true,
            floor_mod: false,
            projectm_flavor: false,
        }
    }
}

/// A translated shader: WGSL text plus the resources it binds.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Translated {
    /// Fragment-stage WGSL (the shared `lactea-abi` prelude, the translated shader and `fs_main`).
    pub wgsl: String,
    /// The textures the shader binds, in binding order.
    pub interface: ShaderInterface,
    /// `texsize_<name>` HLSL parameter names in `PassUniforms.texsize` slot order.
    pub texsize_params: Vec<String>,
    /// Non-fatal notes (e.g. implicit conversions that MD2 would warn about).
    pub warnings: Vec<String>,
}

/// Translate one MD2 warp/composite shader (`src` = the preset's `warp_N=` / `comp_N=` lines
/// joined with `\n`, one leading backtick per line already removed).
pub fn translate(kind: PassKind, src: &str, opts: &Options) -> Result<Translated, TranslateError> {
    let decls = wrap::implicit_declarations(src);
    let full = if decls.is_empty() {
        md2_wrapped_hlsl(kind, src)?
    } else {
        md2_wrapped_hlsl(kind, &format!("{decls}{src}"))?
    };
    translate_wrapped(kind, &full, opts)
}

/// Translate already MD2-wrapped HLSL (the text from [`md2_wrapped_hlsl`]). Unlike [`translate`] this does not add
/// declarations for undeclared `sampler_*` / `texsize_*` names.
pub fn translate_wrapped(
    kind: PassKind,
    full: &str,
    opts: &Options,
) -> Result<Translated, TranslateError> {
    let toks = pp::preprocess(full)?;
    let mut p = parse::Parser::new(toks);
    p.pm_flavor = opts.projectm_flavor;
    p.parse_program()?;
    let prog = p.prog;
    let mut tr = emit::Tr::new(&prog);
    tr.floor_mod = opts.floor_mod;
    tr.pm_flavor = opts.projectm_flavor;
    if !tr.fns.contains_key("PS") {
        return Err(TranslateError::Wrap("no PS function".into()));
    }
    let ps = tr.translate_fn("PS", 0)?;
    let e = tr.finish(&ps, kind);

    // texture interface: samplers in declaration order (== D3D sampler register order)
    let mut samplers = e.samplers;
    samplers.sort_by_key(|s| s.1);
    if samplers.len() > lactea_abi::MAX_PASS_TEXTURES {
        return Err(TranslateError::Unsupported(format!(
            "{} samplers (max {})",
            samplers.len(),
            lactea_abi::MAX_PASS_TEXTURES
        )));
    }
    let mut iface = ShaderInterface::default();
    let mut bindings = String::new();
    for (i, (name, _, dim, used3d)) in samplers.iter().enumerate() {
        let tb = iface::resolve_sampler(name, *dim, *used3d);
        let ty = if tb.dim == TexDim::D3 {
            "texture_3d<f32>"
        } else {
            "texture_2d<f32>"
        };
        bindings.push_str(&format!(
            "@group(1) @binding({}) var tex_{name}: {ty};\n@group(1) @binding({}) var smp_{name}: sampler;\n",
            lactea_abi::texture_binding(i as u32),
            lactea_abi::sampler_binding(i as u32)
        ));
        if let TexSource::Blur(l) = &tb.source {
            iface.max_blur_level = iface.max_blur_level.max(*l);
        }
        iface.textures.push(tb);
    }
    let mut slots = vec![TexSource::Main; e.texsizes.len()];
    for (name, slot) in &e.texsizes {
        slots[*slot] = iface::texsize_source(name, &iface.textures);
    }
    iface.texsize_slots = slots;

    let mut wgsl = String::new();
    if opts.derivative_uniformity_off {
        wgsl.push_str("diagnostic(off, derivative_uniformity);\n");
    }
    wgsl.push_str(lactea_abi::WGSL_COMMON);
    wgsl.push_str(&bindings);
    wgsl.push_str(&e.body);
    let mut texsize_params = vec![String::new(); e.texsizes.len()];
    for (name, slot) in &e.texsizes {
        texsize_params[*slot] = name.clone();
    }
    Ok(Translated {
        wgsl,
        interface: iface,
        texsize_params,
        warnings: e.warnings,
    })
}
