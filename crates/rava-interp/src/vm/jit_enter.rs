//! Entering compiled code: marshal the locals into rooted cells, call, and decode the result.
//!
//! The cell buffer is sized `max_locals + max_stack` because compiled code spills live
//! references above the locals for the collector to find; the box table is truncated back on
//! every exit path.
//!
//! ── ENTRY3: WHAT THE PLAN LETS THE ENTRY *SKIP* (kill switch `RAVA_NO_FAST_ENTER3=1`) ──
//!
//! `Vm::jit_enter` runs ~7.6 M times a boot and is 1.85% of the host callbacks out of
//! compiled code. Its per-entry cost is not one big thing; it is a queue of THREAD-LOCAL
//! RESOLUTIONS of two per-thread structs, each a `LocalKey::with` lazy-init check plus a
//! `RefCell` borrow flag. The `EntryPlan` already removed the descriptor walk. This round
//! uses it to remove the redundant ones:
//!
//!   1. The plan TABLE itself left the thread. It was a per-thread
//!      `RefCell<Vec<Option<Box<EntryPlan>>>>` — one crossing, one borrow, one length test
//!      and two bounds-checked indexes on EVERY entry — and a plan is immutable and
//!      identical on every thread, so it is now a process-global publish-once pointer array
//!      read with a single `Acquire` load (`jit::abi::PLANS`).
//!   2. `set_current_vm` re-resolved `frames::ctx_addr()` to stamp `Vm::jit_ctx_slot`. This
//!      function resolved that same address ~90 lines earlier into `ctx`. Both the publish
//!      and the restore now store the address already in hand.
//!   3. A plan whose `ref_mask` is entirely zero marshals with no per-slot ref-vs-prim
//!      branch and never reaches `box_ref` (see `jit::marshal_cells`).
//!
//! Every one of those has a COLD ARM that is the code it replaces, taken whenever there is
//! no plan (an unpublished `mid`, or `RAVA_NO_ENTRY_PLAN=1`) or the kill switch is set. The
//! switch is read ONCE, at plan-build time, and rides in `EntryPlan::fast3` — a byte in a
//! cache line the entry has already loaded — because a per-callback gate READ is exactly
//! the cost the previous round went and removed three of.
//!
//! ⛔TWO THINGS THIS ROUND DELIBERATELY DID NOT BUILD, both because the work was already
//! absent: a primitive-only return never inspected the descriptor (`unpack_ret` matches on
//! the resolved `JT`), and a callee that boxed nothing never truncates the box table
//! (`box_truncate_via`'s mirror test decides that dynamically, for a wider population than
//! a static leaf proof could). Both are documented at their sites.
//!
//! ── CROSSING2: THE ENTRY DERIVED "IS THIS PROCESS INSTRUMENTED?" FOUR TIMES, IN FOUR
//!    DIFFERENT SPELLINGS (kill switch `RAVA_NO_FAST_CROSSING2=1`) ──────────────────────
//!
//! Reading this function end to end and listing the FACTS it derives per entry turns up ONE
//! fact answered over and over. "No instrument is armed in this process" was asked as:
//!
//!   1. `alloc_sites::RegionGuard::enter` → `regions_armed()` — TWO relaxed atomic loads;
//!   2. `vecgrow::note` → `vecgrow::enabled()` — a `OnceLock` acquire load;
//!   3. `jit_sentinel_enabled()` — a `OnceLock` acquire load;
//!   4. `prof::enabled()` — a `OnceLock` acquire load.
//!
//! Four derivations of one process constant, on a path a product boot takes ~7.6 M times, all
//! answering `false` in every production run because every one of them comes from an
//! environment variable read once and never written again. A fifth process constant,
//! `jit::fast_stub_off()`, is read here for the `fast` local — a BEHAVIOUR gate, so it needs
//! its own answer rather than a place in the disjunction.
//!
//! [`enter_gates`] derives all five ONCE PER THREAD into one plain `Cell<u8>` — the shape
//! `vm/jit_call_body.rs`'s `seam3_gates`, `opcodes::gates::op_gates` and `vmframe::frame_gates`
//! already use, and the one combination `std` lowers to a bare TLS-slot load with no
//! `LocalKey::with` call, no lazy-init state word and no destructor registration. **The cost
//! removed is not the four `mov`s, it is the four BARRIERS**: an acquire load is an
//! optimisation barrier LLVM may not hoist, fold or schedule across, which is why the other
//! side of this same crossing collapsed its eight instrument gates into one cached word.
//!
//! ⚠**[`EG_INSTR`] MAY ONLY REJECT.** Clear ⇒ a PROOF that all four gates are false for the
//! rest of the process, so each site's block is dead and skipping it is the answer that site
//! would have computed. Set ⇒ every site runs its own original gate, in its own order,
//! unchanged. A `debug_assert` re-derives the disjunction live on every entry in a debug build
//! and fails if the cached word ever disagrees.
//!
//! COLD ARM: [`EG_LIVE`] — `RAVA_NO_FAST_CROSSING2=1` or `RAVA_NO_EMIT_MORE=1` — makes every
//! site reach its own gate from its own position, exactly as it did before this round.
//!
//! ── HR-UNWIND: THE `_NLG_Return2` ROW IS NOT AN UNWIND (kill switch `RAVA_NO_HR_UNWIND=1`) ──
//!
//! Every host profile of a boot since 2026-07-27 carries a row `_NLG_Return2` at 1.1-1.9% and
//! labels it "MSVC unwind helper", which would mean something on the compiled entry or call path
//! installs an SEH frame per call. Nothing does: neither this file nor `jit_call_body.rs` has a
//! `catch_unwind`, `SeamFrame` is a plain `Drop` (a cleanup funclet runs only under a real
//! panic), and every `catch_unwind` in the crate brackets a TASK, not a call.
//!
//! The row is a SYMBOLIZER ARTEFACT. `rava.exe` links the DLL CRT (`VCRUNTIME140.dll`), the
//! sampler's symbol path is the exe directory only, so that module resolves by nearest preceding
//! EXPORT. Read off the DLL itself (14.51.36247.0, export table + `.pdata` + the load-config
//! function-override table):
//!
//!   * `__NLG_Return2` (RVA 0x1bc30) is a single `ret`. The 0x1370 bytes after it, up to the next
//!     export `__report_gsfailure`, are the `rep movsb` / SSE / AVX2 / AVX-512 COPY bodies.
//!   * The `memcpy`/`memmove` thunks (`fothk` section) are retargeted BY THE LOADER, per CPU
//!     feature, to 0x1c010 / 0x1c440 / 0x1cb40 / 0x1c890 / 0x1bd60 — every one inside that gap.
//!     ⇒ **`_NLG_Return2` = the loader-selected `memcpy` body.**
//!   * `memset` is retargeted to 0x1e580 / 0x1e190 and `memcmp` to 0x1e490 / 0x1e3b0 — inside
//!     the `memmove` export's gap. ⇒ **the `memmove` row = `memset` + `memcmp`**, which is why
//!     `memset` has never once appeared in a rava profile.
//!
//! So the population behind the row is host callbacks COPYING and ZEROING, not unwinding, and
//! the member of that family this file owns is the cell-buffer zero: `write_bytes(p, 0, total)`
//! with a run-time `total` is a `memset` libcall (IAT `call` → thunk `jmp` → size-dispatch body)
//! on every compiled entry. Zeroing the whole 32-cell inline array instead is a CONSTANT-length
//! 256-byte store that LLVM lowers to eight vector stores with no call at all, and the slice
//! handed out — `[..total]`, all zero — is byte-identical. Counted in [`hr_unwind`], reported as
//! `[HR-UNWIND]` from the periodic dump. Nothing in `jit_call_body.rs` copies or zeroes per
//! crossing (the pooled operand buffer keeps its capacity; the frame is built by field stores),
//! so that file is unchanged.

use crate::*;
use crate::class_loader::LoadedClass;
use std::sync::Arc;

/// `jit_stats` bookkeeping (`RAVA_TRACE_JIT_STATS`): a `String` allocation and a `BTreeMap`
/// probe. OFF in every production boot, so it is pushed out of line — inline it forced the
/// map's key temporary and the `to_string` call's spill slots into the frame of a function
/// entered ~7.6 M times.
#[cold]
#[inline(never)]
pub(super) fn stat_note_run(vm: &mut Vm, k: &str) {
    vm.jit_stats.entry(k.to_string()).or_default().compiled_runs += 1;
}

/// `RAVA_PROF` method crumb. The `format!` is the whole reason this is out of line: building
/// `fmt::Arguments` plus a `String` needs a dozen stack slots that the compiler would
/// otherwise reserve in `jit_enter`'s prologue on EVERY entry, profiler armed or not.
#[cold]
#[inline(never)]
pub(super) fn prof_crumb_enter(class: &str, name: &str) -> u32 {
    crate::prof::crumb_enter(&format!("{class}.{name}"))
}

/// `RAVA_JIT_SENTINEL` culprit report. Latched to fire once; entirely `format!` and I/O.
#[cold]
#[inline(never)]
pub(super) fn sentinel_report(
    class: &Arc<LoadedClass>,
    name: &str,
    descriptor: &str,
    locals: &[Value],
    c0: usize,
    c1: usize,
    h1: u32,
) {
    if JIT_SENTINEL_FIRED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let recv = locals
        .first()
        .map(|v| match v {
            Value::Ref(h) => format!("Ref({h})"),
            Value::Obj(p) => format!("Obj@{:#x}", p.addr()),
            other => format!("{other:?}"),
        })
        .unwrap_or_else(|| "<no-recv>".to_string());
    eprintln!(
        "[JIT-SENTINEL] CORRUPTER = {}.{}{} (compiled) corrupted class of handle={h1} (corrupt {c0}->{c1}) receiver={recv}",
        class.file.this_class, name, descriptor
    );
    use std::io::Write as _;
    let _ = std::io::stderr().flush();
}

/// Convert an arena reference array only at the compiled-to-interpreter return boundary.
///
/// `ArrayRef::from_value` is the representation proof: a raw `Obj` is admitted only when its
/// exact CID metadata says it is a reference array.  Other return values are left byte-for-byte
/// in their existing representation, including legacy `Array` handles and nulls.
#[inline]
pub(super) fn materialize_arena_array_return(vm: &mut Vm, value: Value) -> Value {
    if !matches!(&value, Value::Obj(_)) {
        return value;
    }
    let Some(array) = crate::heap::ArrayRef::from_value(&value) else {
        return value;
    };
    array
        .materialize(&mut vm.heap)
        .map(Value::Array)
        .unwrap_or(value)
}


// ── THE ENTRY'S GATE WORD (kill switch `RAVA_NO_FAST_CROSSING2=1`) ────────────────────────
//
// See the module header for the finding. Deliberately the same shape as
// `vm/jit_call_body.rs`'s `SEAM3_GATES`, `vmframe::FRAME_GATES` and `opcodes::gates::OP_GATES`:
// a `Cell<u8>` with a `const` initialiser and NO destructor, which is the one combination `std`
// lowers to a bare `#[thread_local]` slot (on windows-msvc: the `_tls_index` load, the
// `gs:[0x58]` TEB read and a `movzx`). No `LocalKey::with` call, no lazy-init state word, no
// optimisation barrier.

/// Set on every resolved word, so the byte is never 0 once this thread has asked.
const EG_RESOLVED: u8 = 1 << 0;
/// The kill switch is engaged: distrust every other bit and let each site reach its own gate,
/// from its own position and in its own order, exactly as it did before this round.
pub(super) const EG_LIVE: u8 = 1 << 1;
/// **IS ANY INSTRUMENT IN THIS FUNCTION WIRED AT ALL?** The disjunction of the four gates that
/// were four spellings of the same question — `alloc_sites::regions_armed`, `vecgrow::enabled`,
/// `jit_sentinel_enabled` and `prof::enabled`. Clear is a PROOF that all four are false for the
/// rest of the process; set runs every one of them unchanged.
pub(super) const EG_INSTR: u8 = 1 << 2;
/// `jit::fast_stub_off()` (`RAVA_NO_FAST_STUB`) — the entry's one BEHAVIOUR gate, so it gets its
/// own bit rather than a place in the disjunction: a disjunction cannot answer two questions.
pub(super) const EG_FAST_STUB_OFF: u8 = 1 << 3;
/// **HR-UNWIND (`RAVA_NO_HR_UNWIND=1`): the inline cell buffer is zeroed at its constant full
/// length instead of through a run-time-length `memset` call.** See the module header.
///
/// Resolved OUTSIDE the [`EG_LIVE`] early return below, deliberately — the `S3_CROSS2` discipline
/// in `jit_call_body.rs`: this is a different round with a different switch, and
/// `RAVA_NO_FAST_CROSSING2` must not silently disable it. Its own switch (or the block switch
/// `RAVA_NO_EMIT_MORE`) is the only thing that does, and clear restores the historic
/// `write_bytes(p, 0, total)` verbatim.
pub(super) const EG_HR_UNWIND: u8 = 1 << 4;

thread_local! {
    /// This thread's resolved entry gates. `Cell<u8>`, `const`-initialised, no destructor.
    static ENTER_GATES: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

/// One plain per-thread byte. Never 0 on return.
#[inline(always)]
pub(super) fn enter_gates() -> u8 {
    let g = ENTER_GATES.get();
    let g = if g != 0 { g } else { enter_gates_cold() };
    // ⚠**THE PREFILTER IS CHECKED AGAINST THE PREDICATE IT REPLACES, ON EVERY ENTRY.** The
    // cached disjunction may only REJECT, so the failure that matters is a bit that reads clear
    // while one of the four gates has since become true. Every one of them is derived from an
    // environment variable read once — and `regions_armed()`'s two cells, which are NOT
    // `OnceLock`s, are covered by `pc_profile::enabled()` riding the same disjunction (see
    // `enter_gates_cold`). Debug-only, so the production path is unchanged.
    debug_assert!(
        g & (EG_LIVE | EG_INSTR) != 0 || !live_instr(),
        "an instrument armed after enter_gates cached a clear EG_INSTR"
    );
    debug_assert!(
        g & EG_LIVE != 0 || (g & EG_FAST_STUB_OFF != 0) == jit::fast_stub_off(),
        "EG_FAST_STUB_OFF disagrees with jit::fast_stub_off()"
    );
    g
}

/// The live disjunction, in the order the four sites used to read it. The ONLY reader under the
/// default is the once-per-thread fill below; the `debug_assert` above is the other.
#[inline]
fn live_instr() -> bool {
    // ⚠**LATE ARMING IS THE ONE HAZARD, AND IT IS COVERED BY CONSTRUCTION** — the identical
    // argument `seam_slow_gates` writes on the other side of this crossing. `regions_armed()`
    // reads two cells that are not `OnceLock`s: `RAVA_ALLOC_SIZES` is stored from `main` before
    // any guest code runs, and the region-time flag is set by `pc_profile::start_sampler_if_on`,
    // which the interpreter calls lazily — but only ever `if pc_profile::enabled()`. Including
    // that process constant here makes the late store unreachable while this answer is false.
    crate::alloc_sites::regions_armed()
        || crate::pc_profile::enabled()
        || crate::vecgrow::enabled()
        || jit_sentinel_enabled()
        || crate::prof::enabled()
}

/// First touch on this thread: read the process constants through their `OnceLock`s — the ONLY
/// place that still happens under the default — and cache the answers. Once per thread, so the
/// environment lookups behind them stay exactly as many as they were.
#[cold]
#[inline(never)]
fn enter_gates_cold() -> u8 {
    static LIVE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let live = *LIVE.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_CROSSING2").is_some() || crate::flags::emit_more_disabled()
    });
    let mut g = EG_RESOLVED;
    // HR-UNWIND's bit is resolved BEFORE the `EG_LIVE` early return: it is a later round with its
    // own kill switch, and `RAVA_NO_FAST_CROSSING2` restoring THIS round's shape must not also
    // revert one that is independent of it.
    if hr_unwind::enabled() {
        g |= EG_HR_UNWIND;
    }
    if live {
        // Resolve NOTHING else: with the switch engaged every site must reach its own gate from
        // its own position, in its own order, exactly as before.
        g |= EG_LIVE;
        ENTER_GATES.set(g);
        return g;
    }
    if live_instr() {
        g |= EG_INSTR;
    }
    if jit::fast_stub_off() {
        g |= EG_FAST_STUB_OFF;
    }
    ENTER_GATES.set(g);
    g
}

/// Read one bit of an ALREADY-LOADED word. `live` is the historic `OnceLock` reader, called only
/// under the kill switch — the `s3_bit`/`op_gate_of` discipline, so a caller that needs several
/// gates takes one load rather than several acquire loads.
#[inline(always)]
pub(super) fn eg_bit(g: u8, bit: u8, live: fn() -> bool) -> bool {
    if g & EG_LIVE != 0 { live() } else { g & bit != 0 }
}

/// HR-UNWIND: the switch, the census and the dump line. See the module header for the finding.
pub(crate) mod hr_unwind {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    /// `RAVA_NO_HR_UNWIND` unset and the block switch off. Read ONCE per process; the per-entry
    /// path reads the bit `enter_gates_cold` derived from it, never this.
    pub(crate) fn enabled() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| {
            std::env::var_os("RAVA_NO_HR_UNWIND").is_none() && !crate::flags::emit_more_disabled()
        })
    }

    /// Entries whose inline cell buffer was zeroed by the constant-length store — the population
    /// served. Batched per thread, published every [`BATCH`] entries: never an atomic RMW on a
    /// per-entry path. A dump therefore under-reads by at most `BATCH - 1` per thread.
    static SERVED: AtomicU64 = AtomicU64::new(0);
    const BATCH: u32 = 1024;

    thread_local! {
        /// Unpublished entries on this thread. `Cell<u32>`, `const`-initialised, no destructor —
        /// the bare-TLS-slot shape `ENTER_GATES` uses.
        static PENDING: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }

    #[inline(always)]
    pub(crate) fn note() {
        let n = PENDING.get().wrapping_add(1);
        if n == BATCH {
            SERVED.fetch_add(BATCH as u64, Relaxed);
            PENDING.set(0);
        } else {
            PENDING.set(n);
        }
    }

    /// The `[HR-UNWIND]` dump line. Empty until an entry has been served, so a run that never
    /// enters compiled code (or runs with the switch off and never counts) prints nothing.
    pub(crate) fn report() -> String {
        let served = SERVED.load(Relaxed);
        if served == 0 {
            return String::new();
        }
        format!(
            "[HR-UNWIND] compiled entries whose cell buffer was zeroed by inline stores instead of a memset libcall: {served} (±1023 per thread) | profile row `_NLG_Return2` is vcruntime140's loader-selected memcpy body, NOT an unwind; row `memmove` is memset+memcmp (see vm/jit_enter.rs header)"
        )
    }
}

pub(crate) use hr_unwind::report as hr_unwind_report;
