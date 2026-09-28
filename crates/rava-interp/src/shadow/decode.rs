//! Decoding a shadow value and reading a field out of an object.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::shadow::*;

pub(crate) fn shadow_decode_impl(vm: &mut Vm, class_name: &str, args: &[Value], result: &Result<Option<Value>, VmError>) {
    // decode(receiver, ops, input) — need all three.
    if args.len() < 3 {
        return;
    }
    let input = &args[2];
    SEEN.fetch_add(1, Relaxed);
    let addr = match &args[0] {
        Value::Ref(h) => *h as u64,
        Value::Obj(p) => p.addr(),
        _ => return,
    };
    let Some(node) = crate::codec_flatten::ast_for(addr) else {
        // Diagnostic: log the first few unlinked decode receivers (class + addr) to see WHY the
        // construction-time AST keys don't match the decode-time receiver addresses.
        NO_AST.fetch_add(1, Relaxed);
        // Callsite = the class whose decode() is running (≈ receiver's declaring class). The histogram
        // shows FieldDecoder.decode DOES fire (~7% of decodes) yet never links → construction-time arena
        // addresses don't survive nursery-promotion/GC; the rest are shared leaf/wrapper singletons.
        let mut g = NO_AST_BY_CLASS.lock().unwrap_or_else(|e| e.into_inner());
        *g.get_or_insert_with(HashMap::new).entry(class_name.to_string()).or_insert(0) += 1;
        return; // no pre-built AST for this codec ⇒ not shadow-eligible
    };

    let shadow_res = run_shadow_decode(vm, &node, input);
    let guest_res = match result {
        Ok(Some(v)) => extract_result_from_data_result(vm, v),
        _ => None,
    };
    update_and_log_match(class_name, &node, shadow_res, guest_res);
}

/// Look a named field up in a JSON object / NBT compound / java.util.Map. Read-only (Ref or arena Obj).
pub(crate) fn get_field_from_object(vm: &mut Vm, input: &Value, name: &str) -> Option<Value> {
    let class = val_class(vm, input)?;
    if class == "com/google/gson/JsonObject" {
        let members = read_guest_field(vm, input, "members")?;
        return get_field_from_object(vm, &members, name);
    }
    if class == "net/minecraft/nbt/CompoundTag" {
        let tags = read_guest_field(vm, input, "tags")?;
        return get_field_from_object(vm, &tags, name);
    }
    // gson stores JsonObject members in its own red-black tree (LinkedTreeMap) — walk it natively.
    if class.contains("LinkedTreeMap") {
        let root = read_guest_field(vm, input, "root")?;
        return linked_tree_get(vm, &root, name);
    }
    // A JsonOps MapLike ($1/$2 anon over a JsonObject): find the wrapped JsonObject and recurse.
    if class.starts_with("com/mojang/serialization/JsonOps") {
        let inner = find_json_container(vm, input)?;
        return get_field_from_object(vm, &inner, name);
    }
    // Fall back to a backing java.util.Map ($map marker present) — heap-Ref-backed only.
    if let Value::Ref(h) = input {
        if !matches!(vm.heap.get_field_syn(*h, crate::heap::Syn::Map), Value::Null) {
            let map_id = crate::map::map_id(vm, *h);
            // A string key never reaches guest hashCode/equals — derivation is infallible.
            let key = crate::map::map_key(vm, *h, &Value::Str(name.into())).ok()?;
            return vm.heap.map_get(map_id, &key);
        }
    }
    None
}

pub(crate) fn update_and_log_match(class_name: &str, node: &CodecNode, shadow: Option<Value>, guest: Option<Value>) {
    ATTEMPTS.fetch_add(1, Relaxed);
    match (shadow, guest) {
        (Some(s), Some(g)) => {
            if values_are_equal(&s, &g) {
                let m = MATCHES.fetch_add(1, Relaxed) + 1;
                if m % 10_000 == 0 {
                    eprintln!(
                        "[SHADOW-DECODE] matches={} attempts={} unsupported={} mismatches={}",
                        m,
                        ATTEMPTS.load(Relaxed),
                        UNSUPPORTED.load(Relaxed),
                        MISMATCHES.load(Relaxed)
                    );
                }
            } else {
                let n = MISMATCHES.fetch_add(1, Relaxed) + 1;
                if n <= 40 {
                    eprintln!(
                        "[SHADOW-DECODE-MISMATCH] codec={} node={}\n  shadow={:?}\n  guest ={:?}",
                        class_name,
                        node.render(),
                        s,
                        g
                    );
                }
            }
        }
        // Native decode declined (unmodelled shape) — not a correctness signal.
        (None, _) => {
            UNSUPPORTED.fetch_add(1, Relaxed);
        }
        // Native produced a value the guest did not expose — treat as mismatch for visibility.
        (Some(s), None) => {
            let n = MISMATCHES.fetch_add(1, Relaxed) + 1;
            if n <= 40 {
                eprintln!(
                    "[SHADOW-DECODE-MISMATCH] codec={} node={}\n  shadow={:?}\n  guest =None",
                    class_name,
                    node.render(),
                    s
                );
            }
        }
    }
}

pub(crate) fn native_scalar(vm: &mut Vm, nv: &Value) -> Option<NScalar> {
    match nv {
        Value::Str(s) => return Some(NScalar::Text(s.to_string())),
        Value::Int(i) => return Some(NScalar::Num(i.to_string())),
        Value::Long(i) => return Some(NScalar::Num(i.to_string())),
        Value::Float(f) => return Some(NScalar::Num(f.to_string())),
        Value::Double(d) => return Some(NScalar::Num(d.to_string())),
        _ => {}
    }
    let class = val_class(vm, nv)?;
    match class.as_str() {
        "com/google/gson/JsonPrimitive" => {
            let inner = read_guest_field(vm, nv, "value")?;
            native_scalar(vm, &inner)
        }
        "com/google/gson/internal/LazilyParsedNumber" => {
            let s = read_guest_field(vm, nv, "value")?;
            Some(NScalar::Num(guest_string(vm, &s)?))
        }
        "java/lang/Boolean" => {
            let v = read_guest_field(vm, nv, "value")?;
            match v {
                Value::Int(i) => Some(NScalar::Bool(i != 0)),
                _ => None,
            }
        }
        "java/lang/String" => Some(NScalar::Text(guest_string(vm, nv)?)),
        "java/lang/Integer" | "java/lang/Long" | "java/lang/Short" | "java/lang/Byte"
        | "java/lang/Double" | "java/lang/Float" => {
            let v = read_guest_field(vm, nv, "value")?;
            native_scalar(vm, &v)
        }
        _ => {
            if class.starts_with("net/minecraft/nbt/") && class.ends_with("Tag") {
                let v = read_guest_field(vm, nv, "data")?;
                native_scalar(vm, &v)
            } else {
                None
            }
        }
    }
}
