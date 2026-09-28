use super::*;
use rava_classfile::Constant;

fn sam_pool() -> ConstantPool {
    ConstantPool::from_entries(vec![
        Constant::Unusable,
        Constant::Utf8("Fi".into()),
        Constant::Class { name_index: 1 },
        Constant::Utf8("apply".into()),
        Constant::Utf8("(I)Z".into()),
        Constant::NameAndType {
            name_index: 3,
            desc_index: 4,
        },
        Constant::InterfaceMethodRef {
            class_index: 2,
            nat_index: 5,
        },
    ])
}

#[test]
fn descriptor_split_preserves_exact_types() {
    assert_eq!(
        descriptor_params("(F[FLjava/lang/Object;I)Z").unwrap(),
        ["F", "[F", "Ljava/lang/Object;", "I"]
    );
    assert_eq!(descriptor_return("(I)Z"), Some("Z"));
}

#[test]
fn only_narrow_loads_match_tracer_one() {
    assert!(load_matches_type(0x15, "I"));
    assert!(load_matches_type(0x2a, "[F"));
    assert!(!load_matches_type(0x16, "J"));
    assert!(!load_matches_type(0x17, "I"));
    assert!(one_slot("I"));
    assert!(one_slot("Ljava/lang/Object;"));
    assert!(!one_slot("J"));
    assert!(!one_slot("D"));
}

#[test]
fn sam_use_requires_one_same_block_receiver_arg_call_sequence() {
    let pool = sam_pool();
    let exact = [0x2c, 0x15, 0x05, 0xb9, 0x00, 0x06, 0x02, 0x00, 0xac];
    assert_eq!(
        single_sam_call(&exact, 2, "Fi", "apply", &pool),
        Some((3, "(I)Z".to_string()))
    );

    let branch_into_arg = [
        0x2c, 0x15, 0x05, 0xb9, 0x00, 0x06, 0x02, 0x00, 0xa7, 0xff, 0xf9,
    ];
    assert!(single_sam_call(&branch_into_arg, 2, "Fi", "apply", &pool).is_none());

    let second_use = [
        0x2c, 0x15, 0x05, 0xb9, 0x00, 0x06, 0x02, 0x00, 0x2c, 0x15, 0x05, 0xb9, 0x00, 0x06, 0x02,
        0x00, 0xac,
    ];
    assert!(single_sam_call(&second_use, 2, "Fi", "apply", &pool).is_none());

    let stored_before_use = [0x4d, 0x2c, 0x15, 0x05, 0xb9, 0x00, 0x06, 0x02, 0x00, 0xac];
    assert!(single_sam_call(&stored_before_use, 2, "Fi", "apply", &pool).is_none());
    assert!(single_sam_call(&[0x2c, 0xb0], 2, "Fi", "apply", &pool).is_none());
}

#[test]
fn descriptors_are_compared_without_adaptation() {
    let exact = descriptor_params("(F[FI)Z").unwrap();
    let adapted = descriptor_params("(F[FLjava/lang/Integer;)Z").unwrap();
    assert_ne!(exact, adapted);
    assert_ne!(descriptor_return("(I)Z"), descriptor_return("(I)I"));
}

#[test]
fn caller_capability_accepts_only_static_primitive_hof_pairs() {
    let exact = [
        0x03, 0x2a, 0xbe, 0x22, 0x2a, 0xba, 0x00, 0x01, 0x00, 0x00, 0xb8, 0x00, 0x02, 0x04, 0x64,
        0xac,
    ];
    let pairs = HashMap::from([(5usize, 10usize)]);
    assert!(caller_has_zero_gc_capability(
        &exact, "([FF)I", true, true, &pairs
    ));
    for descriptor in [
        "(Ljava/lang/Object;F)I",
        "([Ljava/lang/Object;F)I",
        "([[FF)I",
        "([FF)[F",
    ] {
        assert!(!caller_has_zero_gc_capability(
            &exact, descriptor, true, true, &pairs
        ));
    }
    assert!(!caller_has_zero_gc_capability(
        &exact, "([FF)I", false, true, &pairs
    ));
    assert!(!caller_has_zero_gc_capability(
        &exact, "([FF)I", true, false, &pairs
    ));
}

#[test]
fn caller_capability_rejects_unproven_or_reference_operations() {
    let pairs = HashMap::from([(0usize, 5usize)]);
    let exact = [0xba, 0x00, 0x01, 0x00, 0x00, 0xb8, 0x00, 0x02, 0xac];
    assert!(caller_has_zero_gc_capability(
        &exact, "(I)I", true, true, &pairs
    ));

    for op in [0x12, 0x32, 0x53, 0xb2, 0xb6, 0xbb, 0xbd, 0xbf, 0xc0, 0xc5] {
        let mut rejected = exact.to_vec();
        rejected[8] = op;
        assert!(
            !caller_has_zero_gc_capability(&rejected, "(I)I", true, true, &pairs),
            "opcode {op:#04x} must fail closed"
        );
    }

    let extra_call = [
        0xba, 0x00, 0x01, 0x00, 0x00, 0xb8, 0x00, 0x02, 0xb8, 0x00, 0x03, 0xac,
    ];
    assert!(!caller_has_zero_gc_capability(
        &extra_call,
        "(I)I",
        true,
        true,
        &pairs
    ));
}
