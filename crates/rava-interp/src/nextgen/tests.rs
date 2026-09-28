//! Arena/GC phase-2 tests.

use crate::arena::ArenaPtr;
use std::sync::Arc;
use super::*;

/// B2 (handle→pointer migration): the pinned region hands out distinct,
/// 8-aligned, low-address pointers that the address-space predicates recognise
/// as deref-safe arena addresses AND as pinned. Being non-moving, a written
/// value survives verbatim (no collector relocates a pinned object). Before the
/// first alloc `is_pinned` is inert (foundation of the bit-exact guarantee).
#[test]
fn pinned_region_allocates_stable_recognised_addresses() {
    let _gil = PINNED_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    assert!(!is_pinned(0), "null is never pinned");
    // ALLOCATED AS OBJECTS, not as raw bytes. `pinned_walk` enumerates this region by striding on
    // each header's `nfields`, which is exact only while the region holds nothing else — and these
    // two `pinned_alloc(64)`/`pinned_alloc(40)` calls used to be the one place in the tree that
    // broke it. They did so silently and only in a FULL test run, because alone they ran after the
    // walk's own test rather than before it.
    let p1 = pinned_object_alloc(1, 5); // HDR + 5*8 = 64 with HDR 24
    let p2 = pinned_object_alloc(2, 2);
    assert_ne!(p1, p2, "distinct allocations");
    assert_eq!(p1 as u64 & 7, 0, "8-aligned");
    assert_eq!(p2 as u64 & 7, 0, "8-aligned");
    assert!(
        (p2 as u64) >= p1 as u64 + crate::nextgen::obj_size(5) as u64,
        "bump advanced past p1"
    );
    for p in [p1, p2] {
        assert!(is_pinned(p as u64), "allocated address is pinned");
        assert!(is_arena_obj_addr(p as u64), "pinned address is a deref-safe arena addr");
    }
    // Non-moving: a written header survives (nothing relocates a pinned object).
    unsafe { *(p1 as *mut u64) = 0xdead_beef_cafe_f00d };
    assert_eq!(unsafe { *(p1 as *const u64) }, 0xdead_beef_cafe_f00d);
    assert!(is_pinned(p1 as u64), "still pinned after write");
}

/// B3 building block: `pinned_object_alloc` writes the SAME header layout as an
/// arena object (class/nfields/fwd=0/hash=0) at a non-moving pinned address, and
/// reserves `obj_size(nfields)` so its inline cells sit at `[p + HDR + slot*8]` —
/// the arena field-access offset. Two allocs don't overlap.
/// `pinned_alloc`'s contract is **single GIL writer**, and `cargo test` runs tests in PARALLEL.
/// Two threads bumping it can interleave: one reserves its range, and the other publishes
/// `PINNED_TOP` past it BEFORE the first has written its header — so a concurrent `pinned_walk`
/// reads zeros, strides by a bare `HDR`, and desynchronises. That is not a defect in the walk or
/// the allocator; it is this test suite standing in for a mutator that is serialised in
/// production. Every test that bumps the pinned region takes this lock, which is the suite's
/// stand-in for the GIL.
#[cfg(feature = "next-gen-gc")]
static PINNED_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn pinned_walk_enumerates_every_object_exactly_once() {
    let _gil = PINNED_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // The walk's soundness rests on one property: the pinned region holds nothing but
    // header-formatted objects, laid end to end by a single bump, so `obj_size(nfields)` is the
    // stride. Allocate a few with DIFFERENT field counts — a uniform size would pass even if the
    // stride ignored `nfields` — and check that every one is seen, in order, with its own class id
    // and width.
    let a = pinned_object_alloc(101, 0);
    let b = pinned_object_alloc(102, 3);
    let c = pinned_object_alloc(103, 7);
    let mut seen: Vec<(usize, u32, u32)> = Vec::new();
    // ⚠Walk from `a`, NOT from the region base. The region is append-only and process-wide: it
    // carries every allocation any earlier test made, and walking the whole of it makes this test
    // an assertion about the OTHER tests' headers. Walking from its own first object tests the
    // stride over a run this test built and nothing else.
    pinned_walk_from(a as usize, |p, cid, nf| {
        seen.push((p as usize, cid, nf));
        true
    });
    assert_eq!(
        &seen[..3.min(seen.len())],
        &[(a as usize, 101, 0), (b as usize, 102, 3), (c as usize, 103, 7)][..],
        "walk must report this test's three objects, in order, each with its own class and width"
    );
    // And the stride landed on each header exactly: consecutive reports are `obj_size` apart.
    assert_eq!(b as usize - a as usize, crate::nextgen::obj_size(0), "stride over a 0-field object");
    assert_eq!(c as usize - b as usize, crate::nextgen::obj_size(3), "stride over a 3-field object");
}

#[test]
fn pinned_object_alloc_writes_arena_header_layout() {
    let _gil = PINNED_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let p = pinned_object_alloc(42, 3);
    assert!(is_pinned(p as u64) && is_arena_obj_addr(p as u64));
    unsafe {
        assert_eq!(*(p as *const u32), 42, "class id at [0]");
        assert_eq!(*(p.add(4) as *const u32), 3, "nfields at [4]");
        assert_eq!(*(p.add(8) as *const u64), 0, "fwd word 0 (pinned never moves)");
        assert_eq!(obj_hash(p), 0, "identity hash unset");
        // inline cells [HDR..HDR+3*8) are addressable and zeroed
        for slot in 0..3 {
            assert_eq!(*(p.add(HDR + slot * 8) as *const u64), 0, "cell {slot} zeroed");
        }
    }
    // The next object must not overlap this one's full obj_size.
    let q = pinned_object_alloc(7, 1);
    assert!((q as u64) >= p as u64 + obj_size(3) as u64, "no overlap");
}

/// The header is exactly the size the active layout feature claims, and cells
/// stay 8-aligned (every cell is an i64, so HDR MUST be a multiple of 8 — that is
/// why 24→16 is the only step available and 24→12 is not).
#[test]
fn header_size_matches_layout_feature() {
    assert_eq!(HDR % 8, 0, "cells are i64 — HDR must stay 8-aligned");
    assert!(HASH_OFF + 4 <= HDR, "hash u32 must fit inside the header");
    assert!(HASH_OFF >= 12, "hash must not overlap class/nfields/fwd");
    if cfg!(feature = "shrink-header") {
        assert_eq!(HDR, 16);
        assert_eq!(HASH_OFF, 12);
    } else {
        assert_eq!(HDR, 24);
        assert_eq!(HASH_OFF, 16);
    }
}

/// Forwarding word round-trip. Under `shrink-header` the target is stored as a
/// biased u32 cell offset from `ARENA_FLOOR`; this asserts the encode/decode is
/// LOSSLESS across the range the collector actually produces, that 0 still means
/// "not forwarded", and — the property the whole scheme rests on — that installing
/// a forward leaves CLASS, NFIELDS and the identity HASH untouched. Those three are
/// what `arena_obj_header_plausible` / `forward_stale_arena` / `stale_young_ptr`
/// re-read on an already-forwarded object; a layout that clobbered them would
/// corrupt silently rather than crash.
#[test]
fn forwarding_word_roundtrip_preserves_class_nfields_hash() {
    let p = pinned_object_alloc(9, 2);
    unsafe {
        assert!(obj_fwd(p).is_null(), "freshly allocated ⇒ not forwarded");
        obj_set_hash(p, 0x2bad_c0de);
        // Targets the evacuators really emit: 8-aligned addresses at/above the floor.
        for delta in [0u64, 8, 4096, 1 << 20, 1 << 30, 3584 * 1024 * 1024] {
            let target = (ARENA_FLOOR + delta) as *mut u8;
            obj_set_fwd(p, target);
            assert_eq!(obj_fwd(p), target, "forward round-trip at +{delta}");
            assert!(!obj_fwd(p).is_null(), "a real target is never read as null");
            assert_eq!(obj_class(p), 9, "class survives forwarding");
            assert_eq!(obj_nfields(p), 2, "nfields survives forwarding");
            assert_eq!(obj_hash(p), 0x2bad_c0de, "identity hash survives forwarding");
        }
        // And the collector's own idempotence probe still reads it back.
        obj_set_fwd(p, ARENA_FLOOR as *mut u8);
        assert_eq!(obj_fwd(p) as u64, ARENA_FLOOR, "offset 0 target is not 'unset'");
    }
}

/// Regression: the inline-bump cursor must NEVER move backwards across a nested
/// compiled-method entry. A compiled method advances `cursor.next` inline
/// without touching `young.top`; if `jit_cursor_addr` (run at every compiled
/// entry) republished `base+young.top` unconditionally it would regress the
/// frontier below the caller's freshly inline-allocated objects, and the nested
/// method would bump-allocate ON TOP of a live object (the record self-store →
/// SoundEvent.toString infinite recursion). jit_cursor_addr now syncs first, so
/// it is idempotent and monotonic.
#[test]
fn jit_cursor_never_regresses_across_nested_entry() {
    // Entry of an outer compiled method: publish the cursor.
    let addr = jit_cursor_addr() as *mut u64;
    let start = unsafe { *addr };
    // The outer method inline-bumps the cursor (advance next, leave young.top) —
    // exactly what emit_inline_arena_new emits for an inline `new`.
    unsafe { *addr = start + 40 };
    let frontier = unsafe { *addr };
    // Entry of a NESTED compiled method: must not regress below the frontier.
    let _ = jit_cursor_addr();
    let after = unsafe { *addr };
    assert!(after >= frontier, "cursor regressed: {after:#x} < {frontier:#x}");
    // A Rust-path alloc must now land AT/ABOVE the inline frontier — never over
    // an object the inline bump already handed out.
    let cid = register_class(&(Arc::from("CursorT") as Arc<str>), &(vec![(0u32, 0u8)].into()));
    let p = tlab_try_alloc(cid, 1).unwrap();
    assert!(p.addr() as u64 >= frontier, "alloc {:#x} collided below frontier {frontier:#x}", p.addr());
}

/// ArenaString survives a Cheney collection with its bytes + hash intact — the
/// proof that modelling a string as a ref-mask-0 arena object is GC-correct (the
/// collector copies `obj_size(nf)` bytes and never traces the inline byte cells).
#[test]
fn arena_string_survives_collection() {
    let _call_context = crate::vm::thread_context::WorkerThreadContextGuard::install();
    let mut fallback = Vec::new();
    crate::vm::thread_context::push_call(
        &mut fallback,
        "GcProbe".to_owned(),
        "collect()V".to_owned(),
    );
    let s1 = tlab_alloc_string(b"item512-3").unwrap();
    let dead = tlab_alloc_string(b"garbage-not-rooted").unwrap();
    let s2 = tlab_alloc_string("juniper\u{2603}".as_bytes()).unwrap(); // multi-byte UTF-8
    assert!(is_arena_string(s1) && is_arena_string(s2));
    assert_eq!(arena_str_bytes(s1), b"item512-3");
    assert_eq!(arena_str_len(s2), "juniper\u{2603}".len());
    assert_eq!(arena_str_hash(s1), "item512-3".chars().fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(c as i32)));
    let _ = dead;

    // Root s1 + s2 (NOT dead) and force a collection; they must move + keep data.
    let mut roots: Vec<*mut u8> = vec![s1.as_ptr(), s2.as_ptr()];
    let (evac, _) = tlab_collect(&mut roots);
    assert_eq!(
        crate::vm::thread_context::with_call_stack(
            &fallback,
            crate::vm::thread_context::render_call_stack,
        ),
        "  at GcProbe.collect()V",
        "moving collection must not disturb worker-local diagnostic state"
    );
    assert!(evac >= 2, "the two rooted strings evacuated");
    let n1 = unsafe { ArenaPtr::new(roots[0]).unwrap() };
    let n2 = unsafe { ArenaPtr::new(roots[1]).unwrap() };
    assert!(is_arena_string(n1) && is_arena_string(n2));
    assert_eq!(arena_str_bytes(n1), b"item512-3", "bytes survived the move");
    assert_eq!(arena_str_bytes(n2), "juniper\u{2603}".as_bytes());
    assert_eq!(arena_str_hash(n1), "item512-3".chars().fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(c as i32)));
}

/// AUDIT B1 PROOF — an arena-lambda survives a Cheney collection with:
///   • a REFERENCE capture (the only ref to a moving arena object) FORWARDED —
///     the captured object is kept alive + the lambda's cell repointed;
///   • a PRIMITIVE (long) capture whose bit pattern IS a live young arena address
///     left UNCHANGED — the collector SKIPPED it (never mis-forwarded a prim).
/// This is the whole B1 question: the per-SIGNATURE class `ref_mask` (kind 4 ⇒ ref,
/// kind 1 ⇒ long/not-ref) is sufficient — NO new per-object capture bitmap.
#[test]
fn arena_lambda_survives_collection() {
    // A tiny captured target object (1 int field = 42), reachable ONLY via the lambda.
    let tname: Arc<str> = Arc::from("ALCaptureTarget");
    let tlayout: Arc<[(u32, u8)]> = vec![(0u32, 0u8)].into();
    let tcid = register_class(&tname, &tlayout);
    let target = tlab_try_alloc(tcid, 1).unwrap();
    unsafe { set_cell(target.as_ptr(), 0, 42) };
    let target_old = target.addr();

    // Site identity via the interned table (NOT the arena address).
    let site = std::sync::Arc::new(crate::heap::LambdaSite::new(
        Arc::from("pkg/Owner"),
        Arc::from("lambda$0"),
        Arc::from("(Ljava/lang/Object;J)Ljava/util/function/Supplier;"),
    ));
    let site_idx = lambda_site_intern(&site);

    // Capture signature: [ref, long]. The ref cell holds the raw arena ptr to
    // `target`; the long cell holds `target_old` bits (an address-aliasing prim
    // — the CHENEY-MISS bait) that MUST be skipped.
    let lam = tlab_alloc_lambda(&[4u8, 1u8], site_idx, 6).unwrap();
    unsafe {
        set_cell(lam.as_ptr(), AL_HDR_CELLS, target_old as i64); // ref capture
        set_cell(lam.as_ptr(), AL_HDR_CELLS + 1, target_old as i64); // long capture (bait)
    }
    assert!(is_arena_lambda(lam));
    assert_eq!(arena_lambda_kind(lam), 6);
    assert_eq!(arena_lambda_site_idx(lam), site_idx);
    assert_eq!(arena_lambda_cap_n(lam), 2);

    // Root ONLY the lambda; force a collection. `target` survives via the ref
    // capture; the unrooted-elsewhere long-bait must not resurrect anything.
    let mut roots: Vec<*mut u8> = vec![lam.as_ptr()];
    let (evac, _) = tlab_collect(&mut roots);
    assert!(evac >= 2, "lambda + its ref-captured target both evacuated ({evac})");
    let nlam = unsafe { ArenaPtr::new(roots[0]).unwrap() };
    assert!(is_arena_lambda(nlam), "class id survived the move");
    assert_eq!(arena_lambda_kind(nlam), 6, "kind cell intact");
    assert_eq!(arena_lambda_site_idx(nlam), site_idx, "site index intact");

    // The REF capture was forwarded to the target's new home; its data intact.
    let (ref_cell, ref_kind) = arena_lambda_cell(nlam, AL_HDR_CELLS);
    assert_eq!(ref_kind, 4, "ref capture declared as ref");
    assert_ne!(ref_cell as u64, target_old, "ref capture FORWARDED off the old addr");
    let ntarget = unsafe { ArenaPtr::new(ref_cell as *mut u8).unwrap() };
    assert_eq!(unsafe { cell(ntarget.as_ptr(), 0) }, 42, "captured target data survived");

    // The PRIM (long) capture bits are UNCHANGED — the collector skipped it (had it
    // been mis-traced as a ref it would now equal the forwarded target address).
    let (long_cell, long_kind) = arena_lambda_cell(nlam, AL_HDR_CELLS + 1);
    assert_eq!(long_kind, 1, "prim capture declared as long");
    assert_eq!(long_cell as u64, target_old, "long-bait bits NOT mis-forwarded");
}

/// The low-address arena invariant: every TLAB-allocated object sits below
/// 2^40, so `addr >> 40 == 0` soundly discriminates a raw `ArenaPtr` from a
/// high-word handle (whose tag occupies bits 40..43). Without the low mapping a
/// Windows heap pointer is ~2.4 TiB and aliases the tag region.
#[test]
fn arena_objects_live_below_2_pow_40() {
    let name: Arc<str> = Arc::from("LowAddrT");
    let layout: Arc<[(u32, u8)]> = vec![(1u32, 0u8), (2, 0)].into();
    let cid = register_class(&name, &layout);
    for _ in 0..64 {
        let p = tlab_try_alloc(cid, 2).unwrap();
        let a = p.addr();
        assert_eq!(a & 7, 0, "arena ptr must be 8-aligned (low 2 bits 0b00)");
        assert_eq!(a >> 40, 0, "arena ptr 0x{a:x} must be < 2^40 for the ABI discriminator");
    }
}

#[test]
fn region_view_aliases_owner_and_drop_is_noop() {
    // G3: a view Region bumps into memory OWNED by another Region; dropping the view must NOT
    // free the owner's mapping (else a fork worker's sub-range would unmap main's nursery).
    let owner = Region::new(4096);
    let sub_off = 512usize;
    let sub_cap = 1024usize;
    let sub_base = unsafe { owner.base().add(sub_off) };
    {
        let mut v = Region::view(sub_base, sub_cap);
        assert!(v.view, "view flag set");
        assert_eq!(v.cap(), sub_cap);
        // Bump + write through the view; the bytes land in the OWNER's mapping.
        let p = v.base();
        v.top += 8;
        unsafe { *(p as *mut u64) = 0xDEAD_BEEF_1234_5678 };
        // Same address seen from the owner's mapping (aliasing).
        assert_eq!(unsafe { *(owner.base().add(sub_off) as *const u64) }, 0xDEAD_BEEF_1234_5678);
    } // v dropped here — must be a no-op (no unmap of the owner's memory).
    // Owner still fully usable after the view dropped.
    unsafe { *(owner.base().add(sub_off) as *mut u64) = 0x1111_2222_3333_4444 };
    assert_eq!(unsafe { *(owner.base().add(sub_off) as *const u64) }, 0x1111_2222_3333_4444);
}

#[test]
fn worker_tlab_view_allocates_into_parent_nursery() {
    // G3: a worker whose TLAB is a VIEW over a parent nursery bump-allocates INTO that parent
    // memory, so objects persist in the parent heap after the worker is torn down.
    let owner = Region::new(1 << 16); // 64 KiB parent nursery
    let name: Arc<str> = Arc::from("VwT");
    let layout: Arc<[(u32, u8)]> = vec![(1u32, 0u8), (2, 0)].into();
    let cid = register_class(&name, &layout);
    worker_tlab_view(owner.base() as usize, owner.cap());
    let mut addrs = Vec::new();
    for _ in 0..100 {
        let p = tlab_try_alloc(cid, 2).expect("view TLAB alloc");
        assert!(owner.contains(p.as_ptr()), "object lands in the parent nursery memory");
        addrs.push(p.addr());
    }
    // Distinct, ascending bump addresses (disjoint objects in the parent).
    assert!(addrs.windows(2).all(|w| w[1] > w[0]), "bump allocation is monotonic");
    worker_tlab_clear(); // drop the view so it can't be reused after `owner` frees.
}

#[test]
fn tlab_alloc_then_field_read_write() {
    // Class "T" with 3 fields (ids 10/11/12, all int kind 0).
    let name: Arc<str> = Arc::from("LabT");
    let layout: Arc<[(u32, u8)]> = vec![(10u32, 0u8), (11, 0), (12, 0)].into();
    let cid = register_class(&name, &layout);

    let p = tlab_try_alloc(cid, 3).unwrap();
    assert_eq!(arena_class_id(p), cid, "class id in header");
    assert_eq!(arena_nfields(p), 3, "nfields in header");
    // default-zeroed
    for s in 0..3 {
        assert_eq!(arena_cell(p, s), 0);
    }
    // write field 11 (by id→slot), read back
    let s11 = slot_of(cid, 11).expect("field 11 in layout");
    arena_set_cell(p, s11, 4242);
    assert_eq!(arena_cell(p, slot_of(cid, 11).unwrap()), 4242);
    // other fields untouched
    assert_eq!(arena_cell(p, slot_of(cid, 10).unwrap()), 0);
}

#[test]
fn tlab_objects_are_distinct_and_independent() {
    let name: Arc<str> = Arc::from("LabU");
    let layout: Arc<[(u32, u8)]> = vec![(20u32, 0u8), (21, 1)].into(); // int, long
    let cid = register_class(&name, &layout);
    let a = tlab_try_alloc(cid, 2).unwrap();
    let b = tlab_try_alloc(cid, 2).unwrap();
    assert_ne!(a.addr(), b.addr(), "distinct bump addresses");
    arena_set_cell(a, 0, 111);
    arena_set_cell(b, 0, 222);
    assert_eq!(arena_cell(a, 0), 111);
    assert_eq!(arena_cell(b, 0), 222); // no aliasing
}

#[test]
fn cyclic_graph_survives_minor_gc_with_memory_shift() {
    // ⛔**FLAKY WITHOUT THIS LOCK.** Both this test and `chain_survives_repeated_gc` failed under
    // the default parallel `cargo test` and passed with `--test-threads=1` — they build their own
    // `GenHeap` but still share process-wide arena state with the other heap tests. A suite that
    // fails nondeterministically is not a veto, and every gate decision in this ledger treats it
    // as one.
    let _gil = PINNED_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // class: 1 ref field (cell 0 is a pointer → ref_mask bit 0).
    let shapes = vec![ClassShape { nfields: 1, ref_mask: 1 }];
    let mut h = GenHeap::new(64 * 1024, 1024 * 1024, shapes);
    let mut none: [*mut u8; 0] = [];
    let a = h.alloc(0, &mut none);
    let b = h.alloc(0, &mut none);
    let c = h.alloc(0, &mut none); // DEAD: never rooted, never linked
    unsafe {
        set_cell(a, 0, b as i64); // a.next = b
        set_cell(b, 0, a as i64); // b.next = a  → 2-cycle
        set_cell(c, 0, 0);
    }
    assert!(h.young.contains(a) && h.young.contains(b));
    let (a_old, c_old) = (a, c);

    // Force the moving collection with root = {a}; addresses SHIFT.
    let mut roots = [a];
    h.minor_gc(&mut roots);
    let a2 = roots[0];

    // a evacuated to tenured, moved.
    assert!(h.old.contains(a2), "a evacuated to tenured");
    assert_ne!(a2, a_old, "a's address shifted");
    // a.next → forwarded b (in tenured); b.next → forwarded a (== a2): CYCLE INTACT.
    let b2 = unsafe { cell(a2, 0) } as usize as *mut u8;
    assert!(h.old.contains(b2), "b evacuated");
    let a_via_b = unsafe { cell(b2, 0) } as usize as *mut u8;
    assert_eq!(a_via_b, a2, "cycle preserved after pointer rewrite");
    // exactly the 2 live objects copied; c (dead) collected.
    assert_eq!(h.copied, 2, "only the live cycle evacuated; dead object reclaimed");
    let _ = c_old;
    // nursery reclaimed in O(1).
    assert_eq!(h.young.top, 0, "nursery reset after evacuation");
}

#[test]
fn chain_survives_repeated_gc() {
    // Same process-wide arena state as its sibling above; see the note there.
    let _gil = PINNED_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // Build a 5-deep chain a0→a1→…→a4, root a0, GC twice; chain intact + values.
    let shapes = vec![ClassShape { nfields: 2, ref_mask: 0b10 }]; // cell0=int, cell1=next
    let mut h = GenHeap::new(64 * 1024, 1024 * 1024, shapes);
    let mut none: [*mut u8; 0] = [];
    let mut prev: *mut u8 = std::ptr::null_mut();
    let mut head = std::ptr::null_mut();
    for i in 0..5 {
        let n = h.alloc(0, &mut none);
        unsafe {
            set_cell(n, 0, i as i64);
            set_cell(n, 1, prev as i64);
        }
        prev = n;
        head = n;
    }
    let mut roots = [head];
    h.minor_gc(&mut roots);
    h.minor_gc(&mut roots); // 2nd GC: already-tenured, no further copy
    // walk the chain from the (forwarded) head, check 5 nodes + values 4,3,2,1,0
    let mut p = roots[0];
    let mut seen = Vec::new();
    while !p.is_null() {
        seen.push(unsafe { cell(p, 0) });
        p = unsafe { cell(p, 1) } as usize as *mut u8;
    }
    assert_eq!(seen, vec![4, 3, 2, 1, 0], "chain + field values survived 2 GCs");
}

#[test]
fn live_tlab_collect_evacuates_and_preserves_cycle() {
    // Through the LIVE thread-local TLAB path: alloc a 2-cycle + a dead object,
    // force tlab_collect, verify evacuation + cycle survival + dead reclaim.
    let name: Arc<str> = Arc::from("LiveCycleT");
    let layout: Arc<[(u32, u8)]> = vec![(700u32, 6u8)].into(); // kind 6 = ref → traced
    let cid = register_class(&name, &layout);
    assert!(ref_bit(&ref_mask_of(cid), 0) && !ref_bit(&ref_mask_of(cid), 1), "ref field traced");

    let a = tlab_try_alloc(cid, 1).unwrap();
    let b = tlab_try_alloc(cid, 1).unwrap();
    let _dead = tlab_try_alloc(cid, 1).unwrap(); // never rooted/linked
    arena_set_cell(a, 0, b.addr() as i64); // a.next = b
    arena_set_cell(b, 0, a.addr() as i64); // b.next = a → cycle
    assert!(tlab_in_nursery(a) && tlab_in_nursery(b));

    let mut roots = [a.as_ptr()];
    let (evac, _tenured) = tlab_collect(&mut roots);
    let a2 = unsafe { ArenaPtr::from_addr(roots[0] as u64) };
    assert!(!tlab_in_nursery(a2), "a evacuated out of the nursery");
    // a.next → forwarded b; b.next → forwarded a (== a2): cycle intact.
    let b2 = unsafe { ArenaPtr::from_addr(arena_cell(a2, 0) as u64) };
    assert!(!tlab_in_nursery(b2), "b evacuated");
    assert_eq!(arena_cell(b2, 0) as u64, a2.addr(), "cycle preserved through tlab_collect");
    assert_eq!(evac, 2, "only the 2 live objects evacuated; dead reclaimed");
}

#[test]
fn identity_hash_lazy_stamp_is_stable_and_gc_survivable() {
    // Lazily stamp an identity hash, prove it: is non-zero, re-reads equal,
    // differs across distinct objects, and SURVIVES a moving collection (the
    // header hash word rides along in the Cheney copy).
    let name: Arc<str> = Arc::from("IdHashT");
    let layout: Arc<[(u32, u8)]> = vec![(800u32, 6u8)].into(); // 1 ref field → traced
    let cid = register_class(&name, &layout);

    let a = tlab_try_alloc(cid, 1).unwrap();
    let b = tlab_try_alloc(cid, 1).unwrap();
    let ha = arena_identity_hash(a);
    let hb = arena_identity_hash(b);
    assert_ne!(ha, 0, "stamped hash is non-zero");
    assert_ne!(hb, 0, "stamped hash is non-zero");
    assert!(ha >= 0 && hb >= 0, "hash is non-negative (RegistryID slot index)");
    assert_ne!(ha, hb, "distinct objects get distinct hashes");
    assert_eq!(arena_identity_hash(a), ha, "re-read returns the same stamp");

    // Root `a`, collect → it moves to tenured; the stamp must follow.
    let mut roots = [a.as_ptr()];
    let _ = tlab_collect(&mut roots);
    let a2 = unsafe { ArenaPtr::from_addr(roots[0] as u64) };
    assert!(!tlab_in_nursery(a2), "a evacuated");
    assert_eq!(arena_identity_hash(a2), ha, "identity hash stable across GC move");
}

#[test]
fn arena_handle_roots_reports_tagged_handles() {
    // A handle (array/ref) stored in an arena ref-cell must surface as a GC
    // root so the heap mark-sweep keeps it alive — else it dangles (the
    // RegistryID-backing-arrays bug).
    let name: Arc<str> = Arc::from("HandleHolderT");
    let layout: Arc<[(u32, u8)]> = vec![(900u32, 6u8), (901, 6)].into(); // 2 ref cells
    let cid = register_class(&name, &layout);
    let p = tlab_try_alloc(cid, 2).unwrap();
    // cell0 = array handle 1234 (tag 0b10), cell1 = ref handle 5678 (tag 0b01).
    arena_set_cell(p, 0, ((1234i64) << 2) | 2);
    arena_set_cell(p, 1, ((5678i64) << 2) | 1);

    let mut roots = Vec::new();
    arena_handle_roots(&mut roots);
    assert!(
        roots.iter().any(|v| matches!(v, crate::value::Value::Array(1234))),
        "array handle from arena cell is a root: {roots:?}"
    );
    assert!(
        roots.iter().any(|v| matches!(v, crate::value::Value::Ref(5678))),
        "ref handle from arena cell is a root: {roots:?}"
    );
}

/// The uniform-refs contract, stated as the trace loops actually consume it.
///
/// An array class has an EMPTY mask on purpose, and every mask-driven loop begins with a
/// `mask_empty` early-out. If `uniform` is not checked FIRST, that early-out skips the
/// object entirely and every element of every reference array is silently dropped — not a
/// wrong number, a dangling reference. This asserts the exact conjunction the loops use,
/// so a future refactor that reorders them fails here instead of in a world save.
/// The uniform-refs constructor is safe once every mask-driven walk consumes the paired
/// `(mask, uniform)` contract. An array class has an EMPTY mask on purpose; `uniform` is the
/// fact that makes every element a traced cell.
#[test]
fn uniform_ref_class_marks_every_cell() {
    let cid = arena_array_class(true);
    let (mask, uniform) = ref_mask_and_uniform(cid);
    assert!(uniform, "reference arrays use the uniform trace shape");
    assert!(mask_empty(&mask), "uniform arrays keep an empty per-slot mask");
    assert!(class_all_refs(cid), "all reference-array cells must be traced");
}

/// A primitive-element array class needs no uniform flag and is reachable today: an empty
/// layout already yields an empty mask, and every walk correctly skips it.
#[test]
fn primitive_array_class_is_never_traced() {
    let pid = arena_array_class(false);
    let (pmask, puniform) = ref_mask_and_uniform(pid);
    assert!(!puniform && mask_empty(&pmask), "primitive arrays are never traced");
    assert_eq!(pid, arena_array_class(false), "one class per element kind");
}

#[test]
fn register_class_is_stable() {
    let name: Arc<str> = Arc::from("LabStable");
    let layout: Arc<[(u32, u8)]> = vec![(99u32, 0u8)].into();
    let id1 = register_class(&name, &layout);
    let id2 = register_class(&name, &layout); // same name → same id
    assert_eq!(id1, id2);
    assert_eq!(&*class_name(id1).unwrap(), "LabStable");
}
