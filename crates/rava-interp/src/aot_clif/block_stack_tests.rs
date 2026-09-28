use super::*;
use crate::cnp::{CompiledObj, ConstDesc};

fn run(bytes: &[u8], locals: &mut [i64]) -> i64 {
    let (code, relocs, consts) = compile_method(bytes, "MergeFixture", locals.len() as u32,
        &|_| None, &|_| None, &|_| None, &|_| None).expect("stack merge compiles");
    let object = CompiledObj::from_bytes(&CompiledObj::from_full(code, relocs, consts).to_bytes()).unwrap();
    let (function, constants) = object.link_full(&crate::jit::aot_symbol_addr, &mut |desc| match desc {
        ConstDesc::ClassPtr(_) => Some(0), _ => None,
    }).unwrap();
    unsafe { function.run_const(locals.as_mut_ptr(), constants.as_ptr()) }
}

#[test]
fn reference_comparison_decodes_distinct_boxes_for_the_same_value() {
    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let previous = crate::jit::set_current_vm(&mut vm as *mut _);
    let base = crate::frames::box_len();
    let value = crate::Value::Str("shared-reference".into());
    let left = crate::jit::box_ref(&vm, &value);
    let right = crate::jit::box_ref(&vm, &value);
    assert_ne!(left, right, "independent ABI boxes must exercise decoded identity");
    // if_acmpne selects false; the result is carried to a shared ireturn.
    let bytes = [0x2a, 0x2b, 0xa6, 0, 7, 0x04, 0xa7, 0, 4, 0x03, 0xac];
    let mut results = Vec::new();
    for (a, b, expected) in [(left, right, 1), (left, 0, 0), (0, right, 0), (0, 0, 1)] {
        results.push((run(&bytes, &mut [a, b]), expected));
        let mut inverted = bytes;
        inverted[2] = 0xa5;
        results.push((run(&inverted, &mut [a, b]), 1 - expected));
    }
    crate::frames::box_truncate(base);
    crate::jit::set_current_vm(previous);
    for (result, expected) in results { assert_eq!(result, expected); }
}

#[test]
fn boolean_branches_merge_their_result() {
    // return first || second; both successors leave a boolean at the shared ireturn.
    let bytes = [0x1a, 0x9a, 0, 7, 0x1b, 0x99, 0, 7, 0x04, 0xa7, 0, 4, 0x03, 0xac];
    for first in [0, 1] {
        for second in [0, 1] {
            assert_eq!(run(&bytes, &mut [first, second]), i64::from(first != 0 || second != 0));
        }
    }
}

#[test]
fn reference_and_wide_results_keep_their_stack_shapes() {
    let refs = [0x1a, 0x99, 0, 7, 0x2b, 0xa7, 0, 4, 0x2c, 0xb0];
    let wide = [0x1a, 0x99, 0, 7, 0x1f, 0xa7, 0, 4, 0x21, 0xad];
    for condition in [0, 1] {
        assert_eq!(run(&refs, &mut [condition, 0, 0x1234567890]),
            if condition == 0 { 0x1234567890 } else { 0 });
        assert_eq!(run(&wide, &mut [condition, i64::MIN, 0, i64::MAX, 0]),
            if condition == 0 { i64::MAX } else { i64::MIN });
    }
}

#[test]
fn backedges_carry_values_and_can_target_bytecode_entry() {
    let carried = [0x03, 0xa7, 0, 3, 0x59, 0x08, 0xa2, 0, 8, 0x04, 0x60, 0xa7, 0xff, 0xf9, 0xac];
    assert_eq!(run(&carried, &mut [0]), 5);
    let to_entry = [0x1a, 0x99, 0, 9, 0x84, 0, 0xff, 0xa7, 0xff, 0xf9, 0x1b, 0xac];
    for count in [0, 1, 16] { assert_eq!(run(&to_entry, &mut [count, 123]), 123); }
}

#[test]
fn incompatible_reference_or_width_merge_is_refused() {
    for bytes in [
        [0x1a, 0x99, 0, 7, 0x04, 0xa7, 0, 4, 0x01, 0xb0],
        [0x1a, 0x99, 0, 7, 0x09, 0xa7, 0, 4, 0x03, 0xad],
    ] {
        assert!(compile_method(&bytes, "InvalidMerge", 1, &|_| None, &|_| None,
            &|_| None, &|_| None).is_none());
    }
}

#[test]
fn switches_preserve_values_below_the_index() {
    // Keep local 1 below the switch index; both targets add a different constant.
    for table in [false, true] {
        let mut bytes = vec![0x1b, 0x1a, if table { 0xaa } else { 0xab }, 0];
        bytes.extend(21i32.to_be_bytes()); // default: PC 23, relative to switch PC 2
        if table {
            bytes.extend(0i32.to_be_bytes()); // low
            bytes.extend(0i32.to_be_bytes()); // high
            bytes.extend(18i32.to_be_bytes()); // case 0: PC 20
        } else {
            bytes.extend(1i32.to_be_bytes()); // one pair
            bytes.extend(0i32.to_be_bytes());
            bytes.extend(18i32.to_be_bytes());
        }
        bytes.extend([0x04, 0x60, 0xac, 0x05, 0x60, 0xac]);
        assert_eq!(run(&bytes, &mut [0, 40]), 41);
        assert_eq!(run(&bytes, &mut [-1, 40]), 42);
        assert_eq!(run(&bytes, &mut [1, 40]), 42);
    }
}
