//! Identity of the pool-dependent inputs consumed by the full AOT translator.
use rava_classfile::{Constant, ConstantPool};
use sha2::{Digest, Sha256};

/// Inlined code depends on same-class callees as well as the caller's pool.
pub(crate) fn class_method_key(class: &rava_classfile::ClassFile, code: &rava_classfile::Code,
    allow_string_constants: bool) -> Option<String> {
    let base = method_key(&class.this_class, &class.pool, &code.bytecode,
        code.max_locals, allow_string_constants)?;
    let mut hash = Sha256::new();
    blob(&mut hash, base.as_bytes());
    // Final classes establish non-overridable instance-leaf dispatch.
    hash.update(class.access_flags.to_le_bytes());
    // Object leaves depend on field ownership and volatility/finality proofs.
    for field in &class.fields {
        blob(&mut hash, field.name.as_bytes());
        blob(&mut hash, field.descriptor.as_bytes());
        hash.update(field.access_flags.to_le_bytes());
    }
    for method in &class.methods {
        blob(&mut hash, method.name.as_bytes());
        blob(&mut hash, method.descriptor.as_bytes());
        hash.update(method.access_flags.to_le_bytes());
        if let Some(code) = &method.code {
            hash.update([1]);
            blob(&mut hash, &code.bytecode);
            hash.update(code.max_locals.to_le_bytes());
            hash.update(code.max_stack.to_le_bytes());
            hash.update([u8::from(!code.exceptions.is_empty())]);
        } else { hash.update([0]); }
    }
    Some(format!("inline_v5_{:x}", hash.finalize()))
}

pub(crate) fn method_key(
    owner: &str,
    pool: &ConstantPool,
    bytecode: &[u8],
    max_locals: u16,
    allow_string_constants: bool,
) -> Option<String> {
    let mut hash = Sha256::new();
    hash.update(b"rava-full-aot-input-v6-progress");
    blob(&mut hash, owner.as_bytes());
    blob(&mut hash, bytecode);
    hash.update(max_locals.to_le_bytes());
    hash.update([u8::from(allow_string_constants)]);
    hash.update((pool.len() as u64).to_le_bytes());
    for index in 0..pool.len() {
        constant(&mut hash, pool.get(u16::try_from(index).ok()?)?);
    }
    Some(format!("full_v3_{}_{}_{:x}", crate::cnp::AOT_FORMAT, compiler_identity()?, hash.finalize()))
}

/// Same executable bytes are required for offline preparation and execution.
/// Unlike timestamps this survives copying and distinguishes same-time rebuilds.
pub(crate) fn compiler_identity() -> Option<&'static str> {
    static ID: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        let bytes = std::fs::read(std::env::current_exe().ok()?).ok()?;
        Some(format!("{:x}", Sha256::digest(&bytes)))
    }).as_deref()
}

fn blob(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

fn pair(hash: &mut Sha256, tag: u8, first: u16, second: u16) {
    hash.update([tag]);
    hash.update(first.to_le_bytes());
    hash.update(second.to_le_bytes());
}

fn constant(hash: &mut Sha256, value: &Constant) {
    match value {
        Constant::Unusable => hash.update([0]),
        Constant::Utf8(s) => { hash.update([1]); blob(hash, s.as_bytes()); }
        Constant::Integer(v) => { hash.update([3]); hash.update(v.to_le_bytes()); }
        Constant::Float(v) => { hash.update([4]); hash.update(v.to_bits().to_le_bytes()); }
        Constant::Long(v) => { hash.update([5]); hash.update(v.to_le_bytes()); }
        Constant::Double(v) => { hash.update([6]); hash.update(v.to_bits().to_le_bytes()); }
        Constant::Class { name_index } => pair(hash, 7, *name_index, 0),
        Constant::String { utf8_index } => pair(hash, 8, *utf8_index, 0),
        Constant::FieldRef { class_index, nat_index } => pair(hash, 9, *class_index, *nat_index),
        Constant::MethodRef { class_index, nat_index } => pair(hash, 10, *class_index, *nat_index),
        Constant::InterfaceMethodRef { class_index, nat_index } => pair(hash, 11, *class_index, *nat_index),
        Constant::NameAndType { name_index, desc_index } => pair(hash, 12, *name_index, *desc_index),
        Constant::MethodHandle { ref_kind, ref_index } => pair(hash, 15, u16::from(*ref_kind), *ref_index),
        Constant::MethodType { desc_index } => pair(hash, 16, *desc_index, 0),
        Constant::Dynamic { bootstrap, nat_index } => pair(hash, 17, *bootstrap, *nat_index),
        Constant::InvokeDynamic { bootstrap, nat_index } => pair(hash, 18, *bootstrap, *nat_index),
        Constant::Module { name_index } => pair(hash, 19, *name_index, 0),
        Constant::Package { name_index } => pair(hash, 20, *name_index, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(value: Constant, locals: u16, unsafe_constants: bool) -> String {
        let pool = ConstantPool::from_entries(vec![Constant::Unusable, value]);
        method_key("SameClass", &pool, &[0x12, 1, 0xac], locals, unsafe_constants).unwrap()
    }

    #[test]
    fn identical_ldc_bytecode_with_changed_pool_does_not_alias() {
        assert_ne!(key(Constant::Integer(100_000), 0, false), key(Constant::Integer(200_000), 0, false));
        assert_eq!(key(Constant::Integer(100_000), 0, false), key(Constant::Integer(100_000), 0, false));
    }

    #[test]
    fn float_payloads_and_signed_zero_keep_their_identity() {
        assert_ne!(key(Constant::Float(0.0), 0, false), key(Constant::Float(-0.0), 0, false));
        assert_ne!(key(Constant::Float(f32::from_bits(0x7fc00001)), 0, false),
                   key(Constant::Float(f32::from_bits(0x7fc00002)), 0, false));
    }

    #[test]
    fn layout_and_unsafe_constant_policy_are_cache_inputs() {
        assert_ne!(key(Constant::Integer(1), 0, false), key(Constant::Integer(1), 1, false));
        assert_ne!(key(Constant::Integer(1), 0, false), key(Constant::Integer(1), 0, true));
    }
}
