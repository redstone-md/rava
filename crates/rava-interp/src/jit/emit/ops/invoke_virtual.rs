//! Virtual invocation: the largest single opcode, with the inline caches, the direct-call cache and the native fast paths in front of it.
//!
//! Arm bodies are unchanged; the preamble restores every name they were written against.

use super::Ops;
use crate::jit::*;

/// ⭐**KILL SWITCH FOR THE EMIT-TIME REFUSALS THIS ROUND LIFTED IN THE INVOKE FAMILY.**
///
/// DEFAULT ON. `RAVA_NO_EMIT_INVOKEOPS=1` — or the round-wide `RAVA_NO_EMIT_MORE=1` — puts every
/// arm back behind the gate it used to sit behind, so a build with either set emits exactly what
/// the previous binary emitted. Consulted at EMIT time, which is the only place it changes
/// codegen: nothing here is re-read per execution.
#[cfg(feature = "next-gen-gc")]
pub(super) fn emit_invokeops_disabled() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| std::env::var_os("RAVA_NO_EMIT_INVOKEOPS").is_some())
        || crate::flags::emit_more_disabled()
}

pub(crate) fn invoke_virtual(o: &mut Ops, pc: usize, op: u8) -> Option<()> {
    let b = &mut *o.b;
    let stack = &mut *o.stack;
    let vars = o.vars;
    let bc = o.bc;
    let pool = o.pool;
    let env = o.env;
    let array_desc_cell = o.array_desc_cell;
    let locals_ptr = o.locals_ptr;
    let cow = o.cow;
    #[cfg(feature = "next-gen-gc")]
    let (licm, dv) = (o.licm, o.dv);
    match op {
        0xb6 => {
            // invokevirtual — a final-effective callee in the inline map folds in
            // directly (monomorphic by type, no guard). Else the arena dispatch IC
            // (4-way machine-code PIC), else a real call.
            let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            // `size()` inline, the invokevirtual twin of the interface arm — a concrete-typed
            // receiver reaches the same 4.46 M-call population through here.
            //
            // ⭐⭐**ONE OPT-IN KILL SWITCH WAS SWITCHING OFF SIX DEFAULT-ON ARMS, PERMANENTLY,
            // AT EVERY `invokevirtual` SITE IN THE PROGRAM.** This block used to open with
            //
            //     if !arena_inline_disabled() && alist_size_inline_enabled() && env.elems_fid > 0
            //
            // and everything below sat INSIDE it: the `CubePointRange` size/getDouble pair,
            // `Iterator.next`/`hasNext`, the `Map$Entry` getters, `DoubleArrayList.getDouble` —
            // and, last, `emit_arraylist_size_inline`. Only that LAST arm is what the two hoisted
            // terms are about. `alist_size_inline_enabled()` is `RAVA_ALIST_SIZE_INLINE`, an
            // OPT-IN that is unset on every production boot, so the group predicate was FALSE and
            // the whole block was dead code; `env.elems_fid` is `java/util/ArrayList.$elems`,
            // which none of the six other arms reads (their ids are `CUBE_PARTS_FID`,
            // `ITER_ELEMS_FID`/`ITER_POS_FID`, `ENTRY_KEY_FID`/`ENTRY_VAL_FID`,
            // `DALIST_A_FID`/`DALIST_SIZE_FID`). A guard belonging to ONE member of a group was
            // answering for all seven — the predicate that answers several different questions at
            // once — and it answered at emit time with no cache to warm and no second chance.
            //
            // The `invokeinterface` twin in `ops::invoke_dynamic` gates these same seven arms
            // INDIVIDUALLY and has always served them. That asymmetry is already recorded in this
            // tree, in the note beside the interface `dalist` arm: the counter had to be placed on
            // the interface side because "the invokevirtual twin sits inside a block that also
            // demands `alist_size_inline_enabled`, which is off by default, so instrumenting it
            // read zero and said nothing". It read zero because of THIS line, not because the
            // population is absent — a concretely-typed receiver reaches `getDouble()`, `next()`,
            // `hasNext()` and `getKey()` through 0xb6, and `CubePointRange.getDouble` alone is
            // 6,504,704 calls a boot, the second largest (class, method) pair in the engine.
            //
            // Each arm now carries exactly the gates its interface twin carries: its own
            // default-ON switch, its own runtime-readiness test (the ids/class word are published
            // by the natives, not baked), a receiver class-word guard in the EMITTED code, and the
            // identical `env.fu_invoke` callback as the cold arm — which on a miss falls through
            // `jit_direct_call` into the unchanged general dispatch, so nothing that used to be
            // served stops being served. Textual order is unchanged, so the arm that claims a
            // given (name, descriptor) is the same one the interface path claims it with.
            // `RAVA_NO_EMIT_INVOKEOPS=1` restores the old nesting exactly.
            //
            // ⛔**AND `size()I` IS DELIBERATELY *NOT* LIFTED — READ THIS BEFORE "FINISHING" IT.**
            // Claiming a site costs the PIC that site would otherwise have emitted, so an arm may
            // only be lifted where its (name, descriptor) key does not OVER-APPROXIMATE the shapes
            // it can serve. `getDouble(I)D`, `next()Ljava/lang/Object;`, `hasNext()Z` and
            // `getKey`/`getValue()Ljava/lang/Object;` name the exact shapes the arms answer, and
            // at 0xb6 they can only be reached by a concretely-typed receiver of one of them.
            // `size()I` is the opposite: it is the commonest zero-arg name in the JDK and in guest
            // code, and `cube_ready()` is true as soon as ONE `CubePointRange` exists anywhere, so
            // lifting it would plant the cube arm on every `size()` site in the program and hand
            // each non-matching receiver to a callback instead of the PIC. This engine has already
            // paid for that exact mistake once — widening the `List.iterator()` admission by one
            // owner made a probe 25% faster and the BOOT 0.7 s slower, because "a probe reproduces
            // the SHAPE, not the receiver DISTRIBUTION". So both `size()I` arms keep the old group
            // predicate verbatim, and lifting them is a separate, measured change.
            #[cfg(feature = "next-gen-gc")]
            if !arena_inline_disabled()
                && (!emit_invokeops_disabled()
                    || (crate::jit::alist_size_inline_enabled() && env.elems_fid > 0))
            {
                let m = pool.member_ref(idx)?;
                // The old group predicate, retained on the two `size()I` arms only — see the
                // over-approximation note above. Under the kill switch it is implied by the outer
                // condition, so this arm's emission is unchanged in both directions.
                let size_arms_ok =
                    crate::jit::alist_size_inline_enabled() && env.elems_fid > 0;
                if m.name == "size"
                    && m.descriptor == "()I"
                    && size_arms_ok
                    && crate::jit::emit::iterator_inline::enabled()
                    && crate::jit::emit::iterator_inline::cube_ready()
                    && !stack.is_empty()
                {
                    crate::jit::emit::iterator_inline::emit_cube_size_inline(
                        b, env, idx, vars, stack, locals_ptr, array_desc_cell, 1, pc,
                    )?;
                    if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_inline(); }
                    return Some(());
                }
                if m.name == "getDouble"
                    && m.descriptor == "(I)D"
                    && crate::jit::emit::iterator_inline::enabled()
                    && crate::jit::emit::iterator_inline::cube_ready()
                    && stack.len() >= 2
                {
                    crate::jit::emit::iterator_inline::emit_cube_getdouble_inline(
                        b, env, idx, vars, stack, locals_ptr, array_desc_cell, 1, pc,
                    )?;
                    return Some(());
                }
                if m.name == "next"
                    && m.descriptor == "()Ljava/lang/Object;"
                    && crate::jit::emit::iterator_inline::enabled()
                    && crate::jit::emit::iterator_inline::ready()
                    && !stack.is_empty()
                {
                    crate::jit::emit::iterator_inline::emit_iter_next_inline(
                        b, env, idx, vars, stack, locals_ptr, array_desc_cell, 1, pc,
                    )?;
                    return Some(());
                }
                if m.name == "hasNext"
                    && m.descriptor == "()Z"
                    && crate::jit::emit::iterator_inline::enabled()
                    && crate::jit::emit::iterator_inline::ready()
                    && !stack.is_empty()
                {
                    crate::jit::emit::iterator_inline::emit_iter_hasnext_inline(
                        b, env, idx, vars, stack, locals_ptr, array_desc_cell, 1, pc,
                    )?;
                    return Some(());
                }
                // `Map$Entry.getKey`/`getValue` — one field read on a two-field fabricated
                // object, 464,896 crossings a boot. Bound by the class WORD at runtime, so an
                // unrelated entry shape simply takes the callback.
                if (m.name == "getKey" || m.name == "getValue")
                    && m.descriptor == "()Ljava/lang/Object;"
                    && crate::jit::entry_inline_enabled()
                    && crate::jit::entry_ready()
                    && !stack.is_empty()
                {
                    crate::jit::emit::collections::emit_entry_get_inline(
                        b, env, idx, vars, stack, locals_ptr, array_desc_cell, 1, m.name == "getKey",
                    )?;
                    return Some(());
                }
                if m.name == "getDouble"
                    && m.descriptor == "(I)D"
                    && crate::jit::dalist_get_inline_enabled()
                    // The arm bakes `a` and `size` by id, so it is emitted only once a live
                    // `DoubleArrayList` has published them (and its class word). `env.elems_fid`
                    // — the iterator's synthetic — was the WRONG precondition and the wrong read.
                    && {
                        // Counted: is the gate reached BEFORE the ids exist (a compile-order
                        // problem) or not reached at all (a different refusal upstream)? Two
                        // wrong conclusions today came from not asking this.
                        // ⚠**AND THE READOUT NOW HAS TWO WRITERS.** Until the group predicate
                        // above was split, this arm was unreachable on a default boot and every
                        // `DALIST_GATE_*` tick came from the `invokeinterface` twin. Both sites
                        // bump the same pair now, so the totals are 0xb6+0xb9 and no longer
                        // attributable to the interface path alone.
                        let r = crate::jit::dalist_ready();
                        if r { crate::jit::DALIST_GATE_READY.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
                        else { crate::jit::DALIST_GATE_COLD.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
                        r
                    }
                    && stack.len() >= 2
                {
                    crate::jit::emit::collections::emit_dalist_getdouble_inline(
                        b, env, idx, vars, stack, locals_ptr, array_desc_cell, 1,
                    )?;
                    return Some(());
                }
                // ⭐**THE ONE ARM THE TWO HOISTED TERMS WERE EVER ABOUT.** `$elems`-backed
                // `ArrayList.size()`: opt-in under `RAVA_ALIST_SIZE_INLINE`, and it reads the
                // `$elems` field id, which the driver only resolves when an `$elems` arm is armed
                // (so `env.elems_fid` is 0 on a default boot and the arm must refuse). Both terms
                // now sit ON the arm instead of on the group, so switching this experiment off no
                // longer switches off the five arms above it. Reached only after `cube_size` has
                // declined, exactly as on the interface path.
                if m.name == "size"
                    && m.descriptor == "()I"
                    && size_arms_ok
                    && !stack.is_empty()
                {
                    if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_inline(); }
                    crate::jit::emit::collections::emit_arraylist_size_inline(
                        b, env, idx, vars, stack, locals_ptr, array_desc_cell, 1,
                    )?;
                    return Some(());
                }
            }
            // FASTUTIL int-list scalar fast-path: getInt/size/isEmpty/iterator served
            // off the backing int[] (no fastutil-bytecode synthetic frame), else the
            // helper falls back to the identical general dispatch.
            #[cfg(feature = "next-gen-gc")]
            if !arena_inline_disabled() {
                let m = pool.member_ref(idx)?;
                // java.util.ArrayList.get(I): native-served ($elems). `fu_fastpath` already
                // answers it, and still costs 187 ns per element because reaching it leaves
                // machine code. Read the element inline instead; the same slow arm
                // (`env.fu_invoke`) covers a subclass, an absent backing, a bad index and the
                // first calls before the class word is published. Same live-reference-operand
                // restriction as the BitSet arm below, for the same SSA reason.
                if (m.owner == "java/util/ArrayList" || m.owner == "java/util/List")
                    && m.name == "get"
                    && m.descriptor == "(I)Ljava/lang/Object;"
                    && crate::jit::alist_get_inline_enabled()
                    && env.elems_fid > 0
                    && stack.len() >= 2
                {
                    if std::env::var_os("RAVA_ALIST_DBG").is_some() {
                        eprintln!("[ALIST] EMIT fid={}", env.elems_fid - 1);
                    }
                    if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_inline(); }
                    crate::jit::emit::collections::emit_arraylist_get_inline(
                        b, env, idx, vars, stack, locals_ptr, array_desc_cell, 1,
                    )?;
                    return Some(());
                }
                // ⭐**THE SITE-KEYED ARENA GETTER, ahead of the routable callback it removes.**
                // Emitted for every zero-argument routable site, because whether the site can be
                // described is only known at RUNTIME: `trivial_getter`'s entry is written by the
                // first crossing, which happens AFTER this method compiled. So the test is in the
                // emitted code, not here — the same bootstrap trap that kept three earlier arms
                // at zero. The fallback is the identical `fu_invoke` callback.
                if fu_routable(&m.owner, &m.name, &m.descriptor) {
                    let desc = m.descriptor.to_string();
                    if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_inline(); }
                    emit_invoke_via(b, stack, env, idx, 1, &desc, vars, locals_ptr, env.fu_invoke)?;
                    return Some(());
                }
                // Enum.ordinal(): final + native-served, so no direct-call/PIC path
                // exists — route to the $enumOrd fast helper (general-dispatch
                // fallback inside covers any non-enum `ordinal()I` receiver).
                if m.name == "ordinal" && m.descriptor == "()I" {
                    emit_invoke_via(b, stack, env, idx, 1, "()I", vars, locals_ptr, env.enum_ordinal)?;
                    return Some(());
                }
                // java.util.BitSet get/set/clear/isEmpty: native-served ($bits), so
                // no direct-call/PIC path exists — route to the packed-word fast
                // helpers (general-dispatch fallback inside covers subclasses,
                // growth-needing sets, and any $bits-less receiver).
                // Bisect gate: RAVA_BITSET_FP=0 disables all; a letter set
                // (g=get s=set c=clear e=isEmpty) enables selectively.
                if m.owner == "java/util/BitSet" {
                    let sel = {
                        static V: std::sync::OnceLock<String> = std::sync::OnceLock::new();
                        V.get_or_init(|| {
                            std::env::var("RAVA_BITSET_FP").unwrap_or_else(|_| "gsce".into())
                        })
                    };
                    let on = |c: char| sel != "0" && (sel == "1" || sel.contains(c));
                    // Inline `BitSet.get(I)Z`: read the packed `$bits` word directly
                    // (no FFI) on the exact-class heap fast path; `env.bitset_get`
                    // stays the slow-arm fallback for idx<0 / subclass / missing-$bits.
                    // Restricted to sites with no live reference operand BELOW recv+idx
                    // so the slow arm's locals-only reload after the (possibly throwing)
                    // callback is SSA-sound on the diamond.
                    if m.name == "get"
                        && m.descriptor == "(I)Z"
                        && on('g')
                        && bitset_get_inline_enabled()
                        && env.bits_fid > 0
                        && !arena_inline_disabled()
                        && stack.len() >= 2
                        && stack[..stack.len() - 2].iter().all(|sv| sv.t != JT::Ref)
                    {
                        if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_inline(); }
                        emit_bitset_get_inline(b, env, idx, vars, stack, locals_ptr, array_desc_cell)?;
                        return Some(());
                    }
                    if m.name == "isEmpty"
                        && m.descriptor == "()Z"
                        && on('e')
                        && bitset_isempty_inline_enabled()
                        && env.bits_fid > 0
                        && !arena_inline_disabled()
                    {
                        if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_inline(); }
                        emit_bitset_isempty_inline(
                            b,
                            env,
                            idx,
                            vars,
                            stack,
                            locals_ptr,
                            array_desc_cell,
                        )?;
                        return Some(());
                    }
                    let via = match (&*m.name, &*m.descriptor) {
                        ("get", "(I)Z") if on('g') => Some(env.bitset_get),
                        ("set", "(I)V") | ("set", "(IZ)V") if on('s') => Some(env.bitset_set),
                        ("clear", "(I)V") if on('c') => Some(env.bitset_clear),
                        ("isEmpty", "()Z") if on('e') => Some(env.bitset_isempty),
                        _ => None,
                    };
                    if let Some(via) = via {
                        let desc = m.descriptor.to_string();
                        emit_invoke_via(b, stack, env, idx, 1, &desc, vars, locals_ptr, via)?;
                        return Some(());
                    }
                }
            }
            // COLLECTION INTRINSIC: `HashMap.put` direct — pop recv/key/val and store
            // via the native map table, bypassing rava_invoke's synthetic frame.
            #[cfg(feature = "next-gen-gc")]
            if !arena_inline_disabled() {
                let m = pool.member_ref(idx)?;
                // STEP 3: arena-map HashMap intrinsics. Under RAVA_ARENA_MAPS a
                // HashMap is an arena ArenaMap; drive the flat-table probe from a
                // direct native helper instead of the interpreter invokevirtual the
                // flag otherwise pays. Spill/reload around each call — the helpers can
                // collect (a put resize, or an object key's structural hash).
                // fastutil Int2ObjectOpenHashMap.get(int)/containsKey(int): the key is
                // ALREADY a raw int on the stack (no Integer.valueOf fusion needed), so
                // route straight to the zero-FFI register probe. The probe's class-id
                // guard falls back (rava_amap_get → jit_map_get for a heap-$map subclass
                // like Paper LoadingIntMap) when the receiver isn't an arena ArenaMap.
                if crate::arena_map::enabled()
                    && m.owner == "it/unimi/dsi/fastutil/ints/Int2ObjectOpenHashMap"
                {
                    let int_probe = match (&*m.name, &*m.descriptor) {
                        ("get", "(I)Ljava/lang/Object;") => Some(false),
                        ("containsKey", "(I)Z") => Some(true),
                        _ => None,
                    };
                    if let Some(is_contains) = int_probe {
                        if stack.last().is_some_and(|sv| sv.t == JT::Int) {
                            let key = stack.pop()?;
                            let recv = stack.pop()?;
                            if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_inline(); }
                            emit_amap_int_probe(b, env, recv.v, key.v, is_contains, vars, stack, locals_ptr)?;
                            return Some(());
                        }
                    }
                }
                if crate::arena_map::enabled() && m.owner == "java/util/HashMap" {
                    // ZERO-FFI REGISTER PROBE: a get/containsKey whose key is a raw
                    // int (the Integer.valueOf+get fusion left it unboxed, JT::Int)
                    // runs the pure-IR open-addressing loop — no helper on the hot path.
                    let int_probe = match (&*m.name, &*m.descriptor) {
                        ("get", "(Ljava/lang/Object;)Ljava/lang/Object;") => Some(false),
                        ("containsKey", "(Ljava/lang/Object;)Z") => Some(true),
                        _ => None,
                    };
                    if let Some(is_contains) = int_probe {
                        if stack.last().is_some_and(|sv| sv.t == JT::Int) {
                            let key = stack.pop()?;
                            let recv = stack.pop()?;
                            if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_inline(); }
                            emit_amap_int_probe(b, env, recv.v, key.v, is_contains, vars, stack, locals_ptr)?;
                            return Some(());
                        }
                    }
                    let (f, two_arg, ret_int) = match (&*m.name, &*m.descriptor) {
                        ("get", "(Ljava/lang/Object;)Ljava/lang/Object;") => (Some(env.amap_get_fn), true, false),
                        ("remove", "(Ljava/lang/Object;)Ljava/lang/Object;") => (Some(env.amap_remove_fn), true, false),
                        ("containsKey", "(Ljava/lang/Object;)Z") => (Some(env.amap_contains_fn), true, true),
                        ("put", "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;") => (Some(env.amap_put_fn), false, false),
                        _ => (None, false, false),
                    };
                    if let Some(f) = f {
                        let valc = if two_arg { None } else { Some(to_cell(b, stack.pop()?)) };
                        let key = stack.pop()?;
                        let recv = stack.pop()?;
                        let keyc = to_cell(b, key);
                        spill_for_gc(b, vars, stack, locals_ptr);
                        let call = match valc {
                            Some(vc) => b.ins().call(f, &[recv.v, keyc, vc]),
                            None => b.ins().call(f, &[recv.v, keyc]),
                        };
                        let res = b.inst_results(call)[0];
                        reload_after_gc(b, vars, stack, locals_ptr);
                        if ret_int {
                            let r = b.ins().ireduce(I32, res);
                            push(stack, r, JT::Int);
                        } else {
                            push(stack, res, JT::Ref);
                        }
                        return Some(());
                    }
                }
                if !crate::arena_map::enabled() && m.owner == "java/util/HashMap"
                    && m.name == "put"
                    && m.descriptor == "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;"
                {
                    let val = stack.pop()?;
                    let key = stack.pop()?;
                    let recv = stack.pop()?;
                    let valc = to_cell(b, val);
                    let keyc = to_cell(b, key);
                    spill_for_gc(b, vars, stack, locals_ptr);
                    let call = b.ins().call(env.map_put_fn, &[recv.v, keyc, valc]);
                    let res = b.inst_results(call)[0];
                    reload_after_gc(b, vars, stack, locals_ptr);
                    push(stack, res, JT::Ref);
                    return Some(());
                }
                // COLLECTION INTRINSIC: `HashMap.get(key)` direct — the read partner
                // of `put`. A statically `java/util/HashMap`-typed receiver always
                // carries a native `$map`, so the lookup bypasses rava_invoke's
                // synthetic frame. `getOrDefault` uses the normal invocation path:
                // this callback cannot distinguish absence from a mapped null.
                if !crate::arena_map::enabled() && m.owner == "java/util/HashMap"
                    && m.name == "get"
                    && m.descriptor == "(Ljava/lang/Object;)Ljava/lang/Object;"
                {
                    let key = stack.pop()?;
                    let recv = stack.pop()?;
                    let keyc = to_cell(b, key);
                    spill_for_gc(b, vars, stack, locals_ptr);
                    let call = b.ins().call(env.map_get_fn, &[recv.v, keyc]);
                    let got = b.inst_results(call)[0];
                    reload_after_gc(b, vars, stack, locals_ptr);
                    push(stack, got, JT::Ref);
                    return Some(());
                }
                // COLLECTION INTRINSIC: `HashMap.remove(key)` direct (mutating).
                if !crate::arena_map::enabled() && m.owner == "java/util/HashMap"
                    && m.name == "remove"
                    && m.descriptor == "(Ljava/lang/Object;)Ljava/lang/Object;"
                {
                    let key = stack.pop()?;
                    let recv = stack.pop()?;
                    let keyc = to_cell(b, key);
                    spill_for_gc(b, vars, stack, locals_ptr);
                    let call = b.ins().call(env.map_remove_fn, &[recv.v, keyc]);
                    let res = b.inst_results(call)[0];
                    reload_after_gc(b, vars, stack, locals_ptr);
                    push(stack, res, JT::Ref);
                    return Some(());
                }
                // COLLECTION INTRINSIC: `HashMap.containsKey(key)` direct → boolean.
                // Uses the presence check (not get != null), so a null-valued key
                // still reports true. No GC (a pure read), so no spill/reload.
                if !crate::arena_map::enabled() && m.owner == "java/util/HashMap"
                    && m.name == "containsKey"
                    && m.descriptor == "(Ljava/lang/Object;)Z"
                {
                    let key = stack.pop()?;
                    let recv = stack.pop()?;
                    let keyc = to_cell(b, key);
                    spill_for_gc(b, vars, stack, locals_ptr);
                    let call = b.ins().call(env.map_contains_fn, &[recv.v, keyc]);
                    let res = b.inst_results(call)[0];
                    reload_after_gc(b, vars, stack, locals_ptr);
                    let r = b.ins().ireduce(I32, res);
                    push(stack, r, JT::Int);
                    return Some(());
                }
                // Integer.intValue() — unbox the ref directly (no synthetic frame).
                if m.owner == "java/lang/Integer" && m.name == "intValue" && m.descriptor == "()I" {
                    let recv = stack.pop()?;
                    let call = b.ins().call(env.unbox_int_fn, &[recv.v]);
                    let res = b.inst_results(call)[0];
                    let v = b.ins().ireduce(I32, res);
                    push(stack, v, JT::Int);
                    return Some(());
                }
                // String.equals(Object) / String.isEmpty() — a load and a compare when
                // the receiver (and, for `equals`, the argument) is this engine's arena
                // string; the ordinary `invokevirtual` dispatch is the cold arm, so a
                // refused guard is only slower. Every guard, the null and typed-`equals`
                // semantics and the kill switch `RAVA_NO_FAST_STRINGS3=1` live with the
                // emission in `jit::emit::strings`; the whole decision is inside
                // `emit_str_ops3`, which claims NOTHING it cannot finish (it checks the
                // operand depth before its first `pop`), so `None` leaves this site to
                // the arms below it exactly as before.
                if m.owner == "java/lang/String"
                    && emit_str_ops3(b, env, stack, vars, locals_ptr, idx, m.name, m.descriptor)
                        .is_some()
                {
                    return Some(());
                }
                // String.charAt(int) — an ASCII ARENA-STRING receiver reads the
                // character with an inline `load.i8`; anything else takes a thin
                // callback. See `emit_str_charat` for the ASCII guard.
                if m.owner == "java/lang/String"
                    && m.name == "charAt"
                    && m.descriptor == "(I)C"
                    && str_charat_intrinsic_enabled()
                {
                    let idx = stack.pop()?;
                    let recv = stack.pop()?;
                    let v = emit_str_charat(b, env, recv.v, idx.v);
                    push(stack, v, JT::Int);
                    return Some(());
                }
                // String.length()/hashCode() — an ARENA-STRING receiver reads the
                // cached value with a native `load.i32`; a boxed Str takes the thin
                // callback. (startsWith has its own inline byte compare.)
                if m.owner == "java/lang/String"
                    && ((m.name == "length" && m.descriptor == "()I")
                        || (m.name == "hashCode" && m.descriptor == "()I"))
                {
                    let is_hash = m.name == "hashCode";
                    // A null receiver must use invocation's exception contract and
                    // leave this body before any later bytecode can have effects.
                    let recv_cell = stack.last()?.v;
                    let nonnull = b.create_block();
                    let fault = b.create_block();
                    let is_null = b.ins().icmp_imm(IntCC::Equal, recv_cell, 0);
                    b.ins().brif(is_null, fault, &[], nonnull, &[]);
                    b.switch_to_block(fault);
                    b.seal_block(fault);
                    // Cold-arm reloads must not replace the nonnull arm's SSA values.
                    let mut fault_stack = stack.clone();
                    spill_for_gc(b, vars, &fault_stack, locals_ptr);
                    let caller = b.ins().iconst(I64, env.class_ptr);
                    let code = b.ins().iconst(I64, env.code_key as i64);
                    let site = b.ins().iconst(I64, pc as i64);
                    b.ins().call(env.npe_invoke_site, &[caller, code, site]);
                    reload_after_gc(b, vars, &mut fault_stack, locals_ptr);
                    let zero = b.ins().iconst(I64, 0);
                    let zero = emit_deep_epilogue(b, env, dv, zero);
                    let zero = abi_ret_from_cell(b, zero);
                    b.ins().return_(&[zero]);
                    b.switch_to_block(nonnull);
                    b.seal_block(nonnull);
                    let recv = stack.pop()?;
                    let arena = b.create_block();
                    let slow = b.create_block();
                    let merge = b.create_block();
                    b.append_block_param(merge, I32);
                    let is_a = emit_is_arena(b, recv.v);
                    b.ins().brif(is_a, arena, &[], slow, &[]);

                    b.switch_to_block(arena);
                    b.seal_block(arena);
                    let off = if is_hash {
                        crate::nextgen::STR_JIT_HASH_OFF
                    } else {
                        crate::nextgen::STR_JIT_CLEN_OFF
                    };
                    let val = b.ins().load(I32, MemFlags::trusted(), recv.v, Offset32::new(off));
                    b.ins().jump(merge, &[val.into()]);

                    b.switch_to_block(slow);
                    b.seal_block(slow);
                    let f = if is_hash { env.str_hash_fn } else { env.str_length_fn };
                    let call = b.ins().call(f, &[recv.v]);
                    let res = b.inst_results(call)[0];
                    let r = b.ins().ireduce(I32, res);
                    b.ins().jump(merge, &[r.into()]);

                    b.switch_to_block(merge);
                    b.seal_block(merge);
                    let v = b.block_params(merge)[0];
                    push(stack, v, JT::Int);
                    return Some(());
                }
                // String.startsWith(String) — when BOTH receiver and prefix are
                // arena strings, compare bytes natively (no callback); otherwise
                // a thin callback. recv.startsWith(prefix).
                if m.owner == "java/lang/String"
                    && m.name == "startsWith"
                    && m.descriptor == "(Ljava/lang/String;)Z"
                {
                    let prefix = stack.pop()?;
                    let recv = stack.pop()?;
                    let v = emit_str_startswith(b, env, recv.v, prefix.v);
                    push(stack, v, JT::Int);
                    return Some(());
                }
            }
            if let Some(callee) = env.inline_targets.get(&idx) {
                // ⚠**A GUARDED CALLEE MUST NOT BE SPLICED UNGUARDED.** `guard_class` is set by
                // the profile-driven admission for interface and non-final virtual sites, where
                // the body is only correct for ONE receiver class. Until the guard is emitted,
                // refuse it here: the site keeps its ordinary call and the admission counter
                // still reports how many sites the optimisation would reach.
                if !callee.is_static
                    && callee.guard_class.is_none()
                    && callee.guard_ref_word.is_none()
                {
                    if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_inline(); }
                    emit_inline(b, stack, env, array_desc_cell, callee, locals_ptr, cow, #[cfg(feature = "next-gen-gc")] licm, #[cfg(feature = "next-gen-gc")] dv, #[cfg(feature = "next-gen-gc")] None, #[cfg(feature = "next-gen-gc")] vars)?;
                    return Some(());
                }
                // GUARDED DEVIRTUALISATION: the callee is only correct for the one receiver class
                // the profile saw, so it runs behind a `class_id` check with the ordinary
                // dispatch as the else arm. See `jit::emit::guarded_inline`.
                #[cfg(feature = "next-gen-gc")]
                if !callee.is_static
                    && (callee.guard_class.is_some() || callee.guard_ref_word.is_some())
                {
                    let desc = pool.member_ref(idx)?.descriptor.to_string();
                    let r = crate::jit::emit::guarded_inline::emit_guarded_inline(
                        b, stack, env, array_desc_cell, callee, locals_ptr, cow, vars, pc, idx, licm, dv,
                        |b, stack| {
                            // ⚠**THE SLOW ARM MUST BE WHAT THE SITE WOULD HAVE EMITTED, EXACTLY.**
                            // Substituting `emit_invoke` for the PIC here — as a bisection step,
                            // with the diamond removed and the guard forced false — still HUNG
                            // ChunkFill on a single site. The two are not interchangeable at a
                            // site the PIC serves, so the guarded path reproduces the original
                            // choice rather than picking the simpler emission.
                            if !arena_inline_disabled()
                                && wide_site_pic_ok(pool, idx, &desc)
                                && arg_slot_widths(&desc).iter().sum::<usize>() <= dispatch_callbuf_slots()
                            {
                                let heap_mode = crate::heappic::emit_mode();
                                emit_dispatch_pic(b, stack, env, idx, 1, &desc, vars, locals_ptr, array_desc_cell, heap_mode, pc)
                            } else {
                                emit_invoke(b, stack, env, idx, 1, &desc, vars, locals_ptr)
                            }
                        },
                    );
                    use crate::jit::emit::guarded_inline::Guarded;
                    match r {
                        Guarded::Emitted => { if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_inline(); } return Some(()) },
                        Guarded::Aborted => return None,
                        // Nothing emitted: fall through to the ordinary dispatch below.
                        Guarded::Declined => {}
                    }
                }
            }
            let desc = pool.member_ref(idx)?.descriptor.to_string();
            #[cfg(feature = "next-gen-gc")]
            if crate::jit::pic_elig_enabled() {
                if let Some(m) = pool.member_ref(idx) {
                    crate::jit::pic_elig_note(
                        1, &m.owner, &m.name, &m.descriptor,
                        !crate::jit::arena_inline_disabled(),
                        crate::jit::wide_site_pic_ok(pool, idx, &desc),
                        crate::jit::arg_slot_widths(&desc).iter().sum::<usize>() <= crate::jit::dispatch_callbuf_slots(),
                    );
                }
            }
            if !arena_inline_disabled()
                && wide_site_pic_ok(pool, idx, &desc)
                && arg_slot_widths(&desc).iter().sum::<usize>() <= dispatch_callbuf_slots()
            {
                // Wide (long/double) params supported: the PIC buffer is laid out
                // in the callee's LOCAL-SLOT positions (see emit_dispatch_pic).
                if crate::picprobe::enabled() { crate::picprobe::site_pic(); }
                // 23.4a/b: heap-Ref PIC (shadow-verify or live) only for DiscreteVoxelShape sites.
                // 23.4b live heap-Ref PIC: emit at every virtual/interface site (leaf-only cacheability in
                // heappic_fill filters WHAT actually caches — non-leaf classes stay -1/miss). DVS-only was
                // a no-op (its getSize/getXSize pool is non-leaf); the win is the general leaf heap-Ref pool.
                let heap_mode = crate::heappic::emit_mode();
                if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_pic(); }
                emit_dispatch_pic(b, stack, env, idx, 1, &desc, vars, locals_ptr, array_desc_cell, heap_mode, pc)?;
                return Some(());
            }
            if crate::picprobe::enabled() { crate::picprobe::site_invoke(1); }
            if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_cb(); }
            emit_invoke(b, stack, env, idx, 1, &desc, vars, locals_ptr)?; // kind 1 = virtual
        }
        // invokedynamic — concat (→ ArenaString) / lambda / record via callback,
        // so a string-concat hot loop compiles (next-gen-gc).
        #[cfg(feature = "next-gen-gc")]
        _ => return None,
    }
    Some(())
}
