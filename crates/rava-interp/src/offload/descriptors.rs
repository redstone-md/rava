//! Descriptor and branch-target helpers the analysis needs.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::offload::*;

/// Freshness of a value that a modelled opcode pushes.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum F {
    /// Freshly allocated in this method, still private/unpublished. `Some(cp)` = the
    /// value came from `new #cp`, so its EXACT dynamic type is known — a virtual/
    /// interface call on it (while still fresh-tracked) DEVIRTUALISES to that concrete
    /// class (increment 2d, F2). `None` = a fresh array / no known concrete class.
    Fresh(Option<u16>),
    /// The method's own receiver `this` (local 0) in a ctor / element-ownership fork. Writable-
    /// private like Fresh; its concrete type is `self_class`, so a virtual/interface call on it
    /// DEVIRTUALISES to the concrete impl (transitive this-call devirt — unblocks e.g.
    /// `initCache`'s `this.asState()` when the element's class is concrete).
    This,
    /// The induction variable of a counted loop being analysed in `iv_local` mode. Used ONLY as an
    /// array index: `arr[iv]` writes are index-partitioned (distinct across the loop's distinct iv
    /// values → disjoint), so they are safe on a SHARED array. Shared-like everywhere else.
    Iv,
    /// GILFREE-RINGS: a lambda minted by a LambdaMetafactory `invokedynamic` at
    /// cp `idx` in THIS method, still fresh/private. A SAM invoke on it
    /// dispatches to EXACTLY the indy site's impl handle (rava's indy service
    /// links that handle), so the invoke arm queries the impl method instead of
    /// the unenumerable functional-interface closure. Shared-like everywhere
    /// else; joins reset it like every freshness fact.
    Lambda(u16),
    Shared,
}

/// The declared REFERENCE parameter types of a method descriptor: an `L<name>;`
/// parameter yields `<name>`; an ARRAY parameter yields its full `[`-prefixed
/// descriptor (never resolvable to a pure `toString` — the caller's dispatch
/// query rejects it). Primitive parameters are skipped (pure Rust stringify).
pub(crate) fn ref_param_types(desc: &str) -> Vec<&str> {
    let b = desc.as_bytes();
    let mut out = Vec::new();
    let mut i = match desc.find('(') {
        Some(p) => p + 1,
        None => return out,
    };
    while i < b.len() && b[i] != b')' {
        let start = i;
        while i < b.len() && b[i] == b'[' {
            i += 1;
        }
        match b.get(i) {
            Some(b'L') => {
                let name_start = i + 1;
                while i < b.len() && b[i] != b';' {
                    i += 1;
                }
                if start < name_start - 1 {
                    out.push(&desc[start..=i]); // array-of-refs: full [L...; descriptor
                } else {
                    out.push(&desc[name_start..i]); // plain L<name>;
                }
                i += 1;
            }
            Some(_) => {
                if i > start {
                    out.push(&desc[start..=i]); // primitive array, e.g. [I
                }
                i += 1;
            }
            None => break,
        }
    }
    out
}

/// (argument slot count, return slot count) of a method descriptor. Long/double
/// arguments and returns count as 2 slots; void returns 0.
pub(crate) fn desc_slots(desc: &str) -> (usize, usize) {
    let b = desc.as_bytes();
    let mut i = match desc.find('(') {
        Some(p) => p + 1,
        None => return (0, 0),
    };
    let mut args = 0usize;
    while i < b.len() && b[i] != b')' {
        let mut arr = false;
        while i < b.len() && b[i] == b'[' {
            arr = true;
            i += 1;
        }
        match b.get(i) {
            Some(b'L') => {
                while i < b.len() && b[i] != b';' {
                    i += 1;
                }
                i += 1;
                args += 1;
            }
            Some(b'J') | Some(b'D') => {
                i += 1;
                args += if arr { 1 } else { 2 };
            }
            Some(_) => {
                i += 1;
                args += 1;
            }
            None => break,
        }
    }
    // return
    let ret = match desc.rfind(')').and_then(|p| b.get(p + 1)) {
        Some(b'V') | None => 0,
        Some(b'J') | Some(b'D') => 2,
        _ => 1,
    };
    (args, ret)
}

/// All explicit jump targets in `bc` (join points where freshness must reset).
/// None if a branch instruction is malformed.
pub(crate) fn branch_targets(bc: &[u8]) -> Option<std::collections::HashSet<usize>> {
    let mut t = std::collections::HashSet::new();
    let mut pc = 0usize;
    while pc < bc.len() {
        let op = bc[pc];
        match op {
            // 2-byte signed offset branches
            0x99..=0xa8 | 0xc6 | 0xc7 => {
                let off = (((bc.get(pc + 1)? .to_owned() as i16) << 8) | bc.get(pc + 2)?.to_owned() as i16) as i32;
                t.insert((pc as i32 + off) as usize);
            }
            // 4-byte offset (goto_w / jsr_w)
            0xc8 | 0xc9 => {
                let off = ((bc.get(pc + 1)?.to_owned() as i32) << 24)
                    | ((bc.get(pc + 2)?.to_owned() as i32) << 16)
                    | ((bc.get(pc + 3)?.to_owned() as i32) << 8)
                    | (bc.get(pc + 4)?.to_owned() as i32);
                t.insert((pc as i32 + off) as usize);
            }
            0xaa => {
                // tableswitch: pad to 4-byte boundary, then default(4), low(4), high(4), (high-low+1) offsets(4)
                let base = pc + 1 + ((4 - ((pc + 1) % 4)) % 4);
                let def = i32be(bc, base)?;
                t.insert((pc as i32 + def) as usize);
                let low = i32be(bc, base + 4)?;
                let high = i32be(bc, base + 8)?;
                let n = (high - low + 1).max(0) as usize;
                for k in 0..n {
                    let off = i32be(bc, base + 12 + k * 4)?;
                    t.insert((pc as i32 + off) as usize);
                }
            }
            0xab => {
                // lookupswitch: pad, default(4), npairs(4), npairs*(match(4), offset(4))
                let base = pc + 1 + ((4 - ((pc + 1) % 4)) % 4);
                let def = i32be(bc, base)?;
                t.insert((pc as i32 + def) as usize);
                let npairs = i32be(bc, base + 4)?.max(0) as usize;
                for k in 0..npairs {
                    let off = i32be(bc, base + 8 + k * 8 + 4)?;
                    t.insert((pc as i32 + off) as usize);
                }
            }
            _ => {}
        }
        pc += crate::jit::insn_len_at(bc, pc)?;
    }
    Some(t)
}
