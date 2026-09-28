//! Escape analysis: the CFG transfer function and the straight-line fast path.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

/// ⭐**WHICH LOCALS MAY HOLD THE NEWBORN — 32 BYTES, NOT A MAP PER SITE.**
///
/// WAS: a `HashMap<usize, bool>` built from scratch for every `new` site (the straight-line
/// walk) and a `BTreeSet<usize>` CLONED on entry to every block visit and re-compared per
/// successor edge (the CFG fixpoint), so the allocation count grew with sites × blocks ×
/// iterations. IS: a `Copy` bitset, no allocation on any path.
///
/// The key domain is exactly a bytecode LOCAL INDEX, and every opcode either walk models reads
/// that index as a single byte (`astore idx`, `astore_<n>`, `aload idx`, `aload_<n>`), so 256
/// bits is the entire space it can ever hold — a `wide`-prefixed form is unmodelled by both
/// walks and takes the conservative arm before it could reach here. Union, membership, emptiness
/// and equality are the same relations on the same sets, so every verdict is unchanged by
/// construction; there is no second conclusion for `RAVA_NO_FAST_ANALYSIS` to select, and this
/// whole file already runs only under `RAVA_DBG_VOA` / `RAVA_JIT_SROA`.
#[cfg(feature = "next-gen-gc")]
type Taint = [u64; 4];

#[cfg(feature = "next-gen-gc")]
#[inline]
fn taint_get(t: &Taint, i: usize) -> bool {
    i < 256 && t[i >> 6] & (1u64 << (i & 63)) != 0
}

#[cfg(feature = "next-gen-gc")]
#[inline]
fn taint_set(t: &mut Taint, i: usize, on: bool) {
    if i < 256 {
        if on {
            t[i >> 6] |= 1u64 << (i & 63);
        } else {
            t[i >> 6] &= !(1u64 << (i & 63));
        }
    }
}

#[cfg(feature = "next-gen-gc")]
#[inline]
fn taint_any(t: &Taint) -> bool {
    (t[0] | t[1] | t[2] | t[3]) != 0
}

/// True if the reference born at `new_pc` escapes (sound straight-line taint walk).
/// Abstract state: a boolean-tainted operand stack + locals; the newborn is the only
/// taint source. Any opcode that lets a tainted value reach an escaping sink, or any
/// opcode whose stack effect we cannot model precisely while a taint is live, returns
/// `true` (conservative).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn object_escapes_straightline(bc: &[u8], pool: &ConstantPool, new_pc: usize) -> bool {
    let mut stack: Vec<bool> = Vec::new(); // true = tainted (the newborn); 1 entry / value
    let mut locals: Taint = [0; 4];
    // Pop `n` values; return true if ANY popped value was tainted (escaped by this op).
    fn popn(s: &mut Vec<bool>, n: usize) -> bool {
        let mut t = false;
        for _ in 0..n {
            if s.pop().unwrap_or(false) {
                t = true;
            }
        }
        t
    }
    let mut pc = new_pc;
    pc += 3; // the `new` pushes the tainted reference
    stack.push(true);
    while pc < bc.len() {
        let op = bc[pc];
        let Some(len) = insn_len_at(bc, pc) else { return true };
        match op {
            // ── taint-MOVING ops (exact) ──
            0x59 => {
                let t = *stack.last().unwrap_or(&false);
                stack.push(t); // dup
            }
            0x5f => {
                let n = stack.len();
                if n >= 2 {
                    stack.swap(n - 1, n - 2);
                }
            }
            0x3a => {
                let t = popn(&mut stack, 1);
                taint_set(&mut locals, *bc.get(pc + 1).unwrap_or(&0) as usize, t); // astore
            }
            0x4b..=0x4e => {
                let t = popn(&mut stack, 1);
                taint_set(&mut locals, (op - 0x4b) as usize, t); // astore_0..3
            }
            0x19 => stack.push(taint_get(&locals, *bc.get(pc + 1).unwrap_or(&0) as usize)),
            0x2a..=0x2d => stack.push(taint_get(&locals, (op - 0x2a) as usize)),
            // ── SAFE consumers of a self-reference ──
            0xb4 => {
                let _recv = popn(&mut stack, 1); // getfield: own-field read is safe
                stack.push(false);
            }
            0xb5 => {
                // putfield [objref, value]: a tainted VALUE escapes; a self putfield
                // (tainted objref, clean value) is safe.
                let value = popn(&mut stack, 1);
                let _objref = popn(&mut stack, 1);
                if value {
                    return true;
                }
            }
            0xb7 => {
                let m = match pool.member_ref(((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16) {
                    Some(m) => m,
                    None => return true,
                };
                let nargs = crate::count_args(m.descriptor);
                let tainted_arg = popn(&mut stack, nargs);
                let recv = popn(&mut stack, 1);
                // The ctor consuming the receiver is safe; a tainted arg (to any
                // invokespecial) leaks the ref; a tainted receiver to a NON-ctor leaks.
                if tainted_arg || (recv && m.name != "<init>") {
                    return true;
                }
                if ret_jt(m.descriptor).is_some() {
                    stack.push(false);
                }
            }
            // ── ESCAPING sinks: any tainted operand leaks ──
            0xb3 => {
                if popn(&mut stack, 1) {
                    return true; // putstatic
                }
            }
            0xb0 => return popn(&mut stack, 1), // areturn — escapes iff the ref is returned
            0x4f..=0x52 | 0x54..=0x56 => {
                if popn(&mut stack, 3) {
                    return true; // *astore (arr, idx, value)
                }
            }
            0x53 => {
                if popn(&mut stack, 3) {
                    return true; // aastore
                }
            }
            0xb6 | 0xb8 | 0xb9 => {
                // invokevirtual/static/interface: a tainted receiver or arg escapes.
                let m = match pool.member_ref(((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16) {
                    Some(m) => m,
                    None => return true,
                };
                let nargs = crate::count_args(m.descriptor);
                let recv = if op == 0xb8 { 0 } else { 1 };
                if popn(&mut stack, nargs + recv) {
                    return true;
                }
                if ret_jt(m.descriptor).is_some() {
                    stack.push(false);
                }
            }
            0xc0 => { /* checkcast: pop+push, taint preserved → leave top as-is */ }
            // pop / pop2: DISCARD the value(s) — dropping the newborn is not an escape.
            0x57 => {
                stack.pop();
            }
            0x58 => {
                stack.pop();
                stack.pop();
            }
            // ── clean producers / consumers (exact pop counts; results are clean) ──
            _ => {
                let (pops, push) = clean_stack_effect(op, bc, pc);
                if pops == usize::MAX {
                    // unknown opcode with a live taint → conservative escape
                    if stack.iter().any(|&t| t) || taint_any(&locals) {
                        return true;
                    }
                } else {
                    if popn(&mut stack, pops) {
                        return true; // a ref used as an arithmetic/other operand → escapes
                    }
                    if push {
                        stack.push(false);
                    }
                }
            }
        }
        pc += len;
    }
    false
}

/// One basic block's taint transfer for the CF-aware escape analysis. Runs the
/// SAME per-opcode rules as `object_escapes_straightline` over `[start,end)`, with an
/// operand stack that begins EMPTY (block-local — see the boundary rule below) and a
/// locals-taint set seeded from `in_taint`. Returns `(locals-taint at block exit,
/// escaped)`. `escaped` = a tainted operand reached a sink OR taint remains on the
/// operand stack at the block edge (stack taint is not carried across CFG edges →
/// conservative). Only the target site's `new` (pc == new_pc) is a taint SOURCE; any
/// OTHER `new` pushes a clean (untainted-for-this-site) reference. MUST stay in
/// lock-step with `object_escapes_straightline`.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn escape_transfer_block(
    bc: &[u8],
    pool: &ConstantPool,
    start: usize,
    end: usize,
    in_taint: &Taint,
    new_pc: usize,
) -> (Taint, u8) {
    // escape code: 0 = ok, 1 = real sink (putfield/putstatic/*astore/areturn/call-arg/athrow),
    // 2 = taint left on the operand stack at the block boundary (conservative).
    let mut stack: Vec<bool> = Vec::new();
    let mut locals = *in_taint;
    fn popn(s: &mut Vec<bool>, n: usize) -> bool {
        let mut t = false;
        for _ in 0..n {
            if s.pop().unwrap_or(false) {
                t = true;
            }
        }
        t
    }
    let mut pc = start;
    while pc < end {
        let op = bc[pc];
        let Some(len) = insn_len_at(bc, pc) else { return (locals, 1) };
        match op {
            0xbb => stack.push(pc == new_pc), // new: taint source only for the target site
            0x59 => {
                let t = *stack.last().unwrap_or(&false);
                stack.push(t); // dup
            }
            0x5f => {
                let n = stack.len();
                if n >= 2 {
                    stack.swap(n - 1, n - 2); // swap
                }
            }
            0x3a => {
                let t = popn(&mut stack, 1);
                let i = *bc.get(pc + 1).unwrap_or(&0) as usize;
                taint_set(&mut locals, i, t); // astore (a clean store clears the taint)
            }
            0x4b..=0x4e => {
                let t = popn(&mut stack, 1);
                taint_set(&mut locals, (op - 0x4b) as usize, t); // astore_0..3
            }
            0x19 => {
                let i = *bc.get(pc + 1).unwrap_or(&0) as usize;
                stack.push(taint_get(&locals, i)); // aload
            }
            0x2a..=0x2d => stack.push(taint_get(&locals, (op - 0x2a) as usize)), // aload_0..3
            0xb4 => {
                let _ = popn(&mut stack, 1); // getfield: own-field read is safe
                stack.push(false);
            }
            0xb5 => {
                let value = popn(&mut stack, 1);
                let _objref = popn(&mut stack, 1);
                if value {
                    return (locals, 1); // putfield of a tainted VALUE escapes
                }
            }
            0xb7 => {
                let m = match pool.member_ref(((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16) {
                    Some(m) => m,
                    None => return (locals, 1),
                };
                let nargs = crate::count_args(m.descriptor);
                let tainted_arg = popn(&mut stack, nargs);
                let recv = popn(&mut stack, 1);
                if tainted_arg || (recv && m.name != "<init>") {
                    return (locals, 1);
                }
                if ret_jt(m.descriptor).is_some() {
                    stack.push(false);
                }
            }
            0xb3 => {
                if popn(&mut stack, 1) {
                    return (locals, 1); // putstatic
                }
            }
            0xb0 => {
                if popn(&mut stack, 1) {
                    return (locals, 1); // areturn (block terminator)
                }
            }
            0x4f..=0x52 | 0x54..=0x56 => {
                if popn(&mut stack, 3) {
                    return (locals, 1); // *astore
                }
            }
            0x53 => {
                if popn(&mut stack, 3) {
                    return (locals, 1); // aastore
                }
            }
            0xb6 | 0xb8 | 0xb9 => {
                let m = match pool.member_ref(((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16) {
                    Some(m) => m,
                    None => return (locals, 1),
                };
                let nargs = crate::count_args(m.descriptor);
                let recv = if op == 0xb8 { 0 } else { 1 };
                if popn(&mut stack, nargs + recv) {
                    return (locals, 1);
                }
                if ret_jt(m.descriptor).is_some() {
                    stack.push(false);
                }
            }
            0xc0 => { /* checkcast: taint preserved */ }
            0x57 => {
                stack.pop();
            }
            0x58 => {
                stack.pop();
                stack.pop();
            }
            // ── control-flow terminators: taint-NEUTRAL (the CFG carries flow; a
            //    branch/compare/switch merely consumes its condition operands). These
            //    MUST be modelled here — the straight-line walk never reaches them
            //    (has_cf bails first), so clean_stack_effect treats them as unmodelled
            //    and would spuriously escape any live-tainted local. ──
            0x99..=0x9e => {
                popn(&mut stack, 1); // ifeq..ifle: pop the int condition
            }
            0x9f..=0xa6 => {
                popn(&mut stack, 2); // if_icmp*/if_acmp*: pop 2 (a ref COMPARE is not an escape)
            }
            0xc6 | 0xc7 => {
                popn(&mut stack, 1); // ifnull/ifnonnull: pop the ref (a null TEST is not an escape)
            }
            0xa7 | 0xc8 => { /* goto / goto_w: no stack effect */ }
            0xaa | 0xab => {
                popn(&mut stack, 1); // tableswitch/lookupswitch: pop the int index
            }
            0xac..=0xaf => {
                popn(&mut stack, 1); // i/l/f/d-return: pop the primitive result
            }
            0xb1 => { /* return void */ }
            0xbf => {
                if popn(&mut stack, 1) {
                    return (locals, 1); // athrow of a tainted throwable escapes
                }
            }
            _ => {
                let (pops, push) = clean_stack_effect(op, bc, pc);
                if pops == usize::MAX {
                    if stack.iter().any(|&t| t) || taint_any(&locals) {
                        return (locals, 1); // unmodelled op with live taint → conservative escape
                    }
                } else {
                    if popn(&mut stack, pops) {
                        return (locals, 1);
                    }
                    if push {
                        stack.push(false);
                    }
                }
            }
        }
        pc += len;
    }
    // Taint left on the operand stack can't be carried across a CFG edge → escape.
    if stack.iter().any(|&t| t) {
        return (locals, 2);
    }
    (locals, 0)
}

/// V2 CF-AWARE escape analysis (inert; feeds RAVA_DBG_VOA). Where the straight-line
/// `analyze_escape` bails to `ControlFlow` on ANY branch, this builds the bytecode
/// CFG (basic blocks from `find_leaders`) and runs a monotone taint-dataflow fixpoint,
/// so an object confined ACROSS branches is proven `Confined`. Returns `Confined` or
/// `Sink` per `new` site (never `ControlFlow` — CF is tracked).
///
/// SOUND UNDER-APPROXIMATION of "confined" (a false `Confined` would be a future
/// use-after-free once this drives codegen, so every uncertainty → `Sink`):
///  • EXCEPTION GATE: `has_exceptions` (≥1 exception-table entry) → every site `Sink`
///    (the implicit handler edges the CFG doesn't model could carry the ref anywhere).
///  • `find_leaders`/`switch_targets` `None` (jsr/ret/irreducible) → every site `Sink`.
///  • Taint alive on the operand STACK at a block boundary → that site `Sink`.
///  • LOOP-CARRIED: taint alive at the OUT of a block with a back-edge → `Sink` (the
///    object would outlive its iteration; per-iteration scalar replacement unsound).
///    CONSERVATIVE — a dead-but-not-reassigned local at a back-edge also trips this
///    (no liveness pass yet; refine later to lift more loop-body temporaries).
/// The taint join at merges is set UNION (may-hold-newborn); the sink rules then make
/// the whole-site verdict a sound "never escapes on any path". Transfer rules are in
/// `escape_transfer_block`, kept in lock-step with `object_escapes_straightline`.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn analyze_escape_cfg(
    bc: &[u8],
    pool: &ConstantPool,
    has_exceptions: bool,
) -> Vec<(usize, EscapeReason)> {
    let mut sites: Vec<usize> = Vec::new();
    {
        let mut pc = 0;
        while pc < bc.len() {
            if bc[pc] == 0xbb {
                sites.push(pc);
            }
            let Some(len) = insn_len_at(bc, pc) else {
                return sites.into_iter().map(|p| (p, EscapeReason::Sink)).collect();
            };
            pc += len;
        }
    }
    if sites.is_empty() {
        return Vec::new();
    }
    let dbg = std::env::var_os("RAVA_DBG_VOA").is_some();
    let all_sink = |sites: Vec<usize>| sites.into_iter().map(|p| (p, EscapeReason::Sink)).collect();
    if has_exceptions {
        if dbg { for _ in &sites { eprintln!("[VOA-WHY] exception_gate"); } }
        return all_sink(sites); // exception gate
    }
    let Some(mut leaders) = find_leaders(bc) else {
        if dbg { for _ in &sites { eprintln!("[VOA-WHY] irreducible"); } }
        return all_sink(sites);
    };
    leaders.sort_unstable();
    leaders.dedup();
    let nblk = leaders.len();
    let block_of = |pc: usize| -> usize {
        match leaders.binary_search(&pc) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        }
    };
    let block_end = |bi: usize| -> usize {
        if bi + 1 < nblk { leaders[bi + 1] } else { bc.len() }
    };
    let s16 = |p: usize| -> i32 { (((bc[p] as u16) << 8 | bc[p + 1] as u16) as i16) as i32 };
    let s32 = |p: usize| -> i32 {
        ((bc[p] as i32) << 24) | ((bc[p + 1] as i32) << 16) | ((bc[p + 2] as i32) << 8) | bc[p + 3] as i32
    };
    // Successors + a back-edge flag per block, from each block's terminator.
    let mut succ: Vec<Vec<usize>> = vec![Vec::new(); nblk];
    let mut has_backedge: Vec<bool> = vec![false; nblk];
    for bi in 0..nblk {
        let start = leaders[bi];
        let end = block_end(bi);
        let mut pc = start;
        let mut last = start;
        while pc < end {
            last = pc;
            let Some(len) = insn_len_at(bc, pc) else {
                return all_sink(sites);
            };
            pc += len;
        }
        let mut edge = |tgt_pc: usize, succ: &mut Vec<usize>, be: &mut bool| {
            let tb = block_of(tgt_pc);
            if !succ.contains(&tb) {
                succ.push(tb);
            }
            if tgt_pc <= last {
                *be = true; // branch to an earlier (or self) PC = back-edge
            }
        };
        let op = bc[last];
        match op {
            0x99..=0xa6 | 0xc6 | 0xc7 => {
                edge((last as i32 + s16(last + 1)) as usize, &mut succ[bi], &mut has_backedge[bi]);
                edge(last + 3, &mut succ[bi], &mut has_backedge[bi]); // fall-through
            }
            0xa7 => edge((last as i32 + s16(last + 1)) as usize, &mut succ[bi], &mut has_backedge[bi]),
            0xc8 => edge((last as i32 + s32(last + 1)) as usize, &mut succ[bi], &mut has_backedge[bi]),
            0xaa | 0xab => {
                let Some((deft, cases)) = switch_targets(bc, last) else {
                    return all_sink(sites);
                };
                edge(deft, &mut succ[bi], &mut has_backedge[bi]);
                for (_, t) in cases {
                    edge(t, &mut succ[bi], &mut has_backedge[bi]);
                }
            }
            0xac..=0xb1 | 0xbf => { /* return / athrow: no successor */ }
            _ => {
                if end < bc.len() {
                    edge(end, &mut succ[bi], &mut has_backedge[bi]); // fall-through
                }
            }
        }
    }
    // Per-site monotone taint fixpoint over locals.
    //
    // ⭐**THE FIXPOINT'S STATE IS ALLOCATED ONCE FOR THE METHOD, NOT ONCE PER SITE.** Each site
    // used to build three fresh vectors of `nblk` entries — two of them vectors of `BTreeSet` —
    // so the allocation count grew as SITES × BLOCKS on a method the caller already walked twice.
    // The state is per-site, but the STORAGE is not: it is reset between sites. With the taint a
    // 32-byte bitset the whole table is 64 bytes per block (a 100-block method: 6.4 KB, reused by
    // every site) and resetting it is three `fill`s.
    let mut in_taint: Vec<Taint> = vec![[0; 4]; nblk];
    let mut out_taint: Vec<Taint> = vec![[0; 4]; nblk];
    let mut processed: Vec<bool> = vec![false; nblk];
    let mut worklist: Vec<usize> = Vec::new();
    let mut out_sites: Vec<(usize, EscapeReason)> = Vec::with_capacity(sites.len());
    for &new_pc in &sites {
        let nb = block_of(new_pc);
        in_taint.fill([0; 4]);
        out_taint.fill([0; 4]);
        processed.fill(false);
        worklist.clear();
        worklist.push(nb);
        let mut escaped = false;
        let mut why = "confined";
        while let Some(bi) = worklist.pop() {
            let (out, esc) =
                escape_transfer_block(bc, pool, leaders[bi], block_end(bi), &in_taint[bi], new_pc);
            if esc != 0 {
                escaped = true;
                why = if esc == 2 { "stack_boundary" } else { "sink" };
                break;
            }
            if !processed[bi] || out != out_taint[bi] {
                processed[bi] = true;
                out_taint[bi] = out;
                for &s in &succ[bi] {
                    // Union only ever ADDS bits, so "the set changed" and "the count changed"
                    // are the same test they were.
                    let before = in_taint[s];
                    for k in 0..4 {
                        in_taint[s][k] |= out[k];
                    }
                    if in_taint[s] != before || !processed[s] {
                        worklist.push(s);
                    }
                }
            }
        }
        if !escaped {
            // Loop-carried: taint alive at any back-edge source outlives its iteration.
            for bi in 0..nblk {
                if has_backedge[bi] && taint_any(&out_taint[bi]) {
                    escaped = true;
                    why = "loop_carried";
                    break;
                }
            }
        }
        if dbg {
            eprintln!("[VOA-WHY] {why}");
        }
        out_sites.push((new_pc, if escaped { EscapeReason::Sink } else { EscapeReason::Confined }));
    }
    out_sites
}
