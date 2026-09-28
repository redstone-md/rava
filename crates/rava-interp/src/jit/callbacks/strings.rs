//! String bridges: the four String reads compiled code takes a callback for.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;


/// String.length()/hashCode() for a NON-arena receiver (thin callback; the arena
/// case is a native load in the emitter).
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_str_length(recv: i64) -> i64 {
    cb(11);
    // GAP-S09 stage 4 SANCTIONED (read-only): string-content probes —
    // `unbox_ref` takes `&Vm` and `value_str_len`/`value_str_hash` read the
    // receiver's characters without publication (the `&mut` receiver on the
    // `jit_str_*` wrappers is signature inheritance only).
    let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };
    vm.jit_str_length(recv)
}
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_str_hash(recv: i64) -> i64 {
    cb(12);
    // GAP-S09 stage 4 SANCTIONED: read-only content hash — see rava_str_length.
    let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };
    vm.jit_str_hash(recv)
}
/// `String.charAt(int)` for a receiver that is NOT an ASCII arena string (thin
/// callback; the ASCII-arena case is an inline byte load in the emitter). The
/// non-ASCII arena case lands here too, because a char index is not a byte index
/// once any code point is multi-byte.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_str_charat(recv: i64, idx: i64) -> i64 {
    cb(13);
    // GAP-S09 stage 4 SANCTIONED: read-only char probe — see rava_str_length.
    let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };
    vm.jit_str_charat(recv, idx)
}
/// `String.startsWith(String)` for a receiver/prefix that is NOT both arena
/// strings (thin callback; the all-arena case is a native byte-compare in the
/// emitter). Returns 1/0.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_str_startswith(recv: i64, prefix: i64) -> i64 {
    cb(14);
    // HR-STRINGS fast arm (see the block at the end of this file): decode both cells by
    // shape and compare the bytes in place. Refuses to the historic arm for every shape it
    // cannot prove. Kill switch `RAVA_NO_HR_STRINGS=1`.
    if hr_strings_enabled() {
        if let Some(r) = hr_startswith(recv, prefix) {
            return r;
        }
    }
    // GAP-S09 stage 4 SANCTIONED: read-only content compare — see rava_str_length.
    let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };
    vm.jit_str_startswith(recv, prefix)
}

// ═══════════════ HR-STRINGS: `startsWith` decoded by SHAPE, not by `Value` ═══════════════
//
// ⭐**WHAT THE CALLBACK DERIVED PER CALL, AND WHAT IT NEEDED.** `rava_str_startswith` fires
// only when the emitter's all-arena arm refused (`emit_str_startswith_words`: one operand is
// null, a `BOX_TAG` cell — a `Value::Str` a native returned, e.g. `substring`/`toLowerCase`
// — or a `REF_TAG` heap handle). For that population the historic arm
// (`Vm::jit_str_startswith`) did, per call:
//   1. a TLS read for the `&mut Vm` (which the fast shapes never use),
//   2. `unbox_ref` TWICE — each a tag chain, a `pinned_handle_of` probe and a `Value`
//      construction; the boxed side clones an `RStr` out of the box table,
//   3. an acquire-ordered `OnceLock` read (`fast_jitcall_enabled`) — an optimisation barrier,
//   4. `borrowed_text` TWICE, and for an ARENA operand that is a full `from_utf8`
//      validation pass over the operand's bytes — O(len(recv)) for a question worth
//      O(len(prefix)) — on every call, for a string whose header already says it is ASCII,
//   5. the compare, then drop glue for two `Value`s (`drop_in_place<Option<Value>>`).
// Only (5)'s compare and one Arc clone for a boxed operand are the answer's own cost.
//
// **THE FAST ARM** reads each cell by the SAME shape tests `unbox_ref_kind` derefs with —
// `cell != 0`, `cell >> 40 == 0` (arena-shaped), not a pinned mirror — then the header's
// class word (`is_arena_string`), and takes the bytes in place. ASCII-ness is the header
// pair `blen == clen` (the emitter's own `charAt` test, `emit_str_charat`), which makes the
// bytes valid UTF-8 without a walk; a non-ASCII arena body is validated exactly as
// `borrowed_text` does. A `BOX_TAG` cell is read from the box table (one `RStr` clone — the
// table has no borrow accessor, and adding one is not this file's to do). Every other shape
// — null, `REF_TAG`, a pinned mirror, an arena object that is not a string, a non-UTF-8
// body — is REFUSED to the historic arm verbatim, so the guard can only reject.
//
// **WHY BYTES DECIDE JAVA'S UTF-16 ANSWER.** Both accepted texts are valid UTF-8 (a Rust
// `String`, or arena bytes that passed the ASCII header test / `from_utf8`). UTF-8 is
// self-synchronising and prefix-free per scalar, so a whole-string byte prefix ends on a
// scalar boundary ⇔ scalar-sequence prefix ⇔ UTF-16 code-unit prefix (each scalar has one
// fixed UTF-16 spelling; lone surrogates cannot occur in either representation — the engine
// carries them PUA-mapped on both sides alike, see `unmap_surrogate`). This is bit-for-bit
// the `r.starts_with(p)` the historic arm computes on the same two texts, and what
// `natives::str_dispatch`'s `"startsWith"` arm computes for the interpreter. A prefix longer
// than the receiver is rejected by the slice length test before any byte is read.
//
// Kill switch: `RAVA_NO_HR_STRINGS=1` (also the block-wide `RAVA_NO_EMIT_MORE`) — the cold
// arm in `rava_str_startswith` is the pre-existing body, untouched.
#[cfg(feature = "next-gen-gc")]
fn hr_strings_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_HR_STRINGS").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// The text behind a reference cell, held without a `Value`. `Arena` borrows the object's
/// inline bytes (no collection can run inside this callback: it allocates nothing and runs no
/// guest code); `Boxed` owns the one clone the box table hands out.
#[cfg(feature = "next-gen-gc")]
enum CellText {
    Arena(&'static [u8]),
    Boxed(crate::value::RStr),
}

#[cfg(feature = "next-gen-gc")]
impl CellText {
    #[inline]
    fn bytes(&self) -> &[u8] {
        match self {
            CellText::Arena(b) => b,
            CellText::Boxed(s) => s.as_str().as_bytes(),
        }
    }
    /// 0 = arena, 1 = boxed — the census column.
    #[inline]
    fn kind(&self) -> usize {
        match self {
            CellText::Arena(_) => 0,
            CellText::Boxed(_) => 1,
        }
    }
}

/// Decode a ref cell to its text by shape. `None` = a shape this arm cannot prove is a
/// string with UTF-8 text; the caller takes the historic path for it.
#[cfg(feature = "next-gen-gc")]
#[inline]
fn cell_text(cell: i64) -> Option<CellText> {
    if cell == 0 {
        return None; // null: the historic arm owns the null answer
    }
    let u = cell as u64;
    if u >> 32 == BOX_TAG_HI {
        // A boxed `Value`: only an inline `Str` is text whose bytes are its own.
        return match crate::frames::box_get((cell & 0xFFFF_FFFF) as usize) {
            Some(crate::value::Value::Str(s)) => Some(CellText::Boxed(s)),
            _ => None,
        };
    }
    if u >> 40 != 0 {
        return None; // REF_TAG / ARRAY_TAG handle: heap `$str` rendering stays historic
    }
    // Arena-shaped (`unbox_ref_kind`'s test). A pinned mirror is a legacy handle in
    // disguise and is refused, exactly where `unbox_ref_kind` turns it into `Ref(h)`.
    if crate::nextgen::pinned_handle_of(u).is_some() {
        return None;
    }
    // SAFETY: the same non-null, untagged, unpinned cell `unbox_ref_kind` builds an
    // `ArenaPtr` from; the class word is read before any string field.
    let p = unsafe { crate::arena::ArenaPtr::from_addr(u) };
    if !crate::nextgen::is_arena_string(p) {
        return None;
    }
    let bytes = crate::nextgen::arena_str_bytes(p);
    // `blen == clen` ⇔ every char is one byte ⇔ pure ASCII ⇔ valid UTF-8, from the header.
    let ascii = crate::nextgen::arena_str_char_len(p) as u32 as usize == bytes.len();
    if ascii || std::str::from_utf8(bytes).is_ok() {
        Some(CellText::Arena(bytes))
    } else {
        None // non-UTF-8 body: `borrowed_text` refuses it the same way
    }
}

// ── census: served by (receiver kind, prefix kind), and the refused remainder. Plain
// load/store cells, never an RMW on the call path; single-threaded probe runs are exact.
#[cfg(feature = "next-gen-gc")]
static HR_SW_SERVED: [std::sync::atomic::AtomicU64; 4] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 4];
#[cfg(feature = "next-gen-gc")]
static HR_SW_COLD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(feature = "next-gen-gc")]
#[inline]
fn bump(c: &std::sync::atomic::AtomicU64) {
    use std::sync::atomic::Ordering::Relaxed;
    c.store(c.load(Relaxed).wrapping_add(1), Relaxed);
}

/// The `startsWith` fast arm: `Some(answer)` for a pair of shapes it can decode, `None` to
/// take the historic path. The census is charged here so the cold count is exact.
#[cfg(feature = "next-gen-gc")]
#[inline]
fn hr_startswith(recv: i64, prefix: i64) -> Option<i64> {
    let (Some(r), Some(p)) = (cell_text(recv), cell_text(prefix)) else {
        bump(&HR_SW_COLD);
        return None;
    };
    bump(&HR_SW_SERVED[r.kind() * 2 + p.kind()]);
    Some(r.bytes().starts_with(p.bytes()) as i64)
}

/// `[HR-STR]` census line for the periodic profiler dump (`pc_profile::dump`); empty until
/// the callback has fired once.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn hr_strings_report() -> String {
    use std::sync::atomic::Ordering::Relaxed;
    let s: Vec<u64> = HR_SW_SERVED.iter().map(|c| c.load(Relaxed)).collect();
    let cold = HR_SW_COLD.load(Relaxed);
    let served: u64 = s.iter().sum();
    if served + cold == 0 {
        return String::new();
    }
    format!(
        "[HR-STR] startsWith callback: served {served} (recv/prefix arena/arena={} arena/boxed={} boxed/arena={} boxed/boxed={}) | historic {cold} (null, heap handle, pinned, non-string arena, non-UTF-8){}",
        s[0],
        s[1],
        s[2],
        s[3],
        if hr_strings_enabled() { "" } else { " [RAVA_NO_HR_STRINGS: fast arm off]" }
    )
}
#[cfg(not(feature = "next-gen-gc"))]
pub(crate) fn hr_strings_report() -> String {
    String::new()
}
