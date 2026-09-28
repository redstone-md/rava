use super::*;
use crate::LambdaSite;
use rava_classfile::Code;
use std::sync::Arc;

fn code(bytecode: Vec<u8>) -> Code {
    Code {
        max_stack: 4,
        max_locals: 4,
        bytecode,
        exceptions: Vec::new(),
     ..Default::default() }
}

#[test]
fn primitive_descriptors_are_exact_and_reference_free() {
    assert_eq!(
        primitive_signature("(IJD)J"),
        Some((
            vec![
                PrimitiveKind::Int,
                PrimitiveKind::Long,
                PrimitiveKind::Double
            ],
            PrimitiveKind::Long
        ))
    );
    assert!(primitive_signature("(Ljava/lang/Object;)J").is_none());
    assert!(primitive_signature("([I)J").is_none());
    assert!(primitive_signature("(I)Ljava/lang/Long;").is_none());
    assert!(post_signature("(J)Ljava/lang/Long;", PrimitiveKind::Long));
    assert!(!post_signature("(J)[J", PrimitiveKind::Long));
    assert!(!post_signature("(J)L;", PrimitiveKind::Long));
    assert!(!post_signature(
        "(J)Ljava/lang/Long;garbage",
        PrimitiveKind::Long
    ));
}

#[test]
fn lambda_wrapper_classifies_exact_capture_descriptors_and_reference_result() {
    assert_eq!(
        lambda_wrapper_signature("(IJD)Ljava/lang/Long;"),
        Some((
            vec![
                LambdaArgument::Primitive(PrimitiveKind::Int),
                LambdaArgument::Primitive(PrimitiveKind::Long),
                LambdaArgument::Primitive(PrimitiveKind::Double)
            ],
            "Ljava/lang/Long;"
        ))
    );
    assert_eq!(
        lambda_wrapper_signature("(Ljava/lang/Object;)Ljava/lang/Long;"),
        Some((vec![LambdaArgument::Reference], "Ljava/lang/Long;"))
    );
    assert!(lambda_wrapper_signature("([I)Ljava/lang/Long;").is_none());
    assert!(lambda_wrapper_signature("(I)[J").is_none());
    assert!(lambda_wrapper_signature("(I)J").is_none());
    assert_eq!(post_result("(J)Ljava/lang/Long;"), Some("Ljava/lang/Long;"));
    assert_eq!(
        lambda_reference_argument_owner("(ILConfig;J)Ljava/lang/Long;", 1),
        Some("Config")
    );
    assert!(lambda_reference_argument_owner("(ILConfig;J)Ljava/lang/Long;", 0).is_none());
    assert!(PrimitiveKind::Int.encode(Value::Null).is_none());
    assert_eq!(PrimitiveKind::Int.encode_field(Value::Null), Some(0));
}

#[test]
fn lambda_loads_must_follow_descriptor_slot_order() {
    let bytecode = [0x1a, 0x1f, 0x18, 0x03];
    assert_eq!(
        primitive_load_len(&bytecode, 0, PrimitiveKind::Int, 0),
        Some(1)
    );
    assert_eq!(
        primitive_load_len(&bytecode, 1, PrimitiveKind::Long, 1),
        Some(1)
    );
    assert_eq!(
        primitive_load_len(&bytecode, 2, PrimitiveKind::Double, 3),
        Some(2)
    );
    assert!(primitive_load_len(&bytecode, 1, PrimitiveKind::Long, 2).is_none());
    assert!(primitive_load_len(&[0xc4, 0x15, 0, 0], 0, PrimitiveKind::Int, 0).is_none());
    assert_eq!(reference_load_len(&[0x2a], 0, 0), Some(1));
    assert_eq!(reference_load_len(&[0x19, 0x04], 0, 4), Some(2));
    assert!(reference_load_len(&[0x2a], 0, 1).is_none());
    assert!(reference_load_len(&[0xc4, 0x19, 0, 0], 0, 0).is_none());
}

#[test]
fn lambda_inputs_may_repeat_and_follow_explicit_bytecode_order() {
    let arguments = [
        LambdaArgument::Reference,
        LambdaArgument::Primitive(PrimitiveKind::Int),
        LambdaArgument::Primitive(PrimitiveKind::Long),
    ];
    let bytecode = [0x2a, 0xb4, 0, 1, 0x1b, 0x2a, 0xb4, 0, 2, 0x20, 0xb8];
    assert_eq!(
        lambda_input_load(&bytecode, 0, &arguments),
        Some((0, LambdaArgument::Reference, 1))
    );
    assert_eq!(
        lambda_input_load(&bytecode, 4, &arguments),
        Some((1, LambdaArgument::Primitive(PrimitiveKind::Int), 1))
    );
    assert_eq!(
        lambda_input_load(&bytecode, 5, &arguments),
        Some((0, LambdaArgument::Reference, 1))
    );
    assert_eq!(
        lambda_input_load(&bytecode, 9, &arguments),
        Some((2, LambdaArgument::Primitive(PrimitiveKind::Long), 1))
    );
    assert!(lambda_input_load(&[0x1c], 0, &arguments).is_none());
    assert!(lambda_input_load(&[0x2b], 0, &arguments).is_none());
}

#[test]
fn captured_reference_field_chains_materialize_only_to_scalar_cells() {
    let mut vm = Vm::new(crate::ClassLoader::new());
    let seed = vm.resolve_field_id("Config", "seed");
    let field = vm.resolve_field_id("Config", "trip");
    let config = vm.heap.alloc("Config");
    vm.heap.put_field_hint(config, seed, Value::Int(5), 0);
    vm.heap.put_field_hint(config, field, Value::Int(17), 0);
    let config_field = vm.resolve_field_id("Holder", "config");
    let holder = vm.heap.alloc("Holder");
    vm.heap
        .put_field_hint(holder, config_field, Value::Ref(config), 0);
    let site = Arc::new(LambdaSite::new(
        Arc::from("Owner"),
        Arc::from("lambda"),
        Arc::from("(LHolder;)Ljava/lang/Long;"),
    ));
    let lambda = vm.heap.alloc_lambda(Lambda {
        kind: 6,
        site: Arc::clone(&site),
        captured: vec![Value::Ref(holder)],
    });
    let plan = DetachedTaskPlan {
        entry: 0,
        frame_len: 2,
        result: PrimitiveKind::Long,
        inputs: vec![
            ScalarInput {
                source: InputSource::LambdaCaptureFields {
                    capture: 0,
                    fields: Arc::from([
                        ResolvedField {
                            owner: Arc::from("Holder"),
                            id: config_field,
                        },
                        ResolvedField {
                            owner: Arc::from("Config"),
                            id: seed,
                        },
                    ]),
                },
                kind: PrimitiveKind::Int,
            },
            ScalarInput {
                source: InputSource::LambdaCaptureFields {
                    capture: 0,
                    fields: Arc::from([
                        ResolvedField {
                            owner: Arc::from("Holder"),
                            id: config_field,
                        },
                        ResolvedField {
                            owner: Arc::from("Config"),
                            id: field,
                        },
                    ]),
                },
                kind: PrimitiveKind::Int,
            },
        ],
        post: PostInvoke {
            owner: String::new(),
            name: String::new(),
            descriptor: String::new(),
        },
    };

    let invocation = plan
        .materialize(&mut vm, &Value::Ref(lambda))
        .expect("primitive field snapshot");
    assert_eq!(invocation.cells, vec![5, 17]);

    let wrong_class_lambda = vm.heap.alloc_lambda(Lambda {
        kind: 6,
        site: Arc::clone(&site),
        captured: vec![Value::Ref(config)],
    });
    assert!(plan
        .materialize(&mut vm, &Value::Ref(wrong_class_lambda))
        .is_none());

    let null_lambda = vm.heap.alloc_lambda(Lambda {
        kind: 6,
        site,
        captured: vec![Value::Null],
    });
    assert!(plan
        .materialize(&mut vm, &Value::Ref(null_lambda))
        .is_none());
}

#[test]
fn numeric_body_proof_rejects_callbacks_heap_and_throwing_division() {
    assert!(detachable_body(
        &code(vec![0x09, 0x3f, 0x1a, 0x85, 0x1f, 0x61, 0xad]),
        PrimitiveKind::Long
    ));
    for forbidden in [0xb8, 0xb4, 0xb2, 0xbb, 0x2e, 0x4f, 0xbf, 0xc2, 0x6c, 0x71] {
        assert!(!detachable_body(
            &code(vec![forbidden, 0xad]),
            PrimitiveKind::Long
        ));
    }
}

#[test]
fn return_type_and_cfg_must_close() {
    assert!(!detachable_body(
        &code(vec![0x03, 0xac]),
        PrimitiveKind::Long
    ));
    assert!(!detachable_body(
        &code(vec![0x03, 0x99, 0x7f, 0xff, 0x85, 0xad]),
        PrimitiveKind::Long
    ));
}
