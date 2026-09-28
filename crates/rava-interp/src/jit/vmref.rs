//! Reaching the VM from compiled code, and naming the method that is running. Every extern "C" bridge starts here.
//!
//! Re-exported from the parent module, so no call site outside it changed.
//!
//! ── WHAT A HOST CROSSING PAYS BEFORE IT DOES ANY WORK ────────────────────────────────
//!
//! A product boot takes ~30M compiled→host crossings, about four per compiled method
//! entry, and every one of the 78 `extern "C"` bridges opens with the same three
//! statements:
//!
//! ```ignore
//! cb(KIND);                                                    // the meter
//! let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };  // the lookup
//! if vm.jit_pending_is_set() { return 0; }                      // the region guard
//! ```
//!
//! Priced by reading, in the units `crate::tls_cluster` established — a `thread_local!`
//! lowers to a bare TLS-slot load ONLY when it is `const`-initialised AND its type has no
//! destructor; anything else compiles to an out-of-line `LocalKey::with` with a lazy-init /
//! destructor-registration state check on EVERY access:
//!
//! * **the lookup** — `CURRENT_VM` is `Cell<*mut Vm>`, `const`-initialised, no destructor.
//!   It is ALREADY the cheap shape: a TLS-slot load and a null test. The only thing it was
//!   paying was the accessor's own out-of-line `call`, because none of the four accessors
//!   carried `#[inline]` and they are consumed from twelve other files (thin LTO may import
//!   them, and may not). That is now one `#[inline(always)]` primitive, [`vm_ptr`].
//! * **the region guard** — `Vm::jit_pending_is_set` reaches `execution_state::STATE`,
//!   whose payload owns an `Option<Value>` and therefore has a destructor: the expensive
//!   TLS shape, paid on every crossing. It is not this module's to fix — see the adoption
//!   note in `jit/callbacks/mod.rs`.
//! * **the meter** — `cb()` is three gates deep and lives in `jit/counters.rs`; its
//!   `gilfree_cf::note_cb` arm is an ungated thread-local write on all 30M crossings.
//!   Also not this module's, and also recorded there.
//!
//! ⭐**THE PART THAT WAS THIS MODULE'S, AND WAS THE EXPENSIVE SHAPE, IS THE ONE NOBODY
//! READS: the compiled-method stack.** `jit_method_push`/`jit_method_pop` bracket EVERY
//! compiled entry in `Vm::jit_enter`, and their storage was a `RefCell<Vec<u32>>` — a
//! destructor-bearing thread-local, so each of the two crossings per entry paid the
//! out-of-line `LocalKey::with` plus its init-state check, then a `RefCell` borrow-flag
//! load/compare/STORE and its release store, then a `Vec` capacity check. Roughly thirty
//! instructions and four dependent loads per compiled entry, and the whole instrument is
//! forensic: its consumers are `crash_handler`, `RAVA_PROBE_STALE`, `RAVA_JIT_DUMP`,
//! `RAVA_TRACE_DIVREM`, the CLINIT-BARRIER and WATCH-CLASS traces. Nothing on the
//! execution path reads it. It is now a `const`-initialised, destructor-free fixed-depth
//! array — the one shape that lowers to a bare TLS load — so the instrument keeps working
//! (the crash reporter still names the compiled method) at a fraction of the price.
//!
//! ── WHERE THE PINNED CONTEXT REGISTER LANDS ──────────────────────────────────────────
//!
//! [`vm_ptr`] is the single point at which a `Vm` address enters host code from a compiled
//! frame; every accessor below is a thin wrapper over it. When a pinned context register
//! arrives, its body is the ONE line that changes.

use super::{resolve_lock_held_by_me, s09census};

use std::cell::Cell;
thread_local! {
    /// `Cell<*mut Vm>`, `const`-initialised, no destructor — the shape that lowers to a
    /// bare TLS-slot load. Read once per host crossing through [`vm_ptr`].
    static CURRENT_VM: Cell<*mut crate::Vm> = const { Cell::new(std::ptr::null_mut()) };
}

/// `RAVA_NO_FAST_CBABI=1` — kill switch for the folded callback prologue, DEFAULT ON. Also
/// consults the block switch, so `RAVA_NO_EMIT_MORE=1` reverts this with the rest of the
/// "cross less" work in one run.
///
/// What the OFF arm restores is the compiled-method stack's historic `RefCell<Vec<u32>>`
/// storage, i.e. UNBOUNDED recording depth (see [`JM_DEPTH`]). Nothing else in this file is
/// switchable: `#[inline]`, the cold-split of the gil-free denial and the single [`vm_ptr`]
/// primitive change no behaviour at all and so have nothing to revert to.
///
/// Read ONCE PER THREAD, on the cold fill in [`jm_mode`], and never on either fast path —
/// the answer is cached in the per-thread mode byte, so the OFF arm reaches the historic
/// body with no call and no gate re-read and is therefore an honest A/B baseline rather
/// than "main plus a handicap".
pub(crate) fn fast_cbabi() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_CBABI").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// ⭐**THE ONE VM LOOKUP.** Every accessor in this file — and therefore every one of the 78
/// `extern "C"` bridges — reaches the `Vm` through exactly this function and nowhere else.
///
/// Today it is a TLS-slot load: `CURRENT_VM` is `const`-initialised and destructor-free, so
/// there is no init check, no call and nothing to register. `Vm::jit_enter` publishes the
/// pointer once per compiled entry (`set_current_vm` / `set_current_vm_worker`) and every
/// crossing inside that entry re-reads it here.
///
/// **THE PINNED-CONTEXT-REGISTER SWITCH IS THIS BODY, AND IT IS ONE LINE.** When a sibling
/// pins the context in a machine register and threads it through the callback ABI, the
/// replacement is `pinned_ctx_vm()` — a register read instead of a TLS read — and nothing
/// else in this file or in any bridge moves, because nothing else names `CURRENT_VM`. The
/// three-state cursor convention (`null` = unresolved,
/// `crate::tls_cluster::CURSOR_OFF` = resolved-and-disabled, anything else = the payload)
/// is available here too if that switch wants to be an A/B rather than a replacement.
#[inline(always)]
pub(crate) fn vm_ptr() -> *mut crate::Vm {
    CURRENT_VM.with(|c| c.get())
}

/// ── THE COMPILED-METHOD STACK (FORENSIC INSTRUMENT) ──────────────────────────────────
///
/// Recording depth of the fast, destructor-free storage. Frames at depth < `JM_DEPTH` are
/// recorded exactly; deeper compiled recursion is not recorded at all, and the readers
/// below then report NOTHING rather than a shallower frame's name.
///
/// Missing beats wrong: this instrument's job is to name the method executing at a fault,
/// and a crash report naming the wrong method is worse than one naming none. A ring buffer
/// would keep the innermost 64 at every depth, but an excursion that went 64 deeper and
/// returned would leave a STALE id in a slot the reader then trusts, which is exactly the
/// wrong-name failure. Every consumer asks for at most 8 frames, a compiled recursion 64
/// deep does not occur in a boot, and `RAVA_NO_FAST_CBABI=1` restores unbounded depth if
/// one ever does.
const JM_DEPTH: usize = 64;

const JM_MODE_UNSET: u8 = 0;
const JM_MODE_FAST: u8 = 1;
const JM_MODE_LEGACY: u8 = 2;

/// `const`-initialisable and DESTRUCTOR-FREE (`Cell<u32>`, `Cell<u8>` and an array of them
/// own nothing), which is the whole point: `std` lowers a thread-local of this shape to a
/// bare TLS-slot load, where the `RefCell<Vec<u32>>` it replaces forced an out-of-line
/// `LocalKey::with` with an init-state check on both of the two crossings every compiled
/// entry performs.
struct JitMethods {
    /// Resolved [`fast_cbabi`] answer for this thread: 0 = unresolved, 1 = fast, 2 = the
    /// historic `Vec`. Cached here rather than re-read per push, so neither arm pays a gate
    /// and the mode byte rides in the cache line the push already loaded.
    mode: Cell<u8>,
    /// TRUE nesting depth, counted past `JM_DEPTH` so push/pop stay balanced under
    /// arbitrarily deep compiled recursion even though only the first `JM_DEPTH` are named.
    depth: Cell<u32>,
    /// Interned ids (`register_compiled_method`), outermost first. Slot `i` is valid iff
    /// `i < depth` and `depth <= JM_DEPTH`.
    ids: [Cell<u32>; JM_DEPTH],
}

impl JitMethods {
    const fn new() -> Self {
        Self {
            mode: Cell::new(JM_MODE_UNSET),
            depth: Cell::new(0),
            ids: [const { Cell::new(0) }; JM_DEPTH],
        }
    }
    /// Index of the innermost RECORDED frame, or `None` when the stack is empty or has
    /// outrun `JM_DEPTH` (see that constant: missing beats wrong).
    #[inline]
    fn top_index(&self) -> Option<usize> {
        let d = self.depth.get() as usize;
        if d == 0 || d > JM_DEPTH {
            None
        } else {
            Some(d - 1)
        }
    }
}

thread_local! {
    /// Stack of the currently-executing compiled methods (innermost last) — set around
    /// call_raw in jit_run so a fault inside compiled code (RAVA_PROBE_STALE) can NAME the
    /// method whose in-flight SSA value went stale, giving RAVA_JIT_DUMP an exact target.
    ///
    /// Holds INTERNED ids, not strings. The printable `owner.name descriptor` is built ONCE
    /// per compiled method at compile publication (`register_compiled_method`) and parked in
    /// `METHOD_NAMES`; entry then costs a `u32` store instead of the `format!` + `String`
    /// allocation + drop this path used to pay on every single compiled entry.
    static CUR_JIT_METHOD: JitMethods = const { JitMethods::new() };
}

thread_local! {
    /// `RAVA_NO_FAST_CBABI=1` arm ONLY: the historic unbounded storage, byte for byte the
    /// body this instrument had before [`JitMethods`] existed. Untouched — and therefore
    /// free, since `std` never registers a destructor for a thread-local no thread ever
    /// initialises — on the default fast arm.
    static CUR_JIT_METHOD_LEGACY: std::cell::RefCell<Vec<u32>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Resolve, once per thread, which storage this thread's compiled-method stack uses.
/// One predicted byte load on every subsequent push and pop.
#[inline]
fn jm_mode(s: &JitMethods) -> u8 {
    let m = s.mode.get();
    if m != JM_MODE_UNSET {
        return m;
    }
    #[cold]
    #[inline(never)]
    fn fill(s: &JitMethods) -> u8 {
        let m = if fast_cbabi() { JM_MODE_FAST } else { JM_MODE_LEGACY };
        s.mode.set(m);
        m
    }
    fill(s)
}

/// Interned printable identities of compiled methods, indexed by `mid - 1` (`mid == 0` means
/// "no compiled frame"). Append-only, written once per compiled method off the hot path.
static METHOD_NAMES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

impl Drop for CompileScope {
    fn drop(&mut self) {
        crate::rava_sampler::CURRENT_METHOD.store(self.0, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Set/clear the current VM around a compiled call. Returns the previous value
/// so nested compiled calls (none today, but future-proof) restore correctly.
/// Also publishes THIS thread's cell-root ctx into the Vm's jit_ctx_slot — the
/// baked pointer a compiled prologue loads on the normal GIL-held path.
///
/// LEFT ALONE DELIBERATELY: `frames::ctx_addr()` is already memoised per thread
/// (`RAVA_NO_PINNED_CTX`), and `jit_ctx_slot` is written only by the GIL-holding thread, so
/// the store lands on a line this thread owns. A load-compare-skip in front of it would
/// trade a cheap owned-line store for a load plus a branch.
pub(crate) fn set_current_vm(vm: *mut crate::Vm) -> *mut crate::Vm {
    if !vm.is_null() {
        unsafe { &*vm }
            .jit_ctx_slot
            .store(crate::frames::ctx_addr() as usize, std::sync::atomic::Ordering::Relaxed);
    }
    CURRENT_VM.with(|c| c.replace(vm))
}

/// GILFREE-RINGS: TLS-only CURRENT_VM publication for a GIL-FREE worker.
/// Deliberately does NOT write the shared `Vm::jit_ctx_slot`: that word is the
/// baked cell-root base every UNARMED compiled prologue loads, so a worker
/// stamping its own TLS address there would repoint the concurrently-running
/// main thread's compiled frames. The armed prologue reads the per-thread ctx
/// through `ctx_addr_fn` instead, which is exactly why the lane arms COW.
pub(crate) fn set_current_vm_worker(vm: *mut crate::Vm) -> *mut crate::Vm {
    CURRENT_VM.with(|c| c.replace(vm))
}

/// Intern `owner.name descriptor` for a freshly compiled body and return its id (never 0).
/// Called exactly once per method, at the `jit_cache` publication point.
pub(crate) fn register_compiled_method(class: &str, name: &str, descriptor: &str) -> u32 {
    match METHOD_NAMES.lock() {
        Ok(mut v) => {
            v.push(format!("{class}.{name}{descriptor}"));
            v.len() as u32
        }
        Err(_) => 0,
    }
}

/// Resolve an interned method id back to its printable identity (dump-time only).
pub(crate) fn compiled_method_name(mid: u32) -> Option<String> {
    if mid == 0 {
        return None;
    }
    METHOD_NAMES.lock().ok()?.get(mid as usize - 1).cloned()
}

/// The synthetic id standing for "Cranelift is compiling a body right now".
pub(crate) fn compile_mid() -> u32 {
    static ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *ID.get_or_init(|| register_compiled_method("<vm>", "cranelift-compile", ""))
}

/// RAII: park the sampler's current-method slot on the synthetic compile id, restoring the
/// previous method on drop. Used around the codegen call so compile time is never billed to
/// the caller frame that happened to trigger the compile.
pub(crate) struct CompileScope(u32);
pub(crate) fn compile_scope() -> CompileScope {
    let prev = crate::rava_sampler::CURRENT_METHOD
        .swap(compile_mid(), std::sync::atomic::Ordering::Relaxed);
    CompileScope(prev)
}

/// One of the two bookkeeping crossings `Vm::jit_enter` performs per compiled entry.
/// `rava_sampler::CURRENT_METHOD` is a no-op type unless the `wall-profile` feature is on,
/// so in a product build this is the TLS load, one predicted branch and two stores below —
/// and nothing else.
#[inline]
pub(crate) fn jit_method_push(mid: u32) {
    CUR_JIT_METHOD.with(|s| {
        if jm_mode(s) == JM_MODE_FAST {
            let d = s.depth.get();
            // `get` is the in-range test AND the cap in one: past `JM_DEPTH` the frame is
            // simply not named, and `depth` still advances so the matching pop balances.
            if let Some(slot) = s.ids.get(d as usize) {
                slot.set(mid);
            }
            s.depth.set(d.wrapping_add(1));
        } else {
            CUR_JIT_METHOD_LEGACY.with(|c| c.borrow_mut().push(mid));
        }
    });
    crate::rava_sampler::CURRENT_METHOD.store(mid, std::sync::atomic::Ordering::Relaxed);
}

/// The matching exit crossing. Publishes the method that becomes innermost again, exactly
/// as the `Vec` form did (`pop()` then `last()`).
#[inline]
pub(crate) fn jit_method_pop() {
    let top = CUR_JIT_METHOD.with(|s| {
        if jm_mode(s) == JM_MODE_FAST {
            let d = s.depth.get();
            if d == 0 {
                // Unbalanced pop (an unwind past an entry that never pushed): the `Vec`
                // form's `pop()` was a no-op here too. Stay at zero.
                return 0;
            }
            s.depth.set(d - 1);
            match s.top_index() {
                Some(i) => s.ids[i].get(),
                None => 0,
            }
        } else {
            CUR_JIT_METHOD_LEGACY.with(|c| {
                let mut b = c.borrow_mut();
                b.pop();
                b.last().copied().unwrap_or(0)
            })
        }
    });
    crate::rava_sampler::CURRENT_METHOD.store(top, std::sync::atomic::Ordering::Relaxed);
}

/// The innermost compiled method's printable identity, or `None` when no compiled frame is
/// active (or the stack has outrun [`JM_DEPTH`] — see that constant).
pub(crate) fn jit_method_cur() -> Option<String> {
    let mid = CUR_JIT_METHOD.with(|s| {
        if jm_mode(s) == JM_MODE_FAST {
            s.top_index().map(|i| s.ids[i].get())
        } else {
            CUR_JIT_METHOD_LEGACY.with(|c| c.borrow().last().copied())
        }
    })?;
    compiled_method_name(mid)
}

/// The top-N innermost compiled-method names (deepest last) — the JIT call chain, so a
/// stranded-value collection can name not just the toString that triggered it but the
/// caller frame that holds the in-flight value.
pub(crate) fn jit_method_stack(n: usize) -> Vec<String> {
    let ids: Vec<u32> = CUR_JIT_METHOD.with(|s| {
        if jm_mode(s) == JM_MODE_FAST {
            let d = s.depth.get() as usize;
            if d > JM_DEPTH {
                // Outran the recorded window: report nothing rather than a set of frames
                // that are not the innermost ones the caller asked for.
                return Vec::new();
            }
            (0..d).rev().take(n).map(|i| s.ids[i].get()).collect()
        } else {
            CUR_JIT_METHOD_LEGACY.with(|c| c.borrow().iter().rev().take(n).copied().collect())
        }
    });
    ids.into_iter().filter_map(compiled_method_name).collect()
}

/// GAP-S09 stage 4: `vm_ref()` is MODE-AWARE. On the GIL-held/serial path it is
/// unchanged (one TLS read). On a GIL-FREE worker a callback reaching this is an
/// UNTYPED `&mut Vm` escape — exclusive access it does not have — so it is only
/// admitted inside the S2 locked-exec envelope (`RAVA_M3_WORKER_LOCKED_EXEC` and
/// this thread OWNS the re-entrant resolve lock, i.e. a `WorkerGate::Locked` /
/// locked dispatch-miss frame is active: main parked + siblings serialized ⇒
/// exclusive `&mut Vm`). Everywhere else the region abort is requested and None
/// is returned (armored-abort discipline: the neutral 0 the callback then
/// returns is discarded by the serial retry), with a debug assert naming the
/// situation — a callback that legitimately fires gil-free must migrate to
/// `vm_shared()` (read-only, signature-proven), run under the locked gate, or
/// use `vm_ref_worker_sanctioned()` with a documented soundness note. Plain
/// `vm_ref()` can no longer silently hand out `&mut Vm` on workers.
///
/// THE GUARD IS UNCHANGED; ONLY ITS PLACEMENT IS. The census, the abort request and the
/// debug assert are a whole `#[cold]` function now ([`vm_ref_gilfree`]), so the serial path
/// every bridge shares is the `gilfree()` test falling straight through to [`vm_ptr`], with
/// none of that code between the crossing and the work.
#[inline]
pub(crate) fn vm_ref() -> Option<&'static mut crate::Vm> {
    #[cfg(feature = "next-gen-gc")]
    if crate::parallel::gilfree() {
        return vm_ref_gilfree();
    }
    vm_ref_unchecked()
}

/// The gil-free half of [`vm_ref`], out of line. Admits the caller only inside the S2
/// locked-exec envelope; otherwise tears the region down and denies, exactly as before.
#[cfg(feature = "next-gen-gc")]
#[cold]
#[inline(never)]
fn vm_ref_gilfree() -> Option<&'static mut crate::Vm> {
    s09census::note(s09census::IDX_VM_REF_WORKER);
    if !(crate::worker_locked_exec_enabled() && resolve_lock_held_by_me()) {
        s09census::note_abort(s09census::IDX_VM_REF_WORKER);
        crate::parallel::request_abort();
        debug_assert!(
            worker_denial_expected(),
            "plain vm_ref() on a gil-free worker without the locked-exec envelope — an \
             untyped &mut Vm escape; migrate this callback to vm_shared(), the locked \
             worker gate, or vm_ref_worker_sanctioned() (GAP-S09 stage 4)"
        );
        return None;
    }
    vm_ref_unchecked()
}

/// GAP-S09 stage 4: SANCTIONED `&mut Vm` accessor for the enumerated callbacks
/// that legitimately fire on gil-free workers and whose worker arms are sound by
/// construction (each call site carries its own note): abort-guarded BEFORE any
/// shared publication, executed under the locked-exec envelope, or touching only
/// TLS / worker-private / frozen state despite the `&mut` receiver its helpers
/// inherit. Grep for `vm_ref_worker_sanctioned` to enumerate the whole surface.
/// Serial behaviour is identical to the old `vm_ref()` (one TLS read).
#[inline]
pub(crate) fn vm_ref_worker_sanctioned() -> Option<&'static mut crate::Vm> {
    vm_ref_unchecked()
}

/// The raw CURRENT_VM reconstruction every accessor shares (the old `vm_ref` body).
///
/// `#[inline]` because this IS the prologue of most of the 78 bridges and it is consumed
/// from twelve other modules: without the attribute (thin LTO may import it across codegen
/// units, and may not) each of ~30M crossings pays an out-of-line call, plus the
/// caller-saved spills around it, for a TLS load and a null test.
#[inline]
pub(crate) fn vm_ref_unchecked() -> Option<&'static mut crate::Vm> {
    let p = vm_ptr();
    if p.is_null() {
        None
    } else {
        Some(unsafe { &mut *p })
    }
}

/// Test hook: the stage-4 negative test EXPECTS the gil-free denial, so the
/// debug assert must not fire there. Compiled out of production builds — in a
/// non-test debug build every denial asserts (the loud development signal).
#[cfg(all(test, feature = "next-gen-gc"))]
thread_local! {
    static EXPECT_WORKER_DENIAL: Cell<bool> = const { Cell::new(false) };
}
#[cfg(all(test, feature = "next-gen-gc"))]
pub(crate) fn expect_worker_denial_for_test(on: bool) {
    EXPECT_WORKER_DENIAL.with(|c| c.set(on));
}
#[cfg(feature = "next-gen-gc")]
#[allow(dead_code)] // referenced only from debug_assert!
pub(crate) fn worker_denial_expected() -> bool {
    #[cfg(test)]
    {
        EXPECT_WORKER_DENIAL.with(|c| c.get())
    }
    #[cfg(not(test))]
    {
        false
    }
}

/// GAP-S09 stage 2: SHARED-reference twin of `vm_ref()` over the same `CURRENT_VM`
/// cell. Hands out `&Vm` — never `&mut` — so a callback that migrates to it is
/// PROVEN by its signature to only READ shared `Vm` state (its whole body must
/// type-check against `&Vm`). On a gil-free worker such a callback is race-free by
/// construction (frozen reads + TLS), whereas every `vm_ref()` user still asserts
/// exclusive access it does not have off the GIL. Same lifetime basis as `vm_ref`:
/// the pointee is the coordinator's Vm, parked for the compiled call / fork region.
#[inline]
pub(crate) fn vm_shared() -> Option<&'static crate::Vm> {
    let p = vm_ptr();
    if p.is_null() {
        None
    } else {
        Some(unsafe { &*p })
    }
}

pub(crate) fn set_pending(vm: &mut crate::Vm, class: &str, msg: &str) {
    if !vm.jit_pending_is_set() {
        let exc = vm.make_exception(class, msg);
        vm.jit_pending_set(exc);
    }
    // Latch the native-store short-circuit: once any fault is pending in this
    // compiled frame, later native stores must no-op (the method is unwinding).
    vm.heap.set_jit_fault();
}
