//! Array bridges: allocation, bounds-checked element access, and the reference-store barriers.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;
use crate::value::Value as JVal;

/// `newarray` (primitive) via the heap. Negative count → NegativeArraySizeException
/// pending. Returns the tagged array cell.
pub(crate) extern "C" fn rava_newarray(atype: i64, count: i64) -> i64 {
    cb(5);
    // GAP-S09 stage 4 SANCTIONED (region claim / abort-before-publication): the
    // gil-free arm below claims a slot from the PRE-RESERVED parallel array
    // region (`worker_newarray`, `&self`) and aborts on exhaustion before any
    // shared Vec push; regions are opened by the fork driver exactly for the
    // admitted newarray-bearing trees. The shared `alloc_array` fall-through is
    // unreachable for those admissions (interp-arm residue → stage-5 quarantine).
    let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };
    if vm.jit_pending_is_set() {
        return 0;
    }
    if count < 0 {
        // The message IS the count on HotSpot ("NegativeArraySizeException: -1");
        // an empty one hid which dimension the guest asked for.
        set_pending(vm, "java/lang/NegativeArraySizeException", &(count as i32).to_string());
        return 0;
    }
    birth_census_note_array(0, count); // [PC-BIRTH] census (callbacks/alloc.rs); the per-kind/per-length census and the recycled buffer live in heap/arrays/pool.rs ([PC-ARRAYS])
    // GILFREE-RINGS: the lane opens no parallel ARRAY region (only a nursery
    // view + an object-handle range), so the fall-through below would take the
    // shared `alloc_array` — a Vec push racing the running main thread. Deopt.
    #[cfg(feature = "next-gen-gc")]
    if crate::gilfree_cf::in_lane() && !vm.heap.parallel_array_active() {
        crate::gilfree_cf::ABORT_ARRAY_ALLOC.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        crate::parallel::request_abort();
        return 0;
    }
    // G3.5: a GIL-free fork worker allocates into the pre-reserved parallel array region (the
    // heap Vec push/realloc/intern are not concurrent-safe). Exhaustion → abort → serial retry.
    #[cfg(feature = "next-gen-gc")]
    if crate::parallel::gilfree() && vm.heap.parallel_array_active() {
        match vm.heap.worker_newarray(atype, count as usize) {
            Some(h) => return pack_local(&JVal::Array(h)),
            None => {
                crate::gilfree_cf::ABORT_ARRAY_REGION_FULL
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                crate::parallel::request_abort();
                return 0;
            }
        }
    }
    let elem = crate::opcodes::primitive_array_type(atype as u8);
    let h = vm.heap.alloc_array(elem, count as usize);
    pack_local(&JVal::Array(h))
}

/// `anewarray` (reference) via the real handler (resolves the element class).
pub(crate) extern "C" fn rava_anewarray(caller_ptr: i64, cp: i64, count: i64) -> i64 {
    cb(5);
    // GAP-S09 stage 4 SANCTIONED (abort-guarded): the stage-3 gil-free triage
    // below claims a pre-reserved slot for a warm element type, runs locked
    // under locked-exec, or fires the abort stub BEFORE `intern_array_element`
    // / the `bytes` tally — the `&mut` never escapes those arms on a worker.
    let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };
    if vm.jit_pending_is_set() {
        return 0;
    }
    if count < 0 {
        // Same message contract as rava_newarray: HotSpot names the count.
        set_pending(vm, "java/lang/NegativeArraySizeException", &(count as i32).to_string());
        return 0;
    }
    birth_census_note_array(1, count); // [PC-BIRTH] census (callbacks/alloc.rs); the per-kind/per-length census and the recycled buffer live in heap/arrays/pool.rs ([PC-ARRAYS])
    // GAP-S09 stage 3: the real handler (jit_anewarray → alloc_array) MUTATES shared
    // state BEFORE the array-table worker guard — `intern_array_element` (Vec push +
    // map insert) and the `bytes` tally. A gil-free worker may only CLAIM a
    // pre-reserved slot for an ALREADY-interned element type (read-only pool +
    // frozen-interner resolve, then `worker_array_put` — the same claim path
    // rava_newarray takes); a cold element type, exhaustion, or no open region
    // aborts BEFORE any of those mutations. Locked-exec runs the real handler under
    // the resolve lock (exclusive &mut Vm), exactly like rava_getstatic_cached.
    #[cfg(feature = "next-gen-gc")]
    let _locked_exec = if crate::parallel::gilfree() {
        if crate::worker_locked_exec_enabled() {
            Some(resolve_lock_acquire())
        } else {
            if vm.heap.parallel_array_active() {
                let caller =
                    unsafe { (caller_ptr as *const crate::class_loader::LoadedClass).as_ref() };
                if let Some(elem) = caller.and_then(|c| c.file.pool.class_name(cp as u16)) {
                    // Same descriptor rule as the interpreter's anewarray: a
                    // component that is already an array type keeps its own
                    // spelling (crate::opcodes::with_ref_desc).
                    let desc = crate::opcodes::with_ref_desc(elem, |d| d.to_string());
                    if let Some(h) = vm.heap.worker_anewarray_ref(&desc, count as usize)
                    {
                        return pack_local(&JVal::Array(h));
                    }
                }
            }
            worker_abort_stub(5, "anewarray(outside reserved region)");
            return 0;
        }
    } else {
        None
    };
    let caller_ptr = caller_ptr as *const crate::class_loader::LoadedClass;
    match vm.jit_anewarray(caller_ptr, cp as u16, count as usize) {
        Ok(cell) => cell,
        Err(crate::VmError::Throw(e)) => {
            if !vm.jit_pending_is_set() {
                vm.jit_pending_set(e);
            }
            0
        }
        Err(_) => 0,
    }
}

/// Array length. Null → NPE pending.
pub(crate) extern "C" fn rava_arraylength(arr: i64) -> i64 {
    cb(22);
    // GAP-S09 stage 4 SANCTIONED (read-only): `array_len` is `&self`; the null
    // fault arm is the pre-existing first-fault surface (TLS pending + the
    // worker-private JitCtx latch; the region tears down on the fault).
    let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };
    if vm.jit_pending_is_set() {
        return 0;
    }
    #[cfg(feature = "next-gen-gc")]
    if let Some(len) = arena_primitive_array_len(arr) {
        return len as i64;
    }
    let value = unbox_ref(vm, arr, "[");
    let Some(array) = crate::heap::ArrayRef::from_value(&value) else {
        set_pending(vm, "java/lang/NullPointerException", "arraylength");
        return 0;
    };
    array.len(&vm.heap).unwrap_or(0) as i64
}

/// Generic array element load. `is_ref` (1/0): aaload is a reference load →
/// `box_ref`; primitive element loads pack flat. Null → NPE; OOB → AIOOBE.
pub(crate) extern "C" fn rava_aload(arr: i64, idx: i64, is_ref: i64) -> i64 {
    cb(8);
    // GAP-S09 stage 4 SANCTIONED (read-only): bounds-checked `array_get`
    // (`&self`) + TLS boxing; fault arms as in rava_arraylength.
    let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };
    if vm.jit_pending_is_set() {
        return 0;
    }
    let array_value = unbox_ref(vm, arr, "[");
    let Some(array) = crate::heap::ArrayRef::from_value(&array_value) else {
        // "array", NOT "aload": the native load's own null branch was removed (null
        // fails the ARRAY_TAG test and arrives here instead), and `rava_array_fault`
        // — the arm that used to handle it — raises the null case with that message.
        // The same guest event must not produce a different exception depending on
        // which JIT gate happened to be taken.
        set_pending(vm, "java/lang/NullPointerException", "array");
        return 0;
    };
    let len = array.len(&vm.heap).unwrap_or(0);
    if idx < 0 || idx as usize >= len {
        array_bounds_pending(vm, idx, len);
        return 0;
    }
    match array.get(&vm.heap, idx as usize) {
        Some(v) => {
            if is_ref != 0 {
                box_ref(vm, &v)
            } else {
                pack_local(&v)
            }
        }
        None => 0,
    }
}

/// DATA-PARALLEL fork support: resolve a primitive-array handle to its raw element
/// base pointer + length, called ONCE per array on the main thread (under the GIL)
/// before the workers start. Writes the length (or -1 for null / a reference array /
/// a bad handle) to `*len_out` and returns the base pointer (0 on failure). No
/// fault is raised — a non-positive length fails the fork's `limit <= length` gate,
/// routing to the bounds-checking serial fallback. Pure read; the pointer stays valid
/// for the alloc-free parallel region (no GC ⇒ the fixed-length backing never moves).
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_array_base_len(arr: i64, len_out: i64) -> i64 {
    cb(23);
    let write_len = |n: i64| unsafe { *(len_out as *mut i64) = n };
    // GAP-S09 stage 2: `&Vm` (vm_shared) — `Heap::array_base_ptr` is `&self` and
    // the "Pure read" contract above (no fault raised) is now signature-enforced.
    let Some(vm) = vm_shared() else { write_len(-1); return 0 };
    let Some(h) = untag_array(arr) else { write_len(-1); return 0 };
    match vm.heap.array_base_ptr(h) {
        Some((p, len)) => {
            write_len(len as i64);
            p as i64
        }
        None => {
            write_len(-1);
            0
        }
    }
}

/// `if_acmp*` reference identity. Unboxes both cells and defers to the
/// interpreter's `ref_equal`, so compiled comparisons match the interpreter
/// exactly (tagged-handle identity, Str content-equality, boxed-primitive
/// value-equality). Returns 1 (equal) / 0 (not equal). Pure read — no side
/// effects, so it stays verify-safe.
pub(crate) extern "C" fn rava_acmp(x: i64, y: i64) -> i64 {
    cb(9);
    // GAP-S09 stage 2: `&Vm` (vm_shared) — `unbox_ref` takes `&Vm` and
    // `ref_equal` compares two decoded values; the "Pure read — no side effects"
    // contract above is now signature-enforced.
    let Some(vm) = vm_shared() else { return 0 };
    let a = unbox_ref(vm, x, "");
    let b = unbox_ref(vm, y, "");
    crate::opcodes::ref_equal(&a, &b) as i64
}

/// aastore fallback: decode the JIT ref cell and store it via the heap (which
/// re-encodes Str/boxed into the persistent array_box). Used for the box / null-
/// array / out-of-bounds cases; the native fast path handles bare handle/null
/// elements inline. No-op after a pending fault.
pub(crate) extern "C" fn rava_aastore(arr: i64, idx: i64, val: i64) -> i64 {
    cb(10);
    // GAP-S09 stage 4 SANCTIONED (disjoint-by-admission / COW-skip): fork
    // admission (`method_fork_safe`) only admits index-disjoint element writes;
    // under RAVA_COW_ARRAY_SKIP a gil-free store into a shared array is skipped
    // entirely (the COW discard twin). Fault arms as in rava_arraylength.
    let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };
    if vm.jit_pending_is_set() {
        return 0;
    }
    let array_value = unbox_ref(vm, arr, "[");
    let Some(array) = crate::heap::ArrayRef::from_value(&array_value) else {
        set_pending(vm, "java/lang/NullPointerException", "aastore");
        return 0;
    };
    let len = array.len(&vm.heap).unwrap_or(0);
    if idx < 0 || idx as usize >= len {
        array_bounds_pending(vm, idx, len);
        return 0;
    }
    // STALE-STORE forensic (RAVA_TRACE_STALESTORE): an aastore of a young arena pointer whose
    // forward word is already set = the collector moved it while the caller held the pre-move
    // ptr → storing the stale addr into the array (the heap-array holder found by
    // find_arena_holders). Names the compiled store site.
    if stale_store_trace() {
        if let Some(fwd) = stale_young_ptr(val) {
            eprintln!("[STALE-ASTORE] val={val:#x} fwd={fwd:#x} idx={idx} in {:?}", jit_method_cur());
        }
    }
    // P3 COW aastore hole (RAVA_COW_ARRAY_SKIP): a shared memo Object[] write races
    // across workers, so preserve the opt-in skip for shared handles. Arrays claimed
    // from the current parallel region are private and must still receive their stores.
    #[cfg(feature = "next-gen-gc")]
    if let crate::heap::ArrayRef::Handle(handle) = &array {
        if crate::parallel::gilfree()
            && cow_array_skip()
            && !vm.heap.array_in_parallel_region(*handle)
        {
            crate::parallel::CNT_ARRAY_SKIP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return 0;
        }
    }
    // GIL-free handle-array store barrier: only a handle allocated from this
    // region's pre-reserved array range is worker-private. Shared-graph handles
    // still abort before mutation and are replayed serially.
    #[cfg(feature = "next-gen-gc")]
    if let crate::heap::ArrayRef::Handle(handle) = &array {
        if crate::gilfree_cf::enabled()
            && crate::parallel::gilfree()
            && !vm.heap.array_in_parallel_region(*handle)
        {
            crate::parallel::request_abort();
            return 0;
        }
    }
    let value = unbox_ref(vm, val, "");
    let rejection = match array {
        crate::heap::ArrayRef::Handle(handle) => {
            crate::opcodes::astore_reject(vm, handle, &value)
        }
        crate::heap::ArrayRef::Arena(pointer) => crate::nextgen::arena_ref_array_component(pointer)
            .and_then(|component| {
                crate::opcodes::astore_reject_for_element(vm, &component, &value)
            }),
    };
    if let Some(message) = rejection {
        set_pending(vm, "java/lang/ArrayStoreException", &message);
        return 0;
    }
    let _ = array.store(&mut vm.heap, idx as usize, value);
    0
}

/// GILFREE-RINGS array-store worker barrier — the hole the field arm never
/// covered. Emitted on the COW-armed side of
/// every `*astore`, so no compiled array write can reach memory ungated.
///
/// Returns 1 = "SKIP the store" — the rings lane deopts rather than write an
/// array off-GIL, since array cells carry no per-object ownership record and a
/// store into the shared graph is precisely the corruption this lane must not
/// commit. Returns 0 = "perform the compiled inline store, unchanged", which is
/// what EVERY other COW-armed context (the chunk-fill / bitstorage forks, whose
/// disjoint element writes are proven by their own admission) gets — so those
/// forks stay byte-identical whether or not this arm was emitted.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_ring_store_guard(arr: i64) -> i64 {
    if crate::gilfree_cf::in_lane() {
        if is_arena_primitive_array(arr) {
            return 0;
        }
        // The reference-array producer admits only method-confined arrays. A raw reference array
        // observed in this lane was therefore allocated from this worker's nursery after the
        // fork began and cannot alias the shared capture graph.
        if arena_reference_arrays_enabled()
            && arr > 0
            && (arr as u64) & 7 == 0
            && (arr as u64) >> 40 == 0
        {
            let pointer = unsafe { crate::arena::ArenaPtr::from_addr(arr as u64) };
            if crate::nextgen::is_arena_ref_array(pointer) {
                return 0;
            }
        }
        // WORKER-OWNED ARRAY → perform the store. The "no per-object ownership record"
        // this barrier was written against is supplied by the open parallel array
        // region: slots [parr_base, parr_cap) are reserved fresh for THIS region, so an
        // array allocated there was allocated BY a worker DURING this region and is
        // unreachable from the shared graph (a task's only inputs are captures that
        // predate the region). Each array owns its own data buffer and the descriptor
        // spine cannot move while the region is open, so the inline store races nothing.
        //
        // Without this the barrier aborted on the FIRST array write of every ring task
        // AND returned 1 = "skip the store", so the body kept running against an array
        // it believed it had filled — which is why the measured first task died a few
        // callbacks later with `NullPointerException: getfield` rather than at the
        // store itself.
        // Behind `RAVA_GILFREE_ARRAY_OWNED` (DEFAULT ON; `RAVA_NO_GILFREE_ARRAY_OWNED` opts out): sound, and it does unblock
        // the body, but the dispatch/lock wall it exposes turns a fast abort into an
        // unbounded grind — see `gilfree_cf::array_owned_stores` for the measurement.
        if crate::gilfree_cf::array_owned_stores() {
            if let Some(h) = untag_array(arr) {
                if vm_shared().is_some_and(|vm| vm.heap.array_in_parallel_region(h)) {
                    return 0;
                }
            }
        }
        crate::gilfree_cf::ABORT_ARRAY_STORE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        crate::parallel::request_abort();
        return 1;
    }
    0
}

/// Cold fault path for NATIVE array access. The native fast path bounds-checks
/// inline and branches here only on null/out-of-bounds: set the matching pending
/// exception (NPE if the reference is null, else AIOOBE) and return. The compiled
/// method then runs to completion harmlessly and `jit_run` propagates the throw —
/// same run-to-completion model as the read-only load callbacks.
/// DIAGNOSTIC (RAVA_VERIFY_LOADS): called on the native primitive-load OK path
/// with the value the inline load produced (packed like pack_local); re-reads
/// via the interpreter and names every mismatch — the exact divergence probe
/// for the disabled-native-loads corruption.
/// SHADOW VERIFY for the bare-deref flip (`RAVA_BARE_VERIFY`, default OFF, diagnostic).
///
/// ⭐**Why a shadow and not the flip itself.** With `RAVA_JIT_BARE_REF_LOAD=1` the boot stops dead
/// just past `Environment:` with no panic — a wild pointer, silently — and the forensic built for
/// it (`RAVA_PROBE_STALE`) fires when `arena_getfield` NOTICES a stale receiver. **The flip removes
/// the check that notices.** A tool that depends on the tag cannot debug the removal of the tag,
/// which is why that run printed nothing.
///
/// So: run with the flip OFF, execution correct and tagged, and ask at every bare-ELIGIBLE site
/// what the bare load WOULD have returned. Two ways it can be wrong, and they need different fixes:
///
/// * `tagged` — the cell is not 0b00, so a bare load returns a wild pointer. This is the RESIDUE
///   the declared-type rule is meant to eliminate; the cell census puts it at 37 of 3,332,584.
/// * `stale` — the cell IS 0b00 but the object has since been evacuated, so the raw pointer is
///   dead while a handle would have survived transparently. This is the population the arc's own
///   record blames for the failure: **76,572 stale receivers with residue at 37.**
///
/// Returns 0; the caller ignores it. Counting only — it cannot change what it measures.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_bare_verify(cell: i64, fid: i64, slot: i64) -> i64 {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    static TOTAL: AtomicU64 = AtomicU64::new(0);
    static TAGGED: AtomicU64 = AtomicU64::new(0);
    static STALE: AtomicU64 = AtomicU64::new(0);
    let n = TOTAL.fetch_add(1, Relaxed) + 1;
    if cell != 0 {
        if (cell as u64) & 0b11 != 0 || (cell as u64) >> 40 != 0 {
            TAGGED.fetch_add(1, Relaxed);
        } else if crate::nextgen::forward_if_moved(cell) != cell {
            STALE.fetch_add(1, Relaxed);
        }
    }
    if n == 1 || n % 20_000 == 0 {
        eprintln!(
            "[BARE-VERIFY] {n} bare-eligible loads | tagged={} stale={} (last fid={fid} slot={slot})",
            TAGGED.load(Relaxed),
            STALE.load(Relaxed)
        );
    }
    0
}

pub(crate) extern "C" fn rava_aload_verify(arr: i64, idx: i64, native_bits: i64, site: i64) -> i64 {
    // GAP-S09 stage 2: `&Vm` (vm_shared) — a pure re-read diagnostic
    // (`array_get`/`array_element`/`array_len`, all `&self`) that only eprints
    // divergences; it never raises a fault or mutates the heap.
    let Some(vm) = vm_shared() else { return 0 };
    #[cfg(feature = "next-gen-gc")]
    if verify_arena_primitive_array_load(arr, idx, native_bits, site) {
        return 0;
    }
    let Some(h) = untag_array(arr) else {
        eprintln!("[LOADDIV] untagged receiver {arr:#x} idx={idx}");
        return 0;
    };
    let interp = match vm.heap.array_get(h, idx as usize) {
        Some(v) => pack_local(&v),
        None => {
            eprintln!("[LOADDIV] interp None h={h} idx={idx}");
            return 0;
        }
    };
    if interp != native_bits {
        let elem = vm.heap.array_element(h).as_deref().unwrap_or("?").to_string();
        let len = vm.heap.array_len(h).unwrap_or(0);
        eprintln!(
            "[LOADDIV] h={h} elem={elem} len={len} idx={idx} site={site} native={native_bits:#x} interp={interp:#x}"
        );
    }
    0
}

pub(crate) extern "C" fn rava_array_fault(arr: i64, idx: i64) -> i64 {
    cb(24);
    // GAP-S09 stage 4 SANCTIONED (fault latch only): sets the TLS pending slot
    // + the ACTIVE (worker-private) JitCtx fault word — see rava_arraylength.
    let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };
    if vm.jit_pending_is_set() {
        return 0;
    }
    // set_pending latches heap.jit_fault so later native stores in this frame
    // no-op (the method is unwinding) — mirrors the callback stores' guard.
    let length = untag_array(arr).map(|h| vm.heap.array_len(h).unwrap_or(0));
    #[cfg(feature = "next-gen-gc")]
    let length = length
        .or_else(|| arena_primitive_array_len(arr))
        .or_else(|| {
            let value = unbox_ref(vm, arr, "[");
            crate::heap::ArrayRef::from_value(&value).and_then(|array| array.len(&vm.heap))
        });
    if let Some(length) = length {
        array_bounds_pending(vm, idx, length);
    } else {
        set_pending(vm, "java/lang/NullPointerException", "array");
    }
    0
}

pub(crate) extern "C" fn rava_iastore(arr: i64, idx: i64, val: i64) -> i64 {
    cb(25);
    // GAP-S09 stage 4 SANCTIONED (disjoint-by-admission): primitive element
    // store into an index-disjoint partition — the exact write class the fork
    // admission proves; fault arms as in rava_arraylength.
    let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };
    if vm.jit_pending_is_set() {
        return 0;
    }
    let Some(h) = untag_array(arr) else {
        set_pending(vm, "java/lang/NullPointerException", "iastore");
        return 0;
    };
    let len = vm.heap.array_len(h).unwrap_or(0);
    if idx < 0 || idx as usize >= len {
        array_bounds_pending(vm, idx, len);
        return 0;
    }
    vm.heap.array_set(h, idx as usize, JVal::Int(val as i32));
    0
}

pub(crate) extern "C" fn rava_dastore(arr: i64, idx: i64, val: i64) -> i64 {
    cb(26);
    // GAP-S09 stage 4 SANCTIONED: disjoint primitive store — see rava_iastore.
    let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };
    if vm.jit_pending_is_set() {
        return 0;
    }
    let Some(h) = untag_array(arr) else {
        set_pending(vm, "java/lang/NullPointerException", "dastore");
        return 0;
    };
    let len = vm.heap.array_len(h).unwrap_or(0);
    if idx < 0 || idx as usize >= len {
        array_bounds_pending(vm, idx, len);
        return 0;
    }
    vm.heap
        .array_set(h, idx as usize, JVal::Double(f64::from_bits(val as u64)));
    0
}

/// Bounds-checked long-array store (the serial map fallback's write path). NPE on null,
/// AIOOBE on out-of-range — exactly where the guest would throw.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_lastore(arr: i64, idx: i64, val: i64) -> i64 {
    cb(27);
    // GAP-S09 stage 4 SANCTIONED: disjoint primitive store — see rava_iastore.
    let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };
    if vm.jit_pending_is_set() {
        return 0;
    }
    let Some(h) = untag_array(arr) else {
        set_pending(vm, "java/lang/NullPointerException", "lastore");
        return 0;
    };
    let len = vm.heap.array_len(h).unwrap_or(0);
    if idx < 0 || idx as usize >= len {
        array_bounds_pending(vm, idx, len);
        return 0;
    }
    vm.heap.array_set(h, idx as usize, JVal::Long(val));
    0
}

fn array_bounds_pending(vm: &mut crate::Vm, index: i64, length: usize) {
    set_pending(vm, "java/lang/ArrayIndexOutOfBoundsException",
        &format!("Index {} out of bounds for length {length}", index as i32));
}
