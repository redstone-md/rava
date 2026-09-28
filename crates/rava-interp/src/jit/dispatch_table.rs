//! The per-site direct-call table the machine-code PIC consults, plus the census that measures
//! its coverage. Positive fills come from validated resolved-dispatch facts; the byte budget
//! bounds allocation independently of which sites receive tables.
//!
//! A table is allocated on the first validated positive fill for a site. Absence, empty entries,
//! and budget refusal all produce a host fallback, so admission changes cache coverage, not the
//! selected method.
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Layout: word 0 = capacity (entry count), words 1..=cap = fptr per class id.
///
/// The measured boot class-id space fits in 4096 slots. The configured byte budget also accounts
/// for full replacement allocations because old tables remain immortal for concurrent readers.
const MIN_CAP: usize = 4096;

pub(crate) fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    // DEFAULT ON (`RAVA_NO_DISPATCH_TABLE` opts out). It measured -1.5% wall alone,
    // which this box's paired A/B cannot separate from noise (+-4%) — but the
    // arithmetic says a 10 ns/call saving IS 1.6% of wall, so "inside the noise"
    // and "worth nothing" are different statements and only the first was
    // established. It ships as part of a stack that is measured as one.
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_DISPATCH_TABLE").is_none())
}


/// Machine-code table HITS and in-range PROBES, incremented inline by the emitted probe
/// (only when `picprobe` is also armed, exactly like `PIC_HIT`). Without these two, "did
/// the table serve anything" is an inference from the fill count, and this tree has paid
/// three times for treating a plausible mechanism as a measured one.
pub(crate) static DT_HIT: AtomicU64 = AtomicU64::new(0);
pub(crate) static DT_PROBE: AtomicU64 = AtomicU64::new(0);
pub(crate) fn dt_hit_addr() -> i64 {
    &DT_HIT as *const AtomicU64 as i64
}
pub(crate) fn dt_probe_addr() -> i64 {
    &DT_PROBE as *const AtomicU64 as i64
}

/// RECEIVER SHAPE OF EVERY PIC MISS. The table probe turned out to be reached on only
/// 1,572,526 of 289,918,574 misses (0.54%) while hitting 96.2% of the time WHEN reached —
/// i.e. it works and is almost never on the path. That can only mean the misses bypass the
/// N-way probe before it, and the emitted control flow says how: a non-arena receiver
/// branches straight to `miss` (or to the heap-Ref 4-way, which evicts exactly like the
/// arena one did). Buckets: 0 = null, 1 = arena object, 2 = heap Ref, 3 = boxed/array/other.
pub(crate) static MISS_SHAPE: [AtomicU64; 4] =
    [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];

/// Classify one miss. Called from the miss handler, which is already the slow path, and
/// only when `picprobe` is armed.
pub(crate) fn note_miss_shape(recv: i64) {
    if !crate::picprobe::enabled() {
        return;
    }
    let hi = (recv as u64) >> 40;
    let b = if recv == 0 {
        0
    } else if hi == 0 {
        1 // an 8-aligned arena pointer: no tag bits above bit 40
    } else if hi == 1 {
        2 // REF_TAG
    } else {
        3 // BOX / ARRAY / ABOX (hi = 2 / 4 / 8)
    };
    MISS_SHAPE[b].fetch_add(1, Relaxed);
}

/// Cache fill is disabled under IC_PARALLEL because its cache pointer refers to a worker-local
/// 16-way slice, not the global per-site box that owns this table pointer.
/// A miss that the HOST could answer — the freeze table or the frozen direct cache already
/// holds a compiled fptr for this (site, receiver class) and is about to call it. This is the
/// population that matters: ~258M of the ~300M PIC misses return from `rava_dispatch_miss`
/// through exactly these arms, before the resolve is ever reached (`[IC-MISS] ENTRY` is only
/// 42.4M). They are not unresolvable and they are not thrash — the answer is known, and the
/// entire cost is the machine-code EXIT taken to go and fetch it.
///
/// Record a validated positive fill from an arena receiver. Its class key matches the emitted PIC
/// probe, including the lambda-site key when required.
pub(crate) fn note_serve(cache_ptr: i64, recv: i64, fill: i64) {
    if !enabled() || cache_ptr == 0 || fill <= 0 || super::ic_parallel_enabled() {
        return;
    }
    if recv == 0 || (recv as u64) >> 40 != 0 {
        return; // not an arena object: no class id to index by
    }
    if crate::picprobe::enabled() {
        SERVES.fetch_add(1, Relaxed);
    }
    let class_id = unsafe { *(recv as *const u32) };
    #[cfg(feature = "next-gen-gc")]
    let class_id = match crate::arena::ArenaPtr::new(recv as *mut u8) {
        Some(p) => crate::nextgen::lambdas::lambda_table_key(p, class_id),
        None => class_id,
    };
    let slot = (cache_ptr + super::DISPATCH_TABLE_SLOT as i64) as *mut i64;
    store(slot, class_id, fill);
}



/// Opt-out for direct-entry table learning, retained for same-binary pricing.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn learn_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_DT_LEARN").is_none())
}

/// Controls deferred wide-parameter handling in `freeze_resolve`; table admission itself always
/// follows validated positive fills.
pub(crate) fn fast_dcache() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_DCACHE").is_none() && !crate::flags::emit_more_disabled()
    })
}






/// `RAVA_LAMBDA_NO_ADMIT` — keep the emitted lambda table key and refuse the entries, so the two
/// halves of `RAVA_LAMBDA_SITE_CID` can be priced apart. Hoisted out of the serve arm below.
#[cfg(feature = "next-gen-gc")]
fn lambda_no_admit() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_LAMBDA_NO_ADMIT").is_some())
}

/// The direct-cache arm calls `note_serve` only after its `DirectEntry` has passed the ABI,
/// frame-registration, and receiver-kind checks required by the emitted machine-code call path.
///
/// ⚠ THE ENTRY MUST BE CALLABLE THE WAY THE MACHINE-CODE HIT ARM CALLS IT, WHICH IS NOT THE WAY
/// `jit_direct_invoke` CALLS IT. Getting this wrong is a silent call into another method with the
/// caller's buffer — a plausible answer, not a crash. Every clause below names the code that
/// requires it:
///
/// * `leaf` — `DirectEntry::leaf` is documented for exactly this: a GC-leaf body whose locals fit
///   the dispatch call buffer, which is what lets a hit arm with no Rust wrapper to `cells_push`
///   for it use a plain `call_indirect`. Encoded `fptr|0`.
/// * `self_reg` under `ic_deep_enabled()` — the Deep-IC v3 prologue registers the callee's own
///   frame, which is the other half of `dispatch_resolve_cacheable`'s admission
///   (`if deep && !f.self_reg { return NeverCacheable }`). Encoded `fptr|1`, and the buffer-fit
///   precondition that resolve checks as `max_locals + max_stack <= dispatch_callbuf_slots()` is
///   re-checked here against `buf_len`.
/// * no lambda machinery — `shift` drops raw[0], `cap_n` prepends re-fetched captures, and
///   `arg0_guard`/`lam_fp`/`site_key` are per-call re-checks the machine-code arm does not
///   perform. Any of them set means the entry is not a plain slot-buffer call.
///
/// Wide (long/double) parameters are NOT excluded, and that is the point. `freeze_resolve::put`
/// refuses them because its serve arm gets the PACKED miss-callback buffer; the table probe runs
/// INSIDE the PIC, on the SLOT buffer the caller emitted, where a wide arg already owns its pair
/// slot. `wide_refused=17` sites are declined by the freeze table, and the ring's hottest callee
/// `ImprovedNoise.sampleAndLerp(IIIDDDD)D` is one of them.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn note_serve_direct(cache_ptr: i64, recv: i64, kind: u8, entry: &super::DirectEntry) {
    if !learn_enabled() || !enabled() || cache_ptr == 0 || super::ic_parallel_enabled() {
        return;
    }
    // RECEIVER-BEARING KINDS ONLY (1 = virtual, 3 = interface). `note_serve` derives the table key
    // by DEREFERENCING `recv` — it rejects only null and the tagged high bits, so on a static or
    // invokespecial site, where `recv` is the first ARGUMENT rather than a receiver, a small
    // integer argument passes both filters and address 5 gets dereferenced. Nothing observed
    // reaches here with those kinds (`[PIC-COV]` puts every static/special site on `emit_invoke` →
    // `rava_invoke`, not on the PIC miss path), so this closes a class rather than a bug — but it
    // costs one comparison and the failure it prevents is a segfault.
    if kind != 1 && kind != 3 {
        learn_refused(0);
        return;
    }
    // SPLIT, because "lambda machinery" refused 6,629,289 of 6,629,289 and ADMITTED 1,442 —
    // and a five-way `||` names nothing. Each field gets its own bucket so the clause that
    // actually starves the table is identifiable rather than inferred.
    // SHIFT (a lambda target taking `raw[1..]`) is admissible only when BOTH halves are in
    // place, and each is checked here rather than assumed:
    //   * the KEY must be exact — under `RAVA_LAMBDA_SITE_CID` an arena lambda's class id is per
    //     impl SITE, and a site's `LambdaSite` determines the target. Without it every lambda of
    //     one capture signature shares an id and a hit would call ANOTHER site's implementation.
    //   * the ABI must be emitted — the arena hit arm applies `emit_lambda_shift_adjust` under
    //     the same flag, so bit 1 means "call with buf+8".
    // Refusing this clause is what kept `fills` at 4,706 while 19,492,939 serves went past.
    let shift_bit = if entry.shift {
        // Split from the emitted-key half so each can be priced alone: the flag turns on BOTH,
        // and `RAVA_LAMBDA_NO_ADMIT` keeps the key and refuses the entries.
        //
        // READ ONCE. This was a bare `std::env::var_os` on the direct-cache serve arm — a scan of
        // the process environment block (and, off Windows, the env lock) per call, for a value
        // that is a constant of the process. It is short-circuited away while
        // `RAVA_LAMBDA_SITE_CID` is unset, which is the default, so it has been latent rather than
        // paid; with the key armed the clause's own comment counts 19,492,939 serves through here.
        // was O(env entries) per serve, is O(1) per process.
        if !crate::nextgen::lambdas::site_cid_enabled() || lambda_no_admit() {
            learn_refused(7);
            return;
        }
        2i64
    } else {
        0
    };
    if entry.cap_n != 0 {
        learn_refused(8);
        return;
    }
    if entry.arg0_guard != 0 {
        learn_refused(9);
        return;
    }
    if entry.lam_fp != 0 {
        learn_refused(10);
        return;
    }
    if entry.site_key != 0 {
        learn_refused(11);
        return;
    }
    let fill = if entry.leaf {
        entry.fptr as i64 | shift_bit
    } else if entry.self_reg {
        if !super::ic_deep_enabled() {
            learn_refused(2);
            return;
        }
        if (entry.buf_len as usize) > super::dispatch_callbuf_slots() {
            learn_refused(3);
            return;
        }
        (entry.fptr as i64) | 1 | shift_bit
    } else {
        learn_refused(4);
        return;
    };
    if fill <= 0 {
        learn_refused(5);
        return;
    }
    learn_refused(6); // admitted
    note_serve(cache_ptr, recv, fill);
}

/// Diagnostics for direct-entry fills refused by structural safety checks. Enabled only by the
/// PIC probe; these counters do not participate in table admission.
#[cfg(feature = "next-gen-gc")]
pub(crate) static LEARN_REFUSE: [AtomicU64; 12] = [
    AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
    AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
    AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
];

#[cfg(feature = "next-gen-gc")]
#[inline]
fn learn_refused(which: usize) {
    if crate::picprobe::enabled() {
        LEARN_REFUSE[which].fetch_add(1, Relaxed);
    }
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn learn_refusal_line() -> String {
    if !crate::picprobe::enabled() {
        return String::new();
    }
    let v: Vec<u64> = LEARN_REFUSE.iter().map(|a| a.load(Relaxed)).collect();
    format!(
        "[DT-LEARN] kind!=virt/iface={} deep_ic_off={} buf_too_wide={} not_leaf_nor_selfreg={}          fptr_zero={} | shift={} cap_n={} arg0_guard={} lam_fp={} site_key={} | ADMITTED={}",
        v[0], v[2], v[3], v[4], v[5], v[7], v[8], v[9], v[10], v[11], v[6]
    )
}

/// Bytes currently committed to per-site tables and budget-refused allocations.
pub(crate) static TABLE_BYTES: AtomicU64 = AtomicU64::new(0);
pub(crate) static BUDGET_REFUSED: AtomicU64 = AtomicU64::new(0);

/// Default ceiling on total bytes reserved for per-site tables. A grow reserves the full
/// replacement allocation because the old table stays live for concurrent readers. The default
/// is 64 MiB; `RAVA_DT_BUDGET_MB=0` explicitly disables the ceiling.
fn table_byte_budget() -> u64 {
    static V: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("RAVA_DT_BUDGET_MB")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(|mb| if mb == 0 { u64::MAX } else { mb * 1024 * 1024 })
            .unwrap_or(64 * 1024 * 1024)
    })
}
/// Tables are immortal during concurrent growth, so budget the full replacement allocation too.
fn table_allocation_bytes(cap: usize) -> u64 {
    (cap as u64 + 1) * std::mem::size_of::<i64>() as u64
}

/// Claim `want` bytes against the ceiling, or refuse. Reserve BEFORE allocating so two racing
/// workers cannot both pass the check; a loser of the publication CAS below leaks its reservation,
/// which only makes the budget slightly conservative — never larger than it claims.
fn reserve(want: u64) -> bool {
    if TABLE_BYTES.fetch_add(want, Relaxed) + want > table_byte_budget() {
        TABLE_BYTES.fetch_sub(want, Relaxed);
        BUDGET_REFUSED.fetch_add(1, Relaxed);
        return false;
    }
    true
}

pub(crate) static SERVES: AtomicU64 = AtomicU64::new(0);

/// Sites holding a table. Under first-fill admission this is the same as sites with at least one
/// positive resolved verdict, subject to the allocation budget.
pub(crate) static PROMOTED: AtomicU64 = AtomicU64::new(0);
pub(crate) static FILLS: AtomicU64 = AtomicU64::new(0);
pub(crate) static GROWS: AtomicU64 = AtomicU64::new(0);
pub(crate) static MAX_CLASS_ID: AtomicU64 = AtomicU64::new(0);
pub(crate) static ENTRIES: AtomicU64 = AtomicU64::new(0);

fn alloc(cap: usize) -> *mut i64 {
    let mut v: Vec<i64> = vec![0; cap + 1];
    v[0] = cap as i64;
    let p = v.as_mut_ptr();
    std::mem::forget(v); // immortal, exactly like the site box and every compiled fptr
    p
}

/// Publish a resolved positive verdict for `(site, class_id)`. Callers hold the resolve lock,
/// which permits sibling cache writes; negative verdicts remain host fallbacks.
pub(crate) fn note(cache_ptr: i64, class_id: u32, fill: i64) {
    if !enabled() || cache_ptr == 0 || super::ic_parallel_enabled() || fill <= 0 {
        return;
    }
    let slot = (cache_ptr + super::DISPATCH_TABLE_SLOT as i64) as *mut i64;
    store(slot, class_id, fill);
}


/// Publish `(class_id -> fill)` into the site's table, allocating or growing it as needed.
///
/// CONCURRENCY. `note_serve` reaches this from gil-free WORKERS with no lock held — the
/// freeze-table serve arm it hooks is deliberately lock-free — so two workers can race to
/// allocate or grow the same site. The table pointer is therefore published with a
/// compare-exchange: the loser frees nothing (tables are immortal) and simply retries against
/// whichever table won, so a race costs one wasted allocation and never a lost or torn entry.
/// Entry stores are plain aligned 8-byte writes of an immortal fptr; a machine-code reader
/// sees either 0 (→ fall to the host, the status quo) or a complete valid pointer.
fn store(slot: *mut i64, class_id: u32, fill: i64) {
    if crate::picprobe::enabled() {
        MAX_CLASS_ID.fetch_max(class_id as u64, Relaxed);
    }
    let a = unsafe { &*(slot as *const std::sync::atomic::AtomicI64) };
    loop {
        let mut tbl = a.load(std::sync::atomic::Ordering::Acquire);
        if tbl == 0 {
            let cap = MIN_CAP.max((class_id as usize + 1).next_power_of_two());
            // Budget the allocation before creating it. Refusal is safe: without a table, the
            // emitted probe misses and takes the existing host fallback.
            if !reserve(table_allocation_bytes(cap)) {
                return;
            }
            let nt = alloc(cap) as i64;
            match a.compare_exchange(
                0,
                nt,
                std::sync::atomic::Ordering::Release,
                std::sync::atomic::Ordering::Acquire,
            ) {
                Ok(_) => {
                    PROMOTED.fetch_add(1, Relaxed);
                    ENTRIES.fetch_add(cap as u64, Relaxed);
                    tbl = nt;
                }
                Err(won) => tbl = won, // someone else promoted first — use theirs
            }
        }
        let cap = unsafe { *(tbl as *const i64) } as usize;
        if class_id as usize >= cap {
            // GROW. The old table is leaked rather than freed and the new one is a COPY, so a
            // concurrent reader observing the old table, the new one, or the pointer mid-swap
            // is correct in all three cases — and a table miss only falls to the host, which
            // is the status quo rather than a wrong method.
            let ncap = (class_id as usize + 1).next_power_of_two().max(cap * 2);
            if !reserve(table_allocation_bytes(ncap)) {
                return;
            }
            let nt = alloc(ncap);
            unsafe { std::ptr::copy_nonoverlapping((tbl as *const i64).add(1), nt.add(1), cap) };
            if a
                .compare_exchange(
                    tbl,
                    nt as i64,
                    std::sync::atomic::Ordering::Release,
                    std::sync::atomic::Ordering::Acquire,
                )
                .is_err()
            {
                continue; // lost the grow race — re-read and try again
            }
            GROWS.fetch_add(1, Relaxed);
            ENTRIES.fetch_add((ncap - cap) as u64, Relaxed);
            tbl = nt as i64;
        }
        // READ BEFORE WRITE. This runs on every SERVE, not only on a new verdict, so the slot is
        // overwritten with the value it already holds millions of times a boot — and a store to a
        // shared line is not free: it takes the line exclusive and bounces it between every core
        // dispatching that site. Measured, and the reason this clause exists: admitting lambda
        // entries (ledger row 334) took the serve population from ~1,400 to ~20 M and ChunkFill
        // from **4,770 ms to 61,706 ms**; the writes, not the entries, were the cost.
        let cellp = unsafe { (tbl as *mut i64).add(1 + class_id as usize) };
        if unsafe { *cellp } == fill {
            return;
        }
        unsafe { *cellp = fill };
        if crate::picprobe::enabled() {
            FILLS.fetch_add(1, Relaxed);
        }
        return;
    }
}

/// One-line census, printed by the lane probe's reporter alongside the PIC coverage split.
///
/// ⚠**AND THAT IS THE ONLY PLACE IT PRINTS, WHICH MAKES IT UNREADABLE ON A PROBE.** The lane
/// probe's reporter runs inside a deferral-lane region; `bench/ShapeOr.java` has none, so
/// `RAVA_LANE_PROBE` produces nothing there and the `miss_shape` split — the one number that
/// would name the ~12 M PIC misses on that workload which are NOT capturing lambdas — cannot be
/// read on the sharpest probe this tree has. Whoever needs it next should print it from
/// `pc_profile`'s periodic dump the way `[PC-CB]` and `[PC-ARM]` already are.
pub(crate) fn report() -> String {
    if !enabled() {
        return String::new();
    }
    format!(
        "mc_hits={} mc_probes={} serves={} miss_shape[null={} arena={} heapref={} boxed/other={}] \
         promoted_sites={} fills={} grows={} max_class_id={} table_entries={} (~{} KB) budget_refused={}",
        DT_HIT.load(Relaxed),
        DT_PROBE.load(Relaxed),
        SERVES.load(Relaxed),
        MISS_SHAPE[0].load(Relaxed),
        MISS_SHAPE[1].load(Relaxed),
        MISS_SHAPE[2].load(Relaxed),
        MISS_SHAPE[3].load(Relaxed),
        PROMOTED.load(Relaxed),
        FILLS.load(Relaxed),
        GROWS.load(Relaxed),
        MAX_CLASS_ID.load(Relaxed),
        ENTRIES.load(Relaxed),
        ENTRIES.load(Relaxed) * 8 / 1024,
        BUDGET_REFUSED.load(Relaxed),
    )
}

/// THE COMPILED SITE THE CURRENT HOST EXCURSION EXITED FROM, take-once.
///
/// `rava_dispatch_miss` knows the site; `jit_direct_call` — three frames down, through `jit_call`
/// — is where the verdict becomes known. Threading the value would touch `jit_call`'s whole caller
/// set, so it travels as a breadcrumb instead, and the breadcrumb is TAKEN rather than read:
/// `jit_call` is entered from places that are not a dispatch miss, and a stale site would publish
/// `(outer site, inner receiver class) -> inner fptr`, which makes the outer site call another
/// class's method. Take-once means only the first direct call after a miss can publish — the one
/// that miss exited for.
#[cfg(feature = "next-gen-gc")]
thread_local! {
    static DISPATCH_SITE: std::cell::Cell<i64> = const { std::cell::Cell::new(0) };
}

/// Leave the breadcrumb for the excursion about to begin.
#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn set_dispatch_site(cache_ptr: i64) {
    DISPATCH_SITE.with(|c| c.set(cache_ptr));
}

/// Consume it. Returns 0 when there is none (a nested or non-dispatch entry).
#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn take_dispatch_site() -> i64 {
    DISPATCH_SITE.with(|c| c.replace(0))
}
#[cfg(test)]
mod table_budget_tests {
    use super::table_allocation_bytes;

    #[test]
    fn replacement_table_reserves_its_full_immortal_allocation() {
        assert_eq!(table_allocation_bytes(4096), 4097 * 8);
        assert_eq!(table_allocation_bytes(8192), 8193 * 8);
    }
}
