//! Allocation and GC bridges: object creation, the write barrier, the safepoint poll, and the boxing pair.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;
use crate::value::Value as JVal;

/// SELF-REGISTRATION overflow path: the compiled prologue's inline cell-root
/// push found the flat per-thread stack full — push through the Rust side,
/// which grows it (Deep-IC v3). Never allocates on the Java heap → no GC.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_cells_push_slow(buf: i64, len: i64) -> i64 {
    crate::frames::cells_push((buf as *const i64, len.max(0) as usize));
    0
}
/// SELF-REGISTRATION box unwind (cold: only when the callee actually grew the
/// box table): restore the table to the prologue's base — RE-HOMING a
/// BOX-tagged return minted by the callee first (a dangling box index decodes
/// to an unrelated value later; mirrors jit_run's decode-before-truncate).
/// Returns the (possibly re-homed) bits.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_box_exit(base: i64, bits: i64) -> i64 {
    let base = base.max(0) as usize;
    if (bits as u64) >> 32 == (crate::value::BOX_TAG as u64) >> 32 {
        let idx = (bits & 0xFFFF_FFFF) as usize;
        if idx >= base {
            if let Some(v) = crate::frames::box_get(idx) {
                crate::frames::box_truncate(base);
                return (crate::frames::box_push(v) as i64) | crate::value::BOX_TAG;
            }
        }
    }
    crate::frames::box_truncate(base);
    bits
}





/// ldc of a reference constant from compiled code (next-gen-gc) → boxed cell.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_ldc(caller_ptr: i64, cp: i64) -> i64 {
    cb(15);
    // GAP-S09 census: cold ldc mutates shared intern/constant state — split out
    // of the base `other` kind (stage-3 abort-stub candidate).
    s09census::note(s09census::IDX_LDC);
    // GAP-S09 stage 4 SANCTIONED (abort-guarded): the gil-free triage directly
    // below either aborts the COLD Class-literal arm BEFORE any interning, takes
    // the locked-exec envelope, or serves a warm/pure constant without shared
    // mutation — the `&mut` never escapes those three arms.
    let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };
    // GAP-S09 stage 3: only the COLD Class-literal arm (`class_objects` interning)
    // is worker-illegal — a warm/pure hit stays served. Locked-exec runs the real
    // handler under the resolve lock, exactly like rava_getstatic_cached.
    let _locked_exec = if crate::parallel::gilfree() {
        if crate::worker_locked_exec_enabled() {
            Some(resolve_lock_acquire())
        } else if ldc_cold_class_literal(vm, caller_ptr, cp) {
            worker_abort_stub(s09census::IDX_LDC, "ldc(cold Class literal)");
            return 0;
        } else {
            None // warm/pure constant: serves without shared mutation
        }
    } else {
        None
    };
    if vm.jit_pending_is_set() {
        return 0;
    }
    let caller = caller_ptr as *const crate::class_loader::LoadedClass;
    match vm.jit_ldc(caller, cp as u16) {
        Ok(bits) => bits,
        Err(_) => {
            vm.heap.set_jit_fault();
            0
        }
    }
}

/// COLLECTION INTRINSIC (next-gen-gc): `Integer.valueOf(int)` — box the int into a
/// ref cell directly (rava boxes Integer as identity `Value::Int`), bypassing the
/// `rava_invoke` synthetic frame. `x` is the int operand (sign-extended).
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_box_int(x: i64) -> i64 {
    // GAP-S09 stage 4: `&Vm` (vm_shared) — `box_ref` takes `&Vm` and pushes onto
    // the thread-local box table; read-only on shared Vm state by signature.
    let Some(vm) = vm_shared() else { return 0 };
    box_ref(vm, &JVal::Int(x as i32))
}

/// COLLECTION INTRINSIC (next-gen-gc): `Integer.intValue()` — unbox a ref cell back
/// to its int directly, bypassing the `rava_invoke` synthetic frame.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_unbox_int(cell: i64) -> i64 {
    // GAP-S09 stage 4: `&Vm` (vm_shared) — `unbox_ref` takes `&Vm`; pure decode.
    let Some(vm) = vm_shared() else { return 0 };
    unbox_ref(vm, cell, "").as_int().unwrap_or(0) as i64
}

/// `new` via the real interpreter handler (runs `<clinit>`, allocates). Returns
/// the tagged reference cell for the fresh object; on a clinit Throw sets
/// jit_pending + latches the fault flag and returns 0 (null).
pub(crate) extern "C" fn rava_new(caller_ptr: i64, cp: i64) -> i64 {
    cb(3);
    // GAP-S09 stage 4 SANCTIONED: arena `new` on a gil-free worker is sound when the
    // target class is ALREADY loaded + initialized + layout-cached — the whole path
    // `:202-410` (memo read → name → layout → `tlab_try_alloc`) is then purely TLS
    // bump + read-only lookups, no shared mutation. A COLD class load or `<clinit>`
    // aborts inside the S07 loader guard (`loader.rs:1177`) or `ensure_init` before
    // any publication — region discards the batch and retries serially. Handle `new`
    // (native-backed / enum) still aborts on `new_object` → `put_object` (handle Vec
    // push), correctly refusing shared mutation. The inline TLAB arm
    // (`build_new_arena`) serves every post-`<clinit>` arena site that compiled AFTER
    // init and never reaches this callback; this path catches only the COLD ARM case
    // (method compiled before its `new` site's first exec → no inline → permanent
    // callback). `vm_ref_worker_sanctioned` drops the unconditional entry denial so
    // the worker can reach the guarded derivation path and succeed when sound.
    let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };
    if vm.jit_pending_is_set() {
        return 0;
    }
    // Site identity for the late-bound inline arm, captured before the cast shadows the raw
    // pointer. It is the same `(class_ptr, cp)` pair the emitter baked a slot address for.
    #[cfg(feature = "next-gen-gc")]
    let site_cls = caller_ptr as usize;
    let caller_ptr = caller_ptr as *const crate::class_loader::LoadedClass;
    match vm.jit_new(caller_ptr, cp as u16) {
        Ok(cell) => {
            // Publish the late-inline shape. The structural experiment reads the allocator's
            // resolved route; the legacy control inspects the returned object.
            #[cfg(feature = "next-gen-gc")]
            {
                let state = publish_new_site(vm, site_cls, cp, cell);
                // BIRTH CENSUS: what this callback just handed back, and why it was still a
                // callback. See the `[PC-BIRTH]` block at the bottom of this file.
                birth_census_note_new(vm, cell, state);
            }
            cell
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
                eprintln!("[JIT-ERR] jit new");
                let exc = vm.make_exception("java/lang/InternalError", "jit new");
                vm.jit_pending_set(exc);
            }
            vm.heap.set_jit_fault();
            0
        }
    }
}

/// instanceof via the real interpreter test (hierarchy + interface walk). Returns
/// 1/0. Read-only (class loads are idempotent), so it stays verify-safe.
pub(crate) extern "C" fn rava_instanceof(caller_ptr: i64, cp: i64, val: i64) -> i64 {
    cb(4);
    // GAP-S09 stage 4 SANCTIONED (read-only / abort-guarded): the hierarchy +
    // interface walk reads frozen loader state; a COLD class load on a gil-free
    // worker aborts inside the S07 loader guard (class_loader.rs) before any
    // publication.
    let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };
    if vm.jit_pending_is_set() {
        return 0;
    }
    // FALLBACK TALLY (RAVA_FALLBACK_TALLY): every compiled `instanceof` lands
    // here — the codegen emits no inline type test — so this is the one and only
    // 0xc1 fallback entry. Two-word key, no allocation, inert when unset.
    crate::fallback_tally::note(
        crate::fallback_tally::Kind::Instanceof,
        caller_ptr as usize,
        cp as u16,
    );
    let caller_ptr = caller_ptr as *const crate::class_loader::LoadedClass;
    match vm.jit_instanceof(caller_ptr, cp as u16, val) {
        Ok(r) => r,
        Err(crate::VmError::Throw(e)) => {
            if !vm.jit_pending_is_set() {
                vm.jit_pending_set(e);
            }
            0
        }
        Err(_) => 0,
    }
}

/// Diagnostic card-table callback for an INLINE arena putfield-ref. `obj` is the
/// raw receiver pointer; `val` is the ARENA-ENCODED value cell just stored inline.
/// Emitted only when `card_mark_callback_enabled()` is true.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_card_mark(obj: i64, val: i64) -> i64 {
    // M3 9.2a shape probe: this barrier fires after EVERY inline arena ref-putfield, so `obj` is the
    // mutated receiver — the choke point for compiled-inline ref writes. Classify vs current initCache.
    if crate::m3shape::enabled() && obj != 0 {
        let ocid = unsafe { ((obj as u64 as *const u64).read_unaligned() & 0xFFFF_FFFF) as u32 };
        crate::m3shape::note_write(obj as u64, ocid);
    }
    // M3 9.2d.1/.2: an inline arena ref-putfield — classify vs the shared RTree state + tag type.
    if crate::m3biome::enabled() && obj != 0 {
        let cid = unsafe { ((obj as u64 as *const u64).read_unaligned() & 0xFFFF_FFFF) as u32 };
        let cn = crate::nextgen::class_name(cid);
        crate::m3biome::note_write(obj as u64, false, cn.as_deref().unwrap_or("?"));
    }
    if val != 0 && (val & 0b11) == 0 {
        // SAFETY: `obj` is the live arena receiver; `val` (tag 0b00, non-null, < 2^40)
        // is a raw arena object pointer — both produced by the compiled store path.
        let recv = unsafe { crate::arena::ArenaPtr::from_addr(obj as u64) };
        let young = unsafe { crate::arena::ArenaPtr::from_addr(val as u64) };
        let val_young = crate::nextgen::tlab_in_nursery(young);
        // STALE-STORE forensic (RAVA_TRACE_STALESTORE): a compiled ref-putfield writing a young
        // pointer the collector already moved out from under this method (see stale_young_ptr) —
        // the store strands the stale addr in `obj`. Names the un-spilled compiled store site.
        if stale_store_trace() {
            if let Some(fwd) = stale_young_ptr(val) {
                let ohdr = unsafe { (obj as u64 as *const u64).read_unaligned() };
                let ocid = (ohdr & 0xFFFF_FFFF) as u32;
                let ocls = crate::nextgen::class_name(ocid).unwrap_or_default();
                // Collector-state context: where does the stale val sit vs the CURRENT nursery?
                // above yhi (young.top) ⇒ its slot was orphaned by a nursery RESET (top dropped
                // below it); inside [ylo,yhi) ⇒ still-nursery-resident but moved; below ylo ⇒ a
                // prior young region. epoch = minor-collection count (temporal marker).
                let (ylo, yhi) = crate::nextgen::tlab_young_window();
                let uval = val as u64;
                let pos = if uval >= yhi {
                    "ABOVE-TOP(reset-orphan)"
                } else if uval >= ylo {
                    "IN-WINDOW(moved)"
                } else {
                    "BELOW-LO(prior-young)"
                };
                eprintln!(
                    "[STALE-STORE] val={val:#x} fwd={fwd:#x} obj={obj:#x} obj_class={ocls} nfields={} pos={pos} window=[{ylo:#x},{yhi:#x}) epoch={} stack={:?}",
                    ohdr >> 32,
                    crate::nextgen::gc_epoch(),
                    jit_method_stack(6)
                );
            }
        }
        if crate::nextgen::shadow_enabled() && !crate::nextgen::tlab_in_nursery(recv) && val_young {
            crate::nextgen::shadow().card_mark((recv.addr() >> 9) as u32);
        }
    }
    0
}

/// Inline-TLAB-bump OVERFLOW handler (next-gen-gc). The compiled bump found the
/// nursery full. Run a native Cheney safepoint (`arena_collect_jit` forwards the
/// spilled JIT roots + interp frames + statics in place AND republishes the cursor
/// to the freed nursery), so the compiled retry loop re-reads a fresh `[next,end]`
/// and bumps into reclaimed space — no trampoline. Live refs were spilled by the
/// caller (`spill_for_gc`) and are reloaded after (`reload_after_gc`). Counted as
/// `other`, NOT `new`, so the per-alloc `new`-callback measurement stays honest
/// (this fires once per nursery-full, not once per object).
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_arena_safepoint() -> i64 {
    cb(20);
    // LANE PROBE: count this poll SEPARATELY from the `other` callback bucket —
    // the spin this callback's own comment predicts for a gil-free worker is only
    // attributable if it can be told apart from the 14 other `other` callbacks.
    crate::lane_probe::note_safepoint();
    // GAP-S09 stage 4 SANCTIONED (status quo, stage-5 target): a gil-free
    // nursery-full normally aborts in the nextgen TLAB slow paths
    // (request_abort_overflow) before this callback can fire; denying here
    // instead would leave the compiled alloc-retry loop (which polls no abort
    // flag) spinning against a full view. Kept behaviour-identical — the
    // worker-mode collection question is quarantined with the interp arms.
    // BOUNDED EXIT (return value). 0 = "space is available, retry the bump"; 1 =
    // "I could not free you anything, take your exit". Every arm that leaves the
    // caller's view exactly as full as it found it must report 1, or the compiled
    // retry loop — which polls nothing — re-enters immediately and spins. Only a
    // GIL-FREE worker can reach a no-progress arm: on main this callback always
    // collects, so the status is a constant 0 there and the extra branch never taken.
    let gilfree = crate::parallel::gilfree();
    let Some(vm) = vm_ref_worker_sanctioned() else { return gilfree as i64 };
    if vm.jit_pending_is_set() {
        return gilfree as i64;
    }
    crate::parallel::take_spill_starved(); // clear stale latch
    vm.arena_collect_jit();
    if gilfree {
        // The gil-free arm of arena_collect_jit either refilled the view from the
        // region spill pool (progress → retry) or starved (no progress → exit). It
        // never collects, and `maybe_gc` below MUST NOT run on a worker: its O(objects)
        // sweep would race the coordinator's parked-but-live `objects` Vec and this
        // worker's live siblings.
        return crate::parallel::take_spill_starved() as i64;
    }
    // JIT-BLIND-SWEEP FIX. arena_collect_jit only evacuates the ARENA; the RUST-side heap
    // (`objects: Vec<Box<Object>>`, arrays) is swept by Vm::gc, whose only trigger was the
    // INTERPRETER's op-counter safepoint. A boot runs 79% in compiled code, so that counter
    // barely advances and the sweep effectively never ran: 3.64M objects accumulated, 2200MB
    // RSS vs HotSpot's 544MB, and a fresh world OOMs on a RUST allocation while arena tenured
    // sits at ~108MB. Poll the same thresholds here, at the safepoint the JIT already emits.
    // Sound for the same reason arena_collect_jit is: this callback runs with the compiled
    // frame's refs spilled into Frame.locals, which is exactly what Vm::gc's frames::all()
    // walk reads. maybe_gc re-checks native_depth()==0 and is threshold-gated, so the
    // O(objects) sweep cannot fire per-safepoint.
    vm.maybe_gc();
    0
}

/// BOUNDED-EXIT allocation for the compiled inline-alloc arm (next-gen-gc). Called
/// only when `rava_arena_safepoint` reported it could free NO space — i.e. a
/// gil-free worker whose view is full and whose region spill pool is exhausted or
/// absent. Returns a writable, 8-aligned, sub-2^40 scratch pointer of `size` bytes
/// so the compiled body can lay its header down and RUN ON to its return instead of
/// re-entering the safepoint forever (the measured 1.44-billion-callback spin).
///
/// The region abort is raised unconditionally here, so nothing computed after this
/// point is ever kept: `run_batch`'s keep-all-or-discard-all vote discards the whole
/// batch and every task re-runs serially. That is what makes handing back throwaway
/// bytes sound — the alternative exits (null, or a bump past `end`) are a SIGSEGV
/// and a heap corruption respectively.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_arena_deopt_alloc(size: i64) -> i64 {
    cb(21);
    crate::parallel::request_abort_overflow();
    crate::nextgen::deopt_scratch(size.max(0) as usize) as i64
}

// ═════════════ LATE-BOUND INLINE `new` — the per-site shape table (next-gen-gc) ═════════════
//
// ⭐**MEASURED: `mi_malloc_aligned` is the single top row of the whole compiled-code profile,
// and `rava_new` / `jit_new` / `new_object` / `new_object_value` are four more rows under it.**
// Every one of those is a `new` that `Vm::build_new_arena` could not prove arena-eligible AT
// EMIT TIME — and that predicate has exactly ONE non-permanent term, `initialized`. A method
// compiles at its execution threshold, which is routinely BEFORE some of its `new` sites have
// first run (a cold arm of a hot method) and therefore before the target's `<clinit>`; a site
// rejected for that reason stays on the callback for the rest of the process, because nothing
// ever re-asks. That is the same bootstrap trap `emit/lambda_mint.rs` documents at length, and
// it takes the same cure: bake only the ADDRESS of a per-site word, publish the shape from the
// callback the first time the site really allocates, and let the emitted guard read it AT
// RUNTIME.
//
// ⭐**What is published is OBSERVED, never re-derived.** The callback just allocated an object
// FOR THIS SITE, so the header word the runtime path wrote IS the header word an inline bump
// must write — there is no second copy of the eligibility predicate to drift out of sync with
// `new_object_value`. Publication additionally demands, of that same object:
//   * a raw 8-aligned arena address inside the committed arena (`is_arena_obj_addr`), which
//     rejects every handle/array/box cell and any small integer that leaked into a ref slot;
//   * residence in THIS thread's nursery, which rejects the pinned-enum region
//     (`pinned_object_alloc`), the tenured space, and a `RAVA_PINNED_PTRS` handle mirror;
//   * a header tail and a full cell run that all read ZERO — the exact state
//     `obj_init_hdr_tail` + `set_cell(_,0)` leave, and the exact state the emitted bump
//     reproduces. This is what rejects `arena_map`'s five-cell map (its CAP/TABLE cells are
//     written at birth) and any future birth route that initialises a cell.
// A site that fails any of those is POISONED once and never examined again.
//
// Cost if removed: every such `new` goes back through `rava_new` → `new_object_value` → class
// name resolution, the route memo, Arc traffic, `intern_class`, and `box_ref` — hundreds of ns
// against the ten or so of a bump, once per object, on the hottest row in the profile.

/// Slots in the site table. Open addressing, 8-deep linear probe, never freed; a full chain
/// simply refuses the site, which leaves it on the callback (always correct).
#[cfg(feature = "next-gen-gc")]
const NEW_SITE_SLOTS: usize = 1 << 15;
#[cfg(feature = "next-gen-gc")]
const NEW_SITE_PROBE: usize = 8;
/// Slot states: `0` = never observed; bit 63 set = POISONED (this site is not a plain zeroed
/// arena bump and never will be); anything else = the header word `(nfields<<32)|class_id`.
/// Bit 63 is free by construction — `nfields` is a `u32` and is refused above 4096 — so the
/// emitted guard is a single signed `> 0` test that rejects both non-inlinable states at once.
#[cfg(feature = "next-gen-gc")]
const NEW_SITE_POISON: u64 = 1 << 63;

#[cfg(feature = "next-gen-gc")]
struct NewSites {
    cls: Box<[std::sync::atomic::AtomicUsize]>,
    cp: Box<[std::sync::atomic::AtomicU32]>,
    hdr: Box<[std::sync::atomic::AtomicU64]>,
}

/// Boxed slices, not a `Vec` that could reallocate: the emitter BAKES the address of `hdr[i]`
/// as an absolute constant into machine code, so it must be stable for the life of the process.
#[cfg(feature = "next-gen-gc")]
static NEW_SITES: std::sync::OnceLock<NewSites> = std::sync::OnceLock::new();
/// Claiming a slot happens on a compile thread and is rare; reading one happens in compiled
/// code and must be lock-free. So insertion serialises here and lookup is pure atomics.
#[cfg(feature = "next-gen-gc")]
static NEW_SITE_INSERT: std::sync::Mutex<()> = std::sync::Mutex::new(());
/// Non-zero once any site has been claimed — the publisher's early out, so a run that compiles
/// no `new` site at all pays exactly one relaxed load per `rava_new`.
#[cfg(feature = "next-gen-gc")]
static NEW_SITE_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// Sites decided so far, split by verdict (`RAVA_INLINE_NEW_TRACE` prints each publication).
#[cfg(feature = "next-gen-gc")]
pub(crate) static NEW_SITE_INLINED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
#[cfg(feature = "next-gen-gc")]
pub(crate) static NEW_SITE_POISONED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

#[cfg(feature = "next-gen-gc")]
fn new_sites() -> &'static NewSites {
    NEW_SITES.get_or_init(|| NewSites {
        cls: (0..NEW_SITE_SLOTS).map(|_| std::sync::atomic::AtomicUsize::new(0)).collect(),
        cp: (0..NEW_SITE_SLOTS).map(|_| std::sync::atomic::AtomicU32::new(0)).collect(),
        hdr: (0..NEW_SITE_SLOTS).map(|_| std::sync::atomic::AtomicU64::new(0)).collect(),
    })
}

/// Site identity is `(class_ptr, cp)` — the SAME pair `alloc_route_cache` keys on, the pair the
/// emitter holds (`env.class_ptr`) and the pair the callback receives (`caller_ptr`). Both
/// halves are stored and compared exactly; the hash only picks the chain.
#[cfg(feature = "next-gen-gc")]
fn new_site_hash(class_ptr: usize, cp: u16) -> usize {
    let mut h = (class_ptr as u64) ^ (cp as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h ^= h >> 30;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 27;
    (h as usize) & (NEW_SITE_SLOTS - 1)
}

/// KILL SWITCH `RAVA_NO_INLINE_NEW=1`: no slot is ever claimed, so the emitter finds no address
/// to bake and every `new` keeps the callback — byte-identical to the pre-existing engine.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn inline_new_late_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| !crate::flags::emit_more_disabled() && std::env::var_os("RAVA_NO_INLINE_NEW").is_none())
}

/// EMIT TIME (compile thread): the stable address of this site's shape word, claiming a slot if
/// the site has none. `None` when the chain is full or the switch is off — the site then keeps
/// the callback. Idempotent across recompiles of the same method, so a site that has already
/// learned its shape does not unlearn it when its method is compiled again.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn new_site_slot(class_ptr: usize, cp: u16) -> Option<i64> {
    // cp 0 is the reserved constant-pool index, so it doubles as the "key half not yet written"
    // marker below; a real `new` never carries it.
    if class_ptr == 0 || cp == 0 || !inline_new_late_enabled() {
        return None;
    }
    let t = new_sites();
    let h0 = new_site_hash(class_ptr, cp);
    let _g = NEW_SITE_INSERT.lock().unwrap_or_else(|e| e.into_inner());
    for k in 0..NEW_SITE_PROBE {
        let i = (h0 + k) & (NEW_SITE_SLOTS - 1);
        let c = t.cls[i].load(std::sync::atomic::Ordering::Relaxed);
        if c == class_ptr && t.cp[i].load(std::sync::atomic::Ordering::Relaxed) == cp as u32 {
            return Some(&t.hdr[i] as *const std::sync::atomic::AtomicU64 as usize as i64);
        }
        if c == 0 {
            // `cp` is published BEFORE `cls` (release) and `new_site_find` reads `cls` first
            // (acquire), so a concurrent reader that sees the owner also sees the whole key and
            // can never match a half-written slot.
            t.cp[i].store(cp as u32, std::sync::atomic::Ordering::Relaxed);
            t.cls[i].store(class_ptr, std::sync::atomic::Ordering::Release);
            NEW_SITE_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Some(&t.hdr[i] as *const std::sync::atomic::AtomicU64 as usize as i64);
        }
    }
    None
}

/// EMIT TIME: the shape this site has ALREADY published, if any. When it is known the emitter
/// bakes the size, the header word and an unrolled cell wipe and guards on exact equality; when
/// it is not, it emits the generic arm whose size and wipe come from the loaded word. A slot is
/// written once (0 → shape, or 0 → poison), so a baked equality guard cannot go stale — it is
/// kept only because a two-instruction proof is cheaper than trusting this sentence.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn new_site_known(addr: i64) -> Option<u64> {
    if addr == 0 {
        return None;
    }
    // SAFETY: `addr` came from `new_site_slot`, which only ever hands back the address of an
    // `AtomicU64` inside the process-lifetime `NEW_SITES` boxed slice.
    let w = unsafe {
        (*(addr as *const std::sync::atomic::AtomicU64)).load(std::sync::atomic::Ordering::Acquire)
    };
    if w == 0 || (w & NEW_SITE_POISON) != 0 { None } else { Some(w) }
}

#[cfg(feature = "next-gen-gc")]
fn new_site_find(class_ptr: usize, cp: u16) -> Option<&'static std::sync::atomic::AtomicU64> {
    let t = new_sites();
    let h0 = new_site_hash(class_ptr, cp);
    for k in 0..NEW_SITE_PROBE {
        let i = (h0 + k) & (NEW_SITE_SLOTS - 1);
        let c = t.cls[i].load(std::sync::atomic::Ordering::Acquire);
        // Slots are never released, so the first empty entry ends the chain.
        if c == 0 {
            return None;
        }
        if c == class_ptr && t.cp[i].load(std::sync::atomic::Ordering::Relaxed) == cp as u32 {
            return Some(&t.hdr[i]);
        }
    }
    None
}

/// Can an inline zeroed TLAB bump reproduce the birth that just produced `cell`? Returns the
/// header word to write, or `None` to poison the site. Every test here is on the OBSERVED
/// object — no name, no package, no workload, no timing.
///
/// The `Err` names WHICH test refused — read once per site by the birth census, so the
/// `[PC-BIRTH]` line can say whether the permanent-callback population is handle-born (the
/// heap's `Box<Object>`), pinned, an arena map, or a birth route that initialises a cell.
#[cfg(feature = "next-gen-gc")]
fn observed_arena_birth(cell: i64) -> Result<u64, PoisonWhy> {
    if cell <= 0 {
        return Err(PoisonWhy::NotArena);
    }
    let a = cell as u64;
    // Not a handle/array/box cell (those carry a high-word tag), not a stray small integer, and
    // inside the committed arena so the header read below cannot fault.
    if !crate::nextgen::is_arena_obj_addr(a) {
        return Err(PoisonWhy::NotArena);
    }
    // SAFETY: `is_arena_obj_addr` proved `a` is an 8-aligned address inside the committed arena,
    // which is the precondition every other arena header read in the engine takes.
    let p = unsafe { crate::arena::ArenaPtr::from_addr(a) };
    // NURSERY ONLY. An inline bump allocates from `[cursor.next, cursor.end)`; the pinned-enum
    // region and the tenured space are different allocators with different lifetimes, and a
    // `RAVA_PINNED_PTRS` handle mirror is neither. Publishing a shape observed there would have
    // compiled code bump a nursery object where the runtime path makes a pinned one.
    if !crate::nextgen::tlab_in_nursery(p) {
        return Err(PoisonWhy::NotNursery);
    }
    // `arena_map` births a five-cell map with CAP/TABLE already written; the zero-cell test
    // below would reject it anyway, but naming it here keeps the reason readable.
    if crate::arena_map::is_arena_map(p) {
        return Err(PoisonWhy::ArenaMap);
    }
    // SAFETY: as above — a validated arena object header.
    let hw = unsafe { std::ptr::read(a as *const u64) };
    let nf = (hw >> 32) as u32;
    // 4096 cells is far past any real class layout and keeps `nfields<<32` clear of bit 63,
    // which is what lets the emitted guard be one signed compare.
    if hw == 0 || nf > 4096 {
        return Err(PoisonWhy::Header);
    }
    // The header TAIL and every cell must read zero — the state `obj_init_hdr_tail` plus the
    // `set_cell(_, 0)` loop leave, and the state the emitted bump writes. Any birth route that
    // initialises a cell (arena maps, synthetic backings) fails here and stays on the callback.
    // SAFETY: validated arena object; the tail is inside `HDR` and the cells inside `nf`.
    unsafe {
        if std::ptr::read((a + 8) as *const u64) != 0 {
            return Err(PoisonWhy::Tail);
        }
        if crate::nextgen::HDR > 16 && std::ptr::read((a + 16) as *const u64) != 0 {
            return Err(PoisonWhy::Tail);
        }
    }
    for i in 0..nf as usize {
        if crate::nextgen::arena_cell(p, i) != 0 {
            return Err(PoisonWhy::Cell);
        }
    }
    Ok(hw)
}

/// Publish a resolved route under RAVA_STRUCTURAL_NEW_SITE, otherwise inspect the object.
/// Reached only on the success
/// path, so `<clinit>` has run and the class is initialised — which is precisely the term that
/// made `build_new_arena` refuse the site at emit time, and it is monotone: a class never
/// becomes uninitialised again.
///
/// Returns the site's state AS THIS CALLBACK FOUND IT, for the birth census — the loads and
/// stores are exactly the ones this function always made; only the early `return`s now carry a
/// name.
#[cfg(feature = "next-gen-gc")]
fn publish_new_site(vm: &crate::Vm, class_ptr: usize, cp: i64, cell: i64) -> SiteState {
    if NEW_SITE_COUNT.load(std::sync::atomic::Ordering::Relaxed) == 0 {
        return SiteState::Unclaimed;
    }
    let Ok(cp) = u16::try_from(cp) else { return SiteState::Unclaimed };
    let Some(slot) = new_site_find(class_ptr, cp) else { return SiteState::Unclaimed };
    let w = slot.load(std::sync::atomic::Ordering::Relaxed);
    if w != 0 {
        // already decided, either way — never re-examined
        return if w & NEW_SITE_POISON != 0 { SiteState::Poisoned } else { SiteState::Inlined };
    }
    // A null cell means the handler produced NO object (a pending `<clinit>` throw, a
    // worker denial, a nursery-full null). That is inconclusive, not a refusal: leave the
    // slot undecided so the next execution of the site gets to answer. Poisoning here would
    // permanently disqualify a good site for a transient condition.
    if cell == 0 {
        return SiteState::Undecided;
    }
    static STRUCTURAL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let structural = *STRUCTURAL.get_or_init(||
        std::env::var_os("RAVA_STRUCTURAL_NEW_SITE").is_some());
    let verdict = if structural {
        // The route is derived from class/layout metadata by the real allocator.
        // A missing route or unfinished initializer is unresolved, never a rejection.
        let routes = vm.runtime_shared();
        match routes.alloc_route_get(&(class_ptr, cp)) {
            Some(crate::opcodes::NewRoute::Arena { class, cid, nf, pinned: false }) => {
                if !class.clinit_done.load(std::sync::atomic::Ordering::Acquire) {
                    return SiteState::Undecided;
                }
                if cid == 0 || nf > 4096 {
                    Err(PoisonWhy::Header)
                } else {
                    Ok(((nf as u64) << 32) | cid as u64)
                }
            }
            Some(crate::opcodes::NewRoute::Arena { pinned: true, .. }) => Err(PoisonWhy::NotNursery),
            Some(crate::opcodes::NewRoute::ArenaMap) => Err(PoisonWhy::ArenaMap),
            Some(crate::opcodes::NewRoute::Handle) => Err(PoisonWhy::NotArena),
            None => return SiteState::Undecided,
        }
    } else {
        observed_arena_birth(cell)
    };
    let hw = verdict.unwrap_or(NEW_SITE_POISON);
    slot.store(hw, std::sync::atomic::Ordering::Release);
    match verdict {
        Err(why) => {
            NEW_SITE_POISONED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            birth_census_note_poison(why);
        }
        Ok(hw) => {
            NEW_SITE_INLINED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if std::env::var_os("RAVA_INLINE_NEW_TRACE").is_some() {
                eprintln!(
                    "[INLINE-NEW] cp={cp} class_id={} nfields={} {:?}",
                    hw as u32,
                    (hw >> 32) as u32,
                    crate::nextgen::class_name(hw as u32)
                );
            }
        }
    }
    SiteState::First
}

// ═════════════════════ BIRTH CENSUS — `[PC-BIRTH]` (kill switch `RAVA_NO_HR_ALLOC=1`) ═════════════════════
//
// ⭐**`mi_malloc_aligned` is 3.07% + `mi_bitmap_try_find_and_claim` 1.61% of the bootstrap bench
// UNDER COMPILED BODIES, and nothing said which birth route was paying it.** Read end to end,
// the allocation bridges reach the host allocator on exactly these shapes, and on no other:
//
//   * `rava_new` → `jit_new` → `new_object_value`, ARENA route (memo hit → `new_route_replay` →
//     `tlab_try_alloc`): ZERO host allocations per object. `Frame::from_parts(.., Vec::new(), ..)`
//     allocates nothing, the memoised `NewRoute::Arena` clone is an `Arc` increment, `box_ref` of a
//     `Value::Obj` is `p.addr()`. Only the FIRST visit to a site builds a name `String` and inserts
//     the memo. And the late-bound arm above takes the site off the callback after that first
//     visit anyway.
//   * `rava_new`, HANDLE route (`NewRoute::Handle` → `new_object` → `alloc_interned` → `put_object`):
//     one `Box<Object>` per object (`heap/objects_alloc.rs`), `FieldBlob::new()` is a sentinel, and
//     `prepopulate_known` is inert unless `RAVA_LAYOUT_ALLOC`. That Box IS the handle heap's
//     representation of the object — a class-constant side structure it is not. Which classes are
//     born this way is a name/ancestor/enum verdict (`birth_native_backed_alloc`), permanent, and
//     it poisons the late-bound slot, so every later `new` at the site is a callback for the life
//     of the process.
//   * `rava_newarray` / `rava_anewarray` → `alloc_array` / `alloc_ref_array_by_id` → `empty_array`:
//     one `Vec` per array (`heap/gens.rs:19`), none for a zero-length array. Structural:
//     `emit/ops/objects.rs` records that arrays are handle-indexed descriptors and
//     `tlab_alloc_array` has zero call sites, so NO array is ever an arena object.
//   * `jit_indy` (not this file): `Box<Object>` + `Box<Lambda>` per minted lambda.
//
// So the census does not price a Vec the class could own — none is left on these paths — it
// prices WHICH of the three structural populations the bench's compiled bodies feed, and how
// many `rava_new` callbacks are the PERMANENT kind (a poisoned site) against the one-shot kind
// (a first visit that the late-bound arm then serves inline). Those are different projects:
// the first is the handle heap, the second is already done, and the third is arrays.
//
// COUNTERS ARE PLAIN CELLS: `load` + `store`, two `mov`s, never a locked RMW — the rule this
// wave set for anything on a per-call path. Guest births run under the GIL, so the cells are
// exact on main; a gil-free ring worker can reach the array bridges and may lose an increment to
// main's, which a census tolerates and a lock would not be worth. `RAVA_NO_HR_ALLOC=1` (or the
// block switch `RAVA_NO_EMIT_MORE=1`) skips every increment and prints nothing — the historic
// behaviour exactly, since the historic behaviour was the absence of this instrument.
//
// REPORTED FROM THE PERIODIC PROFILER DUMP (`pc_profile/dump.rs`, beside `compile_bail::report`),
// never from a teardown a server boot does not reach.

/// What a `rava_new` callback found at its late-bound site (see `publish_new_site`).
#[cfg(feature = "next-gen-gc")]
#[derive(Clone, Copy)]
enum SiteState {
    /// No slot: the method was compiled without the late-bound arm (switch off, no cursor,
    /// probe chain full) — a callback for the life of the compiled body.
    Unclaimed,
    /// Slot undecided and the callback produced no object; the next visit decides.
    Undecided,
    /// This visit published the verdict (inlined or poisoned).
    First,
    /// Permanently a callback: the observed birth was not a plain zeroed nursery bump.
    Poisoned,
    /// A shape is published yet the guard still fell through (baked-equality mismatch or a
    /// cursor that could not serve) — should read ~0; anything else names an emitter gap.
    Inlined,
}

/// Which test in `observed_arena_birth` refused the site.
#[cfg(feature = "next-gen-gc")]
#[derive(Clone, Copy)]
enum PoisonWhy {
    /// A tagged cell (handle / box) or a non-arena address — the handle-heap population.
    NotArena,
    /// Arena address outside this thread's nursery (pinned enum region, tenured, mirror).
    NotNursery,
    ArenaMap,
    Header,
    Tail,
    Cell,
}

#[cfg(feature = "next-gen-gc")]
mod birth_census {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    // Outcome of a `rava_new` callback, by the SHAPE of the returned cell — the high word first
    // (handle `1<<40`, box `1<<41`, array `1<<42`), then `is_arena_obj_addr` for the untagged
    // remainder. No cell is ever dereferenced here.
    pub(super) const O_ARENA: usize = 0;
    pub(super) const O_HANDLE: usize = 1;
    pub(super) const O_BOX: usize = 2;
    pub(super) const O_NULL: usize = 3;
    pub(super) const O_OTHER: usize = 4;
    pub(super) static OUTCOME: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

    // Site state at the callback: Unclaimed, Undecided, First, Poisoned, Inlined.
    pub(super) static SITE: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

    // Why a site was poisoned: NotArena, NotNursery, ArenaMap, Header, Tail, Cell.
    pub(super) static WHY: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];

    // Arrays: [newarray, newarray len 0, anewarray, anewarray len 0].
    pub(super) static ARRAYS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

    /// Plain-cell increment: a load and a store, no lock prefix.
    #[inline(always)]
    pub(super) fn bump(c: &AtomicU64) {
        c.store(c.load(Relaxed).wrapping_add(1), Relaxed);
    }

    /// Handle-born classes seen at `rava_new`, counted only under `RAVA_JIT_PIC_PROBE` (a name
    /// lookup and a mutex per birth are diagnostic-run costs, not product-path ones).
    pub(super) fn names() -> &'static std::sync::Mutex<crate::FastMap<String, u64>> {
        static M: std::sync::OnceLock<std::sync::Mutex<crate::FastMap<String, u64>>> =
            std::sync::OnceLock::new();
        M.get_or_init(|| std::sync::Mutex::new(crate::FastMap::default()))
    }
}

/// `RAVA_NO_HR_ALLOC=1` or `RAVA_NO_EMIT_MORE=1` → no cell is ever bumped, no line is ever
/// printed. Resolved once.
#[cfg(feature = "next-gen-gc")]
fn hr_alloc_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        !crate::flags::emit_more_disabled() && std::env::var_os("RAVA_NO_HR_ALLOC").is_none()
    })
}

/// Classify the cell `rava_new` is about to return, and the site state it was served from.
#[cfg(feature = "next-gen-gc")]
#[inline]
fn birth_census_note_new(vm: &crate::Vm, cell: i64, state: SiteState) {
    if !hr_alloc_enabled() {
        return;
    }
    use birth_census::*;
    let hi = (cell as u64) >> 32;
    let o = if cell == 0 {
        O_NULL
    } else if hi == crate::jit::REF_TAG_HI {
        O_HANDLE
    } else if hi == (crate::value::BOX_TAG as u64) >> 32 {
        O_BOX
    } else if hi == 0 && crate::nextgen::is_arena_obj_addr(cell as u64) {
        O_ARENA
    } else {
        O_OTHER
    };
    bump(&OUTCOME[o]);
    bump(&SITE[state as usize]);
    if o == O_HANDLE && crate::picprobe::enabled() {
        // The handle is the low 32 bits of a `REF_TAG` cell (`box_ref`); `class_of` is a bounds-
        // checked slot read, so a stale or dummy handle answers `None` rather than faulting.
        if let Some(c) = vm.heap.class_of((cell & 0xFFFF_FFFF) as u32) {
            let mut g = names().lock().unwrap_or_else(|e| e.into_inner());
            match g.get_mut(c) {
                Some(n) => *n += 1,
                None => {
                    g.insert(c.to_string(), 1);
                }
            }
        }
    }
}

/// Once per poisoned site: which refusal.
#[cfg(feature = "next-gen-gc")]
fn birth_census_note_poison(why: PoisonWhy) {
    if hr_alloc_enabled() {
        birth_census::bump(&birth_census::WHY[why as usize]);
    }
}

/// One line each in `rava_newarray` (`kind` 0) and `rava_anewarray` (`kind` 1), after their
/// negative-count throw: every call here is one host `Vec` unless `count` is zero.
#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn birth_census_note_array(kind: u8, count: i64) {
    if !hr_alloc_enabled() {
        return;
    }
    let base = if kind == 0 { 0 } else { 2 };
    birth_census::bump(&birth_census::ARRAYS[base]);
    if count == 0 {
        birth_census::bump(&birth_census::ARRAYS[base + 1]);
    }
}
#[cfg(not(feature = "next-gen-gc"))]
#[inline]
pub(crate) fn birth_census_note_array(_kind: u8, _count: i64) {}

/// The `[PC-BIRTH]` block for the periodic profiler dump; empty when nothing was counted.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn birth_census_report() -> String {
    use birth_census::*;
    use std::sync::atomic::Ordering::Relaxed;
    if !hr_alloc_enabled() {
        return String::new();
    }
    let out_n: [u64; 5] = std::array::from_fn(|i| OUTCOME[i].load(Relaxed));
    let site: [u64; 5] = std::array::from_fn(|i| SITE[i].load(Relaxed));
    let why: [u64; 6] = std::array::from_fn(|i| WHY[i].load(Relaxed));
    let arr: [u64; 4] = std::array::from_fn(|i| ARRAYS[i].load(Relaxed));
    let tot: u64 = out_n.iter().sum();
    if tot + arr[0] + arr[2] == 0 {
        return String::new();
    }
    let pct = |x: u64| if tot > 0 { x as f64 * 100.0 / tot as f64 } else { 0.0 };
    let mut s = format!(
        "[PC-BIRTH] rava_new callbacks {tot}: arena {} ({:.1}%, no host alloc) | handle {} ({:.1}%, one Box<Object> each) | box {} | null {} | other {}\n",
        out_n[O_ARENA], pct(out_n[O_ARENA]), out_n[O_HANDLE], pct(out_n[O_HANDLE]), out_n[O_BOX], out_n[O_NULL], out_n[O_OTHER]
    );
    s.push_str(&format!(
        "[PC-BIRTH]   site at callback: poisoned(permanent) {} ({:.1}%) | unclaimed(no slot) {} ({:.1}%) | first-visit {} | undecided(null) {} | inlined-but-called {}\n",
        site[SiteState::Poisoned as usize], pct(site[SiteState::Poisoned as usize]),
        site[SiteState::Unclaimed as usize], pct(site[SiteState::Unclaimed as usize]),
        site[SiteState::First as usize], site[SiteState::Undecided as usize], site[SiteState::Inlined as usize]
    ));
    s.push_str(&format!(
        "[PC-BIRTH]   late-bound sites: claimed {} inlined {} poisoned {} — why: not-arena(handle) {} not-nursery {} arena-map {} header {} tail {} cell {}\n",
        NEW_SITE_COUNT.load(Relaxed), NEW_SITE_INLINED.load(Relaxed), NEW_SITE_POISONED.load(Relaxed),
        why[0], why[1], why[2], why[3], why[4], why[5]
    ));
    s.push_str(&format!(
        "[PC-BIRTH]   arrays from compiled code (never arena, one host Vec each): newarray {} (len0 {}) anewarray {} (len0 {})\n",
        arr[0], arr[1], arr[2], arr[3]
    ));
    if crate::picprobe::enabled() {
        let g = names().lock().unwrap_or_else(|e| e.into_inner());
        let mut rows: Vec<(&String, &u64)> = g.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1));
        let total: u64 = rows.iter().map(|(_, n)| **n).sum::<u64>().max(1);
        for (c, n) in rows.iter().take(20) {
            s.push_str(&format!(
                "[PC-BIRTH]     handle-born {:>6.2}%  {:>9}  {c}\n",
                **n as f64 * 100.0 / total as f64,
                **n
            ));
        }
    }
    s
}
#[cfg(not(feature = "next-gen-gc"))]
pub(crate) fn birth_census_report() -> String {
    String::new()
}
