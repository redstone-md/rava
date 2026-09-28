//! Operand-stack signatures on CFG edges, independent of invocation history.
use super::*;
use cranelift::codegen::ir::{Block, BlockArg};
use std::collections::VecDeque;

type CellShape = (bool, bool);
type StackCell = (Value, bool, bool);

pub(super) struct BlockStacks {
    shapes: Vec<Option<Vec<CellShape>>>,
    pending: VecDeque<usize>,
}

impl BlockStacks {
    pub(super) fn new(count: usize) -> Self {
        let mut shapes = vec![None; count];
        shapes[0] = Some(Vec::new());
        Self { shapes, pending: VecDeque::from([0]) }
    }

    pub(super) fn next(&mut self) -> Option<usize> { self.pending.pop_front() }

    pub(super) fn incoming(&self, b: &FunctionBuilder, index: usize, block: Block) -> Vec<StackCell> {
        b.block_params(block).iter().copied().zip(self.shapes[index].as_ref().unwrap())
            .map(|(value, &(reference, wide))| (value, reference, wide)).collect()
    }

    pub(super) fn edge(&mut self, b: &mut FunctionBuilder, blocks: &[Block],
        target: usize, stack: &[StackCell]) -> Option<Vec<BlockArg>> {
        let shape: Vec<_> = stack.iter().map(|(_, reference, wide)| (*reference, *wide)).collect();
        match self.shapes.get_mut(target)? {
            Some(expected) if *expected != shape => return None,
            Some(_) => {},
            slot @ None => {
                for _ in &shape { b.append_block_param(blocks[target], I64); }
                *slot = Some(shape);
                self.pending.push_back(target);
            }
        }
        Some(stack.iter().map(|(value, _, _)| (*value).into()).collect())
    }

    pub(super) fn branch(&mut self, b: &mut FunctionBuilder, blocks: &[Block],
        taken: Value, yes: usize, no: usize, stack: &[StackCell]) -> Option<()> {
        let yes_args = self.edge(b, blocks, yes, stack)?;
        let no_args = self.edge(b, blocks, no, stack)?;
        b.ins().brif(taken, blocks[yes], &yes_args, blocks[no], &no_args);
        Some(())
    }

    pub(super) fn complete(&self) -> bool { self.shapes.iter().all(Option::is_some) }
}
