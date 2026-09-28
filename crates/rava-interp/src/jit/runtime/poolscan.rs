//! Constant-pool scanning: which field and getstatic sites a method resolves up front.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

#[cfg(feature = "next-gen-gc")]
pub(crate) fn pool_scan_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_POOL_SCAN").is_some())
}

/// Probe one hot method (once per class::name+desc) and log it if it clears the analyzer as a
/// callback-free counted map loop. `pool` resolves the loop's static-call targets to confirm
/// Math-only (any other static call = not callback-free = not logged).
#[cfg(feature = "next-gen-gc")]
pub(crate) static POOL_SCAN_PROBED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(feature = "next-gen-gc")]
pub(crate) static POOL_SCAN_HITS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Rejection anatomy counters (methods that failed the map analyzer). Presence-flags over the
/// method bytecode: does it call methods, branch, use non-int/long/double array element types.
#[cfg(feature = "next-gen-gc")]
pub(crate) static REJ_INVOKE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(feature = "next-gen-gc")]
pub(crate) static REJ_BRANCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(feature = "next-gen-gc")]
pub(crate) static REJ_NONSTD_ARR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// The addressable set: rejected, but NO method call and NO branch beyond a single loop back-edge —
/// i.e. potentially unlockable by relaxing array/element/template rules (what 15.3 targets).
#[cfg(feature = "next-gen-gc")]
pub(crate) static REJ_CLEAN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// 16.3: loose upper bound on what a pre-inline pass could convert to a map candidate:
/// coarse loop-with-invokes/no-extra-branch rejects plus analyzer-ready maps blocked only
/// by a non-Math static call.
#[cfg(feature = "next-gen-gc")]
pub(crate) static REJ_PREINLINE_CEIL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(feature = "next-gen-gc")]
pub(crate) fn pool_scan_reject_reason(bc: &[u8]) -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    let (mut inv, mut br, mut nsa, mut branches) = (false, false, false, 0u32);
    let mut pc = 0usize;
    while pc < bc.len() {
        let op = bc[pc];
        match op {
            0xb6 | 0xb9 | 0xba => inv = true,                 // invokevirtual/interface/dynamic
            0xb8 => inv = true,                                // invokestatic (Math filtered later; coarse)
            0x99..=0xa6 | 0xa7 | 0xc6 | 0xc7 => branches += 1, // if*/if_icmp*/goto/ifnull/ifnonnull
            0xaa | 0xab => br = true,                           // table/lookupswitch
            0x30 | 0x32 | 0x33 | 0x34 | 0x35              // faload/aaload/baload/caload/saload
            | 0x51 | 0x53..=0x56 => nsa = true,                // fastore/aastore/bastore/castore/sastore
            _ => {}
        }
        pc += insn_len_at(bc, pc).unwrap_or(1);
    }
    // A counted map loop legitimately has ONE back-edge (if_icmpge exit + goto header = up to 2
    // branch ops); more than that = real in-body control flow.
    if branches > 2 {
        br = true;
    }
    if inv { REJ_INVOKE.fetch_add(1, Relaxed); }
    if br { REJ_BRANCH.fetch_add(1, Relaxed); }
    if nsa { REJ_NONSTD_ARR.fetch_add(1, Relaxed); }
    if !inv && !br { REJ_CLEAN.fetch_add(1, Relaxed); }
    // 16.3 PRE-INLINE CEILING: the ONLY rejects a pre-inline pass could plausibly convert to
    // candidates — a LOOP (has_back_edge) with invokes but NO extra in-body branch (branches<=2 =
    // just the counted exit+goto). Even these convert only if their invoked leaves are branch-free
    // + std-array (floor/clamp branch, p has baload — so this is a loose UPPER bound).
    let preinline = has_back_edge(bc) && inv && !br;
    if preinline {
        REJ_PREINLINE_CEIL.fetch_add(1, Relaxed);
    }
    preinline
}

/// Exit summary so a zero-candidate run is distinguishable from a probe that never fired.
#[cfg(feature = "next-gen-gc")]
pub fn pool_scan_dump() {
    use std::sync::atomic::Ordering::Relaxed;
    eprintln!(
        "[RAVA-POOL-SCAN] probed {} distinct hot/compilable methods, {} callback-free map-loop candidate(s)",
        POOL_SCAN_PROBED.load(Relaxed), POOL_SCAN_HITS.load(Relaxed)
    );
    eprintln!(
        "[RAVA-POOL-SCAN] rejection anatomy: has-invoke={} has-branch/cflow={} nonstd-array-elem={} | CLEAN(no invoke,no cflow — the addressable set)={}",
        REJ_INVOKE.load(Relaxed), REJ_BRANCH.load(Relaxed), REJ_NONSTD_ARR.load(Relaxed), REJ_CLEAN.load(Relaxed)
    );
    eprintln!(
        "[RAVA-POOL-SCAN] pre-inline ceiling (coarse loop+invoke or analyzer-ready static call): {}",
        REJ_PREINLINE_CEIL.load(Relaxed)
    );
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn pool_scan_probe(class: &str, name: &str, desc: &str, bc: &[u8], pool: &ConstantPool) {
    use std::collections::HashSet;
    use std::sync::atomic::Ordering::Relaxed;
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let key = format!("{class}::{name}{desc}");
    {
        let mut g = SEEN.get_or_init(|| Mutex::new(HashSet::new())).lock().unwrap_or_else(|e| e.into_inner());
        if !g.insert(key) {
            return; // already probed this method
        }
    }
    let probed = POOL_SCAN_PROBED.fetch_add(1, Relaxed) + 1;
    let Some(m) = analyze_data_parallel_map(bc).filter(|m| m.step == 1) else {
        // Rejection anatomy: WHY did this hot method fail the map analyzer? Coarse presence-flags
        // over the whole method decide whether the requested relaxations (multi-array element
        // types / bounds-check branches) could EVER help, vs the irreducible invoke/cflow blockers.
        let preinline = pool_scan_reject_reason(bc);
        if preinline {
            eprintln!("[RAVA-POOL-PREINLINE] Candidate loop with invokes and no extra branch: {}::{}{}", class, name, desc);
        }
        if probed % 250 == 0 {
            pool_scan_dump();
        }
        return;
    };
    // Callback-free ⇔ every static call in the value is Math.max/min (the analyzer already
    // rejected invokevirtual/interface/new; a non-Math static would fall to serial at codegen).
    let math_only = m.static_calls.iter().all(|&(_, cp)| {
        matches!(
            pool.member_ref(cp).map(|mr| (mr.owner, mr.name)),
            Some(("java/lang/Math", "max")) | Some(("java/lang/Math", "min"))
        )
    });
    if math_only {
        POOL_SCAN_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        eprintln!("[RAVA-POOL-TARGET] Found valid callback-free loop candidate: {}::{}{}", class, name, desc);
    } else {
        REJ_PREINLINE_CEIL.fetch_add(1, Relaxed);
        eprintln!("[RAVA-POOL-PREINLINE] Analyzer-ready map blocked by static call: {}::{}{}", class, name, desc);
    }
    if probed % 250 == 0 {
        pool_scan_dump();
    }
}

/// Walk the (already opcode-validated) bytecode and resolve every getfield's
/// cp index to (interned field id, field type). `None` if a getfield's field
/// ref can't be resolved.
pub(crate) fn resolve_fields(
    bc: &[u8],
    pool: &ConstantPool,
    intern: &mut dyn FnMut(&str, &str) -> (u32, Option<u32>),
) -> Option<std::collections::HashMap<u16, (u32, JT, Option<u32>)>> {
    let mut map = std::collections::HashMap::new();
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        let len = insn_len_at(bc, pc)?;
        if op == 0xb4 || op == 0xb5 {
            // getfield / putfield — pre-resolve (interned id, field type, layout
            // slot). The STORAGE id is keyed by the declaring class (shadowing);
            // the slot (when present) is its class-constant index.
            let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            let m = pool.member_ref(idx)?;
            let (id, slot) = intern(m.owner, m.name);
            map.insert(idx, (id, desc_to_jt(m.descriptor), slot));
        }
        pc += len;
    }
    Some(map)
}

pub(crate) fn array_field_indices(bc: &[u8], pool: &ConstantPool) -> std::collections::HashSet<u16> {
    let mut out = std::collections::HashSet::new();
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        let Some(len) = insn_len_at(bc, pc) else { break };
        if op == 0xb4 || op == 0xb5 {
            let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            if let Some(m) = pool.member_ref(idx) {
                if descriptor_keeps_tag(m.descriptor) {
                    out.insert(idx);
                }
            }
        }
        pc += len;
    }
    out
}

/// Every getstatic (0xb2) in `bc` as `(owner, field name)`, read against `bc`'s OWN pool.
/// The inline gate (`inline_target_for_call`) uses this to prove each read's class is already
/// clinit-done before splicing the body — `is_inlinable` admits 0xb2 structurally but cannot
/// see VM state. Returns None if the walk hits a variable-length op (`resolve_fields` bails
/// the same way, so such a method never inlines anyway).
pub(crate) fn getstatic_refs<'a>(
    bc: &[u8],
    pool: &'a ConstantPool,
) -> Option<Vec<(&'a str, &'a str)>> {
    let mut out = Vec::new();
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        let len = insn_len_at(bc, pc)?;
        if op == 0xb2 {
            let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            let m = pool.member_ref(idx)?;
            out.push((m.owner, m.name));
        }
        pc += len;
    }
    Some(out)
}

/// Walk `bc` and collect, in bytecode order, every getstatic (0xb2) of a PRIMITIVE
/// field (int/long/float/double — NOT a reference; a cached ref handle could dangle
/// past a GC) as `(cp index, type)`. The caller reserves one getstatic value cell
/// per entry. Bytecode order is fixed, so the slot assignment is deterministic.
/// Returns empty if the walk hits a variable-length op (then the method won't
/// compile anyway — `resolve_fields` bails the same way).
pub(crate) fn primitive_getstatic_cps(bc: &[u8], pool: &ConstantPool) -> Vec<(u16, JT)> {
    let mut out = Vec::new();
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        let Some(len) = insn_len_at(bc, pc) else { return Vec::new() };
        if op == 0xb2 {
            let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            if let Some(m) = pool.member_ref(idx) {
                let jt = desc_to_jt(m.descriptor);
                // Reference statics join the cell table only under the flag (see
                // `ref_static_cells_enabled` for the soundness argument).
                if jt != JT::Ref || ref_static_cells_enabled() {
                    out.push((idx, jt));
                }
            }
        }
        pc += len;
    }
    out
}

/// `getfield`/`putfield` sites in this bytecode — the COW-diamond sites the
/// unswitch removes from the hot body. Inlined callees carry their own, so the
/// unswitch gate adds `env.inline_targets.len()` to this.
pub(crate) fn count_field_sites(bc: &[u8]) -> usize {
    let mut count = 0usize;
    let mut pc = 0usize;
    while pc < bc.len() {
        let op = bc[pc];
        let Some(len) = insn_len_at(bc, pc) else { return count };
        if op == 0xb4 || op == 0xb5 {
            count += 1;
        }
        pc += len;
    }
    count
}
