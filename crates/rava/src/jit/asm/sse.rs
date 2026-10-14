//! SSE (scalar float and double), locked and fence instructions of the encoder.

use super::{lo, Asm, M, R, X};

impl Asm {
    fn sse_rm(&mut self, prefix: u8, w: bool, op: u8, reg: u8, m: M) {
        if prefix != 0 {
            self.b(prefix);
        }
        self.rex_m(w, reg, m, false);
        self.b(0x0F);
        self.b(op);
        self.modrm_m(reg, m);
    }
    fn sse_rr(&mut self, prefix: u8, w: bool, op: u8, reg: u8, rm: u8) {
        if prefix != 0 {
            self.b(prefix);
        }
        self.rex(w, reg, 0, rm, false);
        self.b(0x0F);
        self.b(op);
        self.b(0xC0 | lo(reg) << 3 | lo(rm));
    }
    /// `xorps x, x`: zero idiom, which also ends any dependency on `x`'s old value.
    fn zero_x(&mut self, x: X) {
        self.sse_rr(0, false, 0x57, x.0, x.0);
    }
    /// Scalar conversions, `sqrtsd` and `roundsd` keep the destination's upper lanes, so they
    /// wait for its last writer (often the previous loop iteration): clear it first, as other
    /// compilers do, unless it is also the source.
    fn break_dep(&mut self, dst: X, src: X) {
        if dst.0 != src.0 {
            self.zero_x(dst);
        }
    }
    fn fp(double: bool) -> u8 {
        if double { 0xF2 } else { 0xF3 }
    }
    pub fn fload(&mut self, double: bool, x: X, m: M) {
        self.sse_rm(Self::fp(double), false, 0x10, x.0, m);
    }
    pub fn fstore(&mut self, double: bool, m: M, x: X) {
        self.sse_rm(Self::fp(double), false, 0x11, x.0, m);
    }
    /// `addsd/subsd/mulsd/divsd x, [m]` by opcode (0x58, 0x5C, 0x59, 0x5E).
    pub fn farith_m(&mut self, double: bool, op: u8, x: X, m: M) {
        self.sse_rm(Self::fp(double), false, op, x.0, m);
    }
    /// `addsd/subsd/mulsd/divsd x, y` by opcode.
    pub fn farith_rr(&mut self, double: bool, op: u8, x: X, y: X) {
        self.sse_rr(Self::fp(double), false, op, x.0, y.0);
    }
    pub fn fsqrt(&mut self, x: X, src: X) {
        self.break_dep(x, src);
        self.sse_rr(0xF2, false, 0x51, x.0, src.0);
    }
    /// `lock cmpxchg [m], src` (compares with rax).
    pub fn lock_cmpxchg(&mut self, m: M, src: R) {
        self.lock_cmpxchg_w(true, m, src);
    }
    /// `lock cmpxchg` on a 64-bit (`w`) or 32-bit memory operand.
    pub fn lock_cmpxchg_w(&mut self, w: bool, m: M, src: R) {
        self.b(0xF0);
        self.op_rm(w, &[0x0F, 0xB1], src as u8, m);
    }
    /// The StoreLoad barrier of a volatile store: `lock addl $0, -64(%rsp)` (as HotSpot; cheaper
    /// than `mfence`, and as strong for ordinary memory). Below the stack pointer, a cache line
    /// away, so that it does not wait on the stores and loads of the top of the stack.
    pub fn storeload(&mut self) {
        for x in [0xF0, 0x83, 0x44, 0x24, 0xC0, 0x00] {
            self.b(x);
        }
    }

    pub fn mfence(&mut self) {
        self.b(0x0F);
        self.b(0xAE);
        self.b(0xF0);
    }
    /// `roundsd x, src, mode` (SSE4.1; mode 0 nearest-even, 1 floor, 2 ceil).
    pub fn round_sd(&mut self, x: X, src: X, mode: u8) {
        self.break_dep(x, src);
        self.b(0x66);
        self.rex(false, x.0, 0, src.0, false);
        self.b(0x0F);
        self.b(0x3A);
        self.b(0x0B);
        self.b(0xC0 | lo(x.0) << 3 | lo(src.0));
        self.b(mode);
    }
    /// `ucomisd/ucomiss a, b`.
    pub fn fcmp(&mut self, double: bool, a: X, b: X) {
        self.sse_rr(if double { 0x66 } else { 0 }, false, 0x2E, a.0, b.0);
    }
    /// `ucomisd/ucomiss a, [m]`.
    pub fn fcmp_m(&mut self, double: bool, a: X, m: M) {
        self.sse_rm(if double { 0x66 } else { 0 }, false, 0x2E, a.0, m);
    }
    /// Integer (`w`: 64-bit) at `m` to float/double.
    pub fn cvt_i2f_m(&mut self, double: bool, w: bool, x: X, m: M) {
        self.zero_x(x);
        self.sse_rm(Self::fp(double), w, 0x2A, x.0, m);
    }
    /// Integer (`w`: 64-bit) to float/double.
    pub fn cvt_i2f(&mut self, double: bool, w: bool, x: X, r: R) {
        self.zero_x(x);
        self.sse_rr(Self::fp(double), w, 0x2A, x.0, r as u8);
    }
    /// float↔double.
    pub fn cvt_f2f(&mut self, to_double: bool, dst: X, src: X) {
        self.break_dep(dst, src);
        self.sse_rr(if to_double { 0xF3 } else { 0xF2 }, false, 0x5A, dst.0, src.0);
    }
    /// Truncating float/double to integer (`w`: 64-bit).
    pub fn cvtt_f2i(&mut self, double: bool, w: bool, r: R, x: X) {
        self.sse_rr(Self::fp(double), w, 0x2C, r as u8, x.0);
    }
    /// The three-byte VEX prefix of an 0F38 instruction with the 66 prefix: `reg` the ModRM reg
    /// operand, `index` and `base` those of r/m, `vvvv` the extra source.
    fn vex_66_0f38(&mut self, w: bool, reg: u8, index: u8, base: u8, vvvv: u8) {
        self.b(0xC4);
        self.b((!reg >> 3 & 1) << 7 | (!index >> 3 & 1) << 6 | (!base >> 3 & 1) << 5 | 0b00010);
        self.b((w as u8) << 7 | (!vvvv & 0xF) << 3 | 0b01);
    }

    /// `vfmadd213sd/ss x, y, z`: x := x * y + z, rounded once (`Math.fma`). Needs the FMA
    /// extension.
    pub fn fma_rr(&mut self, double: bool, x: X, y: X, z: X) {
        self.vex_66_0f38(double, x.0, 0, z.0, y.0);
        self.b(0xA9);
        self.b(0xC0 | lo(x.0) << 3 | lo(z.0));
    }

    /// `vfmadd213sd/ss x, y, [m]`.
    pub fn fma_rm(&mut self, double: bool, x: X, y: X, m: M) {
        let index = m.index.map_or(0, |(i, _)| i as u8);
        self.vex_66_0f38(double, x.0, index, m.base as u8, y.0);
        self.b(0xA9);
        self.modrm_m(x.0, m);
    }

    /// `movaps dst, src` (a register copy).
    pub fn mov_xx(&mut self, dst: X, src: X) {
        self.sse_rr(0, false, 0x28, dst.0, src.0);
    }
    /// `xorpd/xorps x, y`.
    pub fn fxor(&mut self, double: bool, x: X, y: X) {
        self.sse_rr(if double { 0x66 } else { 0 }, false, 0x57, x.0, y.0);
    }
    /// `andpd/andps x, y`.
    pub fn fand(&mut self, double: bool, x: X, y: X) {
        self.sse_rr(if double { 0x66 } else { 0 }, false, 0x54, x.0, y.0);
    }
    /// `movq/movd x, r`.
    pub fn mov_xr(&mut self, w: bool, x: X, r: R) {
        self.sse_rr(0x66, w, 0x6E, x.0, r as u8);
    }
    /// `movq/movd r, x`.
    pub fn mov_rx(&mut self, w: bool, r: R, x: X) {
        self.sse_rr(0x66, w, 0x7E, x.0, r as u8);
    }
}
