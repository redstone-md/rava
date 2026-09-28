//! Direct leaf-call classification for the AOT direct-call arm (Tracer 1).
//!
//! Two independent gates, both fail-closed:
//! 1. `DirectLeafCall::from_member` — purely syntactic. A primitive-shaped
//!    `(I/J/Z)* -> I/J/Z` descriptor is only a CANDIDATE; it proves nothing
//!    about the target's actual body (or even its existence as a leaf).
//! 2. `is_no_gc_primitive_leaf` — the real gate. Validates the resolved
//!    target's access flags and walks its bytecode, admitting only bounded,
//!    callback-free, no-GC primitive computation under the direct System V
//!    ABI `(hidden consts_ptr: i64, args: i64...) -> i64`.
//!
//! Never claim direct execution based on gate 1 alone: the link stage must
//! resolve the leaf pointer (0 on any refusal) so the call site keeps its
//! unchanged `rava_invoke` fallback.

use rava_classfile::Code;

/// Direct-ABI call-convention budget: the hidden consts pointer takes RDI,
/// leaving the five remaining SysV integer registers for user arguments.
const MAX_USER_ARGS: usize = 5;

/// Conservative upper bound on an admissible leaf body. Oversized bodies are
/// rejected outright; AOT emits loop-progress callbacks for real loops, so any
/// loop-shaped body must stay on `rava_invoke` anyway.
const MAX_LEAF_BODY: usize = 256;

/// Argument/result cell kind of a direct leaf. All kinds are represented as
/// i64 cells (ints/booleans sign-extended into the low bits, matching the
/// interpreter's `Value::Int` slot layout).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrimKind {
    Int,
    Long,
    Bool,
}

/// Parsed shape of an admissible direct-leaf descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrimitiveSignature {
    pub(crate) params: Vec<PrimKind>,
    pub(crate) ret: PrimKind,
}

impl PrimitiveSignature {
    /// Exact parse of `(I/J/Z)* -> I/J/Z`. Rejects every other type letter
    /// (including `B`/`C`/`S` — fail-closed even though they would widen),
    /// arrays, objects, void, floats/doubles, and empty/malformed descriptors.
    pub(crate) fn parse(descriptor: &str) -> Option<Self> {
        let bytes = descriptor.as_bytes();
        if bytes.first() != Some(&b'(') {
            return None;
        }
        let mut params = Vec::new();
        let mut i = 1;
        loop {
            match bytes.get(i)? {
                b')' => {
                    i += 1;
                    break;
                }
                b'I' => {
                    params.push(PrimKind::Int);
                    i += 1;
                }
                b'Z' => {
                    params.push(PrimKind::Bool);
                    i += 1;
                }
                b'J' => {
                    params.push(PrimKind::Long);
                    i += 1;
                }
                _ => return None,
            }
        }
        if params.len() > MAX_USER_ARGS {
            return None;
        }
        let ret = match bytes.get(i)? {
            b'I' => PrimKind::Int,
            b'Z' => PrimKind::Bool,
            b'J' => PrimKind::Long,
            _ => return None,
        };
        // Nothing may trail the return type.
        if i + 1 != bytes.len() {
            return None;
        }
        Some(Self { params, ret })
    }
}

/// A symbolic direct-call candidate resolved from a call site's member ref.
/// Kept as the compile-time annotation on `InvokeInfo`; the runtime link
/// resolution decides whether the pointer constant is a real leaf function or
/// 0 (disabled / missing / unloaded / uninitialized / not a leaf).
#[derive(Clone, Debug)]
pub struct DirectLeafCall {
    pub(crate) owner: String,
    pub(crate) name: String,
    pub(crate) descriptor: String,
    pub(crate) signature: PrimitiveSignature,
}

impl DirectLeafCall {
    /// Compile-time candidate for a call site whose member ref is
    /// primitive-shaped. DOES NOT prove the target has a native leaf artifact
    /// or that it is static/non-initializer; target resolution validates the
    /// loaded method later and resolves to 0 on any refusal.
    pub fn from_member(owner: &str, name: &str, descriptor: &str) -> Option<Self> {
        if owner.is_empty() || name.is_empty() || name.starts_with('<') {
            return None;
        }
        let signature = PrimitiveSignature::parse(descriptor)?;
        Some(Self {
            owner: owner.to_string(),
            name: name.to_string(),
            descriptor: descriptor.to_string(),
            signature,
        })
    }
}

/// Actual-target gate: is this method (by its own metadata and body) a
/// bounded, callback-free, no-GC primitive leaf eligible for the direct ABI?
/// Fails closed unless static, unsynchronized, non-native/non-abstract, free
/// of exception handlers, with enough local slots, using only supported
/// primitive computation/branch/return bytecodes, with no calls, no
/// GC-capable or faulting callbacks, and no backwards branch.
///
/// Name checks (non-initializer, "non-special" methods) live with the
/// descriptor shape: `from_member` rejects `<init>`/`<clinit>` up front.
pub fn is_no_gc_primitive_leaf(descriptor: &str, access_flags: u16, code: &Code) -> bool {
    if PrimitiveSignature::parse(descriptor).is_none() {
        return false;
    }
    const ACC_STATIC: u16 = 0x0008;
    const REJECTED: u16 = 0x0020 // synchronized
        | 0x0040 // bridge
        | 0x0080 // varargs
        | 0x0100 // native
        | 0x0400 // abstract
        | 0x1000; // synthetic
    if access_flags & ACC_STATIC == 0 || access_flags & REJECTED != 0 {
        return false;
    }
    // Any handler means the body can throw into frame machinery the direct
    // ABI does not provide.
    if !code.exceptions.is_empty() {
        return false;
    }
    if code.bytecode.len() > MAX_LEAF_BODY || code.bytecode.is_empty() {
        return false;
    }
    leaf_body_ok(descriptor, code)
}

/// Minimal typed cell model for the stack walk.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cell {
    Int,
    Long,
}

fn is_branch(op: u8) -> bool {
    matches!(op, 0x99..=0xa4 | 0xa7)
}

/// Instruction lengths for the ADMITTED opcode set only (unknown → None, which
/// rejects). 0x6c/0x6d/0x70/0x71 (idiv/ldiv/irem/lrem) are deliberately absent:
/// a divide-by-zero trap needs runtime machinery the direct leaf cannot reach.
fn insn_len(op: u8) -> Option<usize> {
    Some(match op {
        0x00 // nop
        | 0x02..=0x08 // iconst
        | 0x0a..=0x0b // lconst
        | 0x1a..=0x21 // iload_n / lload_n
        | 0x60 | 0x61 // iadd / ladd
        | 0x64 | 0x65 // isub / lsub
        | 0x68 | 0x69 // imul / lmul
        | 0x74 | 0x75 // ineg / lneg
        | 0x78..=0x7d // ishl/iushr/lshl/lshr/lushr
        | 0x7e..=0x83 // iand..lxor
        | 0x85 | 0x88 // i2l / l2i
        | 0x94 // lcmp
        | 0xac | 0xad => 1, // ireturn / lreturn
        0x10 | 0x15 | 0x16 => 2, // bipush / iload / lload
        0x11 | 0x99..=0xa4 | 0xa7 => 3, // sipush / if<cc> / if_icmp<cc> / goto
        _ => return None,
    })
}

fn branch_target(bc: &[u8], pc: usize) -> Option<usize> {
    let off = i64::from(i16::from_be_bytes([*bc.get(pc + 1)?, *bc.get(pc + 2)?]));
    Some((pc as i64 + off) as usize)
}

/// Typed-stack walk of the body. Blocks share the SAME empty-stack-boundary
/// model as the AOT translator (it bails on a non-empty stack across a
/// branch), so every leader must be entered with an empty stack. Local loads
/// are restricted to parameter slots (the direct ABI passes args in registers;
/// there is no locals buffer). Returns false on any inconsistency.
fn leaf_body_ok(descriptor: &str, code: &Code) -> bool {
    let Some(sig) = PrimitiveSignature::parse(descriptor) else {
        return false;
    };
    let bc = &code.bytecode;

    // Per-slot parameter-kind map (category-2 params take two slots; the wide
    // second half is unusable). Every local load must read the slot kind it
    // claims: an iload from a long start or an lload from an int/boolean
    // parameter slot (or the high half of a long) is a verifier violation.
    let param_kinds: Vec<Option<Cell>> = {
        let slot = {
            let mut slot = 0usize;
            for p in &sig.params {
                slot += if *p == PrimKind::Long { 2 } else { 1 };
            }
            slot
        };
        if (code.max_locals as usize) < slot {
            return false;
        }
        let mut kinds = vec![None; code.max_locals as usize];
        let mut i = 0usize;
        for p in &sig.params {
            match p {
                PrimKind::Long => {
                    kinds[i] = Some(Cell::Long);
                    i += 2; // high half stays None (unusable)
                }
                _ => {
                    kinds[i] = Some(Cell::Int);
                    i += 1;
                }
            }
        }
        kinds
    };
    let param_kind = |i: usize| -> Option<Cell> { param_kinds.get(i).copied().flatten() };

    // Pass 1: decode every instruction; collect branch targets and the
    // fall-through leaders after each branch.
    let mut leaders: Vec<usize> = vec![0];
    let mut pc = 0usize;
    while pc < bc.len() {
        let op = bc[pc];
        let Some(len) = insn_len(op) else { return false };
        if pc + len > bc.len() {
            return false;
        }
        if is_branch(op) {
            let Some(target) = branch_target(bc, pc) else { return false };
            if op != 0xa7 && pc + len >= bc.len() {
                return false;
            }
            // Forward branches only; AOT emits loop-progress callbacks, so any
            // backwards edge (or a target off the body) refuses the leaf.
            if target <= pc || target >= bc.len() {
                return false;
            }
            leaders.push(target);
            leaders.push(pc + len);
        }
        pc += len;
    }
    leaders.sort_unstable();
    leaders.dedup();

    // Pass 2: typed-stack walk.
    let mut stack: Vec<Cell> = Vec::new();
    let mut visited: Vec<usize> = Vec::new();
    let mut targets: Vec<usize> = Vec::new();
    let mut terminated = false; // the first block is in progress until a terminator
    pc = 0;
    while pc < bc.len() {
        if leaders.contains(&pc) {
            if !terminated && !stack.is_empty() {
                return false; // non-empty stack across a block boundary
            }
            stack.clear();
            terminated = false;
        }
        visited.push(pc);
        let op = bc[pc];
        macro_rules! pop {
            ($kind:expr) => {
                if stack.pop() != Some($kind) {
                    return false;
                }
            };
        }
        match op {
            0x00 => {} // nop
            0x02..=0x08 => stack.push(Cell::Int),
            0x0a | 0x0b => stack.push(Cell::Long),
            0x10 => stack.push(Cell::Int), // bipush
            0x11 => stack.push(Cell::Int), // sipush
            0x15 => {
                // iload: slot must hold an int/boolean parameter.
                if param_kind(bc[pc + 1] as usize) != Some(Cell::Int) {
                    return false;
                }
                stack.push(Cell::Int);
            }
            0x16 => {
                // lload: slot must hold a long parameter start.
                if param_kind(bc[pc + 1] as usize) != Some(Cell::Long) {
                    return false;
                }
                stack.push(Cell::Long);
            }
            0x1a..=0x1d => {
                if param_kind((op - 0x1a) as usize) != Some(Cell::Int) {
                    return false;
                }
                stack.push(Cell::Int);
            }
            0x1e..=0x21 => {
                if param_kind((op - 0x1e) as usize) != Some(Cell::Long) {
                    return false;
                }
                stack.push(Cell::Long);
            }
            0x60 | 0x64 | 0x68 | 0x7e | 0x80 | 0x82 => {
                pop!(Cell::Int);
                pop!(Cell::Int);
                stack.push(Cell::Int);
            }
            0x78 | 0x7a | 0x7c => {
                pop!(Cell::Int);
                pop!(Cell::Int);
                stack.push(Cell::Int);
            }
            0x61 | 0x65 | 0x69 | 0x7f | 0x81 | 0x83 => {
                pop!(Cell::Long);
                pop!(Cell::Long);
                stack.push(Cell::Long);
            }
            0x79 | 0x7b | 0x7d => {
                pop!(Cell::Int);
                pop!(Cell::Long);
                stack.push(Cell::Long);
            }
            0x74 => {
                pop!(Cell::Int);
                stack.push(Cell::Int);
            }
            0x75 => {
                pop!(Cell::Long);
                stack.push(Cell::Long);
            }
            0x85 => {
                pop!(Cell::Int);
                stack.push(Cell::Long);
            }
            0x88 => {
                pop!(Cell::Long);
                stack.push(Cell::Int);
            }
            0x94 => {
                pop!(Cell::Long);
                pop!(Cell::Long);
                stack.push(Cell::Int);
            }
            0x99..=0x9e => {
                // if<cc> vs zero.
                pop!(Cell::Int);
                if !stack.is_empty() {
                    return false;
                }
                let Some(target) = branch_target(bc, pc) else { return false };
                targets.push(target);
                terminated = true;
            }
            0x9f..=0xa4 => {
                // if_icmp<cc>.
                pop!(Cell::Int);
                pop!(Cell::Int);
                if !stack.is_empty() {
                    return false;
                }
                let Some(target) = branch_target(bc, pc) else { return false };
                targets.push(target);
                terminated = true;
            }
            0xa7 => {
                if !stack.is_empty() {
                    return false;
                }
                let Some(target) = branch_target(bc, pc) else { return false };
                targets.push(target);
                terminated = true;
            }
            0xac => {
                // ireturn: only for int/boolean results.
                if !matches!(sig.ret, PrimKind::Int | PrimKind::Bool) {
                    return false;
                }
                pop!(Cell::Int);
                if !stack.is_empty() {
                    return false;
                }
                terminated = true;
            }
            0xad => {
                // lreturn: only for long results.
                if sig.ret != PrimKind::Long {
                    return false;
                }
                pop!(Cell::Long);
                if !stack.is_empty() {
                    return false;
                }
                terminated = true;
            }
            _ => return false,
        }
        // max_stack counts SLOTS (a long occupies two), not stack entries.
        let slot_depth = stack
            .iter()
            .map(|c| if *c == Cell::Long { 2 } else { 1 })
            .sum::<usize>();
        if slot_depth > code.max_stack as usize {
            return false;
        }
        pc += match insn_len(op) { Some(l) => l, None => return false };
    }
    // The body must end on a terminator — falling off the end would execute
    // past the leaf under the direct ABI.
    if !terminated {
        return false;
    }
    // Branch targets must sit on decoded instruction boundaries (pass 2 visits
    // the same boundaries pass 1 decoded).
    targets.iter().all(|t| visited.contains(t))
}

#[cfg(test)]
#[path = "direct_leaf_tests.rs"]
mod tests;
