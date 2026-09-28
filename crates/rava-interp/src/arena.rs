//! `ArenaPtr` — Phase-1 foundation for the handle→raw-pointer migration
//! (`rnd-logs/rava-endgame-migration.md`).
//!
//! The endgame replaces `Value::Ref(u32)` (an index into `Heap::objects`) with a
//! direct pointer to an arena-allocated, inline-cell object — the model the
//! sandbox proved reaches 0.87x JVM. `ArenaPtr` is that pointer, wrapped so it can
//! live inside `Value` (which MUST stay `Send`: rava shares the heap across real
//! OS threads under the GIL — see `frames::all`, the per-thread box tables).
//!
//! # Safety contract (the GIL safepoint)
//! A raw `*mut` is normally `!Send`. `ArenaPtr` asserts `Send + Sync` UNCONDITION-
//! ALLY, sound only under rava's existing execution model:
//!   1. Only the GIL lock-holder runs bytecode / dereferences object pointers.
//!   2. The moving GC relocates objects ONLY at a safepoint, where every other
//!      thread is parked at `gil::acquire` with its frames alive + unmutated.
//!   3. At that safepoint, `forward_roots()` rewrites EVERY `ArenaPtr` in every
//!      parked thread's frames/locals/JIT cells (the root set proven complete by
//!      the live shadow run: 0 mismatches over 16+ GC cycles).
//! So an `ArenaPtr` is only ever dereferenced by the thread holding the GIL, and is
//! never stale across a collection — the same invariant that already makes the
//! handle model sound, expressed for pointers. A bare `*mut` carries none of this,
//! so the wrapper localises the `unsafe impl` to one audited place.

use std::ptr::NonNull;

/// A non-null pointer to an arena object header (see `nextgen::ObjHeader`).
#[derive(Clone, Copy)]
pub struct ArenaPtr(NonNull<u8>);

// SAFETY: see the module-level "GIL safepoint" contract. Dereference happens only
// under the GIL; the moving GC forwards every ArenaPtr at a stop-the-world
// safepoint, so no thread ever observes a stale pointer.
unsafe impl Send for ArenaPtr {}
unsafe impl Sync for ArenaPtr {}

impl ArenaPtr {
    /// Wrap a raw arena pointer. `None` if null.
    #[inline]
    pub fn new(p: *mut u8) -> Option<Self> {
        NonNull::new(p).map(ArenaPtr)
    }

    /// Reconstruct from a raw address (deserialize / forwarding). Caller guarantees
    /// non-zero + arena-valid.
    ///
    /// # Safety
    /// `addr` must be a non-null pointer into a live arena region.
    #[inline]
    pub unsafe fn from_addr(addr: u64) -> Self {
        ArenaPtr(NonNull::new_unchecked(addr as *mut u8))
    }

    #[inline]
    pub fn as_ptr(self) -> *mut u8 {
        self.0.as_ptr()
    }

    #[inline]
    pub fn addr(self) -> u64 {
        self.0.as_ptr() as u64
    }
}

impl std::fmt::Debug for ArenaPtr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ArenaPtr(0x{:x})", self.addr())
    }
}
impl PartialEq for ArenaPtr {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl Eq for ArenaPtr {}

// Serialize/Deserialize so the `Value` derives compile. DORMANT: no live
// `Value::Obj` exists until the heap migration (Phase 2), and snapshots are
// forbidden, so these paths never actually run — they round-trip the address.
impl serde::Serialize for ArenaPtr {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(self.addr())
    }
}
impl<'de> serde::Deserialize<'de> for ArenaPtr {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let a = <u64 as serde::Deserialize>::deserialize(d)?;
        // max(1) keeps NonNull's invariant for a dormant/never-deref'd value.
        Ok(unsafe { ArenaPtr::from_addr(a.max(1)) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}

    #[test]
    fn arenaptr_is_send_and_sync() {
        // Compile-time proof the Send-wall is broken — these fail to compile if the
        // unsafe impls are removed.
        assert_send::<ArenaPtr>();
        assert_sync::<ArenaPtr>();
    }

    #[test]
    fn arenaptr_crosses_thread_boundary() {
        // Moving an ArenaPtr INTO a spawned thread requires `Send` — this closure
        // would not compile without the unsafe impl. Read the address on the other
        // thread (never deref without the GIL); prove it survives the move.
        let mut buf = [0u8; 64];
        let p = ArenaPtr::new(buf.as_mut_ptr()).unwrap();
        let addr = p.addr();
        let got = std::thread::spawn(move || p.addr()).join().unwrap();
        assert_eq!(got, addr);
    }

    #[test]
    fn arenaptr_addr_roundtrip_and_eq() {
        let mut x = 42u64;
        let raw = &mut x as *mut u64 as *mut u8;
        let p = ArenaPtr::new(raw).unwrap();
        assert_eq!(p.addr(), raw as u64);
        let p2 = unsafe { ArenaPtr::from_addr(p.addr()) };
        assert_eq!(p, p2);
        assert_eq!(p.as_ptr(), raw);
    }

    #[test]
    fn arenaptr_null_is_none() {
        assert!(ArenaPtr::new(std::ptr::null_mut()).is_none());
    }

    #[test]
    fn arenaptr_shared_across_threads_sync() {
        // &ArenaPtr shared across threads requires `Sync`. Each reader sees the same
        // address (no deref — models the GIL read contract).
        let mut buf = [0u8; 64];
        let p = ArenaPtr::new(buf.as_mut_ptr()).unwrap();
        std::thread::scope(|s| {
            let pr = &p;
            for _ in 0..4 {
                s.spawn(move || assert_eq!(pr.addr(), p.addr()));
            }
        });
    }
}
