//! The constant pool: the spine of every class file. Indices are 1-based and
//! `Long`/`Double` occupy two slots (JVMS 4.4). We store every entry so offsets
//! stay correct, and expose resolution through methods — callers never index
//! the raw vector.

use crate::reader::Reader;
use crate::ClassError;

#[derive(Debug, Clone)]
pub enum Constant {
    /// Unusable slot following a Long/Double, plus index 0.
    Unusable,
    Utf8(String),
    Integer(i32),
    Float(f32),
    Long(i64),
    Double(f64),
    Class { name_index: u16 },
    String { utf8_index: u16 },
    FieldRef { class_index: u16, nat_index: u16 },
    MethodRef { class_index: u16, nat_index: u16 },
    InterfaceMethodRef { class_index: u16, nat_index: u16 },
    NameAndType { name_index: u16, desc_index: u16 },
    MethodHandle { ref_kind: u8, ref_index: u16 },
    MethodType { desc_index: u16 },
    Dynamic { bootstrap: u16, nat_index: u16 },
    InvokeDynamic { bootstrap: u16, nat_index: u16 },
    Module { name_index: u16 },
    Package { name_index: u16 },
}

pub struct ConstantPool {
    entries: Vec<Constant>,
}

impl ConstantPool {
    /// Parse the pool of a file whose header said `constant_pool_count == count`;
    /// the usable indices are `1..count` (JVMS 4.1 — the count is one MORE than the
    /// number of entries), which is exactly what the loop below walks.
    pub fn parse(reader: &mut Reader, count: u16) -> Result<Self, ClassError> {
        // Index 0 is always invalid; pad it so vector index == pool index.
        // The capacity is exact: one pad plus at most `count - 1` slots, and a
        // Long/Double consumes two of those slots for one entry, so the vector
        // never grows — `count` is in the header, we are not discovering it.
        let mut entries = Vec::with_capacity(count as usize + 1);
        entries.push(Constant::Unusable);

        // Read the MUTF-8 mode ONCE per class rather than once per Utf8 constant.
        // It is a process constant behind a `OnceLock`, i.e. an acquire load, and a
        // 15,000-class boot decodes millions of Utf8 entries; an atomic inside that
        // loop is a barrier LLVM cannot hoist, which is the exact shape that has
        // measured badly elsewhere in this engine. One load, passed down by value.
        let mode = mutf8_mode();

        let mut i = 1u16;
        while i < count {
            let tag = reader.u8()?;
            let constant = Self::parse_entry(reader, tag, mode)?;
            let wide = matches!(constant, Constant::Long(_) | Constant::Double(_));
            entries.push(constant);
            if wide {
                // Long/Double swallow the next index.
                entries.push(Constant::Unusable);
                i += 2;
            } else {
                i += 1;
            }
        }
        Ok(Self { entries })
    }

    fn parse_entry(reader: &mut Reader, tag: u8, mode: u8) -> Result<Constant, ClassError> {
        Ok(match tag {
            1 => {
                let len = reader.u16()? as usize;
                let raw = reader.bytes(len)?;
                Constant::Utf8(decode_modified_utf8(mode, raw))
            }
            3 => Constant::Integer(reader.u32()? as i32),
            4 => Constant::Float(f32::from_bits(reader.u32()?)),
            5 => Constant::Long(reader.i64()?),
            6 => Constant::Double(f64::from_bits(reader.f64_bits()?)),
            7 => Constant::Class { name_index: reader.u16()? },
            8 => Constant::String { utf8_index: reader.u16()? },
            9 => Constant::FieldRef { class_index: reader.u16()?, nat_index: reader.u16()? },
            10 => Constant::MethodRef { class_index: reader.u16()?, nat_index: reader.u16()? },
            11 => Constant::InterfaceMethodRef { class_index: reader.u16()?, nat_index: reader.u16()? },
            12 => Constant::NameAndType { name_index: reader.u16()?, desc_index: reader.u16()? },
            15 => Constant::MethodHandle { ref_kind: reader.u8()?, ref_index: reader.u16()? },
            16 => Constant::MethodType { desc_index: reader.u16()? },
            17 => Constant::Dynamic { bootstrap: reader.u16()?, nat_index: reader.u16()? },
            18 => Constant::InvokeDynamic { bootstrap: reader.u16()?, nat_index: reader.u16()? },
            19 => Constant::Module { name_index: reader.u16()? },
            20 => Constant::Package { name_index: reader.u16()? },
            other => return Err(ClassError::BadConstantTag(other)),
        })
    }

    pub fn get(&self, index: u16) -> Option<&Constant> {
        self.entries.get(index as usize)
    }

    /// Build a pool directly from a slot vector (index 0 should be `Unusable`).
    /// Used by tests and tooling that synthesize bytecode.
    pub fn from_entries(entries: Vec<Constant>) -> Self {
        ConstantPool { entries }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.len() <= 1
    }

    /// Resolve a Utf8 entry to its string.
    pub fn utf8(&self, index: u16) -> Option<&str> {
        match self.get(index)? {
            Constant::Utf8(s) => Some(s),
            _ => None,
        }
    }

    /// Resolve a String constant (`CONSTANT_String`) to its UTF-8 text.
    pub fn string_const(&self, index: u16) -> Option<&str> {
        match self.get(index)? {
            Constant::String { utf8_index } => self.utf8(*utf8_index),
            _ => None,
        }
    }

    /// Resolve a Class entry to its binary name (e.g. `java/lang/Object`).
    pub fn class_name(&self, index: u16) -> Option<&str> {
        match self.get(index)? {
            Constant::Class { name_index } => self.utf8(*name_index),
            _ => None,
        }
    }

    /// Resolve a NameAndType entry to `(name, descriptor)`.
    pub fn name_and_type(&self, index: u16) -> Option<(&str, &str)> {
        match self.get(index)? {
            Constant::NameAndType { name_index, desc_index } => {
                Some((self.utf8(*name_index)?, self.utf8(*desc_index)?))
            }
            _ => None,
        }
    }

    /// Resolve a Field/Method/InterfaceMethod ref to `(owner, name, descriptor)`.
    pub fn member_ref(&self, index: u16) -> Option<MemberRef<'_>> {
        let (class_index, nat_index) = match self.get(index)? {
            Constant::FieldRef { class_index, nat_index }
            | Constant::MethodRef { class_index, nat_index }
            | Constant::InterfaceMethodRef { class_index, nat_index } => {
                (*class_index, *nat_index)
            }
            _ => return None,
        };
        let (name, descriptor) = self.name_and_type(nat_index)?;
        Some(MemberRef { owner: self.class_name(class_index)?, name, descriptor })
    }

    /// Resolve a MethodHandle constant to the member it references.
    pub fn method_handle(&self, index: u16) -> Option<MemberRef<'_>> {
        match self.get(index)? {
            Constant::MethodHandle { ref_index, .. } => self.member_ref(*ref_index),
            _ => None,
        }
    }

    /// Resolve a MethodHandle to `(reference_kind, member)` (JVMS 4.4.8:
    /// 6=invokeStatic, 5=invokeVirtual, 7=invokeSpecial, 9=invokeInterface, ...).
    pub fn method_handle_full(&self, index: u16) -> Option<(u8, MemberRef<'_>)> {
        match self.get(index)? {
            Constant::MethodHandle { ref_kind, ref_index } => {
                Some((*ref_kind, self.member_ref(*ref_index)?))
            }
            _ => None,
        }
    }

    /// Resolve an InvokeDynamic constant to `(bootstrap_index, name, descriptor)`.
    pub fn invoke_dynamic(&self, index: u16) -> Option<(u16, &str, &str)> {
        match self.get(index)? {
            Constant::InvokeDynamic { bootstrap, nat_index } => {
                let (name, desc) = self.name_and_type(*nat_index)?;
                Some((*bootstrap, name, desc))
            }
            _ => None,
        }
    }
}

/// A resolved field/method reference (borrows from the pool).
#[derive(Debug, Clone, Copy)]
pub struct MemberRef<'a> {
    pub owner: &'a str,
    pub name: &'a str,
    pub descriptor: &'a str,
}

/// Decode JVM "modified UTF-8" (JVMS 4.4.7). Handles the common ASCII fast path
/// and the multi-byte forms; malformed bytes become U+FFFD rather than panicking.
/// Map a UTF-16 surrogate code unit (0xD800..=0xDFFF) into Private-Use Area A
/// (0xE000..=0xE7FF) so it survives in a Rust `String` (which forbids lone
/// surrogates). Stays a single BMP unit, so `length`/`charAt` indexing is
/// unchanged. The char natives reverse it via [`unmap_surrogate`]. guava's
/// `CharMatcher.invisible()` range table literally contains `\uD800`, and
/// without round-tripping it `0xFFFD <= 0xF8FF` failed its `checkArgument`.
pub const SURROGATE_PUA_BASE: u32 = 0xE000;
fn map_surrogate(s: u32) -> char {
    char::from_u32(SURROGATE_PUA_BASE + (s - 0xD800)).unwrap_or('\u{FFFD}')
}

// ─────────────────────────────────────────────────────────────────────────────
// MUTF-8: THE ASCII IDENTITY.
//
// `decode_modified_utf8` runs once per `CONSTANT_Utf8` — every class name, every
// member name, every descriptor, every attribute name, every string literal — so
// several million times across the ~15,000 classes a product boot parses, all of
// it one-shot work no JIT ever amortises. Per BYTE it used to do:
// `char::from_u32(cp).unwrap_or(FFFD)`, two `RangeInclusive::contains` calls
// against the surrogate windows, and a `String::push` (which re-tests the char
// width and re-checks capacity). Ten-odd instructions to move one byte.
//
// The saving is not an approximation, it is an IDENTITY. Read the scalar decoder
// below with `b < 0x80` in hand:
//   * `unit` takes its FIRST branch and yields `(b as u32, 1)` — no continuation
//     byte is read, so no two/three-byte form is reachable;
//   * `cp = b <= 0x7F` lies in neither `0xD800..=0xDBFF` nor `0xDC00..=0xDFFF`,
//     so no surrogate branch is reachable and `map_surrogate` is never called;
//   * `char::from_u32(b)` is `Some(b as char)` for every `b <= 0x7F`, so the
//     `unwrap_or(FFFD)` never fires and the pushed char is `b` itself;
//   * `String::push` of a char below 0x80 appends exactly the one byte `b`.
// ⇒ if EVERY byte is below 0x80 the decoder's output is the input, byte for byte,
// and `String::from_utf8_unchecked` over a copy of the input is sound because a
// slice of bytes all below 0x80 is by definition valid UTF-8. The empty slice is
// the degenerate case of the same statement and yields `""` on both paths.
//
// So the whole decision is ONE BIT over the whole buffer — "does any byte have
// bit 7 set" — and that is what the vector code computes. A buffer with any high
// bit falls to the scalar decoder for the WHOLE buffer, unchanged; there is no
// partial-vector decoder and therefore no mixed case to get wrong. JVM binary
// names, descriptors and member names are ASCII by construction, so the fast path
// is the case by an enormous margin and the scalar decoder survives for the
// literals that need it.
//
// COST IF REMOVED: `RAVA_NO_SIMD_MUTF8=1` (or the block switch `RAVA_NO_EMIT_MORE=1`)
// skips the pre-test entirely and runs the original scalar decoder verbatim.
// ─────────────────────────────────────────────────────────────────────────────

/// No ASCII pre-test at all — the original per-byte decoder. What the kill switch restores.
const MUTF8_LEGACY: u8 = 0;
/// Portable byte-OR pre-test. Only reachable on a non-x86_64 build, where the two
/// vector modes below are `cfg`-ed out — hence the `allow`, which is a statement about
/// the target and not about the constant being spare.
#[allow(dead_code)]
const MUTF8_SCALAR: u8 = 1;
/// Pre-test folded 16 bytes at a time. SSE2 is baseline on x86_64 and needs no probe.
const MUTF8_SSE2: u8 = 2;
/// Pre-test folded 32 bytes at a time. Requires a runtime `avx2` probe.
const MUTF8_AVX2: u8 = 3;

/// Which MUTF-8 implementation this process uses.
///
/// MEMOISED: on Windows `var_os` is a linear walk of the environment block plus an
/// allocation, and the question would otherwise be asked once per Utf8 constant. The
/// only writer of the environment is the CLI's config apply, which completes before any
/// class is parsed, so a process constant is the honest model. Read once per class in
/// [`ConstantPool::parse`] and threaded down by value — see the note there.
///
/// DUPLICATED, not imported, exactly as `crate::flags` is: `rava-interp` depends on
/// `rava-classfile`, so consulting its `flags::emit_more_disabled()` from here would be a
/// dependency cycle. The block switch is the same env var, so one same-binary A/B against
/// `RAVA_NO_EMIT_MORE` still prices this change together with the rest of the round.
#[inline]
fn mutf8_mode() -> u8 {
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(detect_mutf8_mode)
}

#[allow(unreachable_code)]
fn detect_mutf8_mode() -> u8 {
    if std::env::var_os("RAVA_NO_SIMD_MUTF8").is_some()
        || std::env::var_os("RAVA_NO_EMIT_MORE").is_some()
    {
        return MUTF8_LEGACY;
    }
    #[cfg(target_arch = "x86_64")]
    {
        // SSE2 is baseline on x86_64 — no probe, no fallback needed. AVX2 is probed
        // because a portable build must stay correct on a pre-Haswell part, not because
        // the narrow path is expected to be the one taken.
        if is_x86_feature_detected!("avx2") {
            return MUTF8_AVX2;
        }
        return MUTF8_SSE2;
    }
    MUTF8_SCALAR
}

/// True iff no byte in `bytes` has bit 7 set. Pure predicate — it reads the buffer and
/// returns a bool, so it cannot alter what the decoder produces, only which decoder runs.
#[inline]
fn all_ascii(mode: u8, bytes: &[u8]) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        if mode == MUTF8_AVX2 && bytes.len() >= 32 {
            // SAFETY: `MUTF8_AVX2` is only ever produced by `detect_mutf8_mode` AFTER
            // `is_x86_feature_detected!("avx2")` returned true, and the mode is a process
            // constant, so this cannot be reached on a part without the instructions.
            return unsafe { all_ascii_avx2(bytes) };
        }
        if mode >= MUTF8_SSE2 && bytes.len() >= 16 {
            return all_ascii_sse2(bytes);
        }
    }
    let _ = mode;
    all_ascii_scalar(bytes)
}

/// Byte-OR fold. `acc` accumulates the OR of every byte, so `acc < 0x80` iff no byte had
/// bit 7 set — the same predicate the vector forms compute, on every length including 0.
#[inline(always)]
fn all_ascii_scalar(bytes: &[u8]) -> bool {
    let mut acc = 0u8;
    for &b in bytes {
        acc |= b;
    }
    acc < 0x80
}

/// 16 bytes per fold.
///
/// COVERAGE — why the answer equals the scalar predicate for every length:
///  * the caller guarantees `n >= 16`;
///  * the indexed chunks cover `[0, 16*floor(n/16))` and the final OVERLAPPING load covers
///    `[n-16, n)`; since `n - 16*floor(n/16) = n mod 16 < 16` the two ranges meet or
///    overlap, so every byte of `[0, n)` is folded in. OR is idempotent, so re-folding a
///    byte cannot change the verdict;
///  * `_mm_movemask_epi8` gathers exactly bit 7 of each accumulated lane, so `== 0` says
///    no lane — hence no byte — had its high bit set.
///
/// NEVER READS PAST THE END: every load offset is `i` with `i + 16 <= n`, or exactly
/// `n - 16` with `n >= 16`. No 16-byte load is issued for a shorter slice, so there is no
/// page-crossing tail read.
#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn all_ascii_sse2(bytes: &[u8]) -> bool {
    use core::arch::x86_64::{
        __m128i, _mm_loadu_si128, _mm_movemask_epi8, _mm_or_si128, _mm_setzero_si128,
    };
    let n = bytes.len();
    debug_assert!(n >= 16);
    let p = bytes.as_ptr();
    // SAFETY: see the coverage note above — every load lies inside the slice.
    unsafe {
        let mut acc = _mm_setzero_si128();
        let mut i = 0usize;
        while i + 16 <= n {
            acc = _mm_or_si128(acc, _mm_loadu_si128(p.add(i) as *const __m128i));
            i += 16;
        }
        // Unconditional overlapping tail: cheaper than a branch, and harmless when
        // `n % 16 == 0` because OR of an already-folded window is a no-op.
        acc = _mm_or_si128(acc, _mm_loadu_si128(p.add(n - 16) as *const __m128i));
        _mm_movemask_epi8(acc) == 0
    }
}

/// 32 bytes per fold. Same coverage argument as [`all_ascii_sse2`] with 16 replaced by 32,
/// and lengths in `16..32` are routed to the SSE2 form by [`all_ascii`] rather than being
/// handled here, so no 32-byte load is ever issued for a slice shorter than 32.
///
/// SAFETY: caller must have observed `is_x86_feature_detected!("avx2")`.
#[cfg(target_arch = "x86_64")]
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn all_ascii_avx2(bytes: &[u8]) -> bool {
    use core::arch::x86_64::{
        __m256i, _mm256_loadu_si256, _mm256_movemask_epi8, _mm256_or_si256, _mm256_setzero_si256,
    };
    let n = bytes.len();
    debug_assert!(n >= 32);
    let p = bytes.as_ptr();
    let mut acc = _mm256_setzero_si256();
    let mut i = 0usize;
    while i + 32 <= n {
        acc = _mm256_or_si256(acc, _mm256_loadu_si256(p.add(i) as *const __m256i));
        i += 32;
    }
    acc = _mm256_or_si256(acc, _mm256_loadu_si256(p.add(n - 32) as *const __m256i));
    _mm256_movemask_epi8(acc) == 0
}

fn decode_modified_utf8(mode: u8, bytes: &[u8]) -> String {
    if mode != MUTF8_LEGACY && all_ascii(mode, bytes) {
        // SAFETY: `all_ascii` proved every byte is below 0x80. A byte sequence in which
        // every byte is below 0x80 is valid UTF-8 (each is a one-byte sequence), and by
        // the identity argued above it is also exactly what the scalar decoder would have
        // produced for this input. One allocation and one `memcpy`, the same allocation
        // count the scalar path pays via `String::with_capacity`.
        return unsafe { String::from_utf8_unchecked(bytes.to_vec()) };
    }
    decode_modified_utf8_scalar(bytes)
}

/// The original per-byte decoder, unchanged. Reached for any buffer containing a byte with
/// bit 7 set — i.e. anything actually encoded in MUTF-8's multi-byte or surrogate forms —
/// and for every buffer when the kill switch is set.
fn decode_modified_utf8_scalar(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    // Read one MUTF-8 unit (1/2/3 bytes) → code point; returns (cp, consumed).
    let unit = |i: usize| -> (u32, usize) {
        let b = bytes[i];
        if b < 0x80 {
            (b as u32, 1)
        } else if b & 0xE0 == 0xC0 && i + 1 < bytes.len() {
            ((((b & 0x1F) as u32) << 6) | (bytes[i + 1] & 0x3F) as u32, 2)
        } else if b & 0xF0 == 0xE0 && i + 2 < bytes.len() {
            (
                (((b & 0x0F) as u32) << 12)
                    | (((bytes[i + 1] & 0x3F) as u32) << 6)
                    | (bytes[i + 2] & 0x3F) as u32,
                3,
            )
        } else {
            (0xFFFD, 1)
        }
    };
    while i < bytes.len() {
        let (cp, n) = unit(i);
        i += n;
        if (0xD800..=0xDBFF).contains(&cp) {
            // High surrogate — pair with a following low surrogate (MUTF-8
            // encodes supplementary chars as a surrogate pair) into one char.
            if i < bytes.len() {
                let (lo, m) = unit(i);
                if (0xDC00..=0xDFFF).contains(&lo) {
                    i += m;
                    let c = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                    out.push(char::from_u32(c).unwrap_or('\u{FFFD}'));
                    continue;
                }
            }
            out.push(map_surrogate(cp)); // lone high surrogate
        } else if (0xDC00..=0xDFFF).contains(&cp) {
            out.push(map_surrogate(cp)); // lone low surrogate
        } else {
            out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
        }
    }
    out
}

/// Reverse [`map_surrogate`]: a PUA-A char produced from a decoded lone
/// surrogate maps back to its 0xD800..=0xDFFF UTF-16 code-unit value. Other
/// code points pass through. Char-producing natives (toCharArray/charAt/...)
/// call this so Java sees the original surrogate value.
pub fn unmap_surrogate(c: u32) -> u32 {
    if (SURROGATE_PUA_BASE..=SURROGATE_PUA_BASE + 0x7FF).contains(&c) {
        0xD800 + (c - SURROGATE_PUA_BASE)
    } else {
        c
    }
}
