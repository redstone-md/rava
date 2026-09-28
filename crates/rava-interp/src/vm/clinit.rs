//! JVMS §5.5 initialisation state, priced on the ACCESS path instead of the init path.
//!
//! A boot loads ~15,000 classes and each one carries an initialisation state that is consulted on
//! every `getstatic`, every `putstatic`, every `new` and every static call. Running a `<clinit>`
//! is one-shot work; ASKING whether it already ran is not, and that question was being answered
//! from scratch every time:
//!
//! * `Vm::ensure_init` is an out-of-line call that, for a class which finished initialising
//!   twenty seconds ago, still performs a relaxed load of `initialized`, a **SeqCst** load of the
//!   process-wide `REAL_THREADS` counter (`gil::has_real_threads`), a load of `clinit_done` and a
//!   load of `init_failed`. The SeqCst one is the expensive member and not because of the `mov`:
//!   it is a barrier LLVM may not hoist out of a loop or fold across, on a body that is inlined
//!   nowhere.
//! * the static-access opcodes re-derived the class the question is even ABOUT. `ref_parts` hands
//!   back three `Arc<str>` — six atomic refcount round-trips per access — and then
//!   `ClassLoader::get_or_load` hashes the owner NAME and clones an `Arc<LoadedClass>` out of the
//!   shared class cache. `loader.rs` already records what the last of those costs at scale:
//!   "cloning an `Arc` out of a SHARED set is an atomic refcount on one cache line, 4.5 million
//!   times".
//! * `putstatic` then walked `class.file.fields` LINEARLY to re-decide which class declares the
//!   field, and on a miss walked the super chain — per write, for an answer fixed by an immutable
//!   classfile.
//!
//! None of that can change once the owner's `<clinit>` has completed, so this module decides it
//! ONCE PER CALL SITE and replays the decision. `RAVA_NO_FAST_CLINIT=1` (and `RAVA_NO_EMIT_MORE`)
//! restores every one of the old paths exactly.

use crate::*;

/// **DEFAULT-ON**, `RAVA_NO_FAST_CLINIT=1` disables, and so does `RAVA_NO_EMIT_MORE` so the whole
/// round bisects in one run. Off, `ensure_init_fast` is a plain `ensure_init` call and the site
/// table is never read and never written, so every static access takes the resolution it took
/// before, byte for byte.
pub(crate) fn fast_clinit() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_CLINIT").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// ⭐**THE WHOLE §5.5 ANSWER FOR A FINISHED CLASS, IN ONE LOAD.**
///
/// `clinit_done` is stored exactly once, with `Release`, immediately after `<clinit>` returned
/// NORMALLY (`Vm::ensure_init`; the trampolined twin in `drive_stack` stores it on the clinit
/// frame's `Normal` exit, and `restore_statics` on a checkpoint restore). The erroneous path
/// stores `init_failed` and then propagates the throw *before* reaching that store, so
///
/// > `clinit_done == true`  ⟹  `initialized == true` **and** `init_failed == false`
///
/// and, because the flag is only ever set and never cleared, that stays true forever. It also
/// subsumes §5.5 step 2: this flag is precisely what the "another thread is still initialising"
/// wait waits FOR, so observing it true means there is nothing left to wait for. The three other
/// loads and the SeqCst `has_real_threads()` probe `ensure_init` performs are all redundant here.
///
/// `Acquire`, not `Relaxed`: a static read that follows must not be hoisted above the flag, or a
/// thread could pair "the clinit finished" with the statics as they stood before it ran. On
/// x86-64 an acquire load is the same single `mov` a relaxed one is — this costs nothing and buys
/// the happens-before that makes the skipped wait legitimate.
#[inline(always)]
pub(crate) fn settled(class: &crate::class_loader::LoadedClass) -> bool {
    class.clinit_done.load(std::sync::atomic::Ordering::Acquire)
}

// ---------------------------------------------------------------------------------------------
// The per-call-site static binding.
// ---------------------------------------------------------------------------------------------

/// What a slot has decided about one static-access site.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Bound {
    /// Nothing decided; the slot is free (or holds another site's decision).
    Empty,
    /// `decl` DECLARES the field and its `<clinit>` has completed: the field's storage is
    /// `decl.statics[name]`, or the descriptor's default when that key is absent — forever.
    Field,
    /// `decl` is the owner class but had not finished initialising when the site first ran —
    /// overwhelmingly the case for a site executed from inside its own class's `<clinit>`.
    /// Re-checked with a single relaxed load on the next access rather than written off, because
    /// writing it off would forfeit exactly the sites a class's own code hits hardest.
    Pending,
    /// The site is not of the provable shape (owner unloadable, or the field is INHERITED / an
    /// interface constant, which resolve by a walk this fast path deliberately does not
    /// reproduce). Never retried; the slow path serves it exactly as it always did.
    Refused,
}

/// One site's decision.
///
/// ⚠**TABLE SIZE IS A CORRECTNESS-OF-PERFORMANCE CONSTRAINT HERE.** 24 bytes a slot
/// (`Option<Arc>` is pointer-sized by niche, `cp`+`state` share the tail padding) × 128 slots =
/// **3,072 bytes**, deliberately in the same bracket the field interner's L1 was cut down to. The
/// obvious 256- or 512-slot version was not built: this tree has already measured a cache growing
/// 2.5 KB → 14 KB as **+6.8% WORSE than the allocation it removed**, and a direct-mapped table
/// that stops living in L1 costs more than the `get_or_load` it exists to remove. A colliding
/// site simply loses its binding and takes the old path.
struct Slot {
    /// Address of the CALLER class's `Arc<LoadedClass>` — 0 when the slot has never been used.
    /// Classes are never unloaded (the loader cache owns every one for the life of the process),
    /// the same stability argument `tlcache`'s site-keyed caches record, so this address can never
    /// be recycled into a different class and produce a false hit.
    cls: usize,
    /// Constant-pool index of the field ref at this site.
    cp: u16,
    state: Bound,
    decl: Option<Arc<crate::class_loader::LoadedClass>>,
}

/// 128 slots — see `Slot` for why this is not larger.
const SLOTS: usize = 128;

thread_local! {
    /// Per-thread, like every other site cache in this engine: the decision is a pure function of
    /// MONOTONIC loader + initialisation state, so a thread that has not yet observed a class as
    /// settled merely re-checks, and two threads deciding the same site decide it identically.
    ///
    /// `UnsafeCell`, for the reason `tlcache::Caches::call` records: the accessors below do
    /// nothing but probe and rewrite one slot, no guest code runs while the reference is live, and
    /// a borrow flag would be a read, a write and a branch on the static-access path.
    static BINDS: std::cell::UnsafeCell<[Slot; SLOTS]> = std::cell::UnsafeCell::new(
        std::array::from_fn(|_| Slot { cls: 0, cp: 0, state: Bound::Empty, decl: None }),
    );
}

/// Direct-mapped index. An `Arc<LoadedClass>` allocation is at least 8-byte aligned, so the low
/// bits carry no entropy — shift them off before mixing in the pool index.
#[inline(always)]
fn slot_of(cls: usize, cp: u16) -> usize {
    ((cls >> 4) ^ ((cp as usize) << 3) ^ (cp as usize)) & (SLOTS - 1)
}

/// True if `class` ITSELF declares a static field called `name` (JLS 8.3: a subclass and its
/// superclass may both declare a same-named static, and those are DISTINCT fields). Linear, but
/// run once per site instead of once per access — which is the whole point of the binding.
#[inline]
fn declares_static(class: &crate::class_loader::LoadedClass, name: &str) -> bool {
    class
        .file
        .fields
        .iter()
        .any(|f| f.name.as_str() == name && f.access_flags & 0x0008 != 0)
}

impl Vm {
    /// `ensure_init`, with the finished case decided inline in one load.
    ///
    /// Every other case — never started, in progress on THIS thread (the cycle break), in progress
    /// on another thread (the §5.5 wait), or erroneous (the `NoClassDefFoundError` latch) — falls
    /// through to the real `ensure_init` untouched. What is removed is the out-of-line call and
    /// the three extra loads on the case that is true for essentially every access after the first
    /// seconds of a boot.
    #[inline(always)]
    pub(crate) fn ensure_init_fast(
        &mut self,
        class: &Arc<crate::class_loader::LoadedClass>,
    ) -> Result<(), VmError> {
        if settled(class) && fast_clinit() {
            return Ok(());
        }
        self.ensure_init(class)
    }

    /// `ensure_init_fast` reachable from a `&SharedVm` static-access path.
    ///
    /// The FINISHED case — true for essentially every access after the first seconds of a boot —
    /// is one acquire load and needs no mutation, so it returns on `&self`. The COLD case runs
    /// `<clinit>` guest bytecode through `invoke`, which is genuinely `&mut self`; it is reached
    /// through the same transitional cast `getstatic_resolve` uses (the sibling static-read leaf),
    /// and for the same reason — the interpreter loop is not yet `&self`, so the initialisation
    /// trampoline remains the one architectural `&mut` on this path. Bit-identical to calling
    /// `ensure_init_fast` directly; only the receiver's provenance differs.
    #[inline(always)]
    #[allow(invalid_reference_casting)]
    pub(crate) fn ensure_init_shared(
        &self,
        class: &Arc<crate::class_loader::LoadedClass>,
    ) -> Result<(), VmError> {
        if settled(class) && fast_clinit() {
            return Ok(());
        }
        unsafe { &mut *(self as *const Self as *mut Self) }.ensure_init(class)
    }

    /// Serve a static access from a BOUND site: `f` is handed the class that owns the field's
    /// storage, with no loader probe, no `<clinit>` check and no declaring-class walk.
    ///
    /// Returns `None` when the site is not (yet) bound, and the caller takes the path it always
    /// took. A `Pending` slot is promoted here, for one relaxed load.
    ///
    /// REFUSAL IS INVISIBLE: on every `None` path nothing is stored, allocated or mutated except
    /// this slot's own decision word.
    #[inline]
    pub(crate) fn static_site<R>(
        caller: usize,
        cp: u16,
        name: &str,
        f: impl FnOnce(&Arc<crate::class_loader::LoadedClass>) -> R,
    ) -> Option<R> {
        if !fast_clinit() {
            return None;
        }
        BINDS.with(|b| {
            // SAFETY: `&mut` to one slot of a thread-local array. Nothing reachable from here
            // re-enters this table — `f` reads or writes a `statics` map and runs no guest code —
            // so no second reference to the array can exist while this one is live.
            let slot = unsafe { &mut (*b.get())[slot_of(caller, cp)] };
            if slot.cls != caller || slot.cp != cp {
                return None;
            }
            match slot.state {
                Bound::Field => {}
                Bound::Pending => {
                    // Has the owner finished since this site last ran? Only then is the decision
                    // knowable; until then the slot stays Pending and costs exactly this load.
                    let Some(c) = slot.decl.as_ref() else { return None };
                    if !settled(c) {
                        return None;
                    }
                    if declares_static(c, name) {
                        slot.state = Bound::Field;
                    } else {
                        // Settled, but the field is not its own: an inherited static or an
                        // interface constant, resolved by a walk. Permanently the slow path's.
                        slot.state = Bound::Refused;
                        slot.decl = None;
                        return None;
                    }
                }
                Bound::Empty | Bound::Refused => return None,
            }
            let decl: *const Arc<crate::class_loader::LoadedClass> = slot.decl.as_ref()?;
            // SAFETY: the slot is not touched again until `f` returns, and `f` cannot reach this
            // table; the `Arc` it points at is owned by the slot and outlives the call.
            Some(f(unsafe { &*decl }))
        })
    }

    /// True when this site has not already been decided, i.e. an attempt to bind it is worth the
    /// loader probe it costs. False for a site of ours that is `Pending` (already re-checked by
    /// `static_site`) or `Refused` — so a site the fast path can never serve never pays for the
    /// attempt more than once.
    #[inline]
    pub(crate) fn static_site_open(caller: usize, cp: u16) -> bool {
        if !fast_clinit() {
            return false;
        }
        BINDS.with(|b| {
            // SAFETY: as `static_site` — a read of one slot of a thread-local array.
            let slot = unsafe { &(*b.get())[slot_of(caller, cp)] };
            slot.cls != caller || slot.cp != cp
        })
    }

    /// Decide a site against a freshly resolved owner class, and say whether it is now BOUND.
    ///
    /// ⭐**THE PRECONDITION, STATED ONCE so the two opcodes cannot state it differently:** the
    /// owner class has finished `<clinit>` AND declares this static itself. Under those two facts
    /// `getstatic_resolve`'s answer is its FIRST branch (the owner's own `statics` map) or its
    /// SECOND (the owner declares it but has not assigned it yet → the descriptor's default), and
    /// neither the super-chain walk nor the interface-constant closure beneath them is reachable;
    /// `putstatic`'s answer is likewise its `owner_declares` branch. Anything else and this
    /// returns false and the caller runs the untouched walk — the fast path never reproduces a
    /// resolution rule it cannot prove it is allowed to skip.
    ///
    /// The three outcomes are also what keeps the attempt from repeating: settled-and-declares
    /// binds, settled-and-not is `Refused` for good, and NOT-YET-SETTLED is `Pending` — the case
    /// of a site first executed from inside its own class's `<clinit>`, which is exactly the hot
    /// one and must not be written off.
    pub(crate) fn static_site_decide(
        caller: usize,
        cp: u16,
        class: &Arc<crate::class_loader::LoadedClass>,
        name: &str,
    ) -> bool {
        if !fast_clinit() {
            return false;
        }
        // Ordered so the field scan runs only when its answer can be acted on: a class that has
        // not finished initialising is `Pending` whatever it declares.
        let (state, bound) = if !settled(class) {
            (Bound::Pending, false)
        } else if declares_static(class, name) {
            (Bound::Field, true)
        } else {
            (Bound::Refused, false)
        };
        BINDS.with(|b| {
            // SAFETY: as `static_site`.
            let slot = unsafe { &mut (*b.get())[slot_of(caller, cp)] };
            slot.cls = caller;
            slot.cp = cp;
            slot.state = state;
            slot.decl = if state == Bound::Refused { None } else { Some(Arc::clone(class)) };
        });
        bound
    }

    /// Record that this site is not of the provable shape. Costs one slot; buys never re-running
    /// the attempt, and its loader probe, on a site the fast path can never serve.
    pub(crate) fn static_site_refuse(caller: usize, cp: u16) {
        if !fast_clinit() {
            return;
        }
        BINDS.with(|b| {
            // SAFETY: as `static_site`.
            let slot = unsafe { &mut (*b.get())[slot_of(caller, cp)] };
            slot.cls = caller;
            slot.cp = cp;
            slot.state = Bound::Refused;
            slot.decl = None;
        });
    }

}
