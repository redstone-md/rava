//! Part 2 of the aot tests.

use super::*;

/// float f(float a, float b){ return a*b - b; } — fload/fmul/fsub/freturn.
/// a at cells[0], b at cells[1] (category-1 floats).
#[test]
fn float_arithmetic() {
    use crate::cnp::{CompiledObj, ConstDesc};
    // fload_0, fload_1, fmul, fload_1, fsub, freturn
    let bc = vec![0x22u8, 0x23, 0x6a, 0x23, 0x66, 0xae];
    let (code, relocs, consts) = compile_method(&bc, "F", 4, &(|_| None), &(|_| None), &(|_: u16| None), &(|_: u16| None)).expect("compiles");
    assert!(relocs.is_empty());
    let obj = CompiledObj::from_bytes(&CompiledObj::from_full(code, relocs, consts).to_bytes()).unwrap();
    let mut rc = |d: &ConstDesc| match d { ConstDesc::ClassPtr(_) => Some(0), _ => None };
    let (f, cvals) = obj.link_full(&|i| resolve_symbol(i), &mut rc).unwrap();
    for (a, bb) in [(2.5f32, 4.0f32), (1.5, -3.0)] {
        let mut locals = [a.to_bits() as i64, bb.to_bits() as i64, 0, 0];
        let ret = unsafe { f.run_const(locals.as_mut_ptr(), cvals.as_ptr()) };
        let got = f32::from_bits(ret as u32);
        let want = a * bb - bb;
        assert!((got - want).abs() < 1e-5, "a={a} b={bb} got={got} want={want}");
    }
}

/// double f(double a, double b){ return a*b + a - b; } — dload/dmul/dadd/dsub/dreturn.
/// locals: a at cells[0] (wide), b at cells[2].
#[test]
fn double_arithmetic() {
    use crate::cnp::{CompiledObj, ConstDesc};
    // dload_0, dload_2, dmul, dload_0, dadd, dload_2, dsub, dreturn
    let bc = vec![0x26u8, 0x28, 0x6b, 0x26, 0x63, 0x28, 0x67, 0xaf];
    let (code, relocs, consts) = compile_method(&bc, "D", 6, &(|_| None), &(|_| None), &(|_: u16| None), &(|_: u16| None)).expect("compiles");
    assert!(relocs.is_empty(), "pure double arithmetic → no relocs");
    let obj = CompiledObj::from_bytes(&CompiledObj::from_full(code, relocs, consts).to_bytes()).unwrap();
    let mut rc = |d: &ConstDesc| match d { ConstDesc::ClassPtr(_) => Some(0), _ => None };
    let (f, cvals) = obj.link_full(&|i| resolve_symbol(i), &mut rc).unwrap();
    for (a, bb) in [(2.5f64, 4.0f64), (1.5, -3.0), (0.1, 0.2)] {
        let mut locals = [a.to_bits() as i64, 0, bb.to_bits() as i64, 0, 0, 0];
        let ret = unsafe { f.run_const(locals.as_mut_ptr(), cvals.as_ptr()) };
        let got = f64::from_bits(ret as u64);
        let want = a * bb + a - bb;
        assert!((got - want).abs() < 1e-9, "a={a} b={bb} got={got} want={want}");
    }
}

#[test]
fn numeric_ldc_inline() {
    use crate::cnp::{CompiledObj, ConstDesc};
    // ldc_w #0 (Int), iload_0, iadd, ireturn
    let bc = vec![0x13u8, 0x00, 0x00, 0x1a, 0x60, 0xac];
    let field_of = |_cp: u16| None;
    let method_of = |_cp: u16| None;
    let ldc_of = |cp: u16| if cp == 0 { Some(LdcConst::Int(1_000_000)) } else { None };
    let (code, relocs, consts) = compile_method(&bc, "N", 4, &field_of, &method_of, &(|_: u16| None), &ldc_of).expect("compiles");
    assert!(relocs.is_empty(), "numeric ldc is inline → no rava_ldc reloc");
    let obj = CompiledObj::from_bytes(&CompiledObj::from_full(code, relocs, consts).to_bytes()).unwrap();
    let mut rc = |d: &ConstDesc| match d { ConstDesc::ClassPtr(_) => Some(0), _ => None };
    let (f, cvals) = obj.link_full(&|i| resolve_symbol(i), &mut rc).unwrap();
    for x in [0i64, 5, -3, 999] {
        let mut locals = [x, 0, 0, 0];
        assert_eq!(unsafe { f.run_const(locals.as_mut_ptr(), cvals.as_ptr()) }, 1_000_000 + x, "x={x}");
    }
}

#[test]
fn idiv_irem_edge_cases() {
    use crate::cnp::{CompiledObj, ConstDesc};
    // int f(int a, int b){ return a OP b; } : iload_0, iload_1, (idiv|irem), ireturn.
    // Non-zero divisors only — div-by-zero routes to the exception callback, which
    // needs a live VM; the arithmetic (incl INT_MIN/-1 overflow) is what we validate.
    for (fold, is_rem) in [(0x6cu8, false), (0x70u8, true)] {
        let bc = vec![0x1au8, 0x1b, fold, 0xac];
        let (code, relocs, consts) = compile_method(&bc, "I", 2, &(|_| None), &(|_| None), &(|_: u16| None), &(|_: u16| None)).expect("compiles");
        let obj = CompiledObj::from_bytes(&CompiledObj::from_full(code, relocs, consts).to_bytes()).unwrap();
        let mut rc = |d: &ConstDesc| match d { ConstDesc::ClassPtr(_) => Some(0), _ => None };
        let (f, cvals) = obj.link_full(&|i| resolve_symbol(i), &mut rc).unwrap();
        for (a, bb) in [(20i32, 3), (-20, 3), (20, -3), (-20, -3), (i32::MIN, -1), (i32::MIN, 1), (7, 2), (0, 5)] {
            let mut locals = [a as i64, bb as i64];
            let ret = unsafe { f.run_const(locals.as_mut_ptr(), cvals.as_ptr()) } as i32;
            let want = if is_rem { a.wrapping_rem(bb) } else { a.wrapping_div(bb) };
            assert_eq!(ret, want, "a={a} b={bb} is_rem={is_rem}");
        }
    }
}

#[test]
fn ldiv_lrem_edge_cases() {
    use crate::cnp::{CompiledObj, ConstDesc};
    // long f(long a, long b){ return a OP b; } : lload_0, lload_2, (ldiv|lrem), lreturn.
    // Non-zero divisors only (div-by-zero needs a live VM); validates the arithmetic incl
    // the LONG_MIN/-1 wrap, via the rava_i64_divrem callback.
    for (fold, is_rem) in [(0x6du8, false), (0x71u8, true)] {
        let bc = vec![0x1eu8, 0x20, fold, 0xad];
        let (code, relocs, consts) = compile_method(&bc, "J", 4, &(|_| None), &(|_| None), &(|_: u16| None), &(|_: u16| None)).expect("compiles");
        let obj = CompiledObj::from_bytes(&CompiledObj::from_full(code, relocs, consts).to_bytes()).unwrap();
        let mut rc = |d: &ConstDesc| match d { ConstDesc::ClassPtr(_) => Some(0), _ => None };
        let (f, cvals) = obj.link_full(&|i| resolve_symbol(i), &mut rc).unwrap();
        for (a, bb) in [(20i64, 3), (-20, 3), (20, -3), (-20, -3), (i64::MIN, -1), (i64::MIN, 1), (7, 2), (1_000_000_000_000i64, 7)] {
            let mut locals = [a, 0, bb, 0];
            let ret = unsafe { f.run_const(locals.as_mut_ptr(), cvals.as_ptr()) };
            let want = if is_rem { a.wrapping_rem(bb) } else { a.wrapping_div(bb) };
            assert_eq!(ret, want, "a={a} b={bb} is_rem={is_rem}");
        }
    }
}

#[test]
fn tableswitch_dispatch() {
    use crate::cnp::{CompiledObj, ConstDesc};
    // int f(int k){ switch(k){ case 0:return 10; case 1:return 20; case 2:return 30;
    //                          default:return 99; } }  — iload_0; tableswitch; case bodies.
    let bc = vec![
        0x1a, // pc0 iload_0
        0xaa, 0x00, 0x00, // pc1 tableswitch + 2 pad bytes (default aligns to pc4)
        0x00, 0x00, 0x00, 0x24, // default = 36  (→ pc37)
        0x00, 0x00, 0x00, 0x00, // low = 0
        0x00, 0x00, 0x00, 0x02, // high = 2
        0x00, 0x00, 0x00, 0x1b, // off[0] = 27 (→ pc28)
        0x00, 0x00, 0x00, 0x1e, // off[1] = 30 (→ pc31)
        0x00, 0x00, 0x00, 0x21, // off[2] = 33 (→ pc34)
        0x10, 0x0a, 0xac, // pc28 case0: bipush 10; ireturn
        0x10, 0x14, 0xac, // pc31 case1: bipush 20; ireturn
        0x10, 0x1e, 0xac, // pc34 case2: bipush 30; ireturn
        0x10, 0x63, 0xac, // pc37 default: bipush 99; ireturn
    ];
    let (code, relocs, consts) = compile_method(&bc, "I", 1, &(|_| None), &(|_| None), &(|_: u16| None), &(|_: u16| None)).expect("tableswitch compiles");
    let obj = CompiledObj::from_bytes(&CompiledObj::from_full(code, relocs, consts).to_bytes()).unwrap();
    let mut rc = |d: &ConstDesc| match d { ConstDesc::ClassPtr(_) => Some(0), _ => None };
    let (f, cvals) = obj.link_full(&|i| resolve_symbol(i), &mut rc).unwrap();
    for (k, want) in [(0i64, 10i64), (1, 20), (2, 30), (5, 99), (-1, 99)] {
        let mut locals = [k];
        assert_eq!(unsafe { f.run_const(locals.as_mut_ptr(), cvals.as_ptr()) }, want, "tableswitch k={k}");
    }
}

#[test]
fn lookupswitch_dispatch() {
    use crate::cnp::{CompiledObj, ConstDesc};
    // int f(int k){ switch(k){ case 10:return 1; case 20:return 2; default:return 9; } }
    // iload_0; lookupswitch{ default, npairs=2, (10→..),(20→..) }.
    let bc = vec![
        0x1a, // pc0 iload_0
        0xab, 0x00, 0x00, // pc1 lookupswitch + 2 pad (default aligns to pc4)
        0x00, 0x00, 0x00, 0x1b, // default = 27 (→ pc28)
        0x00, 0x00, 0x00, 0x02, // npairs = 2
        0x00, 0x00, 0x00, 0x0a, // match 10
        0x00, 0x00, 0x00, 0x1e, // → off 30 (pc31)
        0x00, 0x00, 0x00, 0x14, // match 20
        0x00, 0x00, 0x00, 0x20, // → off 32 (pc33)
        0x10, 0x63, 0xac, // pc28 default: bipush 99; ireturn
        0x04, 0xac, // pc31 case10: iconst_1; ireturn
        0x05, 0xac, // pc33 case20: iconst_2; ireturn
    ];
    let (code, relocs, consts) = compile_method(&bc, "I", 1, &(|_| None), &(|_| None), &(|_: u16| None), &(|_: u16| None)).expect("lookupswitch compiles");
    let obj = CompiledObj::from_bytes(&CompiledObj::from_full(code, relocs, consts).to_bytes()).unwrap();
    let mut rc = |d: &ConstDesc| match d { ConstDesc::ClassPtr(_) => Some(0), _ => None };
    let (f, cvals) = obj.link_full(&|i| resolve_symbol(i), &mut rc).unwrap();
    for (k, want) in [(10i64, 1i64), (20, 2), (0, 99), (15, 99)] {
        let mut locals = [k];
        assert_eq!(unsafe { f.run_const(locals.as_mut_ptr(), cvals.as_ptr()) }, want, "lookupswitch k={k}");
    }
}

#[test]
fn drem_frem_libcall() {
    use crate::cnp::{CompiledObj, ConstDesc};
    let mut rc = |d: &ConstDesc| match d { ConstDesc::ClassPtr(_) => Some(0), _ => None };
    // double a%b : dload_0, dload_2, drem, dreturn.
    let bc = vec![0x26u8, 0x28, 0x73, 0xaf];
    let (code, relocs, consts) = compile_method(&bc, "D", 6, &(|_| None), &(|_| None), &(|_: u16| None), &(|_: u16| None)).expect("drem compiles");
    let obj = CompiledObj::from_bytes(&CompiledObj::from_full(code, relocs, consts).to_bytes()).unwrap();
    let (f, cvals) = obj.link_full(&|i| resolve_symbol(i), &mut rc).unwrap();
    for (a, bb) in [(7.5f64, 2.0f64), (-7.5, 2.0), (10.0, 3.0), (5.5, -1.5)] {
        let mut locals = [a.to_bits() as i64, 0, bb.to_bits() as i64, 0, 0, 0];
        let got = f64::from_bits(unsafe { f.run_const(locals.as_mut_ptr(), cvals.as_ptr()) } as u64);
        assert!((got - a % bb).abs() < 1e-12, "drem a={a} b={bb} got={got}");
    }
    // float a%b : fload_0, fload_1, frem, freturn.
    let bcf = vec![0x22u8, 0x23, 0x72, 0xae];
    let (code, relocs, consts) = compile_method(&bcf, "F", 2, &(|_| None), &(|_| None), &(|_: u16| None), &(|_: u16| None)).expect("frem compiles");
    let obj = CompiledObj::from_bytes(&CompiledObj::from_full(code, relocs, consts).to_bytes()).unwrap();
    let (f, cvals) = obj.link_full(&|i| resolve_symbol(i), &mut rc).unwrap();
    for (a, bb) in [(7.5f32, 2.0f32), (10.0, 3.0), (-5.5, 1.5)] {
        let mut locals = [a.to_bits() as i64, bb.to_bits() as i64];
        let got = f32::from_bits(unsafe { f.run_const(locals.as_mut_ptr(), cvals.as_ptr()) } as u32);
        assert!((got - a % bb).abs() < 1e-6, "frem a={a} b={bb} got={got}");
    }
}

#[test]
fn invoke_returns_ref_decoded() {
    use crate::cnp::{CompiledObj, ConstDesc};
    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    // Object f(){ return helper(); } : invokestatic#2, areturn.
    let bc = vec![0xb8u8, 0x00, 0x02, 0xb0];
    let field_of = |_cp: u16| None;
    let method_of = |cp: u16| if cp == 2 { Some(InvokeInfo { numeric_leaf: None, object_leaf: None, direct_leaf: None, ncells: 1, kind: 0, ret_void: false, ret_ref: true, ret_wide: false }) } else { None };
    let (code, relocs, consts) = compile_method(&bc, "C", 4, &field_of, &method_of, &(|_: u16| None), &(|_: u16| Some(crate::aot_clif::LdcConst::Str("x".into())))).unwrap();
    let obj = CompiledObj::from_bytes(&CompiledObj::from_full(code, relocs, consts).to_bytes()).unwrap();
    let inv_idx = crate::jit::aot_symtab().iter().position(|&(n, _)| n == "rava_invoke").unwrap();
    let rs = |i: u32| if i as usize == inv_idx { stub_invoke_ret_ref as extern "C" fn(i64,i64,i64,*const i64,i64)->i64 as usize } else { crate::jit::aot_symbol_addr(i) };
    let mut rc = |d: &ConstDesc| match d { ConstDesc::ClassPtr(_) => Some(0), ConstDesc::Imm(v) => Some(*v), _ => None };
    let (f, cvals) = obj.link_full(&rs, &mut rc).unwrap();
    let prev = crate::jit::set_current_vm(&mut vm as *mut _);
    let mut cells = vec![0i64; 8];
    crate::frames::cells_push((cells.as_ptr(), cells.len()));
    let ret = unsafe { f.run_const(cells.as_mut_ptr(), cvals.as_ptr()) };
    crate::frames::cells_pop();
    crate::jit::set_current_vm(prev);
    let p = unsafe { crate::arena::ArenaPtr::from_addr(ret as u64) };
    assert_eq!(unsafe { crate::nextgen::cell(p.as_ptr(), 0) }, 99, "returned ref decoded wrong");
}

#[test]
fn invoke_gc_ref_in_local_survives() {
    // Object f(this){ Object r = this.field; helper(); return r; }
    // aload_0, getfield#0, astore_1, invokestatic#1, aload_1, areturn
    let bc = vec![0x2a, 0xb4, 0x00, 0x00, 0x4c, 0xb8, 0x00, 0x01, 0x2b, 0xb0];
    assert_eq!(run_ref_across_invoke(&bc), 42, "ref in local stale across invoke GC");
}

#[test]
fn invoke_gc_ref_on_stack_survives() {
    // Object f(this){ return (this.field, helper())[0] } — ref stays ON THE STACK
    // across the invoke. aload_0, getfield#0, invokestatic#1, areturn
    let bc = vec![0x2a, 0xb4, 0x00, 0x00, 0xb8, 0x00, 0x01, 0xb0];
    assert_eq!(run_ref_across_invoke(&bc), 42, "ref on stack stale across invoke GC");
}

#[test]
fn merged_reference_on_stack_survives_moving_gc() {
    // Both paths read this.field, then join with the reference still on the
    // operand stack before a callback forces collection. Exercise both edges.
    for condition in [0x03, 0x04] {
        let bytes = [condition, 0x99, 0, 10, 0x2a, 0xb4, 0, 0,
            0xa7, 0, 7, 0x2a, 0xb4, 0, 0, 0xb8, 0, 1, 0xb0];
        assert_eq!(run_ref_across_invoke(&bytes), 42);
    }
}

#[test]
fn rejects_callback_shapes() {
    // invokestatic (0xb8) is not in the numeric subset.
    assert!(!numeric_eligible(&[0x1a, 0xb8, 0x00, 0x01, 0xac]));
    // getfield (0xb4) either.
    assert!(!numeric_eligible(&[0x2a, 0xb4, 0x00, 0x01, 0xac]));
}

/// A1 long support: `long f(long a, long b){ return a*b + (a<<3) - b; }` via
/// compile_method (no callbacks → reloc-free), run through run_const. Proves
/// lload/lmul/lshl/ladd/lsub/lreturn + the wide stack model.
/// locals: 0-1 = a (long, cells[0]), 2-3 = b (long, cells[2]).
#[test]
fn compile_method_long_arithmetic() {
    use crate::cnp::CompiledObj;
    // 0:lload_0 1:lload_2 2:lmul       (a*b)
    // 3:lload_0 4:bipush 3 6:lshl      (a<<3)
    // 7:ladd                            (a*b + a<<3)
    // 8:lload_2 9:lsub 10:lreturn       (... - b)
    let bc: Vec<u8> = vec![
        0x1e, 0x20, 0x69, // lload_0, lload_2, lmul
        0x1e, 0x10, 0x03, 0x79, // lload_0, bipush 3, lshl
        0x61, // ladd
        0x20, 0x65, 0xad, // lload_2, lsub, lreturn
    ];
    let field_of = |_cp: u16| None;
    let method_of = |_cp: u16| None;
    let (code, relocs, consts) = compile_method(&bc, "L", 8, &field_of, &method_of, &(|_: u16| None), &(|_: u16| Some(crate::aot_clif::LdcConst::Str("x".into())))).expect("compiles");
    // No call relocs (pure long arithmetic); only the reserved ClassPtr const slot.
    assert!(relocs.is_empty(), "pure long arithmetic carries no relocs");
    let obj = CompiledObj::from_bytes(&CompiledObj::from_full(code, relocs, consts).to_bytes()).unwrap();
    let mut rc = |d: &crate::cnp::ConstDesc| match d {
        crate::cnp::ConstDesc::ClassPtr(_) => Some(0),
        _ => None,
    };
    let (f, cvals) = obj.link_full(&|i| resolve_symbol(i), &mut rc).expect("links");
    for (a, b) in [(3i64, 5i64), (1_000_000, -7), (i32::MAX as i64 + 100, 2)] {
        // a is at cells[0], b at cells[2] (wide locals).
        let mut locals = [a, 0, b, 0];
        let got = unsafe { f.run_const(locals.as_mut_ptr(), cvals.as_ptr()) };
        assert_eq!(got, a.wrapping_mul(b).wrapping_add(a << 3).wrapping_sub(b), "a={a} b={b}");
    }
}
