//! Array and string routines optimized code calls directly in place of the JDK methods marked
//! `@IntrinsicCandidate` for them (HotSpot's string and array intrinsics): comparing, searching
//! and hashing whole arrays a word or a vector at a time instead of an element per iteration.
//! The interpreter and baseline code run the methods' bytecode (for a fill, the VM's body),
//! which computes the same results. Each routine is a leaf: it reads arrays (or fills a
//! primitive one), allocates nothing and cannot stop, so the collector never runs while it
//! holds a reference. Ranges come from the JDK callers, which check them; reads are still
//! clamped to the array so a bad one cannot leave it.

use crate::heap::{obj, Oop};


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Leaf {
    /// `StringLatin1.equals(byte[], byte[])`.
    Latin1Equals,
    /// `ArraysSupport.vectorizedMismatch(Object, long, Object, long, int, int)`.
    Mismatch,
    /// `ArraysSupport.vectorizedHashCode(Object, int, int, int, int)`.
    HashCode,
    /// `StringCoding.countPositives(byte[], int, int)`.
    CountPositives,
    /// `Class.isArray()`, `isPrimitive()`, `isInterface()`.
    IsArray,
    IsPrimitive,
    IsInterface,
    /// `Class.isAssignableFrom(Class)`.
    IsAssignableFrom,
    /// `Reference.refersTo0(Object)` (and `PhantomReference`'s): the referent is the object.
    RefersTo,
    /// `Arrays.fill(x[], x)` for a primitive array of elements of `1 << n` bytes (the only
    /// routine that writes: HotSpot recognizes the loop and calls its fill stub).
    Fill(u8),
}

impl Leaf {
    /// The routine's address (`Intrinsic::Leaf`).
    pub(crate) fn address(self) -> usize {
        match self {
            Leaf::Latin1Equals => latin1_equals as *const () as usize,
            Leaf::Mismatch => vectorized_mismatch as *const () as usize,
            Leaf::HashCode => vectorized_hash_code as *const () as usize,
            Leaf::CountPositives => count_positives as *const () as usize,
            Leaf::IsArray => is_array as *const () as usize,
            Leaf::IsPrimitive => is_primitive as *const () as usize,
            Leaf::IsInterface => is_interface as *const () as usize,
            Leaf::IsAssignableFrom => is_assignable_from as *const () as usize,
            Leaf::RefersTo => refers_to as *const () as usize,
            Leaf::Fill(0) => fill_leaf::<u8> as *const () as usize,
            Leaf::Fill(1) => fill_leaf::<u16> as *const () as usize,
            Leaf::Fill(2) => fill_leaf::<u32> as *const () as usize,
            Leaf::Fill(_) => fill_leaf::<u64> as *const () as usize,
        }
    }

    /// The routine standing for `holder.name desc`.
    pub(crate) fn of(holder: &str, name: &str, desc: &str) -> Option<Leaf> {
        Some(match (holder, name, desc) {
            ("java/lang/StringLatin1", "equals", "([B[B)Z") => Leaf::Latin1Equals,
            ("jdk/internal/util/ArraysSupport", "vectorizedMismatch", "(Ljava/lang/Object;JLjava/lang/Object;JII)I") => Leaf::Mismatch,
            ("jdk/internal/util/ArraysSupport", "vectorizedHashCode", "(Ljava/lang/Object;IIII)I") => Leaf::HashCode,
            ("java/lang/StringCoding", "countPositives", "([BII)I") => Leaf::CountPositives,
            ("java/lang/Class", "isArray", "()Z") => Leaf::IsArray,
            ("java/lang/Class", "isPrimitive", "()Z") => Leaf::IsPrimitive,
            ("java/lang/Class", "isInterface", "()Z") => Leaf::IsInterface,
            ("java/lang/Class", "isAssignableFrom", "(Ljava/lang/Class;)Z") => Leaf::IsAssignableFrom,
            ("java/lang/ref/Reference" | "java/lang/ref/PhantomReference", "refersTo0", "(Ljava/lang/Object;)Z") => Leaf::RefersTo,
            ("java/util/Arrays", "fill", _) => Leaf::Fill(fill_shift(desc)?),
            _ => return None,
        })
    }

    /// Writes memory (an array's elements): an effect, never removed, moved or merged.
    pub(crate) fn writes(self) -> bool {
        matches!(self, Leaf::Fill(_))
    }

    /// Which arguments must not be null (checked before the call): arrays the routine reads,
    /// and the class `isAssignableFrom` compares with. The mismatch's bases may be null, its
    /// offsets then being addresses (direct buffers); receivers are checked by the invoke.
    pub(crate) fn array_args(self) -> &'static [usize] {
        match self {
            Leaf::Latin1Equals => &[0, 1],
            Leaf::Mismatch | Leaf::IsArray | Leaf::IsPrimitive | Leaf::IsInterface | Leaf::RefersTo => &[],
            Leaf::HashCode | Leaf::CountPositives | Leaf::Fill(_) => &[0],
            Leaf::IsAssignableFrom => &[1],
        }
    }

    /// Reads memory a store or a collection can change (array contents, a referent the
    /// collector clears), so a loop cannot compute it once before it starts; the class queries
    /// read immutable metadata.
    pub(crate) fn reads_mutable(self) -> bool {
        !matches!(self, Leaf::IsArray | Leaf::IsPrimitive | Leaf::IsInterface | Leaf::IsAssignableFrom)
    }
}

/// The bytes of array `a` from element `from` (of `size` bytes) for `count` elements, clamped
/// to the array.
///
/// # Safety
/// `a` is a live, non-null primitive array.
unsafe fn bytes<'a>(a: Oop, size: usize, from: i32, count: i32) -> &'a [u8] {
    let len = obj::array_length(a).max(0) as usize;
    let from = (from.max(0) as usize).min(len);
    let count = (count.max(0) as usize).min(len - from);
    std::slice::from_raw_parts(obj::array_data::<u8>(a).add(from * size), count * size)
}

extern "sysv64" fn latin1_equals(a: Oop, b: Oop) -> i64 {
    // SAFETY: two live byte arrays (null-checked by the caller).
    unsafe {
        let n = obj::array_length(a);
        if n != obj::array_length(b) {
            return 0;
        }
        (std::ptr::eq(a, b) || same_bytes(bytes(a, 1, 0, n), bytes(b, 1, 0, n))) as i64
    }
}

/// Whether `x` and `y` (of one length) hold the same bytes, a word at a time: strings are
/// mostly short, and the C library's `memcmp` cost them a call and its dispatch on the size.
#[inline]
fn same_bytes(x: &[u8], y: &[u8]) -> bool {
    let n = x.len();
    let word = |s: &[u8], i: usize| u64::from_le_bytes(s[i..i + 8].try_into().unwrap());
    let mut i = 0;
    while i + 8 <= n {
        if word(x, i) != word(y, i) {
            return false;
        }
        i += 8;
    }
    if n >= 8 {
        // The last word again, overlapping: the tail without a byte loop.
        return word(x, n - 8) == word(y, n - 8);
    }
    x[i..] == y[i..]
}

/// The index of the first element (of `1 << log2` bytes) at which the `length` elements at
/// `a + ao` and `b + bo` differ; else `-1`, the complement of no elements left for the caller
/// to compare (the JDK's contract allows leaving a tail).
extern "sysv64" fn vectorized_mismatch(a: usize, ao: i64, b: usize, bo: i64, length: i32, log2: i32) -> i64 {
    let n = (length.max(0) as usize) << log2;
    // SAFETY: the JDK passes a base and offset (or a null base and an address) covering
    // `length` elements, as for `Unsafe` reads.
    let (x, y) = unsafe { (std::slice::from_raw_parts((a as i64 + ao) as *const u8, n), std::slice::from_raw_parts((b as i64 + bo) as *const u8, n)) };
    let mut i = 0;
    while i + 8 <= n {
        let d = u64::from_le_bytes(x[i..i + 8].try_into().unwrap()) ^ u64::from_le_bytes(y[i..i + 8].try_into().unwrap());
        if d != 0 {
            return ((i + (d.trailing_zeros() / 8) as usize) >> log2) as i64;
        }
        i += 8;
    }
    match (i..n).find(|&k| x[k] != y[k]) {
        Some(k) => (k >> log2) as i64,
        None => -1,
    }
}

/// `31^k` (wrapping) for `k` in `0..=32`.
const POW31: [i32; 33] = {
    let mut p = [1i32; 33];
    let mut k = 1;
    while k < 33 {
        p[k] = p[k - 1].wrapping_mul(31);
        k += 1;
    }
    p
};

/// `31^n * init + sum(e(i) * 31^(n-1-i))` for `i` in `0..n`, element `i` read by `e`.
#[inline(always)]
fn fold_scalar(mut h: i32, n: usize, e: impl Fn(usize) -> i32) -> i32 {
    let mut i = 0;
    while i + 4 <= n {
        h = h.wrapping_mul(POW31[4]).wrapping_add(e(i).wrapping_mul(POW31[3])).wrapping_add(e(i + 1).wrapping_mul(POW31[2])).wrapping_add(e(i + 2).wrapping_mul(31)).wrapping_add(e(i + 3));
        i += 4;
    }
    while i < n {
        h = h.wrapping_mul(31).wrapping_add(e(i));
        i += 1;
    }
    h
}

/// The same fold with AVX2, as HotSpot's `vectorizedHashCode` intrinsic: eight 32-bit lanes per
/// accumulator, four accumulators over 32-element blocks (each lane times `31^32` per block), then
/// 8-element chunks; the lanes are weighted by their remaining powers of 31 and summed. `load`
/// widens the 8 elements at `p` to 32-bit lanes; `size` is the element size in bytes.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn fold_avx2(h: i32, p: *const u8, n: usize, size: usize, load: impl Fn(*const u8) -> std::arch::x86_64::__m256i, e: impl Fn(usize) -> i32) -> i32 {
    use std::arch::x86_64::*;
    // Lane l of a chunk of 8 weighs 31^(7-l); of block j of 4, 31^(31-8j-l).
    let weights = |top: usize| _mm256_setr_epi32(POW31[top], POW31[top - 1], POW31[top - 2], POW31[top - 3], POW31[top - 4], POW31[top - 5], POW31[top - 6], POW31[top - 7]);
    let hsum = |v: __m256i| {
        let s = _mm_add_epi32(_mm256_castsi256_si128(v), _mm256_extracti128_si256::<1>(v));
        let s = _mm_add_epi32(s, _mm_shuffle_epi32::<0b01_00_11_10>(s));
        let s = _mm_add_epi32(s, _mm_shuffle_epi32::<0b10_11_00_01>(s));
        _mm_cvtsi128_si32(s)
    };
    let mut h = h;
    let mut i = 0;
    if n >= 32 {
        let step = _mm256_set1_epi32(POW31[32]);
        let mut acc = [_mm256_setzero_si256(); 4];
        while i + 32 <= n {
            for (j, a) in acc.iter_mut().enumerate() {
                *a = _mm256_add_epi32(_mm256_mullo_epi32(*a, step), load(p.add((i + 8 * j) * size)));
            }
            i += 32;
        }
        let mut sum = _mm256_setzero_si256();
        for (j, a) in acc.iter().enumerate() {
            sum = _mm256_add_epi32(sum, _mm256_mullo_epi32(*a, weights(31 - 8 * j)));
        }
        h = h.wrapping_mul(pow31(i)).wrapping_add(hsum(sum));
    }
    while i + 8 <= n {
        let v = _mm256_mullo_epi32(load(p.add(i * size)), weights(7));
        h = h.wrapping_mul(POW31[8]).wrapping_add(hsum(v));
        i += 8;
    }
    fold_scalar(h, n - i, |k| e(i + k))
}

/// `31^k`, wrapping.
#[cfg(target_arch = "x86_64")]
fn pow31(mut k: usize) -> i32 {
    let (mut r, mut b) = (1i32, 31i32);
    while k > 0 {
        if k & 1 != 0 {
            r = r.wrapping_mul(b);
        }
        b = b.wrapping_mul(b);
        k >>= 1;
    }
    r
}

/// Elements below which `vectorized_hash_code` folds without vectors.
#[cfg(target_arch = "x86_64")]
const SHORT_HASH: usize = 32;

/// The AVX2 fold of `n` elements of kind `ty` at `p` (chosen at run time, so one binary runs
/// on processors with and without AVX2).
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn hash_avx2(init: i32, p: *const u8, n: usize, ty: i32, rd: impl Fn(usize) -> i32) -> i32 {
    use std::arch::x86_64::*;
    let lo = |q: *const u8| _mm_loadl_epi64(q as *const __m128i);
    let full = |q: *const u8| _mm_loadu_si128(q as *const __m128i);
    match ty {
        4 => fold_avx2(init, p, n, 1, |q| _mm256_cvtepu8_epi32(lo(q)), |k| *p.add(k) as i32),
        8 => fold_avx2(init, p, n, 1, |q| _mm256_cvtepi8_epi32(lo(q)), |k| *p.add(k) as i8 as i32),
        5 => fold_avx2(init, p, n, 2, |q| _mm256_cvtepu16_epi32(full(q)), rd),
        9 => fold_avx2(init, p, n, 2, |q| _mm256_cvtepi16_epi32(full(q)), rd),
        _ => fold_avx2(init, p, n, 4, |q| _mm256_loadu_si256(q as *const __m256i), rd),
    }
}

/// `31^n * init + sum(a[from + i] * 31^(n-1-i))` over `n = len` elements of the kind `ty`
/// names (`ArraysSupport.T_BOOLEAN` unsigned bytes, `T_CHAR` chars of a `char[]` or of a UTF-16
/// `byte[]`, `T_BYTE` signed bytes, `T_SHORT`, `T_INT`).
extern "sysv64" fn vectorized_hash_code(a: Oop, from: i32, len: i32, init: i32, ty: i32) -> i64 {
    let size = match ty {
        4 | 8 => 1,
        5 | 9 => 2,
        10 => 4,
        _ => return init as i64,
    };
    // SAFETY: a live array (null-checked by the caller) of the kind the JDK names; the slice
    // bounds every read below. A UTF-16 `byte[]` and a `char[]` both hold 2-byte chars in
    // native order.
    let h = unsafe {
        let b = bytes(a, size, from, len);
        let (p, n) = (b.as_ptr(), b.len() / size);
        let rd = |k: usize| -> i32 {
            match ty {
                4 => *p.add(k) as i32,
                8 => *p.add(k) as i8 as i32,
                5 => (p.add(2 * k) as *const u16).read_unaligned() as i32,
                9 => (p.add(2 * k) as *const i16).read_unaligned() as i32,
                _ => (p.add(4 * k) as *const i32).read_unaligned(),
            }
        };
        // Short arrays (most strings) fold faster in scalar steps of four than through a vector
        // multiply and a horizontal sum.
        #[cfg(target_arch = "x86_64")]
        if n >= SHORT_HASH && std::arch::is_x86_feature_detected!("avx2") {
            hash_avx2(init, p, n, ty, rd)
        } else {
            match ty {
                4 => fold_scalar(init, n, |k| *p.add(k) as i32),
                8 => fold_scalar(init, n, |k| *p.add(k) as i8 as i32),
                _ => fold_scalar(init, n, rd),
            }
        }
        #[cfg(not(target_arch = "x86_64"))]
        fold_scalar(init, n, rd)
    };
    h as i64
}

extern "sysv64" fn count_positives(a: Oop, off: i32, len: i32) -> i64 {
    // SAFETY: a live byte array (null-checked by the caller).
    let b = unsafe { bytes(a, 1, off, len) };
    let mut i = 0;
    while i + 8 <= b.len() {
        if u64::from_le_bytes(b[i..i + 8].try_into().unwrap()) & 0x8080_8080_8080_8080 != 0 {
            break;
        }
        i += 8;
    }
    (i + b[i..].iter().take_while(|&&x| x < 0x80).count()) as i64
}

extern "sysv64" fn is_array(mirror: Oop) -> i64 {
    crate::runtime::klass_of_mirror(mirror).is_some_and(|k| !matches!(k.kind, crate::klass::KlassKind::Instance)) as i64
}

extern "sysv64" fn is_primitive(mirror: Oop) -> i64 {
    crate::runtime::klass_of_mirror(mirror).is_none() as i64
}

extern "sysv64" fn is_interface(mirror: Oop) -> i64 {
    crate::runtime::klass_of_mirror(mirror).is_some_and(|k| k.is_interface()) as i64
}

/// A primitive mirror is assignable only from itself.
extern "sysv64" fn is_assignable_from(to: Oop, from: Oop) -> i64 {
    match (crate::runtime::klass_of_mirror(to), crate::runtime::klass_of_mirror(from)) {
        (Some(to), Some(from)) => from.is_subtype_of(to) as i64,
        _ => (to == from) as i64,
    }
}

/// The element size (log2) of the whole-array primitive `Arrays.fill` with descriptor `desc`.
pub(crate) fn fill_shift(desc: &str) -> Option<u8> {
    Some(match desc {
        "([BB)V" | "([ZZ)V" => 0,
        "([CC)V" | "([SS)V" => 1,
        "([II)V" | "([FF)V" => 2,
        "([JJ)V" | "([DD)V" => 3,
        _ => return None,
    })
}

/// Stores the low bytes of `v` (an element's bits) into every element of primitive array `a`.
///
/// # Safety
/// `a` is a live, non-null array of elements of `T`'s size.
pub(crate) unsafe fn fill<T: Copy + TryFrom<u64> + Default>(a: Oop, v: u64) {
    let x = T::try_from(v & (u64::MAX >> (64 - 8 * std::mem::size_of::<T>()))).unwrap_or_default();
    obj::array_slice::<T>(a).fill(x);
}

extern "sysv64" fn fill_leaf<T: Copy + TryFrom<u64> + Default>(a: Oop, v: u64) -> i64 {
    // SAFETY: a live primitive array of `T` (null-checked by the caller).
    unsafe { fill::<T>(a, v) };
    0
}

extern "sysv64" fn refers_to(r: Oop, o: Oop) -> i64 {
    // SAFETY: a live Reference (the receiver, null-checked by the invoke).
    (unsafe { obj::read_ref(r, crate::natives::misc::referent_offset()) } == o) as i64
}
