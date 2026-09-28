//! Immutable local-cell reconstruction states for protected JIT instructions.

use super::analysis::localtypes::{self, LT, LocalTypes};
use super::JT;
use crate::{Frame, Value, Vm};
use rava_classfile::Code;

mod owned;
pub(crate) use owned::{OwnedExceptionSite, OwnedExceptionSites};
pub(crate) mod publication;

/// Computed once per method, independently of its invocation history.
#[allow(dead_code)]
pub(crate) struct ExceptionFrameMap<'code> {
    code: &'code Code,
    types: LocalTypes,
}

#[allow(dead_code)]
impl<'code> ExceptionFrameMap<'code> {
    pub(crate) fn new(code: &'code Code, descriptor: &str, is_static: bool) -> Option<Self> {
        Some(Self { code, types: localtypes::compute(code, descriptor, is_static)? })
    }

    /// Refuse unprotected, unreachable, or ambiguously typed instruction states.
    pub(crate) fn site(&self, pc: usize) -> Option<ExceptionSite<'code, '_>> {
        describe_site(self.code, &self.types, pc)
    }
}

fn describe_site<'code, 'types>(code: &'code Code, types: &'types LocalTypes, pc: usize)
    -> Option<ExceptionSite<'code, 'types>>
{
    if !code.exceptions.iter().any(|h| h.start_pc as usize <= pc && pc < h.end_pc as usize) {
        return None;
    }
    let locals = types.at(pc)?;
    if locals.len() != code.max_locals as usize || locals.contains(&LT::Unknown) {
        return None;
    }
    Some(ExceptionSite { code, pc, locals })
}

/// The Code borrow prevents recycling its identity while this state is usable.
/// Generated-code ownership must retain that same method before publishing a
/// native site pointer; this type does not extend the class loader's lifetime.
#[allow(dead_code)]
pub(crate) struct ExceptionSite<'code, 'types> {
    code: &'code Code,
    pc: usize,
    locals: &'types [LT],
}

#[allow(dead_code)]
impl ExceptionSite<'_, '_> {
    /// Consume a pending guest fault only after a valid serial reconstruction.
    ///
    /// None means no transition occurred: pending state, fault latch and frame
    /// are unchanged. The caller retains the frame for every interpreter exit,
    /// including nonterminal exits. A native ABI adapter must represent those
    /// exits explicitly rather than treating them as a successful zero return.
    pub(crate) fn resume_pending(
        &self,
        vm: &mut Vm,
        frame: &mut Frame,
        cells: &[i64],
    ) -> Option<Result<crate::opcodes::RunExit, crate::VmError>> {
        // Guest handlers may allocate or call arbitrary methods. Do not consume
        // the worker's abort-associated exception or borrow scheduler state.
        #[cfg(feature = "next-gen-gc")]
        if crate::parallel::gilfree() {
            return None;
        }
        if vm.trampoline || !crate::execution_state::pending_throw_is_set()
            || !self.restore(vm, frame, cells) {
            return None;
        }
        // Owner-thread pending take and fault clear execute no guest code or GC.
        // resume_exception roots the frame and exception before handler lookup.
        let exception = vm.jit_pending_take().expect("serial pending exception disappeared");
        vm.heap.clear_jit_fault();
        Some(crate::opcodes::exception_entry::resume_exception(
            vm, self.code, frame, self.pc, crate::VmError::Throw(exception),
        ))
    }

    /// Restore locals from JVM-slot-indexed cells, discarding the old operand stack.
    ///
    /// The caller must keep the VM under its serial execution/root protocol and
    /// keep reference cells and the pending exception rooted until this returns.
    /// The existing reference decoder performs no guest execution or GC. This
    /// operation never takes the pending exception or clears the fault latch.
    /// On refusal the frame is unchanged. On success it must be registered before
    /// any subsequent operation capable of collecting guest objects.
    pub(crate) fn restore(&self, vm: &Vm, frame: &mut Frame, cells: &[i64]) -> bool {
        if frame.code != self.code as *const Code || frame.nlocals != self.locals.len()
            || cells.len() != self.locals.len()
        {
            return false;
        }
        let mut values = Vec::with_capacity(self.locals.len() + self.code.max_stack as usize);
        for (&kind, &cell) in self.locals.iter().zip(cells) {
            values.push(match kind {
                LT::Known(JT::Int) => Value::Int(cell as i32),
                LT::Known(JT::Long) => Value::Long(cell),
                LT::Known(JT::Float) => Value::Float(f32::from_bits(cell as u32)),
                LT::Known(JT::Double) => Value::Double(f64::from_bits(cell as u64)),
                LT::Known(JT::Ref) => super::unbox_ref(vm, cell, ""),
                // Never interpret an unused slot's arbitrary bits as a GC pointer.
                LT::Unset => Value::Null,
                LT::Unknown => return false,
            });
        }
        frame.buf = values;
        frame.pc = self.pc;
        true
    }
}

#[cfg(test)]
mod tests;
