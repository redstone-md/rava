//! Which classes are allocated and served natively.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::opcodes::*;

/// True for a class whose instances must be backed by a HEAP HANDLE (not an
/// arena object) during the next-gen-gc migration: collections and maps store
/// their payload in synthetic `$elems`/`$map` fields that only exist on a heap
/// object. The allocator (`new_object_value`) routes these to `new_object`
/// instead of the TLAB so their native methods keep working untouched.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn is_native_backed_alloc(class: &str) -> bool {
    crate::stream::is_collection(class)
        || crate::map::is_native_map(class)
        || crate::map::is_entry_type(class)
        || is_native_io_class(class)
}

/// BIRTH-ONLY variant of `is_native_backed_alloc` (gated by
/// `RAVA_ARENA_BIRTH_NS_GUARD`, default OFF → identical to it).
///
/// `is_native_backed_alloc`'s collection arm is `stream::is_collection`, a
/// SUBSTRING test with no namespace guard — the exact false-positive class the
/// JIT side already fixed with `native_served_namespace`/`jit_native_served_deep`.
/// At the ALLOCATOR that false positive is not merely a perf loss: an ordinary
/// bytecode class whose name happens to contain "List"/"Set"/"Stack"/… is FORCED
/// to a heap handle forever, so its reference cells never reach the 0b00 arena
/// form the value-model flip needs.
///
/// With the flag on, a class OUTSIDE the native namespaces is judged by the
/// EXACT-NAME arms only (native map / Map$Entry / native I/O — each of which
/// lists only `java/*`, `com/google/common/*` or `it/unimi/*` names anyway, so
/// they can never fire here). Nothing else about the birth predicate moves: the
/// caller still forces a handle for an enum, for an unloadable class, for a
/// partial field layout, and — via `Vm::birth_native_backed_ancestor`, which
/// applies THIS predicate across the whole super chain — for any genuine
/// subclass of a native-backed collection/map/entry/stream type. Not a class
/// list: a namespace rule.
///
/// ⭐**THE ANCESTOR TERM HAS ONE STRUCTURAL EXEMPTION, AND IT LIVES BESIDE THE JIT'S.** A
/// guest subclass whose OWN bytecode implements the value protocol of an ABSTRACT native-backed
/// base — `size()` plus an indexed `(I)<value>` accessor, primitive-only fields, abstract and
/// fieldless loadable ancestors, the Collection mutators all declared in the chain — inherits no
/// synthetic state at all, so forcing it to a handle bought nothing and cost 92% of a bootstrap's
/// handle births (`net/minecraft/world/phys/shapes/CubePointRange`, 2.48 M `Box<Object>`s). The
/// birth sites keep asking `birth_native_backed_ancestor && !overrides_collection_backing`; the
/// second term now answers with `Vm::value_computer_admits` (`vm/native_backing.rs`), the same
/// predicate that completes its layout and serves its arena receivers in `coll_op`. Kill switch
/// `RAVA_NO_VALUECOMPUTER=1`. This function itself is unchanged: the name term never fires for
/// such a class (the exemption is about what it INHERITS, not what it is called).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn birth_native_backed_alloc(class: &str) -> bool {
    // ⭐**BIRTH IS A NAMESPACE QUESTION, NOT AN OVERRIDE QUESTION — use the PREFIX test.**
    //
    // Overriding a `java.util` class moves its METHODS to bytecode, and that is where the win is
    // (a `List`-typed loop went 1707 -> 140 ns/element). It must NOT also change how the object
    // is BORN: every native that touches a collection is written against a handle, and an
    // arena-born `java/util/ArrayList` silently disabled all of them at once —
    // `Collections.reverse`/`swap`/`rotate`/`fill`/`addAll` became no-ops, `unmodifiableList`
    // returned its own argument, and `new LinkedList<>(guestList)` came back empty. Twelve
    // divergent lines in `ArraysSem` alone, all with the same cause.
    //
    // So the class keeps the natives' representation and changes only who executes its methods.
    if crate::nextgen::birth_ns_guard_enabled() && !native_served_namespace(class) {
        return crate::map::is_native_map(class)
            || crate::map::is_entry_type(class)
            || is_native_io_class(class);
    }
    is_native_backed_alloc(class)
}

/// Classes the JIT must NOT resolve to compiled/inlined bytecode because rava serves
/// their instance methods from the native collection/map/stream layer: every
/// native-backed alloc PLUS the guava immutable collections a provider table names
/// (`FACT_GUAVA_IMMUTABLE` — the exact `GUAVA_IMMUTABLE_PROVIDERS` union). Those
/// members aren't all is_native_backed_alloc (ImmutableSortedMap/ImmutableMap carry no
/// "List"/"Set" substring and aren't in MAP_CLASSES), yet their values()/keySet()/
/// iterator()/stream() are native — their own bytecode reads internal fields rava
/// never populates and returns null (e.g. StateDefinition.<init>
/// `propertiesByName.values().iterator()` NPE'd once `values()` was direct-called as
/// bytecode). Keeping them on the interpreter routes the call to the native handler,
/// which is always correct. The guava classes no table names and no earlier arm
/// matches (`ImmutableTable`, the `Regular*`/`Singleton*` concrete impls, Builders)
/// compile: a full boot's served census proved no native ever answered one.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn jit_native_served(class: &str) -> bool {
    is_native_backed_alloc(class)
        // The guava immutable collections a provider table names — EXACT
        // provider-union membership (`FACT_GUAVA_IMMUTABLE`, see owner_bits), never a
        // package/substring shape. The classes only the historic package+contains
        // literal admitted (no table member, no earlier arm — `ImmutableTable`, the
        // `Regular*`/`Singleton*` impls, Builders) were never once served by a
        // native in a full armed boot, so they compile.
        || crate::owner_bits::facts(class) & crate::owner_bits::FACT_GUAVA_IMMUTABLE != 0
}

/// Serialises HEAP-object allocation (self.objects push + new_site_cache) across GIL-free fork
/// workers — those shared structures aren't concurrent. A heap `new` (a class whose super has no
/// loadable bytecode → "partial" layout → not an arena object) is a small fraction of a heavy
/// per-position body (worldgen findBiome), so this lock is amortised while the compute parallelises.
#[cfg(feature = "next-gen-gc")]
pub(crate) static GILFREE_ALLOC_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A memoised `new`-site allocation route (see `alloc_route_memo_enabled`).
/// Only PERMANENT decisions are ever constructed: `Handle` requires the
/// name/enum/ancestor decision (never the transient not-loadable fallback),
/// `Arena` requires a COMPLETE cached layout, and `ArenaMap` mirrors the
/// process-constant RAVA_ARENA_MAPS cut-over.
#[cfg(feature = "next-gen-gc")]
#[derive(Clone)]
pub(crate) enum NewRoute {
    /// The RAVA_ARENA_MAPS flat-table cut-over classes.
    ArenaMap,
    /// Native-backed / enum / native-super → heap handle (`new_object`, which
    /// has its own per-site name cache).
    Handle,
    /// Arena-born: class registered, layout complete — allocate by cached id.
    Arena {
        class: std::sync::Arc<crate::class_loader::LoadedClass>,
        cid: u32,
        nf: u32,
        /// RAVA_PINNED_ENUM routing (flag + layout are both permanent).
        pinned: bool,
    },
}

/// Outcome of a `new` opcode. `Value` = allocated. The other two (next-gen-gc,
/// trampoline mode only) are signals the `new` op turns into a `RunExit` so the
/// DRIVER handles them at the loop safepoint, then re-executes the same `new`:
/// `ClinitPending` = the class needs `<clinit>` first (run it trampolined, so its
/// own allocations can safepoint — Phase 3c), `NurseryFull` = collect then retry.
pub(crate) enum NewSignal {
    Value(Value),
    #[cfg(feature = "next-gen-gc")]
    NurseryFull,
    #[cfg(feature = "next-gen-gc")]
    ClinitPending(std::sync::Arc<crate::class_loader::LoadedClass>),
}

#[derive(Clone)]
pub(crate) enum Plan {
    Concat { recipe: Option<String>, consts: Vec<Value> },
    Lambda { kind: u8, site: std::sync::Arc<crate::LambdaSite> },
    /// `descs[i]` is the FIELD DESCRIPTOR of `fields[i]`, read from the record's
    /// own class file at link time (empty when the field wasn't found there).
    /// Needed by the JVM-semantics hash path: `boolean` components are stored as
    /// `Value::Int` cells yet hash as 1231/1237, so the value alone can't tell.
    Record { kind: String, fields: Vec<String>, descs: std::sync::Arc<[Box<str>]> },
    // Java 21 pattern-matching switch (typeSwitch/enumSwitch): the static args
    // are the case labels (Class type patterns, enum-constant name Strings, or
    // Integer constants). The call (selector, startIndex) returns the index of
    // the first matching label at/after startIndex, -1 if selector is null, or
    // labels.len() if none match.
    Switch { labels: Vec<Value> },
}

/// Assemble a `StringConcatFactory` result. `` pulls the next dynamic
/// argument, `` the next constant; other recipe chars are literal. With
/// no recipe (`makeConcat`) all dynamic args concatenate in order.
pub(crate) fn build_concat(recipe: Option<&str>, dyn_strs: &[String], consts: &[Value]) -> String {
    // ⭐**ONE ALLOCATION, NOT A GROWTH CURVE.** The result was built from `String::new()` — capacity
    // zero — and then grown one `push_str`/`push` at a time, so every concatenation walked the
    // doubling ladder: **was O(log n) reallocations and ~2n bytes RECOPIED per concat, where n is
    // the length of the string being built**; is one allocation sized from operands already in
    // hand. `invokedynamic` string concat is the largest single language feature in this boot's
    // indy census, and this is the whole of its output path.
    //
    // The estimate is the literal recipe plus the dynamic arguments, which is exact for the common
    // shape (`"…"+x+"…"`): the `\u{1}` / `\u{2}` markers it overcounts by one byte each are the
    // only slack, and a `\u{2}` constant longer than its marker is the only way to spill — into
    // ONE growth, from a capacity that is already most of the answer.
    let hint = recipe.map_or(0, str::len) + dyn_strs.iter().map(String::len).sum::<usize>();
    let mut out = String::with_capacity(hint);
    match recipe {
        Some(recipe) => {
            let mut di = 0;
            let mut ci = 0;
            for ch in recipe.chars() {
                match ch {
                    '\u{1}' => { if let Some(s) = dyn_strs.get(di) { out.push_str(s); } di += 1; }
                    '\u{2}' => { if let Some(v) = consts.get(ci) { out.push_str(&v.display()); } ci += 1; }
                    c => out.push(c),
                }
            }
        }
        None => {
            for s in dyn_strs {
                out.push_str(s);
            }
        }
    }
    out
}
