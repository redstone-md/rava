//! Shadow decode interpreter (agy/gemini design, correctness-audited rewrite).
//!
//! INERT BY CONSTRUCTION: gated behind `RAVA_SHADOW_DECODE` (default off), it never touches the
//! guest's real returned `Value`. At each `com/mojang/serialization.decode` call for which we already
//! built a native CodecNode AST (21.2/21.3), it decodes the SAME input natively "in shadow", extracts
//! the guest's decoded value, and only COMPARES + logs a match/mismatch rate. It is read-only on the
//! VM heap (every field read is layout-guarded so a mis-guess returns None, never panics) and wrapped
//! in `catch_unwind` as belt-and-suspenders. It proves the codec-flatten thesis (a native structural
//! decode reproduces the JVM output bit-for-bit) WITHOUT any gamble on guest output — the go/no-go
//! evidence before a real native-decode switch is ever attempted.



use crate::codec_flatten::CodecNode;
use crate::value::Value;
use crate::{Vm, VmError};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_SHADOW_DECODE").is_some())
}

static ATTEMPTS: AtomicU64 = AtomicU64::new(0);
static MATCHES: AtomicU64 = AtomicU64::new(0);
static MISMATCHES: AtomicU64 = AtomicU64::new(0);
static UNSUPPORTED: AtomicU64 = AtomicU64::new(0);
// Linkage diagnostics: SEEN = decode hooks with >=3 args; NO_AST = receiver addr not in the AST registry.
static SEEN: AtomicU64 = AtomicU64::new(0);
static NO_AST: AtomicU64 = AtomicU64::new(0);
// Per-callsite class histogram of the unlinked decodes — the evidence that construction-time AST keys
// don't reach the runtime decode stream (dominated by shared leaf/wrapper codec singletons).
static NO_AST_BY_CLASS: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);

/// Panic-guarded entry. `catch_unwind` can never let a shadow panic reach the guest; the real safety,
/// though, is that every heap read below is layout-checked so panics don't happen in the first place.
pub fn shadow_decode(vm: &mut Vm, class_name: &str, args: &[Value], result: &Result<Option<Value>, VmError>) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        shadow_decode_impl(vm, class_name, args, result);
    }));
}

/// Native structural decode. Only the fully-structural shapes are handled; anything else → None
/// (counted UNSUPPORTED, never a mismatch).
fn run_shadow_decode(vm: &mut Vm, node: &CodecNode, input: &Value) -> Option<Value> {
    match node {
        CodecNode::Primitive(_) => decode_primitive(vm, input),
        CodecNode::Field(name, sub) => {
            let val = get_field_from_object(vm, input, name)?;
            run_shadow_decode(vm, sub, &val)
        }
        _ => None,
    }
}

/// Concrete class of a heap Ref OR an arena Obj (the boot's DFU objects are arena-allocated Objs, which
/// the 22.1 Ref-only helpers rejected). Read-only.
fn val_class(vm: &Vm, val: &Value) -> Option<String> {
    match val {
        Value::Ref(h) => vm.heap.class_of(*h).map(|s| s.to_string()),
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) => crate::nextgen::class_name(crate::nextgen::arena_class_id(*p)).map(|s| s.to_string()),
        _ => None,
    }
}

/// Unwrap a primitive JSON/NBT/boxed value to its scalar. Read-only.
fn decode_primitive(vm: &mut Vm, input: &Value) -> Option<Value> {
    if matches!(input, Value::Int(_) | Value::Long(_) | Value::Float(_) | Value::Double(_) | Value::Str(_)) {
        return Some(input.clone());
    }
    let class = val_class(vm, input)?;
    match class.as_str() {
        "com/google/gson/JsonPrimitive" => {
            let val = read_guest_field(vm, input, "value")?;
            decode_primitive(vm, &val)
        }
        "java/lang/Integer" | "java/lang/Boolean" | "java/lang/Long" | "java/lang/Double"
        | "java/lang/Float" | "java/lang/Short" | "java/lang/Byte" => {
            read_guest_field(vm, input, "value")
        }
        "java/lang/String" => Some(input.clone()),
        _ => {
            if class.starts_with("net/minecraft/nbt/") && class.ends_with("Tag") {
                let val = read_guest_field(vm, input, "data")?;
                decode_primitive(vm, &val)
            } else {
                None
            }
        }
    }
}

/// Native BST walk of a gson LinkedTreeMap: compare the guest String key at each node, descend. RO.
fn linked_tree_get(vm: &mut Vm, node: &Value, key: &str) -> Option<Value> {
    let mut cur = node.clone();
    for _ in 0..64 {
        // depth-guard: RB tree over N keys is ~2·log2(N) deep
        if matches!(cur, Value::Null) {
            return None;
        }
        let nkey = read_guest_field(vm, &cur, "key")?;
        let ks = guest_string(vm, &nkey)?;
        match key.cmp(&ks) {
            std::cmp::Ordering::Equal => return read_guest_field(vm, &cur, "value"),
            std::cmp::Ordering::Less => cur = read_guest_field(vm, &cur, "left")?,
            std::cmp::Ordering::Greater => cur = read_guest_field(vm, &cur, "right")?,
        }
    }
    None
}

/// Scan a MapLike wrapper's fields for a JsonObject/LinkedTreeMap it delegates to. RO.
fn find_json_container(vm: &mut Vm, input: &Value) -> Option<Value> {
    let class = val_class(vm, input)?;
    let layout = vm.class_field_layout(&class);
    let fids: Vec<u32> = layout.iter().map(|(fid, _)| *fid).collect();
    for fid in fids {
        let Some(v) = vm.read_value_field(input, fid) else { continue };
        if let Some(c) = val_class(vm, &v) {
            if c == "com/google/gson/JsonObject" || c.contains("LinkedTreeMap") {
                return Some(v);
            }
        }
    }
    None
}

/// A guest java.lang.String → Rust String (Ref or arena Obj).
fn guest_string(vm: &mut Vm, v: &Value) -> Option<String> {
    match v {
        Value::Str(s) => Some(s.to_string()),
        Value::Null => None,
        _ => vm.value_to_string(v).ok(),
    }
}

/// Navigate the DFU success result → the decoded object. RO. Handles both the modern sealed record
/// `DataResult$Success(value, lifecycle)` (read `value` directly) and the legacy `result: Either$Left`.
fn extract_result_from_data_result(vm: &mut Vm, val: &Value) -> Option<Value> {
    if !val_class(vm, val)?.contains("DataResult") {
        return None;
    }
    // Modern DFU: DataResult$Success has a `value` field = the decoded object (or Pair). Prefer it;
    // fall back to the legacy `result → Either$Left/Optional → value` shape.
    let inner = read_guest_field(vm, val, "value").or_else(|| {
        let res = read_guest_field(vm, val, "result")?;
        let rc = val_class(vm, &res)?;
        if rc.contains("Left") || rc.contains("Optional") {
            read_guest_field(vm, &res, "value")
        } else {
            None
        }
    })?;
    // `inner` is EITHER a Pair<value, remaining> (Decoder.decode) OR the decoded object DIRECTLY
    // (a record MapDecoder.decode). Peel the Pair only if present.
    if val_class(vm, &inner).map(|c| c.contains("Pair")).unwrap_or(false) {
        return read_guest_field(vm, &inner, "first");
    }
    Some(inner)
}

/// Layout-guarded field read (Ref or arena Obj): resolve the id, verify it's in the class layout, read.
/// Reading a non-layout field id on an arena object PANICS, so the layout guard is the real safety net.
fn read_guest_field(vm: &mut Vm, recv: &Value, field: &str) -> Option<Value> {
    let class = val_class(vm, recv)?;
    let id = vm.resolve_field_id(&class, field);
    let layout = vm.class_field_layout(&class);
    if !layout.iter().any(|(fid, _)| *fid == id) {
        return None;
    }
    vm.read_value_field(recv, id)
}

fn values_are_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Long(x), Value::Long(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y || (x.is_nan() && y.is_nan()),
        (Value::Double(x), Value::Double(y)) => x == y || (x.is_nan() && y.is_nan()),
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Ref(x), Value::Ref(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => x == y,
        (Value::Null, Value::Null) => true,
        // cross-type: a boxed Int vs primitive Int etc. are already unwrapped above, so any
        // remaining cross-type pair is a genuine shape difference.
        _ => false,
    }
}

/// Final tally, printed at shutdown next to the codec-flatten dump.
pub fn dump() {
    if !enabled() {
        return;
    }
    eprintln!(
        "[SHADOW-DECODE] FINAL seen={} no_ast={} attempts={} matches={} mismatches={} unsupported={}",
        SEEN.load(Relaxed),
        NO_AST.load(Relaxed),
        ATTEMPTS.load(Relaxed),
        MATCHES.load(Relaxed),
        MISMATCHES.load(Relaxed),
        UNSUPPORTED.load(Relaxed)
    );
    let g = NO_AST_BY_CLASS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(m) = g.as_ref() {
        let mut rows: Vec<(&String, &u64)> = m.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1));
        eprintln!("[SHADOW-DECODE] unlinked decode callsites (construction-keyed AST never reaches these):");
        for (cls, n) in rows.iter().take(20) {
            eprintln!("[SHADOW-DECODE]   {:>5}  {}", n, cls);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// 24.3 SHADOW-FLAT (RAVA_JIT_SHADOW_FLAT, inert): for an ALL-PRIMITIVE record (a Codec-Tree-Flatten
// target), compare the NATIVE flat read of each field (from `input`) to the field value in the
// DFU-decoded object. Proves `native_read(input, field) == DFU_object.field` on the boot's real
// record stream, WITHOUT allocating a shadow object (the alloc+fill is the interpreter's already-proven
// native `new`+`putfield`, materialized in 24.4). Read-only ⇒ GC-safe + bit-exact.
pub fn flat_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_JIT_SHADOW_FLAT").is_some())
}

static FLAT_RECORDS: AtomicU64 = AtomicU64::new(0);
static FLAT_MATCH: AtomicU64 = AtomicU64::new(0);
static FLAT_MISMATCH: AtomicU64 = AtomicU64::new(0);
static FLAT_UNMAPPED: AtomicU64 = AtomicU64::new(0);
static FLAT_CALLED: AtomicU64 = AtomicU64::new(0); // diag: flat_at_return entered (guard was a record)
static FLAT_TARGET: AtomicU64 = AtomicU64::new(0); // diag: top frame was an all-primitive target
static FLAT_EXTRACT_FAIL: AtomicU64 = AtomicU64::new(0); // diag: target but DFU-result extract failed
static FLAT_NATIVE_FAIL: AtomicU64 = AtomicU64::new(0); // diag: native (JSON/NBT) field read returned None
static FLAT_DFU_FAIL: AtomicU64 = AtomicU64::new(0); // diag: DFU-object field read returned None (name miss)
static FLAT_NONPRIM: AtomicU64 = AtomicU64::new(0); // diag: DFU field decoded to an object (not flattenable)

pub fn flat_arm_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_JIT_FLAT_ARM").is_some())
}

static ARM_RECORDS: AtomicU64 = AtomicU64::new(0); // armable records a shadow object was built for
static ARM_FMATCH: AtomicU64 = AtomicU64::new(0); // per-field: shadow == DFU
static ARM_FMISMATCH: AtomicU64 = AtomicU64::new(0);
static ARM_NOTARMABLE: AtomicU64 = AtomicU64::new(0); // a component wasn't a native-readable scalar
static ARM_BUILD_FAIL: AtomicU64 = AtomicU64::new(0); // ctor resolve / native read / new_instance failed
// 24.5 not-armable reason breakdown (where the coverage is locked up):
static NOTARM_COUNT: AtomicU64 = AtomicU64::new(0); // component count != codec field count
static NOTARM_OBJECT: AtomicU64 = AtomicU64::new(0); // an L… object component (record / registry entry)
static NOTARM_ARRAY: AtomicU64 = AtomicU64::new(0); // a [ list/array component
static BLOCKERS: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None); // blocking descriptor → count
static COUNTMIS: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None); // count-mismatch class(nI/nF) → count

/// Tally a blocking component descriptor (for the 24.5 "what to flatten next" histogram).
fn note_blocker(desc: &str, fname: &str) {
    let key = format!("{fname}:{desc}");
    let mut g = BLOCKERS.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    *m.entry(key).or_insert(0) += 1;
}

/// Tally a count-mismatch record: instance-field count vs observed codec-field count, by class.
fn note_count_mismatch(class: &str, n_inst: usize, n_codec: usize) {
    let key = format!("{class}(inst={n_inst},codec={n_codec})");
    let mut g = COUNTMIS.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    *m.entry(key).or_insert(0) += 1;
}

/// Instance (non-static) fields of a class, (name, jvm-descriptor) in declaration order = the canonical
/// record-ctor parameter order. Records store components 1:1 with ctor params in this order.
fn instance_fields(vm: &mut Vm, class: &str) -> Option<Vec<(String, String)>> {
    let c = vm.loader.get_or_load(class).ok()?;
    let mut out = Vec::new();
    for f in &c.file.fields {
        if f.access_flags & 0x0008 == 0 {
            // not ACC_STATIC
            out.push((f.name.clone(), f.descriptor.clone()));
        }
    }
    Some(out)
}

/// A JVM descriptor that decode_flat can read natively from JSON/NBT: a scalar or a String.
fn is_flat_descriptor(desc: &str) -> bool {
    matches!(desc, "Z" | "B" | "C" | "S" | "I" | "J" | "F" | "D" | "Ljava/lang/String;")
}

/// Read `key` from `input` and coerce to the exact JVM type `desc` expects (the ctor param type).
fn native_typed(vm: &mut Vm, input: &Value, key: &str, desc: &str) -> Option<Value> {
    let nv = get_field_from_object(vm, input, key)?;
    let ns = native_scalar(vm, &nv)?;
    let num = match &ns {
        NScalar::Num(s) => Some(s.clone()),
        NScalar::Bool(b) => Some(if *b { "1".into() } else { "0".into() }),
        NScalar::Text(_) => None,
    };
    match desc {
        "Z" | "B" | "C" | "S" | "I" => num.and_then(|s| s.parse::<f64>().ok()).map(|n| Value::Int(n as i32)),
        "J" => num.and_then(|s| s.parse::<f64>().ok()).map(|n| Value::Long(n as i64)),
        "F" => num.and_then(|s| s.parse::<f32>().ok()).map(Value::Float),
        "D" => num.and_then(|s| s.parse::<f64>().ok()).map(Value::Double),
        "Ljava/lang/String;" => match ns {
            NScalar::Text(t) => Some(Value::Str(t.into())),
            _ => None,
        },
        _ => None,
    }
}

/// Compare two guest scalar/String field values by the field's declared type. Strings are compared by
/// content (shadow holds Value::Str; the DFU field may hold an arena/heap String object).
fn scalar_field_eq(vm: &mut Vm, a: &Value, b: &Value, desc: &str) -> bool {
    if desc == "Ljava/lang/String;" {
        return match (guest_string(vm, a), guest_string(vm, b)) {
            (Some(x), Some(y)) => x == y,
            (None, None) => true,
            _ => false,
        };
    }
    values_are_equal(a, b)
}

/// DFU-object field read by codec key, trying the raw key then its camelCase form (min_inclusive→minInclusive).
fn dfu_field(vm: &mut Vm, obj: &Value, key: &str) -> Option<Value> {
    if let Some(v) = read_guest_field(vm, obj, key) {
        return Some(v);
    }
    let camel = snake_to_camel(key);
    if camel != key {
        return read_guest_field(vm, obj, &camel);
    }
    None
}

fn snake_to_camel(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut up = false;
    for ch in s.chars() {
        if ch == '_' {
            up = true;
        } else if up {
            out.extend(ch.to_uppercase());
            up = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// A stored scalar: number/bool(Int)/string. Ref/Obj/Array = decoded-to-object → not flat.
fn is_scalar(v: &Value) -> bool {
    matches!(
        v,
        Value::Int(_) | Value::Long(_) | Value::Float(_) | Value::Double(_) | Value::Str(_)
    )
}

/// Reduce a raw JSON/boxed input value to a comparable scalar form: Text(String) | Num(String) | Bool.
#[derive(Debug)]
enum NScalar {
    Text(String),
    Num(String),
    Bool(bool),
}

/// Type-directed compare: parse the native scalar to the DFU field's concrete type, then compare bit-exact.
/// Returns None if the native value can't be reduced to a scalar at all.
fn native_matches_scalar(vm: &mut Vm, nv: &Value, dfu: &Value) -> Option<bool> {
    let ns = native_scalar(vm, nv)?;
    let num_str = match &ns {
        NScalar::Num(s) => Some(s.clone()),
        NScalar::Bool(b) => Some(if *b { "1".into() } else { "0".into() }),
        NScalar::Text(_) => None,
    };
    let eq = match dfu {
        Value::Int(i) => match &ns {
            NScalar::Bool(b) => (*b as i64) == *i as i64,
            _ => num_str.and_then(|s| s.parse::<f64>().ok()).map(|n| n as i64 == *i as i64).unwrap_or(false),
        },
        Value::Long(l) => num_str.and_then(|s| s.parse::<f64>().ok()).map(|n| n as i64 == *l).unwrap_or(false),
        Value::Float(f) => num_str.and_then(|s| s.parse::<f32>().ok()).map(|n| n == *f).unwrap_or(false),
        Value::Double(d) => num_str.and_then(|s| s.parse::<f64>().ok()).map(|n| n == *d).unwrap_or(false),
        Value::Str(s) => match &ns {
            NScalar::Text(t) => &t == &s.as_ref(),
            _ => false,
        },
        _ => false,
    };
    Some(eq)
}
mod flat;
pub use flat::*;
mod decode;
pub(crate) use decode::*;
