//! Java string formatting: the %-format engine and its half-up rounding.

use crate::natives::*;

// ────────────────────────────────────────────────────────────────────────────────────────────
// ONE WORD FOR EVERY PROCESS CONSTANT THIS FILE READS
//
// ⭐**SHAPE BEFORE:** four independent `OnceLock`s, each read at its own call site, and two of
// them read TWO acquire-ordered atomics apiece because `&& !emit_more_disabled()` sat OUTSIDE the
// `get_or_init` closure — so `fast_fmt()` and `simd_scan_on()` were 2 loads each, not 1, and
// `have_avx2()` was a further load per VECTOR SCAN, i.e. once per literal run inside
// `format_java`'s loop and once per width/precision measurement.
//
// An acquire load is an optimisation barrier: LLVM may not hoist it out of the caller's loop and
// may not forward one across the opaque calls between these sites, so the format loop re-read
// `have_avx2()` for every `%`-delimited run of a format string it had already decided to vectorise.
//
// ⭐**SHAPE AFTER:** ONE `OnceLock<u8>`, read once at the natural entry (`format_java`, or the
// public accessor for an outside caller) and THREADED DOWN as a plain `u8` argument. The scan
// helpers take the word instead of a `bool`, so "vector scans armed" and "AVX2 present" are two
// predicted bit tests on a register the caller already holds.
//
// POPULATION: every `String.format` / `Formatter` / `printf` call in the engine — `strings.rs`
// (two sites), `io.rs`, `opcodes/invoke_virtual.rs` — plus every `%f`/`%e`/`%g` conversion inside
// one (`fixed_half_up`, `round_significant`, `general_half_up` each read `fast_fmt()`), plus the
// `BitSet` word walks in `buffers.rs`, which call `simd_scan_on()`.
//
// Every bit is the SAME predicate read from the SAME accessor, so an armed run behaves exactly as
// it did. This is `natives::entry::entry_gates()`'s shape, applied to the formatter.
// ────────────────────────────────────────────────────────────────────────────────────────────

/// `RAVA_NO_FAST_NBT` is UNSET: the serialisation-side complexity fixes in this file are armed.
const FG_FASTFMT: u8 = 1 << 0;
/// `RAVA_NO_SIMD_JSON` is UNSET: the hand-vectorised byte scans in this file (and the `BitSet`
/// word walks in `buffers.rs`) are armed.
const FG_SIMD: u8 = 1 << 1;
/// `cpuid` reported AVX2. Folded in here because it is a process constant like the others and was
/// being re-read, as an acquire load, once per scan. (Only consulted by the x86_64 kernels.)
#[allow(dead_code)]
const FG_AVX2: u8 = 1 << 2;
/// KILL SWITCH `RAVA_NO_FAST_NATSHAPE=1` (or `RAVA_NO_EMIT_MORE`): the constant that was being
/// read PER SCAN — AVX2 availability — is read back at its own call site, from its own static,
/// exactly as it was before this round, so the A/B measures the READS and not the answers, which
/// never differed. `FG_FASTFMT` and `FG_SIMD` need no such restoration: `format_java` already
/// hoisted both into locals, so folding them removed a duplicated `emit_more_disabled()` load and
/// nothing that was per-scan. (x86_64 only — the scan kernels are the only re-readable site.)
#[allow(dead_code)]
const FG_LEGACY: u8 = 1 << 3;

/// The whole file's process constants, decided once per process.
#[inline]
fn fmt_gates() -> u8 {
    static W: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *W.get_or_init(|| {
        let mut w = 0u8;
        let block_off = crate::flags::emit_more_disabled();
        if std::env::var_os("RAVA_NO_FAST_NBT").is_none() && !block_off {
            w |= FG_FASTFMT;
        }
        if std::env::var_os("RAVA_NO_SIMD_JSON").is_none() && !block_off {
            w |= FG_SIMD;
        }
        #[cfg(target_arch = "x86_64")]
        if have_avx2() {
            w |= FG_AVX2;
        }
        // Falls together with the other emit-more switches, as every gate in this tree does.
        if std::env::var_os("RAVA_NO_FAST_NATSHAPE").is_some() || block_off {
            w |= FG_LEGACY;
        }
        w
    })
}

/// ⭐**ONE SWITCH FOR THE WHOLE SERIALISATION-SIDE REWRITE IN THIS FILE** (`RAVA_NO_FAST_NBT=1`),
/// and it also folds under `RAVA_NO_EMIT_MORE` so a single same-binary boot A/B prices this file
/// with the rest of the block rather than asking for a boot per function.
///
/// WAS two acquire loads (its own `OnceLock`, then `emit_more_disabled()`'s, because the `&&` sat
/// outside the closure). IS one bit of `fmt_gates()`; `format_java` still hoists it into a local so
/// a long format string reads the word once rather than once per conversion.
#[inline]
fn fast_fmt() -> bool {
    fmt_gates() & FG_FASTFMT != 0
}

// ────────────────────────────────────────────────────────────────────────────────────────────
// HAND-VECTORISED BYTE SCANS
//
// Three byte loops in this file walk a buffer one byte at a time asking a question that is
// INDEPENDENT per byte — "is this `%`?", "is this a UTF-8 continuation byte?" — which is exactly
// the shape a 16- or 32-lane compare answers in one instruction. LLVM will not vectorise any of
// them on its own: all three EXIT EARLY (a `position`, a `count` consumed by a bound, a `nth`), and
// an early exit makes the trip count data-dependent, which is the one thing auto-vectorisation
// cannot handle without speculation it is not allowed to perform on a `&[u8]` whose end it must
// not read past.
//
// Each kernel is written so it can only ever touch bytes inside the slice: the vector loop runs
// while `i + W <= len`, so every load is a whole vector that fits, and the remainder — 0..W-1
// bytes — is finished by the SAME scalar code the kernel replaces. Nothing is read past the end,
// no masked load is needed, and the empty slice takes the scalar tail zero times.
// ────────────────────────────────────────────────────────────────────────────────────────────

/// ⭐**THE KILL SWITCH FOR EVERY VECTOR / WORD-PARALLEL SCAN IN THIS FILE AND THE `BitSet` WORD
/// WALKS IN `buffers.rs`** (`RAVA_NO_SIMD_JSON=1`). Shared rather than duplicated so ONE
/// same-binary boot A/B prices the whole block, and folded under `RAVA_NO_EMIT_MORE` like every
/// other gate in this crate.
///
/// WAS two acquire loads (own `OnceLock` + `emit_more_disabled()`); IS one bit of `fmt_gates()`.
/// The signature is unchanged, so `buffers.rs` — which is not this round's file — is untouched and
/// simply gets the cheaper read.
pub(crate) fn simd_scan_on() -> bool {
    fmt_gates() & FG_SIMD != 0
}

/// AVX2 is NOT baseline on x86_64 (SSE2 is), so the 32-lane kernels are reached only through this.
/// Cached: `is_x86_feature_detected!` is a `cpuid` behind a lazy init, and a scan that runs per
/// format string must not pay it per call.
///
/// ⭐No longer read on the scan path at all — its answer is `FG_AVX2` in the word the scan already
/// holds. It survives for two callers: the word's own initialiser, and the `FG_LEGACY` arm, which
/// exists to restore the historic per-scan read for measurement.
#[cfg(target_arch = "x86_64")]
fn have_avx2() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::is_x86_feature_detected!("avx2"))
}

/// Does this scan take the 32-lane kernel? One bit test of the word the caller already loaded —
/// or, under the kill switch, the historic acquire load at the scan site.
#[cfg(target_arch = "x86_64")]
#[inline]
fn avx2(g: u8) -> bool {
    if g & FG_LEGACY != 0 {
        return have_avx2();
    }
    g & FG_AVX2 != 0
}

/// 32 bytes per compare. Returns the index of the first `%`, or `h.len()`.
///
/// SAFETY: the vector loop only issues a 32-byte load when `i + 32 <= h.len()`, so it stays inside
/// the allocation; the trailing `<32` bytes go through the identical scalar test.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn find_pct_avx2(h: &[u8]) -> usize {
    use core::arch::x86_64::*;
    let (n, p) = (h.len(), h.as_ptr());
    let needle = _mm256_set1_epi8(b'%' as i8);
    let mut i = 0usize;
    while i + 32 <= n {
        let v = _mm256_loadu_si256(p.add(i) as *const __m256i);
        // One bit per lane; `%` is 0x25, which cannot occur inside a multi-byte UTF-8 sequence
        // (every byte of one is >= 0x80), so a byte-wise hit is always a real, char-boundary `%`.
        let m = _mm256_movemask_epi8(_mm256_cmpeq_epi8(v, needle)) as u32;
        if m != 0 {
            return i + m.trailing_zeros() as usize;
        }
        i += 32;
    }
    while i < n && *p.add(i) != b'%' {
        i += 1;
    }
    i
}

/// The SSE2 half of the same scan. SSE2 is unconditionally present on x86_64, so this needs no
/// `#[target_feature]` and no runtime check — it is the floor, not a fast path.
#[cfg(target_arch = "x86_64")]
unsafe fn find_pct_sse2(h: &[u8]) -> usize {
    use core::arch::x86_64::*;
    let (n, p) = (h.len(), h.as_ptr());
    let needle = _mm_set1_epi8(b'%' as i8);
    let mut i = 0usize;
    while i + 16 <= n {
        let v = _mm_loadu_si128(p.add(i) as *const __m128i);
        let m = _mm_movemask_epi8(_mm_cmpeq_epi8(v, needle)) as u32;
        if m != 0 {
            return i + m.trailing_zeros() as usize;
        }
        i += 16;
    }
    while i < n && *p.add(i) != b'%' {
        i += 1;
    }
    i
}

/// Index of the first `%` in `h`, or `h.len()` — bit-identical to
/// `h.iter().position(|&c| c == b'%').unwrap_or(h.len())`, which is what a cleared `FG_SIMD` runs.
///
/// ⚠HONEST COST NOTE: most of MC's format strings are `"%s"` or `"<%1$s> %2$s"`, i.e. shorter than
/// one vector, and for those the kernel does nothing but the scalar tail — the win is confined to
/// the long literal runs (log lines, `--help` text, translation templates with prose between the
/// specifiers). Removing this path costs those, and nothing else.
///
/// ⭐**TAKES THE GATE WORD, NOT A `bool`.** This is called ONCE PER LITERAL RUN of a format string,
/// i.e. inside `format_java`'s loop, and it used to answer "AVX2?" with an acquire-ordered
/// `OnceLock` load — a barrier LLVM may not hoist out of that loop — for a fact that is a process
/// constant. `g` is the word the caller loaded once; both questions are bit tests on it.
fn scan_to_pct(h: &[u8], g: u8) -> usize {
    #[cfg(target_arch = "x86_64")]
    if g & FG_SIMD != 0 {
        // SAFETY: see the kernels — every load is bounded by `i + W <= h.len()`, and `find_pct_avx2`
        // is only reached when `cpuid` reported AVX2.
        return unsafe {
            if avx2(g) { find_pct_avx2(h) } else { find_pct_sse2(h) }
        };
    }
    #[cfg(not(target_arch = "x86_64"))]
    let _ = g;
    h.iter().position(|&c| c == b'%').unwrap_or(h.len())
}

/// Number of `char`s in valid UTF-8 `b`, counted 32 lanes at a time.
///
/// **WHY IT IS BIT-IDENTICAL:** in valid UTF-8 every character occupies exactly one LEAD byte
/// followed by 0-3 CONTINUATION bytes, and the continuation bytes are precisely `0x80..=0xBF`.
/// So `chars().count()` == the number of bytes that are NOT in that range. Read as SIGNED, that
/// range is `-128..=-65`, so "not a continuation byte" is the single compare `x > -65`, which is
/// what `_mm256_cmpgt_epi8` against `set1(-65)` computes per lane. The population count of the
/// lane mask is the block's contribution; block boundaries are safe because the test is per byte
/// and does not look at its neighbours.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn char_count_avx2(b: &[u8]) -> usize {
    use core::arch::x86_64::*;
    let (n, p) = (b.len(), b.as_ptr());
    let lim = _mm256_set1_epi8(-65i8);
    let (mut i, mut cnt) = (0usize, 0usize);
    while i + 32 <= n {
        let v = _mm256_loadu_si256(p.add(i) as *const __m256i);
        let m = _mm256_movemask_epi8(_mm256_cmpgt_epi8(v, lim)) as u32;
        cnt += m.count_ones() as usize;
        i += 32;
    }
    while i < n {
        cnt += ((*p.add(i) as i8) > -65) as usize;
        i += 1;
    }
    cnt
}

/// SSE2 floor of the same count, 16 lanes at a time. Same predicate, same arithmetic.
#[cfg(target_arch = "x86_64")]
unsafe fn char_count_sse2(b: &[u8]) -> usize {
    use core::arch::x86_64::*;
    let (n, p) = (b.len(), b.as_ptr());
    let lim = _mm_set1_epi8(-65i8);
    let (mut i, mut cnt) = (0usize, 0usize);
    while i + 16 <= n {
        let v = _mm_loadu_si128(p.add(i) as *const __m128i);
        let m = _mm_movemask_epi8(_mm_cmpgt_epi8(v, lim)) as u32;
        cnt += m.count_ones() as usize;
        i += 16;
    }
    while i < n {
        cnt += ((*p.add(i) as i8) > -65) as usize;
        i += 1;
    }
    cnt
}

/// `s.chars().count()`, vectorised. Only ever reached for a piece that a WIDTH must be measured
/// against — the no-width case still short-circuits to 0 above and never walks the string at all.
fn char_count(s: &str, g: u8) -> usize {
    #[cfg(target_arch = "x86_64")]
    if g & FG_SIMD != 0 {
        // SAFETY: bounded loads as in `find_pct_*`; `s` is a `&str`, hence valid UTF-8, which is
        // the precondition the continuation-byte identity rests on.
        return unsafe {
            if avx2(g) { char_count_avx2(s.as_bytes()) } else { char_count_sse2(s.as_bytes()) }
        };
    }
    #[cfg(not(target_arch = "x86_64"))]
    let _ = g;
    s.chars().count()
}

/// Byte index of character number `p` (0-based) in valid UTF-8 `b`, or `None` when `b` holds `p`
/// characters or fewer — exactly `str::char_indices().nth(p).map(|(i, _)| i)`.
///
/// Same lane predicate as `char_count_avx2`. The block whose running total first EXCEEDS `p` is
/// the block containing the answer; inside it, clearing the `p - seen` lowest set bits of the mask
/// (`m &= m - 1`) leaves the wanted char-start as the lowest remaining bit, and `seen + c > p`
/// guarantees at least one bit survives. Whole blocks below that are skipped without ever decoding
/// a character, which is the part the scalar `nth` cannot do.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn nth_char_avx2(b: &[u8], p: usize) -> Option<usize> {
    use core::arch::x86_64::*;
    let (n, ptr) = (b.len(), b.as_ptr());
    let lim = _mm256_set1_epi8(-65i8);
    let (mut i, mut seen) = (0usize, 0usize); // `seen` = char starts strictly before byte `i`
    while i + 32 <= n {
        let v = _mm256_loadu_si256(ptr.add(i) as *const __m256i);
        let mut m = _mm256_movemask_epi8(_mm256_cmpgt_epi8(v, lim)) as u32;
        let c = m.count_ones() as usize;
        if seen + c > p {
            for _ in 0..(p - seen) {
                m &= m - 1;
            }
            return Some(i + m.trailing_zeros() as usize);
        }
        seen += c;
        i += 32;
    }
    while i < n {
        if (*ptr.add(i) as i8) > -65 {
            if seen == p {
                return Some(i);
            }
            seen += 1;
        }
        i += 1;
    }
    None
}

/// `s.char_indices().nth(p).map(|(i, _)| i)`, vectorised. Serves `%.Ns`, whose precision cut must
/// land on a character boundary.
fn nth_char_index(s: &str, p: usize, g: u8) -> Option<usize> {
    #[cfg(target_arch = "x86_64")]
    if g & FG_SIMD != 0 && avx2(g) {
        // SAFETY: bounded loads; `s` is valid UTF-8 by construction.
        return unsafe { nth_char_avx2(s.as_bytes(), p) };
    }
    #[cfg(not(target_arch = "x86_64"))]
    let _ = g;
    s.char_indices().nth(p).map(|(i, _)| i)
}

/// Append `n` copies of `fill` — width padding, where `n` is KNOWN before the first byte goes out.
/// `String::extend(repeat(c))` appended ONE `char` at a time, each with its own capacity check and
/// UTF-8 encode, so a `%64s` did 64 grow-checks to emit 64 bytes: **n checked pushes → one
/// `reserve` plus ceil(n/64) `push_str`s.**
///
/// Table size: two 64-byte `&'static str`s = **128 bytes of rodata**, shared by every call — nothing
/// allocated per invocation, nothing that can push a hot table out of L1.
fn push_fill(out: &mut String, fill: char, n: usize) {
    const SPACES: &str = "                                                                ";
    const ZEROS: &str = "0000000000000000000000000000000000000000000000000000000000000000";
    let src = if fill == '0' { ZEROS } else { SPACES };
    out.reserve(n);
    let mut left = n;
    while left > 0 {
        let take = left.min(src.len());
        out.push_str(&src[..take]);
        left -= take;
    }
}

/// `%.Nf` with Java's rounding, which is HALF_UP over the value's SHORTEST decimal
/// representation — not Rust's `{:.*}`, which rounds the exact binary value half-to-
/// even. The two disagree on the everyday cases: `%.1f` of 0.25 is "0.3" in Java and
/// "0.2" in Rust; 9.95 is "10.0" vs "9.9". Rust's `Display` for f64 emits the same
/// shortest round-trip decimal Java's `Double.toString` does and never uses exponent
/// notation, so rounding that STRING half-up reproduces Java.
pub(crate) fn fixed_half_up(v: f64, prec: usize) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v < 0.0 { "-Infinity".into() } else { "Infinity".into() };
    }
    let neg = v.is_sign_negative();
    // `java_plain_decimal`, not `{}`: the digits must be the ones the JDK would
    // print, or the half-up decision below is made on a different last digit.
    let s = crate::java_plain_decimal(v.abs());
    // BORROWED, not `to_string()`d: both halves are only ever read, and copying them cost
    // two allocations per `%f` on top of the digit vector below.
    let (int_part, frac_part) = match s.split_once('.') {
        Some((a, b)) => (a, b),
        None => (s.as_str(), ""),
    };
    let int_len = int_part.len();
    let keep = int_len + prec;
    // ⭐**THE WHOLE PLAIN RENDERING WAS MATERIALISED TO KEEP `keep` DIGITS OF IT.** HALF_UP
    // consults exactly ONE dropped digit (`digits[keep]`); every digit past it is dead, and
    // `java_plain_decimal` never uses exponent notation — so `%.6f` of `1e-300` collected a
    // 307-byte vector in order to read 8 bytes of it. **O(D) → O(keep)** bytes allocated and
    // copied, where D is the digit count of the plain rendering (~324 for a subnormal, ~309 for
    // `1e308`). `Take`'s `size_hint` sizes the allocation exactly too, so the vector no longer
    // grows by doubling on the way in.
    let limit = if fast_fmt() { keep.saturating_add(1) } else { usize::MAX };
    let mut digits: Vec<u8> = int_part.bytes().chain(frac_part.bytes()).take(limit).collect();
    if digits.len() > keep {
        let round_up = digits[keep] >= b'5';
        digits.truncate(keep);
        if round_up {
            let mut k = keep;
            loop {
                if k == 0 {
                    digits.insert(0, b'1'); // carry past the leading digit: 9.95 → 10.0
                    break;
                }
                k -= 1;
                if digits[k] == b'9' {
                    digits[k] = b'0';
                } else {
                    digits[k] += 1;
                    break;
                }
            }
        }
    } else {
        digits.resize(keep, b'0'); // pad the fraction out to `prec`
    }
    // A carry may have grown the integer part by one digit.
    let int_len = digits.len() - prec;
    let mut out = String::with_capacity(digits.len() + 2);
    if neg {
        out.push('-');
    }
    out.push_str(std::str::from_utf8(&digits[..int_len]).unwrap_or("0"));
    if prec > 0 {
        out.push('.');
        out.push_str(std::str::from_utf8(&digits[int_len..]).unwrap_or(""));
    }
    out
}

/// ⭐**THE SHARED HALF_UP CORE OF `%e` AND `%g`**, split out of `sci_half_up` because `%g` used to
/// reach it by BUILDING A STRING AND PARSING IT BACK. Returns `mag`'s shortest plain decimal
/// rounded HALF_UP to exactly `keep` SIGNIFICANT digits, plus the decimal exponent — the power of
/// ten the first returned digit carries, already bumped if the rounding carried out of the top.
///
/// ⭐**TWO COMPLEXITIES LIVED HERE, BOTH IN D = THE DIGIT COUNT OF THE PLAIN RENDERING** (which
/// `java_plain_decimal` never abbreviates: ~324 digits for a subnormal, ~309 for `1e308`).
///   * The leading-zero strip was `digits.remove(0)` inside a loop, so every dropped zero memmoved
///     the whole remaining vector: `%e` of `1e-300` moved ~45 000 bytes to delete 300.
///     **O(D²) → O(D)** — count the zero run once, skip it once, never shift.
///   * The vector then held all D digits when HALF_UP reads exactly ONE past the cut.
///     **O(D) → O(keep)** allocated and copied.
///
/// Same digits, same carry, same all-zero exponent reset as the loop it replaces: dropping a
/// leading zero and skipping it are the same edit, and truncating after `keep + 1` cannot change a
/// HALF_UP decision that only ever looked at `digits[keep]`.
fn round_significant(mag: f64, keep: usize) -> (Vec<u8>, i32) {
    let plain = crate::java_plain_decimal(mag);
    let (ip, fp) = plain.split_once('.').unwrap_or((plain.as_str(), ""));
    let mut exp = ip.len() as i32 - 1;
    let mut digits: Vec<u8>;
    if fast_fmt() {
        // `min(total - 1)` keeps the last digit when EVERY digit is zero, which is what the old
        // `digits.len() > 1` loop guard did; `plain` is never empty, so `total >= 1`.
        let total = ip.len() + fp.len();
        let lead = ip
            .bytes()
            .chain(fp.bytes())
            .take_while(|&d| d == b'0')
            .count()
            .min(total - 1);
        exp -= lead as i32;
        digits = ip
            .bytes()
            .chain(fp.bytes())
            .skip(lead)
            .take(keep.saturating_add(1))
            .collect();
    } else {
        digits = ip.bytes().chain(fp.bytes()).collect();
        while digits.len() > 1 && digits[0] == b'0' {
            digits.remove(0);
            exp -= 1;
        }
    }
    // After the strip the first digit can only still be '0' if every digit was — i.e. `mag == 0.0`.
    // The old `digits == [b'0']` compare said the same thing by materialising the whole answer.
    if digits.first() == Some(&b'0') {
        exp = 0;
    }
    if digits.len() > keep {
        let round_up = digits[keep] >= b'5';
        digits.truncate(keep);
        if round_up {
            let mut k = keep;
            loop {
                if k == 0 {
                    // 9.99 -> 10.0 at this precision: shift back to one leading digit.
                    digits.insert(0, b'1');
                    digits.pop();
                    exp += 1;
                    break;
                }
                k -= 1;
                if digits[k] == b'9' { digits[k] = b'0'; } else { digits[k] += 1; break; }
            }
        }
    } else {
        digits.resize(keep, b'0');
    }
    (digits, exp)
}

/// Lay `digits` (exactly `prec + 1` of them) out as Java's "d.<prec digits>e±dd". Java always signs
/// the exponent and pads it to at least two digits.
fn render_sci(neg: bool, digits: &[u8], exp: i32, prec: usize) -> String {
    let mut out = String::with_capacity(prec + 7);
    if neg { out.push('-'); }
    out.push(digits[0] as char);
    if prec > 0 {
        out.push('.');
        out.push_str(std::str::from_utf8(&digits[1..]).unwrap_or(""));
    }
    out.push('e');
    out.push(if exp < 0 { '-' } else { '+' });
    // `format!("{:02}", ..)` allocated a `String` to render two digits. Same output:
    // `{:02}` zero-pads to two and prints wider values in full, which is what the
    // three-or-more-digit arm does (only |exp| >= 100 reaches it — 1e-300 and friends).
    let e = exp.unsigned_abs();
    if e < 100 {
        out.push((b'0' + (e / 10) as u8) as char);
        out.push((b'0' + (e % 10) as u8) as char);
    } else {
        out.push_str(&e.to_string());
    }
    out
}

/// `%e` — the same HALF_UP-over-the-shortest-decimal rounding as `fixed_half_up`,
/// laid out as "d.<prec digits>e±dd".
pub(crate) fn sci_half_up(v: f64, prec: usize) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v < 0.0 { "-Infinity".into() } else { "Infinity".into() };
    }
    let (digits, exp) = round_significant(v.abs(), prec + 1);
    render_sci(v.is_sign_negative(), &digits, exp, prec)
}

/// `%g` — Java picks decimal layout when the ROUNDED magnitude lands in
/// [10^-4, 10^precision) and scientific otherwise, where `precision` counts
/// SIGNIFICANT digits rather than fraction digits. Unlike C's %g it never strips
/// the trailing zeros.
pub(crate) fn general_half_up(v: f64, prec: usize) -> String {
    if v.is_nan() || v.is_infinite() {
        return sci_half_up(v, prec.saturating_sub(1));
    }
    let p = prec.max(1);
    if !fast_fmt() {
        let sci = sci_half_up(v, p - 1);
        let exp: i32 = sci.rsplit_once('e').and_then(|(_, e)| e.parse().ok()).unwrap_or(0);
        return if (-4..p as i32).contains(&exp) {
            fixed_half_up(v, (p as i32 - 1 - exp).max(0) as usize)
        } else {
            sci
        };
    }
    // ⭐**A STRING BUILT ONLY TO BE PARSED BACK.** The layout decision needs one number — the
    // ROUNDED exponent — and this used to obtain it by rendering the whole scientific form
    // (allocating it, formatting the mantissa, zero-padding the exponent) and then running
    // `rsplit_once('e')` + `i32::parse` over its own output. In the decimal branch that entire
    // string was then THROWN AWAY and the digit pipeline — `java_plain_decimal`, digit vector,
    // rounding — was run a second time by `fixed_half_up`. Now the exponent comes straight out of
    // the rounding that computed it: **one String + one parse removed from every `%g`, and the
    // scientific branch renders the digits it already has instead of re-deriving them.**
    let (digits, exp) = round_significant(v.abs(), p);
    if (-4..p as i32).contains(&exp) {
        fixed_half_up(v, (p as i32 - 1 - exp).max(0) as usize)
    } else {
        render_sci(v.is_sign_negative(), &digits, exp, p - 1)
    }
}

/// Minimal `java.util.Formatter` subset: handles %s %d %x %X %o %f %e %g %b %c %n %%
/// with optional flags/width/precision (e.g. `%-20s`, `%03d`, `%.2f`).
///
/// `Err` is a guest throw: a conversion with NO argument left is
/// `MissingFormatArgumentException` on the JVM, where this used to substitute a
/// zero (or an empty string) and print a plausible line — `String.format("%d %d", 1)`
/// read "1 0", which is a wrong ANSWER rather than a missing feature.
///
/// ⚠ The sibling `IllegalFormatConversionException` (a `%d` handed a String) is
/// deliberately NOT raised here: every caller runs `stringify` over the varargs
/// first, so by this point an object of ANY class arrives as a `Value::Str` and
/// the exception's message — which names the argument's class — could only be
/// invented. Raising it with the wrong class name would trade a wrong number for
/// a wrong message. Fixing it properly means passing the raw values through.
pub(crate) fn format_java<'a>(
    fmt: &str,
    args: &'a [Value],
) -> Result<String, (&'static str, String)> {
    // ⭐**BYTES, AND LITERAL RUNS COPIED WHOLE.** The scan used to build a `Vec<char>` the
    // length of the format string — four bytes per character, one allocation per
    // `String.format` call — and then push the literal text back out ONE `char` at a time,
    // even though literal text is most of a format string. Every part of a specifier
    // (index, flags, width, precision, conversion letter) is ASCII, and UTF-8 is
    // self-synchronising: 0x25 `%` can never occur inside a multi-byte sequence. So the
    // run up to the next `%` is always a valid `str` boundary and goes out with one
    // `push_str`. The only byte that can be non-ASCII is an UNKNOWN conversion letter,
    // which the catch-all arm decodes from the original `&str`.
    let b = fmt.as_bytes();
    // ⭐**FOUR PROCESS CONSTANTS, ONE ACQUIRE LOAD, AT THE NATURAL ENTRY.** WAS: `fast_fmt()` (2
    // loads — its own `OnceLock` plus `emit_more_disabled()`'s), `simd_scan_on()` (2 more), and
    // `have_avx2()` re-read INSIDE the loop below, once per literal run and once per
    // width/precision measurement. IS: this one word, threaded down to the scan helpers as a plain
    // `u8` they bit-test. Population: every `String.format`/`printf`/`Formatter` call in the
    // engine, and every `%`-delimited run inside one.
    let g = fmt_gates();
    let fast = g & FG_FASTFMT != 0;
    let mut out = String::with_capacity(fmt.len() + 16);
    let mut i = 0;
    let mut ai = 0;
    while i < b.len() {
        if b[i] != b'%' {
            let start = i;
            i += 1;
            // ⭐**THE LITERAL RUN IS A `memchr`, ONE BYTE AT A TIME.** Everything between two
            // specifiers is copied out whole (see the note above), so the only work in the run was
            // finding where it ENDS — a per-byte compare against a single constant, which is the
            // textbook 32-lane `cmpeq` + `movemask` + `tzcnt`. Identical index: the scan looks for
            // the same byte 0x25 and stops at the same first occurrence, and `b.len()` when there
            // is none, which is what the `while` condition produced.
            i += scan_to_pct(&b[i..], g);
            out.push_str(&fmt[start..i]);
            continue;
        }
        i += 1; // past '%'
        // Optional argument index: `digits$` (Java positional, e.g. %1$s — MC's
        // chat format is "<%1$s> %2$s"). If the digits aren't followed by '$' they
        // are the width, so rewind. A positional index selects the arg for THIS
        // conversion (1-based); subsequent positionals reset it again.
        let mut explicit_idx: Option<usize> = None;
        {
            let save = i;
            let mut n = 0usize;
            let mut any = false;
            while i < b.len() && b[i].is_ascii_digit() {
                n = n * 10 + (b[i] - b'0') as usize;
                any = true;
                i += 1;
            }
            if any && i < b.len() && b[i] == b'$' {
                i += 1;
                explicit_idx = Some(n.saturating_sub(1));
            } else {
                i = save;
            }
        }
        // flags
        let mut left = false;
        let mut zero = false;
        let mut group = false;
        let mut plus = false;
        // `(` renders a negative number in parentheses and ` ` puts a space before a
        // positive one. `(` was not in the flag set at all, so `%(d` fell out of the
        // conversion switch, echoed the literal "%(" and left the argument cursor
        // one behind — every conversion AFTER it in the same format string printed
        // the wrong argument.
        let mut paren = false;
        let mut space = false;
        while i < b.len() && matches!(b[i], b'-' | b'+' | b' ' | b'0' | b'#' | b',' | b'(') {
            match b[i] {
                b'-' => left = true,
                b'0' => zero = true,
                b',' => group = true,
                b'+' => plus = true,
                b'(' => paren = true,
                b' ' => space = true,
                _ => {}
            }
            i += 1;
        }
        // width
        let mut width = 0usize;
        while i < b.len() && b[i].is_ascii_digit() {
            width = width * 10 + (b[i] - b'0') as usize;
            i += 1;
        }
        // precision
        let mut prec: Option<usize> = None;
        if i < b.len() && b[i] == b'.' {
            i += 1;
            let mut p = 0usize;
            while i < b.len() && b[i].is_ascii_digit() {
                p = p * 10 + (b[i] - b'0') as usize;
                i += 1;
            }
            prec = Some(p);
        }
        if i >= b.len() {
            break;
        }
        let conv = b[i];
        i += 1;
        // AN EXPLICIT INDEX DOES NOT MOVE THE RELATIVE CURSOR. The JDK keeps the two
        // apart (`Formatter` tracks `lastOrdinaryIndex` separately from a
        // specifier's own index), so `%1$s %s` feeds the SECOND conversion from the
        // ordinary sequence — it does not continue after the positional. Assigning
        // `ai = idx` clobbered that: every ordinary conversion following a
        // positional one printed the wrong argument, and with the new
        // missing-argument check it could throw on a legal format string.
        //
        // `cursor` is where THIS conversion reads; `ai` advances only for ordinary
        // ones.
        let positional = explicit_idx.is_some();
        let cursor = explicit_idx.unwrap_or(ai);
        // ⭐**AN `Arc` REFCOUNT PAIR FOR A VALUE THAT IS ONLY READ.** `Value::Str` is an
        // `Arc<String>`, and every caller of this formatter stringifies its varargs FIRST (see the
        // note above), so essentially every argument arrives as one — meaning `.cloned()` bought
        // an atomic increment on the way in and an atomic decrement on the drop at the end of the
        // arm, once per conversion, for a value no arm ever mutates or keeps. The arms take a
        // borrow now; the cursor rule is untouched.
        let next = |ai: &mut usize| -> Option<&'a Value> {
            let v = args.get(cursor);
            if !positional {
                *ai += 1;
            }
            v
        };
        // `%%` and `%n` consume nothing, and an UNKNOWN conversion is not ours to
        // judge — it falls through to the catch-all below, which echoes it. Only a
        // conversion this formatter actually serves can be missing its argument, and
        // running out is the JDK's MissingFormatArgumentException, named after the
        // specifier so a guest can find it in a long format string.
        const CONSUMING: &[u8] = b"sSdxXobBcCfeEgGhH";
        if CONSUMING.contains(&conv) && cursor >= args.len() {
            return Err((
                "java/util/MissingFormatArgumentException",
                format!("Format specifier '%{}'", conv as char),
            ));
        }
        // ⭐**THE ARGUMENT COPIED TWICE TO BE APPENDED ONCE.** `%s` is the boot's dominant
        // conversion and its argument is already a `Value::Str` (every caller stringifies the
        // varargs first). `display()` handed back a fresh `String` COPY of it, which was then
        // copied AGAIN into `out` and dropped. With no width there is nothing to measure and with
        // no precision nothing to cut, so the borrowed bytes go straight out: **two copies plus an
        // allocation per `%s` → one copy, in the argument's length.** Anything else — `%S`, a
        // width, a precision, a non-`Str` argument — falls through to the general arm untouched,
        // and the cursor advances by exactly the rule `next` applies.
        if fast && conv == b's' && width == 0 && prec.is_none() {
            if let Some(Value::Str(sv)) = args.get(cursor) {
                if !positional {
                    ai += 1;
                }
                out.push_str(sv.as_str());
                continue;
            }
        }
        let piece = match conv {
            b'%' => "%".to_string(),
            b'n' => "\n".to_string(),
            b's' | b'S' => {
                let mut s = next(&mut ai).map(|v| v.display()).unwrap_or_default();
                // `%.Ns` truncates to N chars (Java: "the maximum number of
                // characters"). Ignoring it printed the whole string.
                if let Some(p) = prec {
                    if fast {
                        // ⭐**A FULL PASS OVER THE ARGUMENT TO ASK ABOUT ITS p-TH CHARACTER, THEN A
                        // COPY OF THE PART IT KEPT.** `chars().count() > p` decoded every character
                        // of a string whose length is the ARGUMENT's — a stringified collection or
                        // a log line, unbounded — and `chars().take(p).collect()` then built a
                        // second `String` for the prefix. The p-th char boundary answers both
                        // questions: `None` means there is nothing to cut, `Some` is the byte index
                        // to cut at. **O(L) decode + one allocation → O(p) decode, in place.**
                        // …and the boundary itself is now found by counting CHAR STARTS 32 at a
                        // time rather than decoding characters: `nth_char_index` skips whole
                        // 32-byte blocks whose char-start population still leaves it short of `p`,
                        // and only picks a byte apart inside the block that crosses it. Same index
                        // — see its identity note — so the same cut, with the same `None` meaning
                        // "shorter than `p + 1` chars, nothing to truncate".
                        if let Some(idx) = nth_char_index(&s, p, g) {
                            s.truncate(idx);
                        }
                    } else if s.chars().count() > p {
                        s = s.chars().take(p).collect();
                    }
                }
                if conv == b'S' { s.to_uppercase() } else { s }
            }
            b'd' => {
                let n = next(&mut ai).and_then(|v| v.as_long()).unwrap_or(0);
                let mut s = n.unsigned_abs().to_string();
                // The ',' flag was PARSED and then dropped, so `%,d` printed ungrouped.
                if group { s = group_digits(&s); }
                if n < 0 {
                    if paren { format!("({s})") } else { format!("-{s}") }
                } else if plus { format!("+{s}") }
                else if space { format!(" {s}") }
                else { s }
            }
            // Java's %x/%o format the ARGUMENT's width: an Integer masks to 32
            // bits (Java: ffffffd7 for -41), only a Long spans 64. Formatting the
            // sign-extended i64 printed 16 f's for any negative int. (Byte/Short
            // widths are unrecoverable — rava keeps primitives unboxed.)
            b'x' | b'X' | b'o' => {
                let v = next(&mut ai);
                let (n, is_long) = match v {
                    Some(Value::Long(l)) => (*l as u64, true),
                    Some(x) => (x.as_long().unwrap_or(0) as u64, false),
                    None => (0, false),
                };
                let n = if is_long { n } else { n & 0xFFFF_FFFF };
                match conv {
                    b'x' => format!("{n:x}"),
                    b'X' => format!("{n:X}"),
                    _ => format!("{n:o}"),
                }
            }
            b'b' => {
                // Spelled out rather than `matches!(&v, Some(x) if ..)`: `v` is now a borrow, and a
                // guard over a doubly-referenced binding reads as a puzzle for no gain. Same answer
                // — a missing argument is "false", a non-numeric non-null is "true".
                let v = next(&mut ai);
                let truthy = match v {
                    Some(x) => x.as_int().map(|n| n != 0).unwrap_or(!matches!(x, Value::Null)),
                    None => false,
                };
                truthy.to_string()
            }
            // ⚠**`%c`'S ARGUMENT MAY ARRIVE ALREADY RENDERED.** `format_java` is handed values
            // that `natives::stringify` has flattened so `%s` can reach a guest `toString`, and
            // a boxed `Character` (`crate::boxid`) flattens to the ONE-CHARACTER string that is
            // its own `toString` — where `as_int` reads `None` and this printed nothing.
            // Reading the character back out of a single-char string is exact for that producer
            // and changes nothing else: Java's `%c` on a real `String` argument is an
            // `IllegalFormatConversionException`, never text, so no correct program can tell.
            // A boxed `Short`/`Byte` does NOT come through here as text — `stringify` unboxes
            // those to `Value::Int`, so `%c` still gets their code point.
            b'c' => match next(&mut ai) {
                Some(Value::Str(s)) if s.as_str().chars().count() == 1 => s.as_str().to_string(),
                other => other
                    .and_then(|v| v.as_int())
                    .and_then(|c| char::from_u32(c as u32))
                    .map(String::from)
                    .unwrap_or_default(),
            },
            b'f' => {
                let v = match next(&mut ai) {
                    Some(Value::Double(d)) => *d,
                    Some(Value::Float(f)) => *f as f64,
                    Some(other) => other.as_long().unwrap_or(0) as f64,
                    None => 0.0,
                };
                let mut s = fixed_half_up(v, prec.unwrap_or(6));
                if group {
                    let (ip, fp) = match s.split_once('.') { Some((a, b)) => (a, Some(b)), None => (s.as_str(), None) };
                    let (sign, digits) = match ip.strip_prefix('-') { Some(d) => ("-", d), None => ("", ip) };
                    s = match fp {
                        Some(f) => format!("{sign}{}.{f}", group_digits(digits)),
                        None => format!("{sign}{}", group_digits(digits)),
                    };
                }
                match s.strip_prefix('-') {
                    Some(mag) if paren => format!("({mag})"),
                    None if plus => format!("+{s}"),
                    None if space => format!(" {s}"),
                    _ => s,
                }
            }
            // %e/%g fell through to the catch-all below, which echoed the literal
            // "%e" into the output.
            b'e' | b'E' | b'g' | b'G' => {
                let v = match next(&mut ai) {
                    Some(Value::Double(d)) => *d,
                    Some(Value::Float(f)) => *f as f64,
                    Some(other) => other.as_long().unwrap_or(0) as f64,
                    None => 0.0,
                };
                let s = if conv == b'e' || conv == b'E' {
                    sci_half_up(v, prec.unwrap_or(6))
                } else {
                    general_half_up(v, prec.unwrap_or(6))
                };
                let s = if plus && !s.starts_with('-') { format!("+{s}") } else { s };
                if conv.is_ascii_uppercase() { s.to_uppercase() } else { s }
            }
            _ => {
                // An unknown conversion is echoed verbatim — and the letter may be the
                // lead byte of a multi-byte character, the one place in this scan where a
                // raw byte would be wrong. `i - 1` is a char boundary (everything consumed
                // between the `%` and here is ASCII), so decode from the original `&str`
                // and step over the WHOLE character.
                let c = fmt[i - 1..].chars().next().unwrap_or('%');
                i = i - 1 + c.len_utf8();
                out.push('%');
                out.push(c);
                continue;
            }
        };
        // width padding
        // ⭐**A FULL UTF-8 DECODE OF THE PIECE TO ANSWER A QUESTION WITH NO WIDTH IN IT.** `width`
        // is 0 for the overwhelming majority of conversions — MC's own formats are `"%s"` and
        // `"<%1$s> %2$s"` — and `0 > plen` is false for every possible `plen`, so the count could
        // never change the outcome. It still walked the whole rendered argument. **O(L) in the
        // argument's length per conversion → O(1) whenever no width was written.**
        // …and when a width IS written the measurement is a UTF-8 character count, which is a
        // per-byte predicate ("not a continuation byte") and therefore 32 lanes at a time. The
        // count is exact, not an ASCII approximation: see `char_count_avx2`.
        let plen = if width == 0 && fast { 0 } else { char_count(&piece, g) };
        if width > plen {
            let pad = width - plen;
            let fill = if zero && !left { '0' } else { ' ' };
            if left {
                out.push_str(&piece);
                push_fill(&mut out, ' ', pad);
            } else if fill == '0' {
                // Zero padding goes INSIDE the sign: `%05d` of -42 is "-0042", not
                // "00-42", which is what padding the whole piece produced.
                // Zero padding goes INSIDE the sign — and inside the PARENTHESES
                // for the `(` form, which is a sign too: the JDK renders
                // `%(05d` of -42 as `(0042)`, not `00(42)`.
                let (open, rest, close) = match piece.strip_prefix('(') {
                    Some(r) => ("(", r.strip_suffix(')').unwrap_or(r), ")"),
                    None => match piece.strip_prefix(['-', '+', ' ']) {
                        Some(rest) => (&piece[..1], rest, ""),
                        None => ("", piece.as_str(), ""),
                    },
                };
                out.push_str(open);
                push_fill(&mut out, '0', pad);
                out.push_str(rest);
                out.push_str(close);
            } else {
                push_fill(&mut out, fill, pad);
                out.push_str(&piece);
            }
        } else {
            out.push_str(&piece);
        }
    }
    Ok(out)
}
