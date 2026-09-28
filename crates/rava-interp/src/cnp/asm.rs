//! The assembler buffer and the compiled object it produces.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::cnp::*;

/// x86-64 inline code emitter. Registers: rbx = stack index, rsi = stack base,
/// rdi = locals base. Runtime callbacks follow the active platform ABI and the
/// emitter preserves its frame registers across each call.
pub(crate) struct Asm {
    code: Vec<u8>,
}
impl Asm {
    fn new() -> Self {
        Asm { code: Vec::with_capacity(256) }
    }
    fn b(&mut self, bytes: &[u8]) {
        self.code.extend_from_slice(bytes);
    }
    fn i32(&mut self, v: i32) {
        self.code.extend_from_slice(&v.to_le_bytes());
    }
    fn len(&self) -> usize {
        self.code.len()
    }

    fn prologue(&mut self) {
        self.b(&[0x53, 0x56, 0x57]); // preserve rbx/rsi/rdi; rsp is now 16-aligned
        self.b(&[0x48, 0x83, 0xEC, 0x20]); // Windows shadow space; harmless on SysV
        #[cfg(windows)]
        {
            self.b(&[0x48, 0x8B, 0x31]); // mov rsi, [rcx]
            self.b(&[0x48, 0x8B, 0x59, 0x08]); // mov rbx, [rcx+8]
            self.b(&[0x48, 0x8B, 0x79, 0x10]); // mov rdi, [rcx+16]
        }
        #[cfg(target_os = "linux")]
        {
            self.b(&[0x48, 0x8B, 0x37]); // mov rsi, [rdi]
            self.b(&[0x48, 0x8B, 0x5F, 0x08]); // mov rbx, [rdi+8]
            self.b(&[0x48, 0x8B, 0x7F, 0x10]); // mov rdi, [rdi+16]
        }
    }
    /// Pop the stack top into rax and return (rax preserved by the epilogue).
    fn epilogue_ireturn(&mut self) {
        self.b(&[0x48, 0xFF, 0xCB]); // dec rbx
        self.b(&[0x48, 0x8B, 0x04, 0xDE]); // mov rax, [rsi + rbx*8]
        self.b(&[0x48, 0x83, 0xC4, 0x20]); // add rsp, 32
        self.b(&[0x5F, 0x5E, 0x5B]); // pop rdi, pop rsi, pop rbx
        self.b(&[0xC3]); // ret
    }
    /// Pop the receiver cell, call the platform-ABI `rava_getfield` helper and
    /// replace the stack top with its returned cell. The callback may clobber
    /// caller-saved registers; the SysV arm preserves the stack/local bases.
    #[cfg(windows)]
    fn getfield(&mut self, field_id: i64, is_ref: bool, addr: usize) {
        self.b(&[0x48, 0x8B, 0x4C, 0xDE, 0xF8]); // mov rcx, [rsi+rbx*8-8]  (receiver)
        self.b(&[0x48, 0xBA]); // mov rdx, imm64  (field_id)
        self.code.extend_from_slice(&(field_id as u64).to_le_bytes());
        self.b(&[0x49, 0xB8]); // mov r8, imm64   (is_ref)
        self.code.extend_from_slice(&(is_ref as u64).to_le_bytes());
        self.b(&[0x48, 0xB8]); // mov rax, imm64  (rava_getfield)
        self.code.extend_from_slice(&(addr as u64).to_le_bytes());
        self.b(&[0xFF, 0xD0]); // call rax
        self.b(&[0x48, 0x89, 0x44, 0xDE, 0xF8]); // mov [rsi+rbx*8-8], rax  (replace top)
    }
    #[cfg(target_os = "linux")]
    fn getfield(&mut self, field_id: i64, is_ref: bool, addr: usize) {
        self.b(&[0x56, 0x57]); // push rsi; push rdi (preserve stack/local bases)
        self.b(&[0x48, 0x8B, 0x7C, 0xDE, 0xF8]); // mov rdi, [rsi+rbx*8-8]
        self.b(&[0x48, 0xBE]); // mov rsi, imm64 (field id)
        self.code.extend_from_slice(&(field_id as u64).to_le_bytes());
        self.b(&[0x48, 0xBA]); // mov rdx, imm64 (is_ref)
        self.code.extend_from_slice(&(is_ref as u64).to_le_bytes());
        self.b(&[0x48, 0xB8]); // mov rax, callback
        self.code.extend_from_slice(&(addr as u64).to_le_bytes());
        self.b(&[0xFF, 0xD0]); // call rax
        self.b(&[0x5F, 0x5E]); // pop rdi; pop rsi
        self.b(&[0x48, 0x89, 0x44, 0xDE, 0xF8]); // replace receiver with result
    }

    // ---- stack-slot helpers on [rsi + rbx*8 + disp8] (SIB=0xDE) ----
    /// mov qword [rsi+rbx*8], imm32 (sign-extended)  +  inc rbx
    fn push_imm(&mut self, v: i32) {
        self.b(&[0x48, 0xC7, 0x04, 0xDE]); // mov qword [rsi+rbx*8], imm32
        self.i32(v);
        self.b(&[0x48, 0xFF, 0xC3]); // inc rbx
    }
    /// rax = locals[idx]  (mov rax,[rdi+disp32]) ; push rax ; inc rbx
    fn push_local(&mut self, idx: u32) {
        self.b(&[0x48, 0x8B, 0x87]); // mov rax, [rdi+disp32]
        self.i32((idx * 8) as i32);
        self.b(&[0x48, 0x89, 0x04, 0xDE]); // mov [rsi+rbx*8], rax
        self.b(&[0x48, 0xFF, 0xC3]); // inc rbx
    }
    /// dec rbx ; rax = stack top ; locals[idx] = rax
    fn store_local(&mut self, idx: u32) {
        self.b(&[0x48, 0xFF, 0xCB]); // dec rbx
        self.b(&[0x48, 0x8B, 0x04, 0xDE]); // mov rax, [rsi+rbx*8]
        self.b(&[0x48, 0x89, 0x87]); // mov [rdi+disp32], rax
        self.i32((idx * 8) as i32);
    }
    /// locals[idx] += c  (32-bit, stored sign-extended)
    fn iinc(&mut self, idx: u32, c: i32) {
        self.b(&[0x8B, 0x87]); // mov eax, [rdi+disp32]
        self.i32((idx * 8) as i32);
        self.b(&[0x05]); // add eax, imm32
        self.i32(c);
        self.b(&[0x48, 0x63, 0xC0]); // movsxd rax, eax
        self.b(&[0x48, 0x89, 0x87]); // mov [rdi+disp32], rax
        self.i32((idx * 8) as i32);
    }
    fn pop(&mut self) {
        self.b(&[0x48, 0xFF, 0xCB]); // dec rbx
    }
    fn dup(&mut self) {
        self.b(&[0x48, 0x8B, 0x44, 0xDE, 0xF8]); // mov rax, [rsi+rbx*8-8]
        self.b(&[0x48, 0x89, 0x04, 0xDE]); // mov [rsi+rbx*8], rax
        self.b(&[0x48, 0xFF, 0xC3]); // inc rbx
    }
    /// Load the two int operands (a = second, b = top) into eax/e-mem, apply a 32-bit
    /// ALU op producing eax, sign-extend, pop one, store at the new top.
    /// `alu` emits `<op> eax, [rsi+rbx*8-8]` (b), assuming eax already = a.
    fn binop(&mut self, alu: &mut dyn FnMut(&mut Asm)) {
        self.b(&[0x8B, 0x44, 0xDE, 0xF0]); // mov eax, [rsi+rbx*8-16]  (a)
        alu(self); // eax = a <op> b
        self.b(&[0x48, 0x63, 0xC0]); // movsxd rax, eax
        self.b(&[0x48, 0xFF, 0xCB]); // dec rbx
        self.b(&[0x48, 0x89, 0x44, 0xDE, 0xF8]); // mov [rsi+rbx*8-8], rax
    }
    fn ineg(&mut self) {
        self.b(&[0x8B, 0x44, 0xDE, 0xF8]); // mov eax, [rsi+rbx*8-8]
        self.b(&[0xF7, 0xD8]); // neg eax
        self.b(&[0x48, 0x63, 0xC0]); // movsxd rax, eax
        self.b(&[0x48, 0x89, 0x44, 0xDE, 0xF8]); // mov [rsi+rbx*8-8], rax
    }
    /// Shift: a shifted by (b & 31). `op` = 0xE0 shl / 0xF8 sar / 0xE8 shr (ModRM /n).
    fn shift(&mut self, op: u8) {
        self.b(&[0x8B, 0x4C, 0xDE, 0xF8]); // mov ecx, [rsi+rbx*8-8]  (b → cl)
        self.b(&[0x8B, 0x44, 0xDE, 0xF0]); // mov eax, [rsi+rbx*8-16] (a)
        self.b(&[0xD3, op]); // shl/sar/shr eax, cl
        self.b(&[0x48, 0x63, 0xC0]); // movsxd rax, eax
        self.b(&[0x48, 0xFF, 0xCB]); // dec rbx
        self.b(&[0x48, 0x89, 0x44, 0xDE, 0xF8]); // mov [rsi+rbx*8-8], rax
    }
    /// if_icmp<cond>: cmp a,b ; pop both ; Jcc target.
    fn if_icmp(&mut self, cc: Cond, target_pc: usize, fx: &mut Vec<Fixup>) {
        self.b(&[0x8B, 0x44, 0xDE, 0xF0]); // mov eax, [rsi+rbx*8-16] (a)
        self.b(&[0x3B, 0x44, 0xDE, 0xF8]); // cmp eax, [rsi+rbx*8-8]  (b)
        self.b(&[0x48, 0x8D, 0x5B, 0xFE]); // lea rbx, [rbx-2]  (pop 2, PRESERVES flags)
        self.jcc(cc, target_pc, fx);
    }
    /// if<cond> vs 0: pop a ; test a ; Jcc target.
    fn if_zero(&mut self, cc: Cond, target_pc: usize, fx: &mut Vec<Fixup>) {
        self.b(&[0x8B, 0x44, 0xDE, 0xF8]); // mov eax, [rsi+rbx*8-8]
        self.b(&[0x48, 0xFF, 0xCB]); // dec rbx
        self.b(&[0x85, 0xC0]); // test eax, eax
        self.jcc(cc, target_pc, fx);
    }
    fn jcc(&mut self, cc: Cond, target_pc: usize, fx: &mut Vec<Fixup>) {
        self.b(&[0x0F, cc.jcc()]);
        let at = self.code.len();
        self.i32(0);
        fx.push(Fixup { at, end: self.code.len(), target_pc });
    }
    fn goto(&mut self, target_pc: usize, fx: &mut Vec<Fixup>) {
        self.b(&[0xE9]);
        let at = self.code.len();
        self.i32(0);
        fx.push(Fixup { at, end: self.code.len(), target_pc });
    }
}

/// Position-independent compiled method: pure code (cnp emits no relocs). Serialises
/// for the on-disk AOT cache; `link` places it in an executable page. (The relocation
/// list is retained in the format for the C2-quality tier, whose code carries Abs8
/// relocs to rava_* callbacks — see the C2 AOT tests — but is empty for cnp inline.)
pub struct CompiledObj {
    code: Vec<u8>,
    /// Abs8 relocations: `(code offset, symbol index, addend)`. The linker writes an
    /// 8-byte absolute `resolve(symbol) + addend` at `offset`. Empty for the cnp inline
    /// tier and the reloc-free numeric AOT tier; non-empty for compiled methods that
    /// call rava_* runtime callbacks (the C2-quality tier). The symbol index maps to a
    /// fixed callback table filled per process at link time.
    relocs: Vec<(u32, u32, i64)>,
    /// Per-method symbolic constant descriptors (see `ConstDesc`). Slot `i` here is the
    /// value the code loads from `consts[i]`; the linker resolves each to a per-process
    /// i64. Empty for the reloc-free numeric tier.
    consts: Vec<ConstDesc>,
}
impl CompiledObj {
    /// Wrap a raw relocation-free code buffer (the Cranelift numeric AOT tier — see
    /// `aot_clif`). Same on-disk format as the cnp tier, so it shares persist/link.
    pub fn from_code(code: Vec<u8>) -> CompiledObj {
        CompiledObj { code, relocs: Vec::new(), consts: Vec::new() }
    }
    /// Wrap relocatable code + its Abs8 relocation list (the callback-carrying tier).
    pub fn from_code_relocs(code: Vec<u8>, relocs: Vec<(u32, u32, i64)>) -> CompiledObj {
        CompiledObj { code, relocs, consts: Vec::new() }
    }
    /// Wrap code + relocs + symbolic constant table (the full callback-carrying tier).
    pub fn from_full(code: Vec<u8>, relocs: Vec<(u32, u32, i64)>, consts: Vec<ConstDesc>) -> CompiledObj {
        CompiledObj { code, relocs, consts }
    }
    pub fn relocs(&self) -> &[(u32, u32, i64)] {
        &self.relocs
    }
    pub fn consts(&self) -> &[ConstDesc] {
        &self.consts
    }
    /// The raw code buffer (for re-wrapping with a different reloc/const set — the AOT
    /// translator attaches descriptors to a compiled body).
    pub fn code_vec(&self) -> Vec<u8> {
        self.code.clone()
    }
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(16 + self.code.len() + self.relocs.len() * 16);
        out.extend_from_slice(&(self.code.len() as u64).to_le_bytes());
        out.extend_from_slice(&self.code);
        out.extend_from_slice(&(self.relocs.len() as u64).to_le_bytes());
        for &(off, sym, add) in &self.relocs {
            out.extend_from_slice(&off.to_le_bytes());
            out.extend_from_slice(&sym.to_le_bytes());
            out.extend_from_slice(&add.to_le_bytes());
        }
        // Constant descriptor section: [count u64][ (tag u8, fields…) each ]. Strings
        // are length-prefixed (u32 + utf8). Tags: 0 FieldId, 1 ClassPtr, 2 StaticSlot,
        // 3 StrLit, 4 Imm, 5 AotLeaf.
        out.extend_from_slice(&(self.consts.len() as u64).to_le_bytes());
        let put_str = |out: &mut Vec<u8>, s: &str| {
            out.extend_from_slice(&(s.len() as u32).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        };
        for c in &self.consts {
            match c {
                ConstDesc::FieldId(o, n) => { out.push(0); put_str(&mut out, o); put_str(&mut out, n); }
                ConstDesc::ClassPtr(c) => { out.push(1); put_str(&mut out, c); }
                ConstDesc::StaticSlot(o, n) => { out.push(2); put_str(&mut out, o); put_str(&mut out, n); }
                ConstDesc::StrLit(s) => { out.push(3); put_str(&mut out, s); }
                ConstDesc::AotLeaf(o, n, d) => { out.push(5); put_str(&mut out, o); put_str(&mut out, n); put_str(&mut out, d); }
                ConstDesc::Imm(v) => { out.push(4); out.extend_from_slice(&v.to_le_bytes()); }
            }
        }
        out
    }
    pub fn from_bytes(b: &[u8]) -> Option<CompiledObj> {
        let clen = u64::from_le_bytes(b.get(0..8)?.try_into().ok()?) as usize;
        let code = b.get(8..8 + clen)?.to_vec();
        let mut p = 8 + clen;
        // Reloc section is ALWAYS written by to_bytes (count = 0 for reloc-free code),
        // so a missing/truncated count field means a corrupt blob → reject.
        let n = u64::from_le_bytes(b.get(p..p + 8)?.try_into().ok()?) as usize;
        p += 8;
        let mut relocs = Vec::with_capacity(n);
        for _ in 0..n {
            let off = u32::from_le_bytes(b.get(p..p + 4)?.try_into().ok()?);
            let sym = u32::from_le_bytes(b.get(p + 4..p + 8)?.try_into().ok()?);
            let add = i64::from_le_bytes(b.get(p + 8..p + 16)?.try_into().ok()?);
            relocs.push((off, sym, add));
            p += 16;
        }
        // Constant descriptor section (always written since AOT_FORMAT 3).
        let cn = u64::from_le_bytes(b.get(p..p + 8)?.try_into().ok()?) as usize;
        p += 8;
        let get_str = |b: &[u8], p: &mut usize| -> Option<String> {
            let l = u32::from_le_bytes(b.get(*p..*p + 4)?.try_into().ok()?) as usize;
            *p += 4;
            let s = String::from_utf8(b.get(*p..*p + l)?.to_vec()).ok()?;
            *p += l;
            Some(s)
        };
        let mut consts = Vec::with_capacity(cn);
        for _ in 0..cn {
            let tag = *b.get(p)?;
            p += 1;
            let c = match tag {
                0 => ConstDesc::FieldId(get_str(b, &mut p)?, get_str(b, &mut p)?),
                1 => ConstDesc::ClassPtr(get_str(b, &mut p)?),
                2 => ConstDesc::StaticSlot(get_str(b, &mut p)?, get_str(b, &mut p)?),
                3 => ConstDesc::StrLit(get_str(b, &mut p)?),
                4 => {
                    let v = i64::from_le_bytes(b.get(p..p + 8)?.try_into().ok()?);
                    p += 8;
                    ConstDesc::Imm(v)
                }
                5 => ConstDesc::AotLeaf(get_str(b, &mut p)?, get_str(b, &mut p)?, get_str(b, &mut p)?),
                _ => return None,
            };
            consts.push(c);
        }
        Some(CompiledObj { code, relocs, consts })
    }
    /// Link reloc-free code (cnp / numeric tiers). Panics logically only if the object
    /// actually has relocs — use `link_with` then.
    pub fn link(&self) -> Option<CompiledFn> {
        self.link_with(&|_| 0)
    }
    /// Link, patching each Abs8 reloc with `resolve(symbol) + addend`. `resolve` maps a
    /// symbol index (baked at compile time) to the callback's address IN THIS PROCESS —
    /// the mechanism that makes AOT'd callback-carrying code valid across runs.
    pub fn link_with(&self, resolve: &dyn Fn(u32) -> usize) -> Option<CompiledFn> {
        let len = self.code.len();
        let p = alloc_exec(&self.code)?;
        for &(off, sym, add) in &self.relocs {
            let target = (resolve(sym) as i64).wrapping_add(add) as u64;
            unsafe {
                std::ptr::copy_nonoverlapping(
                    target.to_le_bytes().as_ptr(),
                    p.add(off as usize),
                    8,
                );
            }
        }
        Some(CompiledFn { code: p, len })
    }
    /// Full link: patch call relocs (via `resolve_sym`) AND build the per-method
    /// constant table by resolving each `ConstDesc` in THIS process (via `resolve_const`
    /// — field-id interning, class load, string intern). Returns the linked fn + the
    /// owned constant array the caller passes to `run_const`/the compiled ABI. `None` if
    /// any descriptor fails to resolve (the method then stays interpreted/JIT'd).
    pub fn link_full(
        &self,
        resolve_sym: &dyn Fn(u32) -> usize,
        resolve_const: &mut dyn FnMut(&ConstDesc) -> Option<i64>,
    ) -> Option<(CompiledFn, Vec<i64>)> {
        let mut consts = Vec::with_capacity(self.consts.len());
        for d in &self.consts {
            consts.push(resolve_const(d)?);
        }
        let f = self.link_with(resolve_sym)?;
        Some((f, consts))
    }
}

/// Compile with a context (getfield support). See `compile`.
pub fn compile_ctx(bc: &[u8], ctx: &CnpCtx) -> Option<CompiledObj> {
    let mut a = Asm::new();
    let mut off: HashMap<usize, usize> = HashMap::new();
    let mut fx: Vec<Fixup> = Vec::new();
    a.prologue();
    let mut pc = 0usize;
    let mut terminated = false;
    while pc < bc.len() {
        off.insert(pc, a.len());
        let op = bc[pc];
        terminated = matches!(op, 0xac | 0xb0 | 0xa7);
        match op {
            0x02..=0x08 => (a.push_imm(op as i32 - 3), pc += 1).1,
            0x10 => (a.push_imm(*bc.get(pc + 1)? as i8 as i32), pc += 2).1,
            0x11 => (a.push_imm(s16(bc, pc + 1)?), pc += 3).1,
            0x1a..=0x1d => (a.push_local(op as u32 - 0x1a), pc += 1).1,
            0x15 => (a.push_local(*bc.get(pc + 1)? as u32), pc += 2).1,
            // aload_<0..3> / aload — push a reference local (same slot machine code as
            // iload; the value is a cell, tracked by the caller's marshalling).
            0x2a..=0x2d => (a.push_local(op as u32 - 0x2a), pc += 1).1,
            0x19 => (a.push_local(*bc.get(pc + 1)? as u32), pc += 2).1,
            0xb4 => {
                let idx = ((*bc.get(pc + 1)? as u16) << 8) | *bc.get(pc + 2)? as u16;
                let &(fid, is_ref) = ctx.fields.get(&idx)?;
                a.getfield(fid, is_ref, ctx.getfield_addr);
                pc += 3;
            }
            0x3b..=0x3e => (a.store_local(op as u32 - 0x3b), pc += 1).1,
            0x36 => (a.store_local(*bc.get(pc + 1)? as u32), pc += 2).1,
            0x84 => {
                a.iinc(*bc.get(pc + 1)? as u32, *bc.get(pc + 2)? as i8 as i32);
                pc += 3;
            }
            0x57 => (a.pop(), pc += 1).1,
            0x59 => (a.dup(), pc += 1).1,
            0x60 => (a.binop(&mut |a| a.b(&[0x03, 0x44, 0xDE, 0xF8])), pc += 1).1, // add eax,[b]
            0x64 => (a.binop(&mut |a| a.b(&[0x2B, 0x44, 0xDE, 0xF8])), pc += 1).1, // sub eax,[b]
            0x68 => (a.binop(&mut |a| a.b(&[0x0F, 0xAF, 0x44, 0xDE, 0xF8])), pc += 1).1, // imul eax,[b]
            0x7e => (a.binop(&mut |a| a.b(&[0x23, 0x44, 0xDE, 0xF8])), pc += 1).1, // and eax,[b]
            0x80 => (a.binop(&mut |a| a.b(&[0x0B, 0x44, 0xDE, 0xF8])), pc += 1).1, // or eax,[b]
            0x82 => (a.binop(&mut |a| a.b(&[0x33, 0x44, 0xDE, 0xF8])), pc += 1).1, // xor eax,[b]
            0x74 => (a.ineg(), pc += 1).1,
            0x78 => (a.shift(0xE0), pc += 1).1, // shl
            0x7a => (a.shift(0xF8), pc += 1).1, // sar (ishr)
            0x7c => (a.shift(0xE8), pc += 1).1, // shr (iushr)
            0xa7 => {
                a.goto((pc as i32 + s16(bc, pc + 1)?) as usize, &mut fx);
                pc += 3;
            }
            0x9f..=0xa4 => {
                let cc = match op {
                    0x9f => Cond::Eq,
                    0xa0 => Cond::Ne,
                    0xa1 => Cond::Lt,
                    0xa2 => Cond::Ge,
                    0xa3 => Cond::Gt,
                    _ => Cond::Le,
                };
                a.if_icmp(cc, (pc as i32 + s16(bc, pc + 1)?) as usize, &mut fx);
                pc += 3;
            }
            0x99..=0x9e => {
                let cc = match op {
                    0x99 => Cond::Eq,
                    0x9a => Cond::Ne,
                    0x9b => Cond::Lt,
                    0x9c => Cond::Ge,
                    0x9d => Cond::Gt,
                    _ => Cond::Le,
                };
                a.if_zero(cc, (pc as i32 + s16(bc, pc + 1)?) as usize, &mut fx);
                pc += 3;
            }
            0xac | 0xb0 => {
                // ireturn / areturn — same machine code (return the top i64); the caller
                // decodes int vs reference per the method descriptor.
                a.epilogue_ireturn();
                pc += 1;
            }
            _ => return None,
        }
    }
    if !terminated {
        return None;
    }
    for f in &fx {
        let tgt = *off.get(&f.target_pc)?;
        let rel = i32::try_from(tgt as i64 - f.end as i64).ok()?;
        a.code[f.at..f.at + 4].copy_from_slice(&rel.to_le_bytes());
    }
    Some(CompiledObj { code: a.code, relocs: Vec::new(), consts: Vec::new() })
}
