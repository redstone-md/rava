//! Turning class names into descriptors and back.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::reflect::*;

/// Read a string-ish field ($name/$path/$str) from a heap object.
/// Whether a JDK type (no bytecode in the jar) is an interface — for
/// `Class.isInterface` at the JDK boundary. Covers the collection/functional/io
/// interfaces that reflective libraries (gson type resolution) walk.
pub(crate) fn is_jdk_interface(cls: &str) -> bool {
    matches!(cls,
        "java/util/Collection" | "java/util/List" | "java/util/Set" | "java/util/Map"
        | "java/util/Queue" | "java/util/Deque" | "java/util/SortedSet" | "java/util/SortedMap"
        | "java/util/NavigableSet" | "java/util/NavigableMap" | "java/util/Iterator"
        | "java/util/ListIterator" | "java/util/Map$Entry" | "java/util/RandomAccess"
        | "java/util/concurrent/ConcurrentMap" | "java/util/concurrent/BlockingQueue"
        | "java/util/concurrent/Future" | "java/util/concurrent/Callable"
        | "java/util/concurrent/Executor" | "java/util/concurrent/ExecutorService"
        | "java/lang/Iterable" | "java/lang/Comparable" | "java/lang/Runnable"
        | "java/lang/CharSequence" | "java/lang/AutoCloseable" | "java/lang/Cloneable"
        | "java/io/Serializable" | "java/io/Closeable" | "java/io/Flushable"
        | "java/io/DataInput" | "java/io/DataOutput" | "java/io/Externalizable")
        || cls.starts_with("java/util/function/")
        || cls.starts_with("java/util/stream/") && cls.ends_with("Stream")
}

/// Map a `java/lang/Class` object (or its $name) to a descriptor token.
pub(crate) fn desc_token(name: &str) -> String {
    match name {
        "int" => "I".into(),
        "long" => "J".into(),
        "boolean" => "Z".into(),
        "byte" => "B".into(),
        "char" => "C".into(),
        "short" => "S".into(),
        "float" => "F".into(),
        "double" => "D".into(),
        "void" => "V".into(),
        n if n.starts_with('[') => n.replace('.', "/"), // already array descriptor
        n => format!("L{};", n.replace('.', "/")),
    }
}

/// Build an argument descriptor from a `Class[]` (varargs) of parameter types.
pub(crate) fn arg_desc(vm: &Vm, class_array: Option<&Value>) -> String {
    crate::natives::varargs(vm, class_array)
        .iter()
        .filter_map(|c| match c {
            Value::Ref(h) => match vm.heap.get_field_syn(*h, crate::heap::Syn::Name) {
                Value::Str(s) => Some(desc_token(&s)),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

// --- java.lang.Class --------------------------------------------------------

/// Field descriptor -> a class binary name for getType (best-effort).
pub(crate) fn desc_token_to_class(desc: &str) -> String {
    match desc.bytes().next() {
        Some(b'L') => desc[1..desc.len().saturating_sub(1)].to_string(),
        Some(b'[') => desc.to_string(),
        Some(b'I') => "int".into(), Some(b'J') => "long".into(), Some(b'Z') => "boolean".into(),
        Some(b'D') => "double".into(), Some(b'F') => "float".into(), Some(b'B') => "byte".into(),
        Some(b'C') => "char".into(), Some(b'S') => "short".into(),
        Some(b'V') => "void".into(),
        _ => "java/lang/Object".into(),
    }
}

/// Split a parameter descriptor string (the part between `(` and `)`) into the
/// individual field descriptors, e.g. `"ILjava/lang/String;[I"` → `["I",
/// "Ljava/lang/String;", "[I"]`.
pub(crate) fn split_param_descs(s: &str) -> Vec<String> {
    let b = s.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        let start = i;
        while i < b.len() && b[i] == b'[' { i += 1; }
        if i < b.len() && b[i] == b'L' {
            while i < b.len() && b[i] != b';' { i += 1; }
            i += 1; // consume ';'
        } else {
            i += 1; // single primitive char
        }
        out.push(s[start..i.min(s.len())].to_string());
    }
    out
}

pub(crate) fn make_ctor(vm: &mut Vm, cls: &str, arg_desc: &str) -> Value {
    let h = vm.heap.alloc("java/lang/reflect/Constructor");
    vm.heap.put_field(h, "$class", Value::Str(cls.into()));
    vm.heap.put_field(h, "$argdesc", Value::Str(arg_desc.into()));
    Value::Ref(h)
}

pub(crate) fn retype(vm: &mut Vm, v: Option<&Value>, class: &str) -> Value {
    let (path, s) = match v {
        Some(Value::Ref(h)) => (strfield(vm, *h, "$path"), strfield(vm, *h, "$str")),
        _ => (String::new(), String::new()),
    };
    let jarres = match v { Some(Value::Ref(h)) => strfield(vm, *h, "$jarres"), _ => String::new() };
    let h = vm.heap.alloc(class);
    vm.heap.put_field_syn(h, crate::heap::Syn::Path, Value::Str(path.into()));
    vm.heap.put_field_syn(h, crate::heap::Syn::Str, Value::Str(s.into()));
    if !jarres.is_empty() { vm.heap.put_field(h, "$jarres", Value::Str(jarres.into())); }
    Value::Ref(h)
}

/// Whether a value is a Comparator (vs a key-extractor Function) — used to pick
/// the thenComparing overload. True for a `$cmp`-tagged comparator, an object of
/// class java/util/Comparator, or a lambda whose SAM takes two args (compare).
pub(crate) fn value_is_comparator(vm: &Vm, v: &Value) -> bool {
    if let Value::Ref(h) = v {
        if matches!(vm.heap.get_field(*h, "$cmp"), Value::Str(_)) {
            return true;
        }
        if vm.heap.class_of(*h) == Some("java/util/Comparator") {
            return true;
        }
    }
    // A raw comparator lambda (handle OR RAVA_ARENA_LAMBDA arena-lambda): SAM arity 2.
    if let Some(lam) = vm.value_lambda_of(v) {
        return crate::count_args(&lam.descriptor()) == 2;
    }
    false
}

/// Read `$elems` of a collection/iterable argument.
pub(crate) fn collection_elems(vm: &Vm, v: Option<&Value>) -> Vec<Value> {
    if let Some(Value::Ref(h)) = v {
        if let Value::Array(a) = vm.heap.get_field_syn(*h, crate::heap::Syn::Elems) {
            // Spare capacity is not elements — see `Heap::elems_values`.
            return vm.heap.elems_values(*h, a);
        }
    }
    Vec::new()
}

// --- java.lang.invoke MethodHandles / Lookup / VarHandle --------------------

/// Coerce an unset field to its type default for VarHandle reads.
pub(crate) fn vh_default(v: Value, tdesc: &str) -> Value {
    if !matches!(v, Value::Null) { return v; }
    match tdesc.bytes().next() {
        Some(b'J') => Value::Long(0),
        Some(b'F') => Value::Float(0.0),
        Some(b'D') => Value::Double(0.0),
        Some(b'Z') | Some(b'B') | Some(b'C') | Some(b'S') | Some(b'I') => Value::Int(0),
        _ => Value::Null,
    }
}

pub(crate) fn val_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Long(x), Value::Long(y)) => x == y,
        (Value::Ref(x), Value::Ref(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Null, Value::Null) => true,
        // Arena objects compare by IDENTITY (same arena address) — matching the
        // bytecode `if_acmp`/`==` semantics that Atomic*.compareAndSet/Exchange
        // mirror. Without this arm two identical arena Value::Obj fell to `false`,
        // so every arena-object CAS silently skipped its store while the caller's
        // own `if_acmpne` witness check saw success — desyncing e.g. the worldgen
        // ChunkStatus state machine (GenerationChunkHolder.startedWork never
        // advanced past `empty` → "Unexpected last startedWork status").
        #[cfg(feature = "next-gen-gc")]
        (Value::Obj(x), Value::Obj(y)) => x.addr() == y.addr(),
        _ => false,
    }
}
