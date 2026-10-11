//! C `sscanf("%d")` / `sscanf("%f")` emulation (glibc behaviour, C locale).
//!
//! MD2 (`GetFastInt` / `GetFastFloat`) feeds the raw value text of a preset line to
//! `sscanf` and falls back to the default when the conversion count is not 1.  These
//! helpers reproduce which prefixes convert and which fail.

#[inline]
fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// `sscanf(s, "%d", &i) == 1 ? Some(i) : None`.
///
/// Leading whitespace is skipped, one optional sign, then at least one decimal digit; the
/// rest of the string is ignored (`"1.9"` -> 1, `"12abc"` -> 12, `"abc"` -> None).
/// Out-of-range values saturate like `strtol` (64-bit `long`) and are then truncated to
/// 32 bits like glibc's store into an `int`.
pub(crate) fn scan_int(s: &[u8]) -> Option<i32> {
    let mut i = 0;
    while i < s.len() && is_space(s[i]) {
        i += 1;
    }
    let mut neg = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        neg = s[i] == b'-';
        i += 1;
    }
    let start = i;
    let mut v: u64 = 0;
    let mut overflow = false;
    while i < s.len() && s[i].is_ascii_digit() {
        let d = (s[i] - b'0') as u64;
        match v.checked_mul(10).and_then(|x| x.checked_add(d)) {
            Some(n) => v = n,
            None => overflow = true,
        }
        i += 1;
    }
    if i == start {
        return None;
    }
    let l: i64 = if neg {
        if overflow || v > i64::MAX as u64 + 1 {
            i64::MIN
        } else {
            (v as i64).wrapping_neg()
        }
    } else if overflow || v > i64::MAX as u64 {
        i64::MAX
    } else {
        v as i64
    };
    Some(l as i32)
}

/// `sscanf(s, "%f", &f) == 1 ? Some(f) : None` (C locale, glibc semantics).
///
/// Accepts decimal floats (`.5`, `5.`, `1e-3`), `inf`/`infinity`/`nan` (any case) and hex
/// floats (`0x1.8p3`).  Like glibc, an exponent marker that is not followed by digits
/// (`"1e"`, `"2e+"`) is a *matching failure* (None) rather than a prefix conversion.
pub(crate) fn scan_float(s: &[u8]) -> Option<f32> {
    let mut i = 0;
    while i < s.len() && is_space(s[i]) {
        i += 1;
    }
    let mut neg = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        neg = s[i] == b'-';
        i += 1;
    }
    let rest = &s[i..];
    let starts = |w: &[u8]| rest.len() >= w.len() && rest[..w.len()].eq_ignore_ascii_case(w);
    // inf / infinity / nan
    if starts(b"inf") {
        return Some(if neg {
            f32::NEG_INFINITY
        } else {
            f32::INFINITY
        });
    }
    if starts(b"nan") {
        return Some(if neg { -f32::NAN } else { f32::NAN });
    }
    // hex float
    if rest.len() >= 2 && rest[0] == b'0' && (rest[1] == b'x' || rest[1] == b'X') {
        return scan_hex(&rest[2..]).map(|v| if neg { -v } else { v });
    }
    // decimal: take the prefix the way glibc's vfscanf collects it, then require strict
    // consumption by the float grammar.
    let mut got_digit = false;
    let mut got_dot = false;
    let mut exp_start = None;
    let mut j = 0;
    while j < rest.len() {
        let c = rest[j];
        if c.is_ascii_digit() {
            got_digit = true;
        } else if c == b'.' && !got_dot && exp_start.is_none() {
            got_dot = true;
        } else if (c == b'e' || c == b'E') && got_digit && exp_start.is_none() {
            // optional sign directly after the exponent marker
            if j + 1 < rest.len() && (rest[j + 1] == b'+' || rest[j + 1] == b'-') {
                j += 1;
            }
            exp_start = Some(j + 1);
        } else {
            break;
        }
        j += 1;
    }
    if !got_digit {
        return None;
    }
    // an exponent marker with no digits after it is a matching failure
    if exp_start.is_some_and(|e| e >= j) {
        return None;
    }
    // only ASCII digits, '.', 'e'/'E' and signs were consumed
    let v: f32 = std::str::from_utf8(&rest[..j]).ok()?.parse().ok()?;
    Some(if neg { -v } else { v })
}

fn scan_hex(s: &[u8]) -> Option<f32> {
    let mut mant: f64 = 0.0;
    let mut exp: i32 = 0;
    let mut digits = 0;
    let mut j = 0;
    let mut dot = false;
    while j < s.len() {
        let c = s[j];
        if let Some(d) = (c as char).to_digit(16) {
            mant = mant * 16.0 + d as f64;
            if dot {
                exp -= 4;
            }
            digits += 1;
        } else if c == b'.' && !dot {
            dot = true;
        } else {
            break;
        }
        j += 1;
    }
    if digits == 0 {
        return None;
    }
    if j < s.len() && (s[j] == b'p' || s[j] == b'P') {
        let mut k = j + 1;
        let mut eneg = false;
        if k < s.len() && (s[k] == b'+' || s[k] == b'-') {
            eneg = s[k] == b'-';
            k += 1;
        }
        let st = k;
        let mut e: i32 = 0;
        while k < s.len() && s[k].is_ascii_digit() {
            e = e.saturating_mul(10).saturating_add((s[k] - b'0') as i32);
            k += 1;
        }
        if k == st {
            return None;
        }
        exp = exp.saturating_add(if eneg { -e } else { e });
    }
    Some((mant * (2.0f64).powi(exp)) as f32)
}

/// C++ `std::stoi(s)` (base 10) as `Some(v)`; `None` where it throws (`invalid_argument`: no
/// digits; `out_of_range`: outside `int`).  Leading whitespace, one sign and the digit run
/// are read, the rest is ignored (`"3.9"` -> 3).  projectM's `PresetFileParser::GetInt`.
pub(crate) fn stoi(s: &[u8]) -> Option<i32> {
    let mut i = 0;
    while i < s.len() && is_space(s[i]) {
        i += 1;
    }
    let mut neg = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        neg = s[i] == b'-';
        i += 1;
    }
    let start = i;
    let mut v: i128 = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        v = (v * 10 + (s[i] - b'0') as i128).min(i128::from(u64::MAX));
        i += 1;
    }
    if i == start {
        return None;
    }
    let v = if neg { -v } else { v };
    i32::try_from(v).ok()
}

/// C++ `std::stof(s)` as `Some(v)`; `None` where it throws (`invalid_argument`: nothing
/// converts; `out_of_range`: `strtof` set `ERANGE`: overflow, or a non-zero literal that
/// underflows to zero / a subnormal).  Unlike `sscanf("%f")` ([`scan_float`]) the longest
/// valid prefix converts, so `"1e"` is 1.  projectM's `PresetFileParser::GetFloat`.
pub(crate) fn stof(s: &[u8]) -> Option<f32> {
    let mut i = 0;
    while i < s.len() && is_space(s[i]) {
        i += 1;
    }
    let mut j = i;
    if j < s.len() && (s[j] == b'+' || s[j] == b'-') {
        j += 1;
    }
    let rest = &s[j..];
    let is_hex = rest.len() >= 2 && rest[0] == b'0' && (rest[1] == b'x' || rest[1] == b'X');
    let word = |w: &[u8]| rest.len() >= w.len() && rest[..w.len()].eq_ignore_ascii_case(w);
    if word(b"inf") || word(b"nan") {
        return scan_float(&s[i..]);
    }
    if is_hex {
        // `0x` + hex digits (strtof reads the "0" alone when no hex digit follows)
        let negative = s.get(i) == Some(&b'-');
        return Some(scan_float(&s[i..]).unwrap_or(if negative { -0.0 } else { 0.0 }));
    }
    // decimal: digits [. digits] [e[+-]digits]; the exponent counts only with digits
    let mut k = j;
    let mut mant_digits = 0;
    while k < s.len() && s[k].is_ascii_digit() {
        k += 1;
        mant_digits += 1;
    }
    if k < s.len() && s[k] == b'.' {
        k += 1;
        while k < s.len() && s[k].is_ascii_digit() {
            k += 1;
            mant_digits += 1;
        }
    }
    if mant_digits == 0 {
        return None;
    }
    if k < s.len() && (s[k] == b'e' || s[k] == b'E') {
        let mut e = k + 1;
        if e < s.len() && (s[e] == b'+' || s[e] == b'-') {
            e += 1;
        }
        let ds = e;
        while e < s.len() && s[e].is_ascii_digit() {
            e += 1;
        }
        if e > ds {
            k = e;
        }
    }
    let text = std::str::from_utf8(&s[i..k]).ok()?;
    let v: f32 = text.parse().ok()?;
    let nonzero_literal = text
        .bytes()
        .take_while(|&b| b != b'e' && b != b'E')
        .any(|b| (b'1'..=b'9').contains(&b));
    if v.is_infinite() || (nonzero_literal && (v == 0.0 || v.is_subnormal())) {
        return None; // ERANGE
    }
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ints() {
        assert_eq!(scan_int(b"12"), Some(12));
        assert_eq!(scan_int(b"  -7xyz"), Some(-7));
        assert_eq!(scan_int(b"1.9"), Some(1));
        assert_eq!(scan_int(b"+3"), Some(3));
        assert_eq!(scan_int(b""), None);
        assert_eq!(scan_int(b"abc"), None);
        assert_eq!(scan_int(b"-"), None);
        assert_eq!(scan_int(b"= 1"), None);
        assert_eq!(scan_int(b"0x10"), Some(0));
        assert_eq!(scan_int(b"99999999999"), Some(99999999999i64 as i32));
        assert_eq!(scan_int(b"99999999999999999999999"), Some(-1)); // LONG_MAX as int
    }

    #[test]
    fn floats() {
        assert_eq!(scan_float(b"0.5"), Some(0.5));
        assert_eq!(scan_float(b" .5"), Some(0.5));
        assert_eq!(scan_float(b"5."), Some(5.0));
        assert_eq!(scan_float(b"-1.25e2x"), Some(-125.0));
        assert_eq!(scan_float(b"1e"), None);
        assert_eq!(scan_float(b"1e+"), None);
        assert_eq!(scan_float(b"1e+x"), None);
        assert_eq!(scan_float(b"1abc"), Some(1.0));
        assert_eq!(scan_float(b"1.5.3"), Some(1.5));
        assert_eq!(scan_float(b"abc"), None);
        assert_eq!(scan_float(b"."), None);
        assert_eq!(scan_float(b"-"), None);
        assert_eq!(scan_float(b"1,5"), Some(1.0));
        assert_eq!(scan_float(b"0x1.8p1"), Some(3.0));
        assert_eq!(scan_float(b"inf"), Some(f32::INFINITY));
        assert!(scan_float(b"-NaN").unwrap().is_nan());
        assert_eq!(scan_float(b"1.000000"), Some(1.0));
    }
}

#[cfg(test)]
mod pm_tests {
    use super::*;

    #[test]
    fn stoi_like_cpp() {
        assert_eq!(stoi(b"12"), Some(12));
        assert_eq!(stoi(b"  -7xyz"), Some(-7));
        assert_eq!(stoi(b"3.9"), Some(3));
        assert_eq!(stoi(b"0x10"), Some(0));
        assert_eq!(stoi(b""), None);
        assert_eq!(stoi(b"= 1"), None);
        assert_eq!(stoi(b"2147483647"), Some(i32::MAX));
        assert_eq!(stoi(b"2147483648"), None);
        assert_eq!(stoi(b"-2147483648"), Some(i32::MIN));
        assert_eq!(stoi(b"99999999999999999999999"), None);
    }

    #[test]
    fn stof_like_cpp() {
        assert_eq!(stof(b"0.5"), Some(0.5));
        assert_eq!(stof(b" .5"), Some(0.5));
        assert_eq!(stof(b"5."), Some(5.0));
        assert_eq!(stof(b"-1.25e2x"), Some(-125.0));
        assert_eq!(stof(b"1e"), Some(1.0));
        assert_eq!(stof(b"1e+x"), Some(1.0));
        assert_eq!(stof(b"1.5.3"), Some(1.5));
        assert_eq!(stof(b"abc"), None);
        assert_eq!(stof(b"."), None);
        assert_eq!(stof(b"= 1.5"), None);
        assert_eq!(stof(b"1e50"), None);
        assert_eq!(stof(b"1e-50"), None);
        assert_eq!(stof(b"0.0"), Some(0.0));
        assert_eq!(stof(b"0x1.8p1"), Some(3.0));
        assert_eq!(stof(b"inf"), Some(f32::INFINITY));
    }
}
