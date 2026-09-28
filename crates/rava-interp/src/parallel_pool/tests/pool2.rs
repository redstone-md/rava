//! Part 2 of the pool tests.

use super::*;

#[test]
fn interleaved_primitives_survive_sustained_forks() {
    // Armor for the RESERVED_POOL VA-recycle fix: many forks of DIFFERENT primitives,
    // interleaved, must all stay bit-exact and never exhaust the low-arena VA cursor
    // (pre-fix this hard-exited around ~80 forks). 60 rounds × 4 primitives = 240 forks,
    // each spinning worker heaps that must recycle their reserved tenured spans.
    extern "C" fn sum_body(lo: i64, hi: i64, _ok: *mut i32, env: *const i64) -> i64 {
        let mut a = 0i64;
        for i in lo..hi { a = a.wrapping_add(unsafe { *env.add(i as usize) }); }
        a
    }
    let _g = guard();
    let pool = RavaPool::new(4);
    let n = 50_000i64;
    let vals: Vec<i64> = (0..n).map(|k| (k.wrapping_mul(2654435761).wrapping_add(1)) & 0x3ff).collect();
    let serial_sum: i64 = vals.iter().sum();
    let serial_cnt = vals.iter().filter(|&&v| v > 500).count() as i64;
    let serial_argmax = {
        let (mut bv, mut bi) = (i64::MIN, -1i64);
        for (i, &v) in vals.iter().enumerate() {
            if v > bv { bv = v; bi = i as i64; }
        }
        bi
    };
    let serial_sumsq: i64 = vals.iter().fold(0i64, |a, &v| a.wrapping_add(v.wrapping_mul(v)));
    extern "C" fn ss_body(lo: i64, hi: i64, _ok: *mut i32, env: *const i64, out: *mut i64) {
        let (mut s, mut ss) = (0i64, 0i64);
        for i in lo..hi {
            let v = unsafe { *env.add(i as usize) };
            s = s.wrapping_add(v);
            ss = ss.wrapping_add(v.wrapping_mul(v));
        }
        unsafe { *out = s; *out.add(1) = ss; }
    }
    for round in 0..60 {
        // reduce (sum)
        let s = pool.parallel_reduce(n, 4096, ReduceOp::Add, sum_body, vals.as_ptr() as i64);
        assert_eq!(s, Ok(serial_sum), "reduce round {round}");
        // scan (prefix-sum) — last element = total
        let mut out = vec![0i64; n as usize];
        pool.parallel_scan(n, out.as_mut_ptr() as i64, vals.as_ptr() as i64, ReduceOp::Add).unwrap();
        assert_eq!(out[n as usize - 1], serial_sum, "scan round {round}");
        // filter (> 500)
        let mut fout = vec![0i64; n as usize];
        let k = pool.parallel_filter(n, fout.as_mut_ptr() as i64, vals.as_ptr() as i64, 0, 500, 8).unwrap();
        assert_eq!(k, serial_cnt, "filter round {round}");
        // count (> 500)
        let c = pool.parallel_count(n, vals.as_ptr() as i64, 0, 500, 8).unwrap();
        assert_eq!(c, serial_cnt, "count round {round}");
        // argmax
        let am = pool.parallel_argmax(n, vals.as_ptr() as i64, true, 8).unwrap();
        assert_eq!(am, serial_argmax, "argmax round {round}");
        // reduce2 (sum + sumsq)
        let (r2s, r2ss) = pool.parallel_reduce2(n, ReduceOp::Add, ReduceOp::Add, ss_body, vals.as_ptr() as i64).unwrap();
        assert_eq!(r2s, serial_sum, "reduce2 sum round {round}");
        assert_eq!(r2ss, serial_sumsq, "reduce2 sumsq round {round}");
    }
}

#[test]
fn parallel_reduce2_dual_accumulator_matches_serial() {
    extern "C" fn sum_sumsq(lo: i64, hi: i64, _ok: *mut i32, env: *const i64, out: *mut i64) {
        let (mut s, mut ss) = (0i64, 0i64);
        for i in lo..hi {
            let v = unsafe { *env.add(i as usize) };
            s = s.wrapping_add(v);
            ss = ss.wrapping_add(v.wrapping_mul(v));
        }
        unsafe {
            *out = s;
            *out.add(1) = ss;
        }
    }
    let _g = guard();
    let pool = RavaPool::new(4);
    for &trip in &[500_000i64, 1000, 7, 1, 0] {
        let vals: Vec<i64> = (0..trip)
            .map(|k| (k.wrapping_mul(2654435761).wrapping_add(1)) & 0xffff)
            .collect();
        let (s, ss) = pool.parallel_reduce2(trip, ReduceOp::Add, ReduceOp::Add, sum_sumsq, vals.as_ptr() as i64).unwrap();
        let rs: i64 = vals.iter().fold(0i64, |a, &v| a.wrapping_add(v));
        let rss: i64 = vals.iter().fold(0i64, |a, &v| a.wrapping_add(v.wrapping_mul(v)));
        assert_eq!(s, rs, "sum trip={trip}");
        assert_eq!(ss, rss, "sumsq trip={trip}");
    }
}

#[test]
fn parallel_argmax_matches_serial() {
    let _g = guard();
    let pool = RavaPool::new(4);
    for &trip in &[500_000i64, 1000, 7, 1, 0] {
        let vals: Vec<i64> = (0..trip)
            .map(|k| (k.wrapping_mul(1103515245).wrapping_add(12345)) & 0xffff)
            .collect();
        let got = pool.parallel_argmax(trip, vals.as_ptr() as i64, true, 8).unwrap();
        let (mut bv, mut bi) = (i64::MIN, -1i64);
        for (i, &v) in vals.iter().enumerate() {
            if v > bv { bv = v; bi = i as i64; }
        }
        assert_eq!(got, bi, "long argmax trip={trip}");
        // argmin (want_max = false)
        let gmin = pool.parallel_argmax(trip, vals.as_ptr() as i64, false, 8).unwrap();
        let (mut nv, mut ni) = (i64::MAX, -1i64);
        for (i, &v) in vals.iter().enumerate() {
            if v < nv { nv = v; ni = i as i64; }
        }
        assert_eq!(gmin, ni, "long argmin trip={trip}");
        let ivals: Vec<i32> = vals.iter().map(|&v| v as i32).collect();
        let goti = pool.parallel_argmax(trip, ivals.as_ptr() as i64, true, 4).unwrap();
        let (mut bv2, mut bi2) = (i64::MIN, -1i64);
        for (i, &v) in ivals.iter().enumerate() {
            if v as i64 > bv2 { bv2 = v as i64; bi2 = i as i64; }
        }
        assert_eq!(goti, bi2, "int argmax trip={trip}");
    }
    // Explicit tie across chunk boundaries → the LOWEST index must win.
    let mut t = vec![0i64; 200_000];
    t[3] = 999;
    t[100_000] = 999;
    assert_eq!(pool.parallel_argmax(t.len() as i64, t.as_ptr() as i64, true, 8).unwrap(), 3, "tie → lowest index");
}

#[test]
fn parallel_scan_prefix_matches_serial() {
    let _g = guard();
    let pool = RavaPool::new(4);
    for &trip in &[500_000i64, 1000, 7, 1, 0] {
        let vals: Vec<i64> = (0..trip)
            .map(|k| k.wrapping_mul(2654435761).wrapping_add(1))
            .collect();
        // Prefix SUM.
        let mut out = vec![0i64; trip.max(0) as usize];
        let r = pool.parallel_scan(trip, out.as_mut_ptr() as i64, vals.as_ptr() as i64, ReduceOp::Add);
        assert_eq!(r, Ok(()), "scan sum trip={trip}");
        let mut acc = 0i64;
        for k in 0..trip as usize {
            acc = acc.wrapping_add(vals[k]);
            assert_eq!(out[k], acc, "prefix-sum[{k}] trip={trip}");
        }
        // Prefix MAX (a different associative op, to prove generality).
        let mut outm = vec![0i64; trip.max(0) as usize];
        let rm = pool.parallel_scan(trip, outm.as_mut_ptr() as i64, vals.as_ptr() as i64, ReduceOp::Max);
        assert_eq!(rm, Ok(()), "scan max trip={trip}");
        let mut m = i64::MIN;
        for k in 0..trip as usize {
            m = m.max(vals[k]);
            assert_eq!(outm[k], m, "prefix-max[{k}] trip={trip}");
        }
    }
}

#[test]
fn parallel_filter_compaction_matches_serial() {
    let _g = guard();
    let pool = RavaPool::new(4);
    for &trip in &[500_000i64, 1000, 7, 1, 0] {
        let vals: Vec<i64> = (0..trip)
            .map(|k| k.wrapping_mul(2654435761).wrapping_add(1) & 0xffff)
            .collect();
        // long: keep v > 100  (cmp_code 0)
        let mut out = vec![-1i64; trip.max(0) as usize];
        let k = pool.parallel_filter(trip, out.as_mut_ptr() as i64, vals.as_ptr() as i64, 0, 100, 8).unwrap();
        let sref: Vec<i64> = vals.iter().copied().filter(|&v| v > 100).collect();
        assert_eq!(k as usize, sref.len(), "gt count trip={trip}");
        assert_eq!(&out[..sref.len()], &sref[..], "gt order/compaction trip={trip}");
        // int: keep v <= 5000  (cmp_code 3)
        let ivals: Vec<i32> = vals.iter().map(|&v| v as i32).collect();
        let mut iout = vec![-1i32; trip.max(0) as usize];
        let ik = pool.parallel_filter(trip, iout.as_mut_ptr() as i64, ivals.as_ptr() as i64, 3, 5000, 4).unwrap();
        let iref: Vec<i32> = ivals.iter().copied().filter(|&v| v <= 5000).collect();
        assert_eq!(ik as usize, iref.len(), "int le count trip={trip}");
        assert_eq!(&iout[..iref.len()], &iref[..], "int le order trip={trip}");
    }
}

#[test]
fn parallel_reduce_scales_across_cores() {
    let _g = guard();
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let trip = 2_000_000i64;

    // Serial reference (single call over the whole range).
    HEAVY_PEAK.store(0, Ordering::SeqCst);
    HEAVY_ACTIVE.store(0, Ordering::SeqCst);
    let mut ok = 1i32;
    let t0 = std::time::Instant::now();
    let serial = body_heavy(0, trip, &mut ok as *mut i32, std::ptr::null());
    let serial_ns = t0.elapsed().as_nanos();

    // Parallel reduce over the pool.
    HEAVY_PEAK.store(0, Ordering::SeqCst);
    HEAVY_ACTIVE.store(0, Ordering::SeqCst);
    let pool = RavaPool::new(cores);
    let t1 = std::time::Instant::now();
    let par = pool.parallel_reduce(trip, 8192, ReduceOp::Xor, body_heavy, 0);
    let par_ns = t1.elapsed().as_nanos();

    assert_eq!(par, Ok(serial), "parallel result == serial result");
    let speedup = serial_ns as f64 / par_ns.max(1) as f64;
    eprintln!(
        "[KIT3-SCALE] cores={cores} serial={}us parallel={}us speedup={:.2}x peak_lanes={}",
        serial_ns / 1000,
        par_ns / 1000,
        speedup,
        HEAVY_PEAK.load(Ordering::SeqCst),
    );
    if cores >= 2 {
        // Structural proof of multi-core execution (deterministic): ≥2 lanes ran
        // the heavy body concurrently.
        assert!(
            HEAVY_PEAK.load(Ordering::SeqCst) >= 2,
            "expected ≥2 lanes concurrently, peak={}",
            HEAVY_PEAK.load(Ordering::SeqCst)
        );
    }
}

#[test]
fn parallel_reduce_aborts_to_serial_fallback() {
    let _g = guard();
    let pool = RavaPool::new(4);
    let r = pool.parallel_reduce(20_000, 256, ReduceOp::Add, body_abort_past_5000, 0);
    assert_eq!(r, Err(RegionAbort), "a body abort tears the reduce region down");
    assert!(parallel::abort_requested(), "global abort signal raised for the driver");
    parallel::clear_abort();
}

#[test]
fn reduce_op_identities_and_fold_opcodes() {
    assert_eq!(ReduceOp::Add.identity(), 0);
    assert_eq!(ReduceOp::Mul.identity(), 1);
    assert_eq!(ReduceOp::Xor.identity(), 0);
    assert_eq!(ReduceOp::from_fold_opcode(0x61), Some(ReduceOp::Add)); // ladd
    assert_eq!(ReduceOp::from_fold_opcode(0x68), Some(ReduceOp::Mul)); // imul
    assert_eq!(ReduceOp::from_fold_opcode(0x83), Some(ReduceOp::Xor)); // lxor
    assert_eq!(ReduceOp::from_fold_opcode(0x64), None); // isub — not associative-reducible
    // AND / OR (iand/land, ior/lor): associative + commutative, added for universal folds.
    assert_eq!(ReduceOp::And.identity(), -1); // all-1 bits ⇒ x & -1 == x
    assert_eq!(ReduceOp::Or.identity(), 0);
    assert_eq!(ReduceOp::from_fold_opcode(0x7e), Some(ReduceOp::And)); // iand
    assert_eq!(ReduceOp::from_fold_opcode(0x7f), Some(ReduceOp::And)); // land
    assert_eq!(ReduceOp::from_fold_opcode(0x80), Some(ReduceOp::Or)); // ior
    assert_eq!(ReduceOp::from_fold_opcode(0x81), Some(ReduceOp::Or)); // lor
    assert_eq!(ReduceOp::from_fold_opcode(0x6c), None); // idiv — not associative
}

#[test]
fn and_or_folds_match_serial_in_any_partition() {
    // The soundness contract: combining per-worker partials in ANY order (with the op's
    // identity seed) equals the serial left-fold. Exhaustively check AND/OR over a
    // sample split into arbitrary sub-ranges recombined out of order.
    let data: Vec<i64> = vec![0x0F0F, 0xF0F0, 0x1234, -1, 0xFFFF, 0x00FF, 42, 0x5A5A];
    for op in [ReduceOp::And, ReduceOp::Or] {
        let serial = data.iter().fold(op.identity(), |a, &x| op.combine(a, x));
        // Three arbitrary partitions, each folded then recombined in a different order.
        let p0 = data[0..3].iter().fold(op.identity(), |a, &x| op.combine(a, x));
        let p1 = data[3..5].iter().fold(op.identity(), |a, &x| op.combine(a, x));
        let p2 = data[5..].iter().fold(op.identity(), |a, &x| op.combine(a, x));
        let recombined = op.combine(op.combine(p2, p0), p1);
        assert_eq!(recombined, serial, "{op:?} partial recombination diverged");
    }
}
