//! 23.2 PIC-DISPATCH COVERAGE PROBE (RAVA_JIT_PIC_PROBE, inert, lock-free, no alloc). Splits every
//! dynamic call out of compiled code into its true outcome to size machine-code PIC coverage vs the
//! Rust-host fallback (the 109M direct hits + 27M synthetic-frame fall-throughs found in 23.1):
//!   • PIC_HIT   — resolved ENTIRELY in machine code (emit_dispatch_pic 4-way class-id compare →
//!                 call_indirect, no Rust callback). Counted by an inline load/add/store baked into the
//!                 hit arm (gated at emit-time). Probe runs are single-threaded ⇒ exact.
//!   • PIC_MISS  — a PIC site missed → rava_dispatch_miss (Rust). Anatomy already in [[icmiss]].
//!   • PLAIN_INVOKE[kind] — a site with NO PIC (static/special always; virtual/interface that bailed
//!                 wide_site_pic_ok/slot-budget) → rava_invoke (Rust), split by invoke kind.
//! Plus site-level compile-time tallies (how many compiled sites emit a PIC vs emit_invoke, by kind)
//! and the jit_direct_resolve refusal reasons (why a Rust-host call fell to the synthetic-frame
//! gauntlet: callee-not-compiled / native-served / receiver-shape / other). Off ⇒ zero effect.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_JIT_PIC_PROBE").is_some())
}

// --- dynamic call outcomes (frequency-weighted) ---
pub static PIC_HIT: AtomicU64 = AtomicU64::new(0); // machine-code PIC hit (no Rust callback)
pub static PIC_MISS: AtomicU64 = AtomicU64::new(0); // PIC site missed → rava_dispatch_miss
// rava_invoke (non-PIC sites) split by kind: [static, virtual, special, interface].
pub static PLAIN_INVOKE: [AtomicU64; 4] = [
    AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
];

// --- compile-time site tallies (once per compiled call site) ---
pub static SITE_PIC: AtomicU64 = AtomicU64::new(0); // site emitted a machine-code PIC
pub static SITE_INVOKE: [AtomicU64; 4] = [
    AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
];

// --- jit_direct_resolve refusal reasons (the synthetic-frame fall-through population) ---
pub static REF_NOT_COMPILED: AtomicU64 = AtomicU64::new(0); // target found but not compiled yet (tier lag)
pub static REF_NATIVE_SERVED: AtomicU64 = AtomicU64::new(0); // java/native-served owner (no compiled fptr ever)
pub static REF_RECV_SHAPE: AtomicU64 = AtomicU64::new(0); // null/boxed/array/other receiver cell
pub static REF_OTHER: AtomicU64 = AtomicU64::new(0); // lambda-mismatch / logger / arity / uncaught

/// Stable address of the PIC_HIT counter — baked as a Cranelift iconst so the machine-code hit arm
/// increments it inline (load/add/store). Single-threaded probe runs ⇒ no torn write.
pub fn pic_hit_addr() -> i64 {
    &PIC_HIT as *const AtomicU64 as i64
}

#[inline]
pub fn plain_invoke(kind: u8) {
    PLAIN_INVOKE[(kind & 3) as usize].fetch_add(1, Relaxed);
}
#[inline]
pub fn pic_miss() {
    PIC_MISS.fetch_add(1, Relaxed);
}
#[inline]
pub fn site_pic() {
    SITE_PIC.fetch_add(1, Relaxed);
}
#[inline]
pub fn site_invoke(kind: u8) {
    SITE_INVOKE[(kind & 3) as usize].fetch_add(1, Relaxed);
}
/// λ-PIC sizing: `rava_dispatch_miss` host falls whose receiver is a lambda,
/// and the `lambda_directable` (non-capturing, identity-stable) subset a
/// machine-code lambda-identity arm could serve.
pub static LAMBDA_MISS: AtomicU64 = AtomicU64::new(0);
pub static LAMBDA_DIRECTABLE_MISS: AtomicU64 = AtomicU64::new(0);

/// λ-PIC FILL census (`Vm::lambda_pic_fill`): which arm each admitted entry was
/// cached for, and why the rest were refused. This is the load-bearing readout for
/// the leaf-admission change — a zero LPIC_FILL_LEAF with a large LPIC_REFUSE[1]
/// says the population exists but no body qualifies, which is a different verdict
/// from "the population isn't there". Always counted (a fill is already cold).
pub static LPIC_FILL_DEEP: AtomicU64 = AtomicU64::new(0);
pub static LPIC_FILL_LEAF: AtomicU64 = AtomicU64::new(0);
/// 0 = entry shape (not shift / capturing / arg0-guarded), 1 = neither
/// self_reg-deep nor GC-leaf (the rooting refusal), 2 = frame outgrows the buffer.
pub static LPIC_REFUSE: [AtomicU64; 3] =
    [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];

#[inline]
pub fn lpic_refuse(reason: u8) {
    LPIC_REFUSE[(reason as usize).min(2)].fetch_add(1, Relaxed);
}
#[inline]
pub fn lpic_fill(deep: bool) {
    if deep { &LPIC_FILL_DEEP } else { &LPIC_FILL_LEAF }.fetch_add(1, Relaxed);
}

/// Box-table ROOT-SET census (sampled in `frames::boxes_gather`, i.e. once per
/// collection). `BOX_SCANS` = collections that walked it, `BOX_MAX` = the largest
/// table ever cloned, `BOX_TOTAL` = Values copied across the run. A machine-code
/// hit arm performs no `box_truncate`, so this is the direct readout of whether
/// bypassing the Rust wrapper leaks roots.
pub static BOX_SCANS: AtomicU64 = AtomicU64::new(0);
pub static BOX_MAX: AtomicU64 = AtomicU64::new(0);
pub static BOX_TOTAL: AtomicU64 = AtomicU64::new(0);

#[inline]
pub fn note_box_scan(len: u64) {
    BOX_SCANS.fetch_add(1, Relaxed);
    BOX_TOTAL.fetch_add(len, Relaxed);
    BOX_MAX.fetch_max(len, Relaxed);
}

/// CAPTURING arena-lambda feasibility census (sampled in `Vm::jit_direct_invoke`'s
/// arena capture loop). Answers the two questions a machine-code capture-copy arm
/// stands or falls on, BEFORE any codegen is written:
///
///  • which hit arm could serve the impl body at all — `CAP_LEAF` (plain call) /
///    `CAP_SELFREG` (rooted deep call) / `CAP_NEITHER` (unservable);
///  • what a capture cell actually HOLDS. `jit_direct_invoke` runs
///    `decode_arena_cell` → `box_ref`/`pack_local` on it, and only some of those
///    outcomes are a pure bit transform Cranelift can emit:
///      PRIM      — identity copy.
///      NULL      — 0.
///      REF/ARRAY — tagged cross-gen handle: `(cell >> 2) | TAG`, emittable.
///      ARENAPTR  — raw arena pointer: identity, after an arena-window bound.
///      BOXIDX    — `arena_box_get` → a side-table `Value` → `box_ref`, which for a
///                  Str/boxed prim performs a `box_push`. NOT emittable: it mutates
///                  Rust-side state. A non-zero count here is a hard blocker for a
///                  branch-free copy and forces a per-capture bail-to-miss.
pub static CAP_ARM: [AtomicU64; 3] =
    [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)]; // leaf / self_reg / neither
pub static CAP_CELL: [AtomicU64; 6] = [
    AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
    AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
]; // prim / null / ref / array / arenaptr / boxidx

#[inline]
pub fn note_cap_arm(leaf: bool, self_reg: bool) {
    let i = if leaf { 0 } else if self_reg { 1 } else { 2 };
    CAP_ARM[i].fetch_add(1, Relaxed);
}
#[inline]
pub fn note_cap_cell(class: usize) {
    CAP_CELL[class.min(5)].fetch_add(1, Relaxed);
}

/// reason: 0=not-compiled, 1=native-served, 2=recv-shape, 3=other.
#[inline]
pub fn refuse(reason: u8) {
    match reason {
        0 => &REF_NOT_COMPILED,
        1 => &REF_NATIVE_SERVED,
        2 => &REF_RECV_SHAPE,
        _ => &REF_OTHER,
    }
    .fetch_add(1, Relaxed);
}

pub fn dump() {
    #[cfg(feature = "next-gen-gc")]
    crate::jit::dispatch_diag::dump();
    if !enabled() {
        return;
    }
    let hit = PIC_HIT.load(Relaxed);
    let miss = PIC_MISS.load(Relaxed);
    let inv: [u64; 4] = std::array::from_fn(|i| PLAIN_INVOKE[i].load(Relaxed));
    let inv_total: u64 = inv.iter().sum();
    let rust_host = miss + inv_total;
    let total = hit + rust_host;
    let pct = |x: u64| if total > 0 { format!("{}.{}%", x * 1000 / total / 10, x * 1000 / total % 10) } else { "0%".into() };
    eprintln!("[PIC-COV] dynamic calls out of compiled code = {total}");
    eprintln!("[PIC-COV]   machine-code PIC hit (no Rust)       {hit:>12} ({})  <- real PIC coverage", pct(hit));
    eprintln!("[PIC-COV]   fell to Rust host                    {rust_host:>12} ({})", pct(rust_host));
    eprintln!("[PIC-COV]       PIC miss -> rava_dispatch_miss   {miss:>12} ({})", pct(miss));
    let lam = LAMBDA_MISS.load(Relaxed);
    let lamd = LAMBDA_DIRECTABLE_MISS.load(Relaxed);
    eprintln!("[PIC-COV]         lambda-receiver misses {lam:>12} ({})  directable {lamd} ", pct(lam));
    let (fd, fl) = (LPIC_FILL_DEEP.load(Relaxed), LPIC_FILL_LEAF.load(Relaxed));
    let lr: [u64; 3] = std::array::from_fn(|i| LPIC_REFUSE[i].load(Relaxed));
    eprintln!(
        "[PIC-COV]         λ-PIC fills: deep={fd} leaf={fl}  refused: shape={} rooting={} frame={}",
        lr[0], lr[1], lr[2]
    );
    eprintln!(
        "[PIC-COV]         box root-set: collections={} max={} values-cloned={}",
        BOX_SCANS.load(Relaxed), BOX_MAX.load(Relaxed), BOX_TOTAL.load(Relaxed)
    );
    let ca: [u64; 3] = std::array::from_fn(|i| CAP_ARM[i].load(Relaxed));
    let cc: [u64; 6] = std::array::from_fn(|i| CAP_CELL[i].load(Relaxed));
    if ca.iter().chain(cc.iter()).any(|&x| x > 0) {
        eprintln!(
            "[PIC-COV]         capturing arena-λ calls: leaf={} self_reg={} NEITHER={}",
            ca[0], ca[1], ca[2]
        );
        eprintln!(
            "[PIC-COV]         capture cells: prim={} null={} ref={} array={} arenaptr={} BOXIDX={} (BOXIDX is not emittable)",
            cc[0], cc[1], cc[2], cc[3], cc[4], cc[5]
        );
    }
    eprintln!("[PIC-COV]       plain invoke -> rava_invoke      {inv_total:>12} ({})", pct(inv_total));
    eprintln!("[PIC-COV]         static   {:>12}  virtual {:>12}", inv[0], inv[1]);
    eprintln!("[PIC-COV]         special  {:>12}  iface   {:>12}", inv[2], inv[3]);
    let sp = SITE_PIC.load(Relaxed);
    let si: [u64; 4] = std::array::from_fn(|i| SITE_INVOKE[i].load(Relaxed));
    let si_total: u64 = si.iter().sum();
    let sites = sp + si_total;
    let spct = |x: u64| if sites > 0 { x * 100 / sites } else { 0 };
    eprintln!("[PIC-COV] compiled call SITES = {sites}: PIC-emitted={sp} ({}%), invoke-emitted={si_total} ({}%)", spct(sp), spct(si_total));
    eprintln!("[PIC-COV]   invoke sites by kind: static={} virtual={} special={} iface={}  (static+special = structurally non-PIC-able)", si[0], si[1], si[2], si[3]);
    let (nc, ns, rs, ot) = (
        REF_NOT_COMPILED.load(Relaxed), REF_NATIVE_SERVED.load(Relaxed),
        REF_RECV_SHAPE.load(Relaxed), REF_OTHER.load(Relaxed),
    );
    let rtot = nc + ns + rs + ot;
    let rpct = |x: u64| if rtot > 0 { x * 100 / rtot } else { 0 };
    eprintln!("[PIC-COV] synthetic-frame fall-through reasons (per jit_direct_resolve attempt) = {rtot}:");
    eprintln!("[PIC-COV]   callee not compiled yet (tier lag)   {nc:>12} ({}%)", rpct(nc));
    eprintln!("[PIC-COV]   native/java-served (no fptr ever)     {ns:>12} ({}%)", rpct(ns));
    eprintln!("[PIC-COV]   receiver shape (null/boxed/array)     {rs:>12} ({}%)", rpct(rs));
    eprintln!("[PIC-COV]   other (lambda-mismatch/logger/arity)  {ot:>12} ({}%)", rpct(ot));
    eprintln!("[PIC-COV]   NOTE: ref-return is NOT a refusal reason (box_ref handles it) — hypothesis (b) refuted.");
}
