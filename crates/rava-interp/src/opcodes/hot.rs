//! The hot opcode subset, in a function small enough to have registers.
//!
//! `opcodes::run::run` is one 57 KB function holding all 98 opcode arms. A disassembly of the
//! addresses the PC profiler ranked showed what that costs: every `Value` move goes through a
//! stack temporary (`vmovups xmm0,[rbp+630h]` / `vmovups [rcx+rdx*8],xmm0`) and even the
//! safepoint countdown is decremented IN MEMORY (`dec dword ptr [rbp+3E4h]`). With that much
//! live state in one frame there are no registers left for the two pointers and one counter an
//! interpreter actually runs on.
//!
//! So the arms that need nothing but the frame — constants, loads, stores, stack shuffles,
//! arithmetic, conversions, comparisons, forward branches — moved HERE, VERBATIM. They are not a
//! second implementation: `run` no longer has them. What stays there is everything that needs the
//! wider machine (ldc, arrays, fields, calls, returns, switches, exceptions) and every case this
//! function deliberately hands back:
//!
//!   * a BACKWARD branch, so `run` keeps its back-edge probes and its fork detectors, and so the
//!     safepoint still falls on the edge every loop must take;
//!   * integer division and remainder, which throw;
//!   * an exhausted op budget, so the checkpoint stays exactly as often as it was.
//!
//! ⭐**`&SharedVm`** — the hot loop is the dispatcher's last tracer bullet, the same shape as the
//! invoke*/indy families: the function runs on the plain shared reference and everything it reads
//! is `&self`-safe (`trace.big`, `heap.array_get_bounded`, `getfield_value`/`putfield_value`,
//! `trace_big_local`). The one exclusivity left is `ldc` — a Class-literal constant allocates the
//! mirror and INSERTS into `class_objects` — so the two `ldc` arms take it through the file's
//! `vm_mut` below, exactly as `indy.rs` spells its linkage `ldc`s.


/// The transitional `&SharedVm -> &mut Vm` escape for the hot loop's genuinely-`&mut` callee —
/// the SAME consolidated cast the invoke*/indy families spell (see `invoke.rs::vm_mut`). The
/// body of [`run_hot`] is otherwise `&self`-safe, and the only leaf that still needs exclusivity
/// is `ldc` of a Class literal (`make_class` inserts into `class_objects`).
#[allow(clippy::mut_from_ref, invalid_reference_casting)]
fn vm_mut(vm: &crate::vm::thread_context::SharedVm) -> &mut Vm {
    unsafe {
        &mut *(vm as *const crate::vm::thread_context::SharedVm as *mut crate::vm::thread_context::SharedVm)
    }
}
use crate::opcodes::*;

/// Serve `getfield` inside the hot loop instead of returning `Cold` for it. **DEFAULT ON**;
/// `RAVA_NO_HOT_GETFIELD=1` opts out.
///
/// ⭐**Measured before shipping, same-binary flag A/B on `bench/interp_gate.ps1`** (ledger row
/// 350): off **50,243 / 51,410 / 50,088 ms** vs on **48,491 / 49,038 / 48,840** — **every ON run
/// below every OFF run, min-of-N −3.19%, 3/3 paired**, hash `f55ae4b5e17b36c1` on all six. All
/// four canonical gates bit-exact with it on.
///
/// ⚠**Spelled as an OPT-OUT on purpose.** `RAVA_DISPATCH_TABLE` is read by nothing in this tree
/// while three comments call it "default OFF", and an A/B against that name compared two identical
/// configurations and reported the machine — twice. The gate here is the name that IS read.
#[cfg(feature = "next-gen-gc")]
fn hot_getfield() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_HOT_GETFIELD").is_none())
}

#[cfg(not(feature = "next-gen-gc"))]
fn hot_getfield() -> bool {
    false
}


/// Default ON, opt-out `RAVA_NO_HOT_PUTFIELD` — the twin of [`hot_getfield`].
///
/// ⭐**Row 378 measured why this is worth an arm.** `bench/Allocs.java` under `RAVA_JIT=0` exits the
/// hot loop **124,000,057** times for **822,503,037** opcodes — **6.63 opcodes per entry** — and
/// **62,000,000 of those exits (exactly 50%) are `0xb5 putfield`**. It is the single largest reason
/// the loop gives up, on the scenario that is the worst of the seven in the cross-engine sweep
/// (10.95x `java -Xint`, row 377).
#[cfg(feature = "next-gen-gc")]
fn hot_putfield() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_HOT_PUTFIELD").is_none())
}

#[cfg(not(feature = "next-gen-gc"))]
fn hot_putfield() -> bool {
    false
}


/// ⛔**RETIRED BEFORE SHIPPING — the arm this gated was WRONG BY CONSTRUCTION.** Kept only so the
/// reason is next to the code: `run.rs` dispatches `0xbb` through `ops::invoke`, NOT straight to
/// `new_object_value`, and that layer does more (the native gate and its `Plan`s). Calling
/// `new_object_value` directly from the hot loop skipped it and the gates died with
/// `uncaught java/lang/UnsupportedOperationException` — ChunkFill and RingScan produced NO HASH.
/// ⇒ ⭐**Before lifting an opcode into the hot loop, read what its COLD dispatch actually calls**;
/// `getfield`/`putfield` were safe because `run.rs` calls their implementation directly.
///
/// Original note: opt-out `RAVA_NO_HOT_NEW`, the third opcode lifted into the hot loop.
///
/// ⭐**Row 378 removed `putfield` and left `new` holding a quarter of what remained.** On
/// `bench/Allocs.java` the exits went 124,000,057 → 62,000,057, of which `0xbb new` is
/// **15,500,000 (25%)**; the rest are calls, which the loop cannot serve at all.
///
/// ⚠**Only the plain answer is served here.** `new_object_value` returns a `NewSignal`, and two of
/// its three variants mean "the interpreter must do something a loop iteration cannot":
/// `NurseryFull` needs a collection at a frame boundary, `ClinitPending` needs a `<clinit>` run.
/// Both fall back to the cold path with the operand stack and pc UNTOUCHED, so `run` re-executes
/// the opcode exactly as if the hot loop had never looked at it — the same discipline the array-load
/// arm already uses when it peeks and refuses.
#[cfg(feature = "next-gen-gc")]
#[allow(dead_code)]
fn hot_new() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_HOT_NEW").is_none())
}

#[cfg(not(feature = "next-gen-gc"))]
#[allow(dead_code)]
fn hot_new() -> bool {
    false
}


/// Default ON, opt-out `RAVA_NO_HOT_LDC` — `ldc_w`/`ldc2_w` in the hot loop.
///
/// ⭐**Row 381 censused four scenarios instead of one, and `ArithLoop` came back with
/// 100,000,000 hot-run exits of which `0x14 ldc2_w` is 100,000,000 — literally all of them.** A
/// long/double constant load is the simplest operation in the instruction set and it was ejecting
/// the fast loop every time.
///
/// Safe to lift for the reason row 379 established as the test: `run.rs` implements `0x13 | 0x14`
/// DIRECTLY (`vm.ldc(&frame.class, …)`), with no `ops::invoke` layer to skip. `ldc` of a String
/// constant can allocate, so the operand length is published first and the cursor re-read after,
/// exactly as the `getfield` arm does.
#[cfg(feature = "next-gen-gc")]
fn hot_ldc() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_HOT_LDC").is_none())
}

#[cfg(not(feature = "next-gen-gc"))]
fn hot_ldc() -> bool {
    false
}

/// Default ON, opt-out `RAVA_NO_HOT_DIV` — `idiv`/`irem`/`ldiv`/`lrem` in the hot loop.
///
/// ⭐Row 381: `BinarySearchLambda` exits **26,156,189** times on `0x6c idiv` alone (27.1%).
///
/// ⚠**These are the arithmetic opcodes that THROW**, so the divisor is PEEKED and the operands are
/// only consumed once it is known to be non-zero. A zero divisor leaves the stack and pc untouched
/// and falls cold, where `run` raises the ArithmeticException it always did — the same
/// peek-then-commit discipline the array-load arm uses.
#[cfg(feature = "next-gen-gc")]
fn hot_div() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_HOT_DIV").is_none())
}

#[cfg(not(feature = "next-gen-gc"))]
fn hot_div() -> bool {
    false
}


/// ⛔**RETIRED: the arm this gated removed 100,000,000 exits and bought NOTHING.** Kept so the
/// negative result sits with the code. Row 382: `ArrReduce` went 100,000,036 hot-run exits → **36**,
/// output unchanged, and the paired A/B read **−0.50% with the arms OVERLAPPING** (ON
/// 7,311/7,500/6,992/7,481 ms vs OFF 7,108/7,484/7,143/7,027 — a ~500 ms spread INSIDE each arm,
/// larger than any effect). ⇒ ⭐**Confirms row 381's calibration**: a hot-loop exit is ~1.8 ns, and
/// unlike `putfield` the hot and cold implementations of `iastore` cost about the same, so there
/// was no second saving to collect. **35 lines of unsafe peeking in the hottest loop for an effect
/// below the instrument's resolution is a bad trade.**
///
/// Original note: opt-out `RAVA_NO_HOT_ASTORE`, primitive array stores in the hot loop.
///
/// ⭐**Row 382: `bench/ArrReduce.java` exits the hot loop 100,000,036 times and 100,000,000 of them
/// are `0x4f iastore`** — the array LOAD arm has existed for a while, the store never did, so a
/// loop that reads and writes an array bounced out on every write.
///
/// Lifted only after row 379's test: `run.rs` implements `0x4f..=0x56` DIRECTLY, no `ops::invoke`.
/// The arm mirrors the load arm exactly — PEEK all three operands, refuse on anything the cold
/// path would treat specially, and only then commit:
///
/// * **`0x53 aastore` is REFUSED** — it carries the JVMS 6.5 store check against the array's own
///   element descriptor, and a reference store is the one that can be wrong silently.
/// * a null or non-array receiver, a non-int index, or an out-of-bounds index falls cold so `run`
///   raises the NullPointerException / ArrayIndexOutOfBoundsException it always did, with the
///   operands still on the stack for its own handler search.
/// * inside a gil-free ring task the lane barrier must run, so `in_lane()` refuses too.
#[cfg(feature = "next-gen-gc")]
#[allow(dead_code)]
fn hot_astore() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_HOT_ASTORE").is_none())
}

#[cfg(not(feature = "next-gen-gc"))]
#[allow(dead_code)]
fn hot_astore() -> bool {
    false
}

/// Default ON, opt-out `RAVA_NO_HOT_LDC1` — plain `0x12 ldc` in the hot loop, the twin of
/// [`hot_ldc`]'s `0x13`/`0x14`.
///
/// ⭐**THE HOT LOOP SERVED THE RARE ENCODING AND HANDED BACK THE COMMON ONE.** `ldc_w` (`0x13`)
/// exists only for a constant-pool index that does not fit in a byte; javac emits `0x12` for
/// every index below 256, which in ordinary class files is most of them — so the arm row 381
/// added covered the wide form while the narrow one still ended a hot run, returned `Cold`, was
/// re-dispatched through `run`'s 98-arm match and paid this function's whole re-entry preamble.
/// **Was: one hot-run exit per `ldc` of a low-index constant; is: none.** What grows is the run
/// length between exits, which the census in [`hot_putfield`] prices at 6.63 opcodes.
///
/// Safe to lift by row 379's test, and by the strongest form of it: `run.rs`'s `0x12` arm is
/// `vm.ldc(&frame.class, bc[pc + 1] as u16)` — literally the same call the `0x13`/`0x14` arm
/// below makes, differing only in operand width and the pc delta. No `ops::invoke` layer to skip,
/// and the same allocate-then-re-read-the-cursor discipline applies because a String constant
/// allocates.
#[cfg(feature = "next-gen-gc")]
fn hot_ldc1() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_HOT_LDC1").is_none() && !crate::flags::emit_more_disabled()
    })
}

#[cfg(not(feature = "next-gen-gc"))]
fn hot_ldc1() -> bool {
    false
}

// ⭐⭐**THE HOT PREAMBLE'S TEN PROCESS CONSTANTS, ONE PER-THREAD LOAD.**
//
// KILL SWITCH `RAVA_NO_FAST_HOTPRE=1` (also honoured through `crate::flags::emit_more_disabled`).
// Default ON. With it set, [`hot_gates_cold`] resolves ONLY the switch itself, stamps [`HP_LIVE`],
// and the preamble below then calls exactly the `OnceLock` each gate always called, from exactly
// the site it always called it from — an honest A/B, because the off arm IS the historic code.
//
// WHAT THE SHAPE WAS. `run_hot`'s preamble read EIGHT `OnceLock`s — `hot_census::enabled`,
// `slot_census::enabled`, `pair_census::enabled`, `opdump::enabled`, `hot_getfield`,
// `hot_putfield`, `hot_ldc`, `hot_div` — plus `fast_gates()`'s own per-thread load. Every one of
// the eight reaches its value through an ACQUIRE load: on x86 that is a plain `mov`, but as an
// atomic it is an OPTIMISATION BARRIER — LLVM may not fold two of them together, may not sink one
// past a store, and must assume tracked memory is clobbered across each. Row 409 priced this
// shape from the other side: ONE relaxed atomic taken off the operand-push path measured **-15%**,
// and putting it back **+17%**.
//
// ⚠**AND THE HOIST COVERED FAR LESS THAN IT LOOKS.** These were called "hoisted out of the
// dispatch loop", but `run_hot` is not entered once per METHOD — it is entered once per RUN of
// opcodes it can serve, and this file's own census prices that run at **6.63 opcodes**
// (`bench/Allocs.java`, 822,503,037 opcodes over 124,000,057 entries, row 378). Eight acquire
// loads per entry is therefore **~1.21 acquire loads per opcode the interpreter EXECUTES**, and
// the interpreter is 19.4% of the boot over ~197 M opcodes.
//
//     was: eight ACQUIRE loads + one plain TLS load per entry  = ~1.36 loads per opcode executed;
//      is: ONE plain per-thread load per entry, bit-tested ten ways = ~0.15 per opcode executed,
//          and that one is a load LLVM is free to hoist, fold and keep in a register.
//
// THE CURE, copied verbatim from `opcodes::gates::op_gates` (and from `frames::POOL_GATES`,
// `vmframe::FRAME_GATES`, `natives::entry::entry_gates`): a per-thread `Cell<u32>` whose
// initialiser is a `const` block. That is the one combination `std` lowers to a bare
// `#[thread_local]` static — no lazy-init check, no destructor, nothing to call.
//
// ⚠**WHY NOT JUST ADD BITS TO `op_gates`?** Because that word lives in `opcodes/gates.rs`, which
// is not this file, and because these ten are read from ONE site between them. A second word read
// from one place costs the same single load and keeps the two rounds independently bisectable.
// `OG_FAST` is folded in here as [`HP_FAST`] so the entry takes one TLS load rather than two.
//
// **NOTHING ABOUT THE ANSWERS CHANGES.** Every bit is filled from the very same `OnceLock` that
// used to be read at the site, on a cold path that runs once per thread. There is no key, no
// lookup, no miss and no invalidation — this is one word per thread holding booleans that are
// constants of the process.
//
// ⚠**ONLY PURE `env::var_os` GATES ARE IN THE WORD**, the same rule `op_gates` follows: a gate
// whose initialiser has a SIDE EFFECT would have that side effect moved earlier. All ten are
// `env::var_os(..).is_some()`/`.is_none()` and nothing else. `hot_census::start_watchdog()` — the
// one side effect in this preamble, and it ARMS A THREAD — is untouched: it still runs at the
// same site, still only when the census is on, and is deliberately NOT part of the resolution.
/// Bit 0 — this thread has resolved. A word of 0 therefore means "not yet", which is what makes
/// the fast read a single compare against zero.
const HP_RESOLVED: u32 = 1 << 0;
/// Bit 1 — the kill switch is engaged: distrust every other bit and call the `OnceLock`s from the
/// sites that used to call them, as the historic code did.
const HP_LIVE: u32 = 1 << 1;
/// Bit 2 — `opcodes::fast_gates()`, the previous round's hoist switch.
const HP_FAST: u32 = 1 << 2;
/// Bit 3 — [`hot_getfield`] (`RAVA_NO_HOT_GETFIELD`).
const HP_GETFIELD: u32 = 1 << 3;
/// Bit 4 — [`hot_putfield`] (`RAVA_NO_HOT_PUTFIELD`).
const HP_PUTFIELD: u32 = 1 << 4;
/// Bit 5 — [`hot_ldc`] (`RAVA_NO_HOT_LDC`).
const HP_LDC: u32 = 1 << 5;
/// Bit 6 — [`hot_div`] (`RAVA_NO_HOT_DIV`).
const HP_DIV: u32 = 1 << 6;
/// Bit 7 — [`hot_ldc1`] (`RAVA_NO_HOT_LDC1`).
const HP_LDC1: u32 = 1 << 7;
/// Bit 8 — `slot_census::enabled` (`RAVA_SLOT_CENSUS`).
const HP_SLOTCEN: u32 = 1 << 8;
/// Bit 9 — `pair_census::enabled` (`RAVA_PAIR_CENSUS`).
const HP_PAIRCEN: u32 = 1 << 9;
/// Bit 10 — `opdump::enabled` (`RAVA_OPDUMP`).
const HP_OPDUMP: u32 = 1 << 10;
/// Bit 11 — `hot_census::enabled` (`RAVA_HOT_CENSUS`). Read only from the `next-gen-gc` arms.
#[allow(dead_code)]
const HP_HOTCEN: u32 = 1 << 11;

thread_local! {
    /// This thread's resolved hot-preamble gates; see the block comment above. `Cell<u32>` with a
    /// `const` initialiser, deliberately — no destructor and no lazy-init check, so the access is
    /// a bare TLS load rather than an out-of-line `LocalKey::with`.
    static HOT_GATES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// The word. One plain per-thread load; never 0 on return.
#[inline(always)]
fn hot_gates() -> u32 {
    let g = HOT_GATES.get();
    if g != 0 { g } else { hot_gates_cold() }
}

/// First touch on this thread: read the process constants through their `OnceLock`s — the ONLY
/// place that still happens under the default — and cache the answers. Once per thread, so the
/// environment lookups behind them stay exactly as many as they were.
#[cold]
#[inline(never)]
fn hot_gates_cold() -> u32 {
    static LIVE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let live = *LIVE.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_HOTPRE").is_some() || crate::flags::emit_more_disabled()
    });
    let mut g = HP_RESOLVED;
    if live {
        // Resolve NOTHING else: with the switch engaged the preamble must reach each `OnceLock`
        // itself, in its own order, exactly as before.
        g |= HP_LIVE;
        HOT_GATES.set(g);
        return g;
    }
    if crate::opcodes::fast_gates() { g |= HP_FAST; }
    if hot_getfield() { g |= HP_GETFIELD; }
    if hot_putfield() { g |= HP_PUTFIELD; }
    if hot_ldc() { g |= HP_LDC; }
    if hot_div() { g |= HP_DIV; }
    if hot_ldc1() { g |= HP_LDC1; }
    if crate::slot_census::enabled() { g |= HP_SLOTCEN; }
    if crate::pair_census::enabled() { g |= HP_PAIRCEN; }
    if crate::opdump::enabled() { g |= HP_OPDUMP; }
    // Only the `next-gen-gc` build reads this one, so only it pays the resolution.
    #[cfg(feature = "next-gen-gc")]
    if crate::opcodes::hot_census::enabled() { g |= HP_HOTCEN; }
    HOT_GATES.set(g);
    g
}

/// Read one bit of an ALREADY-LOADED word — the `op_gate_of` / `entry_gates` discipline: a
/// preamble that needs ten gates takes one load, not ten acquire loads. `live` is the historic
/// `OnceLock` reader, called only under the kill switch.
#[inline(always)]
fn hp_gate(g: u32, bit: u32, live: fn() -> bool) -> bool {
    if g & HP_LIVE != 0 { live() } else { g & bit != 0 }
}

/// Read a hoisted opcode gate. With `RAVA_NO_FAST_GATES` (or `RAVA_NO_EMIT_MORE`) set, `fast` is
/// false and the guard re-reads the `OnceLock` per opcode exactly as it did before — the two
/// answers are identical by construction, so the switch selects only where the load happens.
#[inline(always)]
fn opgate(fast: bool, hoisted: bool, live: fn() -> bool) -> bool {
    if fast { hoisted } else { live() }
}

/// Why `run_hot` returned.
pub(crate) enum HotExit {
    /// `frame.pc` points at an opcode this function does not handle.
    Cold,
    /// The op budget ran out; `frame.pc` points at the next opcode.
    Poll,
}

/// The per-op tracer (`RAVA_TRACE_OPS`), out of line — one `eprintln!` inlined into the dispatch
/// loop drags `core::fmt` in with it.
#[cold]
#[inline(never)]
fn trace_op(class: &crate::class_loader::LoadedClass, filter: &str, pc: usize, op: u8, sp: usize) {
    if class.file.this_class.contains(filter) {
        eprintln!("[OP] {} pc={pc} op=0x{op:02X} stack={sp}", class.file.this_class);
    }
}

#[inline(never)]
pub(crate) fn run_hot(
    vm: &crate::vm::thread_context::SharedVm,
    frame: &mut Frame,
    bc: &[u8],
    trace_ops: &Option<String>,
    branch_cold: bool,
) -> Result<HotExit, VmError> {
    // THE FRAME, DESTRUCTURED. `pc`, `locals`, `stack` and `class` become disjoint borrows, so the
    // operand-stack cursor below can be a raw pointer and an index in registers instead of three
    // fields re-read from memory. The disassembly of the previous shape showed the cost plainly:
    // every push reloaded the buffer pointer (`mov rax,[rbx]`) and the capacity (`cmp r15,[r13]`)
    // before storing its 24 bytes.
    // `RAVA_HOT_CENSUS` — how many opcodes does one entry into this function execute?
    //
    // ⭐**Ledger row 348.** `run_hot` is 20.5% of interpreter time and `run` another 12.5%
    // (row 344), and `run_hot` hands control BACK on every opcode it does not own — which
    // includes every field access and every call, i.e. the two most common non-arithmetic
    // things a method body does. Each entry re-destructures the frame and reloads the operand
    // cursor, so if the average run is two or three opcodes the split is paying its setup
    // more often than it amortises it. **That is a number, and it has never been taken.**
    //
    // ⭐**ALL TEN PREAMBLE GATES NOW COME OUT OF ONE PER-THREAD LOAD** — see the `HP_*` block for
    // what the eight acquire loads that used to stand here cost on a path entered once per 6.63
    // executed opcodes. Kill switch `RAVA_NO_FAST_HOTPRE=1` restores the historic reads at this
    // very site, in this very order.
    let hpw = hot_gates();
    #[cfg(feature = "next-gen-gc")]
    let census = hp_gate(hpw, HP_HOTCEN, crate::opcodes::hot_census::enabled);
    #[cfg(feature = "next-gen-gc")]
    if census {
        crate::opcodes::hot_census::start_watchdog();
        crate::opcodes::hot_census::note_entry();
    }
    #[allow(unused_mut)]
    let mut ops_here: u64 = 0;
    // Loop-invariant for this whole entry; see the note in `push!`.
    let slotcen = hp_gate(hpw, HP_SLOTCEN, crate::slot_census::enabled);
    // Hoisted for exactly the reason row 409 records: a `OnceLock` read is an atomic the
    // optimiser may not lift out of this loop.
    let paircen = hp_gate(hpw, HP_PAIRCEN, crate::pair_census::enabled);
    // `RAVA_OPDUMP=<path>` — write the first N executed opcodes to a file, so a dispatcher
    // experiment can be run against the REAL stream instead of a synthetic one. The distinction
    // decides a rewrite: a random stream is maximally unpredictable and understates what a
    // per-opcode branch site could learn from real bytecode's higher-order structure.
    let opdump = hp_gate(hpw, HP_OPDUMP, crate::opdump::enabled);
    // ⭐**THE FOUR OPCODE GATES, READ ONCE PER ENTRY — NOT ONCE PER OPCODE.**
    //
    // `hot_gf` was already computed here, with a comment citing row 409 — and then the `0xb4`
    // guard below went on calling `hot_getfield()` anyway, so the hoist was dead and the atomic
    // stayed in the loop. Its three siblings (`putfield`, `ldc_w`/`ldc2_w`, `idiv`/`irem`/`ldiv`/
    // `lrem`) were never hoisted at all. Each is a `OnceLock` read, i.e. an ACQUIRE atomic load,
    // sitting in a match guard — which means one per such opcode EXECUTED, and the arms' own
    // census numbers say what that population is: `getfield` ends 306,233,425 of 1,286,515,599
    // hot-loop exits (row 349), `putfield` 62,000,000 on `Allocs` (row 378), `ldc2_w` all
    // 100,000,000 of `ArithLoop`'s (row 381), `idiv` 26,156,189 of `BinarySearchLambda`'s.
    //
    // **Was O(1) atomic load per getfield / putfield / ldc / idiv-family opcode; is O(1) per entry
    // into `run_hot`.** What grows is opcodes executed. The value is a process constant — env is
    // read once by the `OnceLock` regardless — so the guard cannot answer differently, only sooner.
    // Row 409 priced exactly this shape from the other side: ONE relaxed atomic taken off the push
    // path measured −15%, and putting it back +17%, because an atomic is a thing LLVM may not
    // HOIST, not a `mov` it can schedule.
    //
    // ⭐**AND NOW NONE OF THE FIVE IS AN ATOMIC** — they are five bit tests of `hpw`, the word
    // loaded once above. The previous round folded `fast_gates` into `op_gates`, which made it a
    // plain per-thread load but a SECOND one; folding it in here as `HP_FAST` gets the entry back
    // to a single TLS access for every gate it reads.
    let fast = hp_gate(hpw, HP_FAST, crate::opcodes::fast_gates);
    let hot_gf = hp_gate(hpw, HP_GETFIELD, hot_getfield);
    let hot_pf = hp_gate(hpw, HP_PUTFIELD, hot_putfield);
    let hot_ld = hp_gate(hpw, HP_LDC, hot_ldc);
    let hot_dv = hp_gate(hpw, HP_DIV, hot_div);
    // `0x12 ldc`, admitted by this round; see `hot_ldc1`. Read here with its siblings, so it is a
    // bit test rather than a ninth acquire load.
    let hot_ld1 = hp_gate(hpw, HP_LDC1, hot_ldc1);
    // ⭐**TWO MORE PER-OPCODE MEMORY READS THAT ARE PROCESS CONSTANTS.**
    //
    // `trace_ops` is a `&Option<String>` published once at `Vm::new` and `vm.trace.big` a `bool`
    // set from `RAVA_TRACE_BIG` in the same constructor — neither is ever written again. But the
    // loop below reads them THROUGH POINTERS the compiler must assume are clobbered: `run_hot`
    // takes `vm` by `&mut` and calls back into it (`ldc`, `getfield`, `putfield`), so the option's
    // discriminant was reloaded on EVERY opcode and `vm.trace.big` cost a two-load chain on every
    // `*store` — which is `0x36..=0x4e`, among the most frequent opcodes a method body runs.
    // Copies whose address never escapes cannot alias, so these two stay in registers. `run_body`
    // already does exactly this with its own `trace_on`; this is the same move inside the loop
    // that actually executes the opcodes.
    let trace_filter: Option<&str> = trace_ops.as_deref();
    let trace_big = vm.trace.big;
    let mut prev_op: u16 = 0x100; // out of range = no predecessor yet
    // ⭐**THE CHECKPOINT BUDGET IS A LOCAL, NOT A POINTER** (row 408). `countdown` arrives as
    // `&mut u32` and the loop head both READ and WROTE through it on every single opcode — two
    // memory operations per dispatch, on the function the first interpreted-run profile puts at
    // **44% of wall**. It is published by `sync!` alongside the operand cursor, and `sync!` is
    // already on every exit path from this function for exactly that reason.
    // Cached for this loop only: nothing nests inside it, so no other frame can advance the
    // counter while it is held here. Published by `sync!` on every exit.
    let mut cd = crate::frames::op_countdown_seed();
    // ⭐**ONE BUFFER** (`docs/contiguous-stack-plan.md` stage 1): locals occupy `[0, nloc)` and
    // the operand stack `[nloc, len)`. The cursors below are unchanged in meaning — `sp` is still
    // the operand COUNT, not a buffer index — so every arm of the dispatch loop reads the same.
    let Frame { pc, buf, nlocals, class, .. } = frame;
    let nloc = *nlocals;
    // ⚠**BOTH pointers derive from the same allocation**, so the grow path in `push!` has to
    // refresh the LOCALS base as well. It did not have to when they were separate `Vec`s, and a
    // stale `lbase` after a realloc is a use-after-free on every subsequent local access.
    let mut lbase = buf.as_mut_ptr();
    let mut sbase = unsafe { lbase.add(nloc) };
    let mut cap = buf.capacity() - nloc;
    let mut sp = buf.len() - nloc;

    // Publish the cursor back into the Vec. EVERY exit from this function goes through it — the
    // `Frame` is a GC root, and a stale length would either hide live values from the collector or
    // hand it uninitialised ones.
    macro_rules! sync {
        () => {{
            // SAFETY: `sp` only ever moves within the initialised prefix of the buffer.
            unsafe { buf.set_len(nloc + sp) };
            crate::frames::op_countdown_store(cd);
        }};
    }
    macro_rules! push {
        ($v:expr) => {{
            let v = $v;
            // `RAVA_SLOT_CENSUS` (row 388): what fraction of operand traffic is `Str`?
            // ⚠**HOISTED** (row 408): `enabled()` is a `OnceLock` read — a relaxed ATOMIC load,
            // which LLVM may not lift out of a loop — and this is the PUSH path, the most
            // frequent operation in the interpreter. Read once per entry into the dispatch loop.
            if slotcen {
                crate::slot_census::note(&v);
            }
            if sp < cap {
                // SAFETY: `sp < cap`, so the slot is allocated; the value is moved into it and the
                // length is published by `sync!` before anything else can observe the buffer.
                unsafe { std::ptr::write(sbase.add(sp), v) };
                sp += 1;
            } else {
                // `Frame::new` reserves `max_stack` and the verifier says no valid bytecode
                // exceeds it, so this is unreachable in practice — but growing keeps the old
                // behaviour exactly, and the cursor is re-read because the buffer may have moved.
                sync!();
                buf.push(v);
                lbase = buf.as_mut_ptr();
                sbase = unsafe { lbase.add(nloc) };
                cap = buf.capacity() - nloc;
                sp = buf.len() - nloc;
            }
        }};
    }
    macro_rules! pop {
        () => {{
            if sp == 0 {
                sync!();
                return Err(underflow(class, *pc));
            }
            sp -= 1;
            // SAFETY: the slot below the cursor is initialised, and reading it MOVES the value out
            // — the cursor already excludes it, so it is never read twice.
            unsafe { std::ptr::read(sbase.add(sp)) }
        }};
    }
    macro_rules! pop_int {
        () => {{
            let v = pop!();
            match v.as_int() {
                Some(i) => i,
                None => {
                    sync!();
                    return Err(not_an_int(class, *pc, &v));
                }
            }
        }};
    }
    macro_rules! stack_last {
        () => {
            // SAFETY: the whole prefix below the cursor is initialised.
            if sp == 0 { None } else { Some(unsafe { &*sbase.add(sp - 1) }) }
        };
    }
    macro_rules! stack_nth1 {
        () => {
            // SAFETY: as `stack_last!`, one slot deeper.
            if sp < 2 { None } else { Some(unsafe { &*sbase.add(sp - 2) }) }
        };
    }
    macro_rules! loc {
        ($i:expr) => {{
            let i = $i;
            if i >= nloc {
                sync!();
                return Err(bad_local(class, *pc, i));
            }
            // SAFETY: bounds checked just above; `Frame::new` initialises `max_locals` slots.
            unsafe { (*lbase.add(i)).clone() }
        }};
    }
    macro_rules! loc_set {
        ($i:expr, $v:expr) => {{
            let i = $i;
            let v = $v;
            if i >= nloc {
                sync!();
                return Err(bad_local(class, *pc, i));
            }
            // SAFETY: as `loc!` — and the previous value is dropped, exactly as `locals[i] = v`.
            unsafe { *lbase.add(i) = v };
        }};
    }
    macro_rules! loc_mut {
        ($i:expr) => {{
            let i = $i;
            if i >= nloc {
                sync!();
                return Err(bad_local(class, *pc, i));
            }
            // SAFETY: as `loc!`; the borrow ends inside the arm that takes it.
            unsafe { &mut *lbase.add(i) }
        }};
    }
    loop {
        let pc_now = *pc;
        let op = bc[pc_now];
        #[cfg(feature = "next-gen-gc")]
        if census {
            crate::opcodes::hot_census::note_op();
            ops_here += 1;
        }
        if let Some(filter) = trace_filter {
            trace_op(class, filter, pc_now, op, sp);
        }
        if opdump {
            crate::opdump::note(op);
        }
        if paircen {
            if prev_op != 0x100 {
                crate::pair_census::note(prev_op as u8, op);
            }
            prev_op = op as u16;
        }
        if cd == 0 {
            sync!();
            return Ok(HotExit::Poll);
        }
        cd -= 1;
        match op {
            0x00 => *pc += 1,                                  // nop
            0x01 => { push!(Value::Null); *pc += 1; }     // aconst_null
            0x02..=0x08 => { push!(Value::Int(op as i32 - 0x03)); *pc += 1; } // iconst_m1..5
            0x09 => { push!(Value::Long(0)); *pc += 1; }  // lconst_0
            0x0a => { push!(Value::Long(1)); *pc += 1; }  // lconst_1
            0x0b..=0x0d => { push!(Value::Float((op - 0x0b) as f32)); *pc += 1; } // fconst_0..2
            0x0e | 0x0f => { push!(Value::Double((op - 0x0e) as f64)); *pc += 1; } // dconst_0/1
            0x10 => { push!(Value::Int(bc[pc_now + 1] as i8 as i32)); *pc += 2; } // bipush
            0x11 => {                                               // sipush
                let v = ((bc[pc_now + 1] as i16) << 8 | bc[pc_now + 2] as i16) as i32;
                push!(Value::Int(v));
                *pc += 3;
            }
            0x15..=0x19 => { let i = bc[pc_now + 1] as usize; push!(loc!(i)); *pc += 2; }
            0x1a..=0x2d => { push!(loc!(((op - 0x1a) % 4) as usize)); *pc += 1; }
            0x36..=0x3a => { let i = bc[pc_now + 1] as usize; let v = pop!(); if trace_big { trace_big_local(vm, class, *pc, &v); } loc_set!(i, v); *pc += 2; }
            0x3b..=0x4e => { let v = pop!(); if trace_big { trace_big_local(vm, class, *pc, &v); } loc_set!(((op - 0x3b) % 4) as usize, v); *pc += 1; }
            0x57 => { pop!(); *pc += 1; } // pop
            0x58 => { // pop2: one wide value, or two category-1 values
                let wide = matches!(stack_last!(), Some(v) if is_wide(v));
                pop!();
                if !wide { pop!(); }
                *pc += 1;
            }
            0x59 => { let v = pop!(); push!(v.clone()); push!(v); *pc += 1; } // dup
            0x5a => { // dup_x1
                let a = pop!(); let b = pop!();
                push!(a.clone()); push!(b); push!(a);
                *pc += 1;
            }
            0x5b => { // dup_x2
                let a = pop!(); let b = pop!(); let c = pop!();
                push!(a.clone()); push!(c); push!(b); push!(a);
                *pc += 1;
            }
            0x5c => { // dup2
                if matches!(stack_last!(), Some(v) if is_wide(v)) {
                    let v = pop!(); push!(v.clone()); push!(v);
                } else {
                    let a = pop!(); let b = pop!();
                    push!(b.clone()); push!(a.clone()); push!(b); push!(a);
                }
                *pc += 1;
            }
            0x5d => { // dup2_x1
                if matches!(stack_last!(), Some(v) if is_wide(v)) {
                    // Form 2: ..., v1(cat1), v2(cat2) -> ..., v2, v1, v2
                    let v2 = pop!(); let v1 = pop!();
                    push!(v2.clone()); push!(v1); push!(v2);
                } else {
                    // Form 1: ..., v3, v2, v1 -> ..., v2, v1, v3, v2, v1
                    let a = pop!(); let b = pop!(); let c = pop!();
                    push!(b.clone()); push!(a.clone()); push!(c); push!(b); push!(a);
                }
                *pc += 1;
            }
            0x5e => { // dup2_x2
                let top_wide = matches!(stack_last!(), Some(v) if is_wide(v));
                let second_wide = matches!(stack_nth1!(), Some(v) if is_wide(v));
                match (top_wide, second_wide) {
                    // Form 4: cat2 over cat2 -> v1, v2, v1
                    (true, true) => { let v1 = pop!(); let v2 = pop!(); push!(v1.clone()); push!(v2); push!(v1); }
                    // Form 2: cat2 over two cat1 -> v3? ..., c2,c1,V -> V,c2,c1,V
                    (true, false) => { let v = pop!(); let b = pop!(); let c = pop!(); push!(v.clone()); push!(c); push!(b); push!(v); }
                    // Form 3: two cat1 over cat2 -> ..., W, b, a -> b, a, W, b, a
                    (false, true) => { let a = pop!(); let b = pop!(); let w = pop!(); push!(b.clone()); push!(a.clone()); push!(w); push!(b); push!(a); }
                    // Form 1: four cat1 -> v4,v3,v2,v1 -> v2,v1,v4,v3,v2,v1
                    (false, false) => { let a = pop!(); let b = pop!(); let c = pop!(); let d = pop!(); push!(b.clone()); push!(a.clone()); push!(d); push!(c); push!(b); push!(a); }
                }
                *pc += 1;
            }
            0x5f => { let a = pop!(); let b = pop!(); push!(a); push!(b); *pc += 1; } // swap
            0x60 => { let (b, a) = (pop_int!(), pop_int!()); push!(Value::Int(a.wrapping_add(b))); *pc += 1; } // iadd
            0x64 => { let (b, a) = (pop_int!(), pop_int!()); push!(Value::Int(a.wrapping_sub(b))); *pc += 1; } // isub
            0x68 => { let (b, a) = (pop_int!(), pop_int!()); push!(Value::Int(a.wrapping_mul(b))); *pc += 1; } // imul
            0x84 => { let i = bc[pc_now + 1] as usize; let inc = bc[pc_now + 2] as i8 as i32;             // iinc
                      if let Value::Int(v) = loc_mut!(i) { *v = v.wrapping_add(inc); } *pc += 3; }
            0x74 => { let a = pop_int!(); push!(Value::Int(a.wrapping_neg())); *pc += 1; } // ineg
            0x78 => { let (b, a) = (pop_int!(), pop_int!()); push!(Value::Int(a.wrapping_shl(b as u32 & 0x1f))); *pc += 1; } // ishl
            0x7a => { let (b, a) = (pop_int!(), pop_int!()); push!(Value::Int(a.wrapping_shr(b as u32 & 0x1f))); *pc += 1; } // ishr
            0x7c => { let (b, a) = (pop_int!(), pop_int!()); push!(Value::Int(((a as u32).wrapping_shr(b as u32 & 0x1f)) as i32)); *pc += 1; } // iushr
            0x7e => { let (b, a) = (pop_int!(), pop_int!()); push!(Value::Int(a & b)); *pc += 1; } // iand
            0x80 => { let (b, a) = (pop_int!(), pop_int!()); push!(Value::Int(a | b)); *pc += 1; } // ior
            0x82 => { let (b, a) = (pop_int!(), pop_int!()); push!(Value::Int(a ^ b)); *pc += 1; } // ixor
            0x61 => { let (b, a) = (pop!().as_i64(), pop!().as_i64()); push!(Value::Long(a.wrapping_add(b))); *pc += 1; } // ladd
            0x65 => { let (b, a) = (pop!().as_i64(), pop!().as_i64()); push!(Value::Long(a.wrapping_sub(b))); *pc += 1; } // lsub
            0x69 => { let (b, a) = (pop!().as_i64(), pop!().as_i64()); push!(Value::Long(a.wrapping_mul(b))); *pc += 1; } // lmul
            0x75 => { let a = pop!().as_i64(); push!(Value::Long(a.wrapping_neg())); *pc += 1; } // lneg
            0x79 => { let (b, a) = (pop_int!(), pop!().as_i64()); push!(Value::Long(a.wrapping_shl(b as u32 & 0x3f))); *pc += 1; } // lshl
            0x7b => { let (b, a) = (pop_int!(), pop!().as_i64()); push!(Value::Long(a.wrapping_shr(b as u32 & 0x3f))); *pc += 1; } // lshr
            0x7d => { let (b, a) = (pop_int!(), pop!().as_i64()); push!(Value::Long(((a as u64).wrapping_shr(b as u32 & 0x3f)) as i64)); *pc += 1; } // lushr
            0x7f => { let (b, a) = (pop!().as_i64(), pop!().as_i64()); push!(Value::Long(a & b)); *pc += 1; } // land
            0x81 => { let (b, a) = (pop!().as_i64(), pop!().as_i64()); push!(Value::Long(a | b)); *pc += 1; } // lor
            0x83 => { let (b, a) = (pop!().as_i64(), pop!().as_i64()); push!(Value::Long(a ^ b)); *pc += 1; } // lxor
            0x6e => { let (b, a) = (pop!().as_f32(), pop!().as_f32()); push!(Value::Float(a / b)); *pc += 1; } // fdiv
            0x72 => { let (b, a) = (pop!().as_f32(), pop!().as_f32()); push!(Value::Float(a % b)); *pc += 1; } // frem
            0x6f => { let (b, a) = (pop!().as_f64(), pop!().as_f64()); push!(Value::Double(a / b)); *pc += 1; } // ddiv
            0x73 => { let (b, a) = (pop!().as_f64(), pop!().as_f64()); push!(Value::Double(a % b)); *pc += 1; } // drem
            // ⭐**PURE UNARY NUMERIC + FLOAT/DOUBLE ARITHMETIC — SHARED, NOT RESTATED.**
            // These twenty-three arms used to be twenty-three one-liners here, and `flatbody`
            // refused all of them because duplicating their semantics was judged a liability.
            // `RAVA_STRAIGHT_BLOCKERS` prices that refusal at 19.3% (casts) + 11.9% (fp add/sub/
            // mul) of every call ChunkFill sends down the framed path. Moving the expressions to
            // `pure_ops` — unchanged — lets the frame-free executor run them off the SAME code,
            // so the two paths cannot disagree.
            0x76 | 0x77 | 0x85..=0x93 => {
                let v = pop!();
                match crate::opcodes::pure_ops::pure_unary(op, &v) {
                    Some(r) => push!(r),
                    // ⚠**`sync!` THEN return, like every other cold exit in this function.** A
                    // bare `break` out of the dispatch loop becomes the function's tail value and
                    // skips the operand-cursor publish, leaving `stack.len()` stale for a GC that
                    // treats the frame as a root. Unreachable today — this arm's opcode range is
                    // exactly the set `pure_ops` defines — but the next opcode added to the range
                    // without a matching arm there would find the landmine.
                    None => {
                        sync!();
                        return Ok(HotExit::Cold);
                    }
                }
                *pc += 1;
            }
            0x62 | 0x63 | 0x66 | 0x67 | 0x6a | 0x6b => {
                let b = pop!();
                let a = pop!();
                match crate::opcodes::pure_ops::pure_fp_binary(op, &a, &b) {
                    Some(r) => push!(r),
                    // ⚠**`sync!` THEN return, like every other cold exit in this function.** A
                    // bare `break` out of the dispatch loop becomes the function's tail value and
                    // skips the operand-cursor publish, leaving `stack.len()` stale for a GC that
                    // treats the frame as a root. Unreachable today — this arm's opcode range is
                    // exactly the set `pure_ops` defines — but the next opcode added to the range
                    // without a matching arm there would find the landmine.
                    None => {
                        sync!();
                        return Ok(HotExit::Cold);
                    }
                }
                *pc += 1;
            }
            0x94 => { let (b, a) = (pop!().as_i64(), pop!().as_i64()); push!(Value::Int(a.cmp(&b) as i32)); *pc += 1; } // lcmp
            0x95 | 0x96 => { let (b, a) = (pop!().as_f32(), pop!().as_f32()); // fcmpl/fcmpg
                let c = if a < b { -1 } else if a > b { 1 } else if a == b { 0 } else if op == 0x95 { -1 } else { 1 };
                push!(Value::Int(c)); *pc += 1; }
            0x97 | 0x98 => { let (b, a) = (pop!().as_f64(), pop!().as_f64()); // dcmpl/dcmpg
                let c = if a < b { -1 } else if a > b { 1 } else if a == b { 0 } else if op == 0x97 { -1 } else { 1 };
                push!(Value::Int(c)); *pc += 1; }

            // ARRAY LOADS, IN-BOUNDS ONLY. The array opcodes stayed in `run` because they throw,
            // and a PC profile of `bench/ArrayBench.java` shows what that costs: the fetch itself
            // (`Heap::array_get_bounded`) is 4.0% of the window while `run` is 26.3% — the bounce
            // out of this loop and back, once per array access.
            //
            // So the case that CANNOT throw is served here and every other case returns Cold with
            // the operand stack and pc UNTOUCHED, so `run` re-executes the opcode and raises the
            // exception exactly as before: a null array, an out-of-range index, a non-array
            // receiver and a non-int index all fall through to code that has not changed.
            0x2e..=0x35 => {
                if sp < 2 {
                    sync!();
                    return Err(underflow(class, *pc));
                }
                // SAFETY: `sp >= 2`, so both slots are initialised; peeked, not popped, so the
                // fallback below leaves the stack exactly as the cold path expects to find it.
                let (idx_v, arr_v) = unsafe { (&*sbase.add(sp - 1), &*sbase.add(sp - 2)) };
                let (Some(i), Value::Array(h)) = (idx_v.as_int(), arr_v) else {
                    sync!();
                    {
                    #[cfg(feature = "next-gen-gc")]
                    if census {
                        crate::opcodes::hot_census::note_cold(op);
                    }
                    return Ok(HotExit::Cold);
                }
                };
                let Ok(v) = vm.heap.array_get_bounded(*h, i as usize) else {
                    sync!();
                    {
                    #[cfg(feature = "next-gen-gc")]
                    if census {
                        crate::opcodes::hot_census::note_cold(op);
                    }
                    return Ok(HotExit::Cold);
                }
                };
                sp -= 2;
                push!(v);
                *pc += 1;
            }
            // Branches. `branch_cold` is the disjunction of the four back-edge probes and the
            // forward-goto probe, all of them env-static — armed, every branch goes to `run` and
            // its arms run in full; disarmed, the back edge stays here. It has to stay here: a
            // loop takes one per iteration, and bouncing out for each was 9.2% of a pure
            // interpreter run in the PC profile.
            //
            // The safepoint does not depend on this. The op budget above is decremented per
            // opcode, so a loop that never leaves this function still polls on schedule.
            0x99..=0x9e | 0x9f..=0xa4 | 0xa5 | 0xa6 | 0xa7 | 0xc6 | 0xc7 => {
                if branch_cold {
                    sync!();
                    {
                    #[cfg(feature = "next-gen-gc")]
                    if census {
                        crate::opcodes::hot_census::note_cold(op);
                    }
                    return Ok(HotExit::Cold);
                }
                }
                let t_ = (pc_now as i32 + branch_offset(bc, pc_now)) as usize;
                let take = match op {
                    0x99..=0x9e => {
                        let v = pop_int!();
                        match op { 0x99 => v == 0, 0x9a => v != 0, 0x9b => v < 0, 0x9c => v >= 0, 0x9d => v > 0, _ => v <= 0 }
                    }
                    0x9f..=0xa4 => {
                        let (b, a) = (pop_int!(), pop_int!());
                        match op { 0x9f => a == b, 0xa0 => a != b, 0xa1 => a < b, 0xa2 => a >= b, 0xa3 => a > b, _ => a <= b }
                    }
                    0xa5 | 0xa6 => {
                        let (b, a) = (pop!(), pop!());
                        let eq = ref_equal(&a, &b);
                        if op == 0xa5 { eq } else { !eq }
                    }
                    0xc6 | 0xc7 => {
                        let is_null = matches!(pop!(), Value::Null);
                        if op == 0xc6 { is_null } else { !is_null }
                    }
                    // goto — unconditional, either direction
                    _ => true,
                };
                *pc = if take || op == 0xa7 { t_ } else { pc_now + 3 };
            }
            // ⭐**GETFIELD IN THE HOT LOOP** (default ON; `RAVA_NO_HOT_GETFIELD` opts out).
            //
            // Ledger row 349: `getfield` ends **306,233,425 of 1,286,515,599 hot-run exits —
            // 23.8%, the single largest**, and ~59% of the rest (calls and returns) can never be
            // removed because the callee needs its own frame and a return ends the method. Serving
            // it here deletes, per occurrence, the `Cold` return, the re-dispatch through `run`'s
            // 98-arm match, and this function's re-entry preamble.
            //
            // ⚠**This is the change the file header argues against** — it says the arms that stay
            // in `run` are the ones "that need the wider machine", and this one touches `vm`. So
            // it was measured on `bench/interp_gate.ps1` rather than assumed — the split exists
            // because a disassembly showed register pressure, and the same pressure is what this
            // could reintroduce. It does not: **−3.19%, every ON run below every OFF run, 3/3
            // paired, four canonical gates bit-exact.** See `hot_getfield`.
            //
            // The body is `fields::getfield_value`, the SAME code `run`'s arm calls — not a copy.
            // ldc_w / ldc2_w: a constant from the pool. `vm.ldc` already takes the CLASS, so
            // nothing had to be extracted for this one.
            // ⭐**`0x12 ldc` — THE NARROW ENCODING, WHICH IS THE COMMON ONE** (default ON;
            // `RAVA_NO_HOT_LDC1` opts out; see `hot_ldc1`). One byte of operand instead of two,
            // `pc += 2` instead of 3, and otherwise character-for-character the arm below —
            // because `run.rs`'s `0x12` is character-for-character its own `0x13 | 0x14` too. The
            // `sync!`-before / re-read-after discipline is not optional here: a String constant
            // allocates and may collect.
            0x12 if hot_ld1 && opgate(fast, hot_ld, hot_ldc) => {
                let idx = bc[pc_now + 1] as u16;
                sync!();
                let v = vm_mut(vm).ldc(class, idx)?;
                // Re-read BOTH bases: `vm.` re-entry may have grown the shared buffer.
                lbase = buf.as_mut_ptr();
                sbase = unsafe { lbase.add(nloc) };
                cap = buf.capacity() - nloc;
                push!(v);
                *pc = pc_now + 2;
            }
            0x13 | 0x14 if opgate(fast, hot_ld, hot_ldc) => {
                let idx = u16::from_be_bytes([bc[pc_now + 1], bc[pc_now + 2]]);
                // A String constant allocates, so publish the operand length before the call and
                // re-read the cursor after it.
                sync!();
                let v = vm_mut(vm).ldc(class, idx)?;
                // Re-read BOTH bases: `vm.` re-entry may have grown the shared buffer.
                lbase = buf.as_mut_ptr();
                sbase = unsafe { lbase.add(nloc) };
                cap = buf.capacity() - nloc;
                push!(v);
                *pc = pc_now + 3;
            }
            // idiv / irem / ldiv / lrem. PEEK the divisor: a zero must reach the cold path with the
            // stack exactly as it was, so `run` throws with the operands still in place.
            0x6c | 0x70 | 0x6d | 0x71 if opgate(fast, hot_dv, hot_div) => {
                if sp < 2 {
                    sync!();
                    return Err(underflow(class, *pc));
                }
                let long_form = matches!(op, 0x6d | 0x71);
                // SAFETY: `sp >= 2`, so both slots are initialised; peeked, not popped.
                let (bv, av) = unsafe { (&*sbase.add(sp - 1), &*sbase.add(sp - 2)) };
                let ok = if long_form {
                    matches!((av, bv), (Value::Long(_), Value::Long(_))) && bv.as_i64() != 0
                } else {
                    matches!((av.as_int(), bv.as_int()), (Some(_), Some(v)) if v != 0)
                };
                if !ok {
                    sync!();
                    #[cfg(feature = "next-gen-gc")]
                    if census {
                        crate::opcodes::hot_census::note_cold(op);
                    }
                    return Ok(HotExit::Cold);
                }
                let out = if long_form {
                    let (b, a) = (bv.as_i64(), av.as_i64());
                    Value::Long(if op == 0x6d { a.wrapping_div(b) } else { a.wrapping_rem(b) })
                } else {
                    let (b, a) = (bv.as_int().unwrap(), av.as_int().unwrap());
                    Value::Int(if op == 0x6c { a.wrapping_div(b) } else { a.wrapping_rem(b) })
                };
                sp -= 2;
                push!(out);
                *pc = pc_now + 1;
            }
            0xb5 if opgate(fast, hot_pf, hot_putfield) => {
                let idx = u16::from_be_bytes([bc[pc_now + 1], bc[pc_now + 2]]);
                // JVMS order: the value is on top, the objectref beneath it.
                let value = pop!();
                let recv = pop!();
                // Publish the operand length BEFORE a call that can allocate and collect: the
                // frame is a GC root and its stack must be the right length when the walk runs.
                sync!();
                crate::opcodes::fields::putfield_value(vm, class, idx, recv, value, bc, pc_now)?;
                // `putfield` pushes nothing, but a collection may still have run inside it, so the
                // cursor is re-read on the same discipline the `getfield` arm uses.
                // Re-read BOTH bases: `vm.` re-entry may have grown the shared buffer.
                lbase = buf.as_mut_ptr();
                sbase = unsafe { lbase.add(nloc) };
                cap = buf.capacity() - nloc;
                *pc = pc_now + 3;
            }
            0xb4 if opgate(fast, hot_gf, hot_getfield) => {
                let idx = u16::from_be_bytes([bc[pc_now + 1], bc[pc_now + 2]]);
                let recv = pop!();
                // Publish the operand length BEFORE a call that can allocate and collect: the
                // frame is a GC root and its stack must be the right length when the walk runs.
                sync!();
                let v = crate::opcodes::fields::getfield_value(vm, class, idx, recv, bc, pc_now)?;
                // A collection forwards values IN PLACE and never reallocates this buffer, but the
                // cursor is re-read anyway — the same discipline `push!`'s grow arm uses.
                // Re-read BOTH bases: `vm.` re-entry may have grown the shared buffer.
                lbase = buf.as_mut_ptr();
                sbase = unsafe { lbase.add(nloc) };
                cap = buf.capacity() - nloc;
                push!(v);
                *pc = pc_now + 3;
            }
            _ => {
                sync!();
                {
                    #[cfg(feature = "next-gen-gc")]
                    if census {
                        crate::opcodes::hot_census::note_cold(op);
                        // Attribute this run's opcodes to the class whose body executed them.
                        if crate::opcodes::hot_census::class_census() {
                            crate::opcodes::hot_census::note_class_ops(
                                &class.file.this_class,
                                ops_here,
                            );
                        }
                    }
                    return Ok(HotExit::Cold);
                }
            }
        }
    }
}

/// The `[UNDERFLOW]` diagnostic, out of line and taking the two frame fields it prints.
#[cold]
#[inline(never)]
fn underflow(class: &crate::class_loader::LoadedClass, pc: usize) -> VmError {
    if std::env::var("RAVA_TRACE_UNDERFLOW").is_ok() {
        eprintln!("[UNDERFLOW] {} pc={pc}", class.file.this_class);
    }
    VmError::StackUnderflow
}

/// As `underflow`, for a stack slot that was not an int where one was required.
#[cold]
#[inline(never)]
fn not_an_int(class: &crate::class_loader::LoadedClass, pc: usize, v: &Value) -> VmError {
    if std::env::var("RAVA_TRACE_UNDERFLOW").is_ok() {
        eprintln!("[POPINT-FAIL] {} pc={pc} got non-int {v:?}", class.file.this_class);
    }
    VmError::StackUnderflow
}

/// A local index past `max_locals` — malformed bytecode. `Vec` indexing panicked here; this
/// reports it as an error instead of taking the process down with it.
#[cold]
#[inline(never)]
fn bad_local(class: &crate::class_loader::LoadedClass, pc: usize, i: usize) -> VmError {
    eprintln!("[BAD-LOCAL] {} pc={pc} local {i} past max_locals", class.file.this_class);
    VmError::StackUnderflow
}
