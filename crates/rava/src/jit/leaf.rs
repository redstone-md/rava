//! Array and string routines optimized code calls directly in place of the JDK methods marked
//! `@IntrinsicCandidate` for them (HotSpot's string and array intrinsics): comparing, searching
//! and hashing whole arrays a word or a vector at a time instead of an element per iteration.
//! The interpreter and baseline code run the methods' bytecode, which computes the same
//! results. Each routine is a leaf: it reads arrays, allocates nothing and cannot stop, so the
//! collector never runs while it holds a reference. Ranges come from the JDK callers, which
//! check them; reads are still clamped to the array so a bad one cannot leave it.

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
}

impl Leaf {
    /// The routine's address (`Intrinsic::Leaf`).
    pub(crate) fn address(self) -> usize {
        match self {
            Leaf::Latin1Equals => latin1_equals as *const () as usize,
            Leaf::Mismatch => vectorized_mismatch as *const () as usize,
            Leaf::HashCode => vectorized_hash_code as *const () as usize,
            Leaf::CountPositives => count_positives as *const () as usize,
        }
    }

    /// The routine standing for `holder.name desc`.
    pub(crate) fn of(holder: &str, name: &str, desc: &str) -> Option<Leaf> {
        Some(match (holder, name, desc) {
            ("java/lang/StringLatin1", "equals", "([B[B)Z") => Leaf::Latin1Equals,
            ("jdk/internal/util/ArraysSupport", "vectorizedMismatch", "(Ljava/lang/Object;JLjava/lang/Object;JII)I") => Leaf::Mismatch,
            ("jdk/internal/util/ArraysSupport", "vectorizedHashCode", "(Ljava/lang/Object;IIII)I") => Leaf::HashCode,
            ("java/lang/StringCoding", "countPositives", "([BII)I") => Leaf::CountPositives,
            _ => return None,
        })
    }

    /// Which arguments are arrays the routine reads (null-checked before the call); the
    /// mismatch's bases may be null, its offsets then being addresses (direct buffers).
    pub(crate) fn array_args(self) -> &'static [usize] {
        match self {
            Leaf::Latin1Equals => &[0, 1],
            Leaf::Mismatch => &[],
            Leaf::HashCode | Leaf::CountPositives => &[0],
        }
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

extern "C" fn latin1_equals(a: Oop, b: Oop) -> i64 {
    // SAFETY: two live byte arrays (null-checked by the caller).
    unsafe {
        let n = obj::array_length(a);
        (n == obj::array_length(b) && bytes(a, 1, 0, n) == bytes(b, 1, 0, n)) as i64
    }
}

/// The index of the first element (of `1 << log2` bytes) at which the `length` elements at
/// `a + ao` and `b + bo` differ; else `-1`, the complement of no elements left for the caller
/// to compare (the JDK's contract allows leaving a tail).
extern "C" fn vectorized_mismatch(a: usize, ao: i64, b: usize, bo: i64, length: i32, log2: i32) -> i64 {
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

/// `31^4`, `31^3`, `31^2` and `31`: four elements fold into a hash at once.
const P: [i32; 4] = [923521, 29791, 961, 31];

fn fold(init: i32, mut xs: impl ExactSizeIterator<Item = i32>) -> i32 {
    let mut h = init;
    while xs.len() >= 4 {
        let (a, b, c, d) = (xs.next().unwrap(), xs.next().unwrap(), xs.next().unwrap(), xs.next().unwrap());
        h = h.wrapping_mul(P[0]).wrapping_add(a.wrapping_mul(P[1])).wrapping_add(b.wrapping_mul(P[2])).wrapping_add(c.wrapping_mul(P[3])).wrapping_add(d);
    }
    xs.fold(h, |h, x| h.wrapping_mul(31).wrapping_add(x))
}

/// `31^n * init + sum(a[from + i] * 31^(n-1-i))` over `n = len` elements of the kind `ty`
/// names (`ArraysSupport.T_BOOLEAN` unsigned bytes, `T_CHAR` chars of a `char[]` or of a UTF-16
/// `byte[]`, `T_BYTE` signed bytes, `T_SHORT`, `T_INT`).
extern "C" fn vectorized_hash_code(a: Oop, from: i32, len: i32, init: i32, ty: i32) -> i64 {
    // SAFETY: a live array (null-checked by the caller) of the kind the JDK names.
    let h = unsafe {
        // A UTF-16 `byte[]` and a `char[]` both hold 2-byte chars in native order.
        match ty {
            4 => fold(init, bytes(a, 1, from, len).iter().map(|&x| x as i32)),
            8 => fold(init, bytes(a, 1, from, len).iter().map(|&x| x as i8 as i32)),
            5 => fold(init, bytes(a, 2, from, len).chunks_exact(2).map(|c| u16::from_ne_bytes([c[0], c[1]]) as i32)),
            9 => fold(init, bytes(a, 2, from, len).chunks_exact(2).map(|c| i16::from_ne_bytes([c[0], c[1]]) as i32)),
            10 => fold(init, bytes(a, 4, from, len).chunks_exact(4).map(|c| i32::from_ne_bytes([c[0], c[1], c[2], c[3]]))),
            _ => init,
        }
    };
    h as i64
}

extern "C" fn count_positives(a: Oop, off: i32, len: i32) -> i64 {
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
