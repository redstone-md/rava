//! The interpreter frame, the trace flags, and the error type every path returns.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::value::Value;
use crate::*;
#[derive(Debug)]
pub enum VmError {
    UnknownOpcode(u8),
    StackUnderflow,
    NoCode(String),
    BadConstant(u16),
    UnsupportedNative(String),
    MethodNotFound(String),
    Load(LoadError),
    /// A Java exception in flight (the value is the thrown Throwable), unwinding
    /// until a frame's exception table catches it.
    Throw(Value),
    /// A GIL-free worker's region was aborted while it was deep inside a RECURSIVE
    /// invoke — one that runs on the Rust stack with `trampoline = false` and so
    /// cannot exit by returning a `RunExit` to a loop the way the trampoline can.
    ///
    /// It is deliberately NOT a `Throw`: only `Throw` is routed to `find_handler`,
    /// so this variant unwinds through every guest frame without any `catch` being
    /// able to swallow the abort. Results from the aborted region are discarded, so
    /// nothing it computed is observable.
    #[cfg(feature = "next-gen-gc")]
    RegionAbort,
}

impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VmError::UnknownOpcode(op) => write!(f, "unimplemented opcode 0x{op:02X}"),
            VmError::StackUnderflow => write!(f, "operand stack underflow"),
            VmError::NoCode(m) => write!(f, "method {m} has no Code attribute"),
            VmError::BadConstant(i) => write!(f, "bad/unsupported constant #{i}"),
            VmError::UnsupportedNative(m) => write!(f, "unsupported native {m}"),
            VmError::MethodNotFound(m) => write!(f, "method not found: {m}"),
            VmError::Load(e) => write!(f, "{e}"),
            VmError::Throw(_) => write!(f, "uncaught Java exception"),
            #[cfg(feature = "next-gen-gc")]
            VmError::RegionAbort => write!(f, "gil-free region aborted"),
        }
    }
}

impl std::error::Error for VmError {}

impl From<LoadError> for VmError {
    fn from(e: LoadError) -> Self {
        VmError::Load(e)
    }
}

/// A single method activation.
pub struct Frame {
    pub(crate) pc: usize,
    /// The method body this frame is executing. `Frame::new` is handed it anyway, and the
    /// explicit-stack driver otherwise has to find it again — by NAME, through the class's method
    /// table, every time control returns to a frame.
    ///
    /// A raw pointer because the `Code` lives inside the class this frame holds an `Arc` to, so it
    /// is valid for exactly as long as the frame is, and a borrow would make `Frame` self-referential.
    pub(crate) code: *const Code,
    /// ⭐⭐**ONE BUFFER: `[0, nlocals)` are the locals, `[nlocals, len)` is the operand stack.**
    ///
    /// This was two `Vec`s, and the profile of the CALL ITSELF (`FrameBench`, 42M framed calls
    /// and nothing else) said what that cost: `pool_take` + `pool_give` **7.7%** and
    /// `drop_in_place<Frame>` **2.8%**, against `Frame::new` at **1.6%** — *the buffer traffic
    /// cost five times what building the frame did*. The pool census gave the volume:
    /// **84,000,006 checkouts for 42M calls**, i.e. two buffers taken and two given back, per
    /// call. Merging them halves every one of those numbers and removes one `Vec` drop.
    ///
    /// It is also the shape stage 2 needs: once locals and operands are contiguous, the buffer
    /// can become a window into one per-thread stack, and then a call passes its arguments by
    /// LEAVING THEM WHERE THEY ARE. See `docs/contiguous-stack-plan.md`.
    pub(crate) buf: Vec<Value>,
    /// Where the operand stack begins inside `buf`. Fixed for the frame's life.
    pub(crate) nlocals: usize,
    pub(crate) class: Arc<LoadedClass>,
}

/// ⭐**PER-CALL FRAME WORK THAT IS NOT PER-CALL DATA. DEFAULT ON; `RAVA_NO_FAST_FRAME=1`
/// restores the old shape exactly.**
///
/// 14.1 M interpreted method bodies are entered per product boot and 91% of them build a
/// frame, so everything `Frame::new` does is multiplied by that. Two of the things it did
/// were not functions of the CALL at all:
///
/// * **Two growth points for one constant.** The buffer was `resize`d to `max_locals` and
///   then `reserve`d for `max_stack`. `Vec::reserve` is relative to `len`, and the resize
///   has already moved `len` to `max_locals`, so the two calls are two independent capacity
///   decisions — and a pooled buffer that came back sized for the ARGUMENTS reallocates
///   under BOTH: two `mi_malloc`s and two memcpys to reach a capacity that is a constant of
///   the method (`max_locals + max_stack`) and knowable before the first `Value` moves. The
///   census already named this the predicted site: `Site::FrameLocalsResize` is documented
///   as the buffer that "was just sized for the ARGUMENTS, then resized to the method's
///   LOCALS, which is strictly larger for almost every real method". One reservation, sized
///   from the method's own two `u16`s, removes the second realloc entirely.
///
/// * **A refcount for a pointer the caller already holds live.** `invoke_framed` takes
///   `&Arc<LoadedClass>` and holds it for the whole call; the frame is a local of that same
///   call and cannot outlive it. The `Arc::clone` was therefore a pair of atomics — one on
///   entry, one in the frame's drop — protecting a strong count that provably cannot reach
///   zero while the frame exists. Row 395 prices the same clone/drop pair at 12 ns of a
///   105 ns call. See [`FrameGuard::borrowed`].
///
/// The switch itself is read once per THREAD, not once per frame — see the gate block
/// below — and it is read there rather than threaded through the call so the sites cannot
/// disagree.
///
/// The historic definition, kept as the thing the cached bit is filled FROM (and as the
/// thing the kill switch below falls back to calling per frame, so its arm is the old
/// structure byte for byte, not "main plus overhead").
fn fast_frame_uncached() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_FRAME").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// ── THE FRAME PATH'S PROCESS CONSTANTS, RESOLVED ONCE PER THREAD ─────────────────────
///
/// ⭐**FOUR `OnceLock` READS SAT ON EVERY ONE OF THE 14.1 M INTERPRETED METHOD BODIES A
/// BOOT.** Three of `fast_frame()` — `vm::invoke::may_intercept`, `vm::invoke`'s choice of
/// [`FrameGuard::borrowed`] vs [`FrameGuard::owned`], and [`size_frame_buffer`] — plus one
/// of `vecgrow::enabled()` inside [`size_frame_buffer`], and a fifth in [`Frame::new`].
/// Every one of them answers a question fixed before the first guest opcode runs: is
/// `RAVA_NO_FAST_FRAME` in the environment, is `RAVA_VECGROW_STAT`.
///
/// **The cost is not the load, it is the BARRIER.** `*OnceLock::get_or_init(..)` reaches
/// its value through an `Ordering::Acquire` load on the once-state; on x86 that lowers to
/// a plain `mov`, but as an atomic it is an optimisation barrier — LLVM may not hoist it
/// out of the frame constructor's inline tree, may not fold the three `fast_frame()` reads
/// of one call into one, and must assume memory it can otherwise track is clobbered across
/// it. This tree has priced exactly that shape before: ledger row 405 took ONE relaxed
/// atomic off the operand-push path and measured **−15%**, and recorded the reason as
/// "an atomic's cost is that LLVM can't HOIST it, not the `mov`".
///
/// `frames.rs` already applies the cure to the buffer pool — its `POOL_GATES` block names
/// this as item 2, "the census gates are read once per thread, not three times per call" —
/// and the frame path never got it. This is the same three-state discipline in the same
/// shape: a per-thread `Cell<u8>` whose initialiser is a `const` block, which is the one
/// combination `std` lowers to a bare `#[thread_local]` static (no lazy-init check, no
/// destructor, nothing to call), so a gate read becomes a plain load LLVM is free to hoist
/// and to fold with its siblings.
///
/// **NOTHING ABOUT THE ANSWERS CHANGES.** Both bits are filled from the very same
/// `OnceLock`s, on a cold path that runs once per thread. This is not a memo and not a
/// side table: there is no key, no lookup, no miss and no invalidation — one byte per
/// thread holding two booleans that are constant for the life of the process.
///
/// KILL SWITCH `RAVA_NO_FRAME_GATES=1` (also honoured through `flags::emit_more_disabled`)
/// sets [`G_LIVE`] on that same cold path, and then every reader below calls the
/// `OnceLock`s per frame again exactly where it did before — an honest A/B, because the
/// off arm re-reads the same functions from the same places. It also restores
/// [`size_frame_buffer`]'s `Vec::resize`, for the reason stated at that site.

/// Bit 0 — this thread has resolved its gates. A byte of 0 therefore means "not yet",
/// which is what makes the fast read a single compare against zero.
const G_RESOLVED: u8 = 1;
/// Bit 1 — `fast_frame_uncached()`: the merged-buffer / borrowed-class frame shape is on.
const G_FAST: u8 = 2;
/// Bit 2 — `vecgrow::enabled()`: the vec-growth census is armed, so the frame path must
/// keep feeding it.
const G_CENSUS: u8 = 4;
/// Bit 3 — the kill switch is engaged: distrust the cached bits and call the `OnceLock`s
/// per frame, as the historic code did.
const G_LIVE: u8 = 8;
/// Bit 4 — [`fast_pool2_uncached`]: a `Frame` that dies OUTSIDE a [`FrameGuard`] hands its
/// buffer back to the pool instead of freeing it. See [`impl Drop for Frame`](Frame).
const G_RECLAIM: u8 = 16;

thread_local! {
    /// This thread's resolved frame gates; see the block comment above. `Cell<u8>` with a
    /// `const` initialiser, deliberately — the same shape `frames::POOL_GATES` and
    /// `frames::DEPTH` already use for a TLS slot with no destructor and no init check.
    static FRAME_GATES: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

/// One plain per-thread byte. Never 0 on return.
#[inline(always)]
fn frame_gates() -> u8 {
    let g = FRAME_GATES.get();
    if g != 0 { g } else { frame_gates_cold() }
}

/// First touch on this thread: read the two process constants and the kill switch through
/// their `OnceLock`s — the ONLY place that still happens under the default — and cache the
/// answers. Once per thread, so the environment lookups behind them stay where they were.
#[cold]
#[inline(never)]
fn frame_gates_cold() -> u8 {
    static LIVE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let live = *LIVE.get_or_init(|| {
        std::env::var_os("RAVA_NO_FRAME_GATES").is_some() || crate::flags::emit_more_disabled()
    });
    let mut g = G_RESOLVED;
    if fast_frame_uncached() {
        g |= G_FAST;
    }
    if crate::vecgrow::enabled() {
        g |= G_CENSUS;
    }
    if fast_pool2_uncached() {
        g |= G_RECLAIM;
    }
    if live {
        g |= G_LIVE;
    }
    FRAME_GATES.set(g);
    g
}

/// `RAVA_NO_FAST_FRAME` — is the current frame shape in force? Callers outside this file
/// (`vm::invoke`, twice per interpreted call) are unchanged and pick the cache up for free.
#[inline]
pub(crate) fn fast_frame() -> bool {
    let g = frame_gates();
    if g & G_LIVE != 0 {
        return fast_frame_uncached();
    }
    g & G_FAST != 0
}

/// ⭐**THE POOL DOES NOT MISS BECAUSE IT IS TOO SMALL; IT MISSES BECAUSE BUFFERS DO NOT
/// COME BACK.** `RAVA_NO_FAST_POOL2=1` restores the historic behaviour exactly: a `Frame`
/// that dies anywhere other than inside a [`FrameGuard`] FREES its buffer.
///
/// The census on `POOL_CAP` (see `frames::POOL_CAP`) reports **3,469,985 take misses with
/// 1,330,047 buffers LEAKED**, and raising the ceiling 1024 → 4096 moved the miss rate by a
/// quarter of a point for +100 MB of RSS. A miss is not a cheap event: `pool_take` hands
/// back `Vec::new()`, `pop_args_n` then `reserve(n + 1)` on it, and [`size_frame_buffer`]
/// reallocates that again to `max_locals + max_stack` — two `mi_malloc`s and a memcpy for a
/// buffer the process already owned.
///
/// **`Frame` HAD NO `Drop`** — that is the defect, and the [`impl Drop for Frame`](Frame) at
/// the bottom of this file is the fix. `vm::drive::recycle_frame` says so in as many words and
/// works around it by hand for exactly ONE of the seven places a frame dies; every other one
/// dropped a `Frame` by value and `Vec`'s own destructor freed the allocation:
///
/// * `vm::drive::drive_stack` — any error exit drops the whole `Vec<(Frame, ..)>` at once,
///   which is one free per live frame on the explicit stack;
/// * `vm::jit_call_body` — the synthetic seam frame is recycled on the success path, but
///   `dispatch?` returns BEFORE that line, so every guest throw crossing the seam frees one;
/// * `vm::jit_natives` — same shape, `r?` ahead of the recycle;
/// * `jit::callbacks::deopt`, `vm::lifecycle`, `vm::fork_chunk`, `vm::fork_counted` — frames
///   built by `from_parts`/`Frame::new` and simply dropped.
///
/// So the give side is plugged where it belongs: on the type that owns the buffer, which is
/// the one place that cannot be forgotten by a new call site, an early `?` or an unwind.
/// This ADDS NOTHING to the frame — no field, no memo, no side table — and changes no
/// observable behaviour: the same `Value`s are dropped at the same moment either way (see
/// `frames::pool_give`, which clears on the way in), only the ALLOCATION is retained instead
/// of returned to the allocator.
fn fast_pool2_uncached() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_POOL2").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// `RAVA_NO_FAST_POOL2` — off the same per-thread byte as its siblings, so `Frame::drop`
/// tests one plain load rather than an `OnceLock` acquire per frame.
#[inline(always)]
fn frame_reclaim() -> bool {
    let g = frame_gates();
    if g & G_LIVE != 0 {
        return fast_pool2_uncached();
    }
    g & G_RECLAIM != 0
}

/// `RAVA_VECGROW_STAT` — is the vec-growth census armed? The frame path's own reader, so
/// the two per-frame sites (`size_frame_buffer`, `Frame::new`) fold into one byte load.
///
/// `pub(crate)` for the TAKE side: `opcodes::statics::pop_args_n` ends every interpreted
/// call with a `vecgrow::note`, whose first act is a fresh `OnceLock` acquire over this same
/// process constant. It reads the byte this thread has already resolved instead — the gate
/// is shared, so no second piece of state exists to disagree with this one.
#[inline(always)]
pub(crate) fn frame_census() -> bool {
    let g = frame_gates();
    if g & G_LIVE != 0 {
        return crate::vecgrow::enabled();
    }
    g & G_CENSUS != 0
}

/// Size the frame's ONE buffer so that `[0, max_locals)` is initialised and
/// `[max_locals, max_locals + max_stack)` is reserved — in a single growth decision.
///
/// The guarantee handed to the interpreter is unchanged and is what `Frame::push`'s
/// hand-written fast path relies on: `capacity >= max(len, max_locals) + max_stack`, so the
/// capacity branch is never taken by valid bytecode. What changed is that the guarantee is
/// now computed once from the method's constants instead of being approached in two steps
/// that can each reallocate.
#[inline]
fn size_frame_buffer(buf: &mut Vec<Value>, nlocals: usize, max_stack: usize) {
    let len = buf.len();
    // ONE byte load answers both gates (and the kill switch), instead of two `OnceLock`
    // acquire loads straddling the whole body. See the gate block above.
    let gates = frame_gates();
    let live = gates & G_LIVE != 0;
    let fast = if live { fast_frame_uncached() } else { gates & G_FAST != 0 };
    if !fast {
        // Kill switch: the original two-step shape, byte for byte.
        if len < nlocals {
            let (cb, lb) = (buf.capacity(), len);
            buf.resize(nlocals, Value::Null);
            crate::vecgrow::note(crate::vecgrow::Site::FrameLocalsResize, cb, buf.capacity(), lb);
        }
        let scb = buf.capacity();
        buf.reserve(max_stack);
        crate::vecgrow::note(crate::vecgrow::Site::FrameStackReserve, scb, buf.capacity(), 0);
        return;
    }
    // `len` may already exceed `max_locals` — a synthetic frame built by `from_parts`-style
    // callers can arrive with operands in place — so the locals region is `max(len, nlocals)`,
    // exactly as the two-step shape treated it.
    let need = if len < nlocals { nlocals } else { len } + max_stack;
    // Read the census flag ONCE — and now from the byte already in hand, not from a second
    // `OnceLock`. `vecgrow::note` re-reads it internally, and two reads of a process
    // constant per frame is the kind of per-call tax this whole function exists to delete.
    let census = if live { crate::vecgrow::enabled() } else { gates & G_CENSUS != 0 };
    let cb = if census { buf.capacity() } else { 0 };
    if need > buf.capacity() {
        // `reserve` is relative to `len`, and `len` has NOT been moved yet — this is the one
        // place the old code got wrong, by reserving relative to a length it had just grown.
        buf.reserve(need - len);
    }
    if len < nlocals {
        // Cannot reallocate: the reservation above already covers `nlocals + max_stack`.
        if live {
            // Kill switch: `Vec::resize`, exactly as before.
            buf.resize(nlocals, Value::Null);
        } else {
            // ⭐**THE RESERVATION IS ALREADY MADE — `Vec::resize` MAKES IT AGAIN.** `resize`
            // funnels into `extend_with`, and `extend_with`'s FIRST act is `self.reserve(n)`:
            // a second capacity compare and a second cold-grow call site inlined into the
            // frame constructor, guarding a capacity the `need` block three lines up has
            // just established for the whole `nlocals + max_stack` window. It then fills by
            // `value.clone()` per slot, so the loop also carries `Value`'s clone branch for
            // a constant the compiler can see is `Null`.
            //
            // This is the same treatment `Frame::push` already gets in this file — spell the
            // fast path out, leave the grow to the cold call — on the other half of the same
            // guarantee, and it is the half that runs on EVERY frame whose method declares
            // more locals than the call passed arguments, i.e. almost every real method.
            //
            // SAFETY: `need >= nlocals` by construction and the block above leaves
            // `capacity >= need`, so every slot in `len..nlocals` is allocated and
            // uninitialised. `ptr::write` initialises without reading or dropping what was
            // there, which is what those slots require — they are past `len`, so no `Value`
            // owns them. The length is published only after the last write, and constructing
            // `Value::Null` cannot unwind, so no path can observe a length covering an
            // uninitialised slot.
            unsafe {
                let p = buf.as_mut_ptr();
                for i in len..nlocals {
                    std::ptr::write(p.add(i), Value::Null);
                }
                buf.set_len(nlocals);
            }
        }
    }
    if census {
        let ca = buf.capacity();
        crate::vecgrow::note(crate::vecgrow::Site::FrameLocalsResize, cb, ca, len);
        // Reported as a NON-GROW on purpose. The operand headroom is part of the single
        // reservation above, so this site can no longer move an allocation; a `GREW` column
        // that reads 0 here is the fix showing up in the census, not the site going quiet.
        crate::vecgrow::note(crate::vecgrow::Site::FrameStackReserve, ca, ca, 0);
    }
}

impl Frame {
    /// ⚠`spare` exists only for the handful of callers that still hand a buffer in; it is given
    /// straight back to the pool. The hot paths now pass `Vec::new()`, which `pool_give` drops on
    /// the floor for free (capacity 0), so a framed call checks out ONE buffer and returns ONE.
    pub(crate) fn new(code: &Code, args: Vec<Value>, class: Arc<LoadedClass>, spare: Vec<Value>) -> Self {
        // ⭐**A POOL ROUND-TRIP FOR A BUFFER THE POOL THROWS AWAY.** Every hot caller passes
        // `Vec::new()`, and `pool_give` discards a capacity-0 buffer — so the call was a
        // thread-local resolve, a branch and a census probe to reach a `drop` of nothing. Skip
        // it when there is nothing to return; the census still sees the give when it is armed,
        // so the take/give balance it audits stays readable.
        if spare.capacity() > 0 || frame_census() {
            crate::frames::pool_give(spare);
        }
        Self::build(code, args, class)
    }

    /// `new` without the `spare` hand-back — the shape every hot call site actually wants.
    #[inline]
    fn build(code: &Code, args: Vec<Value>, class: Arc<LoadedClass>) -> Self {
        let mut buf = args;
        let nlocals = code.max_locals as usize;
        size_frame_buffer(&mut buf, nlocals, code.max_stack as usize);
        Self { pc: 0, code, buf, nlocals, class }
    }

    /// The frame's locals — `buf[..nlocals]`.
    #[inline(always)]
    pub(crate) fn locals(&self) -> &[Value] {
        // SAFETY-free: `new` resizes to `nlocals` and nothing ever shrinks below it.
        &self.buf[..self.nlocals]
    }
    #[inline(always)]
    pub(crate) fn locals_mut(&mut self) -> &mut [Value] {
        let n = self.nlocals;
        &mut self.buf[..n]
    }
    /// The operand stack — `buf[nlocals..]`.
    #[inline(always)]
    pub(crate) fn stack(&self) -> &[Value] {
        &self.buf[self.nlocals..]
    }
    #[inline(always)]
    pub(crate) fn stack_mut(&mut self) -> &mut [Value] {
        let n = self.nlocals;
        &mut self.buf[n..]
    }
    #[inline(always)]
    pub(crate) fn stack_len(&self) -> usize {
        self.buf.len() - self.nlocals
    }
    /// Drop every operand, keeping the locals. The unwinder's `stack.clear()`.
    #[inline]
    pub(crate) fn clear_stack(&mut self) {
        self.buf.truncate(self.nlocals);
    }
    /// `Vec::pop` semantics for the operand region — `None` at the locals boundary, never a
    /// local. The `Result` form is `pop`.
    #[inline(always)]
    pub(crate) fn pop_opt(&mut self) -> Option<Value> {
        if self.buf.len() > self.nlocals { self.buf.pop() } else { None }
    }
    /// Set the operand count WITHOUT dropping what is removed — for a caller that has already
    /// MOVED those values out bitwise.
    ///
    /// ⚠This is the distinction that broke the engine when stage 1 first landed: `pop_args_n`
    /// `copy_nonoverlapping`s the arguments into the callee's buffer and then used `set_len` to
    /// forget them. Replacing that with `truncate` ran drop glue on values that had already been
    /// moved — a double free, and the first real program crashed in `sb_append` with an access
    /// violation. The unit tests all passed; the bit-exact gates caught it.
    ///
    /// # Safety
    /// `n` must be <= the current operand count and every removed slot must already be owned
    /// elsewhere.
    #[inline]
    pub(crate) unsafe fn set_stack_len(&mut self, n: usize) {
        unsafe { self.buf.set_len(self.nlocals + n) }
    }
    /// Truncate the operand stack to `n` operands (not `n` buffer slots), DROPPING the rest.
    #[inline]
    pub(crate) fn truncate_stack(&mut self, n: usize) {
        self.buf.truncate(self.nlocals + n);
    }
    /// Build a frame from a buffer that ALREADY has locals at the front — the shape the
    /// explicit-stack driver and the fork lanes construct by hand.
    pub(crate) fn from_parts(code: *const Code, buf: Vec<Value>, nlocals: usize, class: Arc<LoadedClass>) -> Self {
        Self { pc: 0, code, buf, nlocals, class }
    }

    /// Take the operands off, leaving the locals — the `split_off` the trampoline used.
    pub(crate) fn take_stack(&mut self) -> Vec<Value> {
        self.buf.split_off(self.nlocals)
    }

    /// The frame's class, as a reference NOT borrowed from the frame.
    ///
    /// A frame's class is set at construction and never reassigned, and the frame owns an `Arc`
    /// to it for its whole life — so a reference derived from that `Arc` is valid for as long as
    /// the frame is, which is the whole call. The borrow checker cannot express "this field
    /// outlives the &mut on its container", and every interpreted call needs exactly that: read
    /// the resolved call site out of the class while popping arguments off the frame.
    ///
    /// # Safety
    /// The caller picks the lifetime and must keep it inside the frame's own.
    #[inline(always)]
    pub(crate) unsafe fn class_detached<'a>(&self) -> &'a LoadedClass {
        unsafe { &*Arc::as_ptr(&self.class) }
    }

    /// `Vec::push` is `#[inline]`, but rustc outlined it anyway — `Vec::push_mut` was a 7.2%
    /// row of a pure interpreter loop. `Frame::new` reserves `max_stack`, which the verifier
    /// says no valid bytecode exceeds, so the capacity branch here is never taken; spelling the
    /// fast path out keeps it in the caller and leaves the grow to the cold call.
    #[inline(always)]
    pub(crate) fn push(&mut self, v: Value) {
        let len = self.buf.len();
        if len < self.buf.capacity() {
            // SAFETY: `len < capacity`, so the slot is allocated and uninitialised; writing it
            // and then publishing the new length is exactly what `Vec::push` does.
            unsafe {
                std::ptr::write(self.buf.as_mut_ptr().add(len), v);
                self.buf.set_len(len + 1);
            }
        } else {
            self.buf.push(v);
        }
    }

    #[inline(always)]
    pub(crate) fn pop(&mut self) -> Result<Value, VmError> {
        // ⚠Underflow is now `len == nlocals`, not `len == 0`: popping past the operand region
        // would hand back a LOCAL, which is a silently wrong value rather than an error.
        if self.buf.len() > self.nlocals {
            match self.buf.pop() {
                Some(v) => Ok(v),
                None => Err(self.underflow()),
            }
        } else {
            Err(self.underflow())
        }
    }

    /// The diagnostic half of `pop`, OUT OF LINE. `pop` is `#[inline(always)]` and the
    /// interpreter calls it about a hundred times across its arms; with the `env::var` and the
    /// `eprintln!` inline, each of those hundred sites carried a copy of the environment lookup
    /// and the whole `core::fmt` machinery. That is most of why the hot opcode function
    /// disassembled to 75 KB with a 1000-byte stack frame and no registers left for the
    /// interpreter's own pointers.
    #[cold]
    #[inline(never)]
    fn underflow(&self) -> VmError {
        if std::env::var("RAVA_TRACE_UNDERFLOW").is_ok() {
            eprintln!("[UNDERFLOW] {} pc={}", self.class.file.this_class, self.pc);
        }
        VmError::StackUnderflow
    }

    #[inline(always)]
    pub(crate) fn pop_int(&mut self) -> Result<i32, VmError> {
        let v = self.pop()?;
        match v.as_int() {
            Some(i) => Ok(i),
            None => Err(self.not_an_int(&v)),
        }
    }

    /// As `underflow`: out of line, for the same reason.
    #[cold]
    #[inline(never)]
    fn not_an_int(&self, v: &Value) -> VmError {
        if std::env::var("RAVA_TRACE_UNDERFLOW").is_ok() {
            eprintln!(
                "[POPINT-FAIL] {} pc={} got non-int {:?}",
                self.class.file.this_class, self.pc, v
            );
        }
        VmError::StackUnderflow
    }
}

/// ⭐**THE FRAME OWNS ITS BUFFER, SO THE FRAME RETURNS IT.** See [`fast_pool2_uncached`] for
/// the census this exists to close: the pool misses 3.47 M times a boot with 1.33 M buffers
/// never handed back, because seven of the eight places a `Frame` dies drop it by value and
/// `Vec`'s destructor frees the allocation. Kill switch `RAVA_NO_FAST_POOL2=1` (also honoured
/// through `flags::emit_more_disabled`) makes this destructor a no-op, which is byte for byte
/// the historic behaviour — a `Frame` with no `Drop` at all.
///
/// **THE HOT PATH DOES NOT PAY FOR THIS.** [`FrameGuard::drop`] still hands the buffer back
/// by hand, at the same point in the call, under both arms — deliberately, so the guard's
/// behaviour is not coupled to this switch and so the borrowed-class arm (which must never
/// run `Frame`'s destructor) keeps working unchanged. It leaves an empty `Vec` behind, so on
/// every interpreted call this body is a capacity load, a test against zero and a
/// not-taken branch, ahead of the gate.
///
/// **NO DOUBLE GIVE IS POSSIBLE.** Every existing hand-back (`FrameGuard::drop`,
/// `vm::drive::recycle_frame`, `vm::jit_call_body`) takes the buffer with `mem::take`, which
/// installs a capacity-0 `Vec` — and a capacity-0 buffer is rejected on the line below,
/// before the pool is even resolved. A buffer therefore reaches `pool_give` from at most one
/// of the two routes.
///
/// GC contract: `frames::pool_give` clears the buffer before it enters the free list, so the
/// same `Value`s are dropped at the same instant they were dropped when this allocation was
/// simply freed, and the free list — which the collector does not scan — never holds a live
/// cell. Every site that publishes a frame's buffer as a GC root (`frames::push` in
/// `jit_call_body`, `jit_natives`, `deopt`) calls `frames::pop` before the frame can die,
/// including on the `?` exits this destructor exists to catch.
impl Drop for Frame {
    #[inline]
    fn drop(&mut self) {
        // Capacity, not length: a frame whose buffer was already taken has `len == 0` too,
        // but so does a live frame that has just been built for a zero-local method. Only a
        // capacity of zero proves there is no allocation here to give back.
        if self.buf.capacity() == 0 {
            return;
        }
        if frame_reclaim() {
            // `mem::take` and not a move out of `self`: a `Drop` impl only ever sees
            // `&mut self`, and the empty `Vec` left behind is what the field drop that
            // follows this body destroys — for free.
            crate::frames::pool_give(std::mem::take(&mut self.buf));
        }
    }
}

/// A frame that lives on the Rust stack for exactly one call, plus the two things that must
/// happen when it dies — including on the UNWIND path, which is the reason this is a guard
/// and not a pair of statements at the bottom of `invoke_framed`.
///
/// It exists to make [`FrameGuard::borrowed`] safe. A frame whose `class` is a bitwise copy of
/// a caller-held `Arc` must never run `Arc`'s destructor: that would decrement a strong count
/// this frame never incremented, and the second such call frees a `LoadedClass` that is still
/// in use. `opcodes::run` can panic (two of the `RunExit` arms in `invoke_framed` do so by
/// design), so "drop the buffer and `mem::forget` the frame at the end of the function" is not
/// enough — an unwind would skip it. Wrapping the frame in `ManuallyDrop` and deciding in
/// `Drop` makes both paths, return and unwind, take the same branch.
///
/// Cost when the borrowed form is off: one `bool` in the guard and a predictable branch in a
/// destructor that already had to touch the pool.
pub(crate) struct FrameGuard {
    f: std::mem::ManuallyDrop<Frame>,
    /// `true` = `f.class` is a real owning `Arc` and the frame drops normally.
    owns_class: bool,
}

impl FrameGuard {
    /// The frame owns its class `Arc`, exactly as every frame did before.
    #[inline]
    pub(crate) fn owned(f: Frame) -> Self {
        Self { f: std::mem::ManuallyDrop::new(f), owns_class: true }
    }

    /// Build a frame whose `class` is a BITWISE COPY of a caller-held `Arc`: no increment on
    /// entry, no decrement on drop, and the same pointer — so every `Arc::as_ptr(&frame.class)`
    /// memo key, every `Arc::clone(&frame.class)` (which takes a real reference off a count the
    /// true owner is holding above us) and every `frame.class.file…` read behaves identically.
    ///
    /// # Safety
    /// * `class` must be a live `&Arc<LoadedClass>` for the whole life of the returned guard —
    ///   in practice a `&Arc` PARAMETER of the function that builds and drops the guard, which
    ///   is what the borrow checker already proves for the caller's own reference.
    /// * The guard must not escape that function, and nothing may move the frame's `class`
    ///   field out by value: a bitwise copy dropped as if owned is a decrement never paid for.
    ///   Every in-tree use is `&frame.class`, `Arc::as_ptr(&frame.class)` or a `.clone()`, all
    ///   of which are reads.
    #[inline]
    pub(crate) unsafe fn borrowed(code: &Code, args: Vec<Value>, class: &Arc<LoadedClass>) -> Self {
        // SAFETY: `Arc` is a plain pointer; duplicating the bits duplicates the handle without
        // touching the count. The `owns_class: false` below is what keeps the duplicate from
        // ever being destroyed.
        let class = unsafe { std::ptr::read(class) };
        Self { f: std::mem::ManuallyDrop::new(Frame::build(code, args, class)), owns_class: false }
    }
}

impl std::ops::Deref for FrameGuard {
    type Target = Frame;
    #[inline(always)]
    fn deref(&self) -> &Frame {
        &self.f
    }
}
impl std::ops::DerefMut for FrameGuard {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Frame {
        &mut self.f
    }
}

impl Drop for FrameGuard {
    #[inline]
    fn drop(&mut self) {
        // ONE buffer back to this thread's pool — the same `frames::pool_give` the call path
        // used to perform by hand, at the same point in the call, now also correct under an
        // unwind. The second `give_vec(Vec::new())` that sat beside it is gone: stage 1 merged
        // locals and operands into this one buffer, so the operand buffer it returned had not
        // existed for some time and the call was a thread-local resolve plus a census probe to
        // hand the pool a capacity-0 `Vec` it immediately discards.
        let buf = std::mem::take(&mut self.f.buf);
        crate::frames::pool_give(buf);
        if self.owns_class {
            // SAFETY: this frame holds a strong count of its own; releasing it here is the
            // ordinary `Frame` drop. `buf` above is already an empty `Vec` (drop is a no-op),
            // `code` is a raw pointer and `pc`/`nlocals` are plain data, so the only work left
            // is the refcount decrement.
            //
            // ⚠`Frame` now has a `Drop` of its own, which this runs first — and it is a
            // capacity load and a not-taken branch here, because the `mem::take` above left
            // an empty `Vec`. That is why the give stays on THIS side rather than being
            // delegated: the guard's hand-back must not become conditional on
            // `RAVA_NO_FAST_POOL2`, and the borrowed arm below must never reach `Frame`'s
            // destructor at all (the field drop behind it would release an `Arc` count this
            // frame never took).
            unsafe { std::mem::ManuallyDrop::drop(&mut self.f) };
        }
    }
}

/// Diagnostic flags, read from the environment ONCE at startup. Checking
/// `std::env::var` per instruction/call is a syscall + allocation and tanks
/// throughput, so the hot paths consult these cached booleans instead.
#[derive(Default)]
pub struct TraceFlags {
    pub throw: bool,
    /// RAVA_TRACE_FATAL — low-volume (`*Error`-only) boot-killer trace; needs the
    /// call_stack populated for its backtrace, like `throw`.
    pub fatal: bool,
    pub athrow: bool,
    pub lambda: bool,
    pub frontier: bool,
    pub underflow: bool,
    pub jit_stats: bool,
    /// RAVA_HEARTBEAT — periodic op-count probe.
    pub heartbeat: bool,
    /// RAVA_TRACE_ITEM — maintain the logical call_stack (like `throw`) so the
    /// targeted ItemStack.isEmpty NPE diagnostic can print a real backtrace,
    /// without the full RAVA_TRACE_THROW flood.
    pub item: bool,
    /// RAVA_TRACE_GT — green-thread spawn logging + periodic heap-composition
    /// dump (leak diagnosis).
    pub gt: bool,
    /// RAVA_TRACE_BIG — log putfield of a double field whose value is abnormally
    /// large (|v| > 1e7), with the call stack. Pinpoints where a coordinate /
    /// velocity blows up (the player-tick X,Z explosion → ±3e7 clamp).
    pub big: bool,
    /// RAVA_TRACE_OPS substring filter (per-op trace). Read once at startup;
    /// reading these per `run()` was a syscall on every method invocation.
    pub ops: Option<String>,
    /// RAVA_TRACE_CODEC — Wall #11 datapack-codec hunt: dump the message +
    /// `$cause` chain + named backtrace of decode-time throws (needs the
    /// call_stack populated, like `throw`).
    pub codec: bool,
}
