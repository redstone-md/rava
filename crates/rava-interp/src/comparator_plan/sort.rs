//! Sorting by a plan, and the census that prices one.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::comparator_plan::*;

/// Bottom-up stable merge over `0..n`, driven by a FALLIBLE comparison — the schedule the
/// authoritative `Vm::compare` sort in `stream::sort_values` runs, lifted off the heap
/// arrays so it can be exercised without guest code. Reproducing this schedule exactly is
/// what makes the memo's extraction set the authoritative one: which clauses get asked for
/// is decided entirely by which pairs get compared.
pub(crate) fn merge_sort_indices<F>(n: usize, mut compare: F) -> Result<Vec<usize>, VmError>
where
    F: FnMut(usize, usize) -> Result<i32, VmError>,
{
    let mut src: Vec<usize> = (0..n).collect();
    let mut dst: Vec<usize> = vec![0; n];
    let mut width = 1usize;
    while width < n {
        let mut i = 0usize;
        while i < n {
            let mid = (i + width).min(n);
            let hi = (i + 2 * width).min(n);
            let (mut a, mut b, mut k) = (i, mid, i);
            while a < mid && b < hi {
                // `<= 0` takes the LEFT run on a tie — the stability rule, and the same
                // test the heap-array merge makes.
                if compare(src[a], src[b])? <= 0 {
                    dst[k] = src[a];
                    a += 1;
                } else {
                    dst[k] = src[b];
                    b += 1;
                }
                k += 1;
            }
            while a < mid {
                dst[k] = src[a];
                a += 1;
                k += 1;
            }
            while b < hi {
                dst[k] = src[b];
                b += 1;
                k += 1;
            }
            i += 2 * width;
        }
        std::mem::swap(&mut src, &mut dst);
        width *= 2;
    }
    Ok(src)
}

pub fn cmp_plan_census_report() {
    if !cmp_plan_census_enabled() {
        return;
    }
    eprintln!(
        "[CMP-PLAN] plans built {} | keyed comparators refused for a non-Ref key fn {}",
        PLANS_BUILT.load(std::sync::atomic::Ordering::Relaxed),
        KEYFN_NOT_REF.load(std::sync::atomic::Ordering::Relaxed),
    );
    eprintln!(
        "[AL-DISPATCH] arena lambdas reaching the direct-call entry build {} | \
         falling to the general apply {}",
        AL_DIRECT_ENTRY.load(std::sync::atomic::Ordering::Relaxed),
        AL_GENERAL_APPLY.load(std::sync::atomic::Ordering::Relaxed),
    );
    // The entry BUILD count cannot say whether the entries are ever SERVED — the same
    // distinction that made a 212,434 -> 4,607 fill collapse read like success while the
    // machine-code arm reached 0.54% of calls. `callback_stats` carries the direct HIT count
    // and the miss REASONS, so build/serve/refuse are read off one line.
    // Requires RAVA_CALLBACK_STATS (or RAVA_HEARTBEAT) to be armed as well — those counters
    // are inert otherwise, and a silent zero would read as "never served".
    eprintln!("[AL-DISPATCH] direct-call arm: {}", crate::jit::callback_stats());
    #[cfg(feature = "next-gen-gc")]
    eprintln!(
        "[AL-BIRTHS] arena lambdas allocated {} (vs the census's LIVE count — the \
         pinned-region go/no-go)",
        crate::nextgen::ARENA_LAMBDA_BIRTHS.load(std::sync::atomic::Ordering::Relaxed),
    );
    #[cfg(feature = "next-gen-gc")]
    {
        let a: Vec<String> = crate::nextgen::ARENA_LAMBDA_ARITY
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let n = c.load(std::sync::atomic::Ordering::Relaxed);
                if i == 8 { format!("8+={n}") } else { format!("{i}={n}") }
            })
            .collect();
        eprintln!("[AL-ARITY] births by capture arity: {}", a.join(" "));
        eprintln!(
            "[SHAPE-WIDTH] u32-mask scans over a >32-cell object: {} | largest cell count seen: {}",
            crate::nextgen::SHAPE_WIDE_SCANS.load(std::sync::atomic::Ordering::Relaxed),
            crate::nextgen::SHAPE_MAX_NF.load(std::sync::atomic::Ordering::Relaxed),
        );
    }
}
