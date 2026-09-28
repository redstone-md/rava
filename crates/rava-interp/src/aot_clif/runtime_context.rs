//! Call-site information for exceptions raised by offline-compiled methods.
use std::cell::Cell;
use std::marker::PhantomData;
use rava_classfile::Code;

thread_local! {
    static CODE: Cell<*const Code> = const { Cell::new(std::ptr::null()) };
}

pub(crate) struct MethodContext<'a> {
    previous: *const Code,
    lifetime: PhantomData<&'a Code>,
}

impl<'a> MethodContext<'a> {
    pub(crate) fn enter(code: &'a Code) -> Self {
        Self { previous: CODE.with(|slot| slot.replace(code)), lifetime: PhantomData }
    }
}

impl Drop for MethodContext<'_> {
    fn drop(&mut self) { CODE.with(|slot| slot.set(self.previous)); }
}

pub(crate) extern "C" fn null_fault(caller: i64, pc: i64, op: i64, depth: i64) -> i64 {
    let Some(vm) = crate::jit::vm_ref_worker_sanctioned() else { return 0 };
    if vm.jit_pending_is_set() { return 0; }
    let class = unsafe { &*(caller as *const crate::LoadedClass) };
    let message = CODE.with(|slot| {
        let code = slot.get();
        if code.is_null() { return None; }
        // MethodContext borrows this Code through the complete native call.
        let bytes = unsafe { &(*code).bytecode };
        let pc = pc as usize;
        let source = crate::npe::source_at(bytes, pc, depth as usize, &class.file.pool);
        Some(match op as u8 {
            0xb4 | 0xb5 | 0xb6 | 0xb7 | 0xb9 => {
                let cp = u16::from_be_bytes([*bytes.get(pc + 1)?, *bytes.get(pc + 2)?]);
                let member = class.file.pool.member_ref(cp)?;
                match op as u8 {
                    0xb4 => crate::npe::read_field(member.name, source),
                    0xb5 => crate::npe::assign_field(member.name, source),
                    _ => crate::npe::invoke(member.owner, member.name, member.descriptor, source),
                }
            }
            0x2e..=0x35 => crate::npe::array_load(crate::npe::array_kind_for_op(op as u8), source),
            0x4f..=0x56 => crate::npe::array_store(crate::npe::array_kind_for_op(op as u8), source),
            0xbe => crate::npe::array_length(source),
            0xbf => crate::npe::throw_null(),
            _ => return None,
        })
    }).unwrap_or_default();
    crate::jit::set_pending(vm, "java/lang/NullPointerException", &message);
    0
}

/// Reads exception state without allocating, collecting or consuming it.
pub(crate) extern "C" fn pending() -> i64 {
    let Some(vm) = crate::jit::vm_ref_worker_sanctioned() else { return 0 };
    i64::from(vm.jit_pending_is_set())
}

pub(crate) extern "C" fn primitive_store(array: i64, index: i64, cell: i64, op: i64) -> i64 {
    match op as u8 {
        0x50 => crate::jit::rava_lastore(array, index, cell),
        0x52 => crate::jit::rava_dastore(array, index, cell),
        0x51 => {
            let Some(vm) = crate::jit::vm_ref_worker_sanctioned() else { return 0 };
            if vm.jit_pending_is_set() { return 0; }
            let Some(handle) = crate::jit::untag_array(array) else {
                crate::jit::set_pending(vm, "java/lang/NullPointerException", "fastore");
                return 0;
            };
            let length = vm.heap.array_len(handle).unwrap_or(0);
            if index < 0 || index as usize >= length {
                crate::jit::set_pending(vm, "java/lang/ArrayIndexOutOfBoundsException",
                    &format!("Index {} out of bounds for length {length}", index as i32));
                return 0;
            }
            vm.heap.array_set(handle, index as usize, crate::Value::Float(f32::from_bits(cell as u32)));
            0
        }
        _ => crate::jit::rava_iastore(array, index, cell),
    }
}

pub(crate) extern "C" fn checkcast(caller: i64, cp: i64, cell: i64) -> i64 {
    if cell == 0 { return 0; } // Null is assignment-compatible with every reference type.
    let Some(vm) = crate::jit::vm_ref_worker_sanctioned() else { return 0 };
    if vm.jit_pending_is_set() { return 0; }
    let class = unsafe { &*(caller as *const crate::LoadedClass) };
    let value = crate::jit::unbox_ref(vm, cell, "");
    match crate::opcodes::checkcast_reject(vm, class, cp as u16, &value) {
        Ok(Some(message)) => crate::jit::set_pending(vm, "java/lang/ClassCastException", &message),
        Ok(None) => {},
        Err(_) => crate::jit::set_pending(vm, "java/lang/InternalError", "invalid AOT checkcast"),
    }
    cell
}
