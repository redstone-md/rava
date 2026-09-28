use super::*;
use crate::cnp::{CompiledObj, ConstDesc};

#[test]
fn serialized_int_shifts_match_java_width_sign_and_distance_rules() {
    let values = [i32::MIN, i32::MIN + 1, -123456789, -1, 0, 1, 123456789, i32::MAX];
    let counts = [i32::MIN, -65, -33, -32, -31, -1, 0, 1, 15, 16, 30, 31, 32, 33, 63, 64, i32::MAX];
    for op in [0x78, 0x7a, 0x7c] {
        let (code, relocs, consts) = compile_method(&[0x1a, 0x1b, op, 0xac],
            "ShiftFixture", 2, &|_| None, &|_| None, &|_| None, &|_| None).unwrap();
        let object = CompiledObj::from_bytes(&CompiledObj::from_full(code, relocs, consts).to_bytes()).unwrap();
        let (function, constants) = object.link_full(&|_| 0, &mut |desc| match desc {
            ConstDesc::ClassPtr(_) => Some(0), _ => None,
        }).unwrap();
        for value in values {
            for count in counts {
                let distance = (count as u32) & 31;
                let expected = match op {
                    0x78 => value.wrapping_shl(distance),
                    0x7a => value >> distance,
                    0x7c => ((value as u32) >> distance) as i32,
                    _ => unreachable!(),
                };
                let mut locals = [i64::from(value), i64::from(count)];
                let actual = unsafe { function.run_const(locals.as_mut_ptr(), constants.as_ptr()) };
                assert_eq!(actual, i64::from(expected), "op={op:x} value={value} count={count}");
            }
        }
    }
}

#[test]
fn int_shift_refuses_reference_and_wide_operands() {
    for bytes in [[0x2a, 0x1b, 0x78, 0xac], [0x1e, 0x1b, 0x7c, 0xac],
        [0x1a, 0x2b, 0x7a, 0xac], [0x1a, 0x1f, 0x78, 0xac]] {
        assert!(compile_method(&bytes, "InvalidShift", 3, &|_| None, &|_| None,
            &|_| None, &|_| None).is_none());
    }
}
