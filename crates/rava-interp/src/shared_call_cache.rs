//! Static / invokespecial resolutions, published once and readable by every thread.
//!
//! ## The problem this solves
//!
//! `jit_direct_resolve` builds a compiled→compiled DIRECT entry for a `kind 0` (static) or
//! `kind 2` (invokespecial) site from `tlcache::call_get(site)` — and `tlcache` is, as the name
//! says, THREAD-LOCAL. A gil-free ring worker has never run the interpreter on that site, so its
//! map is empty, the direct entry is never built, and every such call falls into the `jit_call`
//! gauntlet. Off-GIL that means taking the global dispatch lock. No amount of warming on main
//! helps, because main's warmth is in main's map.
//!
//! It is not a small residue. On a completing ring region the locked-callee census reads:
//!
//! ```text
//! DensityFunction$SinglePointContext#<init>  360657     <- invokespecial
//! DensityFunctions$Spline$Point#<init>       360655     <- invokespecial
//! Climate#target                             360651     <- invokestatic
//! ```
//!
//! Three guest callees, ~1.08 M of the region's 2.27 M locked calls, none of them rejected by any
//! of `dispatch_resolve_cacheable_cls`'s exclusions. They are locked purely because the resolution
//! that would have made them direct lives on another thread.
//!
//! ## Why publishing is sound
//!
//! A static or invokespecial site resolves from its SYMBOLIC REFERENCE — the constant-pool entry
//! at `(caller class, cp)` — not from any receiver. `opcodes::invokestatic` and
//! `opcodes::invokespecial` already cache that result per-thread FOREVER, with no invalidation, so
//! the engine has always treated it as process-permanent. Publishing it to a shared table asserts
//! exactly the same thing, to more readers.
//!
//! The stored value is a raw `*const LoadedClass`. That is sound because the loader's class cache
//! is INSERT-ONLY — `ClassLoader::cache` is never removed from or cleared (only `missing` and
//! `super_chain_cache` are), so an `Arc<LoadedClass>` it has handed out lives for the process.
//!
//! ## Shape
//!
//! Open-addressing, fixed capacity, append-only, lock-free — deliberately the same shape as
//! `jit::freeze_resolve`, which solved the same problem for VIRTUAL sites. A slot is claimed by
//! CAS on the site word and published site-word-LAST with `Release`, so a reader either sees a
//! complete entry or nothing at all.

use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::OnceLock;

use crate::class_loader::LoadedClass;

const CAP: usize = 1 << 16;
const MASK: usize = CAP - 1;
const MAX_PROBE: usize = 32;
/// Claim marker in the site word. Real site keys carry a heap pointer, so they are never 1.
const BUSY: i64 = 1;
/// `decl` value meaning "this site resolves to NOTHING" — a real resolution of `None`, which is
/// different from "not published yet" (0) and must be cached just as firmly.
const RESOLVED_NONE: usize = usize::MAX;

struct Slot {
    site: AtomicI64,
    decl: AtomicUsize,
}

fn table() -> &'static [Slot] {
    static T: OnceLock<Box<[Slot]>> = OnceLock::new();
    T.get_or_init(|| {
        (0..CAP)
            .map(|_| Slot { site: AtomicI64::new(0), decl: AtomicUsize::new(0) })
            .collect::<Vec<_>>()
            .into_boxed_slice()
    })
}

/// Pack `(caller class ptr, cp)` the same way `freeze_resolve` does: caller pointers are heap
/// addresses well under 2^48, so the cp index rides the top bits without colliding.
fn key(site: (usize, u16)) -> i64 {
    (site.0 as i64) | ((site.1 as i64) << 48)
}

fn slot_of(k: i64) -> usize {
    // Multiplicative hash — the low bits of a site key are an allocation address and a small cp,
    // both poorly distributed on their own.
    ((k as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32) as usize & MASK
}

/// Publish a static/invokespecial resolution. Called only from the interpreter's own `call_put`
/// path, i.e. on a thread that legitimately resolved it.
pub(crate) fn publish(site: (usize, u16), decl: Option<&Arc<LoadedClass>>) {
    if !enabled() {
        return;
    }
    let k = key(site);
    if k == 0 || k == BUSY {
        return;
    }
    let val = match decl {
        Some(a) => Arc::as_ptr(a) as usize,
        None => RESOLVED_NONE,
    };
    let t = table();
    let mut i = slot_of(k);
    for _ in 0..MAX_PROBE {
        let s = &t[i & MASK];
        let cur = s.site.load(Ordering::Acquire);
        if cur == k {
            // Already published. A second publication of the same site must agree — the
            // resolution is a function of the symbolic ref — so there is nothing to do.
            return;
        }
        if cur == 0
            && s.site
                .compare_exchange(0, BUSY, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            s.decl.store(val, Ordering::Release);
            // Site word LAST: a reader that sees the key is guaranteed to see the value.
            s.site.store(k, Ordering::Release);
            return;
        }
        i = i.wrapping_add(1);
    }
    // Full after MAX_PROBE. Dropping the entry costs a locked dispatch, never correctness.
}

/// The published resolution for a static/invokespecial site, if any.
///
/// `Some(Some(class))` = resolves to that class, `Some(None)` = resolves to nothing (cached
/// negative), `None` = never published.
pub(crate) fn get(site: (usize, u16)) -> Option<Option<Arc<LoadedClass>>> {
    if !enabled() {
        return None;
    }
    let k = key(site);
    let t = table();
    let mut i = slot_of(k);
    for _ in 0..MAX_PROBE {
        let s = &t[i & MASK];
        let cur = s.site.load(Ordering::Acquire);
        if cur == k {
            let v = s.decl.load(Ordering::Acquire);
            if v == 0 {
                return None; // claimed but not yet published
            }
            if v == RESOLVED_NONE {
                return Some(None);
            }
            // SAFETY: the loader's class cache is insert-only, so the `Arc<LoadedClass>` whose
            // pointer was published is still alive. Bump the count and rebuild an owned handle.
            let p = v as *const LoadedClass;
            return Some(Some(unsafe {
                Arc::increment_strong_count(p);
                Arc::from_raw(p)
            }));
        }
        if cur == 0 {
            return None; // a never-claimed slot ends the probe run
        }
        i = i.wrapping_add(1);
    }
    None
}

/// `RAVA_NO_SHARED_CALL_CACHE` disables publication and lookup, restoring the strictly
/// thread-local behaviour.
pub(crate) fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_SHARED_CALL_CACHE").is_none())
}
