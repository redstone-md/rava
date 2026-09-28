//! Arena strings: allocation, hashing, the JIT-visible offsets and the intern pool.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::nextgen::*;
use crate::arena::ArenaPtr;
use std::collections::HashMap;
use std::sync::Arc;

/// The (lazily registered) class id for arena strings — empty layout ⇒ ref_mask 0.
pub fn string_class_id() -> u32 {
    static SID: OnceLock<u32> = OnceLock::new();
    *SID.get_or_init(|| {
        let name: Arc<str> = Arc::from("[arena-string]");
        let layout: Arc<[(u32, u8)]> = Arc::from(Vec::<(u32, u8)>::new());
        register_class(&name, &layout)
    })
}

#[inline]
pub fn is_arena_string(p: ArenaPtr) -> bool {
    unsafe { obj_class(p.as_ptr()) == string_class_id() }
}

/// ⭐**FAST STRINGS** — `RAVA_NO_FAST_STRINGS=1` opts out, and the block-wide
/// `RAVA_NO_EMIT_MORE` disables it with everything else.
///
/// ⭐**SIX WALKS OF THE BYTES TO PUBLISH TWO NUMBERS.** Every arena-string birth
/// stamped a cached `hashCode` and a cached `length` into the header, and computed
/// each of them the slow way and SEPARATELY:
///
/// ```text
///   hash:   from_utf8(bytes)          — full UTF-8 validation pass
///           .chars().fold(..)         — decoder state machine + multiply chain
///   clen:   from_utf8(bytes)          — the SAME validation pass, again
///           .chars().count()          — the SAME decode, again
/// ```
///
/// This is on `tlab_alloc_string`, which runs for every JIT concat result, every
/// `String.valueOf` promotion and every `Value::Str` that reaches an arena ref cell.
/// With this on both numbers come out of ONE pass (`hash_and_char_len`), because for
/// pure-ASCII text — field names, resource keys, class names, nearly every literal
/// in the boot's constant pools — the specified char fold IS the byte fold and the
/// char count IS the byte count, and ASCII-ness falls out of the same loop for free.
///
/// ⛔**AND WHAT THIS DELIBERATELY DOES NOT DO: make the hash LAZY.** That is the
/// bigger win — almost nothing ever asks a string for its hash, so nearly every fold
/// is dead — but `jit/emit/ops/invoke_virtual.rs` emits `String.hashCode()` on an
/// arena receiver as a BARE `load.i32` of `STR_JIT_HASH_OFF`, with no branch. A
/// not-yet-computed sentinel there would make compiled code return 0. The fast path
/// cannot prove its precondition, so it refuses: the cell stays eagerly valid, and
/// laziness waits for the emitter to grow a zero-test that falls to `env.str_hash_fn`.
///
/// Costs if removed: the six passes come back on every string birth.
pub(crate) fn fast_strings_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        crate::flags::fast_natives()
            && std::env::var_os("RAVA_NO_FAST_STRINGS").is_none()
            && !crate::flags::emit_more_disabled()
    })
}

/// Java `String.hashCode()` over the UTF-8 chars (matches `str_method` hashCode).
pub(crate) fn java_string_hash(bytes: &[u8]) -> i32 {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.chars().fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(c as i32)),
        Err(_) => bytes.iter().fold(0i32, |h, &b| h.wrapping_mul(31).wrapping_add(b as i32)),
    }
}

/// Both header-cached numbers — `(hashCode, length)` — in ONE pass over the bytes.
///
/// The ASCII arm is not an approximation, it is an identity: a byte below `0x80` IS
/// its own `char`, so the specified `h*31 + c` fold over chars is the byte fold, and
/// `chars().count()` is the byte count. The loop proves ASCII-ness while it folds —
/// `or` accumulates every byte's high bit on an independent chain, so it rides free
/// alongside the multiply chain that already dominates — which is why this is one
/// pass and not "is_ascii, then fold".
///
/// Anything with a byte >= `0x80` falls through to EXACTLY the two original
/// expressions, invalid-UTF-8 arm included, so the answer is bit-identical either way.
#[inline]
pub(crate) fn hash_and_char_len(bytes: &[u8]) -> (i32, usize) {
    let mut h = 0i32;
    let mut or = 0u8;
    for &b in bytes {
        h = h.wrapping_mul(31).wrapping_add(b as i32);
        or |= b;
    }
    if or < 0x80 {
        return (h, bytes.len());
    }
    (
        java_string_hash(bytes),
        std::str::from_utf8(bytes).map(|s| s.chars().count()).unwrap_or(bytes.len()),
    )
}

/// Bump-allocate an arena string holding `bytes` (UTF-8). `None` on a full nursery
/// (caller collects + retries, like `tlab_try_alloc`).
pub fn tlab_alloc_string(bytes: &[u8]) -> Option<ArenaPtr> {
    // G1.4 NO-ALLOC BISECTION GATE (string alloc path): abort before any segment write.
    if crate::parallel::gilfree() && crate::parallel::fork_debug_no_alloc() {
        crate::parallel::request_abort();
        return None;
    }
    let cid = string_class_id();
    let byte_len = bytes.len();
    let nf = (2 + (byte_len + 7) / 8) as u32; // blen/clen + hash cells + byte cells
    // Both header numbers from ONE pass (see `fast_strings_enabled`); the arm below
    // is the original pair of validate-then-decode expressions, kept for the A/B.
    let (jhash, char_len) = if fast_strings_enabled() {
        hash_and_char_len(bytes)
    } else {
        (
            java_string_hash(bytes),
            std::str::from_utf8(bytes).map(|s| s.chars().count()).unwrap_or(byte_len),
        )
    };
    crate::nextgen::tlab::with_tlab(|slot| {
        tlab_init_if_needed(slot);
        let h = slot.as_mut().unwrap();
        sync_young_from_cursor(h);
        let size = obj_size(nf);
        if h.young.top + size > h.young.cap() {
            // A GIL-free worker cannot collect while its siblings run. Refill
            // from the region-owned spill pool just like object allocation.
            if !crate::parallel::gilfree()
                || !crate::nextgen::tlab::refill_worker_tlab(h, size)
            {
                if crate::parallel::gilfree() {
                    crate::parallel::request_abort_overflow();
                }
                return None;
            }
        }
        let p = unsafe { h.young.base().add(h.young.top) };
        h.young.top += size;
        unsafe {
            *(p as *mut u64) = (nf as u64) << 32 | cid as u64;
            obj_init_hdr_tail(p);
            *(p.add(STR_BLEN_OFF) as *mut u32) = byte_len as u32;
            *(p.add(STR_CLEN_OFF) as *mut u32) = char_len as u32;
            *(p.add(STR_HASH_OFF) as *mut i32) = jhash;
            // bytes inline, zero-pad the final cell.
            let dst = p.add(STR_BYTES_OFF);
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, byte_len);
            let padded = (byte_len + 7) & !7;
            for i in byte_len..padded {
                *dst.add(i) = 0;
            }
        }
        refresh_cursor(h);
        crate::parallel::count_arena_alloc();
        Some(ArenaPtr::new(p).expect("tlab string non-null"))
    })
}

/// Byte length (storage size).
#[inline]
pub fn arena_str_len(p: ArenaPtr) -> usize {
    unsafe { *(p.as_ptr().add(STR_BLEN_OFF) as *const u32) as usize }
}
/// Char length (== Java `String.length()`).
#[inline]
pub fn arena_str_char_len(p: ArenaPtr) -> i32 {
    unsafe { *(p.as_ptr().add(STR_CLEN_OFF) as *const i32) }
}
/// The eagerly stamped Java `hashCode()`. A BARE LOAD, and it has to stay one:
/// `jit/emit/ops/invoke_virtual.rs` emits the same load inline for a compiled
/// `String.hashCode()` on an arena receiver, so this cell is always valid and never
/// carries a sentinel (see `fast_strings_enabled`).
#[inline]
pub fn arena_str_hash(p: ArenaPtr) -> i32 {
    unsafe { *(p.as_ptr().add(STR_HASH_OFF) as *const i32) }
}
/// The inline UTF-8 bytes (lifetime tied to the object; valid until the next GC).
#[inline]
pub fn arena_str_bytes<'a>(p: ArenaPtr) -> &'a [u8] {
    unsafe { std::slice::from_raw_parts(p.as_ptr().add(STR_BYTES_OFF), arena_str_len(p)) }
}

// ═══════════════ Permanent interned String constants (JIT LICM) ═══════════════
// A JIT `ldc "..."` resolved a String constant by ALLOCATING a fresh nursery
// ArenaString on EVERY execution (a per-iteration `rava_ldc` callback + nursery
// churn). String constants are immutable and loop-invariant, so we intern each
// ONCE into a dedicated process-lifetime LOW (<2^40) bump region and let the JIT
// bake its stable address as an `iconst` — true loop-invariant code motion with
// zero per-iteration callback or allocation.
//
// Soundness: the region is NOT any thread's nursery, so the Cheney collector
// leaves a baked pointer UNTOUCHED (`copy_obj` returns non-nursery ptrs as-is)
// and never traces its ref-mask-0 bytes; the layout is byte-identical to
// `tlab_alloc_string`, so every `is_arena_string` reader (length/hashCode/
// startsWith/str_of/display/map-key) treats it identically. The base is low so
// the tag ABI (`addr < 2^40` ⇒ arena ptr) holds.
pub(crate) struct PermPool {
    pub(crate) base: *mut u8,
    pub(crate) top: usize,
    pub(crate) cap: usize,
}
pub(crate) fn perm_pool() -> &'static Mutex<(PermPool, std::collections::HashMap<String, usize>)> {
    static P: OnceLock<Mutex<(PermPool, std::collections::HashMap<String, usize>)>> =
        OnceLock::new();
    P.get_or_init(|| {
        let cap = 16 * 1024 * 1024; // 16 MiB permanent string-constant pool
        let base = map_low_zeroed(cap);
        Mutex::new((PermPool { base, top: 0, cap }, std::collections::HashMap::new()))
    })
}

/// Intern `s` as a PERMANENT arena string (deduped by content); return its stable
/// address as an `i64` for the JIT to bake as an `iconst`. Returns 0 if the pool
/// is exhausted (caller falls back to the `rava_ldc` callback). Layout is
/// byte-identical to `tlab_alloc_string`.
pub fn intern_arena_string(s: &str) -> i64 {
    let m = perm_pool();
    let mut g = m.lock().unwrap();
    if let Some(&p) = g.1.get(s) {
        return p as i64;
    }
    let bytes = s.as_bytes();
    let byte_len = bytes.len();
    let nf = (2 + (byte_len + 7) / 8) as u32;
    let size = obj_size(nf);
    let cid = string_class_id();
    // Same one-pass derivation as `tlab_alloc_string`, so the two layouts stay
    // byte-identical (this path is cold — once per DISTINCT literal for the process
    // lifetime — so it is not gated; the values are the same either way).
    let (jhash, char_len) = hash_and_char_len(bytes);
    let (pool, map) = &mut *g;
    if pool.top + size > pool.cap {
        return 0; // pool exhausted → callback path
    }
    let p = unsafe { pool.base.add(pool.top) };
    pool.top += size;
    unsafe {
        *(p as *mut u64) = (nf as u64) << 32 | cid as u64;
        obj_init_hdr_tail(p);
        *(p.add(STR_BLEN_OFF) as *mut u32) = byte_len as u32;
        *(p.add(STR_CLEN_OFF) as *mut u32) = char_len as u32;
        *(p.add(STR_HASH_OFF) as *mut i32) = jhash;
        let dst = p.add(STR_BYTES_OFF);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, byte_len);
        let padded = (byte_len + 7) & !7;
        for i in byte_len..padded {
            *dst.add(i) = 0;
        }
    }
    map.insert(s.to_string(), p as usize);
    p as i64
}

// ═══════════════ ArenaLambda — a functional-interface closure on the arena ═══════════════
// A capturing lambda stored as a REGULAR arena object, so the moving Cheney collector
// copies + forwards it with the STANDARD per-class ref_mask — NO new per-object GC infra.
//
// LAYOUT (cells after the 24B HDR, `[obj + HDR + slot*8]`):
//   slot 0 = kind      — MethodHandle ref-kind (u8, stored as int) — NOT-ref
//   slot 1 = site_idx  — index into the content-interned LAMBDA_SITES table — NOT-ref
//   slot 2 = $fi       — functional-interface class-name String ref (or null) — ref
//   slot 3 = $cmp      — comparator marker String ref (or null) — ref
//   slot 4.. = captured[i], one cell each, encoded by `encode_arena_cell`
//
// THE KEY SOUNDNESS PROPERTY (audit B1): a lambda SITE's capture signature is
// STATICALLY FIXED — the invokedynamic descriptor's parameter types ARE the captured
// arg types. We register a DISTINCT arena class per capture-KIND signature, whose
// per-class `ref_mask` (register_class derives it from cell kinds: `kind>=4` ⇒ ref) is
// therefore correct for every instance of that class:
//   • a REFERENCE capture (descriptor `L…`/`[`) ⇒ kind 4 ⇒ ref_mask bit SET ⇒ the
//     collector traces + forwards it (holding the only ref to a moving-arena object
//     keeps it alive), and the existing arena ref-cell code already handles a ref
//     slot carrying a raw 0b00 arena ptr, a 0b01 handle, or a 0b11 boxed prim/String;
//   • a PRIMITIVE capture (`I/J/F/D/Z/B/C/S`) ⇒ kind 0-3 ⇒ ref_mask bit CLEAR ⇒ the
//     collector SKIPS it, so a `long`/`double` whose bit pattern aliases a young
//     arena address is never mis-forwarded (the CHENEY-MISS the audit feared).
// This is exactly the arena-string / arena-enum pattern (a static per-class mask), so
// the feared "new per-object capture-kind bitmap the Cheney trace consults" is NOT
// needed. This module proves that (see `arena_lambda_survives_collection`).
//
// IDENTITY (audit B2): the (owner,name,descriptor) call-site is carried by an integer
// index into a process-lifetime CONTENT-interned table (`LAMBDA_SITES`), NOT by the
// recyclable arena address — so a swept+reused address can never alias another site's
// identity (the opcodes.rs:352 dedup hazard).
/// Encode `s` as an arena cell whose bit pattern is valid for the PROCESS LIFETIME —
/// the requirement for caching a cell value across collections. Prefers the permanent
/// interned arena-string pool (0b00, GC-immune, content-deduped) when string ref-cells
/// are enabled, and otherwise the append-only box side table (0b11, index never reused).
pub(crate) fn stable_string_cell(s: &str) -> i64 {
    if arena_str_refcell_enabled() {
        let p = intern_arena_string(s);
        if p != 0 {
            return p;
        }
    }
    ((arena_box_push(crate::value::Value::Str(s.into())) as i64) << 2) | 3
}
#[cfg(test)]
mod tests {
    use super::*;

    struct SpillTestCleanup;

    impl Drop for SpillTestCleanup {
        fn drop(&mut self) {
            crate::parallel::set_gilfree(false);
            crate::nextgen::worker_tlab_clear();
            crate::parallel::clear_abort();
            crate::parallel::spill_clear();
        }
    }

    #[test]
    fn worker_string_allocation_refills_from_region_spill_pool() {
        let mut memory = vec![0u64; 1024];
        let base = memory.as_mut_ptr() as usize;
        let spill_base = base + 16;
        let _cleanup = SpillTestCleanup;

        crate::parallel::spill_setup(spill_base, 4096, 4096);
        crate::parallel::set_gilfree(true);
        crate::nextgen::worker_tlab_view(base, 16);

        let string = tlab_alloc_string(b"spill").expect("reserved spill chunk should fit string");
        let string_addr = string.as_ptr() as usize;
        assert!((spill_base..spill_base + 4096).contains(&string_addr));
        assert!(is_arena_string(string));
        unsafe {
            let byte_len = *(string.as_ptr().add(STR_BLEN_OFF) as *const u32) as usize;
            let bytes =
                std::slice::from_raw_parts(string.as_ptr().add(STR_BYTES_OFF), byte_len);
            assert_eq!(bytes, b"spill");
        }
        assert_eq!(crate::parallel::spill_stats().0, 1);
    }
}
