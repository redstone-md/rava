//! GAP-S03: declarative effect contracts for NATIVELY-SERVED guest methods.
//!
//! A native-served method has no bytecode for the escape analysis, so the
//! transitive offload proof cannot analyse it. The ONLY way such a method may
//! participate in an offload/fork proof is through an explicit contract declared
//! here, describing the effects of **rava's own Rust implementation** of that
//! method (the module that serves it is named on every entry). The default is
//! REJECT: a native without a contract, or a query whose mechanical side
//! condition fails, returns `None` and the caller must keep the serial path.
//!
//! Contract queries are keyed by the EXACT dynamic receiver class (the analysis
//! supplies it from `new C` freshness, the fork's concrete element type, an
//! `invokespecial`/`invokestatic` owner, or an enumerated dispatch-closure
//! target — never from a shared receiver's declared interface). Combined with
//! the analysis resolving guest BYTECODE first (a resolvable body is always
//! analysed, never trusted by name), renaming guest classes or methods can only
//! LOSE admission, never gain it: a guest class that happens to share a JDK
//! name still has its real bytecode analysed, and a contract entry that no
//! longer matches anything is dead metadata.
//!
//! Mechanical side conditions checked here, not by the caller:
//! - enum reads (`ordinal`/`name`/...) require `ACC_ENUM` on the exact class
//!   (the flags are passed in by the caller from the loaded class file), or the
//!   exact class `java/lang/Enum` itself;
//! - every other entry matches an exact class name plus method name (and
//!   signature where the implementation is signature-sensitive).
//!
//! Removed relative to the pre-contract allowlists (fail closed, see
//! GAP-S03): the guava `LoadingCache`/`Cache` entries (their native service
//! re-enters a guest loader — `may_callback` — and guava is guest bytecode the
//! interceptor may or may not own) and the fastutil map entries (fastutil is
//! guest bytecode; its bodies are analysed like any other bytecode).

/// What the native implementation may touch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum NativeEffect {
    /// Reads its receiver/arguments and shared state; writes NOTHING another
    /// thread could observe. May allocate fresh guest objects iff `allocates`.
    Pure,
    /// Mutates ONLY its receiver (and reads its arguments). Admissible only
    /// when the caller proves the receiver private (fresh / owned element).
    ReceiverMut,
}

/// The declared contract of one natively-served method family.
#[derive(Clone, Copy, Debug)]
pub(crate) struct NativeContract {
    pub(crate) effect: NativeEffect,
    /// May allocate guest-visible heap objects (boxing, `String.valueOf`,
    /// iterator objects). A no-GC worker context must reject `allocates`.
    pub(crate) allocates: bool,
    /// May re-enter guest bytecode (loader/comparator/callback re-entry).
    /// Worker contexts must reject `may_callback`.
    pub(crate) may_callback: bool,
}

const PURE: NativeContract = NativeContract { effect: NativeEffect::Pure, allocates: false, may_callback: false };
const PURE_ALLOC: NativeContract = NativeContract { effect: NativeEffect::Pure, allocates: true, may_callback: false };
const RECV_MUT: NativeContract = NativeContract { effect: NativeEffect::ReceiverMut, allocates: true, may_callback: false };

/// `ACC_ENUM` (JVMS 4.1) — required on the exact class for inherited enum reads.
const ACC_ENUM: u16 = 0x4000;

/// The contract of the native implementation that serves `class.name desc` for a
/// receiver whose EXACT class is `class`. `class_flags` are the loaded class's
/// access flags (`None` when the class has no loadable class file — the pure
/// JDK boundary). Returns `None` for anything without a declared contract.
pub(crate) fn contract_for(
    class: &str,
    name: &str,
    desc: &str,
    class_flags: Option<u16>,
) -> Option<NativeContract> {
    // Inherited enum reads, served by the Enum natives (natives.rs). Mechanical
    // check: the exact receiver class must really BE an enum (ACC_ENUM), or the
    // call is against java/lang/Enum itself. `Enum.ordinal`/`name` are final in
    // the JDK, and a guest body would have been resolved and analysed BEFORE
    // this lookup, so a bytecode override can never reach the contract.
    let is_enum = class == "java/lang/Enum" || class_flags.is_some_and(|f| f & ACC_ENUM != 0);
    if is_enum {
        match (name, desc) {
            ("ordinal", "()I") | ("hashCode", "()I") => return Some(PURE),
            ("name", "()Ljava/lang/String;") | ("toString", "()Ljava/lang/String;") => return Some(PURE_ALLOC),
            ("equals", "(Ljava/lang/Object;)Z") => return Some(PURE),
            ("compareTo", "(Ljava/lang/Enum;)I") | ("compareTo", "(Ljava/lang/Object;)I") => return Some(PURE),
            _ => {}
        }
    }
    match class {
        // The default no-op constructor (natives.rs serves it as a no-op) —
        // every value object's `<init>` calls it via invokespecial.
        "java/lang/Object" => (name == "<init>").then_some(PURE),
        // java.lang.Record's constructor is the same no-op super().
        "java/lang/Record" => (name == "<init>").then_some(PURE),
        // Pure math (natives.rs math intrinsics). No shared state, no guest heap.
        "java/lang/Math" | "java/lang/StrictMath" => Some(PURE),
        // Immutable JDK value classes served natively (natives.rs string/boxed
        // families): reads plus fresh allocations only.
        "java/lang/String" | "java/lang/Integer" | "java/lang/Long" | "java/lang/Short"
        | "java/lang/Byte" | "java/lang/Double" | "java/lang/Float" | "java/lang/Boolean"
        | "java/lang/Character" | "java/lang/Number" => Some(PURE_ALLOC),
        // READ-only accessors of the native map/collection layer (map.rs /
        // natives.rs). Keyed by the CONCRETE classes that layer owns; within an
        // offload region there are no shared writes, so concurrent reads of a
        // shared container are race-free. Iterator/view accessors allocate.
        "java/util/HashMap" | "java/util/LinkedHashMap" | "java/util/TreeMap"
        | "java/util/IdentityHashMap" => match name {
            "get" | "getOrDefault" | "containsKey" | "containsValue" | "size" | "isEmpty" => Some(PURE),
            "entrySet" | "keySet" | "values" => Some(PURE_ALLOC),
            _ => None,
        },
        "java/util/ArrayList" | "java/util/LinkedList" | "java/util/HashSet"
        | "java/util/LinkedHashSet" | "java/util/TreeSet" => match name {
            "get" | "size" | "isEmpty" | "contains" | "indexOf" => Some(PURE),
            "iterator" => Some(PURE_ALLOC),
            _ => None,
        },
        // ⭐**Java 9+ immutable collection factories** — pure factories with NO
        // guest callback. They allocate a fresh immutable Map/List/Set and
        // return it, but do NOT accept a Function/Predicate/Collector. Hence
        // `PURE_ALLOC { may_callback: false }` rather than a filtered Pure:
        // `Map.of("a",1,"b",2)` is the measured blocker in
        // RecipeManager.<init> → NATIVE/unresolved that kept the whole
        // ReloadableServerResources tree offloaded-false.
        "java/util/Map" | "java/util/List" | "java/util/Set" => match name {
            "of" | "ofEntries" | "copyOf" => Some(PURE_ALLOC),
            _ => None,
        },
        // java/lang/Class methods with a provably pure Rust service (reflect.rs
        // `class_method`). `cast` returns its argument, but is now a REAL checked
        // cast: it runs the read-only name-walk subtype test and, on a confident
        // reject, raises ClassCastException through the NATIVE exception
        // constructor — an allocation, no guest re-entry. That is exactly the
        // envelope `Objects.requireNonNull` rides, so it drops from PURE to
        // PURE_ALLOC; the walk itself only reads the loader cache and the class
        // hierarchy. `asSubclass` returns the receiver mirror itself unchanged
        // (`args.first().cloned()`). Both are signature-checked. NO other Class
        // method is admitted: the name/getter family allocates fresh
        // Strings/arrays per call, and `forName`/`newInstance`/`getMethod` run
        // loaders, `<clinit>` or reflective construction (guest re-entry the
        // analysis cannot see).
        "java/lang/Class" => match (name, desc) {
            ("cast", "(Ljava/lang/Object;)Ljava/lang/Object;") => Some(PURE_ALLOC),
            ("asSubclass", "(Ljava/lang/Class;)Ljava/lang/Class;") => Some(PURE),
            _ => None,
        },
        // java/util/Objects null-plumbing served natively (natives.rs:4056):
        // `requireNonNull` is an identity pass-through of arg 0; its null path
        // raises NPE via the NATIVE exception constructor (allocates the
        // throwable, no guest re-entry — the same envelope the admitted
        // Throwable-ctor contract rides). `isNull`/`nonNull` are pure predicates.
        // NOT equals/hashCode/toString: those re-enter guest equals/hashCode.
        "java/util/Objects" => match name {
            "requireNonNull" => Some(PURE_ALLOC),
            "isNull" | "nonNull" => Some(PURE),
            _ => None,
        },
        // READ-only BitSet accessors (natives.rs BitSet thin natives).
        "java/util/BitSet" => matches!(
            name,
            "get" | "nextClearBit" | "nextSetBit" | "previousClearBit" | "previousSetBit"
                | "cardinality" | "length" | "size" | "isEmpty"
        )
        .then_some(PURE),
        _ => None,
    }
}

/// The RECEIVER-MUTATING contract for `class.name` (exact receiver class):
/// natives that mutate ONLY their receiver. Admissible iff the caller proves
/// the receiver private (fresh `new`/owned element) — mutating a private object
/// is unobservable, and a (possibly shared) argument stored INTO the private
/// receiver stays reachable only through it.
pub(crate) fn receiver_mut_contract(class: &str, name: &str) -> Option<NativeContract> {
    let hit = match class {
        "java/util/BitSet" => {
            matches!(name, "set" | "clear" | "flip" | "and" | "or" | "xor" | "andNot")
        }
        "java/util/ArrayList" | "java/util/LinkedList" | "java/util/HashSet"
        | "java/util/LinkedHashSet" | "java/util/TreeSet" => {
            matches!(name, "<init>" | "add" | "set" | "remove" | "clear" | "addAll")
        }
        "java/util/HashMap" | "java/util/LinkedHashMap" | "java/util/TreeMap"
        | "java/util/IdentityHashMap" => {
            matches!(name, "<init>" | "put" | "remove" | "clear" | "putAll" | "putIfAbsent")
        }
        "java/lang/StringBuilder" | "java/lang/StringBuffer" => {
            matches!(name, "<init>" | "append" | "insert" | "setLength" | "deleteCharAt" | "replace")
        }
        _ => false,
    };
    hit.then_some(RECV_MUT)
}

/// GAP-A01 (drill iter 3): the Throwable-family constructor served by
/// `natives.rs::throwable()`. The Rust service for an UNRESOLVABLE `<init>` on a
/// throwable-routed owner stores the first String argument into the receiver's
/// synthetic `$message` field and the first reference argument into `$cause`,
/// and touches NOTHING else — a receiver-mutation contract (RECV_MUT),
/// admissible only on a PRIVATE (fresh `new` / under-construction `this`)
/// receiver. A (possibly shared) message/cause argument stored INTO the private
/// receiver stays reachable only through it (the standard RECV_MUT argument).
///
/// This function checks only the PURE half of the mechanical key:
/// - `name` is `<init>` and `desc` is one of the four canonical Throwable
///   constructor shapes (message/cause combinations);
/// - the class name satisfies the EXACT predicate the runtime native layer uses
///   to route to `throwable()` (natives.rs: `owner == "java/lang/Throwable" ||
///   owner.ends_with("Exception") || owner.ends_with("Error")`).
///
/// The caller (`Vm::private_throwable_ctor_admissible`) must ALSO verify,
/// against the live loader: (1) the exact class resolves NO bytecode for this
/// `<init>` anywhere in the walk (so the runtime can only serve it natively);
/// (2) the class is UNLOADABLE (a JDK-boundary name — no guest class file
/// exists, so no guest bytecode can ever serve any of its methods); (3) the
/// class is provably a `java/lang/Throwable` subtype via the STRICT hierarchy
/// walk (`opcodes::name_is_subtype_strict` — loaded super-chain + the fixed
/// JDK_SUPERS table, never lenient). (2)+(3) close the admission set to the
/// fixed JDK exception table, where an audit of `natives::try_invoke` shows the
/// throwable arm is the FIRST arm that can serve `<init>` for every such name.
/// No guest naming can gain admission: a loadable guest class fails (2) (and a
/// resolvable body is analysed, never trusted), an unknown JDK-boundary name
/// fails (3).
pub(crate) fn throwable_ctor_contract(class: &str, name: &str, desc: &str) -> Option<NativeContract> {
    if name != "<init>" {
        return None;
    }
    let canonical = matches!(
        desc,
        "()V"
            | "(Ljava/lang/String;)V"
            | "(Ljava/lang/Throwable;)V"
            | "(Ljava/lang/String;Ljava/lang/Throwable;)V"
    );
    let served =
        class == "java/lang/Throwable" || class.ends_with("Exception") || class.ends_with("Error");
    (canonical && served).then_some(RECV_MUT)
}

/// Contract check for a WORKER context (compiled fork tree): the native must be
/// pure, allocation-free and callback-free — a worker has no nursery for native
/// allocations outside its proven regions and must never re-enter guest code.
pub(crate) fn worker_pure(class: &str, name: &str, desc: &str, class_flags: Option<u16>) -> bool {
    matches!(
        contract_for(class, name, desc, class_flags),
        Some(NativeContract { effect: NativeEffect::Pure, allocates: false, may_callback: false })
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_without_contract_rejects() {
        assert!(contract_for("some/Guest", "frobnicate", "()V", Some(0x0021)).is_none());
        assert!(contract_for("net/minecraft/world/level/block/Block", "registerDefaultState", "(LBlockState;)V", Some(0x0021)).is_none());
        // The removed guava/fastutil allowlists must stay removed (fail closed).
        assert!(contract_for("com/google/common/cache/LoadingCache", "get", "(Ljava/lang/Object;)Ljava/lang/Object;", Some(0x0601)).is_none());
        assert!(contract_for("it/unimi/dsi/fastutil/objects/Reference2ObjectArrayMap", "get", "(Ljava/lang/Object;)Ljava/lang/Object;", Some(0x0021)).is_none());
    }

    #[test]
    fn enum_reads_require_acc_enum_on_the_exact_class() {
        // A real enum subclass: admitted with the exact inherited signature.
        assert!(contract_for("any/guest/Direction", "ordinal", "()I", Some(0x4031)).is_some());
        assert!(contract_for("any/guest/Direction", "name", "()Ljava/lang/String;", Some(0x4031)).is_some());
        // Same names WITHOUT ACC_ENUM: no contract (a plain class named like an
        // enum read gains nothing — and its own bytecode would be analysed first).
        assert!(contract_for("any/guest/Direction", "ordinal", "()I", Some(0x0021)).is_none());
        // Wrong signature is not the enum read.
        assert!(contract_for("any/guest/Direction", "ordinal", "(I)I", Some(0x4031)).is_none());
        // Unloadable class (no flags) cannot prove it is an enum.
        assert!(contract_for("any/guest/Direction", "ordinal", "()I", None).is_none());
        // java/lang/Enum itself is the declaring JDK boundary.
        assert!(contract_for("java/lang/Enum", "ordinal", "()I", None).is_some());
    }

    #[test]
    fn pure_and_mutating_families_are_separate() {
        assert!(contract_for("java/lang/Object", "<init>", "()V", None).is_some());
        assert!(contract_for("java/lang/Object", "wait", "()V", None).is_none(), "Object.wait is NOT pure");
        assert!(contract_for("java/lang/Math", "round", "(D)J", None).is_some());
        assert!(contract_for("java/lang/String", "valueOf", "(I)Ljava/lang/String;", None).is_some());
        assert!(contract_for("java/util/BitSet", "get", "(I)Z", None).is_some(), "BitSet.get is a read");
        assert!(contract_for("java/util/BitSet", "set", "(I)V", None).is_none(), "BitSet.set MUTATES — not in the pure family");
        assert!(receiver_mut_contract("java/util/BitSet", "set").is_some());
        assert!(receiver_mut_contract("java/util/BitSet", "get").is_none(), "reads are not receiver-mut");
        assert!(contract_for("java/util/HashMap", "put", "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;", None).is_none());
        assert!(receiver_mut_contract("java/util/HashMap", "put").is_some());
        // Interface names are NOT contract keys — only exact concrete classes.
        assert!(contract_for("java/util/Map", "get", "(Ljava/lang/Object;)Ljava/lang/Object;", Some(0x0601)).is_none());
        assert!(receiver_mut_contract("java/util/Map", "put").is_none());
        assert!(contract_for("java/util/List", "get", "(I)Ljava/lang/Object;", Some(0x0601)).is_none());
    }

    #[test]
    fn class_cast_is_a_pure_identity_contract() {
        // reflect.rs serves Class.cast as a CHECKED identity return: the subtype
        // test only reads the loader cache, but a confident reject allocates a
        // ClassCastException through the native constructor ⇒ PURE + allocates,
        // still worker-pure (no guest re-entry). Same shape as requireNonNull.
        let c = contract_for("java/lang/Class", "cast", "(Ljava/lang/Object;)Ljava/lang/Object;", None);
        assert!(matches!(
            c,
            Some(NativeContract { effect: NativeEffect::Pure, allocates: true, may_callback: false })
        ));
        // NOT worker-pure any more, and that is the point: a compiled-fork worker
        // has no nursery for a native allocation, and the reject path allocates a
        // ClassCastException. Fail closed rather than let a worker take a throw
        // path it cannot service.
        assert!(!worker_pure("java/lang/Class", "cast", "(Ljava/lang/Object;)Ljava/lang/Object;", None));
        // asSubclass is the same one-line identity return of the receiver mirror.
        assert!(contract_for("java/lang/Class", "asSubclass", "(Ljava/lang/Class;)Ljava/lang/Class;", None).is_some());
        // Signature-sensitive: a different desc is NOT the served identity method.
        assert!(contract_for("java/lang/Class", "cast", "(Ljava/lang/Object;)V", None).is_none());
        // Every other Class method stays contract-less (allocating getters,
        // loader/clinit re-entry) — fail closed.
        assert!(contract_for("java/lang/Class", "getName", "()Ljava/lang/String;", None).is_none());
        assert!(contract_for("java/lang/Class", "forName", "(Ljava/lang/String;)Ljava/lang/Class;", None).is_none());
        assert!(contract_for("java/lang/Class", "newInstance", "()Ljava/lang/Object;", None).is_none());
        assert!(contract_for("java/lang/Class", "isAssignableFrom", "(Ljava/lang/Class;)Z", None).is_none());
        assert!(receiver_mut_contract("java/lang/Class", "cast").is_none());
    }

    #[test]
    fn throwable_ctor_contract_is_shape_checked() {
        // The canonical message ctor of a JDK exception name: RECV_MUT shape.
        let c = throwable_ctor_contract("java/lang/IllegalArgumentException", "<init>", "(Ljava/lang/String;)V");
        assert!(matches!(c, Some(NativeContract { effect: NativeEffect::ReceiverMut, may_callback: false, .. })));
        // All four canonical ctor descs, and Throwable itself (== match).
        assert!(throwable_ctor_contract("java/lang/Throwable", "<init>", "()V").is_some());
        assert!(throwable_ctor_contract("java/lang/IllegalStateException", "<init>", "(Ljava/lang/Throwable;)V").is_some());
        assert!(throwable_ctor_contract("java/io/IOException", "<init>", "(Ljava/lang/String;Ljava/lang/Throwable;)V").is_some());
        assert!(throwable_ctor_contract("java/lang/OutOfMemoryError", "<init>", "(Ljava/lang/String;)V").is_some());
        // Only <init>: the accessor family is NOT this contract.
        assert!(throwable_ctor_contract("java/lang/IllegalArgumentException", "getMessage", "()Ljava/lang/String;").is_none());
        // Non-canonical desc rejects (the service ignores desc, but the contract
        // stays as tight as the shapes the drill proved).
        assert!(throwable_ctor_contract("java/lang/IllegalArgumentException", "<init>", "(I)V").is_none());
        // A name outside the runtime's throwable-routing predicate rejects.
        assert!(throwable_ctor_contract("java/lang/Foo", "<init>", "()V").is_none());
        // Suffix alone passes the SHAPE check only — the VM-side conditions
        // (unresolvable + unloadable + strict Throwable subtype) are what keep
        // an arbitrary guest "*Exception" name out (see lib.rs tests).
        assert!(throwable_ctor_contract("com/example/AnythingException", "<init>", "()V").is_some());
    }

    #[test]
    fn worker_context_rejects_allocating_and_callback_contracts() {
        assert!(worker_pure("java/lang/Math", "max", "(II)I", None));
        assert!(worker_pure("java/lang/Object", "<init>", "()V", None));
        assert!(!worker_pure("java/lang/String", "valueOf", "(I)Ljava/lang/String;", None), "allocates → not worker-pure");
        assert!(!worker_pure("java/util/HashMap", "entrySet", "()Ljava/util/Set;", None), "allocates a view");
        assert!(!worker_pure("java/util/BitSet", "set", "(I)V", None), "receiver-mut → not worker-pure");
        assert!(worker_pure("java/util/BitSet", "get", "(I)Z", None));
    }
}
