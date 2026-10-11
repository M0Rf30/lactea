//! Semantic differential: our WGSL vs glslang's SPIR-V of the same MD2-wrapped HLSL, rendered on
//! the GPU at 64x64 from fixed deterministic inputs. Gate: max abs diff <= 2/255.
//!
//!   cargo test -p lactea-hlsl --test diff -- --nocapture                  (sample)
//!   cargo test -p lactea-hlsl --test diff semantic_diff_full -- --ignored --nocapture
//! Env: HLSL_DIFF_FILTER=<substring of label>, HLSL_DIFF_STEP=<n> (every n-th case in the sample),
//!      HLSL_SABOTAGE="from=>to" (mutation-test the harness)
mod common;

use common::diffrun::run_diff;
use common::*;

fn select(step: usize) -> Vec<Case> {
    let filt = std::env::var("HLSL_DIFF_FILTER").ok();
    corpus_cases()
        .into_iter()
        .enumerate()
        .filter(|(i, c)| {
            i % step == 0 && filt.as_ref().map(|f| c.label().contains(f)).unwrap_or(true)
        })
        .map(|(_, c)| c)
        .collect()
}

#[test]
fn semantic_diff_sample() {
    let step: usize = std::env::var("HLSL_DIFF_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let cases = select(step);
    if cases.is_empty() {
        return;
    }
    if let Some((pass, fail, _)) = run_diff(cases, "sample") {
        assert!(
            pass * 100 >= (pass + fail) * 97,
            "semantic differential regressed: {pass} pass / {fail} fail"
        );
    }
}

#[test]
#[ignore]
fn semantic_diff_full() {
    let cases = select(1);
    if let Some((pass, fail, _)) = run_diff(cases, "full") {
        assert!(
            pass * 1000 >= (pass + fail) * 995,
            "semantic differential regressed: {pass} pass / {fail} fail"
        );
    }
}
