//! Direct execution of native implementation families selected by resolved link metadata.

use crate::{Value, Vm, VmError};

/// Resolved implementation family for a native method with no guest bytecode body.
///
/// This is link metadata, not a workload heuristic: callers may consult it only after
/// `resolve_method` has proved that the dynamic receiver dispatches to Rava's bootstrap
/// implementation. The hot path routes on this id and never re-derives the provider from class
/// or method text. `None` is fail-closed and keeps the ordinary native cascade.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum NativeProgram {
    String,
    StringBuilder,
    BitSet,
    ClassCast,
    Map,
    ArrayList,
    Stream,
    BooleanValue,
    BooleanValueOf,
    NumberIntValue,
    NumberLongValue,
    NumberFloatValue,
    NumberDoubleValue,
}

/// Class-level provider identity resolved when the classfile enters the loader.
///
/// This moves class routing out of JIT plan construction. Method-specific checks and opt-out gates
/// remain in `program_for_class`; an unrecognized class has no binding and fails closed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum NativeProgramClass {
    String,
    StringBuilder,
    BitSet,
    Class,
    Map,
    ArrayList,
    Stream,
    Boolean,
    Number,
}

impl NativeProgramClass {
    /// Resolve only exact implementation providers; simple-name and package-shape guesses do not
    /// create a binding.
    pub(crate) fn for_classfile_name(class: &str) -> Option<Self> {
        if crate::map::is_native_map(class) {
            return Some(Self::Map);
        }
        match class {
            "java/lang/String" => Some(Self::String),
            "java/lang/StringBuilder" | "java/lang/StringBuffer" => Some(Self::StringBuilder),
            "java/util/BitSet" => Some(Self::BitSet),
            "java/lang/Class" => Some(Self::Class),
            "java/util/ArrayList" => Some(Self::ArrayList),
            "java/util/stream/Stream" => Some(Self::Stream),
            "java/lang/Boolean" => Some(Self::Boolean),
            "java/lang/Integer"
            | "java/lang/Long"
            | "java/lang/Short"
            | "java/lang/Byte"
            | "java/lang/Float"
            | "java/lang/Double"
            | "java/lang/Character"
            | "java/lang/Number" => Some(Self::Number),
            _ => None,
        }
    }
}


/// Resolve one method against a class-level provider identity.
pub(crate) fn program_for_class(
    class: NativeProgramClass,
    name: &str,
    descriptor: &str,
) -> Option<NativeProgram> {
    static ON: std::sync::LazyLock<bool> =
        std::sync::LazyLock::new(|| std::env::var_os("RAVA_NO_NATIVE_PROGRAM").is_none());
    if !*ON {
        return None;
    }
    match class {
        NativeProgramClass::Map => {
            static MAP_ON: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
                std::env::var_os("RAVA_NO_NATIVE_PROGRAM_MAP").is_none()
            });
            (*MAP_ON).then_some(NativeProgram::Map)
        }
        NativeProgramClass::Class
            if name == "cast" && descriptor == "(Ljava/lang/Object;)Ljava/lang/Object;" =>
        {
            static CLASS_CAST_ON: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
                std::env::var_os("RAVA_NO_NATIVE_PROGRAM_CLASS_CAST").is_none()
            });
            (*CLASS_CAST_ON).then_some(NativeProgram::ClassCast)
        }
        NativeProgramClass::ArrayList | NativeProgramClass::Stream => {
            static STREAM_ON: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
                std::env::var_os("RAVA_NO_NATIVE_PROGRAM_STREAM").is_none()
            });
            if !*STREAM_ON {
                return None;
            }
            Some(if class == NativeProgramClass::ArrayList {
                NativeProgram::ArrayList
            } else {
                NativeProgram::Stream
            })
        }
        NativeProgramClass::Boolean if name == "booleanValue" && descriptor == "()Z" => {
            Some(NativeProgram::BooleanValue)
        }
        NativeProgramClass::Boolean
            if name == "valueOf" && descriptor == "(Z)Ljava/lang/Boolean;" =>
        {
            Some(NativeProgram::BooleanValueOf)
        }
        NativeProgramClass::Number => match (name, descriptor) {
            ("intValue", "()I") | ("shortValue", "()S") | ("byteValue", "()B") => {
                Some(NativeProgram::NumberIntValue)
            }
            ("longValue", "()J") => Some(NativeProgram::NumberLongValue),
            ("floatValue", "()F") => Some(NativeProgram::NumberFloatValue),
            ("doubleValue", "()D") => Some(NativeProgram::NumberDoubleValue),
            _ => None,
        },
        NativeProgramClass::String => Some(NativeProgram::String),
        NativeProgramClass::StringBuilder => Some(NativeProgram::StringBuilder),
        NativeProgramClass::BitSet => Some(NativeProgram::BitSet),
        _ => None,
    }
}

/// Resolve only from a class-load binding; absence means the specialization declines.
pub(crate) fn program_for_binding(
    binding: Option<NativeProgramClass>,
    name: &str,
    descriptor: &str,
) -> Option<NativeProgram> {
    binding.and_then(|class| program_for_class(class, name, descriptor))
}

/// Link the non-virtual Boolean factory directly from a constant-pool method reference.
///
/// The receiver-free signature makes this binding independent of runtime receiver classes.
/// Guest bytecode still wins because callers consult it only after `resolve_method` returns None.
pub(crate) fn program_for_static_site(
    owner: &str,
    name: &str,
    descriptor: &str,
) -> Option<NativeProgram> {
    if owner != "java/lang/Boolean" {
        return None;
    }
    match program_for_class(NativeProgramClass::Boolean, name, descriptor) {
        Some(program @ NativeProgram::BooleanValueOf) => Some(program),
        _ => None,
    }
}

/// Execute one resolved native family without walking the owner cascade.
///
/// `None` is a normal refusal: the family did not implement this member or receiver shape, so the
/// caller must preserve the general cascade. Errors are guest-visible exceptions and must not be
/// retried through another implementation.
pub(crate) fn invoke(
    program: NativeProgram,
    vm: &mut Vm,
    name: &str,
    descriptor: &str,
    args: &[Value],
) -> Result<Option<Option<Value>>, VmError> {
    match program {
        NativeProgram::String => {
            Ok(super::strings::str_dispatch(vm, name, descriptor, args)?.map(Some))
        }
        NativeProgram::StringBuilder => super::strings::sb_dispatch(vm, name, descriptor, args),
        NativeProgram::BitSet => Ok(super::buffers::bitset_dispatch(vm, name, descriptor, args)),
        NativeProgram::Map => {
            crate::frames::native_depth_inc();
            let result = crate::map::map_method(vm, name, args);
            crate::frames::native_depth_dec();
            result
        }
        NativeProgram::ArrayList | NativeProgram::Stream => {
            let owner = match program {
                NativeProgram::ArrayList => "java/util/ArrayList",
                NativeProgram::Stream => "java/util/stream/Stream",
                _ => unreachable!(),
            };
            crate::frames::native_depth_inc();
            let result = crate::stream::dispatch(vm, owner, name, descriptor, args);
            crate::frames::native_depth_dec();
            result
        }
        NativeProgram::BooleanValue => Ok(Some(Some(Value::Int(super::bool_of(vm, args.first()))))),
        NativeProgram::BooleanValueOf => Ok(Some(Some(
            vm.boolean_obj(super::bool_of(vm, args.first()) != 0),
        ))),
        NativeProgram::NumberIntValue => Ok(Some(Some(Value::Int(number_value(vm, args) as i32)))),
        NativeProgram::NumberLongValue => {
            Ok(Some(Some(Value::Long(number_value(vm, args) as i64))))
        }
        NativeProgram::NumberFloatValue => {
            Ok(Some(Some(Value::Float(number_value(vm, args) as f32))))
        }
        NativeProgram::NumberDoubleValue => Ok(Some(Some(Value::Double(number_value(vm, args))))),
        NativeProgram::ClassCast => Ok(Some(Some(class_cast(vm, args)?))),
    }
}

#[inline]
fn number_value(vm: &Vm, args: &[Value]) -> f64 {
    match args.first() {
        Some(Value::Double(value)) => *value,
        Some(Value::Float(value)) => *value as f64,
        Some(value) => super::long_of(vm, Some(value)) as f64,
        None => 0.0,
    }
}

/// Execute allocation-free scalar programs directly on the packed JIT cells.
///
/// This sits before `Vec<Value>` construction and root registration. Only programs whose native
/// contract is a read of the exact receiver may appear here; every allocating, throwing, or
/// callback-capable family returns `None` and uses [`invoke`].
pub(crate) fn invoke_cells(program: NativeProgram, vm: &Vm, cells: &[i64]) -> Option<i64> {
    let recv =
        crate::jit::unbox_ref_kind(vm, crate::nextgen::forward_if_moved(*cells.first()?), false);
    match &recv {
        Value::Ref(handle)
            if matches!(
                vm.heap.get_field_syn(*handle, crate::heap::Syn::FnKind),
                Value::Str(_)
            ) =>
        {
            return None;
        }
        Value::Ref(_) | Value::Obj(_) => {}
        _ => return None,
    }
    let value = match program {
        NativeProgram::BooleanValue => Value::Int(super::bool_of(vm, Some(&recv))),
        NativeProgram::NumberIntValue => {
            Value::Int(number_value(vm, std::slice::from_ref(&recv)) as i32)
        }
        NativeProgram::NumberLongValue => {
            Value::Long(number_value(vm, std::slice::from_ref(&recv)) as i64)
        }
        NativeProgram::NumberFloatValue => {
            Value::Float(number_value(vm, std::slice::from_ref(&recv)) as f32)
        }
        NativeProgram::NumberDoubleValue => {
            Value::Double(number_value(vm, std::slice::from_ref(&recv)))
        }
        NativeProgram::String
        | NativeProgram::StringBuilder
        | NativeProgram::BitSet
        | NativeProgram::ClassCast
        | NativeProgram::BooleanValueOf
        | NativeProgram::ArrayList
        | NativeProgram::Stream
        | NativeProgram::Map => return None,
    };
    Some(crate::jit::pack_local(&value))
}

/// Execute `Class.cast` after method resolution has proved the bootstrap implementation.
///
/// Pass verdicts are epoch-scoped because arena class identities can be recycled after a
/// collection. Refusals still perform the full hierarchy walk so the guest observes the same
/// `ClassCastException` and message.
pub(crate) fn class_cast(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    let value = args.get(1).cloned().unwrap_or(Value::Null);
    if matches!(value, Value::Null) {
        return Ok(value);
    }
    let class_key = match &value {
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(pointer) => 1 << 40 | crate::nextgen::arena_class_id(*pointer) as u64,
        Value::Ref(handle) => 2 << 40 | vm.heap.object_class_word(*handle).unwrap_or(0) as u64,
        Value::Str(_) => 3 << 40,
        Value::Int(_) => 4 << 40,
        Value::Long(_) => 5 << 40,
        Value::Float(_) => 6 << 40,
        Value::Double(_) => 7 << 40,
        _ => 0,
    };
    let mirror = match args.first() {
        Some(Value::Ref(handle)) => *handle as u64,
        #[cfg(feature = "next-gen-gc")]
        Some(Value::Obj(pointer)) => 1 << 32 | crate::nextgen::arena_identity_hash(*pointer) as u64,
        _ => 0,
    };
    let key = (mirror << 24) ^ class_key;
    if mirror != 0 && class_key != 0 {
        std::thread_local! {
            static PASS: std::cell::RefCell<(u64, crate::FastMap<u64, ()>)> =
                std::cell::RefCell::new((0, crate::FastMap::default()));
        }
        let epoch = crate::nextgen::gc_epoch();
        let hit = PASS.with(|cache| {
            let cache = cache.borrow();
            cache.0 == epoch && cache.1.contains_key(&key)
        });
        if hit {
            return Ok(value);
        }
        verify_class_cast(vm, args, &value)?;
        PASS.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.0 != epoch {
                cache.0 = epoch;
                cache.1.clear();
            }
            if cache.1.len() >= 64 * 1024 {
                cache.1.clear();
            }
            cache.1.insert(key, ());
        });
        return Ok(value);
    }
    verify_class_cast(vm, args, &value)?;
    Ok(value)
}

fn verify_class_cast(vm: &mut Vm, args: &[Value], value: &Value) -> Result<(), VmError> {
    let target = crate::reflect::field_str(vm, args.first(), "$name").unwrap_or_default();
    if !target.is_empty()
        && crate::opcodes::instance_of_tri(vm, value, &target) == crate::opcodes::Tri::No
    {
        let message = crate::opcodes::cast_message(vm, value, &target);
        return Err(VmError::Throw(
            vm.make_exception("java/lang/ClassCastException", &message),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod class_binding_tests {
    use super::NativeProgramClass;

    #[test]
    fn class_binding_is_exact_and_does_not_infer_from_simple_names() {
        assert_eq!(
            NativeProgramClass::for_classfile_name("java/util/HashMap"),
            Some(NativeProgramClass::Map),
        );
        assert_eq!(
            NativeProgramClass::for_classfile_name("java/lang/Integer"),
            Some(NativeProgramClass::Number),
        );
        assert_eq!(
            NativeProgramClass::for_classfile_name("net/example/ArrayListish"),
            None,
        );
        assert_eq!(
            NativeProgramClass::for_classfile_name("java/util/Objects"),
            None,
        );
    }

    #[test]
    fn missing_loaded_binding_fails_closed() {
        assert_eq!(super::program_for_binding(None, "append", "()V"), None);
    }
}
#[cfg(test)]
mod static_site_binding_tests {
    #[test]
    fn static_boolean_factory_binding_requires_exact_provider_signature() {
        assert_eq!(
            super::program_for_static_site(
                "java/lang/Boolean",
                "valueOf",
                "(Z)Ljava/lang/Boolean;",
            ),
            Some(super::NativeProgram::BooleanValueOf),
        );
        assert_eq!(
            super::program_for_static_site(
                "java/lang/Boolean",
                "valueOf",
                "(Ljava/lang/String;)Ljava/lang/Boolean;",
            ),
            None,
        );
        assert_eq!(
            super::program_for_static_site(
                "example/Boolean",
                "valueOf",
                "(Z)Ljava/lang/Boolean;",
            ),
            None,
        );
    }
}
