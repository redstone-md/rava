//! Field access bridges: the callbacks compiled code takes when a getfield/putfield/getstatic/putstatic cannot be served inline.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;
use crate::value::Value as JVal;

/// Bridge an arena reference array at a legacy field boundary.
///
/// Arena receivers stay on the arena-cell path below.  This helper is only called immediately
/// before a handle/static store, and admits an arena value through the exact CID-backed
/// `ArrayRef` predicate; primitive arena arrays and ordinary objects remain unchanged.
#[cfg(feature = "next-gen-gc")]
#[inline]
fn materialize_ref_array_for_legacy_store(vm: &mut crate::Vm, value: &mut JVal) {
    if !matches!(value, JVal::Obj(_)) {
        return;
    }
    let Some(array) = crate::heap::ArrayRef::from_value(value) else {
        return;
    };
    if let Some(handle) = array.materialize(&mut vm.heap) {
        *value = JVal::Array(handle);
    }
}

/// ARENA SLOT CACHE — the inline replacement for the `arena_getfield` → `slot_of`
/// round trip.
///
/// A compiled `getfield` whose layout slot the compiler could NOT resolve (the class
/// was not loaded, or only partially laid out, at emit time) leaves machine code on
/// EVERY execution: `rava_getfield` → `unbox_ref` → `arena_getfield` → `slot_of` →
/// a thread-local RefCell memo — five host frames to learn an index that never
/// changes. Those rows are ~5.4% of all callbacks.
///
/// The compiler cannot wait for the answer: the value is published by the very calls
/// the fast arm would serve, so "not ready ⇒ do not emit" never converges. So bake the
/// ADDRESS of this table into the code, read the entry at runtime, and REFUSE (fall to
/// the callback, which publishes) while the entry is still zero.
///
/// ONE 64-BIT WORD PER ENTRY, and that is a correctness requirement, not a size
/// preference: a two-word entry (key, slot) can be torn by a concurrent publisher
/// between the reader's two loads, handing compiled code a slot that belongs to a
/// different field. Packing key AND slot into a single atomically-loaded word makes
/// that race unrepresentable:
///
/// ```text
///   [63:40] class id (24b)   [39:16] field id (24b)   [15:0] slot + 1 (0 = empty)
/// ```
///
/// Both ids and the slot are bounds-checked before publication; anything wider is
/// simply never cached (the site keeps taking the callback, which is correct, just
/// slow). Direct-mapped, no locks, no eviction policy — a collision overwrites and the
/// loser goes back to the callback that will re-publish it.
///
/// SOUNDNESS OF THE CACHED FACT: a class layout is APPEND-ONLY, so a slot index, once
/// observed, cannot move (the same invariant `slot_memo` documents for the host-side
/// memo it replaces). Only positives are ever stored.
#[cfg(feature = "next-gen-gc")]
pub(crate) const ARENA_SLOT_N: usize = 4096;
#[cfg(feature = "next-gen-gc")]
pub(crate) const ARENA_SLOT_MASK: u64 = ARENA_SLOT_N as u64 - 1;

/// 32 KB, direct-mapped. Sized DOWN deliberately: a lookup table that stops fitting
/// beside the working set costs more than the host call it removes (the interner-L1
/// regression — 14 KB of table measured +6.8%, disjoint).
#[cfg(feature = "next-gen-gc")]
pub(crate) static ARENA_SLOT_CACHE: [std::sync::atomic::AtomicU64; ARENA_SLOT_N] =
    [const { std::sync::atomic::AtomicU64::new(0) }; ARENA_SLOT_N];

/// The field id half of the bucket index. `field_id` is a compile-time constant at
/// every emitted site, so this whole multiply folds into an immediate and the runtime
/// index costs one `xor` and one `and`.
#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn arena_slot_mix(field_id: u32) -> u64 {
    (field_id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn arena_slot_index(class_id: u32, field_id: u32) -> usize {
    (((class_id as u64) ^ arena_slot_mix(field_id)) & ARENA_SLOT_MASK) as usize
}

/// Pack, or refuse. `slot + 1` so that a zero word is unambiguously "empty" — the
/// bootstrap-trap sentinel the emitted guard tests.
#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn arena_slot_word(class_id: u32, field_id: u32, slot: u32) -> Option<u64> {
    if class_id >= 1 << 24 || field_id >= 1 << 24 || slot >= (1 << 16) - 1 {
        return None;
    }
    Some(((class_id as u64) << 40) | ((field_id as u64) << 16) | (slot as u64 + 1))
}

/// Base address baked into compiled code. `static`, so it is fixed for the process.
#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn arena_slot_base() -> i64 {
    ARENA_SLOT_CACHE.as_ptr() as usize as i64
}

/// Publish what this callback just had to resolve the slow way. Relaxed: the word is
/// self-validating (reader compares the packed key), so no ordering is owed.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn arena_slot_publish(class_id: u32, field_id: u32, slot: u32) {
    if let Some(w) = arena_slot_word(class_id, field_id, slot) {
        ARENA_SLOT_CACHE[arena_slot_index(class_id, field_id)]
            .store(w, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Called from the arena arms of `rava_getfield` / `rava_putfield` — the exact sites
/// whose repeat visits the inline probe is meant to delete. `slot_of` is memoised and
/// the receiver's class was already read by the caller, so this costs a hash probe on a
/// path that has already paid for a host transition.
///
/// Refuses under the pinned-mirror flags: there a raw address may be a HANDLE object's
/// mirror, whose authoritative cells live in the FieldBlob, so an inline
/// `[obj + HDR + slot*8]` would read or write the wrong storage.
#[cfg(feature = "next-gen-gc")]
fn arena_slot_note(p: crate::arena::ArenaPtr, field_id: u32) {
    if !crate::jit::inline_fields_enabled()
        || crate::nextgen::pinned_ptrs_enabled()
        || crate::nextgen::pinned_read_enabled()
        || crate::nextgen::shadow_enabled()
    {
        return;
    }
    let cid = crate::nextgen::arena_class_id(p);
    // Only a DIRECT hit is published. A `None` here means the receiver is stale and the
    // reader below recovers it by forwarding — caching the post-forward class's slot
    // against the pre-forward class id would be a lie about a different object.
    if let Some(s) = crate::nextgen::slot_of(cid, field_id) {
        if let Ok(s) = u32::try_from(s) {
            arena_slot_publish(cid, field_id, s);
        }
    }
}

/// FIELD-HINT PUBLICATION — the second half of the same bargain, for HANDLE receivers.
///
/// The JIT's `fid`-bucketed slot hint (read by every emitted scan at `ctx+24`) is
/// written ONLY by compiled sites that completed a linear id scan. Every crossing
/// through `rava_getfield` / `rava_putfield` also resolves a slot — and then throws it
/// away, so the site that sent us here re-scans on its next execution and the site next
/// door never learns anything at all. Hand it back.
///
/// Sound by construction: the emitted guard is `hint < count && ids[hint] == fid` before
/// the cell is touched, so a stale, colliding or plain wrong entry costs exactly one
/// re-scan and can never read another field's slot. Written through the same table and
/// with the same unsynchronised shape compiled guest code already uses for it; relaxed
/// atomic rather than a bare store so concurrent workers are not a data race.
#[inline]
fn publish_field_hint(vm: &crate::Vm, field_id: i64, slot: u32) {
    let base = vm.heap.jit_ctx.field_hint_base;
    if base == 0 {
        return;
    }
    let bucket = field_id as usize & (crate::heap::JIT_FIELD_HINT_N - 1);
    // SAFETY: `ensure_field_hints` allocates exactly JIT_FIELD_HINT_N u32s and never
    // resizes or moves the table (that is why the base is cached in JitCtx at all), and
    // `bucket` is masked into range. AtomicU32 has the same layout as u32.
    unsafe {
        (*(base as *const std::sync::atomic::AtomicU32).add(bucket))
            .store(slot, std::sync::atomic::Ordering::Relaxed);
    }
}

/// getstatic via the REAL interpreter handler (System.out/err sentinels, the
/// super/interface-constant walk, native fallback — all intact, no reimpl). May
/// run a class `<clinit>` (ensure_init). `is_ref` (1/0) packs the result with
/// `box_ref` (reference field) or `pack_local` (primitive field).
pub(crate) extern "C" fn rava_getstatic(caller_ptr: i64, cp: i64, is_ref: i64) -> i64 {
    cb(1);
    // FALLBACK TALLY: a getstatic with NO value cell (reference-typed statics
    // today) — every execution pays the full interpreter handler.
    crate::fallback_tally::note(
        crate::fallback_tally::Kind::GetstaticUncached,
        caller_ptr as usize,
        cp as u16,
    );
    // GAP-S09 stage 4 SANCTIONED (warm read / abort-guarded cold): a WARM
    // getstatic is a `statics` RefCell READ of frozen-in-region state; a COLD
    // one needs a class load / `<clinit>`, both of which abort on a gil-free
    // worker before publication (the S07 loader guard, class_loader.rs, and the
    // ClinitPending worker exit) — the plan's "already abort-before-publication"
    // group. The cell-caching twin (rava_getstatic_cached) carries its own gate.
    let Some(vm) = vm_ref_worker_sanctioned() else {
        return 0;
    };
    if vm.jit_pending_is_set() {
        return 0;
    }
    let caller_ptr = caller_ptr as *const crate::class_loader::LoadedClass;
    match vm.jit_getstatic(caller_ptr, cp as u16, is_ref != 0) {
        Ok(packed) => packed,
        Err(crate::VmError::Throw(e)) => {
            if !vm.jit_pending_is_set() {
                vm.jit_pending_set(e);
            }
            vm.heap.set_jit_fault();
            0
        }
        Err(_) => {
            if !vm.jit_pending_is_set() {
                eprintln!("[JIT-ERR] jit getstatic");
                let exc = vm.make_exception("java/lang/InternalError", "jit getstatic");
                vm.jit_pending_set(exc);
            }
            vm.heap.set_jit_fault();
            0
        }
    }
}

/// getstatic that ALSO populates value cell `slot`: same interpreter handler as
/// `rava_getstatic` (so it can run `<clinit>` and never diverges), then writes the
/// cell `(current static_gen, packed)` so the compiled fast path hits next time.
/// Only emitted for PRIMITIVE statics (is_ref is always 0 here). A pending fault
/// leaves the cell untouched (its sentinel/old gen just forces another callback).
pub(crate) extern "C" fn rava_getstatic_cached(
    caller_ptr: i64,
    cp: i64,
    is_ref: i64,
    slot: i64,
) -> i64 {
    cb(1);
    super::GETSTATIC_REFILLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // FALLBACK TALLY: a value-cell MISS (cold cell or a static_gen bump).
    crate::fallback_tally::note(
        crate::fallback_tally::Kind::GetstaticMiss,
        caller_ptr as usize,
        cp as u16,
    );
    // GAP-S09 stage 4 SANCTIONED (abort-guarded): fires only on a static-cell
    // MISS, and the worker triage directly below either runs the miss under the
    // locked-exec envelope (exclusive `&mut Vm`) or performs the 17.1 armored
    // abort BEFORE any `<clinit>`/cell write — the `&mut` never escapes it.
    let Some(vm) = vm_ref_worker_sanctioned() else {
        return 0;
    };
    // 17.1 getstatic callback-isolation. This callback fires ONLY on a static-cell MISS, whose work
    // — jit_getstatic (may run <clinit>) + write_static_cell — mutates the SHARED &mut Vm. On a pool
    // worker that is an unsynchronised race, and a miss's <clinit> is inherently shared state that
    // CANNOT be replicated thread-local. So arm the aborter and unwind: the region falls back to the
    // serial 10.2b split (bit-exact). Compiled HITS never reach here, so once the statics are
    // pre-warmed the shared cell table is READ-ONLY during the fork → concurrent worker reads are
    // race-free (no thread-local copy needed). RAVA-ABORT-DIAG names getstatic as the abort site.
    //
    // Gated on gil-free, not on `gil::is_worker()`: a GIL-BOUND executor guest task holds
    // the GIL and races nothing, but the thread-lifetime worker latch sent it down the
    // armored-abort arm anyway — requesting an abort no region would honour and returning
    // `0` as the static's value. That is a silently WRONG static in ordinary compiled
    // guest code, the getstatic twin of the handle-`0` allocation bug.
    #[cfg(feature = "next-gen-gc")]
    if crate::parallel::gilfree() {
        // P3-S1: under RAVA_M3_WORKER_LOCKED_EXEC run the miss UNDER the re-entrant resolve
        // lock instead of aborting — the abort's discarded-0 return fed a garbage 0 into
        // result-dependent compiled loops, which then spun forever (the measured DETERMINISTIC
        // stuck-worker at the same index every run — the getstatic twin of primitive #7).
        // Main is parked + siblings serialize on this same lock ⇒ exclusive &mut Vm, the same
        // envelope the locked dispatch-miss handler uses; a <clinit> here runs exclusively.
        if crate::worker_locked_exec_enabled() {
            let _g = resolve_lock_acquire();
            if vm.jit_pending_is_set() {
                return 0;
            }
            let caller = caller_ptr as *const crate::class_loader::LoadedClass;
            return match vm.jit_getstatic(caller, cp as u16, is_ref != 0) {
                Ok(packed) => {
                    // A BOX_TAG pack (Str / sentinel / boxed primitive) unwinds with
                    // the compiled frame — never cacheable; leave the cell sentinel
                    // so this site keeps taking the callback (decided per VALUE).
                    if is_ref == 0 || !is_box_cell(packed) {
                        let gen = vm.runtime_shared().static_gen();
                        vm.heap.write_static_cell(slot as u32, gen, packed);
                    }
                    packed
                }
                Err(crate::VmError::Throw(e)) => {
                    if !vm.jit_pending_is_set() {
                        vm.jit_pending_set(e);
                    }
                    vm.heap.set_jit_fault();
                    0
                }
                Err(_) => 0,
            };
        }
        crate::parallel::request_abort();
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        if N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 4 {
            let site = unsafe { (caller_ptr as *const crate::class_loader::LoadedClass).as_ref() }
                .map(|c| c.file.this_class.to_string())
                .unwrap_or_default();
            eprintln!("[RAVA-ABORT-DIAG] getstatic MISS on worker (cp={cp} in {site}) — shared <clinit>/cell-write would race; aborting to serial.");
        }
        let _ = vm;
        return 0; // armored abort (mirrors 11.8): region tears down at the next abort check → serial.
    }
    if vm.jit_pending_is_set() {
        return 0;
    }
    let caller = caller_ptr as *const crate::class_loader::LoadedClass;
    match vm.jit_getstatic(caller, cp as u16, is_ref != 0) {
        Ok(packed) => {
            // A BOX_TAG pack (Str / sentinel / boxed primitive) unwinds with the
            // compiled frame — never cacheable; leave the cell sentinel so this
            // site keeps taking the callback (decided per VALUE, like key-intern).
            if is_ref == 0 || !is_box_cell(packed) {
                // gen AFTER the handler (a clinit may have bumped it); the mirror that
                // the fast path compares against is refreshed in lockstep on every bump.
                let gen = vm.runtime_shared().static_gen();
                vm.heap.write_static_cell(slot as u32, gen, packed);
            }
            packed
        }
        Err(crate::VmError::Throw(e)) => {
            if !vm.jit_pending_is_set() {
                vm.jit_pending_set(e);
            }
            vm.heap.set_jit_fault();
            0
        }
        Err(_) => {
            if !vm.jit_pending_is_set() {
                eprintln!("[JIT-ERR] jit getstatic_cached");
                let exc = vm.make_exception("java/lang/InternalError", "jit getstatic");
                vm.jit_pending_set(exc);
            }
            vm.heap.set_jit_fault();
            0
        }
    }
}

/// putstatic via the REAL interpreter handler (runs `<clinit>`, inherited-static
/// walk). `kind` decodes the value like putfield. Mutating → no-op after a
/// pending fault. On Throw (e.g. a clinit error) sets jit_pending + latches the
/// native-store fault flag.
pub(crate) extern "C" fn rava_putstatic(caller_ptr: i64, cp: i64, val: i64, kind: i64) -> i64 {
    cb(2);
    // GAP-S09 stage 4 SANCTIONED (abort-guarded): the stage-3 worker_illegal_gate
    // directly below aborts (or locks) BEFORE any decode/publication; the code
    // between here and the gate only counts the S2 CNT_PUTSTATIC telemetry.
    let Some(vm) = vm_ref_worker_sanctioned() else {
        return 0;
    };
    // GAP-S09 stage 3 (LIVE HOLE 1 closure): a putstatic on a gil-free worker is an
    // UN-isolated shared write (statics aren't COW-shadowed) — the stage-2 probe
    // only COUNTED it. Keep that S2 telemetry (the [CHUNK-FILL-PAR] "cow2" line
    // reads CNT_PUTSTATIC), then abort BEFORE the write — or, under
    // RAVA_M3_WORKER_LOCKED_EXEC, run the real handler with the resolve lock held
    // (exclusive &mut Vm), exactly like rava_getstatic_cached.
    #[cfg(feature = "next-gen-gc")]
    let _locked_exec = {
        if crate::parallel::gilfree() {
            let n =
                crate::parallel::CNT_PUTSTATIC.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if n < 8 && std::env::var_os("RAVA_FORK_TRACE").is_some() {
                let site =
                    unsafe { (caller_ptr as *const crate::class_loader::LoadedClass).as_ref() }
                        .map(|c| c.file.this_class.to_string())
                        .unwrap_or_default();
                eprintln!("[PUTSTATIC#{n}] in {site} cp={cp} kind={kind}");
            }
        }
        match worker_illegal_gate(2, "putstatic") {
            WorkerGate::Serial => None,
            WorkerGate::Locked(g) => Some(g),
            WorkerGate::Abort => return 0,
        }
    };
    if vm.jit_pending_is_set() {
        return 0;
    }
    let mut value = match kind {
        0 => JVal::Int(val as i32),
        1 => JVal::Long(val),
        2 => JVal::Float(f32::from_bits(val as u32)),
        3 => JVal::Double(f64::from_bits(val as u64)),
        _ => unbox_ref(vm, val, ""),
    };
    #[cfg(feature = "next-gen-gc")]
    materialize_ref_array_for_legacy_store(vm, &mut value);
    let caller_ptr = caller_ptr as *const crate::class_loader::LoadedClass;
    match vm.jit_putstatic(caller_ptr, cp as u16, value) {
        Ok(()) => 0,
        Err(crate::VmError::Throw(e)) => {
            if !vm.jit_pending_is_set() {
                vm.jit_pending_set(e);
            }
            vm.heap.set_jit_fault();
            0
        }
        Err(_) => {
            if !vm.jit_pending_is_set() {
                eprintln!("[JIT-ERR] jit putstatic");
                let exc = vm.make_exception("java/lang/InternalError", "jit putstatic");
                vm.jit_pending_set(exc);
            }
            vm.heap.set_jit_fault();
            0
        }
    }
}

/// getfield read by interned field id. `is_ref` (1/0) = field's static kind:
/// a reference field is `box_ref`'d, a primitive field flat via `pack_local`.
pub(crate) extern "C" fn rava_getfield(obj: i64, field_id: i64, is_ref: i64) -> i64 {
    cb(6);
    // GAP-S09 stage 4 SANCTIONED (read-only + COW): the gil-free read order is
    // worker TLS shadow first (`shadow_get_field`), then the arena/heap cell
    // (`arena_getfield` on the worker-visible arena, `get_field_hint` `&self`)
    // — no publication; boxing goes to the TLS box table. This is the S2/kind-A
    // acceptance read path (shadow-first getfield).
    let Some(vm) = vm_ref_worker_sanctioned() else {
        return 0;
    };
    if vm.jit_pending_is_set() {
        return 0;
    }
    #[cfg(feature = "next-gen-gc")]
    let cow_active = crate::parallel::cow_active();
    #[cfg(feature = "next-gen-gc")]
    let _cow_timer = CowCallbackTimer::new(cow_active, &COW_GET_CALLBACKS, &COW_GET_CALLBACK_NS);
    // M3 COW: a gil-free worker reads its own shadowed write first (read-your-own-writes);
    // a miss falls through to the shared read below (shared is read-only in phase B).
    #[cfg(feature = "next-gen-gc")]
    if cow_active {
        let recv = unbox_ref(vm, obj, "");
        // Worker-owned receiver → direct read (never shadowed under bypass OR undo).
        let owned_skip = (crate::parallel::cow_owned_bypass() || crate::parallel::cow_owned_undo())
            && crate::parallel::cow_owned(&recv);
        if !owned_skip {
            if let Some(key) = crate::parallel::cow_obj_key(&recv) {
                if let Some(v) = crate::parallel::shadow_get_field(key, field_id as u32) {
                    return if is_ref != 0 {
                        box_ref(vm, &v)
                    } else {
                        pack_local(&v)
                    };
                }
            }
        }
    }
    let Some(h) = untag_ref(obj) else {
        // Arena receiver (next-gen-gc): boxed (box_ref) into a BOX_TAG cell since a
        // raw 64-bit ArenaPtr can't carry a high-word tag. Unbox + read the inline
        // cell directly. untag_ref stays the fast path for plain handles.
        // B6a COEXISTENCE BRIDGE (getfield callback): under RAVA_PINNED_PTRS a
        // mirrored handle receiver arrives as its raw pinned ADDRESS (0b00, no
        // REF_TAG). untag_ref (above) misses it, and unbox_ref would canonicalize
        // the pinned address back to the Rust-boundary handle form Ref(h) — which
        // the Obj(p) arena arm below does NOT match, so it fell through to a
        // spurious `getfield` NPE (the full-boot hang: megamorphic registry/tag
        // getfields — Holder/ResourceLocation/TagFile — compile to THIS callback,
        // whereas ChunkFill's inline-admitted hot classes read arena cells directly
        // off the pointer and never reach here). Serve a pinned receiver via the
        // handle funnel, exactly as rava_putfield's B5 arm does. Inert without
        // pinned ptrs: a handle keeps its REF_TAG and is caught by untag_ref above,
        // so this never fires and boot stays byte-identical (default OFF).
        #[cfg(feature = "next-gen-gc")]
        if obj != 0 && (obj as u64) >> 40 == 0 {
            if let Some(h) = crate::nextgen::pinned_handle_of(obj as u64) {
                let (v, _slot) = vm.heap.get_field_hint(h, field_id as u32, 0);
                return if is_ref != 0 {
                    box_ref(vm, &v)
                } else {
                    pack_local(&v)
                };
            }
        }
        #[cfg(feature = "next-gen-gc")]
        if let JVal::Obj(p) = unbox_ref(vm, obj, "") {
            // RAVA_READ_PROBE=<field_id>: log the first reads of the COW-divergence field
            // (id=4765, Spline$Point.next) with the raw arena value — the RYOW hit/miss diff.
            if crate::parallel::gilfree() {
                if let Ok(fid) = std::env::var("RAVA_READ_PROBE") {
                    if fid.trim().parse::<u32>().ok() == Some(field_id as u32) {
                        static N: std::sync::atomic::AtomicU64 =
                            std::sync::atomic::AtomicU64::new(0);
                        if N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 12 {
                            let v = crate::opcodes::arena_getfield(p, field_id as u32);
                            eprintln!(
                                "[READ#{}] recv={:#x} id={field_id} raw={v:?}",
                                N.load(std::sync::atomic::Ordering::Relaxed) - 1,
                                p.addr()
                            );
                        }
                    }
                }
            }
            // STALEFIELD forensic (compiled getfield path — a JIT'd record accessor
            // like ResourceLocation.getPath() reaches here). Name who still holds the
            // un-forwarded stale arena receiver (the WRITEBACK-MISS root) before the
            // arena_getfield panic on `class None`.
            static G_TRACE_STALEFIELD: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            if env_gate("RAVA_TRACE_STALEFIELD", &G_TRACE_STALEFIELD)
                && crate::nextgen::class_name(crate::nextgen::arena_class_id(p)).is_none()
            {
                let addr = p.addr();
                let mut holders = vm.heap.find_arena_holders(addr);
                for &fp in &crate::frames::all() {
                    let f = unsafe { &*(fp as *const crate::Frame) };
                    for (i, lv) in f.locals().iter().enumerate() {
                        if matches!(lv, JVal::Obj(q) if q.addr() == addr) {
                            holders.push(format!("frame {} local[{i}]", f.class.file.this_class));
                        }
                    }
                    for (i, sv) in f.stack().iter().enumerate() {
                        if matches!(sv, JVal::Obj(q) if q.addr() == addr) {
                            holders.push(format!("frame {} stack[{i}]", f.class.file.this_class));
                        }
                    }
                }
                let fname = vm
                    .heap
                    .field_name(field_id as u32)
                    .unwrap_or_else(|| "?".to_string());
                eprintln!("[STALEFIELD-JIT] rava_getfield id={field_id} field={fname} on stale arena obj {addr:#x}");
                eprintln!("[STALE-HOLDERS] {} holder(s) of {addr:#x}:", holders.len());
                for hh in &holders {
                    eprintln!("    {hh}");
                }
            }
            // B5: a PINNED pointer is a handle object — serve via the handle read
            // funnel (mirror-served + synthetic fields), not the arena cell walk.
            #[cfg(feature = "next-gen-gc")]
            if let Some(h) = crate::nextgen::pinned_handle_of(p.addr() as u64) {
                let (v, _slot) = vm.heap.get_field_hint(h, field_id as u32, 0);
                return if is_ref != 0 {
                    box_ref(vm, &v)
                } else {
                    pack_local(&v)
                };
            }
            crate::opcodes::arena_cells::note_gf_jit();
            // Teach the inline probe what this crossing cost to learn. Every later
            // execution of the emitting site reads the slot out of machine code
            // instead of coming back here.
            arena_slot_note(p, field_id as u32);
            let v = crate::opcodes::arena_getfield(p, field_id as u32);
            return if is_ref != 0 {
                box_ref(vm, &v)
            } else {
                pack_local(&v)
            };
        }
        set_pending(vm, "java/lang/NullPointerException", "getfield");
        return 0;
    };
    let (v, slot) = vm.heap.get_field_hint(h, field_id as u32, 0);
    publish_field_hint(vm, field_id, slot);
    if is_ref != 0 {
        box_ref(vm, &v)
    } else {
        pack_local(&v)
    }
}

/// putfield via the real heap. `kind` selects the value decode (0=int,1=long,
/// 2=float,3=double,else reference). Mutating, so it short-circuits after a
/// pending fault (the method is unwinding) — matching the interpreter and the
/// native array stores. Null receiver → NPE pending; set_pending latches the
/// native-store fault flag too.
pub(crate) extern "C" fn rava_putfield(obj: i64, field_id: i64, val: i64, kind: i64) -> i64 {
    cb(7);
    // GAP-S09 stage 4 SANCTIONED (COW-shadowed / disjoint-by-admission): under
    // an armed region a gil-free write routes to the worker's TLS shadow
    // (`shadow_put_field` — the S2 "92,160 shadow puts / zero shared puts"
    // acceptance path) or to a worker-OWNED arena transient; without COW the
    // fork admission (`method_fork_safe`) only admits raw + index-disjoint
    // writes. The shared-heap fall-through is the serial/GIL-held arm.
    let Some(vm) = vm_ref_worker_sanctioned() else {
        return 0;
    };
    if vm.jit_pending_is_set() {
        return 0;
    }
    #[cfg(feature = "next-gen-gc")]
    let cow_active = crate::parallel::cow_active();
    #[cfg(feature = "next-gen-gc")]
    let _cow_timer = CowCallbackTimer::new(cow_active, &COW_PUT_CALLBACKS, &COW_PUT_CALLBACK_NS);
    // M3 9.2a shape probe: classify this compiled-callback write against the current initCache state.
    #[cfg(feature = "next-gen-gc")]
    if crate::m3shape::enabled() {
        if let JVal::Obj(p) = unbox_ref(vm, obj, "") {
            crate::m3shape::note_write(p.addr(), crate::nextgen::arena_class_id(p));
        }
    }
    // M3 9.2d.1/.2 biome probe: classify this compiled-callback write vs the shared RTree + tag type.
    #[cfg(feature = "next-gen-gc")]
    if crate::m3biome::enabled() {
        match unbox_ref(vm, obj, "") {
            JVal::Obj(p) => {
                let cn = crate::nextgen::class_name(crate::nextgen::arena_class_id(p));
                crate::m3biome::note_write(p.addr(), false, cn.as_deref().unwrap_or("?"));
            }
            JVal::Ref(h) => {
                let ty = vm.heap.class_of(h).unwrap_or("?");
                crate::m3biome::note_write(crate::heap::obj_gen(h), true, ty);
            }
            _ => {}
        }
    }
    let mut value = match kind {
        0 => JVal::Int(val as i32),
        1 => JVal::Long(val),
        2 => JVal::Float(f32::from_bits(val as u32)),
        3 => JVal::Double(f64::from_bits(val as u64)),
        _ => unbox_ref(vm, val, ""), // reference/array field
    };
    // PARALLELISM S2 — worker write-barrier (SOUND). A gil-free worker (not in a
    // COW-shadowed region) may write ONLY objects IT owns (its nursery view). A
    // write to a shared / pre-existing object off-GIL corrupts shared state — the
    // SIGSEGV root cause of the earlier gilfree-CF crash (rava_putfield fell
    // through to a DIRECT shared write with no gate, unlike fu_invoke/bitset/indy).
    // Deopt (request_abort → serial re-run) instead of writing. Over-aborts on
    // worker-local Ref handles for now (cow_owned covers arena Obj only) — safe;
    // completeness (reserved-handle ownership) is a later stage, measured by abort rate.
    // Scoped to the gilfree-CF lane (its flag) so existing forks (bitstorage /
    // chunk-fill / FORK_PARALLEL) that also set gilfree() are untouched and the
    // canonical 8790 gate stays byte-neutral when RAVA_GILFREE_CF is off.
    // GILFREE-RINGS: the lane arms `cow_armed` (so this callback fires for EVERY
    // compiled store — the codegen barrier), but it must NOT shadow. A CF task's
    // result is an object the worker itself allocated whose fields have to
    // survive `publish`; shadowing would discard exactly the writes that carry
    // the answer. So: worker-OWNED receiver → fall through to the direct write
    // (private to this task's TLAB view, published wholesale on completion);
    // anything shared → deopt to the serial re-run.
    #[cfg(feature = "next-gen-gc")]
    let ring_lane = crate::gilfree_cf::in_lane();
    #[cfg(feature = "next-gen-gc")]
    if ring_lane {
        let recv = unbox_ref(vm, obj, "");
        if !crate::parallel::cow_owned(&recv) {
            let recorded = if crate::gilfree_cf::field_txn::active() {
                match &recv {
                    JVal::Obj(p) => u8::try_from(kind)
                        .ok()
                        .and_then(crate::gilfree_cf::field_txn::FieldLayout::from_kind)
                        .is_some_and(|layout| {
                            crate::gilfree_cf::field_txn::record(
                                *p,
                                field_id as u32,
                                layout,
                                value.clone(),
                            )
                        }),
                    _ => false,
                }
            } else {
                false
            };
            if recorded {
                return 0;
            }
            crate::gilfree_cf::abort_census::note(vm, &recv);
            crate::gilfree_cf::ABORT_SHARED_FIELD
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            crate::parallel::request_abort();
            return 0;
        }
    } else if crate::gilfree_cf::enabled() && crate::parallel::gilfree() && !cow_active {
        let recv = unbox_ref(vm, obj, "");
        if !crate::parallel::cow_owned(&recv) {
            crate::parallel::request_abort();
            return 0;
        }
    }
    // M3 COW: a gil-free worker's write goes to its thread-local shadow, NOT the shared
    // object — the impure fork body doesn't publish/race. Discarded per element.
    #[cfg(feature = "next-gen-gc")]
    if cow_active && !ring_lane {
        let recv = unbox_ref(vm, obj, "");
        // OWNED-UNDO: direct write + old-value log (rolled back per element).
        if crate::parallel::cow_owned_undo() && crate::parallel::cow_owned(&recv) {
            if let JVal::Obj(p) = recv {
                let old = crate::opcodes::arena_getfield(p, field_id as u32);
                crate::parallel::undo_note(p.addr(), field_id as u32, old);
                crate::opcodes::arena_putfield(p, field_id as u32, &value);
                return 0;
            }
        }
        // Worker-owned receiver → fall through to the direct write (private, dies with the element).
        if !(crate::parallel::cow_owned_bypass() && crate::parallel::cow_owned(&recv)) {
            if let Some(key) = crate::parallel::cow_obj_key(&recv) {
                crate::parallel::shadow_put_field(key, field_id as u32, value);
                return 0;
            }
        }
    }
    let Some(h) = untag_ref(obj) else {
        // Arena / pinned receiver (next-gen-gc). A GENUINE arena object writes its
        // inline cell directly (arena_putfield's card barrier records old→young). A
        // PINNED mirror address MATERIALIZES AS Ref(h) through `unbox_ref` (the B5
        // coexistence rule — `pinned_handle_of` succeeds inside unbox_ref), so it must
        // route through put_field_hint — FieldBlob authoritative + pinned-mirror
        // dual-store — never a raw mirror-cell store that would strand the blob. (The
        // former `if let JVal::Obj(p)` pinned check here was DEAD: unbox_ref returns
        // Ref, not Obj, for a pinned address — the coherence hole once the arena-dual
        // emitters route pinned receivers here.)
        #[cfg(feature = "next-gen-gc")]
        match unbox_ref(vm, obj, "") {
            JVal::Ref(hh) => {
                materialize_ref_array_for_legacy_store(vm, &mut value);
                vm.heap.put_field_hint(hh, field_id as u32, value, 0);
                return 0;
            }
            JVal::Obj(p) => {
                // Same publication as the read side: a store site that had to come here
                // to find its slot gets the answer baked in for its next execution.
                arena_slot_note(p, field_id as u32);
                crate::opcodes::arena_putfield(p, field_id as u32, &value);
                return 0;
            }
            _ => {}
        }
        set_pending(vm, "java/lang/NullPointerException", "putfield");
        return 0;
    };
    #[cfg(feature = "next-gen-gc")]
    materialize_ref_array_for_legacy_store(vm, &mut value);
    let slot = vm.heap.put_field_hint(h, field_id as u32, value, 0);
    publish_field_hint(vm, field_id, slot);
    0
}
