//! Emitting one method as LLVM IR.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::llvm_aot::*;

/// Emit ONE function `@{fn_name}` (the AOT ABI `i64(ptr %locals, ptr %consts)`) into `shared_out`,
/// pushing its double/threshold constants onto the SHARED `consts` at growing ABSOLUTE offsets —
/// the whole module shares one %consts, and the array-ABI slots [0]/[1] are reserved module-wide
/// by the caller. SSA value + block names are LLVM-function-local, so `@f0`/`@f1` never collide.
/// This is the per-function unit the whole-tree driver calls for the root + each eligible callee.
/// (invokestatic 0xb8 is in the subset but hits `_ => None` below → still bails; the invoke
/// emitter is inc 6.3.)
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_llvm_fn(
    shared_out: &mut String,
    consts: &mut Vec<i64>,
    fn_name: &str,
    internal: bool,
    bc: &[u8],
    max_locals: u32,
    callee_names: &std::collections::HashMap<u16, LlvmCallee>,
    ldc_of: &dyn Fn(u16) -> Option<crate::aot_clif::LdcConst>,
) -> Option<()> {
    {
        let mut pc = 0;
        while pc < bc.len() {
            if !llvm_subset_op(bc[pc]) {
                return None;
            }
            pc += crate::jit::insn_len_at(bc, pc)?;
        }
    }
    let leaders = leaders(bc)?;
    let leader_set: std::collections::HashMap<usize, usize> =
        leaders.iter().enumerate().map(|(i, &p)| (p, i)).collect();
    let s16 = |pc: usize| ((bc[pc + 1] as i16) << 8 | bc[pc + 2] as i16) as i32;

    // Double constants ride the runtime `%consts` array (an i64 buffer resolved at link),
    // NOT inline literals — clang would spill an inline double into a `.rdata` constant pool
    // referenced RIP-relative, and our `.text`-only extraction can't bind that relocation.
    // Ints/longs stay inline (materialised as immediates — no data section). `consts` is the
    // SHARED module buffer; `consts.len()` at push time is this constant's absolute slot.
    let mut out = String::new();
    // A callee is `internal alwaysinline` (#0) so clang inlines it into the root and DCEs it →
    // the root is the ONLY surviving .text function, hence at offset 0 (the aot_run entry).
    if internal {
        out.push_str(&format!("define internal i64 @{fn_name}(ptr %locals, ptr %consts) #0 {{\nentry:\n"));
    } else {
        out.push_str(&format!("define i64 @{fn_name}(ptr %locals, ptr %consts) {{\nentry:\n"));
    }
    for i in 0..max_locals {
        out.push_str(&format!("  %l{i} = alloca i64\n"));
        out.push_str(&format!("  %in{i}p = getelementptr i64, ptr %locals, i64 {i}\n"));
        out.push_str(&format!("  %in{i} = load i64, ptr %in{i}p\n"));
        out.push_str(&format!("  store i64 %in{i}, ptr %l{i}\n"));
    }
    out.push_str(&format!("  br label %b{}\n", leaders[0]));

    let mut tmp = 0usize;
    let mut asite = 0usize; // unique suffix per array-access site (labels + SSA names)
    let mut fresh = |out: &mut String, expr: &str| -> String {
        let n = format!("%t{tmp}");
        tmp += 1;
        out.push_str(&format!("  {n} = {expr}\n"));
        n
    };
    // A local read as an int: load the i64 cell, truncate to i32.
    let iload_local =
        |out: &mut String, fresh: &mut dyn FnMut(&mut String, &str) -> String, idx: u8| -> String {
            let raw = fresh(out, &format!("load i64, ptr %l{idx}"));
            fresh(out, &format!("trunc i64 {raw} to i32"))
        };

    for (li, &start) in leaders.iter().enumerate() {
        let end = leaders.get(li + 1).copied().unwrap_or(bc.len());
        out.push_str(&format!("b{start}:\n"));
        // (ssa value or literal, is_long).
        let mut stack: Vec<(String, bool)> = Vec::new();
        let mut pc = start;
        let mut terminated = false;
        while pc < end {
            let op = bc[pc];
            let len = crate::jit::insn_len_at(bc, pc)?;
            match op {
                // ---- int constants / loads / stores ----
                0x02..=0x08 => stack.push((format!("{}", op as i32 - 0x03), false)),
                0x10 => stack.push((format!("{}", bc[pc + 1] as i8 as i32), false)),
                0x11 => stack.push((format!("{}", s16(pc)), false)),
                0x15 => { let v = iload_local(&mut out, &mut fresh, bc[pc + 1]); stack.push((v, false)); }
                0x1a..=0x1d => { let v = iload_local(&mut out, &mut fresh, op - 0x1a); stack.push((v, false)); }
                0x36 | 0x3b..=0x3e => {
                    let idx = if op == 0x36 { bc[pc + 1] } else { op - 0x3b };
                    let (v, _) = stack.pop()?;
                    let e = fresh(&mut out, &format!("sext i32 {v} to i64"));
                    out.push_str(&format!("  store i64 {e}, ptr %l{idx}\n"));
                }
                // ---- long constants / loads / stores ----
                0x09 | 0x0a => stack.push((format!("{}", op - 0x09), true)),
                0x16 => stack.push((fresh(&mut out, &format!("load i64, ptr %l{}", bc[pc + 1])), true)),
                0x1e..=0x21 => stack.push((fresh(&mut out, &format!("load i64, ptr %l{}", op - 0x1e)), true)),
                0x37 | 0x3f..=0x42 => {
                    let idx = if op == 0x37 { bc[pc + 1] } else { op - 0x3f };
                    let (v, _) = stack.pop()?;
                    out.push_str(&format!("  store i64 {v}, ptr %l{idx}\n"));
                }
                // ---- stack ----
                0x57 => { stack.pop()?; }
                0x59 => { let v = stack.last()?.clone(); stack.push(v); }
                // ---- int arithmetic (i32) ----
                0x60 | 0x64 | 0x68 | 0x7e | 0x80 | 0x82 => {
                    let (b, _) = stack.pop()?;
                    let (a, _) = stack.pop()?;
                    let ins = match op { 0x60 => "add", 0x64 => "sub", 0x68 => "mul", 0x7e => "and", 0x80 => "or", _ => "xor" };
                    stack.push((fresh(&mut out, &format!("{ins} i32 {a}, {b}")), false));
                }
                0x74 => { let (a, _) = stack.pop()?; stack.push((fresh(&mut out, &format!("sub i32 0, {a}")), false)); }
                0x78 | 0x7a | 0x7c => {
                    let (cnt, _) = stack.pop()?;
                    let (val, _) = stack.pop()?;
                    let m = fresh(&mut out, &format!("and i32 {cnt}, 31"));
                    let ins = match op { 0x78 => "shl", 0x7a => "ashr", _ => "lshr" };
                    stack.push((fresh(&mut out, &format!("{ins} i32 {val}, {m}")), false));
                }
                // ---- long arithmetic (i64) ----
                0x61 | 0x65 | 0x69 | 0x7f | 0x81 | 0x83 => {
                    let (b, _) = stack.pop()?;
                    let (a, _) = stack.pop()?;
                    let ins = match op { 0x61 => "add", 0x65 => "sub", 0x69 => "mul", 0x7f => "and", 0x81 => "or", _ => "xor" };
                    stack.push((fresh(&mut out, &format!("{ins} i64 {a}, {b}")), true));
                }
                0x75 => { let (a, _) = stack.pop()?; stack.push((fresh(&mut out, &format!("sub i64 0, {a}")), true)); }
                0x79 | 0x7b | 0x7d => {
                    // long shift by an INT count (masked to 63); widen the count to i64.
                    let (cnt, _) = stack.pop()?;
                    let (val, _) = stack.pop()?;
                    let c64 = fresh(&mut out, &format!("zext i32 {cnt} to i64"));
                    let m = fresh(&mut out, &format!("and i64 {c64}, 63"));
                    let ins = match op { 0x79 => "shl", 0x7b => "ashr", _ => "lshr" };
                    stack.push((fresh(&mut out, &format!("{ins} i64 {val}, {m}")), true));
                }
                0x94 => {
                    // lcmp: two longs → int -1/0/1.
                    let (b, _) = stack.pop()?;
                    let (a, _) = stack.pop()?;
                    let lt = fresh(&mut out, &format!("icmp slt i64 {a}, {b}"));
                    let gt = fresh(&mut out, &format!("icmp sgt i64 {a}, {b}"));
                    let g = fresh(&mut out, &format!("select i1 {gt}, i32 1, i32 0"));
                    let r = fresh(&mut out, &format!("select i1 {lt}, i32 -1, i32 {g}"));
                    stack.push((r, false));
                }
                // ---- conversions ----
                0x85 => { let (v, _) = stack.pop()?; stack.push((fresh(&mut out, &format!("sext i32 {v} to i64")), true)); }
                0x88 => { let (v, _) = stack.pop()?; stack.push((fresh(&mut out, &format!("trunc i64 {v} to i32")), false)); }
                // ---- iinc (int local) ----
                0x84 => {
                    let idx = bc[pc + 1];
                    let c = bc[pc + 2] as i8 as i32;
                    let cur = iload_local(&mut out, &mut fresh, idx);
                    let nv = fresh(&mut out, &format!("add i32 {cur}, {c}"));
                    let e = fresh(&mut out, &format!("sext i32 {nv} to i64"));
                    out.push_str(&format!("  store i64 {e}, ptr %l{idx}\n"));
                }
                // ---- branches (int compares) ----
                0x99..=0xa4 => {
                    let target = (pc as i32 + s16(pc)) as usize;
                    let fall = pc + len;
                    let cond = if op <= 0x9e {
                        let (a, _) = stack.pop()?;
                        let cc = ["eq", "ne", "slt", "sge", "sgt", "sle"][(op - 0x99) as usize];
                        fresh(&mut out, &format!("icmp {cc} i32 {a}, 0"))
                    } else {
                        let (b, _) = stack.pop()?;
                        let (a, _) = stack.pop()?;
                        let cc = ["eq", "ne", "slt", "sge", "sgt", "sle"][(op - 0x9f) as usize];
                        fresh(&mut out, &format!("icmp {cc} i32 {a}, {b}"))
                    };
                    if !stack.is_empty() {
                        return None;
                    }
                    let _ = (leader_set.get(&target)?, leader_set.get(&fall)?);
                    out.push_str(&format!("  br i1 {cond}, label %b{target}, label %b{fall}\n"));
                    terminated = true;
                    break;
                }
                0xa7 => {
                    let target = (pc as i32 + s16(pc)) as usize;
                    if !stack.is_empty() {
                        return None;
                    }
                    let _ = leader_set.get(&target)?;
                    out.push_str(&format!("  br label %b{target}\n"));
                    terminated = true;
                    break;
                }
                // ---- returns ----
                0xac => {
                    let (v, _) = stack.pop()?;
                    let r = fresh(&mut out, &format!("sext i32 {v} to i64"));
                    out.push_str(&format!("  ret i64 {r}\n"));
                    terminated = true;
                    break;
                }
                0xad => {
                    let (v, _) = stack.pop()?;
                    out.push_str(&format!("  ret i64 {v}\n"));
                    terminated = true;
                    break;
                }
                // ---- double (a double rides the i64 cell as its bit-pattern) ----
                0x0e | 0x0f => {
                    // dconst_0 / dconst_1 — via the const-table (avoid a .rdata literal).
                    let bits: i64 = if op == 0x0e { 0 } else { 0x3FF0000000000000u64 as i64 };
                    let slot = consts.len();
                    consts.push(bits);
                    let cp = fresh(&mut out, &format!("getelementptr i64, ptr %consts, i64 {slot}"));
                    let cb = fresh(&mut out, &format!("load i64, ptr {cp}"));
                    stack.push((fresh(&mut out, &format!("bitcast i64 {cb} to double")), false));
                }
                0x18 | 0x26..=0x29 => {
                    let idx = if op == 0x18 { bc[pc + 1] } else { op - 0x26 };
                    let raw = fresh(&mut out, &format!("load i64, ptr %l{idx}"));
                    stack.push((fresh(&mut out, &format!("bitcast i64 {raw} to double")), false));
                }
                0x39 | 0x47..=0x4a => {
                    let idx = if op == 0x39 { bc[pc + 1] } else { op - 0x47 };
                    let (v, _) = stack.pop()?;
                    let bits = fresh(&mut out, &format!("bitcast double {v} to i64"));
                    out.push_str(&format!("  store i64 {bits}, ptr %l{idx}\n"));
                }
                0x63 | 0x67 | 0x6b | 0x6f => {
                    let (b, _) = stack.pop()?;
                    let (a, _) = stack.pop()?;
                    let ins = match op { 0x63 => "fadd", 0x67 => "fsub", 0x6b => "fmul", _ => "fdiv" };
                    stack.push((fresh(&mut out, &format!("{ins} double {a}, {b}")), false));
                }
                0x77 => { let (a, _) = stack.pop()?; stack.push((fresh(&mut out, &format!("fneg double {a}")), false)); }
                0x97 | 0x98 => {
                    // dcmpl/dcmpg → int -1/0/1. NaN yields -1 (dcmpl) or 1 (dcmpg).
                    let (b, _) = stack.pop()?;
                    let (a, _) = stack.pop()?;
                    let nan = fresh(&mut out, &format!("fcmp uno double {a}, {b}"));
                    let lt = fresh(&mut out, &format!("fcmp olt double {a}, {b}"));
                    let gt = fresh(&mut out, &format!("fcmp ogt double {a}, {b}"));
                    let r = if op == 0x98 {
                        // dcmpg: (gt or NaN) → 1, else lt → -1, else 0.
                        let g = fresh(&mut out, &format!("or i1 {gt}, {nan}"));
                        let r1 = fresh(&mut out, &format!("select i1 {g}, i32 1, i32 0"));
                        fresh(&mut out, &format!("select i1 {lt}, i32 -1, i32 {r1}"))
                    } else {
                        // dcmpl: (lt or NaN) → -1, else gt → 1, else 0.
                        let l = fresh(&mut out, &format!("or i1 {lt}, {nan}"));
                        let r1 = fresh(&mut out, &format!("select i1 {gt}, i32 1, i32 0"));
                        fresh(&mut out, &format!("select i1 {l}, i32 -1, i32 {r1}"))
                    };
                    stack.push((r, false));
                }
                0x87 => { let (v, _) = stack.pop()?; stack.push((fresh(&mut out, &format!("sitofp i32 {v} to double")), false)); }
                0x8a => { let (v, _) = stack.pop()?; stack.push((fresh(&mut out, &format!("sitofp i64 {v} to double")), false)); }
                0x8e | 0x8f => {
                    // d2i/d2l — JVM: truncate toward zero, saturate MIN/MAX, NaN→0. Use plain
                    // fptosi (x86 cvttsd2si: returns INT_MIN for NaN/underflow/overflow) then
                    // patch the two cases cvttsd2si gets "wrong" vs JVM: overflow-high→MAX and
                    // NaN→0 (underflow already yields the correct INT_MIN). No llvm.fptosi.sat
                    // — that can lower to a compiler-rt libcall (external symbol our reloc-free
                    // .text can't bind → the call jumps to garbage).
                    let (v, _) = stack.pop()?;
                    let (ty, wide, thresh_bits, max) = if op == 0x8f {
                        ("i64", true, 0x43E0000000000000u64 as i64, "9223372036854775807") // 2^63
                    } else {
                        ("i32", false, 0x41E0000000000000u64 as i64, "2147483647") // 2^31
                    };
                    // The saturate threshold is a double literal too — route it through the
                    // const-table so clang never spills it into a .rdata pool (RIP-reloc).
                    let slot = consts.len();
                    consts.push(thresh_bits);
                    let tp = fresh(&mut out, &format!("getelementptr i64, ptr %consts, i64 {slot}"));
                    let tb = fresh(&mut out, &format!("load i64, ptr {tp}"));
                    let td = fresh(&mut out, &format!("bitcast i64 {tb} to double"));
                    let raw = fresh(&mut out, &format!("fptosi double {v} to {ty}"));
                    let big = fresh(&mut out, &format!("fcmp oge double {v}, {td}"));
                    let r1 = fresh(&mut out, &format!("select i1 {big}, {ty} {max}, {ty} {raw}"));
                    let nan = fresh(&mut out, &format!("fcmp uno double {v}, {v}"));
                    let r = fresh(&mut out, &format!("select i1 {nan}, {ty} 0, {ty} {r1}"));
                    stack.push((r, wide));
                }
                0xaf => {
                    // dreturn — the double's bit-pattern into the i64 result cell.
                    let (v, _) = stack.pop()?;
                    let bits = fresh(&mut out, &format!("bitcast double {v} to i64"));
                    out.push_str(&format!("  ret i64 {bits}\n"));
                    terminated = true;
                    break;
                }
                // ---- numeric constants from the pool (inline; refs bail) ----
                0x12 | 0x13 => {
                    let idx = if op == 0x12 { bc[pc + 1] as u16 } else { ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16 };
                    match ldc_of(idx)? {
                        crate::aot_clif::LdcConst::Int(v) => stack.push((format!("{v}"), false)),
                        // float / string / class constants aren't in the numeric subset.
                        _ => return None,
                    }
                }
                0x14 => {
                    let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
                    match ldc_of(idx)? {
                        crate::aot_clif::LdcConst::Long(v) => stack.push((format!("{v}"), true)),
                        crate::aot_clif::LdcConst::Double(bits) => {
                            // Double literal via the const-table (avoid a .rdata spill).
                            let slot = consts.len();
                            consts.push(bits as i64);
                            let cp = fresh(&mut out, &format!("getelementptr i64, ptr %consts, i64 {slot}"));
                            let cb = fresh(&mut out, &format!("load i64, ptr {cp}"));
                            stack.push((fresh(&mut out, &format!("bitcast i64 {cb} to double")), false));
                        }
                        _ => return None,
                    }
                }
                // ---- array reference load (handle rides the i64 cell; is_long slot
                //      tag reused — a ref only ever reaches the array ops below) ----
                0x19 => stack.push((fresh(&mut out, &format!("load i64, ptr %l{}", bc[pc + 1])), true)),
                0x2a..=0x2d => stack.push((fresh(&mut out, &format!("load i64, ptr %l{}", op - 0x2a)), true)),
                // ---- arraylength ----
                0xbe => {
                    let (arr, _) = stack.pop()?;
                    let s = asite;
                    asite += 1;
                    let (_data, len) = emit_arr_desc(&mut out, s, &arr);
                    stack.push((len, false)); // i32
                }
                // ---- iaload / laload (primitive element read) ----
                0x2e | 0x2f => {
                    let (idx, _) = stack.pop()?;
                    let (arr, _) = stack.pop()?;
                    let s = asite;
                    asite += 1;
                    let (data, len) = emit_arr_desc(&mut out, s, &arr);
                    let shift = if op == 0x2f { 3 } else { 2 }; // long=8B, int=4B
                    let ep = emit_arr_addr(&mut out, s, &arr, &idx, &data, &len, shift);
                    let ty = if op == 0x2f { "i64" } else { "i32" };
                    let elem = fresh(&mut out, &format!("load {ty}, ptr {ep}"));
                    stack.push((elem, op == 0x2f));
                }
                // ---- iastore / lastore (primitive element write) ----
                0x4f | 0x50 => {
                    let (val, _) = stack.pop()?;
                    let (idx, _) = stack.pop()?;
                    let (arr, _) = stack.pop()?;
                    let s = asite;
                    asite += 1;
                    let (data, len) = emit_arr_desc(&mut out, s, &arr);
                    let shift = if op == 0x50 { 3 } else { 2 };
                    let ep = emit_arr_addr(&mut out, s, &arr, &idx, &data, &len, shift);
                    let ty = if op == 0x50 { "i64" } else { "i32" };
                    out.push_str(&format!("  store {ty} {val}, ptr {ep}\n"));
                }
                // ---- invokestatic to an LLVM-eligible callee: same-module direct call ----
                0xb8 => {
                    let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
                    let cal = callee_names.get(&idx)?; // absent → callee not eligible → bail whole method
                    let (args, ret) = llvm_desc_args_ret(&cal.desc)?;
                    // Callee-locals buffer. Zero it (the callee prologue loads ALL max_locals cells).
                    let cl = fresh(&mut out, &format!("alloca [{} x i64]", cal.max_locals.max(1)));
                    for i in 0..cal.max_locals {
                        let p = fresh(&mut out, &format!("getelementptr i64, ptr {cl}, i64 {i}"));
                        out.push_str(&format!("  store i64 0, ptr {p}\n"));
                    }
                    // Per-arg local slot (I=1 slot, J/D=2, value in the first).
                    let mut slot_of = Vec::with_capacity(args.len());
                    {
                        let mut s = 0u32;
                        for &k in &args {
                            slot_of.push(s);
                            s += if matches!(k, 'J' | 'D') { 2 } else { 1 };
                        }
                    }
                    // Args are pushed left→right, so the LAST arg is on top: pop in reverse.
                    for k in (0..args.len()).rev() {
                        let (v, _) = stack.pop()?;
                        let cell = match args[k] {
                            'J' => v, // long: already an i64 ssa
                            'D' => fresh(&mut out, &format!("bitcast double {v} to i64")),
                            _ => fresh(&mut out, &format!("sext i32 {v} to i64")), // I/Z/B/C/S
                        };
                        let ap = fresh(&mut out, &format!("getelementptr i64, ptr {cl}, i64 {}", slot_of[k]));
                        out.push_str(&format!("  store i64 {cell}, ptr {ap}\n"));
                    }
                    // DIRECT internal call — clang inlines @{cal.name} into this function + vectorises across.
                    let r = fresh(&mut out, &format!("call i64 @{}(ptr {cl}, ptr %consts)", cal.name));
                    match ret {
                        'V' => {}
                        'J' => stack.push((r, true)),
                        'D' => stack.push((fresh(&mut out, &format!("bitcast i64 {r} to double")), false)),
                        _ => stack.push((fresh(&mut out, &format!("trunc i64 {r} to i32")), false)),
                    }
                }
                _ => return None,
            }
            pc += len;
        }
        if !terminated {
            if !stack.is_empty() {
                return None;
            }
            out.push_str(&format!("  br label %b{end}\n"));
        }
    }
    out.push_str("}\n");
    shared_out.push_str(&out);
    Some(())
}
