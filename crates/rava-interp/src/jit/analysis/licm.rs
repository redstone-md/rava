//! Loop-invariant motion and bounds-check elimination: which loads may be hoisted.
//!
//! Re-exported from the parent module, so no call site outside it changed.

pub mod bce_census {
    use std::sync::atomic::{AtomicU64, Ordering};
    pub static GUARDS_SEEN: AtomicU64 = AtomicU64::new(0);
    pub static HOISTED: AtomicU64 = AtomicU64::new(0);
    pub static REJECT_IV: AtomicU64 = AtomicU64::new(0);
    pub static REJECT_BODY_DIRTY: AtomicU64 = AtomicU64::new(0);
    pub static REJECT_NON_CANONICAL: AtomicU64 = AtomicU64::new(0);
    pub static REJECT_NO_IINC: AtomicU64 = AtomicU64::new(0);
    pub fn bump(c: &AtomicU64) {
        ensure_printer();
        c.fetch_add(1, Ordering::Relaxed);
    }
    fn ensure_printer() {
        static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        ONCE.get_or_init(|| {
            if std::env::var_os("RAVA_BCE_CENSUS").is_none() { return; }
            std::thread::Builder::new().name("bce-printer".into()).spawn(|| loop {
                std::thread::sleep(std::time::Duration::from_secs(5));
                let s = report_inner();
                if !s.is_empty() { eprintln!("{s}"); }
            }).ok();
        });
    }
    fn report_inner() -> String {
        let g = GUARDS_SEEN.load(Ordering::Relaxed);
        let h = HOISTED.load(Ordering::Relaxed);
        let iv = REJECT_IV.load(Ordering::Relaxed);
        let bd = REJECT_BODY_DIRTY.load(Ordering::Relaxed);
        let nc = REJECT_NON_CANONICAL.load(Ordering::Relaxed);
        let ni = REJECT_NO_IINC.load(Ordering::Relaxed);
        let tot = iv + bd + nc + ni;
        format!("[BCE] guards {g} hoisted {h} ({:.1}%) | reject iv={iv} body_dirty={bd} non_canonical={nc} no_iinc={ni} total_reject={tot}\n", if g > 0 { 100.0 * h as f64 / g as f64 } else { 0.0 })
    }
    pub fn report() -> String {
        if std::env::var_os("RAVA_BCE_CENSUS").is_none() { return String::new(); }
        return report_inner();
    }
}

pub mod shape_census {
    use std::sync::atomic::{AtomicU64, Ordering};
    /// Loops whose array ref is provably invariant (`body_dirty == 0`).
    pub static CANDIDATES: AtomicU64 = AtomicU64::new(0);
    /// Primitive array READ sites sitting inside those loops.
    pub static LOAD_SITES: AtomicU64 = AtomicU64::new(0);
    pub fn bump(c: &AtomicU64) { ensure_printer(); c.fetch_add(1, Ordering::Relaxed); }
    pub fn bump_n(c: &AtomicU64, n: u64) { ensure_printer(); c.fetch_add(n, Ordering::Relaxed); }
    fn ensure_printer() {
        static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        ONCE.get_or_init(|| {
            if std::env::var_os("RAVA_SHAPE_CENSUS").is_none() { return; }
            std::thread::Builder::new().name("shape-printer".into()).spawn(|| loop {
                std::thread::sleep(std::time::Duration::from_secs(5));
                let s = report_inner();
                if !s.is_empty() { eprintln!("{s}"); }
            }).ok();
        });
    }
    fn report_inner() -> String {
        let c = CANDIDATES.load(Ordering::Relaxed);
        let s = LOAD_SITES.load(Ordering::Relaxed);
        if c == 0 && s == 0 { return String::new(); }
        format!("[SHAPE] candidates {c} load_sites {s} avg {:.1}/loop\n",
            if c > 0 { s as f64 / c as f64 } else { 0.0 })
    }
    pub fn report() -> String {
        if std::env::var_os("RAVA_SHAPE_CENSUS").is_none() { return String::new(); }
        report_inner()
    }
}

use crate::jit::*;
/// FIELD-LICM pair discovery. In a `no_alloc` method with a loop, every constant-slot
/// PRIMITIVE getfield whose receiver is a pinned local (directly `aload k; getfield`,
/// or through an inlined callee whose receiver/args are consecutive one-slot loads)
/// gets its field-cell pointer resolved ONCE in the prologue. The per-site guard
/// chain (arena-vs-handle diamond) can't be hoisted by the egraph because of its
/// branches; resolving the ADDRESS up front reduces each site to `ptr!=0 ? load :
/// slow-path` — the branch predicts perfectly and the load is the only real work.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn licm_field_pairs(
    bc: &[u8],
    fields: &std::collections::HashMap<u16, (u32, JT, Option<u32>)>,
    inline_targets: &std::collections::HashMap<u16, InlineCallee>,
    max_locals: usize,
) -> Vec<LicmPair> {
    if !has_back_edge(bc) {
        return Vec::new();
    }
    let pinned = pinned_ref_slots(bc, max_locals);
    let mut out: Vec<LicmPair> = Vec::new();
    let mut add = |out: &mut Vec<LicmPair>, slot: usize, fid: u32, cslot: u32| {
        if slot < pinned.len() && pinned[slot] {
            let p = LicmPair { slot: slot as u16, fid, cslot };
            if !out.contains(&p) && out.len() < 16 {
                out.push(p);
            }
        }
    };
    // Consecutive ONE-SLOT loads immediately preceding the current pc: (is_aload, slot).
    // Any other instruction resets the window.
    let mut loads: Vec<(bool, usize)> = Vec::new();
    let mut pc = 0usize;
    while pc < bc.len() {
        let op = bc[pc];
        let Some(len) = insn_len_at(bc, pc) else { return Vec::new() };
        match op {
            0x19 => loads.push((true, bc[pc + 1] as usize)),
            0x2a..=0x2d => loads.push((true, (op - 0x2a) as usize)),
            0x15 | 0x17 => loads.push((false, 0)), // iload / fload (one slot, not a ref)
            0x1a..=0x1d | 0x22..=0x25 => loads.push((false, 0)),
            0xb4 => {
                let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
                if let (Some(&(true, k)), Some(&(fid, jt, Some(cslot)))) =
                    (loads.last(), fields.get(&idx))
                {
                    if jt != JT::Ref {
                        add(&mut out, k, fid, cslot);
                    }
                }
                loads.clear();
            }
            0xb6 | 0xb7 => {
                let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
                if let Some(callee) = inline_targets.get(&idx) {
                    let total =
                        crate::count_args(&callee.descriptor) + usize::from(!callee.is_static);
                    if loads.len() >= total && total > 0 {
                        let base = loads.len() - total;
                        // Which caller local feeds each callee arg slot (one slot per
                        // arg — is_inlinable rejects wide params). A callee that stores
                        // to a slot breaks the mapping for that slot.
                        let ckill = pinned_ref_slots(&callee.bytecode, total);
                        let cbc = &callee.bytecode;
                        let mut cpc = 0usize;
                        let mut prev_aload: Option<usize> = None;
                        while cpc < cbc.len() {
                            let cop = cbc[cpc];
                            let Some(clen) = insn_len_at(cbc, cpc) else { break };
                            match cop {
                                0x19 => prev_aload = Some(cbc[cpc + 1] as usize),
                                0x2a..=0x2d => prev_aload = Some((cop - 0x2a) as usize),
                                0xb4 => {
                                    let cidx = ((cbc[cpc + 1] as u16) << 8) | cbc[cpc + 2] as u16;
                                    if let (Some(j), Some(&(fid, jt, Some(cslot)))) =
                                        (prev_aload, callee.fields.get(&cidx))
                                    {
                                        // ckill[j] = "loaded and never stored in callee"
                                        // — exactly the pin condition for slot j.
                                        if jt != JT::Ref && j < total && ckill[j] {
                                            if let (true, k) = loads[base + j] {
                                                add(&mut out, k, fid, cslot);
                                            }
                                        }
                                    }
                                    prev_aload = None;
                                }
                                _ => prev_aload = None,
                            }
                            cpc += clen;
                        }
                    }
                }
                loads.clear();
            }
            _ => loads.clear(),
        }
        pc += len;
    }
    out
}

/// The PROMO abstract-stack walk: a linear scan with an EXACT logical operand
/// stack of receiver provenances (`Some(outer local)` for a pinned `aload`,
/// `None` otherwise). Conservative merge discipline: the stack must be EMPTY at
/// every branch and every leader (javac statement boundaries guarantee this for
/// the loop shapes we target), so no cross-block stack state exists to merge.
/// `calls = None` inside an inlined callee (no invokes/putfields there —
/// `is_inlinable` already forbids them).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn promo_scan_body(
    bc: &[u8],
    fields: &std::collections::HashMap<u16, (u32, JT, Option<u32>)>,
    pool: &ConstantPool,
    prov: &[Option<u16>],
    pairs: &[LicmPair],
    calls: Option<(
        &std::collections::HashMap<u16, InlineCallee>,
        &std::collections::HashMap<u16, StaticIntrinsic>,
    )>,
    written: &mut std::collections::HashSet<(u16, u32, u32)>,
) -> Option<()> {
    let leaders: std::collections::HashSet<usize> = find_leaders(bc)?.into_iter().collect();
    let mut st: Vec<Option<u16>> = Vec::new();
    let mut pc = 0usize;
    while pc < bc.len() {
        if pc != 0 && leaders.contains(&pc) && !st.is_empty() {
            return None; // non-empty stack at a merge — shape too clever, bail
        }
        let op = bc[pc];
        let len = insn_len_at(bc, pc)?;
        match op {
            0x00 => {}
            0x02..=0x11 => st.push(None), // iconst..sipush (0x01 aconst_null excluded)
            0x12 | 0x13 => {
                // ldc/ldc_w: numeric only — a string/class const can take an
                // intern-miss callback (GC point) mid-loop.
                let idx = if op == 0x12 {
                    bc[pc + 1] as u16
                } else {
                    ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16
                };
                match pool.get(idx)? {
                    Constant::Integer(_) | Constant::Float(_) => st.push(None),
                    _ => return None,
                }
            }
            0x14 => st.push(None),         // ldc2_w long/double
            0x15..=0x18 => st.push(None),  // iload/lload/fload/dload
            0x19 => st.push(prov.get(bc[pc + 1] as usize).copied().flatten()),
            0x1a..=0x29 => st.push(None),  // i/l/f/d load_n
            0x2a..=0x2d => st.push(prov.get((op - 0x2a) as usize).copied().flatten()),
            0x36..=0x3a | 0x3b..=0x4e | 0x57 => {
                st.pop()?; // stores / pop
            }
            0x59 => {
                let t = *st.last()?;
                st.push(t); // dup carries provenance
            }
            0x60..=0x6b | 0x6e | 0x6f | 0x72 | 0x73 | 0x78..=0x83 | 0x94..=0x98 => {
                st.pop()?;
                st.pop()?;
                st.push(None); // add/sub/mul, f/d div+rem, shifts/logic, cmp
            }
            0x6c | 0x6d | 0x70 | 0x71 => return None, // int div/rem: ArithmeticException exit
            0x74..=0x77 => {
                st.pop()?;
                st.push(None); // neg
            }
            0x84 => {} // iinc
            0x85..=0x93 => {
                st.pop()?;
                st.push(None); // conversions
            }
            0x99..=0x9e | 0xc6 | 0xc7 => {
                st.pop()?;
                if !st.is_empty() {
                    return None;
                }
            }
            0x9f..=0xa6 => {
                st.pop()?;
                st.pop()?;
                if !st.is_empty() {
                    return None;
                }
            }
            0xa7 => {
                if !st.is_empty() {
                    return None;
                }
            }
            0xac..=0xb0 => {
                st.pop()?;
                if !st.is_empty() {
                    return None;
                }
            }
            0xb1 => {
                if !st.is_empty() {
                    return None;
                }
            }
            0xb4 => {
                let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
                let &(fid, jt, slot) = fields.get(&idx)?;
                let cslot = slot?;
                if jt == JT::Ref {
                    return None;
                }
                let k = st.pop()??; // receiver provenance must be a pinned local
                if !pairs.contains(&LicmPair { slot: k, fid, cslot }) {
                    return None;
                }
                st.push(None);
            }
            0xb5 => {
                if calls.is_none() {
                    return None; // inlined callees never putfield (is_inlinable)
                }
                let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
                let &(fid, jt, slot) = fields.get(&idx)?;
                let cslot = slot?;
                if jt == JT::Ref {
                    return None;
                }
                st.pop()?; // the stored value
                let k = st.pop()??;
                if !pairs.contains(&LicmPair { slot: k, fid, cslot }) {
                    return None;
                }
                written.insert((k, fid, cslot));
            }
            0xb6 | 0xb7 => {
                let (inline_targets, _) = calls?;
                let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
                let m = pool.member_ref(idx)?;
                if promo_virt_preempted(m.owner, m.name, m.descriptor) {
                    return None;
                }
                let callee = inline_targets.get(&idx)?;
                // A promoted field register is a value held across the whole call, so any GC
                // point in the spliced body would leave it stale. `promo_scan_callee` already
                // refuses every such body through its opcode whitelist; asking the entry
                // directly makes the reason explicit instead of incidental.
                if callee.leaves_gc_point() {
                    return None;
                }
                if callee.is_static {
                    return None; // emit_op would fall through to a real call
                }
                let total = crate::count_args(&callee.descriptor) + 1;
                if st.len() < total {
                    return None;
                }
                let args: Vec<Option<u16>> = st.split_off(st.len() - total);
                promo_scan_callee(callee, &args, pairs)?;
                st.push(None); // is_inlinable guarantees a non-void return
            }
            0xb8 => {
                let (inline_targets, static_intrinsics) = calls?;
                let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
                let m = pool.member_ref(idx)?;
                if m.owner == "java/lang/Integer" && m.name == "valueOf" {
                    return None; // is_box_int preempt — allocates
                }
                let nargs = crate::count_args(m.descriptor);
                if static_intrinsics.contains_key(&idx) {
                    for _ in 0..nargs {
                        st.pop()?;
                    }
                    if ret_full(m.descriptor).is_some() {
                        st.push(None);
                    }
                } else if let Some(callee) = inline_targets.get(&idx) {
                    if !callee.is_static || callee.leaves_gc_point() {
                        return None;
                    }
                    if st.len() < nargs {
                        return None;
                    }
                    let args: Vec<Option<u16>> = st.split_off(st.len() - nargs);
                    promo_scan_callee(callee, &args, pairs)?;
                    st.push(None);
                } else {
                    return None; // a real invokestatic (or an unmirrored Math route)
                }
            }
            _ => return None, // arrays, athrow, statics, switch, wide, monitors, alloc…
        }
        pc += len;
    }
    Some(())
}

/// PCs of array-load ops (`0x2e..=0x35`) whose per-element bounds check may be
/// SKIPPED because they read `arr[iv]` inside a canonical counted loop that
/// guarantees `0 <= iv < arr.length`. Empty when the kill-switch disables
/// loop-BCE, when the method is malformed, or when no candidate is provably safe.
pub(crate) fn safe_bce_loads(bc: &[u8]) -> std::collections::HashSet<usize> {
    let mut safe = std::collections::HashSet::new();
    if !loop_bce_enabled() {
        return safe;
    }
    // Ordered instruction PCs. Any decode failure → no BCE (bail whole method).
    let mut insns: Vec<usize> = Vec::new();
    let mut pc = 0usize;
    while pc < bc.len() {
        insns.push(pc);
        match insn_len_at(bc, pc) {
            Some(l) if l > 0 => pc += l,
            _ => return std::collections::HashSet::new(),
        }
    }
    // index of a PC within `insns`, for O(1) neighbour lookups.
    let pos: std::collections::HashMap<usize, usize> =
        insns.iter().enumerate().map(|(i, &p)| (p, i)).collect();

    for k in 2..insns.len() {
        let guard_pc = insns[k];
        if bc[guard_pc] == 0xbe {
            bce_census::bump(&bce_census::GUARDS_SEEN);
        }
        if bc[guard_pc] != 0xbe {
            continue; // arraylength
        }
        // insns[k-1] = aload arr ; insns[k-2] = iload iv
        let arr = match aload_local(bc, insns[k - 1]) {
            Some(a) => a,
            None => continue,
        };
        let iv = match iload_local(bc, insns[k - 2]) {
            Some(i) => i,
            None => continue,
        };
        let header_pc = insns[k - 2]; // the `iload iv` — back-edge target for head form
        // insns[k+1] must be a compare-branch.
        if k + 1 >= insns.len() {
            continue;
        }
        let bpc = insns[k + 1];
        let branch_op = bc[bpc];
        // Resolve the loop body byte range [body_start, body_end).
        let body: Option<(usize, usize)> = match branch_op {
            // HEAD-controlled: `if_icmpge/gt EXIT` (forward). Body is the
            // fall-through up to a back-edge `goto header_pc` inside (bpc, exit).
            0xa2 | 0xa3 => {
                let exit = (bpc as i64 + s16(bc, bpc + 1) as i64) as usize;
                if exit <= bpc {
                    None // must be forward
                } else {
                    // find a `goto` (0xa7) in (bpc, exit) whose target == header_pc
                    let mut found = None;
                    let mut i = pos[&bpc] + 1;
                    while i < insns.len() && insns[i] < exit {
                        let p = insns[i];
                        if bc[p] == 0xa7 {
                            let t = (p as i64 + s16(bc, p + 1) as i64) as usize;
                            if t == header_pc {
                                found = Some(p);
                                break;
                            }
                        }
                        i += 1;
                    }
                    found.map(|goto_pc| (bpc + 3, goto_pc))
                }
            }
            // BOTTOM-controlled: `if_icmplt/le BODY_START` (must be BACKWARD).
            0xa1 | 0xa4 => {
                let body_start = (bpc as i64 + s16(bc, bpc + 1) as i64) as usize;
                if body_start < header_pc {
                    Some((body_start, header_pc))
                } else {
                    None
                }
            }
            _ => None,
        };
        let (body_start, body_end) = match body {
            Some(r) => r,
            None => { bce_census::bump(&bce_census::REJECT_NON_CANONICAL); continue; },
        };
        if body_start >= body_end || body_end > bc.len() || !pos.contains_key(&body_start) {
            bce_census::bump(&bce_census::REJECT_NON_CANONICAL);
            continue;
        }
        // Soundness gate: iv globally ≥0 AND body writes neither arr nor iv
        // (except a single `iinc iv,1`, which iv_nonneg already bounds).
        if !iv_nonneg_monotonic(bc, iv) {
            bce_census::bump(&bce_census::REJECT_IV);
            continue;
        }
        let mut body_clean = true;
        {
            let mut i = pos[&body_start];
            while i < insns.len() && insns[i] < body_end {
                let p = insns[i];
                if astore_local(bc, p) == Some(arr) {
                    body_clean = false;
                    break;
                }
                // any istore to iv (0x36 / 0x3b..=0x3e / wide istore) inside body → reject
                let ist = match bc[p] {
                    0x36 => bc.get(p + 1).map(|&x| x as usize),
                    op @ 0x3b..=0x3e => Some((op - 0x3b) as usize),
                    0xc4 if bc.get(p + 1) == Some(&0x36) => Some(
                        ((*bc.get(p + 2).unwrap_or(&0) as usize) << 8)
                            | *bc.get(p + 3).unwrap_or(&0) as usize,
                    ),
                    _ => None,
                };
                if ist == Some(iv) {
                    body_clean = false;
                    break;
                }
                i += 1;
            }
        }
        if !body_clean {
            bce_census::bump(&bce_census::REJECT_BODY_DIRTY);
            continue;
        }
        // SHAPE-GATE census (`RAVA_SHAPE_CENSUS=1`): body_dirty == 0 proves `arr`
        // is invariant for the whole counted loop. Every primitive Xaload that
        // sits inside that body pays hi==ARRAY_TAG_HI + any_prim + array_desc_addr
        // on every iteration; a pre-header hoist would leave only bounds+load.
        // Count candidates (loops) and load sites (reads) so the ceiling of that
        // transform can be quoted without building it.
        {
            shape_census::bump(&shape_census::CANDIDATES);
            let mut loads = 0u64;
            // Scan the body for `aload arr; iload iv; Xaload` reads the same way the
            // hoist decision below does, so the count is byte-for-byte with the pop.
            let mut j = pos[&body_start];
            while j < insns.len() && insns[j] < body_end {
                let pc = insns[j];
                let op = bc[pc];
                if (0x2e..=0x35).contains(&op) && j >= 2
                    && aload_local(bc, insns[j-2]) == Some(arr)
                    && iload_local(bc, insns[j-1]) == Some(iv)
                {
                    loads += 1;
                }
                j += 1;
            }
            shape_census::bump_n(&shape_census::LOAD_SITES, loads);
        }
        // Mark every `aload arr; iload iv; Xaload` in the body. Extra-conservative:
        // only loads BEFORE the body's `iinc iv,1` are marked, so a (non-canonical)
        // access after the increment — where iv could equal length — is never
        // skipped. Canonical loops place the iinc last, so coverage is unaffected.
        let iinc_pc = {
            let mut found = usize::MAX;
            let mut i = pos[&body_start];
            while i < insns.len() && insns[i] < body_end {
                let p = insns[i];
                if bc[p] == 0x84 && bc.get(p + 1) == Some(&(iv as u8)) {
                    found = p;
                    break;
                }
                i += 1;
            }
            found
        };
        if iinc_pc == usize::MAX {
            bce_census::bump(&bce_census::REJECT_NO_IINC);
            continue;
        }
        let mut i = pos[&body_start];
        while i < insns.len() && insns[i] < body_end {
            let load_pc = insns[i];
            if load_pc >= iinc_pc {
                break; // don't mark loads at/after the increment
            }
            let op = bc[load_pc];
            if (0x2e..=0x35).contains(&op) && i >= 2 {
                // access shape is `aload arr; iload iv; Xaload`
                let a2 = insns[i - 2]; // expect aload arr
                let a1 = insns[i - 1]; // expect iload iv
                if aload_local(bc, a2) == Some(arr) && iload_local(bc, a1) == Some(iv) {
                    if safe.insert(load_pc) {
                        bce_census::bump(&bce_census::HOISTED);
                    }
                }
            }
            i += 1;
        }
    }
    safe
}
