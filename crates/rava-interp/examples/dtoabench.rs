//! WHAT COSTS 640 ns IN `java_dtoa`? Four timings over one distribution, no boot involved.
//!
//! WHY. Ledger row 312: a `DoubleArrayList` used as a map key is rendered to decimal text, one
//! `Double.toString` per element, **2,154,138 doubles a boot at ~640 ns each = 1.38 s**. The
//! suspect is one line of `value.rs`:
//!
//! ```ignore
//! java_fp_layout(&java_significand(&format!("{v:e}"), |p| format!("{v:.p$e}")))
//! ```
//!
//! The first `format!` exists ONLY to count the shortest form's digits; `java_significand` then
//! formats a SECOND time at fixed precision (that is where the JDK's half-to-even tie-break comes
//! from), may trim with a THIRD, and `java_fp_layout` builds a fourth string. Two float
//! formattings and 2-4 heap allocations per double.
//!
//! Eight mechanism hypotheses have already been refuted by measurement in this hunt, so this one
//! is decomposed rather than asserted:
//!
//!   `shortest`  just `format!("{v:e}")`            — one shortest-repr render + one alloc
//!   `exact`     just `format!("{v:.15e}")`         — one fixed-precision render + one alloc
//!   `both`      the two of them, nothing else      — the floor for the current algorithm
//!   `full`      `java_dtoa` as it ships            — the two renders plus layout and trimming
//!
//! `full - both` is what the layout and trimming cost; `both` is what the algorithm costs even if
//! every allocation around it were free. If `both` is most of `full`, the fix is the ALGORITHM
//! (one render instead of two) and not the allocator.
//!
//!   cargo run --release -p rava-interp --features next-gen-gc --example dtoabench -- 2000000

use rava_interp::java_dtoa;

/// The boot's own distribution, as far as it can be reproduced without one: worldgen density and
/// climate parameters are ordinary magnitudes with long mantissas, not round numbers. A benchmark
/// over `1.0, 2.0, 3.0` would measure the fast path of a formatter that never sees it.
fn corpus(n: usize) -> Vec<f64> {
    let mut v = Vec::with_capacity(n);
    // A cheap LCG — `Math::random` is unavailable in a workflow script and a fixed seed keeps the
    // four arms comparing the SAME values, which is the whole point of the harness.
    let mut s: u64 = 0x2545_F491_4F6C_DD1D;
    for i in 0..n {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let mantissa = (s >> 11) as f64 / (1u64 << 53) as f64;
        // Spread over the exponent range worldgen actually produces: roughly 1e-4 .. 1e4.
        let exp = ((i % 9) as i32) - 4;
        v.push(mantissa * 10f64.powi(exp));
    }
    v
}

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(2_000_000);
    // THE MEMO'S OWN CORPUS. `RAVA_DTOA_CENSUS` on a real boot reads ~300,000 renders over
    // **22 distinct values**, so a corpus of 2 M distinct doubles measures the memo's WORST case
    // (100% misses) and says nothing about the case it exists for. `-realistic` replays the boot's
    // shape: the same few values, over and over.
    let realistic = std::env::args().any(|a| a == "-realistic");
    let xs = if realistic {
        let pool: Vec<f64> = corpus(22);
        (0..n).map(|i| pool[i % pool.len()]).collect()
    } else {
        corpus(n)
    };
    if realistic {
        println!("corpus: 22 DISTINCT values replayed (the boot's shape)");
    }
    println!("n={n} doubles, same values for every arm");
    println!("{:<12} {:>12} {:>12}", "arm", "total ms", "ns/value");

    let mut best: Vec<(&str, f64)> = Vec::new();
    // ABBA-ish: each arm runs twice and keeps its minimum, so a scheduling hiccup in one pass
    // cannot decide the comparison.
    for _ in 0..2 {
        for (name, f) in [
            ("shortest", 0u8),
            ("exact", 1),
            ("both", 2),
            ("full", 3),
        ] {
            let t0 = std::time::Instant::now();
            let mut sink = 0usize;
            for &x in &xs {
                let s = match f {
                    0 => format!("{x:e}"),
                    1 => format!("{x:.15e}"),
                    2 => {
                        let a = format!("{x:e}");
                        let b = format!("{x:.15e}");
                        sink += a.len();
                        b
                    }
                    _ => java_dtoa(x),
                };
                sink += s.len();
            }
            let ms = t0.elapsed().as_secs_f64() * 1e3;
            std::hint::black_box(sink);
            match best.iter_mut().find(|(k, _)| *k == name) {
                Some((_, v)) if *v <= ms => {}
                Some((_, v)) => *v = ms,
                None => best.push((name, ms)),
            }
        }
    }
    for (name, ms) in &best {
        println!("{name:<12} {ms:>12.1} {:>12.1}", ms * 1e6 / n as f64);
    }
    let get = |k: &str| best.iter().find(|(n, _)| *n == k).map(|(_, v)| *v).unwrap_or(0.0);
    println!();
    println!(
        "algorithm floor (both) is {:.0}% of full; layout+trim is the remaining {:.0}%",
        get("both") * 100.0 / get("full").max(1e-9),
        (get("full") - get("both")) * 100.0 / get("full").max(1e-9)
    );
    println!("reference: the boot renders 2,154,138 doubles for map keys at ~640 ns each (row 312)");
}
