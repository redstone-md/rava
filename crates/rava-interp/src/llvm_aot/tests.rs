//! LLVM AOT tests.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::llvm_aot::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cnp::CompiledObj;

    /// End-to-end proof of the build-time pipeline: hand-written LLVM IR matching the AOT
    /// ABI `fn(locals: *mut i64, consts: *const i64) -> i64`, compiled by clang, its `.text`
    /// loaded into an exec page and called. Skips cleanly if no clang is installed.
    #[test]
    fn llvm_numeric_pipeline_runs() {
        if !clang_available() {
            eprintln!("[llvm-aot] no clang — skipping pipeline test");
            return;
        }
        // f(l, c) = l[0]*l[0] + l[1]. Opaque pointers (LLVM 15+).
        let ir = "\
define i64 @f(ptr %l, ptr %c) {
  %a = load i64, ptr %l
  %p1 = getelementptr i64, ptr %l, i64 1
  %b = load i64, ptr %p1
  %m = mul i64 %a, %a
  %s = add i64 %m, %b
  ret i64 %s
}
";
        let code = clang_compile_ir(ir).expect("clang compiles the IR");
        assert!(!code.is_empty(), ".text extracted");
        let f = CompiledObj::from_code(code).link().expect("links into an exec page");
        for (a, b) in [(3i64, 4), (5, -2), (0, 7), (-6, 100)] {
            let mut locals = [a, b];
            let consts0: [i64; 0] = [];
            let got = unsafe { f.run_const(locals.as_mut_ptr(), consts0.as_ptr()) };
            assert_eq!(got, a * a + b, "a={a} b={b}");
        }
    }

    /// The bytecode->LLVM-IR emitter on a real counted loop: `int f(int n){ int s=0; for(int
    /// i=0;i<n;i++) s+=i*i; return s; }`. Compiled via clang, run, bit-exact vs the reference
    /// (with 32-bit wrapping — the emitted i32 ops match JVM int overflow).
    #[test]
    fn llvm_emits_and_runs_a_sum_loop() {
        if !clang_available() {
            eprintln!("[llvm-aot] no clang — skipping emitter test");
            return;
        }
        // s=0;i=0; loop: iload_2,iload_0,if_icmpge end; iload_1,iload_2,iload_2,imul,iadd,
        // istore_1; iinc 2,1; goto loop; end: iload_1,ireturn.
        let bc = vec![
            0x03u8, 0x3c, 0x03, 0x3d, 0x1c, 0x1a, 0xa2, 0x00, 0x0f, 0x1b, 0x1c, 0x1c, 0x68,
            0x60, 0x3c, 0x84, 0x02, 0x01, 0xa7, 0xff, 0xf2, 0x1b, 0xac,
        ];
        let (ir, cvec) = emit_llvm_ir(&bc, 3, &|_| None).expect("emits IR for the loop");
        let code = clang_compile_ir(&ir).expect("clang compiles the loop");
        let f = CompiledObj::from_code(code).link().expect("links");
        for n in [0i32, 1, 5, 10, 100, 1000, -3] {
            let mut locals = [n as i64, 0, 0];
            let got = unsafe { f.run_const(locals.as_mut_ptr(), cvec.as_ptr()) } as i32;
            let want: i32 = (0..n).fold(0i32, |a, i| a.wrapping_add(i.wrapping_mul(i)));
            assert_eq!(got, want, "n={n}");
        }
    }

    /// The long path: `long f(int n){ long s=0; for(int i=0;i<n;i++) s += (long)i*i; return
    /// s; }` — exercises lconst/lstore/lload, i2l, lmul, ladd, lreturn (the ArithLoop kernel
    /// shape). Bit-exact i64 vs the reference.
    #[test]
    fn llvm_long_accumulator_loop() {
        if !clang_available() {
            eprintln!("[llvm-aot] no clang — skipping long test");
            return;
        }
        // s=0(lstore_1); i=0(istore_3); loop: iload_3,iload_0,if_icmpge end; lload_1,iload_3,
        // i2l,iload_3,i2l,lmul,ladd,lstore_1; iinc 3,1; goto loop; end: lload_1,lreturn.
        let bc = vec![
            0x09u8, 0x40, 0x03, 0x3e, 0x1d, 0x1a, 0xa2, 0x00, 0x11, 0x1f, 0x1d, 0x85, 0x1d,
            0x85, 0x69, 0x61, 0x40, 0x84, 0x03, 0x01, 0xa7, 0xff, 0xf0, 0x1f, 0xad,
        ];
        let (ir, cvec) = emit_llvm_ir(&bc, 4, &|_| None).expect("emits long IR");
        let f = CompiledObj::from_code(clang_compile_ir(&ir).expect("clang")).link().unwrap();
        for n in [0i32, 1, 10, 1000, 100_000] {
            let mut locals = [n as i64, 0, 0, 0];
            let got = unsafe { f.run_const(locals.as_mut_ptr(), cvec.as_ptr()) };
            let want: i64 = (0..n as i64).map(|i| i * i).sum();
            assert_eq!(got, want, "n={n}");
        }
    }

    /// Bisect the double path in isolation: the exact ArithLoop double recurrence
    /// `double d=1.0; for(i<n) d=d*1.0000001+(i&7); return (long)d`, compiled via LLVM, run,
    /// compared bit-exact to a Rust f64 reference computing the identical IEEE sequence. If
    /// this matches, the double ops (dmul/dadd/i2d/d2l/ldc2_w) are correct.
    #[test]
    fn llvm_double_recurrence_matches_f64() {
        if !clang_available() {
            eprintln!("[llvm-aot] no clang — skipping double recurrence");
            return;
        }
        // d=1.0(dstore_1); i=0(istore_3); loop: iload_3,iload_0,if_icmpge end; dload_1,
        // ldc2_w#0,dmul,iload_3,bipush 7,iand,i2d,dadd,dstore_1; iinc 3,1; goto; end:
        // dload_1,d2l,lreturn.
        let bc = vec![
            0x0fu8, 0x48, 0x03, 0x3e, 0x1d, 0x1a, 0xa2, 0x00, 0x15, 0x27, 0x14, 0x00, 0x00,
            0x6b, 0x1d, 0x10, 0x07, 0x7e, 0x87, 0x63, 0x48, 0x84, 0x03, 0x01, 0xa7, 0xff,
            0xec, 0x27, 0x8f, 0xad,
        ];
        let c = 1.0000001_f64;
        let ldc = |_cp: u16| Some(crate::aot_clif::LdcConst::Double(c.to_bits()));
        let (ir, cvec) = emit_llvm_ir(&bc, 4, &ldc).expect("emits double IR");
        let f = CompiledObj::from_code(clang_compile_ir(&ir).expect("clang")).link().unwrap();
        for n in [0i32, 1, 2, 5, 100, 1000, 100_000] {
            let mut locals = [n as i64, 0, 0, 0];
            let got = unsafe { f.run_const(locals.as_mut_ptr(), cvec.as_ptr()) };
            let mut d = 1.0f64;
            for i in 0..n {
                d = d * c + (i & 7) as f64;
            }
            let want = d as i64;
            assert_eq!(got, want, "n={n}: llvm {got} vs f64 {want} (d={d})");
        }
    }

    /// Relative A/B: the SAME sum-loop compiled by the Cranelift numeric lane vs the LLVM
    /// tier, run back-to-back on this machine (so the ratio is thermal-robust — both share
    /// the same conditions). Verifies bit-exact, then prints min-time each + the speedup.
    /// Run with: cargo test --release -p rava-interp --features next-gen-gc
    ///   llvm_vs_cranelift_speed -- --nocapture --ignored
    #[test]
    #[ignore]
    fn llvm_vs_cranelift_speed() {
        if !clang_available() {
            eprintln!("[llvm-aot] no clang — skipping speed A/B");
            return;
        }
        let bc = vec![
            0x03u8, 0x3c, 0x03, 0x3d, 0x1c, 0x1a, 0xa2, 0x00, 0x0f, 0x1b, 0x1c, 0x1c, 0x68,
            0x60, 0x3c, 0x84, 0x02, 0x01, 0xa7, 0xff, 0xf2, 0x1b, 0xac,
        ];
        let cl = crate::aot_clif::compile_numeric(&bc).and_then(|o| o.link()).expect("cranelift");
        let (ir, cvec) = emit_llvm_ir(&bc, 3, &|_| None).expect("ir");
        let lv = CompiledObj::from_code(clang_compile_ir(&ir).expect("clang")).link().expect("llvm");
        let n: i64 = 200_000_000;
        let run = |f: &crate::cnp::CompiledFn| -> (i32, std::time::Duration) {
            let mut best = std::time::Duration::MAX;
            let mut r = 0i32;
            for _ in 0..3 {
                let mut locals = [n, 0, 0];
                    let t = std::time::Instant::now();
                r = unsafe { f.run_const(locals.as_mut_ptr(), cvec.as_ptr()) } as i32;
                best = best.min(t.elapsed());
            }
            (r, best)
        };
        let (rc, tc) = run(&cl);
        let (rl, tl) = run(&lv);
        assert_eq!(rc, rl, "cranelift {rc} vs llvm {rl} diverged");
        let ratio = tc.as_secs_f64() / tl.as_secs_f64().max(1e-9);
        eprintln!("[LLVM-AB] n={n} sum={rc} | cranelift {:?} | llvm {:?} | LLVM {ratio:.2}x cranelift", tc, tl);
    }
}
