//! REAL OBJECT MONITORS — `monitorenter`/`monitorexit`, `Object.wait`/`notify`/`notifyAll`.
//!
//! ⭐**Why this exists, and why it did not before.** For as long as exactly one thread ever ran
//! bytecode, a monitor could not be contended, so `monitorenter` was a null check and nothing
//! else (`opcodes/ops/objects.rs`) and `wait`/`notify` returned at once (`natives/cascade.rs`).
//! Both were correct for that engine and are recorded as such.
//!
//! The moment the op-loop GIL hand-off actually let a second thread run guest code, that stopped
//! being true — and it broke in the least visible way possible. Guava's `Suppliers.memoize` is a
//! textbook double-checked lock:
//!
//! ```text
//! if (!initialized) { synchronized (this) { if (!initialized) {
//!     T t = delegate.get(); value = t; initialized = true; delegate = null; return t; } } }
//! return value;
//! ```
//!
//! With no mutual exclusion a second thread walks straight into a critical section the first is
//! still inside and observes a half-built object. Minecraft's `Codec$RecursiveCodec.decode` does
//! `wrapped.get().decode(...)` on exactly such a supplier, and the offloaded datapack reload died
//! with `UnsatisfiedLinkError: invokevirtual DataResult$Error.decode(...)` — the memoized
//! "codec" was a `DataResult`. Not a codec bug and not a GC bug: a lock that did not lock.
//!
//! ⚠**THE DEFAULT PATH IS UNTOUCHED, BY CONSTRUCTION.** Every entry point returns immediately
//! unless a real OS thread is live (`gil::has_real_threads()`), which is false for the entire
//! single-threaded boot and every gate. The exits additionally consult `ACTIVE`, so a monitor
//! taken while a thread was live is still released correctly if that thread dies in between —
//! the asymmetric case a bare `has_real_threads()` check on both sides would leak.
//!
//! ⭐**AND SINCE THE ROUND-6 LOCKING PASS, THE DEFAULT PATH DOES NOT EVEN BUILD ITS ARGUMENT.**
//! `has_real_threads()` is now one relaxed load of a monotone process latch (`gil.rs`), and
//! `key_of` consults the same latch first — so a `monitorenter` on the serial boot is a null
//! check and a load, with no identity stamp, no string hash and no table. Kill switch
//! `RAVA_NO_FAST_LOCKING=1` restores the exact pre-round behaviour on both.
//!
//! **Lock ordering is the one hazard and it has one rule: never acquire the GIL while holding
//! the monitor table.** A contended enter drops the table lock, releases the GIL, waits, and only
//! re-acquires the GIL after the table lock is gone — otherwise the owner could never run to
//! release what we are waiting for.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use crate::value::Value;

/// One monitor's state. `owner` is 0 when free. `notify_gen` bumps on every
/// `notify`/`notifyAll` so a waiter can tell a real wake from a spurious one.
#[derive(Default)]
struct MonState {
    owner: u64,
    count: u32,
    notify_gen: u64,
}

static TABLE: Mutex<Option<HashMap<u64, MonState>>> = Mutex::new(None);
static CV: Condvar = Condvar::new();
/// Number of HELD monitors. Zero for the whole single-threaded boot, which lets
/// `exit` be a single relaxed load rather than a table lock.
static ACTIVE: AtomicU64 = AtomicU64::new(0);
/// Always-on census (four relaxed adds on a path that only runs once a real thread exists):
/// `enter`s taken, `enter`s that found the monitor owned by ANOTHER thread, `wait`s, `notify`s.
/// ⭐The contended count is the one that matters — it is the direct evidence of whether guest
/// code ever actually races here, which no amount of reasoning about the GIL settles.
static N_ENTER: AtomicU64 = AtomicU64::new(0);
static N_CONTENDED: AtomicU64 = AtomicU64::new(0);
static N_WAIT: AtomicU64 = AtomicU64::new(0);
static N_NOTIFY: AtomicU64 = AtomicU64::new(0);

/// One line of monitor census, for a fatal-path dump (see the ULE forensics).
pub(crate) fn stat_line() -> String {
    format!(
        "enter={} contended={} wait={} notify={} held={}",
        N_ENTER.load(Ordering::Relaxed),
        N_CONTENDED.load(Ordering::Relaxed),
        N_WAIT.load(Ordering::Relaxed),
        N_NOTIFY.load(Ordering::Relaxed),
        ACTIVE.load(Ordering::Relaxed),
    )
}

thread_local! {
    /// ⚠**`const`-INITIALISED ON PURPOSE.** This was a lazily-initialised `thread_local! { static
    /// TID: u64 = <expr> }`, and a TLS key with a non-const initialiser is not a TLS slot read: it
    /// is a slot read, an "is this thread's copy built yet" state check and a branch to an
    /// out-of-line initialiser, on EVERY access. `this_thread_id()` sits on the class-init lock's
    /// path (`vm/apply.rs`), which every static access on every already-initialised class walks,
    /// so that init check was being paid per static access for the life of the process.
    ///
    /// WAS: TLS lazy-init state check + branch per ask. IS: a plain TLS load, plus a
    /// predictable `!= 0` on the id itself. Ids are only ever compared for equality and are still
    /// handed out in first-touch order from the same counter, so this is bit-identical.
    /// 0 is the "not yet assigned" sentinel, which is why the counter starts at 1.
    static TID: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}
fn tid() -> u64 {
    TID.with(|t| {
        let v = t.get();
        if v != 0 {
            return v;
        }
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let fresh = NEXT.fetch_add(1, Ordering::Relaxed);
        t.set(fresh);
        fresh
    })
}
/// This thread's monitor identity, for callers that need to name an owner —
/// the class-initialization lock (JVMS §5.5) uses it the same way a monitor does.
pub(crate) fn this_thread_id() -> u64 {
    tid()
}

/// Give up the GIL for `ms`, then take it back. The one legal way to wait for
/// another thread inside guest execution: keep holding it and the thread you are
/// waiting for can never run.
pub(crate) fn park_without_gil(ms: u64) {
    crate::gil::release();
    std::thread::sleep(Duration::from_millis(ms));
    crate::gil::acquire();
}

fn table() -> std::sync::MutexGuard<'static, Option<HashMap<u64, MonState>>> {
    TABLE.lock().unwrap_or_else(|e| e.into_inner())
}

/// The monitor key for a receiver: its OBJECT IDENTITY, not its address.
///
/// ⭐Arena objects move, so an address is not an identity — but the engine already stamps a
/// GC-stable, process-unique id into the header on first `System.identityHashCode`
/// (`nextgen::arena_identity_hash`), and that is exactly the right key: it rides along through
/// Cheney evacuation and no two objects share one. Legacy handles are already stable. Tagged per
/// value form so a handle and an arena id can never collide.
///
/// ⭐**AND ON A SERIAL BOOT IT IS NOT COMPUTED AT ALL.** `monitorenter`/`monitorexit`
/// (`opcodes/ops/objects.rs`), `notify`/`notifyAll` (`opcodes/invoke_virtual.rs`) and
/// `Object.wait` (`natives/cascade.rs`) all build this key BEFORE the entry point that then
/// discards it — every one of them returns immediately while no real thread exists. So the key
/// was pure waste on every lock op of the single-threaded boot, and not cheap waste:
///
///   * the arena arm calls `arena_identity_hash`, which for an unstamped object takes a global
///     `fetch_add` and WRITES the object header — a MUTATION performed by a lock op that does
///     nothing. HotSpot does not install an identity hash on `monitorenter` either (installing
///     one there is precisely what forces inflation), so declining to is the closer behaviour;
///   * the string arm SipHashes the whole string, i.e. O(length) per lock op.
///
/// WAS: an identity stamp (plus header write) or an O(string length) hash per lock op, thrown
/// away, for the whole serial boot. IS: one relaxed load of the thread latch.
///
/// The gate is `gil::serial_locking()` and not `!has_real_threads()`, and the difference is the
/// asymmetric case `exit` exists for: `ACTIVE` can only ever leave zero from inside `enter`,
/// which requires a real thread, which retires the latch FOREVER — so `serial_locking()` implies
/// `ACTIVE == 0` implies every consumer of this key is a no-op. A monitor taken while a worker
/// was live therefore still gets its key, and its release, after that worker dies.
pub(crate) fn key_of(v: &Value) -> Option<u64> {
    if crate::gil::serial_locking() {
        return None;
    }
    key_of_raw(v)
}

/// The key itself, with no liveness gate — the shape the tests pin and the one `key_of` calls
/// once it is worth computing.
fn key_of_raw(v: &Value) -> Option<u64> {
    match v {
        Value::Ref(h) => Some(0x1000_0000_0000_0000 | *h as u64),
        Value::Array(h) => Some(0x2000_0000_0000_0000 | *h as u64),
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) => {
            Some(0x3000_0000_0000_0000 | crate::nextgen::arena_identity_hash(*p) as u32 as u64)
        }
        // A `synchronized` on a string locks the interned instance; hashing its
        // content is the identity this engine has for one.
        Value::Str(s) => {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            s.hash(&mut h);
            Some(0x4000_0000_0000_0000 | (h.finish() & 0x0fff_ffff_ffff_ffff))
        }
        _ => None,
    }
}

/// Enter `key`'s monitor, blocking until it is free. Reentrant for the owner.
/// No-op while no real thread exists — the single-threaded engine's monitor
/// cannot be contended, and this keeps the default boot bit-identical.
pub(crate) fn enter(key: u64) {
    if !crate::gil::has_real_threads() {
        return;
    }
    let me = tid();
    let mut dropped_gil = false;
    loop {
        {
            let mut g = table();
            let map = g.get_or_insert_with(HashMap::new);
            let st = map.entry(key).or_default();
            if st.owner == 0 {
                st.owner = me;
                st.count = 1;
                ACTIVE.fetch_add(1, Ordering::Relaxed);
                N_ENTER.fetch_add(1, Ordering::Relaxed);
            } else if st.owner == me {
                st.count += 1;
            } else {
                N_CONTENDED.fetch_add(1, Ordering::Relaxed);
                // Contended. Drop the table lock FIRST, then the GIL — the owner needs
                // the GIL to make the progress we are waiting for.
                drop(g);
                if !dropped_gil {
                    crate::gil::release();
                    dropped_gil = true;
                }
                let g = table();
                let _ = CV.wait_timeout(g, Duration::from_millis(2));
                continue;
            }
        }
        if dropped_gil {
            crate::gil::acquire();
        }
        return;
    }
}

/// Leave `key`'s monitor (one level of reentrancy).
///
/// ⚠Gated on `ACTIVE`, NOT on `has_real_threads()`: a monitor taken while a worker was live must
/// still be released after that worker exits, or it stays held forever.
pub(crate) fn exit(key: u64) {
    if ACTIVE.load(Ordering::Relaxed) == 0 {
        return;
    }
    let me = tid();
    let mut g = table();
    let Some(map) = g.as_mut() else { return };
    let Some(st) = map.get_mut(&key) else { return };
    if st.owner != me || st.count == 0 {
        return; // not ours: an unbalanced exit is the guest's problem, not a panic here
    }
    st.count -= 1;
    if st.count == 0 {
        st.owner = 0;
        ACTIVE.fetch_sub(1, Ordering::Relaxed);
        drop(g);
        CV.notify_all();
    }
}

/// `Object.wait`: release the monitor entirely, block until notified or the
/// timeout expires, then re-acquire it at the same reentrancy depth.
/// Returns false if this thread does not own the monitor — the caller then keeps
/// the historical no-op behaviour rather than inventing an exception.
pub(crate) fn wait(key: u64, timeout_ms: Option<u64>) -> bool {
    if !crate::gil::has_real_threads() {
        return false;
    }
    let me = tid();
    let saved;
    let gen;
    {
        let mut g = table();
        let Some(map) = g.as_mut() else { return false };
        let Some(st) = map.get_mut(&key) else { return false };
        if st.owner != me {
            return false;
        }
        saved = st.count;
        gen = st.notify_gen;
        st.owner = 0;
        st.count = 0;
        ACTIVE.fetch_sub(1, Ordering::Relaxed);
    }
    N_WAIT.fetch_add(1, Ordering::Relaxed);
    CV.notify_all();
    crate::gil::release();
    let deadline = timeout_ms.map(|ms| std::time::Instant::now() + Duration::from_millis(ms));
    loop {
        let g = table();
        let woken = g
            .as_ref()
            .and_then(|m| m.get(&key))
            .is_some_and(|st| st.notify_gen != gen);
        if woken {
            break;
        }
        if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            break;
        }
        let _ = CV.wait_timeout(g, Duration::from_millis(2));
    }
    crate::gil::acquire();
    // Re-take the monitor at the depth we left it.
    enter(key);
    if saved > 1 {
        let mut g = table();
        if let Some(st) = g.as_mut().and_then(|m| m.get_mut(&key)) {
            st.count = saved;
        }
    }
    true
}

/// `Object.notify` / `notifyAll`. One generation bump wakes every waiter, which
/// then re-checks its own predicate — a legal (and at these thread counts the
/// simplest correct) reading of `notify`'s "one arbitrary thread".
pub(crate) fn notify(key: u64) {
    if !crate::gil::has_real_threads() {
        return;
    }
    {
        let mut g = table();
        let map = g.get_or_insert_with(HashMap::new);
        map.entry(key).or_default().notify_gen += 1;
        N_NOTIFY.fetch_add(1, Ordering::Relaxed);
    }
    CV.notify_all();
}

#[cfg(test)]
mod tests {
    use super::*;

    // The single-threaded engine must not pay for, or observe, any of this.
    #[test]
    fn inert_without_real_threads() {
        enter(0xdead_beef);
        assert_eq!(
            ACTIVE.load(Ordering::Relaxed),
            0,
            "no monitor taken while single-threaded"
        );
        exit(0xdead_beef);
        assert!(!wait(0xdead_beef, Some(1)));
    }

    #[test]
    fn keys_are_disjoint_across_value_forms() {
        // `key_of_raw`, not `key_of`: the public entry declines to build a key at all while the
        // process has never had a second thread, which is exactly the state a unit test runs in.
        let a = key_of_raw(&Value::Ref(7)).unwrap();
        let b = key_of_raw(&Value::Array(7)).unwrap();
        assert_ne!(a, b, "a handle and an array handle are different monitors");
        assert_eq!(key_of_raw(&Value::Null), None);
        assert_eq!(key_of_raw(&Value::Int(3)), None);
    }

    /// The serial-boot short-circuit is a REFUSAL, not a different answer: no key, therefore no
    /// enter, no exit, no allocation, no stamp — and the moment a real thread exists the latch is
    /// retired and the key comes back.
    #[test]
    fn key_is_not_built_while_single_threaded() {
        // Touch the latch through the same entry the engine uses, so the probe runs. Guarded
        // rather than asserted: the test binary shares one process, and a sibling test that
        // starts a real thread legitimately retires the latch for everyone.
        let _ = crate::gil::has_real_threads();
        if crate::gil::serial_locking() {
            assert_eq!(key_of(&Value::Ref(11)), None);
        }
        assert!(key_of_raw(&Value::Ref(11)).is_some());
    }
}
