//! Structural non-null facts for repeated direct-local field reads.
//!
//! A successful `aload local; getfield` proves that local's current reference is
//! non-null until a store or control-flow boundary. Facts are intentionally
//! block-local and direct-local only; anything more complex fails closed.

use crate::jit::*;
use std::collections::HashSet;

/// `RAVA_NO_NONNULL_GETFIELD=1` restores every null guard without changing the
/// proof itself or consulting guest execution history.
pub(crate) fn nonnull_getfield_elision_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RAVA_NO_NONNULL_GETFIELD").is_none())
}

/// PCs of `getfield` instructions whose receiver local was already dereferenced
/// successfully in the same basic block, with no local store in between.
pub(crate) fn repeated_nonnull_getfields(bc: &[u8]) -> HashSet<usize> {
    let Some(mut leaders) = find_leaders(bc) else {
        return HashSet::new();
    };
    leaders.sort_unstable();
    leaders.dedup();
    leaders.retain(|&pc| pc < bc.len());
    leaders.push(bc.len());

    let mut proven = HashSet::new();
    for bounds in leaders.windows(2) {
        let (start, end) = (bounds[0], bounds[1]);
        let mut nonnull_locals = HashSet::new();
        let mut last_aload = None;
        let mut pc = start;
        while pc < end {
            let op = bc[pc];
            let Some(len) = insn_len_at(bc, pc) else {
                return HashSet::new();
            };
            if len == 0 || pc + len > end {
                return HashSet::new();
            }

            match op {
                0x19 => last_aload = bc.get(pc + 1).copied().map(usize::from),
                0x2a..=0x2d => last_aload = Some((op - 0x2a) as usize),
                0xb4 => {
                    if let Some(local) = last_aload {
                        if nonnull_locals.contains(&local) {
                            proven.insert(pc);
                        }
                        // If execution reaches this instruction's continuation, its
                        // receiver was non-null regardless of the field's value.
                        nonnull_locals.insert(local);
                    }
                    last_aload = None;
                }
                // Any local store may reuse a previously-reference-typed slot (wide
                // stores can overlap two slots), so discard every fact conservatively.
                0x36..=0x3a | 0x3b..=0x4e | 0xc4 => {
                    nonnull_locals.clear();
                    last_aload = None;
                }
                _ => last_aload = None,
            }
            pc += len;
        }
    }
    proven
}

#[cfg(test)]
mod tests {
    use super::repeated_nonnull_getfields;

    #[test]
    fn repeated_direct_local_getfield_is_proven_only_after_first_success() {
        let code = [
            0x2b, 0xb4, 0x00, 0x01, 0x57, // aload_1; getfield #1; pop
            0x2b, 0xb4, 0x00, 0x02, 0x57, // aload_1; getfield #2; pop
            0xb1,
        ];
        assert_eq!(repeated_nonnull_getfields(&code), [6].into_iter().collect());
    }

    #[test]
    fn local_store_invalidates_the_nonnull_fact() {
        let code = [
            0x2b, 0xb4, 0x00, 0x01, 0x57, // successful dereference of local 1
            0x01, 0x4c, // aconst_null; astore_1
            0x2b, 0xb4, 0x00, 0x02, 0x57, // new value may be null
            0xb1,
        ];
        assert!(repeated_nonnull_getfields(&code).is_empty());
    }

    #[test]
    fn nonnull_facts_do_not_cross_control_flow_blocks() {
        let code = [
            0x2b, 0xb4, 0x00, 0x01, 0x57, // first dereference
            0xa7, 0x00, 0x03, // goto pc 8
            0x2b, 0xb4, 0x00, 0x02, 0x57, // new basic block
            0xb1,
        ];
        assert!(repeated_nonnull_getfields(&code).is_empty());
    }

    #[test]
    fn different_locals_do_not_share_nonnull_facts() {
        let code = [
            0x2b, 0xb4, 0x00, 0x01, 0x57, // local 1 dereference
            0x2c, 0xb4, 0x00, 0x02, 0x57, // local 2 may be null
            0xb1,
        ];
        assert!(repeated_nonnull_getfields(&code).is_empty());
    }
}
