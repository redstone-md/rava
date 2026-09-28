//! Bounded cooperative progress for full compiled methods.
use crate::aot_clif::*;
use cranelift::codegen::ir::StackSlot;

pub(super) const INTERVAL: i64 = 1024;

pub(super) fn loop_headers(bc: &[u8]) -> Option<std::collections::HashSet<usize>> {
    let mut result = std::collections::HashSet::new();
    let mut pc = 0;
    while pc < bc.len() {
        let len = insn_len_at(bc, pc)?;
        match bc[pc] {
            0x99..=0xa7 | 0xc6 | 0xc7 => {
                let target = pc as i64 + s16(bc, pc + 1) as i64;
                if target >= 0 && target <= pc as i64 { result.insert(target as usize); }
            }
            0xaa | 0xab => {
                let (default, cases) = crate::jit::switch_targets(bc, pc)?;
                for target in std::iter::once(default).chain(cases.into_iter().map(|(_, t)| t)) {
                    if target <= pc { result.insert(target); }
                }
            }
            _ => {}
        }
        pc += len;
    }
    Some(result)
}

pub(super) fn emit(
    b: &mut FunctionBuilder,
    budget: StackSlot,
    callback: cranelift::codegen::ir::FuncRef,
    stack: &mut [(Value, bool, bool)],
    locals: Value,
    max_locals: u32,
) {
    // Spill before the diamond: both paths reload the same rooted cells.
    spill(b, stack, locals, max_locals);
    let count = b.ins().stack_load(I64, budget, 0);
    let next = b.ins().iadd_imm(count, -1);
    b.ins().stack_store(next, budget, 0);
    let due = b.ins().icmp_imm(IntCC::Equal, next, 0);
    let poll = b.create_block();
    let resume = b.create_block();
    b.ins().brif(due, poll, &[], resume, &[]);
    b.switch_to_block(poll);
    b.seal_block(poll);
    let reset = b.ins().iconst(I64, INTERVAL);
    b.ins().stack_store(reset, budget, 0);
    b.ins().call(callback, &[]);
    b.ins().jump(resume, &[]);
    b.switch_to_block(resume);
    b.seal_block(resume);
    reload(b, stack, locals, max_locals);
}

pub(crate) extern "C" fn poll() -> i64 {
    if crate::parallel::gilfree() || crate::frames::native_depth() != 0
        || !crate::gil::has_real_threads()
    {
        return 0;
    }
    let allowed = crate::jit::vm_ref_worker_sanctioned()
        .is_some_and(|vm| !vm.in_pump && !vm.trampoline);
    // End the VM borrow before another thread can enter the runtime.
    if allowed {
        crate::gil::release();
        std::thread::yield_now();
        crate::gil::acquire();
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    thread_local! {
        static CELLS: Cell<*mut i64> = const { Cell::new(std::ptr::null_mut()) };
        static POLLS: Cell<usize> = const { Cell::new(0) };
    }
    extern "C" fn forward() -> i64 {
        POLLS.with(|n| n.set(n.get() + 1));
        CELLS.with(|cells| unsafe {
            // Simulate a collector forwarding both the local and operand root.
            *cells.get() = 32;
            *cells.get().add(2) = 32;
        });
        0
    }

    #[test]
    fn loop_poll_reloads_forwarded_operand_and_resets_budget() {
        // Keep a reference on the operand stack while counting 2050 iterations.
        let bc = [0x2a, 0x03, 0x3c, 0x84, 1, 1, 0x1b, 0x11, 8, 2,
            0xa1, 0xff, 0xf9, 0xb0];
        let (code, relocs, constants) = compile_method(&bc, "Progress", 2,
            &|_| None, &|_| None, &|_| None, &|_| None).unwrap();
        let obj = crate::cnp::CompiledObj::from_full(code, relocs, constants);
        let symbol = crate::jit::aot_symtab().iter()
            .position(|(name, _)| *name == "rava_aot_progress").unwrap();
        let (function, constants) = obj.link_full(&|index| {
            assert_eq!(index as usize, symbol);
            forward as extern "C" fn() -> i64 as usize
        }, &mut |_| Some(0)).unwrap();
        let mut cells = [16, 0, 0, 0];
        CELLS.with(|slot| slot.set(cells.as_mut_ptr()));
        POLLS.with(|n| n.set(0));
        let result = unsafe { function.run_const(cells.as_mut_ptr(), constants.as_ptr()) };
        CELLS.with(|slot| slot.set(std::ptr::null_mut()));
        assert_eq!(result, 32);
        assert_eq!(cells[1], 2050);
        assert_eq!(POLLS.with(Cell::get), 2);
    }

    #[test]
    fn only_backward_edges_require_progress() {
        assert!(loop_headers(&[0x03, 0xac]).unwrap().is_empty());
        assert!(loop_headers(&[0xa7, 0, 3, 0xb1]).unwrap().is_empty());
        assert_eq!(loop_headers(&[0xa7, 0, 0]).unwrap(), [0].into_iter().collect());
    }
}
