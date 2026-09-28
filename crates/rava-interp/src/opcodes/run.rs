//! The interpreter loop: one match over the bytecode, executed frame by frame.
//!
//! Re-exported from the parent module, so no call site outside it changed.
//!
//! ⭐**THE ENTRY IS THE PRICE, NOT THE SWITCH.**
//!
//! A product boot executes **14.1M method bodies** in here. The dispatcher itself is closed: on a
//! real 67M-opcode stream a threaded dispatcher measured 2.252 ns/op against the switch's 2.267
//! (row 413), i.e. 0.7%, so nothing below is allowed to touch the `match`. What was never priced
//! is what every ENTRY pays before the first opcode runs, and what every opcode pays OUTSIDE the
//! `match`. Per body, that used to be:
//!
//! * **seven `OnceLock<bool>` reads** — `pc_profile::enabled`, `backedge::enabled` (twice) and the
//!   five disjuncts of `branch_cold` (`backedge`, `fork_numeric`, `fork_target_scan`,
//!   `fork_numeric_probe`, `chunk_fill_ready`). Every one is an acquire load, a compare, a branch
//!   and a call whose cold arm does not inline. Every one is derived from an environment variable
//!   that cannot change after `Vm::new`.
//! * an eighth `OnceLock` read for the `interp_self` meter handle, plus a 32-byte `MeterGuard`
//!   written to the stack and a `Drop` check planted at EVERY one of this function's ~20 exits.
//! * `vm.trace.ops.clone()` — an `Option<String>` that OWNS a buffer, so the compiler must plant
//!   drop glue and an unwind cleanup pad at each of those same exits, for a value that is `None`
//!   in every run nobody is tracing.
//! * a thread-local take/replace of `RUN_STOP_PC`.
//!
//! ⭐The last one is the one worth naming, because it is not a shave — it is dead work with a
//! proof. `RUN_STOP_PC` is written by exactly two callers (`vm/fork_chunk.rs`,
//! `vm/fork_counted.rs`), and both are reached from exactly two places: the `goto` arms below,
//! behind `fork_numeric() || fork_target_scan() || chunk_fill_ready()` — three of `branch_cold`'s
//! five disjuncts. **So when `branch_cold` is false the bound is provably `usize::MAX`**, and with
//! it the per-body thread-local access, the `pc >= stop_pc` compare per cold opcode and the
//! `bounded` test that decides whether the hot loop may run at all are all paying for a state that
//! cannot occur.
//!
//! ⇒ The whole entry collapses to **one relaxed byte load**. [`run`] reads [`PRE`], a snapshot of
//! every one of those env-statics taken once, and forwards: if any instrument is armed — or the
//! kill switch is set — it goes to [`run_instrumented`], which is the old preamble verbatim;
//! otherwise straight into [`run_body`] with `branch_cold = false` and `stop_pc = usize::MAX`
//! passed as ordinary arguments, which is what makes the thread-local access disappear rather than
//! merely become predictable. `bounded` moves out of the loop, where it was recomputed per opcode
//! from two loop-invariant values.
//!
//! Restored if this is reverted: seven-to-eight atomic loads and one owning heap local per method
//! body, one TLS access per method body, and one compare per cold opcode.
//!
//! Kill switch: **`RAVA_NO_FAST_INTERPLOOP=1`** (and `RAVA_NO_EMIT_MORE=1`) forces every body down
//! [`run_instrumented`], which is byte-for-byte the pre-change path.
//!
//! ---
//!
//! ⭐**SECOND ROUND: THE COLD OPCODE PAID ITS SAFEPOINT TWICE, AND READ ITS BYTECODE THROUGH A
//! `Vec` HEADER.**
//!
//! Nothing here touches the `match` — the dispatcher line stays closed (row 413: 2.252 ns/op
//! threaded against 2.267 switched, 0.7%). Both entries below are already one byte load, so the
//! only work left to find was what a COLD opcode pays between leaving `run_hot` and reaching its
//! arm. There was exactly one item, and it was a duplicate:
//!
//! * `run_hot`'s loop head does `if cd == 0 { Poll }` and then `cd -= 1` **before** its `match`,
//!   and every `HotExit::Cold` return in that function publishes `cd` through `sync!`. So the
//!   opcode's safepoint budget is already spent and stored by the time it arrives here — and this
//!   loop then read the thread-local again, subtracted one again, and wrote it back. Two TLS
//!   accesses (three dependent loads each on Windows) per cold opcode, on the population row 349
//!   counts at 1,286,515,599 hot-loop exits per boot. **Gone on every unbounded body**; kept
//!   verbatim for the bounded fork worker, which skips `run_hot` and therefore owns the only
//!   decrement. The budget now drains at one per opcode instead of two, so `HotExit::Poll` — still
//!   the safepoint site, untouched — lands within `CHECK_INTERVAL` ops exactly as row 417 intends.
//! * `bc` was a `&Vec<u8>`, so every index and every `&[u8]` helper call re-`Deref`ed it: a
//!   pointer and a length loaded out of memory the compiler cannot assume survives `run_hot`
//!   (`&mut Vm`, and callees that reach the same `Code` through the raw `frame.code`). It is a
//!   slice now, taken once — an SSA fat pointer, so the bounds check on `bc[pc]` is a register
//!   compare instead of a load-then-compare.
//!
//! ⛔**Deliberately NOT touched, with the reason**: the five `backedge::enabled()` reads and the
//! four fork/chunk-fill probes in the branch arms below cost nothing in production — `run_hot`
//! serves every branch opcode itself and only hands one back when `branch_cold` is true, so those
//! arms are unreachable unless a census is armed. Adding a `branch_cold &&` in front of them would
//! be correct and buy zero. The `vm.pending` / `vm.yield_request` tests at the loop head are two
//! predictable discriminant loads and are the trampoline's suspend protocol; they stay.
//!
//! Kill switch: **`RAVA_NO_FAST_INTERPLOOP2=1`** (and `RAVA_NO_EMIT_MORE=1`) restores the
//! duplicate per-cold-opcode block. The slice is a type change with no behavioural side, so it is
//! not behind it.

use crate::opcodes::ops::*;
use crate::opcodes::*;
use std::sync::atomic::{AtomicU8, Ordering::Relaxed};

/// Packed one-time snapshot of the env-statics the entry path used to re-derive per method body.
///
/// **TOTAL TABLE SIZE: 1 byte.** A single `AtomicU8`, read relaxed and written once. Two threads
/// racing to initialise it compute the same value from the same environment, so no lock is needed
/// and a doubled initialisation is harmless. It is deliberately a lone byte in `.bss` rather than
/// a field of anything this loop writes: it is read 14.1M times a boot and must never be the
/// reason a line goes shared→modified.
static PRE: AtomicU8 = AtomicU8::new(0);
/// Snapshot taken. While clear, the other bits mean nothing.
const PRE_READY: u8 = 1 << 0;
/// Some instrument is armed, or the kill switch is on ⇒ take the old preamble.
const PRE_SLOW: u8 = 1 << 1;
/// `RAVA_NO_FAST_INTERPLOOP2=1` (or `RAVA_NO_EMIT_MORE=1`) ⇒ keep the DUPLICATE per-cold-opcode
/// checkpoint block in [`run_body`]. Lives in the same byte as the two above so a body still pays
/// one relaxed load, not a second `OnceLock` acquire — the exact cost this file exists to delete.
const PRE_DUP: u8 = 1 << 2;

/// One relaxed byte load on the entry path. `true` ⇒ use [`run_instrumented`].
#[inline(always)]
fn pre_slow() -> bool {
    let p = PRE.load(Relaxed);
    if p & PRE_READY != 0 {
        return p & PRE_SLOW != 0;
    }
    pre_init()
}

/// Is the duplicate per-cold-opcode checkpoint block still wanted? One relaxed byte load per
/// METHOD BODY, off the line [`pre_slow`] already touched.
///
/// [`PRE`] is guaranteed `PRE_READY` at every call site: both entries into [`run_body`] are
/// downstream of [`pre_slow`], which initialises it. Reading it without the ready test is therefore
/// not a race but a proof — and if that ever stopped holding, the byte reads 0, `PRE_DUP` is clear,
/// and the answer is the fast one, which is the answer this arm wants anyway.
#[inline(always)]
fn dup_ops() -> bool {
    PRE.load(Relaxed) & PRE_DUP != 0
}

/// Taken once per process (or once per thread that races in — the computation is pure and reads
/// only environment-derived `OnceLock`s, so every racer stores the same byte).
#[cold]
#[inline(never)]
fn pre_init() -> bool {
    let slow = crate::env_flag!("RAVA_NO_FAST_INTERPLOOP")
        || crate::flags::emit_more_disabled()
        // The meter's own `start()` is already a no-op when disarmed, but the handle read, the
        // 32-byte guard and its drop check at every exit are not — so an armed meter buys the
        // whole slow preamble rather than being tested inside the fast one.
        || crate::diag::interp_self().armed()
        || crate::pc_profile::enabled()
        // Always `false` in a production build; a real category guard under `wall-profile`.
        || crate::rava_sampler::enabled()
        || branch_cold_live();
    // Independent of `slow`: the instrumented preamble and the fast one share ONE `run_body`, so
    // the second-round kill switch has to be readable from inside it. It is deliberately NOT
    // folded into `slow` — an armed meter must not silently re-enable a double decrement, and the
    // kill switch must not drag a body onto the whole old preamble.
    let dup = crate::env_flag!("RAVA_NO_FAST_INTERPLOOP2") || crate::flags::emit_more_disabled();
    PRE.store(
        PRE_READY | if slow { PRE_SLOW } else { 0 } | if dup { PRE_DUP } else { 0 },
        Relaxed,
    );
    slow
}

/// Does a branch have to come back to THIS loop? Only for the probes that hang off one: the
/// back-edge census, the two counted-loop fork detectors, the chunk-fill detector and the
/// forward-goto numeric probe. Every one is an env-static `OnceLock`, which is why the disjunction
/// can be snapshotted into [`PRE`] once instead of re-read per method body — and why, when it is
/// false, `RUN_STOP_PC` is provably unbound (only the fork arms it guards ever set it).
#[cfg(feature = "next-gen-gc")]
#[inline]
fn branch_cold_live() -> bool {
    crate::backedge::enabled()
        || fork_numeric()
        || fork_target_scan()
        || fork_numeric_probe()
        || crate::chunk_fill_ready()
}

#[cfg(not(feature = "next-gen-gc"))]
#[inline]
fn branch_cold_live() -> bool {
    crate::backedge::enabled()
}

/// `RAVA_TRACE_OPS`, published ONCE for the process instead of cloned per method body.
///
/// `Vm::new` reads this straight out of the environment, so every `Vm` in the process holds the
/// same `Option<String>` — handing out a `&'static` to the first one observed is the same value,
/// not a different policy, which is why this side is not behind the kill switch. What it buys is
/// that [`run_body`] no longer OWNS a heap value: an `Option<String>` local plants drop glue and an
/// unwind cleanup pad at each of ~20 exits (every `?` included) for a buffer that exists only when
/// somebody is dumping every opcode.
///
/// TOTAL SIZE: one `OnceLock<Option<String>>` = 32 bytes of static, read once per body.
fn trace_ops_ref(vm: &crate::vm::thread_context::SharedVm) -> &'static Option<String> {
    static T: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    T.get_or_init(|| vm.trace.ops.clone())
}

/// The transitional `&SharedVm -> &mut Vm` escape for the three genuinely-`&mut` callees left on
/// this loop's path (`run_hot`, `checkpoint`, and the fork entry points) — the SAME cast the
/// invoke* family and `getstatic_resolve`/`putstatic` spell at their sites. Everything else in
/// `run_body` is already `&self`-safe: the `vm.pending` / `vm.yield_request` / `vm.gc_enabled` /
/// `vm.trace.*` / `vm.mem_limit` reads at the loop head and preamble, `vm.ldc`'s pool reads, and
/// every `vm.heap.*` read; `guard!`/`throw_exc!` call `find_handler(&Vm)` directly. This helper
/// mirrors `invoke.rs`'s definition verbatim.
#[allow(clippy::mut_from_ref, invalid_reference_casting)]
fn vm_mut(vm: &crate::vm::thread_context::SharedVm) -> &mut Vm {
    unsafe {
        &mut *(vm as *const crate::vm::thread_context::SharedVm as *mut crate::vm::thread_context::SharedVm)
    }
}

pub(crate) fn run(vm: &crate::vm::thread_context::SharedVm, code: &Code, frame: &mut Frame) -> Result<RunExit, VmError> {
    // 20.3 wall-profile: pure bytecode interpretation. Nested invokes push their own category
    // (VM_DISPATCH → the callee's tier), so this attributes only THIS frame's own opcode time.
    // A zero-sized guard in a production build, so `run` stays a tail-call dispatcher.
    let _wp = crate::rava_sampler::enter(crate::rava_sampler::INTERP_EXEC);
    if pre_slow() {
        return run_instrumented(vm, code, frame);
    }
    // FAST ENTRY. Every predicate the old preamble read is off, so there is nothing left to read:
    // `false` and `usize::MAX` are exactly the values those eight atomic loads and that
    // thread-local take would have produced.
    run_body(vm, code, frame, false, usize::MAX)
}

/// The pre-change preamble, verbatim, for the runs that need it. `#[cold]` + `#[inline(never)]`
/// keeps the meter guard, the `String` clone and their cleanup pads out of [`run`] entirely.
#[cold]
#[inline(never)]
fn run_instrumented(vm: &crate::vm::thread_context::SharedVm, code: &Code, frame: &mut Frame) -> Result<RunExit, VmError> {
    // The interpreter's own share, as a self-time meter: nested invokes open their own guard, so
    // this counts THIS frame's opcodes and not its callees'.
    let _mt = crate::diag::interp_self().start();
    // the configuration the interpreter work is measured in. Arm and start it here too; both
    // calls are one cached bool when the flag is off.
    if crate::pc_profile::enabled() {
        crate::pc_profile::start_sampler_if_on();
        crate::pc_profile::arm_current_thread();
    }
    // TAKE the bound (reset the thread-local to unbounded) so a NESTED run() — a callee invoked
    // from a bounded body — runs to its own return, not this frame's body-end pc. Only the
    // outermost (body) run is bounded; the worker re-arms the bound before each index iteration.
    #[cfg(feature = "next-gen-gc")]
    let stop_pc = crate::opcodes::exit::RUN_STOP_PC.with(|c| c.replace(usize::MAX));
    #[cfg(not(feature = "next-gen-gc"))]
    let stop_pc = usize::MAX;
    run_body(vm, code, frame, branch_cold_live(), stop_pc)
}


/// The loop itself. One copy, called from both entries — `#[inline(never)]` so the cold entry
/// cannot duplicate a function this size into the I-cache beside the hot one.
#[inline(never)]
fn run_body(
    vm: &crate::vm::thread_context::SharedVm,
    code: &Code,
    frame: &mut Frame,
    branch_cold: bool,
    stop_pc: usize,
) -> Result<RunExit, VmError> {
    #[cfg(not(feature = "next-gen-gc"))]
    let _ = stop_pc;
    // ⭐**A SLICE, NOT A `&Vec`.** `code.bytecode` is a `Vec<u8>`; every `bc[pc]` and every one of
    // the six helpers below that take `&[u8]` had to run `Deref` on it, i.e. re-load the pointer
    // and the length out of the `Vec` header — memory the compiler may not keep in registers
    // across `run_hot`, which takes `vm` by `&mut` and whose callees reach the same `Code` through
    // `frame.code` (a raw pointer, so no `noalias` conclusion is available). Converting ONCE makes
    // the fat pointer an SSA value: the bounds check on `bc[pc]` becomes a register compare
    // against a length that provably cannot have changed, instead of a dependent load first.
    // Restored if reverted: two loads before the bounds check of every cold opcode.
    let bc: &[u8] = code.bytecode.as_slice();
    // Register this frame so the GC can scan its locals/stack as roots. The
    // Frame is owned by the caller (`invoke`) and lives, unmoved, for the whole
    // body; the guard pops the entry on every exit path.
    crate::frames::push(std::ptr::addr_of!(*frame) as usize);
    let _frame_guard = FramePtrGuard;
    // Run a call that may throw: on a Java exception, jump to a matching handler
    // in THIS frame (clearing the stack, pushing the exception), else propagate.
    macro_rules! guard {
        ($call:expr) => {
            match $call {
                Ok(x) => x,
                Err(VmError::Throw(v)) => match find_handler(vm, frame, code, frame.pc, &v)? {
                    Some(h) => { frame.clear_stack(); frame.push(v); frame.pc = h; continue; }
                    None => return Err(VmError::Throw(v)),
                },
                Err(e) => return Err(e),
            }
        };
    }
    // Raise a Java exception value: catch in this frame or propagate.
    macro_rules! throw_exc {
        ($v:expr) => {{
            let v = $v;
            match find_handler(vm, frame, code, frame.pc, &v)? {
                Some(h) => { frame.clear_stack(); frame.push(v); frame.pc = h; continue; }
                None => return Err(VmError::Throw(v)),
            }
        }};
    }
    // Trace flags cached in Vm::new — reading env per run() was a syscall on every method
    // invocation. BORROWED from a process-wide publication rather than cloned per body: an owning
    // `Option<String>` local costs drop glue and a cleanup pad at every exit of this function, and
    // it is `None` in every run that is not being traced. See `trace_ops_ref`.
    let trace_ops = trace_ops_ref(vm);
    // A register-resident copy of the discriminant. `trace_ops` lives behind a `&'static` and
    // `run_hot` takes `vm` by `&mut`, so nothing lets the compiler assume the option survives the
    // call unchanged — it would reload it per cold opcode. A `bool` local whose address never
    // escapes cannot alias, so this one stays in a register.
    let trace_on = trace_ops.is_some();
    let heartbeat = vm.trace.heartbeat;
    let mem_limit = vm.mem_limit;
    // GC runs by default (gc_enabled); heartbeat/mem_limit also force the check
    // loop on. Only RAVA_GC=0 turns collection off entirely.
    let check_enabled = vm.gc_enabled || heartbeat || mem_limit != 0;
    // Loop-invariant: `stop_pc` is a binding nothing below writes. It used to be recomputed at the
    // top of every iteration — one compare per COLD opcode for a value fixed for the whole body,
    // and on the fast entry a constant `false`.
    #[cfg(feature = "next-gen-gc")]
    let bounded = stop_pc != usize::MAX;
    #[cfg(not(feature = "next-gen-gc"))]
    let bounded = false;
    // ⭐**THE COLD OPCODE'S SAFEPOINT BUDGET WAS DECREMENTED TWICE.**
    //
    // `run_hot`'s loop head reads its opcode, tests `cd == 0` (→ `HotExit::Poll`) and then does
    // `cd -= 1` — BEFORE its `match`. So by the time an arm decides the opcode is not one it
    // serves, the budget has already been spent on it, and every single `return Ok(HotExit::Cold)`
    // in that function goes through `sync!()`, which publishes `cd` back into the thread-local
    // (that discipline is enforced there by a comment on each exit; all seven were checked).
    // This loop then charged the SAME opcode again: `op_countdown_seed()` (a TLS read),
    // `saturating_sub(1)`, `op_countdown_store()` (a TLS write).
    //
    // **Was: two TLS accesses + a store per cold opcode, for a decrement already made. Is: none.**
    // On Windows x86_64 a `thread_local!` access is `_tls_index` → `gs:[0x58]` → index → offset,
    // i.e. three DEPENDENT loads, and this pays it twice on the population that row 349 counted at
    // 1,286,515,599 hot-loop exits in one boot — every field access, every call, every return.
    //
    // WHAT THE GUARD PROVES. The block may only be skipped where `run_hot` provably ran for this
    // very opcode, which is exactly `!bounded`: the sole `continue`-free path from the top of the
    // loop to the `match` below runs `run_hot` under `if !bounded`, and a `HotExit::Poll` return
    // `continue`s instead of reaching here. A BOUNDED run (the counted-loop fork worker's body
    // slice) skips the hot loop entirely, so there the block is the only decrement and stays.
    // Safepoint latency is unchanged in kind and strictly better defined: the budget now drains at
    // exactly one per opcode instead of two, so a poll still lands within `CHECK_INTERVAL` ops —
    // the guarantee the whole countdown exists for (row 417). The `HotExit::Poll` arm above, which
    // restocks and calls `checkpoint`, is untouched and remains the safepoint site.
    //
    // COLD ARM: `bounded` (the fork worker) and the kill switch both keep the old block verbatim.
    // Kill switch: `RAVA_NO_FAST_INTERPLOOP2=1`, or the block-wide `RAVA_NO_EMIT_MORE=1`.
    //
    // Hoisted out of the loop for the same reason `bounded` was: all three inputs are fixed for
    // the whole body, so recomputing them per opcode was a test for an answer that cannot change.
    let per_op_checkpoint = check_enabled && (bounded || dup_ops());
    // P2.1: LOCAL op countdown — the TLS cell is seeded once per run() instead of
    // being decremented on every opcode. The seed is a re-armed TLS value, and the
    // local only drains; nothing but this loop reads the TLS cell, so a missed
    // carry-over between run() boundaries just makes an occasional checkpoint fire
    // slightly early, never late (safepoint latency stays ≤ CHECK_INTERVAL ops).
    // ⭐**THE SAFEPOINT COUNTDOWN LIVES IN EXACTLY ONE PLACE** (row 417).
    //
    // It used to be a LOCAL, seeded from the thread-local and discarded on return, so every
    // frame's progress toward the next safepoint died with the frame. The first fix gave each
    // `run()` its own `Cell` and published it on exit — which was WORSE (5,040 checkpoints to
    // 244 on the same program), because an outer frame's stale value then overwrote everything
    // its callees had counted on the way out.
    //
    // A counter that several nested frames advance cannot be cached by any of them across a
    // call. `run_hot` may cache it — its loop exits to this one for every invoke, so nothing
    // nests inside it — and does, publishing through `sync!`. This loop reads and writes the
    // cell directly, once per COLD opcode, which is the rarer half.
    //
    // One ENTRY into this method. The back-edge census needs it beside the edge count: a hot
    // loop in a method entered MANY times is served by a plain back-edge trigger, one in a method
    // entered ONCE needs OSR, and only the pair distinguishes them. `branch_cold` is a superset of
    // `backedge::enabled()`, so testing it first turns the common case into a register test rather
    // than an eighth acquire load per body.
    if branch_cold && crate::backedge::enabled() {
        crate::backedge::note_entry(code as *const _ as usize, || {
            frame.class.file.this_class.to_string()
        });
    }
    loop {
        // Trampoline suspend point: a prior invoke staged a Java call (the caller
        // pc is already advanced past it). Hand control back to the driver, which
        // pushes the callee frame. Never set in the default recursive mode.
        if vm.pending.is_some() {
            return Ok(RunExit::Call);
        }
        // Cooperative yield point: a blocking native (Thread.sleep/Object.wait)
        // staged a yield. pc is already past the call, so resuming continues the
        // frame normally. Only set in trampoline (scheduler) mode.
        if vm.yield_request.is_some() {
            return Ok(RunExit::Yield);
        }
        // The bound is only ever ARMED for a bounded run, and an unbounded `stop_pc` is
        // `usize::MAX`, which `frame.pc` can never reach — so `bounded` (a register) subsumes the
        // test. Predicating on it removes a load of `frame.pc` and a compare from every COLD
        // opcode of every UNBOUNDED body, which is all of them outside the fork experiments. When
        // it IS armed the hot loop below is skipped, so `frame.pc` here is exactly the `pc` the
        // old separate read produced.
        #[cfg(feature = "next-gen-gc")]
        if bounded && frame.pc >= stop_pc {
            return Ok(RunExit::Bounded); // counted-loop fork: body slice done for this index
        }
        // THE HOT SUBSET RUNS ELSEWHERE (`opcodes::hot`), in a function whose frame is small
        // enough to keep the interpreter's pointers in registers. It returns here for anything
        // it does not handle, for a backward branch, and when the op budget runs out.
        //
        // Skipped only for a BOUNDED run — the counted-loop fork worker's body slice, which must
        // stop at `stop_pc`; that limit lives in this loop, not in the hot one. `bounded` is
        // hoisted to the top of the function: it is derived from a binding nothing here writes, so
        // recomputing it per opcode was a compare for an answer that cannot change mid-body.
        if !bounded {
            match crate::opcodes::run_hot(vm, frame, bc, trace_ops, branch_cold)? {
                crate::opcodes::HotExit::Poll => {
                    crate::frames::op_countdown_restock();
                    // `check_enabled` is false only when collection, the heartbeat and the memory
                    // limit are ALL off (RAVA_GC=0). The budget still has to be restocked — the hot
                    // loop counts down unconditionally — but nothing may be checkpointed, or that
                    // configuration would start collecting.
                    if check_enabled {
                        if let Some(x) = checkpoint(vm_mut(vm), frame, trace_ops, heartbeat, mem_limit)? {
                            return Ok(x);
                        }
                    }
                    continue;
                }
                crate::opcodes::HotExit::Cold => {}
            }
        }
        let pc = frame.pc;
        let op = bc[pc];
        // `trace_on` first: a register test, where `trace_ops` is a memory load the compiler must
        // repeat after every `run_hot` call because that call takes `vm` by `&mut`.
        if trace_on {
            if let Some(filter) = trace_ops {
                if frame.class.file.this_class.contains(filter.as_str()) {
                    eprintln!("[OP] {} pc={pc} op=0x{op:02X} stack={}", frame.class.file.this_class, frame.stack_len());
                }
            }
        }
        // See `per_op_checkpoint` above: `false` exactly when `run_hot` already spent this
        // opcode's budget and published it, which is every unbounded body.
        if per_op_checkpoint {
            let cd = crate::frames::op_countdown_seed().saturating_sub(1);
            crate::frames::op_countdown_store(cd);
            if cd == 0 {
                crate::frames::op_countdown_restock();
                if let Some(x) = checkpoint(vm_mut(vm), frame, trace_ops, heartbeat, mem_limit)? {
                    return Ok(x);
                }
            }
        }
        match op {
            0x12 => { let v = vm_mut(vm).ldc(&frame.class, bc[pc + 1] as u16)?; frame.push(v); frame.pc += 2; } // ldc
            0x13 | 0x14 => { let v = vm_mut(vm).ldc(&frame.class, u16_operand(bc, pc))?; frame.push(v); frame.pc += 3; } // ldc_w/ldc2_w

            // loads with u8 index: i/l/f/d/a load (long/double single-slot here)
            // *load_0..3 for i/l/f/d/a (contiguous 0x1a..0x2d, index = op % 4 within family)

            // stores with u8 index: i/l/f/d/a store
            // *store_0..3 for i/l/f/d/a (contiguous 0x3b..0x4e)

            // arrays — loads (iaload/laload/faload/daload/aaload/baload/caload/saload)
            0x2e..=0x35 => {
                // Keep the SIGNED index for the message. `idx` is widened to `usize` so a negative
                // index fails the same `>= len` test the JVM requires — but printing the widened
                // value made a `catch` block's `getMessage()` read
                // "Index 18446744073709551615" where HotSpot says "Index -1". Same class, same
                // control flow, guest-visible text; found by `bench/ArrEx.java`.
                let idx_i = frame.pop_int()?;
                let idx = idx_i as usize;
                let v = match frame.pop()? {
                    // BOUNDS CHECK. Without it an out-of-range load pushed `Value::Null` and an
                    // out-of-range store was silently dropped — the JVM raises
                    // ArrayIndexOutOfBoundsException for both, and the COMPILED path in this
                    // engine already does (jit/callbacks/arrays.rs). A silently-wrong value is
                    // worse than a fatal one: it corrupts the computation instead of naming the
                    // site. `idx` is a usize widened from an i32, so a negative index arrives as a
                    // huge value and is caught by the same test, as the JVM requires.
                    Value::Array(h) => match vm.heap.array_get_bounded(h, idx) {
                        Ok(v) => v,
                        Err(len) => throw_exc!(vm.make_exception_shared(
                            "java/lang/ArrayIndexOutOfBoundsException",
                            &format!("Index {idx_i} out of bounds for length {len}"),
                        )),
                    },
                    #[cfg(feature = "next-gen-gc")]
                    Value::Obj(pointer)
                        if op == 0x32 && crate::nextgen::is_arena_ref_array(pointer) =>
                    {
                        let array = crate::heap::ArrayRef::Arena(pointer);
                        let len = array.len(&vm.heap).unwrap_or(0);
                        if idx >= len {
                            throw_exc!(vm.make_exception_shared(
                                "java/lang/ArrayIndexOutOfBoundsException",
                                &format!("Index {idx_i} out of bounds for length {len}"),
                            ))
                        }
                        match array.get(&vm.heap, idx) {
                            Some(v) => v,
                            None => throw_exc!(vm.make_exception_shared(
                                "java/lang/ArrayIndexOutOfBoundsException",
                                &format!("Index {idx_i} out of bounds for length {len}"),
                            )),
                        }
                    }
                    // NullPointerException, not AIOOBE: an index is never reached on a null
                    // array, and a guest `catch (NullPointerException)` behaves as it does on
                    // HotSpot only if the class matches.
                    Value::Null => {
                        // throw_exc!, not `return Err(Throw)`: returning unwinds OUT of run() and
                        // skips THIS frame's own exception table, so a guest try/catch around the
                        // access would never see it. The macro looks for a handler here first.
                        //
                        // The array is UNDER the index, so the walk is told one operand sits
                        // above it; the element type comes from the opcode, which is the only
                        // place it survives once the array is null.
                        let code = unsafe { &(*frame.code).bytecode };
                        let src = crate::npe::source_at(code, frame.pc, 1, &frame.class.file.pool);
                        let msg = crate::npe::array_load(
                            crate::npe::array_kind_for_op(code[frame.pc]), src);
                        throw_exc!(vm.make_exception_shared("java/lang/NullPointerException", &msg))
                    }
                    other => {
                        if crate::env_flag!("RAVA_TRACE_THROW") || crate::env_flag!("RAVA_TRACE_UNDERFLOW") {
                            let c = if let Value::Ref(h) = &other { vm.heap.class_of(*h).unwrap_or("?").to_string() } else { other.display() };
                            eprintln!("[BADARRAY] {} pc={} aaload on non-array: {c}", frame.class.file.this_class, frame.pc);
                        }
                        return Err(VmError::StackUnderflow);
                    }
                };
                frame.push(v);
                frame.pc += 1;
            }
            // arrays — stores (iastore/.../sastore)
            0x4f..=0x56 => {
                let value = frame.pop()?;
                let idx_i = frame.pop_int()?;
                let idx = idx_i as usize;
                match frame.pop()? {
                    Value::Array(h) => {
                        // Same lane barrier as the interpreted putfield and the compiled store:
                        // an array element write inside a ring task is only private if the worker
                        // owns the array. The compiled path needed its own arm for exactly this
                        // ("the field arm does not cover them", gilfree_cf/rings.rs); the
                        // interpreted store had none and wrote straight to the shared heap.
                        #[cfg(feature = "next-gen-gc")]
                        if crate::gilfree_cf::in_lane()
                            && !crate::parallel::cow_owned(&Value::Array(h))
                        {
                            crate::gilfree_cf::ABORT_SHARED_FIELD
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            crate::parallel::request_abort();
                            return Err(VmError::RegionAbort);
                        }
                        let len = vm.heap.array_len(h).unwrap_or(0);
                        if idx >= len {
                            throw_exc!(vm.make_exception_shared(
                                "java/lang/ArrayIndexOutOfBoundsException",
                                &format!("Index {idx_i} out of bounds for length {len}"),
                            ))
                        }
                        // aastore STORE CHECK (JVMS 6.5). Only 0x53 carries it —
                        // the primitive stores are type-fixed by their opcode. The
                        // authority is the array's OWN element descriptor, recorded
                        // at allocation, so this is the runtime component type
                        // HotSpot consults, not a static type. Without it a String
                        // landed in an Integer[] and the heap held a value the
                        // array's own type forbids, which every later read of that
                        // array then mis-typed silently.
                        if op == 0x53 {
                            if let Some(bad) = crate::opcodes::astore_reject(vm, h, &value) {
                                throw_exc!(vm.make_exception_shared("java/lang/ArrayStoreException", &bad))
                            }
                        }
                        vm_mut(vm).heap.array_set(h, idx, value)
                    }
                    #[cfg(feature = "next-gen-gc")]
                    Value::Obj(pointer) if crate::nextgen::is_arena_ref_array(pointer) => {
                        let array = crate::heap::ArrayRef::Arena(pointer);
                        let len = array.len(&vm.heap).unwrap_or(0);
                        if idx >= len {
                            throw_exc!(vm.make_exception_shared(
                                "java/lang/ArrayIndexOutOfBoundsException",
                                &format!("Index {idx_i} out of bounds for length {len}"),
                            ))
                        }
                        if op == 0x53 {
                            if let Some(bad) = array
                                .component_descriptor(&vm.heap)
                                .and_then(|elem| {
                                    crate::opcodes::astore_reject_for_element(
                                        vm,
                                        elem.as_ref(),
                                        &value,
                                    )
                                })
                            {
                                throw_exc!(vm.make_exception_shared(
                                    "java/lang/ArrayStoreException",
                                    &bad,
                                ))
                            }
                        }
                        let _ = array.store(&mut vm_mut(vm).heap, idx, value);
                    }
                    // Catchable, like its neighbours: the load arm above and arraylength both
                    // throw. Only the store killed the process.
                    Value::Null => {
                        // throw_exc!, not `return Err(Throw)`: returning unwinds OUT of run() and
                        // skips THIS frame's own exception table, so a guest try/catch around the
                        // access would never see it. The macro looks for a handler here first.
                        //
                        // An array store's operands are [array, index, value], so TWO sit above
                        // the null one.
                        let code = unsafe { &(*frame.code).bytecode };
                        let src = crate::npe::source_at(code, frame.pc, 2, &frame.class.file.pool);
                        let msg = crate::npe::array_store(
                            crate::npe::array_kind_for_op(code[frame.pc]), src);
                        throw_exc!(vm.make_exception_shared("java/lang/NullPointerException", &msg))
                    }
                    other => {
                        if crate::env_flag!("RAVA_TRACE_UNDERFLOW") {
                            eprintln!("[BADASTORE] {} pc={} astore on non-array {:?}", frame.class.file.this_class, frame.pc, other);
                        }
                        return Err(VmError::StackUnderflow);
                    }
                }
                frame.pc += 1;
            }
            0xbc | 0xc2 | 0xc3 | 0xc0 | 0xc1 | 0xbd | 0xbe => {
                if let Some(x) = objects(vm, frame, code, bc, pc, op)? {
                    return Ok(x);
                }
            }
            0xbf => {
                if let Some(x) = athrow(vm, frame, code, bc, pc, op)? {
                    return Ok(x);
                }
            }

            // int arithmetic
            0x6c => { // idiv
                let (b, a) = (frame.pop_int()?, frame.pop_int()?);
                if b == 0 { throw_exc!(vm.make_exception_shared("java/lang/ArithmeticException", "/ by zero")); }
                frame.push(Value::Int(a.wrapping_div(b))); frame.pc += 1;
            }
            0x70 => { // irem
                let (b, a) = (frame.pop_int()?, frame.pop_int()?);
                if b == 0 { throw_exc!(vm.make_exception_shared("java/lang/ArithmeticException", "/ by zero")); }
                frame.push(Value::Int(a.wrapping_rem(b))); frame.pc += 1;
            }

            // int bitwise/shift

            // long arithmetic
            0x6d => { let (b, a) = (frame.pop()?.as_i64(), frame.pop()?.as_i64()); if b == 0 { throw_exc!(vm.make_exception_shared("java/lang/ArithmeticException", "/ by zero")); } frame.push(Value::Long(a.wrapping_div(b))); frame.pc += 1; } // ldiv
            0x71 => { let (b, a) = (frame.pop()?.as_i64(), frame.pop()?.as_i64()); if b == 0 { throw_exc!(vm.make_exception_shared("java/lang/ArithmeticException", "/ by zero")); } frame.push(Value::Long(a.wrapping_rem(b))); frame.pc += 1; } // lrem

            // float arithmetic

            // double arithmetic

            // conversions

            // comparisons → push -1/0/1

            // unary branches (compare to zero)
            0x99..=0x9e => {                                        // ifeq..ifle
                let v = frame.pop_int()?;
                let take = match op { 0x99 => v == 0, 0x9a => v != 0, 0x9b => v < 0, 0x9c => v >= 0, 0x9d => v > 0, _ => v <= 0 };
                let t_ = if take { (pc as i32 + branch_offset(bc, pc)) as usize } else { pc + 3 };
                if crate::backedge::enabled() && take && t_ < pc {
                    crate::backedge::note(code as *const _ as usize, || {
                        format!("{}", frame.class.file.this_class)
                    });
                }
                frame.pc = t_;
            }
            // binary int compares
            0x9f..=0xa4 => {                                        // if_icmpeq..if_icmple
                let (b, a) = (frame.pop_int()?, frame.pop_int()?);
                let take = match op { 0x9f => a == b, 0xa0 => a != b, 0xa1 => a < b, 0xa2 => a >= b, 0xa3 => a > b, _ => a <= b };
                let t_ = if take { (pc as i32 + branch_offset(bc, pc)) as usize } else { pc + 3 };
                if crate::backedge::enabled() && take && t_ < pc {
                    crate::backedge::note(code as *const _ as usize, || {
                        format!("{}", frame.class.file.this_class)
                    });
                }
                frame.pc = t_;
            }
            0xa5 | 0xa6 => {                                        // if_acmpeq / if_acmpne
                let (b, a) = (frame.pop()?, frame.pop()?);
                let eq = ref_equal(&a, &b);
                let take = if op == 0xa5 { eq } else { !eq };
                let t_ = if take { (pc as i32 + branch_offset(bc, pc)) as usize } else { pc + 3 };
                if crate::backedge::enabled() && take && t_ < pc {
                    crate::backedge::note(code as *const _ as usize, || {
                        format!("{}", frame.class.file.this_class)
                    });
                }
                frame.pc = t_;
            }
            0xa7 => {
                let target = (pc as i32 + branch_offset(bc, pc)) as usize;
                // M3 T2 step 3a: at a FORWARD goto (the leading `goto COND` of a counted loop),
                // probe whether it is an offloadable index-partitioned counted loop. Diagnostic
                // only (RAVA_FORK_NUMERIC_PROBE) — traces each loop site once, no fork.
                #[cfg(feature = "next-gen-gc")]
                if fork_numeric_probe() {
                    // Forward goto → bottom-test shape; backward goto → top-test shape (the common
                    // javac form). Try whichever applies.
                    let m = if target > pc {
                        crate::jit::match_numeric_range_loop(bc, pc)
                    } else {
                        crate::jit::match_numeric_range_loop_toptest(bc, pc)
                    };
                    if let Some(m) = m {
                        crate::Vm::debug_assert_not_gilfree("numeric_probe_seen");
                        if vm_mut(vm).numeric_probe_seen.insert((bc.as_ptr() as usize, pc)) {
                            let shape_ok = crate::offload::counted_loop_offloadable(
                                code, &frame.class.file.pool, Some(frame.class.file.bootstrap_methods.as_slice()),
                                m.iv_local, &mut |_: crate::offload::CalleeQuery| true);
                            eprintln!("[NUMERIC-LOOP] pc={pc} iv={} bound={:?} step={} body={}..{} offloadable-shape={shape_ok}",
                                m.iv_local, m.bound, m.step, m.body_start_pc, m.body_end_pc);
                        }
                    }
                }
                // M3 T2 step 3b: at a BACKWARD goto (top-test loop back-edge), try to fork the
                // counted loop's remaining iterations. On success `frame.pc` is set to the final
                // iteration's body (run serially here) — do NOT take the normal back-edge.
                // (The RAVA_FORK_COLUMN prototype that used to sit here was RETIRED at GAP-S09
                // stage 5 — superseded by the S2 chunk-fill fork; see the gap register.)
                #[cfg(feature = "next-gen-gc")]
                if target < pc && (fork_numeric() || fork_target_scan()) {
                    match vm_mut(vm).try_fork_counted_loop(frame, code, pc) {
                        Ok(true) => continue,
                        Ok(false) => {}
                        Err(e) => return Err(e),
                    }
                }
                // M3 10.2a: chunk-fill 3D coord-map detector (ECHO — logs + returns false, byte-identical).
                #[cfg(feature = "next-gen-gc")]
                if target < pc && crate::chunk_fill_ready() {
                    match vm_mut(vm).try_fork_chunk_fill_loop(frame, code, pc) {
                        Ok(true) => continue,
                        Ok(false) => {}
                        Err(e) => return Err(e),
                    }
                }
                if crate::backedge::enabled() && target < pc {
                    crate::backedge::note(code as *const _ as usize, || {
                        format!("{}", frame.class.file.this_class)
                    });
                }
                frame.pc = target;
            } // goto
            // ⭐Both decoders now live in `opcodes::switch`. `tableswitch` is a relocation only —
            // it was already O(1) in the label count. `lookupswitch` **was O(npairs) per
            // execution and is O(log npairs) on a hit**, where `npairs` is the number of case
            // labels the GUEST wrote (javac lowers a String switch to one pair per case, which is
            // the codec/registry dispatch shape). Reads the same bytes out of `bc`; no table, no
            // memo, no allocation. Kill switch `RAVA_NO_FAST_LDC`.
            //
            // ⚠The pop moved AHEAD of the table read (the helper owns the read). Both are
            // side-effect-free, so the only difference is which failure wins on a class file that
            // is both stack-inconsistent AND has a truncated table: a `VmError` instead of an
            // index panic. No valid class reaches either.
            0xaa => { // tableswitch
                let idx = frame.pop_int()?;
                frame.pc = (pc as i32 + tableswitch_target(bc, pc, idx)) as usize;
            }
            0xab => { // lookupswitch
                let key = frame.pop_int()?;
                frame.pc = (pc as i32 + lookupswitch_target(bc, pc, key)) as usize;
            }
            0xc6 | 0xc7 => {                                        // ifnull / ifnonnull
                let is_null = matches!(frame.pop()?, Value::Null);
                let take = if op == 0xc6 { is_null } else { !is_null };
                let t_ = if take { (pc as i32 + branch_offset(bc, pc)) as usize } else { pc + 3 };
                if crate::backedge::enabled() && take && t_ < pc {
                    crate::backedge::note(code as *const _ as usize, || {
                        format!("{}", frame.class.file.this_class)
                    });
                }
                frame.pc = t_;
            }

            // returns
            0xac | 0xad | 0xae | 0xaf | 0xb0 => return Ok(RunExit::Normal(Some(frame.pop()?))), // i/l/f/d/areturn
            0xb1 => return Ok(RunExit::Normal(None)),                // return

            0xb2 => { let v = guard!(getstatic(vm, frame, u16_operand(bc, pc))); frame.push(v); frame.pc += 3; } // getstatic
            0xb3 => { guard!(putstatic(vm, frame, u16_operand(bc, pc))); frame.pc += 3; } // putstatic
            0xb4 => { guard!(getfield(vm, frame, u16_operand(bc, pc))); frame.pc += 3; }  // getfield
            0xb5 => { guard!(putfield(vm, frame, u16_operand(bc, pc))); frame.pc += 3; }  // putfield
            0xb6 | 0xb9 | 0xb7 | 0xb8 | 0xba | 0xbb => {
                if let Some(x) = invoke(vm_mut(vm), frame, code, bc, pc, op)? {
                    return Ok(x);
                }
            }
            0xc4 | 0xc5 => {
                if let Some(x) = wide(vm, frame, code, bc, pc, op)? {
                    return Ok(x);
                }
            }
            other => return Err(VmError::UnknownOpcode(other)),
        }
    }
}
