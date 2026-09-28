//! Constructing an instance reflectively.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::reflect::*;

/// Allocate an instance of `cls` and run its matching constructor.
pub(crate) fn new_instance(vm: &mut Vm, cls: &str, arg_desc: &str, ctor_args: &[Value]) -> Result<Value, VmError> {
    if let Ok(c) = vm.loader.get_or_load(cls) {
        vm.ensure_init(&c)?;
    }
    // ARENA ROUTE (RAVA_ARENA_NATIVE_NEW). Same four vetoes the `new` opcode applies, asked
    // without its frame/site protocol. The guest `<init>` below runs identically on either
    // receiver — `arena_putfield` handles a `Value::Obj` the same way `put_field` handles a
    // handle — so only the representation differs.
    #[cfg(feature = "next-gen-gc")]
    let arena = vm.try_arena_alloc_by_name(cls);
    #[cfg(not(feature = "next-gen-gc"))]
    let arena: Option<crate::arena::ArenaPtr> = None;
    let recv = match arena {
        Some(p) => Value::Obj(p),
        None => Value::Ref(vm.heap.alloc(cls)),
    };
    if let Some((decl, desc, _)) = vm.resolve_by_args(cls, "<init>", arg_desc)? {
        let mut locals = Vec::with_capacity(ctor_args.len() + 1);
        locals.push(recv.clone());
        locals.extend_from_slice(ctor_args);
        vm.invoke(decl, "<init>", &desc, locals)?;
    }
    Ok(recv)
}

// --- java.lang.reflect.Method / Constructor ---------------------------------

pub(crate) fn ctor_method(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    match name {
        "newInstance" => {
            let recv = match args.first() { Some(Value::Ref(h)) => *h, _ => return Ok(Some(Some(Value::Null))) };
            let cls = match vm.heap.get_field(recv, "$class") { Value::Str(s) => s.to_string(), _ => return Ok(Some(Some(Value::Null))) };
            let ad = match vm.heap.get_field(recv, "$argdesc") { Value::Str(s) => s.to_string(), _ => String::new() };
            let call_args = crate::natives::varargs(vm, args.get(1));
            Ok(Some(Some(new_instance(vm, &cls, &ad, &call_args)?)))
        }
        "setAccessible" | "trySetAccessible" => Ok(Some(None)),
        // We don't enforce reflective access control, so a Constructor is always
        // "accessible" — gson's ConstructorConstructor checks this before calling.
        "isAccessible" | "canAccess" => Ok(Some(Some(Value::Int(1)))),
        "getModifiers" => Ok(Some(Some(Value::Int(0x0001)))), // public
        "isSynthetic" | "isVarArgs" => Ok(Some(Some(Value::Int(0)))),
        "getParameterCount" => {
            let ad = field_str(vm, args.first(), "$argdesc").unwrap_or_default();
            Ok(Some(Some(Value::Int(crate::count_args(&format!("({ad})V")) as i32))))
        }
        "getDeclaringClass" => {
            let cls = field_str(vm, args.first(), "$class").unwrap_or_default();
            Ok(Some(Some(vm.make_class(&cls))))
        }
        _ => Ok(None),
    }
}

// --- ClassLoader / URLClassLoader / URI / URL -------------------------------
