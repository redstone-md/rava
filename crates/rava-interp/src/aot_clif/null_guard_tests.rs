use super::*;
use crate::cnp::{CompiledObj, ConstDesc};
use std::sync::atomic::{AtomicU64, Ordering};

static FAULT: AtomicU64 = AtomicU64::new(0);
extern "C" fn record(_caller: i64, pc: i64, op: i64, depth: i64) -> i64 {
    FAULT.store(((pc as u64) << 16) | ((op as u64) << 8) | depth as u64, Ordering::Relaxed);
    0
}

#[test]
fn null_guards_return_before_the_operation_and_preserve_its_location() {
    let cases: &[(&[u8], u64, u64, u64)] = &[
        (&[0x2a, 0xb4, 0, 1, 0xac], 1, 0xb4, 0),
        (&[0x2a, 0x04, 0xb5, 0, 1, 0xb1], 2, 0xb5, 1),
        (&[0x2a, 0x03, 0x2e, 0xac], 2, 0x2e, 1),
        (&[0x2a, 0x03, 0x04, 0x4f, 0xb1], 3, 0x4f, 2),
        (&[0x2a, 0xbe, 0xac], 1, 0xbe, 0),
        (&[0x01, 0xbf], 1, 0xbf, 0),
        (&[0x2a, 0x04, 0xb6, 0, 1, 0xb1], 2, 0xb6, 1),
    ];
    let fault_index = crate::jit::aot_symtab().iter()
        .position(|(name, _)| *name == "rava_aot_null_fault").unwrap();
    for &(bytes, pc, op, depth) in cases {
        let (code, relocs, constants) = compile_method(bytes, "NullGuard", 4,
            &|_| Some(("NullGuard".into(), "field".into(), FieldKind::Int)),
            &|_| Some(InvokeInfo { numeric_leaf: None, object_leaf: None, direct_leaf: None, ncells: 2, kind: 1, ret_void: true, ret_ref: false, ret_wide: false }),
            &|_| None, &|_| None).unwrap();
        let object = CompiledObj::from_bytes(&CompiledObj::from_full(code, relocs, constants).to_bytes()).unwrap();
        let (function, values) = object.link_full(
            &|index| if index as usize == fault_index { record as usize } else { 0 },
            &mut |desc| match desc {
                ConstDesc::ClassPtr(_) | ConstDesc::FieldId(_, _) => Some(1),
                ConstDesc::Imm(value) => Some(*value),
                _ => None,
            }).unwrap();
        let mut locals = [0i64; 8];
        FAULT.store(0, Ordering::Relaxed);
        assert_eq!(unsafe { function.run_const(locals.as_mut_ptr(), values.as_ptr()) }, 0);
        assert_eq!(FAULT.load(Ordering::Relaxed), (pc << 16) | (op << 8) | depth);
    }
}
