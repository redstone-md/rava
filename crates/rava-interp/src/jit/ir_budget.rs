//! Structural ceiling checked during emission, before finalization and codegen.
//! These counts bound admitted IR, not total process memory.

const MAX_BLOCKS: usize = 16_384;
const MAX_VALUES: usize = 262_144;

fn admits(blocks: usize, values: usize) -> bool {
    blocks <= MAX_BLOCKS && values <= MAX_VALUES
}

pub(crate) fn allows(function: &cranelift::codegen::ir::Function) -> bool {
    // Count allocated blocks, including leaders not yet inserted in the layout.
    // Both counts are O(1): this predicate also runs at opcode boundaries.
    admits(function.dfg.num_blocks(), function.dfg.num_values())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn allocated_uninserted_blocks_consume_budget() {
        let mut function = cranelift::codegen::ir::Function::new();
        for _ in 0..MAX_BLOCKS {
            function.dfg.make_block();
        }
        assert_eq!(function.layout.blocks().count(), 0);
        assert!(allows(&function));
        function.dfg.make_block();
        assert!(!allows(&function));
    }
    #[test]
    fn admits_exact_limits() {
        assert!(admits(0, 0));
        assert!(admits(MAX_BLOCKS, MAX_VALUES));
    }
    #[test]
    fn rejects_either_dimension_independently() {
        assert!(!admits(MAX_BLOCKS + 1, 0));
        assert!(!admits(0, MAX_VALUES + 1));
        assert!(!admits(usize::MAX, usize::MAX));
    }
    #[test]
    fn rejects_observed_expansion_before_and_after_finalization() {
        assert!(!admits(296_233, 1_331_566));
        assert!(!admits(296_233, 1_508_830));
        assert!(admits(8_962, 41_202));
    }
}
