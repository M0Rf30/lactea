//! Small shared helpers: ordered parallel map and JSON-safe float serialisation.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Apply `f` to every item on up to `jobs` OS threads, returning results in input order.
/// Work is handed out one item at a time, so memory stays bounded by `jobs` in-flight items.
pub fn par_map<T: Sync, R: Send>(items: &[T], jobs: usize, f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let jobs = jobs.clamp(1, items.len().max(1));
    if jobs == 1 {
        return items.iter().map(&f).collect();
    }
    let next = AtomicUsize::new(0);
    let mut all: Vec<(usize, R)> = Vec::with_capacity(items.len());
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..jobs)
            .map(|_| {
                s.spawn(|| {
                    let mut local = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= items.len() {
                            break;
                        }
                        local.push((i, f(&items[i])));
                    }
                    local
                })
            })
            .collect();
        for h in handles {
            match h.join() {
                Ok(v) => all.extend(v),
                Err(e) => std::panic::resume_unwind(e),
            }
        }
    });
    all.sort_by_key(|(i, _)| *i);
    all.into_iter().map(|(_, r)| r).collect()
}

/// Default worker count: available parallelism.
pub fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

/// Median of a slice (NaN-free expected; `inf` allowed). Empty -> NaN.
pub(crate) fn median(v: &[f64]) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let n = s.len();
    if n % 2 == 1 {
        s[n / 2]
    } else {
        let (a, b) = (s[n / 2 - 1], s[n / 2]);
        if a == b { a } else { (a + b) / 2.0 }
    }
}

/// serde adapter: finite floats as JSON numbers, `NaN`/`±inf` as the strings
/// `"nan"`, `"inf"`, `"-inf"`; reads numbers, those strings, and `null` (-> NaN).
pub mod jf64 {
    use serde::{Deserialize, Deserializer, Serializer};

    /// Writes finite values as numbers and non-finite ones as strings.
    pub fn serialize<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
        if v.is_nan() {
            s.serialize_str("nan")
        } else if v.is_infinite() {
            s.serialize_str(if *v > 0.0 { "inf" } else { "-inf" })
        } else {
            s.serialize_f64(*v)
        }
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Num(f64),
        Str(String),
        Null(()),
    }

    /// Reads a number, a `"nan"`/`"inf"`/`"-inf"` string, or `null` (as NaN).
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
        match Raw::deserialize(d)? {
            Raw::Num(v) => Ok(v),
            Raw::Null(()) => Ok(f64::NAN),
            Raw::Str(s) => s.trim().parse::<f64>().map_err(serde::de::Error::custom),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn par_map_preserves_order() {
        let items: Vec<u32> = (0..100).collect();
        let r = par_map(&items, 8, |x| x * 2);
        assert_eq!(r, (0..100).map(|x| x * 2).collect::<Vec<_>>());
    }

    #[test]
    fn median_basic() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[1.0, 2.0, 3.0, 4.0]), 2.5);
        assert_eq!(median(&[f64::INFINITY, f64::INFINITY]), f64::INFINITY);
        assert!(median(&[]).is_nan());
    }
}
