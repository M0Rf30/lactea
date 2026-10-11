//! Test-harness support (behind the `testing` feature; not part of the stable API): builds the HLSL text the semantic
//! differential feeds to glslang. The MD2 `include.fx` register declarations are replaced by an
//! equivalent uniform block layout (`mu_f[]` = byte-compatible with `MilkUniforms`, `pu_f[]` =
//! `PassUniforms`), user globals become `static` (D3D lets shaders write to uniform globals),
//! everything else is the unmodified MD2-wrapped shader.

use crate::lex::{T, Tok};
use crate::{TranslateError, pp, wrap};
use lactea_abi::PassKind;

/// Prelude replacing the declarations of `include.fx`.
fn prelude() -> String {
    let mut s = String::new();
    s.push_str("[[vk::binding(0,0)]] cbuffer MU { float4 mu_f[102]; };\n");
    s.push_str("[[vk::binding(0,1)]] cbuffer PU { float4 pu_f[16]; };\n");
    s.push_str("static float4 rand_frame = mu_f[0];\nstatic float4 rand_preset = mu_f[1];\n");
    for i in 0..14 {
        s.push_str(&format!("static float4 _c{i} = mu_f[{}];\n", 2 + i));
    }
    for (j, c) in "abcdefgh".chars().enumerate() {
        s.push_str(&format!("static float4 _q{c} = mu_f[{}];\n", 16 + j));
    }
    let mut k = 0;
    for g in ["s", "d", "f", "vf", "uf", "rand"] {
        for n in 1..=4 {
            let base = 24 + k * 3;
            // HLSL float4x3 constructor is row-major; ABI: rot[i][col][row] -> mu_f[base + col][row]
            let mut rows = Vec::new();
            for r in ["x", "y", "z", "w"] {
                rows.push(format!(
                    "mu_f[{base}].{r}, mu_f[{}].{r}, mu_f[{}].{r}",
                    base + 1,
                    base + 2
                ));
            }
            s.push_str(&format!(
                "static float4x3 rot_{g}{n} = float4x3({});\n",
                rows.join(", ")
            ));
            k += 1;
        }
    }
    for (i, n) in [
        "texsize_noise_lq",
        "texsize_noise_lq_lite",
        "texsize_noise_mq",
        "texsize_noise_hq",
        "texsize_noisevol_lq",
        "texsize_noisevol_hq",
    ]
    .iter()
    .enumerate()
    {
        s.push_str(&format!("static float4 {n} = mu_f[{}];\n", 96 + i));
    }
    for n in [
        "sampler_main",
        "sampler_fc_main",
        "sampler_pc_main",
        "sampler_fw_main",
        "sampler_pw_main",
        "sampler_noise_lq",
        "sampler_noise_lq_lite",
        "sampler_noise_mq",
        "sampler_noise_hq",
        "sampler_blur1",
        "sampler_blur2",
        "sampler_blur3",
    ] {
        s.push_str(&format!("sampler2D {n};\n"));
    }
    s.push_str("sampler3D sampler_noisevol_lq;\nsampler3D sampler_noisevol_hq;\n");
    s
}

const TYPE_PREFIXES: &[&str] = &[
    "float", "half", "double", "int", "uint", "bool", "dword", "vector", "matrix",
];

fn is_value_type(n: &str) -> bool {
    TYPE_PREFIXES.iter().any(|p| n.starts_with(p))
}

fn rewrite_globals(toks: Vec<Tok>, texsize_params: &[String]) -> Result<Vec<Tok>, TranslateError> {
    let mut out: Vec<Tok> = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        // find the extent of this top-level item
        let mut j = i;
        let (mut paren, mut brace) = (0i32, 0i32);
        let mut seen_eq = false;
        let mut is_func = false;
        let mut end = toks.len();
        while j < toks.len() {
            let t = &toks[j];
            if t.is("(") {
                if paren == 0 && brace == 0 && !seen_eq {
                    is_func = true;
                }
                paren += 1;
            } else if t.is(")") {
                paren -= 1;
            } else if t.is("{") {
                brace += 1;
            } else if t.is("}") {
                brace -= 1;
                if brace == 0 && is_func && paren == 0 {
                    end = j + 1;
                    break;
                }
            } else if t.is("=") && paren == 0 && brace == 0 {
                seen_eq = true;
            } else if t.is(";") && paren == 0 && brace == 0 {
                end = j + 1;
                break;
            }
            j += 1;
        }
        let item = &toks[i..end.min(toks.len())];
        i = end.min(toks.len());
        if is_func || item.is_empty() {
            out.extend(item.iter().cloned());
            continue;
        }
        // variable declaration: drop `uniform`/`extern`, add `static`
        let words: Vec<&str> = item
            .iter()
            .take_while(|t| t.ident().is_some())
            .filter_map(|t| t.ident())
            .collect();
        let ty = words.iter().find(|w| {
            !matches!(
                **w,
                "static"
                    | "const"
                    | "uniform"
                    | "extern"
                    | "volatile"
                    | "shared"
                    | "row_major"
                    | "column_major"
            )
        });
        let is_static = words.contains(&"static");
        match ty {
            Some(t) if is_value_type(t) => {
                // user `float4 texsize_x;` -> pass uniform
                if item.len() == 3
                    && item[2].is(";")
                    && let (Some(ty), Some(n)) = (item[0].ident(), item[1].ident())
                    && ty == "float4"
                    && let Some(slot) = texsize_params.iter().position(|p| p == n)
                {
                    let src = format!("static float4 {n} = pu_f[{slot}];");
                    out.extend(crate::lex::lex(&src)?);
                    continue;
                }
                if !is_static {
                    let mut first = Tok::new(T::Ident("static".into()));
                    first.space = false;
                    out.push(first);
                }
                for t in item {
                    if matches!(t.ident(), Some("uniform") | Some("extern")) {
                        continue;
                    }
                    out.push(t.clone());
                }
            }
            _ => out.extend(item.iter().cloned()),
        }
    }
    Ok(out)
}

/// Split the top-level arguments of the call whose `(` is at `open`. Returns the arguments and the index of
/// the closing `)` (`toks.len()` if unbalanced).
fn split_call_args(toks: &[Tok], open: usize) -> (Vec<Vec<Tok>>, usize) {
    let mut depth = 0;
    let mut j = open + 1;
    let mut args: Vec<Vec<Tok>> = vec![Vec::new()];
    while j < toks.len() {
        let t = &toks[j];
        if t.is("(") || t.is("[") {
            depth += 1;
        } else if t.is(")") || t.is("]") {
            if depth == 0 {
                break;
            }
            depth -= 1;
        } else if t.is(",") && depth == 0 {
            args.push(Vec::new());
            j += 1;
            continue;
        }
        if let Some(last) = args.last_mut() {
            last.push(t.clone());
        }
        j += 1;
    }
    (args, j)
}

/// `modf(a, ip)` -> `((a) - ((ip) = trunc(a)))`: glslang lowers modf to a GLSL.std.450 `Modf`
/// struct result that naga's SPIR-V front end cannot represent; the rewrite is equivalent.
fn rewrite_modf(toks: Vec<Tok>) -> Result<Vec<Tok>, TranslateError> {
    let mut out: Vec<Tok> = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        if toks[i].ident() == Some("modf") && toks.get(i + 1).map(|t| t.is("(")).unwrap_or(false) {
            let (args, j) = split_call_args(&toks, i + 1);
            if args.len() == 2 && j < toks.len() {
                let a = rewrite_modf(args[0].clone())?;
                let b = rewrite_modf(args[1].clone())?;
                let src = |v: &[Tok]| crate::lex::tokens_to_string(v);
                let text = format!(
                    "( ( {a} ) - ( ( {b} ) = trunc ( {a} ) ) )",
                    a = src(&a),
                    b = src(&b)
                );
                out.extend(crate::lex::lex(&text)?);
                i = j + 1;
                continue;
            }
        }
        out.push(toks[i].clone());
        i += 1;
    }
    Ok(out)
}

/// projectM's GLSL generator (hlslparser) wraps the argument of `rsqrt`, `sqrt`, `log`, `log2`, `log10` and the base of
/// `pow` in `abs()` (D3D9 takes |x|; `pow(x, 1)` with a literal one returns `x`). Our translator does the same, so the
/// glslang reference text gets the same rewrite.
fn rewrite_abs(toks: Vec<Tok>) -> Result<Vec<Tok>, TranslateError> {
    let mut out: Vec<Tok> = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let name = toks[i].ident().unwrap_or("");
        let nargs = match name {
            "rsqrt" | "sqrt" | "log" | "log2" | "log10" => 1,
            "pow" => 2,
            _ => 0,
        };
        if nargs > 0 && toks.get(i + 1).map(|t| t.is("(")).unwrap_or(false) {
            let (args, j) = split_call_args(&toks, i + 1);
            if args.len() == nargs && j < toks.len() {
                let src = |v: &[Tok]| -> Result<String, TranslateError> {
                    Ok(crate::lex::tokens_to_string(&rewrite_abs(v.to_vec())?))
                };
                let one = nargs == 2
                    && matches!(args[1].as_slice(), [t] if matches!(&t.t, T::Int(1)) || matches!(&t.t, T::Float(f) if *f == 1.0));
                let text = if nargs == 1 {
                    format!("{name} ( abs ( {} ) )", src(&args[0])?)
                } else if one {
                    format!("( {} )", src(&args[0])?)
                } else {
                    format!("pow ( abs ( {} ) , {} )", src(&args[0])?, src(&args[1])?)
                };
                out.extend(crate::lex::lex(&text)?);
                i = j + 1;
                continue;
            }
        }
        out.push(toks[i].clone());
        i += 1;
    }
    Ok(out)
}

/// HLSL source for `glslangValidator -D -V -S frag --hlsl-dx9-compatible -e PS`.
pub fn glslang_source(
    kind: PassKind,
    user_src: &str,
    texsize_params: &[String],
) -> Result<String, TranslateError> {
    let wrapped = wrap::md2_wrapped_hlsl(kind, user_src)?;
    let k = wrapped
        .find("#define rad _rad_ang.x")
        .ok_or_else(|| TranslateError::Wrap("wrapper marker missing".into()))?;
    let user_part = &wrapped[k..];
    // keep the macro section of include.fx (the declarations are replaced by `prelude()`)
    let mut text = prelude();
    for l in wrap::strip_comments(wrap::INCLUDE_FX).lines() {
        if l.trim_start().starts_with("#define") {
            text.push_str(l);
            text.push('\n');
        }
    }
    text.push_str(user_part);
    let toks = pp::preprocess(&text)?;
    // D3DX9 compiles `double` as float; glslang would emit real f64
    let toks: Vec<Tok> = toks
        .into_iter()
        .map(|mut t| {
            if let T::Ident(n) = &t.t
                && let Some(rest) = n.strip_prefix("double")
            {
                t.t = T::Ident(format!("float{rest}"));
            }
            t
        })
        .collect();
    let toks = rewrite_abs(rewrite_modf(rewrite_globals(toks, texsize_params)?)?)?;
    let mut s = String::new();
    for t in &toks {
        match &t.t {
            T::Ident(i) => s.push_str(i),
            T::Int(i) => s.push_str(&i.to_string()),
            T::Float(f) => s.push_str(&format!("{f:?}")),
            T::Punct(p) => s.push_str(p),
        }
        if t.is(";") || t.is("{") || t.is("}") {
            s.push('\n');
        } else {
            s.push(' ');
        }
    }
    Ok(s)
}
