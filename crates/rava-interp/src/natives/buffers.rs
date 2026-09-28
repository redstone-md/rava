//! ByteBuffer, the typed buffer views, and BitSet.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::natives::*;

/// Move `len` bytes from `src[soff..]` into `dst[doff..]` as ONE slice read plus one
/// memcpy, or return false having done NOTHING so the caller runs its element loop.
///
/// ⭐**The element loop paid a boxed `Value` per byte in each direction** — an `array_get`
/// that constructs `Value::Int` and an `array_set` that decodes it and truncates back to
/// `u8`. Every bulk `ByteBuffer` transfer went through it: `RegionFile` pulls whole 4 KB
/// chunk sectors through `get(byte[])`, and each NBT/packet blob is copied the same way.
/// The backing storage on both sides is a `Vec<u8>`, so the move is a `copy_from_slice`.
///
/// Bit-exact: `array_write_bytes` stores the raw byte, which is exactly what
/// `array_set(Value::Int(b as i8 as i32))` truncates back to.
///
/// REFUSES — returning false with nothing written — when either side is not a byte array,
/// when the source is SHORT of `len` (the element loop reads past-the-end as zero and
/// writes those zeros; a slice copy cannot reproduce that), or when the destination cannot
/// take the whole run, since `array_write_bytes` range-checks before it copies. A refusal
/// costs one bounds test and one `Vec`, and the caller's loop then behaves as before.
fn bulk_copy(vm: &mut Vm, src: u32, soff: usize, dst: u32, doff: usize, len: usize) -> bool {
    if !fast_decode_on() {
        return false;
    }
    if len == 0 {
        return true;
    }
    let Some(chunk) = vm.heap.array_read_bytes(src, soff, len) else { return false };
    if chunk.len() != len {
        return false;
    }
    vm.heap.array_write_bytes(dst, doff, &chunk)
}

/// java.util.BitSet — a growable bit vector packed into a `$bits` `long[]` (64
/// bits per word) and mutated in place, so set/clear/flip don't reallocate the
/// backing array (block-shape merging hits these millions of times). Generic
/// JDK behaviour, no dependency on any particular caller.
pub(crate) fn bitset_dispatch(vm: &mut Vm, name: &str, desc: &str, args: &[Value]) -> Option<Option<Value>> {
    // Static factory BitSet.valueOf(long[]|byte[]) — no receiver. CarvingMask
    // reconstructs the carving mask from saved words on chunk load.
    if name == "valueOf" {
        let arr = match args.first() { Some(Value::Array(a)) => *a, _ => return None };
        let words: Vec<i64> = if desc.starts_with("([J") {
            vm.heap.array_values(arr).unwrap_or_default().iter().map(|v| v.as_long().unwrap_or(0)).collect()
        } else {
            // byte[] (or ByteBuffer not handled): pack little-endian into longs.
            // The bytes are read as a SLICE — the old form boxed the whole array into a
            // `Vec<Value>` and then unboxed every element back to `u8`, two passes and two
            // allocations over data that is already a `Vec<u8>`. `array_read_bytes` yields
            // the same raw bytes the `as u8` truncation produced.
            let n = vm.heap.array_len(arr).unwrap_or(0);
            let bytes: Vec<u8> = match fast_decode_on().then(|| vm.heap.array_read_bytes(arr, 0, n)).flatten() {
                Some(b) => b,
                None => vm.heap.array_values(arr).unwrap_or_default().iter().map(|v| v.as_int().unwrap_or(0) as u8).collect(),
            };
            bytes.chunks(8).map(|c| {
                let mut w = [0u8; 8];
                w[..c.len()].copy_from_slice(c);
                i64::from_le_bytes(w)
            }).collect()
        };
        let h = vm.heap.alloc("java/util/BitSet");
        let data: Vec<Value> = words.into_iter().map(Value::Long).collect();
        let a = vm.heap.alloc_values("long", data);
        vm.heap.put_field(h, "$bits", Value::Array(a));
        return Some(Some(Value::Ref(h)));
    }
    let recv = &match args.first() { Some(v @ (Value::Ref(_) | Value::Obj(_))) => v.clone(), _ => return None };
    let arg_int = |i: usize| args.get(i).and_then(Value::as_int).unwrap_or(0).max(0) as usize;
    // The packed word array handle, grown (reallocated) only when a higher bit
    // index than the current capacity is touched. In-place word writes use
    // array_set so steady-state set/clear/flip allocate nothing.
    fn words_handle(vm: &mut Vm, recv: &Value) -> Option<u32> {
        match vm.synth_get_ro(recv, "$bits") { Value::Array(a) => Some(a), _ => None }
    }
    fn ensure_words(vm: &mut Vm, recv: &Value, nwords: usize) -> u32 {
        let cur = words_handle(vm, recv);
        let curlen = cur.and_then(|a| vm.heap.array_len(a)).unwrap_or(0);
        if curlen >= nwords {
            return cur.unwrap();
        }
        let new = vm.heap.alloc_array("long", nwords.max(4));
        for i in 0..curlen {
            let w = cur.and_then(|a| vm.heap.array_get(a, i)).unwrap_or(Value::Long(0));
            vm.heap.array_set(new, i, w);
        }
        vm.synth_put(recv, "$bits", Value::Array(new));
        new
    }
    fn get_bit(vm: &Vm, recv: &Value, i: usize) -> bool {
        if let Value::Array(a) = vm.synth_get_ro(recv, "$bits") {
            if let Some(Value::Long(w)) = vm.heap.array_get(a, i / 64) {
                return (w as u64 >> (i % 64)) & 1 != 0;
            }
        }
        false
    }
    /// ⭐**A SINGLE-BIT `set`/`clear` WALKED THE ARRAY TABLE THREE TIMES.** `ensure_words` read
    /// `$bits`, then asked `array_len` whether the word existed, and only then did the caller
    /// `array_get`/`array_set` it — but `array_get` ALREADY answers "does word `wi` exist", so
    /// the length probe re-derived a fact the very next line establishes. That is per-element
    /// work on the voxel-shape fill/join inner loop, which sets bits one at a time.
    ///
    /// FAST ARM guard: `array_get(a, wi)` returning `Some` PROVES `wi < array_len(a)`, which is
    /// exactly the condition under which `ensure_words` returns the existing array untouched —
    /// so the two paths write the same word of the same array with the same mask. COLD ARM (no
    /// `$bits` yet, or `wi` past the end): the original `ensure_words` growth, unchanged.
    /// Kill switch `RAVA_NO_FAST_BITSET=1` takes the cold arm always.
    fn put_bit(vm: &mut Vm, recv: &Value, i: usize, val: bool) {
        let wi = i / 64;
        // `i % 64` is always < 64, so this shift can never overflow.
        let mask = 1u64 << (i % 64);
        if crate::natives::bitset::fast_bitset_on() {
            if let Some(a) = words_handle(vm, recv) {
                if let Some(cur) = vm.heap.array_get(a, wi) {
                    let w = cur.as_long().unwrap_or(0) as u64;
                    let nw = if val { w | mask } else { w & !mask };
                    vm.heap.array_set(a, wi, Value::Long(nw as i64));
                    return;
                }
            }
        }
        let a = ensure_words(vm, recv, wi + 1);
        let w = vm.heap.array_get(a, wi).and_then(|v| v.as_long()).unwrap_or(0) as u64;
        let nw = if val { w | mask } else { w & !mask };
        vm.heap.array_set(a, wi, Value::Long(nw as i64));
    }
    /// ⭐**`set(from, to)` AND `clear(from, to)` RAN THE BIT LOOP.** Every iteration did a
    /// synthetic-field read (`$bits`), an `array_len`, a boxed `array_get` and a boxed
    /// `array_set` — four heap round trips to move ONE bit — so `BitSet.set(0, 4096)`, which is
    /// what a chunk-section shape mask is, cost ~16 000 of them to write 64 words. This writes
    /// the 64 bits of a word in ONE masked read-modify-write: **64× fewer heap operations, and
    /// the capacity grown once instead of once per bit.**
    ///
    /// **BIT-IDENTICAL.** The mask for word `wi` is exactly the bits of `[from, to)` that land in
    /// it — `MAX << lo` clears everything below the first wanted bit, `MAX >> (63 - hi)` clears
    /// everything above the last — and OR-ing (or AND-NOT-ing) that mask sets (or clears) the same
    /// bits the loop's `1 << (i % 64)` did, leaving every other bit of the word untouched. The
    /// final capacity matches too: the loop's largest `ensure_words` call is for the top bit,
    /// `(to - 1) / 64 + 1`, which is the one call made here, and `ensure_words` preserves the
    /// words it copies forward, so the intermediate regrowths the loop performed are invisible.
    /// Empty ranges (`from >= to`) write nothing and grow nothing, which is what the loop did.
    ///
    /// REFUSES — returning false, having done NOTHING — for an absurd top bit, so a bogus
    /// `set(0, Integer.MAX_VALUE)` cannot turn the loop's slow crawl into an instant 256 MB
    /// allocation. The caller then runs the bit loop exactly as before.
    fn fill_bits(vm: &mut Vm, recv: &Value, from: usize, to: usize, val: bool) -> bool {
        if from >= to {
            return true;
        }
        let hi_word = (to - 1) / 64;
        if hi_word >= 1 << 22 {
            return false; // > 256 Mbit: not a real BitSet, let the old path deal with it
        }
        let arr = ensure_words(vm, recv, hi_word + 1);
        let lo_word = from / 64;
        for wi in lo_word..=hi_word {
            let lo = if wi == lo_word { from % 64 } else { 0 };
            let hi = if wi == hi_word { (to - 1) % 64 } else { 63 };
            let mask = (u64::MAX << lo) & (u64::MAX >> (63 - hi));
            let w = vm.heap.array_get(arr, wi).and_then(|v| v.as_long()).unwrap_or(0) as u64;
            let nw = if val { w | mask } else { w & !mask };
            vm.heap.array_set(arr, wi, Value::Long(nw as i64));
        }
        true
    }
    // One gate read for the whole dispatch — the word walks below consult it, never `getenv`.
    let wide = simd_scan_on();
    let ret = match name {
        "<init>" => None, // size hint (if any) ignored — words grow on demand
        "set" => {
            if desc.starts_with("(II") {
                let (from, to) = (arg_int(1), arg_int(2));
                let val = match args.get(3) { Some(v) => v.as_int().unwrap_or(1) != 0, None => true };
                if !(wide && fill_bits(vm, recv, from, to, val)) {
                    for i in from..to { put_bit(vm, recv, i, val); }
                }
            } else {
                let i = arg_int(1);
                let val = match args.get(2) { Some(v) => v.as_int().unwrap_or(1) != 0, None => true };
                put_bit(vm, recv, i, val);
            }
            None
        }
        "get" => Some(Value::Int(get_bit(vm, recv, arg_int(1)) as i32)),
        "clear" => {
            if desc.starts_with("(II") {
                let (from, to) = (arg_int(1), arg_int(2));
                if !(wide && fill_bits(vm, recv, from, to, false)) {
                    for i in from..to { put_bit(vm, recv, i, false); }
                }
            } else if desc.starts_with("(I") {
                put_bit(vm, recv, arg_int(1), false);
            } else if let Some(a) = words_handle(vm, recv) {
                for i in 0..vm.heap.array_len(a).unwrap_or(0) { vm.heap.array_set(a, i, Value::Long(0)); }
            }
            None
        }
        "flip" => {
            let i = arg_int(1);
            put_bit(vm, recv, i, !get_bit(vm, recv, i));
            None
        }
        "clone" => {
            let words = bitset_words(vm, recv);
            let h = vm.heap.alloc("java/util/BitSet");
            let data: Vec<Value> = words.iter().map(|&w| Value::Long(w as i64)).collect();
            let a = vm.heap.alloc_values("long", data);
            vm.heap.put_field(h, "$bits", Value::Array(a));
            Some(Value::Ref(h))
        }
        // ⭐**THREE PREDICATES THAT COPIED THE WHOLE SET TO ANSWER A QUESTION ABOUT ONE WORD.**
        // `bitset_words` materialises a `Vec<u64>` by unboxing EVERY word through `array_get`, so
        // `isEmpty` on a 4096-bit mask allocated 64 words and unboxed all of them in order to
        // discover that word 0 is non-zero, and `length` did the same to read the LAST one. The
        // in-place walk keeps the identical word arithmetic — `count_ones`, `w == 0`, the
        // `leading_zeros` bit length of the top non-zero word — and just stops when the answer is
        // known: **one allocation and O(words) unboxing removed per call, and `isEmpty`/`length`
        // now stop at the first/last word that decides it.**
        // ⭐**THE WORD WALKS NOW RESOLVE THE BUFFER ONCE, NOT ONCE PER WORD.** These three already
        // scanned words rather than bits, but each word arrived through `array_get` — an
        // `self.arrays` index, a bounds check, an `ArrayData` switch and an `Option<Value::Long>`
        // — so a 64-word chunk-section mask paid 64 array-table walks to add up 64 `popcnt`s.
        // `natives::bitset::*` takes the `$bits` buffer as one `&[i64]` (see its guard) and runs
        // the identical word arithmetic over it; the cold arm inside it IS this per-word walk.
        "cardinality" => {
            let mut n = 0u32;
            match wide.then(|| crate::natives::bitset_array(vm, recv)).flatten() {
                Some((arr, len)) => n = crate::natives::bitset::bs_cardinality(vm, arr, len),
                // No `$bits` under the gate answers 0, which is what the empty `Vec` summed to.
                None if wide => {}
                None => n = bitset_words(vm, recv).iter().map(|w| w.count_ones()).sum::<u32>(),
            }
            Some(Value::Int(n as i32))
        }
        "isEmpty" => {
            let mut empty = true;
            match wide.then(|| crate::natives::bitset_array(vm, recv)).flatten() {
                Some((arr, len)) => empty = crate::natives::bitset::bs_is_empty(vm, arr, len),
                // No `$bits` ⇒ `bitset_words` was empty ⇒ `all` was vacuously true.
                None if wide => {}
                None => empty = bitset_words(vm, recv).iter().all(|&w| w == 0),
            }
            Some(Value::Int(empty as i32))
        }
        "length" => {
            let mut len = 0usize;
            match wide.then(|| crate::natives::bitset_array(vm, recv)).flatten() {
                // Backwards, and out at the FIRST non-zero word — the same scan, minus the
                // per-word table walk.
                Some((arr, n)) => len = crate::natives::bitset::bs_length(vm, arr, n),
                None if wide => {}
                None => {
                    let words = bitset_words(vm, recv);
                    len = words.iter().rposition(|&w| w != 0)
                        .map(|wi| wi * 64 + (64 - words[wi].leading_zeros() as usize)).unwrap_or(0);
                }
            }
            Some(Value::Int(len as i32))
        }
        // ⭐**A LENGTH THAT ALLOCATED THE WHOLE SET TO READ ITS OWN LENGTH.** `bitset_words`
        // unboxed every word into a fresh `Vec<u64>` and then only `.len()` was asked of it.
        // `bitset_array` already carries the word count (`array_len`) — same number, no `Vec`,
        // no unboxing. Identical when `$bits` is absent: 0 words ⇒ 0 ⇒ `.max(64)` ⇒ 64.
        "size" => Some(Value::Int(
            (crate::natives::bitset_array(vm, recv).map_or(0, |(_, n)| n) * 64).max(64) as i32,
        )),
        // ⭐**THE SIBLING OF `nextClearBit`, AND IT WAS STILL BIT-AT-A-TIME — OVER A FULL COPY.**
        // It built the whole `Vec<u64>` (one boxed `array_get` per word) and then tested ONE BIT
        // per iteration, so scanning a sparse 4096-bit mask for its next set bit cost 64 unboxings
        // plus up to 4096 shift-and-test steps. A word is zero or it is not: an all-zero word is
        // skipped WHOLE and the answer inside a non-zero one is `trailing_zeros`, one instruction.
        // **O(bits) → O(words), with the allocation gone** — and `nextClearBit`, the same shape,
        // was measured at 1.87% of the boot's compiled-to-host crossings.
        //
        // **BIT-IDENTICAL.** `(from..n).find(bit set)` returns the lowest index >= `from` whose bit
        // is 1. Masking off the bits below `from % 64` in the first word makes that word's
        // `trailing_zeros` name exactly that index, and every later word is scanned from bit 0,
        // which is what `from..n` does once it has passed `from`. No `$bits` ⇒ no words ⇒ -1, the
        // same answer the empty `Vec` gave; `from` past the end leaves the loop unentered ⇒ -1.
        "nextSetBit" => {
            let from = arg_int(1);
            if !wide {
                let words = bitset_words(vm, recv);
                let n = words.len() * 64;
                return Some(Some(Value::Int((from..n).find(|&i| (words[i / 64] >> (i % 64)) & 1 != 0)
                    .map(|i| i as i32).unwrap_or(-1))));
            }
            let mut ans = -1i32;
            if let Some((arr, len)) = crate::natives::bitset_array(vm, recv) {
                ans = crate::natives::bitset::bs_next_set_bit(vm, arr, len, from);
            }
            Some(Value::Int(ans))
        }
        // ⛔**AND THIS ONE WAS STILL PER-BIT INSIDE A WORD.** It skipped an ALL-ONES word whole,
        // but a word that was merely MOSTLY ones was walked one bit at a time — **re-reading the
        // word from the heap on every step** — so `0x0000_FFFF_FFFF_FFFF` scanned from bit 0 cost
        // 48 iterations and 48 array-table walks to answer "48". `bs_next_clear_bit` complements
        // the word and takes `trailing_zeros`: O(words), no inner loop, same answer (the all-ones
        // case complements to 0 and is skipped by the same emptiness test). This call is 1.87% of
        // the boot's compiled-to-host crossings.
        "nextClearBit" => {
            let i = arg_int(1);
            match crate::natives::bitset_array(vm, recv) {
                Some((arr, len)) => Some(Value::Int(
                    crate::natives::bitset::bs_next_clear_bit(vm, arr, len, i),
                )),
                None => Some(Value::Int(i as i32)),
            }
        }
        "equals" => {
            // The ARGUMENT is a receiver too, so it takes the same both-representations
            // shape: a `BitSet.equals` against an arena BitSet must not silently answer 0.
            let other = match args.get(1) {
                Some(v @ (Value::Ref(_) | Value::Obj(_))) => v.clone(),
                _ => return Some(Some(Value::Int(0))),
            };
            let trim = |mut v: Vec<u64>| { while v.last() == Some(&0) { v.pop(); } v };
            Some(Value::Int((trim(bitset_words(vm, recv)) == trim(bitset_words(vm, &other))) as i32))
        }
        // toLongArray: the packed words, trailing all-zero words removed (JDK
        // semantics). CarvingMask serializes the carving bits via this on save.
        "toLongArray" => {
            let mut words = bitset_words(vm, recv);
            while words.last() == Some(&0) { words.pop(); }
            let data: Vec<Value> = words.iter().map(|&w| Value::Long(w as i64)).collect();
            Some(Value::Array(vm.heap.alloc_values("long", data)))
        }
        // toByteArray: little-endian bytes of the (trimmed) words.
        "toByteArray" => {
            let mut words = bitset_words(vm, recv);
            while words.last() == Some(&0) { words.pop(); }
            let mut bytes: Vec<u8> = words.iter().flat_map(|&w| w.to_le_bytes()).collect();
            while bytes.last() == Some(&0) { bytes.pop(); }
            let data: Vec<Value> = bytes.iter().map(|&b| Value::Int(b as i8 as i32)).collect();
            Some(Value::Array(vm.heap.alloc_values("byte", data)))
        }
        // Same edit as `nextSetBit`, applied to the whole enumeration: one `trailing_zeros` per SET
        // bit and `w &= w - 1` to drop it, instead of one shift-and-test per POSSIBLE bit. A
        // sparse mask is the common case, so this is O(popcount) where it was O(capacity).
        // Ascending order is preserved — `trailing_zeros` always names the lowest remaining bit of
        // the current word and the words are walked in index order — so the emitted `int[]` is
        // element-for-element what the filter produced.
        "stream" => {
            let mut set: Vec<Value> = Vec::new();
            match wide.then(|| crate::natives::bitset_array(vm, recv)).flatten() {
                // Same enumeration, one buffer resolution instead of one per word.
                Some((arr, len)) => crate::natives::bitset::bs_set_bits(vm, arr, len, &mut set),
                None if wide => {}
                None => {
                    let words = bitset_words(vm, recv);
                    let n = words.len() * 64;
                    set = (0..n).filter(|&i| (words[i / 64] >> (i % 64)) & 1 != 0)
                        .map(|i| Value::Int(i as i32)).collect();
                }
            }
            let arr = vm.heap.alloc_values("int", set);
            let h = vm.heap.alloc("java/util/stream/IntStream");
            vm.heap.put_field_syn(h, crate::heap::Syn::Elems, Value::Array(arr));
            Some(Value::Ref(h))
        }
        _ => return None,
    };
    Some(ret)
}

/// java.nio.ByteBuffer — a heap buffer over a byte[] with position/limit/order.
/// THE THROWING WRAPPER — same shape as `uuid_dispatch`. A RELATIVE READ PAST THE LIMIT
/// must raise `BufferUnderflowException`: reading off the end returned ZEROS, so a
/// truncated packet decoded as a valid one full of default values, which is the quietest
/// possible framing bug.
pub(crate) fn bytebuffer_dispatch(
    vm: &mut Vm,
    name: &str,
    args: &[Value],
) -> Result<Option<Option<Value>>, VmError> {
    if args.len() == 1 {
        if let Some(width) = relative_read_width(name) {
            if let Some(Value::Ref(bb)) = args.first() {
                let pos = vm.heap.get_field_syn(*bb, crate::heap::Syn::Pos).as_int().unwrap_or(0).max(0) as usize;
                let lim = vm.heap.get_field(*bb, "$limit").as_int().unwrap_or(0).max(0) as usize;
                if pos + width > lim {
                    let e = vm.make_exception("java/nio/BufferUnderflowException", "");
                    return Err(VmError::Throw(e));
                }
            }
        }
    }
    Ok(bytebuffer_dispatch_inner(vm, name, args))
}

/// Bytes consumed by a RELATIVE getter, or `None` when the name is not one.
fn relative_read_width(name: &str) -> Option<usize> {
    Some(match name {
        "get" => 1,
        "getShort" | "getChar" => 2,
        "getInt" | "getFloat" => 4,
        "getLong" | "getDouble" => 8,
        _ => return None,
    })
}

fn bytebuffer_dispatch_inner(vm: &mut Vm, name: &str, args: &[Value]) -> Option<Option<Value>> {
    // Static factories.
    match name {
        "wrap" => {
            let buf = match args.first() { Some(Value::Array(h)) => *h, _ => return Some(Some(Value::Null)) };
            let len = vm.heap.array_len(buf).unwrap_or(0);
            let off = args.get(1).and_then(Value::as_int).unwrap_or(0) as usize;
            let l = args.get(2).and_then(Value::as_int).map(|v| off + v as usize).unwrap_or(len);
            return Some(Some(make_buffer(vm, buf, off, l, len)));
        }
        "allocate" | "allocateDirect" => {
            let n = args.first().and_then(Value::as_int).unwrap_or(0).max(0) as usize;
            let buf = vm.heap.alloc_bytes(&vec![0u8; n]);
            return Some(Some(make_buffer(vm, buf, 0, n, n)));
        }
        _ => {}
    }
    let bb = match args.first() { Some(Value::Ref(h)) => *h, _ => return None };
    let field = |vm: &Vm, f: &str| vm.heap.get_field(bb, f).as_int().unwrap_or(0);
    let buf = match vm.heap.get_field(bb, "$buf") { Value::Array(h) => h, _ => return Some(Some(Value::Null)) };
    let pos = field(vm, "$pos") as usize;
    let little = matches!(vm.heap.get_field(bb, "$order"), Value::Str(s) if s.as_ref() == "LE");
    let set_pos = |vm: &mut Vm, p: usize| vm.heap.put_field_syn(bb, crate::heap::Syn::Pos, Value::Int(p as i32));
    // NO `Vec` PER SCALAR READ. `getInt` malloc'd a four-byte vector, folded it and freed
    // it — one allocation per field of every NBT/packet record decoded through a
    // ByteBuffer. The widest read is 8 bytes, so it is a register-width fold instead.
    // Byte order is applied by walking the indices backwards, which is exactly what the
    // old `.rev()` on the collected vector did.
    let read_n = |vm: &Vm, at: usize, n: usize| -> i64 {
        let mut acc = 0i64;
        for i in 0..n {
            let idx = if little { n - 1 - i } else { i };
            let b = vm.heap.array_get(buf, at + idx).and_then(|v| v.as_int()).unwrap_or(0) as u8;
            acc = (acc << 8) | b as i64;
        }
        acc
    };
    let ret = match name {
        "position" if args.len() == 1 => Value::Int(pos as i32),
        "position" => { set_pos(vm, args.get(1).and_then(Value::as_int).unwrap_or(0) as usize); return Some(Some(Value::Ref(bb))); }
        "limit" if args.len() == 1 => Value::Int(field(vm, "$limit")),
        "limit" => { vm.heap.put_field(bb, "$limit", args.get(1).cloned().unwrap_or(Value::Int(0))); return Some(Some(Value::Ref(bb))); }
        "capacity" => Value::Int(field(vm, "$cap")),
        "remaining" => Value::Int(field(vm, "$limit") - pos as i32),
        "hasRemaining" => Value::Int(((field(vm, "$limit") as usize) > pos) as i32),
        "array" => Value::Array(buf),
        "arrayOffset" => Value::Int(0),
        // `order()` with no argument is the GETTER, and only the setter existed — so a
        // guest that read the order back hit an UnsatisfiedLinkError. Returns the same
        // `ByteOrder` constant shape `get_static` hands out, so `toString` prints
        // BIG_ENDIAN / LITTLE_ENDIAN as the JDK's does.
        "order" if args.len() >= 2 => { // order(ByteOrder)
            let le = matches!(args.get(1), Some(Value::Ref(h)) if matches!(vm.heap.get_field_syn(*h, crate::heap::Syn::Name), Value::Str(s) if s.as_ref().contains("LITTLE")));
            vm.heap.put_field(bb, "$order", Value::Str(if le { "LE" } else { "BE" }.into()));
            return Some(Some(Value::Ref(bb)));
        }
        "order" => {
            let le = matches!(vm.heap.get_field(bb, "$order"), Value::Str(s) if s.as_ref() == "LE");
            let h = vm.heap.alloc("java/nio/ByteOrder");
            let n = if le { "LITTLE_ENDIAN" } else { "BIG_ENDIAN" };
            vm.heap.put_field_syn(h, crate::heap::Syn::Name, Value::Str(n.into()));
            return Some(Some(Value::Ref(h)));
        }
        "clear" => { vm.heap.put_field_syn(bb, crate::heap::Syn::Pos, Value::Int(0)); let c = field(vm, "$cap"); vm.heap.put_field(bb, "$limit", Value::Int(c)); return Some(Some(Value::Ref(bb))); }
        "flip" => { vm.heap.put_field(bb, "$limit", Value::Int(pos as i32)); vm.heap.put_field_syn(bb, crate::heap::Syn::Pos, Value::Int(0)); return Some(Some(Value::Ref(bb))); }
        "rewind" => { vm.heap.put_field_syn(bb, crate::heap::Syn::Pos, Value::Int(0)); return Some(Some(Value::Ref(bb))); }
        "mark" => { vm.heap.put_field(bb, "$mark", Value::Int(pos as i32)); return Some(Some(Value::Ref(bb))); }
        "reset" => { let m = field(vm, "$mark"); set_pos(vm, m as usize); return Some(Some(Value::Ref(bb))); }
        "get" if args.len() == 1 => { let b = read_n(vm, pos, 1) as i8 as i32; set_pos(vm, pos + 1); Value::Int(b) }
        // BULK get(byte[] dst[, off, len]) — copy from position, advance. Missing
        // form was a ULE that killed MC RegionFile chunk reads → every cached
        // chunk looked absent → the boot silently REGENERATED the whole spawn
        // area (worldgen grind on a fully cached world).
        "get" if matches!(args.get(1), Some(Value::Array(_))) => {
            let dst = match args.get(1) { Some(Value::Array(h)) => *h, _ => return None };
            let off = args.get(2).and_then(Value::as_int).unwrap_or(0).max(0) as usize;
            let len = args.get(3).and_then(Value::as_int).map(|v| v.max(0) as usize)
                .unwrap_or_else(|| vm.heap.array_len(dst).unwrap_or(0));
            if !bulk_copy(vm, buf, pos, dst, off, len) {
                for i in 0..len {
                    let b = vm.heap.array_get(buf, pos + i).and_then(|v| v.as_int()).unwrap_or(0);
                    vm.heap.array_set(dst, off + i, Value::Int(b as i8 as i32));
                }
            }
            set_pos(vm, pos + len);
            return Some(Some(Value::Ref(bb)));
        }
        // ABSOLUTE bulk get(int index, byte[] dst[, off, len]) (JDK 13+) — no advance.
        "get" if matches!((args.get(1), args.get(2)), (Some(Value::Int(_)), Some(Value::Array(_)))) => {
            let at = args.get(1).and_then(Value::as_int).unwrap_or(0).max(0) as usize;
            let dst = match args.get(2) { Some(Value::Array(h)) => *h, _ => return None };
            let off = args.get(3).and_then(Value::as_int).unwrap_or(0).max(0) as usize;
            let len = args.get(4).and_then(Value::as_int).map(|v| v.max(0) as usize)
                .unwrap_or_else(|| vm.heap.array_len(dst).unwrap_or(0));
            if !bulk_copy(vm, buf, at, dst, off, len) {
                for i in 0..len {
                    let b = vm.heap.array_get(buf, at + i).and_then(|v| v.as_int()).unwrap_or(0);
                    vm.heap.array_set(dst, off + i, Value::Int(b as i8 as i32));
                }
            }
            return Some(Some(Value::Ref(bb)));
        }
        "get" if matches!(args.get(1), Some(Value::Int(_))) => { let i = args.get(1).and_then(Value::as_int).unwrap_or(0) as usize; Value::Int(read_n(vm, i, 1) as i8 as i32) }
        // BULK put(byte[] src[, off, len]) — the scalar router coerced the array
        // arg to 0 and wrote ONE zero byte (silent corruption); copy + advance.
        "put" if matches!(args.get(1), Some(Value::Array(_))) => {
            let src = match args.get(1) { Some(Value::Array(h)) => *h, _ => return None };
            let off = args.get(2).and_then(Value::as_int).unwrap_or(0).max(0) as usize;
            let len = args.get(3).and_then(Value::as_int).map(|v| v.max(0) as usize)
                .unwrap_or_else(|| vm.heap.array_len(src).unwrap_or(0));
            if !bulk_copy(vm, src, off, buf, pos, len) {
                for i in 0..len {
                    let b = vm.heap.array_get(src, off + i).and_then(|v| v.as_int()).unwrap_or(0);
                    vm.heap.array_set(buf, pos + i, Value::Int(b as i8 as i32));
                }
            }
            set_pos(vm, pos + len);
            return Some(Some(Value::Ref(bb)));
        }
        // BULK put(ByteBuffer src) — drain src's remaining into this buffer.
        "put" if args.len() == 2 && matches!(args.get(1), Some(Value::Ref(_))) => {
            let src = match args.get(1) { Some(Value::Ref(h)) => *h, _ => return None };
            let sbuf = match vm.heap.get_field(src, "$buf") { Value::Array(h) => h, _ => return Some(Some(Value::Ref(bb))) };
            let spos = vm.heap.get_field_syn(src, crate::heap::Syn::Pos).as_int().unwrap_or(0).max(0) as usize;
            let slim = vm.heap.get_field(src, "$limit").as_int().unwrap_or(0).max(0) as usize;
            let len = slim.saturating_sub(spos);
            if !bulk_copy(vm, sbuf, spos, buf, pos, len) {
                for i in 0..len {
                    let b = vm.heap.array_get(sbuf, spos + i).and_then(|v| v.as_int()).unwrap_or(0);
                    vm.heap.array_set(buf, pos + i, Value::Int(b as i8 as i32));
                }
            }
            vm.heap.put_field_syn(src, crate::heap::Syn::Pos, Value::Int(slim as i32));
            set_pos(vm, pos + len);
            return Some(Some(Value::Ref(bb)));
        }
        "getInt" if args.len() == 1 => { let v = read_n(vm, pos, 4) as i32; set_pos(vm, pos + 4); Value::Int(v) }
        "getLong" if args.len() == 1 => { let v = read_n(vm, pos, 8); set_pos(vm, pos + 8); Value::Long(v) }
        "getShort" if args.len() == 1 => { let v = read_n(vm, pos, 2) as i16 as i32; set_pos(vm, pos + 2); Value::Int(v) }
        "getChar" if args.len() == 1 => { let v = read_n(vm, pos, 2) as i32; set_pos(vm, pos + 2); Value::Int(v) }
        "getFloat" if args.len() == 1 => { let v = f32::from_bits(read_n(vm, pos, 4) as u32); set_pos(vm, pos + 4); Value::Float(v) }
        "getDouble" if args.len() == 1 => { let v = f64::from_bits(read_n(vm, pos, 8) as u64); set_pos(vm, pos + 8); Value::Double(v) }
        // ABSOLUTE getX(int index): read AT the index, no position advance.
        "getInt" => Value::Int(read_n(vm, args.get(1).and_then(Value::as_int).unwrap_or(0).max(0) as usize, 4) as i32),
        "getLong" => Value::Long(read_n(vm, args.get(1).and_then(Value::as_int).unwrap_or(0).max(0) as usize, 8)),
        "getShort" => Value::Int(read_n(vm, args.get(1).and_then(Value::as_int).unwrap_or(0).max(0) as usize, 2) as i16 as i32),
        "getChar" => Value::Int(read_n(vm, args.get(1).and_then(Value::as_int).unwrap_or(0).max(0) as usize, 2) as i32),
        "getFloat" => Value::Float(f32::from_bits(read_n(vm, args.get(1).and_then(Value::as_int).unwrap_or(0).max(0) as usize, 4) as u32)),
        "getDouble" => Value::Double(f64::from_bits(read_n(vm, args.get(1).and_then(Value::as_int).unwrap_or(0).max(0) as usize, 8) as u64)),
        "put" | "putInt" | "putLong" | "putShort" | "putChar" | "putFloat" | "putDouble" => {
            return Some(buffer_put(vm, bb, buf, pos, little, name, args));
        }
        "duplicate" | "asReadOnlyBuffer" => Value::Ref(bb),
        // `slice()` is a buffer over the REMAINING bytes: capacity = remaining, position
        // 0. It used to return the receiver itself, so capacity and position were the
        // parent's and every offset a caller computed was wrong.
        //
        // ⚠A COPY, NOT A VIEW: writes through the slice do not reach the parent. The JDK
        // shares the array. Sharing needs an offset on every read/write helper here, and
        // that is a change to the whole buffer path — recorded rather than faked, the
        // same call made for `subList` (ledger row 275).
        "slice" => {
            let lim = field(vm, "$limit").max(0) as usize;
            // `array_bytes` CLONES THE WHOLE BACKING ARRAY and then throws away everything
            // outside `pos..lim` — a 1 MB copy to hand out a 20-byte view. `array_read_bytes`
            // slices. Identical result: it clamps `off`/`len` to the array exactly the way
            // the two `.min(bytes.len())` calls did, and yields empty when `pos >= lim`.
            let rest: Vec<u8> = match fast_decode_on()
                .then(|| vm.heap.array_read_bytes(buf, pos, lim.saturating_sub(pos)))
                .flatten()
            {
                Some(v) => v,
                None => {
                    let bytes = vm.heap.array_bytes(buf).unwrap_or_default();
                    bytes.get(pos.min(bytes.len())..lim.min(bytes.len())).unwrap_or(&[]).to_vec()
                }
            };
            let arr = vm.heap.alloc_bytes(&rest);
            let h = vm.heap.alloc("java/nio/ByteBuffer");
            vm.heap.put_field(h, "$buf", Value::Array(arr));
            vm.heap.put_field_syn(h, crate::heap::Syn::Pos, Value::Int(0));
            vm.heap.put_field(h, "$limit", Value::Int(rest.len() as i32));
            vm.heap.put_field(h, "$cap", Value::Int(rest.len() as i32));
            let ord = vm.heap.get_field(bb, "$order");
            vm.heap.put_field(h, "$order", ord);
            return Some(Some(Value::Ref(h)));
        }
        // `compact()` moves the remaining bytes to the front, sets position to the count
        // moved and limit to capacity — the read-then-refill idiom. Absent before.
        "compact" => {
            let lim = field(vm, "$limit").max(0) as usize;
            let cap = field(vm, "$cap").max(0) as usize;
            let blen = vm.heap.array_len(buf).unwrap_or(0);
            let start = pos.min(blen);
            let n = lim.saturating_sub(pos).min(blen.saturating_sub(start));
            // ⭐**COMPACT WAS QUADRATIC IN THE BUFFER, NOT THE LIVE BYTES.** It cloned the
            // ENTIRE array, shifted inside the clone, then wrote the ENTIRE array back —
            // and `compact()` is called once per refill in the read-drain-refill loop, so a
            // channel drained in small reads copied the whole buffer twice per iteration to
            // move a handful of leftover bytes. Only `[0..n)` ever changes: the bytes above
            // `n` were written back unmodified, because `copy_within` never touched them.
            // The `.min(cap)` keeps the one case where the old form wrote LESS than `n`
            // (a capacity below the live count) answering the same.
            let mut done = false;
            if fast_decode_on() {
                if let Some(chunk) = vm.heap.array_read_bytes(buf, start, n) {
                    let w = chunk.len().min(cap);
                    done = vm.heap.array_write_bytes(buf, 0, &chunk[..w]);
                }
            }
            if !done {
                let mut bytes = vm.heap.array_bytes(buf).unwrap_or_default();
                let end = (start + n).min(bytes.len());
                bytes.copy_within(start..end, 0);
                let _ = vm.heap.array_write_bytes(buf, 0, &bytes[..bytes.len().min(cap)]);
            }
            vm.heap.put_field_syn(bb, crate::heap::Syn::Pos, Value::Int(n as i32));
            vm.heap.put_field(bb, "$limit", Value::Int(cap as i32));
            return Some(Some(Value::Ref(bb)));
        }
        // Typed views over the remaining bytes (element size = 2/4/8). The view
        // shares $buf; its byte origin is the current position.
        "asIntBuffer" | "asLongBuffer" | "asShortBuffer" | "asFloatBuffer"
        | "asDoubleBuffer" | "asCharBuffer" => {
            let (cls, esz) = match name {
                "asIntBuffer" => ("java/nio/IntBuffer", 4),
                "asLongBuffer" => ("java/nio/LongBuffer", 8),
                "asShortBuffer" => ("java/nio/ShortBuffer", 2),
                "asFloatBuffer" => ("java/nio/FloatBuffer", 4),
                "asDoubleBuffer" => ("java/nio/DoubleBuffer", 8),
                _ => ("java/nio/CharBuffer", 2),
            };
            let limit = field(vm, "$limit") as usize;
            let n = limit.saturating_sub(pos) / esz;
            let h = vm.heap.alloc(cls);
            vm.heap.put_field(h, "$buf", Value::Array(buf));
            vm.heap.put_field(h, "$boff", Value::Int(pos as i32));
            vm.heap.put_field(h, "$esz", Value::Int(esz as i32));
            vm.heap.put_field_syn(h, crate::heap::Syn::Pos, Value::Int(0));
            vm.heap.put_field(h, "$limit", Value::Int(n as i32));
            vm.heap.put_field(h, "$cap", Value::Int(n as i32));
            vm.heap.put_field(h, "$order", vm.heap.get_field(bb, "$order"));
            Value::Ref(h)
        }
        _ => return None,
    };
    Some(Some(ret))
}

/// IntBuffer/LongBuffer/… view over a byte array ($buf) at byte origin $boff,
/// element size $esz, position/limit/capacity in ELEMENTS. Backs ByteBuffer's
/// asIntBuffer() etc.
pub(crate) fn typed_buffer_dispatch(vm: &mut Vm, owner: &str, name: &str, args: &[Value]) -> Option<Option<Value>> {
    let tb = match args.first() { Some(Value::Ref(h)) => *h, _ => return None };
    let fi = |vm: &Vm, f: &str| vm.heap.get_field(tb, f).as_int().unwrap_or(0);
    let buf = match vm.heap.get_field(tb, "$buf") { Value::Array(h) => h, _ => return Some(Some(Value::Null)) };
    let esz = fi(vm, "$esz").max(1) as usize;
    let boff = fi(vm, "$boff") as usize;
    let pos = fi(vm, "$pos") as usize;
    let little = matches!(vm.heap.get_field(tb, "$order"), Value::Str(s) if s.as_ref() == "LE");
    let is_fp = matches!(owner, "java/nio/FloatBuffer" | "java/nio/DoubleBuffer");
    // Same register-width fold as `ByteBuffer`'s `read_n`: an `IntBuffer.get()` used to
    // malloc a four-byte vector per element, so walking an int view allocated once per
    // element read. `esz` is at most 8.
    let read = |vm: &Vm, elem: usize| -> i64 {
        let at = boff + elem * esz;
        let mut acc = 0i64;
        for i in 0..esz {
            let idx = if little { esz - 1 - i } else { i };
            let b = vm.heap.array_get(buf, at + idx).and_then(|v| v.as_int()).unwrap_or(0) as u8;
            acc = (acc << 8) | b as i64;
        }
        acc
    };
    let write = |vm: &mut Vm, elem: usize, val: i64| {
        let at = boff + elem * esz;
        for i in 0..esz {
            let shift = if little { i } else { esz - 1 - i };
            let byte = ((val >> (shift * 8)) & 0xff) as i32;
            vm.heap.array_set(buf, at + i, Value::Int(byte));
        }
    };
    let to_val = |raw: i64| -> Value {
        match owner {
            "java/nio/LongBuffer" => Value::Long(raw),
            "java/nio/FloatBuffer" => Value::Float(f32::from_bits(raw as u32)),
            "java/nio/DoubleBuffer" => Value::Double(f64::from_bits(raw as u64)),
            "java/nio/ShortBuffer" => Value::Int(raw as i16 as i32),
            "java/nio/CharBuffer" => Value::Int(raw as u16 as i32),
            _ => Value::Int(raw as i32),
        }
    };
    let arg_raw = |v: Option<&Value>| -> i64 {
        match (is_fp, esz, v) {
            (true, 4, Some(x)) => x.as_f32().to_bits() as i64,
            (true, 8, Some(x)) => x.as_f64().to_bits() as i64,
            (_, _, Some(x)) => x.as_long().unwrap_or_else(|| x.as_int().unwrap_or(0) as i64),
            _ => 0,
        }
    };
    let set_pos = |vm: &mut Vm, p: usize| vm.heap.put_field_syn(tb, crate::heap::Syn::Pos, Value::Int(p as i32));
    let ret = match name {
        "position" if args.len() == 1 => Value::Int(pos as i32),
        "position" => { set_pos(vm, args.get(1).and_then(Value::as_int).unwrap_or(0) as usize); return Some(Some(Value::Ref(tb))); }
        "limit" if args.len() == 1 => Value::Int(fi(vm, "$limit")),
        "limit" => { vm.heap.put_field(tb, "$limit", args.get(1).cloned().unwrap_or(Value::Int(0))); return Some(Some(Value::Ref(tb))); }
        "capacity" => Value::Int(fi(vm, "$cap")),
        "remaining" => Value::Int(fi(vm, "$limit") - pos as i32),
        "hasRemaining" => Value::Int(((fi(vm, "$limit") as usize) > pos) as i32),
        "clear" => { set_pos(vm, 0); let c = fi(vm, "$cap"); vm.heap.put_field(tb, "$limit", Value::Int(c)); return Some(Some(Value::Ref(tb))); }
        "flip" => { vm.heap.put_field(tb, "$limit", Value::Int(pos as i32)); set_pos(vm, 0); return Some(Some(Value::Ref(tb))); }
        "rewind" => { set_pos(vm, 0); return Some(Some(Value::Ref(tb))); }
        "get" if args.len() == 1 => { let v = to_val(read(vm, pos)); set_pos(vm, pos + 1); v }
        "get" if matches!(args.get(1), Some(Value::Int(_))) => { let i = args.get(1).and_then(Value::as_int).unwrap_or(0) as usize; to_val(read(vm, i)) }
        "put" if args.len() == 2 => { let raw = arg_raw(args.get(1)); write(vm, pos, raw); set_pos(vm, pos + 1); return Some(Some(Value::Ref(tb))); }
        "put" if args.len() >= 3 => { let i = args.get(1).and_then(Value::as_int).unwrap_or(0) as usize; let raw = arg_raw(args.get(2)); write(vm, i, raw); return Some(Some(Value::Ref(tb))); }
        "duplicate" | "slice" | "asReadOnlyBuffer" => Value::Ref(tb),
        _ => return None,
    };
    Some(Some(ret))
}

pub(crate) fn buffer_put(vm: &mut Vm, bb: u32, buf: u32, pos: usize, little: bool, name: &str, args: &[Value]) -> Option<Value> {
    let n = match name {
        "put" => 1,
        "putInt" | "putFloat" => 4,
        "putShort" | "putChar" => 2,
        "putLong" | "putDouble" => 8,
        _ => return None,
    };
    // ABSOLUTE form putX(int index, val) has an extra leading index arg vs the
    // RELATIVE putX(val): args[0] is the receiver, so relative = 2 args, absolute
    // = 3. Absolute writes AT the index and does NOT advance position; relative
    // writes at position and advances. Treating absolute as relative wrote the
    // index as the value at the wrong offset (e.g. ByteBuffer.wrap(a).putInt(12,x)
    // corrupted a[0] with 12 — a generic nio bug).
    let absolute = args.len() >= 3;
    let val_arg = if absolute { args.get(2) } else { args.get(1) };
    let at = if absolute {
        args.get(1).and_then(Value::as_int).unwrap_or(0).max(0) as usize
    } else {
        pos
    };
    let val: i64 = match name {
        "putLong" => val_arg.and_then(Value::as_long).unwrap_or(0),
        "putFloat" => val_arg.map(|v| v.as_f32().to_bits() as i64).unwrap_or(0),
        "putDouble" => val_arg.map(|v| v.as_f64().to_bits() as i64).unwrap_or(0),
        _ => val_arg.and_then(Value::as_int).unwrap_or(0) as i64,
    };
    // Stack, not heap: this built a `Vec<u8>` of at most eight bytes per `putX` and then
    // stored them one boxed `Value` at a time. `array_write_bytes` is a `copy_from_slice`
    // into the same backing `Vec<u8>` and stores the identical raw byte the `as u8`
    // truncation inside `array_set` produced; it refuses (writing nothing) when `buf` is
    // not a byte array or the run does not fit, and the element loop then runs as before.
    let mut raw = [0u8; 8];
    for (i, slot) in raw[..n].iter_mut().enumerate() {
        let shift = if little { i } else { n - 1 - i };
        *slot = ((val >> (shift * 8)) & 0xff) as u8;
    }
    if !(fast_decode_on() && vm.heap.array_write_bytes(buf, at, &raw[..n])) {
        for (i, b) in raw[..n].iter().enumerate() {
            vm.heap.array_set(buf, at + i, Value::Int(*b as i8 as i32));
        }
    }
    if !absolute {
        vm.heap.put_field_syn(bb, crate::heap::Syn::Pos, Value::Int((pos + n) as i32));
    }
    Some(Value::Ref(bb)) // chainable
}
