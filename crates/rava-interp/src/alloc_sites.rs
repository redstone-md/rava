//! Sampling profile of ALLOCATION SITES, keyed by return address.
//!
//! ⭐A boot makes **113,497,262 allocations**. Object births account for ~12M of them (5.9M
//! handles at two mallocs each) and the arena does not malloc at all, so ~100M come from
//! somewhere nobody has ever named. `mi_malloc` was 15% of the heaviest phase's samples, which
//! is above every other single mechanism measured this session — but a percentage without a
//! CALLER is not actionable.
//!
//! `RAVA_ALLOC_SITES=1` samples one allocation in `SAMPLE` and records its stack into two
//! lock-free tables. No allocation, no lock, no symbolisation on the hot path — addresses are
//! resolved once at dump time.
//!
//! ⚠**Read the LEAF table, not the stack table.** The stack table blurs the caller with the
//! site: an ancestor present on most stacks (`jit_call`, `invokevirtual`) ranks at the top while
//! allocating nothing itself. The first version had only that table and it read like a call
//! profile.
//!
//! ⚠And read it on `release-diag` only. `/OPT:ICF` folds identical functions, so on the normal
//! release build a site can be attributed to an unrelated symbol — the first run of this profile
//! blamed `natives::entry::class_name_of` for 33% of allocations, which has exactly three call
//! sites and cannot be.

use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

const SLOTS: usize = 8192;
const DEPTH: usize = 12;
/// One in this many allocations is sampled. 4096 gives ~28k samples over a boot.
const SAMPLE: u64 = 4096;

/// The innermost frame — the function that actually called the allocator.
static LEAF_PC: [AtomicUsize; SLOTS] = [const { AtomicUsize::new(0) }; SLOTS];
static LEAF_HITS: [AtomicUsize; SLOTS] = [const { AtomicUsize::new(0) }; SLOTS];
/// Every frame on the stack, for reading the path an allocation arrived through.
static PC: [AtomicUsize; SLOTS] = [const { AtomicUsize::new(0) }; SLOTS];
static HITS: [AtomicUsize; SLOTS] = [const { AtomicUsize::new(0) }; SLOTS];
/// The top `SIG` frames as ONE key. The leaf table names generic helpers — `write_str`,
/// `do_reserve_and_handle` — which are the allocator's callers but not the engine's reason for
/// allocating; only a correlated path answers "who is building this string?".
const SIG: usize = 4;
static SIG_PC: [[AtomicUsize; SIG]; SLOTS] = [const { [const { AtomicUsize::new(0) }; SIG] }; SLOTS];
static SIG_HITS: [AtomicUsize; SLOTS] = [const { AtomicUsize::new(0) }; SLOTS];

/// ⛔Plain atomic, never a `OnceLock`: this is read from inside the global allocator, and a
/// lazily-initialised flag deadlocks there (`mem_meter::BATCH_OFF` documents the failure).
static ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn init_from_env() {
    ON.store(std::env::var_os("RAVA_ALLOC_SITES").is_some(), Relaxed);
    init_sizes_from_env();
}

#[inline]
pub fn on() -> bool {
    ON.load(Relaxed)
}

thread_local! {
    /// ⚠Without this the capture below re-enters the allocator and recurses to death.
    static BUSY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn record_in(pcs: &[AtomicUsize; SLOTS], hits: &[AtomicUsize; SLOTS], pc: usize) {
    if pc == 0 {
        return;
    }
    // Open addressing, insert-only. A full table simply stops learning new sites.
    let mut i = (pc.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 17) % SLOTS;
    for _ in 0..16 {
        let cur = pcs[i].load(Relaxed);
        if cur == pc {
            hits[i].fetch_add(1, Relaxed);
            return;
        }
        if cur == 0 && pcs[i].compare_exchange(0, pc, Relaxed, Relaxed).is_ok() {
            hits[i].fetch_add(1, Relaxed);
            return;
        }
        i = (i + 1) % SLOTS;
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn RtlCaptureStackBackTrace(skip: u32, count: u32, frames: *mut usize, hash: *mut u32) -> u16;
}

/// Called from the counting allocator on every allocation; samples one in `SAMPLE`.
#[inline]
pub fn note(n: u64) {
    if n % SAMPLE != 0 {
        return;
    }
    let _ = BUSY.try_with(|b| {
        if b.get() {
            return;
        }
        b.set(true);
        #[cfg(windows)]
        {
            let mut frames = [0usize; DEPTH];
            // Skip 3: this function, the allocator shim, `__rust_alloc`.
            let got = unsafe {
                RtlCaptureStackBackTrace(3, DEPTH as u32, frames.as_mut_ptr(), std::ptr::null_mut())
            };
            if got > 0 {
                record_in(&LEAF_PC, &LEAF_HITS, frames[0]);
                record_sig(&frames[..(got as usize).min(SIG)]);
            }
            for &f in frames.iter().take(got as usize) {
                record_in(&PC, &HITS, f);
            }
        }
        b.set(false);
    });
}

/// Insert-only, open-addressed on a hash of the whole prefix.
fn record_sig(top: &[usize]) {
    let mut h: usize = 0xcbf2_9ce4_8422_2325;
    for &f in top {
        h = (h ^ f).wrapping_mul(0x0100_0000_01b3);
    }
    let mut i = (h >> 17) % SLOTS;
    for _ in 0..16 {
        let first = SIG_PC[i][0].load(Relaxed);
        if first != 0 && SIG_PC[i].iter().zip(top).all(|(a, &b)| a.load(Relaxed) == b) {
            SIG_HITS[i].fetch_add(1, Relaxed);
            return;
        }
        if first == 0 && SIG_PC[i][0].compare_exchange(0, top[0], Relaxed, Relaxed).is_ok() {
            for (slot, &f) in SIG_PC[i].iter().zip(top).skip(1) {
                slot.store(f, Relaxed);
            }
            SIG_HITS[i].fetch_add(1, Relaxed);
            return;
        }
        i = (i + 1) % SLOTS;
    }
}

fn ranked(pcs: &[AtomicUsize; SLOTS], hits: &[AtomicUsize; SLOTS]) -> (Vec<(usize, usize)>, usize) {
    let mut v: Vec<(usize, usize)> = (0..SLOTS)
        .filter_map(|i| {
            let pc = pcs[i].load(Relaxed);
            let h = hits[i].load(Relaxed);
            if pc != 0 && h != 0 { Some((h, pc)) } else { None }
        })
        .collect();
    v.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    let total = v.iter().map(|&(h, _)| h).sum();
    (v, total)
}

/// Ranked sites, symbolised. Called once, from the profiler dump.
pub fn report(top: usize) -> String {
    let mut s = String::new();
    let (leaf, ltotal) = ranked(&LEAF_PC, &LEAF_HITS);
    s.push_str(&format!(
        "\n[ALLOC-LEAF] the function that CALLED the allocator — {ltotal} sampled allocations (1 in {SAMPLE}), {} distinct sites\n",
        leaf.len()
    ));
    for &(h, pc) in leaf.iter().take(top) {
        s.push_str(&format!(
            "[ALLOC-LEAF] {:>7} {:>5.1}%  {}\n",
            h,
            if ltotal == 0 { 0.0 } else { h as f64 * 100.0 / ltotal as f64 },
            crate::pc_profile::symbolize(pc),
        ));
    }
    let mut sigs: Vec<(usize, usize)> = (0..SLOTS)
        .filter_map(|i| {
            let h = SIG_HITS[i].load(Relaxed);
            if h != 0 && SIG_PC[i][0].load(Relaxed) != 0 { Some((h, i)) } else { None }
        })
        .collect();
    sigs.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    let stotal: usize = sigs.iter().map(|&(h, _)| h).sum();
    s.push_str(&format!("[ALLOC-PATH] top {SIG}-frame paths, {} distinct
", sigs.len()));
    for &(h, i) in sigs.iter().take(top) {
        s.push_str(&format!(
            "[ALLOC-PATH] {:>7} {:>5.1}%
",
            h,
            if stotal == 0 { 0.0 } else { h as f64 * 100.0 / stotal as f64 }
        ));
        for slot in SIG_PC[i].iter() {
            let pc = slot.load(Relaxed);
            if pc != 0 {
                s.push_str(&format!("[ALLOC-PATH]            <- {}
", crate::pc_profile::symbolize(pc)));
            }
        }
    }
    let (v, total) = ranked(&PC, &HITS);
    s.push_str(&format!(
        "[ALLOC-SITES] WHOLE STACKS (an ancestor ranks high without allocating) — {} distinct frames, {total} frame samples\n",
        v.len()
    ));
    for &(h, pc) in v.iter().take(top) {
        s.push_str(&format!(
            "[ALLOC-SITES] {:>7} {:>5.1}%  {}\n",
            h,
            if total == 0 { 0.0 } else { h as f64 * 100.0 / total as f64 },
            crate::pc_profile::symbolize(pc),
        ));
    }
    s
}

/// **A SIZE HISTOGRAM, BECAUSE INLINING CANNOT MOVE A SIZE.**
///
/// The leaf census names the function an allocation was INLINED INTO, which is why its top row
/// (`is_offloadable_rec::closure$1`, 5.1%) is contradicted by that analysis's own counters
/// reading zero. A byte count has no symbol to fold and no frame to attribute, so this survives
/// the optimiser intact — and on a 52 M-allocation workload the shape of the distribution is
/// what says whether the mass is boxes, small vectors, or strings.
///
/// `RAVA_ALLOC_SIZES=1`. Power-of-two buckets; the counter is one relaxed add on a path that
/// already does several, and it is skipped entirely when the flag is off.
static SIZE_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[allow(clippy::declare_interior_mutable_const)]
const SZ0: AtomicUsize = AtomicUsize::new(0);
static SIZE_BUCKETS: [AtomicUsize; 40] = [SZ0; 40];

pub(crate) fn init_sizes_from_env() {
    SIZE_ON.store(std::env::var_os("RAVA_ALLOC_SIZES").is_some(), Relaxed);
}

#[inline]
pub fn note_size(bytes: i64) {
    if !SIZE_ON.load(Relaxed) || bytes <= 0 {
        return;
    }
    // 64 - leading_zeros gives the bucket: 1 -> 1, 8 -> 4, 24 -> 5, 1024 -> 11.
    let b = (64 - (bytes as u64).leading_zeros()) as usize;
    SIZE_BUCKETS[b.min(39)].fetch_add(1, Relaxed);
    note_region(bytes);
}

pub(crate) fn sizes_on() -> bool {
    SIZE_ON.load(Relaxed)
}

pub(crate) fn size_report() -> String {
    let mut out = String::from("[ALLOC-SIZE] allocations by size, power-of-two buckets
");
    let total: usize = SIZE_BUCKETS.iter().map(|b| b.load(Relaxed)).sum();
    if total == 0 {
        return String::new();
    }
    for (i, b) in SIZE_BUCKETS.iter().enumerate() {
        let n = b.load(Relaxed);
        if n == 0 {
            continue;
        }
        let lo = if i == 0 { 0 } else { 1usize << (i - 1) };
        let hi = (1usize << i) - 1;
        out.push_str(&format!(
            "[ALLOC-SIZE] {lo:>10}..{hi:<10} {n:>12}  {:>5.1}%
",
            n as f64 * 100.0 / total as f64
        ));
    }
    out.push_str(&format!("[ALLOC-SIZE] total {total}
"));
    out
}

/// **ATTRIBUTION BY REGION — the answer the leaf census could not give.**
///
/// A leaf sample names the function an allocation was INLINED INTO; a size has no symbol at all.
/// Between them sits the question that actually matters: *which part of the engine* is allocating.
/// A thread-local tag, set by a guard at three or four coarse boundaries, survives every
/// optimisation because it is data, not a frame — and the boundaries are exactly the spans this
/// ledger already prices.
///
/// `RAVA_ALLOC_SIZES=1` turns it on with the histogram.
#[derive(Clone, Copy)]
pub(crate) enum Region {
    Other = 0,
    /// Inside `fu_fastpath` — the native fast path, 38 M calls a boot.
    FuFast = 1,
    /// Inside `jit_call_body` — the general compiled→host crossing with its synthetic frame.
    Seam = 2,
    /// Inside `invoke_framed` — an interpreter frame.
    Interp = 3,
    /// Inside the interpreter's NATIVE cascade (`natives::entry::try_invoke`) — the JDK
    /// implementations reached from interpreted guest code.
    Native = 4,
    /// Inside a COMPILED guest body (entered through `jit_enter`). Without this the compiled
    /// callee's object births are charged to whichever interpreted frame called it.
    Compiled = 5,
}

thread_local! {
    static REGION: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

/// **THE SAME TAG, PUBLISHED WHERE A SAMPLER CAN READ IT.**
///
/// The allocation census reads the region from the allocating thread's own TLS. A PC sampler
/// cannot: it SUSPENDS another thread and reads its registers, and TLS is not reachable that way.
/// So the guard also writes the tag here, and `pc_profile` reads it at sample time — which turns
/// the same boundaries into a TIME attribution.
///
/// ⚠**One global slot, and the assumption is stated:** guest code runs under the GIL and the boot
/// measures 1.08 cores, so at most one thread is inside these regions at a time. A future
/// genuinely-parallel run would blur this and needs a per-thread slot keyed the way the sampler
/// keys its targets.
pub(crate) static REGION_NOW: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Armed by the allocation census OR by the region-time profile — the guards are shared.
static TIME_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn arm_region_time() {
    TIME_ON.store(true, Relaxed);
}

#[inline]
pub(crate) fn regions_armed() -> bool {
    SIZE_ON.load(Relaxed) || TIME_ON.load(Relaxed)
}
static REGION_N: [AtomicUsize; 6] = [SZ0; 6];
static REGION_B: [AtomicUsize; 6] = [SZ0; 6];

/// Sets the region for its lifetime and restores the previous one — regions NEST (a seam
/// crossing runs an interpreter frame), and the innermost is the one that allocated.
pub(crate) struct RegionGuard(u8);

impl RegionGuard {
    #[inline]
    pub(crate) fn enter(r: Region) -> Option<Self> {
        if !regions_armed() {
            return None;
        }
        let prev = REGION.with(|c| {
            let p = c.get();
            c.set(r as u8);
            p
        });
        REGION_NOW.store(r as u8, Relaxed);
        Some(RegionGuard(prev))
    }
}

impl Drop for RegionGuard {
    #[inline]
    fn drop(&mut self) {
        REGION.with(|c| c.set(self.0));
        REGION_NOW.store(self.0, Relaxed);
    }
}

#[inline]
fn note_region(bytes: i64) {
    let r = REGION.with(|c| c.get()) as usize;
    if r < 6 {
        REGION_N[r].fetch_add(1, Relaxed);
        REGION_B[r].fetch_add(bytes.max(0) as usize, Relaxed);
    }
}

pub(crate) fn region_report() -> String {
    let names = [
        "other",
        "fu_fastpath",
        "seam(jit_call_body)",
        "interpreter frame",
        "native cascade",
        "compiled guest body",
    ];
    let tot: usize = REGION_N.iter().map(|c| c.load(Relaxed)).sum();
    if tot == 0 {
        return String::new();
    }
    let mut out = String::from("[ALLOC-REGION] allocations by engine region (nested: innermost wins)
");
    for i in 0..6 {
        let n = REGION_N[i].load(Relaxed);
        let b = REGION_B[i].load(Relaxed);
        out.push_str(&format!(
            "[ALLOC-REGION] {:<22} {n:>12}  {:>5.1}%   {:>12} bytes
",
            names[i],
            n as f64 * 100.0 / tot as f64,
            b
        ));
    }
    out
}

/// Region ticks taken by the PC sampler — the TIME twin of `REGION_N`.
static REGION_TICKS: [AtomicUsize; 6] = [SZ0; 6];

/// Called once per PC sample. One relaxed load and one increment; the sampler already pays a
/// thread suspend per sample, so this is free by comparison.
#[inline]
pub(crate) fn note_region_tick() {
    let r = REGION_NOW.load(Relaxed) as usize;
    if r < 6 {
        REGION_TICKS[r].fetch_add(1, Relaxed);
    }
}

pub(crate) fn region_time_report() -> String {
    let names = [
        "other (host, unowned)",
        "fu_fastpath",
        "seam(jit_call_body)",
        "interpreter frame",
        "native cascade",
        "compiled guest body",
    ];
    let tot: usize = REGION_TICKS.iter().map(|c| c.load(Relaxed)).sum();
    if tot == 0 {
        return String::new();
    }
    let mut out = String::from(
        "[REGION-TIME] PC samples by engine region — mutually exclusive, innermost wins
",
    );
    for i in 0..6 {
        let n = REGION_TICKS[i].load(Relaxed);
        out.push_str(&format!(
            "[REGION-TIME] {:<24} {n:>10}  {:>5.1}%
",
            names[i],
            n as f64 * 100.0 / tot as f64
        ));
    }
    out.push_str(&format!("[REGION-TIME] total samples {tot}
"));
    out
}

/// RIPs sampled while the innermost region was the SEAM — the crossing's own marshalling, with
/// the callee excluded by construction.
///
/// ⭐The region tick says the seam is 20.3% of the boot; it cannot say WHICH part of the crossing.
/// Symbolising only these samples answers that without a second profiling mode: the ordinary
/// `[PC-PROFILE]` table mixes the crossing with everything it calls, which is exactly the overlap
/// this instrument exists to remove.
pub(crate) static SEAM_RIPS: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

/// Cap so a long boot cannot grow this without bound; 200k samples is far more than any run
/// produces and keeps the memory at ~1.6 MB.
const SEAM_CAP: usize = 200_000;

#[inline]
pub(crate) fn note_seam_rip(rip: usize) {
    if REGION_NOW.load(Relaxed) != Region::Seam as u8 {
        return;
    }
    if let Ok(mut v) = SEAM_RIPS.lock() {
        if v.len() < SEAM_CAP {
            v.push(rip);
        }
    }
}

/// RIPs sampled while the innermost region was COMPILED — kept so the dump can split that 63%
/// into MACHINE CODE and the callbacks it makes into the host.
///
/// ⭐The region says where the engine is; the compiled-range test says whether the instruction
/// pointer is in emitted code. Neither alone separates "the compiler emits slow code" from "the
/// runtime it calls is slow", and those are two different projects.
pub(crate) static COMPILED_RIPS: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

#[inline]
pub(crate) fn note_compiled_rip(rip: usize) {
    if REGION_NOW.load(Relaxed) != Region::Compiled as u8 {
        return;
    }
    if let Ok(mut v) = COMPILED_RIPS.lock() {
        if v.len() < SEAM_CAP {
            v.push(rip);
        }
    }
}
