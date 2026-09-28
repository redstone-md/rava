//! The crate's fast hasher: FxHash over the integer keys the interpreter uses everywhere.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::fast_strhash;
/// A fast non-cryptographic hasher (FxHash) for the hot internal caches, which
/// are keyed by raw pointers / small integers (call sites, `Code` addresses).
/// The standard library's SipHash is DoS-resistant — wasted on engine-internal
/// integer keys — and its per-lookup cost is visible on dispatch-heavy boots
/// (these maps are consulted on every method call). FxHash is ~5x cheaper here.
///
/// The integer entry points below each fold their key in ONE round, which is what makes
/// this cheap for the pointer/site keys it was written for. `write` — the BYTE-SLICE entry
/// point, i.e. every `&str`/`String`/`Box<str>` key — folds ONE BYTE per round, so a
/// 62-character class name costs 62 dependent rotate-xor-multiply rounds. That shape
/// arrived with the integer-keyed maps and was never revisited when name-keyed maps
/// (`ClassLoader::cache`, `missing`, `super_chain_cache`) adopted `FastMap`, where it is
/// strictly worse than the SipHash it replaced. See `fast_strhash` for the word-at-a-time
/// fold and the measurement.
#[derive(Default)]
pub(crate) struct FxHasher {
    hash: u64,
}
pub(crate) const FX_SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

impl std::hash::Hasher for FxHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        // ⛔**NO SIMD HERE, AND THE REASON IS ALGEBRAIC, NOT EFFORT.** The round is
        // `h = (rotl(h,5) ^ w) * K`: every step reads the previous step's product, so the whole
        // fold is ONE dependency chain of rol(1) + xor(1) + imul(3) ≈ 5 cycles per 8 bytes. A
        // vector unit cannot shorten a latency chain, and x86-64 has no 64x64->64 packed multiply
        // below AVX-512DQ anyway, so "lanes" would have to be independent GPR accumulators — and
        // independent accumulators produce a DIFFERENT `finish()` value for the same bytes. That
        // reorders every string-keyed `FastMap` in the engine (`gates.rs` already flags exactly
        // this hazard for `fast_strhash`), which is arbitrated by the bit-exact hash gates, not by
        // inspection. Estimated ceiling if it were taken: ~40 -> ~25 cycles on a 62-byte class
        // name, against ~30 M string hashes a boot, i.e. a few tenths of a percent. Priced,
        // declined. What IS taken below is the work that is NOT on the chain: the gate read and
        // the redundant re-entry for the terminator byte (see `write_u8`).
        //
        // Under four bytes the two arms are the SAME code — the 8-byte and 4-byte chunk splits
        // both fail and only the trailing byte loop runs — so the gate cannot change a single
        // output bit here, and reading it would buy an `Acquire` load plus a branch that LLVM may
        // not hoist out of the caller. Short names and the `Hash for str` terminator take this.
        if bytes.len() < 4 {
            for &b in bytes {
                self.hash = (self.hash.rotate_left(5) ^ b as u64).wrapping_mul(FX_SEED);
            }
            return;
        }
        if !fast_strhash() {
            for &b in bytes {
                self.hash = (self.hash.rotate_left(5) ^ b as u64).wrapping_mul(FX_SEED);
            }
            return;
        }
        // Same round function, eight bytes at a time: the multiply's 3-cycle latency is paid
        // once per WORD rather than once per byte, so the dependent chain over a 62-byte
        // class name goes from 62 rounds to 8.
        let mut rest = bytes;
        while let Some((head, tail)) = rest.split_first_chunk::<8>() {
            self.hash =
                (self.hash.rotate_left(5) ^ u64::from_le_bytes(*head)).wrapping_mul(FX_SEED);
            rest = tail;
        }
        if let Some((head, tail)) = rest.split_first_chunk::<4>() {
            self.hash = (self.hash.rotate_left(5) ^ u64::from(u32::from_le_bytes(*head)))
                .wrapping_mul(FX_SEED);
            rest = tail;
        }
        for &b in rest {
            self.hash = (self.hash.rotate_left(5) ^ b as u64).wrapping_mul(FX_SEED);
        }
    }
    /// ⭐**`Hash for str` APPENDS A `0xFF` TERMINATOR, AND THE DEFAULT `write_u8` ROUTES IT BACK
    /// THROUGH `write`.** `Hasher::write_u8`'s provided body is `self.write(&[i])`, so EVERY
    /// `&str` / `String` / `Box<str>` map probe in this engine entered the byte-slice path twice:
    /// once for the name, then once more to fold a single byte — and that second entry re-read the
    /// `fast_strhash()` gate (a `OnceLock`, i.e. an `Acquire` load and a branch), re-ran the
    /// chunking ladder, and passed a one-element slice by pointer+length. The population is every
    /// string-keyed `FastMap` lookup in the process: `ClassLoader::cache` alone is ~11 M a boot,
    /// plus `field_ids`, `array_element_ids`, `class_pool` and the map-key memo.
    ///
    /// BIT-IDENTICAL, and provably so rather than by measurement: for a one-byte slice BOTH arms
    /// of `write` reduce to exactly this expression — `split_first_chunk::<8>` fails,
    /// `split_first_chunk::<4>` fails, and the trailing byte loop runs once. Removing this
    /// override costs nothing in correctness and puts the double entry back.
    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.hash = (self.hash.rotate_left(5) ^ i as u64).wrapping_mul(FX_SEED);
    }
    #[inline]
    fn write_u16(&mut self, i: u16) {
        self.hash = (self.hash.rotate_left(5) ^ i as u64).wrapping_mul(FX_SEED);
    }
    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.hash = (self.hash.rotate_left(5) ^ i as u64).wrapping_mul(FX_SEED);
    }
    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.hash = (self.hash.rotate_left(5) ^ i).wrapping_mul(FX_SEED);
    }
    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.hash = (self.hash.rotate_left(5) ^ i as u64).wrapping_mul(FX_SEED);
    }
}
/// `HashMap` using [`FxHasher`] — for engine-internal integer/pointer-keyed maps.
pub(crate) type FastMap<K, V> =
    std::collections::HashMap<K, V, std::hash::BuildHasherDefault<FxHasher>>;
/// `HashSet` twin of [`FastMap`] — engine-internal integer-keyed membership sets.
pub(crate) type FastSet<K> =
    std::collections::HashSet<K, std::hash::BuildHasherDefault<FxHasher>>;
