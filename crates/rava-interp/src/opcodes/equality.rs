//! Guest-defined hashCode/equals and the record components they walk.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::opcodes::*;

/// Kill switch for this round's equality/type-test shortcuts, DEFAULT ON:
/// `RAVA_NO_FAST_EQ=1` (or the global `RAVA_NO_EMIT_MORE`) puts every one of them back on its
/// per-call path — the identity clause below, the shared class-name `Arc`, and the two
/// `instance_of` preconditions in `typecheck.rs`. Read ONCE per process into a `OnceLock`: the
/// answer is a process constant, and reading an env var per call is the exact shape this round
/// exists to remove.
#[inline(always)]
pub(crate) fn fast_eq_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_EQ").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// The receiver's dispatch class as a SHARED `Arc<str>` instead of a fresh `String`.
///
/// COMPLEXITY: `object_class_name` copies the class name — `net/minecraft/world/level/block/
/// state/BlockBehaviour$BlockStateBase` is 62 bytes — on EVERY `Objects.equals`/`Objects.
/// hashCode` that reaches a guest body, and the name is a CONSTANT of the receiver's class. WAS
/// one malloc + memcpy of O(name length) per CALL; IS one direct-mapped probe per call and one
/// build per distinct class (~1 590 in a boot). No new table: this is the SAME `FAST_CLASS`
/// slot array `invokevirtual` already fills, keyed the identical way (interned class-name
/// pointer for a handle, registry class id for an arena object), so a receiver that dispatches
/// and then compares hits a line that is already warm.
///
/// Deliberately returns the REGISTRY name for an arena object rather than `invokevirtual`'s
/// arena-string mapping: `object_class_name` does the same, and `record_component_eq` reads
/// this answer, so the two must not diverge.
fn eq_class_name(vm: &Vm, v: &Value) -> Option<Arc<str>> {
    match v {
        Value::Ref(h) => {
            let c = vm.heap.class_of(*h)?;
            // A zero name pointer is an empty/absent class name — not a stable identity, so it
            // must never claim a slot that another class would then read.
            match vm.heap.class_name_ptr(*h) {
                0 => Some(Arc::from(c)),
                p => Some(dispatch_class(p as u64 | IOF_REF_TAG, || Arc::from(c))),
            }
        }
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) => {
            let cid = crate::nextgen::arena_class_id(*p);
            dispatch_class_opt(cid as u64, || crate::nextgen::class_name(cid))
        }
        _ => None,
    }
}

/// `actual` for the two dispatchers below, shared so the memo-off path is one branch and not a
/// second copy of the resolution. `java/lang/Object` is the fallback for a non-object cell, and
/// it goes through `wrapper_class` so the constant name is not re-allocated per call either.
#[inline]
fn dispatch_target(vm: &mut Vm, v: &Value) -> Arc<str> {
    if fast_eq_enabled() {
        return eq_class_name(vm, v).unwrap_or_else(|| wrapper_class("java/lang/Object"));
    }
    Arc::from(object_class_name(vm, v).unwrap_or_else(|| "java/lang/Object".to_string()))
}

/// `Objects.hashCode(component)` for a reference component: virtual dispatch to
/// the value's own `hashCode()` — bytecode when the class has one (a nested
/// record's synthesised hashCode re-enters this file's indy path, which is the
/// recursion), else the JDK native walked up the super chain (String/boxes/
/// Object identity hash). Mirrors how `value_to_string` dispatches `toString`.
pub(crate) fn invoke_guest_hashcode(vm: &mut Vm, v: &Value) -> Result<i32, VmError> {
    let actual = dispatch_target(vm, v);
    let sig = "()I";
    let result = match vm.resolve_method(&actual, "hashCode", sig)? {
        Some(decl) => vm.invoke(decl, "hashCode", sig, vec![v.clone()])?,
        None => {
            // COMPLEXITY: the argument slice was REBUILT INSIDE THE LOOP, so a receiver whose
            // hashCode is served k levels up cloned the value k times to ask k questions with
            // the identical argument. `try_invoke` takes `&[Value]` and cannot mutate it, so one
            // array serves the whole chain. WAS O(ancestors of the receiver class) `Value`
            // clones per call — each one an `Arc` refcount bump or an `RStr` copy — IS one.
            let args = [v.clone()];
            let chain = vm.super_chain(&actual);
            let mut r = None;
            for cn in chain.iter() {
                if let Some(ret) = crate::natives::try_invoke(vm, cn, "hashCode", sig, &args)? {
                    r = ret;
                    break;
                }
            }
            r
        }
    };
    Ok(match result {
        Some(Value::Int(i)) => i,
        _ => 0,
    })
}

/// `Objects.equals(a, b)` for a reference component: virtual dispatch to `a`'s
/// own `equals(Object)` (a nested record's synthesised equals re-enters the indy
/// path), else the JDK native up the super chain (`Object.equals` = identity).
pub(crate) fn invoke_guest_equals(vm: &mut Vm, a: &Value, b: &Value) -> Result<bool, VmError> {
    // ⭐**THE IDENTITY CLAUSE IS THE JDK'S OWN FIRST LINE, NOT A SHORTCUT AROUND IT.**
    // `Objects.equals` is literally `(a == b) || (a != null && a.equals(b))`, and every caller
    // of this function implements exactly that contract (the `Objects` native, and the record
    // components a synthesised `equals` compares — `ObjectMethods` bootstraps them onto
    // `Objects::equals`). `ref_equal` IS this engine's `if_acmpeq`, so using it here is the
    // spec's `==`, including the arena/handle identity bridge.
    //
    // COMPLEXITY: without it, `list.equals(list)` resolved a method, built a guest frame and
    // interpreted a loop over EVERY element to answer a question the first word already
    // answered — O(elements) for an O(1) fact. Same for a self-compared String or record.
    // A guest `equals` that is not reflexive cannot be observed here: the JDK does not call it
    // in this case either, so this is the more correct answer as well as the cheaper one.
    if fast_eq_enabled() && ref_equal(a, b) {
        return Ok(true);
    }
    let actual = dispatch_target(vm, a);
    let sig = "(Ljava/lang/Object;)Z";
    let result = match vm.resolve_method(&actual, "equals", sig)? {
        Some(decl) => vm.invoke(decl, "equals", sig, vec![a.clone(), b.clone()])?,
        None => {
            // Same hoist as `invoke_guest_hashcode`, and twice the price here — both sides were
            // re-cloned per ancestor. WAS 2 × O(ancestors of the receiver class) `Value` clones
            // per call, IS 2.
            let args = [a.clone(), b.clone()];
            let chain = vm.super_chain(&actual);
            let mut r = None;
            for cn in chain.iter() {
                if let Some(ret) = crate::natives::try_invoke(vm, cn, "equals", sig, &args)? {
                    r = ret;
                    break;
                }
            }
            r
        }
    };
    Ok(matches!(result, Some(Value::Int(i)) if i != 0))
}

/// One component's contribution to the JVM-semantics record hash. `desc` is the
/// component's FIELD DESCRIPTOR: only `Z` needs it (booleans live in `Value::Int`
/// cells but hash 1231/1237); byte/short/char/int all hash to their own value,
/// exactly the wrapper rules.
pub(crate) fn record_component_hash(vm: &mut Vm, v: &Value, desc: &str) -> Result<i32, VmError> {
    Ok(match v {
        Value::Int(i) => {
            if desc == "Z" {
                if *i != 0 { 1231 } else { 1237 }
            } else {
                *i
            }
        }
        // (int)(l ^ (l >>> 32)) — the int cast keeps only the low 32 bits, where
        // arithmetic and logical shift agree.
        Value::Long(l) => (*l ^ (*l >> 32)) as i32,
        Value::Float(f) => java_float_bits(*f) as i32,
        Value::Double(d) => {
            let b = java_double_bits(*d);
            (b ^ (b >> 32)) as i32
        }
        // Same fold Rava's own String.hashCode native uses, so a record hash and a
        // direct `component.hashCode()` call can never disagree inside one VM.
        Value::Str(s) => s.chars().fold(0i32, |a, c| a.wrapping_mul(31).wrapping_add(c as i32)),
        Value::Null => 0,
        r @ (Value::Ref(_) | Value::Obj(_) | Value::Array(_)) => invoke_guest_hashcode(vm, r)?,
        _ => 0,
    })
}

/// One component pair's contribution to the JVM-semantics record equals:
/// `==` for primitive cells, `Float/Double.compare == 0` bit rules for floats,
/// `Objects.equals` (virtual dispatch) for references.
pub(crate) fn record_component_eq(vm: &mut Vm, a: &Value, b: &Value) -> Result<bool, VmError> {
    Ok(match (a, b) {
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Long(x), Value::Long(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => java_float_bits(*x) == java_float_bits(*y),
        (Value::Double(x), Value::Double(y)) => java_double_bits(*x) == java_double_bits(*y),
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Null, Value::Null) => true,
        (Value::Null, _) | (_, Value::Null) => false,
        // Arrays compare by identity (Objects.equals → Object.equals) on the JVM.
        (Value::Array(x), Value::Array(y)) => x == y,
        // A `String` component can surface as a bare Str cell on one side and a
        // heap/arena String object on the other; compare content, but ONLY when
        // the object really is a String (never run an arbitrary toString here).
        //
        // The class test is FIRST for a reason that is now load-bearing: it decides the arm
        // without materialising anything, and `eq_class_name` answers it from the shared
        // dispatch-class slot instead of allocating a copy of the name to throw away. The
        // `value_to_string` behind it only runs once the class is known to be String.
        (Value::Str(x), o @ (Value::Ref(_) | Value::Obj(_)))
        | (o @ (Value::Ref(_) | Value::Obj(_)), Value::Str(x)) => {
            let is_string = if fast_eq_enabled() {
                eq_class_name(vm, o).as_deref() == Some("java/lang/String")
            } else {
                object_class_name(vm, o).as_deref() == Some("java/lang/String")
            };
            is_string && *vm.value_to_string(o)? == **x
        }
        (r @ (Value::Ref(_) | Value::Obj(_)), o) => invoke_guest_equals(vm, r, o)?,
        _ => false,
    })
}

/// `RAVA_RECORD_LINK` (default OFF) — resolve a record call site's class name and
/// component field ids ONCE per (site, receiver class) instead of on every call.
pub(crate) fn record_link_enabled() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_RECORD_LINK").is_some())
}
