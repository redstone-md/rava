//! Which arithmetic shapes a fork may contain, and why one is refused.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::offload::analyze::*;

/// Model the stack effect of the "everything else" opcodes — arithmetic, control
/// flow, fields, invokes, allocation. Mutates `stack`/`locals`; returns Err with a
/// reason to fail the analysis (an unmodelled op, a shared-target write, a
/// non-offloadable callee). `pc` is NOT advanced here (the caller does).
#[allow(clippy::too_many_arguments)]
pub(crate) fn arith_or_reject(
    op: u8,
    bc: &[u8],
    pc: usize,
    pool: &ConstantPool,
    bsm: Option<&[BootstrapMethod]>,
    stack: &mut Vec<F>,
    relaxed: bool,
    txn: bool,
    self_class: Option<&str>,
    callee: &mut dyn FnMut(CalleeQuery) -> bool,
    #[cfg(test)] effects: &mut Option<&mut dyn FnMut(crate::offload::effect_summary::EffectFact)>,
) -> Result<(), OffReject> {
    macro_rules! pop1 {
        () => {
            match stack.pop() {
                Some(f) => f,
                None => return Err(OffReject::Malformed),
            }
        };
    }
    macro_rules! push_shared {
        ($n:expr) => {
            for _ in 0..$n {
                stack.push(F::Shared);
            }
        };
    }
    #[cfg(test)]
    macro_rules! check_callee {
        ($query:expr) => {{
            let query = $query;
            crate::offload::effect_summary::observe_query(effects, pc, query);
            callee(query)
        }};
    }
    #[cfg(not(test))]
    macro_rules! check_callee {
        ($query:expr) => {
            callee($query)
        };
    }
    match op {
        // int binary (pop2 slots, push1): add/sub/mul/div/rem/shl/shr/ushr/and/or/xor
        0x60 | 0x64 | 0x68 | 0x6c | 0x70 | 0x78 | 0x7a | 0x7c | 0x7e | 0x80 | 0x82 => {
            pop1!();
            pop1!();
            push_shared!(1);
        }
        // long binary add/sub/mul/div/rem/and/or/xor (pop 4 slots, push 2)
        0x61 | 0x65 | 0x69 | 0x6d | 0x71 | 0x7f | 0x81 | 0x83 => {
            for _ in 0..4 {
                pop1!();
            }
            push_shared!(2);
        }
        // long shifts lshl/lshr/lushr (pop long 2 + int 1 = 3, push 2)
        0x79 | 0x7b | 0x7d => {
            for _ in 0..3 {
                pop1!();
            }
            push_shared!(2);
        }
        // float binary (pop2, push1)
        0x62 | 0x66 | 0x6a | 0x6e | 0x72 => {
            pop1!();
            pop1!();
            push_shared!(1);
        }
        // double binary (pop4, push2)
        0x63 | 0x67 | 0x6b | 0x6f | 0x73 => {
            for _ in 0..4 {
                pop1!();
            }
            push_shared!(2);
        }
        // negates: ineg/fneg (1->1), lneg/dneg (2->2)
        0x74 | 0x76 => { /* pop1 push1 same width: net 0 */ }
        0x75 | 0x77 => { /* pop2 push2: net 0 */ }
        0x84 => { /* iinc: no stack change */ }
        // conversions
        0x85 | 0x87 => {
            pop1!();
            push_shared!(2);
        } // i2l / i2d (1->2)
        0x86 => {
            pop1!();
            push_shared!(1);
        } // i2f
        0x88 | 0x89 => {
            pop1!();
            pop1!();
            push_shared!(1);
        } // l2i / l2f (2->1)
        0x8a => { /* l2d: 2->2 */ }
        0x8b => {
            pop1!();
            push_shared!(1);
        } // f2i
        0x8c | 0x8d => {
            pop1!();
            push_shared!(2);
        } // f2l / f2d (1->2)
        0x8e | 0x90 => {
            pop1!();
            pop1!();
            push_shared!(1);
        } // d2i / d2f (2->1)
        0x8f => { /* d2l: 2->2 */ }
        0x91 | 0x92 | 0x93 => { /* i2b/i2c/i2s: 1->1 */ }
        // compares
        0x94 => {
            for _ in 0..4 {
                pop1!();
            }
            push_shared!(1);
        } // lcmp
        0x95 | 0x96 => {
            pop1!();
            pop1!();
            push_shared!(1);
        } // fcmpl/fcmpg
        0x97 | 0x98 => {
            for _ in 0..4 {
                pop1!();
            }
            push_shared!(1);
        } // dcmpl/dcmpg
        // conditional branches pop their operand(s); the target join is handled by
        // the reset. if<cond> pop1; if_icmp/if_acmp pop2; ifnull/ifnonnull pop1.
        0x99..=0x9e | 0xc6 | 0xc7 => {
            pop1!();
        }
        0x9f..=0xa6 => {
            pop1!();
            pop1!();
        }
        0xa7 | 0xc8 => {} // goto / goto_w
        0xaa | 0xab => {
            pop1!();
        } // tableswitch / lookupswitch (key)
        // returns
        0xac | 0xae | 0xb0 => {
            pop1!();
        } // ireturn / freturn / areturn
        0xad | 0xaf => {
            pop1!();
            pop1!();
        } // lreturn / dreturn
        0xb1 => {} // return
        // fields
        0xb2 => {
            // getstatic: push the field width (Shared).
            #[cfg(test)]
            crate::offload::effect_summary::observe_field(effects, bc, pc, op, pool, None);
            let w = field_width(pool, u16be(bc, pc + 1));
            if txn {
                // FieldTxn mode: a static read reads state the txn cannot journal
                // or isolate — rejected regardless of freshness (statics are never
                // private to the transaction frame).
                return Err(OffReject::SharedRead(pc, op));
            }
            push_shared!(w);
        }
        0xb3 => {
            #[cfg(test)]
            crate::offload::effect_summary::observe_field(effects, bc, pc, op, pool, None);
            return Err(OffReject::Opcode(pc, op)); // putstatic — always shared
        }
        0xb4 => {
            // getfield: pop objref, push field width (Shared).
            let recv = pop1!();
            #[cfg(test)]
            crate::offload::effect_summary::observe_field(effects, bc, pc, op, pool, Some(recv));
            if txn && !matches!(recv, F::Fresh(_) | F::This) {
                // FieldTxn mode: reading a SHARED object's field is a shared
                // read the txn cannot journal. Fresh/owned `this` receivers
                // are private — their fields are invisible to other threads.
                return Err(OffReject::SharedRead(pc, op));
            }
            let w = field_width(pool, u16be(bc, pc + 1));
            push_shared!(w);
        }
        0xb5 => {
            // putfield: pop value (field width) then objref; write allowed iff the
            // OBJECT is fresh (or relaxed ceiling probe).
            let w = field_width(pool, u16be(bc, pc + 1));
            for _ in 0..w {
                pop1!();
            }
            let obj = pop1!();
            #[cfg(test)]
            crate::offload::effect_summary::observe_field(effects, bc, pc, op, pool, Some(obj));
            // M3 COW RELAX: under RAVA_FORK_COW a putfield to a SHARED object is no longer
            // a hopeless publish — the compiled/interp field hooks redirect the worker's
            // write to its thread-local shadow (inc 1/2), so it neither races nor escapes.
            // The write becomes COW-isolated. Sound BECAUSE only bytecode putfield is
            // shadowed: putstatic, array-store (no array shadow yet), and receiver-mutating
            // natives on shared receivers stay REJECTED below — their side effects are NOT
            // isolable. Gate off (default) still hard-blocks the shared putfield → sterile.
            // FieldTxn (txn) mode: a shared instance-field write is the shape the mode
            // exists to admit — the txn runtime JOURNALS it for commit/abort replay — so
            // the shared-target rejection is lifted (relaxed-style) while putstatic,
            // shared array stores and shared reads stay rejected. Fresh-target writes
            // keep passing for the same reason as in every mode.
            if !txn
                && !relaxed
                && !crate::parallel::cow_gate()
                && !matches!(obj, F::Fresh(_) | F::This)
            {
                return Err(OffReject::SharedWrite(pc, op));
            }
        }
        // invokes: pop args (descriptor slot widths) then the receiver (non-static),
        // decide offloadability, push the return width (Shared). Offloadable iff:
        // (a) a RECEIVER-MUTATING native on a FRESH (private) receiver — it mutates
        //     only `this`, and a fresh `this` is unpublished, so no shared write; OR
        // (b) the callee's subtree is offloadable (pure native / offloadable bytecode).
        0xb6 | 0xb7 | 0xb8 | 0xb9 => {
            let idx = u16be(bc, pc + 1);
            let Some(m) = pool.member_ref(idx) else {
                return Err(OffReject::Malformed);
            };
            let (arg_slots, ret_slots) = desc_slots(m.descriptor);
            let is_static = op == 0xb8;
            let is_special = op == 0xb7;
            for _ in 0..arg_slots {
                pop1!();
            }
            let recv = if is_static { F::Shared } else { pop1!() };
            let recv_private = matches!(recv, F::Fresh(_) | F::This);
            #[cfg(test)]
            crate::offload::effect_summary::observe_call(effects, pc, op, &m, recv);
            // GILFREE-RINGS: a SAM invoke on a STILL-FRESH lambda dispatches to
            // exactly the indy site's impl handle (rava's indy service links that
            // handle) — query the impl instead of the unenumerable functional-
            // interface closure. static/special impls are exact by construction;
            // a virtual/interface impl dispatches on the first CAPTURE (unknown
            // type) → the ordinary closure query over the impl owner (may reject).
            if matches!(op, 0xb6 | 0xb9) {
                if let F::Lambda(site_idx) = recv {
                    if let Some((kind, io, inm, id)) = lmf_indy_impl(pool, bsm, site_idx) {
                        let ok = match kind {
                            6 | 7 => check_callee!(CalleeQuery::exact(io, inm, id)),
                            _ => check_callee!(CalleeQuery::closure(io, inm, id)),
                        };
                        if !ok {
                            return Err(OffReject::Callee(format!("{io}.{inm} (lambda impl)")));
                        }
                        push_shared!(ret_slots);
                        return Ok(());
                    }
                    return Err(OffReject::Callee("lambda-impl-unresolvable".to_string()));
                }
            }
            // DEVIRTUALISATION: a virtual/interface call on a receiver whose EXACT dynamic type
            // is known resolves to that concrete impl, not the CP's (possibly abstract/interface)
            // declared owner. Two sources: (F2) a still-fresh `new C` receiver → type C from its
            // CP; (transitive this-devirt) the `this` receiver (`F::This`) → the fork's concrete
            // element type `self_class`. Sound with no extra runtime info.
            let devirt = if matches!(op, 0xb6 | 0xb9) {
                match recv {
                    F::Fresh(Some(cp)) => pool.class_name(cp),
                    F::This => self_class,
                    _ => None,
                }
            } else {
                None
            };
            // GAP-S06: a virtual/interface call is admitted only against an EXACT dynamic
            // receiver type (fresh `new C` / the fork's concrete `this`) or through the full
            // dispatch-closure query (`CalleeQuery::closure` — the implementation enumerates
            // every loaded concrete subtype's resolved target, or rejects). The constant-pool
            // owner alone is a DECLARATION, not a dispatch target: accepting it could admit an
            // unanalysed override behind a shared receiver. `invokestatic`/`invokespecial`
            // owners are exact by construction. A RECEIVER-MUTATING native contract is
            // admissible only on a private (fresh/owned) receiver, keyed by the exact class.
            // GAP-A01 (drill iter 3): an exact query carries the receiver's
            // proven privacy so the implementation may consult receiver-mut
            // native contracts (Throwable `<init>`) that need a VM-side
            // mechanical check (no-bytecode + unloadable + strict subtype) the
            // pure `receiver_mut_contract` table cannot perform here.
            let exact_q = |o| {
                if recv_private {
                    CalleeQuery::exact_private(o, m.name, m.descriptor)
                } else {
                    CalleeQuery::exact(o, m.name, m.descriptor)
                }
            };
            // FieldTxn mode: the receiver-mutating-native shortcut is DISABLED —
            // a native callee's writes cannot be journal-replayed by the txn
            // runtime, so EVERY invoke must be proven via `callee` (which the VM
            // caller wires to reject native / no-bytecode targets outright).
            let recv_mut_ok = |o: &str| {
                !txn && crate::native_contract::receiver_mut_contract(o, m.name).is_some()
            };
            let ok = if is_static {
                check_callee!(CalleeQuery::exact(m.owner, m.name, m.descriptor))
            } else if is_special {
                (recv_mut_ok(m.owner) && (relaxed || recv_private))
                    || check_callee!(exact_q(m.owner))
            } else if let Some(c) = devirt {
                (recv_mut_ok(c) && (relaxed || recv_private)) || check_callee!(exact_q(c))
            } else if relaxed && !txn {
                // Ceiling probe (diagnostic upper bound only): keep the historical
                // owner-based acceptance so the GAP metric stays comparable.
                crate::native_contract::receiver_mut_contract(m.owner, m.name).is_some()
                    || check_callee!(CalleeQuery::exact(m.owner, m.name, m.descriptor))
            } else {
                check_callee!(CalleeQuery::closure(m.owner, m.name, m.descriptor))
            };
            if !ok {
                // Carry the DESCRIPTOR too: the diagnostic walker re-resolves this
                // callee to follow the reject to its real origin.
                return Err(OffReject::Callee(format!(
                    "{}.{}{}",
                    m.owner, m.name, m.descriptor
                )));
            }
            push_shared!(ret_slots);
        }
        // invokedynamic — modelled ONLY when the bootstrap is
        // `StringConcatFactory.makeConcatWithConstants` (javac's `+` operator on
        // Java 9+). Rava itself serves the linked call site (opcodes.rs
        // `invokedynamic` → `Plan::Concat`; the JIT's all-int `ConcatShape`
        // inline): it reads the popped dynamic arguments, builds ONE fresh
        // String and publishes nothing — a pure-alloc expression. SIDE
        // CONDITION: the serving implementation stringifies each REFERENCE
        // argument through `Vm::value_to_string`, i.e. the argument's real
        // guest `toString()` bytecode, so each reference-typed parameter of the
        // call-site descriptor must itself pass the standard callee proof for
        // `toString()` (dispatch closure over the DECLARED type — same envelope
        // as a shared-receiver virtual call). Exact `java/lang/String` is
        // exempt: rava's String is natively served (natives.rs PURE_ALLOC
        // family) and `value_to_string` renders Str/ArenaString in pure Rust
        // with no guest re-entry. Primitives stringify in pure Rust. Any OTHER
        // bootstrap (LambdaMetafactory, ObjectMethods, SwitchBootstraps, …)
        // keeps the hard reject, as does a missing BootstrapMethods table.
        0xba => {
            let idx = u16be(bc, pc + 1);
            // GILFREE-RINGS: a LambdaMetafactory indy MINTS a fresh lambda object
            // capturing the popped args (shared refs stored INTO a fresh object —
            // the ctor-arg shape, no publish). rava's indy service allocates it
            // natively (no guest re-entry). The value is tracked as F::Lambda(idx)
            // so a SAM invoke on it devirtualises to the site's exact impl handle.
            if lmf_indy_impl(pool, bsm, idx).is_some() {
                #[cfg(test)]
                crate::offload::effect_summary::observe_dynamic(effects, pc, true);
                let (_b, _n, site_desc) =
                    pool.invoke_dynamic(idx).ok_or(OffReject::Opcode(pc, op))?;
                let (arg_slots, _ret) = desc_slots(site_desc);
                for _ in 0..arg_slots {
                    pop1!();
                }
                stack.push(F::Lambda(idx));
                return Ok(());
            }
            let Some(site_desc) = concat_indy_desc(pool, bsm, idx) else {
                #[cfg(test)]
                crate::offload::effect_summary::observe_dynamic(effects, pc, false);
                return Err(OffReject::Opcode(pc, op)); // non-concat / unresolvable indy
            };
            #[cfg(test)]
            crate::offload::effect_summary::observe_dynamic(effects, pc, true);
            for t in ref_param_types(site_desc) {
                if t != "java/lang/String"
                    && !check_callee!(CalleeQuery::closure(t, "toString", "()Ljava/lang/String;"))
                {
                    return Err(OffReject::Callee(format!("{t}.toString (concat arg)")));
                }
            }
            let (arg_slots, _ret) = desc_slots(site_desc);
            for _ in 0..arg_slots {
                pop1!();
            }
            stack.push(F::Fresh(None)); // the ONE fresh result String
        }
        // athrow of a FRESH (this-method-allocated, unpublished) throwable is
        // PATH-TERMINAL like a return: the method has no exception handlers
        // (enforced at analyze() entry), so the throw unwinds straight out of
        // the frame — and every transitively-offloadable caller up to the fork
        // root is handler-free by the same rule, so the resulting VmError
        // reaches the fork/serial driver, which surfaces it as an abort/serial
        // retry or the exact serial rethrow. The fresh exception publishes
        // nothing (freshness = never stored to shared state). A NON-fresh
        // thrown value keeps the reject: throwing a published object hands it
        // to an unknown handler in another frame. The linear scan continues at
        // the next pc with an EMPTIED stack — this basic block ends here, and
        // any live successor is a branch target whose join reset assumes the
        // conservative bottom anyway (mirrors the post-`return` dead
        // continuation, minus the residual slots that would leak stale
        // heights into the next block).
        0xbf => {
            let thrown = pop1!();
            #[cfg(test)]
            if let Some(observer) = effects.as_deref_mut() {
                observer(crate::offload::effect_summary::EffectFact::Throw {
                    pc,
                    target: crate::offload::effect_summary::target_freshness(thrown),
                });
            }
            if !relaxed && !matches!(thrown, F::Fresh(_)) {
                return Err(OffReject::Opcode(pc, op)); // shared-athrow
            }
            stack.clear();
        }
        0xc2 | 0xc3 => return Err(OffReject::Opcode(pc, op)), // monitorenter / monitorexit
        0xbb => stack.push(F::Fresh(Some(u16be(bc, pc + 1)))), // new #cp (exact type known)
        0xbc => {
            pop1!();
            stack.push(F::Fresh(None));
        } // primitive newarray
        0xbd => {
            if txn {
                return Err(OffReject::Opcode(pc, op));
            }
            pop1!();
            stack.push(F::Fresh(None));
        } // anewarray can resolve an uninitialized class
        0xbe => {
            pop1!();
            push_shared!(1);
        } // arraylength
        0xc0 | 0xc1 => {
            if txn {
                return Err(OffReject::Opcode(pc, op));
            }
            if op == 0xc1 {
                pop1!();
                push_shared!(1);
            }
        } // checkcast / instanceof can resolve a class
        0xc5 => {
            if txn {
                return Err(OffReject::Opcode(pc, op));
            }
            let dims = *bc.get(pc + 3).unwrap_or(&0) as usize;
            for _ in 0..dims {
                pop1!();
            }
            stack.push(F::Fresh(None));
        } // multianewarray can resolve a class
        // Unmodelled (jsr/ret/wide/dup_x2/dup2_x1/dup2_x2/jsr_w/…) → reject (sound).
        _ => return Err(OffReject::Opcode(pc, op)),
    }
    Ok(())
}
