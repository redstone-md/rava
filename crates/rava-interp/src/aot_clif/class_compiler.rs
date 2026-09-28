//! Class-file-only compilation: no VM, class initialization, or heap access.
use super::InvokeInfo;
use crate::cnp::CompiledObj;
use rava_classfile::{ClassFile, Code, Constant};

/// Produces relocatable code using immutable class-file inputs only.
/// Linking runtime symbols and initializing classes remain the runner's job.
pub struct ClassCompiler<'a> {
    class: &'a ClassFile,
    allow_string_constants: bool,
}

impl<'a> ClassCompiler<'a> {
    pub fn new(class: &'a ClassFile) -> Self {
        Self {
            class,
            allow_string_constants: false,
        }
    }

    pub fn compile_method(&self, method_index: usize) -> Option<CompiledObj> {
        self.compile_code(self.class.methods.get(method_index)?.code.as_ref()?)
    }

    /// Versioned identity for the direct-leaf ABI artifact. The full class and method access
    /// flags participate because they are part of the runtime admission policy.
    pub fn direct_leaf_key(&self, method_index: usize) -> Option<String> {
        let method = self.class.methods.get(method_index)?;
        let key = self.method_key(method_index)?;
        Some(format!(
            "direct_leaf_v2_{key}_c{:04x}_m{:04x}",
            self.class.access_flags, method.access_flags
        ))
    }

    /// Compile this method as a direct-leaf artifact (System V ABI
    /// `(hidden consts_ptr: i64, args: i64...) -> i64`). Verifies the resolved
    /// method's metadata and body first — any refusal, including a missing
    /// emitter, yields None so the call site keeps its rava_invoke fallback.
    pub fn compile_direct_leaf_method(&self, method_index: usize) -> Option<CompiledObj> {
        let method = self.class.methods.get(method_index)?;
        if method.name.starts_with('<')
            || self.class.access_flags & 0x0001 == 0
            || method.access_flags & 0x0001 == 0
        {
            return None;
        }
        let code = method.code.as_ref()?;
        if !super::direct_leaf::is_no_gc_primitive_leaf(
            &method.descriptor,
            method.access_flags,
            code,
        ) {
            return None;
        }
        super::compile_native_leaf(&code.bytecode, code.max_locals as u32, &method.descriptor)
    }

    /// Artifact identity shared with the runtime reader. No execution history.
    pub fn method_key(&self, method_index: usize) -> Option<String> {
        let code = self.class.methods.get(method_index)?.code.as_ref()?;
        super::cache_key::class_method_key(self.class, code, self.allow_string_constants)
    }

    pub fn with_string_constants(mut self, allow: bool) -> Self {
        self.allow_string_constants = allow;
        self
    }

    pub(crate) fn compile_code(&self, code: &Code) -> Option<CompiledObj> {
        if !code.exceptions.is_empty() {
            return None;
        }
        let pool = &self.class.pool;
        let field_of = |cp: u16| -> Option<(String, String, crate::aot_clif::FieldKind)> {
            use crate::aot_clif::FieldKind;
            let m = pool.member_ref(cp)?;
            let k = match m.descriptor.as_bytes().first()? {
                b'L' | b'[' => FieldKind::Ref,
                b'I' | b'Z' | b'B' | b'C' | b'S' => FieldKind::Int,
                b'J' => FieldKind::Long,
                b'F' => FieldKind::Float,
                b'D' => FieldKind::Double,
                _ => return None,
            };
            Some((m.owner.to_string(), m.name.to_string(), k))
        };
        let ret_shape = |desc: &str| -> Option<(bool, bool, bool)> {
            let rb = desc.as_bytes();
            let rs = desc.rfind(')').map(|p| p + 1)?;
            Some((
                rb.get(rs) == Some(&b'V'),
                matches!(rb.get(rs), Some(b'L') | Some(b'[')),
                matches!(rb.get(rs), Some(b'J') | Some(b'D')),
            ))
        };
        let method_of = |cp: u16| -> Option<InvokeInfo> {
            let m = pool.member_ref(cp)?;
            let (ret_void, ret_ref, ret_wide) = ret_shape(m.descriptor)?;
            Some(InvokeInfo {
                numeric_leaf: super::numeric_leaf::NumericLeaf::resolve(self.class, cp),
                object_leaf: super::object_leaf::ObjectLeaf::resolve(self.class, cp),
                direct_leaf: super::direct_leaf::DirectLeafCall::from_member(
                    &m.owner,
                    &m.name,
                    &m.descriptor,
                ),
                ncells: crate::count_args(m.descriptor) as u32 + 1, // + receiver
                kind: 1,
                ret_void,
                ret_ref,
                ret_wide,
            })
        };
        let indy_of = |cp: u16| -> Option<InvokeInfo> {
            let (_bs, _n, desc) = pool.invoke_dynamic(cp)?;
            let (ret_void, ret_ref, ret_wide) = ret_shape(desc)?;
            Some(InvokeInfo {
                numeric_leaf: None,
                object_leaf: None,
                direct_leaf: None,
                ncells: crate::count_args(desc) as u32,
                kind: 0,
                ret_void,
                ret_ref,
                ret_wide,
            })
        };
        // NUMERIC ldc (Integer/Float) is emitted inline and PROVEN correct
        // (numeric_ldc_inline test). STRING/Class ldc (rava_ldc → alloc_arena_string)
        // has a residual bug, so it bails by default; RAVA_AOT_UNSAFE allows it.
        let allow_strldc = self.allow_string_constants;
        let ldc_of = |cp: u16| -> Option<crate::aot_clif::LdcConst> {
            match pool.get(cp)? {
                Constant::String { .. } if !allow_strldc => None,
                Constant::String { .. } => Some(crate::aot_clif::LdcConst::Str(
                    pool.string_const(cp)?.to_string(),
                )),
                Constant::Class { .. } if !allow_strldc => None,
                Constant::Class { .. } => Some(crate::aot_clif::LdcConst::Class),
                Constant::Integer(v) => Some(crate::aot_clif::LdcConst::Int(*v)),
                Constant::Float(f) => Some(crate::aot_clif::LdcConst::Float(f.to_bits())),
                Constant::Long(v) => Some(crate::aot_clif::LdcConst::Long(*v)),
                Constant::Double(d) => Some(crate::aot_clif::LdcConst::Double(d.to_bits())),
                _ => None,
            }
        };
        let (code, relocs, constants) = crate::aot_clif::compile_method(
            &code.bytecode,
            &self.class.this_class,
            code.max_locals as u32,
            &field_of,
            &method_of,
            &indy_of,
            &ldc_of,
        )?;
        Some(CompiledObj::from_full(code, relocs, constants))
    }
}
