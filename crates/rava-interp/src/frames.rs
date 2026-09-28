//! Per-thread GC frame-root registry. Each OS thread keeps its OWN stack of live
//! `Frame*` (a single shared Vec would corrupt: when threads interleave mid-run
//! — main pauses at a GIL hand-off with frames on the stack, the daemon runs and
//! pauses with ITS frames on top — a later pop on one thread would remove the
//! other's entry). GC (run by the GIL holder; every other thread is parked at
//! `gil::acquire` with its frames alive) scans all registered thread stacks.

use std::cell::{Cell, RefCell};
use std::sync::Mutex;

/// The per-thread frame state that needs a destructor, in ONE struct — so the paths
/// that touch several of these pay ONE thread-local access rather than one each.
///
/// RIP sampling puts `Vm::jit_direct_invoke` at 14.9% of the ring's `LocalKey::with`
/// cluster; it reaches it through `box_len` and `box_truncate`, and the latter touched
/// `BOXES` and then `CTX` — two accesses for one logical operation. `box_push` did the
/// same. Grouping them is the fix for that half; the pointer cursor behind [`tl`]
/// removes what remains of the access itself.
///
/// Only droppable state moves here. `DEPTH`, `NATIVE_DEPTH` and `OP_COUNTDOWN` stay as
/// plain `Cell`s: a `Cell<u32>` with a `const` initialiser has no destructor, which is
/// exactly the shape `std` already compiles to a bare TLS load — they were never on the
/// profile and there is nothing to win by moving them.
///
/// Interior addresses are as stable as they were: the struct lives in this thread's
/// TLS block for the thread's lifetime, so `LOCAL`'s and `CTX`'s addresses — which
/// `register` publishes into the cross-thread GC-root registries — behave exactly as
/// the standalone statics did, including the `unregister`-before-exit contract.
pub(crate) struct Locals {
    /// This thread's live `Frame*` stack — the GC's interpreter root set.
    ///
    /// ⚠`UnsafeCell`, not `RefCell`, for the same reason the frame-buffer free list ([`PoolSlot`])
    /// is. This stack is
    /// pushed and popped once per interpreted call AND once per general-path compiled→host
    /// crossing (`jit_call_body`'s synthetic frame), and the borrow flag it used to carry was a
    /// read, a write and a branch on each of those for a re-entrancy that cannot happen:
    /// [`push`], [`pop`] and [`local`] run no user code, call nothing that could reach this cell,
    /// and the cell is per-thread. Nothing may hold a borrow of it across a call to anything
    /// else, which is why all three are one-liners.
    ///
    /// The flag never protected the interesting case anyway: [`all`] — the collector's actual
    /// reader — has always gone through the raw address published into `REG`, ignoring the
    /// `RefCell` entirely. Its safety argument is the parked-owner one stated on [`all`], not the
    /// borrow flag, and nothing about it changes here.
    pub(crate) local: std::cell::UnsafeCell<Vec<usize>>,
    /// Reusable GC root-gather buffers (locs / ptrs / cell_locs). A MAJOR
    /// collection's full arena-cell scan reaches ~256MB; allocating a FRESH Vec
    /// that large each collection doubled (grow_one) and spiked transient memory,
    /// aborting under the Windows commit cap. Persisting the buffers across
    /// collections grows them ONCE to the worldgen peak and reuses the capacity —
    /// no per-collection realloc, no doubling spike — which lets forced majors run
    /// (keeping old-gen small → fast rem-set scans) without OOM. Per-thread (GC is
    /// GIL-synchronous on the collecting thread); cleared on each take.
    #[cfg(feature = "next-gen-gc")]
    gc_gather: RefCell<(Vec<*mut crate::value::Value>, Vec<*mut u8>, Vec<*mut i64>)>,
    /// Per-thread live JIT cell-buffer roots of compiled frames suspended on
    /// THIS thread's Rust stack, as a FLAT C-visible stack (`JitThreadCtx`) so
    /// a compiled non-leaf function's PROLOGUE can push its own frame buffer
    /// with inline machine code (Deep-IC v3) instead of a TLS callback. Still
    /// strictly per-thread: a compiled call runs interpreter bytecode in a
    /// callback, which can hit the op-loop GIL hand-off — so two threads
    /// interleave with compiled frames live, and a pop on one thread would
    /// drop the other's entry (LIFO violated → segfault). GC scans every
    /// thread's via `cells_all`.
    ctx: RefCell<Option<Box<JitThreadCtx>>>,
    /// ⭐**THE SOFTWARE FORM OF THE PINNED CONTEXT REGISTER.** The address of this thread's
    /// [`JitThreadCtx`] is a per-thread CONSTANT: the `Box` above is created on first use by
    /// `get_or_insert_with` and is never replaced, never taken and never moved (only
    /// [`JitThreadCtx::grow`] runs, and that reallocates the *entry buffer*, not the struct) —
    /// which is exactly the property that would let it live in a pinned machine register. It
    /// cannot live in one (see `jit::runtime::module`'s `RAVA_R1_PINNED_CONTEXT` note: Cranelift
    /// drops R15 out of the callee-save set the moment the pinned register is enabled, so nothing
    /// on the compiled side may WRITE it without a save/restore at every Rust→compiled edge), so
    /// it lives here instead, one word inside the cluster the caller has already loaded.
    ///
    /// WHAT IT REMOVES. Every warm compiled dispatch calls [`ctx_addr`] exactly once
    /// (`Vm::jit_direct_invoke` takes the address for the whole synthetic frame, and `jit_enter`
    /// does the same per entry), and that call used to run the full `with_ctx` body: a `RefCell`
    /// borrow flag load + compare + STORE, an `Option` discriminant branch, and a second STORE to
    /// release the borrow — five memory operations, two of them writes that dirty the line, to
    /// re-derive a pointer that has not changed since the thread's first compiled call. The
    /// population is the direct-hit count, which a real boot reports in the hundreds of millions
    /// for the `Done` window alone.
    ///
    /// THREE STATES, the convention `tls_cluster` established and for the same reason: null = this
    /// thread has not resolved the gate, [`crate::tls_cluster::CURSOR_OFF`] = resolved and
    /// disabled (the kill switch's arm reaches the historic body with no call and no gate re-read,
    /// so an A/B measures the memo and not a handicap), anything else = the ctx address.
    ///
    /// TEARDOWN. The memo is a FIELD of the cluster rather than a thread-local of its own, so its
    /// lifetime is the cluster's: it cannot outlive the `Box` it names, and an access after
    /// thread-local destruction still goes through [`tl`], which nulls its own cursor in
    /// `Drop for Locals` and then hits `std`'s "accessed during or after destruction" panic —
    /// the behaviour a bare `with()` has today, unchanged.
    ctx_ptr: Cell<*mut JitThreadCtx>,
    /// Per-thread JIT box side-table (`box_ref`/`unbox_ref`): the exact Value
    /// for reference cells that aren't a flat handle/null. Indices and the
    /// `truncate(box_base)` unwind are per-compiled-frame and ONLY valid on
    /// the boxing thread; a shared table would mis-truncate across an
    /// interleave and hand back stale/garbage Values. Same thread boxes and
    /// unboxes (a compiled frame resumes on its own OS thread), so per-thread
    /// is correct. Not GC-scanned (holds Str/StdOut/boxed-prim, no handles).
    boxes: RefCell<Vec<crate::Value>>,
    /// In-flight native-call arg buffers `(ptr, len)` — a STACK (natives nest). The
    /// args the opcode dispatch popped off the GC-rooted frame into a local Vec for
    /// `natives::try_invoke` are no longer frame-rooted, so a moving collection
    /// inside the native would leave an arena arg stale (Wall #19). The arena
    /// collector forwards these slots IN PLACE so the native's `&[Value]` sees the
    /// relocated pointer. GIL-synchronous: the native is parked in its allocation,
    /// not reading args, when the collection runs.
    ///
    /// ⚠`UnsafeCell`, not `RefCell`, for the reason [`Locals::local`] states and for one more
    /// that is specific to this field: **the borrow flag never protected the interesting
    /// case.** [`native_args_gather`] — the collector's actual reader, and the only one that
    /// runs while another thread's entries are live — reaches every stack through the raw
    /// address published into `NARGS_REG` and has always ignored the `RefCell` entirely. Its
    /// safety argument is the parked-owner one stated on the function, not the flag. Meanwhile
    /// [`native_args_push`] and [`native_args_truncate`] paid a flag load, a compare, a store
    /// and a release store on the guard's drop, once per native call each, for a re-entrancy
    /// that cannot happen: both are one-liners over a `Vec` of plain `usize` pairs — no drop
    /// glue, no user code, nothing that can reach this cell — and neither holds the pointer
    /// across a call to anything else.
    native_args: std::cell::UnsafeCell<Vec<(usize, usize)>>,
}

impl Locals {
    /// Const initialiser, so the cluster keeps the EAGER thread-local storage every
    /// one of these fields had as a standalone `const { .. }` static — same lifetime,
    /// same first-touch behaviour, nothing new to go wrong during thread teardown.
    const NEW: Locals = Locals {
        local: std::cell::UnsafeCell::new(Vec::new()),
        #[cfg(feature = "next-gen-gc")]
        gc_gather: RefCell::new((Vec::new(), Vec::new(), Vec::new())),
        ctx: RefCell::new(None),
        ctx_ptr: Cell::new(std::ptr::null_mut()),
        boxes: RefCell::new(Vec::new()),
        native_args: std::cell::UnsafeCell::new(Vec::new()),
    };
}

/// ── THE CLUSTER'S CURSOR, UNCONDITIONALLY ────────────────────────────────────────────
///
/// ⛔⭐**THE HOIST NOBODY OWNED. `Locals` WAS DECLARED THROUGH `thread_cluster!`, WHOSE
/// POINTER CURSOR IS BEHIND `RAVA_TLS_CLUSTER` AND IS DEFAULT OFF** — so in every shipping
/// run [`tl`] resolved to `CURSOR_OFF` on its first touch and then, on EVERY subsequent
/// access for the life of the thread, ran the resolved-OFF arm: a `tls_cluster::stat::note`
/// (a relaxed atomic load on a process-wide `AtomicU8`, plus a branch) and then a real
/// `LocalKey::with` — an out-of-line call with a lazy-init/registration state check,
/// because `Locals` owns `RefCell<Vec<..>>` fields and therefore registers a destructor.
///
/// THE POPULATION IS THE LARGEST IN THE ENGINE'S OWN CENSUS. `tls_cluster`'s module header
/// counts a boot to `Done` at 1.016 G cluster crossings and attributes **0.607 G of them to
/// `frames`** — more than `tlcache` (0.308 G) and `owner_bits` (0.102 G) together. Every one
/// of them is a `tl()`: [`push`]/[`pop`] once per interpreted call and once per general-path
/// compiled→host crossing, [`ctx_addr`] on every compiled crossing that takes a synthetic
/// frame, and the box/native-arg accessors on top. RIP sampling puts `LocalKey::with` in the
/// top three leaves of every window of the map for exactly this reason.
///
/// The buffer pool in this same file already diagnosed it in as many words — "the free list
/// lived in the `Locals` cluster, whose pointer cursor is behind `RAVA_TLS_CLUSTER` and is
/// DEFAULT OFF" — and escaped by moving OUT of the cluster and carrying its own cursor
/// ([`POOL_PTR`]). It fixed the pool and left the cluster it named still paying. This gives
/// the cluster the same treatment in place, so nothing has to move out again.
///
/// WHAT CHANGES: only WHEN the address is derived, never WHICH address. The payload keeps
/// its ordinary (destructor-registering) `thread_local!` storage with a `const` initialiser,
/// so its lifetime, its drop at thread exit and the stability of every interior address —
/// the addresses [`register`] publishes into `REG`/`CELLS_REG`/`NARGS_REG`/`BOXES_REG` — are
/// byte for byte what they were. The cursor is a `Cell<*const Locals>`: `Copy`, no
/// destructor, `const` initialiser, which is the one combination `std` lowers to a bare
/// `#[thread_local]` static, i.e. a TLS-slot load with no init check and nothing to call.
///
/// THREE STATES, the discipline `tls_cluster` established and for the same reason: null =
/// this thread has not resolved the gate, [`crate::tls_cluster::CURSOR_OFF`] = resolved and
/// disabled, anything else = the payload address. The OFF arm is therefore an honest
/// baseline — one TLS load, two predicted compares, then EXACTLY the historic `stat::note`
/// plus `LocalKey::with`, with no extra call and no gate re-read.
///
/// Kill switch `RAVA_NO_FAST_FRAMES2=1` (also honoured through `flags::emit_more_disabled`).
/// `RAVA_TLS_CLUSTER` is no longer consulted here: this cluster's cursor is now on by
/// default, and the flag stays what it always was for `tlcache` and `owner_bits`.
fn fast_frames2() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_FRAMES2").is_none() && !crate::flags::emit_more_disabled()
    })
}

thread_local! {
    /// The cluster payload. ORDINARY thread-local storage — the `RefCell<Vec<..>>` fields
    /// must be freed when a worker thread dies — with a `const` initialiser, so the storage
    /// stays EAGER exactly as `thread_cluster!`'s `slot .. : const =` form made it.
    static LOCALS: Locals = const { Locals::NEW };
    /// Cursor onto this thread's [`LOCALS`]; see the block above for the three states.
    static LOCALS_PTR: Cell<*const Locals> = const { Cell::new(std::ptr::null()) };
}

/// Nulls the cursor before the payload's fields are dropped — `std` runs a thread-local's
/// `Drop` before it tears the slot down — so an access after thread-local destruction finds
/// a null cursor, falls into the cold path and hits `std`'s own "accessed during or after
/// destruction" panic. That is what a bare `with()` does today, preserved exactly, and it is
/// also what keeps [`Locals::ctx_ptr`] (a memo whose lifetime is this struct's) from ever
/// being read against freed state.
impl Drop for Locals {
    fn drop(&mut self) {
        LOCALS_PTR.set(std::ptr::null());
    }
}

/// This thread's frame state, through one inlined TLS read.
#[inline(always)]
pub(crate) fn tl() -> &'static Locals {
    let p = LOCALS_PTR.get();
    if p as usize > crate::tls_cluster::CURSOR_OFF {
        // SAFETY: the cursor holds a real address only between the cold fill below and this
        // thread's `Drop for Locals`, which nulls it before dropping any field. In that
        // window it points at this thread's own `LOCALS`, whose storage lives in the
        // thread's TLS block and never moves.
        return unsafe { &*p };
    }
    if p as usize == crate::tls_cluster::CURSOR_OFF {
        crate::tls_cluster::stat::note(crate::tls_cluster::stat::FRAMES);
        // SAFETY (and the point of this arm): byte for byte the `LocalKey::with` this
        // accessor ran before the cursor was unconditional. The borrow is not held past the
        // return — every caller re-reads through `tl()`.
        return LOCALS.with(|s| unsafe { &*(s as *const Locals) });
    }
    tl_fill()
}

#[cold]
#[inline(never)]
fn tl_fill() -> &'static Locals {
    let on = fast_frames2();
    let p = LOCALS.with(|s| s as *const Locals);
    LOCALS_PTR.set(if on { p } else { crate::tls_cluster::CURSOR_OFF as *const Locals });
    if !on {
        crate::tls_cluster::stat::note(crate::tls_cluster::stat::FRAMES);
    }
    // SAFETY: `with` proved the slot is alive on this thread for the duration of the call,
    // and the borrow is not held past the return — every caller re-reads through `tl()`.
    unsafe { &*p }
}

thread_local! {
    /// Per-thread recursion depth (a shared counter would sum both threads'
    /// depths and trip the stack-overflow guard falsely → corruption).
    static DEPTH: Cell<u32> = const { Cell::new(0) };
    /// Per-thread re-entrant-native nesting depth — GC may run only when THIS
    /// thread is at 0 (no native holds unreachable `Value`s in Rust locals). A
    /// shared counter would SUM threads (a daemon mid-native would wrongly block
    /// the main thread's safe-point GC); per-thread is correct + the prerequisite
    /// for GIL-free worker bytecode (parallel worldgen).
    static NATIVE_DEPTH: Cell<u32> = const { Cell::new(0) };
    /// Per-thread op-loop checkpoint countdown. A GIL-free offload worker (parallel
    /// worldgen) runs the interpreter concurrently with main, so it needs its OWN
    /// countdown to reach its safepoint poll — a shared `Vm.op_countdown` would
    /// race. Behaviour-identical for the single-threaded/GIL case.
    static OP_COUNTDOWN: Cell<u32> = const { Cell::new(crate::opcodes::CHECK_INTERVAL) };
}
/// Push an in-flight native arg buffer; returns the prior stack height (scope base).
pub(crate) fn native_args_push(ptr: *const crate::Value, len: usize) -> usize {
    // SAFETY: per-thread cell, and the borrow ends before anything else runs (see the field).
    let b = unsafe { &mut *tl().native_args.get() };
    let base = b.len();
    b.push((ptr as usize, len));
    base
}
/// Drop every native arg buffer at/after `base` (the scope's high-water mark).
pub(crate) fn native_args_truncate(base: usize) {
    // SAFETY: as `native_args_push` — per-thread, and the entries are plain `usize` pairs,
    // so the truncate runs no drop glue and calls nothing that could reach this cell.
    unsafe { &mut *tl().native_args.get() }.truncate(base);
}
/// Forward each in-flight native arg slot of EVERY REGISTERED THREAD (arena collector root pass).
///
/// ⛔⭐**This gathered only `tl()` — the collecting thread — and that was a real hole the moment
/// two threads ran guest code.** Frames (`all`) and JIT cells (`cells_all`) were already
/// cross-thread; native args and the box table were not. So a collection triggered by the worker
/// forwarded the worker's roots and left MAIN's in-flight native args stale, and vice versa. It
/// surfaced as `UnsatisfiedLinkError: invokevirtual Codec.decode(...)` in the offloaded datapack
/// reload with `[ULE-CLASSIFY] young=true fwd=None` and a header word of `0x1def55` — not a
/// header at all, but reused nursery bytes.
///
/// SAFETY, and it is the same argument `all()` already makes: every address is a live thread's
/// TLS-resident `Vec`, and the owning thread is parked at `gil::acquire` — not mutating it —
/// while the GIL holder collects.
pub(crate) fn native_args_gather(f: &mut dyn FnMut(&mut crate::Value)) {
    let r = NARGS_REG.lock().unwrap_or_else(|e| e.into_inner());
    // ⚠The COLLECTING thread is scanned whether or not it registered. `register` runs from
    // `run_main_with_args` and from each worker scope, but an embedded/test Vm may collect
    // without either — and dropping the current thread's roots would turn this fix into a
    // worse bug than the one it fixes.
    let me = tl().native_args.get() as usize;
    for &addr in r.iter().chain(if r.contains(&me) { None } else { Some(&me) }) {
        let stack = unsafe { &*(addr as *const Vec<(usize, usize)>) };
        for &(ptr, len) in stack.iter() {
            let s = unsafe { std::slice::from_raw_parts_mut(ptr as *mut crate::Value, len) };
            for v in s.iter_mut() {
                f(v);
            }
        }
    }
}
/// ── THE FRAME-BUFFER POOL ────────────────────────────────────────────────────────────
///
/// ⭐**`pool_take` + `pool_give` RUN ON EVERY INTERPRETED CALL AND A PROFILE OF PHASE A
/// PUTS THEM AT 0.59% EACH.** They are not doing 1.2% of *useful* work: a round trip was
/// two out-of-line thread-local crossings, three reads of process-constant census gates,
/// and a `clear()` performed on the way OUT of the pool rather than on the way in. This
/// block takes all three out. Kill switch `RAVA_NO_FAST_POOL=1` (also honoured through
/// `flags::emit_more_disabled`) restores the historic structure exactly.
///
/// WHAT GROWS: one extra `thread_local!` slot and one pointer-sized cursor per thread
/// (the free list moved out of the [`Locals`] cluster so it can own a cursor whose
/// destructor nulls it). `POOL_CAP` is unchanged, so the number of retained buffers —
/// the only memory that matters here — is exactly what it was.
///
/// 1. **ONE INLINED TLS LOAD INSTEAD OF A `LocalKey::with` CALL.** The free list lived in
///    the `Locals` cluster, whose pointer cursor is behind `RAVA_TLS_CLUSTER` and is
///    DEFAULT OFF — so in the shipping configuration both halves of every round trip paid
///    a real `LocalKey::with` (a lazy-init check plus an out-of-line call, because
///    `Locals` has a destructor) plus the cluster's `stat::note` relaxed load. The pool
///    now carries its own cursor, unconditionally, using the same three-state discipline
///    the cluster documents: null = this thread has not resolved yet, `CURSOR_OFF` =
///    resolved and the kill switch is engaged, anything else = the free list's address.
///
/// 2. **THE CENSUS GATES ARE READ ONCE PER THREAD, NOT THREE TIMES PER CALL.**
///    `vecgrow::note_take`, `vecgrow::note_give` and `pool_census` each re-read an
///    `OnceLock<bool>` over an environment variable — process constants, fixed before any
///    guest code runs, but they are out-of-line calls that LLVM cannot hoist out of the
///    caller. Their conjunction is cached in [`POOL_GATES`] on the same cold path that
///    fills the cursor, so the shipping path tests one plain per-thread byte and, when it
///    is clear, contains no census code at all.
///
/// 3. **THE `clear()` HAPPENS AT `pool_give`, NOT `pool_take`.** The same slots are
///    dropped either way — this is not a saving of drop glue, and it is not claimed as
///    one. It buys two things. The buffer's `Value`s die with the frame that owned them
///    instead of surviving in the free list until the next checkout, which is what every
///    doc comment around this pool already ASSERTS ("the pool never holds a `Value` across
///    a GC point") and what `FrameGuard::drop` — the dominant giver, and the one that
///    hands back a buffer still holding `nlocals + operands` live cells — has been
///    quietly violating: those cells include raw arena pointers that the collector does
///    not forward, because the free list is not a root set. And it moves the drop loop
///    off the checkout path, which is what makes `pool_take` small enough to inline into
///    `pop_args` at all.
///
/// The clear side is decided by the same resolved cursor as the pointer, so a thread can
/// never mix the two disciplines: fast ⇒ give clears and take does not, kill switch ⇒
/// take clears and give does not. Nothing in between exists, and that is the invariant
/// that keeps a dirty buffer from ever being handed out.

/// ── WHAT A MISS COSTS, AND WHY IT IS NOT THE CEILING ─────────────────────────────────
///
/// ⭐**THE CENSUS'S OWN ARITHMETIC CLOSES, AND IT SAYS THE LEAK IS THE MINORITY.** The
/// numbers quoted on [`POOL_CAP`] and on `vmframe::fast_pool2_uncached` are
/// `TAKE_MISS = 3,469,985`, `LEAKED = 1,330,047`, `GIVE_FULL = 2,132,888` — and
/// `1,330,047 + 2,132,888 = 3,462,935`, i.e. **99.8% of every miss**. So a take misses
/// for exactly one of two reasons: the buffer it would have popped was never given back
/// (38.3%), or it WAS given back and this thread threw it away at the ceiling (61.5%).
/// There is no third population, and both halves resolve to the same event: a buffer the
/// process already owned went back to the allocator.
///
/// Neither cause is fixable here. The leak is a `Vec<Value>` moved by value into
/// `Vm::invoke_idx` and dropped on one of its early returns (`MethodNotFound`, `NoCode`,
/// the four remaining by-name native intercepts, the `StackOverflowError` depth cap) plus the
/// `-> NATIVE branch` exits the census counts separately in `opcodes::invoke*`; the
/// ceiling was MEASURED at 4096 and reverted (`+100 MB` RSS for a quarter of a point).
///
/// So this block does not chase either cause. It makes the CONSEQUENCE cheap, which is a
/// thing neither fix would do on its own, and which the tree has already written down
/// twice without acting on it — `vmframe::fast_pool2_uncached`, verbatim:
///
/// > A miss is not a cheap event: `pool_take` hands back `Vec::new()`, `pop_args_n` then
/// > `reserve(n + 1)` on it, and `size_frame_buffer` reallocates that again to
/// > `max_locals + max_stack` — **two `mi_malloc`s and a memcpy** for a buffer the process
/// > already owned.
///
/// **THE FRAME BUFFER'S FIRST ALLOCATION IS NOW THE ONLY ONE.** The size is not a guess and
/// not a tuning constant: it is [`Pool::hint`], the capacity of the last buffer THIS thread
/// returned to the pool — a direct observation of the frames this thread is actually
/// running, recorded for free on a line [`pool_give`] already dirties, and published by
/// [`pool_size_hint`]. A miss happens when the free list has just drained, so the last give
/// before it is a sibling frame of the one about to be built; `pop_args_n` reserves that
/// much instead of `n + 1`, and `size_frame_buffer`'s `max_locals + max_stack` then fits
/// without a `finish_grow`.
///
/// **WHERE IT IS SPENT, AND WHY NOT IN [`pool_take`].** Sizing the miss inside `pool_take`
/// was written first and is wrong twice over: `natives::CanonArgs`, `scratch` and
/// `jit_call_body` take from this same pool and want nothing of the sort, and five existing
/// tests identify a miss by `pool_take().capacity() == 0` — the only observable this pool
/// exposes. The consumer that knows BOTH that it missed and what the frame will need is
/// `opcodes::statics::pop_args_n`, and it already has the branch: it reads the hint only
/// when the capacity in hand is short, i.e. on the path that was going to call the allocator
/// regardless. The 94.6% of takes that hit never touch it.
///
/// **GUARD:** the hint is used only when it is non-zero AND under its own ceiling
/// ([`Pool::hint_cap`]), so a thread that has returned nothing yet, and a thread whose kill
/// switch is engaged, both leave the caller reserving exactly what it reserved before.
/// **COLD ARM:** `buf.reserve(n + 1)` — the historic expression, reached with no gate read.
/// Kill switch `RAVA_NO_FAST_POOLLEAK=1` (also honoured through `flags::emit_more_disabled`)
/// zeroes [`Pool::hint_cap`] once per thread, which restores the take side byte for byte;
/// the give side keeps ONE `mov` into a cache line it is already writing, and that is stated
/// rather than hidden, because an OFF arm that lies about its cost is worse than no OFF arm.
///
/// **WHAT IS DELIBERATELY NOT DONE:** the hint is not a running maximum. A single method
/// with `max_locals = 400` would ratchet every later miss on that thread to a 6 KB
/// allocation and never come down — the shape ledger row 447 records as "a cache that
/// stopped fitting is worse than the allocation it removes". Last-give tracks the current
/// working set and decays with it, and [`POOL_HINT_SLOTS`] bounds the worst case anyway.
struct Pool {
    /// The free list itself — the field this struct used to BE.
    list: Vec<Vec<crate::Value>>,
    /// Capacity in slots of the last buffer [`pool_give`] pooled on this thread; 0 until
    /// the first give. Written on every give — one store, no branch, into the same line as
    /// `list`'s header, which the give reads for its `POOL_CAP` test and writes for its
    /// `push` — and read only through [`pool_size_hint`], i.e. only by a caller that has
    /// already decided it must grow.
    hint: u32,
    /// Ceiling on `hint`, resolved once per thread in [`pool_resolve_cold`]:
    /// [`POOL_HINT_SLOTS`] when the fast pool AND this block are both enabled, 0 otherwise.
    /// Folding the kill switch into the clamp is what leaves the consumer with no gate of
    /// its own: `hint.min(hint_cap) == 0` is simultaneously "no observation yet",
    /// "disabled", and "reserve exactly what you were going to reserve".
    hint_cap: u32,
}

/// Ceiling on how far a miss may over-allocate, in `Value` slots (16 bytes each), so one
/// unusually deep frame cannot make every later miss on that thread allocate a page.
///
/// 32 slots = 512 bytes, which covers `max_locals + max_stack` for ordinary javac output
/// with room to spare; a method that needs more grows ONCE at `size_frame_buffer`, exactly
/// as it does today. The memory this can hold is bounded by buffers IN FLIGHT (call depth),
/// not by the free list — a pooled buffer of this size is one the frame would have grown to
/// regardless, which is why this does not repeat the `POOL_CAP = 4096` trade.
const POOL_HINT_SLOTS: u32 = 32;

/// This thread's frame-buffer free list. Its own thread-local rather than a field of
/// [`Locals`] for one reason: the cursor in [`POOL_PTR`] must be nulled BEFORE the
/// buffers are dropped at thread exit, and the only place to hang that is a `Drop` on the
/// slot's own type (`Locals` has its own hand-written `Drop`, which nulls `LOCALS_PTR`). `UnsafeCell`, not `RefCell`, for the reason the field carried before: the
/// borrow flag was a read, a write and a branch per call for a re-entrancy that cannot
/// happen — `pool_take` and `pool_give` run no user code and call nothing that can reach
/// this cell — and nothing may hold a borrow of it across a call to anything else.
///
/// Still strictly PER-THREAD, which is the property the free list was moved out of `Vm`
/// for: a checkout/return per method call races the instant two threads interpret at once.
struct PoolSlot(std::cell::UnsafeCell<Pool>);

impl Drop for PoolSlot {
    fn drop(&mut self) {
        // Nulls the cursor before this thread's buffers are freed, so a pool access after
        // thread-local destruction takes the cold path and hits `std`'s own
        // "accessed during or after destruction" panic — byte for byte the behaviour the
        // `LocalKey::with` route had — instead of writing through a dangling pointer.
        POOL_PTR.set(std::ptr::null_mut());
    }
}

thread_local! {
    /// The buffers themselves. Ordinary (destructor-registering) thread-local storage:
    /// the free list must be freed when a worker thread dies, and leaking up to
    /// `POOL_CAP` buffers per dead thread is not a trade worth making for a cold path.
    static POOL: PoolSlot = const {
        PoolSlot(std::cell::UnsafeCell::new(Pool { list: Vec::new(), hint: 0, hint_cap: 0 }))
    };
    /// Cursor onto this thread's [`POOL`]. `Cell<*mut _>` is `Copy`, has no destructor and
    /// its initialiser is a `const` block — the one combination `std` lowers to a bare
    /// `#[thread_local]` static, i.e. a TLS-slot load with no init check and nothing to call.
    static POOL_PTR: std::cell::Cell<*mut Pool> =
        const { std::cell::Cell::new(std::ptr::null_mut()) };
    /// Per-thread cache of the two process-constant census gates (`RAVA_VECGROW_STAT`,
    /// `RAVA_POOL_CENSUS`): 0 = unresolved, 1 = both off, 2 = at least one armed. Filled on
    /// the same cold path that fills [`POOL_PTR`], so by the time either pool function reads
    /// it the value is never 0.
    static POOL_GATES: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

/// `RAVA_NO_FAST_POOL=1` — restore the historic pool structure: a `LocalKey::with` per
/// access, the census gates re-read per call, and `clear()` on checkout instead of on
/// return. Read ONCE PER THREAD, on the cold resolve, and the answer is cached in the
/// cursor itself, so neither arm re-reads it per call.
pub(crate) fn fast_pool() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_POOL").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// `RAVA_NO_FAST_POOLLEAK=1` — restore the historic miss path: `opcodes::statics::pop_args_n`
/// reserves `n + 1` on the `Vec::new()` a missed take hands it, and `size_frame_buffer` then
/// reallocates that to `max_locals + max_stack` — two `mi_malloc`s and a memcpy per miss.
/// Read ONCE PER THREAD, on the cold resolve below, and folded into [`Pool::hint_cap`], so
/// neither the pool nor its consumer reads a gate per call.
pub(crate) fn fast_poolleak() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_POOLLEAK").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// This thread's free list, plus whether the fast discipline is in force.
///
/// The `bool` is the CURSOR's own state, not a fresh gate read: `true` only ever comes
/// back once the cold path below has resolved `fast_pool()` to true, so the pointer and
/// the clear-side decision can never disagree within a thread — including on the very
/// first call of a thread, which is why the cold path returns the resolved answer rather
/// than letting the caller re-read the cursor it just filled.
#[inline(always)]
fn pool_resolve() -> (*mut Pool, bool) {
    let p = POOL_PTR.get();
    if p as usize > crate::tls_cluster::CURSOR_OFF {
        // SAFETY: the cursor holds a real address only between the cold fill below and
        // `Drop for PoolSlot`, which nulls it before freeing anything. In that window it
        // points at this thread's own `POOL`, whose storage lives in the thread's TLS
        // block and never moves.
        return (p, true);
    }
    pool_resolve_cold(p as usize == crate::tls_cluster::CURSOR_OFF)
}

#[cold]
#[inline(never)]
fn pool_resolve_cold(resolved_off: bool) -> (*mut Pool, bool) {
    // This is the historic route, and it is exactly what the kill-switch arm keeps paying
    // on every access — an honest baseline, not "main plus overhead": the extra work in
    // this arm is the `with()` call the shipping path used to make anyway.
    let a = POOL.with(|s| s.0.get());
    if resolved_off {
        return (a, false);
    }
    POOL_GATES.set(if crate::vecgrow::enabled() || pool_census() { 2 } else { 1 });
    let fast = fast_pool();
    // The miss-sizing ceiling, resolved with the rest of this thread's pool gates and never
    // read again. Zero unless BOTH switches are on, so the two disciplines can no more mix
    // here than the clear side can: `RAVA_NO_FAST_POOL` restores the whole historic structure
    // (including `pop_args_n`'s bare `reserve(n + 1)`) without needing to know this exists.
    //
    // SAFETY: `a` is this thread's own `POOL` payload, just proved live by the `with` above;
    // no other borrow of it exists at this point (the pool functions are one-liners that hold
    // none across a call), and the field is a plain `u32`.
    unsafe {
        (*a).hint_cap = if fast && fast_poolleak() { POOL_HINT_SLOTS } else { 0 };
    }
    POOL_PTR.set(if fast { a } else { crate::tls_cluster::CURSOR_OFF as *mut _ });
    (a, fast)
}

/// Is any pool census armed? One plain per-thread byte; see [`POOL_GATES`]. Only ever
/// called after [`pool_resolve`], which is what guarantees the byte is resolved.
#[inline(always)]
fn census_armed() -> bool {
    POOL_GATES.get() == 2
}

/// The census work for one checkout, kept out of line so the shipping path is a load, a
/// compare and a not-taken branch.
///
/// `POOL_TAKES` counts HITS only, exactly as it did when this was written inline: a miss
/// hands back a fresh `Vec` with no slots to clear, and counting it would dilute the
/// "slots per round trip" ratio the report divides by it.
#[cold]
#[inline(never)]
fn note_take_census(hit: bool, slots: usize) {
    crate::vecgrow::note_take(hit);
    if hit && pool_census() {
        POOL_TAKES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        POOL_SLOTS.fetch_add(slots as u64, std::sync::atomic::Ordering::Relaxed);
    }
}

/// As [`note_take_census`], for the return side. `slots` is the buffer's length BEFORE the
/// clear — under the fast discipline that is where the drop glue is now spent, so it is
/// the number `RAVA_POOL_CENSUS` has to report for its "slots per round trip" to keep
/// meaning what it meant when the clear was on the checkout side.
#[cold]
#[inline(never)]
fn note_give_census(cap: usize, full: bool, slots: usize) {
    crate::vecgrow::note_give(cap, full);
    if pool_census() && cap > 0 {
        POOL_SLOTS.fetch_add(slots as u64, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Check out a cleared reusable Value buffer from this thread's pool.
///
/// The "cleared" guarantee is unchanged; it is now established at the OTHER end. Every
/// buffer in the free list was cleared by [`pool_give`] before it went in, and a miss
/// returns `Vec::new()`, so what comes back is indistinguishable from a fresh `Vec` in
/// length and contents — only its capacity differs, which is the entire point. Under the
/// kill switch nothing entered the list cleared, so this end clears instead.
///
/// ⚠**A MISS STILL RETURNS AN UNALLOCATED `Vec`, DELIBERATELY.** Sizing it here was written
/// and then moved: three tests in `opcodes::tests` and two in this file identify a miss by
/// `pool_take().capacity() == 0`, and that is not merely a test convention — it is the only
/// observable this pool exposes, and `natives`, `scratch` and `jit_call_body` all take from
/// the same pool with no idea what size a FRAME wants. The pre-sizing therefore lives at the
/// one caller that knows both the miss and the eventual size, `opcodes::statics::pop_args_n`,
/// which reads [`pool_size_hint`] on the branch where it was going to allocate anyway.
#[inline]
pub(crate) fn pool_take() -> Vec<crate::Value> {
    let (p, fast) = pool_resolve();
    // SAFETY: per-thread cell, and the borrow ends before anything else runs.
    let pool = unsafe { &mut *p };
    match pool.list.pop() {
        Some(mut v) => {
            if census_armed() {
                // ⭐**Ledger row 346** measured `pool_take` at 3.4% of interpreter time
                // against `Frame::new` at 0.5% — the buffer traffic, not frame
                // construction, is the frame's real cost — and `RAVA_POOL_CENSUS` counts
                // the slots the drop glue actually runs over, because inference is not
                // measurement.
                note_take_census(true, v.len());
            }
            if !fast {
                v.clear();
            }
            v
        }
        None => {
            if census_armed() {
                note_take_census(false, 0);
            }
            Vec::new()
        }
    }
}

/// How many `Value` slots the last buffer this thread pooled had — the size a caller that
/// is ABOUT TO ALLOCATE should ask for instead of the bare minimum. Zero means "no
/// observation yet" and, identically, "the kill switch is engaged": [`Pool::hint_cap`] is
/// the clamp and the gate at once, so a caller needs no gate of its own and a zero answer
/// leaves it on exactly the expression it used before.
///
/// ⚠**CALL THIS ONLY ON A PATH THAT WAS GOING TO ALLOCATE.** It resolves the pool cursor,
/// which is one inlined TLS load on the shipping path but a real `LocalKey::with` under
/// `RAVA_NO_FAST_POOL` — cheap against a `mi_malloc`, not against nothing. The one caller
/// guards it behind the capacity test that decides whether a grow happens at all.
#[inline]
pub(crate) fn pool_size_hint() -> usize {
    let (p, _) = pool_resolve();
    // SAFETY: as every other pool accessor — this thread's own cell, two plain `u32` reads,
    // no borrow held across a call.
    let pool = unsafe { &*p };
    pool.hint.min(pool.hint_cap) as usize
}
/// Take the reusable GC gather buffers (cleared, capacity retained). Pair with
/// `gc_gather_put` at the end of the collection to return them.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn gc_gather_take() -> (
    Vec<*mut crate::value::Value>,
    Vec<*mut u8>,
    Vec<*mut i64>,
) {
    let mut bb = tl().gc_gather.borrow_mut();
    let mut t = (
        std::mem::take(&mut bb.0),
        std::mem::take(&mut bb.1),
        std::mem::take(&mut bb.2),
    );
    t.0.clear();
    t.1.clear();
    t.2.clear();
    t
}
/// Return the GC gather buffers so their capacity is reused next collection.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn gc_gather_put(
    locs: Vec<*mut crate::value::Value>,
    ptrs: Vec<*mut u8>,
    cells: Vec<*mut i64>,
) {
    let mut bb = tl().gc_gather.borrow_mut();
    bb.0 = locs;
    bb.1 = ptrs;
    bb.2 = cells;
}
/// Per-thread buffer-pool ceiling.
///
/// ⛔ MEASURED AT 4096 AND REVERTED. Raising it 1024 → 4096 moved the take-miss rate only
/// 5.38% → 5.11% and `give_full` 2,132,888 → 1,965,233, for **peak RSS 2075 → 2177 MB
/// (+100 MB)**. A hundred megabytes for a quarter of a percentage point is not a trade.
///
/// The census says why the ceiling was the wrong suspect: **`LEAKED=1,330,047`** buffers are
/// taken and never returned — about 40% of the 3.3M misses — and `Vm::invoke entered`
/// (12,853,740) minus `reached expand_locals` (12,301,531) leaves **552,209** early exits
/// that never reach the return point. **The pool does not miss because it is too small; it
/// misses because buffers do not come back.**
///
/// ⭐**AND THE REST OF THE MISSES ARE THIS CEILING, WHICH IS WHY RAISING IT DID NOT HELP.**
/// `LEAKED (1,330,047) + GIVE_FULL (2,132,888) = 3,462,935` against `TAKE_MISS = 3,469,985`
/// — the two account for **99.8%** of every miss, with no third population. That is not an
/// argument for a bigger number: a give discarded at the ceiling and a take that then
/// misses is a working set this thread genuinely holds, and 4096 bought a quarter of a
/// point for 100 MB. It is an argument for making the miss cheap, which is what the block
/// above `Pool` does — see it for the full derivation and for where the 1.33M leaked
/// buffers are actually dropped (all of them outside this file).
const POOL_CAP: usize = 1024;

/// Return a buffer to this thread's pool (bounded so it can't grow unboundedly).
///
/// ⭐**THIS IS WHERE THE BUFFER IS CLEARED NOW.** `FrameGuard::drop` — the dominant caller,
/// once per interpreted call — hands the buffer back still holding `nlocals + operands`
/// live cells, and until now those cells sat in the free list until the next checkout
/// cleared them. That is one round trip's worth of `Value`s kept alive across an
/// arbitrary number of collections, in a structure the collector does not scan, and it is
/// the exact opposite of the contract stated on every other user of this pool
/// (`natives::CanonArgs`, `opcodes::statics::PooledVals`: "clearing means the pool never
/// holds a `Value` across a GC point"). Clearing here makes that true for the frame path
/// as well, drops each frame's cells at the moment the frame dies, and takes the drop loop
/// off the checkout path. It is the SAME slots either way — no drop glue is saved, and
/// none is claimed.
///
/// The clear is skipped under the kill switch so the historic clear-on-checkout shape is
/// restored whole; see the block comment on [`pool_take`] for why the two disciplines can
/// never be mixed within a thread.
#[inline]
pub(crate) fn pool_give(mut v: Vec<crate::Value>) {
    let cap = v.capacity();
    if cap > 0 {
        let (p, fast) = pool_resolve();
        if fast {
            // Runs `Value`'s drop glue over the live slots, so what enters the free list
            // is indistinguishable from a fresh `Vec` except for its capacity.
            let slots = v.len();
            v.clear();
            // SAFETY: as `pool_take` — per-thread, and no call escapes while the borrow is
            // live. Taken AFTER the clear, which touches only `v`'s own allocation and
            // cannot reach this cell.
            let pool = unsafe { &mut *p };
            let full = pool.list.len() >= POOL_CAP;
            // ⭐**THE ONLY COST THIS BLOCK ADDS TO THE HOT PATH: ONE STORE.** No branch, no
            // gate read, and into the same cache line the `POOL_CAP` test above just read
            // and the `push` below is about to write. It records what this thread's frames
            // actually measure so a later miss can allocate once instead of twice; the clamp
            // and the kill switch both live in `hint_cap`, read only through
            // [`pool_size_hint`] on `pop_args_n`'s grow branch. A give whose buffer is
            // absurdly large truncates here, and the `min(hint_cap)` on the read side —
            // itself clamped to 32 slots — makes the truncation unobservable.
            pool.hint = cap as u32;
            if census_armed() {
                note_give_census(cap, full, slots);
            }
            // A discarded buffer is a future miss — and the census now says HOW MANY: these
            // 2,132,888 discards plus the 1,330,047 leaked buffers are 99.8% of the
            // 3,469,985 misses (derivation in the block above `Pool`). The usage is BURSTY,
            // not steady: takes miss 5.38% of 64.4M WHILE 2.1M gives are discarded for a
            // full pool, and both cannot happen in a steady state — deep call chains drain
            // the pool and then return every buffer at once. Per-thread, so the memory is
            // bounded by depth x threads, not by the heap. The discard itself stays: 4096
            // was measured and cost 100 MB for a quarter of a point. What changed is that
            // the miss it causes now costs one allocation instead of two and a memcpy.
            if !full {
                pool.list.push(v);
            }
            return;
        }
        // Kill switch: the historic body, byte for byte — no clear here, no hint, the census
        // gates read live, and the free list reached through the cold `LocalKey::with` route.
        let pool = unsafe { &mut *p };
        crate::vecgrow::note_give(cap, pool.list.len() >= POOL_CAP);
        if pool.list.len() < POOL_CAP {
            pool.list.push(v);
        }
    } else {
        // Capacity 0: nothing to pool and nothing to clear (an unallocated `Vec` has no
        // elements). The census still wants to see the give, so the take/give balance it
        // audits stays readable — hence the resolve, which is also what guarantees
        // `census_armed` is meaningful here.
        let (_, fast) = pool_resolve();
        if !fast || census_armed() {
            crate::vecgrow::note_give(0, false);
        }
    }
}
/// This thread's flat JIT cell-root stack, laid out for the compiled
/// prologue's INLINE push (fixed field offsets; entries are 2 u64 each:
/// buffer pointer, buffer length in cells). `storage` owns the entry
/// memory; `buf`/`cap` are its raw view so machine code never touches the
/// Vec. Only the owning thread mutates it (under the GIL); the collector
/// reads every thread's via the `CELLS_REG` registry while owners are
/// parked — the same contract the old per-thread Vec had.
#[repr(C)]
pub(crate) struct JitThreadCtx {
    /// +0: number of live (ptr, len) entries.
    pub(crate) top: u64,
    /// +8: entry capacity of `buf`.
    pub(crate) cap: u64,
    /// +16: entry storage, 2 u64 per entry.
    pub(crate) buf: *mut u64,
    /// +24: mirror of this thread's BOXES.len() — the compiled epilogue
    /// compares it to its prologue snapshot and only takes the box-unwind
    /// callback when the callee actually boxed something.
    pub(crate) box_len: u64,
    /// +32: PHASE-A STACK MAPS ONLY (`RAVA_STACKMAPS=1`), null otherwise. One u64 per
    /// `buf` entry: the SP of the compiled frame that pushed that cell-root range, taken
    /// with `get_stack_pointer` in the self-registering prologue. Cranelift x64 holds SP
    /// constant through a body, so this is the SP at every safepoint in that frame — the
    /// base a `UserStackMap`'s offsets are relative to, and the place `*(sp-8)` finds the
    /// return address that keys the map. Never read unless the flag armed it, and never
    /// written by compiled code unless the flag armed the emission.
    pub(crate) sp_buf: *mut u64,
    storage: Vec<u64>,
    sp_storage: Vec<u64>,
}
/// Initial per-thread entry capacity (32KB). Deeper recursion grows by
/// doubling via the cold `cells_push` path / `rava_cells_push_slow`.
const CTX_INITIAL_ENTRIES: usize = 2048;
impl JitThreadCtx {
    fn new() -> Box<Self> {
        let mut storage = Vec::<u64>::with_capacity(CTX_INITIAL_ENTRIES * 2);
        let buf = storage.as_mut_ptr();
        // The SP mirror exists only under the Phase-A stack-map flag; with it off the
        // pointer stays null, nothing allocates, and no emitted instruction reads it.
        let mut sp_storage = if stackmaps_on() {
            vec![0u64; CTX_INITIAL_ENTRIES]
        } else {
            Vec::new()
        };
        let sp_buf = if sp_storage.is_empty() { std::ptr::null_mut() } else { sp_storage.as_mut_ptr() };
        Box::new(JitThreadCtx {
            top: 0,
            cap: CTX_INITIAL_ENTRIES as u64,
            buf,
            box_len: 0,
            sp_buf,
            storage,
            sp_storage,
        })
    }
    /// Double the entry capacity, preserving the live entries. The buffer
    /// moves, but nothing holds interior pointers into it: machine code
    /// re-reads `buf` from the ctx on every push, and the collector reads
    /// it fresh at each collection.
    fn grow(&mut self) {
        let new_cap = (self.cap as usize) * 2;
        let mut ns = Vec::<u64>::with_capacity(new_cap * 2);
        unsafe {
            std::ptr::copy_nonoverlapping(self.buf, ns.as_mut_ptr(), (self.top as usize) * 2);
        }
        self.buf = ns.as_mut_ptr();
        self.cap = new_cap as u64;
        self.storage = ns;
        if !self.sp_buf.is_null() {
            let mut ns = vec![0u64; new_cap];
            ns[..self.top as usize].copy_from_slice(&self.sp_storage[..self.top as usize]);
            self.sp_buf = ns.as_mut_ptr();
            self.sp_storage = ns;
        }
    }
    #[inline]
    pub(crate) fn push(&mut self, p: (*const i64, usize)) {
        if self.top == self.cap {
            self.grow();
        }
        unsafe {
            let a = self.buf.add((self.top as usize) * 2);
            *a = p.0 as u64;
            *a.add(1) = p.1 as u64;
            // A push from the Rust side (a wrapper entry, or the prologue's capacity
            // overflow arm) has no compiled frame SP to record — zero the mirror slot so
            // the cross-check cannot read the last frame's SP for this range.
            if !self.sp_buf.is_null() {
                *self.sp_buf.add(self.top as usize) = 0;
            }
        }
        self.top += 1;
    }
}

/// `RAVA_STACKMAPS=1`, answered without depending on the `next-gen-gc` module tree (this
/// file compiles in the default build too). One `OnceLock`, read on ctx creation only.
#[inline]
fn stackmaps_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    // ⛔**ONE FACT, ONE DERIVATION — and this is what happens when it is two.** This predicate and
    // `jit::runtime::stackmaps::mode()` both answered "is the stack-map protocol on?", and they
    // answered it from the RAW VARIABLE in two places. The moment the mode gained a DEFAULT (the
    // variable unset now means ON), the two disagreed: `mode()` said Full, so the emitter stopped
    // emitting `spill_for_gc`/`reload_*`, while this gate still read the bare variable, said false,
    // and the prologue never recorded the frame's SP — so the collector could not locate a single
    // compiled frame and NOTHING was rooted. `write of 0x0` inside compiled `Identifier.isValidPath`,
    // 5 failures in a row, on a build whose every explicit-flag run was green.
    //
    // The mode is now asked, not re-derived. The `#[cfg]` split keeps this file compiling in the
    // default build, where the module tree that owns the mode does not exist and the answer is
    // false by construction.
    #[cfg(feature = "next-gen-gc")]
    {
        *ON.get_or_init(crate::jit::runtime::stackmaps::records_sp)
    }
    #[cfg(not(feature = "next-gen-gc"))]
    {
        *ON.get_or_init(|| false)
    }
}
#[inline]
fn with_ctx<R>(f: impl FnOnce(&mut JitThreadCtx) -> R) -> R {
    let mut c = tl().ctx.borrow_mut();
    f(c.get_or_insert_with(JitThreadCtx::new))
}
/// `with_ctx` for a caller that has ALREADY resolved the cluster — `box_push` and
/// `box_truncate` touch the box table and the ctx in one operation, and paid a
/// thread-local access for each before they shared a struct.
#[inline]
fn with_ctx_of<R>(l: &Locals, f: impl FnOnce(&mut JitThreadCtx) -> R) -> R {
    let mut c = l.ctx.borrow_mut();
    f(c.get_or_insert_with(JitThreadCtx::new))
}
/// `RAVA_NO_PINNED_CTX=1` — kill switch for the per-thread context memo in
/// [`Locals::ctx_ptr`], default ON. Also consults the block switch, so
/// `RAVA_NO_EMIT_MORE=1` reverts this with the rest of the "cross less" work in one run.
///
/// Read ONCE PER THREAD, on the cold fill below, and never on either fast path — the answer is
/// cached in the memo word itself (`CURSOR_OFF` for "resolved, disabled"), so the OFF arm runs
/// exactly the historic `with_ctx` body with no extra call and no gate re-read.
fn pinned_ctx_memo() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_PINNED_CTX").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// Stable address of this thread's ctx (allocating it on first use) — the
/// per-Vm slot the compiled prologue loads it from.
///
/// This is the pointer a pinned machine register would have carried, and it is resolved on EVERY
/// compiled crossing that takes a synthetic frame. See [`Locals::ctx_ptr`] for why the address is
/// a per-thread constant, what the memo removes, and why it is a field of the cluster rather than
/// a thread-local of its own.
#[inline]
pub(crate) fn ctx_addr() -> *mut JitThreadCtx {
    ctx_addr_of(tl())
}

/// [`ctx_addr`] for a caller that has ALREADY resolved the cluster.
///
/// ⭐**THE MEMO EXISTED AND TWO OF ITS THREE READERS WERE NOT USING IT.** `ctx_addr` answered
/// from [`Locals::ctx_ptr`]; [`box_push`] and [`box_truncate`] — the other per-operation
/// touches of the same per-thread constant — still went through [`with_ctx_of`], i.e. the
/// exact body the memo was built to delete: a `RefCell` borrow-flag load, a compare and a
/// STORE, an `Option` discriminant branch, and a release STORE when the guard drops. Five
/// memory operations, two of them writes that dirty the line, to re-derive a pointer that
/// has not changed since this thread's first compiled call. Splitting the resolve out of
/// `ctx_addr` lets all three readers share the one word. The gate is the memo's own
/// (`RAVA_NO_PINNED_CTX`), unchanged: its OFF arm still routes every reader through
/// `with_ctx_of`, so the historic shape is restored for all of them at once.
#[inline]
fn ctx_addr_of(l: &Locals) -> *mut JitThreadCtx {
    let p = l.ctx_ptr.get();
    if p as usize > crate::tls_cluster::CURSOR_OFF {
        // Resolved and enabled: the `Box` this names was created on this thread and is never
        // replaced or moved, so the word is as valid now as when it was filled.
        return p;
    }
    if p as usize == crate::tls_cluster::CURSOR_OFF {
        // Resolved-OFF: byte for byte the body this function had before the memo existed.
        return with_ctx_of(l, |ctx| ctx as *mut JitThreadCtx);
    }
    #[cold]
    #[inline(never)]
    fn fill(l: &Locals) -> *mut JitThreadCtx {
        // The insert is the historic one — the memo changes WHEN the address is derived, never
        // WHICH address it is, so the first call through either arm allocates the same ctx.
        let a = with_ctx_of(l, |ctx| ctx as *mut JitThreadCtx);
        l.ctx_ptr.set(if pinned_ctx_memo() {
            a
        } else {
            crate::tls_cluster::CURSOR_OFF as *mut JitThreadCtx
        });
        a
    }
    fill(l)
}
#[inline]
pub(crate) fn cells_push(p: (*const i64, usize)) {
    with_ctx(|ctx| ctx.push(p));
}
#[inline]
pub(crate) fn cells_pop() {
    with_ctx(|ctx| ctx.top = ctx.top.saturating_sub(1));
}
/// ── FAST-FRAME OPS: the same bookkeeping on an ALREADY-ACQUIRED ctx handle ──
/// A warm compiled→compiled dispatch used to do its cell-root push/pop and its
/// box-table base/unwind through FIVE separate `LocalKey::with` crossings
/// (each a lazy-init check plus a `RefCell` borrow) for what is one thread's
/// one struct. These take the address the caller already paid for once via
/// [`ctx_addr`], which is exactly the address the self-registering compiled
/// prologue loads and mutates, so the aliasing story is the established one:
/// the ctx `Box` is allocated on first use and never replaced or moved, and
/// no Rust borrow of it is held across the guest call.
///
/// A `RAVA_TLS_STAT` census of a real boot priced this: it removes 635.8M of the
/// engine's 1.654G crossings, 91.6% of every crossing `frames` performs in the
/// Done window. That is why the path is unconditional and no longer has an OFF
/// arm — see `jit_direct_invoke`.
///
/// # Safety
/// `ctx` must come from [`ctx_addr`] on the CURRENT thread.
#[inline]
pub(crate) unsafe fn ctx_cells_push(ctx: *mut JitThreadCtx, p: (*const i64, usize)) {
    unsafe { (*ctx).push(p) };
}
/// # Safety
/// `ctx` must come from [`ctx_addr`] on the CURRENT thread.
#[inline]
pub(crate) unsafe fn ctx_cells_pop(ctx: *mut JitThreadCtx) {
    unsafe { (*ctx).top = (*ctx).top.saturating_sub(1) };
}
/// This thread's box-table length WITHOUT touching `BOXES`: `ctx.box_len` is
/// an exact mirror (only `box_push` and `box_truncate` write either, and both
/// write both), which is why the compiled epilogue is already allowed to
/// decide on it alone.
///
/// # Safety
/// `ctx` must come from [`ctx_addr`] on the CURRENT thread.
#[inline]
pub(crate) unsafe fn ctx_box_len(ctx: *mut JitThreadCtx) -> usize {
    unsafe { (*ctx).box_len as usize }
}
/// Unwind the box table to `len`, skipping BOTH thread-local crossings when
/// the mirror proves nothing was pushed since `len` was taken. With
/// `box_len <= len` the `Vec::truncate` is a no-op and `box_len.min(len)`
/// re-writes the value it already holds, so the skip is observationally
/// identical.
///
/// # Safety
/// `ctx` must come from [`ctx_addr`] on the CURRENT thread.
#[inline]
pub(crate) unsafe fn box_truncate_via(ctx: *mut JitThreadCtx, len: usize) {
    if unsafe { ctx_box_len(ctx) } <= len {
        return;
    }
    box_truncate(len);
}
#[inline]
pub(crate) fn box_len() -> usize {
    tl().boxes.borrow().len()
}
#[inline]
pub(crate) fn box_push(v: crate::Value) -> usize {
    let l = tl();
    let idx = {
        let mut b = l.boxes.borrow_mut();
        let idx = b.len();
        b.push(v);
        idx
    };
    // SAFETY: `ctx_addr_of` hands back this thread's own ctx `Box`, which is allocated on
    // first use and never replaced or moved, and no Rust borrow of it is live here — the
    // `boxes` borrow above ended with its block, and nothing between that and this write can
    // reach the ctx. This is the same aliasing story `ctx_cells_push` already runs on.
    unsafe { (*ctx_addr_of(l)).box_len = (idx + 1) as u64 };
    idx
}
#[inline]
pub(crate) fn box_get(idx: usize) -> Option<crate::Value> {
    tl().boxes.borrow().get(idx).cloned()
}
#[inline]
pub(crate) fn box_truncate(len: usize) {
    let l = tl();
    l.boxes.borrow_mut().truncate(len);
    // SAFETY: as `box_push` — this thread's own never-moved ctx, no live borrow of it, and
    // the `boxes` guard above is dropped at the end of its own statement.
    unsafe {
        let c = ctx_addr_of(l);
        (*c).box_len = (*c).box_len.min(len as u64);
    }
}
/// Every live in-flight boxed Value (heap-GC ROOTS). A value living ONLY in the
/// box table — a compiled callee's boxed return held as a BOX-tagged cell in the
/// caller's JIT locals until its next use — was invisible to the mark-sweep
/// (`cell_to_root` maps BOX cells to None): the object was SWEPT mid-flight and
/// its handle reused, so the box later decoded to an UNRELATED object (a $Lambda
/// with a different capture arity → args-shifted call → the th=128 NPE).
/// ⚠Every REGISTERED THREAD's box table, for the same reason `native_args_gather` walks the
/// registry: a box held by a thread that is not the one collecting is still live.
pub(crate) fn boxes_gather(out: &mut Vec<crate::Value>) {
    let r = BOXES_REG.lock().unwrap_or_else(|e| e.into_inner());
    let me = tl().boxes.as_ptr() as usize;
    for &addr in r.iter().chain(if r.contains(&me) { None } else { Some(&me) }) {
        let b = unsafe { &*(addr as *const Vec<crate::Value>) };
        // λ-PIC diagnostic (RAVA_JIT_PIC_PROBE only): the box table is a ROOT SET
        // cloned in full on every collection, so a path that pushes without
        // truncating turns each GC into an O(box_len) copy. Sampled here (cold,
        // once per collection) rather than in box_push (hot).
        if crate::picprobe::enabled() {
            crate::picprobe::note_box_scan(b.len() as u64);
        }
        out.extend(b.iter().cloned());
    }
}
/// Addresses of every live thread's CELLS, for GC root scanning.
static CELLS_REG: Mutex<Vec<usize>> = Mutex::new(Vec::new());
pub(crate) fn depth() -> u32 {
    DEPTH.with(|d| d.get())
}
pub(crate) fn depth_inc() {
    DEPTH.with(|d| d.set(d.get() + 1));
}
pub(crate) fn depth_dec() {
    DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
}
#[inline]
pub(crate) fn native_depth() -> u32 {
    NATIVE_DEPTH.with(|d| d.get())
}
#[inline]
pub(crate) fn native_depth_inc() {
    NATIVE_DEPTH.with(|d| d.set(d.get() + 1));
}
#[inline]
pub(crate) fn native_depth_dec() {
    NATIVE_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
}
/// Seed for the interpreter's LOCAL op countdown (P2.1): read the TLS value
/// once per `run()` instead of touching TLS on every opcode.
#[inline]
pub(crate) fn op_countdown_seed() -> u32 {
    OP_COUNTDOWN.with(|d| d.get())
}
/// ⭐**CARRY THE REMAINDER BACK ACROSS A `run()` BOUNDARY** (row 417).
///
/// `op_countdown_seed` only READS this cell and `run()` drained a LOCAL copy that was then
/// discarded on return — so every `run()` entry restarted from a full `CHECK_INTERVAL` and only a
/// single invocation that itself executed 100,000 opcodes could ever reach a checkpoint. Measured
/// on a real boot: **125 op-loop checkpoints in fifteen minutes of interpretation**, where the
/// design intends roughly five hundred a second.
///
/// The safepoint is where collection, the network pump and the GIL hand-off all live, so losing it
/// is not a diagnostic problem: it is why a worker thread can never be handed the GIL, which is the
/// substrate every `supplyAsync` offload attempt has died on since row 367.
#[inline]
pub(crate) fn op_countdown_store(v: u32) {
    OP_COUNTDOWN.with(|d| d.set(v));
}

/// Re-arm the TLS countdown after a local countdown hit zero. Only writes are
/// ever made through these two — the local counter is the hot path.
#[inline]
pub(crate) fn op_countdown_restock() {
    OP_COUNTDOWN.with(|d| d.set(crate::opcodes::CHECK_INTERVAL));
}
/// Addresses (`*const Vec<usize>` as usize) of every live thread's frame stack.
static REG: Mutex<Vec<usize>> = Mutex::new(Vec::new());
/// Addresses of every live thread's in-flight native-arg stack and box table. These are
/// GC ROOT SETS exactly like `REG` and `CELLS_REG`, and were the two that stayed
/// thread-local — see `native_args_gather` for what that cost.
static NARGS_REG: Mutex<Vec<usize>> = Mutex::new(Vec::new());
static BOXES_REG: Mutex<Vec<usize>> = Mutex::new(Vec::new());

fn reg_add(reg: &Mutex<Vec<usize>>, addr: usize) {
    let mut r = reg.lock().unwrap_or_else(|e| e.into_inner());
    if !r.contains(&addr) {
        r.push(addr);
    }
}
pub(crate) fn register() {
    let l = tl();
    reg_add(&REG, l.local.get() as usize);
    // `ctx_addr_of`, not `ctx_addr`: the cluster is already resolved into `l`.
    reg_add(&CELLS_REG, ctx_addr_of(l) as usize);
    reg_add(&NARGS_REG, l.native_args.get() as usize);
    reg_add(&BOXES_REG, l.boxes.as_ptr() as usize);
    crate::execution_state::register();
}
pub(crate) fn unregister() {
    crate::execution_state::unregister();
    let l = tl();
    let addr = l.local.get() as usize;
    REG.lock().unwrap_or_else(|e| e.into_inner()).retain(|&a| a != addr);
    let caddr = ctx_addr_of(l) as usize;
    CELLS_REG.lock().unwrap_or_else(|e| e.into_inner()).retain(|&a| a != caddr);
    let naddr = l.native_args.get() as usize;
    NARGS_REG.lock().unwrap_or_else(|e| e.into_inner()).retain(|&a| a != naddr);
    let baddr = l.boxes.as_ptr() as usize;
    BOXES_REG.lock().unwrap_or_else(|e| e.into_inner()).retain(|&a| a != baddr);
}
/// Every live explicit-stack driver's frame vector.
///
/// The global registry is the collector's cross-thread root set. A separate
/// per-thread registry tracks the innermost active vector for each Vm so
/// diagnostics never read another worker's stack.
static EXEC_REG: Mutex<Vec<usize>> = Mutex::new(Vec::new());

thread_local! {
    static EXEC_LOCAL: RefCell<Vec<(usize, usize)>> = const { RefCell::new(Vec::new()) };
}

/// The current thread's active explicit stack for `owner`, if it is driving one.
pub(crate) fn exec_current(owner: usize) -> Option<usize> {
    EXEC_LOCAL.with(|scopes| {
        scopes
            .borrow()
            .iter()
            .rev()
            .find_map(|&(candidate, ptr)| (candidate == owner).then_some(ptr))
    })
}

/// Registers a driver's frame vector for the lifetime of the drive.
pub(crate) struct ExecScope {
    owner: usize,
    ptr: usize,
    _thread_affinity: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl ExecScope {
    pub(crate) fn new(owner: usize, ptr: usize) -> Self {
        EXEC_REG.lock().unwrap_or_else(|e| e.into_inner()).push(ptr);
        EXEC_LOCAL.with(|scopes| scopes.borrow_mut().push((owner, ptr)));
        ExecScope {
            owner,
            ptr,
            _thread_affinity: std::marker::PhantomData,
        }
    }
}
impl Drop for ExecScope {
    fn drop(&mut self) {
        let mut r = EXEC_REG.lock().unwrap_or_else(|e| e.into_inner());
        // Remove ONE occurrence: a re-entrant drive of the same vector must not
        // deregister the outer scope's entry too.
        if let Some(i) = r.iter().rposition(|&a| a == self.ptr) {
            r.remove(i);
        }
        EXEC_LOCAL.with(|scopes| {
            let mut scopes = scopes.borrow_mut();
            if let Some(i) = scopes
                .iter()
                .rposition(|&(owner, ptr)| owner == self.owner && ptr == self.ptr)
            {
                scopes.remove(i);
            }
        });
    }
}
/// Every registered driver frame vector, for GC root scanning.
pub(crate) fn exec_all() -> Vec<usize> {
    EXEC_REG.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Snapshot of every registered thread's live JIT cell roots, for GC. SAFETY
/// as `all`: GIL-held, owning threads parked with their compiled frames alive.
pub(crate) fn cells_all() -> Vec<(*const i64, usize)> {
    let cr = CELLS_REG.lock().unwrap_or_else(|e| e.into_inner());
    let mut out = Vec::new();
    for &addr in cr.iter() {
        let ctx = unsafe { &*(addr as *const JitThreadCtx) };
        for k in 0..ctx.top as usize {
            unsafe {
                let a = ctx.buf.add(k * 2);
                out.push((*a as *const i64, *a.add(1) as usize));
            }
        }
    }
    out
}

/// PHASE-A STACK MAPS: [`cells_all`] plus, per entry, the SP the publishing compiled frame
/// recorded (0 when none — see `JitThreadCtx::sp_buf`). Empty unless `RAVA_STACKMAPS=1`.
/// Same safety contract as [`cells_all`].
pub(crate) fn cells_all_sp() -> Vec<(*const i64, usize, u64)> {
    let cr = CELLS_REG.lock().unwrap_or_else(|e| e.into_inner());
    let mut out = Vec::new();
    for &addr in cr.iter() {
        let ctx = unsafe { &*(addr as *const JitThreadCtx) };
        if ctx.sp_buf.is_null() {
            continue;
        }
        for k in 0..ctx.top as usize {
            unsafe {
                let a = ctx.buf.add(k * 2);
                out.push((*a as *const i64, *a.add(1) as usize, *ctx.sp_buf.add(k)));
            }
        }
    }
    out
}
#[inline]
pub(crate) fn push(p: usize) {
    // SAFETY: per-thread cell; the borrow ends before anything else runs (see the field).
    unsafe { &mut *tl().local.get() }.push(p);
}
#[inline]
pub(crate) fn pop() {
    // SAFETY: as `push` — per-thread, and no call escapes while the borrow is live.
    unsafe { &mut *tl().local.get() }.pop();
}
/// Snapshot of every registered thread's frame pointers, for GC. SAFETY: each
/// address points to a still-live thread-local `Vec<usize>`; the owning thread
/// is parked at `gil::acquire` (not mutating it) while the GIL holder runs GC.
pub(crate) fn all() -> Vec<usize> {
    let r = REG.lock().unwrap_or_else(|e| e.into_inner());
    let mut out = Vec::new();
    for &addr in r.iter() {
        let v = unsafe { &*(addr as *const Vec<usize>) };
        out.extend_from_slice(v);
    }
    out
}
/// This thread's frame pointers (for backtrace on the current thread).
pub(crate) fn local(out: &mut Vec<usize>) {
    // SAFETY: as `push` — this thread's own stack, and `extend_from_slice` calls no guest code.
    out.extend_from_slice(unsafe { &*tl().local.get() });
}

/// `RAVA_POOL_CENSUS` — slots the pool's `clear()` runs `Value` drop glue over, per round
/// trip, and the number of checkouts they are divided by. The slots are counted wherever the
/// clear actually happens: on the RETURN side under the default fast pool, on the CHECKOUT
/// side under `RAVA_NO_FAST_POOL`. Either way it is one count per round trip, so the ratio
/// means the same thing. See `pool_take` and `pool_give`.
pub(crate) static POOL_TAKES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static POOL_SLOTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn pool_census() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_POOL_CENSUS").is_some())
}

pub fn pool_census_report() -> String {
    if !pool_census() {
        return String::new();
    }
    use std::sync::atomic::Ordering::Relaxed;
    let (t, s) = (POOL_TAKES.load(Relaxed), POOL_SLOTS.load(Relaxed));
    if t == 0 {
        return String::new();
    }
    format!(
        "[POOL-CENSUS] takes={t} slots_cleared={s} = {:.2} slots per round trip",
        s as f64 / t as f64
    )
}

/// Empty this thread's free list AND clear the miss-size hint, returning how many buffers
/// the list held. Test-only, and it reaches the list directly rather than draining through
/// [`pool_take`]: libtest runs the tests of one binary on the SAME thread under
/// `--test-threads=1`, so a test that leaves a hint behind would be observed by the next
/// one. Resetting both fields is what keeps each test's assertions about its own traffic.
#[cfg(test)]
fn pool_drain_for_test() -> usize {
    let (p, _) = pool_resolve();
    // SAFETY: as every other pool accessor — this thread's own cell, no borrow held across
    // a call, and the buffers dropped here are already cleared (or were never pooled).
    let pool = unsafe { &mut *p };
    let n = pool.list.len();
    pool.list.clear();
    pool.hint = 0;
    n
}

/// Is miss-sizing in force on this thread? Both pool switches have to be on, and a test
/// run with either of them set in the environment must skip rather than fail — the same
/// discipline every other gated behaviour in the tree is tested under.
#[cfg(test)]
fn pool_hint_active_for_test() -> bool {
    let (p, _) = pool_resolve();
    unsafe { (*p).hint_cap != 0 }
}

/// The one invariant the fast pool rests on, tested from both ends: whatever the
/// discipline in force, a buffer handed out by [`pool_take`] is indistinguishable from a
/// fresh `Vec` in length and contents. Moving the `clear()` from checkout to return is
/// only sound while that holds, and it is the kind of thing that fails silently — a stale
/// `Value` handed to a frame as a local reads as a live object of the wrong class.
#[cfg(test)]
mod pool_tests {
    use crate::Value;

    /// Empty this thread's free list so a test observes only its own traffic.
    fn drain() -> usize {
        super::pool_drain_for_test()
    }

    #[test]
    fn a_dirty_give_comes_back_clean() {
        drain();
        let mut v = Vec::with_capacity(8);
        // Exercise the owning variant too: `Str` is the one variant of eleven with drop
        // glue, so it is the one that proves the clear actually ran.
        v.push(Value::Int(7));
        v.push(Value::Str("held".into()));
        v.push(Value::Null);
        let addr = v.as_ptr() as usize;
        super::pool_give(v);

        let back = super::pool_take();
        assert_eq!(back.as_ptr() as usize, addr, "the SAME allocation came back");
        assert!(back.is_empty(), "a pooled buffer must hold no Values");
        assert!(back.capacity() >= 8, "capacity is retained, so no regrow");
    }

    #[test]
    fn a_capacity_zero_give_pools_nothing() {
        drain();
        // A capacity-0 give takes the branch that pools nothing AND records no hint, so the
        // checkout below still misses into the historic `Vec::new()`.
        super::pool_give(Vec::new());
        assert_eq!(super::pool_take().capacity(), 0, "nothing was pooled behind our back");
    }

    /// The free list is bounded: past `POOL_CAP` a give drops the buffer instead of
    /// growing without limit. (Checked at a scale the ceiling actually reaches, so the
    /// test fails if the bound is ever removed rather than merely raised.)
    #[test]
    fn the_pool_is_bounded() {
        drain();
        for _ in 0..super::POOL_CAP + 64 {
            super::pool_give(Vec::with_capacity(1));
        }
        assert_eq!(drain(), super::POOL_CAP, "the ceiling is enforced on the give side");
    }

    /// ⭐**A MISS MUST STILL LOOK LIKE A MISS.** Five tests — three of them in
    /// `opcodes::tests`, which this file may not edit — identify a pool miss by the
    /// capacity of what comes back, and the miss-sizing work must never move that
    /// observable. This pins it: the hint exists, and `pool_take` still hands back an
    /// unallocated `Vec`.
    #[test]
    fn a_miss_still_returns_an_unallocated_buffer() {
        drain();
        super::pool_give(Vec::<Value>::with_capacity(12));
        let hit = super::pool_take();
        assert!(hit.capacity() >= 12, "the pooled buffer itself came back");
        drop(hit);
        assert_eq!(super::pool_take().capacity(), 0, "a miss is still a bare Vec::new()");
    }

    /// The hint records the last give, so the caller that was about to allocate asks for a
    /// frame-sized buffer instead of an argument-sized one. Clamped, because one method
    /// with 4096 locals must not make every later miss on this thread allocate a page.
    #[test]
    fn the_size_hint_tracks_the_last_give_and_is_clamped() {
        if !super::pool_hint_active_for_test() {
            return; // a pool kill switch is set in this environment
        }
        drain();
        assert_eq!(super::pool_size_hint(), 0, "no give yet, so no hint and no change");

        // `>=`, not `==`: `Vec::with_capacity` is free to hand back more than it was asked
        // for, and the assertion this test exists to make is "the hint reflects the give",
        // not "the allocator is exact".
        let mut v = Vec::<Value>::with_capacity(12);
        let want = v.capacity().min(super::POOL_HINT_SLOTS as usize);
        v.push(Value::Int(1)); // a live slot, so the give's clear() is exercised too
        super::pool_give(v);
        assert_eq!(super::pool_size_hint(), want, "the hint is the last give's capacity");
        drop(super::pool_take());

        super::pool_give(Vec::<Value>::with_capacity(4096));
        assert_eq!(
            super::pool_size_hint(),
            super::POOL_HINT_SLOTS as usize,
            "POOL_HINT_SLOTS bounds what a miss may over-allocate"
        );
        drop(super::pool_take());

        // A capacity-0 give records nothing: it never reaches the pooling branch.
        drain();
        super::pool_give(Vec::new());
        assert_eq!(super::pool_size_hint(), 0, "an unallocated give is not an observation");
    }
}

#[cfg(test)]
mod exec_scope_tests {
    use super::{exec_current, exec_all, ExecScope};
    use crate::vmframe::Frame;
    use std::sync::Arc;

    type ExplicitFrames = Vec<(Frame, Arc<str>, Arc<str>)>;

    #[test]
    fn current_explicit_stack_is_nested_and_scoped_per_vm() {
        let outer = ExplicitFrames::new();
        let nested = ExplicitFrames::new();
        let other_vm = ExplicitFrames::new();
        let owner = 0xA11CE;
        let other_owner = 0xB0B;
        let outer_ptr = &outer as *const _ as usize;
        let nested_ptr = &nested as *const _ as usize;
        let other_ptr = &other_vm as *const _ as usize;

        assert_eq!(exec_current(owner), None);
        let outer_scope = ExecScope::new(owner, outer_ptr);
        assert_eq!(exec_current(owner), Some(outer_ptr));
        assert_eq!(exec_current(other_owner), None);
        assert!(exec_all().contains(&outer_ptr));

        {
            let _other_scope = ExecScope::new(other_owner, other_ptr);
            assert_eq!(exec_current(owner), Some(outer_ptr));
            assert_eq!(exec_current(other_owner), Some(other_ptr));
        }

        let nested_scope = ExecScope::new(owner, nested_ptr);
        assert_eq!(exec_current(owner), Some(nested_ptr));
        drop(outer_scope);
        assert_eq!(exec_current(owner), Some(nested_ptr));
        drop(nested_scope);
        assert_eq!(exec_current(owner), None);
        assert!(!exec_all().contains(&outer_ptr));
    }

    #[test]
    fn explicit_stack_lookup_is_isolated_between_threads() {
        let owner = 0xC0FFEE;
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let frames = ExplicitFrames::new();
            let ptr = &frames as *const _ as usize;
            let _scope = ExecScope::new(owner, ptr);
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            assert_eq!(exec_current(owner), Some(ptr));
        });

        entered_rx.recv().unwrap();
        assert_eq!(exec_current(owner), None);
        release_tx.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(exec_current(owner), None);
    }
}
