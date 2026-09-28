//! Part 1 of the pool tests.

use super::*;

#[test]
fn parallel_for_covers_every_index_exactly_once() {
    let _g = guard();
    let n = 10_000i64;
    let hits: Vec<AtomicUsize> = (0..n).map(|_| AtomicUsize::new(0)).collect();
    let sum = AtomicI64::new(0);
    let pool = RavaPool::new(4);
    let r = pool.parallel_for(0, n, 64, |i| {
        hits[i as usize].fetch_add(1, Ordering::Relaxed);
        sum.fetch_add(i, Ordering::Relaxed);
        Ok(())
    });
    assert_eq!(r, Ok(()));
    assert!(hits.iter().all(|h| h.load(Ordering::Relaxed) == 1), "each index runs exactly once");
    assert_eq!(sum.load(Ordering::Relaxed), (0..n).sum::<i64>(), "full coverage");
}

#[test]
fn parallel_for_is_actually_concurrent_across_workers() {
    let _g = guard();
    // A bounded rendezvous forces real overlap deterministically (no reliance on
    // the scheduler spreading fast bodies): each body bumps `active`, records the
    // peak concurrency, then spin-waits (bounded) for a partner before leaving.
    let pool = RavaPool::new(4);
    let active = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);
    let threads: Vec<AtomicUsize> = (0..200).map(|_| AtomicUsize::new(0)).collect();
    let r = pool.parallel_for(0, 200, 4, |i| {
        let cur = active.fetch_add(1, Ordering::SeqCst) + 1;
        peak.fetch_max(cur, Ordering::SeqCst);
        let mut spins = 0;
        while active.load(Ordering::SeqCst) < 2 && spins < 200_000 {
            std::hint::spin_loop();
            spins += 1;
        }
        let id = format!("{:?}", std::thread::current().id());
        let h = id.bytes().fold(0usize, |a, b| a.wrapping_mul(131).wrapping_add(b as usize));
        threads[i as usize].store(h, Ordering::Relaxed);
        active.fetch_sub(1, Ordering::SeqCst);
        Ok(())
    });
    assert_eq!(r, Ok(()));
    let distinct: std::collections::HashSet<usize> =
        threads.iter().map(|t| t.load(Ordering::Relaxed)).collect();
    // On a multi-core host the rendezvous guarantees ≥2 lanes overlapped; on a
    // single hardware thread (no true parallelism) accept the serial outcome.
    if std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) > 1 {
        assert!(
            peak.load(Ordering::SeqCst) >= 2 && distinct.len() >= 2,
            "expected ≥2 lanes overlapping; peak={} distinct={}",
            peak.load(Ordering::SeqCst),
            distinct.len()
        );
    }
}

#[test]
fn tlab_overflow_aborts_region_for_serial_fallback() {
    let _g = guard();
    let pool = RavaPool::new(4);
    let done = AtomicUsize::new(0);
    // Body "overflows its TLAB" at index 5000 → the whole region must abort.
    let r = pool.parallel_for(0, 10_000, 16, |i| {
        done.fetch_add(1, Ordering::Relaxed);
        if i == 5000 {
            Err(()) // simulate nursery overflow
        } else {
            Ok(())
        }
    });
    assert_eq!(r, Err(RegionAbort), "region reports abort to the driver");
    assert!(parallel::abort_requested(), "global abort flag set for the alloc path");
    // Not every index ran (the abort short-circuited the remaining chunks).
    assert!(done.load(Ordering::Relaxed) < 10_000, "abort stopped further work");
    parallel::clear_abort();
}

#[test]
fn workers_allocate_arena_strings_isolated_gilfree() {
    let _g = guard();
    // Ample per-worker nursery → no overflow. Each lane allocates an ArenaString
    // GIL-free into its OWN nursery and validates the round-trip; every allocation
    // lands at a distinct address (isolation — no shared alloc pointer / no race).
    let n = 400i64;
    let pool = RavaPool::with_nursery(4, 4 * 1024 * 1024);
    let addrs: Vec<AtomicUsize> = (0..n).map(|_| AtomicUsize::new(0)).collect();
    let ok = AtomicUsize::new(0);
    let r = pool.parallel_for(0, n, 8, |i| {
        assert!(parallel::gilfree(), "worker runs GIL-free");
        let s = format!("arena-worker-string-{}", i);
        match nextgen::tlab_alloc_string(s.as_bytes()) {
            Some(p) => {
                // Round-trip the layout: class tag, byte length, and bytes.
                assert!(nextgen::is_arena_string(p));
                assert_eq!(nextgen::arena_str_len(p), s.len());
                assert_eq!(nextgen::arena_str_bytes(p), s.as_bytes());
                addrs[i as usize].store(p.as_ptr() as usize, Ordering::Relaxed);
                ok.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            None => Err(()),
        }
    });
    assert_eq!(r, Ok(()), "ample nursery → region completes");
    assert_eq!(ok.load(Ordering::Relaxed), n as usize, "every index allocated");
    let distinct: std::collections::HashSet<usize> =
        addrs.iter().map(|a| a.load(Ordering::Relaxed)).collect();
    assert_eq!(distinct.len(), n as usize, "every allocation isolated (distinct address)");
}

#[test]
fn worker_nursery_overflow_aborts_then_serial_fallback_is_clean() {
    let _g = guard();
    // (1) Tiny per-worker nursery → a worker's bump overflows mid-region; it must
    //     abort (NOT collect GIL-free) → the coordinator returns RegionAbort.
    let n = 20_000i64;
    let pool = RavaPool::with_nursery(4, 4096);
    let r = pool.parallel_for(0, n, 16, |i| {
        let s = format!("overflow-pressure-arena-string-number-{}", i);
        match nextgen::tlab_alloc_string(s.as_bytes()) {
            Some(_) => Ok(()),
            None => Err(()), // nursery exhausted GIL-free → region abort
        }
    });
    assert_eq!(r, Err(RegionAbort), "tiny nursery overflows → region aborts");
    assert!(parallel::abort_requested(), "the gilfree alloc path raised the abort signal");

    // (2) SERIAL FALLBACK: the driver re-runs under the GIL. Prove the same
    //     overflow on a NON-gilfree thread is a benign collect signal — it never
    //     raises a region abort. (Done on a throwaway thread with a tiny nursery
    //     so the tiny TLAB dies with the thread and can't leak into later tests.)
    parallel::clear_abort();
    let handle = std::thread::spawn(move || {
        gil::acquire(); // serial path holds the GIL
        nextgen::worker_tlab_init(4096); // tiny nursery, but gilfree()==false here
        let mut overflowed = false;
        for i in 0..n {
            let s = format!("overflow-pressure-arena-string-number-{}", i);
            if nextgen::tlab_alloc_string(s.as_bytes()).is_none() {
                overflowed = true;
                break;
            }
        }
        gil::release();
        (overflowed, parallel::abort_requested())
    });
    let (overflowed, aborted) = handle.join().unwrap();
    assert!(overflowed, "the tiny serial nursery also fills");
    assert!(!aborted, "a NON-gilfree overflow is a collect signal, never a region abort");
}

#[test]
fn empty_and_singleton_ranges_are_noops() {
    let _g = guard();
    let pool = RavaPool::new(4);
    assert_eq!(pool.parallel_for(5, 5, 8, |_| Ok(())), Ok(()));
    let c = AtomicUsize::new(0);
    assert_eq!(pool.parallel_for(7, 8, 8, |_| { c.fetch_add(1, Ordering::Relaxed); Ok(()) }), Ok(()));
    assert_eq!(c.load(Ordering::Relaxed), 1);
}

// ───────────────────── Kit 3: parallel_reduce (fork + reduce) ─────────────────────
// `extern "C"` bodies over the EXACT ReduceBody ABI the JIT will emit. Each folds
// trip-indices [lo, hi) into a partial; partials combine via the ReduceOp.

/// Sum fold: partial = Σ k for k in [lo, hi). Op = Add → Σ over [0, trip).
extern "C" fn body_sum(lo: i64, hi: i64, ok: *mut i32, _env: *const i64) -> i64 {
    unsafe { *ok = 1 };
    let mut s = 0i64;
    for k in lo..hi {
        s = s.wrapping_add(k);
    }
    s
}

#[test]
fn parallel_reduce_sum_matches_serial() {
    let _g = guard();
    let trip = 1_000_000i64;
    let pool = RavaPool::new(4);
    let got = pool.parallel_reduce(trip, 4096, ReduceOp::Add, body_sum, 0);
    let want: i64 = (0..trip).fold(0i64, |a, k| a.wrapping_add(k));
    assert_eq!(got, Ok(want), "parallel Add-reduce == serial fold");
}

#[test]
fn parallel_reduce_xor_matches_serial_and_is_order_independent() {
    let _g = guard();
    let trip = 500_000i64;
    let pool = RavaPool::new(4);
    // Tiny chunk → many out-of-order partials; associativity must still hold.
    let got = pool.parallel_reduce(trip, 37, ReduceOp::Xor, body_xor_hash, 0);
    let want = (0..trip).fold(0i64, |a, k| a ^ ihash(k));
    assert_eq!(got, Ok(want), "Xor-reduce is order-independent");
}

/// A MAP body over the MapBody ABI: env[0]=in base, env[1]=out base; writes
/// out[i] = f(in[i]) for i in [lo, hi). Disjoint indices → no combine.
extern "C" fn body_map_sq(lo: i64, hi: i64, ok: *mut i32, env: *const i64) {
    unsafe {
        *ok = 1;
        let in_base = *env.add(0) as *const i64;
        let out_base = *env.add(1) as *mut i64;
        for k in lo..hi {
            let x = *in_base.add(k as usize);
            *out_base.add(k as usize) = x.wrapping_mul(x).wrapping_add(x >> 13);
        }
    }
}

#[test]
fn parallel_map_writes_disjoint_and_matches_serial() {
    let _g = guard();
    let trip = 500_000i64;
    let inv: Vec<i64> = (0..trip)
        .map(|k| k.wrapping_mul(2654435761).wrapping_add(1))
        .collect();
    let mut outv = vec![0i64; trip as usize];
    let env = [inv.as_ptr() as i64, outv.as_mut_ptr() as i64];
    let pool = RavaPool::new(4);
    // Tiny chunk → many workers over disjoint index ranges.
    let r = pool.parallel_map(trip, 37, body_map_sq, env.as_ptr() as i64);
    assert_eq!(r, Ok(()), "map region completes");
    for k in 0..trip as usize {
        let x = inv[k];
        let want = x.wrapping_mul(x).wrapping_add(x >> 13);
        assert_eq!(outv[k], want, "out[{k}] == serial f(in[k])");
    }
}
