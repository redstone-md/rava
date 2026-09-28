//! Escape-analysis and cell-ABI tests.

use super::*;

#[cfg(feature = "next-gen-gc")]
#[test]
fn analyze_escape_distinguishes_confined_from_escaping_allocations() {
    let pool = escape_pool();
    // Collapse the richer verdict back to the original confined-vs-escaped boolean
    // for these cases (Confined = false; ControlFlow / Sink = escaped = true).
    let escapes = |bc: &[u8]| {
        analyze_escape(bc, &pool)
            .first()
            .map(|&(_, e)| e != EscapeReason::Confined)
    };

    // NON-escaping: created, constructed, field read, dropped.
    // new #2; dup; invokespecial #6; getfield #10; ireturn
    assert_eq!(
        escapes(&[0xbb, 0x00, 0x02, 0x59, 0xb7, 0x00, 0x06, 0xb4, 0x00, 0x0a, 0xac]),
        Some(false),
        "confined object → SROA candidate"
    );
    // NON-escaping: created, constructed, discarded.
    assert_eq!(escapes(&[0xbb, 0x00, 0x02, 0x59, 0xb7, 0x00, 0x06, 0x57, 0xb1]), Some(false));

    // ESCAPES via areturn (returned to caller).
    assert_eq!(escapes(&[0xbb, 0x00, 0x02, 0x59, 0xb7, 0x00, 0x06, 0xb0]), Some(true), "areturn escapes");
    // ESCAPES via putstatic (stored in a static).
    assert_eq!(
        escapes(&[0xbb, 0x00, 0x02, 0x59, 0xb7, 0x00, 0x06, 0xb3, 0x00, 0x0a, 0xb1]),
        Some(true),
        "putstatic escapes"
    );
    // ESCAPES via a call argument (passed OUT to invokestatic).
    assert_eq!(
        escapes(&[0xbb, 0x00, 0x02, 0x59, 0xb7, 0x00, 0x06, 0xb8, 0x00, 0x0e, 0xb1]),
        Some(true),
        "passed to a call escapes"
    );
    // ESCAPES via stored into a local then NOT confined (astore_1; aload_1; areturn).
    assert_eq!(
        escapes(&[0xbb, 0x00, 0x02, 0x59, 0xb7, 0x00, 0x06, 0x4c, 0x2b, 0xb0]),
        Some(true),
        "stored to a local then returned escapes"
    );
    // CONTROL FLOW present → conservative escape for every site.
    // new #2; dup; invokespecial #6; ifnull +3; pop; return
    assert_eq!(
        escapes(&[0xbb, 0x00, 0x02, 0x59, 0xb7, 0x00, 0x06, 0xc6, 0x00, 0x03, 0x57, 0xb1]),
        Some(true),
        "a branch → conservative escape (taint merges untracked)"
    );
}

#[cfg(feature = "next-gen-gc")]
#[test]
fn analyze_escape_cfg_tracks_control_flow() {
    let pool = escape_pool();
    let cf = |bc: &[u8]| analyze_escape_cfg(bc, &pool, false).first().map(|&(_, e)| e);
    // CONFINED across a branch (straight-line bails to ControlFlow; the CFG detector
    // proves it): new; dup; <init>; astore_1; iconst_0; ifeq +8; aload_1; getfield; pop; return.
    assert_eq!(
        cf(&[0xbb, 0x00, 0x02, 0x59, 0xb7, 0x00, 0x06, 0x4c, 0x03, 0x99, 0x00, 0x08, 0x2b, 0xb4, 0x00, 0x0a, 0x57, 0xb1]),
        Some(EscapeReason::Confined),
        "confined across a branch"
    );
    // ESCAPES across a branch: same but aload_1 is passed to a call (invokestatic #14).
    assert_eq!(
        cf(&[0xbb, 0x00, 0x02, 0x59, 0xb7, 0x00, 0x06, 0x4c, 0x03, 0x99, 0x00, 0x08, 0x2b, 0xb8, 0x00, 0x0e, 0x57, 0xb1]),
        Some(EscapeReason::Sink),
        "passed to a call across a branch escapes"
    );
    // LOOP-CARRIED: L: new; astore_1; goto L → the ref is alive at the back-edge.
    assert_eq!(
        cf(&[0xbb, 0x00, 0x02, 0x4c, 0xa7, 0xff, 0xfc]),
        Some(EscapeReason::Sink),
        "loop-carried allocation escapes"
    );
    // EXCEPTION GATE: any exception-table entry → the whole method is unfit.
    assert_eq!(
        analyze_escape_cfg(&[0xbb, 0x00, 0x02, 0x59, 0xb7, 0x00, 0x06, 0xb4, 0x00, 0x0a, 0xb1], &pool, true)
            .first()
            .map(|&(_, e)| e),
        Some(EscapeReason::Sink),
        "exception table gates the whole method"
    );
}

#[test]
fn jit_cell_roots_preserve_heap_kind() {
    match cell_to_root(pack_local(&JVal::Ref(7))) {
        Some(JVal::Ref(7)) => {}
        other => panic!("object root decoded incorrectly: {other:?}"),
    }
    match cell_to_root(pack_local(&JVal::Array(9))) {
        Some(JVal::Array(9)) => {}
        other => panic!("array root decoded incorrectly: {other:?}"),
    }
    assert!(cell_to_root(0).is_none());
    assert!(cell_to_root(BOX_TAG | 3).is_none());
}

/// Unified low-2-bit cell ABI (`jit_abi`) — the foundation for Task 1's ATOMIC
/// ref-ABI flip. These post-flip semantics are address-independent: a raw
/// `ArenaPtr` (8-aligned → low 2 bits 0b00) and a handle `(h<<2)|01` are told
/// apart by `cell & 3`, which is the ONLY sound discriminator (the high-bit
/// scheme fails because Windows heap pointers occupy bits 40..46 — see the
/// tracer write-up in `NEXTGEN_GC_CHECKPOINT.md`). Uses a heap allocation so
/// the pointer is a real (high-address) arena-style pointer, not a low stack
/// address that would mask the discriminator.
#[cfg(feature = "next-gen-gc")]
#[test]
fn jit_abi_unified_cells_are_address_independent() {
    let backing = vec![0u8; 64]; // heap → high address on Windows
    let p = crate::arena::ArenaPtr::new(backing.as_ptr() as *mut u8).unwrap();
    let addr = p.addr();
    assert_eq!(addr & 0b11, 0, "arena ptr must be 8-aligned (low 2 bits 0b00)");

    // Tag layout: Ref=01, Array=10, ArenaPtr=raw(00).
    assert_eq!(jit_abi::unify_pack(&JVal::Ref(7)), (7 << 2) | 0b01);
    assert_eq!(jit_abi::unify_pack(&JVal::Array(9)), (9 << 2) | 0b10);
    assert_eq!(jit_abi::unify_pack(&JVal::Obj(p)), addr as i64);

    // `cell & 3` separates ptr from handle REGARDLESS of address (the whole
    // point — this holds even though `addr >> 40 != 0` here on Windows).
    assert_eq!(jit_abi::unify_untag(addr as i64), (0b00, addr as i64));
    assert_eq!(jit_abi::unify_untag((7 << 2) | 0b01), (0b01, 7));
    assert_eq!(jit_abi::unify_untag((9 << 2) | 0b10), (0b10, 9));

    // GC root decode: handles + non-null ptr are roots; null + boxed are not.
    assert!(matches!(jit_abi::unify_cell_to_root((7 << 2) | 0b01), Some(JVal::Ref(7))));
    assert!(matches!(jit_abi::unify_cell_to_root((9 << 2) | 0b10), Some(JVal::Array(9))));
    match jit_abi::unify_cell_to_root(addr as i64) {
        Some(JVal::Obj(q)) => assert_eq!(q.addr(), addr),
        other => panic!("arena ptr must decode as a root: {other:?}"),
    }
    assert!(jit_abi::unify_cell_to_root(0).is_none());
    assert!(jit_abi::unify_cell_to_root((3 << 2) | 0b11).is_none()); // boxed Str
}
