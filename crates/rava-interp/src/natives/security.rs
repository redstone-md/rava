//! Privileged actions and key pairs.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::natives::*;

/// ENUM MIGRATION (RAVA_ARENA_ENUM): write a synthetic enum field to EITHER a
/// handle enum or an arena-born enum (flag ON). No-op if the receiver is neither
/// (mirrors the historic `if let Some(Value::Ref(_))` guard).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn enum_put_field(vm: &mut Vm, v: Option<&Value>, field: &str, val: Value) {
    match v {
        Some(Value::Ref(h)) => vm.heap.put_field(*h, field, val),
        Some(Value::Obj(p)) if crate::nextgen::arena_enum_enabled() => {
            let id = vm.heap.intern_field(field);
            crate::opcodes::arena_putfield(*p, id, &val);
        }
        _ => {}
    }
}
#[cfg(not(feature = "next-gen-gc"))]
pub(crate) fn enum_put_field(vm: &mut Vm, v: Option<&Value>, field: &str, val: Value) {
    if let Some(Value::Ref(h)) = v {
        vm.heap.put_field(*h, field, val);
    }
}

/// java.util.Objects — null-checks (which may throw NPE), equality, hashing.
/// Run a `PrivilegedAction`/`PrivilegedExceptionAction`'s `run()` method — a
/// lambda (apply), a synthetic functional object, or a class instance with a
/// `run():Object` body.
pub(crate) fn run_privileged(vm: &mut Vm, action: &Value) -> Result<Option<Value>, VmError> {
    // A lambda/method-ref action (handle OR arena-lambda): apply its SAM directly.
    if vm.value_is_lambda(action)
        || matches!(action, Value::Ref(h) if matches!(vm.heap.get_field_syn(*h, crate::heap::Syn::FnKind), Value::Str(_)))
    {
        return vm.apply(action, Vec::new());
    }
    // A concrete PrivilegedAction/PrivilegedExceptionAction object: invoke its
    // run():Object. The action may be a heap `Value::Ref` OR — under the arena GC —
    // a `Value::Obj` (e.g. netty's `ChannelHandlerMask$2`, `NetUtil$SoMaxConnAction`).
    // The old code only matched `Value::Ref`, so every arena-allocated action fell
    // through to `null`; callers then unboxed it (`checkcast Boolean`/`Integer` →
    // `booleanValue`/`intValue` NPE) and aborted boot. Resolve+invoke off the value's
    // class for BOTH representations, passing the action itself as the receiver.
    if matches!(action, Value::Ref(_) | Value::Obj(_)) {
        let cls = vm.value_class_name(action);
        if !cls.is_empty() {
            if let Some(decl) = vm.resolve_method(&cls, "run", "()Ljava/lang/Object;")? {
                return vm.invoke(decl, "run", "()Ljava/lang/Object;", vec![action.clone()]);
            }
        }
    }
    Ok(Some(Value::Null))
}

/// java.security KeyPairGenerator / KeyPair / Key — opaque stubs. The server
/// generates a keypair at startup ("Generating keypair") but only USES it during
/// a client encryption handshake, which never happens on the path to "Done". So
/// the keys carry no real cryptographic material; if a client ever connected,
/// this would need a real RSA implementation.
pub(crate) fn keypair_dispatch(vm: &mut Vm, owner: &str, name: &str, args: &[Value]) -> Option<Option<Value>> {
    match name {
        "getInstance" => {
            let h = vm.heap.alloc(owner);
            if let Some(Value::Str(s)) = args.first() { vm.heap.put_field(h, "$algo", Value::Str(s.clone())); }
            Some(Some(Value::Ref(h)))
        }
        "initialize" | "init" => Some(None), // void, no-op
        "generateKeyPair" | "genKeyPair" => {
            let pubk = vm.heap.alloc("java/security/PublicKey");
            let privk = vm.heap.alloc("java/security/PrivateKey");
            let kp = vm.heap.alloc("java/security/KeyPair");
            vm.heap.put_field(kp, "$public", Value::Ref(pubk));
            vm.heap.put_field(kp, "$private", Value::Ref(privk));
            Some(Some(Value::Ref(kp)))
        }
        "generateKey" => Some(Some(Value::Ref(vm.heap.alloc("javax/crypto/SecretKey")))),
        "getPublic" => Some(Some(field_or_null(vm, args.first(), "$public"))),
        "getPrivate" => Some(Some(field_or_null(vm, args.first(), "$private"))),
        "getEncoded" => Some(Some(Value::Array(vm.heap.alloc_bytes(&[])))),
        "getAlgorithm" => Some(Some(Value::Str("RSA".into()))),
        "getFormat" => Some(Some(Value::Str(
            if owner.contains("Private") { "PKCS#8" } else { "X.509" }.into()))),
        "generatePublic" | "generatePrivate" => Some(Some(Value::Ref(vm.heap.alloc(
            if name.ends_with("Private") { "java/security/PrivateKey" } else { "java/security/PublicKey" })))),
        _ => None,
    }
}
