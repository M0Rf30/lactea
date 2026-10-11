#![allow(dead_code)]
//! glslang oracle: MD2-wrapped HLSL -> SPIR-V (glslangValidator, DX9-compatible HLSL front end)
//! -> split combined image samplers (spirv-opt) -> naga module with the ABI bindings.

use lactea_abi::*;
use lactea_hlsl::{Translated, harness::glslang_source};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn have_tools() -> bool {
    Command::new("glslangValidator")
        .arg("--version")
        .output()
        .is_ok()
        && Command::new("spirv-opt").arg("--version").output().is_ok()
}

pub enum OracleError {
    /// glslang rejected the shader (HLSL invalid or outside glslang's front-end).
    Glslang(String),
    SpirvOpt(String),
    Naga(String),
    Source(String),
}

impl OracleError {
    pub fn category(&self) -> String {
        let (k, m) = match self {
            OracleError::Glslang(m) => ("glslang", m),
            OracleError::SpirvOpt(m) => ("spirv-opt", m),
            OracleError::Naga(m) => ("naga-spv", m),
            OracleError::Source(m) => ("source", m),
        };
        // first ERROR line, identifiers collapsed
        let line = m
            .lines()
            .find(|l| l.contains("ERROR") || l.contains("error"))
            .unwrap_or_else(|| m.lines().next().unwrap_or(""));
        let mut s = String::new();
        let mut q = false;
        for c in line
            .split(':')
            .skip(if line.contains("ERROR") { 2 } else { 0 })
            .collect::<Vec<_>>()
            .join(":")
            .chars()
        {
            if c == '\'' {
                q = !q;
                s.push(c);
            } else if !q {
                s.push(c);
            }
        }
        format!("{k}: {}", s.trim().chars().take(90).collect::<String>())
    }
    pub fn message(&self) -> &str {
        match self {
            OracleError::Glslang(m)
            | OracleError::SpirvOpt(m)
            | OracleError::Naga(m)
            | OracleError::Source(m) => m,
        }
    }
}

pub fn compile_oracle(
    c: &super::Case,
    t: &Translated,
    dir: &Path,
    idx: usize,
) -> Result<naga::Module, OracleError> {
    let src = glslang_source(c.kind, &c.text, &t.texsize_params)
        .map_err(|e| OracleError::Source(e.to_string()))?;
    std::fs::create_dir_all(dir).ok();
    let hlsl: PathBuf = dir.join(format!("{idx:04}.hlsl"));
    let spv: PathBuf = dir.join(format!("{idx:04}.spv"));
    let spv2: PathBuf = dir.join(format!("{idx:04}.split.spv"));
    std::fs::write(&hlsl, &src).map_err(|e| OracleError::Source(e.to_string()))?;
    let mut cmd = Command::new("glslangValidator");
    cmd.args([
        "-D",
        "-V",
        "-S",
        "frag",
        "--hlsl-dx9-compatible",
        "-e",
        "PS",
    ]);
    cmd.arg(&hlsl).arg("-o").arg(&spv);
    for (i, tb) in t.interface.textures.iter().enumerate() {
        cmd.args([
            "--resource-set-binding",
            "frag",
            &tb.hlsl_name,
            "1",
            &texture_binding(i as u32).to_string(),
        ]);
    }
    let out = cmd
        .output()
        .map_err(|e| OracleError::Glslang(e.to_string()))?;
    if !out.status.success() {
        return Err(OracleError::Glslang(
            String::from_utf8_lossy(&out.stdout).to_string()
                + &String::from_utf8_lossy(&out.stderr),
        ));
    }
    let out = Command::new("spirv-opt")
        .arg("--split-combined-image-sampler")
        .arg(&spv)
        .arg("-o")
        .arg(&spv2)
        .output()
        .map_err(|e| OracleError::SpirvOpt(e.to_string()))?;
    if !out.status.success() {
        return Err(OracleError::SpirvOpt(
            String::from_utf8_lossy(&out.stderr).to_string(),
        ));
    }
    let mut bytes = std::fs::read(&spv2).map_err(|e| OracleError::SpirvOpt(e.to_string()))?;
    patch_fmod_to_frem(&mut bytes);
    let mut module =
        naga::front::spv::parse_u8_slice(&bytes, &naga::front::spv::Options::default())
            .map_err(|e| OracleError::Naga(format!("{e:?}")))?;
    for (_, g) in module.global_variables.iter_mut() {
        let Some(name) = g.name.clone() else { continue };
        for (i, tb) in t.interface.textures.iter().enumerate() {
            if name == format!("{}_image", tb.hlsl_name) {
                g.binding = Some(naga::ResourceBinding {
                    group: 1,
                    binding: texture_binding(i as u32),
                });
            } else if name == format!("{}_sampler", tb.hlsl_name) {
                g.binding = Some(naga::ResourceBinding {
                    group: 1,
                    binding: sampler_binding(i as u32),
                });
            }
        }
    }
    Ok(module)
}

/// Does glslang's DX9-compatible HLSL front end accept the MD2-wrapped shader? (validity only)
pub fn glslang_validity(c: &super::Case, dir: &Path, idx: usize) -> Result<(), OracleError> {
    let src =
        glslang_source(c.kind, &c.text, &[]).map_err(|e| OracleError::Source(e.to_string()))?;
    std::fs::create_dir_all(dir).ok();
    let hlsl: PathBuf = dir.join(format!("{idx:04}.hlsl"));
    let spv: PathBuf = dir.join(format!("{idx:04}.spv"));
    std::fs::write(&hlsl, &src).map_err(|e| OracleError::Source(e.to_string()))?;
    let out = Command::new("glslangValidator")
        .args([
            "-D",
            "-V",
            "-S",
            "frag",
            "--hlsl-dx9-compatible",
            "-e",
            "PS",
        ])
        .arg(&hlsl)
        .arg("-o")
        .arg(&spv)
        .output()
        .map_err(|e| OracleError::Glslang(e.to_string()))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(OracleError::Glslang(
            String::from_utf8_lossy(&out.stdout).to_string()
                + &String::from_utf8_lossy(&out.stderr),
        ))
    }
}

/// glslang lowers HLSL `%`/`fmod` to OpFMod (floor-based, GLSL `mod`). HLSL/D3D `fmod` is the
/// C remainder (sign of the dividend) = OpFRem, which is also what our `%` emits. Patch the
/// oracle's SPIR-V so it represents the D3D semantic (operand layout is identical).
fn patch_fmod_to_frem(bytes: &mut [u8]) {
    let mut i = 5 * 4;
    while i + 4 <= bytes.len() {
        let w = u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
        let (count, op) = ((w >> 16) as usize, w & 0xffff);
        if count == 0 {
            break;
        }
        if op == 141 {
            let nw = (w & 0xffff_0000) | 140;
            bytes[i..i + 4].copy_from_slice(&nw.to_le_bytes());
        }
        i += count * 4;
    }
}

/// Whether the SPIR-V fragment input at location 1 (`_uv`) is a vec2 (composite shaders).
pub fn max_abs_diff(a: &[u8], b: &[u8]) -> (u32, usize) {
    let mut m = 0u32;
    let mut at = 0;
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        let d = (*x as i32 - *y as i32).unsigned_abs();
        if d > m {
            m = d;
            at = i;
        }
    }
    (m, at)
}
