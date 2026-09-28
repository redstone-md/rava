//! Annotations and the JDK generic signatures kept for known classes.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::reflect::*;

/// Whether `class` has annotation `want` (internal name), honoring @Inherited.
/// The annotation may be declared directly on the class, or — if the annotation
/// type is itself @Inherited — inherited from a superclass. netty's @Sharable is
/// @Inherited and declared on ChannelInitializer, so a handler SUBCLASS
/// (ServerConnection$4) must report it present, else netty refuses to add the
/// shared childHandler to a second connection's pipeline ("not a @Sharable
/// handler") and every login after the first status ping breaks.
pub(crate) fn class_has_annotation(vm: &mut Vm, class: &str, want: &str) -> bool {
    let want_desc = format!("L{want};");
    if let Ok(c) = vm.loader.get_or_load(class) {
        if c.file.annotations.iter().any(|a| *a == want_desc) { return true; }
    }
    let inherited = vm.loader.get_or_load(want).ok()
        .map(|ac| ac.file.annotations.iter().any(|a| a == "Ljava/lang/annotation/Inherited;"))
        .unwrap_or(false);
    if !inherited { return false; }
    let mut cur = vm.loader.get_or_load(class).ok().and_then(|c| c.file.super_class.clone());
    let mut guard = 0;
    while let Some(sc) = cur {
        if sc == "java/lang/Object" || guard > 50 { break; }
        guard += 1;
        match vm.loader.get_or_load(&sc) {
            Ok(c) => {
                if c.file.annotations.iter().any(|a| *a == want_desc) { return true; }
                cur = c.file.super_class.clone();
            }
            Err(_) => break,
        }
    }
    false
}

/// Declaring-class name of a TypeVariable handle (empty if unset).
pub(crate) fn decl_name(vm: &Vm, tv: u32) -> String {
    match vm.heap.get_field(tv, "$decl") {
        Value::Ref(d) => field_str(vm, Some(&Value::Ref(d)), "$name").unwrap_or_default(),
        _ => String::new(),
    }
}

/// Generic metadata for the JDK collection/map types (which have no classfile here,
/// so reflection can't read it). Returns (this class's type-parameter names, its
/// DIRECT generic super-interfaces as (raw name, the type-arg names threaded from
/// THIS class's parameters)). This is what lets gson's `$Gson$Types` walk e.g.
/// `List<UserCacheEntry>` → `Collection<E>` and resolve E := UserCacheEntry, so it
/// picks the reflective adapter instead of falling to LinkedTreeMap (fixes the
/// UserCache/ops.json gson deserialization bug). Rule-clean: keyed on JDK type
/// SEMANTICS (the real java.util generic signatures), not on any MC class.
pub(crate) fn jdk_generics(cls: &str) -> Option<(&'static [&'static str], &'static [(&'static str, &'static [&'static str])])> {
    Some(match cls {
        "java/util/Collection" | "java/lang/Iterable" | "java/util/AbstractCollection" => (&["E"], &[]),
        "java/util/List" | "java/util/Set" | "java/util/Queue" | "java/util/SortedSet"
        | "java/util/NavigableSet" => (&["E"], &[("java/util/Collection", &["E"])]),
        "java/util/Deque" => (&["E"], &[("java/util/Queue", &["E"])]),
        "java/util/AbstractList" | "java/util/ArrayList" | "java/util/LinkedList"
        | "java/util/Vector" | "java/util/Stack" | "java/util/concurrent/CopyOnWriteArrayList" => {
            (&["E"], &[("java/util/List", &["E"])])
        }
        "java/util/AbstractSet" | "java/util/HashSet" | "java/util/LinkedHashSet"
        | "java/util/TreeSet" => (&["E"], &[("java/util/Set", &["E"])]),
        "java/util/Map" | "java/util/AbstractMap" => (&["K", "V"], &[]),
        "java/util/HashMap" | "java/util/LinkedHashMap" | "java/util/TreeMap"
        | "java/util/Hashtable" | "java/util/concurrent/ConcurrentHashMap" => {
            (&["K", "V"], &[("java/util/Map", &["K", "V"])])
        }
        _ => return None,
    })
}

/// Method surface of the synthesized ParameterizedType objects.
pub(crate) fn parameterized_type(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    let recv = match args.first() { Some(Value::Ref(h)) => *h, _ => return Ok(None) };
    let ret = match name {
        "getRawType" => vm.heap.get_field(recv, "$rawtype"),
        "getActualTypeArguments" => vm.heap.get_field(recv, "$targs"),
        "getOwnerType" => Value::Null,
        "getTypeName" | "toString" => {
            let raw = match vm.heap.get_field(recv, "$rawtype") {
                v @ Value::Ref(_) => field_str(vm, Some(&v), "$name").unwrap_or_default(),
                _ => String::new(),
            };
            Value::Str(raw.replace('/', ".").into())
        }
        "hashCode" => Value::Int(recv as i32),
        _ => return Ok(None),
    };
    Ok(Some(Some(ret)))
}
