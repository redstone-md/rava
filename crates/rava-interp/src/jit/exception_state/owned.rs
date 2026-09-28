//! Metadata ownership for an exception exit embedded in generated code.

use super::{describe_site, ExceptionFrameMap, ExceptionSite, Frame, LocalTypes, LT, Value, Vm};
use crate::class_loader::LoadedClass;
use crate::opcodes::RunExit;
use crate::VmError;
use std::sync::Arc;

/// One type analysis per compilation, shared by every protected instruction.
#[allow(dead_code)]
pub(crate) struct OwnedExceptionSites {
    class: Arc<LoadedClass>,
    method: usize,
    types: LocalTypes,
}

#[allow(dead_code)]
impl OwnedExceptionSites {
    pub(crate) fn new(class: Arc<LoadedClass>, method: usize) -> Option<Self> {
        let types = {
            let member = class.file.methods.get(method)?;
            ExceptionFrameMap::new(member.code.as_ref()?, &member.descriptor,
                member.access_flags & 8 != 0)?.types
        };
        Some(Self { class, method, types })
    }

    pub(crate) fn site(&self, pc: usize) -> Option<OwnedExceptionSite> {
        let code = self.class.file.methods[self.method].code.as_ref()?;
        let locals = describe_site(code, &self.types, pc)?.locals.into();
        Some(OwnedExceptionSite { class: Arc::clone(&self.class), method: self.method, pc, locals })
    }
}

/// Owns the defining class; no forged static Code borrow or bare class pointer.
///
/// Publication must retain this object at a stable address for as long as any
/// generated call can reach it, including calls through superseded JIT entries.
/// Retaining it only in an evictable JitFn/cache entry would not satisfy that.
#[allow(dead_code)]
pub(crate) struct OwnedExceptionSite {
    class: Arc<LoadedClass>,
    method: usize,
    pc: usize,
    locals: Box<[LT]>,
}

#[allow(dead_code)]
impl OwnedExceptionSite {
    fn view(&self) -> ExceptionSite<'_, '_> {
        // Class members are immutable after loader publication. The constructor
        // validated this index and the Arc keeps its Code storage alive.
        let code = self.class.file.methods[self.method].code.as_ref().unwrap();
        ExceptionSite { code, pc: self.pc, locals: &self.locals }
    }

    /// A valid empty activation owned by the caller, including its class pin.
    pub(crate) fn frame(&self) -> Frame {
        let site = self.view();
        let nlocals = site.code.max_locals as usize;
        let mut values = Vec::with_capacity(nlocals + site.code.max_stack as usize);
        values.resize(nlocals, Value::Null);
        Frame::from_parts(site.code, values, nlocals, Arc::clone(&self.class))
    }

    pub(crate) fn resume_pending(
        &self,
        vm: &mut Vm,
        frame: &mut Frame,
        cells: &[i64],
    ) -> Option<Result<RunExit, VmError>> {
        if !Arc::ptr_eq(&self.class, &frame.class) {
            return None;
        }
        self.view().resume_pending(vm, frame, cells)
    }
}

#[cfg(test)]
pub(crate) mod tests;
