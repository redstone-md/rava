//! Compression functions of the SHA digests, supplied by the VM for the JDK's
//! `@IntrinsicCandidate` `implCompress0` methods (HotSpot's SHA stubs): one 64- or 128-byte
//! block of `buf` at `ofs` folded into the digest's `state`. `DigestBase.implCompressMultiBlock0`
//! folds consecutive blocks the same way for these digests (and calls `implCompress` per block
//! for any other).

use super::{ret_void, NativeRegistry};
use crate::heap::{obj, Oop};
use crate::runtime::{throw_new, vm_field};
use crate::thread::{JResult, Thread};

mod sha_ni;

vm_field!(sha1_state, "sun/security/provider/SHA", "state", "[I");
vm_field!(sha2_state, "sun/security/provider/SHA2", "state", "[I");
vm_field!(sha5_state, "sun/security/provider/SHA5", "state", "[J");
vm_field!(block_size, "sun/security/provider/DigestBase", "blockSize", "I");

type Stub = fn(&mut Thread, &[u64]) -> JResult<u64>;
/// Folds `blocks` (a whole number of blocks) into the digest state, block by block.
type Blocks<W, const S: usize> = fn(&mut [W; S], &[u8]);

/// One of the SHA families: compresses `n` consecutive blocks of `buf` from `ofs` into the
/// receiver's state.
type Family = fn(&mut Thread, Oop, Oop, i32, usize) -> JResult<()>;

const SHA1: Family = |t, this, buf, ofs, n| compress::<u32, 5, 64>(t, this, buf, ofs, n, sha1_state(), sha1);
const SHA256: Family = |t, this, buf, ofs, n| compress::<u32, 8, 64>(t, this, buf, ofs, n, sha2_state(), sha256_blocks());
const SHA512: Family = |t, this, buf, ofs, n| compress::<u64, 8, 128>(t, this, buf, ofs, n, sha5_state(), sha512);

/// `implCompress0(b, ofs)` of a family: one block.
fn one_block<const F: usize>(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    [SHA1, SHA256, SHA512][F](t, a[0] as Oop, a[1] as Oop, a[2] as i32, 1)?;
    ret_void()
}

pub(super) fn register(r: &mut NativeRegistry) {
    r.add_stub("sun/security/provider/SHA", "implCompress0", "([BI)V", one_block::<0> as Stub);
    r.add_stub("sun/security/provider/SHA2", "implCompress0", "([BI)V", one_block::<1> as Stub);
    r.add_stub("sun/security/provider/SHA5", "implCompress0", "([BI)V", one_block::<2> as Stub);
    r.add_stub("sun/security/provider/DigestBase", "implCompressMultiBlock0", "([BII)I", multi_block);
}

/// The block function for the receiver's digest, if it is one of the SHA families.
fn family_of(k: &'static crate::klass::Klass) -> Option<Family> {
    std::iter::successors(Some(k), |c| c.super_klass).find_map(|c| match c.name.as_str() {
        "sun/security/provider/SHA" => Some(SHA1),
        "sun/security/provider/SHA2" => Some(SHA256),
        "sun/security/provider/SHA5" => Some(SHA512),
        _ => None,
    })
}

/// `implCompressMultiBlock0(b, ofs, limit)`: compresses the blocks starting at `ofs` up to
/// `limit`; returns the offset after the last.
fn multi_block(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let (this, buf, mut ofs, limit) = (a[0] as Oop, a[1], a[2] as i32, a[3] as i32);
    // SAFETY: the live receiver.
    let (k, size) = unsafe { (obj::klass(this), obj::read::<i32>(this, block_size())) };
    if ofs > limit || size <= 0 {
        return super::ret_int(ofs);
    }
    let n = ((limit - ofs) / size + 1) as usize;
    if let Some(f) = family_of(k) {
        f(t, this, buf as Oop, ofs, n)?;
        return super::ret_int(ofs + n as i32 * size);
    }
    let base = crate::loader::load("sun/security/provider/DigestBase").expect("DigestBase");
    let decl = base.declared_method(crate::symbol::Symbol::intern("implCompress"), crate::symbol::Symbol::intern("([BI)V")).expect("implCompress");
    let m = crate::link::select_method(k, decl).unwrap_or(decl);
    for _ in 0..n {
        t.call(m, &[this as u64, buf, ofs as u32 as u64])?;
        ofs += size;
    }
    super::ret_int(ofs)
}

/// Reads `n` blocks of `B` bytes, applies `f` to the receiver's `state` array (`S` words of
/// type `W`).
fn compress<W: Copy, const S: usize, const B: usize>(
    t: &mut Thread,
    this: Oop,
    buf: Oop,
    ofs: i32,
    n: usize,
    state_offset: u32,
    f: Blocks<W, S>,
) -> JResult<()> {
    if buf.is_null() {
        return Err(throw_new(t, "java/lang/NullPointerException", ""));
    }
    // SAFETY: the receiver's state array and the byte[] argument are live.
    let (state, bytes) = unsafe { (obj::read::<Oop>(this, state_offset), obj::array_slice::<u8>(buf)) };
    let blocks = usize::try_from(ofs).ok().and_then(|o| bytes.get(o..o.checked_add(B * n)?));
    let (Some(blocks), false) = (blocks, state.is_null()) else {
        return Err(throw_new(t, "java/lang/ArrayIndexOutOfBoundsException", &format!("Index {ofs} out of bounds for length {}", bytes.len())));
    };
    // SAFETY: the state array holds S words of W (int[] or long[]).
    let words = unsafe { obj::array_slice::<W>(state) };
    let Ok(words) = <&mut [W; S]>::try_from(words) else {
        return Err(throw_new(t, "java/lang/ArrayIndexOutOfBoundsException", "digest state"));
    };
    f(words, blocks);
    Ok(())
}

/// SHA-256 with the CPU's SHA extensions when it has them (as HotSpot's stub), else portable.
fn sha256_blocks() -> Blocks<u32, 8> {
    static PICK: std::sync::OnceLock<Blocks<u32, 8>> = std::sync::OnceLock::new();
    *PICK.get_or_init(|| sha_ni::available().then_some(sha_ni::sha256 as Blocks<u32, 8>).unwrap_or(sha256))
}

fn sha1(h: &mut [u32; 5], blocks: &[u8]) {
    blocks.chunks_exact(64).for_each(|b| sha1_block(h, b));
}

fn sha1_block(h: &mut [u32; 5], block: &[u8]) {
    let mut w = [0u32; 80];
    for i in 0..16 {
        w[i] = u32::from_be_bytes(block[4 * i..4 * i + 4].try_into().unwrap());
    }
    for i in 16..80 {
        w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
    }
    let [mut a, mut b, mut c, mut d, mut e] = *h;
    for (i, &wi) in w.iter().enumerate() {
        let (f, k) = match i {
            0..=19 => ((b & c) | (!b & d), 0x5a827999),
            20..=39 => (b ^ c ^ d, 0x6ed9eba1),
            40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1bbcdc),
            _ => (b ^ c ^ d, 0xca62c1d6),
        };
        let tmp = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(wi);
        (e, d, c, b, a) = (d, c, b.rotate_left(30), a, tmp);
    }
    for (x, v) in h.iter_mut().zip([a, b, c, d, e]) {
        *x = x.wrapping_add(v);
    }
}

const K256: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01,
    0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
    0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
    0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08,
    0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn sha256(h: &mut [u32; 8], blocks: &[u8]) {
    blocks.chunks_exact(64).for_each(|b| sha256_block(h, b));
}

fn sha256_block(h: &mut [u32; 8], block: &[u8]) {
    let mut w = [0u32; 64];
    for i in 0..16 {
        w[i] = u32::from_be_bytes(block[4 * i..4 * i + 4].try_into().unwrap());
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = *h;
    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ (!e & g);
        let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K256[i]).wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        (hh, g, f, e, d, c, b, a) = (g, f, e, d.wrapping_add(t1), c, b, a, t1.wrapping_add(t2));
    }
    for (x, v) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
        *x = x.wrapping_add(v);
    }
}

const K512: [u64; 80] = [
    0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc, 0x3956c25bf348b538, 0x59f111f1b605d019,
    0x923f82a4af194f9b, 0xab1c5ed5da6d8118, 0xd807aa98a3030242, 0x12835b0145706fbe, 0x243185be4ee4b28c, 0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f, 0x80deb1fe3b1696b1, 0x9bdc06a725c71235, 0xc19bf174cf692694, 0xe49b69c19ef14ad2, 0xefbe4786384f25e3,
    0x0fc19dc68b8cd5b5, 0x240ca1cc77ac9c65, 0x2de92c6f592b0275, 0x4a7484aa6ea6e483, 0x5cb0a9dcbd41fbd4, 0x76f988da831153b5,
    0x983e5152ee66dfab, 0xa831c66d2db43210, 0xb00327c898fb213f, 0xbf597fc7beef0ee4, 0xc6e00bf33da88fc2, 0xd5a79147930aa725,
    0x06ca6351e003826f, 0x142929670a0e6e70, 0x27b70a8546d22ffc, 0x2e1b21385c26c926, 0x4d2c6dfc5ac42aed, 0x53380d139d95b3df,
    0x650a73548baf63de, 0x766a0abb3c77b2a8, 0x81c2c92e47edaee6, 0x92722c851482353b, 0xa2bfe8a14cf10364, 0xa81a664bbc423001,
    0xc24b8b70d0f89791, 0xc76c51a30654be30, 0xd192e819d6ef5218, 0xd69906245565a910, 0xf40e35855771202a, 0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8, 0x1e376c085141ab53, 0x2748774cdf8eeb99, 0x34b0bcb5e19b48a8, 0x391c0cb3c5c95a63, 0x4ed8aa4ae3418acb,
    0x5b9cca4f7763e373, 0x682e6ff3d6b2b8a3, 0x748f82ee5defb2fc, 0x78a5636f43172f60, 0x84c87814a1f0ab72, 0x8cc702081a6439ec,
    0x90befffa23631e28, 0xa4506cebde82bde9, 0xbef9a3f7b2c67915, 0xc67178f2e372532b, 0xca273eceea26619c, 0xd186b8c721c0c207,
    0xeada7dd6cde0eb1e, 0xf57d4f7fee6ed178, 0x06f067aa72176fba, 0x0a637dc5a2c898a6, 0x113f9804bef90dae, 0x1b710b35131c471b,
    0x28db77f523047d84, 0x32caab7b40c72493, 0x3c9ebe0a15c9bebc, 0x431d67c49c100d4c, 0x4cc5d4becb3e42b6, 0x597f299cfc657e2a,
    0x5fcb6fab3ad6faec, 0x6c44198c4a475817,
];

fn sha512(h: &mut [u64; 8], blocks: &[u8]) {
    blocks.chunks_exact(128).for_each(|b| sha512_block(h, b));
}

fn sha512_block(h: &mut [u64; 8], block: &[u8]) {
    let mut w = [0u64; 80];
    for i in 0..16 {
        w[i] = u64::from_be_bytes(block[8 * i..8 * i + 8].try_into().unwrap());
    }
    for i in 16..80 {
        let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
        let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
        w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = *h;
    for i in 0..80 {
        let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
        let ch = (e & f) ^ (!e & g);
        let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K512[i]).wrapping_add(w[i]);
        let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        (hh, g, f, e, d, c, b, a) = (g, f, e, d.wrapping_add(t1), c, b, a, t1.wrapping_add(t2));
    }
    for (x, v) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
        *x = x.wrapping_add(v);
    }
}
