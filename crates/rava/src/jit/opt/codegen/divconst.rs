//! Division and remainder by a constant without `idiv` (C2's `DivI`/`DivL`/`ModI`/`ModL` by
//! constant, after Warren, *Hacker's Delight* ch. 10): ±1 needs no division, a power of two is
//! a biased shift, any other divisor a multiplication by its magic reciprocal and a shift.
//! Quotients truncate toward zero and remainders take the dividend's sign, as in Java; `MIN / -1`
//! wraps.

use super::Gen;
use crate::jit::asm::{Alu, Shift, R};
use crate::jit::opt::ir::V;

/// Magic multiplier and shift for signed division by `d` (`|d| >= 2`, not a power of two) in
/// `bits`-bit words: `q = hi(n * m) (+ n if d > 0 and m < 0, - n if d < 0 and m > 0) >> s`,
/// plus one when negative. `m` is a `bits`-bit signed value.
pub(crate) fn magic(d: i64, bits: u32) -> (i64, u32) {
    let two = 1u128 << (bits - 1);
    let ad = d.unsigned_abs() as u128;
    let t = two + u128::from(d < 0);
    let anc = t - 1 - t % ad;
    let (mut q1, mut r1) = (two / anc, two % anc);
    let (mut q2, mut r2) = (two / ad, two % ad);
    let mut p = bits - 1;
    loop {
        p += 1;
        q1 *= 2;
        r1 *= 2;
        if r1 >= anc {
            q1 += 1;
            r1 -= anc;
        }
        q2 *= 2;
        r2 *= 2;
        if r2 >= ad {
            q2 += 1;
            r2 -= ad;
        }
        let delta = ad - r2;
        if !(q1 < delta || (q1 == delta && r1 == 0)) {
            break;
        }
    }
    // q2 + 1 as a `bits`-bit two's complement value, negated for a negative divisor.
    let wrap = |x: u128| (((x << (128 - bits)) as i128) >> (128 - bits)) as i64;
    let m = wrap(q2 + 1);
    (if d < 0 { wrap((m as i128).wrapping_neg() as u128) } else { m }, p - bits)
}

impl Gen<'_> {
    /// `x / d` or `x % d` for a constant `d` (non-zero; `w` for long). Leaves the result in rax.
    pub(in crate::jit::opt) fn div_const(&mut self, v: V, w: bool, rem: bool, x: V, d: i64) {
        let bits = if w { 64 } else { 32 };
        let d = if w { d } else { d as i32 as i64 };
        self.load(R::Rax, x);
        let a = &mut self.a;
        match d.unsigned_abs() {
            1 if rem => a.alu_rr(false, Alu::Xor, R::Rax, R::Rax),
            1 => {
                if d < 0 {
                    a.neg(w, R::Rax);
                }
            }
            ad if ad.is_power_of_two() => {
                let k = ad.trailing_zeros() as u8;
                // bias = 2^k - 1 for a negative dividend, else 0.
                a.mov_rr(w, R::Rdx, R::Rax);
                a.shift_ri(w, Shift::Sar, R::Rdx, bits as u8 - 1);
                a.shift_ri(w, Shift::Shr, R::Rdx, bits as u8 - k);
                if rem {
                    a.alu_rr(w, Alu::Add, R::Rax, R::Rdx);
                    match i32::try_from(ad - 1) {
                        Ok(mask) => a.alu_ri(w, Alu::And, R::Rax, mask),
                        Err(_) => {
                            a.mov_ri(R::Rcx, (ad - 1) as i64);
                            a.alu_rr(w, Alu::And, R::Rax, R::Rcx);
                        }
                    }
                    a.alu_rr(w, Alu::Sub, R::Rax, R::Rdx);
                } else {
                    a.alu_rr(w, Alu::Add, R::Rax, R::Rdx);
                    a.shift_ri(w, Shift::Sar, R::Rax, k);
                    if d < 0 {
                        a.neg(w, R::Rax);
                    }
                }
            }
            _ => {
                let (m, s) = magic(d, bits);
                // The dividend stays in r11; the high half of n * m ends up in rdx.
                a.mov_rr(true, R::R11, R::Rax);
                if w {
                    a.mov_ri(R::Rcx, m);
                    a.imul_wide(true, R::Rcx);
                } else {
                    // Exact 64-bit product of the sign-extended dividend; its high 32 bits.
                    a.movsxd(R::Rax, R::Rax);
                    a.imul_rri(true, R::Rdx, R::Rax, m as i32);
                    a.shift_ri(true, Shift::Sar, R::Rdx, 32);
                }
                if d > 0 && m < 0 {
                    a.alu_rr(w, Alu::Add, R::Rdx, R::R11);
                } else if d < 0 && m > 0 {
                    a.alu_rr(w, Alu::Sub, R::Rdx, R::R11);
                }
                if s > 0 {
                    a.shift_ri(w, Shift::Sar, R::Rdx, s as u8);
                }
                // Plus one when negative: truncation toward zero.
                a.mov_rr(w, R::Rax, R::Rdx);
                a.shift_ri(w, Shift::Shr, R::Rax, bits as u8 - 1);
                a.alu_rr(w, Alu::Add, R::Rax, R::Rdx);
                if rem {
                    // x - q * d.
                    match i32::try_from(d) {
                        Ok(d) => a.imul_rri(w, R::Rax, R::Rax, d),
                        Err(_) => {
                            a.mov_ri(R::Rcx, d);
                            a.imul_rr(w, R::Rax, R::Rcx);
                        }
                    }
                    a.alu_rr(w, Alu::Sub, R::R11, R::Rax);
                    a.mov_rr(w, R::Rax, R::R11);
                }
            }
        }
        if !w {
            // An int result keeps a zero high half.
            a.mov_rr(false, R::Rax, R::Rax);
        }
        self.put(v, R::Rax);
    }
}

#[cfg(test)]
mod tests {
    use super::magic;

    /// The emitted sequence, modelled: the quotient by magic multiplication.
    fn quot(n: i64, d: i64, bits: u32) -> i64 {
        let (m, s) = magic(d, bits);
        let wrap = |x: i128| (((x << (128 - bits)) as i128) >> (128 - bits)) as i128;
        let mut q = wrap((n as i128 * m as i128) >> bits);
        if d > 0 && m < 0 {
            q = wrap(q + n as i128);
        } else if d < 0 && m > 0 {
            q = wrap(q - n as i128);
        }
        q >>= s;
        (q + i128::from(q < 0)) as i64
    }

    #[test]
    fn magic_quotients_truncate() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let divisors: Vec<i64> = [3, 5, 6, 7, 10, 12, 25, 100, 641, 1000, 1_000_000_007, -3, -7, -10, -1000, i32::MAX as i64, i32::MIN as i64 + 1]
            .into_iter()
            .chain((0..200).map(|_| (next() as i32 >> (next() % 31)) as i64))
            .filter(|d| d.unsigned_abs() >= 2 && !d.unsigned_abs().is_power_of_two())
            .collect();
        for &d in &divisors {
            let d = d as i32;
            for n in [0, 1, -1, i32::MAX, i32::MIN, d, -d, d.wrapping_mul(3), d.wrapping_mul(3) - 1].into_iter().chain((0..300).map(|_| next() as i32)) {
                assert_eq!(quot(n as i64, d as i64, 32) as i32, n.wrapping_div(d), "{n} / {d}");
            }
        }
        let wide: Vec<i64> = [3, 7, 10, 1000, 0x5851f42d4c957f2d, -9, i64::MAX, i64::MIN + 1].into_iter().chain((0..200).map(|_| next() as i64 >> (next() % 63))).filter(|d| d.unsigned_abs() >= 2 && !d.unsigned_abs().is_power_of_two()).collect();
        for &d in &wide {
            for n in [0, 1, -1, i64::MAX, i64::MIN, d, -d].into_iter().chain((0..300).map(|_| next() as i64)) {
                assert_eq!(quot(n, d, 64), n.wrapping_div(d), "{n} / {d}");
            }
        }
    }
}
