//! The capture-unpacking thunk for a capturing arena lambda.
//!
//! WHY THIS EXISTS. A capturing lambda cannot be served from the machine-code PIC today, and the
//! refusal is stated in `dispatch_table::note_serve_direct` as `entry.cap_n != 0`: the emitted hit
//! arm has no way to place the closure's captures in front of the SAM arguments, so every such
//! call leaves compiled code for `jit_direct_invoke` to do it in Rust. On `bench/ShapeOr.java`
//! that is 7.67 M of ~20 M host excursions.
//!
//! The thunk removes the REASON for the refusal rather than the refusal: one small compiled body
//! per (impl, capture shape) whose signature is the one the JIT already uses for a method —
//! `(locals_ptr, ctx) -> i64` — so a PIC entry can point at it with no new ABI. It reads the
//! captures out of the receiver's arena cells, lays `[captures…, args…]` into a stack slot and
//! calls the impl.
//!
//! ROOTING IS THE CALLEE'S. The impl is `self_reg`, so its Deep-IC v3 prologue pushes
//! `(buffer, frame_len)` onto this thread's cell-root stack — the buffer being exactly the stack
//! slot handed over here. A collection inside the impl therefore sees the captures as roots, and
//! the thunk emits no rooting of its own.
//!
//! PER-SLOT SHAPES, verified against `decode_arena_cell` AND `jit::abi::box_ref` — they have to
//! agree, and they do:
//!
//! * primitive slot (`cap_ref_mask` bit clear) — identity copy;
//! * reference slot — `0 -> 0`, else on `raw & 3`: `1 -> (raw>>2)|REF_TAG`,
//!   `2 -> (raw>>2)|ARRAY_TAG`, `0 -> raw` (an arena pointer, identity),
//!   `3 -> rava_cap_cell_slow` (an `arena_box` index; `[PC-CAP]` measures that kind at **zero** on
//!   both a full boot and the probe, so the arm exists for correctness and never runs).
//!
//! ⚠**THE `raw & 3 == 0` IDENTITY HOLDS ONLY WHILE PINNED MIRRORS ARE OFF.** `decode_arena_cell`
//! probes `pinned_handle_of` on that arm and `box_ref` probes `pinned_mirror_get` under
//! `pinned_ptrs_enabled` — the same precondition from both sides. The builder refuses outright
//! while pinned pointers are on rather than bake an identity that would silently become wrong.

use crate::jit::*;

/// The un-emittable capture encoding, handled out of line. Never reached on any measured
/// workload; present so the thunk is correct rather than merely fast.
pub(crate) extern "C" fn rava_cap_cell_slow(raw: i64) -> i64 {
    #[cfg(feature = "next-gen-gc")]
    {
        let v = crate::nextgen::arena_box_get((raw >> 2) as usize);
        match v {
            crate::Value::Ref(h) => (h as i64) | crate::value::REF_TAG,
            crate::Value::Array(h) => (h as i64) | crate::value::ARRAY_TAG,
            crate::Value::Null => 0,
            other => {
                let idx = crate::frames::box_push(other);
                (idx as i64) | crate::value::BOX_TAG
            }
        }
    }
    #[cfg(not(feature = "next-gen-gc"))]
    {
        let _ = raw;
        0
    }
}

// ⛔**AND THE WIRING IS NOT A BREADCRUMB — MEASURED.** Two hooks were built and both failed, and
// the counters name why, so neither is re-derived:
//
// 1. **In `dispatch_miss_resolve`** — `offered=13 refused=5 installed=0` against 7.67 M
//    capturing-lambda calls. The negative cache runs that resolve ONCE per (site, class): the
//    first miss writes `-1` and it never re-runs, and that single shot lands before the lambda's
//    impl is compiled, so `DirectEntry::fptr` is not yet a pointer.
// 2. **A take-once thread-local crumb** from `jit_direct_call`'s `DirectProbe::Hit` arm (which
//    holds a compiled `fptr`) to `rava_dispatch_miss` (which holds `cache_ptr`) — **6,176,184
//    crumbs placed, 908 taken, 0 installed.** The producer fires overwhelmingly from nested calls
//    that never reach that consumer, and the `(caller, cp)` check correctly rejected every one
//    that did: publishing them would be exactly the `(outer site, inner receiver) -> inner fptr`
//    error `take_dispatch_site`'s discipline exists to prevent.
//
// ⇒ The two facts the install needs — the site's `cache_ptr` and a resolved entry whose `fptr` is
// already compiled — must meet in ONE frame. Carrying the entry UP does not work; the remaining
// route is threading `cache_ptr` DOWN into `jit_direct_call`, which is a signature change through
// `jit_call` and is the next thing to try.

/// The site whose 4-way PIC is currently being missed: `(cache_ptr, caller_ptr, cp)`.
///
/// ⭐**PUBLISHED DOWNWARD, VERIFIED AT THE FAR END** — which is the fix for the crumb that
/// failed. Carrying a resolved entry UP from `jit_direct_call` to `rava_dispatch_miss` measured
/// 6,176,184 placed / 908 taken / 0 installed, because the producer fires from nested calls that
/// never reach that consumer. Publishing the SITE downward instead has the same nesting hazard —
/// an inner call would see the outer site — but here the reader is `jit_direct_call`, which knows
/// its OWN `(caller_ptr, cp)` and can simply refuse anything that does not match. So the check
/// happens where the entry is live, not where it is stale.
#[cfg(feature = "next-gen-gc")]
pub(crate) mod site {
    thread_local! {
        static CUR: std::cell::Cell<(i64, usize, u16)> = const { std::cell::Cell::new((0, 0, 0)) };
    }

    /// Restores the previous value on drop: `jit_call` re-enters through nested dispatches, and a
    /// bare set/clear would leave an inner miss's site published to its caller.
    pub(crate) struct Scope((i64, usize, u16));

    impl Drop for Scope {
        fn drop(&mut self) {
            CUR.with(|c| c.set(self.0));
        }
    }

    pub(crate) fn publish(cache_ptr: i64, caller: usize, cp: u16) -> Scope {
        let prev = CUR.with(|c| c.replace((cache_ptr, caller, cp)));
        Scope(prev)
    }

    /// The published cache pointer, but only for the site the caller names.
    pub(crate) fn cache_for(caller: usize, cp: u16) -> Option<i64> {
        let (cache, c, p) = CUR.with(|c| c.get());
        (cache != 0 && c == caller && p == cp).then_some(cache)
    }
}

/// How many capturing-lambda entries reached the install and how many took a PIC slot.
pub static INSTALLED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static SEEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Which refusal excluded a capturing-lambda entry: 0 = the void-AND-wide corner, 1 = arg0 class
/// guard, 2 = reference return, 3 = not a `shift` entry.
///
/// ⛔**AND THE REFUSED CORNER IS NOT WORTH CHASING — CONVERT IT TO A SHARE OF THE BOOT FIRST.**
/// A refused entry never installs, so it comes back on every call: `refused voidwide=81,238` on a
/// full boot IS the number of host excursions still being paid, not a population of sites. At the
/// ~150 ns an excursion costs on this engine that is **≈12 ms of a 46.6 s boot — 0.03%.** It reads
/// as "96.7% of what is left" and it is a rounding error, which is exactly the trap the ledger's
/// conversion rule exists for.
///
/// The thunk's value is in what it DID install: those sites stopped leaving machine code
/// altogether, which is why the probe moved 10.8% and the boot ~2%.
///
/// ⭐**A FULL BOOT READS `wide=81,238` OF 83,690 — 97.1%** (`arg0=0 retref=2,300 noshift=0`), so
/// the wide-parameter refusal is the whole remaining population and every other clause is noise.
///
/// ⛔**AND WIDENING IT WAS TRIED AND BROKE THE GATES.** `wide_mask` indexes the WRITE order —
/// captures then arguments, `w += 1` and again for a wide — so the destination offsets are
/// computable at build time and the emit stays straight-line; that was implemented and ChunkFill
/// still died with `IllegalArgumentException: The min values need to be smaller or equals to the
/// max values`, the same signature as the original 1:1 copy. So the offsets are not the whole
/// story and something else about a wide capture or argument differs. Reverted rather than
/// debugged blind, because the narrow version is shipped and works.
///
/// ⭐⭐**THE CAUSE, READ RATHER THAN GUESSED (`hr_capvoid`): THE SOURCE SIDE WAS INDEXED PACKED,
/// AND THE BUFFER IS LAID OUT IN JVM SLOTS.** The destination arithmetic above was right all
/// along. What the thunk READS is wrong for a wide SAM argument: this thunk is installed straight
/// into a `emit_dispatch_pic` 4-way cache, whose hit arm hands the callee the call buffer "laid
/// out in JVM SLOTS (wide args leave their pair slot unused …)" — only `rava_dispatch_miss` gets
/// the PACKED copy, one cell per argument. The thunk took `argc` from that packed copy and read
/// argument `j` at `buf[(1+j)*8]`, packed indexing on a slot-laid buffer.
///
/// The refused population is `VoxelShape.lambda$optimize$0(VoxelShape[], DDDDDD)V` and its
/// siblings (`toAabbs$3`, `closestPointTo$5`): one array capture, six doubles, void. Packed
/// reads on `[recv, d0, pad, d1, pad, d2, pad, …]` deliver `d0, 0, d1, 0, d2, 0`, and
/// `Shapes.box(x1 = d0, y1 = 0, z1 = d1, x2 = 0, …)` throws exactly "The min values need to be
/// smaller or equals to the max values". It is deterministic, which is why the gate died every
/// time. And a SINGLE TRAILING wide argument reads correctly by coincidence (its pad is never
/// read), which is why "every wide mask among non-void callees" passed and the corner got
/// labelled VOID: the label was a proxy for "a wide argument followed by another argument".
///
/// Two more defects ride on the same misreading and are fixed together: the bail arm handed the
/// slot-laid `buf` to `rava_dispatch_miss`, whose ABI is packed; and a wide value's dead pair
/// slot in the thunk's OWN stack slot was never written, although the self-registering callee
/// registers `(buffer, frame_len)` as a cell-root range and its prologue zeroes only
/// `[args_slots, frame_len)` (the rule `emit_direct_mono` states at its own marshal loop).
pub static REFUSE: [std::sync::atomic::AtomicU64; 4] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 4];
pub static SITE_OK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Per-shape census of what `build_site_thunk` did, indexed by [`shape_of`]:
/// `0` non-void/narrow, `1` non-void/wide, `2` void/narrow, `3` void/wide. A build that
/// succeeds IS an install (`install_cap_thunk` writes the slot unconditionally on `Some`), so
/// `SHAPE_BUILT` counts installs. `SHAPE_REFUSED` counts refusals decided HERE, split by reason
/// in `BUILD_WHY`; the admission refusals stay in `REFUSE`, and `REFUSE[0]` (void-AND-wide) is
/// shape 3 by definition.
///
/// Bumped with a plain load/store pair, not an RMW: a refused shape re-enters the builder on
/// every call of its site, so this is a per-call path for exactly the population being counted.
pub static SHAPE_BUILT: [std::sync::atomic::AtomicU64; 4] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 4];
pub static SHAPE_REFUSED: [std::sync::atomic::AtomicU64; 4] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 4];
/// Why a build refused: 0 frame fit, 1 pinned mirrors on, 2 no descriptor at `(caller, cp)`,
/// 3 descriptor arity ≠ `argc`, 4 SAM/impl width disagreement, 5 malformed entry (cap/fptr/cache).
pub static BUILD_WHY: [std::sync::atomic::AtomicU64; 6] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 6];

/// The shape index: bit 1 = void callee, bit 0 = any wide slot in `wide_mask` (captures or
/// arguments — the same test the admission's "void AND wide" corner uses).
pub(crate) fn shape_of(void_ret: bool, wide_mask: u64) -> usize {
    (usize::from(void_ret) << 1) | usize::from(wide_mask != 0)
}

#[cfg(feature = "next-gen-gc")]
fn bump(c: &std::sync::atomic::AtomicU64) {
    use std::sync::atomic::Ordering::Relaxed;
    c.store(c.load(Relaxed).wrapping_add(1), Relaxed);
}

#[cfg(feature = "next-gen-gc")]
fn refuse_build(shape: usize, why: usize) {
    bump(&SHAPE_REFUSED[shape]);
    bump(&BUILD_WHY[why]);
}

/// The shape census as one line, empty while nothing has been built or refused.
pub fn report() -> String {
    use std::sync::atomic::Ordering::Relaxed;
    let built: [u64; 4] = std::array::from_fn(|i| SHAPE_BUILT[i].load(Relaxed));
    let refused: [u64; 4] = std::array::from_fn(|i| SHAPE_REFUSED[i].load(Relaxed));
    let why: [u64; 6] = std::array::from_fn(|i| BUILD_WHY[i].load(Relaxed));
    if built.iter().sum::<u64>() + refused.iter().sum::<u64>() == 0 {
        return String::new();
    }
    format!(
        "[PC-CAPSHAPE] hr_capvoid={} | installed nonvoid/narrow={} nonvoid/wide={} void/narrow={} void/wide={} | build-refused nonvoid/narrow={} nonvoid/wide={} void/narrow={} void/wide={} | why fit={} pinned={} nodesc={} arity={} width={} malformed={} | admission-refused void/wide={} (REFUSE[0])",
        if hr_capvoid_enabled() { "on" } else { "off" },
        built[0], built[1], built[2], built[3],
        refused[0], refused[1], refused[2], refused[3],
        why[0], why[1], why[2], why[3], why[4], why[5],
        REFUSE[0].load(Relaxed),
    )
}

/// Print [`report`] when there is one — hooked beside `compile_bail::report` in the periodic
/// profiler dump, because a server boot never reaches a teardown.
pub fn report_shapes() {
    let r = report();
    if !r.is_empty() {
        eprintln!("{r}");
    }
}

/// DEFAULT ON. `RAVA_NO_HR_CAPVOID=1` or `RAVA_NO_EMIT_MORE=1` restores the historic thunk
/// exactly: packed-indexed argument reads, the slot-laid `buf` handed to the miss arm, no pad
/// stores, and the void-AND-wide admission refusal in `install_cap_thunk`.
///
/// With it on, `build_site_thunk` derives the CALLER's slot layout from the call site's own
/// descriptor — the same `arg_slot_widths(desc)` the PIC emitter laid the buffer out with — and
/// refuses any site whose descriptor it cannot read, whose arity disagrees with `argc`, or whose
/// per-argument width disagrees with the impl's `wide_mask` (a LambdaMetafactory widening or
/// boxing adaptation the copy could not express). Resolved once.
pub(crate) fn hr_capvoid_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_HR_CAPVOID").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// Where each SAM argument sits in the CALLER's slot-laid buffer, in cells from its base.
///
/// `src[j]` is the cell of argument `j` (`1` for the first — cell 0 is the receiver);
/// `holed` says the buffer has at least one pad, so the bail arm must build a PACKED copy for
/// `rava_dispatch_miss` rather than hand the buffer over as is.
#[cfg(feature = "next-gen-gc")]
pub(crate) struct SiteLayout {
    pub(crate) src: Vec<i32>,
    pub(crate) holed: bool,
}

#[cfg(feature = "next-gen-gc")]
impl SiteLayout {
    /// From the caller's per-arg slot widths (`arg_slot_widths`: `[1 (receiver), w0, w1, …]`),
    /// checked against the impl's view of the same arguments. `Err(why)` indexes `BUILD_WHY`.
    ///
    /// REJECT-ONLY. Every argument the thunk copies must be a value the impl reads at the width
    /// the caller wrote it: a caller-side wide with an impl-side narrow (or the reverse) is an
    /// adaptation, not a copy, and is refused rather than truncated.
    pub(crate) fn from_widths(
        widths: &[usize],
        cap_n: u16,
        wide_mask: u64,
        argc: u16,
    ) -> Result<Self, usize> {
        if widths.len() != usize::from(argc) + 1 || widths[0] != 1 {
            return Err(3);
        }
        let wide_at = |i: usize| i < 64 && wide_mask & (1u64 << i) != 0;
        let mut src = Vec::with_capacity(usize::from(argc));
        let mut off: i32 = 1;
        let mut holed = false;
        for (j, &wd) in widths[1..].iter().enumerate() {
            let caller_wide = wd == 2;
            if caller_wide != wide_at(usize::from(cap_n) + j) {
                return Err(4);
            }
            src.push(off);
            off += wd as i32;
            holed |= caller_wide;
        }
        Ok(Self { src, holed })
    }

    /// Read the call site's descriptor and derive its layout. `caller_ptr` is the baked
    /// `LoadedClass` address every PIC site carries (dereferenced the same way by
    /// `install_cap_thunk`'s trace and `rava_dispatch_miss`'s error path); classes are pinned
    /// for the life of the process.
    fn derive(caller_ptr: i64, cp: i64, cap_n: u16, wide_mask: u64, argc: u16) -> Result<Self, usize> {
        let Ok(cp) = u16::try_from(cp) else { return Err(2) };
        if caller_ptr == 0 {
            return Err(2);
        }
        // SAFETY: a non-zero `caller_ptr` on this path is the site's `LoadedClass`, leaked and
        // baked into the caller's machine code; it outlives every thunk built for it.
        let caller = unsafe { &*(caller_ptr as *const crate::class_loader::LoadedClass) };
        let Some(m) = caller.file.pool.member_ref(cp) else { return Err(2) };
        let widths = arg_slot_widths(m.descriptor);
        Self::from_widths(&widths, cap_n, wide_mask, argc)
    }
}

/// ⛔**THE INSTALL MUST NOT BE A COMPILE LOOP.** `install_cap_thunk` keys the 4-way cache on the
/// receiver's arena CLASS id, and every lambda of one capture signature shares that id; the thunk
/// then checks the lambda's SITE index against the one baked into it. So a call site that sees two
/// lambda sites of the same signature ping-pongs: guard mismatch → miss → a fresh Cranelift build →
/// install over the same way → the other lambda arrives → mismatch → build… Each round is a whole
/// compile, and the product boot spent 65% of its last seconds in regalloc2 doing exactly that
/// (found by `RAVA_PROFILE_PC_STACKS`, `build_site_thunk` under 12.3% of all stacks). The loop
/// opened when arena births made more lambdas share a capture signature and the void+wide corner
/// admitted their thunks. Cure: one thunk per `(site, lambda site)` — reused, never rebuilt — and a
/// site whose way flips between lambda sites more than twice is refused for good.
#[cfg(feature = "next-gen-gc")]
pub(crate) mod site_memo {
    use std::cell::RefCell;
    use std::collections::HashMap;
    pub(crate) enum Verdict {
        Reuse(usize),
        Build,
        Refuse,
    }
    struct Way {
        last_site: u32,
        flips: u8,
        thunks: Vec<(u32, usize)>,
    }
    thread_local! {
        static WAYS: RefCell<HashMap<(i64, u32), Way>> = RefCell::new(HashMap::new());
    }
    pub static POLY_REFUSED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    pub static REUSED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    const MAX_FLIPS: u8 = 2;
    pub(crate) fn ask(cache_ptr: i64, cid: u32, site_idx: u32) -> Verdict {
        WAYS.with(|w| {
            let mut w = w.borrow_mut();
            match w.get_mut(&(cache_ptr, cid)) {
                None => Verdict::Build,
                Some(way) => {
                    if way.flips > MAX_FLIPS {
                        return Verdict::Refuse;
                    }
                    if way.last_site != site_idx {
                        way.flips += 1;
                        if way.flips > MAX_FLIPS {
                            POLY_REFUSED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            return Verdict::Refuse;
                        }
                    }
                    match way.thunks.iter().find(|(s, _)| *s == site_idx) {
                        Some((_, t)) => {
                            way.last_site = site_idx;
                            REUSED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            Verdict::Reuse(*t)
                        }
                        None => Verdict::Build,
                    }
                }
            }
        })
    }
    pub(crate) fn built(cache_ptr: i64, cid: u32, site_idx: u32, thunk: usize) {
        WAYS.with(|w| {
            let mut w = w.borrow_mut();
            let way = w.entry((cache_ptr, cid)).or_insert(Way { last_site: site_idx, flips: 0, thunks: Vec::new() });
            way.last_site = site_idx;
            way.thunks.push((site_idx, thunk));
        })
    }
}

/// DEFAULT ON (`RAVA_NO_CAP_THUNK` opts out).
///
/// ⭐**MEASURED, and this is the first time this engine serves a capturing lambda from machine
/// code at all.** Seven installs on `bench/ShapeOr.java` turn 6.19 M host excursions into PIC
/// hits:
///
/// | | HOTNS |
/// |---|---|
/// | thunk | **9.747 / 9.540 s** |
/// | without | 10.718 / 10.739 s |
///
/// **−10.3%, disjoint, `SUM=3cfa327f01e0c901` either way.** And on the product boot, once the
/// VOID refusal replaced the wide-parameter one (see `jit_direct_call::install_cap_thunk` — the
/// wide clause was masking a different bug and 97% of the population was behind it):
///
/// | | wall | CPU |
/// |---|---|---|
/// | thunk | **46.6 / 46.4 s** | **49.6 / 49.9 s** |
/// | without | 47.6 / 47.6 s | 51.2 / 51.3 s |
///
/// **−2.3% wall, −2.9% CPU, disjoint** — up from −0.8% while the wide population was refused. A
/// second batch of three pairs on a noisier box reads 47.0/48.1/46.6 against 47.6/48.0/48.4
/// (min-of-3 −2.1%, means −1.6%, CPU −1.7%) and overlaps at the top, so **call it ~2%, not 2.3%**.
///
/// The admission is the UNION of two separately gate-proven sets — a void callee with no wide
/// parameters (the original shipped default) and every wide mask among non-void callees — with
/// only their intersection refused. Serving the union measures the same as refusing void
/// outright (47.4 against 47.1 over three pairs, overlapping), so the win lives entirely in the
/// wide non-void population; the union is kept because it is a strict superset of two proven
/// sets, not because it added anything.
///
/// Verified with the flag on: 4 gates on their hashes, 431 tests, 18/18 differential probes, and a
/// 120 s product soak alive with 0 new throws.
pub(crate) fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_CAP_THUNK").is_none())
}

/// Compile a thunk for one (impl, capture shape), or `None` if this shape must keep the Rust path.
///
/// Cached by the whole shape, because two sites that agree on every field of it are served by the
/// same code — a lambda site's identity lives in `impl_fptr`, so nothing here needs a site key
/// (which is exactly the thing that costs 3.3× when it is pushed into the dispatch table's class
/// space instead).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn cap_thunk_for(
    impl_fptr: usize,
    cap_n: u16,
    cap_ref_mask: u64,
    buf_len: u16,
    argc: u16,
) -> Option<usize> {
    if !enabled() {
        return None;
    }
    thread_local! {
        static MEMO: std::cell::RefCell<crate::FastMap<(usize, u16, u64, u16, u16), Option<usize>>> =
            std::cell::RefCell::new(crate::FastMap::default());
    }
    let key = (impl_fptr, cap_n, cap_ref_mask, buf_len, argc);
    if let Some(v) = MEMO.with(|m| m.borrow().get(&key).copied()) {
        return v;
    }
    let v = build(impl_fptr, cap_n, cap_ref_mask, buf_len, argc);
    MEMO.with(|m| {
        m.borrow_mut().insert(key, v);
    });
    v
}

#[cfg(feature = "next-gen-gc")]
fn build(
    impl_fptr: usize,
    cap_n: u16,
    cap_ref_mask: u64,
    buf_len: u16,
    argc: u16,
) -> Option<usize> {
    use cranelift::codegen::ir::{StackSlotData, StackSlotKind};
    use cranelift::frontend::{FunctionBuilder, FunctionBuilderContext};

    if cap_n == 0 || impl_fptr == 0 || cap_n > 63 {
        return None;
    }
    // The impl's frame must hold the captures AND the arguments. A shape that does not fit is not
    // a thunk candidate — it is a resolve bug, and refusing here keeps it readable as one.
    if cap_n as usize + argc as usize > buf_len as usize {
        return None;
    }
    // See the module note: the identity arm is conditional on this.
    if crate::nextgen::pinned_ptrs_enabled() {
        return None;
    }

    let mut guard = ModuleGuard {
        module: Some(
            SHARED_MODULE
                .with(|c| c.borrow_mut().take())
                .unwrap_or_else(build_shared_module),
        ),
    };
    let module = guard.module.as_mut()?;
    let mut ctx = module.make_context();
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(I64)); // locals pointer
    sig.params.push(AbiParam::new(I64)); // array descriptor-base cell pointer
    sig.returns.push(AbiParam::new(I64)); // result bits
    ctx.func.signature = sig.clone();

    let mut slow_sig = module.make_signature();
    slow_sig.params.push(AbiParam::new(I64));
    slow_sig.returns.push(AbiParam::new(I64));
    let slow_id = module
        .declare_function("rava_cap_cell_slow", Linkage::Import, &slow_sig)
        .ok()?;

    let hdr = crate::nextgen::HDR as i32;
    let cap0 = crate::nextgen::AL_HDR_CELLS as i32;

    let mut fb_ctx = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fb_ctx);
        let slow_ref = module.declare_func_in_func(slow_id, b.func);
        let ss = b.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            u32::from(buf_len) * 8,
            3,
        ));
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        b.seal_block(entry);
        let buf = b.block_params(entry)[0];
        let jctx = b.block_params(entry)[1];
        let slot = b.ins().stack_addr(I64, ss, 0);
        let recv = b.ins().load(I64, MemFlags::trusted(), buf, Offset32::new(0));

        for i in 0..i32::from(cap_n) {
            let raw = b.ins().load(
                I64,
                MemFlags::trusted(),
                recv,
                Offset32::new(hdr + (cap0 + i) * 8),
            );
            let v = if cap_ref_mask & (1u64 << i) != 0 {
                emit_ref_cell(&mut b, raw, slow_ref)
            } else {
                raw
            };
            b.ins()
                .store(MemFlags::trusted(), v, slot, Offset32::new(i * 8));
        }
        for j in 0..i32::from(argc) {
            let a = b.ins().load(
                I64,
                MemFlags::trusted(),
                buf,
                Offset32::new((1 + j) * 8),
            );
            b.ins().store(
                MemFlags::trusted(),
                a,
                slot,
                Offset32::new((i32::from(cap_n) + j) * 8),
            );
        }

        let sref = b.import_signature(sig.clone());
        let target = b.ins().iconst(I64, impl_fptr as i64);
        let call = b.ins().call_indirect(sref, target, &[slot, jctx]);
        let r = b.inst_results(call)[0];
        b.ins().return_(&[r]);
        b.finalize();
    }

    let func_id = module.declare_anonymous_function(&sig).ok()?;
    module.define_function(func_id, &mut ctx).ok()?;
    module.clear_context(&mut ctx);
    module.finalize_definitions().ok()?;
    Some(module.get_finalized_function(func_id) as usize)
}

/// The site-guarded thunk: the shape a PIC entry can actually point at.
///
/// ⭐**THE SITE CHECK LIVES HERE, NOT IN THE DISPATCH KEY** — and that is the whole reason this
/// can work at all. Every lambda of one capture signature shares a class id, so a PIC keyed by
/// class id alone would call another site's impl. The tree has measured both ways of teaching the
/// KEY about sites and both are disasters: `RAVA_LAMBDA_SITE_CID` (the dispatch table's class-id
/// space multiplied by the site count) is **3.3× slower** on `ShapeOr`, and computing the lambda
/// key before the 4-way probe is **ChunkFill 4,589 ms → 60,282 ms**, a 13× regression, which is
/// why `dispatch_pic.rs` moved that computation onto the miss path.
///
/// So the PIC keeps keying on the plain class id — **zero extra instructions on the hit path** —
/// and the thunk opens with one `AL_SITE_SLOT` load and one compare against a BAKED site index.
/// On a mismatch it calls `rava_dispatch_miss` with this call site's own `cache_ptr`,
/// `caller_ptr`, `cp` and `kind` baked in, all of which are known here precisely because this
/// thunk is per CALL SITE. The check is therefore paid only after the PIC has already hit, on a
/// path that today leaves machine code entirely.
#[cfg(feature = "next-gen-gc")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_site_thunk(
    impl_fptr: usize,
    cap_n: u16,
    cap_ref_mask: u64,
    wide_mask: u64,
    buf_len: u16,
    argc: u16,
    site_idx: u32,
    cache_ptr: i64,
    caller_ptr: i64,
    cp: i64,
    kind: i64,
    // A VOID callee's returned bits are not defined by its body; return a zero rather than pass
    // them through. See the void-AND-wide corner in `install_cap_thunk`.
    void_ret: bool,
) -> Option<usize> {
    // `hr_capvoid`: read the caller's slot layout from its descriptor. `None` is the historic
    // emission verbatim — packed reads, `buf` to the miss arm, no pad stores.
    let layout = if hr_capvoid_enabled() {
        match SiteLayout::derive(caller_ptr, cp, cap_n, wide_mask, argc) {
            Ok(l) => Some(l),
            Err(why) => {
                refuse_build(shape_of(void_ret, wide_mask), why);
                return None;
            }
        }
    } else {
        None
    };
    build_site_thunk_laid(
        impl_fptr, cap_n, cap_ref_mask, wide_mask, buf_len, argc, site_idx, cache_ptr, caller_ptr,
        cp, kind, void_ret, layout,
    )
}

/// The emitter behind [`build_site_thunk`], with the caller's layout made explicit so a test can
/// hand it a slot-laid buffer without a live `LoadedClass`.
///
/// `layout == None` is the HISTORIC thunk: SAM argument `j` is read from cell `1 + j`, the bail
/// arm hands `buf` itself to `rava_dispatch_miss`, and no pad slot is written. With a layout,
/// argument `j` is read from `layout.src[j]`, every wide value's pair slot in the thunk's own
/// stack slot is zeroed (it lies below the callee's `args_slots` and inside the cell-root range
/// the callee registers), and a holed site's bail arm packs `[recv, args…]` one cell per
/// argument before calling the miss — the ABI the miss reads.
#[cfg(feature = "next-gen-gc")]
#[allow(clippy::too_many_arguments)]
fn build_site_thunk_laid(
    impl_fptr: usize,
    cap_n: u16,
    cap_ref_mask: u64,
    wide_mask: u64,
    buf_len: u16,
    argc: u16,
    site_idx: u32,
    cache_ptr: i64,
    caller_ptr: i64,
    cp: i64,
    kind: i64,
    void_ret: bool,
    layout: Option<SiteLayout>,
) -> Option<usize> {
    use cranelift::codegen::ir::{StackSlotData, StackSlotKind};
    use cranelift::frontend::{FunctionBuilder, FunctionBuilderContext};

    let shape = shape_of(void_ret, wide_mask);
    if cap_n == 0 || impl_fptr == 0 || cap_n > 63 || cache_ptr == 0 {
        refuse_build(shape, 5);
        return None;
    }
    // `wide_mask` indexes the WRITE order — captures first, then arguments — and a long or double
    // takes its cell AND a pad slot, exactly as `jit_direct_invoke`'s `write_arg` advances
    // (`w += 1`, again if wide). Every destination offset is therefore known here.
    let wide_at = |i: usize| i < 64 && wide_mask & (1u64 << i) != 0;
    let total = cap_n as usize + argc as usize;
    let mut dst: Vec<i32> = Vec::with_capacity(total);
    let mut w: i32 = 0;
    for i in 0..total {
        dst.push(w);
        w += if wide_at(i) { 2 } else { 1 };
    }
    if w > i32::from(buf_len) {
        refuse_build(shape, 0);
        return None;
    }
    if crate::nextgen::pinned_ptrs_enabled() {
        refuse_build(shape, 1);
        return None;
    }
    if let Some(l) = &layout {
        // The layout was derived for exactly this `argc`; anything else is a caller bug, and a
        // wrong `src` index would read past the site's buffer.
        if l.src.len() != usize::from(argc) {
            refuse_build(shape, 3);
            return None;
        }
    }

    let mut guard = ModuleGuard {
        module: Some(
            SHARED_MODULE
                .with(|c| c.borrow_mut().take())
                .unwrap_or_else(build_shared_module),
        ),
    };
    let module = guard.module.as_mut()?;
    let mut ctx = module.make_context();
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(I64));
    sig.params.push(AbiParam::new(I64));
    sig.returns.push(AbiParam::new(I64));
    ctx.func.signature = sig.clone();

    let mut slow_sig = module.make_signature();
    slow_sig.params.push(AbiParam::new(I64));
    slow_sig.returns.push(AbiParam::new(I64));
    let slow_id = module
        .declare_function("rava_cap_cell_slow", Linkage::Import, &slow_sig)
        .ok()?;
    let mut miss_sig = module.make_signature();
    for _ in 0..8 {
        miss_sig.params.push(AbiParam::new(I64));
    }
    miss_sig.returns.push(AbiParam::new(I64));
    let miss_id = module
        .declare_function("rava_dispatch_miss", Linkage::Import, &miss_sig)
        .ok()?;

    let hdr = crate::nextgen::HDR as i32;
    let cap0 = crate::nextgen::AL_HDR_CELLS as i32;
    let site_off = hdr + (crate::nextgen::AL_SITE_SLOT as i32) * 8;

    let mut fb_ctx = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fb_ctx);
        let slow_ref = module.declare_func_in_func(slow_id, b.func);
        let miss_ref = module.declare_func_in_func(miss_id, b.func);
        let ss = b.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            u32::from(buf_len) * 8,
            3,
        ));
        let entry = b.create_block();
        let unpack = b.create_block();
        let bail = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        b.seal_block(entry);
        let buf = b.block_params(entry)[0];
        let jctx = b.block_params(entry)[1];
        let recv = b.ins().load(I64, MemFlags::trusted(), buf, Offset32::new(0));
        let site = b
            .ins()
            .load(I64, MemFlags::trusted(), recv, Offset32::new(site_off));
        let same = b.ins().icmp_imm(IntCC::Equal, site, i64::from(site_idx));
        b.ins().brif(same, unpack, &[], bail, &[]);

        // The receiver is a lambda of THIS capture shape but a DIFFERENT site: hand it back to the
        // host exactly as an un-cached dispatch would have gone.
        b.switch_to_block(bail);
        b.seal_block(bail);
        let a0 = b.ins().iconst(I64, cache_ptr);
        let a2 = b.ins().iconst(I64, caller_ptr);
        let a3 = b.ins().iconst(I64, cp);
        let a4 = b.ins().iconst(I64, kind);
        let a6 = b.ins().iconst(I64, i64::from(argc) + 1);
        let a7 = b.ins().iconst(I64, 0);
        // `rava_dispatch_miss` reads `n` PACKED cells. A narrow site's buffer already is that;
        // a holed one is repacked here, exactly as the PIC's own miss arm packs its copy.
        let mbuf = match &layout {
            Some(l) if l.holed => {
                let ps = b.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    (u32::from(argc) + 1) * 8,
                    3,
                ));
                b.ins().stack_store(recv, ps, 0);
                for (j, &s) in l.src.iter().enumerate() {
                    let a = b.ins().load(I64, MemFlags::trusted(), buf, Offset32::new(s * 8));
                    b.ins().stack_store(a, ps, (j as i32 + 1) * 8);
                }
                b.ins().stack_addr(I64, ps, 0)
            }
            _ => buf,
        };
        let mcall = b.ins().call(miss_ref, &[a0, recv, a2, a3, a4, mbuf, a6, a7]);
        let mbits = b.inst_results(mcall)[0];
        b.ins().return_(&[mbits]);

        b.switch_to_block(unpack);
        b.seal_block(unpack);
        let slot = b.ins().stack_addr(I64, ss, 0);
        // THE DEAD PAIR SLOT IS SCANNED (the rule `emit_direct_mono` and `emit_dispatch_pic` both
        // pay for): the impl registers `(slot, frame_len)` and its prologue zeroes only above
        // `args_slots`; every pad below that is this thunk's to write. Materialised only when the
        // layout arm is on AND a wide exists, so the historic and narrow emissions are untouched.
        let pad_zero = match &layout {
            Some(_) if wide_mask != 0 => Some(b.ins().iconst(I64, 0)),
            _ => None,
        };
        let pad = |b: &mut FunctionBuilder, i: usize| {
            if let Some(z) = pad_zero {
                if wide_at(i) {
                    b.ins().store(MemFlags::trusted(), z, slot, Offset32::new((dst[i] + 1) * 8));
                }
            }
        };
        for i in 0..i32::from(cap_n) {
            let raw = b.ins().load(
                I64,
                MemFlags::trusted(),
                recv,
                Offset32::new(hdr + (cap0 + i) * 8),
            );
            let v = if cap_ref_mask & (1u64 << i) != 0 {
                emit_ref_cell(&mut b, raw, slow_ref)
            } else {
                raw
            };
            b.ins().store(
                MemFlags::trusted(),
                v,
                slot,
                Offset32::new(dst[i as usize] * 8),
            );
            pad(&mut b, i as usize);
        }
        for j in 0..i32::from(argc) {
            // SOURCE cell: the caller's slot layout when known, packed `1 + j` historically.
            let src = match &layout {
                Some(l) => l.src[j as usize],
                None => 1 + j,
            };
            let a = b
                .ins()
                .load(I64, MemFlags::trusted(), buf, Offset32::new(src * 8));
            b.ins().store(
                MemFlags::trusted(),
                a,
                slot,
                Offset32::new(dst[(i32::from(cap_n) + j) as usize] * 8),
            );
            pad(&mut b, (i32::from(cap_n) + j) as usize);
        }
        let sref = b.import_signature(sig.clone());
        let target = b.ins().iconst(I64, impl_fptr as i64);
        let call = b.ins().call_indirect(sref, target, &[slot, jctx]);
        let r = if void_ret {
            b.ins().iconst(I64, 0)
        } else {
            b.inst_results(call)[0]
        };
        b.ins().return_(&[r]);
        b.finalize();
    }

    // DIAGNOSTIC (`RAVA_CAP_CLIF`): dump the thunk's IR, with the source cells it reads.
    if std::env::var_os("RAVA_CAP_CLIF").is_some() {
        let src: Option<&[i32]> = layout.as_ref().map(|l| l.src.as_slice());
        eprintln!(
            "==== CAP-THUNK cap_n={cap_n} argc={argc} wide=0x{wide_mask:x} buf_len={buf_len} void={void_ret} dst={dst:?} src={src:?}
{}
==== END ====",
            ctx.func.display()
        );
    }
    let func_id = module.declare_anonymous_function(&sig).ok()?;
    module.define_function(func_id, &mut ctx).ok()?;
    module.clear_context(&mut ctx);
    module.finalize_definitions().ok()?;
    let f = module.get_finalized_function(func_id) as usize;
    bump(&SHAPE_BUILT[shape]);
    Some(f)
}

/// One reference capture: null, handle, array handle, arena pointer, or the out-of-line box index.
#[cfg(feature = "next-gen-gc")]
fn emit_ref_cell(
    b: &mut cranelift::frontend::FunctionBuilder,
    raw: cranelift::codegen::ir::Value,
    slow: cranelift::codegen::ir::FuncRef,
) -> cranelift::codegen::ir::Value {
    let tag = b.ins().band_imm(raw, 3);
    let shifted = b.ins().sshr_imm(raw, 2);
    let as_ref = b.ins().bor_imm(shifted, crate::value::REF_TAG);
    let as_arr = b.ins().bor_imm(shifted, crate::value::ARRAY_TAG);
    let is_ref = b.ins().icmp_imm(IntCC::Equal, tag, 1);
    let is_arr = b.ins().icmp_imm(IntCC::Equal, tag, 2);
    let v = b.ins().select(is_ref, as_ref, raw);
    let v = b.ins().select(is_arr, as_arr, v);
    let zero = b.ins().iconst(I64, 0);
    let is_null = b.ins().icmp_imm(IntCC::Equal, raw, 0);
    let fast = b.ins().select(is_null, zero, v);

    // ⛔**THE BOX ARM MUST BE A BRANCH, NOT A `select`.** It was a select over a `call`, and a
    // select is not a branch — Cranelift evaluates BOTH arms, so `rava_cap_cell_slow` ran on
    // EVERY reference capture with whatever tag it had, indexing the arena box table by a garbage
    // index and pushing a box per call. The IR dump is what showed it: `v20 = call fn0(v12)` sat
    // above the compares, unguarded. Now the slow path is a real block, entered only when the
    // low two bits actually say `arena_box` — which `[PC-CAP]` measures at ZERO on both a boot
    // and the probe, so it should never execute at all.
    let is_box = b.ins().icmp_imm(IntCC::Equal, tag, 3);
    let slow_blk = b.create_block();
    let join = b.create_block();
    b.append_block_param(join, I64);
    b.ins().brif(is_box, slow_blk, &[], join, &[fast.into()]);
    b.switch_to_block(slow_blk);
    b.seal_block(slow_blk);
    let call = b.ins().call(slow, &[raw]);
    let boxed = b.inst_results(call)[0];
    b.ins().jump(join, &[boxed.into()]);
    b.switch_to_block(join);
    b.seal_block(join);
    b.block_params(join)[0]
}

#[cfg(all(test, feature = "next-gen-gc"))]
mod tests {
    use super::*;

    /// Stands in for a compiled impl: reports the buffer it was handed, so a test can assert on
    /// the exact `[captures…, args…]` layout the thunk built.
    static SEEN: std::sync::Mutex<Vec<i64>> = std::sync::Mutex::new(Vec::new());

    extern "C" fn fake_impl(buf: *mut i64, _ctx: i64) -> i64 {
        let n = SEEN.lock().unwrap().capacity().max(8);
        let v: Vec<i64> = (0..n).map(|i| unsafe { *buf.add(i) }).collect();
        *SEEN.lock().unwrap() = v;
        0x5eed
    }

    /// A receiver is only ever touched through `cell(p, i) = *(p + HDR + i*8)`, so a plain buffer
    /// with the right cells IS a valid stand-in — no arena, no allocator, no GC.
    fn fake_recv(cells: &[i64]) -> (Vec<u8>, i64) {
        let mut mem = vec![0u8; crate::nextgen::HDR + (crate::nextgen::AL_HDR_CELLS + cells.len()) * 8];
        let base = mem.as_mut_ptr();
        for (i, &c) in cells.iter().enumerate() {
            unsafe {
                *(base.add(crate::nextgen::HDR + (crate::nextgen::AL_HDR_CELLS + i) * 8) as *mut i64) = c;
            }
        }
        let addr = base as i64;
        (mem, addr)
    }

    fn run(cap_cells: &[i64], cap_ref_mask: u64, args: &[i64]) -> Vec<i64> {
        let _g = crate::parallel::TEST_LOCK.lock();
        unsafe { std::env::set_var("RAVA_CAP_THUNK", "1") };
        let cap_n = cap_cells.len() as u16;
        let argc = args.len() as u16;
        let buf_len = cap_n + argc;
        *SEEN.lock().unwrap() = Vec::with_capacity(buf_len as usize);
        let t = build(fake_impl as usize, cap_n, cap_ref_mask, buf_len, argc)
            .expect("thunk builds for an emittable shape");
        let (_mem, recv) = fake_recv(cap_cells);
        let mut inbuf: Vec<i64> = Vec::with_capacity(1 + args.len());
        inbuf.push(recv);
        inbuf.extend_from_slice(args);
        let f: extern "C" fn(*mut i64, i64) -> i64 = unsafe { std::mem::transmute(t) };
        let r = f(inbuf.as_mut_ptr(), 0);
        assert_eq!(r, 0x5eed, "the thunk must return the impl's result unchanged");
        SEEN.lock().unwrap().clone()
    }

    #[test]
    fn primitive_captures_are_copied_verbatim_and_args_follow() {
        let got = run(&[7, -3], 0, &[11, 12]);
        assert_eq!(&got[..4], &[7, -3, 11, 12]);
    }

    #[test]
    fn reference_captures_take_the_shapes_box_ref_gives() {
        // 0 -> null; (h<<2)|1 -> Ref; (h<<2)|2 -> Array; an untagged pointer passes through.
        let cells = [0i64, (5i64 << 2) | 1, (9i64 << 2) | 2, 0x4000i64];
        let got = run(&cells, 0b1111, &[42]);
        assert_eq!(got[0], 0, "a null cell stays null");
        assert_eq!(got[1], 5i64 | crate::value::REF_TAG);
        assert_eq!(got[2], 9i64 | crate::value::ARRAY_TAG);
        assert_eq!(got[3], 0x4000, "an arena pointer is passed through unchanged");
        assert_eq!(got[4], 42, "the SAM argument follows the captures");
    }

    /// The site-guarded thunk on its MATCHING path: the guard must be transparent when the
    /// receiver really is from the site the thunk was built for. (The mismatch path calls the real
    /// `rava_dispatch_miss` and needs a live VM, so it is exercised by the gates, not here.)
    #[test]
    fn the_site_guard_is_transparent_on_a_match() {
        let _g = crate::parallel::TEST_LOCK.lock();
        unsafe { std::env::set_var("RAVA_CAP_THUNK", "1") };
        let cells = [7i64, (5i64 << 2) | 1];
        let (cap_n, argc) = (2u16, 1u16);
        *SEEN.lock().unwrap() = Vec::with_capacity((cap_n + argc) as usize);
        // The historic (layout-less) emission; `caller`/`cp` are never dereferenced by it.
        let t = build_site_thunk_laid(
            fake_impl as usize, cap_n, 0b10, /*wide_mask*/ 0, cap_n + argc, argc,
            /*site*/ 77, /*cache*/ 0x1000, /*caller*/ 0x2000, /*cp*/ 3, /*kind*/ 1,
            /*void_ret*/ false, None,
        )
        .expect("a site thunk builds");
        let (_mem, base) = site_recv(77, &cells);
        let mut inbuf = vec![base, 99];
        let f: extern "C" fn(*mut i64, i64) -> i64 = unsafe { std::mem::transmute(t) };
        assert_eq!(f(inbuf.as_mut_ptr(), 0), 0x5eed);
        let got = SEEN.lock().unwrap().clone();
        assert_eq!(got[0], 7);
        assert_eq!(got[1], 5i64 | crate::value::REF_TAG);
        assert_eq!(got[2], 99);
    }

    /// A receiver with its site index in `AL_SITE_SLOT` and its captures after the header.
    fn site_recv(site: i64, cells: &[i64]) -> (Vec<u8>, i64) {
        let mut mem =
            vec![0u8; crate::nextgen::HDR + (crate::nextgen::AL_HDR_CELLS + cells.len()) * 8];
        let base = mem.as_mut_ptr();
        unsafe {
            *(base.add(crate::nextgen::HDR + crate::nextgen::AL_SITE_SLOT * 8) as *mut i64) = site;
            for (i, &c) in cells.iter().enumerate() {
                *(base.add(crate::nextgen::HDR + (crate::nextgen::AL_HDR_CELLS + i) * 8)
                    as *mut i64) = c;
            }
        }
        (mem, base as i64)
    }

    /// `hr_capvoid`: the shape that killed ChunkFill — an array capture and wide SAM arguments
    /// arriving in the caller's SLOT-laid buffer `[recv, d0, pad, d1, pad]`. The thunk must read
    /// `d1` from cell 3 (not cell 2, the pad), lay `[cap, d0, 0, d1, 0]` for the impl, and write
    /// both pads.
    #[test]
    fn wide_sam_arguments_are_read_from_their_slots_and_their_pads_are_written() {
        let _g = crate::parallel::TEST_LOCK.lock();
        unsafe { std::env::set_var("RAVA_CAP_THUNK", "1") };
        let cells = [(9i64 << 2) | 2];
        let (cap_n, argc) = (1u16, 2u16);
        let buf_len = 5u16; // cap + 2 × (cell + pad)
        let wide_mask = 0b110u64; // positions 1 and 2 of the write order are wide
        let layout = SiteLayout::from_widths(&[1, 2, 2], cap_n, wide_mask, argc).unwrap();
        assert_eq!(layout.src, [1, 3]);
        assert!(layout.holed);
        *SEEN.lock().unwrap() = Vec::with_capacity(buf_len as usize);
        let t = build_site_thunk_laid(
            fake_impl as usize, cap_n, 0b1, wide_mask, buf_len, argc, /*site*/ 5,
            /*cache*/ 0x1000, /*caller*/ 0x2000, /*cp*/ 3, /*kind*/ 3, /*void_ret*/ true,
            Some(layout),
        )
        .expect("a wide site thunk builds");
        let (_mem, base) = site_recv(5, &cells);
        let d0 = 1.5f64.to_bits() as i64;
        let d1 = (-2.25f64).to_bits() as i64;
        let mut inbuf = vec![base, d0, 0x7bad_0000_0000_0000u64 as i64, d1, 0x7bad];
        let f: extern "C" fn(*mut i64, i64) -> i64 = unsafe { std::mem::transmute(t) };
        assert_eq!(f(inbuf.as_mut_ptr(), 0), 0, "a void callee's bits are replaced by zero");
        let got = SEEN.lock().unwrap().clone();
        assert_eq!(got[0], 9i64 | crate::value::ARRAY_TAG);
        assert_eq!(got[1], d0);
        assert_eq!(got[2], 0, "the first wide's pair slot is written");
        assert_eq!(got[3], d1, "the second wide is read from its SLOT, not the pad after d0");
        assert_eq!(got[4], 0, "the second wide's pair slot is written");
    }

    /// The layout guard only rejects: an arity mismatch, a receiver width other than one, and
    /// a per-argument width the caller and the impl disagree on all keep the general path.
    #[test]
    fn the_layout_guard_refuses_what_a_copy_cannot_express() {
        // Narrow throughout: packed and slot layouts coincide, no hole.
        let l = SiteLayout::from_widths(&[1, 1, 1], 2, 0, 2).unwrap();
        assert_eq!(l.src, [1, 2]);
        assert!(!l.holed);
        // A trailing wide (the shape the historic thunk served by coincidence).
        let l = SiteLayout::from_widths(&[1, 1, 2], 1, 0b100, 2).unwrap();
        assert_eq!(l.src, [1, 2]);
        assert!(l.holed);
        // Arity: the descriptor names three arguments, the site handed two.
        assert_eq!(SiteLayout::from_widths(&[1, 1, 1, 1], 0, 0, 2).err(), Some(3));
        // Width: caller wide, impl narrow (a widening/boxing adaptation, not a copy).
        assert_eq!(SiteLayout::from_widths(&[1, 2], 1, 0, 1).err(), Some(4));
        // Width: caller narrow, impl wide.
        assert_eq!(SiteLayout::from_widths(&[1, 1], 1, 0b10, 1).err(), Some(4));
        // A wide CAPTURE is the impl's business, not the caller's: still admitted.
        let l = SiteLayout::from_widths(&[1, 1], 1, 0b1, 1).unwrap();
        assert_eq!(l.src, [1]);
        assert!(!l.holed);
    }

    #[test]
    fn a_shape_whose_frame_cannot_hold_it_is_refused() {
        let _g = crate::parallel::TEST_LOCK.lock();
        assert!(build(fake_impl as usize, 3, 0, 2, 2).is_none());
        assert!(build(fake_impl as usize, 0, 0, 8, 2).is_none());
        assert!(build(0, 2, 0, 8, 2).is_none());
    }
}
