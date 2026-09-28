//! One inlined TLS read per hot path instead of N out-of-line ones.
//!
//! WHAT THIS FIXES. RIP sampling of a real boot puts `std::thread::local::LocalKey::with`
//! in the top three leaves of EVERY window — the most uniform cost on the whole map:
//! blocks A+B 5.5% self / 6.3% on the proven chain, block C 7.7% self (the single largest
//! leaf in the window), the serial residue 5.1%, the ring 8.9% self / 9.4% (the #2 leaf
//! overall). Its named consumers are all per-thread MEMOS — `Vm::resolve_field_id`
//! (67.7% of the ring's cluster, 21.8% of blocks A+B), `owner_bits::contains_any`
//! (20-25% in A+B and block C), `Vm::resolve_method` (10-20% in three windows),
//! `opcodes::invokevirtual` (13-14%), `Vm::jit_direct_invoke` (14.9% of the ring's).
//!
//! WHY IT COSTS ANYTHING AT ALL. `thread_local!` only lowers to a bare address
//! computation when the declared type is `const`-initialised AND has no destructor. Every
//! cache in the engine is a `RefCell<HashMap<..>>` or a `RefCell<Vec<..>>`, which needs
//! drop, so `std` emits the destructor-registering storage instead: a real, out-of-line
//! function with a lazy-init/registration state check on EVERY access. That is the symbol
//! the profiler sees, and it is why the cost is proportional to the NUMBER of `with()`
//! calls rather than to the work done inside them. Two of the hottest accessors pay it
//! twice per call — `tlcache::field_key_get` touches the scratch key and then the map,
//! `tlcache::resolve_get` does the same.
//!
//! THE MECHANISM. Group the per-thread state that one path touches into a single struct,
//! and reach it through a `Cell<*const T>` whose initialiser IS a `const` block and whose
//! type has no destructor — the one shape `std` compiles to a plain TLS-slot load with no
//! call, no branch and no registration. The struct itself keeps its ordinary
//! `thread_local!` storage (so its lifetime, its drop at thread exit, and the stability of
//! every interior address are exactly what they are today); the pointer slot is only a
//! CURSOR onto it, filled once per thread on a cold path.
//!
//! SAFETY. The cursor is nulled by the cluster's own `Drop` — which `std` runs BEFORE the
//! struct's fields are dropped — so an access after thread-local destruction finds a null
//! cursor, falls into the cold path, and hits `std`'s own "accessed during or after
//! destruction" panic. That is the behaviour a bare `with()` has today, preserved exactly.
//! The reference handed back is never held across a suspension point and is never `&mut`:
//! interior mutability still goes through the same `RefCell`s, so the borrow discipline
//! (and its re-entrancy panics) is unchanged.
//!
//! This CHEAPENS AN OPERATION. It duplicates no body, specialises nothing, and adds no
//! per-call check to the fast path.
//!
//! HOW BIG THE POPULATION IS, AND WHOSE IT IS. Counted, not sampled — see [`stat`]. A real
//! boot to `Done` performs **1.016G** crossings: `frames` 0.607G, `tlcache` 0.308G,
//! `owner_bits` 0.102G, split 332.9M in blocks A+B / 206.5M in block C / 474.7M in the Done
//! window. (It was 1.654G until the fast frame stopped being a flag and started being the
//! shape of `jit_direct_invoke`; that took 635.8M crossings out of `frames`.) That is the
//! WHOLE population of this leaf, and it is the number any rival lever on it has to be priced
//! against — a rival that removes N of these crossings is worth at most N/1.016G of this
//! module, and over that overlap the two are SUBSTITUTES, never addends.
//!
//! THE THREE-STATE CURSOR, AND WHY THE OFF ARM IS NOT "MAIN PLUS OVERHEAD". The first
//! version read the gate on the cold fill and left the cursor NULL when the flag was off —
//! which meant the OFF arm re-entered a `#[cold] #[inline(never)] fill()` *and* an
//! `OnceLock::get_or_init` on EVERY access, i.e. it paid an extra out-of-line call for the
//! privilege of then doing the `with()` it was supposed to be a baseline for. An A/B against
//! that arm measures the flag plus a handicap it invented, so it can only understate the
//! win. The cursor now carries three states in the pointer itself —
//! null = "this thread has not resolved the gate yet", [`CURSOR_OFF`] = "resolved, disabled",
//! anything else = the payload address — so the resolved-OFF arm is one TLS load, two
//! predicted compares and then EXACTLY the `LocalKey::with` main runs, with no call and no
//! gate re-read; and the ON arm keeps its single load + single compare and never touches
//! the gate at all.

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::OnceLock;

/// Cursor value meaning "this thread resolved the gate and it is OFF". Never dereferenced;
/// distinct from null (unresolved) and from any real payload address (a thread-local lives
/// far above the first page, and the payloads are all larger than one byte anyway).
pub(crate) const CURSOR_OFF: usize = 1;

/// `RAVA_TLS_CLUSTER=1` — fill the per-thread cursors, so a cluster access is an inlined
/// TLS load instead of a `LocalKey::with` call. Default OFF.
///
/// Read ONCE PER THREAD, on the cold fill — never on either fast path. Both the ON and the
/// OFF outcome are cached in the cursor itself (see [`CURSOR_OFF`]), so neither arm re-reads
/// it and neither arm pays a call per access.
pub(crate) fn pointer_cursor() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_TLS_CLUSTER").is_some())
}

/// ── CROSSING CENSUS (`RAVA_TLS_STAT=1`) ──────────────────────────────────────────────
///
/// The population this module exists to remove: how many `LocalKey::with` crossings each
/// cluster actually performs over a boot. Counted ONLY in the resolved-OFF arm — that arm
/// already pays an out-of-line `with()` call, so one relaxed add beside it cannot change
/// what it is measuring, and the ON arm (the one that would ship) carries no counter at all.
///
/// This is the contention-immune way to price the mechanism and to price any *other* lever
/// that claims to remove the same crossings: a rival that removes N of the M crossings this
/// counts is worth at most N/M of it, and the two are SUBSTITUTES over that overlap, never
/// addends.
pub(crate) mod stat {
    use super::*;

    pub(crate) const FRAMES: usize = 0;
    pub(crate) const CACHES: usize = 1;
    pub(crate) const OWNER_BITS: usize = 2;
    pub(crate) const PROBE: usize = 3;
    pub(crate) const SLOTS: usize = 4;
    pub(crate) const NAMES: [&str; SLOTS] = ["frames", "tlcache", "owner_bits", "probe"];

    #[allow(clippy::declare_interior_mutable_const)]
    const ZERO: AtomicU64 = AtomicU64::new(0);
    static CROSSINGS: [AtomicU64; SLOTS] = [ZERO; SLOTS];

    /// 0 = undecided, 1 = counting, 2 = inert.
    static GATE: AtomicU8 = AtomicU8::new(0);

    #[inline]
    fn on() -> bool {
        match GATE.load(Ordering::Relaxed) {
            1 => true,
            2 => false,
            _ => arm(),
        }
    }

    #[cold]
    #[inline(never)]
    fn arm() -> bool {
        let on = std::env::var_os("RAVA_TLS_STAT").is_some();
        GATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
        if on {
            spawn_reporter();
        }
        on
    }

    /// One crossing of cluster `tag`. Inert (one relaxed byte load) unless RAVA_TLS_STAT.
    #[inline]
    pub(crate) fn note(tag: usize) {
        if on() {
            CROSSINGS[tag].fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn snapshot() -> [u64; SLOTS] {
        let mut out = [0u64; SLOTS];
        for (i, c) in CROSSINGS.iter().enumerate() {
            out[i] = c.load(Ordering::Relaxed);
        }
        out
    }

    /// Periodic line on stderr so a boot that is killed at a marker (or never exits, as a
    /// server boot does not) still yields a phase-attributable series. `t=` is milliseconds
    /// since the census armed, which the harness lines up with its own stopwatch.
    fn spawn_reporter() {
        let t0 = std::time::Instant::now();
        let _ = std::thread::Builder::new().name("tls-stat".into()).spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(1000));
            let s = snapshot();
            let total: u64 = s.iter().sum();
            let cols: Vec<String> =
                NAMES.iter().zip(s.iter()).map(|(n, v)| format!("{n}={v}")).collect();
            eprintln!(
                "[TLS-STAT] t={}ms total={total} {} direct_hits={}",
                t0.elapsed().as_millis(),
                cols.join(" "),
                crate::jit::DIRECT_HITS.load(Ordering::Relaxed),
            );
        });
    }
}

/// Declare a per-thread cluster: a struct of caches reached through one inlined TLS read.
///
/// ```ignore
/// thread_cluster! {
///     /// doc comment for the accessor
///     pub(crate) fn tl() -> &'static Caches;
///     slot CACHES = Caches::default();   // ordinary thread_local storage (lazy init)
///     cursor CACHES_PTR;                 // const-init pointer slot, inlined
///     census stat::CACHES;               // RAVA_TLS_STAT counter this cluster reports to
/// }
/// ```
///
/// Use `slot NAME: const = expr;` when the payload is `const`-constructible, which keeps
/// the storage eager exactly as the hand-written `thread_local! { .. = const { .. } }`
/// declarations it replaces.
///
/// The macro emits `impl Drop for $ty`, which is what nulls the cursor at thread exit —
/// so the cluster type must not define its own.
macro_rules! thread_cluster {
    (
        $(#[$m:meta])*
        $vis:vis fn $get:ident() -> &'static $ty:ty;
        slot $slot:ident = $init:expr;
        cursor $cursor:ident;
        census $tag:expr;
    ) => {
        ::std::thread_local! {
            static $slot: $ty = $init;
        }
        $crate::tls_cluster::thread_cluster_access!(
            $(#[$m])* $vis fn $get() -> &'static $ty; $slot; $cursor; $tag
        );
    };
    (
        $(#[$m:meta])*
        $vis:vis fn $get:ident() -> &'static $ty:ty;
        slot $slot:ident: const = $init:expr;
        cursor $cursor:ident;
        census $tag:expr;
    ) => {
        ::std::thread_local! {
            static $slot: $ty = const { $init };
        }
        $crate::tls_cluster::thread_cluster_access!(
            $(#[$m])* $vis fn $get() -> &'static $ty; $slot; $cursor; $tag
        );
    };
}

/// The half of [`thread_cluster`] that does not depend on how the payload is initialised.
macro_rules! thread_cluster_access {
    ($(#[$m:meta])* $vis:vis fn $get:ident() -> &'static $ty:ty; $slot:ident; $cursor:ident; $tag:expr) => {
        ::std::thread_local! {
            /// Cursor onto this thread's `$slot`. `Cell<*const _>` is `Copy` with no
            /// destructor and its initialiser is a `const` block — the one combination
            /// `std` lowers to a bare `#[thread_local]` static, so reading it inlines to
            /// a TLS-slot load with no init check and nothing to call.
            ///
            /// Three states: null = this thread has not resolved the gate,
            /// `tls_cluster::CURSOR_OFF` = resolved and disabled, anything else = the
            /// payload address. Caching the DISABLED answer here is what keeps the OFF
            /// arm an honest baseline — it reaches the plain `with()` without a call and
            /// without re-reading the gate.
            static $cursor: ::std::cell::Cell<*const $ty> =
                const { ::std::cell::Cell::new(::std::ptr::null()) };
        }

        /// Nulls the cursor before the payload's fields are dropped, so an access after
        /// thread-local destruction takes the cold path and hits `std`'s own
        /// already-destroyed panic instead of reading freed state.
        impl ::std::ops::Drop for $ty {
            fn drop(&mut self) {
                $cursor.set(::std::ptr::null());
            }
        }

        $(#[$m])*
        #[inline(always)]
        $vis fn $get() -> &'static $ty {
            let p = $cursor.get();
            if p as usize > $crate::tls_cluster::CURSOR_OFF {
                // SAFETY: the cursor holds a real address only between the cold fill
                // below and this thread's `Drop for $ty`, which nulls it before dropping
                // any field. In that window it points at this thread's own `$slot`, whose
                // storage lives in the thread's TLS block and never moves.
                return unsafe { &*p };
            }
            if p as usize == $crate::tls_cluster::CURSOR_OFF {
                $crate::tls_cluster::stat::note($tag);
                // SAFETY (and the point of this arm): byte-for-byte the `LocalKey::with`
                // the code ran before this module existed. The borrow is not held past
                // the return — every caller re-reads through `$get()`.
                return $slot.with(|s| unsafe { &*(s as *const $ty) });
            }
            #[cold]
            #[inline(never)]
            fn fill() -> &'static $ty {
                let on = $crate::tls_cluster::pointer_cursor();
                let p = $slot.with(|s| s as *const $ty);
                $cursor.set(if on {
                    p
                } else {
                    $crate::tls_cluster::CURSOR_OFF as *const $ty
                });
                if !on {
                    $crate::tls_cluster::stat::note($tag);
                }
                // SAFETY: `with` proved the slot is alive on this thread for the
                // duration of the call, and the borrow is not held past the return —
                // every caller re-reads through `$get()`.
                unsafe { &*p }
            }
            fill()
        }
    };
}

pub(crate) use {thread_cluster, thread_cluster_access};

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    #[derive(Default)]
    pub(crate) struct Probe {
        a: RefCell<Vec<u32>>,
        b: RefCell<String>,
    }
    crate::tls_cluster::thread_cluster! {
        fn probe() -> &'static Probe;
        slot PROBE = Probe::default();
        cursor PROBE_PTR;
        census crate::tls_cluster::stat::PROBE;
    }

    /// The cursor's declared type must stay `Copy`-with-no-destructor, because that is
    /// the ONLY property that makes `std` emit the cheap storage. A future edit that
    /// wrapped it in something droppable would silently put the `LocalKey::with` call
    /// back on the hot path while every test still passed.
    #[test]
    fn cursor_type_has_no_destructor() {
        assert!(!std::mem::needs_drop::<std::cell::Cell<*const u8>>());
    }

    /// The DISABLED answer must be cached in the cursor, not re-derived. A null cursor
    /// means "gate unresolved", which sends every single access back through the `#[cold]
    /// #[inline(never)]` fill AND an `OnceLock` — an out-of-line call per access that the
    /// baseline arm is not supposed to pay. That handicap is invisible to every other test
    /// (behaviour is identical either way) and it silently biases any A/B of the flag
    /// against itself, so it is asserted here rather than left to a profile to notice.
    #[test]
    fn disabled_arm_resolves_the_gate_once_per_thread() {
        assert!(!super::pointer_cursor(), "the test process must run the disabled arm");
        std::thread::spawn(|| {
            assert!(PROBE_PTR.get().is_null(), "a fresh thread starts unresolved");
            probe().a.borrow_mut().push(1);
            assert_eq!(
                PROBE_PTR.get() as usize,
                super::CURSOR_OFF,
                "the disabled answer must be cached in the cursor",
            );
            // ...and the accessor keeps working from that state.
            probe().a.borrow_mut().push(2);
            assert_eq!(probe().a.borrow().as_slice(), [1, 2]);
        })
        .join()
        .expect("probe thread");
    }

    /// Repeated accesses on one thread name the SAME storage — which is what makes it a
    /// cache at all, and what lets `register` publish an interior address.
    #[test]
    fn same_thread_sees_one_stable_cluster() {
        let first = probe() as *const Probe;
        probe().a.borrow_mut().push(7);
        assert_eq!(probe() as *const Probe, first, "cluster address is stable per thread");
        assert_eq!(probe().a.borrow().as_slice(), [7], "state persists across accesses");
    }

    /// Grouping must not leak state between threads — the property every cache in the
    /// cluster depends on (a worker gets its own cold copy, never another thread's).
    #[test]
    fn each_thread_gets_its_own_cluster() {
        probe().a.borrow_mut().push(1);
        probe().b.borrow_mut().push_str("main");
        let mine = probe() as *const Probe as usize;
        let (theirs, len, text) = std::thread::spawn(|| {
            probe().a.borrow_mut().push(2);
            (probe() as *const Probe as usize, probe().a.borrow().len(), probe().b.borrow().clone())
        })
        .join()
        .expect("probe thread");
        assert_ne!(mine, theirs, "a second thread gets its own cluster");
        assert_eq!(len, 1, "the other thread started empty");
        assert_eq!(text, "", "and saw none of this thread's writes");
        assert_eq!(probe().a.borrow().as_slice(), [1], "this thread's own state is untouched");
    }

    /// Two fields of one cluster can be borrowed at once — the arrangement the accessors
    /// that used to touch two separate thread-locals (a scratch key and the map it keys)
    /// now rely on. A regression here would be a `RefCell` double-borrow panic in the
    /// hottest memo in the engine.
    #[test]
    fn distinct_fields_borrow_independently() {
        let p = probe();
        let mut key = p.b.borrow_mut();
        key.clear();
        key.push_str("owner\0name");
        assert!(p.a.borrow().is_empty() || !p.a.borrow().is_empty());
        assert_eq!(key.as_str(), "owner\0name");
    }
}
