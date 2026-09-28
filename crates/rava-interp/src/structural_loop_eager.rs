use rava_classfile::{Code, MemberInfo};

const EAGER_SCALAR: u8 = 1 << 0;
const EAGER_PARALLEL: u8 = 1 << 1;
const MAX_SCALAR_BYTES: usize = 512;
const MAX_SCALAR_BLOCKS: usize = 64;
const NO_CODE_FLAGS: u16 = 0x0020 | 0x0100 | 0x0400;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Admission {
    Normal,
    EagerScalar,
    EagerParallel,
}

fn scalar_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RAVA_NO_LOOP_EAGER").is_none())
}

/// Derive immutable scheduling facts before the class is published. Names, invocation counts,
/// timings, and previous compiler outcomes are deliberately absent from this boundary.
pub(crate) fn classify_loaded_methods(methods: &mut [MemberInfo]) {
    classify_methods(scalar_enabled(), methods);
}

fn classify_methods(scalar_on: bool, methods: &mut [MemberInfo]) {
    for method in methods {
        let Some(code) = method.code.as_ref() else {
            continue;
        };
        let flags = classify_method(scalar_on, method.access_flags, &method.descriptor, code);
        method.code.as_mut().expect("code presence checked").structural_flags = flags;
    }
}

fn classify_method(scalar_on: bool, access: u16, descriptor: &str, code: &Code) -> u8 {
    // The overwhelmingly common straight-line body pays one allocation-free bytecode walk.
    // `None` includes malformed control flow and opcodes the production compiler cannot decode.
    let Some(has_back_edge) = backedge_prefilter(&code.bytecode) else {
        return 0;
    };
    if !has_back_edge {
        return 0;
    }
    if access & NO_CODE_FLAGS != 0 || !code.exceptions.is_empty() || code.max_stack == 0 {
        return 0;
    }
    let Some(arg_slots) = descriptor_slots(descriptor) else {
        return 0;
    };
    let required = arg_slots + usize::from(access & 0x0008 == 0);
    if required > code.max_locals as usize || !locals_fit(code) {
        return 0;
    }
    #[cfg(feature = "next-gen-gc")]
    if crate::parallel_compile_policy::structural_candidate(code, descriptor, has_back_edge) {
        return EAGER_PARALLEL;
    }
    if !scalar_on || code.bytecode.len() > MAX_SCALAR_BYTES {
        return 0;
    }
    let Some((graph, blocks)) = decode_cfg(&code.bytecode) else {
        return 0;
    };
    if scalar_loop_candidate(&graph, blocks) {
        EAGER_SCALAR
    } else {
        0
    }
}

pub(crate) const fn admission(flags: u8) -> Admission {
    if flags & EAGER_PARALLEL != 0 {
        Admission::EagerParallel
    } else if flags & EAGER_SCALAR != 0 {
        Admission::EagerScalar
    } else {
        Admission::Normal
    }
}

/// The parallel feature switch controls execution, not structural eligibility.
pub(crate) fn compile_eager_now(flags: u8) -> bool {
    match admission(flags) {
        Admission::EagerScalar => true,
        #[cfg(feature = "next-gen-gc")]
        Admission::EagerParallel => crate::jit::jit_parallel_enabled(),
        _ => false,
    }
}

fn descriptor_slots(descriptor: &str) -> Option<usize> {
    fn field(b: &[u8], i: &mut usize, void_ok: bool) -> Option<usize> {
        let c = *b.get(*i)?;
        *i += 1;
        match c {
            b'V' if void_ok => Some(0),
            b'B' | b'C' | b'F' | b'I' | b'S' | b'Z' => Some(1),
            b'J' | b'D' => Some(2),
            b'L' => {
                let n = b.get(*i..)?.iter().position(|&x| x == b';')?;
                if n == 0 {
                    return None;
                }
                *i += n + 1;
                Some(1)
            }
            b'[' => {
                while b.get(*i) == Some(&b'[') {
                    *i += 1;
                }
                let _ = field(b, i, false)?;
                Some(1)
            }
            _ => None,
        }
    }
    let b = descriptor.as_bytes();
    if b.first() != Some(&b'(') {
        return None;
    }
    let mut i = 1;
    let mut slots = 0usize;
    while b.get(i) != Some(&b')') {
        slots = slots.checked_add(field(b, &mut i, false)?)?;
    }
    i += 1;
    let _ = field(b, &mut i, true)?;
    (i == b.len()).then_some(slots)
}

/// Find at least one retreating edge without allocating. A positive result is only a prefilter:
/// `decode_cfg` subsequently proves that every target is an instruction boundary. A negative
/// result can return immediately because a straight-line method cannot be eager.
fn backedge_prefilter(bc: &[u8]) -> Option<bool> {
    fn scan_target(bc: &[u8], pc: usize, at: usize, found: &mut bool) -> Option<()> {
        let off = crate::jit::be_i32(bc, at)? as i64;
        let q = target(pc, off, bc.len())?;
        *found |= q <= pc;
        Some(())
    }

    let mut found = false;
    let mut pc = 0usize;
    while pc < bc.len() {
        let op = bc[pc];
        let len = match op {
            0x99..=0xa7 | 0xc6 | 0xc7 => {
                let off = i16::from_be_bytes([*bc.get(pc + 1)?, *bc.get(pc + 2)?]) as i64;
                let q = target(pc, off, bc.len())?;
                found |= q <= pc;
                3
            }
            // The baseline compiler has no c8 arm. Recognising its length but admitting it would
            // turn a structural scheduling proof into a guaranteed compile refusal.
            0xc8 => return None,
            0xaa => {
                let base = (pc & !3).checked_add(4)?;
                if bc.get(pc + 1..base)?.iter().any(|&b| b != 0) { return None; }
                scan_target(bc, pc, base, &mut found)?;
                let low = crate::jit::be_i32(bc, base.checked_add(4)?)?;
                let high = crate::jit::be_i32(bc, base.checked_add(8)?)?;
                if high < low { return None; }
                let count = (high as i64 - low as i64 + 1) as usize;
                let end = (base + 12).checked_add(count.checked_mul(4)?)?;
                if end > bc.len() { return None; }
                for at in (base + 12..end).step_by(4) {
                    scan_target(bc, pc, at, &mut found)?;
                }
                end - pc
            }
            0xab => {
                let base = (pc & !3).checked_add(4)?;
                if bc.get(pc + 1..base)?.iter().any(|&b| b != 0) { return None; }
                scan_target(bc, pc, base, &mut found)?;
                let count = crate::jit::be_i32(bc, base.checked_add(4)?)?;
                if count < 0 { return None; }
                let end = (base + 8).checked_add((count as usize).checked_mul(8)?)?;
                if end > bc.len() { return None; }
                let mut last = None;
                for pair in 0..count as usize {
                    let at = base + 8 + pair * 8;
                    let key = crate::jit::be_i32(bc, at)?;
                    if last.is_some_and(|prev| key <= prev) { return None; }
                    last = Some(key);
                    scan_target(bc, pc, at + 4, &mut found)?;
                }
                end - pc
            }
            _ => crate::jit::insn_len_at(bc, pc)?,
        };
        pc = pc.checked_add(len)?;
        if pc > bc.len() { return None; }
    }
    Some(found)
}

fn locals_fit(code: &Code) -> bool {
    let bc = &code.bytecode;
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        let (local, width) = match op {
            0x15..=0x19 | 0x36..=0x3a => (*bc.get(pc + 1).unwrap_or(&u8::MAX) as usize,
                usize::from(matches!(op, 0x16 | 0x18 | 0x37 | 0x39)) + 1),
            0x1a..=0x1d => ((op - 0x1a) as usize, 1),
            0x1e..=0x21 => ((op - 0x1e) as usize, 2),
            0x22..=0x25 => ((op - 0x22) as usize, 1),
            0x26..=0x29 => ((op - 0x26) as usize, 2),
            0x2a..=0x2d => ((op - 0x2a) as usize, 1),
            0x3b..=0x3e => ((op - 0x3b) as usize, 1),
            0x3f..=0x42 => ((op - 0x3f) as usize, 2),
            0x43..=0x46 => ((op - 0x43) as usize, 1),
            0x47..=0x4a => ((op - 0x47) as usize, 2),
            0x4b..=0x4e => ((op - 0x4b) as usize, 1),
            0x84 => (*bc.get(pc + 1).unwrap_or(&u8::MAX) as usize, 1),
            _ => (0, 0),
        };
        if width != 0 && local + width > code.max_locals as usize {
            return false;
        }
        let Some(len) = instruction_len(bc, pc) else {
            return false;
        };
        pc += len;
    }
    true
}

#[derive(Clone)]
struct Insn {
    pc: usize,
    op: u8,
    succ: Vec<usize>,
}

fn instruction_len(bc: &[u8], pc: usize) -> Option<usize> {
    crate::jit::insn_len_at(bc, pc)
        .filter(|len| pc.checked_add(*len).is_some_and(|end| end <= bc.len()))
}

fn target(pc: usize, offset: i64, len: usize) -> Option<usize> {
    let t = (pc as i64).checked_add(offset)?;
    (0..len as i64).contains(&t).then_some(t as usize)
}

fn decode_cfg(bc: &[u8]) -> Option<(Vec<Insn>, usize)> {
    if bc.is_empty() {
        return None;
    }
    let mut pcs = Vec::new();
    let mut pc = 0;
    while pc < bc.len() {
        pcs.push(pc);
        pc += instruction_len(bc, pc)?;
    }
    let mut at = vec![usize::MAX; bc.len()];
    for (i, &p) in pcs.iter().enumerate() { at[p] = i; }
    let mut leaders = vec![false; bc.len()];
    leaders[0] = true;
    let mut out = Vec::with_capacity(pcs.len());
    for (i, &p) in pcs.iter().enumerate() {
        let op = bc[p];
        let next = pcs.get(i + 1).copied();
        let mut raw = Vec::new();
        let mut branch_targets = false;
        match op {
            0x99..=0xa6 | 0xc6 | 0xc7 => {
                let off = i16::from_be_bytes([*bc.get(p + 1)?, *bc.get(p + 2)?]) as i64;
                raw.push(target(p, off, bc.len())?);
                raw.push(next?);
                leaders[next?] = true;
                branch_targets = true;
            }
            0xa7 => {
                let off = i16::from_be_bytes([*bc.get(p + 1)?, *bc.get(p + 2)?]) as i64;
                raw.push(target(p, off, bc.len())?);
                if let Some(n) = next { leaders[n] = true; }
                branch_targets = true;
            }
            0xaa | 0xab => {
                let (default, cases) = crate::jit::switch_targets(bc, p)?;
                raw.push(default);
                raw.extend(cases.into_iter().map(|(_, t)| t));
                if let Some(n) = next { leaders[n] = true; }
                branch_targets = true;
            }
            0xac..=0xb1 | 0xbf => if let Some(n) = next { leaders[n] = true; },
            _ => raw.push(next?),
        }
        let mut succ = Vec::with_capacity(raw.len());
        for (edge, q) in raw.into_iter().enumerate() {
            let j = *at.get(q)?;
            if j == usize::MAX {
                return None;
            }
            // Conditional fallthrough was already marked above; all other listed edges are
            // explicit branch targets and therefore begin blocks.
            if branch_targets && !(matches!(op, 0x99..=0xa6 | 0xc6 | 0xc7) && edge == 1) {
                leaders[q] = true;
            }
            if !succ.contains(&j) {
                succ.push(j);
            }
        }
        out.push(Insn { pc: p, op, succ });
    }
    let blocks = pcs.iter().filter(|&&p| leaders[p]).count();
    Some((out, blocks))
}

fn primitive_cycle_op(op: u8) -> bool {
    matches!(op,
        0x00 | 0x02..=0x0f | 0x10..=0x11 |
        0x15..=0x18 | 0x1a..=0x29 | 0x36..=0x39 | 0x3b..=0x4a |
        0x57..=0x5f | 0x60..=0x84 | 0x85..=0x98 |
        0x99..=0xa4 | 0xa7 | 0xaa..=0xab | 0xac..=0xaf | 0xb1)
}

fn scalar_loop_candidate(graph: &[Insn], blocks: usize) -> bool {
    if blocks > MAX_SCALAR_BLOCKS {
        return false;
    }
    let mut reachable = vec![false; graph.len()];
    let mut stack = vec![0];
    while let Some(v) = stack.pop() {
        if reachable[v] {
            continue;
        }
        reachable[v] = true;
        stack.extend(graph[v].succ.iter().copied());
    }
    let mut pred = vec![Vec::new(); graph.len()];
    for (v, insn) in graph.iter().enumerate() {
        for &w in &insn.succ {
            pred[w].push(v);
        }
    }

    // Dominators are a structural class-load fact. For each reachable node, begin with every
    // reachable node and converge by predecessor intersection; the graph is capped at 512 bytes.
    let mut dom = vec![reachable.clone(); graph.len()];
    dom[0].fill(false);
    dom[0][0] = true;
    loop {
        let mut changed = false;
        for v in 1..graph.len() {
            if !reachable[v] {
                dom[v].fill(false);
                continue;
            }
            let mut next = reachable.clone();
            let mut has_pred = false;
            for &p in &pred[v] {
                if !reachable[p] { continue; }
                has_pred = true;
                for (bit, &pbit) in next.iter_mut().zip(&dom[p]) { *bit &= pbit; }
            }
            if !has_pred { return false; }
            next[v] = true;
            if next != dom[v] {
                dom[v] = next;
                changed = true;
            }
        }
        if !changed { break; }
    }

    let mut any_back_edge = false;
    for (source, insn) in graph.iter().enumerate() {
        if !reachable[source] { continue; }
        for &header in &insn.succ {
            if graph[header].pc > insn.pc { continue; }
            any_back_edge = true;
            // A retreating edge is a natural-loop back edge only when its header dominates its
            // source. This rejects multi-entry/irreducible SCCs, including nested ones.
            if !dom[source][header] { return false; }
            let mut in_loop = vec![false; graph.len()];
            in_loop[header] = true;
            in_loop[source] = true;
            let mut work = if source == header { Vec::new() } else { vec![source] };
            while let Some(v) = work.pop() {
                for &p in &pred[v] {
                    if !reachable[p] || in_loop[p] { continue; }
                    if !dom[p][header] { return false; }
                    in_loop[p] = true;
                    if p != header { work.push(p); }
                }
            }
            if in_loop.iter().enumerate().any(|(v, &inside)|
                inside && !primitive_cycle_op(graph[v].op)) {
                return false;
            }
        }
    }
    any_back_edge
}

#[cfg(test)]
mod tests {
    use super::*;

    fn method(name: &str, descriptor: &str, bytecode: &[u8]) -> MemberInfo {
        MemberInfo { access_flags: 0x0008, name: name.into(), descriptor: descriptor.into(),
            code: Some(Code { max_stack: 8, max_locals: 8, bytecode: bytecode.to_vec(),
                ..Code::default() }) }
    }

    fn verdict(mut m: MemberInfo) -> Admission {
        classify_methods(true, std::slice::from_mut(&mut m));
        admission(m.code.unwrap().structural_flags)
    }

    #[test]
    fn identical_body_ignores_name_and_count() {
        let body = [0x03, 0x3b, 0x1a, 0x10, 0x0a, 0xa2, 0x00, 0x09, 0x84, 0x00, 0x01,
            0xa7, 0xff, 0xf7, 0xb1];
        for name in ["main", "<clinit>", "helper"] {
            assert_eq!(verdict(method(name, "()V", &body)), Admission::EagerScalar);
        }
    }

    #[test]
    fn calls_outside_loop_are_allowed_but_inside_are_refused() {
        let outside = [0xb8, 0, 1, 0x03, 0x3b, 0x1a, 0x06, 0xa2, 0, 9, 0x84, 0, 1,
            0xa7, 0xff, 0xf8, 0xb8, 0, 2, 0xb1];
        let inside = [0x03, 0x3b, 0x1a, 0x06, 0xa2, 0, 12, 0xb8, 0, 1, 0x84, 0, 1,
            0xa7, 0xff, 0xf5, 0xb1];
        assert_eq!(verdict(method("a", "()V", &outside)), Admission::EagerScalar);
        assert_eq!(verdict(method("b", "()V", &inside)), Admission::Normal);
    }

    #[test]
    fn malformed_exception_synchronized_oversized_and_irreducible_are_refused() {
        assert_eq!(verdict(method("x", "()V", &[0xa7, 0, 1])), Admission::Normal);
        assert_eq!(verdict(method("x", "not-a-descriptor", &[0xa7, 0, 0])), Admission::Normal);
        let mut bad_frame = method("x", "(J)V", &[0xa7, 0, 0]);
        bad_frame.code.as_mut().unwrap().max_locals = 1;
        assert_eq!(verdict(bad_frame), Admission::Normal);
        let mut exceptional = method("x", "()V", &[0xa7, 0, 0]);
        exceptional.code.as_mut().unwrap().exceptions.push(rava_classfile::ExceptionHandler {
            start_pc: 0, end_pc: 3, handler_pc: 0, catch_type: 0 });
        assert_eq!(verdict(exceptional), Admission::Normal);
        let mut synchronized = method("x", "()V", &[0xa7, 0, 0]);
        synchronized.access_flags |= 0x0020;
        assert_eq!(verdict(synchronized), Admission::Normal);
        let mut oversized = vec![0x00; MAX_SCALAR_BYTES];
        oversized.extend([0xa7, 0, 0]);
        assert_eq!(verdict(method("x", "()V", &oversized)), Admission::Normal);
        let irreducible = [0x03, 0x99, 0, 6, 0xa7, 0, 6, 0xa7, 0, 3, 0xa7, 0xff, 0xfd];
        assert_eq!(verdict(method("x", "()V", &irreducible)), Admission::Normal);
    }

    #[test]
    fn prefilter_handles_branches_and_switch_but_rejects_goto_w() {
        assert_eq!(backedge_prefilter(&[0x03, 0xb1]), Some(false));
        assert_eq!(backedge_prefilter(&[0xa7, 0, 0]), Some(true));
        let table = [
            0xaa, 0, 0, 0, // opcode + alignment padding
            0, 0, 0, 0, // default -> pc 0
            0, 0, 0, 0, // low
            0, 0, 0, 0, // high
            0, 0, 0, 0, // case 0 -> pc 0
        ];
        assert_eq!(backedge_prefilter(&table), Some(true));
        assert_eq!(verdict(method("x", "()V", &[0xc8, 0, 0, 0, 0])), Admission::Normal);
        assert_eq!(verdict(method("x", "()V", &[0xaa, 0, 0, 0])), Admission::Normal);
    }

    #[test]
    fn nested_irreducible_graph_is_refused() {
        let graph = vec![
            Insn { pc: 0, op: 0x03, succ: vec![1] },
            Insn { pc: 3, op: 0x99, succ: vec![2, 5] },
            Insn { pc: 6, op: 0x99, succ: vec![3, 4] },
            Insn { pc: 9, op: 0xa7, succ: vec![4] },
            Insn { pc: 12, op: 0xa7, succ: vec![3, 1] },
            Insn { pc: 15, op: 0xb1, succ: vec![] },
        ];
        assert!(!scalar_loop_candidate(&graph, 6));
    }

    #[cfg(feature = "next-gen-gc")]
    #[test]
    fn parallel_map_candidate_gets_parallel_bit() {
        let body = [0x03, 0x3e, 0x1d, 0x1c, 0xa2, 0, 0x13, 0x2a, 0x1d, 0x2b, 0x1d,
            0x2f, 0x2b, 0x1d, 0x2f, 0x69, 0x50, 0x84, 3, 1, 0xa7, 0xff, 0xee, 0xb1];
        assert_eq!(verdict(method("anything", "([J[JI)V", &body)), Admission::EagerParallel);
    }
}
