//! Parallel-stream bridges: the fork points a compiled stream pipeline calls into.
//!
//! Re-exported from the parent module, so no call site outside it changed.




/// Stop-only safepoint for a compiled backward branch. It parks a region worker
/// when the coordinator requests STW; it does not inspect roots or collect.
pub(crate) extern "C" fn rava_jit_safepoint() -> i64 {
    crate::parallel::safepoint();
    0
}

/// KIT 3 — the JIT fork bridge. A compiled data-parallel loop calls this with the
/// address of its specialised `ReduceBody` (a second JIT function), the trip count,
/// the reduce op code (0=Add,1=Mul,2=Xor), and an abort-flag out-pointer. Runs the
/// fold across `RavaPool` GIL-free and returns the reduced accumulator; on a
/// `RegionAbort` (a GIL-free TLAB overflow inside the body) it sets `*abort_out = 1`
/// and the compiled caller jumps to its serial fallback under the GIL.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_parallel_reduce(body: i64, trip: i64, op_code: i64, abort_out: *mut i32, env: i64) -> i64 {
    use crate::parallel_pool::{RavaPool, ReduceBody, ReduceOp};
    if { static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); crate::jit::env_gate("RAVA_DBG_PAR", &ON) } {
        eprintln!("[DBG-PAR] rava_parallel_reduce FORK trip={trip} op_code={op_code}");
    }
    let op = match op_code {
        0 => ReduceOp::Add,
        1 => ReduceOp::Mul,
        3 => ReduceOp::And,
        4 => ReduceOp::Or,
        5 => ReduceOp::Max,
        6 => ReduceOp::Min,
        _ => ReduceOp::Xor,
    };
    let body_fn: ReduceBody = unsafe { std::mem::transmute::<usize, ReduceBody>(body as usize) };
    let pool = RavaPool::global();
    let chunk = (trip / (pool.workers() as i64 * 8)).max(1024);
    match pool.parallel_reduce(trip, chunk, op, body_fn, env) {
        Ok(v) => {
            unsafe { *abort_out = 0 };
            v
        }
        Err(_) => {
            unsafe { *abort_out = 1 };
            0
        }
    }
}

/// Fork a data-parallel MAP across `RavaPool` (the reduce counterpart with no accumulator).
/// `body` is the JIT-emitted `MapBody`; it writes disjoint `out[i]`. Sets `*abort_out = 1`
/// on a GIL-free TLAB overflow so the driver re-runs the loop serially.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_parallel_map(body: i64, trip: i64, abort_out: *mut i32, env: i64) {
    use crate::parallel_pool::{MapBody, RavaPool};
    if { static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); crate::jit::env_gate("RAVA_DBG_PAR", &ON) } {
        eprintln!("[DBG-PAR] rava_parallel_map FORK trip={trip}");
    }
    let body_fn: MapBody = unsafe { std::mem::transmute::<usize, MapBody>(body as usize) };
    let pool = RavaPool::global();
    let chunk = (trip / (pool.workers() as i64 * 8)).max(1024);
    match pool.parallel_map(trip, chunk, body_fn, env) {
        Ok(()) => unsafe { *abort_out = 0 },
        Err(_) => unsafe { *abort_out = 1 },
    }
}

/// Fork a FILTER/compaction across `RavaPool`. `pred_ptr` = the JIT-emitted predicate fn
/// (extern C fn(i64)->i32). Returns the surviving count; sets `*abort_out = 1` on TLAB
/// overflow → the driver re-runs serially.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_parallel_filter(trip: i64, out_base: i64, in_base: i64, cmp_code: i64, cst: i64, elem_bytes: i64, abort_out: *mut i32) -> i64 {
    use crate::parallel_pool::RavaPool;
    if { static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); crate::jit::env_gate("RAVA_DBG_PAR", &ON) } {
        eprintln!("[DBG-PAR] rava_parallel_filter FORK trip={trip} w={elem_bytes}");
    }
    match RavaPool::global().parallel_filter(trip, out_base, in_base, cmp_code, cst, elem_bytes) {
        Ok(k) => {
            unsafe { *abort_out = 0 };
            k
        }
        Err(_) => {
            unsafe { *abort_out = 1 };
            0
        }
    }
}

/// Fork a COUNT (`#{i : pred(in[i])}`) across `RavaPool`. Returns the count; sets `*abort_out
/// = 1` on TLAB overflow → the driver re-runs serially.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_parallel_count(trip: i64, in_base: i64, cmp_code: i64, cst: i64, elem_bytes: i64, abort_out: *mut i32) -> i64 {
    use crate::parallel_pool::RavaPool;
    if { static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); crate::jit::env_gate("RAVA_DBG_PAR", &ON) } {
        eprintln!("[DBG-PAR] rava_parallel_count FORK trip={trip} w={elem_bytes}");
    }
    match RavaPool::global().parallel_count(trip, in_base, cmp_code, cst, elem_bytes) {
        Ok(k) => {
            unsafe { *abort_out = 0 };
            k
        }
        Err(_) => {
            unsafe { *abort_out = 1 };
            0
        }
    }
}

/// Fork a prefix-SCAN across `RavaPool` (the 2-pass engine). Writes `out[i] = op-prefix of
/// in[0..=i]` over the resolved long-array bases. Sets `*abort_out = 1` on TLAB overflow → the
/// driver re-runs the scan serially.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_parallel_scan(trip: i64, out_base: i64, vals_base: i64, op_code: i64, elem_bytes: i64, abort_out: *mut i32) {
    use crate::parallel_pool::{RavaPool, ReduceOp};
    if { static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); crate::jit::env_gate("RAVA_DBG_PAR", &ON) } {
        eprintln!("[DBG-PAR] rava_parallel_scan FORK trip={trip} op={op_code} w={elem_bytes}");
    }
    let op = match op_code {
        0 => ReduceOp::Add,
        1 => ReduceOp::Mul,
        3 => ReduceOp::And,
        4 => ReduceOp::Or,
        5 => ReduceOp::Max,
        6 => ReduceOp::Min,
        _ => ReduceOp::Xor,
    };
    match RavaPool::global().parallel_scan_w(trip, out_base, vals_base, op, elem_bytes) {
        Ok(()) => unsafe { *abort_out = 0 },
        Err(_) => unsafe { *abort_out = 1 },
    }
}

/// Fork an ARGMAX across `RavaPool`. Returns the index of the maximum element (ties → lowest
/// index); sets `*abort_out = 1` on TLAB overflow → the driver re-runs serially.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_parallel_argmax(trip: i64, in_base: i64, want_max: i64, elem_bytes: i64, abort_out: *mut i32) -> i64 {
    use crate::parallel_pool::RavaPool;
    if { static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); crate::jit::env_gate("RAVA_DBG_PAR", &ON) } {
        eprintln!("[DBG-PAR] rava_parallel_argmax FORK trip={trip} want_max={want_max} w={elem_bytes}");
    }
    match RavaPool::global().parallel_argmax(trip, in_base, want_max != 0, elem_bytes) {
        Ok(idx) => {
            unsafe { *abort_out = 0 };
            idx
        }
        Err(_) => {
            unsafe { *abort_out = 1 };
            0
        }
    }
}

/// Fork a DUAL reduction across `RavaPool`: `body` folds two accumulators in one pass; the
/// pool combines per-chunk pairs under op1/op2. Writes `out[0]=acc1, out[1]=acc2`; sets
/// `*abort_out=1` on TLAB overflow → the driver re-runs serially.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_parallel_reduce2(trip: i64, body_ptr: i64, env: i64, op1_code: i64, op2_code: i64, out: *mut i64, abort_out: *mut i32) {
    use crate::parallel_pool::{ReduceBody2, ReduceOp, RavaPool};
    if { static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); crate::jit::env_gate("RAVA_DBG_PAR", &ON) } {
        eprintln!("[DBG-PAR] rava_parallel_reduce2 FORK trip={trip}");
    }
    let dec = |c: i64| match c {
        0 => ReduceOp::Add,
        1 => ReduceOp::Mul,
        3 => ReduceOp::And,
        4 => ReduceOp::Or,
        5 => ReduceOp::Max,
        6 => ReduceOp::Min,
        _ => ReduceOp::Xor,
    };
    let body: ReduceBody2 = unsafe { std::mem::transmute::<usize, ReduceBody2>(body_ptr as usize) };
    match RavaPool::global().parallel_reduce2(trip, dec(op1_code), dec(op2_code), body, env) {
        Ok((a1, a2)) => unsafe {
            *out = a1;
            *out.add(1) = a2;
            *abort_out = 0;
        },
        Err(_) => unsafe { *abort_out = 1 },
    }
}

