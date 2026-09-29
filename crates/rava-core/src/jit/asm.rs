//! A small x86-64 encoder: exactly the instructions the baseline compiler emits.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum R {
    Rax = 0, Rcx, Rdx, Rbx, Rsp, Rbp, Rsi, Rdi, R8, R9, R10, R11, R12, R13, R14, R15,
}

/// An SSE register.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct X(pub u8);

/// `[base + index*scale + disp]`.
#[derive(Clone, Copy, Debug)]
pub struct M {
    pub base: R,
    pub index: Option<(R, u8)>,
    pub disp: i32,
}

pub fn mem(base: R, disp: i32) -> M {
    M { base, index: None, disp }
}

pub fn mem_idx(base: R, index: R, scale: u8, disp: i32) -> M {
    M { base, index: Some((index, scale)), disp }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Cond {
    O = 0, No, B, Ae, E, Ne, Be, A, S, Ns, P, Np, L, Ge, Le, G,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Label(u32);

#[derive(Clone, Copy)]
pub enum Alu {
    Add = 0, Or = 1, And = 4, Sub = 5, Xor = 6, Cmp = 7,
}

#[derive(Clone, Copy)]
pub enum Shift {
    Shl = 4, Shr = 5, Sar = 7,
}

#[derive(Default)]
pub struct Asm {
    pub buf: Vec<u8>,
    labels: Vec<Option<u32>>,
    fixups: Vec<(u32, Label)>,
}

fn lo(r: u8) -> u8 {
    r & 7
}

impl Asm {
    pub fn pos(&self) -> u32 {
        self.buf.len() as u32
    }

    pub fn new_label(&mut self) -> Label {
        self.labels.push(None);
        Label(self.labels.len() as u32 - 1)
    }

    pub fn bind(&mut self, l: Label) {
        self.labels[l.0 as usize] = Some(self.pos());
    }

    pub fn label_pos(&self, l: Label) -> Option<u32> {
        self.labels[l.0 as usize]
    }

    /// Resolves every label reference; returns the finished code.
    pub fn finish(mut self) -> Vec<u8> {
        for (at, l) in std::mem::take(&mut self.fixups) {
            let target = self.labels[l.0 as usize].expect("unbound label") as i64;
            let rel = (target - (at as i64 + 4)) as i32;
            self.buf[at as usize..at as usize + 4].copy_from_slice(&rel.to_le_bytes());
        }
        self.buf
    }

    fn b(&mut self, x: u8) {
        self.buf.push(x);
    }

    fn d32(&mut self, x: i32) {
        self.buf.extend_from_slice(&x.to_le_bytes());
    }

    /// REX prefix when needed (always when `w`, or any extended register, or `force`).
    fn rex(&mut self, w: bool, reg: u8, index: u8, base: u8, force: bool) {
        let v = 0x40 | (w as u8) << 3 | (reg >> 3) << 2 | (index >> 3) << 1 | (base >> 3);
        if v != 0x40 || force {
            self.b(v);
        }
    }

    fn rex_m(&mut self, w: bool, reg: u8, m: M, force: bool) {
        let index = m.index.map_or(0, |(i, _)| i as u8);
        self.rex(w, reg, index, m.base as u8, force);
    }

    fn modrm_m(&mut self, reg: u8, m: M) {
        let base = m.base as u8;
        let (md, disp8) = match m.disp {
            0 if lo(base) != 5 => (0u8, false),
            d if (-128..128).contains(&d) => (1, true),
            _ => (2, false),
        };
        match m.index {
            None if lo(base) == 4 => {
                self.b(md << 6 | lo(reg) << 3 | 4);
                self.b(0x24);
            }
            None => self.b(md << 6 | lo(reg) << 3 | lo(base)),
            Some((idx, scale)) => {
                let ss = match scale { 1 => 0, 2 => 1, 4 => 2, _ => 3 };
                self.b(md << 6 | lo(reg) << 3 | 4);
                self.b(ss << 6 | lo(idx as u8) << 3 | lo(base));
            }
        }
        match (md, disp8) {
            (1, _) => self.b(m.disp as i8 as u8),
            (2, _) => self.d32(m.disp),
            _ => {}
        }
    }

    fn op_rm(&mut self, w: bool, opcode: &[u8], reg: u8, m: M) {
        self.rex_m(w, reg, m, false);
        self.buf.extend_from_slice(opcode);
        self.modrm_m(reg, m);
    }

    fn op_rr(&mut self, w: bool, opcode: &[u8], reg: u8, rm: u8) {
        self.rex(w, reg, 0, rm, false);
        self.buf.extend_from_slice(opcode);
        self.b(0xC0 | lo(reg) << 3 | lo(rm));
    }

    // ---- moves ----
    pub fn mov_rr(&mut self, w: bool, dst: R, src: R) {
        self.op_rr(w, &[0x89], src as u8, dst as u8);
    }
    pub fn load(&mut self, w: bool, dst: R, m: M) {
        self.op_rm(w, &[0x8B], dst as u8, m);
    }
    pub fn store(&mut self, w: bool, m: M, src: R) {
        self.op_rm(w, &[0x89], src as u8, m);
    }
    pub fn store16(&mut self, m: M, src: R) {
        self.b(0x66);
        self.op_rm(false, &[0x89], src as u8, m);
    }
    pub fn store8(&mut self, m: M, src: R) {
        let force = (4..8).contains(&(src as u8));
        self.rex_m(false, src as u8, m, force);
        self.b(0x88);
        self.modrm_m(src as u8, m);
    }
    /// `movsx r64, byte/word [m]` (size 1 or 2), `movsxd r64, dword [m]` (size 4).
    pub fn load_sx(&mut self, size: u8, dst: R, m: M) {
        match size {
            1 => self.op_rm(true, &[0x0F, 0xBE], dst as u8, m),
            2 => self.op_rm(true, &[0x0F, 0xBF], dst as u8, m),
            _ => self.op_rm(true, &[0x63], dst as u8, m),
        }
    }
    /// `movzx r32, byte/word [m]`.
    pub fn load_zx(&mut self, size: u8, dst: R, m: M) {
        let op = if size == 1 { [0x0F, 0xB6] } else { [0x0F, 0xB7] };
        self.op_rm(false, &op, dst as u8, m);
    }
    /// `movsxd dst, src32`.
    pub fn movsxd(&mut self, dst: R, src: R) {
        self.op_rr(true, &[0x63], dst as u8, src as u8);
    }
    pub fn mov_ri(&mut self, dst: R, imm: i64) {
        if imm >= 0 && imm <= u32::MAX as i64 {
            self.rex(false, 0, 0, dst as u8, false);
            self.b(0xB8 + lo(dst as u8));
            self.d32(imm as u32 as i32);
        } else if imm >= i32::MIN as i64 && imm <= i32::MAX as i64 {
            self.rex(true, 0, 0, dst as u8, false);
            self.b(0xC7);
            self.b(0xC0 | lo(dst as u8));
            self.d32(imm as i32);
        } else {
            self.rex(true, 0, 0, dst as u8, false);
            self.b(0xB8 + lo(dst as u8));
            self.buf.extend_from_slice(&imm.to_le_bytes());
        }
    }
    /// `mov qword [m], imm32` (sign-extended).
    pub fn store_imm(&mut self, m: M, imm: i32) {
        self.op_rm(true, &[0xC7], 0, m);
        self.d32(imm);
    }
    pub fn lea(&mut self, dst: R, m: M) {
        self.op_rm(true, &[0x8D], dst as u8, m);
    }

    // ---- arithmetic ----
    pub fn alu_rr(&mut self, w: bool, op: Alu, dst: R, src: R) {
        self.op_rr(w, &[(op as u8) << 3 | 1], src as u8, dst as u8);
    }
    pub fn alu_rm(&mut self, w: bool, op: Alu, dst: R, m: M) {
        self.op_rm(w, &[(op as u8) << 3 | 3], dst as u8, m);
    }
    pub fn alu_ri(&mut self, w: bool, op: Alu, dst: R, imm: i32) {
        if (-128..128).contains(&imm) {
            self.op_rr(w, &[0x83], op as u8, dst as u8);
            self.b(imm as i8 as u8);
        } else {
            self.op_rr(w, &[0x81], op as u8, dst as u8);
            self.d32(imm);
        }
    }
    pub fn alu_mi(&mut self, w: bool, op: Alu, m: M, imm: i32) {
        if (-128..128).contains(&imm) {
            self.op_rm(w, &[0x83], op as u8, m);
            self.b(imm as i8 as u8);
        } else {
            self.op_rm(w, &[0x81], op as u8, m);
            self.d32(imm);
        }
    }
    /// `cmp byte [m], imm8`.
    pub fn cmp8_mi(&mut self, m: M, imm: u8) {
        self.op_rm(false, &[0x80], 7, m);
        self.b(imm);
    }
    pub fn test_rr(&mut self, w: bool, a: R, b: R) {
        self.op_rr(w, &[0x85], b as u8, a as u8);
    }
    pub fn imul_rr(&mut self, w: bool, dst: R, src: R) {
        self.op_rr(w, &[0x0F, 0xAF], dst as u8, src as u8);
    }
    /// `cdq` / `cqo` then `idiv src` (quotient rax, remainder rdx).
    pub fn idiv(&mut self, w: bool, src: R) {
        if w {
            self.b(0x48);
        }
        self.b(0x99);
        self.op_rr(w, &[0xF7], 7, src as u8);
    }
    pub fn neg(&mut self, w: bool, r: R) {
        self.op_rr(w, &[0xF7], 3, r as u8);
    }
    /// Shift `r` by `cl`.
    pub fn shift_cl(&mut self, w: bool, op: Shift, r: R) {
        self.op_rr(w, &[0xD3], op as u8, r as u8);
    }
    pub fn setcc(&mut self, c: Cond, r: R) {
        self.rex(false, 0, 0, r as u8, (4..8).contains(&(r as u8)));
        self.b(0x0F);
        self.b(0x90 + c as u8);
        self.b(0xC0 | lo(r as u8));
    }
    /// `movzx r32, r8`.
    pub fn movzx8(&mut self, dst: R, src: R) {
        self.rex(false, dst as u8, 0, src as u8, (4..8).contains(&(src as u8)));
        self.buf.extend_from_slice(&[0x0F, 0xB6]);
        self.b(0xC0 | lo(dst as u8) << 3 | lo(src as u8));
    }
    pub fn movsx_rr(&mut self, size: u8, dst: R, src: R) {
        match size {
            1 => {
                self.rex(true, dst as u8, 0, src as u8, true);
                self.buf.extend_from_slice(&[0x0F, 0xBE]);
                self.b(0xC0 | lo(dst as u8) << 3 | lo(src as u8));
            }
            _ => self.op_rr(true, &[0x0F, 0xBF], dst as u8, src as u8),
        }
    }
    pub fn movzx16_rr(&mut self, dst: R, src: R) {
        self.op_rr(false, &[0x0F, 0xB7], dst as u8, src as u8);
    }

    // ---- control flow ----
    pub fn jmp(&mut self, l: Label) {
        self.b(0xE9);
        self.fixups.push((self.pos(), l));
        self.d32(0);
    }
    pub fn jcc(&mut self, c: Cond, l: Label) {
        self.b(0x0F);
        self.b(0x80 + c as u8);
        self.fixups.push((self.pos(), l));
        self.d32(0);
    }
    pub fn call_r(&mut self, r: R) {
        self.op_rr(false, &[0xFF], 2, r as u8);
    }
    pub fn call_m(&mut self, m: M) {
        self.op_rm(false, &[0xFF], 2, m);
    }
    pub fn jmp_r(&mut self, r: R) {
        self.op_rr(false, &[0xFF], 4, r as u8);
    }
    /// Calls an absolute address through `rax`.
    pub fn call_abs(&mut self, addr: usize) {
        self.mov_ri(R::Rax, addr as i64);
        self.call_r(R::Rax);
    }
    pub fn push(&mut self, r: R) {
        self.rex(false, 0, 0, r as u8, false);
        self.b(0x50 + lo(r as u8));
    }
    pub fn pop(&mut self, r: R) {
        self.rex(false, 0, 0, r as u8, false);
        self.b(0x58 + lo(r as u8));
    }
    pub fn ret(&mut self) {
        self.b(0xC3);
    }

    // ---- SSE (f64 when `double`, else f32) ----
    fn sse_rm(&mut self, prefix: u8, w: bool, op: u8, reg: u8, m: M) {
        self.b(prefix);
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
    pub fn fsqrt(&mut self, x: X, src: X) {
        self.sse_rr(0xF2, false, 0x51, x.0, src.0);
    }
    pub fn mfence(&mut self) {
        self.b(0x0F);
        self.b(0xAE);
        self.b(0xF0);
    }
    /// `roundsd x, src, mode` (SSE4.1; mode 0 nearest-even, 1 floor, 2 ceil).
    pub fn round_sd(&mut self, x: X, src: X, mode: u8) {
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
    /// Integer (`w`: 64-bit) to float/double.
    pub fn cvt_i2f(&mut self, double: bool, w: bool, x: X, r: R) {
        self.sse_rr(Self::fp(double), w, 0x2A, x.0, r as u8);
    }
    /// float↔double.
    pub fn cvt_f2f(&mut self, to_double: bool, dst: X, src: X) {
        self.sse_rr(if to_double { 0xF3 } else { 0xF2 }, false, 0x5A, dst.0, src.0);
    }
    /// Truncating float/double to integer (`w`: 64-bit).
    pub fn cvtt_f2i(&mut self, double: bool, w: bool, r: R, x: X) {
        self.sse_rr(Self::fp(double), w, 0x2C, r as u8, x.0);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(f: impl FnOnce(&mut Asm)) -> String {
        let mut a = Asm::default();
        f(&mut a);
        a.finish().iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn encodings_match_the_manual() {
        assert_eq!(hex(|a| a.load(true, R::Rax, mem(R::Rbx, 8))), "48 8b 43 08");
        assert_eq!(hex(|a| a.store(true, mem(R::R13, 16), R::Rcx)), "49 89 4d 10");
        assert_eq!(hex(|a| a.load(true, R::Rax, mem(R::Rsp, 0))), "48 8b 04 24");
        assert_eq!(hex(|a| a.load(true, R::Rax, mem(R::R13, 0))), "49 8b 45 00");
        assert_eq!(hex(|a| a.load(true, R::Rax, mem(R::R12, 24))), "49 8b 44 24 18");
        assert_eq!(hex(|a| a.alu_rr(false, Alu::Add, R::Rax, R::Rcx)), "01 c8");
        assert_eq!(hex(|a| a.lea(R::Rsi, mem(R::R13, 0x40))), "49 8d 75 40");
        assert_eq!(hex(|a| a.load(true, R::Rdx, mem_idx(R::Rax, R::Rcx, 8, 24))), "48 8b 54 c8 18");
        assert_eq!(hex(|a| a.fload(true, X(0), mem(R::R13, 8))), "f2 41 0f 10 45 08");
        assert_eq!(hex(|a| a.store8(mem(R::Rax, 24), R::Rsi)), "40 88 70 18");
        assert_eq!(hex(|a| a.mov_ri(R::R10, -1)), "49 c7 c2 ff ff ff ff");
        assert_eq!(hex(|a| a.push(R::R12)), "41 54");
        assert_eq!(hex(|a| a.setcc(Cond::L, R::Rax)), "0f 9c c0");
        assert_eq!(hex(|a| a.idiv(false, R::Rcx)), "99 f7 f9");
        assert_eq!(hex(|a| a.cvtt_f2i(true, true, R::Rax, X(1))), "f2 48 0f 2c c1");
        assert_eq!(hex(|a| a.call_m(mem(R::Rdx, 16))), "ff 52 10");
        assert_eq!(hex(|a| a.round_sd(X(0), X(1), 1)), "66 0f 3a 0b c1 01");
        assert_eq!(hex(|a| a.round_sd(X(9), X(2), 2)), "66 44 0f 3a 0b ca 02");
    }
}
