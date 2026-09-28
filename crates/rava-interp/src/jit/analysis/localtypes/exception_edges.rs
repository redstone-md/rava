//! Validated conservative exceptional successors for local-type propagation.

use rava_classfile::{Code, ExceptionHandler};

pub(super) struct ExceptionEdges<'a> {
    handlers: &'a [ExceptionHandler],
}

impl<'a> ExceptionEdges<'a> {
    pub(super) fn new(code: &'a Code, lengths: &[u32]) -> Option<Self> {
        if code.exceptions.is_empty() {
            return Some(Self { handlers: &code.exceptions });
        }
        let end = code.bytecode.len();
        let instruction = |pc: usize| pc < end && lengths.get(pc).is_some_and(|&n| n != 0);
        // Bound analysis work using immutable input, never execution history.
        // Each sweep scans this table once per reached instruction (at most 8 sweeps).
        const MAX_EDGE_CHECKS_PER_SWEEP: usize = 1 << 20;
        let instructions = lengths.iter().filter(|&&n| n != 0).count();
        if instructions.checked_mul(code.exceptions.len())? > MAX_EDGE_CHECKS_PER_SWEEP {
            return None;
        }
        for h in &code.exceptions {
            let start = h.start_pc as usize;
            let stop = h.end_pc as usize;
            if start >= stop || !instruction(start)
                || !(stop == end || instruction(stop))
                || !instruction(h.handler_pc as usize)
            {
                return None;
            }
        }
        Some(Self { handlers: &code.exceptions })
    }

    pub(super) fn at(&self, pc: usize) -> impl Iterator<Item = usize> + '_ {
        // Use every covering entry, including shadowed catches. No exception-type
        // or may-throw speculation: over-approximation can only reduce precision.
        // Even nonthrowing stores publish their incoming state before normal
        // transfer; publishing the updated type could invent a handler-local type.
        self.handlers.iter().filter_map(move |h| {
            ((h.start_pc as usize) <= pc && pc < h.end_pc as usize)
                .then_some(h.handler_pc as usize)
        })
    }
}
