//! Reproduction of `CPlugin::LoadShaderFromMemory` text assembly (plugin.cpp), the default
//! warp/comp shader generators (`GenWarpPShaderText` / `GenCompPShaderText`) and
//! `StripComments`.

use crate::TranslateError;
use lactea_abi::PassKind;

/// `data/include.fx` of MilkDrop 2 (BSD-3, Nullsoft), copied verbatim.
pub(crate) const INCLUDE_FX: &str = include_str!("include.fx");

const WARP_DEFINES: &str =
    "#define rad _rad_ang.x\n#define ang _rad_ang.y\n#define uv _uv.xy\n#define uv_orig _uv.zw\n";
// [sic] MD2: in the composite shader `uv_orig` is the same as `uv` (`_uv` is only a float2 there).
const COMP_DEFINES: &str = "#define rad _rad_ang.x\n#define ang _rad_ang.y\n#define uv _uv.xy\n#define uv_orig _uv.xy\n#define hue_shader _vDiffuse.xyz\n";
const WARP_PARAMS: &str = "float4 _vDiffuse : COLOR, float4 _uv : TEXCOORD0, float2 _rad_ang : TEXCOORD1, out float4 _return_value : COLOR0";
const COMP_PARAMS: &str = "float4 _vDiffuse : COLOR, float2 _uv : TEXCOORD0, float2 _rad_ang : TEXCOORD1, out float4 _return_value : COLOR0";
const FIRST_LINE: &str = "    float3 ret = 0;";
const LAST_LINE: &str = "    _return_value = float4(ret.xyz, _vDiffuse.w);";

/// Port of `StripComments` (plugin.cpp), including its quirks (`/*/` is a complete comment;
/// `//` ends at CR or LF and the line break is kept; block comments swallow newlines).
pub(crate) fn strip_comments(s: &str) -> String {
    let b: Vec<char> = s.chars().collect();
    if b.len() < 2 {
        return s.to_string();
    }
    let mut out = String::new();
    let mut ign_eol = false;
    let mut ign_close = false;
    let mut skip = 0;
    for i in 0..b.len() {
        let c0 = b[i];
        let c1 = if i + 1 < b.len() { b[i + 1] } else { '\0' };
        if !ign_close && c0 == '/' && c1 == '/' {
            ign_eol = true;
        }
        if ign_eol && (c0 == '\n' || c0 == '\r') {
            ign_eol = false;
            skip = 0;
        }
        if !ign_eol && c0 == '/' && c1 == '*' {
            ign_close = true;
        }
        if ign_close && c0 == '*' && c1 == '/' {
            ign_close = false;
            skip = 2;
        }
        if !ign_eol && !ign_close {
            if skip > 0 {
                skip -= 1;
            } else {
                out.push(c0);
            }
        }
    }
    out
}

/// Identifiers (`[A-Za-z_][A-Za-z0-9_]*`) of comment-free HLSL text, in order; numeric literals are skipped whole.
fn identifiers(s: &str) -> Vec<&str> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_digit() {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.' || b[i] == b'_') {
                i += 1;
            }
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            out.push(&s[start..i]);
        } else {
            i += 1;
        }
    }
    out
}

/// `sampler_*` / `texsize_*` names declared by `text` (a sampler / `float4` declaration or a `#define`).
fn declared_names(text: &str) -> std::collections::HashSet<&str> {
    let ids = identifiers(text);
    let mut set = std::collections::HashSet::new();
    for (k, id) in ids.iter().enumerate() {
        if (id.starts_with("sampler_") || id.starts_with("texsize_"))
            && k > 0
            && matches!(
                ids[k - 1],
                "sampler" | "sampler2D" | "sampler3D" | "samplerCUBE" | "float4" | "define"
            )
        {
            set.insert(*id);
        }
    }
    set
}

/// Declarations for the samplers and `texsize_*` constants a shader uses without declaring them.
///
/// projectM (`MilkdropShader::GetReferencedSamplers`, `TranspileHLSLShader`) takes every `sampler_<name>` /
/// `texsize_<name>` token of the comment-stripped shader as a texture reference and emits the declarations itself, so
/// `tex2D(sampler_pw_noise_lq, ..)` works without `sampler sampler_pw_noise_lq;`. MD2 rejects such a shader (undeclared
/// identifier); following projectM, the text returned here is prepended to the user shader.
pub(crate) fn implicit_declarations(user: &str) -> String {
    let user = user.replace('\u{1}', "\n").replace("\r\n", "\n");
    let stripped = strip_comments(&user);
    let include = strip_comments(INCLUDE_FX);
    let mut declared = declared_names(&include);
    declared.extend(declared_names(&stripped));
    let mut seen = std::collections::HashSet::new();
    let mut out = String::new();
    for id in identifiers(&stripped) {
        if id == "sampler_state" || declared.contains(id) || !seen.insert(id) {
            continue;
        }
        if id.starts_with("sampler_") {
            let three_d = crate::iface::sampler_root(id).0.starts_with("noisevol");
            out.push_str(&format!(
                "{} {id};\n",
                if three_d { "sampler3D" } else { "sampler2D" }
            ));
        } else if id.starts_with("texsize_") {
            out.push_str(&format!("float4 {id};\n"));
        }
    }
    out
}

/// The complete HLSL text MD2 hands to `D3DXCompileShader` (entry point `PS`):
/// comment-stripped include.fx + per-kind `#define`s + the user shader with the `shader_body`
/// surgery applied.
///
/// Errors with [`TranslateError::Wrap`] when `shader_body`, its opening `{` or the closing `}` is missing.
pub fn md2_wrapped_hlsl(kind: PassKind, user: &str) -> Result<String, TranslateError> {
    // lines are joined with LINEFEED_CONTROL_CHAR (1) in MD2 and converted to CRLF
    let user = user.replace('\u{1}', "\n").replace("\r\n", "\n");
    let mut text = strip_comments(INCLUDE_FX);
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(match kind {
        PassKind::Warp => WARP_DEFINES,
        PassKind::Comp => COMP_DEFINES,
    });
    let stripped = strip_comments(&user);
    let params = match kind {
        PassKind::Warp => WARP_PARAMS,
        PassKind::Comp => COMP_PARAMS,
    };
    let s = stripped.as_str();
    let pos = s
        // plain substring search, like MD2's `strstr`: no word-boundary check
        .find("shader_body")
        .ok_or_else(|| TranslateError::Wrap("no `shader_body` found".into()))?;
    // replace "shader_body" with spaces, then insert "void PS( params )\n"
    let before = &s[..pos];
    let after = &s[pos + "shader_body".len()..];
    let mut body = String::new();
    body.push_str(before);
    body.push_str(&" ".repeat("shader_body".len()));
    body.push_str(&format!("void PS( {params} )\n"));
    let header_len = body.len();
    body.push_str(after);
    // first '{' after the inserted header
    let brace = body[header_len - 1..].find('{').map(|i| i + header_len - 1);
    let brace = brace.ok_or_else(|| TranslateError::Wrap("no `{` after shader_body".into()))?;
    let (head, rest) = body.split_at(brace + 1);
    let mut out = String::from(head);
    out.push_str(FIRST_LINE);
    out.push('\n');
    // last '}' in what follows; everything from it on is replaced by " <last line>\n}\n"
    let last = rest
        .rfind('}')
        .ok_or_else(|| TranslateError::Wrap("no closing `}` in shader".into()))?;
    out.push_str(&rest[..last]);
    out.push_str(&format!(" {LAST_LINE}\n}}\n"));
    text.push_str(&out);
    Ok(text)
}

/// `CPlugin::GenWarpPShaderText`: text MD2 synthesizes for a warp shader when the preset has a
/// pixel-shader version but no `warp_N` lines.
pub fn default_warp_shader(decay: f32, wrap: bool) -> String {
    let mut s = String::from("shader_body\n{\n");
    s.push_str("    // sample previous frame\n");
    s.push_str(&format!(
        "    ret = tex2D( sampler{}_main, uv ).xyz;\n",
        if wrap { "" } else { "_fc" }
    ));
    s.push_str("    \n    // darken (decay) over time\n");
    s.push_str(&format!(
        "    ret *= {:.2}; //or try: ret -= 0.004;\n",
        decay
    ));
    s.push_str("}\n");
    s
}

/// `CPlugin::GenCompPShaderText`: text MD2 synthesizes for a composite shader when the preset has
/// a pixel-shader version but no `comp_N` lines. `ve_orient` is `nVideoEchoOrientation`.
#[allow(clippy::too_many_arguments)]
pub fn default_comp_shader(
    brightness: f32,
    ve_alpha: f32,
    ve_zoom: f32,
    ve_orient: i32,
    hue_shader: f32,
    brighten: bool,
    darken: bool,
    solarize: bool,
    invert: bool,
) -> String {
    let mut s = String::from("shader_body\n{\n");
    if ve_alpha > 0.001 {
        let ox = if ve_orient % 2 != 0 { -1 } else { 1 };
        let oy = if ve_orient >= 2 { -1 } else { 1 };
        s.push_str(&format!(
            "    float2 uv_echo = (uv - 0.5)*{:.3}*float2({},{}) + 0.5;\n",
            1.0 / ve_zoom,
            ox,
            oy
        ));
        s.push_str("    ret = lerp( tex2D(sampler_main, uv).xyz, \n");
        s.push_str("                tex2D(sampler_main, uv_echo).xyz, \n");
        s.push_str(&format!("                {:.2} \n", ve_alpha));
        s.push_str("              ); //video echo\n");
        s.push_str(&format!("    ret *= {:.2}; //gamma\n", brightness));
    } else {
        s.push_str("    ret = tex2D(sampler_main, uv).xyz;\n");
        s.push_str(&format!("    ret *= {:.2}; //gamma\n", brightness));
    }
    if hue_shader >= 1.0 {
        s.push_str("    ret *= hue_shader; //old hue shader effect\n");
    } else if hue_shader > 0.001 {
        s.push_str(&format!(
            "    ret *= {:.2} + {:.2}*hue_shader; //old hue shader effect\n",
            1.0 - hue_shader,
            hue_shader
        ));
    }
    if brighten {
        s.push_str("    ret = 1 - (1-ret) * (1-ret); //brighten\n");
    }
    if darken {
        s.push_str("    ret *= ret; //darken\n");
    }
    if solarize {
        s.push_str("    ret = ret*(1-ret)*2; //solarize\n");
    }
    if invert {
        s.push_str("    ret = 1 - ret; //invert\n");
    }
    s.push_str("}\n");
    s
}

#[cfg(test)]
mod tests {
    use super::strip_comments;

    #[test]
    fn strip_comments_matches_md2() {
        assert_eq!(strip_comments("a // c\nb"), "a \nb");
        assert_eq!(strip_comments("a /* c\nc2 */ b"), "a  b");
        // quirk: `/*/` is a complete comment in MD2's state machine
        assert_eq!(strip_comments("a /*/ b"), "a  b");
        assert_eq!(strip_comments("x"), "x");
    }
}
