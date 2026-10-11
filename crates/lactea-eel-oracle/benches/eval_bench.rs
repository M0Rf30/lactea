//! `cargo bench -p lactea-eel-oracle` : ns per evaluation of representative MilkDrop programs,
//! lactea-eel vs projectm-eval (C interpreter).
//!
//! Per-vertex scenario: 49*37 mesh points x 600 frames = 1,087,800 evaluations; every
//! evaluation sets x/y/rad/ang (like the engine does per vertex), runs the program and reads
//! back the outputs.  Both sides use pre-resolved variable handles.
//! Unix only: CPU time comes from `clock_gettime(CLOCK_THREAD_CPUTIME_ID)`.

#[cfg(not(unix))]
compile_error!("eval_bench needs a Unix target (clock_gettime thread CPU time)");

use lactea_eel::{Context, GlobalMemory};
use lactea_eel_oracle::{OracleContext, OracleGlobals};
use std::hint::black_box;
use std::sync::Arc;

/// CPU time of the calling thread in seconds (immune to other processes hogging the machine).
fn cpu_now() -> f64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid pointer to a timespec; CLOCK_THREAD_CPUTIME_ID is always available on Linux.
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    ts.tv_sec as f64 + ts.tv_nsec as f64 * 1e-9
}

const GX: usize = 49;
const GY: usize = 37;
const FRAMES: usize = 600;

const PER_VERTEX: &str = "
zoom = zoom + 0.05 * sin(rad * 3 + time) * (1 - rad);
rot = rot + 0.02 * cos(ang * 2 + time * 0.5) * rad;
dx = dx + 0.01 * sin(y * 10 + time) + q1 * 0.1;
dy = dy + 0.01 * cos(x * 10 + time) - q2 * 0.1;
d = sqrt(sqr(x - 0.5) + sqr(y - 0.5));
zoom = zoom + if(above(d, 0.3), 0.01 * sin(d * 20 + bass), -0.01 * cos(d * 20 - treb));
q = max(0, 1 - rad * rad);
sx = sx + q * 0.001 * (mid + 1);
sy = sy + abs(sin(ang * 3 + time)) * 0.001;
warp = warp + equal(int(x * 8) % 2, int(y * 8) % 2) * 0.01;
cx = cx + bnot(below(rad, 0.2)) * 0.002 * sin(time * 2);
";

const ARITH: &str = "
a = x * 1.5 + y * 0.5 - rad * 0.25;
b = a * a + ang * 0.1;
c = (b - a) / (rad + 1.5) + a * b - 0.3;
zoom = zoom + c * 0.01 + a * b * 0.001;
rot = rot + (a + b + c) * 0.0003;
";

const MEMLOOP: &str = "
i = 0;
loop(8, megabuf(i) = megabuf(i) * 0.9 + x * 0.1 + i; i += 1);
zoom = zoom + megabuf(3) * 0.001 + megabuf(7) * 0.0001;
t = 0; k = 0;
while(k += 1; t += k * 0.01; k < 4);
rot = rot + t * 0.001 + sin(y * 6) * 0.0005;
";

struct Result {
    ns_rust: f64,
    ns_c: f64,
    checksum_equal: bool,
}

fn bench(code: &str) -> Result {
    // --- Rust
    let mut rctx = Context::new(Arc::new(GlobalMemory::new()));
    let (x, y, rad, ang, time) = (
        rctx.var("x"),
        rctx.var("y"),
        rctx.var("rad"),
        rctx.var("ang"),
        rctx.var("time"),
    );
    let (zoom, rot, dx, dy) = (
        rctx.var("zoom"),
        rctx.var("rot"),
        rctx.var("dx"),
        rctx.var("dy"),
    );
    let prog = rctx.compile(code).expect("rust compile");

    // --- C
    let mut cctx = OracleContext::new(OracleGlobals::new());
    let cprog = cctx.compile(code).expect("C compile");
    let (cx, cy, crad, cang, ctime) = (
        cctx.handle("x"),
        cctx.handle("y"),
        cctx.handle("rad"),
        cctx.handle("ang"),
        cctx.handle("time"),
    );
    let (czoom, crot, cdx, cdy) = (
        cctx.handle("zoom"),
        cctx.handle("rot"),
        cctx.handle("dx"),
        cctx.handle("dy"),
    );

    // precomputed per-vertex inputs (same for both)
    let mut pts = Vec::with_capacity(GX * GY);
    for j in 0..GY {
        for i in 0..GX {
            let px = i as f64 / (GX - 1) as f64;
            let py = j as f64 / (GY - 1) as f64;
            let (dxp, dyp) = (px - 0.5, py - 0.5);
            let r = (dxp * dxp + dyp * dyp).sqrt() * 2f64.sqrt();
            pts.push((px, py, r, dyp.atan2(dxp)));
        }
    }

    let mut best_r = f64::MAX;
    let mut best_c = f64::MAX;
    let mut sum_r = 0.0;
    let mut sum_c = 0.0;
    for _round in 0..9 {
        sum_r = 0.0;
        let t0 = cpu_now();
        for f in 0..FRAMES {
            let t = f as f64 / 60.0;
            rctx.set(time, t);
            for &(px, py, r, a) in &pts {
                rctx.set(x, px);
                rctx.set(y, py);
                rctx.set(rad, r);
                rctx.set(ang, a);
                rctx.set(zoom, 1.0);
                rctx.set(rot, 0.0);
                rctx.set(dx, 0.0);
                rctx.set(dy, 0.0);
                rctx.run(black_box(&prog));
                sum_r += rctx.get(zoom) + rctx.get(rot) + rctx.get(dx) + rctx.get(dy);
            }
        }
        best_r = best_r.min(cpu_now() - t0);

        sum_c = 0.0;
        let t0 = cpu_now();
        // SAFETY: every handle was obtained from `cctx` above and `cctx` outlives this loop.
        unsafe {
            for f in 0..FRAMES {
                let t = f as f64 / 60.0;
                cctx.set_h(ctime, t);
                for &(px, py, r, a) in &pts {
                    cctx.set_h(cx, px);
                    cctx.set_h(cy, py);
                    cctx.set_h(crad, r);
                    cctx.set_h(cang, a);
                    cctx.set_h(czoom, 1.0);
                    cctx.set_h(crot, 0.0);
                    cctx.set_h(cdx, 0.0);
                    cctx.set_h(cdy, 0.0);
                    cctx.run(black_box(&cprog));
                    sum_c +=
                        cctx.get_h(czoom) + cctx.get_h(crot) + cctx.get_h(cdx) + cctx.get_h(cdy);
                }
            }
        }
        best_c = best_c.min(cpu_now() - t0);
    }
    let n = (GX * GY * FRAMES) as f64;
    Result {
        ns_rust: best_r / n * 1e9,
        ns_c: best_c / n * 1e9,
        checksum_equal: sum_r.to_bits() == sum_c.to_bits() || (sum_r.is_nan() && sum_c.is_nan()),
    }
}

fn main() {
    println!(
        "evaluations per run: {} ({}x{} points x {} frames); best of 9 rounds, thread CPU time",
        GX * GY * FRAMES,
        GX,
        GY,
        FRAMES
    );
    println!(
        "{:<14} {:>14} {:>14} {:>8}  results-bitwise-equal",
        "program", "lactea-eel", "projectm-eval", "ratio"
    );
    for (name, code) in [
        ("harness-only", "zoom = zoom;"),
        ("per-vertex", PER_VERTEX),
        ("arith", ARITH),
        ("megabuf+loop", MEMLOOP),
    ] {
        let r = bench(code);
        println!(
            "{:<14} {:>11.1} ns {:>11.1} ns {:>7.2}x  {}",
            name,
            r.ns_rust,
            r.ns_c,
            r.ns_c / r.ns_rust,
            r.checksum_equal
        );
    }
}
