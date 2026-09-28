//! The one global re-entrant resolve lock a gil-free worker takes, its RAII guard, and the attribution that names which callee anchors a serialized span.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use super::{has_wide_params, JitStatus};

#[cfg(feature = "next-gen-gc")]
impl ResolveGuard {
    /// Did THIS guard take the real mutex (as opposed to passing through an
    /// ancestor's hold)? The outer/nested split is what the envelope-width
    /// question turns on: only an `Outer` guard's lifetime is a serialized span.
    pub(crate) fn is_outer(&self) -> bool {
        matches!(self, ResolveGuard::Outer(_))
    }
}

#[cfg(feature = "next-gen-gc")]
impl Drop for ResolveGuard {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;
        match self {
            // Top frame: count returns to 0. Clear the owner BEFORE the MutexGuard field drops (which
            // releases the mutex) so the next acquirer never observes a stale self-owner.
            ResolveGuard::Outer(_) => {
                // LANE PROBE: bill the HOLD span — the serialized moment itself,
                // distinct from the WAIT the acquirer already billed. Re-entrant
                // (`Nested`) frames are inside this span and must not double-count,
                // which is why the timer hangs off the outer guard only.
                #[cfg(feature = "next-gen-gc")]
                if crate::lane_probe::enabled() {
                    LOCK_T0.with(|c| {
                        if let Some(t0) = c.take() {
                            crate::lane_probe::note_lock_hold(t0.elapsed().as_nanos() as u64);
                        }
                    });
                }
                // OUTER ATTRIBUTION (`RAVA_LANE_OUTER`): which CALLEE opened the
                // serialized span. `locked_tally` counts every locked entry, 99.99%
                // of which are re-entrant pass-throughs inside somebody else's hold;
                // only the outer acquirer's identity says where the envelope is
                // actually anchored, and its hold is the whole subtree beneath it.
                #[cfg(feature = "next-gen-gc")]
                if outer_attrib::enabled() {
                    outer_attrib::record_hold();
                }
                LOCK_OWNER.store(0, Ordering::Release);
            }
            ResolveGuard::Nested => {}
            ResolveGuard::None => {}
        }
    }
}

/// 11.4/11.6 — RESOLUTION FALLBACK LOCK, RE-ENTRANT (Thread-Owner Guard). The dispatch-miss handler is
/// the ONLY place a pool worker touches non-Sync `&mut Vm` resolution state (resolve_method / class
/// `<clinit>` / jit_call, + jit_pending / set_jit_fault) — the second half of the G5 wall. A worker
/// holds this exclusively for the whole miss so no two workers mutate `Vm` at once. But the miss's
/// `jit_call` RUNS the resolved callee, whose OWN compiled PIC sites can miss and RE-ENTER this handler
/// on the SAME thread — a plain `Mutex` self-deadlocks there (11.5). So this is a custom re-entrant
/// lock: an atomic owner token + a recursion count. Re-entry by the current owner just bumps the count
/// and passes through LOCK-FREE (under the outer hold); the real mutex is taken once at the top frame
/// and released once when the count returns to 0. Main thread never locks. Hit path never enters here,
/// so once a worker's per-thread IC (11.3) + seeded cache (11.2) stabilise, misses cease → lock-free.
/// (std, no parking_lot dep — a cold path.)
#[cfg(feature = "next-gen-gc")]
/// An `RwLock`, not a `Mutex`, since 2026-08-16. The exclusive side is unchanged — every
/// existing acquirer takes `.write()` and the semantics it had as a mutex — but a
/// PROVABLY-READ-ONLY callback can now take `.read()` and run beside its siblings instead
/// of behind them. `RAVA_LANE_OUTER` put **82,382,752 of a boot's 86,302,646 acquisitions**
/// on one gate, a per-kind tally made them 100% `fu_invoke`, and a per-member tally made
/// those 100% **`getDouble`** — the ring was serialising four workers to read an array
/// element. See `Vm::fu_read_shared`.
pub(crate) static VM_RESOLVE_LOCK: std::sync::RwLock<()> = std::sync::RwLock::new(());
/// Owner token of the thread currently holding VM_RESOLVE_LOCK (0 = free). A stable per-thread u64
/// (below) — ThreadId has no atomic-storable numeric form on stable Rust, so we mint our own identity.
#[cfg(feature = "next-gen-gc")]
pub(crate) static LOCK_OWNER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
// REMOVED: `LOCK_COUNT`, the owner's re-entrancy depth. It was WRITTEN in four places and READ
// in none — a value nobody consumed, maintained by two contended atomic RMWs per acquisition on
// a cache line four ring workers share. The armed lane probe puts that at `locks=86,302,652`
// per boot, i.e. ~172 M read-modify-writes, and the ring's own host profile (ledger row 158)
// bills `resolve_lock_acquire` 5.50% + `drop_in_place<ResolveGuard>` 2.04% of live ring cpu.
//
// The depth was never needed: `ResolveGuard`'s own Outer/Nested split already carries it, since
// only an `Outer` guard releases the mutex and only a `Nested` one passes through.
#[cfg(feature = "next-gen-gc")]
pub(crate) static NEXT_TID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1); // 0 reserved = "no owner"
#[cfg(feature = "next-gen-gc")]
thread_local! {
    /// This thread's stable, unique identity token (assigned once, never reused — monotonic).
    static MY_TID: u64 = NEXT_TID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Diagnostic: current VM_RESOLVE_LOCK owner token (0 = free). For the [WORKER-ALIVE] heartbeat.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn lock_owner_dbg() -> u64 {
    LOCK_OWNER.load(std::sync::atomic::Ordering::Relaxed)
}

/// Does the callee at `(caller, cp)` declare a wide (`J`/`D`) parameter?
///
/// Delegates to `has_wide_params`, which is the same question asked of a descriptor and is
/// already tested. Writing a second scanner here was a mistake and a wrong one: it treated `[J`
/// and `[D` as wide, when an array of anything is a single NARROW reference — so it refused
/// sites that were perfectly serviceable.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn site_has_wide_param(caller_ptr: i64, cp: i64) -> bool {
    let caller = unsafe { (caller_ptr as *const crate::class_loader::LoadedClass).as_ref() };
    let Some(caller) = caller else { return false };
    let Some(m) = caller.file.pool.member_ref(cp as u16) else { return false };
    has_wide_params(m.descriptor)
}

/// `RAVA_FREEZE_VERIFY` — check every freeze-table hit against a fresh resolution.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn freeze_verify_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_FREEZE_VERIFY").is_some())
}

/// Re-derive the body the LOCKED path would enter for `(caller, cp, receiver)` and report when
/// the freeze table is about to call a different one.
///
/// Pure reads: `resolve_method` consults the shared-readable loader plus a thread-local cache, and
/// the `jit_cache` lookup is a map read. Nothing here publishes, so arming the check cannot change
/// what the run computes — which is the property a divergence hunt needs from its own instrument.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn freeze_verify(vm: &crate::Vm, caller_ptr: i64, cp: i64, recv: i64, called: usize) {
    let caller = unsafe { (caller_ptr as *const crate::class_loader::LoadedClass).as_ref() };
    let Some(caller) = caller else { return };
    let Some(m) = caller.file.pool.member_ref(cp as u16) else { return };
    let raw_u = recv as u64;
    let cls: String = if recv != 0 && raw_u >> 40 == 0 {
        let cid = unsafe { crate::nextgen::obj_class(recv as *mut u8) };
        match crate::nextgen::class_name(cid) {
            Some(s) => s.to_string(),
            None => return,
        }
    } else if raw_u >> 40 == 1 {
        let h = (recv & (crate::value::REF_TAG - 1)) as u32;
        match vm.heap.class_of(h) {
            Some(s) => s.to_string(),
            None => return,
        }
    } else {
        return;
    };
    let Ok(Some(decl)) = vm.resolve_method(&cls, m.name, m.descriptor) else { return };
    let Some(code) = decl.file.method(m.name, m.descriptor).and_then(|x| x.code.as_ref()) else {
        return;
    };
    let key = code as *const crate::Code as usize;
    let want = match vm.jit_cache.get(&key) {
        Some(JitStatus::Compiled(f)) => f.raw() as usize,
        _ => return, // not compiled: the locked path would interpret it, nothing to compare
    };
    if want == called {
        return;
    }
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    if N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 12 {
        eprintln!(
            "[FREEZE-MISMATCH] site={}.{}{} recv_class={cls} called={called:#x} want={want:#x} \
             (declaring={})",
            caller.file.this_class, m.name, m.descriptor, decl.file.this_class
        );
    }
}

/// GAP-S09 stage 4: does THIS thread currently own the re-entrant resolve lock?
/// Exactly the ownership test `resolve_lock_acquire` uses for its lock-free
/// re-entry (owner token == our stable TID) — i.e. "a `WorkerGate::Locked` /
/// locked-exec envelope is active somewhere up this thread's stack". The
/// mode-aware `vm_ref()` uses it to admit `&mut Vm` inside the exclusive
/// envelope (main parked + siblings serialized on the same lock) and deny it
/// everywhere else on a gil-free worker.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn resolve_lock_held_by_me() -> bool {
    // RAVA_LANE_NOLOCK — DELIBERATELY UNSOUND, MEASUREMENT ONLY, never a default.
    //
    // The experiment drops the guard at the dispatch site to price what eight workers CAN do.
    // Dropping it there alone does not answer the question: every downstream admission gate is
    // phrased as "do I own the resolve lock", so with the guard gone the first `vm_ref()` denies,
    // the region tears down, and the run measures an abort instead of a parallel execution —
    // which is exactly what the first two no-lock boots measured.
    //
    // Under the flag this thread therefore CLAIMS ownership it does not have. Results may be
    // wrong and the run may crash; neither invalidates the timing, because what is being priced
    // is the wall of a region that runs, not the values it produces.
    if crate::lane_nolock_experiment() {
        return true;
    }
    MY_TID.with(|t| LOCK_OWNER.load(std::sync::atomic::Ordering::Acquire) == *t)
}

/// RAII holder for the re-entrant resolve lock. `Outer` owns the real mutex guard (releases it, after
/// clearing the owner token, when it drops); `Nested` is a pass-through re-entry (just decrements the
/// count); `None` is the main-thread / non-worker case (no lock at all).
#[cfg(feature = "next-gen-gc")]
pub(crate) enum ResolveGuard {
    Outer(#[allow(dead_code)] std::sync::RwLockWriteGuard<'static, ()>),
    Nested,
    None,
}

/// Acquire the re-entrant resolve lock for THIS worker. Nested re-entry (this thread already owns) is
/// lock-free; a fresh acquire blocks on the mutex, then stamps ownership.
/// pub(crate): the G1.1 jit_call worker guard (lib.rs) also takes it under
/// RAVA_M3_WORKER_LOCKED_EXEC — locked general execution instead of a region abort.
#[cfg(feature = "next-gen-gc")]
#[track_caller]
pub(crate) fn resolve_lock_acquire() -> ResolveGuard {
    use std::sync::atomic::Ordering;
    let tid = MY_TID.with(|t| *t);
    if LOCK_OWNER.load(Ordering::Acquire) == tid {
        // Re-entry: we already hold the mutex (owner ⇔ holder). Pass through under the outer
        // hold — no bookkeeping, because the guard variant IS the bookkeeping.
        ResolveGuard::Nested
    } else {
        // LANE PROBE: split the acquisition into WAIT (blocked behind a sibling)
        // and HOLD (billed in Drop). Only these two numbers can say whether the
        // one global resolve lock is the lane's ceiling; a count alone cannot.
        #[cfg(feature = "next-gen-gc")]
        let probe_t0 = crate::lane_probe::enabled().then(std::time::Instant::now);
        let guard = VM_RESOLVE_LOCK.write().unwrap_or_else(|e| e.into_inner());
        #[cfg(feature = "next-gen-gc")]
        if let Some(t0) = probe_t0 {
            crate::lane_probe::note_lock_wait(t0.elapsed().as_nanos() as u64);
            LOCK_T0.with(|c| c.set(Some(std::time::Instant::now())));
        }
        LOCK_OWNER.store(tid, Ordering::Release);
        // OUTER ATTRIBUTION: bill this serialized span to the acquisition SITE.
        // `jit_call` is only one of ~8 acquirers; the measured 392 outer spans are
        // dominated by others, so a jit_call-only tag leaves the real anchor unnamed.
        if outer_attrib::enabled() {
            outer_attrib::note_outer_at(std::panic::Location::caller());
        }
        ResolveGuard::Outer(guard)
    }
}

/// SHARED acquisition, for a caller that will only READ through `&Vm`.
///
/// Returns `None` — meaning "do not use the shared path" — in the one case where taking it
/// would be wrong: **this thread already holds the exclusive side**. `std::sync::RwLock` is
/// not re-entrant and has no upgrade, so a `.read()` under our own `.write()` is a
/// self-deadlock. The caller falls through to the normal gate, which sees the owner token
/// and passes through as `Nested` at no cost — the same answer, by the path that already
/// had exclusivity.
///
/// Readers still exclude WRITERS. This removes the serialisation among readers, not the
/// mutual exclusion that makes the reads correct in the first place.
///
/// ⛔A READER-SHARDED version of this (a big-reader lock: eight shards, reader takes one,
/// writer takes all eight under the exclusive lock) was BUILT AND REVERTED. The motivation
/// was real — after the conversion the ring profile still bills `RwLock::read_contended`
/// 7.35% of live samples while writers are under 500,000 a boot, so the remaining
/// coordination is readers bouncing ONE cache line between four cores — and the sharding
/// did move that row down. **`Done` did not move at all** (15.49/15.39 against 15.48/15.33),
/// so the row was not on the critical path. Eight extra write acquisitions per writer and a
/// second lock order in the tree's most delicate concurrency path, for no measured gain, is
/// the trade rule 10 exists to refuse.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn resolve_lock_read() -> Option<std::sync::RwLockReadGuard<'static, ()>> {
    use std::sync::atomic::Ordering;
    if LOCK_OWNER.load(Ordering::Acquire) == MY_TID.with(|t| *t) {
        return None;
    }
    Some(VM_RESOLVE_LOCK.read().unwrap_or_else(|e| e.into_inner()))
}

#[cfg(feature = "next-gen-gc")]
thread_local! {
    /// LANE PROBE: when this thread took the OUTER resolve lock. One slot suffices
    /// — the lock is re-entrant, so a thread holds at most one outer guard.
    static LOCK_T0: std::cell::Cell<Option<std::time::Instant>> =
        const { std::cell::Cell::new(None) };
}

/// OUTER-ACQUIRER ATTRIBUTION (`RAVA_LANE_OUTER=1`, default OFF, diagnostic only).
///
/// The lane probe says the resolve lock is held ~82% of a ring region, but a
/// per-worker millisecond total cannot say WHY: the hold is anchored by whichever
/// callee first reached the general `jit_call` path on that thread, and it then
/// spans that callee's ENTIRE subtree (measured: ~395 outer acquisitions covering
/// 3.78M locked entries, ~400 ms each). Deciding whether the envelope can be
/// narrowed requires naming those anchors — a callee that is native/pure-read is a
/// candidate to serve lock-free (collapsing the span to the real general-path calls
/// deeper in the tree); a callee that is a large bytecode body is not.
///
/// One TLS symbol slot per thread (set at the acquisition site only when the guard
/// is `Outer`) plus one mutex-guarded tally updated once per outer RELEASE — i.e.
/// ~395 map touches per region, not per call. Byte-neutral when unset.
#[cfg(feature = "next-gen-gc")]
pub(crate) mod outer_attrib {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    pub(crate) fn enabled() -> bool {
        static ON: OnceLock<bool> = OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("RAVA_LANE_OUTER").is_some())
    }

    thread_local! {
        /// `(symbol, acquired_at)` for the outer guard this thread currently holds.
        static CUR: std::cell::RefCell<Option<(String, std::time::Instant)>> =
            const { std::cell::RefCell::new(None) };
    }

    /// `symbol -> (outer acquisitions, total hold ns, max hold ns)`.
    fn tally() -> &'static Mutex<HashMap<String, (u64, u64, u64)>> {
        static T: OnceLock<Mutex<HashMap<String, (u64, u64, u64)>>> = OnceLock::new();
        T.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// An OUTER acquisition just completed at `loc` (the `resolve_lock_acquire`
    /// call site). Starts the span here — at the ACQUIRE — so the billed hold is
    /// the real serialized extent regardless of which site anchored it.
    pub(crate) fn note_outer_at(loc: &'static std::panic::Location<'static>) {
        let label = format!("{}:{}", loc.file(), loc.line());
        CUR.with(|c| *c.borrow_mut() = Some((label, std::time::Instant::now())));
    }

    /// The `jit_call` envelope refines its span's label with the callee symbol,
    /// keeping the acquire-time `t0` so the hold stays honest.
    pub(crate) fn refine_sym(sym: &str) {
        CUR.with(|c| {
            if let Some((label, _)) = c.borrow_mut().as_mut() {
                label.push(' ');
                label.push_str(sym);
            }
        });
    }

    /// The outer guard is releasing — bill the span to the site that opened it.
    pub(crate) fn record_hold() {
        let Some((sym, t0)) = CUR.with(|c| c.borrow_mut().take()) else { return };
        let ns = t0.elapsed().as_nanos() as u64;
        if let Ok(mut m) = tally().lock() {
            let e = m.entry(sym).or_insert((0, 0, 0));
            e.0 += 1;
            e.1 += ns;
            e.2 = e.2.max(ns);
        }
    }

    /// Top `n` anchors by TOTAL held time — the ranking that decides the boundary.
    pub(crate) fn report(n: usize) -> String {
        let Ok(m) = tally().lock() else { return String::new() };
        let mut v: Vec<_> = m.iter().map(|(k, &(c, t, mx))| (t, c, mx, k.clone())).collect();
        v.sort_unstable_by(|a, b| b.0.cmp(&a.0));
        v.truncate(n);
        v.iter()
            .map(|(t, c, mx, k)| {
                format!("{k} n={c} hold_ms={} max_ms={}", t / 1_000_000, mx / 1_000_000)
            })
            .collect::<Vec<_>>()
            .join("\n[LANE-OUTER]   ")
    }
}
