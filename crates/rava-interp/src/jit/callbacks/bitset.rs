//! BitSet bridges: word-level get/set/clear/isEmpty against the native $bits backing.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;


/// java.util.BitSet get/set/clear/isEmpty FAST-PATH (next-gen-gc): same ABI as
/// `rava_invoke`. BitSet is native-served (`$bits` long[] on the heap receiver),
/// so neither the direct-call cache nor the dispatch PIC can ever serve these
/// sites — every call paid the full invoke gauntlet (block-shape merging:
/// ~8.5M `get` + ~1M `set` per RingScan setup, the boot Blocks-bootstrap twin).
/// Read/write the packed word straight off the heap; ANY mismatch (non-Ref
/// receiver, subclass, missing/short `$bits`, negative index) falls back to the
/// identical general dispatch, which is always correct.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_bitset_get(caller_ptr: i64, cp: i64, kind: i64, buf: *const i64, n: i64) -> i64 {
    // GAP-S09 stage 4 SANCTIONED (read-only probe): `jit_bitset_get` only READS
    // the receiver's `$bits` word (its `&mut` receiver is signature inheritance);
    // the `$bits` mutator twins (set/clear) are worker-gated (abort/locked), so
    // the word is frozen for the region. Miss arm = abort-guarded `rava_invoke`.
    if let Some(vm) = vm_ref_worker_sanctioned() {
        if !vm.jit_pending_is_set() && n >= 2 {
            let recv = crate::nextgen::forward_if_moved(unsafe { *buf });
            let idx = unsafe { *buf.add(1) } as i32;
            if let Some(bits) = vm.jit_bitset_get(recv, idx) {
                return bits;
            }
        }
    }
    rava_invoke(caller_ptr, cp, kind, buf, n)
}
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_bitset_set(caller_ptr: i64, cp: i64, kind: i64, buf: *const i64, n: i64) -> i64 {
    s09census::note(s09census::IDX_BITSET_MUT);
    // GAP-S09 stage 3: BitSet.set writes the shared `$bits` word (heap receiver,
    // not COW-shadowed) — worker-illegal. Abort BEFORE the write (the general-
    // dispatch fallback would race the same word); locked-exec runs the whole
    // body (fast arm + fallback) under the resolve lock.
    let _locked_exec = match worker_illegal_gate(s09census::IDX_BITSET_MUT, "bitset_set") {
        WorkerGate::Serial => None,
        WorkerGate::Locked(g) => Some(g),
        WorkerGate::Abort => return 0,
    };
    if let Some(vm) = vm_ref() {
        if !vm.jit_pending_is_set() && n >= 2 {
            let recv = crate::nextgen::forward_if_moved(unsafe { *buf });
            let idx = unsafe { *buf.add(1) } as i32;
            // set(I)V → true; set(IZ)V → the third cell's truth.
            let val = if n >= 3 { (unsafe { *buf.add(2) }) != 0 } else { true };
            if let Some(bits) = vm.jit_bitset_put(recv, idx, val) {
                return bits;
            }
        }
    }
    rava_invoke(caller_ptr, cp, kind, buf, n)
}
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_bitset_clear(caller_ptr: i64, cp: i64, kind: i64, buf: *const i64, n: i64) -> i64 {
    s09census::note(s09census::IDX_BITSET_MUT);
    // GAP-S09 stage 3: receiver-mutator (see rava_bitset_set).
    let _locked_exec = match worker_illegal_gate(s09census::IDX_BITSET_MUT, "bitset_clear") {
        WorkerGate::Serial => None,
        WorkerGate::Locked(g) => Some(g),
        WorkerGate::Abort => return 0,
    };
    if let Some(vm) = vm_ref() {
        if !vm.jit_pending_is_set() && n >= 2 {
            let recv = crate::nextgen::forward_if_moved(unsafe { *buf });
            let idx = unsafe { *buf.add(1) } as i32;
            if let Some(bits) = vm.jit_bitset_put(recv, idx, false) {
                return bits;
            }
        }
    }
    rava_invoke(caller_ptr, cp, kind, buf, n)
}
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_bitset_isempty(caller_ptr: i64, cp: i64, kind: i64, buf: *const i64, n: i64) -> i64 {
    // GAP-S09 stage 4 SANCTIONED: read-only `$bits` scan — see rava_bitset_get.
    if let Some(vm) = vm_ref_worker_sanctioned() {
        if !vm.jit_pending_is_set() && n >= 1 {
            let recv = crate::nextgen::forward_if_moved(unsafe { *buf });
            if let Some(bits) = vm.jit_bitset_isempty(recv) {
                return bits;
            }
        }
    }
    rava_invoke(caller_ptr, cp, kind, buf, n)
}
