//! Interning names, classes and field ids, plus the layout each class gets.

use crate::heap::*;

/// Bumped once per newly interned field name. The ONLY event that can turn a
/// negative field lookup into a positive one, which is what lets `field_id_existing`
/// cache its misses (see the L1 there).
static FIELD_EPOCH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Process-wide mirror of the interned field NAMES, indexed by field id, for diagnostics that run
/// where no `Vm` is reachable. Written once per distinct name at mint time; read only by failure
/// paths. Never consulted by any decision the engine makes.
static FIELD_NAME_MIRROR: std::sync::RwLock<Vec<Box<str>>> = std::sync::RwLock::new(Vec::new());

pub(crate) fn field_name_mirror_push(name: &str) {
    FIELD_NAME_MIRROR.write().unwrap_or_else(|e| e.into_inner()).push(name.into());
}

/// The name behind a field id, or `None` when the mirror has not seen it (a build that minted
/// before this existed, or an id from another source).
pub fn field_name_of(id: u32) -> Option<String> {
    FIELD_NAME_MIRROR
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(id as usize)
        .map(|n| n.to_string())
}

/// Both of this file's switches live in ONE `OnceLock`, read ONCE per entry point and then
/// passed down by value. Two separate `OnceLock`s would mean two `Acquire` loads per call on
/// the hottest interner path, and an `Acquire` load is not a free `mov` — it is a load LLVM may
/// not hoist, CSE or reorder around. This tree has already priced that shape once (one relaxed
/// atomic on the dispatch push path was worth 15%), so the flag reads are counted here.
///
/// ⭐**AND ONE `OnceLock` IS STILL TWO DEPENDENT LOADS AND A BARRIER — SEE [`intern_gates`].**
/// The struct itself is unchanged; what changed is where its bits are STORED.
#[derive(Clone, Copy)]
struct InternFlags {
    /// `RAVA_NO_FAST_INTERN3=1` (or the block switch) puts every read of this struct back on the
    /// `OnceLock` below — an `Acquire` load of the once-state, a compare, a branch, and only then
    /// a SECOND, dependent load of the payload bytes. The ON arm reads one relaxed word and
    /// bit-tests it in registers. Answers are identical in both arms by construction: the ON
    /// arm's word and the OFF arm's struct are computed from the same environment questions in
    /// the same order (see [`init_intern_gates`] and [`intern_flags_lazy`]), so this switch
    /// prices GATE STORAGE FORM and nothing else. Not a field of the lazy arm's answer — the
    /// bit is tested before either arm is chosen — but carried here so a call site that must
    /// keep the OFF arm's exact shape (the array-birth tracer) can see which arm it is in.
    fast3: bool,
    /// `RAVA_NO_FAST_INTERN=1` (or the block switch `RAVA_NO_EMIT_MORE=1`) restores the previous
    /// interner behaviour: the field L1 goes back to being indexed by the name's ADDRESS,
    /// `field_id` goes back to taking the interner's read lock on every call even for a name it
    /// has already resolved, and `intern_array_element` goes back to hashing its element
    /// descriptor on every array birth. One switch, three sites, so a single same-binary A/B
    /// prices the block — a per-site switch would report three separate zeros and miss their sum.
    fast: bool,
    /// `RAVA_NO_FAST_INTERN2=1` puts the three variable-length `memcpy`/`bcmp` LIBCALLS back:
    /// `L1Name::new`'s `copy_from_slice`, `name_fold`'s two short-name `copy_from_slice`s, and
    /// `intern_array_element`'s slice `==`. Answers are bit-identical either way — the keys, the
    /// folds and the ids are the same values — so this switch prices CONSTRUCTION COST alone.
    fast2: bool,
    /// `RAVA_NO_SIMD_INTERN=1` (or the block switch) restores two things at once, because they
    /// are one idea — do not pay for a question you have already answered:
    ///   * name equality above sixteen bytes goes back to the `bcmp` LIBCALL (`eq` forced to
    ///     [`SimdEq::Off`]), instead of the hand-written SSE2/AVX2 kernels below;
    ///   * `field_id_existing` goes back to loading `FIELD_EPOCH` (an `Acquire` atomic) BEFORE
    ///     the L1 probe, on every call, including the overwhelming majority that hit a POSITIVE
    ///     entry — and a positive id is permanent, so the epoch is irrelevant to it.
    /// Both are answer-preserving: the compares return exact byte equality either way, and the
    /// epoch deferral changes only WHEN a value is read, never which entry is believed.
    simd: bool,
    /// `RAVA_NO_FAST_HEAPTLS=1` (or the block switch) puts this file's two flag-reachable
    /// thread-local tables back on std's LAZY storage. See the `IDS`/`AE_IDS` declarations: the
    /// tables, their slot counts, their keys and every value they hold are IDENTICAL in both
    /// arms — the only difference is whether each access first loads and tests std's
    /// lazy-initialisation `State` byte. Nothing observable can differ, so this switch prices
    /// TLS ACCESS FORM and nothing else.
    heaptls: bool,
    /// Widest byte-equality kernel this CPU may run, resolved ONCE. `is_x86_feature_detected!`
    /// is itself a relaxed atomic load and a bit test; on a path taken per array birth that is
    /// not free, and the answer cannot change during the process.
    eq: SimdEq,
    /// Mirror of `array_intern_stats::enabled()` — `RAVA_ARRAY_INTERN_STATS`, off in every
    /// non-debug run. It lives here because it is asked ONCE PER ARRAY BIRTH (8.95 M a boot) at
    /// the top of [`Heap::intern_array_element`], and asking it through its own `OnceLock` was a
    /// second `Acquire` load plus a second dependent payload load on that path, for a counter
    /// nobody is reading. Same env question, same answer; only the storage moves.
    stats: bool,
}

/// Which hand-written byte-equality kernel [`same_bytes`] may use above sixteen bytes.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
enum SimdEq {
    /// Fall back to `a == b`, i.e. the CRT's `bcmp`.
    Off,
    /// Baseline on x86-64; needs no runtime detection.
    Sse2,
    /// Runtime-detected.
    Avx2,
}

#[cfg(target_arch = "x86_64")]
fn detect_eq() -> SimdEq {
    if std::is_x86_feature_detected!("avx2") { SimdEq::Avx2 } else { SimdEq::Sse2 }
}

#[cfg(not(target_arch = "x86_64"))]
fn detect_eq() -> SimdEq {
    SimdEq::Off
}

/// ⭐**THE GATE WORD. ONE RELAXED LOAD, NO PAYLOAD, NO BARRIER, NO SECOND CACHE LINE.**
///
/// SHAPE BEFORE: `OnceLock<InternFlags>`. `get_or_init` on the hit path is
/// `Once::is_completed()` — an **`Acquire`** load of the once-state — a compare against
/// `COMPLETE`, a branch, and then a SECOND load, dependent on the first, of the payload bytes
/// out of the `UnsafeCell`. Two of those three costs are structural rather than the `mov`: an
/// `Acquire` load is an optimisation barrier LLVM may not hoist, CSE or move non-atomic memory
/// operations across, and the payload load cannot issue until the state load retires.
///
/// SHAPE AFTER: one `u32` of bits that IS the answer. `load(Relaxed)`, `test`, and every switch
/// is then a bit test on a value already in a register. The `Relaxed` ordering is sound here in
/// a way it is NOT for a `OnceLock`: nothing is published THROUGH this word — there is no
/// separate payload for another thread to observe half-built. A thread either reads a finished
/// word or reads `0` and recomputes it, and the computation is a pure function of the process
/// environment, so every thread that computes it computes the same bits. The store may
/// therefore race and the last writer wins with the identical value.
///
/// `G_INIT` is always set once the word is computed, so `0` is unambiguously "not yet computed"
/// and the empty word needs no separate flag.
///
/// POPULATION: every by-name field access (`field_id_existing`, 31.1% of `Map.get` and 20.7% of
/// `ArrayList.size()` by caller-attributed sampling) and every array birth (8.95 M a boot).
static INTERN_GATES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

const G_INIT: u32 = 1 << 0;
const G_FAST3: u32 = 1 << 1;
const G_FAST: u32 = 1 << 2;
const G_FAST2: u32 = 1 << 3;
const G_SIMD: u32 = 1 << 4;
const G_HEAPTLS: u32 = 1 << 5;
/// `eq` is three-valued, so it takes two bits rather than being derived from `G_SIMD`: on a
/// non-x86-64 build `detect_eq()` answers `Off` even with `simd` on, and deriving would have
/// silently promoted that to `Sse2`.
const G_EQ_SSE2: u32 = 1 << 6;
const G_EQ_AVX2: u32 = 1 << 7;
const G_STATS: u32 = 1 << 8;

/// Compute the word. Runs at most a handful of times per process (once per thread that races
/// the first read); `#[inline(never)]` + `#[cold]` keeps the environment walk, the string
/// compares and `is_x86_feature_detected!`'s own atomic entirely out of every caller's body.
#[inline(never)]
#[cold]
fn init_intern_gates() -> u32 {
    // SAME QUESTIONS, SAME ORDER as `intern_flags_lazy` — the two arms must not drift.
    let block = crate::flags::emit_more_disabled();
    let simd = std::env::var_os("RAVA_NO_SIMD_INTERN").is_none() && !block;
    let mut w = G_INIT;
    if std::env::var_os("RAVA_NO_FAST_INTERN3").is_none() && !block {
        w |= G_FAST3;
    }
    if std::env::var_os("RAVA_NO_FAST_INTERN").is_none() && !block {
        w |= G_FAST;
    }
    if std::env::var_os("RAVA_NO_FAST_INTERN2").is_none() && !block {
        w |= G_FAST2;
    }
    if simd {
        w |= G_SIMD;
    }
    if std::env::var_os("RAVA_NO_FAST_HEAPTLS").is_none() && !block {
        w |= G_HEAPTLS;
    }
    match if simd { detect_eq() } else { SimdEq::Off } {
        SimdEq::Avx2 => w |= G_EQ_AVX2,
        SimdEq::Sse2 => w |= G_EQ_SSE2,
        SimdEq::Off => {}
    }
    // The tracer's own gate, hoisted out of the per-array-birth path. Read through
    // `array_intern_stats::enabled()` rather than re-reading its env var, so there is exactly
    // ONE place in the process that decides what `RAVA_ARRAY_INTERN_STATS` means and the mirror
    // cannot disagree with the counters it guards.
    if array_intern_stats::enabled() {
        w |= G_STATS;
    }
    INTERN_GATES.store(w, std::sync::atomic::Ordering::Relaxed);
    w
}

/// The previous shape, kept whole as the `RAVA_NO_FAST_INTERN3=1` arm so a same-binary A/B
/// prices the `OnceLock` read against the word read and nothing else. Deliberately still
/// `#[inline]`, exactly as it was: making it a call would add a cost the old default never paid
/// and would inflate the switch's delta past the thing being measured.
#[inline]
fn intern_flags_lazy() -> &'static InternFlags {
    static F: std::sync::OnceLock<InternFlags> = std::sync::OnceLock::new();
    F.get_or_init(|| {
        let block = crate::flags::emit_more_disabled();
        let simd = std::env::var_os("RAVA_NO_SIMD_INTERN").is_none() && !block;
        InternFlags {
            fast3: false,
            fast: std::env::var_os("RAVA_NO_FAST_INTERN").is_none() && !block,
            fast2: std::env::var_os("RAVA_NO_FAST_INTERN2").is_none() && !block,
            simd,
            heaptls: std::env::var_os("RAVA_NO_FAST_HEAPTLS").is_none() && !block,
            eq: if simd { detect_eq() } else { SimdEq::Off },
            // Unused in this arm: with `fast3` off, `intern_array_element` keeps its original
            // unconditional call into the tracer, which asks its own gate as it always did.
            stats: false,
        }
    })
}

/// EVERY switch this file has, resolved by ONE load, returned BY VALUE so the caller passes it
/// down instead of asking again. Callers must read this ONCE at their entry point — see
/// `field_id`, which used to read the gate and then call `field_id_existing`, which read it a
/// second time because it is not inlined into its callers.
#[inline(always)]
fn intern_gates() -> InternFlags {
    let w = INTERN_GATES.load(std::sync::atomic::Ordering::Relaxed);
    let w = if w == 0 { init_intern_gates() } else { w };
    if w & G_FAST3 == 0 {
        return *intern_flags_lazy();
    }
    let simd = w & G_SIMD != 0;
    InternFlags {
        fast3: true,
        fast: w & G_FAST != 0,
        fast2: w & G_FAST2 != 0,
        simd,
        heaptls: w & G_HEAPTLS != 0,
        eq: if w & G_EQ_AVX2 != 0 {
            SimdEq::Avx2
        } else if w & G_EQ_SSE2 != 0 {
            SimdEq::Sse2
        } else {
            SimdEq::Off
        },
        stats: w & G_STATS != 0,
    }
}

/// Byte equality over TWO SLICES OF THE SAME LENGTH `n >= 16`, sixteen bytes per compare.
///
/// # Safety
/// `a` and `b` must each be readable for `n` bytes and `n >= 16` must hold.
///
/// BIT-IDENTICAL to `a == b`: `pcmpeqb` sets a lane to `0xFF` exactly when the two bytes are
/// equal, so `pmovmskb == 0xFFFF` is precisely "these sixteen bytes are pairwise equal", and the
/// blocks tile `0..n` with the last block OVERLAPPING the previous one. The overlap re-compares
/// bytes already proven equal, which is free and — the point — keeps every load entirely inside
/// both allocations. Nothing is ever read at or past `a.add(n)`, so no page boundary can be
/// crossed by this function; that is the failure mode that makes a naive 16-byte tail load work
/// ninety-nine times in a hundred and then fault.
#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn eq_sse2(a: *const u8, b: *const u8, n: usize) -> bool {
    use core::arch::x86_64::{_mm_cmpeq_epi8, _mm_loadu_si128, _mm_movemask_epi8, __m128i};
    debug_assert!(n >= 16);
    let mut i = 0usize;
    while i + 16 <= n {
        let va = _mm_loadu_si128(a.add(i) as *const __m128i);
        let vb = _mm_loadu_si128(b.add(i) as *const __m128i);
        if _mm_movemask_epi8(_mm_cmpeq_epi8(va, vb)) != 0xFFFF {
            return false;
        }
        i += 16;
    }
    if i < n {
        // Tail: one more block ENDING at `n`. `n >= 16` ⇒ `n - 16 >= 0`, and it overlaps the
        // block just compared, so `i` is never advanced past the buffer.
        let off = n - 16;
        let va = _mm_loadu_si128(a.add(off) as *const __m128i);
        let vb = _mm_loadu_si128(b.add(off) as *const __m128i);
        if _mm_movemask_epi8(_mm_cmpeq_epi8(va, vb)) != 0xFFFF {
            return false;
        }
    }
    true
}

/// [`eq_sse2`] at twice the width, for `n >= 32`. Same tiling, same overlapping tail, same
/// byte-identity argument — `vpcmpeqb` + `vpmovmskb` all-ones is exactly thirty-two pairwise
/// equal bytes.
///
/// # Safety
/// `a` and `b` must each be readable for `n` bytes, `n >= 32`, and the CPU must support AVX2 —
/// which is why the only caller reaches this through [`SimdEq::Avx2`], resolved once by
/// `is_x86_feature_detected!`.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn eq_avx2(a: *const u8, b: *const u8, n: usize) -> bool {
    use core::arch::x86_64::{
        _mm256_cmpeq_epi8, _mm256_loadu_si256, _mm256_movemask_epi8, __m256i,
    };
    debug_assert!(n >= 32);
    let mut i = 0usize;
    while i + 32 <= n {
        let va = _mm256_loadu_si256(a.add(i) as *const __m256i);
        let vb = _mm256_loadu_si256(b.add(i) as *const __m256i);
        if _mm256_movemask_epi8(_mm256_cmpeq_epi8(va, vb)) != -1 {
            return false;
        }
        i += 32;
    }
    if i < n {
        let off = n - 32;
        let va = _mm256_loadu_si256(a.add(off) as *const __m256i);
        let vb = _mm256_loadu_si256(b.add(off) as *const __m256i);
        if _mm256_movemask_epi8(_mm256_cmpeq_epi8(va, vb)) != -1 {
            return false;
        }
    }
    true
}

// `fast_intern_enabled()` used to live here — a one-line `pub(crate)` wrapper over the switch
// word whose ONLY caller was `field_id`, which now reads the word once and hands it down. A
// wrapper that exists to re-ask a question its caller already holds the answer to is the exact
// shape this round is removing, so it is gone rather than left to be re-adopted.

/// Load `b` (0..=8 bytes) into the low bytes of a `u64`, little-endian, zero-padded above,
/// using only FIXED-WIDTH loads.
///
/// ⭐**`copy_from_slice` WITH A RUNTIME LENGTH IS A LIBCALL, AND THE CALL IS THE COST.** The
/// bytes moved here are one to fifteen; what `memcpy` charges for them is a call, an internal
/// size dispatch, and — the part that hurts the caller rather than the copy — the clobbering of
/// every caller-saved register, which forces the surrounding hot function to spill and reload
/// across the call. `field_id_existing` builds one such key per by-name field access and
/// `intern_array_element` folds one per array birth (8.95M a boot), so the libcall is paid on
/// the two highest-population paths in this file.
///
/// The head and tail loads OVERLAP; the overlapping bytes are literally the same bytes, so `|`
/// reconstructs `b` exactly. Nothing is read past `b`, so no page can be crossed.
/// Caller guarantees `b.len() <= 8` — the head/tail pair only tiles the whole name up to twice
/// the load width.
#[inline]
fn word_le(b: &[u8]) -> u64 {
    let n = b.len();
    debug_assert!(n <= 8);
    if let (Some(h), Some(t)) = (b.first_chunk::<4>(), b.last_chunk::<4>()) {
        u32::from_le_bytes(*h) as u64 | ((u32::from_le_bytes(*t) as u64) << ((n as u32 - 4) * 8))
    } else if n == 0 {
        0
    } else {
        // 1..=3 bytes: first, middle and last. For n = 1 all three positions coincide and for
        // n = 2 two of them do, and coinciding positions carry the same byte, so `|` is exact.
        (b[0] as u64)
            | ((b[n >> 1] as u64) << (((n as u32) >> 1) * 8))
            | ((b[n - 1] as u64) << ((n as u32 - 1) * 8))
    }
}

/// [`word_le`] at twice the width: `b` (0..=16 bytes) zero-padded into a `u128`. Same overlap
/// argument, same no-libcall property. Caller guarantees `b.len() <= 16`.
#[inline]
fn dword_le(b: &[u8]) -> u128 {
    let n = b.len();
    debug_assert!(n <= 16);
    if let (Some(h), Some(t)) = (b.first_chunk::<8>(), b.last_chunk::<8>()) {
        u64::from_le_bytes(*h) as u128 | ((u64::from_le_bytes(*t) as u128) << ((n as u32 - 8) * 8))
    } else {
        word_le(b) as u128
    }
}

/// Byte equality without the `bcmp` LIBCALL, at every length this file actually compares.
///
/// ⭐**THE LENGTH TEST IS THE WHOLE FAST REJECT, AND IT COSTS TWO INSTRUCTIONS.** Two names of
/// different length cannot be equal, and `&[u8] == &[u8]` only learns that INSIDE the call — after
/// the arguments are marshalled and every caller-saved register is written off. Testing it here
/// means the common non-match never reaches a call at all; the first sixteen-byte compare below
/// then does the same job for the first sixteen bytes, which separates essentially every
/// remaining pair of engine names (they diverge in the package, not the suffix).
///
/// Same argument as [`word_le`] for the short half: `intern_array_element` verifies its L1 hit
/// with a slice `==` on every array birth, and the element descriptors that dominate that
/// population are the primitive names (`int`, `byte`, `char`, `long` — three to seven bytes). One
/// or two register compares replace the call.
///
/// ⭐**AND THE LONG HALF IS NOT RARE HERE — IT IS EVERY OBJECT ARRAY.** `"Ljava/lang/Object;"` and
/// `"Ljava/lang/String;"` are eighteen bytes, so the reference-array share of the boot's 8.95 M
/// array births used to land on the `_ => a == b` arm and pay a libcall to compare eighteen bytes.
/// [`eq_sse2`] / [`eq_avx2`] do it in one or two vector compares with no call, no register
/// clobber, and no length dispatch. Above sixty-four bytes the CRT's routine is genuinely good and
/// the names are rare, so that stays a `==`. `RAVA_NO_SIMD_INTERN=1` puts the whole long half back
/// on `bcmp`.
#[inline]
fn same_bytes(a: &[u8], b: &[u8], fast2: bool, eq: SimdEq) -> bool {
    if !fast2 {
        return a == b;
    }
    if a.len() != b.len() {
        return false;
    }
    // Lengths are equal, so an exact zero-padded encoding of each is equal iff the bytes are.
    match a.len() {
        0..=8 => word_le(a) == word_le(b),
        9..=16 => dword_le(a) == dword_le(b),
        #[cfg(target_arch = "x86_64")]
        n if n <= 64 && eq != SimdEq::Off => {
            // SAFETY: both slices are readable for `n` bytes (they are live `&[u8]` of that
            // length) and `n >= 17 > 16`, so both kernels' preconditions hold. `Avx2` is only
            // ever produced by `is_x86_feature_detected!("avx2")`, and the 32-byte kernel is
            // only entered at `n >= 32`.
            unsafe {
                if eq == SimdEq::Avx2 && n >= 32 {
                    eq_avx2(a.as_ptr(), b.as_ptr(), n)
                } else {
                    eq_sse2(a.as_ptr(), b.as_ptr(), n)
                }
            }
        }
        _ => {
            let _ = eq;
            a == b
        }
    }
}

/// Sixteen bytes: up to 15 name bytes, zero-padded, with the byte length in the last slot.
///
/// The whole struct compares in one 16-byte compare and folds to a cache index in two loads and
/// one multiply — which is the entire point. The L1 it keys used to be indexed by the name's
/// ADDRESS, and an address is not a property of the name: the datapack decode resolves field
/// names off freshly-built strings, so the same name arrived at a different address on every
/// document and could never hit. Content indexing makes a repeated name land in the same set
/// no matter where its bytes live.
///
/// Held as the TWO WORDS of that sixteen-byte image rather than as `[u8; 16]`: same sixteen
/// bytes, same alignment, same slot size — but the compare is two integer compares instead of a
/// `raw_eq`, and `fold` reads the words it was built from instead of round-tripping them through
/// the stack to re-load them. `lo` is bytes 0..8, `hi` is bytes 8..16 (so `hi`'s top byte is the
/// length). Nothing outside this impl may assume anything else about the encoding.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct L1Name {
    lo: u64,
    hi: u64,
}

impl L1Name {
    /// `len == 0` is unreachable for a real field name (the JVM forbids an empty name), so the
    /// all-zero slot doubles as "empty" and needs no separate occupancy flag.
    const EMPTY: L1Name = L1Name { lo: 0, hi: 0 };

    #[inline]
    fn new(s: &str, fast2: bool) -> Option<L1Name> {
        let b = s.as_bytes();
        // A name longer than the buffer simply is not cached — exactly the behaviour such a
        // name had before this cache existed. Widening the buffer is NOT free: 31 bytes over
        // 256 slots made this table 14 KB and the boot 6.8% SLOWER, in both orders.
        if b.is_empty() || b.len() > 15 {
            return None;
        }
        // ⭐**WAS ONE `memcpy` LIBCALL PER PROBE, IS TWO FIXED-WIDTH LOADS.** The population is
        // by-name field accesses — millions a boot, and `field_id_existing` is the row this
        // whole file exists to shrink. `dword_le` builds the identical sixteen-byte image (see
        // its overlap argument); `RAVA_NO_FAST_INTERN2=1` restores the `copy_from_slice` so a
        // same-binary A/B prices exactly this and nothing else.
        let v = if fast2 {
            dword_le(b)
        } else {
            let mut a = [0u8; 16];
            a[..b.len()].copy_from_slice(b);
            u128::from_le_bytes(a)
        };
        // Byte 15 is the length. `b.len() <= 15` ⇒ the name's own bytes never reach it, so the
        // OR cannot corrupt a name and the encoding stays injective over (bytes, length).
        Some(L1Name { lo: v as u64, hi: (v >> 64) as u64 | ((b.len() as u64) << 56) })
    }

    #[inline]
    fn fold(&self) -> u64 {
        (self.lo.rotate_left(5) ^ self.hi).wrapping_mul(crate::FX_SEED)
    }
}

/// A length-independent fold of a name, for the caches whose names are LONGER than `L1Name`
/// holds (array element descriptors: `Ljava/lang/Object;` is eighteen characters).
///
/// Two loads and one multiply regardless of length — as against `FxHasher`, which folds a word
/// per round and therefore charges by the name. The TAIL is folded as well as the head because
/// engine names share long prefixes (`Lnet/minecraft/…`); a head-only fold would put a whole
/// package in one slot. This is an INDEX, never an answer: every user of it verifies the hit
/// against the real bytes, so a bad fold costs a miss and can never return the wrong id.
///
/// ⭐**THE SHORT BRANCH WAS TWO `memcpy` LIBCALLS, AND IT IS THE BRANCH THE BOOT TAKES.** Every
/// array birth folds its element descriptor here — 8.95M a boot — and the descriptors that
/// dominate that count are the primitive names, ALL of which are under eight bytes (`int` 3,
/// `byte`/`char`/`long` 4, `float`/`short` 5, `double` 6, `boolean` 7). So the hot path of a
/// per-allocation function was two variable-length `copy_from_slice` calls to move at most seven
/// bytes. `word_le` produces the same word with fixed-width loads; the fold's VALUE is unchanged
/// in both branches, so no cache slot moves. `RAVA_NO_FAST_INTERN2=1` restores the copies.
#[inline]
fn name_fold(b: &[u8], fast2: bool) -> u64 {
    let n = b.len();
    let (lo, hi) = if fast2 {
        match (b.first_chunk::<8>(), b.last_chunk::<8>()) {
            (Some(h), Some(t)) => (u64::from_le_bytes(*h), u64::from_le_bytes(*t)),
            // Under eight bytes head and tail are the same zero-padded word, exactly as the
            // twin `copy_from_slice`s below produced.
            _ => {
                let w = word_le(b);
                (w, w)
            }
        }
    } else {
        let mut lo = [0u8; 8];
        let mut hi = [0u8; 8];
        if n >= 8 {
            lo.copy_from_slice(&b[..8]);
            hi.copy_from_slice(&b[n - 8..]);
        } else {
            lo[..n].copy_from_slice(b);
            hi[..n].copy_from_slice(b);
        }
        (u64::from_le_bytes(lo), u64::from_le_bytes(hi))
    };
    (lo.rotate_left(5) ^ hi ^ (n as u64)).wrapping_mul(crate::FX_SEED)
}

/// A synthetic field name that a HOT path probes by constant rather than by string.
/// The discriminant IS the cache slot, so the order here is the order of `SYN_NAMES`.
#[derive(Clone, Copy)]
pub(crate) enum Syn {
    Str = 0,
    Name,
    Path,
    KeyStr,
    Map,
    Elems,
    IdKeys,
    Key,
    Val,
    // ⭐**THE SECOND WAVE, AND `$pos` ALONE HAD 77 SITES.** The first nine covered the collection
    // BACKING fields; these are the ones every iterator and every synthetic object touches on
    // each operation. `field_id_existing`'s thread-local L1 shows at 1.8% of
    // `bench/NativeCost.java`'s host profile, which is what a by-name access costs when it is on
    // a path taken millions of times.
    Pos,
    Size,
    Init,
    Source,
    SrcLen,
    FnKind,
    Sb,
    SKey,
    ExpSize,
}

pub(crate) const SYN_NAMES: [&str; 18] = [
    "$str", "$name", "$path", "$keystr", "$map", "$elems", "$idkeys", "$key", "$val",
    "$pos", "$size", "$init", "$source", "$srclen", "$fnkind", "$sb", "$skey", "$expsize",
];


impl Heap {
    /// Intern a class name to a shared `Arc<str>`.
    pub(crate) fn intern(&self, name: &str) -> Arc<str> {
        if let Some(rc) = self.class_pool.read().unwrap_or_else(|e| e.into_inner()).get(name) {
            return Arc::clone(rc);
        }
        // Re-check under the write lock: two threads sighting the same class name must agree on
        // one `Arc<str>`, because object identity comparisons elsewhere use pointer equality on it.
        let mut w = self.class_pool.write().unwrap_or_else(|e| e.into_inner());
        if let Some(rc) = w.get(name) {
            return Arc::clone(rc);
        }
        let rc: Arc<str> = Arc::from(name);
        w.insert(name.into(), Arc::clone(&rc));
        rc
    }

    /// Intern one array element descriptor under the owning heap. Mutation occurs
    /// only on the GIL-held allocation/reservation paths; workers receive copied ids.
    ///
    /// ⭐**8,954,743 ARRAY BIRTHS A BOOT, EACH RE-HASHING AN EIGHTEEN-CHARACTER DESCRIPTOR.**
    /// `"Ljava/lang/Object;"` was folded word-at-a-time by `FxHasher` and then probed through
    /// hashbrown on every single allocation, to answer a question with **eight to a few dozen**
    /// distinct answers for the whole process. The previous note here priced that at ~0.7% of a
    /// boot and declined a pointer-keyed L1 for being unmeasurable; the L1 below is not
    /// pointer-keyed and costs nothing extra to build, because it reuses this file's `name_fold`
    /// and needs no stored copy of the name at all.
    ///
    /// **The slot holds only the id.** Verification reads the interner's OWN name vector at that
    /// id and compares bytes — one index and one `memcmp`, no hash, no lock, no probe. A stale
    /// slot (a fold collision, or a second `Heap` in this thread whose id space differs) fails
    /// the compare and falls through to the map; it can never return a wrong id, which is the
    /// property that makes a name-free cache admissible here at all.
    ///
    /// **64 slots x 4 B = 256 B total.** Deliberately tiny: this tree measured a table that grew
    /// from 2.5 KB to 14 KB costing +6.8% on the boot because it stopped fitting in L1.
    pub(crate) fn intern_array_element(&self, name: &str) -> u32 {
        // ⭐**THE "INERT ONE-BRANCH CHECK" WAS AN `Acquire` ONCELOCK READ, 8.95 M TIMES A BOOT.**
        // `count_lookup` is `#[inline]`, but the first thing it inlines is
        // `array_intern_stats::enabled()` — a second `OnceLock<bool>`, i.e. an `Acquire` load of
        // the once-state, a compare, a branch and a dependent load of the payload byte — asked
        // once per array birth to decide whether to bump a counter that is off in every
        // non-debug run. The answer is now a bit of the word this function has to load anyway,
        // so the whole call disappears from the default path: no second static touched, no
        // second cache line, no barrier between the caller's key fold and the L1 probe.
        //
        // With `RAVA_NO_FAST_INTERN3=1` the call is made unconditionally, exactly as before, so
        // the switch prices this removal rather than a rearranged version of it. Counting is
        // bit-identical in every arm: `flags.stats` IS `array_intern_stats::enabled()` (the
        // initialiser calls it), and `count_lookup` re-checks it anyway when it is entered.
        let flags = intern_gates();
        if flags.fast3 {
            if flags.stats {
                array_intern_stats::count_lookup(name);
            }
        } else {
            array_intern_stats::count_lookup(name);
        }
        const AE_L1: usize = 64;
        type AeTable = [std::cell::Cell<u32>; AE_L1];
        // ⭐**`thread_local!` TAKES ITS CONST PATH ONLY FOR `= const { … }`, AND `[const { … }; N]`
        // IS NOT THAT.** The macro's fast arm matches the literal token sequence
        // `= const $init:block`; an ARRAY-REPEAT expression whose ELEMENT happens to be a const
        // block starts with `[`, so it never matches and falls to the `$init:expr` arm.
        //
        // SHAPE BEFORE: `LazyStorage<AeTable, ()>` — a `State` byte plus a
        // `MaybeUninit<AeTable>`, and every `.with` loaded that byte, compared it against `Alive`
        // and branched before it could even form the table's address. The byte lives at offset 0
        // of the storage while the probed slot is anywhere in the 256 B table, so the check
        // usually touched a SECOND cache line to learn something that has been true since the
        // thread's first array birth.
        // SHAPE AFTER: the true const arm ⇒ `needs_drop::<AeTable>()` is false ⇒ std emits a bare
        // `#[thread_local] static VAL: AeTable = INIT;` and `.with` is `&VAL` — one TLS-base read
        // and an immediate displacement, no state, no branch, no lazy path to keep alive.
        //
        // Both forms are declared so `RAVA_NO_FAST_HEAPTLS=1` can price the difference in a
        // same-binary A/B. They are two independent caches of the same facts: each starts empty,
        // each stores only ids it has verified against `array_element_names`, and only one is ever
        // touched in a process. `u32::MAX` is `DEAD_ARRAY_ELEMENT_ID`, which this function never
        // mints, so it is free to serve as the empty marker in both.
        std::thread_local! {
            static AE_IDS: AeTable = const { [const { std::cell::Cell::new(u32::MAX) }; AE_L1] };
            static AE_IDS_LAZY: AeTable = [const { std::cell::Cell::new(u32::MAX) }; AE_L1];
        }
        /// One accessor pair for both storage forms, so the two arms cannot drift apart.
        #[inline(always)]
        fn ae_get(heaptls: bool, slot: usize) -> u32 {
            if heaptls {
                AE_IDS.with(|t| t[slot].get())
            } else {
                AE_IDS_LAZY.with(|t| t[slot].get())
            }
        }
        #[inline(always)]
        fn ae_set(heaptls: bool, slot: usize, id: u32) {
            if heaptls {
                AE_IDS.with(|t| t[slot].set(id))
            } else {
                AE_IDS_LAZY.with(|t| t[slot].set(id))
            }
        }
        // Already loaded above for the tracer gate — destructured, never re-read.
        let InternFlags { fast, fast2, eq, heaptls, .. } = flags;
        let slot = if fast {
            (name_fold(name.as_bytes(), fast2) >> 32) as usize & (AE_L1 - 1)
        } else {
            0
        };
        if fast {
            let cand = ae_get(heaptls, slot);
            if cand != DEAD_ARRAY_ELEMENT_ID {
                // Read side of the RwLock interner: one shared guard, one bounds-checked clone.
                // Clone is needed because the &str borrows into `names` and cannot outlive the guard.
                // The bump below (fast path) skips this whole branch, so the clone is off the hot path.
                let maybe = self.array_element_names.read().unwrap_or_else(|e| e.into_inner()).get(cand as usize).cloned();
                if let Some(n) = maybe {
                    // The verification compare, not a `bcmp` call — see `same_bytes`. This is
                    // the HIT path, so the compare almost always runs to completion rather than
                    // being cut short by the length check: it is paid in full, per array birth.
                    if same_bytes(n.as_bytes(), name.as_bytes(), fast2, eq) {
                        return cand;
                    }
                }
            }
        }
        if let Some(&id) = self.array_element_ids.read().unwrap_or_else(|e| e.into_inner()).get(name) {
            if fast {
                ae_set(heaptls, slot, id);
            }
            return id;
        }
        array_intern_stats::count_miss();
        // Hold write guards for the rest of this path (append-only late arrival).
        let mut names = self.array_element_names.write().unwrap_or_else(|e| e.into_inner());
        let mut ids = self.array_element_ids.write().unwrap_or_else(|e| e.into_inner());
        // Re-check under exclusive lock — another thread may have inserted while we dropped the read guard.
        if let Some(&id) = ids.get(name) {
            let id = id;
            drop(ids);
            drop(names);
            if fast {
                ae_set(heaptls, slot, id);
            }
            return id;
        }
        let id = u32::try_from(names.len())
            .ok()
            .filter(|&id| id != DEAD_ARRAY_ELEMENT_ID)
            .expect("array element interner exhausted its u32 id space");
        let canonical = self.intern(name);
        names.push(canonical);
        ids.insert(name.into(), id);
        if fast {
            ae_set(heaptls, slot, id);
        }
        id
    }

    #[inline]
    pub(crate) fn array_element_name(&self, id: u32) -> Option<Arc<str>> {
        if id == DEAD_ARRAY_ELEMENT_ID {
            return None;
        }
        self.array_element_names.read().unwrap_or_else(|e| e.into_inner()).get(id as usize).cloned()
    }

    /// Intern the 8 primitive array element names into the process-wide mirror
    /// (idempotent). Called once at VM construction; parallel workers and the JIT
    /// then read the ids without touching the interner.
    pub(crate) fn ensure_prim_element_ids(&mut self) {
        if PRIM_ELEMENT_IDS.get().is_some() {
            return;
        }
        let ids: [u32; 8] = std::array::from_fn(|i| {
            self.intern_array_element(
                ["boolean", "char", "float", "double", "byte", "short", "int", "long"][i],
            )
        });
        let _ = PRIM_ELEMENT_IDS.set(ids);
    }

    /// Intern a class name to its shared `Arc<str>` (pub for the per-`new`-site cache).
    pub(crate) fn intern_class(&self, name: &str) -> Arc<str> {
        self.intern(name)
    }

    /// Read-only interner probe: would `intern(name)` be a pure lookup? A gil-free
    /// worker may only take paths whose interner consultations HIT — a miss inserts
    /// into the shared `class_pool`, which is exactly the publication the S09 gates
    /// exist to stop.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn class_is_interned(&self, name: &str) -> bool {
        self.class_pool.read().unwrap_or_else(|e| e.into_inner()).contains_key(name)
    }

    /// Record a class's field layout so the alloc choke-point can pre-populate
    /// objects of that class (universal pre-population). Pushed in by
    /// `Vm::class_field_layout` once it resolves the super-chain.
    pub(crate) fn set_layout(&self, class: &str, layout: Arc<[(u32, u8)]>) {
        // B-series pinned-mirror: build the id→slot index once per class, ONLY
        // under the flag. `set_layout` runs strictly before any object of this
        // class is allocated (hence before its mirror), so the index is always
        // present by the time a maintenance path needs it. Off by default ⇒ no
        // allocation on the default path ⇒ byte-identical canonical.
        #[cfg(feature = "next-gen-gc")]
        if crate::nextgen::pinned_mirror_enabled() && !self.layout_slot_index.read().unwrap_or_else(|e| e.into_inner()).contains_key(class) {
            let mut idx: crate::FastMap<u32, u32> =
                crate::FastMap::with_capacity_and_hasher(layout.len(), Default::default());
            for (slot, &(id, _)) in layout.iter().enumerate() {
                // FIRST wins — matches `layout.iter().position(id)`.
                idx.entry(id).or_insert(slot as u32);
            }
            self.layout_slot_index.write().unwrap_or_else(|e| e.into_inner()).insert(class.into(), idx);
        }
        self.layouts.write().unwrap_or_else(|e| e.into_inner()).entry(class.into()).or_insert(layout);
    }

    /// O(1) field-id → layout slot for the pinned-mirror maintenance paths.
    /// `Some` only when the mirror is on AND the class layout has been recorded
    /// (both guaranteed together with any live mirror). Replaces the per-field
    /// `layout.iter().position(id)` linear scan.
    #[cfg(feature = "next-gen-gc")]
    #[inline]
    pub(crate) fn layout_slot(&self, class: &str, id: u32) -> Option<usize> {
        self.layout_slot_index
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(class)
            .and_then(|m| m.get(&id).copied())
            .map(|s| s as usize)
    }

    /// Intern a field name to a stable id (allocating only the first time a name
    /// is ever seen, not per write).
    pub(crate) fn field_id(&self, name: &str) -> u32 {
        // ⭐**AN ALREADY-INTERNED NAME NEVER TOUCHES THE LOCK OR THE HASH.** This entry point is
        // the *interning* one, so it always went straight to `field_ids_r()` — an `RwLock` read
        // guard plus an `FxHash` of the name plus a hashbrown probe — even though the
        // overwhelming majority of its calls are repeat resolutions of names minted long ago
        // (`$elems`/`$size`/`$init` per stream op, one per declared field per class layout, one
        // per record component). `field_id_existing`'s L1 answers those from a 16-byte compare.
        //
        // Sound because a POSITIVE is permanent: an id, once minted, is an index into
        // `field_names` and never changes and never disappears. A negative is not consulted here
        // at all — a miss falls through to the mint below, which re-checks under the write lock.
        //
        // ⭐**THE GATE WAS READ TWICE PER CALL, AND THE SECOND READ WAS INVISIBLE HERE.**
        // `fast_intern_enabled()` reads the switch word, and then `field_id_existing` — a large
        // function that no caller inlines — read it AGAIN at its own entry. Two static reads
        // (two `Acquire` loads and two dependent payload loads, before the word) to answer one
        // question that is a constant of the process. The gate is now read once and handed down
        // by value through `field_id_existing_gated`; the by-name entry point keeps its old
        // signature and simply supplies the word itself.
        // POPULATION: every class layout's declared fields, every record component, `$elems` /
        // `$size` / `$init` per stream op — i.e. the interning entry point's calls are dominated
        // by REPEAT resolutions of names minted long ago, which is why the L1 is consulted first.
        let flags = intern_gates();
        if flags.fast {
            if let Some(id) = self.field_id_existing_gated(name, flags) {
                return id;
            }
        } else if let Some(&id) = self.field_ids_r().get(name) {
            return id;
        }
        // Miss: take both writers. Re-check under the write lock — two threads can race the same
        // first sighting, and the second must return the id the first published rather than mint
        // a duplicate (ids are indices into `field_names`, so a duplicate would be a silent alias).
        let mut names = self.field_names_w();
        let mut ids = self.field_ids_w();
        if let Some(&id) = ids.get(name) {
            return id;
        }
        let id = names.len() as u32;
        names.push(name.into());
        ids.insert(name.into(), id);
        // ⭐**A PROCESS-WIDE MIRROR, FOR DIAGNOSTICS THAT HAVE NO `Vm`.** The field-name table
        // hangs off the heap, so the one place that most needs to print a name — the arena
        // field-layout panic, which fires on a background datapack thread where `vm_shared()` is
        // `None` — could only ever print the bare id, and a bare id cannot tell a SHORT layout
        // from an id COLLISION. Minting happens once per distinct name (a few thousand a boot),
        // so mirroring here costs nothing measurable and is never read on a hot path.
        field_name_mirror_push(name);
        // A new name changes the answer to EVERY outstanding negative lookup, which
        // is what makes the L1's negative entries safe to cache: they are stamped
        // with this counter and a bump invalidates all of them at once. Ordering is
        // Release/Acquire against the L1 read so a reader that sees the old epoch
        // cannot also be holding a stale positive for this name — it has none, the
        // name is new.
        FIELD_EPOCH.fetch_add(1, std::sync::atomic::Ordering::Release);
        id
    }

    pub(crate) fn field_names_r(&self) -> std::sync::RwLockReadGuard<'_, Vec<Box<str>>> {
        self.field_names.read().unwrap_or_else(|e| e.into_inner())
    }
    pub(crate) fn field_names_w(&self) -> std::sync::RwLockWriteGuard<'_, Vec<Box<str>>> {
        self.field_names.write().unwrap_or_else(|e| e.into_inner())
    }
    pub(crate) fn field_ids_r(&self) -> std::sync::RwLockReadGuard<'_, crate::FastMap<Box<str>, u32>> {
        self.field_ids.read().unwrap_or_else(|e| e.into_inner())
    }
    pub(crate) fn field_ids_w(&self) -> std::sync::RwLockWriteGuard<'_, crate::FastMap<Box<str>, u32>> {
        self.field_ids.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Look up an already-interned field id (no allocation). A name never written
    /// to any object isn't interned, so it definitionally has no field → None.
    ///
    /// The by-name entry point: reads the switch word ONCE and hands it to the body. Callers
    /// that already hold the word (`field_id`) call [`Heap::field_id_existing_gated`] directly
    /// instead of paying a second read — the body is far too large to inline, so a nested read
    /// really is a second load of a process constant, not something LLVM folds away.
    #[inline]
    pub(crate) fn field_id_existing(&self, name: &str) -> Option<u32> {
        self.field_id_existing_gated(name, intern_gates())
    }

    /// [`Heap::field_id_existing`] with the switch word supplied by the caller.
    fn field_id_existing_gated(&self, name: &str, flags: InternFlags) -> Option<u32> {
        // TLS L1 in front of the interner. `field_ids` is an `RwLock<FastMap<Box<str>, u32>>`,
        // so the plain path pays a read-guard acquire/release plus an FxHash of the name on
        // EVERY call — and the natives read their synthetic backing fields (`$elems`, `$map`,
        // `$buf`, `$pos`, …) by NAME on every call. Caller-attributed PC sampling of a single
        // `ArrayList.size()` puts this one function at **20.7%**, the largest row by a factor
        // of two, and `ArrayList.size()` is the CHEAP end of the cascade — the end the boot
        // actually lives at (ledger row 175).
        //
        // ⚠ HISTORY, kept because the later block only makes sense against it: this L1 WAS keyed
        // by the name's ADDRESS and verified by content against a stored copy. It is now keyed by
        // the CONTENT and verified the same way — see "AND THE INDEX WAS THE NAME'S ADDRESS"
        // below for why, and `RAVA_NO_FAST_INTERN=1` to put the address index back. The address
        // alone would be faster and is NOT sound: most callers pass `&'static` literals, but
        // not all, and a freed temporary's address can be reused by a different name — which
        // would resolve the wrong FIELD, silently. One short `memcmp` buys that away and still
        // skips both the lock and the hash.
        //
        // Only a RESOLVED id is cached. Caching a `None` would freeze an answer taken before
        // the name was ever interned, and every object created afterwards would be misclassified
        // — the same rule `map.rs` already applies to its `$name`/`$skey` ids.
        // ⚠ THE MISS IS THE EXPENSIVE CASE, and it was the uncached one. A name that
        // no object has ever carried is NOT interned, so it returns `None` — and a
        // `None` was never stored, so every probe for it re-took the read guard and
        // re-hashed the name. That is the shape every synthetic-field TEST has: a
        // `$mapview` probe on a collection that is not a view, an `$immutable` probe
        // on a mutable one. Measured on the latter: **wall 57.7 -> 68.1 s** for one
        // such probe per collection call.
        //
        // The negative is now cached too, stamped with `FIELD_EPOCH` — bumped once
        // per newly interned name, which is the only event that can turn a `None`
        // into a `Some`. A stale negative is therefore impossible rather than
        // unlikely: the entry is only believed while the epoch it was taken under is
        // still current. Positives need no stamp — an id, once minted, is permanent.
        // ⛔**THE VERIFICATION COPY WAS A HEAP ALLOCATION, ON EVERY FILL.** The slot stored a
        // `Box<str>`, so every L1 MISS did `name.into()` — and a caller-attributed profile of the
        // datapack phase (the one that runs 5.3× slower than `java -Xint`, against 2.4× for the
        // boot as a whole) puts **43.4% of ALL allocation in that phase inside this closure**.
        // With 64 pointer-keyed slots and a decode that touches hundreds of distinct field names,
        // the table thrashes, so nearly every probe missed and nearly every miss allocated.
        //
        // The copy is still made — the comment above is right that address equality alone is
        // unsound — but it is made INLINE. Field names are short (`$elems`, `$map`, `$pos`, and
        // ordinary Java identifiers); anything longer than the inline buffer simply is not
        // cached, which is exactly the behaviour such a name had before this cache existed.
        // ⚠**THE TABLE MUST STAY IN CACHE, WHICH IS WHY THESE NUMBERS ARE SMALL.** The first
        // version of this used 256 slots and a 31-byte inline name — 56 bytes per slot, 14 KB of
        // table — and the boot got **6.7% SLOWER, in both orders of an interleaved A/B**. The
        // `Box<str>` slot before it was 40 bytes over 64 slots, about 2.5 KB, and it fit.
        //
        // ⭐⭐**AND THE INDEX WAS THE NAME'S ADDRESS, WHICH IS NOT A PROPERTY OF THE NAME.**
        // That is why this cluster still measures 11.6% of the datapack-reload phase after the
        // allocation was removed from it. The synthetic probes pass `&'static` literals and hit;
        // the reload does NOT — it resolves ordinary field names off strings the JSON/NBT decode
        // has just built, so the SAME name arrives at a DIFFERENT address on every document and
        // the L1 misses by construction, every time, forever. A content-derived index makes a
        // repeated name land in the same set wherever its bytes live, which is the whole path,
        // not a shaved call.
        //
        // Three consequences, all of which make the table CHEAPER as well as more effective:
        //   * the stored address disappears — it was only ever a pre-filter for the `memcmp`
        //     that decides the answer, and the `memcmp` is one 16-byte compare;
        //   * the slot is `L1Name` + id + epoch = 16 + 4 + 4 = **24 bytes**, and 64 of them is
        //     **1,536 B TOTAL** — 40% smaller than the 2,560 B it replaces;
        //   * `Cell<[…; 64]>` replaces `RefCell<Vec<Option<…>>>`, so a lookup loses the borrow
        //     flag, the heap indirection and the slice bounds check.
        //
        // Two-way, not direct-mapped, at the SAME byte size (32 sets x 2 ways): a datapack
        // decode touches hundreds of distinct field names, and two ways is what turns a pair of
        // names that collide in one set from permanent mutual eviction into two hits. The ways
        // are a depth-2 FIFO — a hit performs NO write, so the hot path stays read-only.
        const L1_SETS: usize = 32;
        const L1_WAYS: usize = 2;
        #[derive(Clone, Copy)]
        struct L1Slot {
            name: L1Name,
            /// `id + 1`, so `0` means "no such field". A resolved id is believed forever; an
            /// absence only while `epoch` is the current `FIELD_EPOCH`.
            id1: u32,
            epoch: u32,
        }
        const L1_EMPTY: L1Slot =
            L1Slot { name: L1Name::EMPTY, id1: 0, epoch: u32::MAX };
        type L1Table = [std::cell::Cell<L1Slot>; L1_SETS * L1_WAYS];
        // ⭐⭐**THE TABLE WAS THE CHEAP SHAPE; THE ACCESS WAS NOT.** Two rounds have already been
        // spent on this L1's CONTENT — a `Box<str>` copy removed from its fill, and a 14 KB
        // widening reverted at +6.8% — and both left the ACCESS untouched, because the slot type
        // (a `Cell` of a 24-byte `Copy` struct) already looks like the cheap shape. It was not
        // being STORED like one.
        //
        // `thread_local!` emits a bare TLS slot ONLY when the initialiser is literally
        // `= const { … }`; its fast arm matches the token sequence `const $init:block`. The
        // initialiser here was `[const { … }; N]` — an array-repeat expression that merely
        // CONTAINS const blocks — which starts with `[`, matches no const arm, and silently took
        // the `$init:expr` arm instead.
        //
        // SHAPE BEFORE: `LazyStorage<L1Table, ()>` = a `State` byte + `MaybeUninit<L1Table>`.
        // Every `.with` loaded that byte, compared it against `Alive` and branched, and only then
        // formed `&value`. The byte sits at offset 0 while the probed set is anywhere across
        // 1,536 B (24 cache lines), so the check normally pulled in a SECOND line — on a function
        // that is 31.1% of `Map.get` and 20.7% of `ArrayList.size()`, and that this cluster's
        // 11.6% of the datapack-reload phase is measured on. The miss path paid it TWICE (probe,
        // then refill), and the cold `get_or_init_slow` call kept the closure bodies from folding
        // flat.
        // SHAPE AFTER: the const arm ⇒ `needs_drop::<L1Table>()` is false ⇒ std emits
        // `#[thread_local] static VAL: L1Table = INIT;` and `.with` is `&VAL`. One TLS-base read,
        // then a constant displacement; no state byte, no compare, no branch, no lazy path.
        //
        // ⚠A memo must not outlive what it names: this table names nothing outside the thread and
        // holds no owner — `L1Slot` is 24 bytes of `Copy` integers, so a per-thread destructor was
        // never needed and dropping it at thread exit was never doing any work. Removing the lazy
        // storage removes only the init bookkeeping.
        //
        // Both forms are declared so `RAVA_NO_FAST_HEAPTLS=1` prices exactly this and nothing
        // else. They are two independent caches of the same facts — same sets, same ways, same
        // epoch discipline, same content verification — and only one is touched in a process, so
        // the unused one costs address space in `.tls` and never a cache line.
        std::thread_local! {
            static IDS: L1Table = const { [const { std::cell::Cell::new(L1_EMPTY) }; L1_SETS * L1_WAYS] };
            static IDS_LAZY: L1Table = [const { std::cell::Cell::new(L1_EMPTY) }; L1_SETS * L1_WAYS];
        }
        /// The probe body, written ONCE and shared by both storage forms so the arms cannot drift.
        #[inline(always)]
        fn l1_probe(t: &L1Table, base: usize, key: L1Name, simd: bool) -> Option<Option<u32>> {
            if simd {
                for w in 0..L1_WAYS {
                    let s = t[base + w].get();
                    // The empty slot is an all-zero name, which no real field name can equal.
                    if s.name == key {
                        if s.id1 != 0 {
                            return Some(Some(s.id1 - 1));
                        }
                        // Absence: NOW the epoch matters, and only now is it read.
                        return if s.epoch == FIELD_EPOCH.load(std::sync::atomic::Ordering::Acquire)
                        {
                            Some(None)
                        } else {
                            None
                        };
                    }
                }
                None
            } else {
                let epoch = FIELD_EPOCH.load(std::sync::atomic::Ordering::Acquire);
                for w in 0..L1_WAYS {
                    let s = t[base + w].get();
                    if s.name == key && (s.id1 != 0 || s.epoch == epoch) {
                        return Some(if s.id1 == 0 { None } else { Some(s.id1 - 1) });
                    }
                }
                None
            }
        }
        /// The depth-2 FIFO refill, likewise shared.
        #[inline(always)]
        fn l1_fill(t: &L1Table, base: usize, key: L1Name, id1: u32, epoch: u32) {
            for w in (1..L1_WAYS).rev() {
                t[base + w].set(t[base + w - 1].get());
            }
            t[base].set(L1Slot { name: key, id1, epoch });
        }
        // One flag read for all switches, hoisted above the key build so the key CONSTRUCTION
        // can be A/B'd without a second `Acquire` load appearing on this path. `heaptls` rides
        // along in the same already-loaded struct, so selecting the storage form costs a test on
        // a byte that is already in a register — never a second load.
        //
        // ⭐**AND THE READ ITSELF HAS MOVED OUT OF THIS FUNCTION.** It is now the caller's, so
        // the interning entry point (`field_id`) stops paying for it twice. See `intern_gates`
        // for what the load lowers to; nothing on this path reads a static any more except
        // `FIELD_EPOCH`, and that one is deferred to the absence case below.
        let InternFlags { fast, fast2, simd, heaptls, .. } = flags;
        // Too long (or empty) for the inline buffer ⇒ uncacheable, exactly as before.
        let Some(key) = L1Name::new(name, fast2) else {
            return self.field_ids_r().get(name).copied();
        };
        let set = if fast {
            // High 32 bits: the fold's low bits carry the multiply's weakest mixing.
            (key.fold() >> 32) as usize & (L1_SETS - 1)
        } else {
            // RAVA_NO_FAST_INTERN: the historic address-derived index. Verification is
            // unchanged (it was always the content compare that made the answer sound), so
            // this arm differs from the default only in WHICH set a name lands in.
            ((name.as_ptr() as usize) >> 4) & (L1_SETS - 1)
        };
        let base = set * L1_WAYS;
        // ⭐**THE ATOMIC WAS PAID BY THE ANSWER THAT DOES NOT NEED IT.** `FIELD_EPOCH` only
        // decides whether a cached ABSENCE is still true; a cached id is permanent and is believed
        // forever. The load nevertheless ran ahead of the probe on EVERY call — and this function
        // is 31.1% of `Map.get` and 20.7% of `ArrayList.size()`, so its hot path is where an
        // `Acquire` load that LLVM may not hoist, CSE or reorder across is least affordable (this
        // tree has already priced that exact shape at 15% once, on the dispatch push path).
        //
        // Deferring it needs one property: the FIRST way whose name matches is authoritative, so
        // the probe may return on it instead of scanning on to find a fresher copy. It is. A
        // refill pushes into way 0 and shifts the old way 0 into way 1, so way 1's entry for a
        // given key is always the OLDER of the two and its epoch can only be older. And a stale
        // NEGATIVE at way 0 can never sit in front of a live POSITIVE at way 1: a negative is only
        // ever written after `field_ids` returned `None` for that name, and an id, once minted, is
        // permanent — so `None` can never be observed after a `Some` for the same name, and the
        // (K, absent) refill can never follow a (K, present) one. Same entries believed, same ids
        // returned; only the read moves.
        let hit = if heaptls {
            IDS.with(|t| l1_probe(t, base, key, simd))
        } else {
            IDS_LAZY.with(|t| l1_probe(t, base, key, simd))
        };
        if let Some(id) = hit {
            return id;
        }
        // ⚠**SAMPLED BEFORE THE MAP READ, AND THE ORDER IS LOAD-BEARING.** If the epoch were read
        // AFTER `get`, another thread could intern exactly this name in between, bump the counter,
        // and this thread would then stamp its already-stale `None` with the NEW epoch — a
        // negative that stays believed until some unrelated third name is interned. Reading first
        // means any interning that races us leaves our stamp behind the counter, so the entry is
        // simply disbelieved.
        let epoch = FIELD_EPOCH.load(std::sync::atomic::Ordering::Acquire);
        let id = self.field_ids_r().get(name).copied();
        // `u32::MAX` has no room in the `id + 1` encoding; it would read back as an absence,
        // which is the one error this cache must never make. Unreachable in practice (ids are
        // indices into `field_names`) — refuse to cache rather than rely on that.
        let id1 = match id {
            Some(v) if v == u32::MAX => return id,
            Some(v) => v + 1,
            None => 0,
        };
        if heaptls {
            IDS.with(|t| l1_fill(t, base, key, id1, epoch));
        } else {
            IDS_LAZY.with(|t| l1_fill(t, base, key, id1, epoch));
        }
        id
    }

    /// THE SYNTHETIC NAMES THIS ENGINE PROBES ON HOT PATHS, ADDRESSED BY A CONSTANT.
    ///
    /// `field_id_existing` is already cached — a content-keyed, content-verified L1. It is not
    /// missing; it is being HIT several times per operation, and a hit still costs a TLS access,
    /// a slot index folded from the name's bytes and a 16-byte
    /// compare. Measured: deriving a map key from a plain `new Object()` — an object with NO
    /// FIELDS AT ALL — probes six of these names (`$str`, `$name`, `$path`, `$keystr`, `$map`,
    /// `$elems`), and `field_id_existing` is **31.1% of `Map.get`** with `Heap::get_field` a
    /// further 10.6% (`bench/MapKeyProf.java`). Widening the L1 from 64 to 128 slots moved that
    /// number by nothing (643 ns vs 644), which is the same statement from the other side.
    ///
    /// These names are `&'static` literals known at compile time, so they need no hashing and no
    /// address keying: the caller names a VARIANT and the slot is its discriminant. One array
    /// read and one compare replace the whole L1 path.
    ///
    /// The epoch discipline is the L1's, unchanged and for the same reason: a resolved id is
    /// permanent, an ABSENCE is only true until the next name is interned (`FIELD_EPOCH`), so a
    /// negative is believed only for the epoch it was taken under. A name that no object has yet
    /// carried is exactly the common case here — `$str` on a non-string, `$elems` on a non-list —
    /// and caching that negative wrongly would misclassify every object created afterwards.
    pub(crate) fn syn_field_id(&self, s: Syn) -> Option<u32> {
        let i = s as usize;
        // (id + 1, epoch). `id + 1 != 0` = resolved, believed forever. `0` = absent, believed
        // only while `epoch` is current. The initial `(0, u32::MAX)` is neither: it reads as an
        // absence stamped with an epoch the counter would have to wrap to reach.
        // ⭐**CONST-INIT, UNGATED — AND THE MISSING SWITCH IS THE MEASUREMENT, NOT AN OVERSIGHT.**
        // Same defect as `IDS`/`AE_IDS`: `[const { … }; N]` is an array-repeat expression, not the
        // `= const { … }` the macro's fast arm matches, so this 144-byte table of `Copy` pairs was
        // living in `LazyStorage` and every `.with` loaded a `State` byte, compared it and
        // branched before reaching the slot. The const arm makes it a bare
        // `#[thread_local] static` — one TLS-base read and a constant displacement.
        //
        // This one is NOT wired to `RAVA_NO_FAST_HEAPTLS`, deliberately, and the switch word
        // does not change that. `syn_field_id`'s POSITIVE path reads no static at all today —
        // one TLS-base read, a constant displacement, a test — so giving it a runtime switch
        // means an `intern_gates()` load plus a test and a branch on that path, which is
        // instruction-for-instruction the lazy-init check being removed. A gated arm would
        // price its own guard and report zero. This file already refuses that trade once, in
        // `InternFlags`'s "two `OnceLock`s would mean two `Acquire` loads" note, and the whole
        // point of the word is that gate reads are counted, not that they are free. There is
        // nothing to kill either:
        // the type, the values, the slot indices and the epoch discipline are unchanged, so no
        // answer this function can return depends on which storage form std picked.
        std::thread_local! {
            static SYN: [std::cell::Cell<(u32, u32)>; SYN_NAMES.len()] =
                const { [const { std::cell::Cell::new((0, u32::MAX)) }; SYN_NAMES.len()] };
        }
        let cached = SYN.with(|t| t[i].get());
        if cached.0 != 0 {
            return Some(cached.0 - 1);
        }
        let epoch = FIELD_EPOCH.load(std::sync::atomic::Ordering::Acquire);
        if cached.1 == epoch {
            return None;
        }
        let id = self.field_ids_r().get(SYN_NAMES[i]).copied();
        SYN.with(|t| {
            t[i].set(match id {
                Some(v) => (v.wrapping_add(1), 0),
                None => (0, epoch),
            })
        });
        id
    }

    /// Public view of `field_id_existing` — lets the JIT fast paths resolve a synthetic
    /// field name (`$pos`/`$elems`) once and then read/write by id (no per-call hash).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn field_id_opt(&self, name: &str) -> Option<u32> {
        self.field_id_existing(name)
    }

    /// Intern a field name to its stable id (pub for the bytecode call-site
    /// field cache, which resolves the name once and reuses the id).
    pub(crate) fn intern_field(&self, name: &str) -> u32 {
        self.field_id(name)
    }

    /// Reverse field-id lookup (`Owner#name`) — diagnostics only (RAVA_TRACE_STALEFIELD).
    /// Returns an owned name rather than a borrow: the interner is behind an `RwLock`, so a
    /// reference cannot outlive the guard. Diagnostics-only, so the clone is free of consequence.
    pub(crate) fn field_name(&self, id: u32) -> Option<String> {
        self.field_names_r().get(id as usize).map(|s| s.to_string())
    }

    /// Pre-populate a freshly allocated object with its full per-class field
    /// layout (id, default-kind) — every declared instance field present at slot
    /// 0..N with a JVM-default value (cell 0). After this, every putfield hits an
    /// existing slot (native, no append/realloc) and every getfield finds its
    /// slot at a class-constant index (the lever-#1 foundation). Only called on an
    /// object with no fields yet; synthetic fields ($map/$elems/…) still append
    /// after the layout slots. No-op if `layout` is empty.
    pub(crate) fn prepopulate_layout(&self, handle: u32, layout: &[(u32, u8)]) {
        if layout.is_empty() {
            return;
        }
        if let Some(o) = self.objects.write().unwrap_or_else(|e| e.into_inner()).get_mut(handle as usize).and_then(ObjectSlot::get_mut) {
            // Idempotent: an object pre-populated once (by the alloc choke-point)
            // must not be re-populated by new_object's prepopulate_new (it would
            // double the field list). Already-populated ⇒ nothing to do.
            if !o.fields.is_empty() {
                return;
            }
            o.fields.reserve(layout.len());
            for &(id, kind) in layout {
                o.fields.push(id, 0, kind);
            }
        }
        self.bytes.fetch_add(13 * layout.len(), std::sync::atomic::Ordering::Relaxed);
        self.sync_obj_desc(handle);
    }
}
