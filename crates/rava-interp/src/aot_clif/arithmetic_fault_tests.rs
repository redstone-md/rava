use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

static FAULT: AtomicBool = AtomicBool::new(false);

extern "C" fn divide(x: i64, y: i64, remainder: i64) -> i64 {
    if y == 0 {
        FAULT.store(true, Ordering::Relaxed);
        0
    } else if remainder != 0 {
        x.wrapping_rem(y)
    } else {
        x.wrapping_div(y)
    }
}

#[test]
fn aot_arithmetic_fault_stops_before_following_local_write() {
    for (op, wide) in [(0x6c, false), (0x70, false), (0x6d, true), (0x71, true)] {
        let output = if wide { 2 } else { 1 };
        let mut bytes = vec![0x10, 6, 0x3e]; // A visible effect before the fault.
        bytes.extend(if wide {
            vec![0x0a, 0x1e, op, 0x58, 0x10, 7, 0x3d, 0x10, 9, 0xac]
        } else {
            vec![0x04, 0x1a, op, 0x57, 0x10, 7, 0x3c, 0x10, 9, 0xac]
        });
        let (code, relocs, constants) = compile_method(&bytes, "ArithmeticFault", 4,
            &|_| None, &|_| None, &|_| None, &|_| None).unwrap();
        let object = CompiledObj::from_full(code, relocs, constants);
        let (function, values) = object.link_full(&|index| {
            let (name, address) = crate::jit::aot_symtab()[index as usize];
            if matches!(name, "rava_i32_divrem" | "rava_i64_divrem") {
                divide as usize
            } else { address }
        }, &mut |constant| match constant {
            crate::cnp::ConstDesc::ClassPtr(_) => Some(1),
            _ => None,
        }).unwrap();
        for divisor in [0, 1, -1] {
            let mut locals = [0i64; 8];
            locals[0] = divisor;
            locals[output] = 5;
            FAULT.store(false, Ordering::Relaxed);
            let result = unsafe { function.run_const(locals.as_mut_ptr(), values.as_ptr()) };
            assert_eq!(FAULT.load(Ordering::Relaxed), divisor == 0);
            assert_eq!(locals[3], 6);
            assert_eq!(result, if divisor == 0 { 0 } else { 9 }, "opcode {op:x}");
            assert_eq!(locals[output], if divisor == 0 { 5 } else { 7 });
        }
    }
}
