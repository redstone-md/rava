//! Linux bridge for the copy-and-patch tier.

use crate::*;

const MAX_CELLS: usize = 64;

#[derive(Clone, Copy)]
enum CnpReturn {
    Int,
    Ref { array: bool },
}

#[derive(Clone, Copy)]
struct CnpSignature {
    inputs: usize,
    ref_mask: u64,
    ret: CnpReturn,
}

pub(crate) struct CnpEntry {
    compiled: cnp::CompiledFn,
    signature: CnpSignature,
    local_cells: usize,
    stack_cells: usize,
}

impl Vm {
    /// Run a structurally supported int/reference method as SysV x86-64 code.
    /// Unsupported signatures, opcodes, or values refuse to the ordinary tier.
    pub(crate) fn cnp_run(
        &mut self,
        class: &Arc<LoadedClass>,
        code: &Code,
        descriptor: &str,
        is_static: bool,
        locals: &[Value],
    ) -> Option<Result<Value, VmError>> {
        let key = code as *const Code as usize;
        let signature = match self.cnp_cache.get(&key) {
            Some(Some(entry)) => entry.signature,
            Some(None) => return None,
            None => cnp_signature(descriptor, is_static)?,
        };
        if locals.len() < signature.inputs {
            return None;
        }
        for (slot, value) in locals.iter().take(signature.inputs).enumerate() {
            let is_ref = signature.ref_mask & (1 << slot) != 0;
            if (!is_ref && !matches!(value, Value::Int(_)))
                || (is_ref
                    && !matches!(
                        value,
                        Value::Str(_)
                            | Value::Ref(_)
                            | Value::Array(_)
                            | Value::Obj(_)
                            | Value::Null
                            | Value::StdOut
                            | Value::StdErr
                    ))
            {
                return None;
            }
        }

        let local_cells = (code.max_locals as usize).max(signature.inputs);
        let stack_cells = code.max_stack as usize + 2;
        if local_cells > MAX_CELLS || stack_cells > MAX_CELLS {
            Self::debug_assert_not_gilfree("cnp_cache");
            self.cnp_cache.insert(key, None);
            return None;
        }

        // Locals and operand stack share one registered root range. A getfield
        // null fault constructs an exception and may collect before native code
        // returns, so callback-capable CNP bodies need the same root discipline
        // as Cranelift bodies.
        let box_base = frames::box_len();
        let mut cells = [0i64; MAX_CELLS * 2];
        for (slot, value) in locals.iter().take(signature.inputs).enumerate() {
            cells[slot] = if signature.ref_mask & (1 << slot) != 0 {
                jit::box_ref(self, value)
            } else {
                let Value::Int(value) = value else { unreachable!() };
                *value as i64
            };
        }
        frames::cells_push((cells.as_ptr(), local_cells + stack_cells));

        if !self.cnp_cache.contains_key(&key) {
            let entry = self
                .cnp_context(class, code)
                .and_then(|ctx| cnp::compile_ctx(&code.bytecode, &ctx)?.link())
                .map(|compiled| CnpEntry {
                    compiled,
                    signature,
                    local_cells,
                    stack_cells,
                });
            Self::debug_assert_not_gilfree("cnp_cache");
            self.cnp_cache.insert(key, entry);
        }

        let Some(entry) = self.cnp_cache.get(&key).and_then(Option::as_ref) else {
            frames::cells_pop();
            frames::box_truncate(box_base);
            return None;
        };
        let code_ptr = entry.compiled.code_ptr();
        let ret = entry.signature.ret;
        let local_cells = entry.local_cells;
        let stack_cells = entry.stack_cells;

        let mut frame = cnp::CnpFrame {
            stack: cells[local_cells..local_cells + stack_cells].as_mut_ptr(),
            sp: 0,
            locals: cells[..local_cells].as_mut_ptr(),
        };
        self.heap.clear_jit_fault();
        let prev = jit::set_current_vm(self as *mut Vm);
        let run: extern "C" fn(*mut cnp::CnpFrame) -> i64 =
            unsafe { std::mem::transmute(code_ptr) };
        let bits = run(&mut frame);
        jit::set_current_vm(prev);

        let result = match ret {
            CnpReturn::Int => Value::Int(bits as i32),
            CnpReturn::Ref { array } => jit::unbox_ref_kind(self, bits, array),
        };
        let pending = crate::execution_state::pending_error_take();
        frames::cells_pop();
        frames::box_truncate(box_base);
        Some(match pending {
            Some(error) => Err(error),
            None => Ok(result),
        })
    }

    fn cnp_context(&self, class: &LoadedClass, code: &Code) -> Option<cnp::CnpCtx> {
        let fields = jit::resolve_fields(
            &code.bytecode,
            &class.file.pool,
            &mut |owner, name| {
                let id = self.resolve_field_id(owner, name);
                (id, self.layout_slot(owner, id))
            },
        )?;
        let mut mapped = std::collections::HashMap::with_capacity(fields.len());
        for (index, (id, kind, _)) in fields {
            let is_ref = match kind {
                jit::JT::Int => false,
                jit::JT::Ref => true,
                _ => return None,
            };
            mapped.insert(index, (id as i64, is_ref));
        }
        Some(cnp::CnpCtx {
            fields: mapped,
            getfield_addr: jit::rava_getfield as *const () as usize,
        })
    }
}

fn cnp_signature(descriptor: &str, is_static: bool) -> Option<CnpSignature> {
    let bytes = descriptor.as_bytes();
    if bytes.first() != Some(&b'(') {
        return None;
    }
    let mut cursor = 1;
    let mut inputs = usize::from(!is_static);
    let mut ref_mask = if is_static { 0 } else { 1 };
    while *bytes.get(cursor)? != b')' {
        let is_ref = parse_cnp_type(bytes, &mut cursor)?;
        if inputs >= 64 {
            return None;
        }
        if is_ref {
            ref_mask |= 1 << inputs;
        }
        inputs += 1;
    }
    cursor += 1;
    let ret = match *bytes.get(cursor)? {
        b'Z' | b'B' | b'C' | b'S' | b'I' => {
            cursor += 1;
            CnpReturn::Int
        }
        b'L' => {
            parse_cnp_type(bytes, &mut cursor)?;
            CnpReturn::Ref { array: false }
        }
        b'[' => {
            parse_cnp_type(bytes, &mut cursor)?;
            CnpReturn::Ref { array: true }
        }
        _ => return None,
    };
    if cursor != bytes.len() {
        return None;
    }
    Some(CnpSignature {
        inputs,
        ref_mask,
        ret,
    })
}

fn parse_cnp_type(bytes: &[u8], cursor: &mut usize) -> Option<bool> {
    match *bytes.get(*cursor)? {
        b'Z' | b'B' | b'C' | b'S' | b'I' => {
            *cursor += 1;
            Some(false)
        }
        b'L' => {
            *cursor += 1;
            while *bytes.get(*cursor)? != b';' {
                *cursor += 1;
            }
            *cursor += 1;
            Some(true)
        }
        b'[' => {
            while bytes.get(*cursor) == Some(&b'[') {
                *cursor += 1;
            }
            match *bytes.get(*cursor)? {
                b'Z' | b'B' | b'C' | b'S' | b'I' => *cursor += 1,
                b'L' => {
                    *cursor += 1;
                    while *bytes.get(*cursor)? != b';' {
                        *cursor += 1;
                    }
                    *cursor += 1;
                }
                _ => return None,
            }
            Some(true)
        }
        _ => None,
    }
}
