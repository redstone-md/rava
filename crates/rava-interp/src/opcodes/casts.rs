//! The CHECKED half of the type system: `checkcast`, the `aastore` store check
//! and `Class.cast`.
//!
//! All three ask one question — "is this value assignable to that type?" — and
//! all three must THROW on a no. That is why they cannot use `instance_of`:
//! its `bool` is lenient, so its `false` means "provably not a subtype OR the
//! walk never reached the JDK boundary", and throwing on the second reading
//! would invent `ClassCastException`s that HotSpot does not raise. Everything
//! here routes through [`instance_of_tri`], which keeps the two apart, and
//! throws only on [`Tri::No`].
//!
//! Admission is by DESCRIPTOR and the loaded hierarchy only — the constant-pool
//! target name, the array's own element descriptor, and the class files on the
//! classpath. No workload identity enters any decision.
//!
//! ⭐**THE MEMOS IN THIS FILE ARE ONE BLOCK, KILLED BY `RAVA_NO_FAST_CASTS=1`**
//! (`classkey::fast_casts_enabled`, which also honours `RAVA_NO_EMIT_MORE`). With
//! the switch thrown every path here is byte-for-byte what it was: the pair memo
//! is bypassed, the cast key narrows back to `instanceof_class_key`, and the
//! `aastore` check re-derives its verdict per store. None of them CHANGES an
//! answer — each one only stops re-deriving an answer that is a constant of the
//! (source class, target class) pair, which is what the immutability of the
//! loaded hierarchy buys.

use crate::opcodes::*;

/// THREE-VALUED subtype answer.
///
/// Every subtype test in this engine is decided by WALKING NAMES: a class file
/// declares its super and its interfaces by name, so a hierarchy made entirely
/// of loadable class files is fully enumerable. The JDK boundary is not — a
/// native-served type has no class file, so the walk stops there and the honest
/// answer is neither yes nor no. `instanceof` collapses that third answer into
/// `true` (leniency keeps execution moving), which is exactly why its `bool`
/// cannot drive a THROW: `false` from a lenient test conflates "provably not a
/// subtype" with "the walk never got far enough". `Tri` keeps them apart so
/// `checkcast`/`aastore`/`Class.cast` can throw on `No` and pass on `Unknown`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum Tri {
    Yes,
    No,
    Unknown,
}

/// JDK interfaces whose supertype set is exactly `{Object}` — nothing above them
/// to enumerate, so a walk that reaches one has NOT lost information.
///
/// Deliberately NOT rows of `JDK_SUPERS`: that table is read by `instance_of`,
/// `name_is_subtype` and `name_may_subtype`, and adding rows there would make
/// their `inconclusive` fallbacks fire less often — a behaviour change to every
/// `instanceof` in the boot. This set is consumed ONLY by the three-valued walk
/// below, so the new knowledge reaches the throwing paths and nothing else.
const JDK_TERMINAL: &[&str] = &[
    "java/lang/Comparable",
    "java/lang/CharSequence",
    "java/lang/Cloneable",
    "java/lang/Runnable",
    "java/lang/AutoCloseable",
    "java/io/Serializable",
    // JDK 12+: the interfaces every String and boxed primitive also implements.
    // Without them a cast of a String to one of them could not be judged at all.
    "java/lang/constant/Constable",
    "java/lang/constant/ConstantDesc",
];

/// Can this engine place `target` in a hierarchy at all? Either it has bytecode
/// for it (every super/interface is then named in the file), or it is a row of
/// `JDK_SUPERS`, or it is a terminal JDK interface. A target that is NONE of
/// those is a type we cannot even begin to relate to anything, so a miss against
/// it is always `Unknown` — never a throw.
fn target_placeable(vm: &Vm, target: &str) -> bool {
    jdk_supers(target).is_some()
        || JDK_TERMINAL.contains(&target)
        || vm.loader.get_or_load(target).is_ok()
}

/// THE SAME walk as [`name_is_subtype`], answered three-valued.
///
/// The one difference is the FALLBACK, and it is the whole point of this
/// function. `name_is_subtype`'s bool asks a question about `sup` ("is the
/// supertype something I can place?"), which is the wrong side for a THROW: what
/// decides whether a miss is real is whether `sub`'s supertype closure was
/// enumerated COMPLETELY. If the walk stopped at any type with no class file
/// that neither `JDK_SUPERS` nor `JDK_TERMINAL` names, the receiver may well
/// have a supertype we never saw, and `No` would be a fabrication — e.g.
/// `java/time/DayOfWeek` has no class file and no table row, so a walk from it
/// reaches nothing and would otherwise "prove" it is not a `java/lang/Enum`.
///
/// `Tri::No` therefore implies `!inconclusive`, which implies the old bool is
/// `false` too: this answer is strictly more conservative than `instanceof`'s,
/// never the other way round. `name_is_subtype` keeps its own body so that no
/// `instanceof` answer in the boot moves.
fn name_is_subtype_tri(vm: &Vm, sub: &str, sup: &str) -> Tri {
    if sup == "java/lang/Object" || sub == sup { return Tri::Yes; }
    if !fast_casts_enabled() {
        return name_is_subtype_tri_walk(vm, sub, sup);
    }
    if let Some(hit) = SUBTYPE_TRI.with(|m| {
        let m = m.borrow();
        m.get(sub).and_then(|inner| inner.get(sup)).copied()
    }) {
        return hit;
    }
    // The walk must run with NO borrow held: `elem_assignable` re-enters this
    // function for nested array elements.
    let answer = name_is_subtype_tri_walk(vm, sub, sup);
    SUBTYPE_TRI.with(|m| {
        let mut m = m.borrow_mut();
        // `get_mut` first so a repeat `sub` does not allocate a key just to
        // discover the entry already exists.
        if let Some(inner) = m.get_mut(sub) {
            inner.insert(Box::from(sup), answer);
        } else {
            let mut inner = crate::FastMap::default();
            inner.insert(Box::from(sup), answer);
            m.insert(Box::from(sub), inner);
        }
    });
    answer
}

thread_local! {
    /// `sub` -> (`sup` -> answer): the three-valued subtype verdict for one NAME
    /// PAIR, which is what the walk below actually computes.
    ///
    /// COMPLEXITY: the walk WAS O(supertype closure of `sub`) per ASK, and it
    /// allocated per node — a `String` for the start, a `String` clone for every
    /// class and interface visited, a `Vec` and a SipHash `HashSet` per call —
    /// re-run for every `checkcast`/`aastore`/`Class.cast` that missed the site
    /// memo, including every lambda receiver (whose `$fi` answer is per-OBJECT
    /// and so can never be site-keyed). It IS two hash probes. What grows is the
    /// number of DISTINCT (source class, target class) pairs the boot asks —
    /// thousands — not the number of asks, which is millions.
    ///
    /// SOUNDNESS: the answer is a pure function of the pair, because the loaded
    /// hierarchy is immutable (a class file's declared super and interfaces never
    /// change) and loadability is a property of the CLASSPATH, not of load order —
    /// the same argument `IOF_MEMO` and `CCAST_MEMO` already ship on. The one
    /// answer that could in principle move is `Unknown`, which a later-defined
    /// class could sharpen into `Yes`/`No`; `Unknown` is the LENIENT verdict (no
    /// throw), so a stale one only ever declines to throw, exactly as the
    /// un-memoised engine did before that class existed. `No` cannot move at all:
    /// it requires the whole closure to have been enumerated, and defining a new
    /// class adds no supertype to an existing one.
    ///
    /// SIZE: this is a MISS-path structure, not a per-opcode table — it is
    /// consulted only when the site memo has nothing, so it is not competing for
    /// L1 with the interpreter loop. Roughly 100 B per distinct pair.
    static SUBTYPE_TRI: std::cell::RefCell<crate::FastMap<Box<str>, crate::FastMap<Box<str>, Tri>>> =
        std::cell::RefCell::new(crate::FastMap::default());
}

/// The walk itself. Cold once [`name_is_subtype_tri`]'s pair memo is warm.
fn name_is_subtype_tri_walk(vm: &Vm, sub: &str, sup: &str) -> Tri {
    // Engine-internal name set: FxHash, not the standard library's SipHash, and
    // the visited name is MOVED in after the load rather than cloned before it —
    // one `String` per node was being allocated purely to key this set.
    let mut seen: crate::FastSet<String> = crate::FastSet::default();
    let mut queue = vec![sub.to_string()];
    let mut inconclusive = false;
    // Did the walk pass through a class whose supertypes came from `JDK_SUPERS`
    // rather than from a class file? See the interface rule at the bottom.
    let mut via_table = false;
    while let Some(cn) = queue.pop() {
        if cn == sup { return Tri::Yes; }
        if seen.contains(&cn) { continue; }
        match vm.loader.get_or_load(&cn) {
            Ok(c) => {
                if c.file.interfaces.iter().any(|i| i == sup) { return Tri::Yes; }
                queue.extend(c.file.interfaces.iter().cloned());
                if let Some(s) = c.file.super_class.clone() { queue.push(s); }
            }
            // No bytecode (native-backed JDK type): continue through the known
            // JDK hierarchy. A terminal interface adds nothing to enumerate, so
            // it is NOT a loss of information; anything else is.
            Err(_) => match jdk_supers(&cn) {
                Some(supers) => {
                    via_table = true;
                    queue.extend(supers.iter().map(|s| s.to_string()));
                }
                None => if !JDK_TERMINAL.contains(&cn.as_str()) { inconclusive = true; },
            },
        }
        seen.insert(cn);
    }
    if inconclusive {
        return Tri::Unknown;
    }
    // ⚠ `JDK_SUPERS` IS AN APPROXIMATION, AND A THROW MAY NOT TREAT IT AS A
    // COMPLETE ENUMERATION. Its rows list the supertypes this engine happened to
    // need; a real JDK class implements more interfaces than any hand-written
    // table records. When the walk went through such a row and the target is an
    // INTERFACE, "not found" therefore means "not in the table", which is not the
    // same as "not a supertype" — and answering `No` would throw
    // ClassCastException on a cast HotSpot performs. A CLASS target is safe: the
    // superclass CHAIN in those rows is what they get right, and a missing link
    // there would already have shown up as an unresolvable name.
    if via_table && !target_is_class(vm, sup) {
        return Tri::Unknown;
    }
    Tri::No
}

/// Is `sup` known to be a CLASS (not an interface)? Only a loadable class file
/// answers this; anything else is treated as possibly-an-interface, which is the
/// conservative side for [`name_is_subtype_tri`]'s table rule.
fn target_is_class(vm: &Vm, sup: &str) -> bool {
    /// `ACC_INTERFACE`, JVMS 4.1.
    const ACC_INTERFACE: u16 = 0x0200;
    match vm.loader.get_or_load(sup) {
        Ok(c) => c.file.access_flags & ACC_INTERFACE == 0,
        Err(_) => false,
    }
}

/// Supertypes of EVERY array type, per JVMS 4.10.1.2: an array is an Object and
/// implements exactly Cloneable and java.io.Serializable. The list is closed, so
/// a target outside it that we can place is an authoritative `No`.
const ARRAY_SUPERS: [&str; 3] =
    ["java/lang/Object", "java/lang/Cloneable", "java/io/Serializable"];

/// Supertypes of `java.lang.String`, which is final — the set is closed, so any
/// PLACEABLE target outside it is an authoritative `No`. The `instance_of`
/// `Value::Str` arm answers `true` for the first five; Constable/ConstantDesc
/// are here because they are real String supertypes (JDK 12+) and a cast to one
/// must not be rejected just because the lenient arm never needed to know.
const STRING_SUPERS: [&str; 7] = [
    "java/lang/String",
    "java/lang/Object",
    "java/lang/CharSequence",
    "java/lang/Comparable",
    "java/io/Serializable",
    "java/lang/constant/Constable",
    "java/lang/constant/ConstantDesc",
];

/// May a value this engine holds UNBOXED be assignable to `target`?
///
/// REPRESENTATION GAP, and the reason this is one predicate rather than one per
/// wrapper: rava keeps boxed primitives unboxed, and `Value::Int` is the
/// representation of a boxed `Integer`, `Short`, `Byte`, `Character` AND
/// `Boolean` alike — the box's own type is not retained anywhere. So a cast of
/// an unboxed value to ANY wrapper has to pass: `(Character) c` reaches here as
/// a `Value::Int` and rejecting it would throw on correct guest code. (That is
/// exactly how the ChunkFill gate caught the first version of this predicate.)
///
/// What it still decides confidently is everything OUTSIDE the wrapper world —
/// `(String) anInteger`, `(CharSequence) anInteger`, `(SomeGuestType) aLong` —
/// which is where the divergences actually were. Making these exact needs boxed
/// -primitive type retention in the value model; see the report.
fn boxed_supers(target: &str) -> bool {
    matches!(target,
        "java/lang/Integer" | "java/lang/Long" | "java/lang/Short" | "java/lang/Byte"
        | "java/lang/Character" | "java/lang/Boolean" | "java/lang/Float" | "java/lang/Double"
        | "java/lang/Number" | "java/lang/Comparable" | "java/io/Serializable" | "java/lang/Object"
        | "java/lang/constant/Constable" | "java/lang/constant/ConstantDesc")
}

/// Subtype test from a value's RUNTIME class, with the one guard a throwing
/// consumer needs on top of the walk.
///
/// A class with NO class file is one this engine serves from its own native
/// layer, and that layer routinely hands back a STAND-IN whose class is not the
/// class the guest's bytecode was compiled against: `ImmutableMap.copyOf` is
/// intercepted in `map/dispatch.rs:237` and returns an object whose class is
/// literally `java/util/LinkedHashMap`, so the guest's `checkcast
/// com/google/common/collect/ImmutableMap` is a real miss — against an object
/// WE substituted. Throwing there would blame the guest for our substitution.
/// So a `No` from a bytecodeless receiver class is downgraded to `Unknown`.
///
/// This is the converse of the rule `instance_of` already states for the same
/// reason ("if the object's OWN class is loadable, a miss is authoritative");
/// only the loadable half can support a throw.
///
/// It does NOT weaken the value-kind arms (`Str`, the boxed primitives, arrays):
/// those are REPRESENTATIONS of final types, never stand-ins for another class.
fn runtime_class_tri(vm: &Vm, class: &str, target: &str) -> Tri {
    match name_is_subtype_tri(vm, class, target) {
        Tri::No if vm.loader.get_or_load(class).is_err() => Tri::Unknown,
        other => other,
    }
}

/// `Tri::No` unless `target` is a type we cannot place at all, in which case the
/// honest answer is `Unknown`. Used by the FINAL-type arms above, which decide
/// by set membership instead of by walking, so they must apply the same
/// "can we place the target?" gate the walk applies in its fallback.
fn closed_set_miss(vm: &Vm, target: &str) -> Tri {
    if target_placeable(vm, target) { Tri::No } else { Tri::Unknown }
}

/// Canonical form of one array ELEMENT type. Two spellings reach here and they
/// disagree: an array's stored element name uses primitive WORDS (`"int"`,
/// written by `alloc_array`/`primitive_array_type`) while a constant-pool array
/// descriptor uses LETTERS (`"I"`). Normalising both into this enum is what lets
/// `(int[]) new long[1]` be decided at all.
#[derive(PartialEq, Eq, Debug)]
enum Elem<'a> {
    Prim(&'a str),
    Class(&'a str),
    /// A nested array; the payload is the inner element descriptor (`"[Ljava/…"`
    /// minus its leading `[`).
    Array(&'a str),
}

/// Parse an element type written EITHER as a field descriptor (`I`, `Ljava/lang/
/// String;`, `[I`) or as this heap's primitive word (`int`). `None` = a spelling
/// we do not recognise, which callers must treat as `Unknown` rather than guess.
fn parse_elem(s: &str) -> Option<Elem<'_>> {
    match s {
        "int" | "I" => Some(Elem::Prim("int")),
        "long" | "J" => Some(Elem::Prim("long")),
        "boolean" | "Z" => Some(Elem::Prim("boolean")),
        "char" | "C" => Some(Elem::Prim("char")),
        "byte" | "B" => Some(Elem::Prim("byte")),
        "short" | "S" => Some(Elem::Prim("short")),
        "float" | "F" => Some(Elem::Prim("float")),
        "double" | "D" => Some(Elem::Prim("double")),
        _ => {
            if let Some(inner) = s.strip_prefix('[') {
                Some(Elem::Array(inner))
            } else if let Some(n) = s.strip_prefix('L').and_then(|n| n.strip_suffix(';')) {
                Some(Elem::Class(n))
            } else {
                None
            }
        }
    }
}

/// Is an array whose element type is `have` assignable to an array type whose
/// element type is `want`? JVMS 4.10.1.2: array covariance is by element
/// subtyping for reference elements and by IDENTITY for primitive ones — `int[]`
/// is assignable only to `int[]`, never to `long[]` or `Object[]`.
fn elem_assignable(vm: &Vm, have: &str, want: &str) -> Tri {
    if have == want {
        return Tri::Yes;
    }
    let (h, w) = match (parse_elem(have), parse_elem(want)) {
        (Some(h), Some(w)) => (h, w),
        // An element spelling this engine did not write itself: refuse to decide.
        _ => return Tri::Unknown,
    };
    match (h, w) {
        // Primitive element types are invariant AND closed: equal or nothing.
        (Elem::Prim(a), Elem::Prim(b)) => if a == b { Tri::Yes } else { Tri::No },
        (Elem::Prim(_), _) | (_, Elem::Prim(_)) => Tri::No,
        // Nested arrays recurse; a nested array against a CLASS element is the
        // "array as an object" rule again (Object/Cloneable/Serializable only).
        (Elem::Array(a), Elem::Array(b)) => elem_assignable(vm, a, b),
        (Elem::Array(_), Elem::Class(c)) => {
            if ARRAY_SUPERS.contains(&c) { Tri::Yes } else { closed_set_miss(vm, c) }
        }
        // ENGINE GAP, deliberately lenient: this runtime's own natives
        // manufacture `Object[]` where HotSpot would produce the precise
        // component type — `Collection.toArray` allocates with the constant
        // `OBJ` element descriptor (stream/mod.rs), and erased generic code
        // allocates `new Object[n]` for a `T[]`. javac then emits `checkcast
        // [Ljava/lang/String;` at the use site, so answering `No` here would
        // throw on correct guest code because of OUR allocation, not theirs.
        // Refuse to decide instead. The reverse direction (`(Object[]) String[]`)
        // and unrelated element types (`(String[]) Integer[]`) are unaffected.
        //
        // ⚠ This arm must sit ABOVE the Class-vs-Array one, and a boot is what
        // proved it: MC casts an `Object[]` to `Predicate[][][]`, whose element
        // is itself an ARRAY, so an `Object` element reached the "a plain object
        // element is never an array" rule and threw ClassCastException in the
        // server tick loop. The leniency is a property of OUR element descriptor,
        // not of the shape on the other side.
        (Elem::Class("java/lang/Object"), _) => Tri::Unknown,
        // A plain object element is never an array.
        (Elem::Class(_), Elem::Array(_)) => Tri::No,
        (Elem::Class(a), Elem::Class(b)) => name_is_subtype_tri(vm, a, b),
    }
}

/// THREE-VALUED `instanceof`, over the same receiver shapes and the same
/// authorities as [`instance_of`], with two refinements that only a THROWING
/// consumer needs (and which `instance_of` deliberately does not make, so that
/// `instanceof`'s answers stay byte-for-byte what the boot already runs on):
///   * an array TARGET is decided by its element descriptor, not just by "the
///     value is some array" — that is the whole content of `(String[]) x`;
///   * an array VALUE against a class target answers `Yes` for Cloneable and
///     Serializable, which `instance_of` reports as a plain `false`.
///
/// `Value::Null` answers `No` here (it is the `instanceof` answer). The cast
/// consumers must short-circuit null BEFORE calling: JVMS 6.5 makes `checkcast`
/// and `aastore` accept null unconditionally.
pub(crate) fn instance_of_tri(vm: &Vm, value: &Value, target: &str) -> Tri {
    // ---- array target: decided by the element descriptor -------------------
    if let Some(want) = target.strip_prefix('[') {
        return match value {
            Value::Array(h) => match vm.heap.array_element(*h) {
                Some(have) => elem_assignable(vm, &have, want),
                None => Tri::Unknown,
            },
            Value::Null => Tri::No,
            // An arena STRING or arena LAMBDA is a known non-array shape, so the
            // rejection is as authoritative as for `Value::Str`/`Value::Ref`.
            #[cfg(feature = "next-gen-gc")]
            Value::Obj(p)
                if crate::nextgen::is_arena_string(*p) || crate::nextgen::is_arena_lambda(*p) =>
            {
                Tri::No
            }
            #[cfg(feature = "next-gen-gc")]
            Value::Obj(p) if arena_array_descriptor(*p).is_some() => {
                let have = arena_array_descriptor(*p).expect("arena array descriptor");
                elem_assignable(vm, &have[1..], want)
            }
            // A plain arena object, though, is NOT decidable here: the arena
            // registers array classes of its own (`nextgen::arena_array_class`),
            // so an arena-allocated array would arrive as `Obj` and must not be
            // rejected on a representation detail.
            #[cfg(feature = "next-gen-gc")]
            Value::Obj(_) => Tri::Unknown,
            // Arrays are the ONLY `Value::Array`; every other shape is an object
            // or a primitive and is authoritatively not an array.
            _ => Tri::No,
        };
    }
    if target == "java/lang/Object" {
        return Tri::Yes;
    }
    match value {
        Value::Null => Tri::No,
        Value::Array(_) => {
            if ARRAY_SUPERS.contains(&target) { Tri::Yes } else { closed_set_miss(vm, target) }
        }
        Value::Str(_) => {
            if STRING_SUPERS.contains(&target) { Tri::Yes } else { closed_set_miss(vm, target) }
        }
        Value::Int(_) | Value::Long(_) | Value::Float(_) | Value::Double(_) => {
            if boxed_supers(target) { Tri::Yes } else { closed_set_miss(vm, target) }
        }
        Value::Ref(h) => {
            // A lambda IS-A its recorded functional interface and nothing else —
            // same authority the `instance_of` handle arm uses.
            //
            // GUARDED BY THE O(1) FLAG. `$fi` is written only on the handle
            // `alloc_lambda` just returned (`indy.rs`), so `is_lambda` is the
            // EXACT precondition for the read — the same proof `instanceof_class_key`
            // already relies on. It WAS an O(fields-per-object) name-keyed field
            // probe on every cast of every object; it IS a bit test on the object
            // slot, and the probe now runs only for the handles that can answer it.
            if vm.heap.is_lambda(*h) {
                if let Value::Str(fi) = vm.heap.get_field(*h, "$fi") {
                    return name_is_subtype_tri(vm, &fi, target);
                }
            }
            match vm.heap.class_of(*h) {
                // The class name is BORROWED from the heap's interner, and both
                // borrows here are shared — the `to_string` that used to sit in
                // this line allocated a copy of an immortal name per cast.
                Some(c) => runtime_class_tri(vm, c, target),
                // No class recorded: nothing to walk from.
                None => Tri::Unknown,
            }
        }
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) if crate::nextgen::is_arena_string(*p) => {
            if STRING_SUPERS.contains(&target) { Tri::Yes } else { closed_set_miss(vm, target) }
        }
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) if crate::nextgen::is_arena_lambda(*p) => match arena_lambda_fi(*p) {
            Value::Str(fi) => name_is_subtype_tri(vm, &fi, target),
            // No functional interface recorded — the handle arm can only reach
            // Object from here, so refuse to decide rather than invent a No.
            _ => Tri::Unknown,
        },
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) if arena_array_descriptor(*p).is_some() => {
            if ARRAY_SUPERS.contains(&target) { Tri::Yes } else { closed_set_miss(vm, target) }
        }
        // `class_name` already hands back the registry's `Arc<str>` through the
        // lock-free per-thread mirror; deref it instead of copying the bytes.
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) => match crate::nextgen::class_name(crate::nextgen::arena_class_id(*p)) {
            Some(c) => runtime_class_tri(vm, &c, target),
            None => Tri::Unknown,
        },
        // StdOut/StdErr sentinels: not real objects, nothing to walk.
        _ => Tri::Unknown,
    }
}

/// Class name of a value for an exception MESSAGE only — never for a decision.
fn value_class(vm: &Vm, v: &Value) -> String {
    value_class_internal(vm, v).replace('/', ".")
}

/// The same class name in INTERNAL form (`java/lang/String`), which `cast_message` needs
/// to decide which loader owns it before it dots the name for display.
fn value_class_internal(vm: &Vm, v: &Value) -> String {
    let internal = match v {
        Value::Str(_) => "java/lang/String".to_string(),
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) if crate::nextgen::is_arena_string(*p) => "java/lang/String".to_string(),
        Value::Int(_) => "java/lang/Integer".to_string(),
        Value::Long(_) => "java/lang/Long".to_string(),
        Value::Float(_) => "java/lang/Float".to_string(),
        Value::Double(_) => "java/lang/Double".to_string(),
        // The heap stores a PRIMITIVE element as a word (`"int"`, written by
        // `primitive_array_type`), not as the JVM letter — so the naive `"[" + elem`
        // rendered `[int` in the exception text where HotSpot prints `[I`. A
        // reference element is already a descriptor and passes through.
        Value::Array(h) => {
            let e = vm.heap.array_element(*h).unwrap_or_else(|| std::sync::Arc::from("?"));
            let letter = match &*e {
                "int" => "I",
                "long" => "J",
                "double" => "D",
                "float" => "F",
                "short" => "S",
                "byte" => "B",
                "char" => "C",
                "boolean" => "Z",
                other => other,
            };
            format!("[{letter}")
        }
        Value::Ref(h) => vm.heap.class_of(*h).unwrap_or("?").to_string(),
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) => arena_array_descriptor(*p).unwrap_or_else(|| {
            crate::nextgen::class_name(crate::nextgen::arena_class_id(*p))
                .map(|c| c.to_string())
                .unwrap_or_else(|| "?".to_string())
        }),
        _ => "?".to_string(),
    };
    internal
}

/// The `ClassCastException` message HotSpot shapes as
/// "class A cannot be cast to class B (<where A is>; <where B is>)". Only reached on a
/// confident reject.
///
/// THE PARENTHESISED CLAUSE IS PART OF THE MESSAGE, and ours omitted it — a differential
/// probe reads `class java.lang.String cannot be cast to class java.lang.Integer` where
/// `java -Xint` reads the same followed by
/// `(java.lang.String and java.lang.Integer are in module java.base of loader
/// 'bootstrap')`. Guest code logs this text, so the tail is an answer like any other.
///
/// HotSpot collapses the clause when both classes sit in the same place and spells both
/// out otherwise; a JDK class is in `java.base` under the bootstrap loader and everything
/// this engine loads from the classpath is in the unnamed module under `app`.
pub(crate) fn cast_message(vm: &Vm, v: &Value, target: &str) -> String {
    let from_internal = value_class_internal(vm, v);
    let from = from_internal.replace('/', ".");
    let to = target.replace('/', ".");
    let where_of = |internal: &str| -> &'static str {
        if is_bootstrap_class(internal) { "module java.base of loader 'bootstrap'" } else { "unnamed module of loader 'app'" }
    };
    let (wf, wt) = (where_of(&from_internal), where_of(target));
    let tail = if wf == wt {
        format!("({from} and {to} are in {wf})")
    } else {
        format!("({from} is in {wf}; {to} is in {wt})")
    };
    format!("class {from} cannot be cast to class {to} {tail}")
}

/// Is this internal class name one the bootstrap loader owns? The JDK's own packages
/// are, and everything on the application classpath is not.
fn is_bootstrap_class(internal: &str) -> bool {
    internal.starts_with("java/")
        || internal.starts_with("javax/")
        || internal.starts_with("jdk/")
        || internal.starts_with("sun/")
}

thread_local! {
    /// `(caller class ptr, cp, receiver class identity)` → REJECT?
    ///
    /// Same key shape and the same stability argument as `IOF_MEMO` (see
    /// `classkey`): a caller class is never unloaded, the class-name interner
    /// never evicts, and an arena class id is a registry index. Separate from
    /// `IOF_MEMO` because it caches a different question — `instanceof`'s lenient
    /// bool folds `Unknown` into `true`, and a cast must distinguish it.
    static CCAST_MEMO: std::cell::RefCell<crate::FastMap<(usize, u16, u64), bool>> =
        std::cell::RefCell::new(crate::FastMap::default());
}

/// `checkcast` AT A SITE. `Some(message)` = throw `ClassCastException`.
///
/// Memoised on `(site, receiver class)` for the same reason `instance_of_at` is:
/// the target comes from the caller's immutable constant pool and the hierarchy
/// is immutable once loaded, so the answer is a pure function of the pair. Null
/// is handled by the caller (JVMS 6.5: a null reference casts to anything).
pub(crate) fn checkcast_reject(
    vm: &Vm,
    caller: &crate::class_loader::LoadedClass,
    cp: u16,
    value: &Value,
) -> Result<Option<String>, VmError> {
    // WIDENED: `cast_class_key` keys the `String`, unboxed-primitive and ARRAY
    // receivers too, which `instanceof_class_key` leaves unkeyed because the
    // lenient `instanceof` reads them differently. Those were the shapes that
    // re-ran the whole test on every execution — an array receiver against an
    // array target is a full element walk.
    let key = if fast_casts_enabled() {
        cast_class_key(&vm.heap, value)
    } else {
        instanceof_class_key(&vm.heap, value)
    };
    let site = (caller as *const crate::class_loader::LoadedClass as usize, cp);
    if let Some(k) = key {
        if let Some(hit) = CCAST_MEMO.with(|c| c.borrow().get(&(site.0, site.1, k)).copied()) {
            if !hit {
                return Ok(None);
            }
            let target = caller.file.pool.class_name(cp).ok_or(VmError::BadConstant(cp))?;
            return Ok(Some(cast_message(vm, value, target)));
        }
    }
    let target = caller.file.pool.class_name(cp).ok_or(VmError::BadConstant(cp))?;
    let reject = instance_of_tri(vm, value, target) == Tri::No;
    if let Some(k) = key {
        CCAST_MEMO.with(|c| { c.borrow_mut().insert((site.0, site.1, k), reject); });
    }
    Ok(if reject { Some(cast_message(vm, value, target)) } else { None })
}

/// The `aastore` STORE CHECK (JVMS 6.5 aastore). `Some(message)` = throw
/// `ArrayStoreException`.
///
/// The authority is the array's OWN element descriptor, recorded at allocation
/// (`Heap::alloc_array`) — the same descriptor `anewarray` built from the
/// constant pool. So this is a descriptor-level test, exactly like the runtime
/// component type HotSpot consults; no static type information is needed.
pub(crate) fn astore_reject(vm: &Vm, array: u32, value: &Value) -> Option<String> {
    // JVMS: null stores into any reference array.
    if matches!(value, Value::Null) {
        return None;
    }
    let elem = vm.heap.array_element(array)?;
    astore_reject_for_element(vm, &elem, value)
}

/// The descriptor-owned half of [`astore_reject`]. Arena reference arrays retain the same exact
/// component descriptor on their immutable arena CID, so both representations must share this
/// authority instead of growing a second assignability implementation.
pub(crate) fn astore_reject_for_element(
    vm: &Vm,
    elem: &str,
    value: &Value,
) -> Option<String> {
    // JVMS: null stores into any reference array. Keep this guard here as well so direct arena
    // callers have the same contract as the handle wrapper above.
    if matches!(value, Value::Null) {
        return None;
    }
    // The overwhelmingly common element type, and the one that accepts
    // everything — check it before parsing anything.
    if elem == "Ljava/lang/Object;" {
        return None;
    }
    // `aastore` had NO memo at all: every store into a typed reference array
    // re-parsed the element descriptor and re-ran `instance_of_tri`, which for a
    // class element is the hierarchy WALK. The verdict is a pure function of
    // (element type, receiver identity) — the element descriptor is fixed at
    // allocation and the hierarchy is immutable — so it WAS O(supertype closure)
    // per STORE and IS one probe per distinct pair. What grows is the number of
    // (element type x receiver class) pairs, not the number of elements stored.
    let key = if fast_casts_enabled() {
        cast_class_key(&vm.heap, value).map(|k| (elem.as_ptr() as u64, k))
    } else {
        None
    };
    if let Some(k) = key {
        if let Some(hit) = ASTORE_MEMO.with(|c| c.borrow().get(&k).copied()) {
            // The message is rebuilt on the throwing path only, which is cold by
            // construction — a hot `aastore` never rejects.
            return hit.then(|| value_class(vm, value));
        }
    }
    let tri = match parse_elem(elem) {
        // A primitive-element array cannot be the operand of a valid `aastore`;
        // leave it alone rather than manufacture a verdict about it.
        Some(Elem::Prim(_)) | None => return None,
        Some(Elem::Class(c)) => instance_of_tri(vm, value, c),
        // `String[][]` stores a `String[]`: the target IS the element descriptor.
        Some(Elem::Array(_)) => instance_of_tri(vm, value, elem),
    };
    let reject = tri == Tri::No;
    if let Some(k) = key {
        ASTORE_MEMO.with(|c| { c.borrow_mut().insert(k, reject); });
    }
    reject.then(|| value_class(vm, value))
}

thread_local! {
    /// `(array element name identity, receiver class identity)` → REJECT?
    ///
    /// The element key is the DATA pointer of the array-element interner's
    /// `Arc<str>` (`Heap::array_element`), which is push-only and never evicts —
    /// the same stability argument `CCAST_MEMO` makes for the class-name interner,
    /// and neither is an object address or a handle, so no GC epoch applies.
    /// Bounded by (distinct element descriptors x distinct stored classes), i.e.
    /// tens of bytes per pair and a few thousand pairs at most.
    static ASTORE_MEMO: std::cell::RefCell<crate::FastMap<(u64, u64), bool>> =
        std::cell::RefCell::new(crate::FastMap::default());
}
