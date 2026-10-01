//! SHA-256 on the x86 SHA extensions (`sha256rnds2`, `sha256msg1/2`), as HotSpot's
//! `sha256_implCompress` stub uses them: four rounds per pair of instructions, the message
//! schedule in vector registers. State lanes are kept as ABEF/CDGH, the layout `sha256rnds2`
//! works on.

use super::K256;
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

/// Whether the CPU has the instructions [`sha256`] needs.
pub(super) fn available() -> bool {
    #[cfg(target_arch = "x86_64")]
    return is_x86_feature_detected!("sha") && is_x86_feature_detected!("sse4.1") && is_x86_feature_detected!("ssse3");
    #[cfg(not(target_arch = "x86_64"))]
    false
}

/// Folds `blocks` (a whole number of 64-byte blocks) into `h`; only called when [`available`].
pub(super) fn sha256(h: &mut [u32; 8], blocks: &[u8]) {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: selected only after `available()` found the instructions.
    unsafe {
        compress(h, blocks)
    }
    #[cfg(not(target_arch = "x86_64"))]
    unreachable!("{h:?} {}", blocks.len())
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
unsafe fn compress(h: &mut [u32; 8], blocks: &[u8]) {
    // Big-endian words within each 32-bit lane.
    let bswap = _mm_set_epi64x(0x0c0d_0e0f_0809_0a0b, 0x0405_0607_0001_0203);
    let p = h.as_mut_ptr() as *mut __m128i;
    let dcba = _mm_shuffle_epi32(_mm_loadu_si128(p), 0xb1); // CDAB
    let hgfe = _mm_shuffle_epi32(_mm_loadu_si128(p.add(1)), 0x1b); // EFGH
    let mut abef = _mm_alignr_epi8(dcba, hgfe, 8);
    let mut cdgh = _mm_blend_epi16(hgfe, dcba, 0xf0);
    for block in blocks.chunks_exact(64) {
        let (abef0, cdgh0) = (abef, cdgh);
        let src = block.as_ptr() as *const __m128i;
        let mut w = [0, 1, 2, 3].map(|i| _mm_shuffle_epi8(_mm_loadu_si128(src.add(i)), bswap));
        for i in 0..16 {
            if i >= 4 {
                // w[i] = σ1(w[i-2]) + w[i-7] + σ0(w[i-15]) + w[i-16], four lanes at a time.
                let s = _mm_sha256msg1_epu32(w[i % 4], w[(i + 1) % 4]);
                let s = _mm_add_epi32(s, _mm_alignr_epi8(w[(i + 3) % 4], w[(i + 2) % 4], 4));
                w[i % 4] = _mm_sha256msg2_epu32(s, w[(i + 3) % 4]);
            }
            let wk = _mm_add_epi32(w[i % 4], _mm_loadu_si128(K256.as_ptr().add(4 * i) as *const __m128i));
            cdgh = _mm_sha256rnds2_epu32(cdgh, abef, wk);
            abef = _mm_sha256rnds2_epu32(abef, cdgh, _mm_shuffle_epi32(wk, 0x0e));
        }
        abef = _mm_add_epi32(abef, abef0);
        cdgh = _mm_add_epi32(cdgh, cdgh0);
    }
    let feba = _mm_shuffle_epi32(abef, 0x1b);
    let dchg = _mm_shuffle_epi32(cdgh, 0xb1);
    _mm_storeu_si128(p, _mm_blend_epi16(feba, dchg, 0xf0));
    _mm_storeu_si128(p.add(1), _mm_alignr_epi8(dchg, feba, 8));
}

#[cfg(test)]
mod tests {
    /// The SHA-NI path agrees with the portable one on multi-block input.
    #[test]
    fn matches_portable() {
        if !super::available() {
            return;
        }
        let data: Vec<u8> = (0..64 * 9).map(|i| (i * 31 + 7) as u8).collect();
        let iv = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
        let (mut a, mut b) = (iv, iv);
        super::sha256(&mut a, &data);
        super::super::sha256(&mut b, &data);
        assert_eq!(a, b);
    }
}
