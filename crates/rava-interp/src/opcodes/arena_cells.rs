//! Reading and writing arena objects through the tagged cell encoding.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::opcodes::*;

/// Encode a `Value` into an arena inline cell (Phase 2). Primitives pack directly;
/// an arena→arena reference stores the raw pointer; null is 0. Cross-generation
/// refs (arena→handle) and Str/sentinels PANIC predictably — they need Phase 3
/// (moving GC + hybrid pointer management) / Phase 5 (box-table migration).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn encode_arena_cell(v: &Value) -> i64 {
    match v {
        Value::Int(i) => *i as i64,
        Value::Long(l) => *l,
        Value::Float(f) => f.to_bits() as u32 as i64,
        Value::Double(d) => d.to_bits() as i64,
        Value::Null => 0,
        // Arena pointers are 8-byte aligned (header + i64 cells), so the low 3
        // bits are always 0. That lets us TAG cross-generation handle refs in the
        // same cell: low 2 bits 0b01 = a heap handle (Value::Ref), 0b10 = an array
        // handle (Value::Array). The moving GC skips any cell with non-zero low
        // bits (it's a handle, not an arena pointer to forward). Untagged non-zero
        // = a real arena→arena pointer.
        Value::Obj(p) => p.addr() as i64,
        // B5 PRODUCTION FLIP (RAVA_PINNED_PTRS, default OFF): a handle ref whose
        // object HAS a pinned mirror stores as the raw mirror ADDRESS — the
        // unified untagged pointer form. Readers decode Obj(pinned); the identity/
        // liveness/hash/map bridges make it the same object as Ref(h). A handle
        // without a mirror keeps the tagged form (coexistence).
        Value::Ref(h) => {
            #[cfg(feature = "next-gen-gc")]
            if crate::nextgen::pinned_ptrs_enabled() {
                let m = crate::nextgen::pinned_mirror_get(*h);
                if m != 0 {
                    return m as i64;
                }
            }
            ((*h as i64) << 2) | 1
        }
        Value::Array(a) => ((*a as i64) << 2) | 2,
        // STEP 1 BOX-ARM RETIREMENT (RAVA_ARENA_STR_REFCELL, default OFF): a String
        // in a ref cell is BORN as an arena-STRING object — a raw 0b00 pointer,
        // decoded with zero side-table lookup (is_arena_obj_addr → Value::Obj, which
        // every is_arena_string reader treats as a String). Single-store: no box
        // index, no dual write. tlab_alloc_string NEVER collects (returns None on a
        // full nursery), so the receiver `p` at the putfield callsite cannot move.
        // On a full nursery, fall through to the 0b11 box (the coexistence fallback).
        Value::Str(s) => {
            if crate::nextgen::arena_str_refcell_enabled() {
                if let Some(p) = crate::nextgen::tlab_alloc_string(s.as_bytes()) {
                    return p.addr() as i64;
                }
                // NURSERY FULL — the one path that kept minting 0b11 cells for strings,
                // and the reason this arm could never reach zero. `tlab_alloc_string`
                // must not collect (the receiver at the putfield callsite is a raw
                // pointer that a collection would move), so instead place the string in
                // the PERMANENT pool: still a 0b00 arena string, GC-immune, and
                // content-deduped. Dedup is invisible because arena strings compare by
                // CONTENT (see `ref_equal`), which is what `Value::Str` already did.
                let p = crate::nextgen::intern_arena_string(s);
                if p != 0 {
                    return p;
                }
            }
            ((crate::nextgen::arena_box_push(v.clone()) as i64) << 2) | 3
        }
        // StdOut / stderr sentinels can't pack into an i64 and have no arena form —
        // box them into the arena side table (tag 0b11). The GC skips tagged cells.
        Value::StdOut | Value::StdErr => {
            ((crate::nextgen::arena_box_push(v.clone()) as i64) << 2) | 3
        }
    }
}

/// Encode a value for a reference-kind arena cell.
///
/// Boxed Java primitives are represented by flat `Value` variants at the Rust boundary. In a
/// reference cell their raw bits would alias the untagged arena-pointer form, so they must use
/// the side table just like the existing `arena_putfield` path.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn encode_arena_ref_cell(v: &Value) -> i64 {
    if matches!(
        v,
        Value::Int(_) | Value::Long(_) | Value::Float(_) | Value::Double(_)
    ) {
        ((crate::nextgen::arena_box_push(v.clone()) as i64) << 2) | 3
    } else {
        encode_arena_cell(v)
    }
}

/// Encode and store one reference-kind cell, including the old→young shadow barrier.
///
/// The barrier deliberately consumes the encoded cell.  `Value::Str` and a pinned `Value::Ref`
/// can become an untagged arena pointer during encoding even though the input value is not
/// `Value::Obj`; testing the Rust value before encoding is therefore incomplete.
#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn store_arena_ref_cell(p: crate::arena::ArenaPtr, slot: usize, value: &Value) {
    store_arena_ref_cell_encoded(p, slot, encode_arena_ref_cell(value));
}

/// Store an already encoded reference cell and preserve its old→young barrier.
///
/// The lane transaction pre-encodes every value before mutating the first
/// destination; this entry point lets it commit without allocating or reboxing.
#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn store_arena_ref_cell_encoded(p: crate::arena::ArenaPtr, slot: usize, cell: i64) {
    if crate::nextgen::shadow_enabled() && !crate::nextgen::tlab_in_nursery(p) {
        let raw = cell as u64;
        if cell != 0
            && raw & 0b11 == 0
            && crate::nextgen::is_arena_obj_addr(raw)
            && crate::nextgen::tlab_in_nursery(unsafe { crate::arena::ArenaPtr::from_addr(raw) })
        {
            crate::nextgen::shadow().card_mark((p.addr() >> 9) as u32);
        }
    }
    crate::nextgen::arena_set_cell(p, slot, cell);
}

/// Decode an arena inline cell back to a `Value` using the field's layout kind
/// (primitive) or the cell (ref-ish: arena ptr / null). Mirrors `encode_arena_cell`.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn decode_arena_cell(cell: i64, kind: u8) -> Value {
    match kind {
        0 => Value::Int(cell as i32),                   // FK_INT (also Z/B/C/S)
        1 => Value::Long(cell),                         // FK_LONG
        2 => Value::Float(f32::from_bits(cell as u32)), // FK_FLOAT
        3 => Value::Double(f64::from_bits(cell as u64)), // FK_DOUBLE
        _ => {
            // ref-ish (layout kind 6 for L/[): null, a tagged cross-gen handle
            // (low 2 bits 0b01=Ref, 0b10=Array — see encode), or an untagged
            // arena→arena pointer (low bits 0).
            if cell == 0 {
                Value::Null
            } else {
                match cell & 3 {
                    1 => Value::Ref((cell >> 2) as u32),
                    2 => Value::Array((cell >> 2) as u32),
                    3 => crate::nextgen::arena_box_get((cell >> 2) as usize),
                    // An untagged arena→arena pointer — but ONLY if it is a real arena
                    // address. A ref-kind cell holding a stray small value (e.g. `4`,
                    // low bits 0) is NOT a pointer; making it `Obj(4)` hands every
                    // downstream `is_arena_*`/getfield a wild deref. Such a cell is
                    // corrupt/uninitialised → null (the ref default), never dereferenced.
                    _ => {
                        let a = cell as u64;
                        // B5: a PINNED mirror address materializes as Ref(h) at the
                        // Rust boundary (natives/dispatch stay handle-based during
                        // coexistence); compiled code keeps the raw pointer form.
                        if let Some(h) = crate::nextgen::pinned_handle_of(a) {
                            return Value::Ref(h);
                        }
                        if crate::nextgen::is_arena_obj_addr(a) {
                            Value::Obj(unsafe { crate::arena::ArenaPtr::from_addr(a) })
                        } else {
                            if crate::env_flag!("RAVA_TRACE_BADCELL") {
                                eprintln!(
                                    "[BADCELL] ref cell={cell:#x} not arena/handle/null → Null"
                                );
                            }
                            Value::Null
                        }
                    }
                }
            }
        }
    }
}

#[cfg(feature = "next-gen-gc")]
thread_local! {
    /// PER-CALL-SITE arena-lambda shape memo. Keyed exactly like the resolved indy
    /// plan (`tlcache::indy_put`) — `(class pointer, constant-pool index)` — so it
    /// inherits that cache's lifetime assumption and introduces no new one. Thread
    /// local because a gil-free worker never mints an arena lambda (worker exclusion),
    /// so there is no cross-thread sharing to coordinate.
    pub(crate) static ARENA_LAMBDA_SHAPES: std::cell::RefCell<
        std::collections::HashMap<(usize, u16), std::rc::Rc<crate::nextgen::ArenaLambdaShape>>,
    > = std::cell::RefCell::new(std::collections::HashMap::new());
}

/// The memoized arena shape for invokedynamic call site `site`. Everything it holds is
/// derived from the site's STATIC descriptor and impl method, so one resolution serves
/// every instantiation. See `nextgen::ArenaLambdaShape` for what deriving it costs.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn arena_lambda_shape(
    site: (usize, u16),
    desc: &str,
    impl_site: &std::sync::Arc<crate::heap::LambdaSite>,
) -> std::rc::Rc<crate::nextgen::ArenaLambdaShape> {
    if let Some(hit) = ARENA_LAMBDA_SHAPES.with(|m| m.borrow().get(&site).cloned()) {
        return hit;
    }
    let cap_kinds: Vec<u8> = concat_param_types(desc)
        .iter()
        .map(|&b| match b {
            b'J' => 1u8,
            b'F' => 2,
            b'D' => 3,
            b'L' | b'[' => 4,
            _ => 0, // I/S/B/C/Z
        })
        .collect();
    // The functional-interface type is the indy descriptor's RETURN type. A DEFAULT
    // method declared on it — not the SAM — must dispatch to its real bytecode, which
    // is what `$fi` records (e.g. FloatModifier$Simple.argumentCodec() → Codec.FLOAT).
    let fi = desc
        .rsplit(')')
        .next()
        .and_then(|r| r.strip_prefix('L').and_then(|s| s.strip_suffix(';')))
        .unwrap_or("");
    // An EMPTY `fi` produces a lambda that can never answer a type test: `instance_of`'s
    // arena arm then falls back to `java/lang/Object` alone, and a sealed pattern switch
    // over it matches nothing (the `EnvironmentAttributeLayer` MatchException). Name the
    // descriptors that do it instead of guessing which they are.
    if fi.is_empty() && crate::env_flag!("RAVA_TRACE_NOFI") {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        if N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 12 {
            eprintln!(
                "[NO-FI] desc={desc} impl={}.{}",
                impl_site.owner, impl_site.name
            );
        }
    }
    let shape = std::rc::Rc::new(crate::nextgen::ArenaLambdaShape::resolve(
        &cap_kinds, impl_site, fi,
    ));
    ARENA_LAMBDA_SHAPES.with(|m| m.borrow_mut().insert(site, shape.clone()));
    shape
}

/// The functional-interface class name ($fi cell) of an arena-lambda, or `Value::Null`.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn arena_lambda_fi(p: crate::arena::ArenaPtr) -> Value {
    let (c, k) = crate::nextgen::arena_lambda_cell(p, crate::nextgen::ARENA_LAMBDA_FI_SLOT);
    let v = decode_arena_cell(c, k);
    // THE FUNCTIONAL INTERFACE IS STORED AS AN ARENA STRING, AND EVERY CONSUMER MATCHES
    // `Value::Str`. `ArenaLambdaShape::resolve` writes it through `stable_string_cell`,
    // which prefers the permanent interned arena-string pool — a raw 0b00 pointer — so
    // `decode_arena_cell` hands back `Value::Obj(ArenaPtr)` and not a string. Measured
    // with both ends printed: `fi_cell=0x280000238` written, `cell=0x280000338 kind=4
    // decoded=Obj(ArenaPtr)` read. Every reader (`instance_of`'s arena arm, the SAM
    // dispatch's default-method lookup) then takes its "no $fi" branch, so the lambda
    // answers only `java/lang/Object` and a sealed pattern switch over it matches
    // nothing — the `EnvironmentAttributeLayer` MatchException under RAVA_ARENA_LAMBDA.
    // Normalise here, once, rather than teaching every consumer both encodings.
    // ⛔**THIS BUILT THE SAME STRING FROM SCRATCH ON EVERY CALL — TWICE.**
    // `from_utf8_lossy(..).into_owned()` allocates a `String`, and `.into()` allocates the
    // `Arc<str>` behind `Value::Str`. The functional-interface name of a lambda site is a
    // CONSTANT, and this runs on every SAM dispatch and every lambda type test: a
    // caller-attributed profile of the datapack phase (the block running 5.3× slower than
    // `java -Xint`) put this function at **23.2% of all allocation in the phase**, second only
    // to the interner L1 that had the same defect.
    //
    // Memoised on the arena pointer, which is a sound key here and only here: the comment above
    // records that the value is written through `stable_string_cell`, which prefers the
    // **permanent, GC-immune interned arena-string pool** — those pointers are never reused, so
    // there is nothing to verify against. A pointer that is NOT from that pool simply misses and
    // takes the old path.
    #[cfg(feature = "next-gen-gc")]
    let v = match v {
        Value::Obj(sp) if crate::nextgen::is_arena_string(sp) => Value::Str(fi_name(sp)),
        other => other,
    };
    // RAVA_TRACE_FICELL: the READ side of the pair. The write side prints the same cell
    // at birth; the two numbers next to each other are the whole question — a lambda
    // whose `$fi` reads back as anything but a `Str` cannot answer a type test, and a
    // sealed pattern switch over it matches nothing.
    if !matches!(v, Value::Str(_)) && crate::env_flag!("RAVA_TRACE_FICELL") {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        if N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 8 {
            eprintln!(
                "[FICELL-READ] cid={} cell={c:#x} kind={k} decoded={v:?}",
                crate::nextgen::arena_class_id(p)
            );
        }
    }
    v
}

#[cfg(feature = "next-gen-gc")]
/// `RAVA_STALE_STAT` — the population the BARE-DEREF FLIP has to survive.
///
/// Ledger rows 319/320: `RAVA_JIT_BARE_REF_LOAD=1` kills the boot 29.1 s in, the LOAD half alone
/// is fatal, and the arc's forensic (`RAVA_PROBE_STALE`) is disabled BY the flip because it fires
/// where `arena_getfield` NOTICES a stale receiver — the very check the flip removes. Row 320
/// prescribed counting on the HOST side instead; this is that count, and it runs with the flip
/// OFF, on a boot that works.
///
/// `RECOVERED` is a receiver that was ALREADY stale and was silently repaired by the forwarding
/// word. Those calls are exactly the ones a bare load would have dereferenced into evacuated
/// space. `OK` is the same path taken with no repair needed, so the two give a RATE.
///
/// ⚠It counts only what the layout test NOTICES. Evacuated memory keeps its old header until the
/// space is reused, so a stale receiver whose class still has the field reads stale data and is
/// counted as `OK` — this is a LOWER BOUND, and the fact that it is one is the point.
#[cfg(feature = "next-gen-gc")]
pub(crate) static STALE_RECOVERED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
#[cfg(feature = "next-gen-gc")]
pub(crate) static STALE_OK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// ROUND-7 ARENA-CELL FAST PATH. `RAVA_NO_FAST_ARENACELL=1` restores the pre-round shape at
/// every site below (paired `slot_of`/`kind_of` lookups, and the synthetic accessors re-proving
/// a slot the caller already proved). Also honours `RAVA_NO_EMIT_MORE`.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn fast_arenacell() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        crate::flags::round7()
            && std::env::var_os("RAVA_NO_FAST_ARENACELL").is_none()
            && !crate::flags::emit_more_disabled()
    })
}

/// Slot AND kind of `(class, field)` in ONE probe of the per-class field index.
///
/// WAS: every arena field access derived the two halves of one 8-byte layout-index entry
/// SEPARATELY — `slot_of` then `kind_of` — so it paid two `fast_layout()` gate loads and two
/// thread-local class-mirror lookups, and with the index refused (empty `fx`) two linear scans
/// of O(fields per class). IS: one gate load, one mirror lookup, one probe. The pair is a
/// constant of the class; nothing about it varies per object or per access.
#[cfg(feature = "next-gen-gc")]
#[inline]
fn slot_kind(class_id: u32, field_id: u32) -> Option<(usize, u8)> {
    if fast_arenacell() {
        return crate::nextgen::slot_kind_of(class_id, field_id);
    }
    // Kill-switch arm: the pre-round pair, `unwrap_or(0)` included (a slot that resolved always
    // has a kind, so the default is unreachable — it is kept only to stay byte-identical).
    let s = crate::nextgen::slot_of(class_id, field_id)?;
    Some((s, crate::nextgen::kind_of(class_id, field_id).unwrap_or(0)))
}

/// THE THREE DIAGNOSTIC GATES OF THE ARENA READ PATH, READ ONCE FOR THE PROCESS.
///
/// WAS: three independent `OnceLock` acquire loads spread through `arena_getfield` — `stale_stat`
/// on the resolve, `NULLREF_ON` on a null ref cell, and `STALEREAD_ON` on every NON-null ref cell,
/// i.e. on the common case of every reference field read. All three are process constants, and an
/// atomic load is exactly the thing LLVM may not hoist out of the caller, so each access paid all
/// of them. IS: one load of one record whose `trace` field is false on every default boot, so the
/// entire diagnostic tail collapses to a single perfectly-predicted branch.
///
/// SIZE: 4 bytes of `bool`, one `OnceLock<GfGates>` for the process — not per class, thread or
/// object. (`env_flag!`/`OnceLock`-per-site is the shape this replaces; see the note in
/// `flatbody.rs` that cites `NULLREF_ON` for why the env read itself may never be on this path.)
#[cfg(feature = "next-gen-gc")]
#[derive(Copy, Clone)]
struct GfGates {
    /// RAVA_STALE_STAT — count ok/recovered receivers.
    stale: bool,
    /// RAVA_TRACE_NULLREF — a ref field reading a zero cell.
    nullref: bool,
    /// RAVA_TRACE_STALEREAD — a ref field reading a stale young pointer.
    staleread: bool,
    /// `nullref | staleread`: the one test the read tail actually branches on.
    trace: bool,
}

#[cfg(feature = "next-gen-gc")]
#[inline]
fn gf_gates() -> GfGates {
    static G: std::sync::OnceLock<GfGates> = std::sync::OnceLock::new();
    *G.get_or_init(|| {
        let stale = std::env::var_os("RAVA_STALE_STAT").is_some();
        let nullref = std::env::var_os("RAVA_TRACE_NULLREF").is_some();
        let staleread = std::env::var_os("RAVA_TRACE_STALEREAD").is_some();
        GfGates {
            stale,
            nullref,
            staleread,
            trace: nullref || staleread,
        }
    })
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn stale_stat() -> bool {
    gf_gates().stale
}

/// WHO reads an arena field through the HOST. Row 326 leaves ~22 s of the arena migration's
/// regression unattributed with GC refuted (+60 ms) and host-transition counts priced at ~290 ns,
/// while host `arena_getfield` itself rises **20.3 M → 36.8 M** across the flag. A compiled body
/// is supposed to read a field INLINE; every arrival at `rava_getfield`'s arena arm is a field
/// read the emitter declined to inline, which is the same shape as row 324's receiver guard.
#[cfg(feature = "next-gen-gc")]
pub(crate) static GF_FROM_JIT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(feature = "next-gen-gc")]
pub(crate) static GF_FROM_INTERP: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn note_gf_jit() {
    if stale_stat() {
        GF_FROM_JIT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn note_gf_interp() {
    if stale_stat() {
        GF_FROM_INTERP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn stale_stat_report() -> String {
    use std::sync::atomic::Ordering::Relaxed;
    if !stale_stat() {
        return String::new();
    }
    let (r, o) = (STALE_RECOVERED.load(Relaxed), STALE_OK.load(Relaxed));
    format!(
        "[STALE-STAT] arena_getfield: ok={o} recovered_from_stale={r} ({:.6}%)          | callers: from_compiled={} from_interp={} other={}          -- `recovered` is the LOWER BOUND of receivers a bare load would have read dead",
        r as f64 * 100.0 / (r + o).max(1) as f64,
        GF_FROM_JIT.load(Relaxed),
        GF_FROM_INTERP.load(Relaxed),
        (r + o).saturating_sub(GF_FROM_JIT.load(Relaxed) + GF_FROM_INTERP.load(Relaxed)),
    )
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn arena_getfield(p: crate::arena::ArenaPtr, id: u32) -> Value {
    let cid = crate::nextgen::arena_class_id(p);
    let g = gf_gates();
    // ⭐**THE SLOT AND THE KIND ARRIVE TOGETHER.** This used to resolve the slot here and then ask
    // `kind_of` for the kind of the SAME (class, field) below the cell read — see `slot_kind`.
    let (p, cid, slot, kind) = match slot_kind(cid, id) {
        Some((slot, kind)) => {
            if g.stale {
                STALE_OK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            (p, cid, slot, kind)
        }
        None => match forward_stale_arena(p) {
            Some((np, ncid)) => {
                if g.stale {
                    STALE_RECOVERED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                let (slot, kind) = slot_kind(ncid, id).unwrap_or_else(|| {
                    panic!(
                        "PHASE5: arena getfield of non-layout field id={id} on class {:?} (post-forward)",
                        crate::nextgen::class_name(ncid)
                    )
                });
                (np, ncid, slot, kind)
            }
            // RAVA_STALE_SOFT: an irrecoverable stale receiver normally aborts the
            // process (PHASE5 panic). Opt into failing SOFT — read the field as the
            // type default (Null) — so a localized GC-staleness (e.g. a few recipe-
            // reload resources) degrades to a catchable guest error instead of killing
            // the boot. Diagnostic only: it MASKS a real GC bug; off by default.
            None => {
                // 3c forensic (RAVA_PROBE_STALE): classify the stale receiver —
                // was it ever evacuated (writeback miss) or never a root (transient)?
                if crate::nextgen::fwd_log_enabled() {
                    let sp = p.addr();
                    match crate::nextgen::lookup_fwd(sp) {
                        Some(np) => {
                            eprintln!(
                                "[STALE-CLASSIFY] id={id} ptr={sp:#x} WAS FORWARDED to {np:#x} \
                                 => WRITEBACK MISS (a live root location was not updated)"
                            );
                            // Name the holder(s) of the un-updated OLD ptr — the root
                            // location the arena writeback missed. This is the precise Wall-#2
                            // target: whatever type appears here needs adding to
                            // arena_root_locations (or its writeback fixed).
                            // GAP-S09 stage 4: `&Vm` (vm_shared) — pure forensic
                            // reads (find_arena_holders/field_name/backtrace, all `&self`).
                            if let Some(vm) = crate::jit::vm_shared() {
                                let mut holders = vm.heap.find_arena_holders(sp);
                                // Frame locals/stack (a recursive Vm::invoke frame's arena
                                // receiver the collector's frame-forward missed).
                                for &fp in &crate::frames::all() {
                                    let f = unsafe { &*(fp as *const crate::Frame) };
                                    for (i, lv) in f.locals().iter().enumerate() {
                                        if matches!(lv, Value::Obj(q) if q.addr() == sp) {
                                            holders.push(format!(
                                                "frame {} local[{i}]",
                                                f.class.file.this_class
                                            ));
                                        }
                                    }
                                    for (i, sv) in f.stack().iter().enumerate() {
                                        if matches!(sv, Value::Obj(q) if q.addr() == sp) {
                                            holders.push(format!(
                                                "frame {} stack[{i}]",
                                                f.class.file.this_class
                                            ));
                                        }
                                    }
                                }
                                // Arena-object cells (an OLD arena obj → YOUNG receiver edge
                                // the minor-GC rem-set missed → not rewritten).
                                crate::nextgen::arena_for_each_obj(&mut |ap| {
                                    let apr =
                                        unsafe { crate::arena::ArenaPtr::from_addr(ap as u64) };
                                    let cid2 = crate::nextgen::arena_class_id(apr);
                                    if let Some(cname) = crate::nextgen::class_name(cid2) {
                                        let nf = crate::nextgen::arena_nfields(apr) as usize;
                                        for s in 0..nf {
                                            if crate::nextgen::arena_cell(apr, s) as u64 == sp {
                                                holders.push(format!(
                                                    "arena obj class={cname} cell[{s}] @{:#x}",
                                                    apr.addr()
                                                ));
                                            }
                                        }
                                    }
                                });
                                eprintln!("[STALE-HOLDER] {} holder(s) of {sp:#x}:", holders.len());
                                for h in &holders {
                                    eprintln!("    {h}");
                                }
                                // Name the field being read (id=5037) + which classes declare
                                // it — identifies the receiver's real type → the compiled
                                // method whose in-flight SSA receiver went stale.
                                eprintln!(
                                    "[STALE-FIELD] id={id} name={:?} fwd_target_class={:?}",
                                    vm.heap.field_name(id),
                                    crate::nextgen::class_name(crate::nextgen::arena_class_id(
                                        unsafe { crate::arena::ArenaPtr::from_addr(np) }
                                    ))
                                );
                                // The innermost COMPILED method running at the fault — the
                                // RAVA_JIT_DUMP target whose in-flight SSA receiver went stale.
                                eprintln!("[STALE-JITMETHOD] {:?}", crate::jit::jit_method_cur());
                                // Guest method chain at the fault (empty ⇒ JIT context).
                                eprintln!("[STALE-BT]\n{}", vm.backtrace());
                            }
                        }
                        None => eprintln!(
                            "[STALE-CLASSIFY] id={id} ptr={sp:#x} NOT in forward log \
                             => NEVER ROOTED (freed as garbage / in-flight transient)"
                        ),
                    }
                }
                if crate::env_flag!("RAVA_STALE_SOFT") {
                    stale_soft_count(1);
                    return Value::Null;
                }
                // Self-explaining failure: the bare id says nothing, and this fires on a
                // background thread during datapack load where a panic kills only that
                // thread — so the next reproduction must carry its own diagnosis. Print
                // the field NAME and the class's ACTUAL layout ids, which distinguishes
                // the two candidate causes: a PARTIAL layout (super transiently
                // unloadable, see class_field_layout's `complete` flag) shows a short id
                // list missing the inherited fields, whereas a mis-resolved field ref
                // shows a full layout that simply does not contain this id.
                let fname = crate::nextgen::class_layout(cid)
                    .map(|(_, l)| {
                        l.iter()
                            .map(|(i, _)| i.to_string())
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .unwrap_or_else(|| "<no layout>".into());
                // ⭐**IDS DO NOT DISCRIMINATE THE TWO CAUSES THIS MESSAGE NAMES, AND NAMES DO.**
                // Printing only ids left "the layout is short" and "the field ref resolved
                // elsewhere" indistinguishable, which is exactly where the vanilla-Minecraft
                // investigation stopped. Field ids are interned by NAME ALONE, process-wide, so on
                // OBFUSCATED bytecode — where every class declares `a`, `b`, `c` — a shadowed
                // super field and an unrelated sibling's field collapse onto one id while a layout
                // holds one slot per id. The names say at once whether that is what happened: a
                // layout of `[a, b]` asked for `c` is a SHORT layout, and a layout of `[a, b]`
                // asked for `a` is a COLLISION.
                let named = |i: u32| -> String {
                    crate::heap::intern::field_name_of(i).unwrap_or_else(|| format!("#{i}"))
                };
                let layout_names = crate::nextgen::class_layout(cid)
                    .map(|(_, l)| {
                        l.iter()
                            .map(|(i, _)| named(*i))
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .unwrap_or_else(|| "<no layout>".into());
                let chain = crate::nextgen::class_name(cid)
                    .and_then(|cn| {
                        crate::jit::vm_shared().map(|vm| {
                            vm.loader
                                .super_chain(&cn)
                                .iter()
                                .map(|s| s.to_string())
                                .collect::<Vec<_>>()
                                .join(" <- ")
                        })
                    })
                    .unwrap_or_else(|| "<no chain>".into());
                panic!(
                    "PHASE5: arena getfield of non-layout field id={id} ({}) on class {:?};\n\
                     layout ids=[{fname}] names=[{layout_names}]\n  super chain: {chain}",
                    named(id),
                    crate::nextgen::class_name(cid)
                )
            }
        },
    };
    arena_read_cell(p, cid, id, slot, kind, g)
}

/// The read half, once: fetch the inline cell at an ALREADY-RESOLVED slot and decode it with an
/// ALREADY-RESOLVED kind. Shared by `arena_getfield` and `arena_getfield_opt` so the two entry
/// points cannot drift in what they count or what they trace.
#[cfg(feature = "next-gen-gc")]
#[inline]
fn arena_read_cell(
    p: crate::arena::ArenaPtr,
    cid: u32,
    id: u32,
    slot: usize,
    kind: u8,
    g: GfGates,
) -> Value {
    let cell = crate::nextgen::arena_cell(p, slot);
    // `g.trace` is one already-loaded bool. Both diagnostics below used to test their own
    // `OnceLock` on the way in — `STALEREAD_ON` on every NON-NULL ref cell, which is the common
    // case — so a default boot paid an atomic acquire load per reference field read for an
    // answer that cannot change after startup. See `GfGates`.
    if g.trace {
        // Diagnostic (RAVA_TRACE_NULLREF): a REF field (kind >= 4) reading null on an arena
        // object — the arena-migration ref-field bug. Log class + field + slot + the raw cell
        // so a null read is classified: cell==0 (never written / wrong slot) vs a live layout
        // with a stale/mis-encoded ref. Off by default. Env is read ONCE (a per-getfield
        // std::env::var_os is a syscall + env-lock on the hot path — a real boot slowdown).
        if g.nullref && kind >= 4 && cell == 0 {
            // GAP-S09 stage 4: `&Vm` (vm_shared) — read-only diagnostic (`field_name` is `&self`).
            if let Some(vm) = crate::jit::vm_shared() {
                let fname = vm.heap.field_name(id).unwrap_or_else(|| "?".to_string());
                eprintln!(
                    "[NULLREF] class={:?} field={fname} id={id} slot={slot} kind={kind} cell=0 layout_has_id={}",
                    crate::nextgen::class_name(cid),
                    crate::nextgen::slot_of(cid, id).is_some()
                );
            }
        }
        // Diagnostic (RAVA_TRACE_STALEREAD): a REF field reading a STALE young pointer (its slot
        // moved/reused) names the exact HOLDER (receiver class + field) of the strand — e.g. the
        // worldgen Holder$Direct.value that HolderHolder.minValue derefs. Logs whether the receiver
        // itself is young (an inline putfield into a young object skips rava_card_mark, so
        // RAVA_TRACE_STALESTORE can't see that write) vs old, + the innermost compiled method.
        if g.staleread && kind >= 4 && cell != 0 {
            if let Some(fwd) = crate::jit::stale_young_ptr(cell) {
                // GAP-S09 stage 4: `&Vm` (vm_shared) — read-only diagnostic.
                if let Some(vm) = crate::jit::vm_shared() {
                    let fname = vm.heap.field_name(id).unwrap_or_else(|| "?".to_string());
                    let recv_young = crate::nextgen::tlab_in_nursery(p);
                    eprintln!(
                        "[STALE-READ] recv_class={:?} recv={:#x} recv_young={recv_young} field={fname} id={id} stale_val={cell:#x} fwd={fwd:#x} jit={:?}",
                        crate::nextgen::class_name(cid),
                        p.addr(),
                        crate::jit::jit_method_cur()
                    );
                }
            }
        }
    }
    decode_arena_cell(cell, kind)
}

/// Read arena field `id` from `p` ONLY IF the receiver's class actually declares it — exactly
/// the question `Vm::synth_get`/`synth_get_ro` ask before calling `arena_getfield`.
///
/// WAS: the caller proved presence with its OWN `arena_class_id` + `slot_of`, then handed the same
/// pointer to `arena_getfield`, which read the class id out of the header again and probed the
/// layout index again — two header reads and two index probes per synthetic field access, all
/// four answering one question about one value that could not have changed in between. IS: one
/// header read and one probe, with the proof and the read fused.
///
/// It deliberately does NOT run `forward_stale_arena`: the callers it replaces returned
/// `Value::Null` on a slot miss and never forwarded, so a stale receiver behaves exactly as
/// before. `None` here is that same "this class has no such cell" answer.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn arena_getfield_opt(p: crate::arena::ArenaPtr, id: u32) -> Option<Value> {
    let cid = crate::nextgen::arena_class_id(p);
    let (slot, kind) = slot_kind(cid, id)?;
    let g = gf_gates();
    if g.stale {
        STALE_OK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    Some(arena_read_cell(p, cid, id, slot, kind, g))
}

/// Arena putfield (Phase 2): resolve slot, encode the value, write the inline cell.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn arena_putfield(p: crate::arena::ArenaPtr, id: u32, value: &Value) {
    let cid = crate::nextgen::arena_class_id(p);
    // Wall #19 read barrier (see forward_stale_arena): recover a stale/evacuated
    // receiver before the slot lookup panics.
    // ONE PROBE, NOT TWO: the slot resolved here and the kind the encoder needs below are the two
    // halves of one layout-index entry — see `slot_kind`. The old shape asked `slot_of` here and
    // `kind_of` again after the card-mark, per arena field WRITE.
    let (p, slot, kind) = match slot_kind(cid, id) {
        Some((slot, kind)) => (p, slot, kind),
        None => match forward_stale_arena(p) {
            Some((np, ncid)) => {
                let (slot, kind) = slot_kind(ncid, id).unwrap_or_else(|| {
                    panic!(
                        "PHASE5: arena putfield of non-layout field id={id} on class {:?} (post-forward)",
                        crate::nextgen::class_name(ncid)
                    )
                });
                (np, slot, kind)
            }
            // RAVA_STALE_SOFT: drop the write to an irrecoverable stale receiver
            // rather than abort.
            None => {
                if crate::env_flag!("RAVA_STALE_SOFT") {
                    stale_soft_count(1);
                    return;
                }
                panic!(
                    "PHASE5: arena putfield of non-layout field id={id} on class {:?} layout={:?}",
                    crate::nextgen::class_name(cid),
                    crate::nextgen::layout_dump(cid)
                )
            }
        },
    };
    // A flat boxed primitive (identity-boxed Integer/Long/… = `Value::Int`/…) stored
    // into a REFERENCE-kind field (Object/generic, e.g. `Pair`'s components) MUST be
    // tag-boxed via the side table — a raw small int aliases an untagged arena pointer
    // and would decode back as `Obj(small)` → a wild deref. Primitive-kind fields keep
    // the flat encoding (the field's own type disambiguates on read).
    if kind >= 4 {
        store_arena_ref_cell(p, slot, value);
    } else {
        crate::nextgen::arena_set_cell(p, slot, encode_arena_cell(value));
    }
}

/// The functional-interface name behind an interned arena string, built once per pointer.
/// See `arena_lambda_fi` for why the pointer alone is a sound key.
#[cfg(feature = "next-gen-gc")]
fn fi_name(sp: crate::arena::ArenaPtr) -> crate::value::RStr {
    const N: usize = 32;
    thread_local! {
        static MEMO: std::cell::RefCell<Vec<Option<(usize, crate::value::RStr)>>> =
            std::cell::RefCell::new(vec![None; N]);
    }
    let key = sp.addr() as usize;
    let slot = (key >> 4) & (N - 1);
    if let Some(hit) = MEMO.with(|m| {
        m.borrow()[slot]
            .as_ref()
            .and_then(|(k, v)| (*k == key).then(|| v.clone()))
    }) {
        return hit;
    }
    let built: crate::value::RStr = String::from_utf8_lossy(crate::nextgen::arena_str_bytes(sp))
        .into_owned()
        .into();
    MEMO.with(|m| m.borrow_mut()[slot] = Some((key, built.clone())));
    built
}
