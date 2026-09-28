//! Array element access and length.
//!
//! Arm bodies are unchanged; the preamble restores every name they were written against.

use super::Ops;
use crate::jit::*;

pub(crate) fn arrays(o: &mut Ops, pc: usize, op: u8) -> Option<()> {
    let b = &mut *o.b;
    let stack = &mut *o.stack;
    let env = o.env;
    let array_desc_cell = o.array_desc_cell;
    let cow = o.cow;
    match op {
        0x2e | 0x33 | 0x34 | 0x35 | 0x2f | 0x30 | 0x31 => {
            // Primitive array load via the interpreter callback. (A native flat-load
            // path diverged from the interpreter on the datapack codec — it read
            // wrong element data on some arrays despite matching desc/addressing in
            // review — so loads stay on the proven callback; re-enable for native
            // benchmarking only with RAVA_NATIVE_LOADS, NOT in production.)
            let jt = match op {
                0x2f => JT::Long,
                0x30 => JT::Float,
                0x31 => JT::Double,
                _ => JT::Int,
            };
            let idx = stack.pop()?;
            let arr = stack.pop()?;
            // Native inline loads: DEFAULT ON for next-gen-gc builds — the historical
            // "datapack codec divergence" that kept this path disabled was NOT the
            // loads (RAVA_VERIFY_LOADS cross-checked every native load bit-identical
            // on the chunk/DFU repros; the real corruption was nio path_str dropping
            // ArenaString filenames). Validated: 206 tests green, RegionRead5/7
            // bit-exact, full MC noise-world boot to Done with 937M aload callbacks
            // reduced to 372k. RAVA_NATIVE_LOADS=0 disables; a letter set
            // (b=baload c=caload s=saload i=iaload l=laload f=faload d=daload)
            // enables selectively (divergence bisect). Non-feature builds keep the
            // proven callback default (byte-identical mandate).
            let native_on = {
                static V: std::sync::OnceLock<String> = std::sync::OnceLock::new();
                let v = V.get_or_init(|| std::env::var("RAVA_NATIVE_LOADS").unwrap_or_default());
                match v.as_str() {
                    "" => cfg!(feature = "next-gen-gc"),
                    "0" | "off" | "false" | "no" => false,
                    "1" => true,
                    sel => {
                        let c = match op {
                            0x33 => 'b',
                            0x34 => 'c',
                            0x35 => 's',
                            0x2f => 'l',
                            0x30 => 'f',
                            0x31 => 'd',
                            _ => 'i',
                        };
                        sel.contains(c)
                    }
                }
            };
            if native_on {
                let (lt, sh, sx) = match op {
                    0x33 => (types::I8, 0, true), // baload: byte, sign-extend
                    0x2f => (I64, 3, false),
                    0x30 => (F32, 2, false),
                    0x31 => (F64, 3, false),
                    _ => (I32, 2, false), // i/c/s aload: 32-bit slot
                };
                #[cfg(feature = "next-gen-gc")]
                let elem = if arena_primitive_arrays_enabled() {
                    native_primitive_load_arena_dual(
                        b,
                        env,
                        array_desc_cell,
                        arr.v,
                        idx.v,
                        lt,
                        sh,
                        sx,
                        env.bce_safe.contains(&pc),
                        pc as i64,
                        op,
                    )
                } else {
                    native_primitive_load(
                        b,
                        env,
                        array_desc_cell,
                        arr.v,
                        idx.v,
                        lt,
                        sh,
                        sx,
                        env.bce_safe.contains(&pc),
                        pc as i64,
                    )
                };
                #[cfg(not(feature = "next-gen-gc"))]
                let elem = native_primitive_load(
                    b,
                    env,
                    array_desc_cell,
                    arr.v,
                    idx.v,
                    lt,
                    sh,
                    sx,
                    env.bce_safe.contains(&pc),
                    pc as i64,
                );
                push(stack, elem, jt);
            } else {
                let idxs = b.ins().sextend(I64, idx.v);
                let is_ref = b.ins().iconst(I64, 0);
                let call = b.ins().call(env.aload, &[arr.v, idxs, is_ref]);
                let res = b.inst_results(call)[0];
                let sv = cell_to_sv(b, res, jt);
                stack.push(sv);
            }
        }
        // aaload — reference array, now a flat tagged-cell buffer: native load +
        // tag check (ABOX elements route to the callback to box them).
        0x32 => {
            let idx = stack.pop()?;
            let arr = stack.pop()?;
            let cell = native_aaload(b, env, array_desc_cell, arr.v, idx.v, env.bce_safe.contains(&pc));
            push(stack, cell, JT::Ref);
        }

        // Primitive array stores — NATIVE: inline bounds-checked indexed store,
        // no callback (fault latch suppresses stores after a pending exception).
        // iastore/castore/sastore → i32; bastore → byte; lastore → i64; fastore →
        // f32; dastore → f64. aastore (0x53) is excluded (reference store bails).
        0x4f | 0x50 | 0x51 | 0x52 | 0x54 | 0x55 | 0x56 => {
            let val = stack.pop()?;
            let idx = stack.pop()?;
            let arr = stack.pop()?;
            let (sval, shift) = match op {
                0x54 => (b.ins().ireduce(types::I8, val.v), 0), // bastore: low byte
                0x50 => (val.v, 3),                             // lastore: i64
                0x51 => (val.v, 2),                             // fastore: f32
                0x52 => (val.v, 3),                             // dastore: f64
                _ => (val.v, 2), // iastore/castore/sastore: i32 slot
            };
            let guard = emit_ring_store_guard(b, env, array_desc_cell, cow, arr.v);
            // LANE STORE-FORM CENSUS (diagnostic only, off unless RAVA_LANE_STORE_CENSUS):
            // `guard.is_some()` is exactly the ring-diamond decision, so `None`'s two
            // causes (rings gate off = config; COW Disarmed = unswitch settled) stay
            // separable via the recorded arm state.
            #[cfg(feature = "next-gen-gc")]
            crate::jit::emit::store_census::record_array_store(
                env,
                pc,
                op,
                guard.is_some(),
                cow,
            );
            #[cfg(feature = "next-gen-gc")]
            if arena_primitive_arrays_enabled() {
                native_primitive_store_arena_dual(
                    b,
                    env,
                    array_desc_cell,
                    arr.v,
                    idx.v,
                    sval,
                    shift,
                    op,
                );
            } else {
                native_primitive_store(b, env, array_desc_cell, arr.v, idx.v, sval, shift);
            }
            #[cfg(not(feature = "next-gen-gc"))]
            native_primitive_store(b, env, array_desc_cell, arr.v, idx.v, sval, shift);
            close_ring_store_guard(b, guard);
        }
        // aastore — reference array store (flat tagged cells).
        0x53 => {
            let val = stack.pop()?;
            let idx = stack.pop()?;
            let arr = stack.pop()?;
            let guard = emit_ring_store_guard(b, env, array_desc_cell, cow, arr.v);
            // LANE STORE-FORM CENSUS — same `None`-cause split as the primitive stores.
            #[cfg(feature = "next-gen-gc")]
            crate::jit::emit::store_census::record_array_store(
                env,
                pc,
                op,
                guard.is_some(),
                cow,
            );
            native_aastore(b, env, array_desc_cell, arr.v, idx.v, val.v);
            close_ring_store_guard(b, guard);
        }

        // arraylength — NATIVE: load len from the flat descriptor, no callback.
        0xbe => {
            let arr = stack.pop()?;
            #[cfg(feature = "next-gen-gc")]
            let len = if arena_reference_arrays_enabled() {
                native_reference_arraylength_dual(b, env, array_desc_cell, arr.v)
            } else if arena_primitive_arrays_enabled() {
                native_arraylength_arena_dual(b, env, array_desc_cell, arr.v)
            } else {
                native_arraylength(b, env, array_desc_cell, arr.v)
            };
            #[cfg(not(feature = "next-gen-gc"))]
            let len = native_arraylength(b, env, array_desc_cell, arr.v);
            push(stack, len, JT::Int);
        }
        // ldc / ldc_w — numeric inline; a ref constant (String→ArenaString / Class)
        // resolves via the `rava_ldc` callback under next-gen-gc, else bails.
        _ => return None,
    }
    Some(())
}
