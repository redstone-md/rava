//! Arithmetic bridges: the four operations whose trapping cases are cheaper to hand to the host than to guard inline.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;


/// JVM int division/remainder. Cranelift's raw signed div/rem can trap on
/// zero/overflow; Java needs `/ by zero` as a pending exception and MIN/-1 wrap.
pub(crate) extern "C" fn rava_i32_divrem(lhs: i64, rhs: i64, is_rem: i64) -> i64 {
    cb(16);
    let a = lhs as i32;
    let b = rhs as i32;
    if b == 0 {
        // GAP-S09 stage 4: on a gil-free worker `set_pending` is worker-illegal
        // (`make_exception` allocates a shared heap Ref) — abort the region
        // instead; the serial retry re-divides and raises the real exception.
        // The returned 0 is the discarded garbage-continuation value.
        #[cfg(feature = "next-gen-gc")]
        if crate::parallel::gilfree() && !(crate::worker_locked_exec_enabled() && resolve_lock_held_by_me()) {
            crate::parallel::request_abort();
            return 0;
        }
        if let Some(vm) = vm_ref() {
            set_pending(vm, "java/lang/ArithmeticException", "/ by zero");
        }
        return 0;
    }
    let out = if is_rem != 0 {
        a.wrapping_rem(b)
    } else {
        a.wrapping_div(b)
    };
    out as i64
}

/// JVM long division/remainder — i64 analogue of `rava_i32_divrem`. Cranelift's
/// raw sdiv/srem trap on `/ by zero` and on `MIN / -1` overflow; Java needs the
/// zero case as a pending ArithmeticException and `wrapping_*` for MIN/-1.
pub(crate) extern "C" fn rava_i64_divrem(lhs: i64, rhs: i64, is_rem: i64) -> i64 {
    cb(17);
    // RAVA_TRACE_DIVREM: this callback is the WHOLE remaining cost of
    // RAVA_ARENA_LAMBDA — 1,545,513 calls without the flag against 9,508,179 with it,
    // and every other split census bucket is byte-identical between the two arms. The
    // census can say WHICH callback but not WHO calls it, so name the compiled method.
    if crate::env_flag!("RAVA_TRACE_DIVREM") {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        if N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 10 {
            eprintln!("[DIVREM] in {:?} rhs={rhs} is_rem={is_rem}", jit_method_cur());
        }
    }
    if rhs == 0 {
        // GAP-S09 stage 4: worker ÷0 aborts to serial — see rava_i32_divrem.
        #[cfg(feature = "next-gen-gc")]
        if crate::parallel::gilfree() && !(crate::worker_locked_exec_enabled() && resolve_lock_held_by_me()) {
            crate::parallel::request_abort();
            return 0;
        }
        if let Some(vm) = vm_ref() {
            set_pending(vm, "java/lang/ArithmeticException", "/ by zero");
        }
        return 0;
    }
    if is_rem != 0 {
        lhs.wrapping_rem(rhs)
    } else {
        lhs.wrapping_div(rhs)
    }
}

/// JVM `frem`/`drem`: truncated floating remainder (`a - (a/b truncated)*b`),
/// which is exactly Rust's `%` on floats (C `fmod`). Cranelift has no frem op,
/// so these route through a libcall. `b == 0` yields NaN — no exception, per
/// JVMS (unlike integer div/rem).
pub(crate) extern "C" fn rava_f32_rem(a: f32, b: f32) -> f32 {
    cb(18);
    a % b
}
pub(crate) extern "C" fn rava_f64_rem(a: f64, b: f64) -> f64 {
    cb(19);
    a % b
}
