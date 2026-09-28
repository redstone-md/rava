//! Member keys: how a `(name, descriptor)` pair becomes a table slot, and how
//! [`crate::ClassFile::method`] finds one.
//!
//! Split out of `lib.rs` (which was 1011 lines against the project's 500-line guidance) as a
//! whole unit rather than by line count: everything here answers ONE question — "which
//! `MemberInfo` does this key name?" — and nothing else in the crate calls into it except
//! `ClassFile::method`. `lib.rs` keeps the parser.
//!
// ─────────────────────────────────────────────────────────────────────────────
// MEMBER-KEY BYTE WORK — the two spots in this crate where the compiler is
// physically unable to emit the code we want.
//
// A method is identified by `(name, descriptor)`: two short byte strings
// (`<init>`, `(Ljava/lang/Object;)Ljava/lang/Object;`). Every lookup does exactly
// two things with them — FOLD them into a table index, and VERIFY the candidate
// slot byte-for-byte — and the index is BUILT by folding every member of the
// class, so the same two operations run once per method per class as well.
//
//   FOLD    `crc32q` (SSE4.2): one instruction, one uop, 3-cycle latency, eats 8
//           bytes. The scalar fold below it is rotate+xor+imul — three uops and
//           ~5 cycles of dependent latency for the same 8 bytes — plus, for the
//           1..7-byte tail, a zeroed stack buffer and a variable-length
//           `copy_from_slice`. LLVM will never synthesise `crc32` for a hash it
//           did not author, so `-C target-cpu=native` cannot reach this.
//
//   VERIFY  an inline SSE2 compare, and below 16 bytes an overlapping-load
//           ladder. `a == b` on `&str` lowers to the `bcmp`/`memcmp` IMPORT THUNK
//           on MSVC — a real call with argument spills — because the length is a
//           runtime value; LLVM only inlines the compare for constant lengths.
//           Keys here are 1-60 bytes, i.e. exactly the range where the call is
//           most of the cost.
//
// BIT-IDENTITY. The verify is exact byte equality over the whole key (coverage
// proof on `inline_bytes_eq`), so `ClassFile::method` returns the SAME
// `&MemberInfo` it returned before. The fold is a pure ACCELERATOR — it only
// decides which slot is probed first, and every hit is verified against the real
// bytes — so changing it cannot change which method is found, only how many slots
// are touched getting there. Build and lookup read the same mode, so the table is
// never probed with a fold other than the one that built it.
//
// COST IF REMOVED: `RAVA_NO_SIMD_CP=1` restores the scalar fold and the `memcmp`
// verify byte for byte, on a path that is one-shot per class and that no JIT ever
// amortises.
// ─────────────────────────────────────────────────────────────────────────────

use crate::{FastMap, MemberInfo, FX_SEED};

/// Kill switch for the one behaviour this module added on top of the fold/verify work:
/// the fold mode travelling INSIDE the table it built.
///
/// `RAVA_NO_FAST_MEMBERS=1` restores the previous shape exactly — every lookup re-reads the
/// process-wide [`member_key_mode`] `OnceLock` instead of the byte the table carries. Same
/// answer either way (the mode is a process constant), so this is an A/B lever over ONE
/// variable: an acquire load per lookup versus a byte already in the cache line the probe is
/// about to touch.
///
/// DEFINED LOCALLY, not imported from `rava-interp`: that crate depends on this one, so calling
/// its `flags::emit_more_disabled()` from here would be a dependency cycle. `RAVA_NO_EMIT_MORE`
/// is honoured as the block switch so a single same-binary A/B still prices this with the rest.
///
/// MEMOISED: on Windows `var_os` is `RtlQueryEnvironmentVariable` — a linear walk of the
/// environment block plus an allocation — and this is consulted once per class that is ever
/// looked up, ~15 000 times a boot.
#[inline]
fn fast_members() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_MEMBERS").is_none()
            && std::env::var_os("RAVA_NO_EMIT_MORE").is_none()
    })
}

/// FxHash over a byte slice, EIGHT BYTES AT A TIME.
///
/// [`crate::FxHasher`] is byte-at-a-time because `std`'s `Hash` for `str` hands it bytes.
/// A method descriptor is routinely 30-40 bytes (`(Ljava/lang/Object;)Ljava/lang/Object;` is
/// 38), so hashing one byte-wise is ~40 dependent multiplies on a path taken on every method
/// resolution; word-wise it is 5. Correctness does not depend on the hash at all — every hit is
/// verified against the real name and descriptor — so the mixing only has to spread indices.
#[inline]
fn fx_words(mut h: u64, bytes: &[u8]) -> u64 {
    let mut chunks = bytes.chunks_exact(8);
    for c in &mut chunks {
        let w = u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
        h = (h.rotate_left(5) ^ w).wrapping_mul(FX_SEED);
    }
    let rem = chunks.remainder();
    if !rem.is_empty() {
        let mut buf = [0u8; 8];
        buf[..rem.len()].copy_from_slice(rem);
        h = (h.rotate_left(5) ^ u64::from_le_bytes(buf)).wrapping_mul(FX_SEED);
    }
    h
}

/// Scalar fold + `memcmp` verify. What `RAVA_NO_SIMD_CP=1` restores.
const KEY_LEGACY: u8 = 0;
/// Inline verify, scalar fold. Only reachable on a portable build for a pre-SSE4.2 part.
const KEY_INLINE_EQ: u8 = 1;
/// DEFAULT on x86_64: inline verify + `crc32q` fold.
const KEY_INLINE_EQ_CRC: u8 = 2;

/// Which member-key implementation this process uses.
///
/// A PROCESS CONSTANT, and that is load-bearing rather than a micro-optimisation: an index built
/// with one fold and probed with another would still return the right method (every hit is
/// verified) but every lookup would miss its home slot and walk the table. `OnceLock` gives that
/// for free and keeps `var_os` — a linear walk of the Windows environment block plus an
/// allocation — off a path taken once per class, ~15 000 times a boot.
///
/// Read ONCE PER TABLE by default; see [`MethodIndex::Hashed`].
#[inline]
fn member_key_mode() -> u8 {
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(detect_key_mode)
}

#[allow(unreachable_code)]
fn detect_key_mode() -> u8 {
    // `fast_cfmethod` is the block switch (`RAVA_NO_EMIT_MORE`) plus this mechanism's own
    // `RAVA_NO_FAST_CFMETHOD`; `RAVA_NO_SIMD_CP` is the per-change switch for the vector work.
    if std::env::var_os("RAVA_NO_SIMD_CP").is_some() || !crate::flags::fast_cfmethod() {
        return KEY_LEGACY;
    }
    #[cfg(target_arch = "x86_64")]
    {
        // SSE2 — the verify — is baseline on x86_64 and needs no probe. SSE4.2 — the fold —
        // does. Every x86_64 part since Nehalem (2008) has it, so the split exists so a
        // portable build stays correct, not because the fallback is expected to fire.
        if is_x86_feature_detected!("sse4.2") {
            return KEY_INLINE_EQ_CRC;
        }
        return KEY_INLINE_EQ;
    }
    KEY_LEGACY
}

/// Unaligned load of `T` at `p + off`.
///
/// SAFETY: every caller proves `off + size_of::<T>() <= len` from the length test that guards
/// its branch. `read_unaligned` is required — a member key is a `String` body at an arbitrary
/// malloc offset, and the ladder below deliberately reads at `len - k`.
#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn rd<T: Copy>(p: *const u8, off: usize) -> T {
    (p.add(off) as *const T).read_unaligned()
}

/// Byte equality without the `memcmp` call.
///
/// COVERAGE PROOF — this is why it is bit-identical to `a == b`:
///  * lengths are compared first, so a length mismatch is rejected exactly as `==` does;
///  * for `n >= 16`, the aligned-by-index chunks cover `[0, 16*floor(n/16))` and the final
///    OVERLAPPING vector covers `[n-16, n)`; since `n - 16*floor(n/16) = n mod 16 < 16`, the two
///    ranges meet or overlap, so every byte of `[0, n)` is compared. Re-comparing bytes already
///    found equal cannot change the verdict;
///  * for `n < 16` the ladder compares `[0, k)` and `[n-k, n)` with `k` = 8, 4 or 2 chosen so
///    that `n <= 2k` — the two windows again cover `[0, n)` with no gap;
///  * `n == 1` compares the single byte, `n == 0` is trivially equal.
/// Integer equality of two identically-sized loads is byte equality of the loaded ranges
/// regardless of endianness, because both sides are loaded the same way.
///
/// NEVER READS PAST THE END: every load offset is `i` with `i + w <= n` or exactly `n - w` with
/// `n >= w`, inside BOTH slices. No 16-byte load is ever issued for a slice shorter than 16, so
/// there is no page-crossing tail read.
#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn inline_bytes_eq(a: &[u8], b: &[u8]) -> bool {
    use core::arch::x86_64::{__m128i, _mm_cmpeq_epi8, _mm_loadu_si128, _mm_movemask_epi8};
    let n = a.len();
    if n != b.len() {
        return false;
    }
    let (p, q) = (a.as_ptr(), b.as_ptr());
    // SAFETY: see the coverage proof above — every load is inside both slices.
    unsafe {
        if n >= 16 {
            let mut i = 0usize;
            while i + 16 <= n {
                let x = _mm_loadu_si128(p.add(i) as *const __m128i);
                let y = _mm_loadu_si128(q.add(i) as *const __m128i);
                if _mm_movemask_epi8(_mm_cmpeq_epi8(x, y)) != 0xFFFF {
                    return false;
                }
                i += 16;
            }
            if i == n {
                return true;
            }
            let x = _mm_loadu_si128(p.add(n - 16) as *const __m128i);
            let y = _mm_loadu_si128(q.add(n - 16) as *const __m128i);
            return _mm_movemask_epi8(_mm_cmpeq_epi8(x, y)) == 0xFFFF;
        }
        if n >= 8 {
            return rd::<u64>(p, 0) == rd::<u64>(q, 0)
                && rd::<u64>(p, n - 8) == rd::<u64>(q, n - 8);
        }
        if n >= 4 {
            return rd::<u32>(p, 0) == rd::<u32>(q, 0)
                && rd::<u32>(p, n - 4) == rd::<u32>(q, n - 4);
        }
        if n >= 2 {
            return rd::<u16>(p, 0) == rd::<u16>(q, 0)
                && rd::<u16>(p, n - 2) == rd::<u16>(q, n - 2);
        }
        if n == 1 {
            return *p == *q;
        }
        true
    }
}

/// Fold `bytes` into the running CRC, 8 bytes per instruction.
///
/// The 1..7-byte tail is folded by an OVERLAPPING read rather than a zero-filled stack buffer:
/// the trailing 8 bytes are re-read from `n - 8`, which double-counts a few bytes. That is fine
/// for a hash — it only has to be deterministic and to spread — and it removes the buffer, the
/// memset and the variable-length copy that dominate the scalar tail for keys this short.
/// Below 8 bytes the same trick steps down through 4/2/1-byte CRC forms.
///
/// SAFETY: caller must have observed `is_x86_feature_detected!("sse4.2")`. Every load is bounded
/// by the branch that guards it, exactly as in `inline_bytes_eq`.
#[cfg(target_arch = "x86_64")]
#[inline]
#[target_feature(enable = "sse4.2")]
unsafe fn crc_fold(mut h: u64, bytes: &[u8]) -> u64 {
    use core::arch::x86_64::{_mm_crc32_u16, _mm_crc32_u32, _mm_crc32_u64, _mm_crc32_u8};
    let n = bytes.len();
    let p = bytes.as_ptr();
    if n >= 8 {
        let mut i = 0usize;
        while i + 8 <= n {
            h = _mm_crc32_u64(h, rd::<u64>(p, i));
            i += 8;
        }
        if i < n {
            h = _mm_crc32_u64(h, rd::<u64>(p, n - 8));
        }
    } else if n >= 4 {
        let t = _mm_crc32_u32(h as u32, rd::<u32>(p, 0));
        h = _mm_crc32_u32(t, rd::<u32>(p, n - 4)) as u64;
    } else if n >= 2 {
        let t = _mm_crc32_u16(h as u32, rd::<u16>(p, 0));
        h = _mm_crc32_u16(t, rd::<u16>(p, n - 2)) as u64;
    } else if n == 1 {
        h = _mm_crc32_u8(h as u32, *p) as u64;
    }
    h
}

/// SAFETY: caller must have observed `is_x86_feature_detected!("sse4.2")`.
#[cfg(target_arch = "x86_64")]
#[inline]
#[target_feature(enable = "sse4.2")]
unsafe fn crc_member_hash(seed: u64, name: &[u8], descriptor: &[u8]) -> u32 {
    use core::arch::x86_64::_mm_crc32_u64;
    // `crc32q` consumes all 64 bits of its operand, so the seed carries BOTH lengths (32 bits
    // each) into the state — a name/descriptor split cannot alias another one, same as scalar.
    let h = _mm_crc32_u64(0, seed);
    let h = crc_fold(h, name);
    let h = crc_fold(h, descriptor);
    // LOW half, not high: `crc32q` leaves the running CRC in the low 32 bits and zeroes the top
    // half, so the scalar fold's `>> 32` would hand back a constant 0 here and collapse the
    // whole table onto one probe chain. CRC32C's low bits are well distributed, which is what
    // the table's mask consumes.
    h as u32
}

/// The whole lookup key — name AND descriptor — folded into one 32-bit value.
///
/// The old map hashed the NAME only and then walked a `Vec` of overloads doing a full `memcmp`
/// of each candidate descriptor. Folding both in means the common case probes once and verifies
/// once. The two lengths seed the hash so a name/descriptor split cannot alias another one.
///
/// `mode` is passed in rather than read here: it is a process constant, and reading the
/// `OnceLock` per member would put an acquire load inside `MethodIndex::build`'s insert loop —
/// the shape that measured −15% elsewhere in this engine, because an atomic is a barrier LLVM
/// cannot hoist out of the loop, not because the load itself is expensive.
#[inline]
fn member_hash(mode: u8, name: &str, descriptor: &str) -> u32 {
    let seed = ((name.len() as u64) << 32) ^ descriptor.len() as u64;
    #[cfg(target_arch = "x86_64")]
    {
        if mode == KEY_INLINE_EQ_CRC {
            // SAFETY: `KEY_INLINE_EQ_CRC` is only ever produced by `detect_key_mode` AFTER
            // `is_x86_feature_detected!("sse4.2")` returned true, and the mode is a process
            // constant, so this cannot be reached on a part without the instruction.
            return unsafe { crc_member_hash(seed, name.as_bytes(), descriptor.as_bytes()) };
        }
    }
    let _ = mode;
    let h = fx_words(fx_words(seed, name.as_bytes()), descriptor.as_bytes());
    // Take the HIGH half: FxHash's mixing pushes entropy upward, and the low bits are what the
    // table masks with.
    (h >> 32) as u32
}

/// Verify a probed slot against the wanted key. Byte-identical to `candidate == wanted`.
#[inline]
fn key_eq(mode: u8, candidate: &str, wanted: &str) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        if mode != KEY_LEGACY {
            return inline_bytes_eq(candidate.as_bytes(), wanted.as_bytes());
        }
    }
    let _ = mode;
    candidate == wanted
}

/// The open-addressed probe, factored out of [`MethodIndex::find`] so the tests can drive it at
/// every `mode` without restating the walk (a second copy of it in a test would be free to drift
/// away from the one that ships).
#[inline]
fn lookup<'a>(
    slots: &[MethodSlot],
    methods: &'a [MemberInfo],
    mode: u8,
    name: &str,
    descriptor: &str,
) -> Option<&'a MemberInfo> {
    let mask = slots.len() - 1;
    let h = member_hash(mode, name, descriptor);
    let mut p = (h as usize) & mask;
    loop {
        // SAFETY-BY-CONSTRUCTION: `build` leaves the table under 0.67 full, so an empty slot
        // always exists and this walk terminates.
        let (sh, si) = slots[p];
        if si == 0 {
            return None;
        }
        if sh == h {
            let mi = &methods[si as usize - 1];
            // The hash is an accelerator, never the answer: confirm the real bytes before
            // returning, so a collision costs a compare, not a wrong method.
            if key_eq(mode, &mi.name, name) && key_eq(mode, &mi.descriptor, descriptor) {
                return Some(mi);
            }
        }
        p = (p + 1) & mask;
    }
}

/// One slot of the method index: `(hash, method_index + 1)`.
///
/// `+1` so that `0` is a usable EMPTY sentinel without a second field — method indices are
/// 0-based and a class cannot hold `u32::MAX` methods (JVMS caps `methods_count` at 65535).
type MethodSlot = (u32, u32);

/// How `ClassFile::method` finds a method. Two shapes so the kill switch restores the exact
/// prior structure rather than an approximation of it — an A/B that changes two things at once
/// prices neither.
pub(crate) enum MethodIndex {
    /// DEFAULT. One contiguous open-addressed table, ONE allocation for the whole class.
    ///
    /// `mode` is the fold this table was BUILT with, carried BESIDE the table instead of being
    /// re-derived per lookup. The invariant "never probe a table with a fold other than the one
    /// that built it" is the reason `member_key_mode` is a process constant at all; storing the
    /// answer here makes that invariant STRUCTURAL — the fold and the slots it produced travel
    /// together and cannot be separated — and drops one `OnceLock` acquire out of every
    /// `ClassFile::method` call, a 1.38% row of the callback profile. An acquire load on x86 is
    /// a plain `mov`; what it costs is that LLVM may not hoist or reorder across it, which is
    /// the same shape that priced at −15% on the interpreter's push path.
    ///
    /// `None` is what `RAVA_NO_FAST_MEMBERS=1` stores: ask the global on every lookup, exactly
    /// as before. Same answer, one variable, so the A/B prices only this.
    ///
    /// SIZE — CHECKED, NOT ASSUMED, AND IT IS NOT FREE. The enum was **16 bytes** and is now
    /// **24**. The old shape fit in a bare `Box<[MethodSlot]>` because `Legacy`'s discriminant
    /// was NICHE-ENCODED as a null pointer at offset 0; a second field in `Hashed` forces a real
    /// tag and a third word. `OnceLock<MethodIndex>` therefore goes 24 -> 32, so every
    /// `ClassFile` is 8 bytes bigger: **~120 KB across a 15 000-class boot**, against one
    /// acquire load removed from every `ClassFile::method` call — 1.38% of the callback profile.
    /// `Option<u8>` costs nothing over a plain `u8` here (both land at offset 16), so the kill
    /// switch itself is free; the 8 bytes are the price of carrying the mode at all.
    Hashed {
        mode: Option<u8>,
        slots: Box<[MethodSlot]>,
    },
    /// `RAVA_NO_FAST_CLASSFILE=1`. The original name -> [(descriptor, index)] map, which
    /// allocated a `Box<str>` COPY of every method name and every method descriptor the
    /// `ClassFile` already owned — ~3 allocations per method, ~1.5 M across a boot. Boxed so
    /// this enum stays 24 bytes instead of the `HashMap`'s 48; `ClassFile` is instantiated
    /// ~15 000 times a boot and the fast arm must not pay for the dead one.
    Legacy(Box<FastMap<Box<str>, Vec<(Box<str>, usize)>>>),
}

impl MethodIndex {
    /// Build the index for `methods`. Called once per class, from a `OnceLock`.
    pub(crate) fn build(methods: &[MemberInfo]) -> MethodIndex {
        if !crate::flags::fast_classfile() {
            let mut m: FastMap<Box<str>, Vec<(Box<str>, usize)>> =
                FastMap::with_capacity_and_hasher(methods.len(), Default::default());
            for (i, mi) in methods.iter().enumerate() {
                m.entry(mi.name.as_str().into())
                    .or_default()
                    .push((mi.descriptor.as_str().into(), i));
            }
            return MethodIndex::Legacy(Box::new(m));
        }
        // ONE read of the memoised mode for the whole table, not one per member: the fold must
        // be the same one `find` will use, and hoisting the acquire load out of the insert
        // loop is the difference between a loop LLVM can software-pipeline and one it cannot.
        let mode = member_key_mode();
        MethodIndex::Hashed {
            mode: if fast_members() { Some(mode) } else { None },
            slots: Self::fill(methods, mode),
        }
    }

    /// Lay out the open-addressed table.
    ///
    /// SIZE: `slots.len() * 8` bytes, ONE allocation, contiguous. `len + len/2 + 1` rounded up
    /// to a power of two keeps the load factor between 0.34 and 0.67, which is what makes the
    /// linear probe below terminate (there is always at least one empty slot). Concretely: a
    /// 30-method class costs 64 slots = **512 bytes**; a 200-method class 256 slots = **2 KB**.
    /// The map it replaces was a hashbrown table of 40-byte entries PLUS a `Vec` header block
    /// PLUS two heap string copies per method, all scattered — strictly larger and strictly
    /// less local, which is why this is not the "cache that stopped fitting in L1" mistake.
    fn fill(methods: &[MemberInfo], mode: u8) -> Box<[MethodSlot]> {
        // `synthesize` accepts an arbitrary `Vec`, so clamp rather than trust the JVMS 65535
        // cap: `n` bounds both the `+1` index encoding and the `next_power_of_two` below.
        let n = methods.len().min(u32::MAX as usize - 1);
        let cap = (n + (n >> 1) + 1).next_power_of_two().max(8);
        let mask = cap - 1;
        let mut slots = vec![(0u32, 0u32); cap].into_boxed_slice();
        for (i, mi) in methods.iter().take(n).enumerate() {
            let h = member_hash(mode, &mi.name, &mi.descriptor);
            let mut p = (h as usize) & mask;
            // Linear probe. A duplicate (name, descriptor) — illegal per JVMS 4.6 but not
            // rejected here — lands AFTER the first, so a lookup still returns the lowest
            // index, exactly as the old `Vec::push` order plus `Iterator::find` did.
            while slots[p].1 != 0 {
                p = (p + 1) & mask;
            }
            slots[p] = (h, i as u32 + 1);
        }
        slots
    }

    /// Resolve `(name, descriptor)` against `methods`, which MUST be the slice this index was
    /// built from.
    #[inline]
    pub(crate) fn find<'a>(
        &self,
        methods: &'a [MemberInfo],
        name: &str,
        descriptor: &str,
    ) -> Option<&'a MemberInfo> {
        match self {
            // `mode` is the byte `build` folded this table with — see the variant's doc for why
            // it lives here rather than in a `OnceLock` consulted per call. `None` only when
            // `RAVA_NO_FAST_MEMBERS=1` restored the old read.
            MethodIndex::Hashed { mode, slots } => {
                let mode = match *mode {
                    Some(m) => m,
                    None => member_key_mode(),
                };
                lookup(slots, methods, mode, name, descriptor)
            }
            // Name hit (O(1)) then a tiny linear over overloads (usually one).
            MethodIndex::Legacy(map) => map
                .get(name)?
                .iter()
                .find(|(d, _)| d.as_ref() == descriptor)
                .map(|(_, i)| &methods[*i]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClassFile, Constant, ConstantPool};

    fn member(name: &str, descriptor: &str) -> MemberInfo {
        MemberInfo {
            access_flags: 0,
            name: name.to_string(),
            descriptor: descriptor.to_string(),
            code: None,
        }
    }

    /// The index is a pure accelerator: for every member it must return THAT member, and for
    /// anything else it must return `None`. Overloads (same name, different descriptor) are the
    /// case the old name-keyed map handled with a linear walk and the new one folds into the key.
    #[test]
    fn hashed_index_finds_every_overload_and_nothing_else() {
        let names: Vec<MemberInfo> = vec![
            member("<init>", "()V"),
            member("<clinit>", "()V"),
            member("get", "(Ljava/lang/Object;)Ljava/lang/Object;"),
            member("get", "(I)Ljava/lang/Object;"),
            member("get", "()Ljava/lang/Object;"),
            member("put", "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;"),
            member("size", "()I"),
            member("equals", "(Ljava/lang/Object;)Z"),
        ];
        let cf = ClassFile::synthesize(
            ConstantPool::from_entries(vec![Constant::Unusable]),
            0,
            "X".to_string(),
            None,
            Vec::new(),
            names.clone(),
        );
        for want in &names {
            let got = cf.method(&want.name, &want.descriptor).expect("present");
            assert_eq!(got.name, want.name);
            assert_eq!(got.descriptor, want.descriptor);
        }
        assert!(cf.method("get", "(J)Ljava/lang/Object;").is_none());
        assert!(cf.method("size", "()J").is_none());
        assert!(cf.method("absent", "()V").is_none());
    }

    /// A class with no methods must not probe off the end of an eight-slot table.
    #[test]
    fn empty_method_table_misses() {
        let cf = ClassFile::synthesize(
            ConstantPool::from_entries(vec![Constant::Unusable]),
            0,
            "X".to_string(),
            None,
            Vec::new(),
            Vec::new(),
        );
        assert!(cf.method("<init>", "()V").is_none());
    }

    /// The vector verify must agree with `==` on EVERY length and EVERY differing position —
    /// including the lengths where the overlapping-load ladder switches width (1, 2, 4, 8, 16)
    /// and the ones where the 16-byte loop leaves a partial tail (17..31, 33..).
    ///
    /// The slices are carved out of one buffer at a rolling offset so the two pointers are
    /// mutually unaligned, which is what a `String` body at an arbitrary malloc offset looks
    /// like and what `read_unaligned`/`loadu` have to survive.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn inline_verify_is_byte_identical_to_str_eq() {
        let pool: Vec<u8> = (0u16..256).map(|i| b'a' + (i % 26) as u8).collect();
        for off in 0..8usize {
            for n in 0..=72usize {
                let a = &pool[off..off + n];
                let mirror = a.to_vec();
                assert!(inline_bytes_eq(a, &mirror), "equal, off {off} len {n}");
                // Every single-byte difference must be caught, at every position.
                for k in 0..n {
                    let mut b = mirror.clone();
                    b[k] ^= 0xFF;
                    assert_eq!(
                        a == b.as_slice(),
                        inline_bytes_eq(a, &b),
                        "off {off} len {n} pos {k}"
                    );
                }
                // Length mismatch, both directions.
                if n > 0 {
                    assert!(!inline_bytes_eq(a, &mirror[..n - 1]));
                    assert!(!inline_bytes_eq(&mirror[..n - 1], a));
                }
            }
        }
    }

    /// A `&str` sitting at the very end of its allocation is the case where a naive 16-byte tail
    /// load walks off the page. Nothing here can prove the absence of a fault by itself, but
    /// driving every short length through a slice whose end IS the allocation's end is what a
    /// page-guard run (or ASAN) needs in order to catch it.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn inline_verify_never_reads_past_the_allocation() {
        for n in 0..=40usize {
            let a: Vec<u8> = (0..n).map(|i| b'x' ^ (i as u8)).collect();
            let b = a.clone();
            assert!(inline_bytes_eq(&a, &b));
            let mut c = a.clone();
            if n > 0 {
                c[n - 1] ^= 1;
                assert!(!inline_bytes_eq(&a, &c));
            }
        }
    }

    /// The fold is an accelerator, but it must at least be a FUNCTION: same key, same value,
    /// every mode. (A fold that varied per call would build a table nothing could probe.)
    #[test]
    fn member_hash_is_a_function_of_the_key() {
        let keys = [
            ("", "()V"),
            ("f", "()V"),
            ("<init>", "()V"),
            ("hashCode", "()I"),
            ("get", "(Ljava/lang/Object;)Ljava/lang/Object;"),
            ("putIfAbsent", "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;"),
        ];
        for mode in [KEY_LEGACY, KEY_INLINE_EQ, KEY_INLINE_EQ_CRC] {
            for (n, d) in keys {
                assert_eq!(member_hash(mode, n, d), member_hash(mode, n, d));
            }
        }
    }

    /// End-to-end at EVERY mode: build the table with a mode and probe it with the same mode.
    /// Whichever fold is in play, the answer must be the exact `MemberInfo` and nothing else —
    /// that is the whole bit-identity claim for this change. `fill` is the shipping layout, so
    /// the test drives it rather than restating the insert loop.
    #[test]
    fn every_key_mode_finds_the_same_members() {
        let members = vec![
            member("<init>", "()V"),
            member("<clinit>", "()V"),
            member("get", "(Ljava/lang/Object;)Ljava/lang/Object;"),
            member("get", "(I)Ljava/lang/Object;"),
            member("get", "()Ljava/lang/Object;"),
            member("put", "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;"),
            member("size", "()I"),
            member("equals", "(Ljava/lang/Object;)Z"),
            // Two keys differing only in the LAST byte of a >16-byte descriptor: the case the
            // 16-byte chunk loop only separates via its overlapping tail vector.
            member("x", "(Ljava/lang/String;Ljava/lang/String;)I"),
            member("x", "(Ljava/lang/String;Ljava/lang/String;)J"),
        ];
        for mode in [KEY_LEGACY, KEY_INLINE_EQ, KEY_INLINE_EQ_CRC] {
            let slots = MethodIndex::fill(&members, mode);
            for want in &members {
                let got = lookup(&slots, &members, mode, &want.name, &want.descriptor)
                    .unwrap_or_else(|| panic!("mode {mode}: {} missing", want.name));
                assert_eq!(got.name, want.name, "mode {mode}");
                assert_eq!(got.descriptor, want.descriptor, "mode {mode}");
            }
            assert!(lookup(&slots, &members, mode, "get", "(J)Ljava/lang/Object;").is_none());
            assert!(lookup(&slots, &members, mode, "x", "(Ljava/lang/String;Ljava/lang/String;)V").is_none());
            assert!(lookup(&slots, &members, mode, "absent", "()V").is_none());
        }
    }

    /// Both settings of `RAVA_NO_FAST_MEMBERS` must resolve identically. The switch only decides
    /// WHERE the mode byte is read from, and the mode is a process constant either way, so a
    /// table carrying `Some(mode)` and one carrying `None` are required to agree member for
    /// member — that is the whole correctness claim for carrying it in the table.
    #[test]
    fn carried_mode_and_global_mode_agree() {
        let members = vec![
            member("<init>", "()V"),
            member("get", "(Ljava/lang/Object;)Ljava/lang/Object;"),
            member("get", "(I)Ljava/lang/Object;"),
            member("putIfAbsent", "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;"),
        ];
        let slots = MethodIndex::fill(&members, member_key_mode());
        let carried = MethodIndex::Hashed { mode: Some(member_key_mode()), slots };
        let asking = MethodIndex::Hashed {
            mode: None,
            slots: MethodIndex::fill(&members, member_key_mode()),
        };
        for want in &members {
            let a = carried.find(&members, &want.name, &want.descriptor).expect("carried");
            let b = asking.find(&members, &want.name, &want.descriptor).expect("asking");
            assert_eq!(a as *const MemberInfo, b as *const MemberInfo);
        }
        assert!(carried.find(&members, "absent", "()V").is_none());
        assert!(asking.find(&members, "absent", "()V").is_none());
    }

    /// Duplicate (name, descriptor) is illegal per JVMS 4.6 and not rejected here; the probe
    /// order must still hand back the LOWEST index, matching the old `Vec::push` + `find`.
    #[test]
    fn duplicate_member_resolves_to_the_first() {
        let cf = ClassFile::synthesize(
            ConstantPool::from_entries(vec![Constant::Unusable]),
            0,
            "X".to_string(),
            None,
            Vec::new(),
            vec![member("f", "()V"), member("g", "()V"), member("f", "()V")],
        );
        let found = cf.method("f", "()V").expect("present") as *const MemberInfo;
        assert_eq!(found, &cf.methods[0] as *const MemberInfo);
    }
}
