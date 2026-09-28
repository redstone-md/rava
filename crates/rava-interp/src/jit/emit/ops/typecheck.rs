//! **`instanceof` IN MACHINE CODE.**
//!
//! Every compiled `instanceof` left machine code. `rava_instanceof` says so in its own words —
//! "the codegen emits no inline type test — so this is the one and only 0xc1 fallback entry" —
//! and the fallback tally counts it as one of the largest single callback populations in a boot,
//! because registry building and codec decode run type tests in their inner loops.
//!
//! The ANSWER was already cheap: `opcodes::instance_of_at` memoises on
//! `(caller, cp, instanceof_class_key(receiver))`, so a warm site does a hash probe and returns.
//! What it could not remove is the CROSSING — the spill of every live root, the call, the VM
//! reconstruction, the unbox, the thread-local memo borrow, the reload. This file removes the
//! crossing for the receivers that carry a stable class identity, and leaves the callback in
//! place, unchanged, as the cold arm for everything else.
//!
//! ## What is emitted
//!
//! ```text
//!   recv == 0                    -> answer 0          (JVMS 6.5: instanceof null is false,
//!                                                       and it never throws, so no callback)
//!   recv >> 40 == 0 (arena obj)  -> key = (class_id << 1) | 1
//!   recv >> 40 == 1 (heap Ref)   -> key = Object.class word (Arc<str> inner ptr)
//!   anything else (array / box)  -> cold arm
//!   probe 4 × (key, answer) at the site's baked cache address:
//!       hit  -> answer inline, no call
//!       miss -> cold arm, which fills the entry for next time
//! ```
//!
//! ## What the guard proves
//!
//! The probe answers only for a key that the HOST installed, and the host installs a key only
//! when it has proved three things about the receiver it just answered for:
//!
//!   1. `opcodes::instanceof_class_key` returned `Some` for it — the engine's OWN predicate for
//!      "this receiver's class identity determines the answer at this site". It returns `None`
//!      for a lambda (arena `is_lambda_cid`, handle `Heap::is_lambda`), whose answer comes from
//!      its `$fi` functional interface and NOT from its class: every lambda shares one class
//!      word, so caching one lambda's answer under it would answer for all of them. Unkeyable
//!      receivers are never installed, so the emitted probe can never hit for one.
//!   2. the raw cell's TAG and the decoded `Value`'s variant agree — an arena cell that decodes
//!      to `Ref` (a PINNED MIRROR, see `unbox_ref_kind`) is refused, because emitted code would
//!      key it by its arena class id while the host answered for a handle.
//!   3. the answer came back `Ok` with no pending exception. A failed target resolution is never
//!      cached; it re-crosses and re-throws exactly as before.
//!
//! Given that, an entry is a pure function of (site, receiver class) — the same claim
//! `instance_of_at` already makes for `IOF_MEMO`, keyed at the same granularity, and no weaker:
//! the class hierarchy is immutable once loaded and the site's target resolves once.
//!
//! ## What it costs if removed
//!
//! Every `instanceof` in compiled code goes back to `spill_for_gc` + call + `vm_ref` + `unbox` +
//! thread-local memo probe + reload — a full host crossing for an answer that is two loads and a
//! compare.
//!
//! ## Checked casts use a separate compatibility rule
//!
//! `checkcast` (0xc0) is emitted in objects.rs through the interpreter's
//! `checkcast_reject`. That rule passes on `Unknown`, so a cached
//! `instanceof == false` is not a licence to throw a cast exception.

#![cfg(feature = "next-gen-gc")]

use crate::jit::*;
use crate::value::Value as JVal;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

/// Entries in one site's cache. Four (key, answer) pairs = exactly one 64-byte cache line, so
/// the whole probe is a single line touch. This tree has MEASURED that a bigger table loses (a
/// plain scan at 30.7 ns against 34.1 ns for a 196 KB memo), and a site with more than four
/// receiver classes is megamorphic anyway — it keeps the callback, which is what it did before.
const WAYS: usize = 4;

/// `answer` encoding. 0 is not "false", it is EMPTY: the reader accepts a slot only when the
/// answer word is non-zero, which is also what makes a half-published entry (key claimed,
/// answer not yet stored) read as a miss instead of as `false`.
const A_EMPTY: i64 = 0;
const A_FALSE: i64 = 1;
const A_TRUE: i64 = 2;

/// One site's inline answer cache. `w[2i]` is the key, `w[2i+1]` the answer.
///
/// ⚠**Entries are WRITE-ONCE.** A slot goes empty → claimed → answered and never changes again,
/// which is what makes the emitted two-load probe safe without any fence: the loads may be
/// reordered by Cranelift or by the machine, and the only accepted combination (key matches AND
/// answer non-zero) can only be produced by the thread that claimed that key. Replacement, by
/// contrast, would let a reader pair a NEW key with the PREVIOUS entry's answer.
#[repr(C, align(64))]
struct SiteCache {
    w: [AtomicI64; WAYS * 2],
}

impl SiteCache {
    const fn new() -> Self {
        #[allow(clippy::declare_interior_mutable_const)]
        const Z: AtomicI64 = AtomicI64::new(0);
        SiteCache { w: [Z; WAYS * 2] }
    }
}

/// Emit-time site registry. Keyed by (caller `LoadedClass` address, cp index) — the same pair
/// `instance_of_at` keys its memo on, so a site's inline cache and the host memo answer the same
/// question. Consulted ONLY on the compile thread, once per site per compile, so a `Mutex` costs
/// nothing on any executing path; the map holds the leaked address so a recompile of the same
/// method reuses the cache it already warmed instead of starting cold.
///
/// One 64-byte allocation per site that actually compiles an `instanceof`, and none at all if
/// none does — deliberately NOT a preallocated table (round eight allocated 29 MB of tables
/// before any verdict existed and paid for it).
static SITES: std::sync::Mutex<Option<std::collections::HashMap<(usize, u16), usize>>> =
    std::sync::Mutex::new(None);

/// KILL SWITCH `RAVA_NO_EMIT_TYPETEST=1` (default ON), consulted at EMIT time only: with it set
/// no cache is ever claimed, `emit_instanceof` declines, and the generated code for 0xc1 is
/// byte-identical to the callback-only form.
fn typetest_emit_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_EMIT_TYPETEST").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// `RAVA_TYPETEST_STATS=1`: count inline hits (an increment inside the taken arm) and host
/// misses, and print the pair from the miss helper every 64 K misses. Default off and inert —
/// with it unset the counters are not even emitted, so the hit arm is two loads and a compare.
fn typetest_stats_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_TYPETEST_STATS").is_some())
}

/// Inline-arm executions. Written by EMITTED code with a plain load/add/store, so counts can be
/// lost under threading — the question is orders of magnitude. It must be an atomic (not a plain
/// static) or the linker places it in read-only memory and the generated store faults.
static TT_HITS: AtomicU64 = AtomicU64::new(0);
static TT_MISSES: AtomicU64 = AtomicU64::new(0);
static TT_SITES: AtomicU64 = AtomicU64::new(0);
static TT_FILLED: AtomicU64 = AtomicU64::new(0);

/// EMIT TIME (compile thread): the stable address of this site's answer cache, allocating it on
/// first sight. `None` — site keeps the callback — when the switch is off or the caller class is
/// unknown (an inlined body whose `class_ptr` is zero cannot be keyed).
fn site_cache(class_ptr: usize, cp: u16) -> Option<i64> {
    if class_ptr == 0 || !typetest_emit_enabled() {
        return None;
    }
    let mut g = SITES.lock().unwrap_or_else(|e| e.into_inner());
    let map = g.get_or_insert_with(std::collections::HashMap::new);
    if let Some(&a) = map.get(&(class_ptr, cp)) {
        return Some(a as i64);
    }
    let addr = Box::leak(Box::new(SiteCache::new())) as *const SiteCache as usize;
    map.insert((class_ptr, cp), addr);
    TT_SITES.fetch_add(1, Ordering::Relaxed);
    Some(addr as i64)
}

/// The key emitted code computes for a receiver cell, recomputed on the host so the two sides
/// agree bit-for-bit. Returns `None` for every shape the emitted probe does not key, so a
/// disagreement can only ever mean "no entry installed", never "wrong entry installed".
///
/// * ARENA object (`cell >> 40 == 0`, non-null): the class id in the low half of the header word
///   at +0, shifted left one and tagged odd.
/// * HEAP handle (`cell >> 40 == 1`): the first word of `Object.class` (the `Arc<str>` inner
///   pointer), read exactly as `emit_heap_ref_class_key` reads it — 8-aligned, hence EVEN, so
///   the two key spaces are disjoint by parity and no arena class id can ever alias a class word.
/// * everything else — array, box, a mirror whose arena cell decodes to a handle, a swept slot:
///   `None`.
fn emitted_key(vm: &crate::Vm, cell: i64, v: &JVal) -> Option<i64> {
    let hi = (cell as u64) >> 40;
    match v {
        JVal::Obj(p) if hi == 0 && cell != 0 && p.addr() == cell as u64 => {
            let cid = crate::nextgen::arena_class_id(*p);
            Some(((cid as i64) << 1) | 1)
        }
        JVal::Ref(h) if hi == 1 => {
            let w = vm.heap.object_class_word(*h)?;
            // An odd class word would collide with the arena key space. It cannot happen (an
            // `ArcInner` is 8-aligned) — refusing is free and makes the disjointness a checked
            // fact rather than an argument.
            (w != 0 && w & 1 == 0).then_some(w)
        }
        _ => None,
    }
}

/// Publish `(key, answer)` into the first empty way. Never overwrites: a full cache means this
/// site sees more than `WAYS` receiver classes and keeps crossing, exactly as it did before this
/// file existed.
fn install(cache: i64, key: i64, ans: i64) {
    // SAFETY: `cache` came from `site_cache`, which only ever hands out the address of a leaked,
    // process-lifetime `SiteCache`.
    let c = unsafe { &*(cache as *const SiteCache) };
    let word = if ans != 0 { A_TRUE } else { A_FALSE };
    for i in 0..WAYS {
        let k = c.w[2 * i].load(Ordering::Acquire);
        if k == key {
            return; // already ours, or another thread is publishing the same pair
        }
        if k == 0 {
            // CLAIM FIRST, ANSWER SECOND. If two threads store answers into the same way and
            // then race to claim it, the surviving key can end up paired with the OTHER thread's
            // answer. Claiming with a CAS makes the way exclusively the winner's before any
            // answer is written, and the reader's "answer must be non-zero" test turns the gap
            // between the two writes into a miss.
            if c.w[2 * i]
                .compare_exchange(0, key, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                c.w[2 * i + 1].store(word, Ordering::Release);
                TT_FILLED.fetch_add(1, Ordering::Relaxed);
            }
            return;
        }
    }
}

/// **THE COLD ARM.** Reached only when the inline probe missed: an unkeyed receiver shape, a
/// receiver class this site has not answered for yet, or a site whose four ways are taken.
///
/// Does exactly what `rava_instanceof` does — same VM accessor, same sanctioned-worker
/// reasoning, same pending-exception protocol, same fallback tally, same
/// `Vm::jit_instanceof` — and then, when the receiver was provably keyable, installs the answer
/// so this (site, class) pair never crosses again.
///
/// ORDER MATTERS: the key is computed from `val` BEFORE `jit_instanceof` runs, because that call
/// can load a class, allocate and collect, after which `val` may name a moved object. The key is
/// a plain integer identity, so it survives the move; the pointer would not.
pub(crate) extern "C" fn rava_instanceof_cached(
    cache: i64,
    caller_ptr: i64,
    cp: i64,
    val: i64,
) -> i64 {
    cb(4);
    // GAP-S09 stage 4 SANCTIONED (read-only / abort-guarded), identical to `rava_instanceof`:
    // the hierarchy + interface walk reads frozen loader state; a COLD class load on a gil-free
    // worker aborts inside the S07 loader guard before any publication.
    let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };
    if vm.jit_pending_is_set() {
        return 0;
    }
    crate::fallback_tally::note(
        crate::fallback_tally::Kind::Instanceof,
        caller_ptr as usize,
        cp as u16,
    );
    if typetest_stats_enabled() {
        let n = TT_MISSES.fetch_add(1, Ordering::Relaxed) + 1;
        if n % 65_536 == 0 {
            eprintln!(
                "[TYPETEST] inline_hits={} host_misses={} sites={} entries={}",
                TT_HITS.load(Ordering::Relaxed),
                n,
                TT_SITES.load(Ordering::Relaxed),
                TT_FILLED.load(Ordering::Relaxed),
            );
        }
    }

    // The receiver's identity, decided before anything can move it. `instanceof_class_key` is
    // the engine's own "is this answer a function of the class" predicate (it refuses lambdas);
    // `emitted_key` is what the machine code will compute for the same cell. Both must agree
    // that this receiver is keyable, or nothing is installed.
    let value = crate::jit::unbox_ref(vm, val, "");
    let key = if crate::opcodes::instanceof_class_key(&vm.heap, &value).is_some() {
        emitted_key(vm, val, &value)
    } else {
        None
    };

    let caller = caller_ptr as *const crate::class_loader::LoadedClass;
    match vm.jit_instanceof(caller, cp as u16, val) {
        Ok(r) => {
            // A pending exception means the walk did not complete (a target that would not
            // resolve); its zero is not an answer and must not be remembered.
            if let Some(k) = key {
                if !vm.jit_pending_is_set() {
                    install(cache, k, r);
                }
            }
            r
        }
        Err(crate::VmError::Throw(e)) => {
            if !vm.jit_pending_is_set() {
                vm.jit_pending_set(e);
            }
            0
        }
        Err(_) => 0,
    }
}

/// Emit the inline type test for `instanceof` at cp index `cp`. `Some(())` — the answer is on
/// the operand stack and the caller must emit nothing; `None` — nothing was emitted and the
/// operand stack is exactly as it was found, so the caller emits its ordinary callback arm.
///
/// Every fallible check happens before the first instruction, so `None` never leaves a partly
/// built diamond behind.
pub(crate) fn emit_instanceof(
    b: &mut FunctionBuilder,
    env: &Env,
    stack: &mut Vec<SV>,
    vars: &[Variable],
    locals_ptr: Value,
    array_desc_cell: Value,
    cp: u16,
) -> Option<()> {
    if stack.last().is_none() {
        return None;
    }
    let cache_addr = site_cache(env.class_ptr as usize, cp)?;
    let recv = stack.pop()?.v;

    // Every live reference on the operand stack survives the cold arm, which can collect and
    // MOVE objects; the inline arm moves nothing. Both arms therefore hand the same references
    // to the merge block as parameters, the inline arm passing its unchanged values — the
    // discipline `emit_dispatch_pic` and `emit_inline_lambda` use. Getting this wrong does not
    // fail a gate: it strands a stale pointer until the next collection touches it.
    let ref_slots: Vec<usize> =
        stack.iter().enumerate().filter(|(_, sv)| sv.t == JT::Ref).map(|(i, _)| i).collect();

    let nz = b.create_block();
    let arena_k = b.create_block();
    let ref_chk = b.create_block();
    let ref_k = b.create_block();
    let probe = b.create_block();
    let miss = b.create_block();
    let merge = b.create_block();
    b.append_block_param(probe, I64); // the receiver's class key
    b.append_block_param(merge, I64); // 0 / 1
    for _ in &ref_slots {
        b.append_block_param(merge, I64);
    }

    // Materialised HERE, in the block that dominates every other one: the probe reads it and so
    // does the cold arm (which is reachable from `ref_chk` and `ref_k`, neither of which the
    // probe dominates — defining it inside the probe is an SSA violation the verifier catches).
    let cache = b.ins().iconst(I64, cache_addr);

    // ── null: JVMS 6.5 — `instanceof` on null is false and throws nothing, so it does not even
    // need the target class resolved. This is the one arm that answers without a key.
    let zero = b.ins().iconst(I64, 0);
    let is_null = b.ins().icmp_imm(IntCC::Equal, recv, 0);
    let mut null_args: Vec<cranelift::codegen::ir::BlockArg> = vec![zero.into()];
    for &i in &ref_slots {
        null_args.push(stack[i].v.into());
    }
    b.ins().brif(is_null, merge, &null_args, nz, &[]);

    // ── which world is the receiver in? `cell >> 40`: 0 = raw arena pointer (the arena is
    // low-address, every ArenaPtr < 2^40), 1 = REF_TAG handle, 2 = BOX_TAG, 4 = ARRAY_TAG.
    // Arrays and boxes are not keyed here (an array's answer depends on its ELEMENT type, which
    // is not in the cell) and take the cold arm.
    b.switch_to_block(nz);
    b.seal_block(nz);
    let hi = b.ins().ushr_imm(recv, 40);
    let is_arena = b.ins().icmp_imm(IntCC::Equal, hi, 0);
    b.ins().brif(is_arena, arena_k, &[], ref_chk, &[]);

    // ARENA: the class id is the low half of the header word at +0 — the same load the
    // devirtualisation guard and every inline arena field op make. Shift-and-set-bit-0 puts it
    // in the odd half of the key space, disjoint from the even class words below.
    b.switch_to_block(arena_k);
    b.seal_block(arena_k);
    let cid = b.ins().load(I32, MemFlags::trusted(), recv, Offset32::new(0));
    let cid64 = b.ins().uextend(I64, cid);
    let ak = b.ins().ishl_imm(cid64, 1);
    let ak = b.ins().bor_imm(ak, 1);
    b.ins().jump(probe, &[ak.into()]);

    b.switch_to_block(ref_chk);
    b.seal_block(ref_chk);
    let is_ref = b.ins().icmp_imm(IntCC::Equal, hi, 1);
    b.ins().brif(is_ref, ref_k, &[], miss, &[]);

    // HEAP HANDLE: the interned per-class word, read exactly as the PIC reads it. A swept
    // nullable slot has no `Object` at all; `emit_heap_ref_class_key` routes that to `miss`
    // rather than dereferencing address zero.
    b.switch_to_block(ref_k);
    b.seal_block(ref_k);
    let cw = emit_heap_ref_class_key(b, array_desc_cell, recv, miss);
    b.ins().jump(probe, &[cw.into()]);

    // ── the probe. Four (key, answer) pairs in one cache line, a select chain rather than a
    // branch chain so there is nothing to mispredict. `ans != 0` rejects both an empty way and
    // a way whose key is claimed but whose answer is not yet published.
    b.switch_to_block(probe);
    b.seal_block(probe);
    let key = b.block_params(probe)[0];
    let mut acc = b.ins().iconst(I64, A_EMPTY);
    for i in 0..WAYS as i32 {
        let ke = b.ins().load(I64, MemFlags::trusted(), cache, Offset32::new(i * 16));
        let ae = b.ins().load(I64, MemFlags::trusted(), cache, Offset32::new(i * 16 + 8));
        let keq = b.ins().icmp(IntCC::Equal, ke, key);
        let live_e = b.ins().icmp_imm(IntCC::NotEqual, ae, A_EMPTY);
        let hit = b.ins().band(keq, live_e);
        acc = b.ins().select(hit, ae, acc);
    }
    // A_FALSE/A_TRUE are 1/2, so the answer the opcode pushes is one subtraction away. The value
    // is computed before the branch and is simply unused on the miss edge.
    let ans_hit = b.ins().iadd_imm(acc, -1);
    let got = b.ins().icmp_imm(IntCC::NotEqual, acc, A_EMPTY);
    if typetest_stats_enabled() {
        let addr = b.ins().iconst(I64, (&TT_HITS as *const AtomicU64) as i64);
        let cur = b.ins().load(I64, MemFlags::trusted(), addr, Offset32::new(0));
        let inc = b.ins().iadd_imm(cur, 1);
        // Off the hit edge as well as on it — a diagnostic that needed its own block would
        // change the shape it is measuring. Counted only when the flag is on.
        let keep = b.ins().select(got, inc, cur);
        b.ins().store(MemFlags::trusted(), keep, addr, Offset32::new(0));
    }
    let mut hit_args: Vec<cranelift::codegen::ir::BlockArg> = vec![ans_hit.into()];
    for &i in &ref_slots {
        hit_args.push(stack[i].v.into());
    }
    b.ins().brif(got, merge, &hit_args, miss, &[]);

    // ── the cold arm: the callback this file exists to avoid, emitted with exactly the
    // spill/call/reload discipline the 0xc1 arm has always used. Locals come back through
    // `def_var` (SSA-safe on a diamond arm); the ref-typed operand stack comes back through the
    // merge parameters, because reloading those in place would break SSA across the join.
    b.switch_to_block(miss);
    b.seal_block(miss);
    spill_for_gc(b, vars, stack, locals_ptr);
    let conv = b.func.signature.call_conv;
    let mut cs = cranelift::codegen::ir::Signature::new(conv);
    for _ in 0..4 {
        cs.params.push(AbiParam::new(I64));
    }
    cs.returns.push(AbiParam::new(I64));
    let sigref = b.import_signature(cs);
    let fnp = b.ins().iconst(I64, rava_instanceof_cached as usize as i64);
    let clsv = b.ins().iconst(I64, env.class_ptr);
    let cpv = b.ins().iconst(I64, cp as i64);
    let call = b.ins().call_indirect(sigref, fnp, &[cache, clsv, cpv, recv]);
    let res = b.inst_results(call)[0];
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
    let v = b.ins().ireduce(I32, mparams[0]);
    push(stack, v, JT::Int);
    Some(())
}

// The cast callback uses checkcast_reject's own memo and tri-state rule.
// Do not use this instanceof cache to decide whether a checked cast must throw.
