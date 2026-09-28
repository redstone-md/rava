//! Dual consumers for opt-in arena primitive arrays.
//!
//! The producer is admitted from bytecode/CFG facts. These consumers route only by the
//! resulting exact class ids and preserve the historical tagged-array emitters as their
//! complete fallback.

#[cfg(feature = "next-gen-gc")]
use crate::jit::*;

/// Shared producer/consumer gate for the arena primitive-array tracer bullet.
pub(crate) fn arena_primitive_arrays_enabled() -> bool {
    #[cfg(feature = "next-gen-gc")]
    {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| {
            let native_loads = std::env::var("RAVA_NATIVE_LOADS").unwrap_or_default();
            std::env::var("RAVA_ARENA_PRIM_ARRAYS").ok().as_deref() == Some("1")
                && emit_array()
                && matches!(native_loads.as_str(), "" | "1")
        })
    }
    #[cfg(not(feature = "next-gen-gc"))]
    {
        false
    }
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_is_raw_arena_shape(b: &mut FunctionBuilder, arr: Value) -> Value {
    // Host callbacks additionally use the dynamic arena high-water bound. Emitted code
    // first proves the static address shape and then checks an exact CID before payload use.
    let non_null = b.ins().icmp_imm(IntCC::NotEqual, arr, 0);
    let floor = b.ins().iconst(I64, crate::nextgen::ARENA_FLOOR as i64);
    let above_floor = b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, arr, floor);
    let hi = b.ins().ushr_imm(arr, 40);
    let below_ceiling = b.ins().icmp_imm(IntCC::Equal, hi, 0);
    let low_bits = b.ins().band_imm(arr, 7);
    let aligned = b.ins().icmp_imm(IntCC::Equal, low_bits, 0);
    let range = b.ins().band(non_null, above_floor);
    let range = b.ins().band(range, below_ceiling);
    b.ins().band(range, aligned)
}

/// Route a raw arena reference array to the representation-aware callback while preserving the
/// existing descriptor load for tagged arrays. The callback validates the exact CID before it
/// reads `nfields`; the static address-shape test alone is never treated as an array proof.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn native_reference_arraylength_dual(
    b: &mut FunctionBuilder,
    env: &Env,
    desc_cell: Value,
    arr: Value,
) -> Value {
    debug_assert!(arena_reference_arrays_enabled());
    let raw = b.create_block();
    let legacy = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, I32);

    let shape = emit_is_raw_arena_shape(b, arr);
    b.ins().brif(shape, raw, &[], legacy, &[]);

    b.switch_to_block(raw);
    b.seal_block(raw);
    let call = b.ins().call(env.arrlen, &[arr]);
    let raw_len = b.inst_results(call)[0];
    let len = b.ins().ireduce(I32, raw_len);
    b.ins().jump(merge, &[len.into()]);

    b.switch_to_block(legacy);
    b.seal_block(legacy);
    let len = native_arraylength(b, env, desc_cell, arr);
    b.ins().jump(merge, &[len.into()]);

    b.switch_to_block(merge);
    b.seal_block(merge);
    b.block_params(merge)[0]
}

#[cfg(feature = "next-gen-gc")]
fn emit_raw_cid_test(b: &mut FunctionBuilder, cid: Value, atypes: &[u8]) -> Value {
    let mut matched = b.ins().iconst(I8, 0);
    for &atype in atypes {
        let registered = crate::nextgen::primitive_array_cid(atype)
            .expect("primitive-array opcode carries a valid atype");
        let expected = b.ins().iconst(I32, registered as i64);
        let this = b.ins().icmp(IntCC::Equal, cid, expected);
        matched = b.ins().bor(matched, this);
    }
    matched
}

#[cfg(feature = "next-gen-gc")]
fn raw_atypes_for_opcode(op: u8) -> &'static [u8] {
    match op {
        0x2e | 0x4f => &[10],   // int
        0x2f | 0x50 => &[11],   // long
        0x30 | 0x51 => &[6],    // float
        0x31 | 0x52 => &[7],    // double
        0x33 | 0x54 => &[4, 8], // boolean or byte
        0x34 | 0x55 => &[5],    // char
        0x35 | 0x56 => &[9],    // short
        _ => &[],
    }
}

#[cfg(feature = "next-gen-gc")]
fn raw_result_type(op: u8) -> Type {
    match op {
        0x2f => I64,
        0x30 => F32,
        0x31 => F64,
        _ => I32,
    }
}

#[cfg(feature = "next-gen-gc")]
fn emit_raw_load_value(b: &mut FunctionBuilder, raw: Value, cid: Value, op: u8) -> Value {
    match op {
        0x2e => b.ins().ireduce(I32, raw),
        0x2f => raw,
        0x30 => {
            let bits = b.ins().ireduce(I32, raw);
            b.ins().bitcast(F32, MemFlags::new(), bits)
        }
        0x31 => b.ins().bitcast(F64, MemFlags::new(), raw),
        0x33 => {
            let byte = b.ins().ireduce(I8, raw);
            let signed = b.ins().sextend(I32, byte);
            let boolean_cid = crate::nextgen::primitive_array_cid(4)
                .expect("boolean primitive-array CID initialized");
            let expected = b.ins().iconst(I32, boolean_cid as i64);
            let is_boolean = b.ins().icmp(IntCC::Equal, cid, expected);
            let bit = b.ins().band_imm(raw, 1);
            let boolean = b.ins().ireduce(I32, bit);
            b.ins().select(is_boolean, boolean, signed)
        }
        0x34 => {
            let value = b.ins().ireduce(types::I16, raw);
            b.ins().uextend(I32, value)
        }
        0x35 => {
            let value = b.ins().ireduce(types::I16, raw);
            b.ins().sextend(I32, value)
        }
        _ => unreachable!("raw primitive load opcode"),
    }
}

#[cfg(feature = "next-gen-gc")]
fn emit_raw_store_cell(b: &mut FunctionBuilder, val: Value, cid: Value, op: u8) -> Value {
    match op {
        0x4f => b.ins().uextend(I64, val),
        0x50 => val,
        0x51 => {
            let bits = b.ins().bitcast(I32, MemFlags::new(), val);
            b.ins().uextend(I64, bits)
        }
        0x52 => b.ins().bitcast(I64, MemFlags::new(), val),
        0x54 => {
            let byte = b.ins().uextend(I64, val);
            let boolean_cid = crate::nextgen::primitive_array_cid(4)
                .expect("boolean primitive-array CID initialized");
            let expected = b.ins().iconst(I32, boolean_cid as i64);
            let is_boolean = b.ins().icmp(IntCC::Equal, cid, expected);
            let boolean = b.ins().band_imm(byte, 1);
            b.ins().select(is_boolean, boolean, byte)
        }
        0x55 | 0x56 => {
            let narrow = b.ins().ireduce(types::I16, val);
            b.ins().uextend(I64, narrow)
        }
        _ => unreachable!("raw primitive store opcode"),
    }
}

#[cfg(feature = "next-gen-gc")]
fn emit_verify_raw_load(
    b: &mut FunctionBuilder,
    env: &Env,
    arr: Value,
    idx: Value,
    elem: Value,
    op: u8,
    site: i64,
) {
    static VERIFY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*VERIFY.get_or_init(|| std::env::var_os("RAVA_VERIFY_LOADS").is_some()) {
        return;
    }
    let jt = match op {
        0x2f => JT::Long,
        0x30 => JT::Float,
        0x31 => JT::Double,
        _ => JT::Int,
    };
    let packed = to_cell(
        b,
        SV {
            v: elem,
            t: jt,
            src: None,
            arena: false,
        },
    );
    let idx = b.ins().sextend(I64, idx);
    let site = b.ins().iconst(I64, site);
    b.ins().call(env.aload_verify, &[arr, idx, packed, site]);
}

/// Read nfields as the length for any of the eight exact primitive-array CIDs.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn native_arraylength_arena_dual(
    b: &mut FunctionBuilder,
    env: &Env,
    desc_cell: Value,
    arr: Value,
) -> Value {
    debug_assert!(arena_primitive_arrays_enabled());
    let raw_header = b.create_block();
    let legacy = b.create_block();
    let raw = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, I32);

    let shape = emit_is_raw_arena_shape(b, arr);
    b.ins().brif(shape, raw_header, &[], legacy, &[]);

    b.switch_to_block(raw_header);
    b.seal_block(raw_header);
    let header = b.ins().load(
        I64,
        MemFlags::trusted().with_readonly(),
        arr,
        Offset32::new(0),
    );
    let cid = b.ins().ireduce(I32, header);
    let exact = emit_raw_cid_test(b, cid, &[4, 5, 6, 7, 8, 9, 10, 11]);
    b.ins().brif(exact, raw, &[], legacy, &[]);
    b.seal_block(legacy);

    b.switch_to_block(legacy);
    let len = native_arraylength(b, env, desc_cell, arr);
    b.ins().jump(merge, &[len.into()]);

    b.switch_to_block(raw);
    b.seal_block(raw);
    let len = b.ins().ushr_imm(header, 32);
    let len = b.ins().ireduce(I32, len);
    b.ins().jump(merge, &[len.into()]);

    b.switch_to_block(merge);
    b.seal_block(merge);
    b.block_params(merge)[0]
}

/// Load one exact raw primitive-array cell, or run the tagged-array path unchanged.
#[allow(clippy::too_many_arguments)]
#[cfg(feature = "next-gen-gc")]
pub(crate) fn native_primitive_load_arena_dual(
    b: &mut FunctionBuilder,
    env: &Env,
    desc_cell: Value,
    arr: Value,
    idx: Value,
    load_ty: Type,
    shift: i64,
    signext_byte: bool,
    skip_bounds: bool,
    site: i64,
    op: u8,
) -> Value {
    debug_assert!(arena_primitive_arrays_enabled());
    let result_ty = raw_result_type(op);
    let raw_header = b.create_block();
    let legacy = b.create_block();
    let wrong_cid = b.create_block();
    let raw_bounds = b.create_block();
    let raw_ok = b.create_block();
    let raw_fault = (!skip_bounds).then(|| b.create_block());
    let merge = b.create_block();
    b.append_block_param(merge, result_ty);

    let shape = emit_is_raw_arena_shape(b, arr);
    b.ins().brif(shape, raw_header, &[], legacy, &[]);

    b.switch_to_block(raw_header);
    b.seal_block(raw_header);
    let header = b.ins().load(
        I64,
        MemFlags::trusted().with_readonly(),
        arr,
        Offset32::new(0),
    );
    let cid = b.ins().ireduce(I32, header);
    let exact = emit_raw_cid_test(b, cid, raw_atypes_for_opcode(op));
    b.ins().brif(exact, raw_bounds, &[], wrong_cid, &[]);

    b.switch_to_block(wrong_cid);
    b.seal_block(wrong_cid);
    let idx64 = b.ins().sextend(I64, idx);
    let is_ref = b.ins().iconst(I64, 0);
    let call = b.ins().call(env.aload, &[arr, idx64, is_ref]);
    let packed = b.inst_results(call)[0];
    let value = if result_ty == I32 {
        b.ins().ireduce(I32, packed)
    } else if result_ty == F32 {
        let bits = b.ins().ireduce(I32, packed);
        b.ins().bitcast(F32, MemFlags::new(), bits)
    } else if result_ty == F64 {
        b.ins().bitcast(F64, MemFlags::new(), packed)
    } else {
        packed
    };
    b.ins().jump(merge, &[value.into()]);

    b.switch_to_block(legacy);
    b.seal_block(legacy);
    let value = native_primitive_load(
        b,
        env,
        desc_cell,
        arr,
        idx,
        load_ty,
        shift,
        signext_byte,
        skip_bounds,
        site,
    );
    b.ins().jump(merge, &[value.into()]);

    b.switch_to_block(raw_bounds);
    b.seal_block(raw_bounds);
    let len = b.ins().ushr_imm(header, 32);
    let len = b.ins().ireduce(I32, len);
    if let Some(fault) = raw_fault {
        let oob = b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, idx, len);
        b.ins().brif(oob, fault, &[], raw_ok, &[]);
    } else {
        b.ins().jump(raw_ok, &[]);
    }

    b.switch_to_block(raw_ok);
    b.seal_block(raw_ok);
    let idx64 = b.ins().uextend(I64, idx);
    let offset = b.ins().ishl_imm(idx64, 3);
    let cells = b.ins().iadd_imm(arr, crate::nextgen::HDR as i64);
    let address = b.ins().iadd(cells, offset);
    let raw = b
        .ins()
        .load(I64, MemFlags::trusted(), address, Offset32::new(0));
    let value = emit_raw_load_value(b, raw, cid, op);
    emit_verify_raw_load(b, env, arr, idx, value, op, site);
    b.ins().jump(merge, &[value.into()]);

    if let Some(fault) = raw_fault {
        b.switch_to_block(fault);
        b.seal_block(fault);
        let idx64 = b.ins().sextend(I64, idx);
        b.ins().call(env.array_fault, &[arr, idx64]);
        // Match the hardened legacy load: once the Java exception is latched, no
        // bytecode after this access may execute in the compiled caller or an inline.
        let zero = abi_zero(b);
        b.ins().return_(&[zero]);
    }

    b.switch_to_block(merge);
    b.seal_block(merge);
    b.block_params(merge)[0]
}

/// Store one JVM-narrowed raw primitive-array cell, or use the tagged-array emitter.
#[allow(clippy::too_many_arguments)]
#[cfg(feature = "next-gen-gc")]
pub(crate) fn native_primitive_store_arena_dual(
    b: &mut FunctionBuilder,
    env: &Env,
    ctx_cell: Value,
    arr: Value,
    idx: Value,
    val: Value,
    shift: i64,
    op: u8,
) {
    debug_assert!(arena_primitive_arrays_enabled());
    let proceed = b.create_block();
    let raw_header = b.create_block();
    let legacy = b.create_block();
    let wrong_cid = b.create_block();
    let raw_bounds = b.create_block();
    let raw_ok = b.create_block();
    let raw_fault = b.create_block();
    let done = b.create_block();

    let pending = b
        .ins()
        .load(I32, MemFlags::trusted(), ctx_cell, Offset32::new(8));
    let already = b.ins().icmp_imm(IntCC::NotEqual, pending, 0);
    b.ins().brif(already, done, &[], proceed, &[]);

    b.switch_to_block(proceed);
    b.seal_block(proceed);
    let shape = emit_is_raw_arena_shape(b, arr);
    b.ins().brif(shape, raw_header, &[], legacy, &[]);

    b.switch_to_block(raw_header);
    b.seal_block(raw_header);
    let header = b.ins().load(
        I64,
        MemFlags::trusted().with_readonly(),
        arr,
        Offset32::new(0),
    );
    let cid = b.ins().ireduce(I32, header);
    let exact = emit_raw_cid_test(b, cid, raw_atypes_for_opcode(op));
    b.ins().brif(exact, raw_bounds, &[], wrong_cid, &[]);

    b.switch_to_block(wrong_cid);
    b.seal_block(wrong_cid);
    // The generic callback preserves the historical NPE for a wrong raw CID.
    let idx64 = b.ins().sextend(I64, idx);
    let is_ref = b.ins().iconst(I64, 0);
    b.ins().call(env.aload, &[arr, idx64, is_ref]);
    b.ins().jump(done, &[]);

    b.switch_to_block(legacy);
    b.seal_block(legacy);
    native_primitive_store(b, env, ctx_cell, arr, idx, val, shift);
    b.ins().jump(done, &[]);

    b.switch_to_block(raw_bounds);
    b.seal_block(raw_bounds);
    let len = b.ins().ushr_imm(header, 32);
    let len = b.ins().ireduce(I32, len);
    let oob = b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, idx, len);
    b.ins().brif(oob, raw_fault, &[], raw_ok, &[]);

    b.switch_to_block(raw_ok);
    b.seal_block(raw_ok);
    let idx64 = b.ins().uextend(I64, idx);
    let offset = b.ins().ishl_imm(idx64, 3);
    let cells = b.ins().iadd_imm(arr, crate::nextgen::HDR as i64);
    let address = b.ins().iadd(cells, offset);
    let cell = emit_raw_store_cell(b, val, cid, op);
    b.ins()
        .store(MemFlags::trusted(), cell, address, Offset32::new(0));
    b.ins().jump(done, &[]);

    b.switch_to_block(raw_fault);
    b.seal_block(raw_fault);
    let idx64 = b.ins().sextend(I64, idx);
    b.ins().call(env.array_fault, &[arr, idx64]);
    // Do not inherit the legacy store's continuation gap: abrupt Java exceptions
    // must prevent later side effects even after this array becomes dead in the plan.
    let zero = abi_zero(b);
    b.ins().return_(&[zero]);

    b.switch_to_block(done);
    b.seal_block(done);
}
