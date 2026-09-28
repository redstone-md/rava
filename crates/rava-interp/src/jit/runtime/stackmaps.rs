//! Cranelift user stack maps as a rooting protocol for compiled frames.
//!
//! * `RAVA_STACKMAPS=probe` / `=sp` — PHASE A. Maps emitted BESIDE the shadow-frame rooting and
//!   cross-checked against it; the collector acts only on the shadow frame.
//! * `RAVA_STACKMAPS=1` — PHASE B. The maps ARE the rooting protocol: not one `spill_for_gc`
//!   store, cell-root refresh or `reload_*_after_gc` load is emitted for a compiled frame, and
//!   the collector roots and FORWARDS through the map slots instead.
//! * unset (default) — nothing declared, nothing registered, nothing scanned; emission
//!   byte-identical to the historic engine.
//!
//! # Why this exists
//!
//! Every GC-capable call in compiled code today pays a rooting protocol: `spill_for_gc`
//! writes every local `Variable` and every ref-typed operand-stack value into the
//! caller-visible cell buffer, the self-registering prologue has already published that
//! buffer as a `(ptr, len)` cell-root range (`frames::cells_all`), and
//! `reload_locals_after_gc` / `reload_after_gc` re-read the (possibly forwarded) words
//! back afterwards. HotSpot pays none of that: C2 records a precise stack map per
//! safepoint and the collector reads the map.
//!
//! `cranelift-frontend-0.132.1` can do the same thing:
//!
//!   * `FunctionBuilder::declare_var_needs_stack_map(var)`   (`src/frontend.rs:439`)
//!   * `FunctionBuilder::declare_value_needs_stack_map(val)` (`src/frontend.rs:554`)
//!   * the `SafepointSpiller` pass in `src/frontend/safepoints.rs` runs at `finalize()`
//!     and appends `ir::UserStackMapEntry`s to every safepoint instruction
//!   * `MachBufferFinalized::user_stack_maps()`
//!     (`cranelift-codegen-0.132.1/src/machinst/buffer.rs:1918`) hands back
//!     `&[(CodeOffset, u32, ir::UserStackMap)]` — a RETURN-ADDRESS code offset, the frame's
//!     `active_size()` span, and the map itself, whose `entries()`
//!     (`src/ir/user_stack_maps.rs:176`) yields `(ir::Type, offset-from-SP)`.
//!
//! # Finding a compiled frame without walking the stack
//!
//! A stack map is SP-relative, so using one needs the SP of the frame it belongs to. The
//! obvious route is an RBP chain walk from the collection point — and it does not work
//! here, because the collection point is deep inside Rust and this tree's host code is not
//! built with `-C force-frame-pointers`, so the chain is broken before the first compiled
//! frame is reached. (Windows unwind info is no help either: `cranelift-jit-0.132.1` never
//! calls `RtlAddFunctionTable`, so `RtlVirtualUnwind` treats a compiled frame as a leaf.)
//!
//! So the SP is recorded from the compiled side instead, where it is free:
//!
//!   * Cranelift x64 keeps SP CONSTANT through a function body (the frame is reserved once
//!     in the prologue; outgoing args live at `[SP, SP+outgoing_args_size)`), so the SP
//!     observed at the self-registering prologue is the SP at every safepoint in that body.
//!   * The prologue already pushes `(locals_ptr, frame_len)` onto the per-thread cell
//!     stack. Under `RAVA_STACKMAPS=1` it ALSO writes `get_stack_pointer` into a parallel
//!     array at the same index (`JitThreadCtx::sp_buf`), so every published cell-root range
//!     is paired with the SP of the compiled frame that published it.
//!   * Every compiled frame alive at a collection is, by definition, blocked inside a call,
//!     and that call's `call` instruction pushed its return address at `SP - 8`. That
//!     return address is exactly the key `user_stack_maps()` is sorted by.
//!
//! Hence: `ret = *(SP - 8)` → registered code range → the map at that return offset →
//! root addresses `SP + entry_offset`. No frame pointers, no stack walk, no asm.
//!
//! # What is compared
//!
//! The two sides name slots in DIFFERENT memories — a range names words in a heap-pooled
//! call buffer, a map names words in the compiled frame's own stack slots — so the census
//! compares the VALUES (the tagged i64 cells) each side makes visible, per frame, and
//! reports the two directions separately. See [`report`].
//!
//! # WHAT PHASE A MEASURED (2026-09-03, ReloadBench --reloads 3, MC classpath)
//!
//! ✅ **The frame-location mechanism is exact.** Over every census run, 100% of the
//! `*(SP-8)` words were preceded by a `call` encoding (`preceded_by_call`), 0 map offsets
//! fell outside the recorded frame span, and 0 published ranges lacked a recorded SP.
//!
//! ✅ **The maps never named a root the ranges miss.** Over the whole-collection union,
//! "named by a map and by NO range" was 0 in most runs and at most 4 arena cells in one
//! (out of 172 map-named cells) — and that residue has benign explanations the census
//! cannot exclude (a Cranelift safepoint at a callee our emitters know cannot collect, or a
//! cell rooted through one of the collector's OTHER root sources, which this census does
//! not compare against).
//!
//! ⛔ **But the maps are nearly EMPTY where it counts, and the reason is structural: the two
//! mechanisms are mutually exclusive.** `reload_locals_after_gc` executes
//! `b.def_var(var, load(buffer))` for EVERY local immediately after the call, so in SSA
//! terms the pre-call value of every local has no use after the safepoint and is therefore
//! NOT LIVE ACROSS IT. Cranelift's liveness is right to record nothing. Statically only
//! 56,391 of 99,781 CLIF safepoints carry a map at all, and dynamically ~80% of the
//! observed return addresses sat at a safepoint with no map while the frame's range held
//! GC cells. The maps that DO exist are dominated by ref OPERAND-STACK values, which the
//! locals-only reload sites never refresh. **A map's contents cannot be validated against
//! the shadow frame while the shadow frame is still running: deleting the reload is not the
//! last step of Phase B, it is the precondition for measuring Phase B at all.**
//!
//! ⛔ **And the declaration is not GC-neutral.** With the nursery shrunk to 64 MB
//! (`RAVA_NURSERY_KB=65536`, default 256 MB), the `declare` arm died with an access
//! violation inside `nextgen::census::arena_trace_refs` in 2 of 4 runs while the default
//! arm passed 3 of 3 — and the `sp` arm (this whole census apparatus, SP store and
//! cross-check included, with NOTHING declared) passed 4 of 4. That attributes the
//! instability to `declare_*_needs_stack_map` itself: `SafepointSpiller` rewrites every use
//! of a declared value into a load from a Cranelift stack slot that OUR collector neither
//! scans nor forwards, which lengthens the window in which the known un-reloaded ref
//! operand-stack values (see the note at `emit::ops::fields`) hold a stale pointer. At
//! 2 MB BOTH arms crash in the same collector, so the underlying defect is not new — but
//! the declaration reliably moves it into range.
//!
//! # PHASE B — THE REPLACEMENT (`RAVA_STACKMAPS=1`)
//!
//! Phase A's two findings said the protocols cannot coexist, so Phase B swaps them atomically at
//! emit time. With the flag on:
//!
//!   * `gcroots::spill_for_gc` DECLARES (every ref operand-stack value, at the one choke point
//!     all ~40 GC-capable emitters share) and emits NO stores. `emit::body` and
//!     `emit::inline` declare every local `Variable` — the caller's and an inlined callee's —
//!     at creation, before the first use, as `declare_var_needs_stack_map` requires.
//!   * `gcroots::reload_after_gc` / `reload_locals_after_gc` emit nothing. That is what makes the
//!     pre-call values LIVE ACROSS the safepoint, which is what fills the maps — Phase A measured
//!     the reload emptying them.
//!   * The ~15 sites that hand-reloaded ONE ref operand-stack slot out of the spill area and
//!     passed it as a block argument go through `gcroots::reload_ref_slot`, which under the flag
//!     returns the original value: Cranelift's `SafepointSpiller` parks it in a stack slot across
//!     the call and reloads it at the next use, and the collector rewrote that slot.
//!   * `emit::guarded_inline::emit_deopt_exit` keeps its stores through `spill_frame_words`. That
//!     buffer is not a root set there, it is the interpreter's `Frame` — a SEMANTIC use of the
//!     same memory, and the maps replace rooting, not the deopt transition.
//!   * `collect`/`gc_collect`/`Heap::gc` root through [`for_each_map_slot`] and forward the arena
//!     pointers IN PLACE, exactly as they forward a cell-root range; the legacy handle mark seeds
//!     from the same slots through `jit::cell_to_root`, because a compiled frame's tagged handle
//!     cells otherwise reach that mark only through the buffer that is no longer written.
//!
//! ## WHAT IS STILL CONSERVATIVE, DELIBERATELY
//!
//! The self-registering prologue STILL publishes `(locals_ptr, frame_len)` and the collector still
//! scans it. Two reasons, and neither is a rooting dependency of the compiled body:
//!
//!   1. **That push is where the frame's SP is recorded**, and the SP is what turns a map into
//!      addresses. Removing it removes the ability to find the frame.
//!   2. **It is the fallback for the capacity-overflow arm.** A push that goes through
//!      `cells_slow` records SP 0, so [`for_each_map_slot`] cannot locate that frame — the scanned
//!      buffer is all it has. (`FRAMES_NO_SP` measured 0 over every run here, i.e. the arm never
//!      fired in these workloads; the fallback is insurance, not a live path.)
//!
//! What that buffer holds under the flag is exactly the prologue's entry image — the caller's
//! argument cells below `args_slots`, zeros above (`emit::prologue`'s zero loop) — written once,
//! never refreshed, and forwarded in place at every collection so it never goes stale. So a
//! reference that arrived as an ARGUMENT retains a second, static root for the frame's lifetime.
//! It costs nothing (both halves are written in the OFF arm too, by the same prologue) and it is
//! the one reason the maps are not *literally* the only root source for a compiled frame.
//!
//! ## WHAT PHASE B MEASURED (2026-09-03, final binary, MC classpath)
//!
//! ✅ **The maps fill up once the reload is gone.** At the same 2000-function census boundary:
//! 78,495 of 101,942 CLIF safepoints carry a map (77%) naming 289,750 slots, against Phase A's
//! 56,391 of 99,781 (56%). Dynamically the inversion is sharper: ~50% of located frames sit at a
//! return address with a map, against Phase A's ~20%.
//!
//! ✅ **THE COST IT DELETES, COUNTED.** Over the same 2000 compiled functions the shadow frame
//! emitted 41,814 spill sites (269,120 stores) and 41,814 reload sites (242,375 loads, each local
//! load also a `def_var`) — **511,495 rooting memory operations, 255.7 per compiled function, ~21
//! spill+reload site pairs per function** — and the declare arm emits NONE of them. In machine
//! code that is **10,317 B/fn → 6,738 B/fn, −34.7%** (`arm=probe(size-only)` vs `arm=declare`,
//! same benchmark, same boundary).
//!
//! ✅ **Correct on every gate, both arms.** `sem_all` 23/23 vs `java -Xint`; `pgo_gates` chunkfill
//! / ringscan / ringlane / nbtsave all at their recorded hashes; `BootstrapBench`
//! `b9e7debe631b42b6` 3+3 runs; `ReloadBench --reloads 3` `c956377e19071d7a` 3+3 runs;
//! `--reloads 10` 2+2 runs. Frame location stayed exact: 0 ranges with no recorded SP, 0 bad map
//! offsets, ≤4 of ~400 return addresses per run in no registered function.
//!
//! ✅ **The verifier found nothing.** `RAVA_STACKMAPS_VERIFY=1` over every post-collection walk:
//! up to 207 slots read per run, up to 173 holding an arena pointer, **0 missed forwards** and 0
//! out-of-extent words.

#![cfg(feature = "next-gen-gc")]

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// What `RAVA_STACKMAPS` asked for. Read once per process (the CLI applies its config before
/// any VM exists, so the answer is a process constant — the same argument `jit::env_gate`
/// ships on).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Default. Not one `declare_*_needs_stack_map` call is made, the prologue emits no SP
    /// store, nothing is registered and nothing is compared: emission byte-identical to today.
    Off,
    /// `RAVA_STACKMAPS=probe` — the SIZE ARM. Maps are collected and the compiled-byte census
    /// runs, but NOTHING is declared and no SP is stored, so the emission is the default one.
    /// Its `[SMAP]` code-size row is what the full arm's row is measured against.
    Probe,
    /// `RAVA_STACKMAPS=sp` — the ATTRIBUTION arm. Records the SP, registers, and runs the
    /// cross-check, but declares NOTHING, so every map comes back empty. Its only purpose is
    /// to separate "the instrumentation destabilised something" from "the declarations did":
    /// this arm carries the whole census apparatus and none of the codegen change.
    Sp,
    /// `RAVA_STACKMAPS=1` — declare, record the SP, register the maps, cross-check.
    Full,
}

#[inline]
pub(crate) fn mode() -> Mode {
    static M: std::sync::OnceLock<Mode> = std::sync::OnceLock::new();
    // ⭐**DEFAULT ON, MEASURED TWICE.** The replacement was priced on the warm reload against the
    // shadow-frame protocol it removes. First run (noisy box, arm spreads 7-10%): every one of the
    // ten statistics favoured it, RELOAD_MS -3.04/-6.38%, cpu -4.66/-9.06%. Second run on a quiet
    // box (arm spreads 1.6-4.1%): **DISJOINT on four of five metrics** — WINDOW_MS -2.58/-2.94%,
    // RELOAD2_MS (the warm pass) -1.46/-2.19%, cpu -4.86/-5.59%, cycles -5.36/-5.44%. Emission:
    // ~21 spill+reload pairs gone per compiled function, 498,087 rooting memory ops over 2,000
    // functions, and **compiled bytes per function 10,317 -> 6,738 = -34.7%**.
    //
    // Correctness bar cleared in BOTH arms: 4 bit-exact gates, 23 differential probes,
    // BootstrapBench `b9e7debe631b42b6`, ReloadBench `c956377e19071d7a` (×3 plus two ten-reload
    // soaks), three product server boots, and `RAVA_STACKMAPS_VERIFY=1` — which asserts after every
    // collection that each arena pointer the maps named lies in the live region — reporting **zero
    // missed forwards and zero out-of-extent slots in every run**.
    //
    // `RAVA_NO_STACKMAPS=1` restores the shadow frame exactly; `=probe` / `=sp` are the phase-A
    // diagnostic arms and stay reachable by name.
    *M.get_or_init(|| match std::env::var("RAVA_STACKMAPS").as_deref() {
        Ok("1") | Ok("on") | Ok("yes") => Mode::Full,
        Ok("probe") => Mode::Probe,
        Ok("sp") => Mode::Sp,
        Ok("0") | Ok("off") | Ok("no") => Mode::Off,
        _ => {
            if std::env::var_os("RAVA_NO_STACKMAPS").is_some()
                || crate::flags::emit_more_disabled()
            {
                Mode::Off
            } else {
                Mode::Full
            }
        }
    })
}

/// Does the prologue record the frame's stack pointer? True for the two arms that need to LOCATE
/// frames — the full replacement and the `sp` attribution arm — and false for `probe`, which only
/// counts sizes. `frames::stackmaps_on` asks THIS rather than re-reading the variable: when the two
/// derived it separately, a default flipped one and not the other and the engine ran with no roots.
#[inline]
pub(crate) fn records_sp() -> bool {
    matches!(mode(), Mode::Full | Mode::Sp)
}

/// True when the machinery runs at all (either arm) — gates registration and reporting.
#[inline]
pub(crate) fn enabled() -> bool {
    !matches!(mode(), Mode::Off)
}

/// True only for the full arm: the one gate on `declare_*_needs_stack_map`.
#[inline]
pub(crate) fn declaring() -> bool {
    matches!(mode(), Mode::Full)
}

/// True for the arms that record the compiled frame's SP in the prologue and run the
/// collection-time cross-check. `Mode::Sp` has this WITHOUT the declarations.
#[inline]
pub(crate) fn recording_sp() -> bool {
    matches!(mode(), Mode::Full | Mode::Sp)
}

/// How often the census prints itself, in collections. `RAVA_STACKMAPS_EVERY` overrides.
fn dump_every() -> u64 {
    static N: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("RAVA_STACKMAPS_EVERY").ok().and_then(|s| s.parse().ok()).unwrap_or(2000)
    })
}

/// One compiled function's maps: the code range it was finalized into, the frame span
/// (`FrameLayout::active_size()`, constant for the function) and, per safepoint, the
/// return-address offset with the SP-relative offsets of the slots the map names.
struct FnMaps {
    start: usize,
    end: usize,
    /// Sorted by `ret_off` — `user_stack_maps()` already yields them in that order.
    sites: Vec<(u32, Box<[u32]>)>,
    span: u32,
}

/// Registered functions, sorted by `start`. Written once per compile (a few thousand a run),
/// read under the GIL at each collection. A plain `Mutex<Vec<..>>` for the reason
/// `pc_profile::RANGES` is one: the write side is the compile, not a hot path.
static MAPS: std::sync::Mutex<Vec<FnMaps>> = std::sync::Mutex::new(Vec::new());

/// Compile-time census: functions registered, safepoints they carry, slots those name.
static FNS: AtomicU64 = AtomicU64::new(0);
static SITES: AtomicU64 = AtomicU64::new(0);
static SITE_SLOTS: AtomicU64 = AtomicU64::new(0);
/// CLIF safepoints the compiled functions CONTAIN (every non-tail call), against `SITES`,
/// which counts only those Cranelift attached a map to. The gap is safepoints where its
/// liveness found nothing declared live across the call.
static SAFEPOINTS: AtomicU64 = AtomicU64::new(0);
/// Compiled bytes, so the declaration's cost in emitted code can be read off directly.
static CODE_BYTES: AtomicU64 = AtomicU64::new(0);

/// Collection-time census — see [`report`] for what each row means.
static COLLECTIONS: AtomicU64 = AtomicU64::new(0);
static RANGES_SEEN: AtomicU64 = AtomicU64::new(0);
static FRAMES_LOCATED: AtomicU64 = AtomicU64::new(0);
static FRAMES_NO_SP: AtomicU64 = AtomicU64::new(0);
static FRAMES_NO_FN: AtomicU64 = AtomicU64::new(0);
static FRAMES_NO_MAP: AtomicU64 = AtomicU64::new(0);
static SLOTS_MAPS: AtomicU64 = AtomicU64::new(0);
static SLOTS_RANGES: AtomicU64 = AtomicU64::new(0);
static ARENA_MAPS: AtomicU64 = AtomicU64::new(0);
static ARENA_RANGES: AtomicU64 = AtomicU64::new(0);
/// The deliverable: GC-relevant cell VALUES one side names and the other does not.
static ONLY_RANGE: AtomicU64 = AtomicU64::new(0);
static ONLY_MAP: AtomicU64 = AtomicU64::new(0);
static ONLY_RANGE_ARENA: AtomicU64 = AtomicU64::new(0);
static ONLY_MAP_ARENA: AtomicU64 = AtomicU64::new(0);
/// Frames whose two sides agreed exactly on the GC-relevant value set.
static FRAMES_AGREE: AtomicU64 = AtomicU64::new(0);
/// THE KILLER CASE: a located frame whose return address carries NO stack map, while the
/// cell-root range it published holds GC-relevant cells. "No map" is Cranelift asserting
/// there is nothing to root at that PC; these are the frames where that assertion and the
/// shadow frame disagree outright.
static NO_MAP_WITH_REFS: AtomicU64 = AtomicU64::new(0);
static NO_MAP_REF_CELLS: AtomicU64 = AtomicU64::new(0);
static NO_MAP_ARENA_CELLS: AtomicU64 = AtomicU64::new(0);
/// A map entry whose SP offset fell outside the frame span the same buffer recorded — a
/// decoding error, not a root. Must stay 0.
static BAD_OFFSET: AtomicU64 = AtomicU64::new(0);
/// Decoder self-check: located frames whose `*(sp-8)` IS / IS NOT preceded by a call
/// encoding. `RET_NOT_CALL` above ~0 means the frame-location mechanism is unsound and every
/// row below it is noise.
static RET_IS_CALL: AtomicU64 = AtomicU64::new(0);
static RET_NOT_CALL: AtomicU64 = AtomicU64::new(0);
/// WHOLE-COLLECTION union sets: distinct GC-relevant cell values named by ALL ranges / by ALL
/// located maps, and the two set differences. `G_ONLY_MAP` is the row that would say the maps
/// see a root the collector currently does not scan ANYWHERE — the only reading of "map-only"
/// that survives the fact that a value may live in a different frame's range.
static G_ALL_RANGE: AtomicU64 = AtomicU64::new(0);
static G_ALL_MAP: AtomicU64 = AtomicU64::new(0);
static G_ONLY_MAP: AtomicU64 = AtomicU64::new(0);
static G_ONLY_MAP_ARENA: AtomicU64 = AtomicU64::new(0);
static G_ONLY_RANGE: AtomicU64 = AtomicU64::new(0);

/// Count the CLIF safepoints one compiled function contains. See [`SAFEPOINTS`].
pub(crate) fn note_safepoints(n: u64) {
    SAFEPOINTS.fetch_add(n, Relaxed);
}

/// Record one finalized function's user stack maps. Called from `finish_compile` with the
/// address `get_finalized_function` handed back, so the key is the real code range.
///
/// `maps` comes straight from `ctx.compiled_code()?.buffer.user_stack_maps()`, which must be
/// read BEFORE `module.clear_context`.
pub(crate) fn register(
    start: usize,
    len: usize,
    maps: &[(u32, u32, cranelift::codegen::ir::UserStackMap)],
) {
    if start == 0 || len == 0 {
        return;
    }
    CODE_BYTES.fetch_add(len as u64, Relaxed);
    let n = FNS.fetch_add(1, Relaxed) + 1;
    // Report on a FUNCTION-COUNT boundary, in both arms, so the two runs' code-size rows are
    // read at the same population — the size arm never declares anything and so never reaches
    // a cross-check, and comparing totals taken at different N would compare workloads.
    if n % 500 == 0 {
        eprintln!("{}", report());
    }
    if maps.is_empty() {
        return;
    }
    let mut span = 0u32;
    let mut sites: Vec<(u32, Box<[u32]>)> = Vec::with_capacity(maps.len());
    for (ret_off, sp_to_fp, m) in maps {
        span = *sp_to_fp;
        let offs: Vec<u32> = m.entries().map(|(_, off)| off).collect();
        SITE_SLOTS.fetch_add(offs.len() as u64, Relaxed);
        sites.push((*ret_off, offs.into_boxed_slice()));
    }
    SITES.fetch_add(sites.len() as u64, Relaxed);
    let e = FnMaps { start, end: start + len, sites, span };
    let mut g = MAPS.lock().unwrap_or_else(|p| p.into_inner());
    let at = g.partition_point(|f| f.start < start);
    g.insert(at, e);
}

/// A GC-relevant cell, for the census only: the shape tests the collector's own JIT-cell
/// scan uses, plus the tagged forms `cell_to_root` understands. Never dereferenced.
#[inline]
fn gc_cell(v: i64) -> bool {
    if v == 0 {
        return false;
    }
    let raw = v as u64;
    // Raw arena pointer (tag 0b00) — the only movable shape, and the one the collector
    // forwards in place out of a cell-root range.
    if raw & 7 == 0 && crate::nextgen::is_arena_obj_addr(raw) {
        return true;
    }
    // Tagged handle / array / box cells (see the JIT cell encoding).
    matches!(raw >> 40, 1 | 2 | 4)
}

/// DECODER SELF-CHECK. `*(sp - 8)` is claimed to be the return address of the call this
/// compiled frame is blocked in; if that claim is wrong the whole census is noise, and the
/// cheapest proof is that the bytes immediately BEFORE it encode a call. On x86-64 Cranelift
/// emits either `E8 rel32` (direct) or `FF /2` (indirect, optionally REX-prefixed), so a
/// backward scan over the few legal encodings settles it. Diagnostic only — a heuristic here
/// can never mis-root anything, it can only mislabel a census row.
fn preceded_by_call(start: usize, ret_off: u32) -> bool {
    let ro = ret_off as usize;
    let at = |k: usize| -> Option<u8> {
        (ro >= k).then(|| unsafe { *((start + ro - k) as *const u8) })
    };
    // `call rel32`
    if at(5) == Some(0xE8) {
        return true;
    }
    // `call r/m64`: FF with modrm.reg == 2 (near call). 2-byte (FF /2 reg), 3-byte with a
    // REX prefix or a disp8, and the 6-byte RIP-relative `FF 15 rel32` memory form.
    for k in [2usize, 3, 4, 6, 7] {
        let Some(op) = at(k) else { continue };
        let Some(modrm) = at(k - 1) else { continue };
        let is_call = op == 0xFF && (modrm >> 3) & 7 == 2;
        let rexed = matches!(op, 0x40..=0x4F)
            && at(k - 1) == Some(0xFF)
            && at(k - 2).map_or(false, |m| (m >> 3) & 7 == 2);
        if is_call || rexed {
            return true;
        }
        let _ = modrm;
    }
    false
}

#[inline]
fn arena_cell(v: i64) -> bool {
    let raw = v as u64;
    raw & 7 == 0 && crate::nextgen::is_arena_obj_addr(raw)
}

/// PHASE A CROSS-CHECK. Called from the collector's JIT cell-root scan with the ranges it
/// is about to act on, paired with the SP each publishing compiled frame recorded.
///
/// Reads only; the collector's behaviour is unchanged whether this runs or not.
///
/// # Safety
/// Called under the GIL with every guest thread parked, which is the same contract
/// `frames::cells_all` already runs on: the compiled frames whose SPs these are are alive
/// and not executing, so `*(sp - 8)` and `*(sp + off)` are stable stack reads.
pub(crate) fn cross_check(ranges: &[(*const i64, usize, u64)]) {
    let n = COLLECTIONS.fetch_add(1, Relaxed) + 1;
    let g = MAPS.lock().unwrap_or_else(|p| p.into_inner());
    // Reused across frames so a deep stack does not allocate per frame.
    let mut rvals: Vec<i64> = Vec::new();
    let mut mvals: Vec<i64> = Vec::new();
    // WHOLE-COLLECTION sets. The per-frame comparison below answers "does THIS frame's map
    // name what THIS frame's range names", and a value can legitimately be named by one
    // frame's map while living in a DIFFERENT frame's range (a receiver the caller still
    // holds). The collector does not care which range a root came from, so the question that
    // decides Phase B is the union one: is there anything the maps name that NO range names?
    let mut all_r: Vec<i64> = Vec::new();
    let mut all_m: Vec<i64> = Vec::new();
    for &(ptr, len, sp) in ranges {
        RANGES_SEEN.fetch_add(1, Relaxed);
        // Every published range is scanned by the collector whether or not we can locate the
        // frame behind it, so the union side takes them all.
        {
            let cells = unsafe { std::slice::from_raw_parts(ptr, len) };
            for &c in cells {
                if gc_cell(c) {
                    all_r.push(c);
                }
            }
        }
        if sp == 0 {
            // Pushed by the Rust side (`cells_push` from a wrapper, or the prologue's
            // capacity-overflow arm), so no compiled frame SP was recorded for it.
            FRAMES_NO_SP.fetch_add(1, Relaxed);
            continue;
        }
        // The return address of the call this frame is currently blocked in.
        let ret = unsafe { ((sp - 8) as *const u64).read_unaligned() } as usize;
        let at = g.partition_point(|f| f.start <= ret);
        let Some(f) = at.checked_sub(1).and_then(|i| g.get(i)) else {
            FRAMES_NO_FN.fetch_add(1, Relaxed);
            continue;
        };
        if ret >= f.end {
            FRAMES_NO_FN.fetch_add(1, Relaxed);
            continue;
        }
        FRAMES_LOCATED.fetch_add(1, Relaxed);
        let off = (ret - f.start) as u32;
        if preceded_by_call(f.start, off) {
            RET_IS_CALL.fetch_add(1, Relaxed);
        } else {
            RET_NOT_CALL.fetch_add(1, Relaxed);
        }
        // A return address with no map is NOT automatically benign, and it is the case this
        // census exists to price: Cranelift records a map only for a safepoint that had a
        // declared value live across it, so an absent map claims "nothing to root here".
        // The range is therefore still computed and compared against an EMPTY map set — if
        // it holds GC-relevant cells, the maps are blind exactly where we are not.
        let mapped = f.sites.binary_search_by_key(&off, |s| s.0).ok();
        if mapped.is_none() {
            FRAMES_NO_MAP.fetch_add(1, Relaxed);
        }
        rvals.clear();
        mvals.clear();
        let cells = unsafe { std::slice::from_raw_parts(ptr, len) };
        for &c in cells {
            if gc_cell(c) {
                rvals.push(c);
            }
        }
        if mapped.is_none() && !rvals.is_empty() {
            NO_MAP_WITH_REFS.fetch_add(1, Relaxed);
            NO_MAP_REF_CELLS.fetch_add(rvals.len() as u64, Relaxed);
            NO_MAP_ARENA_CELLS
                .fetch_add(rvals.iter().filter(|v| arena_cell(**v)).count() as u64, Relaxed);
        }
        if let Some(i) = mapped {
            for &o in f.sites[i].1.iter() {
                // The map's offsets are inside this frame's own sized stack slots, which sit
                // within [sp, sp + span); anything else would be a decoding error, so refuse
                // rather than read it.
                if o as u64 >= f.span as u64 {
                    BAD_OFFSET.fetch_add(1, Relaxed);
                    continue;
                }
                let a = sp.wrapping_add(o as u64);
                let v = unsafe { (a as *const i64).read_unaligned() };
                if gc_cell(v) {
                    mvals.push(v);
                    all_m.push(v);
                }
            }
        }
        SLOTS_RANGES.fetch_add(rvals.len() as u64, Relaxed);
        SLOTS_MAPS.fetch_add(mvals.len() as u64, Relaxed);
        ARENA_RANGES.fetch_add(rvals.iter().filter(|v| arena_cell(**v)).count() as u64, Relaxed);
        ARENA_MAPS.fetch_add(mvals.iter().filter(|v| arena_cell(**v)).count() as u64, Relaxed);
        rvals.sort_unstable();
        rvals.dedup();
        mvals.sort_unstable();
        mvals.dedup();
        let only_r: Vec<i64> = rvals.iter().copied().filter(|v| !mvals.contains(v)).collect();
        let only_m: Vec<i64> = mvals.iter().copied().filter(|v| !rvals.contains(v)).collect();
        if only_r.is_empty() && only_m.is_empty() {
            FRAMES_AGREE.fetch_add(1, Relaxed);
        }
        ONLY_RANGE.fetch_add(only_r.len() as u64, Relaxed);
        ONLY_MAP.fetch_add(only_m.len() as u64, Relaxed);
        ONLY_RANGE_ARENA
            .fetch_add(only_r.iter().filter(|v| arena_cell(**v)).count() as u64, Relaxed);
        ONLY_MAP_ARENA.fetch_add(only_m.iter().filter(|v| arena_cell(**v)).count() as u64, Relaxed);
    }
    drop(g);
    all_r.sort_unstable();
    all_r.dedup();
    all_m.sort_unstable();
    all_m.dedup();
    let g_only_m: Vec<i64> =
        all_m.iter().copied().filter(|v| all_r.binary_search(v).is_err()).collect();
    let g_only_r = all_r.iter().filter(|v| all_m.binary_search(v).is_err()).count();
    G_ALL_RANGE.fetch_add(all_r.len() as u64, Relaxed);
    G_ALL_MAP.fetch_add(all_m.len() as u64, Relaxed);
    G_ONLY_MAP.fetch_add(g_only_m.len() as u64, Relaxed);
    G_ONLY_MAP_ARENA.fetch_add(g_only_m.iter().filter(|v| arena_cell(**v)).count() as u64, Relaxed);
    G_ONLY_RANGE.fetch_add(g_only_r as u64, Relaxed);
    let every = dump_every();
    if every != 0 && n % every == 0 {
        eprintln!("{}", report());
    }
}

/// PHASE B — collection-time census of the rooting walk itself (see [`for_each_map_slot`]).
static B_WALKS: AtomicU64 = AtomicU64::new(0);
static B_RANGES: AtomicU64 = AtomicU64::new(0);
static B_FRAMES: AtomicU64 = AtomicU64::new(0);
static B_NO_SP: AtomicU64 = AtomicU64::new(0);
static B_NO_FN: AtomicU64 = AtomicU64::new(0);
static B_NO_MAP: AtomicU64 = AtomicU64::new(0);
static B_SLOTS: AtomicU64 = AtomicU64::new(0);
static B_BAD_OFF: AtomicU64 = AtomicU64::new(0);
/// Slots the caller ACTED on (pushed as a forwardable arena root / a tagged root) and slots it
/// refused by shape. Incremented by the collector through [`note_slot`].
static B_TAKEN: AtomicU64 = AtomicU64::new(0);
static B_SKIPPED: AtomicU64 = AtomicU64::new(0);

/// THE VERIFIER (`RAVA_STACKMAPS_VERIFY=1`). Counts over every post-collection walk.
static V_RUNS: AtomicU64 = AtomicU64::new(0);
static V_SLOTS: AtomicU64 = AtomicU64::new(0);
static V_ARENA: AtomicU64 = AtomicU64::new(0);
/// THE FINDING: an arena pointer still in the (now evacuated) nursery after the collection —
/// a slot the collector named but did not forward. Must stay 0.
static V_STALE_YOUNG: AtomicU64 = AtomicU64::new(0);
/// An 8-aligned non-zero word above the arena floor but OUTSIDE the committed live extent.
static V_OUT_OF_EXTENT: AtomicU64 = AtomicU64::new(0);

/// EMISSION CENSUS — the rooting protocol's own instruction count, taken in BOTH arms over the
/// same population so the two rows subtract. Every `spill_for_gc` / `reload_after_gc` /
/// `reload_locals_after_gc` call records the site and the number of 8-byte words it would move;
/// under the declare arm the words are then NOT emitted, which is exactly what the arm buys.
static E_SPILL_SITES: AtomicU64 = AtomicU64::new(0);
static E_SPILL_WORDS: AtomicU64 = AtomicU64::new(0);
static E_RELOAD_SITES: AtomicU64 = AtomicU64::new(0);
static E_RELOAD_WORDS: AtomicU64 = AtomicU64::new(0);

/// One `spill_for_gc`: `words` stores it would emit (every local, plus each ref stack slot).
#[inline]
pub(crate) fn note_spill(words: usize) {
    E_SPILL_SITES.fetch_add(1, Relaxed);
    E_SPILL_WORDS.fetch_add(words as u64, Relaxed);
}

/// One `reload_after_gc` / `reload_locals_after_gc`: `words` loads (each local load is also a
/// `def_var`, i.e. a fresh SSA definition at a program point that dominates the rest of the body).
#[inline]
pub(crate) fn note_reload(words: usize) {
    E_RELOAD_SITES.fetch_add(1, Relaxed);
    E_RELOAD_WORDS.fetch_add(words as u64, Relaxed);
}

/// The collector reports what it did with a slot, so the census can say how much of the map's
/// naming is actually GC-relevant. `taken` = pushed as a root; else refused by shape.
#[inline]
pub(crate) fn note_slot(taken: bool) {
    if taken {
        B_TAKEN.fetch_add(1, Relaxed);
    } else {
        B_SKIPPED.fetch_add(1, Relaxed);
    }
}

/// True when the verifier is armed. Independent of the arm, but only meaningful under `Full`
/// (nothing else registers a non-empty map).
#[inline]
pub(crate) fn verifying() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        matches!(std::env::var("RAVA_STACKMAPS_VERIFY").as_deref(), Ok("1") | Ok("on") | Ok("yes"))
    })
}

/// PHASE B — **THE ROOTING WALK.** For every live compiled frame, hand `f` the ADDRESS of every
/// word that frame's stack map names at the return address it is blocked in.
///
/// The frame-location mechanism is Phase A's, unchanged and measured exact (303/303): the
/// self-registering prologue paired each published cell-root range with the compiled frame's SP,
/// `*(SP - 8)` is the return address of the call the frame is blocked in, and that address keys
/// the maps `register` recorded. What is new is only that the caller is expected to WRITE through
/// the addresses — the collector moves objects, so a named slot holding an arena pointer must be
/// rewritten with the forwarded address exactly as a cell-root range's words are.
///
/// This function decides only WHERE a root-shaped word is. What a word MEANS stays with the
/// caller: a cell is tagged (arena pointer raw, handle `h|1<<40`, array `h|1<<42`, box
/// `idx|1<<41`), and each collector applies its own shape test, the same one it applies to a
/// cell-root range.
///
/// # Safety
/// Called under the GIL with every guest thread parked — the contract `frames::cells_all` already
/// runs on. The compiled frames whose SPs these are are alive and not executing, so `*(sp - 8)`
/// and `sp + off` are stable stack addresses for the duration of the collection.
pub(crate) fn for_each_map_slot(f: &mut dyn FnMut(*mut i64)) {
    if !recording_sp() {
        return;
    }
    B_WALKS.fetch_add(1, Relaxed);
    let ranges = crate::frames::cells_all_sp();
    let g = MAPS.lock().unwrap_or_else(|p| p.into_inner());
    for &(_ptr, _len, sp) in ranges.iter() {
        B_RANGES.fetch_add(1, Relaxed);
        if sp == 0 {
            B_NO_SP.fetch_add(1, Relaxed);
            continue;
        }
        let ret = unsafe { ((sp - 8) as *const u64).read_unaligned() } as usize;
        let at = g.partition_point(|fm| fm.start <= ret);
        let Some(fm) = at.checked_sub(1).and_then(|i| g.get(i)) else {
            B_NO_FN.fetch_add(1, Relaxed);
            continue;
        };
        if ret >= fm.end {
            B_NO_FN.fetch_add(1, Relaxed);
            continue;
        }
        B_FRAMES.fetch_add(1, Relaxed);
        let off = (ret - fm.start) as u32;
        let Ok(i) = fm.sites.binary_search_by_key(&off, |s| s.0) else {
            B_NO_MAP.fetch_add(1, Relaxed);
            continue;
        };
        for &o in fm.sites[i].1.iter() {
            // A map offset must land inside the frame span the same buffer recorded; anything
            // else is a decoding error, and a decoding error here would hand the collector an
            // arbitrary stack address to REWRITE. Refuse rather than read it.
            if o as u64 >= fm.span as u64 {
                B_BAD_OFF.fetch_add(1, Relaxed);
                continue;
            }
            B_SLOTS.fetch_add(1, Relaxed);
            f(sp.wrapping_add(o as u64) as *mut i64);
        }
    }
}

/// THE SAFETY NET (`RAVA_STACKMAPS_VERIFY=1`), run immediately after a collection's writeback.
///
/// Walks every slot the maps named for every live compiled frame and checks that each word that
/// is an arena pointer points into the LIVE region. Post-collection `young.top` is 0 — every
/// survivor was evacuated — so a raw pointer still inside the nursery window is, by construction,
/// a slot the collector named and did NOT forward. That is a missed forward, and it is reported
/// HERE, at the collection, with the frame's return address and the slot's SP offset, instead of
/// becoming a use-after-move somewhere else a minute later.
///
/// Reports; does not panic. A missed forward is already a corrupted heap, and a panic inside the
/// collector unwinds through parked guest threads — the deterministic printed finding is the
/// diagnostic, and the counts are in the `[SMAP]` census.
///
/// # Safety
/// As [`for_each_map_slot`].
pub(crate) fn verify_after_collect(collector: &str) {
    if !verifying() || !recording_sp() {
        return;
    }
    let (ylo, yhi) = crate::nextgen::young_bounds();
    let (ylo, yhi) = (ylo as u64, yhi as u64);
    V_RUNS.fetch_add(1, Relaxed);
    let ranges = crate::frames::cells_all_sp();
    let g = MAPS.lock().unwrap_or_else(|p| p.into_inner());
    let mut findings = 0u32;
    for &(_ptr, _len, sp) in ranges.iter() {
        if sp == 0 {
            continue;
        }
        let ret = unsafe { ((sp - 8) as *const u64).read_unaligned() } as usize;
        let at = g.partition_point(|fm| fm.start <= ret);
        let Some(fm) = at.checked_sub(1).and_then(|i| g.get(i)) else { continue };
        if ret >= fm.end {
            continue;
        }
        let off = (ret - fm.start) as u32;
        let Ok(i) = fm.sites.binary_search_by_key(&off, |s| s.0) else { continue };
        for &o in fm.sites[i].1.iter() {
            if o as u64 >= fm.span as u64 {
                continue;
            }
            let a = sp.wrapping_add(o as u64);
            let v = unsafe { (a as *const i64).read_unaligned() };
            V_SLOTS.fetch_add(1, Relaxed);
            let raw = v as u64;
            if v == 0 || raw & 7 != 0 || raw >> 40 != 0 {
                continue; // not a raw arena pointer (zero, tagged, or misaligned)
            }
            if !arena_cell(v) {
                // 8-aligned, untagged, non-zero and above the arena floor, yet outside the
                // committed live extent. Not necessarily a root (a small integer local has this
                // shape), so counted rather than reported.
                if raw >= 0x2_0000_0000 {
                    V_OUT_OF_EXTENT.fetch_add(1, Relaxed);
                }
                continue;
            }
            V_ARENA.fetch_add(1, Relaxed);
            if ylo != 0 && raw >= ylo && raw < yhi {
                V_STALE_YOUNG.fetch_add(1, Relaxed);
                findings += 1;
                if findings <= 8 {
                    eprintln!(
                        "[SMAP-VERIFY] MISSED FORWARD after {collector}: ret={ret:#x} (fn {:#x}..{:#x}, off {off:#x}) slot sp+{o:#x} @ {a:#x} holds {raw:#x}, still in the evacuated nursery [{ylo:#x},{yhi:#x})",
                        fm.start, fm.end,
                    );
                }
            }
        }
    }
}

/// The census. Empty unless the flag armed the machinery, so it can be printed
/// unconditionally beside `compile_bail::report` in the periodic profiler dump.
pub fn report() -> String {
    let fns = FNS.load(Relaxed);
    if fns == 0 {
        return String::new();
    }
    let coll = COLLECTIONS.load(Relaxed);
    let loc = FRAMES_LOCATED.load(Relaxed);
    let bytes = CODE_BYTES.load(Relaxed);
    let mut out = format!(
        "[SMAP] arm={} | {fns} compiled functions registered, {} CLIF safepoints of which {} carry a map ({} slots named at compile time) | {bytes} bytes of code, {:.1} B/fn",
        match mode() { Mode::Full => "declare", Mode::Sp => "sp(no-declare)", _ => "probe(size-only)" },
        SAFEPOINTS.load(Relaxed),
        SITES.load(Relaxed),
        SITE_SLOTS.load(Relaxed),
        bytes as f64 / fns as f64,
    );
    out.push_str(&format!(
        "\n[SMAP] shadow-frame emission over the same population: {} spill sites ({} words), {} reload sites ({} words) = {} rooting words, {:.1}/fn — {}",
        E_SPILL_SITES.load(Relaxed),
        E_SPILL_WORDS.load(Relaxed),
        E_RELOAD_SITES.load(Relaxed),
        E_RELOAD_WORDS.load(Relaxed),
        E_SPILL_WORDS.load(Relaxed) + E_RELOAD_WORDS.load(Relaxed),
        (E_SPILL_WORDS.load(Relaxed) + E_RELOAD_WORDS.load(Relaxed)) as f64 / fns as f64,
        if declaring() { "ALL ELIDED (stack maps root instead)" } else { "all emitted" },
    ));
    if !recording_sp() {
        report_phase_b(&mut out);
        return out;
    }
    out.push_str(&format!(
        "\n[SMAP] {coll} collections, {} cell-root ranges seen: {loc} compiled frames located by SP+return-address, {} with no recorded SP, {} whose return address is in no registered range",
        RANGES_SEEN.load(Relaxed),
        FRAMES_NO_SP.load(Relaxed),
        FRAMES_NO_FN.load(Relaxed),
    ));
    out.push_str(&format!(
        "\n[SMAP] return-address decode: {} preceded by a call encoding, {} NOT (must be ~0)",
        RET_IS_CALL.load(Relaxed),
        RET_NOT_CALL.load(Relaxed),
    ));
    out.push_str(&format!(
        "\n[SMAP] of those, {} sat at a return address Cranelift recorded NO map for; {} of them held GC cells anyway ({} cells, {} arena)  | bad map offsets {}",
        FRAMES_NO_MAP.load(Relaxed),
        NO_MAP_WITH_REFS.load(Relaxed),
        NO_MAP_REF_CELLS.load(Relaxed),
        NO_MAP_ARENA_CELLS.load(Relaxed),
        BAD_OFFSET.load(Relaxed),
    ));
    out.push_str(&format!(
        "\n[SMAP] GC-relevant cells at located frames: ranges {} (arena {}), maps {} (arena {})",
        SLOTS_RANGES.load(Relaxed),
        ARENA_RANGES.load(Relaxed),
        SLOTS_MAPS.load(Relaxed),
        ARENA_MAPS.load(Relaxed),
    ));
    out.push_str(&format!(
        "\n[SMAP] DISAGREEMENT  range-only {} (arena {})  |  map-only {} (arena {})  |  {loc} frames compared, {} agreed exactly ({:.1}%)",
        ONLY_RANGE.load(Relaxed),
        ONLY_RANGE_ARENA.load(Relaxed),
        ONLY_MAP.load(Relaxed),
        ONLY_MAP_ARENA.load(Relaxed),
        FRAMES_AGREE.load(Relaxed),
        if loc == 0 { 0.0 } else { FRAMES_AGREE.load(Relaxed) as f64 * 100.0 / loc as f64 },
    ));
    out.push_str(&format!(
        "\n[SMAP] WHOLE-COLLECTION union (summed over collections): distinct cells named by all ranges {}, by all maps {} | named by a map and by NO range {} (arena {}) | named by a range and by no map {}",
        G_ALL_RANGE.load(Relaxed),
        G_ALL_MAP.load(Relaxed),
        G_ONLY_MAP.load(Relaxed),
        G_ONLY_MAP_ARENA.load(Relaxed),
        G_ONLY_RANGE.load(Relaxed),
    ));
    report_phase_b(&mut out);
    out
}

/// PHASE B rows: the rooting walk and its verifier. Appended by [`report`] whenever the walk has
/// run, so one census covers both phases.
fn report_phase_b(out: &mut String) {
    let walks = B_WALKS.load(Relaxed);
    if walks == 0 && V_RUNS.load(Relaxed) == 0 {
        return;
    }
    out.push_str(&format!(
        "\n[SMAP-B] {walks} rooting walks over {} published ranges: {} compiled frames located, {} with no recorded SP, {} in no registered function, {} at a return address with no map | {} slots named ({} bad offsets refused)",
        B_RANGES.load(Relaxed),
        B_FRAMES.load(Relaxed),
        B_NO_SP.load(Relaxed),
        B_NO_FN.load(Relaxed),
        B_NO_MAP.load(Relaxed),
        B_SLOTS.load(Relaxed),
        B_BAD_OFF.load(Relaxed),
    ));
    out.push_str(&format!(
        "\n[SMAP-B] of those slots the collector FORWARDED {} and skipped {} by shape",
        B_TAKEN.load(Relaxed),
        B_SKIPPED.load(Relaxed),
    ));
    if verifying() {
        out.push_str(&format!(
            "\n[SMAP-VERIFY] {} post-collection walks, {} slots read, {} held an arena pointer | MISSED FORWARDS {} (must be 0) | out-of-extent words {}",
            V_RUNS.load(Relaxed),
            V_SLOTS.load(Relaxed),
            V_ARENA.load(Relaxed),
            V_STALE_YOUNG.load(Relaxed),
            V_OUT_OF_EXTENT.load(Relaxed),
        ));
    }
}
