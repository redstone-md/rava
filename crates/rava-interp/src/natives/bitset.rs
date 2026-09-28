//! java.util.BitSet WORD SCANS — resolve the backing buffer ONCE per operation, then
//! do i64 arithmetic over it.
//!
//! ⭐**WHAT THIS TAKES OUT.** The BitSet accessors were already word-at-a-time rather than
//! bit-at-a-time (`nextSetBit`, `isEmpty`, `length`, `cardinality`), but every word they
//! looked at was re-derived from the `$bits` HANDLE: `Heap::array_get` indexes `self.arrays`,
//! bounds-checks it, switches on the `ArrayData` discriminant, indexes the element `Vec`, and
//! constructs an `Option<Value::Long>` — per WORD. A 4096-bit chunk-section mask is 64 words,
//! so an `isEmpty` that decides on word 0 still paid that table walk once per word, and a
//! `cardinality` paid it 64 times to add up 64 `popcnt`s.
//!
//! ⛔**AND ONE OF THEM WAS STILL PER-BIT.** `nextClearBit` skipped an all-ones word whole, but
//! inside a word that was merely MOSTLY ones it advanced ONE BIT AT A TIME **and re-read the
//! word from the heap on every one of those steps**. A word of `0x0000_FFFF_FFFF_FFFF` scanned
//! from bit 0 cost 48 iterations and 48 array-table walks to answer "bit 48" — which is one
//! `trailing_zeros` of the complement. `nextClearBit` is the sibling of the call the seam census
//! priced at 1.87% of the boot's compiled-to-host crossings, and the voxel-shape join that
//! dominates phase A walks its discrete bitset exactly this way.
//!
//! IS: one `array_base_ptr` per operation and a `&[i64]` scan; `nextClearBit` is
//! `(!word) & (MAX << from%64)` then `trailing_zeros`, O(words) with no inner bit loop.
//!
//! KILL SWITCH `RAVA_NO_FAST_BITSET=1` (default ON) restores BOTH the per-word `array_get`
//! walk and `nextClearBit`'s original bit loop, exactly.

use crate::value::Value;
use crate::Vm;

/// Kill switch, read once. Off ⇒ every scan below falls back to the by-handle word walk that
/// shipped before this file, and `nextClearBit` falls back to its per-bit loop.
pub(crate) fn fast_bitset_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_FAST_BITSET").is_none())
        && !crate::flags::emit_more_disabled()
}

/// The `$bits` words as a flat `&[i64]`, or `None` to make the caller use the cold arm.
///
/// **WHAT THE GUARD PROVES, IN THIS ORDER — THE ORDER IS LOAD-BEARING.** Nothing may form the
/// `&[i64]` until the storage kind is known, because an `&[i64]` of `n` elements laid over a
/// `byte[]`'s `n`-byte buffer is out of bounds even if never read.
///   1. `array_base_ptr` yields a base and an element count. It returns `None` for a REFERENCE
///      array, so a boxed-cell buffer can never reach the steps below;
///   2. that element count equals the `len` the caller scanned with (it came from `array_len`),
///      and the base is non-null and 8-aligned;
///   3. `array_get(arr, 0)` is `Value::Long`. Given (1), that is a PROOF of the storage kind and
///      not a guess about the element-descriptor NAME: for a non-reference array `array_get`
///      yields `Value::Long` from the `ArrayData::Long` arm and from no other (`Byte`/`Int` give
///      `Int`, `Float` gives `Float`, `Double` gives `Double`). So the buffer is a `Vec<i64>` of
///      exactly `n` elements — `n * 8` readable, initialised bytes.
/// Then one CONTENT cross-check, `slice[0] == w0`: the same "verify the fast answer against the
/// authoritative one" rule the field interner's L1 uses. It costs one table walk per OPERATION,
/// not per word, and makes any future divergence between the flat descriptor and the element
/// storage a FALLBACK rather than silently wrong words.
///
/// **SOUNDNESS OF THE BORROW.** The slice's lifetime is tied to `&Vm`, so nothing can mutate or
/// reallocate the heap while it lives; a Java array's backing `Vec` is fixed-length and never
/// reallocates (the same promise `array_base_ptr` documents for the data-parallel fork).
///
/// COST IF REMOVED: every scan in this file goes back to one `self.arrays` walk, one enum
/// switch and one `Option<Value>` construction PER WORD.
#[cfg(feature = "next-gen-gc")]
fn words_slice<'a>(vm: &'a Vm, arr: u32, len: usize) -> Option<&'a [i64]> {
    if !fast_bitset_on() || len == 0 {
        return None;
    }
    // (1)+(2): not a reference array, and base/extent agree with the caller's length.
    let (p, n) = vm.heap.array_base_ptr(arr)?;
    if n != len || p.is_null() || (p as usize) & 7 != 0 {
        return None;
    }
    // (3): storage kind, proved by the value the authoritative accessor produces.
    let Some(Value::Long(w0)) = vm.heap.array_get(arr, 0) else { return None };
    // SAFETY: (1)-(3) proved the buffer is a `Vec<i64>` of `n` initialised elements and that `p`
    // is its non-null, 8-aligned start; the `&Vm` borrow keeps it immutable and alive.
    let s = unsafe { std::slice::from_raw_parts(p as *const i64, n) };
    (s[0] == w0).then_some(s)
}

#[cfg(not(feature = "next-gen-gc"))]
fn words_slice<'a>(_vm: &'a Vm, _arr: u32, _len: usize) -> Option<&'a [i64]> {
    None
}

/// COLD ARM: one word through the heap's boxed accessor — the shape every scan used before,
/// kept verbatim so the kill switch restores the previous behaviour exactly.
#[inline]
fn word_at(vm: &Vm, arr: u32, i: usize) -> u64 {
    super::bitset_word(vm, arr, i)
}

// ---------------------------------------------------------------------------------------------
// The algorithms. Each is generic over "how do I get word i" and is instantiated twice — once
// over the slice (a tight loop LLVM can unroll and vectorise) and once over the heap accessor —
// so the two arms cannot drift apart in ANSWER, only in cost.
// ---------------------------------------------------------------------------------------------

/// Lowest index >= `from` whose bit is set, or -1. Masking off the bits below `from % 64` in the
/// first word makes that word's `trailing_zeros` name exactly that index; later words start at
/// bit 0, which is what `from..nbits` does once it has passed `from`. `from / 64 >= len` leaves
/// the loop unentered ⇒ -1, the answer the empty word list gave.
#[inline]
fn next_set<W: Fn(usize) -> u64>(len: usize, from: usize, w: W) -> i32 {
    let lo = from / 64;
    let mut wi = lo;
    while wi < len {
        let mut x = w(wi);
        if wi == lo {
            // `from % 64` is always < 64, so this shift can never overflow.
            x &= u64::MAX << (from % 64);
        }
        if x != 0 {
            return (wi * 64 + x.trailing_zeros() as usize) as i32;
        }
        wi += 1;
    }
    -1
}

/// Lowest index >= `from` whose bit is CLEAR, or `len * 64` if every bit from `from` to the end
/// of the words is set. `from` at or past `len * 64` answers `from` unchanged.
///
/// **BIT-IDENTICAL to the loop it replaces.** That loop returned the first `i >= from` with
/// `(word(i/64) >> (i%64)) & 1 == 0`, skipping an all-ones word whole and otherwise stepping one
/// bit; `!word` turns "first clear bit" into "first set bit of the complement", the `MAX << from%64`
/// mask starts it at `from`, and an all-ones word complements to 0 and is skipped by the same
/// `x != 0` test the loop's `w == u64::MAX` special case implemented by hand.
#[inline]
fn next_clear<W: Fn(usize) -> u64>(len: usize, from: usize, w: W) -> i32 {
    let nbits = len * 64;
    if from >= nbits {
        return from as i32;
    }
    let lo = from / 64;
    let mut wi = lo;
    loop {
        let mut x = !w(wi);
        if wi == lo {
            x &= u64::MAX << (from % 64);
        }
        if x != 0 {
            return (wi * 64 + x.trailing_zeros() as usize) as i32;
        }
        wi += 1;
        if wi >= len {
            return nbits as i32;
        }
    }
}

/// THE ORIGINAL per-bit walk, reachable only with the kill switch set. Kept so `RAVA_NO_FAST_BITSET=1`
/// is a true revert of this file and not merely of its slice borrow.
fn next_clear_by_bit(vm: &Vm, arr: u32, len: usize, from: usize) -> i32 {
    let mut i = from;
    let n = len * 64;
    while i < n {
        let w = word_at(vm, arr, i / 64);
        if w == u64::MAX {
            i = (i / 64 + 1) * 64;
            continue;
        }
        if (w >> (i % 64)) & 1 == 0 {
            break;
        }
        i += 1;
    }
    i as i32
}

#[inline]
fn all_zero<W: Fn(usize) -> u64>(len: usize, w: W) -> bool {
    (0..len).all(|i| w(i) == 0)
}

/// Bit length: 64 * index of the top non-zero word + its bit width. Scans BACKWARDS and stops at
/// the first non-zero word, which is what the `rposition` form did — after copying every word.
#[inline]
fn bit_length<W: Fn(usize) -> u64>(len: usize, w: W) -> usize {
    for wi in (0..len).rev() {
        let x = w(wi);
        if x != 0 {
            return wi * 64 + (64 - x.leading_zeros() as usize);
        }
    }
    0
}

/// Every set bit, ascending, appended as `Value::Int`. `trailing_zeros` names the lowest
/// remaining bit of the current word and `x &= x - 1` drops it, so the order is the ascending
/// order a `(0..nbits).filter(bit set)` produced — O(popcount), not O(capacity).
#[inline]
fn each_set<W: Fn(usize) -> u64>(len: usize, w: W, out: &mut Vec<Value>) {
    for wi in 0..len {
        let mut x = w(wi);
        while x != 0 {
            out.push(Value::Int((wi * 64 + x.trailing_zeros() as usize) as i32));
            x &= x - 1;
        }
    }
}

#[inline]
fn popcount<W: Fn(usize) -> u64>(len: usize, w: W) -> u32 {
    let mut n = 0u32;
    for i in 0..len {
        n += w(i).count_ones();
    }
    n
}

// ---------------------------------------------------------------------------------------------
// Entry points. `arr`/`len` come from `natives::bitset_array` (one `$bits` read per call).
// ---------------------------------------------------------------------------------------------

pub(crate) fn bs_next_set_bit(vm: &Vm, arr: u32, len: usize, from: usize) -> i32 {
    match words_slice(vm, arr, len) {
        Some(s) => next_set(len, from, |i| s[i] as u64),
        None => next_set(len, from, |i| word_at(vm, arr, i)),
    }
}

pub(crate) fn bs_next_clear_bit(vm: &Vm, arr: u32, len: usize, from: usize) -> i32 {
    if !fast_bitset_on() {
        return next_clear_by_bit(vm, arr, len, from);
    }
    match words_slice(vm, arr, len) {
        Some(s) => next_clear(len, from, |i| s[i] as u64),
        None => next_clear(len, from, |i| word_at(vm, arr, i)),
    }
}

pub(crate) fn bs_is_empty(vm: &Vm, arr: u32, len: usize) -> bool {
    match words_slice(vm, arr, len) {
        Some(s) => all_zero(len, |i| s[i] as u64),
        None => all_zero(len, |i| word_at(vm, arr, i)),
    }
}

pub(crate) fn bs_length(vm: &Vm, arr: u32, len: usize) -> usize {
    match words_slice(vm, arr, len) {
        Some(s) => bit_length(len, |i| s[i] as u64),
        None => bit_length(len, |i| word_at(vm, arr, i)),
    }
}

pub(crate) fn bs_set_bits(vm: &Vm, arr: u32, len: usize, out: &mut Vec<Value>) {
    match words_slice(vm, arr, len) {
        Some(s) => each_set(len, |i| s[i] as u64, out),
        None => each_set(len, |i| word_at(vm, arr, i), out),
    }
}

pub(crate) fn bs_cardinality(vm: &Vm, arr: u32, len: usize) -> u32 {
    match words_slice(vm, arr, len) {
        Some(s) => popcount(len, |i| s[i] as u64),
        None => popcount(len, |i| word_at(vm, arr, i)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The word algorithms against the loops they replace, over a closure — no VM needed. The
    /// point is the EQUIVALENCE, which is the whole correctness claim of this file.
    fn ref_next_set(words: &[u64], from: usize) -> i32 {
        let n = words.len() * 64;
        (from..n)
            .find(|&i| (words[i / 64] >> (i % 64)) & 1 != 0)
            .map(|i| i as i32)
            .unwrap_or(-1)
    }

    fn ref_next_clear(words: &[u64], from: usize) -> i32 {
        let mut i = from;
        let n = words.len() * 64;
        while i < n {
            let w = words[i / 64];
            if w == u64::MAX {
                i = (i / 64 + 1) * 64;
                continue;
            }
            if (w >> (i % 64)) & 1 == 0 {
                break;
            }
            i += 1;
        }
        i as i32
    }

    #[test]
    fn word_scans_match_the_bit_loops() {
        let cases: [&[u64]; 6] = [
            &[],
            &[0],
            &[u64::MAX],
            &[0x0000_FFFF_FFFF_FFFF, 0, u64::MAX, 1],
            &[u64::MAX, u64::MAX, u64::MAX],
            &[0x8000_0000_0000_0000, 0x0000_0000_0000_0001],
        ];
        for words in cases {
            let len = words.len();
            for from in 0..(len * 64 + 3) {
                assert_eq!(
                    next_set(len, from, |i| words[i]),
                    ref_next_set(words, from),
                    "nextSetBit {words:?} from {from}"
                );
                assert_eq!(
                    next_clear(len, from, |i| words[i]),
                    ref_next_clear(words, from),
                    "nextClearBit {words:?} from {from}"
                );
            }
            assert_eq!(all_zero(len, |i| words[i]), words.iter().all(|&w| w == 0));
            assert_eq!(
                bit_length(len, |i| words[i]),
                words
                    .iter()
                    .rposition(|&w| w != 0)
                    .map(|wi| wi * 64 + (64 - words[wi].leading_zeros() as usize))
                    .unwrap_or(0)
            );
            assert_eq!(
                popcount(len, |i| words[i]),
                words.iter().map(|w| w.count_ones()).sum::<u32>()
            );
        }
    }
}
