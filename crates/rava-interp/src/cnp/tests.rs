//! Compiled-native-path tests.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::cnp::*;

#[cfg(test)]
mod tests {
    use super::*;

    fn interp(bc: &[u8], locals: &[i64]) -> i64 {
        let mut st: Vec<i32> = Vec::new();
        let mut lo: Vec<i32> = locals.iter().map(|&x| x as i32).collect();
        lo.resize(16, 0);
        let mut pc = 0usize;
        loop {
            let op = bc[pc];
            match op {
                0x02..=0x08 => (st.push(op as i32 - 3), pc += 1).1,
                0x10 => (st.push(bc[pc + 1] as i8 as i32), pc += 2).1,
                0x11 => (st.push(i16::from_be_bytes([bc[pc + 1], bc[pc + 2]]) as i32), pc += 3).1,
                0x1a..=0x1d => (st.push(lo[op as usize - 0x1a]), pc += 1).1,
                0x15 => (st.push(lo[bc[pc + 1] as usize]), pc += 2).1,
                0x3b..=0x3e => {
                    lo[op as usize - 0x3b] = st.pop().unwrap();
                    pc += 1;
                }
                0x36 => {
                    lo[bc[pc + 1] as usize] = st.pop().unwrap();
                    pc += 2;
                }
                0x84 => {
                    lo[bc[pc + 1] as usize] =
                        lo[bc[pc + 1] as usize].wrapping_add(bc[pc + 2] as i8 as i32);
                    pc += 3;
                }
                0x57 => {
                    st.pop();
                    pc += 1;
                }
                0x59 => {
                    let t = *st.last().unwrap();
                    st.push(t);
                    pc += 1;
                }
                0x60 | 0x64 | 0x68 | 0x74 | 0x7e | 0x80 | 0x82 | 0x78 | 0x7a | 0x7c => {
                    if op == 0x74 {
                        let a = st.pop().unwrap();
                        st.push(a.wrapping_neg());
                    } else {
                        let b = st.pop().unwrap();
                        let a = st.pop().unwrap();
                        st.push(match op {
                            0x60 => a.wrapping_add(b),
                            0x64 => a.wrapping_sub(b),
                            0x68 => a.wrapping_mul(b),
                            0x7e => a & b,
                            0x80 => a | b,
                            0x82 => a ^ b,
                            0x78 => a.wrapping_shl((b & 31) as u32),
                            0x7a => a.wrapping_shr((b & 31) as u32),
                            _ => (a as u32).wrapping_shr((b & 31) as u32) as i32,
                        });
                    }
                    pc += 1;
                }
                0xa7 => pc = (pc as i32 + i16::from_be_bytes([bc[pc + 1], bc[pc + 2]]) as i32) as usize,
                0x9f..=0xa4 => {
                    let b = st.pop().unwrap();
                    let a = st.pop().unwrap();
                    let take = match op {
                        0x9f => a == b,
                        0xa0 => a != b,
                        0xa1 => a < b,
                        0xa2 => a >= b,
                        0xa3 => a > b,
                        _ => a <= b,
                    };
                    pc = if take {
                        (pc as i32 + i16::from_be_bytes([bc[pc + 1], bc[pc + 2]]) as i32) as usize
                    } else {
                        pc + 3
                    };
                }
                0x99..=0x9e => {
                    let a = st.pop().unwrap();
                    let take = match op {
                        0x99 => a == 0,
                        0x9a => a != 0,
                        0x9b => a < 0,
                        0x9c => a >= 0,
                        0x9d => a > 0,
                        _ => a <= 0,
                    };
                    pc = if take {
                        (pc as i32 + i16::from_be_bytes([bc[pc + 1], bc[pc + 2]]) as i32) as usize
                    } else {
                        pc + 3
                    };
                }
                0xac => return st.pop().unwrap() as i64,
                _ => panic!("ref interp: unsupported op {op:#x}"),
            }
        }
    }

    fn run_linked(f: &CompiledFn, locals: &[i64]) -> i64 {
        let mut stack = vec![0i64; 64];
        let mut locs = locals.to_vec();
        locs.resize(16, 0);
        let mut frame = CnpFrame { stack: stack.as_mut_ptr(), sp: 0, locals: locs.as_mut_ptr() };
        unsafe { f.run(&mut frame) }
    }
    fn jit(bc: &[u8], locals: &[i64]) -> i64 {
        run_linked(&compile_link(bc).expect("compilable"), locals)
    }
    fn diff(bc: &[u8], local_sets: &[&[i64]]) {
        for ls in local_sets {
            assert_eq!(jit(bc, ls), interp(bc, ls), "bc={bc:02x?} locals={ls:?}");
        }
    }
    extern "C" fn fake_getfield(receiver: i64, field_id: i64, is_ref: i64) -> i64 {
        receiver + field_id + is_ref
    }

    #[test]
    fn getfield_uses_platform_callback_abi() {
        let mut ctx = CnpCtx::default();
        ctx.fields.insert(1, (11, true));
        ctx.getfield_addr = fake_getfield as usize;
        let body = [0x2a, 0xb4, 0x00, 0x01, 0xb0];
        let linked = compile_ctx(&body, &ctx)
            .expect("getfield compiles")
            .link()
            .expect("getfield links");
        assert_eq!(run_linked(&linked, &[7]), 19);
    }

    const SUM_LOOP: &[u8] = &[
        0x03, 0x3c, 0x03, 0x3d, 0x1c, 0x1a, 0xa2, 0x00, 0x0d, 0x1b, 0x1c, 0x60, 0x3c, 0x84, 0x02,
        0x01, 0xa7, 0xff, 0xf4, 0x1b, 0xac,
    ];

    #[test]
    fn straight_line_arith() {
        diff(&[0x08, 0x06, 0x64, 0x07, 0x68, 0xac], &[&[]]);
        diff(&[0x1a, 0x10, 0x0a, 0x60, 0xac], &[&[32], &[-5], &[i32::MAX as i64]]);
    }
    #[test]
    fn all_binops() {
        // local0 OP local1 for each op, across sign/overflow/shift-edge inputs.
        for (op, _) in [
            (0x60u8, "add"), (0x64, "sub"), (0x68, "mul"), (0x7e, "and"), (0x80, "or"),
            (0x82, "xor"), (0x78, "shl"), (0x7a, "shr"), (0x7c, "ushr"),
        ] {
            let bc = [0x1a, 0x1b, op, 0xac];
            diff(&bc, &[&[7, 3], &[-7, 3], &[i32::MIN as i64, 1], &[-1, 31], &[123456, 5]]);
        }
        diff(&[0x1a, 0x74, 0xac], &[&[5], &[-5], &[i32::MIN as i64]]); // ineg
    }
    #[test]
    fn dup_pop() {
        // iload_0 dup iadd ireturn  → 2*local0
        diff(&[0x1a, 0x59, 0x60, 0xac], &[&[21], &[-3]]);
        // iconst_5 iload_0 pop ireturn → 5
        diff(&[0x08, 0x1a, 0x57, 0xac], &[&[99]]);
    }
    #[test]
    fn sum_loop() {
        diff(SUM_LOOP, &[&[0], &[1], &[5], &[100], &[1000]]);
    }
    #[test]
    fn countdown_ifne() {
        let bc: Vec<u8> = vec![
            0x1a, 0x3c, 0x1b, 0x99, 0x00, 0x0a, 0x1b, 0x04, 0x64, 0x3c, 0xa7, 0xff, 0xf8, 0x1b,
            0xac,
        ];
        diff(&bc, &[&[0], &[3], &[10]]);
    }
    #[test]
    fn all_icmp_conditions() {
        // if_icmp<cc> local0,local1 → return 1 else 0
        for cc in [0x9fu8, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4] {
            // @0 iload_0 @1 iload_1 @2 if_icmp<cc> +5→@7 @5 iconst_0 @6 ireturn
            // @7 iconst_1 @8 ireturn   → return (a <cc> b) ? 1 : 0
            let bc = [0x1a, 0x1b, cc, 0x00, 0x05, 0x03, 0xac, 0x04, 0xac];
            diff(&bc, &[&[3, 5], &[5, 5], &[5, 3], &[-1, 1]]);
        }
    }
    #[test]
    fn unsupported_returns_none() {
        assert!(compile(&[0x00, 0xac]).is_none());
        assert!(compile(&[0x03]).is_none());
    }

    /// The VoxelShape BooleanOp `(ZZ)Z` lambdas that cnp fires on 3.2M times in the MC
    /// boot — reconstructed from the live trace. Differential vs the reference over all
    /// (a,b) in {0,1}^2.
    #[test]
    fn boolean_ops() {
        let ops: &[&[u8]] = &[
            &[0x1a, 0x99, 0x00, 0x0b, 0x1b, 0x99, 0x00, 0x07, 0x04, 0xa7, 0x00, 0x04, 0x03, 0xac],
            &[0x1a, 0x99, 0x00, 0x0b, 0x1b, 0x9a, 0x00, 0x07, 0x04, 0xa7, 0x00, 0x04, 0x03, 0xac],
            &[0x1a, 0x9a, 0x00, 0x07, 0x1b, 0x99, 0x00, 0x07, 0x04, 0xa7, 0x00, 0x04, 0x03, 0xac],
        ];
        for bc in ops {
            diff(bc, &[&[0, 0], &[0, 1], &[1, 0], &[1, 1]]);
        }
    }

    /// Every distinct method cnp fires on in the early MC boot (from the live trace) —
    /// isPowerOfTwo, bit-round-up, RGBA pack, char class. If cnp mis-compiles any, this
    /// catches it (the boot failure would then be pinned to compute, not semantics).
    #[test]
    fn boot_fired_methods() {
        // (I)Z isPowerOfTwo
        diff(
            &[0x1a, 0x99, 0x00, 0x0f, 0x1a, 0x1a, 0x04, 0x64, 0x7e, 0x9a, 0x00, 0x07, 0x04, 0xa7, 0x00, 0x04, 0x03, 0xac],
            &[&[0], &[1], &[2], &[3], &[4], &[7], &[8], &[16], &[1024], &[-1]],
        );
        // (I)I (7-x)*2
        diff(&[0x10, 0x07, 0x1a, 0x64, 0x05, 0x68, 0xac], &[&[0], &[3], &[7], &[-5]]);
        // (I)I round-up-to-power-of-two
        diff(
            &[0x1a, 0x04, 0x64, 0x3c, 0x1b, 0x1b, 0x04, 0x7a, 0x80, 0x3c, 0x1b, 0x1b, 0x05, 0x7a, 0x80, 0x3c, 0x1b, 0x1b, 0x07, 0x7a, 0x80, 0x3c, 0x1b, 0x1b, 0x10, 0x08, 0x7a, 0x80, 0x3c, 0x1b, 0x1b, 0x10, 0x10, 0x7a, 0x80, 0x3c, 0x1b, 0x04, 0x60, 0xac],
            &[&[1], &[5], &[100], &[1000], &[0], &[3]],
        );
        // (IIII)I RGBA pack
        diff(
            &[0x1a, 0x11, 0x00, 0xff, 0x7e, 0x10, 0x18, 0x78, 0x1b, 0x11, 0x00, 0xff, 0x7e, 0x10, 0x10, 0x78, 0x80, 0x1c, 0x11, 0x00, 0xff, 0x7e, 0x10, 0x08, 0x78, 0x80, 0x1d, 0x11, 0x00, 0xff, 0x7e, 0x80, 0xac],
            &[&[255, 128, 64, 1], &[-1, 0, 255, 256], &[0, 0, 0, 0]],
        );
        // (C)Z char classification
        let cz: &[u8] = &[
            0x1a, 0x10, 0x5f, 0x9f, 0x00, 0x2d, 0x1a, 0x10, 0x2d, 0x9f, 0x00, 0x27, 0x1a, 0x10, 0x61,
            0xa1, 0x00, 0x09, 0x1a, 0x10, 0x7a, 0xa4, 0x00, 0x1b, 0x1a, 0x10, 0x30, 0xa1, 0x00, 0x09,
            0x1a, 0x10, 0x39, 0xa4, 0x00, 0x0f, 0x1a, 0x10, 0x2f, 0x9f, 0x00, 0x09, 0x1a, 0x10, 0x2e,
            0xa0, 0x00, 0x07, 0x04, 0xa7, 0x00, 0x04, 0x03, 0xac,
        ];
        for c in 0i64..256 {
            diff(cz, &[&[c]]);
        }
        // compound (I)Z: (x>0) & isPowerOfTwo(x) — combines TWO booleans on the stack
        diff(
            &[0x1a, 0x9e, 0x00, 0x07, 0x04, 0xa7, 0x00, 0x04, 0x03, 0x1a, 0x1a, 0x04, 0x64, 0x7e, 0x9a, 0x00, 0x07, 0x04, 0xa7, 0x00, 0x04, 0x03, 0x7e, 0xac],
            &[&[0], &[1], &[2], &[3], &[4], &[7], &[8], &[16], &[-1], &[-8]],
        );
        // (II)I  x*10 + y
        diff(&[0x1a, 0x10, 0x0a, 0x68, 0x1b, 0x60, 0xac], &[&[3, 7], &[0, 0], &[-5, 2]]);
        // 2nd (C)Z char classification (different offsets) — all chars
        let cz2: &[u8] = &[
            0x1a, 0x10, 0x5f, 0x9f, 0x00, 0x27, 0x1a, 0x10, 0x2d, 0x9f, 0x00, 0x21, 0x1a, 0x10, 0x61,
            0xa1, 0x00, 0x09, 0x1a, 0x10, 0x7a, 0xa4, 0x00, 0x15, 0x1a, 0x10, 0x30, 0xa1, 0x00, 0x09,
            0x1a, 0x10, 0x39, 0xa4, 0x00, 0x09, 0x1a, 0x10, 0x2e, 0xa0, 0x00, 0x07, 0x04, 0xa7, 0x00,
            0x04, 0x03, 0xac,
        ];
        for c in 0i64..256 {
            diff(cz2, &[&[c]]);
        }
    }

    #[test]
    fn aot_dir_load_or_compile_roundtrip() {
        // iload_0; iload_1; iadd; ireturn — through the DIRECTORY cache: first call
        // compiles + persists; a fresh link from the persisted file must compute the
        // same result. (aot_dir() latches RAVA_AOT_DIR once per process, so this test
        // drives the file layer directly: key → write → read → link.)
        let bc = [0x1a, 0x1b, 0x60, 0xac];
        let dir = std::env::temp_dir().join(format!("rava_aot_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let key = aot_key(&bc);
        let obj = compile(&bc).expect("compiles");
        std::fs::write(dir.join(&key), obj.to_bytes()).unwrap();
        // A "later process": read + link the persisted object, no compile.
        let bytes = std::fs::read(dir.join(&key)).unwrap();
        let f = CompiledObj::from_bytes(&bytes).expect("decodes").link().expect("links");
        let mut locals = [7i64, 35, 0, 0];
        let mut stack = [0i64; 8];
        let mut fr = CnpFrame { stack: stack.as_mut_ptr(), sp: 0, locals: locals.as_mut_ptr() };
        assert_eq!(unsafe { f.run(&mut fr) }, 42);
        // Key stability + version prefix (an emitter bump must change the name).
        assert_eq!(key, aot_key(&bc));
        assert!(key.starts_with(&format!("cnp{AOT_FORMAT}_")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn aot_roundtrip() {
        let blob = compile(SUM_LOOP).expect("compilable").to_bytes();
        let obj = CompiledObj::from_bytes(&blob).expect("valid blob");
        let f = obj.link().expect("linkable");
        for n in [0i64, 5, 100, 1000] {
            assert_eq!(run_linked(&f, &[n]), interp(SUM_LOOP, &[n]), "n={n}");
        }
    }
    #[test]
    fn aot_rejects_corrupt() {
        let mut blob = compile(SUM_LOOP).unwrap().to_bytes();
        blob.truncate(blob.len() - 3); // chop the code
        assert!(CompiledObj::from_bytes(&blob).is_none());
    }

    /// Overhead re-measurement after inlining — run explicitly:
    ///   cargo test ... cnp_overhead -- --ignored --nocapture
    #[test]
    #[ignore]
    fn cnp_overhead() {
        use std::time::Instant;
        let n: i64 = 50_000_000;
        let f = compile_link(SUM_LOOP).expect("compilable");
        let mut stack = vec![0i64; 64];
        let mut locs = vec![n, 0, 0, 0];
        let mut frame = CnpFrame { stack: stack.as_mut_ptr(), sp: 0, locals: locs.as_mut_ptr() };
        let t = Instant::now();
        let r_jit = unsafe { f.run(&mut frame) };
        let d_jit = t.elapsed();
        let t = Instant::now();
        let r_ref = interp(SUM_LOOP, &[n]);
        let d_ref = t.elapsed();
        assert_eq!(r_jit, r_ref);
        let ops = n as f64 * 6.0;
        eprintln!(
            "[CNP-OVERHEAD inline] {:.0}M ops: cnp {:?} ({:.0}M op/s) | ref-dispatch {:?} ({:.0}M op/s) | speedup {:.2}x",
            ops / 1e6,
            d_jit,
            ops / d_jit.as_secs_f64() / 1e6,
            d_ref,
            ops / d_ref.as_secs_f64() / 1e6,
            d_ref.as_secs_f64() / d_jit.as_secs_f64(),
        );
    }

    // ---- C2-quality AOT proofs (unchanged; separate from the cnp inline tier) ----

    extern "C" fn ext_add42(x: i64) -> i64 {
        x + 42
    }

    #[test]
    fn c2_quality_aot_proof() {
        use cranelift::codegen::ir::{types::I64, AbiParam, Function, InstBuilder, Signature, UserFuncName};
        use cranelift::codegen::{settings, Context};
        use cranelift::frontend::{FunctionBuilder, FunctionBuilderContext};
        use cranelift::prelude::Configurable;

        let mut fb = settings::builder();
        fb.set("is_pic", "false").unwrap();
        fb.set("opt_level", "speed").unwrap();
        let isa = cranelift_native::builder().unwrap().finish(settings::Flags::new(fb)).unwrap();
        let mut sig = Signature::new(isa.default_call_conv());
        sig.params.push(AbiParam::new(I64));
        sig.returns.push(AbiParam::new(I64));
        let mut func = Function::with_name_signature(UserFuncName::default(), sig);
        let mut fctx = FunctionBuilderContext::new();
        {
            let mut b = FunctionBuilder::new(&mut func, &mut fctx);
            let blk = b.create_block();
            b.append_block_params_for_function_params(blk);
            b.switch_to_block(blk);
            b.seal_block(blk);
            let a = b.block_params(blk)[0];
            let k = b.ins().iconst(I64, 42);
            let r = b.ins().iadd(a, k);
            b.ins().return_(&[r]);
            b.finalize();
        }
        let mut ctx = Context::for_function(func);
        let compiled = ctx.compile(&*isa, &mut Default::default()).expect("compile");
        let code: Vec<u8> = compiled.code_buffer().to_vec();
        assert!(compiled.buffer.relocs().is_empty());
        let p = alloc_exec(&code).expect("exec page");
        let f: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(p) };
        assert_eq!(f(100), 142);
        assert_eq!(f(-42), 0);
        unsafe { dealloc_exec(p, code.len()) };
    }

    #[test]
    fn c2_aot_reloc_roundtrip() {
        use cranelift::codegen::binemit::Reloc;
        use cranelift::codegen::ir::{
            types::I64, AbiParam, ExtFuncData, ExternalName, Function, InstBuilder, Signature,
            UserExternalName, UserFuncName,
        };
        use cranelift::codegen::{settings, Context};
        use cranelift::frontend::{FunctionBuilder, FunctionBuilderContext};
        use cranelift::prelude::Configurable;

        let mut fb = settings::builder();
        fb.set("is_pic", "false").unwrap();
        fb.set("opt_level", "speed").unwrap();
        let isa = cranelift_native::builder().unwrap().finish(settings::Flags::new(fb)).unwrap();
        let mut sig = Signature::new(isa.default_call_conv());
        sig.params.push(AbiParam::new(I64));
        sig.returns.push(AbiParam::new(I64));
        let mut func = Function::with_name_signature(UserFuncName::default(), sig.clone());
        let sigref = func.import_signature(sig);
        let name = func.declare_imported_user_function(UserExternalName::new(0, 0));
        let extfn = func.import_function(ExtFuncData {
            name: ExternalName::user(name),
            signature: sigref,
            colocated: false,
            patchable: false,
        });
        let mut fctx = FunctionBuilderContext::new();
        {
            let mut b = FunctionBuilder::new(&mut func, &mut fctx);
            let blk = b.create_block();
            b.append_block_params_for_function_params(blk);
            b.switch_to_block(blk);
            b.seal_block(blk);
            let a = b.block_params(blk)[0];
            let call = b.ins().call(extfn, &[a]);
            let r = b.inst_results(call)[0];
            b.ins().return_(&[r]);
            b.finalize();
        }
        let mut ctx = Context::for_function(func);
        let compiled = ctx.compile(&*isa, &mut Default::default()).expect("compile");
        let code: Vec<u8> = compiled.code_buffer().to_vec();
        let relocs: Vec<(u32, u32, i64)> = compiled
            .buffer
            .relocs()
            .iter()
            .map(|r| {
                assert!(matches!(r.kind, Reloc::Abs8));
                (r.offset, 0u32, r.addend)
            })
            .collect();
        assert_eq!(relocs.len(), 1);
        let resolve = |idx: u32| -> usize {
            match idx {
                0 => ext_add42 as extern "C" fn(i64) -> i64 as usize,
                _ => panic!("unknown symbol idx"),
            }
        };
        let p = alloc_exec(&code).expect("exec page");
        for &(offset, idx, addend) in &relocs {
            let target = (resolve(idx) as i64).wrapping_add(addend) as u64;
            unsafe { std::ptr::copy_nonoverlapping(target.to_le_bytes().as_ptr(), p.add(offset as usize), 8) };
        }
        let f: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(p) };
        assert_eq!(f(100), 142);
        assert_eq!(f(0), 42);
        unsafe { dealloc_exec(p, code.len()) };
    }
}
