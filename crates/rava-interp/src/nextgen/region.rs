//! A mapped region of the arena, and what its Drop releases.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::nextgen::*;
use crate::arena::ArenaPtr;

/// Fixed-capacity region (stable base — never reallocs; the GC collects instead,
/// so raw pointers into it stay valid). RAVA_HARD_MB bounds the tenured region;
/// overflow panics → the process exits, the host never freezes.
/// A bump region backed by a LOW-ADDRESS OS mapping (`base < 2^40`). The low base
/// is the soundness foundation for the unified-ABI pointer/handle discriminator: a
/// raw `ArenaPtr` (a byte inside this region) then has bits 40..63 == 0, so
/// `addr >> 40 == 0` distinguishes it from a high-word handle (whose tag sits in
/// bits 40..43) REGARDLESS of where the OS would otherwise place heap memory — on
/// Windows that is ~2.4 TiB (bit 41), which aliases the tag region and is exactly
/// why a plain `vec![0u8; _]` backing cannot be told apart from a handle. See
/// `NEXTGEN_GC_CHECKPOINT.md` §4.
pub struct Region {
    pub(crate) ptr: *mut u8,
    pub(crate) len: usize,
    pub top: usize,
    /// Bytes actually COMMITTED (backed by RAM/pagefile). For an eager region this
    /// equals `len`; for a reserved (lazy) region it grows on demand via `ensure`.
    /// Windows counts committed memory against the RAVA_PROC_CAP_MB Job Object, so a
    /// multi-GB tenured region committed upfront falsely fills the cap — reserve the
    /// VA and commit only what `top` actually reaches. (Linux mmap is demand-paged,
    /// so `ensure` is a no-op there and `committed` just tracks `len`.)
    pub(crate) committed: usize,
    /// Non-zero for a RESERVED region whose VA span is recycled through `RESERVED_POOL`
    /// on drop instead of unmapped — so a fork's short-lived worker heaps (a 2 GiB reserved
    /// tenured each) reuse the same VA every fork rather than marching `NEXT_LOW` toward the
    /// 2^40 ceiling. 0 = an eager/restored region that unmaps normally.
    pub(crate) pool_span: usize,
    /// A NON-OWNING view over memory owned by ANOTHER Region — a fork worker's bump sub-range of
    /// main's young nursery (G3). Its allocations land in main's heap, so they persist after the
    /// parallel region (no cross-heap dangling). Drop is a no-op: the owner frees the mapping.
    pub(crate) view: bool,
}


// ─────────────────── committed-extent table (row 420) ───────────────────
//
// ⭐**WHY.** `is_arena_obj_addr` bounds a candidate pointer by `arena_high_water()` and its own
// comment claims that "rejects the reserved-but-uncommitted band, whose pages fault on read".
// On Windows it does not: `map_low_reserved` advances `NEXT_LOW` by the whole span while
// `commit_floor` commits **nothing**, so every region's uncommitted tail sits INSIDE the range the
// guard calls safe. The GC mark is the only pass that dereferences untrusted candidate cells, and
// it faulted there — `0xC0000005` reading `0x20ffff3b8` in `arena_trace_refs` — which is the crash
// row 339 worked around by raising the nursery so collection would not run at all.
//
// ⚠**THIS TABLE MAY ONLY EVER REJECT, NEVER ADMIT.** A mark that skips a live object produces a
// dangling handle — silent corruption, strictly worse than the crash. So the query answers a
// deliberately narrow question: *is this address inside a region I KNOW about, past that region's
// committed prefix?* An address in no known region keeps exactly today's behaviour.
//
// Lock-free and fixed-size: the mark consults it per candidate cell, and regions number in the
// tens. A full table simply stops rejecting, which is the safe direction.
const REG_MAX: usize = 64;
static REG_BASE: [std::sync::atomic::AtomicU64; REG_MAX] =
    [const { std::sync::atomic::AtomicU64::new(0) }; REG_MAX];
static REG_END: [std::sync::atomic::AtomicU64; REG_MAX] =
    [const { std::sync::atomic::AtomicU64::new(0) }; REG_MAX];
static REG_COMMITTED: [std::sync::atomic::AtomicU64; REG_MAX] =
    [const { std::sync::atomic::AtomicU64::new(0) }; REG_MAX];

/// One past the highest slot index ever CLAIMED — the bound every scan below runs to.
///
/// ⭐The table is sized 64 for headroom but a boot holds a handful of regions (nursery,
/// tenured, a major's to-space, the spill span, whatever is parked), so a fixed `0..64` walk
/// spent ~58 of its 64 acquire loads proving that empty slots are empty. That is charged per
/// CANDIDATE CELL of the mark pass (`arena_trace_refs` consults `in_uncommitted_tail` for the
/// object AND for every reference cell it holds), which makes it the wrong loop bound on the
/// one path that runs per-pointer. It never shrinks — a retired slot keeps its index reserved
/// — so the bound stays sound across recycling: a slot below it may be empty, a slot above it
/// has never been written.
static REG_USED: AtomicUsize = AtomicUsize::new(0);

/// ⭐⭐**THE LOWEST BYTE OF ANY UNCOMMITTED TAIL IN THE WHOLE ARENA — the one load that answers
/// the query for nearly every address, so the scan below stops being charged per pointer.**
///
/// WAS: `in_uncommitted_tail` was O(REG_USED) per candidate cell, and **`REG_USED` is a
/// high-water that never falls** — it counts slots EVER CLAIMED, not regions currently live.
/// Every worker heap entering a parallel region claims two (young + tenured), every major GC's
/// to-space claims another, and `reg_retire` only blanks the slot; the bound stays wherever the
/// process's peak left it while the live set falls back to the usual three. So the per-pointer
/// cost of the mark grew with the process's HISTORY, and most of the loads it made were an
/// emptied slot proving itself empty. IS: one relaxed load and one compare for any address
/// below this floor, with the scan reached only above it.
///
/// **Sound because the union of every uncommitted tail lies at or above this value**, so
/// `a < floor` ⇒ `a` is in no tail — the same `false` the scan would return, which is also the
/// direction this table is only ever allowed to answer in (a wrong `true` would make the mark
/// skip a live object; a wrong `false` costs nothing but a scan). `u64::MAX` = no region has a
/// tail at all ⇒ nothing can be in one; `0` = the floor is unknown (start-up, or the kill
/// switch) ⇒ every query falls through to the scan, i.e. exactly today's behaviour.
///
/// **Effectiveness is structural, not a heuristic:** the nursery is EAGER (`committed == len`,
/// no tail, excluded), so the lowest tail belongs to the lowest RESERVED region — main's
/// tenured. Its committed prefix covers `top` by construction (`ensure` precedes every write),
/// therefore every live object in the nursery AND every live object in that tenured space sits
/// below the floor. **Total new state: one `AtomicU64`, 8 bytes** — no per-address table, and
/// nothing consulted on the bump path.
static TAIL_FLOOR: AtomicU64 = AtomicU64::new(0);

/// Kill switch `RAVA_NO_FAST_TLABHDR=1`. Consulted only on the COLD publish/retire path: with
/// the fast path off the floor is simply never raised above its initial 0, so the query's guard
/// can never fire and the hot path carries neither a branch on this flag nor a second load.
#[inline]
fn fast_tlabhdr() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_TLABHDR").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// Re-derive `TAIL_FLOOR` from the table. Called ONLY from `reg_publish` / `reg_retire` — a
/// region construction or an 8 MiB commit step, i.e. tens of times in a boot — which is the
/// whole point: the scan moves off the per-pointer path and onto the per-region one.
///
/// ⚠**Raising the floor is the only direction that can lie, and both raisers are safe.** It
/// rises when a region COMMITS more (the pages are backed before `reg_publish` is called, so
/// the wider prefix is already true) and when a region RETIRES (its slot is blanked first, so
/// the scan would answer `false` for its addresses too — identical verdict). A slot caught
/// mid-claim reads `END == 0` and is skipped, or reads `COMMITTED == 0` and contributes its
/// bare base, which is the correct conservative tail start for a freshly reserved region.
fn tail_floor_recompute() {
    use std::sync::atomic::Ordering::{Acquire, Relaxed};
    if !fast_tlabhdr() {
        return;
    }
    let mut lo = u64::MAX;
    for i in 0..REG_USED.load(Acquire) {
        let b = REG_BASE[i].load(Acquire);
        if b == 0 {
            continue;
        }
        let end = REG_END[i].load(Acquire);
        let c = REG_COMMITTED[i].load(Acquire);
        if c < end {
            lo = lo.min(c.max(b));
        }
    }
    TAIL_FLOOR.store(lo, Relaxed);
}

/// Publish (or update) a region's committed prefix. Called on construction and from `ensure`.
fn reg_publish(base: u64, len: u64, committed: u64) {
    use std::sync::atomic::Ordering::{Acquire, Release};
    if base == 0 {
        return;
    }
    for i in 0..REG_MAX {
        let b = REG_BASE[i].load(Acquire);
        if b == base {
            // Committed only ever grows; publish it BEFORE anything reads a wider extent.
            REG_COMMITTED[i].store(base + committed, Release);
            tail_floor_recompute();
            return;
        }
        if b == 0
            && REG_BASE[i]
                .compare_exchange(0, base, Release, Acquire)
                .is_ok()
        {
            REG_END[i].store(base + len, Release);
            REG_COMMITTED[i].store(base + committed, Release);
            // Widen the scan bound only AFTER the slot is fully written, so a concurrent
            // reader that sees the wider bound also sees a complete entry.
            REG_USED.fetch_max(i + 1, Release);
            tail_floor_recompute();
            return;
        }
    }
    // Table full: stop tracking. The query then admits, i.e. behaves as it did before — and
    // the floor needs no adjustment for it, because an UNTRACKED region's tail is invisible to
    // the scan as well: both arms answer `false` for its addresses, which is the pre-existing
    // acceptance this table was always documented to fall back to.
}

/// Total page-backed arena bytes, summed straight out of the published extent table.
///
/// ⛔The obvious implementation — ask the TLAB for `young.committed + old.committed` — panics
/// `RefCell already borrowed`: every caller worth having (the GC census, an OOM report) runs
/// INSIDE a `with_tlab` borrow, so the diagnostic aborts the process it was measuring. This
/// reads the same numbers from the lock-free table `reg_publish` already maintains, which is
/// re-entrant by construction and additionally covers the regions the TLAB cannot see (pooled
/// spans and other threads').
pub(crate) fn reg_committed_total() -> usize {
    use std::sync::atomic::Ordering::Acquire;
    let mut sum = 0u64;
    for i in 0..REG_USED.load(Acquire) {
        let b = REG_BASE[i].load(Acquire);
        let c = REG_COMMITTED[i].load(Acquire);
        if b != 0 && c > b {
            sum += c - b;
        }
    }
    sum as usize
}

/// Retire a region's entry so a recycled VA span cannot be judged against a stale prefix.
fn reg_retire(base: u64) {
    use std::sync::atomic::Ordering::{Acquire, Release};
    for i in 0..REG_USED.load(Acquire) {
        if REG_BASE[i].load(Acquire) == base {
            REG_COMMITTED[i].store(0, Release);
            REG_END[i].store(0, Release);
            REG_BASE[i].store(0, Release);
            // The retired region may have OWNED the floor. Recompute AFTER the slot is blank,
            // so the floor it lands on is the min over the regions that are still real.
            tail_floor_recompute();
            return;
        }
    }
}

/// Is `a` inside a KNOWN region but beyond its committed prefix — i.e. reserved VA that faults on
/// read? `false` for anything not positively known to be so, including unknown addresses.
pub fn in_uncommitted_tail(a: u64) -> bool {
    use std::sync::atomic::Ordering::Acquire;
    // ⭐O(1) ARM. Below the lowest tail in the arena ⇒ inside no tail. This is the answer for
    // every nursery object and every object in the lowest reserved region, which between them
    // are almost the whole population the mark walks — see `TAIL_FLOOR`. `0` (start-up, or
    // `RAVA_NO_FAST_TLABHDR=1`, which simply never raises it) can never satisfy the compare, so
    // the disabled build falls straight into the scan below with no added branch weight.
    if a < TAIL_FLOOR.load(std::sync::atomic::Ordering::Relaxed) {
        return false;
    }
    for i in 0..REG_USED.load(Acquire) {
        let b = REG_BASE[i].load(Acquire);
        if b == 0 {
            continue;
        }
        if a >= b && a < REG_END[i].load(Acquire) {
            return a >= REG_COMMITTED[i].load(Acquire);
        }
    }
    false
}

impl Region {
    /// Eager region: the whole cap is committed immediately. Use for small regions
    /// (the nursery) that the JIT bumps INLINE with no commit hook.
    pub fn new(cap: usize) -> Self {
        let r = Region { ptr: map_low_zeroed(cap), len: cap, top: 0, committed: cap, pool_span: 0, view: false };
        reg_publish(r.ptr as u64, r.len as u64, r.committed as u64);
        r
    }
    /// A NON-OWNING view over `[ptr, ptr+cap)` owned by another Region — gives a fork worker a
    /// bump sub-range of main's young nursery (G3) so its allocations land in main's heap and
    /// persist after the region. Drop never unmaps it. `top` starts 0 (bump from the sub-base).
    pub fn view(ptr: *mut u8, cap: usize) -> Self {
        Region { ptr, len: cap, top: 0, committed: cap, pool_span: 0, view: true }
    }
    /// Lazy region: the cap is RESERVED (cheap VA) but committed on demand by
    /// `ensure`. Use for the large tenured region and the major-GC to-space, which
    /// are written only on the Rust copy paths (copy_obj / major_copy) where an
    /// `ensure` can precede every write.
    pub fn new_reserved(cap: usize) -> Self {
        let (ptr, span, parked_committed) = reserve_low_pooled(cap);
        // A reused parked span may still be page-backed (gate off) — carry its committed count
        // forward so `ensure`/accounting stay truthful; a decommitted or fresh span starts at 0
        // (commit_floor on Linux reports the demand-paged whole cap).
        let committed = parked_committed.min(cap).max(commit_floor(cap));
        let r = Region { ptr, len: cap, top: 0, committed, pool_span: span, view: false };
        reg_publish(r.ptr as u64, r.len as u64, r.committed as u64);
        r
    }
    /// Ensure `[0, need)` is committed before a write reaches that far. Commits in
    /// 64 KiB-granular chunks (and over-commits a slab ahead to amortise the syscall).
    #[inline]
    pub fn ensure(&mut self, need: usize) {
        if need <= self.committed {
            return;
        }
        let gran = 64 * 1024usize;
        // Grow committed by at least 8 MiB at a time to keep VirtualAlloc calls rare.
        let want = need.max(self.committed + 8 * 1024 * 1024);
        let target = ((want + gran - 1) & !(gran - 1)).min(self.len);
        commit_pages(unsafe { self.ptr.add(self.committed) }, target - self.committed);
        self.committed = target;
        // The mark's guard reads this; publish the WIDER prefix only after the pages are backed.
        if !self.view {
            reg_publish(self.ptr as u64, self.len as u64, self.committed as u64);
        }
    }
    #[inline]
    pub fn base(&self) -> *mut u8 {
        self.ptr
    }
    #[inline]
    pub fn cap(&self) -> usize {
        self.len
    }
    #[inline]
    pub fn contains(&self, p: *mut u8) -> bool {
        let b = self.ptr as usize;
        let a = p as usize;
        a >= b && a < b + self.len
    }
    /// Bulk-zero on recycle — lowers to AVX stores (multi-GiB/s).
    pub fn wipe(&mut self) {
        unsafe { std::ptr::write_bytes(self.ptr, 0, self.len) }
        self.top = 0;
    }
    /// Rebuild a region AT its recorded base (checkpoint restore): the serialized
    /// object bytes contain raw intra-arena pointers, valid only at that address.
    /// `reserve_only` = the lazy tenured shape (commit up to `top`, reserve the rest).
    pub(crate) fn restored(base: u64, cap: usize, top: usize, bytes: &[u8], reserve_only: bool) -> Result<Region, String> {
        let ptr = try_map_at(base, cap, !reserve_only)
            .ok_or_else(|| format!("arena restore: base {base:#x} (+{cap}) unavailable"))?;
        let mut r = Region {
            ptr,
            len: cap,
            top: 0,
            committed: if reserve_only { commit_floor(cap) } else { cap },
            pool_span: 0,
            view: false,
        };
        if reserve_only {
            r.ensure(top.max(1));
        }
        let n = top.min(bytes.len());
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, n) };
        r.top = top;
        Ok(r)
    }
}

/// ⭐**A PARKED SPAN KEEPS ITS PAGES FOREVER, AND THAT IS THE RSS THIS ENGINE CANNOT ACCOUNT
/// FOR.** `RESERVED_POOL` recycles a reserved VA span instead of unmapping it, so the VA cursor
/// never marches at the 2^40 ceiling — but the PHYSICAL pages ride along: a major GC swaps
/// `old` for its to-space and parks the from-space at its full committed high-water, and that
/// working set is never touched again and never returned. `RAVA_ARENA_DECOMMIT` already fixes
/// it, and is already argued sound in `Drop` below (the span stays RESERVED so the VA is still
/// recyclable, `committed` is recorded as 0 so `ensure` recommits on demand, and a recommitted
/// page reads as zeros — the tenured copy paths write before they read). It was opt-in, which
/// means the default build pays the retention.
///
/// The default is now ON **for spans big enough to be worth a syscall**. Below the threshold
/// the RSS is rounding error and a `VirtualFree`/`VirtualAlloc` pair per park is pure churn on
/// the fork path, which drops a worker heap per region; above it, one `MEM_DECOMMIT` returns
/// the whole retained working set. `RAVA_ARENA_DECOMMIT` still forces it at any size (that is
/// the measurement handle for the small end); `RAVA_NO_PARK_DECOMMIT=1` turns the default off.
#[inline]
fn park_decommit_big(committed: usize) -> bool {
    /// 64 MiB — a from-space or a worker tenured that reached this is a real RSS line item;
    /// a nursery-sized park is not.
    const BIG: usize = 64 * 1024 * 1024;
    if committed < BIG {
        return false;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_PARK_DECOMMIT").is_none() && !crate::flags::emit_more_disabled()
    })
}

impl Drop for Region {
    fn drop(&mut self) {
        if self.view {
            return; // non-owning view over another Region's memory — never unmap here.
        }
        // Retire the committed-extent entry FIRST: a recycled or decommitted span must never be
        // judged against a stale prefix, and "unknown" is the safe answer.
        reg_retire(self.ptr as u64);
        if self.pool_span != 0 {
            // Recycle the reserved VA span instead of unmapping — the NEXT_LOW cursor never
            // rewinds, so re-mapping a fresh span every fork would march it to the ceiling.
            // RAVA_ARENA_DECOMMIT: return the committed pages to the OS before parking. Without
            // this, every parked from-space (major GC swaps `old` here) retains its full
            // committed high-water FOREVER — measured as the dominant missing-RSS domain.
            // Sound: the span stays RESERVED (VA stable for reuse), `committed` is recorded as 0
            // so `ensure` recommits on demand, and recommitted pages read as zeros — the tenured
            // copy paths write before they read.
            let mut committed = self.committed;
            if committed > 0 && (arena_decommit_enabled() || park_decommit_big(committed)) {
                decommit_pages(self.ptr, committed);
                committed = 0;
            }
            if let Ok(mut pool) = RESERVED_POOL.lock() {
                pool.push((self.ptr as usize, self.pool_span, committed));
                return;
            }
        }
        unsafe { unmap_low(self.ptr, self.len) }
    }
}
