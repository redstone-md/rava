//! java.lang.Class and the class loader.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::reflect::*;

/// How a class is nested, as javac's binary name records it.
enum Nest { TopLevel, Member, Local, Anonymous }

/// Classify a binary name: see the `isAnonymousClass` arm for the naming rules.
fn nest_kind(binary: &str) -> Nest {
    let Some((_, tail)) = binary.rsplit_once('$') else { return Nest::TopLevel };
    let mut bytes = tail.bytes();
    match bytes.next() {
        None => Nest::TopLevel,
        Some(b) if b.is_ascii_digit() => {
            if bytes.all(|b| b.is_ascii_digit()) { Nest::Anonymous } else { Nest::Local }
        }
        Some(_) => Nest::Member,
    }
}

/// `Class.toString()`: `("interface " | "class " | "") + getName()`.
///
/// Shared with `Vm::value_to_string`, which renders a `java/lang/Class` receiver
/// when the guest concatenates one — the two must not disagree about the format.
pub(crate) fn class_display_name(vm: &mut Vm, binary: &str) -> String {
    let prefix = if matches!(binary,
        "int" | "long" | "boolean" | "char" | "byte" | "short" | "float" | "double" | "void") {
        ""
    } else if vm.loader.get_or_load(binary)
        .map(|c| c.file.access_flags & 0x0200 != 0)
        .unwrap_or_else(|_| is_jdk_interface(binary))
    {
        "interface "
    } else {
        "class "
    };
    format!("{prefix}{}", binary.replace('/', "."))
}

/// `Class.getSimpleName()` from a `$name`: a binary name (`P2$Nested$Deeper`),
/// a primitive word (`int`), or an array descriptor (`[[LP2$A;`).
///
/// An array is `component[]`; a nested or local class keeps only the part after
/// the last `$` (javac prefixes a LOCAL class with digits, which the JVM drops);
/// an anonymous class has no simple name at all.
/// The CANONICAL name — the one a Java source file would write. Same as the binary name
/// for an ordinary class, `int[]` rather than `[I` for an array, and dots for the package
/// separator throughout. `getName` keeps the binary form; only `getCanonicalName` and
/// `getTypeName` come here.
fn canonical_name_of(binary: &str) -> String {
    if let Some(elem) = binary.strip_prefix('[') {
        return format!("{}[]", canonical_name_of(&descriptor_to_name(elem)));
    }
    binary.replace('/', ".")
}

fn simple_name_of(binary: &str) -> String {
    if let Some(elem) = binary.strip_prefix('[') {
        return format!("{}[]", simple_name_of(&descriptor_to_name(elem)));
    }
    let after_pkg = binary.rsplit('/').next().unwrap_or(binary);
    let after_outer = after_pkg.rsplit('$').next().unwrap_or(after_pkg);
    after_outer.trim_start_matches(|c: char| c.is_ascii_digit()).to_string()
}

/// One field descriptor as a class name: `I` → `int`, `LP2$A;` → `P2$A`,
/// `[I` stays an array descriptor for `simple_name_of` to peel again.
pub(crate) fn descriptor_to_name(desc: &str) -> String {
    match desc.as_bytes().first() {
        Some(b'I') => "int".into(),
        Some(b'J') => "long".into(),
        Some(b'Z') => "boolean".into(),
        Some(b'B') => "byte".into(),
        Some(b'C') => "char".into(),
        Some(b'S') => "short".into(),
        Some(b'F') => "float".into(),
        Some(b'D') => "double".into(),
        Some(b'L') => desc[1..desc.len().saturating_sub(1)].to_string(),
        _ => desc.to_string(),
    }
}

pub(crate) fn class_method(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    let ret = match name {
        "forName" => {
            // forName(String) | forName(String, boolean, ClassLoader)
            let cn = match args.first() {
                Some(Value::Str(s)) => binary(s),
                _ => return Ok(Some(Some(Value::Null))),
            };
            match vm.loader.get_or_load(&cn) {
                Ok(c) => { vm.ensure_init(&c)?; vm.make_class(&cn) }
                // Core `java.*` classes are native-backed (no bytecode) but
                // always present in a conformant JDK. Libraries feature-detect
                // them via Class.forName (e.g. joml's class$ probing
                // java.lang.Math.fma / Long.rotateLeft); a ClassNotFoundException
                // here wrongly escapes as NoClassDefFoundError and aborts boot.
                // Synthesize a mirror so getDeclaredMethod can then probe.
                // Third-party names keep CNFE semantics for genuine detection.
                Err(_) if cn.starts_with("java/") || cn.starts_with("sun/") || cn.starts_with("jdk/") => vm.make_class(&cn),
                Err(_) => return Err(VmError::Throw(vm.make_exception("java/lang/ClassNotFoundException", &cn.replace('/', ".")))),
            }
        }
        // `getName` is the BINARY name — `[I` for an int array — while
        // `getCanonicalName` and `getTypeName` are the source-level one, `int[]`. All
        // three shared one arm, so an array reported `[I` where the JVM reports `int[]`,
        // in exactly the places a canonical name is asked for: messages and codec keys.
        "getName" => {
            Value::Str(field_str(vm, args.first(), "$name").unwrap_or_default().replace('/', ".").into())
        }
        "getCanonicalName" | "getTypeName" => {
            let n = field_str(vm, args.first(), "$name").unwrap_or_default();
            Value::Str(canonical_name_of(&n).into())
        }
        // getSimpleName: the name WITHOUT any package or enclosing class, and an
        // array renders as `component[]`. Splitting only on the package separator
        // returned the binary name for every nested class (`P2$A` instead of `A`)
        // and the raw descriptor for every array (`[I` instead of `int[]`) — both
        // of them names guests put in messages and use as map keys.
        "getSimpleName" => {
            let n = field_str(vm, args.first(), "$name").unwrap_or_default();
            Value::Str(simple_name_of(&n).into())
        }
        // Class.toString() is `("interface "|"class "|"") + getName()`, not the
        // inherited Object.toString — it was falling through to `java.lang.Class@0`,
        // so every log line that printed a Class printed a handle number.
        "toString" => {
            let n = field_str(vm, args.first(), "$name").unwrap_or_default();
            Value::Str(class_display_name(vm, &n).into())
        }
        "getMethod" | "getDeclaredMethod" => {
            let cls = field_str(vm, args.first(), "$name").unwrap_or_default();
            let mname = match args.get(1) { Some(Value::Str(s)) => s.to_string(), _ => return Ok(Some(Some(Value::Null))) };
            let ad = arg_desc(vm, args.get(2));
            make_method(vm, &cls, &mname, &ad)
        }
        "getConstructor" | "getDeclaredConstructor" => {
            let cls = field_str(vm, args.first(), "$name").unwrap_or_default();
            let ad = arg_desc(vm, args.get(1));
            make_ctor(vm, &cls, &ad)
        }
        "newInstance" => {
            let cls = field_str(vm, args.first(), "$name").unwrap_or_default();
            new_instance(vm, &cls, "", &[])?
        }
        "getFields" | "getDeclaredFields" => {
            let cls = field_str(vm, args.first(), "$name").unwrap_or_default();
            let fields: Vec<(String, String)> = match vm.loader.get_or_load(&cls) {
                Ok(c) => c.file.fields.iter().map(|f| (f.name.clone(), f.descriptor.clone())).collect(),
                Err(_) => Vec::new(),
            };
            let objs: Vec<Value> = fields.into_iter().map(|(n, d)| make_field(vm, &cls, &n, &d)).collect();
            Value::Array(vm.heap.alloc_values("Ljava/lang/Object;", objs))
        }
        // `getRecordComponents` — the components in DECLARATION order, which for a record
        // are exactly its instance fields. Absent before, so any codec that reflected over
        // a record hit an UnsatisfiedLinkError; the codec libraries this jar ships lean on
        // records heavily.
        //
        // Returns `null` for a non-record, as the JDK does, rather than an empty array —
        // `getRecordComponents() == null` is the documented way to ask.
        "getRecordComponents" => {
            let cls = field_str(vm, args.first(), "$name").unwrap_or_default();
            let Ok(c) = vm.loader.get_or_load(&cls) else { return Ok(Some(Some(Value::Null))) };
            if c.file.super_class.as_deref() != Some("java/lang/Record") {
                return Ok(Some(Some(Value::Null)));
            }
            let comps: Vec<(String, String)> = c
                .file
                .fields
                .iter()
                .filter(|f| !f.name.starts_with('$'))
                .map(|f| (f.name.clone(), f.descriptor.clone()))
                .collect();
            let objs: Vec<Value> = comps
                .into_iter()
                .map(|(n, d)| {
                    let h = vm.heap.alloc("java/lang/reflect/RecordComponent");
                    vm.heap.put_field_syn(h, crate::heap::Syn::Name, Value::Str(n.into()));
                    vm.heap.put_field(h, "$desc", Value::Str(d.into()));
                    vm.heap.put_field(h, "$owner", Value::Str(cls.clone().into()));
                    Value::Ref(h)
                })
                .collect();
            Value::Array(vm.heap.alloc_values("Ljava/lang/Object;", objs))
        }
        "getMethods" | "getDeclaredMethods" => {
            let cls = field_str(vm, args.first(), "$name").unwrap_or_default();
            // (name, inner-arg-descriptor) for each declared method, excluding
            // <init>/<clinit> (those are Constructors, not Methods).
            let methods: Vec<(String, String)> = match vm.loader.get_or_load(&cls) {
                Ok(c) => c.file.methods.iter()
                    .filter(|m| m.name != "<init>" && m.name != "<clinit>")
                    .map(|m| {
                        let inner = m.descriptor.strip_prefix('(')
                            .and_then(|d| d.split(')').next())
                            .unwrap_or("").to_string();
                        (m.name.clone(), inner)
                    }).collect(),
                Err(_) => Vec::new(),
            };
            let objs: Vec<Value> = methods.into_iter().map(|(n, d)| make_method(vm, &cls, &n, &d)).collect();
            Value::Array(vm.heap.alloc_values("Ljava/lang/reflect/Method;", objs))
        }
        "getField" | "getDeclaredField" => {
            let cls = field_str(vm, args.first(), "$name").unwrap_or_default();
            let fname = match args.get(1) { Some(Value::Str(s)) => s.to_string(), _ => return Ok(Some(Some(Value::Null))) };
            let fdesc = match vm.loader.get_or_load(&cls) {
                Ok(c) => c.file.fields.iter().find(|f| f.name == fname).map(|f| f.descriptor.clone()),
                Err(_) => None,
            };
            match fdesc {
                Some(d) => make_field(vm, &cls, &fname, &d),
                None => return Err(VmError::Throw(vm.make_exception("java/lang/NoSuchFieldException", &fname))),
            }
        }
        "getResource" => {
            let res = match args.get(1) { Some(Value::Str(s)) => s.to_string(), _ => return Ok(Some(Some(Value::Null))) };
            let path = res.strip_prefix('/').unwrap_or(&res).to_string();
            if resource_exists(vm, &path) { make_resource_url(vm, &path) } else { Value::Null }
        }
        // Class.cast is the REFLECTIVE checkcast and throws the same way: it was
        // an identity pass-through, so `String.class.cast(anInteger)` handed back
        // the Integer and the caller went on using it as a String. Same three-
        // valued test the bytecode cast uses (`Tri::Unknown` passes), and the
        // same null rule — cast(null) is null, never a throw.
        "cast" => {
            let v = args.get(1).cloned().unwrap_or(Value::Null);
            if !matches!(v, Value::Null) {
                let sup = field_str(vm, args.first(), "$name").unwrap_or_default();
                if !sup.is_empty()
                    && crate::opcodes::instance_of_tri(vm, &v, &sup) == crate::opcodes::Tri::No
                {
                    let msg = crate::opcodes::cast_message(vm, &v, &sup);
                    return Err(VmError::Throw(
                        vm.make_exception("java/lang/ClassCastException", &msg),
                    ));
                }
            }
            v
        }
        // A.isAssignableFrom(B): is B a subtype of A? Decided by a real
        // class/interface walk (netty's ChannelHandlerMask relies on this to tell
        // inbound from outbound handlers — a blanket `true` made every handler
        // look outbound and mis-routed bind()).
        // Checked cast of a Class object: the mirror itself, unchanged (the
        // cast's static type is erased here; callers rely on identity).
        "asSubclass" => args.first().cloned().unwrap_or(Value::Null),
        // Classfile access_flags use the same bit values as java.lang.reflect
        // Modifier (PUBLIC=1, FINAL=0x10, INTERFACE=0x200, ABSTRACT=0x400...),
        // so the real flags pass through; bytecode-less JDK mirrors read PUBLIC.
        "getModifiers" => {
            let cls = field_str(vm, args.first(), "$name").unwrap_or_default();
            let flags = vm.loader.get_or_load(&cls).map(|c| c.file.access_flags as i32).unwrap_or(1);
            Value::Int(flags)
        }
        // Generic superclass: when the classfile carries a Signature attribute
        // with a parameterized super (e.g. an anonymous `new TypeToken<X>(){}`
        // subclass), synthesize a real ParameterizedType — gson's TypeToken
        // hard-requires it in its constructor (so Gson.<clinit> itself fails
        // without this). Otherwise the raw superclass mirror.
        // The formal type parameters declared by the class (e.g. the "I" in
        // MessageToByteEncoder<I>). netty's TypeParameterMatcher.find reads these
        // by name to resolve a handler's message type. Parsed from the class's
        // generic Signature; each TypeVariable carries its name in $name.
        "getTypeParameters" => {
            let cls = field_str(vm, args.first(), "$name").unwrap_or_default();
            // From the class's own generic Signature, else the JDK-collection table
            // (java.util types have no classfile here). Each TypeVariable carries its
            // declaring class so gson's resolveTypeVariable identity matches.
            let names: Vec<String> = vm.loader.get_or_load(&cls).ok()
                .and_then(|c| c.file.signature.clone())
                .map(|s| formal_type_param_names(&s))
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| jdk_generics(&cls).map(|(tp, _)| tp.iter().map(|s| s.to_string()).collect()).unwrap_or_default());
            let vars: Vec<Value> = names.iter().map(|n| make_type_variable(vm, n, &cls)).collect();
            Value::Array(vm.heap.alloc_values("Ljava/lang/reflect/TypeVariable;", vars))
        }
        "getGenericSuperclass" => {
            let cls = field_str(vm, args.first(), "$name").unwrap_or_default();
            let loaded = vm.loader.get_or_load(&cls).ok();
            if let Some(sig) = loaded.as_ref().and_then(|c| c.file.signature.clone()) {
                if let Some(t) = generic_super_from_signature(vm, &sig) {
                    return Ok(Some(Some(t)));
                }
            }
            match loaded.and_then(|c| c.file.super_class.clone()) {
                Some(s) => vm.make_class(&s),
                None => Value::Null,
            }
        }
        // Nested-class predicates by name shape. The three are MUTUALLY EXCLUSIVE and
        // javac's naming distinguishes all three: `Outer$1` anonymous (tail all
        // digits), `Outer$1Local` local (digits then an identifier — the digits
        // disambiguate same-named locals in different methods), `Outer$Inner` member
        // (tail starts with an identifier character). Previously `isLocalClass` was a
        // hardcoded 0 and `isMemberClass` claimed every non-anonymous tail, so a local
        // class answered "member" and never "local" — both wrong at once.
        "isAnonymousClass" => Value::Int(matches!(nest_kind(&field_str(vm, args.first(), "$name").unwrap_or_default()), Nest::Anonymous) as i32),
        "isLocalClass" => Value::Int(matches!(nest_kind(&field_str(vm, args.first(), "$name").unwrap_or_default()), Nest::Local) as i32),
        "isMemberClass" => Value::Int(matches!(nest_kind(&field_str(vm, args.first(), "$name").unwrap_or_default()), Nest::Member) as i32),
        "isSynthetic" => Value::Int(0),
        // Enclosing class by name: Outer$Inner / Outer$1 -> Outer.
        "getEnclosingClass" | "getDeclaringClass" => {
            let cls = field_str(vm, args.first(), "$name").unwrap_or_default();
            match cls.rsplit_once('$') {
                Some((outer, _)) if !outer.is_empty() => vm.make_class(outer),
                _ => Value::Null,
            }
        }
        "isAssignableFrom" => {
            let sup = field_str(vm, args.first(), "$name").unwrap_or_default();
            let sub = field_str(vm, args.get(1), "$name").unwrap_or_default();
            if sub.is_empty() || sub == sup { Value::Int(1) }
            else { Value::Int(crate::opcodes::name_is_subtype_strict(vm, &sub, &sup) as i32) }
        }
        "isInstance" => {
            // Delegate to the instanceof core so array targets, String, boxed
            // primitives and null are all judged consistently (the old lenient
            // `_ => 1` matched a String/array against any Class, and array targets
            // matched non-arrays — which made snakeyaml treat objects as arrays).
            let sup = field_str(vm, args.first(), "$name").unwrap_or_default();
            let v = args.get(1).cloned().unwrap_or(Value::Null);
            Value::Int(crate::opcodes::instance_of(vm, &v, &sup) as i32)
        }
        "desiredAssertionStatus" => Value::Int(0),
        "isArray" => Value::Int(field_str(vm, args.first(), "$name").map(|n| n.starts_with('[')).unwrap_or(false) as i32),
        "isPrimitive" => Value::Int(matches!(field_str(vm, args.first(), "$name").as_deref(),
            Some("int" | "long" | "boolean" | "char" | "byte" | "short" | "float" | "double" | "void")) as i32),
        "isInterface" => {
            let cls = field_str(vm, args.first(), "$name").unwrap_or_default();
            let r = match vm.loader.get_or_load(&cls) {
                Ok(c) => c.file.access_flags & 0x0200 != 0,
                // JDK boundary (no bytecode): consult a known-interface set. gson's
                // $Gson$Types.getGenericSupertype walks List/Collection generics and
                // NPE'd when isInterface(List) wrongly returned false (it then took
                // the class-superclass branch into a null).
                Err(_) => is_jdk_interface(&cls),
            };
            Value::Int(r as i32)
        }
        "isEnum" => {
            let cls = field_str(vm, args.first(), "$name").unwrap_or_default();
            // ACC_ENUM alone is not the test. javac also marks the ANONYMOUS class it
            // generates for a constant-specific body (`PLUS { int apply(..){..} }` →
            // Op$1) — so `PLUS.getClass().isEnum()` answered true where HotSpot
            // answers false. `Class.isEnum` is `ACC_ENUM && getSuperclass() == Enum`,
            // and the body class's superclass is the enum type, not Enum.
            let r = vm.loader.get_or_load(&cls)
                .map(|c| c.file.access_flags & 0x4000 != 0
                    && c.file.super_class.as_deref() == Some("java/lang/Enum"))
                .unwrap_or(false);
            Value::Int(r as i32)
        }
        // A record is exactly a class whose DIRECT superclass is java.lang.Record
        // (JVMS 4.1: only a record class may name it), which is decidable from the
        // classfile we already parse — the Record attribute itself is not retained
        // here. Without this arm `Class.isRecord()` was an UnsatisfiedLinkError, so
        // any library that branches on it (gson/jackson pick a record adapter) died
        // instead of taking a branch.
        "isRecord" => {
            let cls = field_str(vm, args.first(), "$name").unwrap_or_default();
            let r = vm.loader.get_or_load(&cls)
                .map(|c| c.file.super_class.as_deref() == Some("java/lang/Record"))
                .unwrap_or(false);
            Value::Int(r as i32)
        }
        "getInterfaces" => {
            let cls = field_str(vm, args.first(), "$name").unwrap_or_default();
            // Direct interfaces from the classfile, else the JDK-collection table
            // (java.util types have no classfile → would be empty, breaking gson's
            // List→Collection supertype walk).
            let ifaces: Vec<String> = match vm.loader.get_or_load(&cls) {
                Ok(c) => c.file.interfaces.clone(),
                Err(_) => jdk_generics(&cls).map(|(_, sup)| sup.iter().map(|(n, _)| n.to_string()).collect()).unwrap_or_default(),
            };
            let objs: Vec<Value> = ifaces.iter().map(|i| vm.make_class(i)).collect();
            Value::Array(vm.heap.alloc_values("Ljava/lang/Object;", objs))
        }
        // Generic interfaces: the PARAMETERIZED direct super-interfaces. For a jar
        // class, parse them from the Signature; for a JDK collection, synthesize from
        // the table (Collection<E> etc., E threaded from this class's type vars). This
        // is the piece gson's $Gson$Types.getSupertype needs to resolve a collection's
        // element type — without it the element defaults to Object → LinkedTreeMap.
        "getGenericInterfaces" => {
            let cls = field_str(vm, args.first(), "$name").unwrap_or_default();
            let mut out: Vec<Value> = Vec::new();
            match vm.loader.get_or_load(&cls).ok().and_then(|c| c.file.signature.clone()) {
                Some(sig) => {
                    for t in generic_ifaces_from_signature(vm, &sig) {
                        out.push(t);
                    }
                }
                None => {
                    if let Some((_, sup)) = jdk_generics(&cls) {
                        for (raw, arg_names) in sup {
                            if arg_names.is_empty() {
                                out.push(vm.make_class(raw));
                            } else {
                                let args: Vec<Value> = arg_names.iter().map(|n| make_type_variable(vm, n, &cls)).collect();
                                out.push(make_parameterized_type(vm, raw, args));
                            }
                        }
                    } else {
                        // Non-collection JDK / unknown: raw interfaces (best effort).
                        let raw: Vec<String> = vm.loader.get_or_load(&cls).map(|c| c.file.interfaces.clone()).unwrap_or_default();
                        for i in raw {
                            out.push(vm.make_class(&i));
                        }
                    }
                }
            }
            Value::Array(vm.heap.alloc_values("Ljava/lang/reflect/Type;", out))
        }
        "getSuperclass" => {
            let cls = field_str(vm, args.first(), "$name").unwrap_or_default();
            match vm.loader.get_or_load(&cls).ok().and_then(|c| c.file.super_class.clone()) {
                Some(s) => vm.make_class(&s),
                // JDK class with no bytecode: we don't know its exact super, but
                // it ultimately extends Object. Returning Object (not null) lets
                // reflective type walks (gson's getGenericSupertype) terminate at
                // Object instead of recursing into a null Class and NPEing on
                // getInterfaces. Object/interfaces/primitives/arrays have none.
                None => {
                    // An ARRAY class's superclass IS java.lang.Object (JLS 10.8) —
                    // it was grouped with Object/interfaces/primitives, so
                    // `int[].class.getSuperclass()` answered null where HotSpot
                    // answers Object, and a reflective walk up an array's hierarchy
                    // NPE'd on the first step instead of terminating.
                    let none = cls.is_empty() || cls == "java/lang/Object"
                        || is_jdk_interface(&cls)
                        || matches!(cls.as_str(), "int" | "long" | "boolean" | "char" | "byte" | "short" | "float" | "double" | "void");
                    if none { Value::Null } else { vm.make_class("java/lang/Object") }
                }
            }
        }
        "getEnumConstants" => {
            let cls = field_str(vm, args.first(), "$name").unwrap_or_default();
            // Build the constants from the enum's own ACC_ENUM static fields rather
            // than calling values() — values() does `$VALUES.clone()`, and $VALUES
            // is assigned LAST in <clinit> (after each constant's ctor). A constant
            // ctor that re-enters reflection (Paper's VersionHistoryManager ctor
            // runs gson, which builds an EnumTypeAdapter and asks for the constants)
            // would hit a still-null $VALUES → NPE on clone. Reading the static
            // fields tolerates that partial-init window (returns what exists so far).
            // NOT an enum type ⇒ null, not an empty array: `getEnumConstants` is
            // specified to return null for a non-enum, and callers test for null
            // (an empty array reads as "an enum with no constants"). The
            // constant-body class Op$1 carries ACC_ENUM but is not the enum type,
            // so it landed here and answered `[]`.
            let is_enum_type = vm.loader.get_or_load(&cls)
                .map(|c| c.file.access_flags & 0x4000 != 0
                    && c.file.super_class.as_deref() == Some("java/lang/Enum"))
                .unwrap_or(false);
            if !is_enum_type {
                return Ok(Some(Some(Value::Null)));
            }
            if let Ok(c) = vm.loader.get_or_load(&cls) {
                vm.ensure_init(&c)?;
                let consts: Vec<Value> = c.file.fields.iter()
                    .filter(|f| f.access_flags & 0x4000 != 0) // ACC_ENUM
                    .filter_map(|f| c.statics.read().unwrap_or_else(|e| e.into_inner()).get(f.name.as_str()).cloned())
                    .filter(|v| !matches!(v, Value::Null))
                    .collect();
                Value::Array(vm.heap.alloc_values(&format!("L{cls};"), consts))
            } else {
                Value::Null
            }
        }
        "getPackageName" => {
            let n = field_str(vm, args.first(), "$name").unwrap_or_default().replace('/', ".");
            Value::Str(n.rsplit_once('.').map(|(p, _)| p.to_string()).unwrap_or_default().into())
        }
        "getPackage" => {
            let n = field_str(vm, args.first(), "$name").unwrap_or_default();
            let pkg = n.rsplit_once('/').map(|(p, _)| p.to_string()).unwrap_or_default();
            let h = vm.heap.alloc("java/lang/Package");
            vm.heap.put_field_syn(h, crate::heap::Syn::Name, Value::Str(pkg.replace('/', ".").into()));
            Value::Ref(h)
        }
        "getComponentType" => {
            let n = field_str(vm, args.first(), "$name").unwrap_or_default();
            if let Some(elem) = n.strip_prefix('[') { vm.make_class(&desc_token_to_class(elem)) } else { Value::Null }
        }
        "getAnnotation" | "getDeclaredAnnotation" => Value::Null,
        // Check the class's RuntimeVisibleAnnotations for the queried annotation
        // type. netty's @Sharable check (isAnnotationPresent) gates whether a
        // ChannelHandler can be reused across pipelines — returning false for an
        // actually-@Sharable handler (ChannelInitializer) made every connection
        // after the first throw "not a @Sharable handler".
        "isAnnotationPresent" => {
            let cls = field_str(vm, args.first(), "$name").unwrap_or_default();
            let want = field_str(vm, args.get(1), "$name").unwrap_or_default();
            Value::Int(class_has_annotation(vm, &cls, &want) as i32)
        }
        "getAnnotations" | "getDeclaredAnnotations" | "getAnnotationsByType" => {
            Value::Array(vm.heap.alloc_values("Ljava/lang/annotation/Annotation;", Vec::new()))
        }
        "getClassLoader" => Value::Ref(vm.heap.alloc("java/lang/ClassLoader")),
        "getSigners" => Value::Null,
        "getProtectionDomain" => Value::Null,
        "getDeclaringClass" | "getEnclosingClass" | "getNestHost" => {
            // Nested-class metadata isn't modelled; report top-level (null).
            let n = field_str(vm, args.first(), "$name").unwrap_or_default();
            if name == "getNestHost" { vm.make_class(&n) } else { Value::Null }
        }
        _ => return Ok(None),
    };
    Ok(Some(Some(ret)))
}

pub(crate) fn loader_method(vm: &mut Vm, owner: &str, name: &str, args: &[Value]) -> Handled {
    let ret = match (owner, name) {
        ("java/net/URLClassLoader", "<init>") => {
            // (URL[] urls[, parent]) — register each URL on the classpath.
            for url in crate::natives::varargs(vm, args.get(1)) {
                if let Value::Ref(h) = url {
                    let path = match vm.heap.get_field_syn(h, crate::heap::Syn::Path) {
                        Value::Str(s) => s.to_string(),
                        _ => url_to_path(&strfield(vm, h, "$str")),
                    };
                    vm.loader.add_classpath(&path);
                }
            }
            return Ok(Some(None));
        }
        (_, "loadClass") => {
            let cn = match args.get(1) { Some(Value::Str(s)) => binary(s), _ => return Ok(Some(Some(Value::Null))) };
            match vm.loader.get_or_load(&cn) {
                Ok(c) => { vm.ensure_init(&c)?; vm.make_class(&cn) }
                Err(_) if cn.starts_with("java/") || cn.starts_with("sun/") || cn.starts_with("jdk/") => vm.make_class(&cn),
                Err(_) => return Err(VmError::Throw(vm.make_exception("java/lang/ClassNotFoundException", &cn.replace('/', ".")))),
            }
        }
        ("java/lang/ClassLoader", "getSystemClassLoader") => {
            Value::Ref(vm.heap.alloc("java/lang/ClassLoader"))
        }
        ("java/lang/ClassLoader", "getParent") => Value::Null, // bootstrap parent
        (_, "getResources") | (_, "getSystemResources") | (_, "findResources") => {
            let res = match args.get(1) { Some(Value::Str(s)) => s.to_string(), _ => String::new() };
            let path = res.strip_prefix('/').unwrap_or(&res).to_string();
            let mut urls = Vec::new();
            if resource_exists(vm, &path) {
                urls.push(make_resource_url(vm, &path));
            }
            crate::stream::iterator_of(vm, urls) // serves as the Enumeration
        }
        (_, "getResource") | (_, "findResource") => {
            let res = match args.get(1) { Some(Value::Str(s)) => s.to_string(), _ => return Ok(Some(Some(Value::Null))) };
            let path = res.strip_prefix('/').unwrap_or(&res).to_string();
            if resource_exists(vm, &path) { make_resource_url(vm, &path) } else { Value::Null }
        }
        (_, "getResourceAsStream") => {
            let res = match args.get(1) { Some(Value::Str(s)) => s.to_string(), _ => return Ok(Some(Some(Value::Null))) };
            let path = res.strip_prefix('/').unwrap_or(&res).to_string();
            match vm.loader.read_resource(&path) {
                Some(bytes) => crate::natives::make_stream(vm, &bytes),
                None => Value::Null,
            }
        }
        // URL.openConnection[(Proxy)] — a lazy connection object. We don't do
        // real networking, so reads fail like an unreachable host; callers that
        // fetch over HTTP at boot (e.g. the services public-key check) treat
        // that as "offline" and continue. Carry the URL string for diagnostics.
        ("java/net/URL", "openConnection") => {
            let h = vm.heap.alloc("java/net/HttpURLConnection");
            vm.heap.put_field_syn(h, crate::heap::Syn::Str, Value::Str(strfield_or(vm, args.first(), "$str").into()));
            Value::Ref(h)
        }
        // URI/URL conversions: both carry $path + $str, so just retype.
        ("java/net/URI", "toURL") => retype(vm, args.first(), "java/net/URL"),
        ("java/net/URL", "toURI") => retype(vm, args.first(), "java/net/URI"),
        ("java/net/URI", "create") => {
            let s = match args.first() { Some(Value::Str(s)) => s.to_string(), _ => String::new() };
            let p = url_to_path(&s);
            crate::nio::make_uri(vm, "java/net/URI", &p)
        }
        ("java/net/URI" | "java/net/URL", "toString") => Value::Str(strfield_or(vm, args.first(), "$str").into()),
        ("java/net/URI" | "java/net/URL", "getPath" | "getFile") => {
            Value::Str(strfield_or(vm, args.first(), "$path").into())
        }
        ("java/net/URI" | "java/net/URL", "toExternalForm" | "toASCIIString") => {
            Value::Str(strfield_or(vm, args.first(), "$str").into())
        }
        ("java/net/URI" | "java/net/URL", "getProtocol" | "getScheme") => {
            let s = strfield_or(vm, args.first(), "$str");
            Value::Str(s.split(':').next().unwrap_or("").to_string().into())
        }
        ("java/net/URL", "getHost") | ("java/net/URI", "getHost") => Value::Str("".into()),
        ("java/net/URL", "getPort") => Value::Int(-1),
        ("java/net/URL", "openStream") => {
            // A classpath resource ($res) reads through the loader; a file URL
            // ($path) reads from disk.
            let res = strfield_or(vm, args.first(), "$res");
            if !res.is_empty() {
                match vm.loader.read_resource(&res) {
                    Some(bytes) => crate::natives::make_stream(vm, &bytes),
                    None => Value::Null,
                }
            } else {
                let path = strfield_or(vm, args.first(), "$path");
                match std::fs::read(&path) {
                    Ok(bytes) => crate::natives::make_stream(vm, &bytes),
                    Err(_) => Value::Null,
                }
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(Some(ret)))
}
