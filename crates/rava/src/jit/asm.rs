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
    /// RIP-relative to this label instead (`base`, `index` and `disp` unused).
    pub rip: Option<Label>,
}

/// `[rip + label]`: data placed in the code, such as a constant.
pub fn rip(label: Label) -> M {
    M { base: R::Rax, index: None, disp: 0, rip: Some(label) }
}

pub fn mem(base: R, disp: i32) -> M {
    M { base, index: None, disp, rip: None }
}

/// `[addr]`, an absolute address in the low 2 GiB (`crate::low`): a SIB byte with neither base
/// nor index (rbp as base and rsp as index mark it).
pub fn abs32(addr: usize) -> M {
    debug_assert!(crate::low::is_low(addr));
    M { base: R::Rbp, index: Some((R::Rsp, 1)), disp: addr as i32, rip: None }
}

pub fn mem_idx(base: R, index: R, scale: u8, disp: i32) -> M {
    M { base, index: Some((index, scale)), disp, rip: None }
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
    /// Branch peepholes at `bind` (see `enable_peephole`).
    peephole: bool,
    /// Where each `jmp` / `jcc` emitted since the last bind starts (with its fixup index).
    branches: Vec<(u32, usize)>,
    /// The position labels were last bound at, and those labels.
    bound_at: u32,
    bound: Vec<u32>,
    /// Relative calls to absolute addresses: (displacement position, target), resolved when the
    /// code is installed (`code_cache::install`).
    relocs: Vec<(u32, usize)>,
}

/// Finished machine code and the call displacements its installation resolves.
pub struct Code {
    pub bytes: Vec<u8>,
    pub relocs: Vec<(u32, usize)>,
}

impl std::ops::Deref for Code {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.bytes
    }
}

mod relax;
mod sse;

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

    /// Lets `bind` drop a `jmp` to the label being bound and turn `jcc L; jmp M; L:` into
    /// `j!cc M`. Code that records positions itself (`pos`) must not enable it.
    pub fn enable_peephole(&mut self) {
        self.peephole = true;
    }

    pub fn bind(&mut self, l: Label) {
        if self.peephole {
            self.branch_peephole(l);
        }
        let at = self.pos();
        if at != self.bound_at {
            self.bound.clear();
            self.bound_at = at;
        }
        self.bound.push(l.0);
        self.labels[l.0 as usize] = Some(at);
    }

    pub fn label_pos(&self, l: Label) -> Option<u32> {
        self.labels[l.0 as usize]
    }

    /// Resolves every label reference; returns the finished code.
    /// Emits 8 bytes of data at a bound label (a constant for `rip` operands).
    pub fn data8(&mut self, l: Label, bits: u64) {
        while self.buf.len() % 8 != 0 {
            self.b(0xCC);
        }
        self.bind(l);
        self.buf.extend_from_slice(&bits.to_le_bytes());
    }

    /// `finish`, with the final position of label `l` (bound).
    pub fn finish_at(mut self, l: Label) -> (Code, u32) {
        self.resolve();
        let at = self.labels[l.0 as usize].expect("bound label");
        (Code { bytes: self.buf, relocs: self.relocs }, at)
    }

    /// `finish_at`, also with the final positions of `more` (bound labels).
    pub fn finish_labels(mut self, l: Label, more: &[Label]) -> (Code, u32, Vec<u32>) {
        self.resolve();
        let pos = |l: &Label| self.labels[l.0 as usize].expect("bound label");
        let (at, rest) = (pos(&l), more.iter().map(pos).collect());
        (Code { bytes: self.buf, relocs: self.relocs }, at, rest)
    }

    pub fn finish(mut self) -> Code {
        self.resolve();
        Code { bytes: self.buf, relocs: self.relocs }
    }

    fn resolve(&mut self) {
        if self.peephole {
            self.relax();
        }
        for (at, l) in std::mem::take(&mut self.fixups) {
            let target = self.labels[l.0 as usize].expect("unbound label") as i64;
            let rel = (target - (at as i64 + 4)) as i32;
            self.buf[at as usize..at as usize + 4].copy_from_slice(&rel.to_le_bytes());
        }
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
        if let Some(l) = m.rip {
            // mod 00, r/m 101: disp32 from the end of the instruction (the displacement is last
            // in every instruction using this form here).
            self.b(lo(reg) << 3 | 5);
            self.fixups.push((self.pos(), l));
            self.d32(0);
            return;
        }
        if let (R::Rbp, Some((R::Rsp, _))) = (m.base, m.index) {
            // mod 00, r/m 100, SIB 00 100 101: disp32 alone.
            self.b(lo(reg) << 3 | 4);
            self.b(0x25);
            self.d32(m.disp);
            return;
        }
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
    /// `popcnt` / `lzcnt` / `tzcnt` (`F3 0F B8/BD/BC`), 64-bit with `w`.
    pub fn bit_count(&mut self, opcode: u8, w: bool, dst: R, src: R) {
        self.b(0xF3);
        self.op_rr(w, &[0x0F, opcode], dst as u8, src as u8);
    }
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
    /// Stores the 64-bit constant `value` to `m`: one `mov qword [m], imm32` when it fits (a low
    /// address, `crate::low`), else through `scratch`.
    pub fn store_const(&mut self, m: M, value: i64, scratch: R) {
        match i32::try_from(value) {
            Ok(imm) => self.store_imm(m, imm),
            Err(_) => {
                self.mov_ri(scratch, value);
                self.store(true, m, scratch);
            }
        }
    }
    /// `mov qword [m], imm32` (sign-extended).
    pub fn store_imm(&mut self, m: M, imm: i32) {
        self.op_rm(true, &[0xC7], 0, m);
        self.d32(imm);
    }
    /// `lea dst, [base + disp32]` with a displacement patched later; returns its position.
    pub fn lea_patchable(&mut self, dst: R, base: R) -> u32 {
        self.lea(dst, mem(base, 0x7fff_0000));
        self.pos() - 4
    }
    pub fn patch_i32(&mut self, at: u32, v: i32) {
        self.buf[at as usize..at as usize + 4].copy_from_slice(&v.to_le_bytes());
    }
    /// `mov dword [m], imm32`.
    pub fn store_imm32(&mut self, m: M, imm: i32) {
        self.op_rm(false, &[0xC7], 0, m);
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
    /// `mov byte [m], imm8`.
    pub fn store8_imm(&mut self, m: M, imm: u8) {
        self.op_rm(false, &[0xC6], 0, m);
        self.b(imm);
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
    /// `imul dst, [m]`.
    pub fn imul_rm(&mut self, w: bool, dst: R, m: M) {
        self.op_rm(w, &[0x0F, 0xAF], dst as u8, m);
    }
    /// `imul dst, src, imm32`.
    pub fn imul_rri(&mut self, w: bool, dst: R, src: R, imm: i32) {
        self.op_rr(w, &[0x69], dst as u8, src as u8);
        self.d32(imm);
    }
    /// One-operand `imul src`: the signed double-width product of rax and src in rdx:rax.
    pub fn imul_wide(&mut self, w: bool, src: R) {
        self.op_rr(w, &[0xF7], 5, src as u8);
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
    /// `cmovcc dst, src`.
    pub fn cmov(&mut self, w: bool, c: Cond, dst: R, src: R) {
        self.op_rr(w, &[0x0F, 0x40 + c as u8], dst as u8, src as u8);
    }
    /// Shift `r` by an immediate count.
    pub fn shift_ri(&mut self, w: bool, op: Shift, r: R, count: u8) {
        self.op_rr(w, &[0xC1], op as u8, r as u8);
        self.b(count);
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
        self.branches.push((self.pos(), self.fixups.len()));
        self.b(0xE9);
        self.fixups.push((self.pos(), l));
        self.d32(0);
    }
    pub fn jcc(&mut self, c: Cond, l: Label) {
        self.branches.push((self.pos(), self.fixups.len()));
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
    /// `call rel32` to a label in this code.
    pub fn call_label(&mut self, l: Label) {
        self.b(0xE8);
        self.fixups.push((self.pos(), l));
        self.d32(0);
    }
    /// Calls an absolute address: `call rel32` when the code cache is within reach of it
    /// (resolved at installation), else through `rax`.
    pub fn call_abs(&mut self, addr: usize) {
        if super::code_cache::near() {
            self.b(0xE8);
            self.relocs.push((self.pos(), addr));
            self.d32(0);
        } else {
            self.mov_ri(R::Rax, addr as i64);
            self.call_r(R::Rax);
        }
    }
    /// Jumps to an absolute address: `jmp rel32` when the code cache is within reach of it,
    /// else through r11.
    pub fn jmp_abs(&mut self, addr: usize) {
        if super::code_cache::near() {
            self.b(0xE9);
            self.relocs.push((self.pos(), addr));
            self.d32(0);
        } else {
            self.mov_ri(R::R11, addr as i64);
            self.jmp_r(R::R11);
        }
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
        // xorps first: roundsd writes only the low half of its destination.
        assert_eq!(hex(|a| a.round_sd(X(0), X(1), 1)), "0f 57 c0 66 0f 3a 0b c1 01");
        assert_eq!(hex(|a| a.shift_ri(false, Shift::Shl, R::R9, 3)), "41 c1 e1 03");
        assert_eq!(hex(|a| a.imul_wide(true, R::Rcx)), "48 f7 e9");
        assert_eq!(hex(|a| a.shift_ri(true, Shift::Shr, R::Rsi, 32)), "48 c1 ee 20");
        assert_eq!(hex(|a| a.farith_rr(true, 0x59, X(0), X(1))), "f2 0f 59 c1");
        assert_eq!(hex(|a| a.push(R::Rbx)), "53");
        assert_eq!(hex(|a| a.cmov(true, Cond::L, R::Rax, R::Rcx)), "48 0f 4c c1");
        assert_eq!(hex(|a| a.load(true, R::Rbp, mem(R::R13, 8))), "49 8b 6d 08");
        assert_eq!(hex(|a| a.round_sd(X(9), X(2), 2)), "45 0f 57 c9 66 44 0f 3a 0b ca 02");
        assert_eq!(hex(|a| a.mov_xx(X(10), X(3))), "44 0f 28 d3");
        assert_eq!(hex(|a| a.fxor(true, X(2), X(1))), "66 0f 57 d1");
        assert_eq!(hex(|a| a.fstore(true, mem(R::Rsp, 16), X(12))), "f2 44 0f 11 64 24 10");
    }
}
