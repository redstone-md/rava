//! Reading generic signatures back out of a class file.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::reflect::*;

/// Parse the superclass part of a CLASS Signature attribute and synthesize a
/// `ParameterizedType` when the super is parameterized (else None → caller
/// falls back to the raw mirror). Grammar: `[<TypeParams>] Super Interfaces*`.
/// Extract formal type-parameter NAMES from a class generic signature, e.g.
/// `<I:Ljava/lang/Object;>L...;` -> ["I"], `<K:..;V:..>..` -> ["K","V"]. A name
/// sits at depth 0 of the leading `<...>` block, right after the block start or a
/// bound-closing `;`, and runs up to its `:`.
pub(crate) fn formal_type_param_names(sig: &str) -> Vec<String> {
    let b = sig.as_bytes();
    if b.first() != Some(&b'<') { return Vec::new(); }
    // Find the matching '>' that closes the leading block.
    let mut depth = 0usize;
    let mut end = b.len();
    for (i, &c) in b.iter().enumerate() {
        if c == b'<' { depth += 1; }
        else if c == b'>' { depth -= 1; if depth == 0 { end = i; break; } }
    }
    let inner = &b[1..end.min(b.len())];
    let mut out = Vec::new();
    let mut d = 0i32;
    let mut i = 0;
    let mut at_param_start = true;
    while i < inner.len() {
        if d == 0 && at_param_start {
            let s = i;
            while i < inner.len() && !matches!(inner[i], b':' | b'<' | b'>' | b';') { i += 1; }
            if i < inner.len() && inner[i] == b':' && i > s {
                out.push(String::from_utf8_lossy(&inner[s..i]).to_string());
            }
            at_param_start = false;
            continue;
        }
        match inner[i] { b'<' => d += 1, b'>' => d -= 1, b';' if d == 0 => at_param_start = true, _ => {} }
        i += 1;
    }
    out
}

pub(crate) fn generic_super_from_signature(vm: &mut Vm, sig: &str) -> Option<Value> {
    let b = sig.as_bytes();
    let mut i = 0;
    // Skip the class's own <TypeParams> block (balanced angle brackets).
    if b.first() == Some(&b'<') {
        let mut depth = 0usize;
        while i < b.len() {
            match b[i] {
                b'<' => depth += 1,
                b'>' => {
                    depth -= 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
    let (t, _) = sig_type(vm, b, i)?;
    // Only a parameterized super is interesting here.
    match &t {
        Value::Ref(h) if vm.heap.class_of(*h) == Some("java/lang/reflect/ParameterizedType") => Some(t),
        _ => None,
    }
}

/// A reflected ParameterizedType: raw-type mirror + actual type arguments.
/// The parameterized DIRECT super-interfaces from a class generic Signature
/// (`<TypeParams>SuperSig Iface1Sig Iface2Sig…`) — skip the type-param block + the
/// superclass, then parse each interface signature into a Type mirror.
pub(crate) fn generic_ifaces_from_signature(vm: &mut Vm, sig: &str) -> Vec<Value> {
    let b = sig.as_bytes();
    let mut i = 0;
    if b.first() == Some(&b'<') {
        let mut depth = 0usize;
        while i < b.len() {
            match b[i] {
                b'<' => depth += 1,
                b'>' => { depth -= 1; if depth == 0 { i += 1; break; } }
                _ => {}
            }
            i += 1;
        }
    }
    // Skip the superclass signature.
    match sig_type(vm, b, i) {
        Some((_, ni)) => i = ni,
        None => return Vec::new(),
    }
    let mut out = Vec::new();
    while i < b.len() {
        match sig_type(vm, b, i) {
            Some((t, ni)) if ni > i => { out.push(t); i = ni; }
            _ => break,
        }
    }
    out
}
