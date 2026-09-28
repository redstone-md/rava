//! Inline-cache and PIC tests: the lambda PIC, the per-thread IC table, and the COW prologue.

use super::*;

#[test]
#[cfg(feature = "next-gen-gc")]
fn nullable_object_slot_class_load_branches_before_dereference() {
    let mut module = build_shared_module();
    let mut ctx = module.make_context();
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(I64)); // tagged heap Ref
    sig.params.push(AbiParam::new(I64)); // JitCtx address
    sig.returns.push(AbiParam::new(I64));
    ctx.func.signature = sig.clone();

    let mut fb_ctx = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fb_ctx);
        let entry = b.create_block();
        let dead = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        b.seal_block(entry);
        let recv = b.block_params(entry)[0];
        let jit_ctx = b.block_params(entry)[1];
        let key = emit_heap_ref_class_key(&mut b, jit_ctx, recv, dead);
        b.ins().return_(&[key]);

        b.switch_to_block(dead);
        b.seal_block(dead);
        let sentinel = b.ins().iconst(I64, -1);
        b.ins().return_(&[sentinel]);
        b.finalize();
    }

    let id = module.declare_anonymous_function(&sig).unwrap();
    module.define_function(id, &mut ctx).unwrap();
    module.clear_context(&mut ctx);
    module.finalize_definitions().unwrap();
    let raw = module.get_finalized_function(id);
    let call: extern "C" fn(i64, i64) -> i64 = unsafe { std::mem::transmute(raw) };

    // JitCtx.objects_base lives at byte offset 48. The selected slot contains
    // the exact transparent representation of ObjectSlot(None): a null word.
    let slots = [0usize];
    let mut fake_jit_ctx = [0i64; 7];
    fake_jit_ctx[6] = slots.as_ptr() as i64;
    let recv = crate::value::REF_TAG;
    assert_eq!(call(recv, fake_jit_ctx.as_ptr() as i64), -1);
}

/// λ-PIC machine code, composed from the EXACT production emit helpers
/// (`emit_heap_ref_class_key` → `emit_lambda_key_select` → `emit_refpic_probe`
/// → `emit_lambda_shift_adjust`, the same pipeline `emit_dispatch_pic` bakes
/// under RAVA_LAMBDA_PIC):
///  (a) a "$Lambda"-class receiver probes by IDENTITY key `(recv<<1)|1`, and a
///      hit whose fptr carries the DEEP+SHIFT markers (|1|2) calls the target
///      MASKED of both bits on bufaddr+8 — the callee sees the args WITHOUT
///      the receiver slot;
///  (b) a non-lambda heap Ref (class word ≠ LAMBDA_CLASS_WORD, low bit 0)
///      still routes the class-word path: unshifted buffer, unmasked fptr,
///      and an identity-keyed way in the same cache can never serve it.
#[test]
#[cfg(feature = "next-gen-gc")]
fn lambda_pic_shifted_identity_hit_and_classword_path() {
    let mut module = build_shared_module();
    let mut ctx = module.make_context();
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(I64)); // tagged heap Ref receiver
    sig.params.push(AbiParam::new(I64)); // JitCtx address
    sig.params.push(AbiParam::new(I64)); // site cache [i64; 8]
    sig.params.push(AbiParam::new(I64)); // slot-laid call buffer
    sig.returns.push(AbiParam::new(I64));
    ctx.func.signature = sig.clone();

    let mut fb_ctx = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fb_ctx);
        let entry = b.create_block();
        let hit = b.create_block();
        let miss = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        b.seal_block(entry);
        let recv = b.block_params(entry)[0];
        let jit_ctx = b.block_params(entry)[1];
        let cache = b.block_params(entry)[2];
        let bufaddr = b.block_params(entry)[3];
        let key = emit_heap_ref_class_key(&mut b, jit_ctx, recv, miss);
        let key = emit_lambda_key_select(&mut b, recv, key);
        let fptr = emit_refpic_probe(&mut b, cache, key);
        let have = b.ins().icmp_imm(IntCC::NotEqual, fptr, 0);
        b.ins().brif(have, hit, &[], miss, &[]);

        b.switch_to_block(hit);
        b.seal_block(hit);
        let (fp, effbuf) = emit_lambda_shift_adjust(&mut b, fptr, bufaddr);
        let conv = b.func.signature.call_conv;
        let mut cs = cranelift::codegen::ir::Signature::new(conv);
        cs.params.push(AbiParam::new(I64));
        cs.params.push(AbiParam::new(I64));
        cs.returns.push(AbiParam::new(I64));
        let sigref = b.import_signature(cs);
        let hcall = b.ins().call_indirect(sigref, fp, &[effbuf, jit_ctx]);
        let bits = b.inst_results(hcall)[0];
        b.ins().return_(&[bits]);

        b.switch_to_block(miss);
        b.seal_block(miss);
        let sentinel = b.ins().iconst(I64, -1);
        b.ins().return_(&[sentinel]);
        b.finalize();
    }

    let id = module.declare_anonymous_function(&sig).unwrap();
    module.define_function(id, &mut ctx).unwrap();
    module.clear_context(&mut ctx);
    module.finalize_definitions().unwrap();
    let raw = module.get_finalized_function(id);
    let call: extern "C" fn(i64, i64, i64, i64) -> i64 = unsafe { std::mem::transmute(raw) };

    // Two fake heap objects: handle 0 = a "$Lambda" (class word == the published
    // LAMBDA_CLASS_WORD), handle 1 = a plain class (a different 8-aligned word).
    let class_off = crate::heap::Heap::object_class_offset() as usize;
    let lam_word: i64 = 0x5712_2334_4008; // 8-aligned (low bit 0), nonzero
    let plain_word: i64 = 0x5712_2334_4010;
    let mut lam_obj = vec![0u8; class_off + 8];
    let mut plain_obj = vec![0u8; class_off + 8];
    unsafe {
        std::ptr::write(lam_obj.as_mut_ptr().add(class_off) as *mut i64, lam_word);
        std::ptr::write(plain_obj.as_mut_ptr().add(class_off) as *mut i64, plain_word);
    }
    let slots = [lam_obj.as_ptr() as usize, plain_obj.as_ptr() as usize];
    let mut fake_jit_ctx = [0i64; 7];
    fake_jit_ctx[6] = slots.as_ptr() as i64; // JitCtx.objects_base @ byte 48
    LAMBDA_CLASS_WORD.store(lam_word, std::sync::atomic::Ordering::SeqCst);

    // (a) IDENTITY hit with DEEP+SHIFT markers → target sees bufaddr+8, args intact.
    let lam_recv = crate::value::REF_TAG; // handle 0
    let mut cache = [0i64; 8];
    cache[0] = (lam_recv << 1) | 1;
    cache[1] = lp_target as *const () as usize as i64 | 0b11;
    let callbuf = [lam_recv, 4242i64, 0, 0];
    let r = call(
        lam_recv,
        fake_jit_ctx.as_ptr() as i64,
        cache.as_ptr() as i64,
        callbuf.as_ptr() as i64,
    );
    assert_eq!(
        LP_SEEN_BUF.load(std::sync::atomic::Ordering::SeqCst),
        callbuf.as_ptr() as i64 + 8,
        "SHIFT marker ⇒ the SAM impl is entered on the buffer WITHOUT the receiver slot"
    );
    assert_eq!(r, 4242, "arg0 of the SAM call is the callee's local 0");

    // (a') LEAF+SHIFT (fptr|0b10 — bit0 clear): the leaf entry now shares the
    // rooted hit with deep entries. SHIFT must still omit the receiver slot and
    // bit1 must be masked from the function pointer; this is the only entry
    // where bit1 is set while bit0 is clear, so it distinguishes the helper's
    // `-4` mask from a `-2` mask.
    let mut cache_leaf = [0i64; 8];
    cache_leaf[0] = (lam_recv << 1) | 1;
    cache_leaf[1] = lp_target as *const () as usize as i64 | 0b10;
    let callbuf_leaf = [lam_recv, 909i64, 0, 0];
    let rl = call(
        lam_recv,
        fake_jit_ctx.as_ptr() as i64,
        cache_leaf.as_ptr() as i64,
        callbuf_leaf.as_ptr() as i64,
    );
    assert_eq!(
        LP_SEEN_BUF.load(std::sync::atomic::Ordering::SeqCst),
        callbuf_leaf.as_ptr() as i64 + 8,
        "a LEAF lambda entry (bit1 set, bit0 clear) is still entered shifted"
    );
    assert_eq!(rl, 909, "leaf entry reached the real fptr — bit1 was masked off");

    // (b) plain heap Ref: the select keeps the CLASS word ⇒ the class-keyed way
    // serves it — unshifted buffer, unmasked (8-aligned) fptr.
    let plain_recv = crate::value::REF_TAG | 1; // handle 1
    let mut cache2 = [0i64; 8];
    cache2[0] = plain_word;
    cache2[1] = lp_target as *const () as usize as i64;
    let callbuf2 = [7777i64, 0, 0, 0];
    let r2 = call(
        plain_recv,
        fake_jit_ctx.as_ptr() as i64,
        cache2.as_ptr() as i64,
        callbuf2.as_ptr() as i64,
    );
    assert_eq!(
        LP_SEEN_BUF.load(std::sync::atomic::Ordering::SeqCst),
        callbuf2.as_ptr() as i64,
        "class-word entry (low bits 00) ⇒ unshifted buffer"
    );
    assert_eq!(r2, 7777);

    // (b') the plain receiver against the LAMBDA cache: its class word never
    // matches an identity key (odd) ⇒ machine-code miss (sentinel), proving the
    // two key spaces are disjoint within one cache.
    let r3 = call(
        plain_recv,
        fake_jit_ctx.as_ptr() as i64,
        cache.as_ptr() as i64,
        callbuf2.as_ptr() as i64,
    );
    assert_eq!(r3, -1, "identity keys are unreachable from class-word probes");
}

/// Gate OFF (the default; this test process never sets RAVA_LAMBDA_PIC): the
/// dispatch_miss fill hook must never write a λ identity entry — even for a
/// perfectly-eligible directable singleton receiver.
#[test]
#[cfg(feature = "next-gen-gc")]
fn lambda_pic_gate_off_never_fills() {
    if lambda_pic_enabled() {
        // The read-once gate was armed by the test ENVIRONMENT (RAVA_LAMBDA_PIC set)
        // — the OFF-path contract can't be observed in this process. Skip rather
        // than fail: the null caller_ptr below is only safe behind the gate bail.
        return;
    }
    let mut vm = crate::Vm::new(crate::ClassLoader::new());
    let site = std::sync::Arc::new(crate::LambdaSite::new(
        std::sync::Arc::from("Owner"),
        std::sync::Arc::from("apply"),
        std::sync::Arc::from("(I)I"),
    ));
    let h = vm.heap.alloc_lambda(crate::Lambda { kind: 6, site, captured: Vec::new() });
    vm.indy_singletons.insert((1, 1), h);
    vm.indy_singleton_handles.insert(h);
    assert!(vm.heap.lambda_directable(h), "eligible receiver by construction");
    let cache = [0i64; 8];
    let recv = crate::value::REF_TAG | h as i64;
    let buf = [recv];
    // caller_ptr is never dereferenced: the gate check is the first bail-out.
    vm.lambda_pic_fill(cache.as_ptr() as i64, recv, std::ptr::null(), 0, 1, buf.as_ptr(), 1);
    assert_eq!(cache, [0i64; 8], "gate off ⇒ no fill, cache untouched");
}

/// 11.2: a worker's per-thread IC table is physically allocated, SNAPSHOT-SEEDED byte-for-byte
/// from the shared global caches (each site at its `ic_offset`), and freed with no leak. Uses the
/// real allocator: append a known site via `ic_site_alloc`, then verify + free on a spawned
/// (worker) thread so the thread-local + RAII path is exercised.
#[test]
#[cfg(feature = "next-gen-gc")]
fn ic_per_thread_table_seeds_and_frees() {
    let _g = crate::parallel::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // A known shared "global cache": a leaked [i64; 8] with a distinct pattern.
    let pattern: [i64; 8] = [0x1122_3344_5566_7788u64 as i64, 2, 3, 4, 5, 6, 7, 0x7fff_ffff_ffff_ffff];
    let cache_ptr = Box::into_raw(Box::new(pattern)) as usize;
    let off = super::ic_site_alloc(cache_ptr); // append it; get its monotonic offset

    // Exercise on a real spawned thread (the worker context): alloc + seed + read-back + free.
    let checked = std::thread::spawn(move || {
        super::ic_worker_table_init();
        let base = super::per_thread_ic_base();
        assert!(!base.is_null(), "table allocated (IC_SITES is non-empty)");
        // The seeded 64-B slot at `off` must equal the global cache byte-for-byte.
        let got: [i64; 8] = unsafe { std::ptr::read_unaligned(base.add(off as usize) as *const [i64; 8]) };
        assert_eq!(got, pattern, "snapshot-seed copied the global cache into base+ic_offset");
        super::ic_worker_table_free();
        assert!(super::per_thread_ic_base().is_null(), "freed → base reset (no dangling table)");
        true
    })
    .join()
    .expect("worker thread panicked");
    assert!(checked);
    // NOTE: the appended IC_SITES entry is permanent (entries are never removed — other threads
    // may seed from them concurrently), so its backing cache is INTENTIONALLY leaked. Freeing it
    // would leave a dangling cache_ptr that a later worker's seeder would copy from (UAF).
}

/// PROMO gate: a `q.f = q.f + 1; x++` counted loop over a pinned receiver
/// promotes (written set = the pair); an int-div in the body (exception
/// exit) or a field site that misses the hoisted pairs bails.
#[test]
#[cfg(feature = "next-gen-gc")]
fn promo_scan_gates_whitelist_and_pairs() {
    // 0: iconst_0; istore_1; 2: iload_1; bipush 10; if_icmpge->24;
    // 8: aload_0; aload_0; getfield #1; iconst_1; iadd; putfield #1;
    // 18: iinc 1 1; goto->2; 24: return
    let bc = vec![
        0x03, 0x3c, 0x1b, 0x10, 0x0a, 0xa2, 0x00, 0x13, 0x2a, 0x2a, 0xb4, 0x00, 0x01, 0x04,
        0x60, 0xb5, 0x00, 0x01, 0x84, 0x01, 0x01, 0xa7, 0xff, 0xed, 0xb1,
    ];
    let mut fields = std::collections::HashMap::new();
    fields.insert(1u16, (7u32, JT::Int, Some(3u32)));
    let no_calls = std::collections::HashMap::new();
    let no_si = std::collections::HashMap::new();
    let pairs = [LicmPair { slot: 0, fid: 7, cslot: 3 }];
    let w = promo_scan(&bc, &fields, &empty_pool(), &no_calls, &no_si, &pairs, 2)
        .expect("promotable");
    assert_eq!(w.len(), 1);
    assert!(w.contains(&(0, 7, 3)));
    // idiv in the body → ArithmeticException exit possible → bail
    let mut bad = bc.clone();
    bad[14] = 0x6c;
    assert!(promo_scan(&bad, &fields, &empty_pool(), &no_calls, &no_si, &pairs, 2).is_none());
    // field sites not covered by the hoisted pairs → bail
    let other = [LicmPair { slot: 0, fid: 99, cslot: 4 }];
    assert!(promo_scan(&bc, &fields, &empty_pool(), &no_calls, &no_si, &other, 2).is_none());
}

#[test]
#[cfg(feature = "next-gen-gc")]
fn cow_armed_deep_prologue_uses_each_worker_root_context() {
    let _g = crate::parallel::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let shared_slot = Box::new(std::sync::atomic::AtomicUsize::new(
        crate::frames::ctx_addr() as usize,
    ));
    let c = code(vec![0x04, 0xac], 0); // iconst_1; ireturn
    let f = compile_with_static_intrinsics(
        &c,
        "()I",
        &empty_pool(),
        0,
        &mut |_, _| (0, None),
        &std::collections::HashMap::new(),
        &no_inline(),
        &no_native(),
        &no_statics(),
        &no_new_arena(),
        &no_ctor_inlines(),
        &no_concat_shapes(),
        &no_hof_fusions(),
        &no_getter_inlines(),
        Some(DeepShape {
            args_slots: 0,
            frame_len: 8,
            ctx_slot: &*shared_slot as *const std::sync::atomic::AtomicUsize as usize,
        }),
        #[cfg(feature = "next-gen-gc")]
        None,
        #[cfg(feature = "next-gen-gc")]
        empty_prim_arrays(),
    )
    .expect("deep body compiles");
    assert!(f.self_reg);

    let raw = f.raw() as usize;
    // With COW off, the existing callback-free slot path still owns the
    // registration. Force its slow push and observe that exact context grow.
    let normal_desc = [0i64; 8];
    let normal_addr = normal_desc.as_ptr() as i64;
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let dummy = [0i64; 1];
                crate::frames::cells_push((dummy.as_ptr(), dummy.len()));
                let ctx = crate::frames::ctx_addr();
                unsafe { (*ctx).cap = 1 };
                shared_slot.store(ctx as usize, std::sync::atomic::Ordering::Relaxed);
                let call: extern "C" fn(*mut i64, i64) -> i64 =
                    unsafe { std::mem::transmute(raw) };
                let mut cells = [0i64; 8];
                assert_eq!(call(cells.as_mut_ptr(), normal_addr), 1);
                assert_eq!(unsafe { (*ctx).top }, 1);
                assert_eq!(unsafe { (*ctx).cap }, 2, "serial slot path remains intact");
                crate::frames::cells_pop();
            })
            .join()
            .expect("serial deep probe panicked");
    });

    // JitCtx+8 lower word = fault flag 0; JitCtx+12 upper word = cow_armed 1.
    let desc = [0i64, 1i64 << 32, 0, 0, 0, 0, 0, 0];
    let desc_addr = desc.as_ptr() as i64;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
    std::thread::scope(|scope| {
        let mut joins = Vec::new();
        for _ in 0..4 {
            let barrier = std::sync::Arc::clone(&barrier);
            joins.push(scope.spawn(move || {
                let dummy = [0i64; 1];
                crate::frames::cells_push((dummy.as_ptr(), dummy.len()));
                let ctx = crate::frames::ctx_addr();
                unsafe { (*ctx).cap = 1 };
                let call: extern "C" fn(*mut i64, i64) -> i64 =
                    unsafe { std::mem::transmute(raw) };
                let mut cells = [0i64; 8];
                barrier.wait();
                assert_eq!(call(cells.as_mut_ptr(), desc_addr), 1);
                assert_eq!(unsafe { (*ctx).top }, 1, "deep epilogue restored worker top");
                assert_eq!(unsafe { (*ctx).cap }, 2, "worker slow-push grew its own ctx");
                crate::frames::cells_pop();
            }));
        }
        for join in joins {
            join.join().expect("deep worker panicked");
        }
    });
}

/// COW METHOD-ENTRY UNSWITCH: ONE compiled body, two worlds chosen ONCE at
/// entry from the invocation-invariant `[ctx+12]` flag. The point of the
/// unswitch is that the hot (DISARMED) half carries no COW code at all — so
/// this asserts BOTH halves of the same machine code from the same `JitFn`:
///   * disarmed → the putfield inlines, `rava_putfield` never fires;
///   * armed    → the very same body routes the write to the shadow-aware
///     callback, i.e. a phase-B worker's COW semantics are untouched.
/// A regression that emitted only the disarmed body would silently drop the
/// worker's write isolation, and only the second half of this test would fail.
#[cfg(feature = "next-gen-gc")]
#[test]
fn cow_unswitch_keeps_the_armed_write_on_the_shadow_callback() {
    if !crate::parallel::cow_gate() {
        return; // RAVA_NO_FORK_COW build emits no COW code in either body
    }
    let _g = crate::parallel::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    use std::sync::Arc;
    let name: Arc<str> = Arc::from("CowUnswitchPF");
    let layout: Arc<[(u32, u8)]> = vec![(0u32, 0u8)].into();
    let cid = crate::nextgen::register_class(&name, &layout);
    let p = crate::nextgen::tlab_try_alloc(cid, 1).unwrap();
    crate::nextgen::arena_set_cell(p, 0, 0);

    // int f(CowUnswitchPF o, int v){ o.x = v; return v; }
    let pool = ConstantPool::from_entries(vec![
        Constant::Unusable,
        Constant::Utf8("CowUnswitchPF".into()),
        Constant::Class { name_index: 1 },
        Constant::Utf8("x".into()),
        Constant::Utf8("I".into()),
        Constant::NameAndType { name_index: 3, desc_index: 4 },
        Constant::FieldRef { class_index: 2, nat_index: 5 },
    ]);
    let c = code(vec![0x2a, 0x1b, 0xb5, 0x00, 0x06, 0x1b, 0xac], 2);
    let f = compile(&c, "(LCowUnswitchPF;I)I", &pool, 0, &mut |_, _| (0, Some(0)))
        .expect("putfield-int method compiles");

    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let desc_cell = vm.heap.array_desc_cell_ptr();
    let prev = set_current_vm(&mut vm as *mut crate::Vm);

    vm.heap.set_cow_armed(false);
    let mut locals = vec![pack_local(&JVal::Obj(p)), pack_local(&JVal::Int(11))];
    let before = tl_cb(7); // 7 = putfield
    let got = unsafe { f.call_desc(&mut locals, desc_cell) } as i32;
    let disarmed = tl_cb(7);
    assert_eq!(got, 11);
    assert_eq!(disarmed, before, "the DISARMED body must carry no COW callback");

    vm.heap.set_cow_armed(true);
    let mut locals = vec![pack_local(&JVal::Obj(p)), pack_local(&JVal::Int(22))];
    let got = unsafe { f.call_desc(&mut locals, desc_cell) } as i32;
    let armed = tl_cb(7);
    vm.heap.set_cow_armed(false);
    set_current_vm(prev);

    assert_eq!(got, 22);
    assert!(armed > disarmed, "the ARMED body must still take the COW write callback");
}
