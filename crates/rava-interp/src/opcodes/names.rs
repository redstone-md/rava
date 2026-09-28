//! Class names and the memo over them, plus the double-bit helpers.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::opcodes::*;

/// `RAVA_FORK_NUMERIC_PROBE` (M3 T2 step 3a): at each forward `goto`, try the counted-loop matcher
/// + index-partitioned offloadability check and trace what it finds (once per loop site). Pure
/// diagnostic — no fork — to confirm empirically whether the real worldgen hot path even HAS
/// offloadable counted loops (vs recursive density eval). Off by default.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn fork_numeric_probe() -> bool {
    pub(crate) static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *F.get_or_init(|| std::env::var_os("RAVA_FORK_NUMERIC_PROBE").is_some())
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn run_stop_clear() { exit::RUN_STOP_PC.with(|c| c.set(usize::MAX)); }

pub(crate) static IOF_UNKEYED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// `RAVA_VIRT_CLASS_MEMO` — OPT-IN (default OFF). Shares ONE `Arc<str>` per receiver class for
/// `invokevirtual`'s dispatch class name instead of rebuilding it per call.
///
/// ⛔ **SHIPPED DEFAULT-ON AND REVERTED THE SAME DAY (2026-07-29).** The mechanism is real and
/// enormous by COUNT, and worth NOTHING on the clock.
///
/// MEASURED by census (`RAVA_VIRT_CLASS_STATS`) on a real MC server boot, counted not timed:
///
/// ```text
/// memo OFF : 24,000,000 dispatch_class calls, 23,999,999 builds  (0.0% served)
/// memo ON  : 24,000,000 dispatch_class calls,         1,590 builds (100.0% served)
/// ```
///
/// So the boot performs 24 MILLION of these and there are only ~1,590 distinct receiver classes.
/// Every one of the removed builds is an `Arc<str>` allocation for a heap `Ref`, or - for an ARENA
/// receiver - a lock on the GLOBAL class-registry mutex plus an `Arc` clone plus a `String`.
/// Verified caller attribution had already put `dispatch_class` at 27.3% of `mi_malloc_aligned`
/// samples, with allocation (`Heap::gc` 5.5% + `mi_malloc_aligned` 4.5%) the largest leaf block of
/// the serial window, which is 90-95% host-bound.
///
/// It was built, documented and left switched off. Counted evidence was used to ship it, because
/// the box was too noisy for a sub-5% wall A/B and 24M removed builds is a fact about work rather
/// than about the machine. THE TIMED A/B THEN REFUSED IT — interleaved, 3 reps per arm, pcore:
///
/// ```text
/// memo ON  : Done 23.023 / 23.079 / 23.506
/// memo OFF : Done 22.658 / 22.901 / 23.312     <- min-of-N -1.59% in favour of OFF, OVERLAP
/// ```
///
/// Not "ON CLEAN" in either direction, and the direction that exists points AWAY. Mechanically
/// consistent: `build()` is a small allocation from mimalloc's THREAD-LOCAL heap (~15-20 ns) and,
/// for the arena arm, an UNCONTENDED mutex on a single-threaded boot - while the memo costs a
/// `RefCell` borrow, a hash lookup and an ATOMIC `Arc` refcount bump. Comparable, so 24M of them
/// cancel.
///
/// ★★ This is the FOURTH structural proxy in this tree to remove huge counted work for zero time:
/// 35M host callbacks (direct-mapped dispatch table), half of all dispatch BLOCKS (heap-Ref arm,
/// which measured WORSE), and now 24M allocations. **Counting work is not a way to price time
/// here.** Keep the census (`RAVA_VIRT_CLASS_STATS`) - the count was right; the inference was not.
///
/// `invokevirtual` is THE compiled→interpreter invoke fallback entry (0xb6 and
/// 0xb9 both land there), and `RAVA_FALLBACK_TALLY` ranks 25.2M of them in a
/// single `bench/RsProbe.java` run. The top rows — `Iterator.hasNext/next`,
/// `Map$Entry.getKey/getValue`, `List.get/set` — are all NATIVE-SERVED types with
/// no bytecode, so they can never be admitted to a compiled direct call AND they
/// never populate the per-(site,class) dispatch caches: every single one of them
/// rebuilds this name. For a heap `Ref` that is a `String` allocation; for an
/// ARENA receiver it is a lock on the global class-registry mutex, an `Arc` clone
/// AND the `String`. The name is a pure function of the class identity that the
/// sibling caches already key on, so the memo removes the work rather than
/// intercepting anything.
pub(crate) fn virt_class_memo_enabled() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_VIRT_CLASS_MEMO").is_some())
}

thread_local! {
    /// receiver class identity → its dispatch class name. Same key space as
    /// `IOF_MEMO` (tagged interned-name pointer for `Ref`, arena class id for
    /// `Obj`), same stability argument: neither is an object address or a handle.
    pub(crate) static VIRT_CLASS: std::cell::RefCell<crate::FastMap<u64, Arc<str>>> =
        std::cell::RefCell::new(crate::FastMap::default());
}

/// ⭐**Takes the CLASS, not the frame** — the only thing it ever read was `frame.class`, and the
/// hot interpreter loop has the class but no `Frame` (row 379). Same extraction as
/// `getfield_value`/`putfield_value`.
pub(crate) fn class_name_at(class: &std::sync::Arc<crate::class_loader::LoadedClass>, index: u16) -> Result<String, VmError> {
    class
        .file
        .pool
        .class_name(index)
        .map(str::to_string)
        .ok_or(VmError::BadConstant(index))
}

/// Reconstruct a `heap::Lambda` from an arena-lambda object (RAVA_ARENA_LAMBDA). The
/// captured values are decoded from the inline cells with the declared per-slot kind —
/// the mirror of the birth path in `invokedynamic_assemble`. Site identity comes from
/// the content-interned table (never the recyclable arena address). The read is a
/// SNAPSHOT (the returned `Vec<Value>` may contain arena ptrs valid until the next GC);
/// callers root them exactly as the handle path does (`push_call_roots`).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn arena_lambda_to_struct(p: crate::arena::ArenaPtr) -> Option<crate::heap::Lambda> {
    // CONSUMER ACCOUNTING (RAVA_GC_STATS) — the twin of `Heap::lambda_of`.
    if crate::nextgen::gc_stats_enabled() {
        let t = std::time::Instant::now();
        let r = arena_lambda_to_struct_inner(p);
        if r.is_some() {
            crate::nextgen::gc_stats_note_snapshot(true, t.elapsed().as_nanos() as u64);
        }
        return r;
    }
    arena_lambda_to_struct_inner(p)
}

/// Arena getfield (Phase 2): resolve the field's slot in the class layout, read
/// the inline cell, decode. A field not in the declared layout (synthetic) panics
/// predictably (Phase 5).
/// Wall #19 read barrier: a getfield/putfield (JIT-emitted OR interpreted) can hand
/// `arena_{get,put}field` a STALE young arena ptr — the receiver was captured (a JIT
/// `box_ref` cell, an in-flight call buffer, a re-loaded local) and then evacuated by
/// a GC that didn't update that capture. The class id then no longer decodes and the
/// slot lookup fails. Follow the evacuated slot's forwarding word to the tenured copy
/// (same mechanism as the jit_run/jit_direct_call entry barrier, Wall #8) and retry.
/// Returns the forwarded ptr + its fresh class id, or None if `p` isn't a recoverable
/// stale slot (then the caller panics as before — a genuine non-layout field).
/// RAVA_STALE_SOFT bookkeeping: count + periodically report how many arena field
/// accesses hit an irrecoverable stale receiver and were degraded to a default/no-op
/// (instead of aborting). Lets a boot reveal whether the staleness is a handful of
/// resources (survivable) or pervasive (cascades).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn stale_soft_count(n: u64) {
    use std::sync::atomic::{AtomicU64, Ordering};
    pub(crate) static N: AtomicU64 = AtomicU64::new(0);
    let v = N.fetch_add(n, Ordering::Relaxed) + n;
    if v == 1 || v % 1000 == 0 {
        eprintln!("[STALE-SOFT] {v} stale arena field accesses degraded to default");
    }
}

/// `Double.hashCode` / `doubleToLongBits`: canonical NaN, signed zeros distinct.
pub(crate) fn java_double_bits(d: f64) -> u64 {
    if d.is_nan() { 0x7ff8_0000_0000_0000 } else { d.to_bits() }
}
