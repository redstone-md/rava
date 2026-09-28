//! Reserving and committing the low address range the arena lives in.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::nextgen::*;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

/// Recycled reserved-VA spans (ptr, span, committed), keyed by span at the reuse site. Forks are
/// issued serially under the GIL, so a plain Mutex suffices. The VA stays reserved (never unmapped)
/// while parked here. `committed` records how many bytes of the parked span are still page-backed:
/// with RAVA_ARENA_DECOMMIT the pages are returned to the OS at park time (committed=0, the RSS
/// lever — a parked from-space otherwise retains its full high-water forever); without the gate the
/// pages persist and `committed` keeps the accounting honest for `arena_committed_bytes`. The
/// tenured copy paths write before they read, so both a persisted and a freshly-recommitted
/// (zero-filled) page are equally sound — no wipe is needed either way.
pub(crate) static RESERVED_POOL: std::sync::Mutex<Vec<(usize, usize, usize)>> = std::sync::Mutex::new(Vec::new());

pub(crate) fn reserve_low_pooled(size: usize) -> (*mut u8, usize, usize) {
    let gran = 64 * 1024u64;
    let span = (((size as u64).max(1) + gran - 1) & !(gran - 1)) as usize;
    if let Ok(mut pool) = RESERVED_POOL.lock() {
        if let Some(i) = pool.iter().position(|&(_, s, _)| s == span) {
            let (p, s, c) = pool.swap_remove(i);
            return (p as *mut u8, s, c);
        }
    }
    (map_low_reserved(size), span, 0)
}

/// Next low-arena base to try. Starts at 8 GiB — above the usual low-VA clutter
/// (program image, default mmaps), far below 2^40 (1 TiB). ~1016 GiB of low space
/// remains for all threads' nursery+tenured regions.
pub(crate) static NEXT_LOW: AtomicU64 = AtomicU64::new(8 * 1024 * 1024 * 1024);
pub(crate) const LOW_CEILING: u64 = 1 << 40;

/// Reserve + commit `size` zeroed bytes at a base address `< 2^40`. Fails LOUDLY
/// (process exit) if no low address is available: a high fallback would silently
/// corrupt the GC root scan (the discriminator would misread pointers as handles),
/// so we refuse it rather than continue unsound.
pub(crate) fn map_low_zeroed(size: usize) -> *mut u8 {
    map_low(size, true)
}

/// Reserve low VA WITHOUT committing it (see `Region::new_reserved`). On Windows the
/// pages are committed later by `commit_pages`; on Linux the mmap is demand-paged so
/// this is identical to the eager map (page faults back it lazily).
pub(crate) fn map_low_reserved(size: usize) -> *mut u8 {
    map_low(size, false)
}

pub(crate) fn map_low(size: usize, commit: bool) -> *mut u8 {
    let gran: u64 = 64 * 1024; // Windows allocation granularity (page-safe on unix too)
    let span = ((size as u64).max(1) + gran - 1) & !(gran - 1);
    loop {
        let base = NEXT_LOW.fetch_add(span, Ordering::SeqCst);
        if base + span > LOW_CEILING {
            eprintln!(
                "rava[next-gen-gc]: low-arena VA space (<2^40) exhausted requesting {size}B \
                 (cursor 0x{base:x}). Hard exit — a high arena base breaks the ABI discriminator."
            );
            std::process::exit(3);
        }
        if let Some(p) = try_map_at(base, span as usize, commit) {
            assert!(
                (p as u64) < LOW_CEILING && (p as u64) & 7 == 0,
                "low arena map returned an unaligned/high base 0x{:x}",
                p as u64
            );
            return p;
        }
        // `base` was busy → loop; the cursor has already advanced past it.
    }
}

/// Initial committed bytes for a freshly reserved region. Windows commits nothing
/// upfront (lazy via `ensure`); Linux's demand-paged mmap is effectively fully
/// committed for free, so report the whole cap and let `ensure` no-op.
#[cfg(windows)]
pub(crate) fn commit_floor(_cap: usize) -> usize { 0 }
#[cfg(not(windows))]
pub(crate) fn commit_floor(cap: usize) -> usize { cap }

/// Commit `[ptr, ptr+len)` so writes there are backed. Windows-only; on other
/// platforms the reserved mapping is already demand-paged.
#[cfg(windows)]
pub(crate) fn commit_pages(ptr: *mut u8, len: usize) {
    use std::os::raw::c_void;
    const MEM_COMMIT: u32 = 0x1000;
    const PAGE_READWRITE: u32 = 0x04;
    extern "system" {
        fn VirtualAlloc(addr: *mut c_void, size: usize, typ: u32, protect: u32) -> *mut c_void;
    }
    if len == 0 {
        return;
    }
    let p = unsafe { VirtualAlloc(ptr as *mut c_void, len, MEM_COMMIT, PAGE_READWRITE) };
    assert!(!p.is_null(), "rava[next-gen-gc]: arena commit of {len}B failed (RAVA_PROC_CAP_MB?)");
}
#[cfg(not(windows))]
pub(crate) fn commit_pages(_ptr: *mut u8, _len: usize) {}

/// Return `[ptr, ptr+len)`'s physical pages to the OS while KEEPING the VA reserved
/// (`MEM_DECOMMIT`, the exact inverse of `commit_pages`). Windows-only lever: on Linux the
/// demand-paged mmap has no commit charge to return (madvise(DONTNEED) would be the analog —
/// not needed while the RSS gap is a Windows measurement).
#[cfg(windows)]
pub(crate) fn decommit_pages(ptr: *mut u8, len: usize) {
    use std::os::raw::c_void;
    const MEM_DECOMMIT: u32 = 0x4000;
    extern "system" {
        fn VirtualFree(addr: *mut c_void, size: usize, typ: u32) -> i32;
    }
    if len == 0 {
        return;
    }
    // Failure is benign (pages simply stay resident) — never abort over an RSS optimisation.
    unsafe { VirtualFree(ptr as *mut c_void, len, MEM_DECOMMIT) };
}
#[cfg(not(windows))]
pub(crate) fn decommit_pages(_ptr: *mut u8, _len: usize) {}

#[cfg(windows)]
pub(crate) fn try_map_at(base: u64, size: usize, commit: bool) -> Option<*mut u8> {
    use std::os::raw::c_void;
    const MEM_COMMIT: u32 = 0x1000;
    const MEM_RESERVE: u32 = 0x2000;
    const PAGE_READWRITE: u32 = 0x04;
    extern "system" {
        fn VirtualAlloc(addr: *mut c_void, size: usize, typ: u32, protect: u32) -> *mut c_void;
    }
    // VirtualAlloc with a non-null base returns THAT base (committed→zero-filled, or
    // merely reserved) or NULL if the range is unavailable — it never relocates, so
    // the result is guaranteed low when the requested base is low.
    let typ = if commit { MEM_COMMIT | MEM_RESERVE } else { MEM_RESERVE };
    let p = unsafe { VirtualAlloc(base as *mut c_void, size, typ, PAGE_READWRITE) };
    if p.is_null() {
        None
    } else {
        Some(p as *mut u8)
    }
}

#[cfg(windows)]
pub(crate) unsafe fn unmap_low(ptr: *mut u8, _len: usize) {
    use std::os::raw::c_void;
    const MEM_RELEASE: u32 = 0x8000;
    extern "system" {
        fn VirtualFree(addr: *mut c_void, size: usize, typ: u32) -> i32;
    }
    VirtualFree(ptr as *mut c_void, 0, MEM_RELEASE);
}

#[cfg(target_os = "linux")]
pub(crate) fn try_map_at(base: u64, size: usize, _commit: bool) -> Option<*mut u8> {
    use std::os::raw::c_void;
    const PROT_RW: i32 = 0x1 | 0x2;
    const MAP_PRIVATE: i32 = 0x02;
    const MAP_ANONYMOUS: i32 = 0x20;
    const MAP_FIXED_NOREPLACE: i32 = 0x10_0000; // Linux 4.17+: honour base or fail
    extern "C" {
        fn mmap(addr: *mut c_void, len: usize, prot: i32, flags: i32, fd: i32, off: i64)
            -> *mut c_void;
        fn munmap(addr: *mut c_void, len: usize) -> i32;
    }
    let p = unsafe {
        mmap(
            base as *mut c_void,
            size,
            PROT_RW,
            MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE,
            -1,
            0,
        )
    };
    if p as isize == -1 {
        return None;
    }
    if p as u64 != base {
        unsafe { munmap(p, size) };
        return None;
    }
    Some(p as *mut u8) // MAP_ANONYMOUS pages are zero-filled
}

#[cfg(target_os = "linux")]
pub(crate) unsafe fn unmap_low(ptr: *mut u8, len: usize) {
    use std::os::raw::c_void;
    extern "C" {
        fn munmap(addr: *mut c_void, len: usize) -> i32;
    }
    munmap(ptr as *mut c_void, len);
}

// Portable fallback (non-Windows, non-Linux): a leaked boxed allocation. It can't
// honour a base hint, so soundness rests on the `map_low_zeroed` assert — if the
// allocator hands back a high address the process exits rather than corrupting GC.
#[cfg(not(any(windows, target_os = "linux")))]
pub(crate) fn try_map_at(_base: u64, size: usize, _commit: bool) -> Option<*mut u8> {
    let b = vec![0u8; size].into_boxed_slice();
    let p = Box::into_raw(b) as *mut u8;
    if (p as u64) < LOW_CEILING {
        Some(p)
    } else {
        // High base — reclaim and signal failure; the loop will exhaust + exit.
        unsafe { drop(Box::from_raw(std::slice::from_raw_parts_mut(p, size))) };
        None
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
pub(crate) unsafe fn unmap_low(ptr: *mut u8, len: usize) {
    drop(Box::from_raw(std::slice::from_raw_parts_mut(ptr, len)));
}
