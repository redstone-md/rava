//! WHAT THE TWO BAKED RANGE TESTS IN FRONT OF EVERY COMPILED CALL ACTUALLY DISCRIMINATE
//! (`[CALLSEQ]`, default OFF, zero instructions emitted unless a census gate is armed).
//!
//! ## The sequence this counts
//!
//! A CLIF dump of `BlockBehaviour$BlockStateBase.initCache` (21.1% of all machine-code time in a
//! boot, ~28k executions) shows this ten-instruction preamble at every dispatch site, eighteen
//! times in that one method:
//!
//! ```text
//!   block55(v142):  v153 = icmp_imm ne v142, 0          ; PIC probe result, 0 = no way hit
//!                   brif v153, block45, block46         ; -> hit / -> miss
//!   block45:        v154 = iconst.i64 0x0093_889b_0000  ; A BAKED BASE ADDRESS
//!                   v155 = isub.i64 v142, v154
//!                   v156 = icmp_imm ult v155, 0x0031_8000   ; RANGE TEST A (3.2 MB span)
//!                   brif v156, block56, block57
//!   block56:        v157 = stack_addr.i64 ss2
//!                   v158 = icmp_imm.i64 ult v155, 0x0004_8000  ; RANGE TEST B (288 KB span)
//!                   brif v158, block58, block60
//! ```
//!
//! ## Where it comes from, read end to end
//!
//! Both tests are emitted by `jit::emit::dispatch_pic::emit_dispatch_pic`, and both classify the
//! word the 4-way PIC probe just returned. There is exactly ONE emitter of each shape in the
//! tree; `emit_direct_mono` (`jit/emit/calls.rs:376-435`) emits no range test at all.
//!
//! * **RANGE TEST A — `desc - pool_base() <u pool_span()`** — "is the cached word a
//!   `pic_native::NativeEntry` (a host-thunk binding) or a compiled guest function pointer?"
//!   Emitted at `jit/emit/dispatch_pic.rs:443-446` for the ARENA receiver arm, and again,
//!   identically, at `jit/emit/dispatch_pic.rs:624-627` for the heap-Ref (`refpic`) arm. The
//!   membership test is a RANGE and not a tag bit on purpose: `pic_native`'s module doc
//!   (`jit/callbacks/dispatch.rs:568-580`) records that bit 2 of a compiled fptr is exactly the
//!   first bit this tree's 8-byte code alignment does NOT claim, whereas every descriptor is
//!   carved from one leaked fixed pool, so one `isub` plus one unsigned compare identifies it and
//!   cannot alias a code address (a Rust heap allocation and the JIT's executable mapping are
//!   disjoint).
//!
//! * **NATIVE ARM, SPLIT AT `leaf_span()` (default) OR UNIFIED (kill switch)** — after
//!   range test A identifies a descriptor, `jit::emit::pic_helpers::emit_pic_native_arms`
//!   branches on test B when `pic_native::leaf_fastcall_enabled()`: a LEAF descriptor is
//!   called UNPILLED (no `spill_for_gc`, no reloads — a pure read cannot collect, so the
//!   caller's live references cannot move under the call), a ROOTED descriptor spills,
//!   invokes its thunk, and reloads forwarded references verbatim. `RAVA_NO_LEAF_FASTCALL=1`
//!   (or `RAVA_NO_PIC_NATIVE=1` / `RAVA_NO_EMIT_MORE=1` via the conjuncts) restores the
//!   ONE rooted arm for both classes, byte-identical.
//!
//! The regions and constants remain derived rather than guessed:
//! `NativeEntry` is 8 `AtomicI64` + 2 `AtomicU32` = **72 bytes**;
//! `leaf_span = NAT_LEAF_N * 72 = 4096 * 72 = 294,912 = 0x48000`, the diagnostic leaf boundary;
//! `pool_span = (NAT_LEAF_N + nat_root_n()) * 72 = (4096 + 2*20480) * 72 = 3,244,032 = 0x318000`
//! — test A's immediate, with the `2 *` present because `pic_native::ref_enabled()` is default ON
//! (`jit/callbacks/dispatch.rs:704-724`).
//!
//! ## What is decidable AT EMIT TIME, and what is not
//!
//! **Test A is not decidable at emit time.** Its answer is true only once
//! `pic_native::try_fill` has written a descriptor into one of the site's four ways, and that fill
//! requires the site's cache to ALREADY hold a `(class_id, -1)` permanent refusal — a run-time
//! verdict about a receiver class this site has not yet seen. The one clause of `try_fill` that IS
//! an emit-time fact is its kind filter (`kind != 1 && kind != 3 => return`,
//! `jit/callbacks/dispatch.rs:1096-1098`), and `KIND_DEAD` below counts how often that clause
//! would reject — reporting the zero is the point, because it proves the kind gate is not a lever.
//!
//! **Test B IS decidable at emit time, at every site, from the constant pool alone.** The LEAF
//! half can only ever be entered at a site whose descriptor was allocated with `leaf.is_some()`,
//! and the `leaf` in `try_fill` is `leaf_op(class_name, m.name, m.descriptor)` where `m` is
//! `member_ref(cp)` — the SITE's own constant-pool entry. `leaf_op` /`leaf_op_ref`
//! (`jit/callbacks/dispatch.rs:838-899`) are closed `match`es. The receiver class is unknown at
//! emit time, but `(name, descriptor)` is known exactly, so:
//!
//! > if the site's `(name, descriptor)` matches NO arm of `leaf_op`, then `leaf_op` returns `None`
//! > for EVERY class, so no leaf descriptor can ever be allocated for this site, so `t < leaf_span`
//! > is false at every execution — test B, its block, its thunk call and its refusal branch are
//! > dead code at that site.
//!
//! Three checks make that a proof rather than a hope, all of them read rather than assumed:
//! 1. a descriptor's half is fixed at allocation (`LEAF_NEXT` vs `NAT_LEAF_N + ROOT_NEXT`,
//!    `jit/callbacks/dispatch.rs:1146-1163`) and is never moved;
//! 2. the only transition between halves is `note_refusal`, which DEMOTES leaf to rooted
//!    (`jit/callbacks/dispatch.rs:916-970`) and never promotes;
//! 3. a descriptor installed in site S's ways is one allocated FOR site S — `try_fill` writes into
//!    the `cache_ptr` it was handed and `seen()` is keyed by `(cache_ptr, key)` — so a leaf
//!    descriptor minted for some other site's `(name, desc)` cannot appear here.
//!
//! A fourth is what keeps the ARENA and heap-Ref projections separate even though the two arms
//! SHARE one 4-way cache: the arena probe compares against a small dense `class_id` and every
//! heap-Ref key is a large aligned class word, so the arena probe can never return a heap-Ref
//! arm's descriptor. Hence the arena arm is measured against `leaf_op` and the refpic arm against
//! `leaf_op_ref`, and both projections are reported.
//!
//! ⚠**THE PROJECTIONS BELOW ARE A COPY, AND THAT IS A REPORTING RISK ONLY.** `leaf_op` and
//! `leaf_op_ref` are private `fn`s of `pic_native` and this round may not reshape that module, so
//! `LEAF_NAMES` / `LEAF_NAMES_REF` mirror them by hand. If someone adds an arm there and not here,
//! this census UNDERCOUNTS `leaf-possible` — it emits no codegen decision of any kind, so a drift
//! produces a wrong number, never a wrong program. Keep them in step.
//!
//! ## Why the run-time counters are shaped the way they are
//!
//! The three counts are taken BRANCHLESSLY in the dominating block, before test A's `brif`:
//! `hits += 1`, `native += (is_nat as u64)`, `leaf += (is_leaf as u64)`, each a
//! load/add/store against a baked address. Modelled on `dispatch_table_probe.rs:145-155`, which
//! takes exactly this shape for exactly this reason — a branch would perturb the thing being
//! measured — and on `picprobe::pic_hit_addr`, which has baked a counter address into the PIC hit
//! arm since 23.2.
//!
//! `leaf` needs no `and` with `is_nat`: `leaf_span < pool_span` on the same `t`, so `is_leaf`
//! IMPLIES `is_nat` arithmetically and the count is exact without the extra instruction.
//!
//! ⚠**THIS TREE PAID FOR THE OBVIOUS MISTAKE ONCE**: an unconditional atomic RMW on one global
//! line, sitting on the per-call path, took a counter from 5,750 to 20,000,000 calls a boot. So
//! nothing here is atomic and nothing here is unconditional: the counters are plain 64-bit cells
//! written by three emitted instructions each, and the emitter asks `enabled()` ONCE per site (a
//! `OnceLock` load, never a `var_os`) — with the gate off, not one instruction is emitted and the
//! codegen is byte-identical to the build before this file existed.
//!
//! The plain (non-atomic) RMW is the same accuracy trade `picprobe` documents: workers execute
//! compiled code too, so concurrent updates can be lost and the absolute totals are a LOWER BOUND.
//! The RATIO between the three is what this census is read for, and all three are perturbed the
//! same way by the same threads.
//!
//! ## Reporting
//!
//! From the periodic sampler dump, beside `compile_bail` — NOT from a teardown. This tree has now
//! found five censuses reporting only at an exit path a server boot never reaches, because the
//! harness kills the process at `Done (`.

/// Brings `FunctionBuilder`, `Value`, `InstBuilder`, `IntCC`, `MemFlags`, `I64` and `Offset32`
/// into scope the way every other emit file in this module does. Gated with the emitter itself so
/// a build without the feature imports nothing it does not use.
#[cfg(feature = "next-gen-gc")]
use crate::jit::*;

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Armed by the existing census gate (`RAVA_JIT_PIC_PROBE`, i.e. `picprobe::enabled()`) or by
/// `RAVA_JIT_CALLSEQ` on its own, so this can ride an existing probe run or be taken alone.
///
/// Forced OFF by `RAVA_NO_CALLSEQ=1` and by `crate::flags::emit_more_disabled()`
/// (`RAVA_NO_EMIT_MORE=1`), either of which restores the historic emission EXACTLY: `note_site`
/// returns before it touches the builder, so no value is created, no instruction is numbered
/// differently, and nothing a bit-exactness gate can see changes.
///
/// Read ONCE per process. `emit_dispatch_pic` runs ~9,500 times a boot on the serial critical
/// path and `env::var_os` is a linear environment-block walk on Windows.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        if std::env::var_os("RAVA_NO_CALLSEQ").is_some() || crate::flags::emit_more_disabled() {
            return false;
        }
        std::env::var_os("RAVA_JIT_CALLSEQ").is_some() || crate::picprobe::enabled()
    })
}

// ---------------------------------------------------------------------------------------------
// EMIT-TIME tallies. One `Relaxed` add per compiled site (~9,500 a boot), only when armed.
// ---------------------------------------------------------------------------------------------

/// Sites that emitted range test A on the ARENA arm.
static SITES: AtomicU64 = AtomicU64::new(0);
/// ...of those, sites that emit a SECOND, identical range test on the heap-Ref arm — i.e. sites
/// that pay the preamble twice. `heap_mode == 2 && pic_native::ref_enabled()`.
static SITES_REF_TOO: AtomicU64 = AtomicU64::new(0);
/// Sites where test B is DECIDABLE and its answer is "never LEAF": the site's `(name, descriptor)`
/// matches no arm of `pic_native::leaf_op`, so no class can produce a leaf binding here.
static LEAF_DEAD: AtomicU64 = AtomicU64::new(0);
/// Sites where test B is undecidable at emit time: `(name, descriptor)` is in the projection, so
/// whether a leaf descriptor is ever minted depends on the receiver class (and on
/// `pure_memo::armed()` at fill time, which is a run-time state and not evidence available here).
static LEAF_LIVE: AtomicU64 = AtomicU64::new(0);
/// Same two verdicts against `leaf_op_ref`, for the heap-Ref arm's copy of the sequence.
static LEAF_DEAD_REF: AtomicU64 = AtomicU64::new(0);
static LEAF_LIVE_REF: AtomicU64 = AtomicU64::new(0);
/// Sites whose `member_ref(cp)` could not be read (null class pointer, or a cp entry that is not a
/// Field/Method/InterfaceMethod ref). Counted separately and never folded into "decidable":
/// unknown is not the same verdict as proven-dead, and a census that hides the difference is how a
/// wrong lever gets picked.
static UNKNOWN: AtomicU64 = AtomicU64::new(0);
/// Sites `try_fill`'s `kind != 1 && kind != 3` clause would reject, making test A provably dead.
static KIND_DEAD: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------------------------
// RUN-TIME counters, incremented by three emitted instructions each. Plain cells: the emitted
// load/add/store is not an atomic RMW (see the module doc).
// ---------------------------------------------------------------------------------------------

/// Executions of range test A on the arena arm — i.e. machine-code PIC hits at a site that emits
/// the native preamble. The denominator for both arm splits.
pub(crate) static RT_HITS: AtomicU64 = AtomicU64::new(0);
/// ...of those, executions where test A said "pic_native descriptor" (the host-thunk arm won).
pub(crate) static RT_NATIVE: AtomicU64 = AtomicU64::new(0);
/// ...of those, executions where test B said "LEAF half". `RT_NATIVE - RT_LEAF` is the rooted arm.
pub(crate) static RT_LEAF: AtomicU64 = AtomicU64::new(0);

#[cfg(feature = "next-gen-gc")]
#[inline]
fn addr(c: &'static AtomicU64) -> i64 {
    c as *const AtomicU64 as i64
}

/// `(name, descriptor)` projection of `pic_native::leaf_op` — the ARENA arm's leaf list.
/// MIRRORS `jit/callbacks/dispatch.rs:889-898`; see the module doc's drift warning.
#[cfg(feature = "next-gen-gc")]
const LEAF_NAMES: [(&str, &str); 5] = [
    ("get", "(I)Ljava/lang/Object;"),          // java/util/ArrayList
    ("get", "()J"),                            // java/util/concurrent/atomic/AtomicLong
    ("isPresent", "()Z"),                      // java/util/Optional
    ("isEmpty", "()Z"),                        // java/util/Optional
    ("get", "()Ljava/lang/Object;"),           // java/util/Optional
];

/// `(name, descriptor)` projection of `pic_native::leaf_op_ref` — the heap-Ref arm's leaf list,
/// which is `leaf_op` plus the two `ThreadLocal` shapes only a handle receiver can satisfy.
/// MIRRORS `jit/callbacks/dispatch.rs:921-932`.
#[cfg(feature = "next-gen-gc")]
const LEAF_NAMES_REF: [(&str, &str); 2] = [
    ("get", "()Ljava/lang/Object;"),           // ThreadLocal.get (also in LEAF_NAMES)
    ("set", "(Ljava/lang/Object;)V"),          // ThreadLocal.set
];

#[cfg(feature = "next-gen-gc")]
#[inline]
fn in_projection(list: &[(&str, &str)], name: &str, desc: &str) -> bool {
    list.iter().any(|&(n, d)| n == name && d == desc)
}

/// THE ONE ADDED CALL. Placed in `emit_dispatch_pic`'s arena arm between the `icmp_imm` that
/// computes test A and the `brif` that consumes it, so `b`'s current block is the PIC `hit` block
/// and the emitted counters dominate BOTH outcomes.
///
/// `t` is `fptr - pool_base()` (already computed for test A) and `is_nat` is test A's own result;
/// neither is recomputed, and nothing here observes a value the site did not already have.
///
/// The emit-time half classifies the site from `(class_ptr, cp)` — the SAME pair `try_fill`
/// resolves against at run time (`jit/callbacks/dispatch.rs:1120-1126`) and the same pair
/// `emit_inline` rebinds for an inlined callee body (`jit/emit/inline.rs:532`), so a site inlined
/// from another class is classified against ITS pool and not the top method's.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn note_site(
    b: &mut FunctionBuilder,
    class_ptr: i64,
    cp: u16,
    kind: u8,
    heap_mode: u8,
    t: Value,
    is_nat: Value,
) {
    if !enabled() {
        return;
    }

    // ---- emit-time classification -----------------------------------------------------------
    SITES.fetch_add(1, Relaxed);
    let ref_too = heap_mode == 2 && crate::jit::pic_native::ref_enabled();
    if ref_too {
        SITES_REF_TOO.fetch_add(1, Relaxed);
    }
    // 1 = invokevirtual, 3 = invokeinterface — the only kinds `try_fill` will bind.
    if kind != 1 && kind != 3 {
        KIND_DEAD.fetch_add(1, Relaxed);
    }
    // SAFETY: `class_ptr` is `Arc::as_ptr(&caller_class)` for the class being compiled (or, inside
    // an inlined body, for the callee's class); it outlives the compile. The null guard mirrors
    // `runtime::finish::match_class`, which documents that a bailed/synthetic compile can carry a
    // null class pointer.
    // The pair is taken from `member_ref(cp)` and NOT from the `desc: &str` the emitter already
    // holds, so this reads exactly what `try_fill` reads — one source, no second copy to drift.
    let member: Option<(String, String)> = if class_ptr == 0 {
        None
    } else {
        let cls = unsafe { &*(class_ptr as *const crate::class_loader::LoadedClass) };
        cls.file
            .pool
            .member_ref(cp)
            .map(|m| (m.name.to_string(), m.descriptor.to_string()))
    };
    match member {
        None => {
            UNKNOWN.fetch_add(1, Relaxed);
        }
        Some((name, desc)) => {
            if in_projection(&LEAF_NAMES, &name, &desc) {
                LEAF_LIVE.fetch_add(1, Relaxed);
            } else {
                LEAF_DEAD.fetch_add(1, Relaxed);
            }
            if ref_too {
                let live = in_projection(&LEAF_NAMES, &name, &desc)
                    || in_projection(&LEAF_NAMES_REF, &name, &desc);
                if live {
                    LEAF_LIVE_REF.fetch_add(1, Relaxed);
                } else {
                    LEAF_DEAD_REF.fetch_add(1, Relaxed);
                }
            }
        }
    }

    // ---- run-time counters, branchless, in the dominating block -------------------------------
    // hits += 1
    let ha = b.ins().iconst(I64, addr(&RT_HITS));
    let hcur = b.ins().load(I64, MemFlags::trusted(), ha, Offset32::new(0));
    let hnew = b.ins().iadd_imm(hcur, 1);
    b.ins().store(MemFlags::trusted(), hnew, ha, Offset32::new(0));

    // native += (test A result)
    let nat64 = b.ins().uextend(I64, is_nat);
    let na = b.ins().iconst(I64, addr(&RT_NATIVE));
    let ncur = b.ins().load(I64, MemFlags::trusted(), na, Offset32::new(0));
    let nnew = b.ins().iadd(ncur, nat64);
    b.ins().store(MemFlags::trusted(), nnew, na, Offset32::new(0));

    // leaf += (test B result). Recomputed here rather than read from the arm, because the arm's
    // own compare lives inside `nat_blk` and would only ever see the native side. `leaf_span <
    // pool_span` on the same `t`, so this implies test A and needs no `and` with `is_nat`.
    let is_leaf =
        b.ins().icmp_imm(IntCC::UnsignedLessThan, t, crate::jit::pic_native::leaf_span() as i64);
    let leaf64 = b.ins().uextend(I64, is_leaf);
    let la = b.ins().iconst(I64, addr(&RT_LEAF));
    let lcur = b.ins().load(I64, MemFlags::trusted(), la, Offset32::new(0));
    let lnew = b.ins().iadd(lcur, leaf64);
    b.ins().store(MemFlags::trusted(), lnew, la, Offset32::new(0));
}

/// The `[CALLSEQ]` block. Empty (and prints nothing) when no site was ever counted, so an
/// unarmed run adds no output.
pub fn report() -> String {
    let sites = SITES.load(Relaxed);
    if sites == 0 {
        return String::new();
    }
    let pct = |x: u64, of: u64| if of > 0 { x as f64 * 100.0 / of as f64 } else { 0.0 };

    let ref_too = SITES_REF_TOO.load(Relaxed);
    let mut out = format!(
        "[CALLSEQ] {sites} compiled PIC sites emit the pic_native pool range test on the arena arm; \
         {ref_too} of them ({:.1}%) emit a SECOND identical test on the heap-Ref arm",
        pct(ref_too, sites)
    );

    out.push_str(&pool_line());

    out.push_str(
        "\n[CALLSEQ]   TEST A `fptr - pool_base <u pool_span`: cached word is a pic_native NativeEntry (host thunk) vs a COMPILED GUEST fptr",
    );
    out.push_str(
        "\n[CALLSEQ]   TEST B `t <u leaf_span` (same subtraction): LEAF descriptor (pure read, caller's refs unspilled) vs ROOTED (spill -> call -> reload)",
    );

    // --- emit-time decidability ---
    let (ld, ll, unk, kd) = (
        LEAF_DEAD.load(Relaxed),
        LEAF_LIVE.load(Relaxed),
        UNKNOWN.load(Relaxed),
        KIND_DEAD.load(Relaxed),
    );
    out.push_str(&format!(
        "\n[CALLSEQ]   EMIT-TIME, test A: 0 sites decidable by construction (a fill needs a run-time `-1` verdict); {kd} ({:.1}%) would be rejected by try_fill's kind clause",
        pct(kd, sites)
    ));
    out.push_str(&format!(
        "\n[CALLSEQ]   EMIT-TIME, test B (arena, vs leaf_op):     PROVABLY NEVER LEAF {ld} ({:.1}%) | undecidable {ll} ({:.1}%) | member_ref unreadable {unk} ({:.1}%)",
        pct(ld, sites),
        pct(ll, sites),
        pct(unk, sites)
    ));
    if ref_too > 0 {
        let (ldr, llr) = (LEAF_DEAD_REF.load(Relaxed), LEAF_LIVE_REF.load(Relaxed));
        out.push_str(&format!(
            "\n[CALLSEQ]   EMIT-TIME, test B (heap-Ref, vs leaf_op_ref): PROVABLY NEVER LEAF {ldr} ({:.1}%) | undecidable {llr} ({:.1}%)",
            pct(ldr, ref_too),
            pct(llr, ref_too)
        ));
    }

    // --- run-time arm counts ---
    let hits = RT_HITS.load(Relaxed);
    let nat = RT_NATIVE.load(Relaxed).min(hits);
    let leaf = RT_LEAF.load(Relaxed).min(nat);
    if hits == 0 {
        out.push_str(
            "\n[CALLSEQ]   RUN-TIME: 0 executions recorded — the arm counters are emitted only under this gate, so this run never executed a counted site",
        );
        return out;
    }
    out.push_str(&format!(
        "\n[CALLSEQ]   RUN-TIME (arena arm, plain non-atomic cells => a LOWER BOUND under worker threads; read the ratio): {hits} executions reached test A"
    ));
    out.push_str(&format!(
        "\n[CALLSEQ]     test A -> GUEST fptr  {:>14} ({:.3}%)   <- the arm that pays for the test",
        hits - nat,
        pct(hits - nat, hits)
    ));
    out.push_str(&format!(
        "\n[CALLSEQ]     test A -> pic_native  {:>14} ({:.3}%)   of which  LEAF {leaf} ({:.3}% of all hits)  ROOTED {} ({:.3}%)",
        nat,
        pct(nat, hits),
        pct(leaf, hits),
        nat - leaf,
        pct(nat - leaf, hits)
    ));
    out.push_str(&format!(
        "\n[CALLSEQ]     COST: 4 IR instructions (iconst/isub/icmp/brif) on 100% of hits to route {:.3}% of them; test B costs 1 more to route {:.3}%",
        pct(nat, hits),
        pct(leaf, hits)
    ));

    // --- pic_native's own fill/serve counters, if its stats gate happened to be armed. Read only,
    // no new instrumentation: they say whether a low native share is "nothing bound" or "bound and
    // refusing", which the three counts above cannot separate on their own.
    out.push_str(&fill_line());
    out
}

/// The pool geometry both immediates are baked from. A non-zero `SITES` proves
/// `pic_native::enabled()` was true and the pool is already allocated and already baked into
/// machine code, so reading it here allocates nothing new.
#[cfg(feature = "next-gen-gc")]
fn pool_line() -> String {
    let base = crate::jit::pic_native::pool_base();
    let span = crate::jit::pic_native::pool_span();
    let leaf = crate::jit::pic_native::leaf_span();
    let esz = crate::jit::pic_native::entry_size().max(1);
    format!(
        "\n[CALLSEQ]   pool base {base:#x} span {span} (0x{span:x}, {} entries x {esz} B), leaf half {leaf} (0x{leaf:x}, {} entries)",
        span / esz,
        leaf / esz
    )
}

#[cfg(not(feature = "next-gen-gc"))]
fn pool_line() -> String {
    String::new()
}

/// `pic_native`'s OWN fill/serve counters, READ and never written, so a low native share can be
/// told apart from "bound, and refusing". They sit behind `freeze_resolve::stats_on()` — a
/// different gate from this census — so the line appears only when something in it is non-zero.
#[cfg(feature = "next-gen-gc")]
fn fill_line() -> String {
    use crate::jit::pic_native as pn;
    let g = |c: &std::sync::atomic::AtomicI64| c.load(Relaxed);
    let v = [
        g(&pn::NAT_FILLED),
        g(&pn::NAT_REF_FILLED),
        g(&pn::NAT_LEAF_SERVE),
        g(&pn::NAT_LEAF_REFUSE),
        g(&pn::NAT_ROOT_SERVE),
        g(&pn::NAT_RETIRED),
        g(&pn::NAT_REF_NOWAY),
        g(&pn::NAT_LEAF_FULL),
        g(&pn::NAT_ROOT_FULL),
    ];
    if v.iter().all(|&x| x == 0) {
        return String::new();
    }
    format!(
        "\n[CALLSEQ]   pic_native fills (its own stats gate): arena {} ref {} | leaf serve {} refuse {} | rooted serve {} | retired {} no-way {} | pool full: leaf {} root {}",
        v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7], v[8]
    )
}

#[cfg(not(feature = "next-gen-gc"))]
fn fill_line() -> String {
    String::new()
}
