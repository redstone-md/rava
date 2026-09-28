//! Pool tests.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::parallel_pool::*;

#[cfg(test)]
use super::*;
use std::sync::atomic::{AtomicI64, AtomicUsize};

// Serialize against the other users of the global region/abort statics.
// Serialize against the other users of the global region/abort statics.
fn guard() -> std::sync::MutexGuard<'static, ()> {
    parallel::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

/// Xor fold over a cheap hash of each index (the assoc-reducible microbench shape).
fn ihash(k: i64) -> i64 {
    let mut x = k.wrapping_mul(0x9E3779B97F4A7C15u64 as i64);
    x ^= x >> 29;
    x
}
extern "C" fn body_xor_hash(lo: i64, hi: i64, ok: *mut i32, _env: *const i64) -> i64 {
    unsafe { *ok = 1 };
    let mut acc = 0i64;
    for k in lo..hi {
        acc ^= ihash(k);
    }
    acc
}

// Heavy CPU body (XOR fold) + deterministic peak-concurrency instrumentation, so
// multi-core execution is proven structurally (not by flaky timing).
// Heavy CPU body (XOR fold) + deterministic peak-concurrency instrumentation, so
// multi-core execution is proven structurally (not by flaky timing).
pub(crate) static HEAVY_ACTIVE: AtomicUsize = AtomicUsize::new(0);
pub(crate) static HEAVY_PEAK: AtomicUsize = AtomicUsize::new(0);
extern "C" fn body_heavy(lo: i64, hi: i64, ok: *mut i32, _env: *const i64) -> i64 {
    let cur = HEAVY_ACTIVE.fetch_add(1, Ordering::SeqCst) + 1;
    HEAVY_PEAK.fetch_max(cur, Ordering::SeqCst);
    let mut acc = 0i64;
    for k in lo..hi {
        let mut x = k;
        for _ in 0..200 {
            x = x.wrapping_mul(2654435761).wrapping_add(k ^ 0x5DEECE66D);
        }
        acc ^= x;
    }
    HEAVY_ACTIVE.fetch_sub(1, Ordering::SeqCst);
    unsafe { *ok = 1 };
    acc
}

// Body that aborts (sets *ok=0) once its range crosses a threshold — models a
// GIL-free TLAB overflow.
extern "C" fn body_abort_past_5000(lo: i64, hi: i64, ok: *mut i32, _env: *const i64) -> i64 {
    if hi > 5000 {
        unsafe { *ok = 0 };
        parallel::request_abort();
        return 0;
    }
    unsafe { *ok = 1 };
    let mut s = 0i64;
    for k in lo..hi {
        s = s.wrapping_add(k);
    }
    s
}

mod pool1;
mod pool2;
