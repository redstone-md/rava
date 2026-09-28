//! Arming a thread and taking one sample.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::pc_profile::*;

/// Publish the calling thread as the sample target (first caller wins). Called from the
/// compiled-entry boundary, so the thread that actually runs machine code is the one sampled.
pub fn arm_current_thread() {
    if !enabled() {
        return;
    }
    #[cfg(windows)]
    {
        if TARGET.load(Relaxed) != 0 {
            return;
        }
        let h = win::duplicate_current_thread();
        if h != 0 {
            // First writer wins; a loser leaks one handle for the process lifetime, which is
            // preferable to closing a handle the winner may be mid-`SuspendThread` on.
            let _ = TARGET.compare_exchange(0, h, Relaxed, Relaxed);
        }
    }
}

/// Take one instruction-pointer sample of the guest thread. Called from the sampler thread.
pub fn sample() {
    if !enabled() {
        return;
    }
    #[cfg(windows)]
    {
        let h = TARGET.load(Relaxed);
        // WHOLE-PROCESS CAPTURE FIRST, and independently of `TARGET`.
        //
        // This block used to sit BELOW the `h == 0` early return — i.e. the capture built
        // precisely so it would not depend on a thread announcing itself was gated behind a
        // thread announcing itself. With `RAVA_JIT=0` nothing ever enters compiled code, so
        // `arm_current_thread` never fires, `TARGET` stays 0 and the profiler produced ZERO
        // rows for an interpreter-only boot: the one run it is now most needed for.
        if all_enabled() {
            const REFRESH_TICKS: u64 = 200;
            static TICK: AtomicUsize = AtomicUsize::new(0);
            let n = TICK.fetch_add(1, Relaxed) as u64;
            if n % REFRESH_TICKS == 0 {
                refresh_targets();
            }
            let t_all = now_ms();
            for (i, slot) in TARGETS.iter().enumerate() {
                let th = slot.load(Relaxed);
                if th == 0 {
                    continue;
                }
                if let Some(rip) = win::sample_rip(th) {
                    if let Ok(mut v) = ALL_SAMPLES.lock() {
                        v.push((t_all, rip, i as u8));
                    }
                }
            }
        }
        if h == 0 {
            return;
        }
        let t = now_ms();
        if crate::pc_stack::enabled() {
            // One extra kernel read inside the same suspend window: the leaf RIP alone has no
            // caller attribution, which is exactly what an unowned host cost needs.
            std::thread_local! {
                static BUF: std::cell::RefCell<Vec<u8>> =
                    std::cell::RefCell::new(vec![0u8; crate::pc_stack::STACK_BYTES]);
            }
            BUF.with(|b| {
                let mut b = b.borrow_mut();
                if let Some((rip, n)) = win::sample_rip_stack(h, &mut b) {
                    if let Ok(mut v) = SAMPLES.lock() {
                        v.push((t, rip));
                    }
                    crate::pc_stack::record(t, rip, &b[..n]);
                }
            });
            return;
        }
        if let Some(rip) = win::sample_rip(h) {
            // The same boundaries the allocation census uses, read at sample time — see
            // `alloc_sites::REGION_NOW`. This is what makes the time attribution mutually
            // exclusive instead of a set of overlapping categories.
            crate::alloc_sites::note_region_tick();
            crate::alloc_sites::note_seam_rip(rip);
            crate::alloc_sites::note_compiled_rip(rip);
            if let Ok(mut v) = SAMPLES.lock() {
                v.push((t, rip));
            }
        }
    }
}
