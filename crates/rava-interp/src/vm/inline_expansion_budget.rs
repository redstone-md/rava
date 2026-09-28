//! Bound bytecode expansion by emitted call sites, not constant-pool identities.

use std::collections::{HashMap, HashSet};

trait Tree: Sized {
    fn bytecode(&self) -> &[u8];
    fn nested(&self) -> &HashMap<u16, Self>;
}

impl Tree for crate::jit::InlineCallee {
    fn bytecode(&self) -> &[u8] { &self.bytecode }
    fn nested(&self) -> &HashMap<u16, Self> { &self.nested }
}

/// Preserve complete subtrees. Removing an interior edge could break a callee
/// whose emitter requires every nested call to inline.
pub(super) fn constrain(
    bytecode: &[u8],
    targets: &mut HashMap<u16, crate::jit::InlineCallee>,
    limit: usize,
) {
    constrain_tree(bytecode, targets, limit);
}

fn occurrences(bytecode: &[u8]) -> Option<Vec<(u16, usize)>> {
    let mut sites = Vec::<(u16, usize)>::new();
    let mut positions = HashMap::new();
    let mut pc = 0;
    while pc < bytecode.len() {
        let len = crate::jit::insn_len_at(bytecode, pc)?;
        if len == 0 || pc.checked_add(len)? > bytecode.len() { return None; }
        if matches!(bytecode[pc], 0xb6..=0xb9) {
            let cp = u16::from_be_bytes([*bytecode.get(pc + 1)?, *bytecode.get(pc + 2)?]);
            let index = *positions.entry(cp).or_insert_with(|| {
                sites.push((cp, 0));
                sites.len() - 1
            });
            sites[index].1 += 1;
        }
        pc += len;
    }
    Some(sites)
}

fn expanded_cost<T: Tree>(tree: &T, limit: usize) -> Option<usize> {
    let mut cost = tree.bytecode().len();
    if cost > limit { return None; }
    for (cp, count) in occurrences(tree.bytecode())? {
        if let Some(child) = tree.nested().get(&cp) {
            let child_cost = expanded_cost(child, limit - cost)?;
            cost = cost.checked_add(child_cost.checked_mul(count)?)?;
            if cost > limit { return None; }
        }
    }
    Some(cost)
}

fn constrain_tree<T: Tree>(bytecode: &[u8], targets: &mut HashMap<u16, T>, limit: usize) {
    if targets.is_empty() { return; }
    let Some(sites) = occurrences(bytecode) else {
        targets.clear();
        return;
    };
    let mut remaining = limit;
    let mut keep = HashSet::new();
    // First bytecode occurrence gives deterministic priority, independent of
    // HashMap iteration, receiver history and prior compiler outcomes.
    for (cp, count) in sites {
        let Some(target) = targets.get(&cp) else { continue };
        let Some(cost) = expanded_cost(target, remaining)
            .and_then(|cost| cost.checked_mul(count)) else { continue };
        if cost <= remaining {
            remaining -= cost;
            keep.insert(cp);
        }
    }
    targets.retain(|cp, _| keep.contains(cp));
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Body { code: Vec<u8>, children: HashMap<u16, Body> }
    impl Tree for Body {
        fn bytecode(&self) -> &[u8] { &self.code }
        fn nested(&self) -> &HashMap<u16, Self> { &self.children }
    }
    fn leaf() -> Body {
        Body { code: vec![0x03, 0x04, 0x60, 0xac], children: HashMap::new() }
    }

    #[test]
    fn repeated_pool_index_pays_for_every_expansion() {
        let code = [0xb8, 0, 1, 0xb8, 0, 1, 0xb1];
        let mut targets = HashMap::from([(1, leaf())]);
        constrain_tree(&code, &mut targets, 7);
        assert!(targets.is_empty());
        targets.insert(1, leaf());
        constrain_tree(&code, &mut targets, 8);
        assert_eq!(targets.len(), 1);
    }

    #[test]
    fn nested_multiplicity_is_charged_without_pruning_children() {
        let tree = Body {
            code: vec![0xb8, 0, 2, 0xb8, 0, 2, 0xb1],
            children: HashMap::from([(2, leaf())]),
        };
        assert_eq!(expanded_cost(&tree, 14), None);
        assert_eq!(expanded_cost(&tree, 15), Some(15));
        let mut targets = HashMap::from([(1, tree)]);
        constrain_tree(&[0xb8, 0, 1, 0xb8, 0, 1, 0xb1], &mut targets, 29);
        assert!(targets.is_empty());
    }

    #[test]
    fn first_bytecode_site_wins_independent_of_map_order() {
        let mut targets = HashMap::from([(1, leaf()), (2, leaf())]);
        constrain_tree(&[0xb8, 0, 2, 0xb8, 0, 1, 0xb1], &mut targets, 4);
        assert!(targets.contains_key(&2));
        assert!(!targets.contains_key(&1));
    }

    #[test]
    fn truncated_invocation_does_not_retain_unbounded_plan() {
        let mut targets = HashMap::from([(1, leaf())]);
        constrain_tree(&[0xb8, 0], &mut targets, usize::MAX);
        assert!(targets.is_empty());
    }
}
