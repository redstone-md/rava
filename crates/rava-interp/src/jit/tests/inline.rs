//! Inline-admission tests: what is_inlinable, the HOF wrapper and partial inlining accept and refuse.

use super::*;

#[test]
fn is_inlinable_accepts_pure_leaves_incl_branches_rejects_calls_heap() {
    // int sq(int x){ return x*x; }  iload_0; iload_0; imul; ireturn
    assert!(is_inlinable(&code(vec![0x1a, 0x1a, 0x68, 0xac], 1), "(I)I", false));
    // branchy but pure: int f(int x){ return x==0 ? 1 : 2; }  — now inlinable
    assert!(is_inlinable(
        &code(vec![0x1a, 0x99, 0x00, 0x05, 0x05, 0xac, 0x04, 0xac], 1),
        "(I)I",
        false,
    ));
    // getter: Object getX(){ return this.x; }  aload_0; getfield; areturn — inlinable
    assert!(is_inlinable(
        &code(vec![0x2a, 0xb4, 0x00, 0x01, 0xb0], 1),
        "()Ljava/lang/Object;",
        false,
    ));
    // invoke / putfield / void → reject in LEAF mode (allow_invokes=false)
    assert!(!is_inlinable(&code(vec![0x1a, 0xb8, 0x00, 0x01, 0xac], 1), "(I)I", false));
    assert!(!is_inlinable(&code(vec![0x2a, 0x1b, 0xb5, 0x00, 0x01, 0xb1], 2), "(I)V", false));
    assert!(!is_inlinable(&code(vec![0xb1], 0), "()V", false));
    // 16.2: an invokestatic body IS accepted in NESTED mode (allow_invokes=true) — the recursive
    // target-builder then validates the callee separately.
    assert!(is_inlinable(&code(vec![0x1a, 0xb8, 0x00, 0x01, 0xac], 1), "(I)I", true));
    // GETSTATIC-INLINE: getstatic is admitted STRUCTURALLY in both modes (emit_inline resolves
    // it against the callee's own pool via the generic callback). The soundness gate — the
    // field's class must be clinit-done — needs VM state, so it lives in inline_target_for_call,
    // not here. This is the shape of ImprovedNoise.gradDot: `getstatic` + invokestatic.
    assert!(is_inlinable(&code(vec![0xb2, 0x00, 0x01, 0xac], 0), "()I", false));
    assert!(is_inlinable(
        &code(vec![0xb2, 0x00, 0x01, 0x1a, 0xb8, 0x00, 0x02, 0xac], 1),
        "(I)I",
        true,
    ));
    // putstatic (0xb3) stays excluded — a side-effecting static write, not a read.
    assert!(!is_inlinable(&code(vec![0x1a, 0xb3, 0x00, 0x01, 0x1a, 0xac], 1), "(I)I", false));
}

#[cfg(feature = "next-gen-gc")]
#[test]
fn hof_wrapper_requires_complete_adjacent_pair_coverage() {
    let exact = code(
        vec![0xba, 0x00, 0x01, 0x00, 0x00, 0xb8, 0x00, 0x02, 0xac],
        0,
    );
    let pairs = std::collections::HashMap::from([(0usize, 5usize)]);
    assert!(is_hof_wrapper_shape(&exact, "()I", &pairs));

    let missing = std::collections::HashMap::new();
    assert!(!is_hof_wrapper_shape(&exact, "()I", &missing));

    let non_adjacent = code(
        vec![0xba, 0x00, 0x01, 0x00, 0x00, 0x00, 0xb8, 0x00, 0x02, 0xac],
        0,
    );
    assert!(!is_hof_wrapper_shape(&non_adjacent, "()I", &pairs));

    let extra_call = code(
        vec![
            0xba, 0x00, 0x01, 0x00, 0x00, 0xb8, 0x00, 0x02,
            0xb8, 0x00, 0x03, 0xac,
        ],
        0,
    );
    assert!(!is_hof_wrapper_shape(&extra_call, "()I", &pairs));
}

#[cfg(feature = "next-gen-gc")]
#[test]
fn hof_consumer_rejects_resolution_fault_and_allocation_before_sam() {
    let exact = code(
        vec![0x2a, 0x1b, 0xb9, 0x00, 0x01, 0x02, 0x00, 0xac],
        2,
    );
    assert!(is_hof_consumer_inlinable(
        &exact,
        "(Ljava/util/function/IntPredicate;I)I",
        2,
    ));
    let safe_idiv = code(
        vec![
            0x1b, 0x05, 0x6c, 0x3c, 0x2a, 0x1b, 0xb9, 0x00, 0x01, 0x02, 0x00, 0xac,
        ],
        2,
    );
    assert!(is_hof_consumer_inlinable(
        &safe_idiv,
        "(Ljava/util/function/IntPredicate;I)I",
        6,
    ));

    let forbidden_prefixes: &[&[u8]] = &[
        &[0x12, 0x01],
        &[0x13, 0x00, 0x01],
        &[0x14, 0x00, 0x01],
        &[0x6c],
        &[0x6d],
        &[0x70],
        &[0x71],
        &[0x72],
        &[0x73],
        &[0xbe],
        &[0xc0, 0x00, 0x01],
        &[0xc5, 0x00, 0x01, 0x01],
    ];
    for prefix in forbidden_prefixes {
        let mut bc = prefix.to_vec();
        let sam_pc = bc.len() + 2;
        bc.extend_from_slice(&exact.bytecode);
        assert!(
            !is_hof_consumer_inlinable(
                &code(bc, 2),
                "(Ljava/util/function/IntPredicate;I)I",
                sam_pc,
            ),
            "opcode {:02x} must fail closed",
            prefix[0],
        );
    }
}

#[test]
fn partial_inline_refuses_getstatic_beside_a_real_call() {
    let pool = field_ref_pool();
    let none: std::collections::HashMap<u16, InlineCallee> = std::collections::HashMap::new();
    // The enum values() shape: `getstatic $VALUES; invokevirtual clone(); checkcast; areturn`.
    // The clone() survives as a REAL, ALLOCATING call and the tail (checkcast/areturn) is
    // clean, so before the getstatic rule this spliced — and a GC inside clone() left the
    // CALLER's refs unrooted (emit_inline only spills the callee's bvars), which surfaced as
    // `UnsatisfiedLinkError: Integer.isEmpty()` on a receiver that arrived as Int(0).
    let values_shape = vec![0xb2, 0x00, 0x06, 0xb6, 0x00, 0x06, 0xc0, 0x00, 0x02, 0xb0];
    // The OFF arm (`RAVA_NO_PARTIAL_FREE=1`, or the maps off) is asserted explicitly rather than
    // through the process-wide `OnceLock`, so both arms are covered from one test binary.
    let off = |bc: &[u8]| partial_inline_gc_safe_mode(bc, &none, 64, 4, &pool, false);
    let on = |bc: &[u8]| partial_inline_gc_safe_mode(bc, &none, 64, 4, &pool, true);
    assert_eq!(off(&values_shape), PartialSplice::No);
    // A getstatic body that FULLY COLLAPSES (no invoke at all) has no GC point ⇒ still safe.
    // This is the Shapes.empty() shape: `getstatic <ref>; areturn`.
    assert_eq!(off(&[0xb2, 0x00, 0x06, 0xb0]), PartialSplice::Admit);
    // No getstatic + one real call + a clean result-forwarding tail ⇒ unchanged ADMISSION, and
    // the verdict now says what the admission always contained: a surviving GC point.
    assert_eq!(
        off(&[0x2a, 0xb6, 0x00, 0x06, 0xb0]),
        PartialSplice::AdmitGcPoint { free_only: false }
    );
    // ── THE STACK-MAP ARM. The rooting proof is discharged, so the enum `values()` shape is
    // admitted — and reported as the round's own population (`free_only`), because the historic
    // getstatic clause is what had refused it.
    assert_eq!(on(&values_shape), PartialSplice::AdmitGcPoint { free_only: true });
    // A full collapse stays a full collapse in both arms: no clause was in play.
    assert_eq!(on(&[0xb2, 0x00, 0x06, 0xb0]), PartialSplice::Admit);
    // And a body the historic clauses already admitted is NOT counted as newly admitted.
    assert_eq!(
        on(&[0x2a, 0xb6, 0x00, 0x06, 0xb0]),
        PartialSplice::AdmitGcPoint { free_only: false }
    );
    // TWO surviving calls with a field read between them — `real_calls != 1`, the clause the
    // interface band's twin retired first. Refused OFF, admitted ON as newly-admitted.
    let two_calls = vec![0x2a, 0xb6, 0x00, 0x06, 0x2a, 0xb6, 0x00, 0x06, 0xb0];
    assert_eq!(off(&two_calls), PartialSplice::No);
    assert_eq!(on(&two_calls), PartialSplice::AdmitGcPoint { free_only: true });
    // ⛔THE CLAUSE THAT IS KEPT: a callee frame larger than the caller's buffer is refused in
    // BOTH arms — it is a capacity bound on the shape, not a rooting proof.
    assert_eq!(
        partial_inline_gc_safe_mode(&two_calls, &none, 4, 64, &pool, true),
        PartialSplice::No
    );
}

#[test]
fn getstatic_refs_reads_callee_pool() {
    let pool = field_ref_pool();
    // getstatic #6; aaload; areturn — every 0xb2 is reported as (owner, name) so the
    // inline gate can prove the class is clinit-done before splicing.
    let refs = getstatic_refs(&[0xb2, 0x00, 0x06, 0x32, 0xb0], &pool).unwrap();
    assert_eq!(refs, vec![("pkg/Holder", "GRADIENT")]);
    // No getstatic → empty (Some, not None: nothing to gate).
    assert_eq!(getstatic_refs(&[0x1a, 0xac], &pool).unwrap(), vec![]);
    // A 0xb2 whose operand is not a FieldRef must NOT silently pass the gate.
    assert!(getstatic_refs(&[0xb2, 0x00, 0x02, 0xb0], &pool).is_none());
}

/// DISPATCH IC leaf gate: a pure field-getter (`aload_0; getfield; dreturn`) is
/// GC-leaf (cacheable in the machine-code PIC, never spills); a body with an
/// invoke / new / getstatic is NOT (it can GC, so the IC must not call it on a
/// non-zeroed, unrooted buffer).
#[cfg(feature = "next-gen-gc")]
#[test]
fn is_gc_leaf_accepts_field_getter_rejects_callbacks() {
    // aload_0; getfield #2; dreturn  (Circle.area-like, all inline)
    assert!(is_gc_leaf(&[0x2a, 0xb4, 0x00, 0x02, 0xaf]));
    // arithmetic + branch + return — leaf
    assert!(is_gc_leaf(&[0x1a, 0x1b, 0x60, 0xa7, 0x00, 0x03, 0xac]));
    // contains invokevirtual — NOT leaf
    assert!(!is_gc_leaf(&[0x2a, 0xb6, 0x00, 0x02, 0xb1]));
    // contains new — NOT leaf
    assert!(!is_gc_leaf(&[0xbb, 0x00, 0x02, 0xb1]));
    // contains getstatic — NOT leaf
    assert!(!is_gc_leaf(&[0xb2, 0x00, 0x02, 0xac]));
}

#[test]
fn bails_on_unsupported() {
    let c = code(vec![0xb8, 0x00, 0x01, 0xb1], 1); // invokestatic
    assert!(compile(&c, "()V", &empty_pool(), 0, &mut |_, _| (0, None)).is_none());
}

/// FEASIBILITY BOUNDARY for moving Cranelift codegen off the serial critical path.
///
/// Compile time is ~4.5s of the boot's 54s SERIAL path (RAVA_COMPILE_TIME), and the standing
/// plan for it is a background compiler thread. That requires the compile INPUTS to be `Send`.
/// Most of them are, and this pins that so a future field cannot silently break it.
///
/// `InlineCallee` is NOT, and the compiler names why rather than leaving it to reading:
///
/// ```text
/// error[E0277]: `RefCell<HashMap<String, Value, …>>` cannot be shared between threads safely
///   = help: within `LoadedClass`, the trait `Sync` is not implemented for
///           `RefCell<HashMap<String, Value, …>>`      <- LoadedClass::statics
///   note: required because it appears within the type `LoadedClass`
///   note: required because it appears within the type `jit::InlineCallee`
/// error[E0277]: `Cell<bool>` cannot be shared between threads safely   <- ::initialized
/// error[E0277]: `RefCell<HashMap<u16, (Arc<str>, Arc<str>, Arc<str>), …>>` …  <- ::site_cache
/// ```
///
/// So the blocker is `LoadedClass` being `!Sync`, and codegen cannot avoid it — it reads the
/// constant pool through exactly that `Arc<LoadedClass>`. This is the same shared-readable-
/// loader direction the branch is already pursuing (`get_or_load` takes `&self`).
///
/// CLEARED. All three blockers are gone and `assert_send::<InlineCallee>()` below now COMPILES,
/// which is the whole point of this test: the claim is checked by the compiler rather than
/// asserted in prose. The latches became `AtomicBool`, `site_cache` became a lazily-built
/// dense table of `OnceLock`s, and `statics` became an `RwLock`.
///
/// `statics` was recorded here as the hard one — "a `RefCell` of guest `Value`s is the GIL
/// problem in miniature, not a lock that can simply be swapped in". That is right about the
/// SEMANTICS and was being read as a claim about the TYPES. `Value` is primitives, an
/// `Arc<str>`, plain handles and an `ArenaPtr` that already carries `unsafe impl Send + Sync`,
/// so it was `Send + Sync` all along and the swap needed no new unsafe. Conflating the two
/// kept a one-line type change parked behind a semantics problem it did not have.
///
/// ⚠ What remains open is the SEMANTIC half, and it is NOT claimed by this test: whether
/// concurrent static access is correct under the GIL-free lane. Type-reachability from two
/// threads is not thread-safety of the guest program.
///
/// PRICE, RE-MEASURED — and the two lenses AGREE, which is worth stating because an earlier
/// pass here accused the sampler of being 2x wrong when the error was arithmetic.
///
/// `<vm>.cranelift-compile` is 428 of 3067 JIT_EXEC ticks = 14.0% — but that is a share of
/// JIT_EXEC, which is itself 43.5% of the run, so it is 6.1% of WALL ≈ 4.0s at a 66.5s boot.
/// Direct instrumentation (`RAVA_COMPILE_TIME`) reports 6.24s over 2000 compiles on a
/// contended boot (wall 92.5s vs 66.5s clean) ⇒ ~4.5s adjusted. Within 12% of each other, and
/// both reproduce the figure at `jit.rs:10803` on a much newer tree.
///
/// ⚠ Method-table rows are shares of JIT_EXEC, never of wall. Reading one straight into
/// seconds inflates it by 1/0.435 ≈ 2.3x.
///
/// Only part of ~4.5s is recoverable: a background thread MOVES the work, it does not delete
/// it, and the main thread interprets until the code is published. Realistically ~2-3s.
///
/// ⇒ Worth doing, not worth over-promising, and now gated on ONE field rather than three.
#[test]
fn compile_inputs_are_send_except_the_recorded_blocker() {
    fn assert_send<T: Send>() {}
    assert_send::<CtorInline>();
    assert_send::<DeepShape>();
    assert_send::<StaticIntrinsic>();
    // NO LONGER COMMENTED OUT. This line failing again means someone reintroduced a `!Sync`
    // field on `LoadedClass` and re-blocked the background compile thread — which is exactly
    // what this test exists to catch, since nothing else would notice until the lever is
    // attempted and the error appears to be about the lever rather than about the field.
    assert_send::<InlineCallee>();
}

#[test]
fn has_wide_params_classifies_correctly() {
    assert!(!has_wide_params("(II)I"));
    assert!(!has_wide_params("()I"));
    assert!(!has_wide_params("(Ljava/lang/Object;I)Z"));
    assert!(!has_wide_params("([J[D)V")); // long/double ARRAYS are narrow refs
    assert!(!has_wide_params("([[I)V"));
    assert!(has_wide_params("(J)V"));
    assert!(has_wide_params("(ID)V"));
    assert!(has_wide_params("(Ljava/lang/Object;D)V"));
    // THE REGRESSION GUARD for the freeze-table wrong-answer bug. `freeze_resolve::put`
    // refuses to publish a site whose callee takes a wide parameter, because the lock-free
    // serve arm hands the operand buffer over un-marshalled and a wide arg occupies TWO callee
    // local slots. This is the ring's actual noise callee; publishing it made a worker read
    // four doubles out of the wrong slots and return plausible, wrong numbers.
    assert!(
        has_wide_params("(IIIDDDD)D"),
        "ImprovedNoise.sampleAndLerp must be classified wide - see freeze_resolve::put"
    );
    // ...and the shape the boot's ring site itself has: a Predicate/RandomSource/Sampler
    // signature carries no wide scalar, so it must stay servable.
    assert!(!has_wide_params(
        "(IIIILjava/util/function/Predicate;Lnet/minecraft/util/RandomSource;Lnet/minecraft/world/level/biome/Climate$Sampler;)Lcom/mojang/datafixers/util/Pair;"
    ));
}

#[test]
fn is_inlinable_gates_void_and_invokes() {
    // iload_0; ireturn — small pure int identity → inlinable
    assert!(is_inlinable(&code(vec![0x1a, 0xac], 1), "(I)I", false));
    // void return → not inlinable
    assert!(!is_inlinable(&code(vec![0xb1], 1), "(I)V", false));
    // 16.2: wide (long/double) param IS now inlinable — lload_0; l2i; ireturn for (J)I. emit_inline
    // places each arg at its cumulative 2-slot-aware local index.
    assert!(is_inlinable(&code(vec![0x1e, 0x88, 0xac], 2), "(J)I", false));
    // body contains an invoke → not inlinable in LEAF mode
    assert!(!is_inlinable(&code(vec![0x2a, 0xb6, 0x00, 0x01, 0xac], 1), "(I)I", false));
    // getfield IS allowed in an inlinable body (aload_0; getfield; ireturn)
    assert!(is_inlinable(&code(vec![0x2a, 0xb4, 0x00, 0x01, 0xac], 1), "()I", false));
}
