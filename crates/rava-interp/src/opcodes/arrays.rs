//! Array work whose cost grows with ELEMENTS when it should not.
//!
//! Two things live here, and they are the same defect wearing two hats: a question whose answer
//! is a constant of the ARRAY (its storage kind, its element descriptor) being re-asked once per
//! ELEMENT.
//!
//!  * [`bulk_arraycopy`] — `System.arraycopy` was a per-element `array_get`/`array_set` round
//!    trip. Both of those re-derive the handle, the storage kind and the bounds from scratch, and
//!    the value crosses a `Value` in between. That is O(elements) × (2 handle-table lookups + 2
//!    storage-kind switches + a box/unbox) for a copy whose shape is fixed before the first
//!    element. It is now O(1) lookups + ONE `memmove`.
//!  * [`astore_memo`] — the JVMS 6.5 `aastore` store check walks the value's supertype closure
//!    (`name_is_subtype_tri`: a BFS that allocates a `String` per node and a `HashSet<String>`
//!    per call) once per ELEMENT STORED, for an answer that depends only on the pair (array
//!    element descriptor, value's dynamic class). `checkcast` next door has had a memo for this
//!    exact reason since it was written; `aastore` never got one.
//!
//! ⭐**RAVA_NO_FAST_OPS=1 restores the pre-round behaviour verbatim** — both entry points then
//! answer by running the caller's own slow path, so nothing here can be the difference.

use crate::heap::{ArrayData, Heap, DEAD_ARRAY_ELEMENT_ID};
use crate::opcodes::casts::Tri;
use crate::opcodes::*;

/// ⛔**NOT a `OnceLock`, for the reason `heap::arraytypes::fast_arrays` spells out**: a
/// `get_or_init` is an *acquire* load plus a branch LLVM may refuse to hoist out of a loop, and
/// this is consulted once per `aastore` and once per `arraycopy`. A relaxed `AtomicU8` on x86 is a
/// plain `mov` from a permanently L1-resident static and the branch predicts perfectly.
/// **Table size: 1 byte.**
static FAST_OPS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(2);

#[inline(always)]
fn fast_ops() -> bool {
    match FAST_OPS.load(std::sync::atomic::Ordering::Relaxed) {
        1 => true,
        0 => false,
        _ => fast_ops_init(),
    }
}

/// Read the environment once, off the hot path. `emit_more_disabled()` is consulted because this
/// belongs to the same "emit more, cross less" block and has to fall with it.
#[cold]
#[inline(never)]
fn fast_ops_init() -> bool {
    let on = std::env::var_os("RAVA_NO_FAST_OPS").is_none() && !crate::flags::emit_more_disabled();
    FAST_OPS.store(on as u8, std::sync::atomic::Ordering::Relaxed);
    on
}
pub mod arraycopy_census {
    use std::sync::atomic::{AtomicU64, Ordering};
    pub static ATTEMPTS: AtomicU64 = AtomicU64::new(0);
    pub static BYTES: AtomicU64 = AtomicU64::new(0);
    pub static FALLBACK: AtomicU64 = AtomicU64::new(0);
    pub(crate) fn bump_attempt(len: usize) {
        ensure_printer();
        ATTEMPTS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(len as u64, Ordering::Relaxed);
    }
    pub(crate) fn bump_fallback() {
        ensure_printer();
        FALLBACK.fetch_add(1, Ordering::Relaxed);
    }
    fn ensure_printer() {
        static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        ONCE.get_or_init(|| {
            if std::env::var_os("RAVA_ARRAYCOPY_CENSUS").is_none() { return; }
            std::thread::Builder::new().name("arraycopy-printer".into()).spawn(|| loop {
                std::thread::sleep(std::time::Duration::from_secs(5));
                let s = report_inner();
                if !s.is_empty() { eprintln!("{s}"); }
            }).ok();
        });
    }
    fn report_inner() -> String {
        let a = ATTEMPTS.load(Ordering::Relaxed);
        let b = BYTES.load(Ordering::Relaxed);
        let f = FALLBACK.load(Ordering::Relaxed);
        if a == 0 && f == 0 { return String::new(); }
        format!("[ARRAYCOPY] bulk attempts {a} bytes {b} | fallback {f}\n")
    }
    pub fn report() -> String {
        if std::env::var_os("RAVA_ARRAYCOPY_CENSUS").is_none() { return String::new(); }
        return report_inner();
    }
}

// ======================================================================

/// Bulk `System.arraycopy`. `true` = the copy is DONE; `false` = refused, run the element loop.
///
/// ⭐**WAS O(elements) × (two `self.arrays` lookups + two `ArrayData` discriminant switches + a
/// `Value` construction and destruction); IS O(1) lookups + one `copy_from_slice`/`copy_within`.**
/// The thing that grows is ELEMENTS COPIED — `StringBuilder`'s `char[]`, every `ArrayList` grow,
/// every `HashMap` resize, every IO `byte[]`, every NBT payload. The storage kind and the bounds
/// are constants of the two handles, decided once here instead of per element.
///
/// ⚠**IT REPRODUCES THE ELEMENT LOOP'S LENIENCY EXACTLY, INCLUDING ITS BUGS.** The loop it
/// replaces raises nothing: `array_get` yields `None` past the source end (the element is skipped)
/// and `array_set` is a silent no-op past the destination end. Both conditions are PREFIX
/// conditions in `i`, so the set of positions the loop actually writes is exactly the prefix
/// `0..n` computed below — which is what this copies. Making it throw the `IndexOutOfBounds` /
/// `ArrayStore` / `NullPointerException` that JVMS §System.arraycopy requires is a SEPARATE
/// change with its own differential probe; a speed round must not smuggle it in.
///
pub(crate) fn bulk_arraycopy(
    heap: &mut Heap,
    src: u32,
    sp: usize,
    dst: u32,
    dp: usize,
    len: usize,
) -> bool {
    if !fast_ops() {
        return false;
    }
    // The per-element `array_set` reports every store to the M3 biome write census. One bulk
    // memmove cannot, so when that diagnostic is armed the slow loop is the correct answer.
    #[cfg(feature = "next-gen-gc")]
    if crate::m3biome::enabled() {
        return false;
    }
    let (su, du) = (src as usize, dst as usize);
    let arrays = heap.arrays.read().unwrap_or_else(|e| e.into_inner());
    let (Some(sa), Some(da)) = (arrays.get(su), arrays.get(du)) else {
        return false;
    };
    // Same storage kind or nothing: the element loop converts through `Value`, so an `int[]` into
    // a `long[]` would WIDEN. That never reaches here from a verified `System.arraycopy`, but the
    // raw path must not be the thing that assumes it.
    if std::mem::discriminant(&sa.data) != std::mem::discriminant(&da.data) {
        return false;
    }
    // `sp`/`dp`/`len` arrive as `i32 as usize`, so a negative argument is a huge value and
    // saturates to a zero-length copy — the same nothing the element loop does with it.
    let n = len
        .min(sa.data.len().saturating_sub(sp))
        .min(da.data.len().saturating_sub(dp));
    drop(arrays);
    if n == 0 {
        return true;
    }
    if su == du {
        let mut arrays = heap.arrays.write().unwrap_or_else(|e| e.into_inner());
        let Some(a) = arrays.get_mut(su) else { return false };
        return copy_within(&mut a.data, sp, dp, n);
    }
    // Two DISTINCT indices of one `Vec`, so the borrow splits at the HIGHER of them: whichever
    // half holds the destination, the source is in the other. No `unsafe` — `split_at_mut` is
    // exactly the primitive for "two disjoint elements of one slice".
    let hi = su.max(du);
    let mut arrays = heap.arrays.write().unwrap_or_else(|e| e.into_inner());
    let (left, right) = arrays.split_at_mut(hi);
    if su < du {
        let s = &left[su];
        let d = &mut right[0];
        copy_between(&s.data, &mut d.data, sp, dp, n)
    } else {
        let d = &mut left[du];
        let s = &right[0];
        copy_between(&s.data, &mut d.data, sp, dp, n)
    }
}

/// `memmove` inside one array. The ranges are already proven in range by `n`.
fn copy_within(d: &mut ArrayData, sp: usize, dp: usize, n: usize) -> bool {
    match d {
        ArrayData::Byte(v) => v.copy_within(sp..sp + n, dp),
        ArrayData::Int(v) => v.copy_within(sp..sp + n, dp),
        ArrayData::Long(v) => v.copy_within(sp..sp + n, dp),
        ArrayData::Float(v) => v.copy_within(sp..sp + n, dp),
        ArrayData::Double(v) => v.copy_within(sp..sp + n, dp),
        // Flat tagged cells — see `copy_between` for why the raw `i64` is the whole value.
        ArrayData::Ref(v) => v.copy_within(sp..sp + n, dp),
    }
    true
}

/// `memcpy` between two arrays of the same storage kind. Ranges already proven by `n`.
fn copy_between(s: &ArrayData, d: &mut ArrayData, sp: usize, dp: usize, n: usize) -> bool {
    match (s, d) {
        (ArrayData::Byte(s), ArrayData::Byte(d)) => d[dp..dp + n].copy_from_slice(&s[sp..sp + n]),
        (ArrayData::Int(s), ArrayData::Int(d)) => d[dp..dp + n].copy_from_slice(&s[sp..sp + n]),
        (ArrayData::Long(s), ArrayData::Long(d)) => d[dp..dp + n].copy_from_slice(&s[sp..sp + n]),
        (ArrayData::Float(s), ArrayData::Float(d)) => d[dp..dp + n].copy_from_slice(&s[sp..sp + n]),
        (ArrayData::Double(s), ArrayData::Double(d)) => {
            d[dp..dp + n].copy_from_slice(&s[sp..sp + n])
        }
        // ⭐**A REFERENCE CELL IS COPYABLE AS ITS RAW `i64`, AND THAT IS NOT A SHORTCUT.**
        // `flat_ref_cell` already encodes `Ref`/`Array`/`Null`/arena-`Obj` entirely inside the
        // cell, so for those the round trip through `decode_ref_cell` → `Value` →
        // `flat_ref_cell` is the identity. The one remaining shape — an `ABOX_TAG` index into
        // `Heap::array_box` (a `Str` or a boxed primitive) — decodes by CLONING the boxed value
        // out and re-encodes by APPENDING an equal one, so the two arrays end up holding equal
        // values either way; sharing the index instead is safe because `array_box` is
        // append-only and never mutated or swept in place (see `Heap::array_box`). The only
        // observable difference is that the shared form does NOT append a box slot, so
        // `approx_bytes()` no longer charges for a copy that allocates nothing — the GC trigger
        // gets MORE accurate, not less.
        (ArrayData::Ref(s), ArrayData::Ref(d)) => d[dp..dp + n].copy_from_slice(&s[sp..sp + n]),
        // Unreachable: the discriminants were compared before `n` was computed. Refuse rather
        // than assume, so a storage kind added later falls back instead of silently doing nothing.
        _ => return false,
    }
    true
}

// ======================================================================
// aastore store check — memoised on the pair that decides it.
// ======================================================================

/// Direct-mapped memo for the `aastore` store check, keyed on
/// `(array element type id, value's dynamic class)`.
///
/// ⭐**A `FastMap` WAS THE OBVIOUS CHOICE AND IS THE WRONG ONE.** The pairs a boot presents run to
/// the thousands, and a hash table that size stops fitting in L1 — this tree measured a memo grown
/// from 2.5 KB to 14 KB as **+6.8% WORSE than the allocation it removed**. A store-check site is
/// effectively monomorphic (a loop fills `Node[]` with `Node`s), so a 64-entry direct-mapped table
/// hits nearly always, never allocates, never grows, and a collision costs exactly one re-walk.
/// **Table size: 64 × 16 B = 1024 bytes, per thread.**
#[derive(Clone, Copy)]
struct Slot {
    key: u64,
    elem: u32,
    /// 0 = empty, 1 = accept (`Tri::Yes`), 2 = reject (`Tri::No`).
    verdict: u8,
}

const SLOTS: usize = 64;

thread_local! {
    static ASTORE_MEMO: std::cell::RefCell<[Slot; SLOTS]> =
        const { std::cell::RefCell::new([Slot { key: 0, elem: 0, verdict: 0 }; SLOTS]) };
}

#[inline]
fn slot_index(elem: u32, key: u64) -> usize {
    // Fibonacci hash of the pair; the TOP bits are the mixed ones, so shift from the high end.
    let h = (key ^ ((elem as u64) << 32)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    (h >> 58) as usize & (SLOTS - 1)
}

/// Memoise one `aastore` store-check verdict. `slow` runs only on a miss.
///
/// ⭐**WAS O(supertypes of the stored value's class) PER ELEMENT STORED — a BFS that allocates a
/// `String` per visited node and a `HashSet<String>` per call — AND IS O(1) after the first store
/// of a given (element type, value class) pair.** The thing that grows is ELEMENTS STORED × the
/// depth of the value's supertype closure; filling a `Block[]` with 1,000 blocks walked the same
/// closure 1,000 times. `checkcast` has had exactly this memo (`CCAST_MEMO`) since it was written,
/// for exactly this reason; `aastore` is the one throwing path that never got one.
///
/// ⚠**`Tri::Unknown` IS NEVER CACHED, AND THAT IS THE WHOLE CORRECTNESS ARGUMENT.** `Unknown`
/// means the walk stopped at a type with no class file that neither `JDK_SUPERS` nor
/// `JDK_TERMINAL` names — an answer about the LOADER'S STATE, which changes as classes load, so
/// freezing it would let a later genuine `ArrayStoreException` go unraised. `Yes` and `No` are
/// both stable: a found path stays found, and `Tri::No` is only ever returned when the closure was
/// enumerated COMPLETELY (`!inconclusive`), so loading more classes cannot add a supertype to it.
/// The remaining input, the array's element descriptor, is a constant of `element_id` — the
/// interner is 1:1 with the descriptor string.
///
/// Refuses (runs `slow` uncached) for a value with no stable class key — `Value::Str` and the
/// primitives, whose arms in `instance_of_tri` are closed-set membership tests and already O(1).
///
/// ⚠**`allow(dead_code)` UNTIL THE CALL SITE LANDS** — `opcodes/casts.rs` belongs to another
/// editor this round; the hand-off wraps the two `instance_of_tri` calls in `astore_reject`.
#[allow(dead_code)]
pub(crate) fn astore_memo(
    vm: &Vm,
    array: u32,
    value: &Value,
    slow: impl FnOnce() -> Tri,
) -> Tri {
    if !fast_ops() {
        return slow();
    }
    let elem = {
        let descs = vm.heap.array_descs.read().unwrap_or_else(|e| e.into_inner());
        match descs.get(array as usize) {
            Some(desc) => desc.element_id,
            None => return slow(),
        }
    };
    if elem == DEAD_ARRAY_ELEMENT_ID {
        return slow();
    }
    let Some(key) = crate::opcodes::classkey::instanceof_class_key(&vm.heap, value) else {
        return slow();
    };
    let i = slot_index(elem, key);
    let hit = ASTORE_MEMO.with(|c| {
        let s = c.borrow()[i];
        (s.verdict != 0 && s.elem == elem && s.key == key).then_some(s.verdict)
    });
    match hit {
        Some(1) => return Tri::Yes,
        Some(_) => return Tri::No,
        None => {}
    }
    let t = slow();
    let verdict = match t {
        Tri::Yes => 1,
        Tri::No => 2,
        // Loader-state-dependent — see the note above. Leave the slot alone.
        Tri::Unknown => return t,
    };
    ASTORE_MEMO.with(|c| c.borrow_mut()[i] = Slot { key, elem, verdict });
    t
}
