//! Per-OS-thread guest execution state.
//!
//! This module is the ownership seam between the shared VM/heap and code that may
//! execute outside the global GIL. Pending exceptions belong to one compiled call
//! chain on one OS thread; sharing the slot through `Vm` let a worker poison the
//! coordinator or another worker. Slots register their stable addresses so a
//! stop-the-world collector can still trace and forward pending exception values.
//!
//! ── THE SHAPE OF THE REGION GUARD ────────────────────────────────────────────────
//!
//! SHAPE BEFORE. One `thread_local!` — `STATE` — holding an `ExecutionState`. It is
//! `const`-initialised, but its payload owns an `Option<Value>` and it carries a `Drop`
//! impl, so it needs drop: `std` therefore emits the destructor-registering storage, and
//! EVERY access is an out-of-line `LocalKey::with` call carrying a lazy-init /
//! destructor-registration state check. `pending_is_set` is the first statement of 41 of
//! the 76 compiled→host bridges (see `jit/callbacks/mod.rs`, the prologue census), on a
//! path taken ~30M times a product boot — so the boolean "is an exception pending" was
//! being answered through the most expensive TLS shape `std` has.
//!
//! SHAPE AFTER. The BIT moves; the VALUE does not. `PENDING_BITS` is a second
//! `thread_local!` holding a `Cell<u8>` — `const`-initialised AND destructor-free, the one
//! shape that lowers to a bare TLS-slot load with no call, no branch and no registration.
//! `pending_is_set` becomes: load the byte, test one bit, test another. The `Option<Value>`
//! stays exactly where it is, at exactly the address `PENDING_ROOTS` published, so the STW
//! collector's contract is untouched — a `Cell<u8>` beside the slot moves no root.
//!
//! WHY THE MIRROR CANNOT LIE. Both cells are thread-local to the SAME thread; nothing else
//! ever writes either one (`visit_pending_roots` mutates a `Value` in place under STW and
//! can never change the slot's `is_some()`). Within the owning thread the two are updated
//! in one straight-line sequence, and the order is chosen so **the flag is never AHEAD of
//! the value**: it is raised only AFTER the value has been stored, and lowered only BEFORE
//! the value is removed. An observer that catches an intermediate state therefore always
//! reads the OLDER of the two consistent states — "nothing pending" — never a raised flag
//! pointing at an absent value. Reading a stale "nothing pending" is the safe direction:
//! it is what the caller would have seen one instruction earlier, and the exception is
//! still latched for the next guard and for `pending_take`, which reads the truth.
//!
//! The gate is resolved once per thread into the same byte (`MODE_FAST` / `MODE_SLOW`), so
//! the fast path never re-reads an `OnceLock`; with the kill switch on, every read falls
//! into the cold path and re-derives the answer from the `Option` itself, i.e. exactly the
//! code that ran before this change.

use std::cell::{Cell, UnsafeCell};
use std::sync::Mutex;

use crate::Value;

static PENDING_ROOTS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// `RAVA_NO_FAST_PENDING=1` (or the block switch `RAVA_NO_EMIT_MORE=1`) sends every
/// `pending_is_set` back through `LocalKey::with` on the value-owning slot. Default ON.
///
/// Read at most once per thread on the cold path; the outcome is cached in `PENDING_BITS`
/// itself, so neither arm re-reads it per access.
fn fast_pending() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_PENDING").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// Bit 0: mirrors `pending.is_some()` for this thread. Maintained unconditionally by every
/// writer below, so it is correct even in the `MODE_SLOW` arm that never reads it.
const PEND: u8 = 0b001;
/// Bit 1: this thread resolved [`fast_pending`] and it is ON — bit 0 may be trusted.
const MODE_FAST: u8 = 0b010;
/// Bit 2: this thread resolved [`fast_pending`] and it is OFF — always take the cold path.
const MODE_SLOW: u8 = 0b100;

thread_local! {
    /// `const`-initialised and destructor-free — a BARE TLS-slot load. Deliberately NOT a
    /// field of `ExecutionState`: living inside a type that needs drop is precisely what
    /// made the read expensive.
    static PENDING_BITS: Cell<u8> = const { Cell::new(0) };
}

/// Bring the mirror bit in step with the slot. Callers must invoke this on the safe side of
/// the write (see the module header: after a store, before a take).
#[inline]
fn note_pending(present: bool) {
    let bits = PENDING_BITS.get();
    PENDING_BITS.set(if present { bits | PEND } else { bits & !PEND });
}

struct ExecutionState {
    pending: UnsafeCell<Option<Value>>,
    // Throw values use pending, whose stable address is registered with the GC.
    failure: std::cell::RefCell<Option<crate::VmError>>,
    registered: Cell<bool>,
}

impl ExecutionState {
    const fn new() -> Self {
        Self {
            pending: UnsafeCell::new(None),
            failure: std::cell::RefCell::new(None),
            registered: Cell::new(false),
        }
    }

    fn register(&self) {
        self.failure.borrow_mut().take();
        if self.registered.replace(true) {
            // Flag down BEFORE the value leaves: the intermediate state reads as
            // "nothing pending", which is what is true the instant after.
            note_pending(false);
            self.with_pending(|pending| *pending = None);
            return;
        }
        // A reused OS thread starts a new guest execution scope pending-free.
        note_pending(false);
        unsafe { *self.pending.get() = None };
        let address = self.pending.get() as usize;
        let mut roots = PENDING_ROOTS.lock().unwrap_or_else(|e| e.into_inner());
        if !roots.contains(&address) {
            roots.push(address);
        }
    }

    fn unregister(&self) {
        if !self.registered.replace(false) {
            return;
        }
        self.failure.borrow_mut().take();
        let address = self.pending.get() as usize;
        PENDING_ROOTS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|&root| root != address);
        // No collector can retain the address after the registry lock was released.
        // Flag down first, then the value — the mirror is never ahead of the slot.
        note_pending(false);
        unsafe { *self.pending.get() = None };
    }

    fn with_pending<R>(&self, f: impl FnOnce(&mut Option<Value>) -> R) -> R {
        // SAFETY: ExecutionState is thread-local. The owning thread is the only
        // normal mutator; STW root visiting is allowed only while owners are parked.
        f(unsafe { &mut *self.pending.get() })
    }
}

impl Drop for ExecutionState {
    fn drop(&mut self) {
        // `unregister` touches `PENDING_BITS`, which is `const`-initialised and
        // destructor-free: `std` hands back its address unconditionally, with no
        // lazy-init state to be "already destroyed". Safe to reach from TLS teardown.
        self.unregister();
    }
}

thread_local! {
    static STATE: ExecutionState = const { ExecutionState::new() };
}

/// Attach this TLS state to the registered execution-thread/STW lifecycle.
pub(crate) fn register() {
    STATE.with(ExecutionState::register);
}

/// Detach this TLS state after the thread has left every guest execution region.
pub(crate) fn unregister() {
    STATE.with(ExecutionState::unregister);
}

/// The region guard 41 compiled→host bridges open with, ~30M times a product boot.
///
/// FAST ARM: one bare TLS-slot load of a destructor-free `Cell<u8>`, one bit test to
/// confirm this thread resolved the gate to ON, one bit test for the answer. No call, no
/// lazy-init check, no borrow flag.
///
/// COLD ARM (first touch on a thread, and EVERY touch when the kill switch is on): resolve
/// the gate and read the truth out of the `Option<Value>` through `STATE.with` — the
/// pre-change code path verbatim — then re-seed the mirror from it, so even a writer that
/// somehow failed to maintain the bit cannot make the first read wrong.
#[inline]
pub(crate) fn pending_is_set() -> bool {
    let bits = PENDING_BITS.get();
    if bits & MODE_FAST != 0 {
        return bits & PEND != 0;
    }
    pending_is_set_cold()
}

#[cold]
#[inline(never)]
fn pending_is_set_cold() -> bool {
    let present = STATE.with(|state| state.failure.borrow().is_some()
        || state.with_pending(|pending| pending.is_some()));
    let mut bits = if fast_pending() { MODE_FAST } else { MODE_SLOW };
    if present {
        bits |= PEND;
    }
    // PEND / MODE_FAST / MODE_SLOW are the only bits, so a full rewrite is a full sync.
    PENDING_BITS.set(bits);
    present
}

/// Store the first exception for this thread's compiled call chain.
#[inline]
pub(crate) fn pending_set_first(value: Value) -> bool {
    let stored = STATE.with(|state| {
        if state.failure.borrow().is_some() { return false; }
        state.with_pending(|pending| {
            if pending.is_some() {
                false
            } else {
                // VALUE FIRST, FLAG SECOND. Between the two statements the mirror still
                // reads "nothing pending" — the state that was true one instruction ago,
                // and the conservative one: a guard catching it behaves as it would have
                // before the throw, and the very next guard sees the flag. The reverse
                // order would publish a raised flag over an empty slot, which no observer
                // may ever see.
                *pending = Some(value);
                note_pending(true);
                true
            }
        })
    });
    // A compiled callback cannot unwind through machine code. In a detached
    // worker, make the whole transactional region fail closed to serial retry.
    if stored && crate::parallel::gilfree() {
        #[cfg(feature = "next-gen-gc")]
        STATE.with(|state| {
            state.with_pending(|pending| {
                if let Some(v) = pending.as_ref() {
                    crate::gilfree_cf::note_pending_set(v);
                }
            })
        });
        crate::parallel::request_abort();
    }
    stored
}

#[inline]
pub(crate) fn pending_take() -> Option<Value> {
    let taken = STATE.with(|state| {
        if state.failure.borrow().is_some() { return None; }
        state.with_pending(|pending| {
            // FLAG FIRST, VALUE SECOND — the mirror of the store above. The intermediate
            // state is "flag down, value still there", i.e. the state that is true one
            // instruction later; the reverse order would leave a raised flag over a slot
            // the caller has already emptied.
            note_pending(false);
            pending.take()
        })
    });
    #[cfg(feature = "next-gen-gc")]
    if taken.is_some() {
        crate::gilfree_cf::note_pending_taken();
    }
    taken
}

#[inline]
pub(crate) fn pending_clear() {
    // Flag down before the value goes: never a raised flag over an empty slot.
    note_pending(false);
    STATE.with(|state| {
        state.failure.borrow_mut().take();
        state.with_pending(|pending| *pending = None);
    });
}

/// A handler may consume only a guest Throwable, never an execution failure.
pub(crate) fn pending_throw_is_set() -> bool {
    STATE.with(|state| state.with_pending(|pending| pending.is_some()))
}

/// Preserve the first failure without allocating a replacement Java exception.
pub(crate) fn pending_error_set_first(error: crate::VmError) -> bool {
    if let crate::VmError::Throw(value) = error { return pending_set_first(value); }
    if pending_is_set() { return false; }
    STATE.with(|state| *state.failure.borrow_mut() = Some(error));
    note_pending(true);
    if crate::parallel::gilfree() { crate::parallel::request_abort(); }
    true
}

/// Used at the outer native boundary. The common successful return touches no
/// destructor-bearing TLS payload; both exceptional representations remain cold.
pub(crate) fn pending_error_take() -> Option<crate::VmError> {
    if !pending_is_set() { return None; }
    let failure = STATE.with(|state| {
        let mut slot = state.failure.borrow_mut();
        if slot.is_some() { note_pending(false); }
        slot.take()
    });
    if failure.is_some() {
        return failure;
    }
    pending_take().map(crate::VmError::Throw)
}

/// Visit every registered thread's pending exception as a mutable GC root.
///
/// The caller must hold the GIL or a global STW handshake that parks every other
/// registered execution thread. This is the same contract as `frames::all`.
///
/// This is the ONLY cross-thread writer of a pending slot, and it forwards a `Value`
/// through `as_mut()` — it can never turn `Some` into `None` or back. The owning thread's
/// `PENDING_BITS` mirror therefore stays correct without the collector touching it, which
/// is why the flag could be split off a slot the collector still reads by address.
pub(crate) fn visit_pending_roots(mut visit: impl FnMut(&mut Value)) {
    let roots = PENDING_ROOTS.lock().unwrap_or_else(|e| e.into_inner());
    for &address in roots.iter() {
        // SAFETY: registered addresses remain stable until TLS destruction. The
        // registry lock excludes destruction, and the STW contract excludes owner
        // mutation while the collector visits the slot.
        if let Some(value) = unsafe { &mut *(address as *mut Option<Value>) }.as_mut() {
            visit(value);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_failures_preserve_first_error_and_pending_mirror() {
        use crate::VmError;
        super::register();
        assert!(super::pending_error_set_first(VmError::BadConstant(73)));
        assert!(!super::pending_set_first(crate::Value::Int(9)));
        assert!(!super::pending_error_set_first(VmError::StackUnderflow));
        assert!(super::pending_is_set());
        assert!(!super::pending_throw_is_set());
        assert!(super::pending_take().is_none());
        assert!(super::pending_is_set_cold());
        assert!(matches!(super::pending_error_take(), Some(VmError::BadConstant(73))));
        assert!(!super::pending_is_set());
        assert!(super::pending_error_set_first(VmError::Throw(crate::Value::Int(4))));
        assert!(!super::pending_error_set_first(VmError::StackUnderflow));
        assert!(matches!(super::pending_error_take(), Some(VmError::Throw(crate::Value::Int(4)))));
        assert!(super::pending_error_set_first(VmError::StackUnderflow));
        super::unregister();
        assert!(!super::pending_is_set_cold());
    }
    use std::sync::{Arc, Barrier};

    use super::*;

    #[test]
    fn pending_is_first_wins_and_take_clears() {
        register();
        pending_clear();
        assert!(pending_set_first(Value::Int(7)));
        assert!(!pending_set_first(Value::Int(9)));
        assert!(pending_is_set());
        assert!(matches!(pending_take(), Some(Value::Int(7))));
        assert!(!pending_is_set());
        unregister();
    }

    #[test]
    fn pending_slots_are_thread_isolated_and_gc_visible() {
        pending_clear();
        let ready = Arc::new(Barrier::new(3));
        let release = Arc::new(Barrier::new(3));
        let mut joins = Vec::new();
        for value in [11, 29] {
            let ready = Arc::clone(&ready);
            let release = Arc::clone(&release);
            joins.push(std::thread::spawn(move || {
                register();
                pending_clear();
                assert!(pending_set_first(Value::Int(value)));
                ready.wait();
                release.wait();
                assert!(matches!(pending_take(), Some(Value::Int(v)) if v == value));
                unregister();
            }));
        }
        ready.wait();
        assert!(!pending_is_set());
        let mut visible = Vec::new();
        visit_pending_roots(|value| {
            if let Value::Int(value) = value {
                visible.push(*value);
            }
        });
        visible.sort_unstable();
        assert_eq!(visible, [11, 29]);
        release.wait();
        for join in joins {
            join.join().unwrap();
        }
    }

    /// The mirror bit and the slot must give the same answer after every transition, and
    /// the cold re-seed must agree with the fast bit.
    #[test]
    fn pending_flag_mirrors_the_slot_through_every_transition() {
        let truth = || STATE.with(|state| state.with_pending(|p| p.is_some()));
        register();
        pending_clear();
        assert_eq!(pending_is_set(), truth());
        assert!(!pending_is_set());

        assert!(pending_set_first(Value::Int(3)));
        assert_eq!(pending_is_set(), truth());
        assert!(pending_is_set());

        // A second set is refused and must not disturb either half.
        assert!(!pending_set_first(Value::Int(4)));
        assert_eq!(pending_is_set(), truth());

        // Forcing the cold arm re-derives the answer from the slot; it must not flip.
        PENDING_BITS.set(PENDING_BITS.get() & !(MODE_FAST | MODE_SLOW));
        assert!(pending_is_set());
        assert_eq!(pending_is_set(), truth());

        assert!(pending_take().is_some());
        assert_eq!(pending_is_set(), truth());
        assert!(!pending_is_set());

        assert!(pending_set_first(Value::Int(5)));
        pending_clear();
        assert_eq!(pending_is_set(), truth());
        assert!(!pending_is_set());

        // register/unregister are writers too.
        assert!(pending_set_first(Value::Int(6)));
        register();
        assert_eq!(pending_is_set(), truth());
        assert!(!pending_is_set());
        unregister();
        assert_eq!(pending_is_set(), truth());
    }

    #[test]
    fn pending_root_is_mutated_in_place() {
        register();
        assert!(pending_set_first(Value::Int(41)));
        visit_pending_roots(|value| {
            if matches!(value, Value::Int(41)) {
                *value = Value::Int(42);
            }
        });
        assert!(matches!(pending_take(), Some(Value::Int(42))));
        unregister();
    }

    #[test]
    fn worker_pending_fault_aborts_detached_region() {
        let _serial = crate::parallel::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::parallel::clear_abort();
        crate::parallel::set_gilfree(true);
        register();

        assert!(pending_set_first(Value::Int(7)));
        assert!(crate::parallel::abort_requested());

        assert!(pending_take().is_some());
        unregister();
        crate::parallel::set_gilfree(false);
        crate::parallel::clear_abort();
    }
}
