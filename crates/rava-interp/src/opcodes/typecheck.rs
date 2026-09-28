//! instanceof and the name-based subtype tests behind it.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::opcodes::*;

/// `instanceof` AT A SITE: `(caller, cp)` names the target class, so the answer is
/// a pure function of (site, receiver class) for every non-lambda receiver — the
/// class hierarchy is immutable once loaded and loadability is a property of the
/// classpath, not of load ORDER. Memo off (or a non-memoisable receiver) ⇒ byte
/// -for-byte the old path, including the constant-pool borrow, which now also
/// spares the target `String` on the MISS.
#[allow(invalid_reference_casting)]
pub(crate) fn instance_of_at(
    vm: &crate::vm::thread_context::SharedVm,
    caller: &crate::class_loader::LoadedClass,
    cp: u16,
    value: &Value,
) -> Result<bool, VmError> {
    let on = instanceof_memo_enabled();
    let key = if on { instanceof_class_key(&vm.heap, value) } else { None };
    let site = (caller as *const crate::class_loader::LoadedClass as usize, cp);
    if let Some(k) = key {
        if let Some(hit) = crate::opcodes::classkey::IOF_MEMO.with(|c| c.borrow().get(&(site.0, site.1, k)).copied()) {
            iof_count(&IOF_HIT);
            return Ok(hit);
        }
    } else if on {
        iof_count(&IOF_UNKEYED);
    }
    let target = caller.file.pool.class_name(cp).ok_or(VmError::BadConstant(cp))?;
    let answer = instance_of(vm, value, target);
    if let Some(k) = key {
        iof_count(&IOF_MISS);
        crate::opcodes::classkey::IOF_MEMO.with(|c| {
            c.borrow_mut().insert((site.0, site.1, k), answer);
        });
    }
    Ok(answer)
}

/// Return the exact JVM descriptor for an arena array object.
///
/// Reference arrays retain their component descriptor on the CID; primitive arrays retain the
/// exact `newarray` atype in the primitive-array CID table.  Synthetic registry names are not
/// descriptors and are deliberately never consulted here.
#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn arena_array_descriptor(p: crate::arena::ArenaPtr) -> Option<String> {
    if let Some(component) = crate::nextgen::arena_ref_array_component(p) {
        return Some(format!("[{}", component.as_ref()));
    }
    let atype = crate::nextgen::primitive_array_atype(p)?;
    let primitive = match atype {
        4 => 'Z', // boolean
        5 => 'C', // char
        6 => 'F', // float
        7 => 'D', // double
        8 => 'B', // byte
        9 => 'S', // short
        10 => 'I', // int
        11 => 'J', // long
        _ => return None,
    };
    Some(format!("[{primitive}"))
}

/// CENSUS for the dispatch-class memo (`RAVA_VIRT_CLASS_STATS`, default OFF and inert).
///
/// The memo is built but has never been measured, and this tree has a recorded rule that
/// "it exists behind a flag" is not evidence it works - the census is. `note_build` counts the
/// calls that actually run `build()`, which for a heap `Ref` allocates an `Arc<str>` and for an
/// ARENA receiver takes the global class-registry mutex, clones an `Arc` and builds a `String`.
/// calls vs builds is therefore the allocation the memo would remove, counted rather than timed -
/// load-independent, which matters on a box too noisy for a sub-5% wall A/B.
pub(crate) mod virt_class_stats {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    pub(crate) static CALLS: AtomicU64 = AtomicU64::new(0);
    pub(crate) static BUILDS: AtomicU64 = AtomicU64::new(0);

    pub(crate) fn enabled() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("RAVA_VIRT_CLASS_STATS").is_some())
    }
    #[inline]
    pub(crate) fn note_call() {
        if enabled() {
            let n = CALLS.fetch_add(1, Relaxed) + 1;
            if n % 2_000_000 == 0 {
                let b = BUILDS.load(Relaxed);
                eprintln!(
                    "[VIRT-CLASS] {n} dispatch_class calls, {b} builds ({:.1}% served from memo)",
                    100.0 * (n - b) as f64 / n as f64
                );
            }
        }
    }
    #[inline]
    pub(crate) fn note_build() {
        if enabled() {
            BUILDS.fetch_add(1, Relaxed);
        }
    }
}

/// Is a value of array-component descriptor `from` assignable to component `to`?
/// (JLS 4.10.3, the widening rule `S[] <: T[] iff S <: T`.)
///
/// Descriptors, not class names: `I`, `Ljava/lang/Object;`, `[I`. Primitive
/// components are INVARIANT — `int[]` is not a `long[]` and not an `Object[]` —
/// which is what makes `(Object[]) someIntArray` a ClassCastException on HotSpot.
fn array_desc_assignable(vm: &Vm, from: &str, to: &str) -> bool {
    if from == to {
        return true;
    }
    fn class_of(d: &str) -> Option<&str> {
        d.strip_prefix('L').and_then(|r| r.strip_suffix(';'))
    }
    match (from.as_bytes().first(), to.as_bytes().first()) {
        // Reference (or nested-array) component into a class/interface component.
        (Some(b'L' | b'['), Some(b'L')) => {
            let sup = match class_of(to) { Some(s) => s, None => return false };
            if sup == "java/lang/Object" {
                return true;
            }
            match class_of(from) {
                Some(sub) => name_is_subtype(vm, sub, sup),
                // A nested array component: only the three types every array has.
                None => matches!(sup, "java/lang/Cloneable" | "java/io/Serializable"),
            }
        }
        // Nested arrays recurse a dimension; a class component never holds an array.
        (Some(b'['), Some(b'[')) => array_desc_assignable(vm, &from[1..], &to[1..]),
        // A primitive component matches only itself (handled by `from == to`).
        _ => false,
    }
}

/// `instanceof` test. Walks the loaded class hierarchy + interfaces; when a
/// superclass/interface isn't loadable (JDK boundary) it can't be disproved, so
/// the test is lenient (true) to keep execution moving.
pub(crate) fn instance_of(vm: &Vm, value: &Value, target: &str) -> bool {
    // An array target type ("[...") only matches array values. The lenient
    // name-walk below treats a non-loadable "[..." target as inconclusive and
    // returned true for any object, so `x instanceof Object[]` / Object[].class.
    // isInstance(x) wrongly passed for non-arrays.
    if target.starts_with('[') {
        let vd = match value {
            Value::Array(h) => {
                // ...and the ELEMENT types have to be assignable (JVMS 6.5 checkcast).
                // "any array matches any array target" made `new Object[3] instanceof B[]`
                // true, which is the exact test a guest uses before an unchecked cast — so
                // the cast that follows was licensed by a wrong answer.
                let elem = vm.heap.array_element(*h).unwrap_or_else(|| std::sync::Arc::from("Ljava/lang/Object;"));
                crate::natives::array_class_name(&elem)
            }
            #[cfg(feature = "next-gen-gc")]
            Value::Obj(p) => match arena_array_descriptor(*p) {
                Some(descriptor) => descriptor,
                None => return false,
            },
            _ => return false,
        };
        return array_desc_assignable(vm, &vd[1..], &target[1..]);
    }
    // Boxed primitives (kept unboxed in our value model) answer instanceof by
    // their wrapper type + Number/Comparable/Serializable/Object.
    let numeric = |wrapper: &str| -> bool {
        matches!(target,
            "java/lang/Number" | "java/lang/Comparable" | "java/io/Serializable" | "java/lang/Object")
            || target == wrapper
    };
    let start = match value {
        Value::Null => return false,
        // Every array type implements Cloneable and java.io.Serializable (JLS 10.7),
        // and its only superclass is Object. Both interfaces were missing, so a
        // guest that routes on `x instanceof Serializable` (any serializer, any
        // deep-copy helper) took the non-array branch for an array. The "[" target
        // case is already decided above.
        Value::Array(_) => return matches!(target,
            "java/lang/Object" | "java/lang/Cloneable" | "java/io/Serializable"),
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) if arena_array_descriptor(*p).is_some() => return matches!(target,
            "java/lang/Object" | "java/lang/Cloneable" | "java/io/Serializable"),
        Value::Str(_) => return matches!(target,
            "java/lang/String" | "java/lang/Object" | "java/lang/CharSequence" | "java/lang/Comparable" | "java/io/Serializable"),
        Value::Int(_) => return numeric("java/lang/Integer"),
        Value::Long(_) => return numeric("java/lang/Long"),
        Value::Float(_) => return numeric("java/lang/Float"),
        Value::Double(_) => return numeric("java/lang/Double"),
        Value::Ref(h) => {
            // A lambda/method-reference object IS-A its functional interface
            // ($fi). It's `instanceof T` iff that interface is a subtype of T —
            // e.g. a `value -> ...` layer is an EnvironmentAttributeLayer$Constant,
            // which a sealed-type pattern switch tests via instanceof.
            //
            // COMPLEXITY: reading `$fi` is a name→id probe in the global field interner plus a
            // scan of the OBJECT'S id array — and on a non-lambda that scan runs to the END and
            // finds nothing, so the whole cost was paid to answer "is this a lambda?". WAS
            // O(fields per object) + one interner probe on EVERY heap-`Ref` instanceof; IS one
            // object-slot load and an `Option` test, with the field read left to the objects
            // that actually carry one. The precondition is the tree's own: `$fi` is written only
            // by `alloc_lambda`, which is exactly what `instanceof_class_key` already relies on
            // to keep lambdas out of the instanceof memo — so `!is_lambda` PROVES the read would
            // have returned `Null`, and this cannot change an answer.
            if !crate::opcodes::fast_eq_enabled() || vm.heap.is_lambda(*h) {
                if let Value::Str(fi) = vm.heap.get_field(*h, "$fi") {
                    return target == "java/lang/Object" || name_is_subtype(vm, &fi, target);
                }
            }
            match vm.heap.class_of(*h) {
                Some(c) => c.to_string(),
                None => return true,
            }
        }
        // An arena STRING answers instanceof exactly like Value::Str — its
        // getClass() is java/lang/String (see val_class_name), so instanceof MUST
        // agree, else a getClass/instanceof mismatch trips gson's
        // JsonPrimitive.getAsString AssertionError ("Unexpected value type:
        // java/lang/String"). Its arena class name is the synthetic "[arena-string]",
        // which the name-walk below can't relate to java/lang/String.
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) if crate::nextgen::is_arena_string(*p) => {
            return matches!(target,
                "java/lang/String" | "java/lang/Object" | "java/lang/CharSequence"
                | "java/lang/Comparable" | "java/io/Serializable")
        }
        // RAVA_ARENA_LAMBDA: a capturing closure IS-A its functional interface,
        // exactly as the handle arm above decides it. Without this arm the generic
        // name-walk below sees the synthetic class "[arena-lambda:…]", which no
        // loader can relate to anything, goes inconclusive, and answers FALSE for
        // every jar-loadable target — so the same logical lambda would be
        // `instanceof T` as a handle and not as an arena object. A sealed-type
        // pattern switch decides control flow on exactly that answer.
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) if crate::nextgen::is_arena_lambda(*p) => {
            return match arena_lambda_fi(*p) {
                Value::Str(fi) => target == "java/lang/Object" || name_is_subtype(vm, &fi, target),
                // No $fi recorded (the indy descriptor's return type wasn't a class):
                // the handle arm falls through to a class-name walk that can only
                // reach Object, so match that outcome rather than guessing a subtype.
                _ => target == "java/lang/Object",
            };
        }
        // Arena object: its concrete class lives in the arena header; run the same
        // name-walk subtype test as a handle.
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) => match crate::nextgen::class_name(crate::nextgen::arena_class_id(*p)) {
            Some(c) => c.to_string(),
            None => return true,
        },
        _ => return false,
    };
    if target == "java/lang/Object" {
        return true;
    }
    // A jar-loadable target can be fully verified by name-walk; a JDK target we
    // can't enumerate is treated leniently (only when the walk is inconclusive).
    // If the object's OWN class is loadable, its supertype chain is fully
    // enumerable from classfiles (each declares its super + interfaces by name),
    // so a miss is authoritative — never leniently true. Leniency is only for a
    // genuine JDK object whose class we can't introspect. Without this, a known
    // jar class (Bukkit MemorySection) leniently matched unrelated concrete JDK
    // classes (Date, Calendar), so snakeyaml dumped it via the wrong Represent.
    //
    // ⭐**BOTH OF THOSE PROBES FEED ONLY THE FALLBACK EXPRESSION AT THE BOTTOM**, which a walk
    // that DECIDES the question never reaches — and deciding it is the overwhelmingly common
    // case. Each probe is a lock plus a hash of a class name, and for a name not yet in the
    // cache it is a full classfile READ AND PARSE. COMPLEXITY: WAS 2 loader probes (up to 2
    // class loads) per `instance_of` EXECUTION; IS 0 on every conclusive answer, 2 on a
    // genuinely inconclusive walk. What grows is executions — the JIT never amortises them,
    // because this runs on the host side of the boundary either way. Loading is not
    // initialisation, so nothing observable is deferred: no `<clinit>` runs here, and the walk
    // below still loads exactly the classes it needs, in exactly the same order.
    let eager = !crate::opcodes::fast_eq_enabled();
    let (mut target_loadable, mut start_loadable) = (false, false);
    if eager {
        target_loadable = vm.loader.get_or_load(target).is_ok();
        start_loadable = vm.loader.get_or_load(&start).is_ok();
    }
    // `FastSet`, not `HashSet`: the keys are class NAMES and the default hasher is SipHash,
    // which this crate replaced everywhere else for exactly this reason (`fxhash`, word-at-a-
    // time). Membership order is never observed, so the walk is bit-identical.
    let mut seen: crate::FastSet<String> = crate::FastSet::default();
    let mut queue = vec![start.clone()];
    let mut inconclusive = false;
    while let Some(cn) = queue.pop() {
        if cn == target {
            return true;
        }
        if seen.contains(&cn) {
            continue;
        }
        match vm.loader.get_or_load(&cn) {
            Ok(c) => {
                // Interface names are recorded even for JDK interfaces, so a
                // direct name match works without loading them.
                if c.file.interfaces.iter().any(|i| i == target) {
                    return true;
                }
                // Filter at PUSH time. An interface already visited is a name COPIED, queued,
                // popped and thrown away — and a diamond hierarchy re-presents the same marker
                // interfaces from every branch it has. The load sequence is unchanged: a name
                // in `seen` would have been popped and skipped without loading anyway.
                queue.extend(
                    c.file.interfaces.iter().filter(|i| !seen.contains(i.as_str())).cloned(),
                );
                if let Some(s) = c.file.super_class.clone() {
                    queue.push(s);
                }
            }
            // No bytecode: continue through the known JDK hierarchy (same table
            // as name_is_subtype) — only a type it doesn't know is inconclusive.
            Err(_) => match jdk_supers_indexed(&cn) {
                Some(supers) => queue.extend(
                    supers.iter().filter(|s| !seen.contains(**s)).map(|s| s.to_string()),
                ),
                None => inconclusive = true,
            },
        }
        // MOVE, don't clone. `seen.insert(cn.clone())` allocated a private copy of every name
        // it visited, and the popped name is dead the moment the load above is done with it.
        // WAS one `String` malloc + memcpy per node visited (a class name is 30-60 bytes); IS
        // zero. The membership test moved ahead of the load so the visit order, the load order
        // and the answer are all byte-identical.
        seen.insert(cn);
    }
    // Well-known JDK targets are fully determinable: String and the primitive
    // wrappers are final (can only match exactly, already checked above), and
    // CharSequence/Number are declared as a super/interface by any subtype (the
    // name-walk above catches that). So a miss here is a genuine false — never
    // leniently true. (Without this, `LazilyParsedNumber instanceof String` was
    // true, so gson's getAsString returned the number object → indexOf frontier.)
    if matches!(target,
        "java/lang/String" | "java/lang/CharSequence" | "java/lang/Number"
        | "java/lang/Integer" | "java/lang/Long" | "java/lang/Double" | "java/lang/Float"
        | "java/lang/Short" | "java/lang/Byte" | "java/lang/Boolean" | "java/lang/Character")
    {
        return false;
    }
    // A table-known JDK target was walked authoritatively above — a miss is a
    // real no (the old blanket leniency made everything `instanceof EnumSet`).
    if eager {
        return inconclusive && !target_loadable && !start_loadable && jdk_supers(target).is_none();
    }
    // Same conjunction, cheapest and most selective term first: `inconclusive` is false for
    // nearly every walk, the table probe is a hash, and only then are the two loader probes
    // paid. `&&` short-circuits, so this is the identical predicate.
    inconclusive
        && jdk_supers_indexed(target).is_none()
        && vm.loader.get_or_load(target).is_err()
        && vm.loader.get_or_load(&start).is_err()
}

/// `sup.isAssignableFrom(sub)` by name: is `sub` a subtype of `sup`? Walks
/// `sub`'s superclass + interface chain. Fully decided for jar-loadable
/// hierarchies; only lenient (true) when the walk hits a JDK ancestor we can't
/// enumerate AND `sup` is itself a non-loadable JDK type.
/// Direct supertypes (superclass + interfaces) of core JDK classes we have no
/// bytecode for. Lets the subtype walk continue through native-backed types
/// instead of going inconclusive — the old "both sides unloadable → true"
/// fallback made EVERY `instanceof <jdk type>` succeed (e.g. a guava filtered
/// collection tested true for `instanceof EnumSet`, sending library code down
/// wildly wrong branches). Generic JDK knowledge, same standing as the natives.
pub(crate) const JDK_SUPERS: &[(&str, &[&str])] = &[
    // The root: Object extends nothing, implements nothing. Without this row
    // EVERY chain terminating at the (native-served) Object was inconclusive,
    // so may-subtype over-included the whole loaded universe for unloadable
    // interface owners (the 3.7k `.size()I` name-collision explosion).
    ("java/lang/Object", &[]),
    // exceptions / errors
    ("java/lang/Throwable", &[]),
    ("java/lang/Exception", &["java/lang/Throwable"]),
    ("java/lang/Error", &["java/lang/Throwable"]),
    ("java/lang/RuntimeException", &["java/lang/Exception"]),
    ("java/lang/NullPointerException", &["java/lang/RuntimeException"]),
    ("java/lang/IllegalArgumentException", &["java/lang/RuntimeException"]),
    ("java/lang/IllegalStateException", &["java/lang/RuntimeException"]),
    ("java/lang/IndexOutOfBoundsException", &["java/lang/RuntimeException"]),
    ("java/lang/ArrayIndexOutOfBoundsException", &["java/lang/IndexOutOfBoundsException"]),
    ("java/lang/StringIndexOutOfBoundsException", &["java/lang/IndexOutOfBoundsException"]),
    ("java/lang/ClassCastException", &["java/lang/RuntimeException"]),
    ("java/lang/UnsupportedOperationException", &["java/lang/RuntimeException"]),
    ("java/lang/ArithmeticException", &["java/lang/RuntimeException"]),
    ("java/lang/NumberFormatException", &["java/lang/IllegalArgumentException"]),
    ("java/lang/NegativeArraySizeException", &["java/lang/RuntimeException"]),
    ("java/util/NoSuchElementException", &["java/lang/RuntimeException"]),
    ("java/util/ConcurrentModificationException", &["java/lang/RuntimeException"]),
    ("java/lang/ClassNotFoundException", &["java/lang/ReflectiveOperationException"]),
    ("java/lang/NoSuchFieldException", &["java/lang/ReflectiveOperationException"]),
    ("java/lang/NoSuchMethodException", &["java/lang/ReflectiveOperationException"]),
    ("java/lang/ReflectiveOperationException", &["java/lang/Exception"]),
    ("java/lang/InterruptedException", &["java/lang/Exception"]),
    ("java/io/IOException", &["java/lang/Exception"]),
    ("java/io/FileNotFoundException", &["java/io/IOException"]),
    ("java/io/EOFException", &["java/io/IOException"]),
    ("java/io/UncheckedIOException", &["java/lang/RuntimeException"]),
    ("java/nio/file/NoSuchFileException", &["java/nio/file/FileSystemException"]),
    ("java/nio/file/FileSystemException", &["java/io/IOException"]),
    ("java/lang/OutOfMemoryError", &["java/lang/VirtualMachineError"]),
    ("java/lang/StackOverflowError", &["java/lang/VirtualMachineError"]),
    ("java/lang/InternalError", &["java/lang/VirtualMachineError"]),
    ("java/lang/VirtualMachineError", &["java/lang/Error"]),
    ("java/lang/NoClassDefFoundError", &["java/lang/LinkageError"]),
    ("java/lang/UnsatisfiedLinkError", &["java/lang/LinkageError"]),
    ("java/lang/ExceptionInInitializerError", &["java/lang/LinkageError"]),
    ("java/lang/LinkageError", &["java/lang/Error"]),
    ("java/lang/AssertionError", &["java/lang/Error"]),
    // A JDK throwable MISSING from this table has no provable supertype at all,
    // so `instance_of` refuses even `catch (Throwable)` and the exception escapes
    // every handler in the program (measured: a thrown ArrayStoreException walked
    // straight out of `catch (RuntimeException)`, `catch (Exception)` AND
    // `catch (Throwable)`). Each row below is the class's real direct superclass,
    // read off HotSpot with `getSuperclass()`.
    ("java/lang/ArrayStoreException", &["java/lang/RuntimeException"]),
    ("java/lang/IllegalMonitorStateException", &["java/lang/RuntimeException"]),
    ("java/lang/SecurityException", &["java/lang/RuntimeException"]),
    ("java/lang/CloneNotSupportedException", &["java/lang/Exception"]),
    ("java/lang/InstantiationException", &["java/lang/ReflectiveOperationException"]),
    ("java/lang/IllegalAccessException", &["java/lang/ReflectiveOperationException"]),
    ("java/lang/reflect/InvocationTargetException", &["java/lang/ReflectiveOperationException"]),
    ("java/lang/IncompatibleClassChangeError", &["java/lang/LinkageError"]),
    ("java/lang/NoSuchMethodError", &["java/lang/IncompatibleClassChangeError"]),
    ("java/lang/NoSuchFieldError", &["java/lang/IncompatibleClassChangeError"]),
    ("java/lang/AbstractMethodError", &["java/lang/IncompatibleClassChangeError"]),
    ("java/lang/ClassFormatError", &["java/lang/LinkageError"]),
    ("java/lang/UnsupportedClassVersionError", &["java/lang/ClassFormatError"]),
    ("java/util/EmptyStackException", &["java/lang/RuntimeException"]),
    ("java/util/InputMismatchException", &["java/util/NoSuchElementException"]),
    ("java/util/concurrent/TimeoutException", &["java/lang/Exception"]),
    ("java/util/concurrent/ExecutionException", &["java/lang/Exception"]),
    ("java/util/concurrent/CancellationException", &["java/lang/IllegalStateException"]),
    ("java/util/concurrent/RejectedExecutionException", &["java/lang/RuntimeException"]),
    ("java/util/zip/ZipException", &["java/io/IOException"]),
    ("java/util/zip/DataFormatException", &["java/lang/Exception"]),
    ("java/io/UnsupportedEncodingException", &["java/io/IOException"]),
    ("java/io/InterruptedIOException", &["java/io/IOException"]),
    ("java/net/SocketException", &["java/io/IOException"]),
    ("java/net/SocketTimeoutException", &["java/io/InterruptedIOException"]),
    ("java/net/UnknownHostException", &["java/io/IOException"]),
    // collections
    ("java/lang/Iterable", &[]),
    ("java/util/Iterator", &[]),
    ("java/util/ListIterator", &["java/util/Iterator"]),
    ("java/util/Collection", &["java/lang/Iterable"]),
    ("java/util/List", &["java/util/Collection"]),
    ("java/util/Set", &["java/util/Collection"]),
    ("java/util/Queue", &["java/util/Collection"]),
    ("java/util/Deque", &["java/util/Queue"]),
    ("java/util/SortedSet", &["java/util/Set"]),
    ("java/util/NavigableSet", &["java/util/SortedSet"]),
    ("java/util/Map", &[]),
    ("java/util/SortedMap", &["java/util/Map"]),
    ("java/util/NavigableMap", &["java/util/SortedMap"]),
    ("java/util/AbstractCollection", &["java/util/Collection"]),
    ("java/util/AbstractList", &["java/util/AbstractCollection", "java/util/List"]),
    ("java/util/AbstractSet", &["java/util/AbstractCollection", "java/util/Set"]),
    ("java/util/AbstractMap", &["java/util/Map"]),
    ("java/util/ArrayList", &["java/util/AbstractList", "java/util/RandomAccess"]),
    ("java/util/LinkedList", &["java/util/AbstractList", "java/util/Deque"]),
    ("java/util/HashSet", &["java/util/AbstractSet"]),
    ("java/util/LinkedHashSet", &["java/util/HashSet"]),
    ("java/util/TreeSet", &["java/util/AbstractSet", "java/util/NavigableSet"]),
    ("java/util/EnumSet", &["java/util/AbstractSet"]),
    ("java/util/HashMap", &["java/util/AbstractMap"]),
    ("java/util/LinkedHashMap", &["java/util/HashMap"]),
    ("java/util/TreeMap", &["java/util/AbstractMap", "java/util/NavigableMap"]),
    ("java/util/EnumMap", &["java/util/AbstractMap"]),
    ("java/util/IdentityHashMap", &["java/util/AbstractMap"]),
    // reflection
    ("java/lang/reflect/Type", &[]),
    ("java/lang/Class", &["java/lang/reflect/Type"]),
    ("java/lang/reflect/ParameterizedType", &["java/lang/reflect/Type"]),
    ("java/lang/reflect/GenericArrayType", &["java/lang/reflect/Type"]),
    ("java/lang/reflect/WildcardType", &["java/lang/reflect/Type"]),
    ("java/lang/reflect/TypeVariable", &["java/lang/reflect/Type"]),
    // common value types
    ("java/lang/Number", &[]),
    ("java/lang/Integer", &["java/lang/Number", "java/lang/Comparable"]),
    ("java/lang/Long", &["java/lang/Number", "java/lang/Comparable"]),
    ("java/lang/Float", &["java/lang/Number", "java/lang/Comparable"]),
    ("java/lang/Double", &["java/lang/Number", "java/lang/Comparable"]),
    ("java/lang/Short", &["java/lang/Number", "java/lang/Comparable"]),
    ("java/lang/Byte", &["java/lang/Number", "java/lang/Comparable"]),
    // Boolean/Character are the two boxed value types that are NOT Numbers.
    // Their absence made every inconclusive-chain loaded class a "possible
    // subtype" of java/lang/Boolean in the lenient walk's fallback, which the
    // GAP-A01 unloadable-owner closed-check (lib.rs) inherits — final JDK
    // classes no loaded class can actually reach. Same generic-JDK standing as
    // the Integer row above.
    ("java/lang/Boolean", &["java/lang/Comparable"]),
    ("java/lang/Character", &["java/lang/Comparable"]),
    ("java/lang/String", &["java/lang/CharSequence", "java/lang/Comparable"]),
    ("java/lang/StringBuilder", &["java/lang/CharSequence"]),
    ("java/lang/Enum", &["java/lang/Comparable"]),
];

/// Indexed lookup into [`JDK_SUPERS`] — the form every walk in this file uses.
///
/// COMPLEXITY: `jdk_supers` (parent module) is `JDK_SUPERS.iter().find(|(n, _)| *n == name)`, a
/// LINEAR SCAN with a full string compare per row, run once per BYTECODELESS node of every
/// hierarchy walk — and every walk ends at bytecodeless nodes, because that is what a JDK
/// ancestor is. The MISSES cost the most and are the most frequent: the marker interfaces every
/// chain meets (`Comparable`, `Cloneable`, `java/io/Serializable`, `CharSequence`,
/// `RandomAccess`) are not keys, so each one scanned all 111 rows to learn nothing, on every
/// walk. WAS O(rows in `JDK_SUPERS`) string compares per bytecodeless node; IS one hash and one
/// probe. What grows is the TABLE — it has gained a row every time this engine met a JDK type
/// it could not enumerate — while the walks that read it are on the interpreted path.
///
/// SIZE: 111 entries of (`&'static str`, `&'static [&'static str]`) — two fat pointers, 32 B a
/// bucket. hashbrown rounds up to 128 buckets ⇒ **4 096 B + 128 control bytes ≈ 4.2 KB, ONE
/// allocation for the whole process**, built on first use and read-only after. That is not a
/// probe in front of a two-entry scan (the shape this tree has twice measured LOSING): it
/// replaces a scan that already streamed 1.8 KB of table and dereferenced up to 111 names.
fn jdk_supers_indexed(name: &str) -> Option<&'static [&'static str]> {
    static INDEX: std::sync::OnceLock<crate::FastMap<&'static str, &'static [&'static str]>> =
        std::sync::OnceLock::new();
    INDEX.get_or_init(|| JDK_SUPERS.iter().copied().collect()).get(name).copied()
}

/// SHARED-EXECUTION twin of [`name_is_subtype`] (`&Vm`).
///
/// Same walk, same order, same conservative fallback — the ONE difference is that a class which
/// is not already loaded makes it return `None` ("cannot answer without loading") instead of
/// loading it. That is not a weaker contract on the path this exists for: `get_or_load` already
/// refuses to load on a gil-free worker, requesting the region abort and failing closed, so a
/// worker could never have taken the loading branch anyway. `None` simply lets the CALLER decide
/// (abort, or fall back to the `&mut` path on the main thread) instead of silently answering from
/// a failed load.
///
/// Kept as a twin rather than replacing the original: the main thread must still be able to load
/// during `instanceof`, and duplicating thirty lines is cheaper than threading a mode flag through
/// a hot recursive walk.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn name_is_subtype_shared(vm: &Vm, sub: &str, sup: &str) -> Option<bool> {
    if sup == "java/lang/Object" || sub == sup { return Some(true); }
    // Same walk economics as `instance_of`: `FastSet` over the class names, the visited name
    // MOVED into it instead of copied, and no `String` minted for a parent already visited.
    // WAS one malloc + one SipHash per node; IS one Fx probe.
    let mut seen: crate::FastSet<String> = crate::FastSet::default();
    let mut queue = vec![sub.to_string()];
    let mut inconclusive = false;
    while let Some(cn) = queue.pop() {
        if cn == sup { return Some(true); }
        if seen.contains(&cn) { continue; }
        match vm.loader.get_loaded(&cn) {
            Some(c) => {
                if c.file.interfaces.iter().any(|i| i == sup) { return Some(true); }
                queue.extend(c.file.interfaces.iter().filter(|i| !seen.contains(i.as_str())).cloned());
                if let Some(s) = c.file.super_class.clone() { queue.push(s); }
            }
            // Not in the cache. If it is PROVEN absent, this is the `Err` arm of the original
            // walk and the JDK table decides; if it is merely not loaded yet, answering would
            // require a load and we must not.
            None if vm.loader.known_missing(&cn) => match jdk_supers_indexed(&cn) {
                Some(supers) => queue.extend(supers.iter().filter(|s| !seen.contains(**s)).map(|s| s.to_string())),
                None => inconclusive = true,
            },
            None => return None,
        }
        seen.insert(cn);
    }
    // Same fallback as the original, with the same read-only substitution for its final probe.
    let sup_absent = vm.loader.get_loaded(sup).is_none();
    if sup_absent && !vm.loader.known_missing(sup) {
        return None;
    }
    Some(inconclusive && jdk_supers_indexed(sup).is_none() && sup_absent)
}

/// ⭐**THE ONE WALK NO MEMO STANDS IN FRONT OF.** `instance_of` is memoised per (site, receiver
/// class) — except for LAMBDAS, which are deliberately unkeyed because their answer comes from
/// the per-object functional interface, and a lambda's `instanceof` lands here. A boot that
/// builds its registries out of codecs asks this question constantly, so this walk runs raw.
pub(crate) fn name_is_subtype(vm: &Vm, sub: &str, sup: &str) -> bool {
    if sup == "java/lang/Object" || sub == sup { return true; }
    let mut seen: crate::FastSet<String> = crate::FastSet::default();
    let mut queue = vec![sub.to_string()];
    let mut inconclusive = false;
    while let Some(cn) = queue.pop() {
        if cn == sup { return true; }
        if seen.contains(&cn) { continue; }
        match vm.loader.get_or_load(&cn) {
            Ok(c) => {
                if c.file.interfaces.iter().any(|i| i == sup) { return true; }
                queue.extend(c.file.interfaces.iter().filter(|i| !seen.contains(i.as_str())).cloned());
                if let Some(s) = c.file.super_class.clone() { queue.push(s); }
            }
            // No bytecode (native-backed JDK type): continue through the known
            // JDK hierarchy; only a type the table doesn't know is inconclusive.
            Err(_) => match jdk_supers_indexed(&cn) {
                Some(supers) => queue.extend(supers.iter().filter(|s| !seen.contains(**s)).map(|s| s.to_string())),
                None => inconclusive = true,
            },
        }
        seen.insert(cn);
    }
    // Conservative fallback ONLY for genuinely unknown pairs: if the supertype
    // is a table-known JDK type, the walk above was authoritative — answer no.
    inconclusive && jdk_supers_indexed(sup).is_none() && vm.loader.get_or_load(sup).is_err()
}

/// Strict variant for `Class.isAssignableFrom`: `sub` is a subtype of `sup`
/// ONLY if the hierarchy walk DEFINITELY reaches `sup`; an unknown JDK boundary
/// answers NO (never lenient). Adapter-matching libraries (gson registers a
/// `TypeHierarchyAdapter` for InetAddress) call this to pick a serializer, and a
/// lenient true made the InetAddress adapter match an ArrayList → every empty
/// list serialized as "127.0.0.1".
pub(crate) fn name_is_subtype_strict(vm: &Vm, sub: &str, sup: &str) -> bool {
    if sup == "java/lang/Object" || sub == sup { return true; }
    let mut seen: crate::FastSet<String> = crate::FastSet::default();
    let mut queue = vec![sub.to_string()];
    while let Some(cn) = queue.pop() {
        if cn == sup { return true; }
        if seen.contains(&cn) { continue; }
        match vm.loader.get_or_load(&cn) {
            Ok(c) => {
                if c.file.interfaces.iter().any(|i| i == sup) { return true; }
                queue.extend(c.file.interfaces.iter().filter(|i| !seen.contains(i.as_str())).cloned());
                if let Some(s) = c.file.super_class.clone() { queue.push(s); }
            }
            Err(_) => if let Some(supers) = jdk_supers_indexed(&cn) {
                queue.extend(supers.iter().filter(|s| !seen.contains(**s)).map(|s| s.to_string()));
            },
        }
        seen.insert(cn);
    }
    false
}

/// GAP-A01 (unloadable-iface closure, Gate 1): MAY `sub` be a subtype of `sup`?
/// Third variant beside `name_is_subtype`/`name_is_subtype_strict`, in the
/// OVER-INCLUSIVE direction a closure ENUMERATION needs: any walk that dies at
/// an unknown JDK boundary (an unloadable name absent from `JDK_SUPERS`,
/// anywhere in the chain) answers UNCONDITIONALLY true. The lenient
/// `name_is_subtype` fallback (`jdk_supers(sup).is_none() && sup unloadable`)
/// is the right shape for its REFUTATION consumer (`unloadable_owner_closed`:
/// an unrefuted pair conservatively REJECTS an owner) but UNDER-includes for an
/// enumerator whenever `sup` is a table key like `java/util/List` — there an
/// inconclusive implementor would be silently DROPPED from the closure (a
/// missing dispatch target, unsound), not merely over-rejected.
pub(crate) fn name_may_subtype(vm: &mut Vm, sub: &str, sup: &str) -> bool {
    if sup == "java/lang/Object" || sub == sup { return true; }
    let mut seen: crate::FastSet<String> = crate::FastSet::default();
    let mut queue = vec![sub.to_string()];
    while let Some(cn) = queue.pop() {
        if cn == sup { return true; }
        if seen.contains(&cn) { continue; }
        match vm.loader.get_or_load(&cn) {
            Ok(c) => {
                if c.file.interfaces.iter().any(|i| i == sup) { return true; }
                queue.extend(c.file.interfaces.iter().filter(|i| !seen.contains(i.as_str())).cloned());
                if let Some(s) = c.file.super_class.clone() { queue.push(s); }
            }
            Err(_) => match jdk_supers_indexed(&cn) {
                Some(supers) => queue.extend(
                    supers.iter().filter(|s| !seen.contains(**s)).map(|s| s.to_string()),
                ),
                // GILFREE-RINGS precision: a BYTECODELESS (native-served) chain
                // node cannot implement a LOADABLE guest interface — a class with
                // no class file has exactly the interface surface rava's native
                // model gives it, and that model never names guest types (JVM
                // delegation: bootstrap-era code cannot link against app types).
                // So when `sup` itself is loadable, this branch is conclusively
                // dead; only an UNLOADABLE `sup` keeps the old may-subtype-ANYTHING.
                None => {
                    if vm.loader.cached(sup).is_none() {
                        return true;
                    }
                }
            },
        }
        seen.insert(cn);
    }
    false
}
