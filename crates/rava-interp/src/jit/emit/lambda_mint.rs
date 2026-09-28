//! **MINT THE CLOSURE IN EMITTED CODE.**
//!
//! `RAVA_DIAG=indy`: 2,537,018 LambdaMetafactory assemblies a datapack run, only 6,188 of them
//! non-capturing. Each of the rest returns to Rust to pop its captures and bump an arena object,
//! and the region profile prices that group at **7.4% of the compiled region** - the biggest
//! single item in the 51.5% that compiled code spends inside the runtime.
//!
//! `emit_inline_concat` already does exactly this for the StringConcatFactory arm; this is the
//! same shape: a TLAB bump test, the header the Rust mint writes, then the cells.
//!
//! The shape - capture-signature class id, interned impl-site index, MethodHandle kind and the
//! `$fi` cell - does not exist until the site bootstrap has been resolved, so it is read from
//! `jit::lambda_shape` AT RUNTIME and the guard refuses while the entry is zero. Baking it would
//! be the bootstrap trap that kept three arms at zero earlier today.

use crate::jit::*;

// ⭐**THE ARITY LIMIT WAS NEVER AN ARITY LIMIT — IT WAS TWO CELL ABIs WEARING ONE TYPE.**
//
// Twelve hypotheses were measured and discarded against this arm (the header verifier, the
// capture print, the 2,172 correct mints and the Cranelift IR itself all agreed the emitter's
// plumbing was clean), and the note that stood here concluded "whether this SITE should be
// minting at all". It should. What was wrong is one conversion the arm never performed:
//
//   * a JT::Ref operand in compiled code is a **high-word** cell (`jit::abi`): `0` null, a raw
//     arena pointer (`cell >> 40 == 0`), `h|REF_TAG` = `1<<40`, `h|ARRAY_TAG` = `1<<42`,
//     `idx|BOX_TAG` = `1<<41`;
//   * an arena capture cell (`opcodes::encode_arena_cell`) is a **low-2-bit** cell: `0b00` raw
//     arena pointer / null, `0b01` `(h<<2)|1` heap handle, `0b10` `(h<<2)|2` array handle,
//     `0b11` box index.
//
// The two agree on exactly ONE form — the raw arena pointer. That is why every mint that ever
// worked captured arena objects, and why `StateDefinition` cp=119 (five captures, one of them a
// heap handle) died: `h|REF_TAG` decoded as a low-2-bit cell is `(h|1<<40) & 3`, which is `0`
// whenever `h` is 4-aligned — then `is_arena_obj_addr` rejects the address and
// `decode_arena_cell` yields **`Value::Null`, the null local** — and `1` whenever `h ≡ 1 (mod 4)`,
// yielding `Value::Ref(cell >> 2)`, an unrelated LIVE handle — **the InternalError the spill probe
// produced when it perturbed which handles were in play**. Two recorded symptoms, one cause, and
// it explains why moving `spill_for_gc` "changed the symptom" without being the fix.
//
// The arm now converts the tag (or refuses the cell), so a heap-handle or array capture is minted
// exactly as `encode_capture_cell` would have written it. Primitives needed no conversion and
// never did: `to_cell` and `encode_arena_cell` are byte-identical for I/J/F/D (sign-extend,
// pass-through, zero-extended f32 bits, f64 bits).
//
// What still refuses, and why it must:
//   * `BOX_TAG` (`cell >> 40 == 2`) — an index into the TRANSIENT frame box table, which unwinds
//     with the compiled frame. A closure outlives the frame, so the cell would dangle.
//   * a capture whose static descriptor kind disagrees with its operand's JIT type.
//   * any site whose RECORDED shape disagrees with the descriptor (arity, kind, or a capture
//     nibble) — see the emit-time admission below.
//
// What no longer refuses (HR-INDY, `RAVA_NO_HR_INDY=1`): a site in a method with NO hoisted
// TLAB cursor — `emit_inline_lambda_sitecursor` fetches one at the site. Every refusal above,
// at emit time and at run time, is now COUNTED (`lambda_shape::EMIT_REFUSE` / `RT_REFUSE`) and
// printed as `[PC-INDY]` by the periodic profiler dump.

/// The recorder's capture-kind nibbles, derived from the indy descriptor exactly as
/// `arena_lambda_shape` derives them (`concat_param_types`, then J/F/D/ref). Shared parse, so an
/// emit-time cross-check against the recorded nibbles compares the SAME function's output on both
/// sides and can only ever fail when the two sides are looking at different descriptors.
fn cap_kinds_of(desc: &str) -> Option<Vec<u8>> {
    let types = crate::opcodes::concat_param_types(desc);
    if types.len() != crate::count_args(desc) {
        return None;
    }
    let mut out = Vec::with_capacity(types.len());
    for t in types {
        out.push(match t {
            b'J' => 1u8,
            b'F' => 2,
            b'D' => 3,
            b'L' | b'[' => 4,
            b'I' | b'S' | b'B' | b'C' | b'Z' => 0,
            _ => return None, // not a descriptor parameter char — refuse rather than guess
        });
    }
    Some(out)
}

/// The JIT operand type a capture of kind `k` must have. `encode_capture_cell` BOXES a primitive
/// that lands in a reference slot — a side table emitted code cannot reach — so the agreement is
/// not a nicety, it is the precondition for storing the operand at all.
fn jt_of_kind(k: u8) -> JT {
    match k {
        1 => JT::Long,
        2 => JT::Float,
        3 => JT::Double,
        4 => JT::Ref,
        _ => JT::Int,
    }
}

/// The five `MethodHandle` reference kinds `LambdaMetafactory` accepts as an implementation
/// method handle (JVMS 4.4.8 / `LambdaMetafactory.metafactory`): invokeVirtual, invokeStatic,
/// invokeSpecial, newInvokeSpecial, invokeInterface.
///
/// ⛔**The old set was `{6,7,8}` and the note beside it blamed kind 5 for the arity-3 failure.**
/// That was wrong — the failure was the capture cell ABI above — and the kind is not something
/// this arm can be wrong about in the first place: the Rust birth path stores it as a pass-through
/// cell (`tlab_alloc_lambda_shaped(&shape, kind)` → `AL_KIND_SLOT`) and branches on it NOWHERE
/// between `Plan::Lambda` and the mint. So the set is exactly the kinds a lambda site can legally
/// carry, and a field-accessor kind (1-4) or a garbled zero refuses.
fn kind_is_lambda_impl(k: u8) -> bool {
    matches!(k, 5..=9)
}

fn bump_ctr(b: &mut FunctionBuilder, c: &'static std::sync::atomic::AtomicU64) {
    let addr = b.ins().iconst(I64, (c as *const _) as i64);
    let cur = b.ins().load(I64, MemFlags::trusted(), addr, Offset32::new(0));
    let inc = b.ins().iadd_imm(cur, 1);
    b.ins().store(MemFlags::trusted(), inc, addr, Offset32::new(0));
}

/// Emit the mint, or return `None` to leave the call on its existing path.
///
/// The HOISTED-CURSOR entry: `body.rs` fetched the TLAB cursor in the prologue (the method has
/// an arena `new` or a concat shape) and hands it in as a Variable.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_inline_lambda(
    b: &mut FunctionBuilder,
    env: &Env,
    stack: &mut Vec<SV>,
    vars: &[Variable],
    locals_ptr: Value,
    cursor_var: Variable,
    cp: u16,
    desc: &str,
) -> Option<()> {
    emit_inline_lambda_core(b, env, stack, vars, locals_ptr, Some(cursor_var), cp, desc)
}

/// **THE SITE-CURSOR ENTRY — the mint for a method that has NO hoisted TLAB cursor.**
/// Gated by `lambda_shape::hr_enabled()` (`RAVA_NO_HR_INDY=1` restores the callback).
///
/// ⭐**THE DOMINANT REFUSAL WAS NEVER IN THIS FILE.** `body.rs` creates `cursor_var` only for a
/// method with an arena `new` or a concat shape, and `ops::invoke_dynamic` calls the mint only
/// when it has one — so a method whose ONLY allocation is a closure never reached
/// `emit_inline_lambda` at all, and no counter here could have said so. On the bootstrap bench
/// that is exactly the population: `BitSetDiscreteVoxelShape.lambda$join$2` (10 captures,
/// `[I` + `[Z`), `lambda$join$1` (12 captures), `Shapes.lambda$joinIsNotEmpty$3` / `$2`,
/// `VoxelShape.forAllBoxes` — nested closures minted per row / per strip from bodies that
/// allocate nothing else (read with `javap`; not one of them contains a `new`).
///
/// The cursor is fetched AT THE SITE, on the arm that has already passed the shape guard,
/// with one call to `env.cursor_addr_fn` — the discipline `emit_inline_arena_new_late` already
/// uses for a late-bound `new` in the same situation (`RAVA_NO_INLINE_NEW_SITECURSOR`):
/// `jit_cursor_addr` is idempotent (it syncs `young.top` from the cursor BEFORE republishing,
/// so the cursor never moves backwards), never collects and never allocates, so nothing on the
/// operand stack can move across it and the spill above needs no reload.
///
/// ⚠**ON THE RECORDED "NULL LOCAL" DEATH.** `body.rs` records that once handing this mint a
/// cursor in closure-only methods made a run die on a null local, and refuses the hoist on
/// that memory. The file header above explains that symptom exactly: a `h|REF_TAG` capture
/// stored unconverted decodes as `Value::Null` — the null local — and widening the reach to
/// these methods is what first put a heap-handle capture (an `IndexMerger`, a `BooleanOp`)
/// through the arm. The conversion has since been written; the guard below still refuses every
/// cell form it cannot re-encode. The hoist in `body.rs` is left exactly as it is: this arm
/// costs one leaf call per MINT, not one per entry, and only where the shape guard passed.
#[cfg(feature = "next-gen-gc")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_inline_lambda_sitecursor(
    b: &mut FunctionBuilder,
    env: &Env,
    stack: &mut Vec<SV>,
    vars: &[Variable],
    locals_ptr: Value,
    cp: u16,
    desc: &str,
) -> Option<()> {
    if !crate::jit::lambda_shape::hr_enabled() {
        return None;
    }
    emit_inline_lambda_core(b, env, stack, vars, locals_ptr, None, cp, desc)
}

/// Run-time refusal indices into `lambda_shape::RT_REFUSE` (see its doc for the meanings).
const RT_UNDESCRIBED: i64 = 0;
const RT_KIND_OR_ARITY: i64 = 1;
const RT_CAPTURE_TAG: i64 = 2;
const RT_TLAB_FULL: i64 = 3;
const RT_FORCED_SLOW: i64 = 4;

/// Bump `RT_REFUSE[reason]` from emitted code: a plain load/add/store on a `'static` cell.
fn bump_rt_refuse(b: &mut FunctionBuilder, reason: Value) {
    let base = b.ins().iconst(I64, crate::jit::lambda_shape::rt_refuse_base());
    let off = b.ins().imul_imm(reason, 8);
    let addr = b.ins().iadd(base, off);
    let cur = b.ins().load(I64, MemFlags::trusted(), addr, Offset32::new(0));
    let inc = b.ins().iadd_imm(cur, 1);
    b.ins().store(MemFlags::trusted(), inc, addr, Offset32::new(0));
}

/// The mint proper. `cursor_var` is `Some` on the hoisted-cursor entry and `None` on the
/// site-cursor entry; with `hr_enabled()` off the `None` form is never requested and every
/// `hr`-gated addition below is skipped, so the emitted IR is the pre-round IR verbatim.
#[allow(clippy::too_many_arguments)]
fn emit_inline_lambda_core(
    b: &mut FunctionBuilder,
    env: &Env,
    stack: &mut Vec<SV>,
    vars: &[Variable],
    locals_ptr: Value,
    cursor_var: Option<Variable>,
    cp: u16,
    desc: &str,
) -> Option<()> {
    use crate::jit::lambda_shape::note_emit_refuse as refuse;
    let hr = crate::jit::lambda_shape::hr_enabled();
    if cursor_var.is_none() && !hr {
        return None;
    }
    if !crate::jit::lambda_shape::mint_admits_site(env.class_ptr as usize, cp) { return None; } // HR-UNDESC (added line; `RAVA_NO_HR_UNDESC=1` admits everything): a non-LambdaMetafactory site has no shape to record and would refuse `undescribed` forever — see `lambda_shape::mint_admits_site`.
    let n = crate::count_args(desc);
    // The shape table packs at most `max_captures()` nibbles (`lambda_shape::record` refuses past
    // that — 14 under HR-INDY, the historic 8 without), so the bound is the table's own rather
    // than a guess. `RAVA_LM_MAXN` lowers it for bisection.
    let cap = std::env::var("RAVA_LM_MAXN")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or_else(crate::jit::lambda_shape::max_captures);
    if n == 0 {
        return None; // non-capturing: the site cache's population, not a mint
    }
    if n > cap {
        if hr {
            refuse(1);
        }
        return None;
    }
    if stack.len() < n {
        if hr {
            refuse(2);
        }
        return None;
    }
    let Some(caps) = cap_kinds_of(desc).filter(|c| c.len() == n) else {
        if hr {
            refuse(3);
        }
        return None;
    };
    // EVIDENCE 1 — the operand's static JIT type must be the one its descriptor slot declares.
    // Nothing below can recover from a mismatch: a primitive that reaches a reference slot has to
    // be boxed through `arena_box_push`, which emitted code cannot do.
    for (k, ck) in caps.iter().enumerate() {
        if stack[stack.len() - n + k].t != jt_of_kind(*ck) {
            if hr {
                refuse(4);
            }
            return None;
        }
    }
    // EVIDENCE 2 — when the site ALREADY has a recorded shape, it must agree with the descriptor
    // on every field the mint lays down. A method usually compiles before some of its indy sites
    // have run, so the row is often absent and the emitted guard carries the check instead (that
    // runtime form is worth 20x the coverage of demanding it here). But when the row IS present
    // and disagrees, the honest answer is not to emit at all: the arm would otherwise be planted
    // at a site it can never serve and pay its guard on every execution to find that out again.
    if let (Some((_, _, kind, arity)), Some(packed)) = (
        crate::jit::lambda_shape::shape_at(env.class_ptr as usize, cp),
        crate::jit::lambda_shape::packed_kinds_at(env.class_ptr as usize, cp),
    ) {
        if arity != n || !kind_is_lambda_impl(kind) {
            if hr {
                refuse(5);
            }
            return None;
        }
        for (k, ck) in caps.iter().enumerate() {
            if ((packed >> (k * 4)) & 0xF) as u8 != *ck {
                if hr {
                    refuse(5);
                }
                return None;
            }
        }
    }
    if hr {
        let which = if cursor_var.is_some() { 0 } else { 1 };
        crate::jit::lambda_shape::EMIT_ARMED[which].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    // ⚠**THE SHAPE IS READ AT RUNTIME FOR THE LAYOUT, NOT AT EMIT TIME.** Requiring the entry
    // while emitting looked safer and cost almost everything: a method compiles at its threshold,
    // which is often before one of its indy sites has ever run, so the arm was emitted at 2,167
    // mints instead of tens of thousands. Baking only the ADDRESS and checking the entry in the
    // emitted guard is the same discipline every other arm in this session ended up needing.
    let (k_addr, v_addr, fi_addr, kinds_addr) =
        crate::jit::lambda_shape::entry_addrs(env.class_ptr as usize, cp);

    let mut args: Vec<SV> = Vec::with_capacity(n);
    for _ in 0..n {
        args.push(stack.pop()?);
    }
    args.reverse();

    let ref_slots: Vec<usize> = stack
        .iter()
        .enumerate()
        .filter(|(_, sv)| sv.t == JT::Ref)
        .map(|(i, _)| i)
        .collect();

    let fast = b.create_block();
    let slow = b.create_block();
    // The refusal REASON rides into the slow arm as a block parameter: the guard edge passes
    // the reason it computed, the TLAB-full edge passes its own constant. Absent when `hr` is
    // off, so the historic slow block keeps its historic (empty) signature.
    if hr {
        b.append_block_param(slow, I64);
    }
    let probe = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, I64);
    for _ in &ref_slots {
        b.append_block_param(merge, I64);
    }

    let ka = b.ins().iconst(I64, k_addr);
    let va = b.ins().iconst(I64, v_addr);
    let key = b.ins().load(I64, MemFlags::trusted(), ka, Offset32::new(0));
    let want_key = b.ins().iconst(
        I64,
        crate::jit::lambda_shape::key_for(env.class_ptr as usize, cp) as i64,
    );
    let mine = b.ins().icmp(IntCC::Equal, key, want_key);
    let val = b.ins().load(I64, MemFlags::trusted(), va, Offset32::new(0));
    let described = b.ins().icmp_imm(IntCC::NotEqual, val, 0);
    // KIND AND ARITY, both from the recorded word. The arity check guards the header's `nfields`
    // against a shape whose capture count differs from the indy descriptor's argument count — a
    // bound method reference can carry a capture the descriptor does not list, and laying the
    // header from the descriptor is what once produced a closure that decoded as an array. The
    // kind check bounds the byte to the five `LambdaMetafactory` implementation kinds, so a row
    // that is not a lambda shape at all can never reach the header.
    let kind_rt = b.ins().band_imm(val, 0xFF);
    let k_lo = b.ins().icmp_imm(IntCC::UnsignedGreaterThanOrEqual, kind_rt, 5);
    let k_hi = b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, kind_rt, 9);
    let kind_ok = b.ins().band(k_lo, k_hi);
    let kaddr = b.ins().iconst(I64, kinds_addr);
    let kinds_w = b.ins().load(I64, MemFlags::trusted(), kaddr, Offset32::new(0));
    let arity = b.ins().ushr_imm(kinds_w, 56);
    let arity_ok = b.ins().icmp_imm(IntCC::Equal, arity, n as i64);
    let ok0 = b.ins().band(mine, described);
    let ok1 = b.ins().band(ok0, kind_ok);
    let mut ok = b.ins().band(ok1, arity_ok);

    // ⭐**EVERY REFERENCE CAPTURE IS RE-TAGGED FROM THE JIT CELL ABI INTO THE ARENA CELL ABI.**
    // See the file header: the two encodings agree only on the raw arena pointer, and storing an
    // unconverted `h|REF_TAG` is the null-local / wrong-handle failure this arm carried for a
    // session. Three forms are convertible and everything else refuses:
    //
    //   `cell >> 40 == 0`  raw arena pointer or null — already the arena form; additionally
    //                      required to be 8-aligned, which is what an arena object address always
    //                      is and what a stray small integer in a reference slot is not.
    //   `cell >> 40 == 1`  `h|REF_TAG`   → `(h << 2) | 1`, exactly `encode_arena_cell(Ref)`.
    //   `cell >> 40 == 4`  `h|ARRAY_TAG` → `(h << 2) | 2`, exactly `encode_arena_cell(Array)`.
    //
    // `BOX_TAG` (`>> 40 == 2`) and `ABOX_TAG` (`>> 40 == 8`) refuse: the first indexes the
    // transient per-frame box table, which is gone once the frame unwinds, and the second is a
    // form this arm has no evidence about. A refusal costs the callback that was there anyway.
    let mut tags: Vec<Option<(cranelift::codegen::ir::Value, cranelift::codegen::ir::Value)>> =
        Vec::with_capacity(n);
    for a in &args {
        if a.t == JT::Ref {
            let hi = b.ins().ushr_imm(a.v, 40);
            let is_arena = b.ins().icmp_imm(IntCC::Equal, hi, 0);
            let is_href = b.ins().icmp_imm(IntCC::Equal, hi, 1);
            let is_aref = b.ins().icmp_imm(IntCC::Equal, hi, 4);
            let tagged = b.ins().bor(is_href, is_aref);
            let known = b.ins().bor(is_arena, tagged);
            ok = b.ins().band(ok, known);
            // The untagged arm is the only one whose payload is dereferenced as a pointer, so it
            // is the only one that has to prove alignment.
            let lo3 = b.ins().band_imm(a.v, 7);
            let aligned = b.ins().icmp_imm(IntCC::Equal, lo3, 0);
            let align_ok = b.ins().bor(tagged, aligned);
            ok = b.ins().band(ok, align_ok);
            tags.push(Some((is_href, is_aref)));
        } else {
            tags.push(None);
        }
    }
    // WHICH clause refused, for the census — derived from the same predicates the guard was
    // built from, so it can only ever name a clause that actually failed: not described, else
    // kind/arity, else (the only remaining way `ok` can be false) a capture tag.
    let mut reason: Option<Value> = None;
    if hr {
        let ka_ok = b.ins().band(kind_ok, arity_ok);
        let r_tag = b.ins().iconst(I64, RT_CAPTURE_TAG);
        let r_ka = b.ins().iconst(I64, RT_KIND_OR_ARITY);
        let r_und = b.ins().iconst(I64, RT_UNDESCRIBED);
        let r1 = b.ins().select(ka_ok, r_tag, r_ka);
        reason = Some(b.ins().select(ok0, r1, r_und));
    }
    // Bisection handle: force every site down the fallback arm, which separates a wrong MINT
    // from a wrong hand-back of the captures.
    if std::env::var_os("RAVA_LM_FORCE_SLOW").is_some() {
        ok = b.ins().iconst(cranelift::codegen::ir::types::I8, 0);
        if hr {
            reason = Some(b.ins().iconst(I64, RT_FORCED_SLOW));
        }
    }

    spill_for_gc(b, vars, stack, locals_ptr);
    match reason {
        Some(r) => b.ins().brif(ok, probe, &[], slow, &[r.into()]),
        None => b.ins().brif(ok, probe, &[], slow, &[]),
    };

    b.switch_to_block(probe);
    b.seal_block(probe);
    let nf = (crate::nextgen::AL_HDR_CELLS + n) as i64;
    let size = b.ins().iconst(I64, crate::nextgen::HDR as i64 + nf * 8);
    // The TLAB cursor: the prologue's Variable when the method hoisted one, else fetched HERE,
    // past the shape guard, by the leaf call `emit_inline_arena_new_late` already makes for a
    // late-bound `new` — no GC, no allocation, idempotent (see `emit_inline_lambda_sitecursor`).
    let cur = match cursor_var {
        Some(cv) => b.use_var(cv),
        None => {
            let c = b.ins().call(env.cursor_addr_fn, &[]);
            b.inst_results(c)[0]
        }
    };
    let next = b.ins().load(I64, MemFlags::trusted(), cur, Offset32::new(0));
    let limit = b.ins().load(I64, MemFlags::trusted(), cur, Offset32::new(8));
    let nn = b.ins().iadd(next, size);
    let fits = b.ins().icmp(IntCC::UnsignedLessThanOrEqual, nn, limit);
    if hr {
        let r_full = b.ins().iconst(I64, RT_TLAB_FULL);
        b.ins().brif(fits, fast, &[], slow, &[r_full.into()]);
    } else {
        b.ins().brif(fits, fast, &[], slow, &[]);
    }

    b.switch_to_block(fast);
    b.seal_block(fast);
    b.ins().store(MemFlags::trusted(), nn, cur, Offset32::new(0));
    let cid = {
        let c = b.ins().ushr_imm(val, 40);
        b.ins().iadd_imm(c, -1)
    };
    let nf_sh = b.ins().iconst(I64, nf << 32);
    let hw = b.ins().bor(nf_sh, cid);
    b.ins().store(MemFlags::trusted(), hw, next, Offset32::new(0));
    let zero64 = b.ins().iconst(I64, 0);
    b.ins().store(MemFlags::trusted(), zero64, next, Offset32::new(8));
    if crate::nextgen::HDR > 16 {
        b.ins().store(MemFlags::trusted(), zero64, next, Offset32::new(16));
    }
    let hdr = crate::nextgen::HDR as i32;
    b.ins().store(MemFlags::trusted(), kind_rt, next, Offset32::new(hdr));
    let site_idx = {
        let sx = b.ins().ushr_imm(val, 8);
        b.ins().band_imm(sx, 0xFFFF_FFFF)
    };
    b.ins().store(MemFlags::trusted(), site_idx, next, Offset32::new(hdr + 8));
    let fa = b.ins().iconst(I64, fi_addr);
    let fi = b.ins().load(I64, MemFlags::trusted(), fa, Offset32::new(0));
    b.ins().store(MemFlags::trusted(), fi, next, Offset32::new(hdr + 16));
    b.ins().store(MemFlags::trusted(), zero64, next, Offset32::new(hdr + 24));
    for (i, a) in args.iter().enumerate() {
        let raw = to_cell(b, *a);
        let c = match tags[i] {
            None => raw, // primitive: `to_cell` and `encode_arena_cell` are byte-identical
            Some((is_href, is_aref)) => {
                let h = b.ins().band_imm(raw, 0xFFFF_FFFF);
                let sh = b.ins().ishl_imm(h, 2);
                let as_ref = b.ins().bor_imm(sh, 1);
                let as_arr = b.ins().bor_imm(sh, 2);
                let t0 = b.ins().select(is_href, as_ref, raw);
                b.ins().select(is_aref, as_arr, t0)
            }
        };
        let off = hdr + ((crate::nextgen::AL_HDR_CELLS + i) as i32) * 8;
        b.ins().store(MemFlags::trusted(), c, next, Offset32::new(off));
    }
    bump_ctr(b, &crate::jit::lambda_shape::MINTED);
    if hr && cursor_var.is_none() {
        bump_ctr(b, &crate::jit::lambda_shape::MINTED_SITECURSOR);
    }
    let mut fast_args: Vec<cranelift::codegen::ir::BlockArg> = vec![next.into()];
    for &i in &ref_slots {
        fast_args.push(stack[i].v.into());
    }
    b.ins().jump(merge, &fast_args);

    b.switch_to_block(slow);
    b.seal_block(slow);
    if hr {
        // One plain cell bump on an arm that is about to pay a full `rava_indy` crossing.
        let r = b.block_params(slow)[0];
        bump_rt_refuse(b, r);
    }
    // The fallback hands the captures over in the JIT cell ABI, unconverted — that is what
    // `rava_indy` reads them as (`unbox_ref`), and converting here would corrupt the very path
    // this arm falls back to.
    for (i, a) in args.iter().enumerate() {
        let c = to_cell(b, *a);
        b.ins().stack_store(c, env.args_slot, (i as i32) * 8);
    }
    let buf = b.ins().stack_addr(I64, env.args_slot, 0);
    let cls = b.ins().iconst(I64, env.class_ptr);
    let cpv = b.ins().iconst(I64, cp as i64);
    let nv = b.ins().iconst(I64, n as i64);
    let call = b.ins().call(env.indy, &[cls, cpv, buf, nv]);
    let res = b.inst_results(call)[0];
    reload_locals_after_gc(b, vars, locals_ptr);
    let mut slow_args: Vec<cranelift::codegen::ir::BlockArg> = vec![res.into()];
    for &i in &ref_slots {
        let rv = reload_ref_slot(b, vars.len(), i, stack[i].v, locals_ptr);
        slow_args.push(rv.into());
    }
    b.ins().jump(merge, &slow_args);

    b.switch_to_block(merge);
    b.seal_block(merge);
    let mparams: Vec<Value> = b.block_params(merge).to_vec();
    for (k, &i) in ref_slots.iter().enumerate() {
        stack[i].v = mparams[1 + k];
    }
    let sv = cell_to_sv(b, mparams[0], JT::Ref);
    stack.push(sv);
    Some(())
}

/// **SERVE A WARM NON-CAPTURING `invokedynamic` WITHOUT A CALLBACK.**
///
/// The other half of the indy family. A zero-argument `LambdaMetafactory` site is a
/// `ConstantCallSite`: `opcodes::invokedynamic` links it once, publishes the object into
/// `indy_singletons` / `arena_indy_singletons`, and every later execution is a map lookup plus a
/// push. From compiled code that constant currently costs a full `rava_indy` crossing — the
/// worker sanction, `vm_ref`, a `to_string` of the descriptor, a synthetic `Frame`, the
/// thread-local plan cache, `invokedynamic_assemble` — to reproduce an answer that cannot change.
///
/// So the site gets a baked result cell: load it, and if this site published it, push it. The
/// MISS arm is byte-for-byte the callback that was already there, followed by one call to
/// `lambda_shape::indy_site_publish`, which admits only a cell whose object is immortal AND
/// address-stable (a pinned arena lambda, or a `REF_TAG` handle rooted in `indy_singletons`) and
/// writes nothing otherwise. The publish call is why the arm is gated on the site's BOOTSTRAP
/// below: at a site that can never qualify it would run on every execution forever.
///
/// Returns `None` to leave the site on its existing path.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_indy_site_cache(
    b: &mut FunctionBuilder,
    env: &Env,
    stack: &mut Vec<SV>,
    vars: &[Variable],
    locals_ptr: Value,
    cp: u16,
    desc: &str,
) -> Option<()> {
    // The cache holds ONE reference cell. A site that consumes operands is not a constant call
    // site, and a site that yields a primitive is a record/switch bootstrap computing over its
    // arguments — neither is cacheable and both are refused structurally, not by guard.
    if crate::count_args(desc) != 0 || ret_full(desc) != Some(JT::Ref) {
        return None;
    }
    if !indy_site_is_metafactory(env, cp) {
        return None;
    }
    let (k_addr, v_addr) = crate::jit::lambda_shape::site_entry_addrs(env.class_ptr as usize, cp);
    let want = crate::jit::lambda_shape::key_for(env.class_ptr as usize, cp) as i64;

    let ref_slots: Vec<usize> = stack
        .iter()
        .enumerate()
        .filter(|(_, sv)| sv.t == JT::Ref)
        .map(|(i, _)| i)
        .collect();

    let hit = b.create_block();
    let miss = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, I64);
    for _ in &ref_slots {
        b.append_block_param(merge, I64);
    }

    let ka = b.ins().iconst(I64, k_addr);
    let va = b.ins().iconst(I64, v_addr);
    let want_key = b.ins().iconst(I64, want);
    // Key THEN value, both required. The publisher writes value-then-key, so the only interleaving
    // a reader can catch mid-publish is "new key, value still zero" — which refuses.
    let key = b.ins().load(I64, MemFlags::trusted(), ka, Offset32::new(0));
    let mine = b.ins().icmp(IntCC::Equal, key, want_key);
    let cached = b.ins().load(I64, MemFlags::trusted(), va, Offset32::new(0));
    let live = b.ins().icmp_imm(IntCC::NotEqual, cached, 0);
    let ok = b.ins().band(mine, live);

    spill_for_gc(b, vars, stack, locals_ptr);
    b.ins().brif(ok, hit, &[], miss, &[]);

    b.switch_to_block(hit);
    b.seal_block(hit);
    bump_ctr(b, &crate::jit::lambda_shape::SITE_HITS);
    let mut hit_args: Vec<cranelift::codegen::ir::BlockArg> = vec![cached.into()];
    for &i in &ref_slots {
        hit_args.push(stack[i].v.into());
    }
    b.ins().jump(merge, &hit_args);

    b.switch_to_block(miss);
    b.seal_block(miss);
    let buf = b.ins().stack_addr(I64, env.args_slot, 0);
    let cls = b.ins().iconst(I64, env.class_ptr);
    let cpv = b.ins().iconst(I64, cp as i64);
    let nv = b.ins().iconst(I64, 0);
    let call = b.ins().call(env.indy, &[cls, cpv, buf, nv]);
    let res = b.inst_results(call)[0];
    // Publish before the reload: `indy_site_publish` allocates nothing and cannot collect, so the
    // frame's spilled roots are still exactly what the callback left them as.
    let conv = b.func.signature.call_conv;
    let mut cs = cranelift::codegen::ir::Signature::new(conv);
    for _ in 0..4 {
        cs.params.push(AbiParam::new(I64));
    }
    let sigref = b.import_signature(cs);
    let pfn = b
        .ins()
        .iconst(I64, crate::jit::lambda_shape::indy_site_publish as usize as i64);
    b.ins().call_indirect(sigref, pfn, &[ka, va, want_key, res]);
    reload_locals_after_gc(b, vars, locals_ptr);
    let mut miss_args: Vec<cranelift::codegen::ir::BlockArg> = vec![res.into()];
    for &i in &ref_slots {
        let rv = reload_ref_slot(b, vars.len(), i, stack[i].v, locals_ptr);
        miss_args.push(rv.into());
    }
    b.ins().jump(merge, &miss_args);

    b.switch_to_block(merge);
    b.seal_block(merge);
    let mparams: Vec<Value> = b.block_params(merge).to_vec();
    for (k, &i) in ref_slots.iter().enumerate() {
        stack[i].v = mparams[1 + k];
    }
    let sv = cell_to_sv(b, mparams[0], JT::Ref);
    stack.push(sv);
    Some(())
}

/// Is this `invokedynamic`'s BOOTSTRAP `LambdaMetafactory`?
///
/// ⭐**This is the instruction's own operand, not a heuristic over guest identifiers.** An
/// `invokedynamic` names its bootstrap method handle in `BootstrapMethods` (JVMS 4.7.23); which
/// bootstrap it names IS the opcode's meaning, and `opcodes::invokedynamic` switches on exactly
/// this to decide whether the site builds a lambda, a concat, a record method or a pattern switch.
/// It is read here for the same reason: only the `LambdaMetafactory` arm has the
/// `ConstantCallSite` property the result cache depends on. A zero-argument
/// `StringConcatFactory` site, by contrast, returns a FRESH moving arena string every time —
/// `indy_site_publish`'s `is_pinned` test would refuse it on every execution, so refusing it here
/// keeps the publish call off a site that can never qualify.
#[cfg(feature = "next-gen-gc")]
fn indy_site_is_metafactory(env: &Env, cp: u16) -> bool {
    if env.class_ptr == 0 {
        return false;
    }
    // SAFETY: `env.class_ptr` is the `Arc<LoadedClass>` pointer this method (or, under inlining,
    // this CALLEE — `emit_inline` overrides it precisely so cp indices and class agree) is being
    // compiled from; the class is alive for the whole compile.
    let class = unsafe { &*(env.class_ptr as *const crate::class_loader::LoadedClass) };
    let Some((bsm, _, _)) = class.file.pool.invoke_dynamic(cp) else {
        return false;
    };
    let Some(bm) = class.file.bootstrap_methods.get(bsm as usize) else {
        return false;
    };
    let Some(h) = class.file.pool.method_handle(bm.handle_index) else {
        return false;
    };
    h.owner == "java/lang/invoke/LambdaMetafactory"
}
