//! Which stream/collection family serves a call.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::stream::*;

/// How far a generated stream will walk before it refuses. A bounded `iterate` whose
/// predicate never goes false, or a `limit` larger than this, is a guest bug or a shape
/// this engine cannot serve — either way, reporting beats running forever.
pub(crate) const GENERATOR_CAP: usize = 1 << 20;

/// Kill switch for this round's dispatcher gate: `RAVA_NO_FAST_STREAMDISP=1` (or the blanket
/// `RAVA_NO_EMIT_MORE`) puts every call back on the full walk — the argument rooting first, then
/// the owner literals in order, then `is_collection`. Read once per process.
#[inline]
pub(crate) fn fast_streamdisp() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_STREAMDISP").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// The nineteen method names this dispatcher can serve WITHOUT looking at the owner: the four
/// `Iterable` defaults plus the fused-terminal set of a deferred (`$ops`) stream.
///
/// ⭐**ONE KEY, THREE QUESTIONS.** `dispatch` asked the name three separate times — a four-name
/// `matches!` for the Iterable defaults, a nineteen-name `matches!` for the lazy terminals, and a
/// `name == "forEach"` — and it is entered by every `java.util`, `java.io` and `com.google.common`
/// native in the process (the `owner_bits` stage gate admits all three prefixes wholesale), which
/// is nineteen million calls a boot. The key is derived once and every later test is an integer
/// compare.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SName {
    Other,
    // The `Iterable` defaults — served on ANY receiver, whatever the owner.
    Spliterator,
    Stream,
    Iterator,
    ForEach,
    // The rest of the deferred-stream terminal set.
    ForEachOrdered,
    Close,
    Count,
    FindFirst,
    FindAny,
    AnyMatch,
    AllMatch,
    NoneMatch,
    Sequential,
    Parallel,
    Unordered,
    OnClose,
    Boxed,
    AsLongStream,
    AsDoubleStream,
}

/// `(length, first byte)` leaves at most TWO candidates for any of the nineteen, so the answer
/// costs a switch and at most two `memcmp`s — where the three `matches!` chains it replaces cost
/// a length switch and up to four each, three times over. No table, no hash, nothing to probe:
/// the discriminants are bytes the caller already has in registers.
///
/// **This is a faster way to compute the same predicate, not a new one** — `name_key_matches_the_old_chain`
/// below re-derives both `matches!` sets from it over a corpus, so the two cannot drift.
/// `Map.forEach` is recognised by its `BiConsumer` descriptor (default ON); `RAVA_NO_FOREACH_BY_DESC=1`
/// restores the shape-only test so the change prices as a same-binary A/B.
#[inline]
fn foreach_by_desc() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_FOREACH_BY_DESC").is_none())
}

#[inline]
fn name_key(n: &str) -> SName {
    use SName::*;
    let b = n.as_bytes();
    let Some(&b0) = b.first() else { return Other };
    match (b.len(), b0) {
        (5, b'c') => {
            if n == "close" {
                Close
            } else if n == "count" {
                Count
            } else {
                Other
            }
        }
        (5, b'b') => if n == "boxed" { Boxed } else { Other },
        (6, b's') => if n == "stream" { Stream } else { Other },
        (7, b'f') => {
            if n == "forEach" {
                ForEach
            } else if n == "findAny" {
                FindAny
            } else {
                Other
            }
        }
        (7, b'o') => if n == "onClose" { OnClose } else { Other },
        (8, b'i') => if n == "iterator" { Iterator } else { Other },
        (8, b'a') => {
            if n == "anyMatch" {
                AnyMatch
            } else if n == "allMatch" {
                AllMatch
            } else {
                Other
            }
        }
        (8, b'p') => if n == "parallel" { Parallel } else { Other },
        (9, b'f') => if n == "findFirst" { FindFirst } else { Other },
        (9, b'n') => if n == "noneMatch" { NoneMatch } else { Other },
        (9, b'u') => if n == "unordered" { Unordered } else { Other },
        (10, b's') => if n == "sequential" { Sequential } else { Other },
        (11, b's') => if n == "spliterator" { Spliterator } else { Other },
        (12, b'a') => if n == "asLongStream" { AsLongStream } else { Other },
        (14, b'f') => if n == "forEachOrdered" { ForEachOrdered } else { Other },
        (14, b'a') => if n == "asDoubleStream" { AsDoubleStream } else { Other },
        _ => Other,
    }
}

/// EVERY owner literal the `match owner` at the bottom of `dispatch` can match — the single
/// definition the length mask is derived from, so an arm added there without a row here is
/// caught by `owner_len_mask_covers_every_arm` rather than by a missing native at runtime.
/// `pub(crate)` so [`crate::map::GUAVA_IMMUTABLE_PROVIDERS`] can take its Immutable rows from the
/// one place they are written (the drift test in `map/classes.rs` re-derives that containment).
pub(crate) const OWNER_LITERALS: [&str; 26] = [
    "java/util/stream/Stream",
    "java/util/stream/IntStream",
    "java/util/stream/LongStream",
    "java/util/stream/Stream$Builder",
    "java/util/stream/IntStream$Builder",
    "java/util/stream/LongStream$Builder",
    "java/util/stream/DoubleStream$Builder",
    "java/util/stream/Collectors",
    "com/google/common/collect/ImmutableMap",
    "com/google/common/collect/ImmutableBiMap",
    "java/util/stream/Collector",
    "java/util/stream/StreamSupport",
    "java/util/Arrays",
    "java/util/Collections",
    "com/google/common/collect/Sets",
    "java/io/BufferedReader",
    "java/io/InputStreamReader",
    "java/util/List",
    "java/util/Set",
    "java/util/Iterator",
    "java/util/ListIterator",
    "java/util/Spliterator",
    "java/util/Optional",
    "java/util/OptionalInt",
    "java/util/OptionalLong",
    "java/util/OptionalDouble",
];

/// Bit `n` is set when some owner literal is `n` bytes long.
const fn owner_len_mask() -> u64 {
    let mut m = 0u64;
    let mut i = 0;
    while i < OWNER_LITERALS.len() {
        let l = OWNER_LITERALS[i].len();
        // Every literal is far short of 64 bytes. The assert is here so a future arm that is
        // not cannot silently drop out of the mask and take its native with it.
        assert!(l < 64);
        m |= 1u64 << l;
        i += 1;
    }
    m
}
const OWNER_LEN_MASK: u64 = owner_len_mask();

/// A SUPERSET of "some literal arm of the `match owner` matches this owner": every literal's
/// length is a bit in `OWNER_LEN_MASK` and every literal begins `j` or `c`, so a `false` here
/// PROVES no literal arm can fire. May only ever over-admit — an over-admitted owner walks the
/// same arms in the same order it always did.
#[inline]
fn owner_may_match(o: &str) -> bool {
    let b = o.as_bytes();
    b.len() < 64
        && (OWNER_LEN_MASK >> b.len()) & 1 != 0
        && matches!(b.first(), Some(b'j' | b'c'))
}

pub(crate) fn dispatch(vm: &mut Vm, owner: &str, name: &str, descriptor: &str, args: &[Value]) -> Handled {
    // Iterable/Collection default methods on ANY iterable Ref receiver — reached
    // here only when normal resolution failed (e.g. a Minecraft Registry whose
    // Iterable.spliterator()/stream() default lives in the JDK, not the jar).
    // Drain the receiver generically. (Map.forEach is handled earlier by
    // map::dispatch, so a forEach reaching here is a single-arg Collection one.)
    // A Map (has $map) must NOT be drained as a Collection here — its forEach is
    // a 2-arg BiConsumer and its iteration is key/value pairs. Let map::dispatch
    // handle it. (stream::dispatch runs before map::dispatch in try_invoke.)
    // Only the Iterable-default fast paths below consult this, and they all gate
    // on the method name first — so skip the per-call `$map` field probe for the
    // (overwhelming majority of) other native names that reach this dispatcher.
    // ── THE DECLINE IS THE COMMON CASE, AND IT USED TO PAY FOR THE WHOLE HIT PATH FIRST ──────
    // (RAVA_NO_FAST_STREAMDISP=1 opts out.) The `owner_bits` stage gate admits EVERY
    // `java/util/*`, `java/io/*` and `com/google/common/*` owner to this dispatcher, so most of
    // what arrives — `Objects.equals`, `Random.nextInt`, a guava builder, an `InputStream` read —
    // has no arm here at all and leaves through the `_ =>` at the bottom. Getting there cost, in
    // order: a clone and a `gc_protect` push per argument (plus the truncate on the way out), a
    // four-name `matches!`, a four-literal `matches!` on the owner, a nineteen-name `matches!`,
    // and then the owner literals of the final match. All of it to answer "no".
    //
    // The two facts that decide the whole call are cheaper than any one of those: the name's key,
    // and whether the owner's LENGTH is one any literal arm has. Ask them first.
    // ⭐**THE ONE ATOMIC LOAD FOR THE WHOLE CALL** (`RAVA_NO_MS_GATEWORD=1` restores the per-site
    // loads). `stream_gates` in the parent module carries the count and the population: this
    // dispatcher is entered by every `java/util/*`, `java/io/*` and `com/google/common/*` native in
    // the process — nineteen million calls a boot — and it was reading `fast_streamdisp`,
    // `lazy_streams_enabled` (TWICE on a `Stream` pipeline op), `fast_streams_enabled`,
    // `fork_shapes` and `arena_map::enabled` as separate acquire loads. `forEach` on a collection
    // went 4 → 1, an `Iterable` default 2-3 → 1, a `Stream` op 3 → 1. Everything below is a bit
    // test on `g`, which lives in a register.
    let k = name_key(name);
    let g = crate::stream::stream_gates();
    let fast = sg_fastdisp(g);
    let may_lit = !fast || owner_may_match(owner);
    // `o if is_collection(o)` is the ONE owner test in the match that is not a literal, so the
    // gate has to ask it — and asks it exactly once. `may_lit` is true for every literal arm
    // above that one, so a `false` there means the collection arm is the only owner arm left;
    // the arm itself then reads this answer rather than probing the `owner_bits` memo a second
    // time for `java/util/ArrayList`, which is the hottest owner that reaches here.
    let coll = if may_lit { None } else { Some(is_collection(owner)) };
    // NOTHING BELOW CAN FIRE. No owner arm matches, and the name keys none of the owner-agnostic
    // Iterable/terminal arms — so the historic route's only possible answer is `Ok(None)`.
    // Returning HERE is what makes the argument rooting conditional: nothing on this path
    // allocates or re-enters the interpreter, so no collection can observe those roots.
    if fast && k == SName::Other && !may_lit && coll == Some(false) {
        return Ok(None);
    }
    // Root ALL arguments (source collection, mapper/accumulator/comparator lambdas, …)
    // for the whole dispatch: the hot ops re-enter the interpreter per element and now run
    // a mid-sweep GC (stream_maybe_gc), so any arg held only in a Rust local — notably the
    // lambda objects — would be swept between applies. Cheap: pushes a handful of Values —
    // and now paid only by a call that can still reach one of those ops.
    let _gpargs = {
        let base = crate::vm::thread_context::root_len(&vm.gc_protect);
        // ⭐**ONLY A REFERENCE CAN BE SWEPT OR MOVED, SO ONLY A REFERENCE IS PUSHED.** The loop
        // cloned EVERY argument. A scalar bought a `Vec` push the root walk will never look at;
        // a `Value::Str` bought TWO ATOMIC READ-MODIFY-WRITES — the `Arc<String>` increment on the
        // clone and the matching decrement when `GpScope` truncates — for a string the collector
        // does not own (see `is_gc_rootable`). The caller's `args` slice holds that `Arc` live for
        // the whole call, so the clone existed only to be dropped. `opcodes::statics`'s call-root
        // guard already filters with this exact predicate on the hot invoke path. Nothing reads
        // these slots by index; `GpScope` only truncates.
        for a in args {
            if crate::stream::is_gc_rootable(a) {
                crate::vm::thread_context::push_root(&mut vm.gc_protect, a.clone());
            }
        }
        GpScope(&mut vm.gc_protect as *mut _, base)
    };
    let needs_iter = matches!(
        k,
        SName::Spliterator | SName::Stream | SName::Iterator | SName::ForEach
    );
    // A Map receiver must NOT be drained as a generic Iterable here — its
    // `forEach`/iteration belongs to `map::dispatch` (a `BiConsumer(k,v)`, not a
    // single-arg `Consumer` over elements). This covers a heap `$map` handle AND —
    // under RAVA_ARENA_MAPS — an arena map (`Value::Obj`). Missing the arena case
    // let `Map.forEach` on an arena HashMap fall into the generic `forEach` below,
    // which ran `iterate_recv` (the Collection contract) over it and drained
    // nothing — so e.g. FallbackResourceManager.listResources' resultMap.forEach
    // copied ZERO entries into the result, leaving every data-driven registry empty.
    //
    // ⭐**AND THE DESCRIPTOR SAYS SO BEFORE ANY SHAPE PROBE DOES.** Both tests above ask what
    // the receiver IS on this engine (a `$map` handle, a rava arena map), so a map whose state
    // lives in its own bytecode fields — fastutil's `Reference2ObjectArrayMap`, `key[]` /
    // `value[]` / `size` — matched neither, fell into the generic Iterable drain below,
    // `iterate_recv` found no `$elems`, and `forEach` visited NOTHING, silently. The call site
    // itself already says which `forEach` it is: `Map.forEach` takes a `BiConsumer` and
    // `Iterable.forEach` a `Consumer`, and no `Iterable.forEach` anywhere takes a `BiConsumer`.
    // A `BiConsumer` descriptor is therefore a map iteration whatever the receiver's
    // representation, and this dispatcher must decline it so the receiver's own bytecode (the
    // `Reference2ObjectMap.forEach` default over `reference2ObjectEntrySet()`) runs.
    // `DataComponentPatch.split()` was the visible casualty (bench/CompSem.java `split.*`: an
    // empty result from a three-entry patch). Kill switch `RAVA_NO_FOREACH_BY_DESC=1`.
    let is_map_recv = needs_iter
        && (match args.first() {
            Some(Value::Ref(h)) => matches!(vm.heap.get_field_syn(*h, crate::heap::Syn::Map), Value::Int(_)),
            #[cfg(feature = "next-gen-gc")]
            Some(Value::Obj(p)) => sg_arena_map(g) && crate::arena_map::is_arena_map(*p),
            _ => false,
        } || (k == SName::ForEach && foreach_by_desc() && descriptor.starts_with("(Ljava/util/function/BiConsumer;")));
    // An Optional is NOT an Iterable: its stream()/iterator() must yield its one
    // value (or nothing), not drain a (non-existent) $elems. Let optional_op
    // handle it — else Optional.stream() was always an empty stream, which made
    // `xs.flatMap(Optional::stream)` drop everything (e.g. tag loading).
    // Gated on `needs_iter` for the same reason the `$map` probe above is: the ONE reader of this
    // answer additionally requires the name to be `spliterator`/`stream`/`iterator`, a strict
    // subset of `needs_iter`, so for every other name a `false` here is not merely cheap but
    // unobserved. Was four owner `memcmp`s on every call into this dispatcher; is none.
    let is_optional = needs_iter
        && matches!(owner,
            "java/util/Optional" | "java/util/OptionalInt" | "java/util/OptionalLong" | "java/util/OptionalDouble");
    // A lazy (`$ops`) stream receiver has no `$elems`, so the generic Iterable
    // fast-paths below (which drain via iterate_recv) would see it as empty. Route
    // it to stream_op, which materialises the fused chain. (Only when the flag is on.)
    // `recv_is_lazy_stream` costs a by-NAME field probe (`$ops`), and `dispatch` is entered by
    // every `java.util` native call in the process — nineteen million a boot. Only the names
    // below ever read the answer, so ask the question only for them; for anything else a `false`
    // is not merely cheap but unobserved. (Same shape as `needs_iter` gating the `$map` probe.)
    // `SName` IS that nineteen-name set — `Other` is exactly "no arm here reads the name" — so the
    // `matches!` chain that stood here is now the key computed at the top of the call.
    let recv_lazy = k != SName::Other && recv_is_lazy_stream(vm, g, args.first());
    // ── FUSED TERMINALS (RAVA_NO_FAST_STREAMS=1 opts out) ─────────────────────────────
    // A deferred chain reaching a terminal that only READS does not need the chain's output
    // list. `fuse_drive` runs the map/filter/peek stages in one pass and hands each surviving
    // element straight to the terminal, so `filter(p).count()` allocates nothing at all and
    // `map(f).forEach(c)` allocates neither the mapped array nor the `Vec` decoded from it.
    //
    // ⭐It is also where SHORT-CIRCUITING becomes possible. The eager path had already
    // materialised every element before `findFirst`/`anyMatch` looked at the first one, so the
    // mapper ran over the whole source where a real JVM applies it to exactly one — a
    // different observable sequence of calls, not just a slower one. `allMatch`/`noneMatch`
    // did not stop at their deciding element either. Each of the three has one outcome that
    // decides it, and the pass ends there.
    //
    // Guarded by `recv_is_lazy_stream`, which proves the receiver is one of OUR `$ops`
    // streams; an eager stream, a generator, or a guest object falls through untouched.
    if recv_lazy {
        if let Some(Value::Ref(h)) = args.first() {
            let h = *h;
            match name {
                // Our streams hold no resources and carry no element type, so these are
                // identities over the receiver — and `close()` in particular MUST NOT
                // materialise: a JDK `close()` never executes the pipeline, and routing it
                // through the eager body would have run every deferred mapper for its
                // side effects and then thrown the result away.
                "close" => return Ok(Some(None)),
                "sequential" | "parallel" | "unordered" | "onClose" | "boxed"
                | "asLongStream" | "asDoubleStream" => return Ok(Some(Some(Value::Ref(h)))),
                "count" => return Ok(Some(Some(Value::Long(fuse_count(vm, h)?)))),
                "forEach" | "forEachOrdered" => {
                    // Root the consumer: an arena-lambda MOVES under each element's guest GC,
                    // so re-read the FORWARDED function per element rather than binding it once.
                    with_rooted_fn(vm, args.get(1).cloned().unwrap_or(Value::Null), |vm, fs| {
                        fuse_drive(vm, h, |vm, e| {
                            let f = rooted_fn(vm, fs);
                            vm.apply(&f, vec![e])?;
                            Ok(std::ops::ControlFlow::Continue(()))
                        })
                    })?;
                    return Ok(Some(None));
                }
                "findFirst" | "findAny" => {
                    // The winner rides in a rooted 1-slot heap array: `make_optional`
                    // allocates, and the pass that produced the element may have moved it.
                    let slot = vm.heap.alloc_values(OBJ, vec![Value::Null]);
                    let _gp = gc_protect_array(vm, slot);
                    let mut found = false;
                    fuse_drive(vm, h, |vm, e| {
                        found = true;
                        vm.heap.array_set(slot, 0, e);
                        Ok(std::ops::ControlFlow::Break(()))
                    })?;
                    let v = if found { vm.heap.array_get(slot, 0) } else { None };
                    return Ok(Some(Some(make_optional(vm, v))));
                }
                "anyMatch" | "allMatch" | "noneMatch" => {
                    let (mut hit, mut miss) = (false, false);
                    with_rooted_fn(vm, args.get(1).cloned().unwrap_or(Value::Null), |vm, fs| {
                        fuse_drive(vm, h, |vm, e| {
                            let f = rooted_fn(vm, fs);
                            if truthy(&vm.apply(&f, vec![e])?) { hit = true } else { miss = true }
                            // `allMatch` is decided by the first rejection, the other two by
                            // the first acceptance. Empty falls out correctly: nothing ran, so
                            // `allMatch`/`noneMatch` are true and `anyMatch` is false.
                            let decided = if name == "allMatch" { miss } else { hit };
                            Ok(if decided {
                                std::ops::ControlFlow::Break(())
                            } else {
                                std::ops::ControlFlow::Continue(())
                            })
                        })
                    })?;
                    let r = match name {
                        "anyMatch" => hit,
                        "allMatch" => !miss,
                        _ => !hit,
                    };
                    return Ok(Some(Some(Value::Int(r as i32))));
                }
                _ => {}
            }
        }
    }
    if !is_map_recv
        && !is_optional
        && !recv_lazy
        && matches!(k, SName::Spliterator | SName::Stream | SName::Iterator)
    {
        // Receiver may be a heap handle OR an arena object (a Minecraft registry is
        // a bytecode class → arena `Value::Obj`, whose `Iterable.spliterator()`/
        // `stream()` defaults have no jar bytecode and reach here). `iterate_recv`
        // drains either generically.
        if let Some(recv @ (Value::Ref(_) | Value::Obj(_))) = args.first() {
            let recv = recv.clone();
            // ⛔**COPYING THE ARRAY DIRECTLY MEASURED EXACTLY ZERO, AND THAT IS THE FINDING.**
            // The generic path below runs `iterate_recv` (decode every cell into a `Vec<Value>`)
            // and then `iterator_of` (re-encode every one into a fresh array) — two passes and
            // two allocations for a copy the raw cells already are. A `Heap::clone_array`
            // shortcut was built for exactly the receivers where it is sound (class
            // `java/util/ArrayList` with a native `$elems`, so snapshot semantics are unchanged;
            // `iterate_recv` also orders sorted sets and drains guest `Iterable`s, which it must
            // not claim). It fired **780,000 times** on `bench/StreamApplyCost.java` — verified,
            // not assumed — and the probe did not move:
            //   clone ON   2920 3075 3155 ns/element      clone OFF  3266 3040 3130
            // ⭐So the 2,064 ns `bridgestat` charges `List#iterator` is NOT the copy. It is the
            // CROSSING: `List.iterator` is not admitted to `fu_fastpath`, so every call builds a
            // synthetic frame. `RAVA_ITER_INTRINSIC=1`, which admits it, moves the same probe
            // 2861-2965 -> 2337-2533 ns/element (-17%). Reverted; the number is the point.
            // `iterate_recv` -> `iterate_object` already puts a sorted set in its
            // own order, so this path needs nothing beyond the drain.
            let elems = vm.iterate_recv(&recv)?;
            let v = match name {
                "stream" => make_stream(vm, elems),
                "iterator" => {
                    // Record the backing collection so Iterator.remove() can delete
                    // from it. This generic fast-path runs BEFORE coll_op's own
                    // iterator() arm, so without this the iterator had no $source
                    // and remove() was a silent no-op — which made MC's advancement
                    // topological sort (`while(!map.isEmpty()){ it=map.entrySet()
                    // .iterator(); ...; it.remove(); }`) loop forever, re-building a
                    // fresh Advancement each pass and ballooning the root Set to 100k+.
                    let it = iterator_of(vm, elems);
                    if let Value::Ref(ih) = it {
                        vm.heap.put_field_syn(ih, crate::heap::Syn::Source, recv.clone());
                        // FAIL-FAST WITNESS. ⚠This generic path is where MOST iterators come
                        // from — it runs BEFORE `coll_op`'s arm, as the note above says — so
                        // stamping only the other two creation sites left the
                        // ConcurrentModificationException check dead for nearly every loop
                        // in the guest. Same shape as the shared-read fast path that served
                        // zero of eighty million calls: instrument WHICH site the traffic
                        // uses before believing a stamp is in place.
                        if let Value::Ref(sh) = recv {
                            if let Some(n) = crate::stream::collections::coll_op::source_len(vm, sh) {
                                vm.heap.put_field_syn(ih, crate::heap::Syn::ExpSize, Value::Int(n as i32));
                            }
                        }
                        // The iterator carries its source's contract — this generic
                        // path runs BEFORE `coll_op`'s own iterator arm, so leaving
                        // it out here left most iterations unguarded.
                        if let Value::Ref(sh) = recv {
                            crate::immutable::inherit(vm, sh, &it);
                        }
                    }
                    it
                }
                _ => {
                    let arr = vm.heap.alloc_values(OBJ, elems);
                    let sp = vm.heap.alloc("java/util/Spliterator");
                    vm.heap.put_field_syn(sp, crate::heap::Syn::Elems, Value::Array(arr));
                    Value::Ref(sp)
                }
            };
            return Ok(Some(Some(v)));
        }
    }
    if !is_map_recv && !recv_lazy && k == SName::ForEach {
        if let (Some(recv @ (Value::Ref(_) | Value::Obj(_))), Some(f)) = (args.first(), args.get(1)) {
            let recv = recv.clone();
            let f = f.clone();
            // ⭐**A TERMINAL THAT ONLY ITERATES DOES NOT NEED A MATERIALISED SOURCE COPY**
            // (RAVA_NO_FAST_STREAMS=1 opts out). Below, `iterate_recv` decodes every cell into
            // a `Vec<Value>` and `alloc_values` re-encodes every one into a fresh heap array
            // whose ONLY job is to be a GC root — two O(n) copies and two allocations, on the
            // arm where a guest `forEach` actually lands. The receiver is already a root (every
            // argument is, at the top of this function), so its own `$elems` array is rooted
            // too; `direct_elems` hands it over exactly when the drain would have answered that
            // array verbatim, and refuses (views, sorted collections, bytecode-drained
            // Collections, arena receivers) otherwise. Walking the live array is also the
            // JDK's shape: `ArrayList.forEach` binds `elementData` and re-checks modCount as it
            // goes, rather than iterating a private copy.
            if let Some((arr, n)) = direct_elems(vm, g, &recv) {
                // `before` is read HERE and not hoisted above the fallback's `iterate_recv`:
                // that drain can run a jar collection's own `iterator()` bytecode, so a length
                // taken ahead of it would be comparing against a different moment.
                let before = match &recv {
                    Value::Ref(h) => crate::stream::collections::coll_op::source_len(vm, *h),
                    _ => None,
                };
                // ROOT THE BORROWED ARRAY IN ITS OWN RIGHT. Reachability through the receiver
                // is not enough: a consumer that bulk-writes the collection (`Collections
                // .reverse`, `sort`, `addAll`) makes `store_elems` REPLACE `$elems` with a new
                // array, and the one we are walking would then be unreachable and sweepable
                // mid-loop. Rooted, the walk finishes over the array it started on — exactly
                // what the copy below did — and the length check still reports the mutation.
                let _gpa = gc_protect_array(vm, arr);
                #[cfg(feature = "next-gen-gc")]
                if n >= 2 && sg_fork(g) {
                    vm.fork_each_probe(&f, arr, n);
                }
                with_rooted_fn(vm, f, |vm, fs| {
                    for i in 0..n {
                        // FAIL-FAST, PER ELEMENT — and it has to be per element here. The copy
                        // path below can afford one check after the loop because it walks a
                        // private snapshot; this walk reads the LIVE cells, so a consumer that
                        // shortens the list must stop the iteration rather than read past the
                        // end and hand the consumer a `null`. `ArrayList.forEach` compares
                        // modCount on every step for the same reason.
                        if matches!(&recv, Value::Ref(h)
                            if crate::stream::collections::coll_op::source_len(vm, *h) != before)
                        {
                            return Err(VmError::Throw(vm.make_exception(
                                "java/util/ConcurrentModificationException", "")));
                        }
                        let e = vm.heap.array_get(arr, i).unwrap_or(Value::Null);
                        let f = rooted_fn(vm, fs);
                        vm.apply(&f, vec![e])?;
                    }
                    Ok::<(), VmError>(())
                })?;
                if before.is_some()
                    && matches!(&recv, Value::Ref(h)
                        if crate::stream::collections::coll_op::source_len(vm, *h) != before)
                {
                    return Err(VmError::Throw(
                        vm.make_exception("java/util/ConcurrentModificationException", ""),
                    ));
                }
                return Ok(Some(None));
            }
            let elems = vm.iterate_recv(&recv)?;
            // Each `apply` can run heavy guest code — e.g. a RegistryDataLoader.Loader
            // decoding a registry's JSON — which triggers a MOVING arena collection.
            // A bare Rust Vec of arena objects is no GC root, so the not-yet-processed
            // elements would dangle to a reused young slot (a Loader element re-read as
            // a freshly-decoded DataResult → `loadFromResources` on a DataResult$Success
            // → UnsatisfiedLinkError, aborting datapack/registry load). Materialise into
            // a HEAP array first: the collector forwards heap ref-array cells in place,
            // so each `array_get` reads the FORWARDED element. (Same discipline the
            // arena-map `forEach` uses.)
            // FAIL-FAST. `ArrayList.forEach` compares modCount AFTER the loop, so a body
            // that adds to the list it is walking throws — and this generic arm, not
            // `coll_op`'s, is where a guest `forEach` actually lands.
            let before = match &recv {
                Value::Ref(h) => crate::stream::collections::coll_op::source_len(vm, *h),
                _ => None,
            };
            let arr = vm.heap.alloc_values("Ljava/lang/Object;", elems);
            let n = vm.heap.array_len(arr).unwrap_or(0);
            // M3 F3.1: report whether this forEach is a forkable element-ownership map (still
            // runs serially below — byte-identical). Gated; validates detection on a live boot.
            #[cfg(feature = "next-gen-gc")]
            if n >= 2 && sg_fork(g) {
                vm.fork_each_probe(&f, arr, n);
            }
            // Root the consumer `f`: an arena-lambda consumer MOVES under each
            // element's guest GC, so re-read the FORWARDED function per iteration.
            with_rooted_fn(vm, f, |vm, fs| {
                for i in 0..n {
                    let e = vm.heap.array_get(arr, i).unwrap_or(Value::Null);
                    let f = rooted_fn(vm, fs);
                    vm.apply(&f, vec![e])?;
                }
                Ok::<(), VmError>(())
            })?;
            if before.is_some()
                && matches!(&recv, Value::Ref(h)
                    if crate::stream::collections::coll_op::source_len(vm, *h) != before)
            {
                return Err(VmError::Throw(
                    vm.make_exception("java/util/ConcurrentModificationException", ""),
                ));
            }
            return Ok(Some(None));
        }
    }
    match owner {
        "java/util/stream/Stream" | "java/util/stream/IntStream" | "java/util/stream/LongStream" => {
            if name == "of" {
                return Ok(Some(Some(stream_of(vm, args))));
            }
            if name == "range" || name == "rangeClosed" {
                let s = args.first().and_then(Value::as_int).unwrap_or(0);
                let e = args.get(1).and_then(Value::as_int).unwrap_or(0);
                let end = if name == "rangeClosed" { e + 1 } else { e };
                let elems: Vec<Value> = (s..end).map(Value::Int).collect();
                return Ok(Some(Some(make_stream(vm, elems))));
            }
            if name == "empty" {
                return Ok(Some(Some(make_stream(vm, Vec::new()))));
            }
            if name == "builder" {
                let h = vm.heap.alloc("java/util/stream/Stream$Builder");
                let arr = vm.heap.alloc_values(OBJ, Vec::new());
                vm.heap.put_field_syn(h, crate::heap::Syn::Elems, Value::Array(arr));
                return Ok(Some(Some(Value::Ref(h))));
            }
            // GENERATORS. `Stream.iterate(seed, f)` and `Stream.generate(sup)` are
            // INFINITE, and this engine materialises a stream into `$elems` — so there is
            // nothing to materialise until something bounds them. Both were absent
            // entirely (`UnsatisfiedLinkError`).
            //
            // The three-argument `iterate(seed, hasNext, next)` IS bounded, so it is
            // generated here and behaves like any other stream afterwards.
            //
            // The two-argument forms record the generator on the stream and produce no
            // elements; `limit(n)` below is what turns them into values. Anything else
            // applied to an unbounded generator would run forever on a real JVM too, and
            // this engine says so instead of hanging or inventing a bound.
            if name == "iterate" && args.len() >= 3 {
                let mut cur = args.first().cloned().unwrap_or(Value::Null);
                let pred = args.get(1).cloned().unwrap_or(Value::Null);
                let next = args.get(2).cloned().unwrap_or(Value::Null);
                let mut out = Vec::new();
                // A bounded iterate is still guest-controlled; cap the walk so a predicate
                // that never goes false reports a limitation rather than wedging the VM.
                for _ in 0..GENERATOR_CAP {
                    let keep = vm.apply(&pred, vec![cur.clone()])?.and_then(|v| v.as_int()).unwrap_or(0);
                    if keep == 0 {
                        break;
                    }
                    out.push(cur.clone());
                    cur = vm.apply(&next, vec![cur])?.unwrap_or(Value::Null);
                }
                return Ok(Some(Some(make_stream(vm, out))));
            }
            if (name == "iterate" && args.len() == 2) || name == "generate" {
                let st = make_stream(vm, Vec::new());
                if let Value::Ref(h) = st {
                    vm.heap.put_field(h, "$genkind", Value::Str(name.into()));
                    if name == "iterate" {
                        vm.heap.put_field(h, "$genseed", args.first().cloned().unwrap_or(Value::Null));
                        vm.heap.put_field(h, "$genfn", args.get(1).cloned().unwrap_or(Value::Null));
                    } else {
                        vm.heap.put_field(h, "$genfn", args.first().cloned().unwrap_or(Value::Null));
                    }
                }
                return Ok(Some(Some(st)));
            }
            if name == "concat" {
                // `stream_elems`, not `elems_of`: an operand may be a DEFERRED stream, and
                // materialising it runs its mappers — which can throw. `elems_of` swallows
                // that into an empty vector, so a failing operand would have concatenated
                // silently as nothing instead of propagating.
                let mut e = stream_elems(vm, args.first())?;
                e.extend(stream_elems(vm, args.get(1))?);
                return Ok(Some(Some(make_stream(vm, e))));
            }
            // A GENERATOR RECEIVER MUST NOT DEFER. `stream_op`'s deferral arm runs BEFORE its
            // unbounded-generator arm, so with fusion on, `Stream.generate(f).map(g)` would
            // record a deferred `map` over the generator's (empty) `$elems` and every later
            // terminal would answer EMPTY — trading today's loud refusal for the quiet kind of
            // wrong answer. Refuse here instead, with the message `stream_op` gives, so the two
            // paths cannot disagree about an unbounded stream. One field probe, and only on a
            // deferrable name.
            // The SECOND read of `lazy_streams_enabled` in this call, and LLVM could not fold it
            // into the first (`recv_is_lazy_stream`'s) — an acquire load is a barrier, not a `mov`.
            // Now both are bit tests on the same byte.
            if sg_lazy(g) && lazy_defer_kind(name).is_some() {
                if let Some(Value::Ref(rh)) = args.first() {
                    if let Value::Str(kind) = vm.heap.get_field(*rh, "$genkind") {
                        let kind = kind.to_string();
                        let e = vm.make_exception(
                            "java/lang/UnsupportedOperationException",
                            &format!("{name} on an unbounded Stream.{kind} — bound it with limit(n) first"),
                        );
                        return Err(VmError::Throw(e));
                    }
                }
            }
            stream_op(vm, name, args)
        }
        "java/util/stream/Stream$Builder" | "java/util/stream/IntStream$Builder"
        | "java/util/stream/LongStream$Builder" | "java/util/stream/DoubleStream$Builder" => {
            let recv = match args.first() { Some(Value::Ref(h)) => *h, _ => return Ok(Some(Some(Value::Null))) };
            match name {
                "add" | "accept" => {
                    let mut e = elems_of(vm, args.first());
                    e.push(args.get(1).cloned().unwrap_or(Value::Null));
                    let arr = vm.heap.alloc_values(OBJ, e);
                    vm.heap.put_field_syn(recv, crate::heap::Syn::Elems, Value::Array(arr));
                    // add() returns the builder (chainable); accept() is void.
                    if name == "add" { Ok(Some(Some(Value::Ref(recv)))) } else { Ok(Some(None)) }
                }
                "build" => { let e = elems_of(vm, args.first()); Ok(Some(Some(make_stream(vm, e)))) }
                _ => Ok(Some(Some(Value::Null))),
            }
        }
        "java/util/stream/Collectors" => Ok(Some(Some(collector(vm, name, args)))),
        // Guava's `ImmutableMap.toImmutableMap(keyFn, valFn[, mergeFn])` collector,
        // run as guava bytecode, builds a real guava ImmutableMap whose `get` works
        // but whose `entrySet()/keySet().stream()` come back EMPTY in our interp (its
        // view iterator has no drainable bytecode). That silently emptied
        // `RegistryAccess.registries()` (= `registries.entrySet().stream()`, where the
        // registries map is built by `ImmutableMap.toImmutableMap`), so
        // `LayeredRegistryAccess.collectRegistries` dropped every registry and the
        // composite lookup threw `Missing registry: minecraft:dimension` (Wall #16).
        // Route it to our marked toMap collector so `collect` yields a native map
        // whose entrySet/keySet/values stream consistently. Same shape as JDK
        // `toUnmodifiableMap`. Gated → default build byte-identical.
        #[cfg(feature = "next-gen-gc")]
        "com/google/common/collect/ImmutableMap" | "com/google/common/collect/ImmutableBiMap"
            if name == "toImmutableMap" =>
        {
            Ok(Some(Some(collector(vm, "toMap", args))))
        }
        "java/util/stream/Collector" if name == "of" => {
            // of(supplier, accumulator, combiner[, finisher], characteristics...)
            let h = vm.heap.alloc("java/util/stream/Collector");
            vm.heap.put_field(h, "$kind", Value::Str("custom".into()));
            if let Some(s) = args.first().cloned() { vm.heap.put_field(h, "$supplier", s); }
            if let Some(a) = args.get(1).cloned() { vm.heap.put_field(h, "$accum", a); }
            // 4th arg is a finisher only if it's a non-array functional object — a
            // HANDLE (Value::Ref) OR an ARENA lambda (Value::Obj) under next-gen-gc.
            // Matching only Ref dropped an arena finisher (e.g. guava
            // ImmutableMap.Builder::build in toImmutableMap), so `collect` returned the
            // un-built Builder instead of the map → wrong capture → IndirectMerger.get
            // ULE in Block.lambda$getShapeForEachState$13. (Characteristics varargs
            // arrive as a Value::Array, correctly excluded.)
            match args.get(3) {
                Some(f @ Value::Ref(_)) => { vm.heap.put_field(h, "$finisher", f.clone()); }
                #[cfg(feature = "next-gen-gc")]
                Some(f @ Value::Obj(_)) => { vm.heap.put_field(h, "$finisher", f.clone()); }
                _ => {}
            }
            Ok(Some(Some(Value::Ref(h))))
        }
        "java/util/stream/StreamSupport" if matches!(name, "stream" | "intStream" | "longStream" | "doubleStream") => {
            // Object and primitive spliterator streams drain the same way; our
            // stream model is element-type agnostic ($elems holds the values).
            Ok(Some(Some(stream_from_spliterator(vm, args.first())?)))
        }
        "java/util/Arrays" => arrays_static(vm, name, args),
        "java/util/Collections" => collections_static(vm, name, args),
        // guava Sets — the lazy SetView returned by union/difference/intersection
        // iterates as empty in our interp (its custom iterator has no drainable
        // bytecode), so ContextKeySet.build()'s `copyOf(Sets.union(required,
        // optional))` produced an EMPTY allowed set and every loot/enchantment
        // condition validated as "parameters not provided". Materialise instead.
        "com/google/common/collect/Sets"
            if matches!(name, "union" | "difference" | "intersection" | "newHashSet" | "newLinkedHashSet" | "newTreeSet" | "newConcurrentHashSet" | "newIdentityHashSet") =>
        {
            let a = elems_of(vm, args.first());
            let result: Vec<Value> = match name {
                "union" => {
                    let b = elems_of(vm, args.get(1));
                    let mut seen: std::collections::HashSet<String> = a.iter().map(Value::display).collect();
                    let mut v = a;
                    for x in b { if seen.insert(x.display()) { v.push(x); } }
                    v
                }
                "difference" => {
                    let b: std::collections::HashSet<String> = elems_of(vm, args.get(1)).iter().map(Value::display).collect();
                    a.into_iter().filter(|x| !b.contains(&x.display())).collect()
                }
                "intersection" => {
                    let b: std::collections::HashSet<String> = elems_of(vm, args.get(1)).iter().map(Value::display).collect();
                    a.into_iter().filter(|x| b.contains(&x.display())).collect()
                }
                // newHashSet(Iterable/varargs) or empty
                _ => match args.first() {
                    Some(Value::Array(h)) => vm.heap.array_values(*h).unwrap_or_default(),
                    Some(Value::Ref(_)) => a,
                    _ => Vec::new(),
                },
            };
            let h = vm.heap.alloc("java/util/LinkedHashSet");
            let arr = vm.heap.alloc_values(OBJ, result);
            vm.heap.put_field_syn(h, crate::heap::Syn::Elems, Value::Array(arr));
            Ok(Some(Some(Value::Ref(h))))
        }
        "java/io/BufferedReader" | "java/io/InputStreamReader" if name == "lines" => {
            Ok(Some(Some(lines(vm, args.first()))))
        }
        "java/util/List" | "java/util/Set" if name == "of" => {
            // of() / of(E...) — single array (varargs) or individual elements.
            let elems = match args.first() {
                Some(Value::Array(h)) => vm.heap.array_values(*h).unwrap_or_default(),
                _ => args.to_vec(),
            };
            // FAIL-FAST: `ImmutableCollections` runs `Objects.requireNonNull` on
            // every element, and `Set.of` additionally rejects a duplicate — both
            // BEFORE the collection exists. Ours accepted either silently, so a
            // null that the JDK would have named at the factory only surfaced far
            // downstream (or not at all).
            reject_nulls(vm, &elems)?;
            let is_set = owner == "java/util/Set";
            if is_set {
                reject_duplicate_elements(vm, &elems)?;
            }
            let v = if is_set { make_set(vm, elems) } else { make_list(vm, elems) };
            crate::immutable::mark(vm, &v, crate::immutable::IMMUTABLE);
            Ok(Some(Some(v)))
        }
        "java/util/List" | "java/util/Set" if name == "copyOf" => {
            let elems = elems_of(vm, args.first());
            reject_nulls(vm, &elems)?;
            // `Set.copyOf` DEDUPLICATES (unlike `Set.of`, which throws on a
            // duplicate); ours kept both copies, so its `size()` over-reported.
            let is_set = owner == "java/util/Set";
            let elems = if is_set { dedup_elements(vm, elems) } else { elems };
            let v = if is_set { make_set(vm, elems) } else { make_list(vm, elems) };
            crate::immutable::mark(vm, &v, crate::immutable::IMMUTABLE);
            Ok(Some(Some(v)))
        }
        // The gate above already asked this whenever no literal arm could match, which is the
        // path `java/util/ArrayList` and friends take — read that answer instead of probing the
        // `owner_bits` memo twice. `None` means the gate never needed it (a literal-length owner,
        // or the kill switch), so the arm asks exactly as it always did.
        o if matches!(coll, Some(true)) || (coll.is_none() && is_collection(o)) => {
            coll_op(vm, name, descriptor, args)
        }
        // A `Spliterator` produced by `coll_op` is the SAME `$elems`/`$pos` cursor an
        // iterator is, so it is served by the same operations — otherwise `spliterator()`
        // succeeds and every method on the result is an UnsatisfiedLinkError, which is
        // exactly how bench/IterSem.java stayed truncated after the first fix.
        "java/util/Iterator" | "java/util/ListIterator" | "java/util/Spliterator" => {
            iter_op(vm, name, args)
        }
        "java/util/Optional" | "java/util/OptionalInt" | "java/util/OptionalLong" | "java/util/OptionalDouble"
            if matches!(name, "empty" | "of" | "ofNullable") =>
        {
            // `Optional.of(null)` THROWS; only `ofNullable` accepts one. Treating them
            // alike turned a programming error into a silently-empty Optional, which is
            // exactly the bug `of` exists to catch.
            if name == "of" && matches!(args.first(), Some(Value::Null) | None) {
                let e = vm.make_exception("java/lang/NullPointerException", "");
                return Err(VmError::Throw(e));
            }
            let v = if name == "empty" {
                None
            } else {
                args.first().cloned().filter(|x| !matches!(x, Value::Null))
            };
            Ok(Some(Some(make_optional(vm, v))))
        }
        "java/util/Optional" | "java/util/OptionalInt" | "java/util/OptionalLong" | "java/util/OptionalDouble" => optional_op(vm, name, args),
        _ => Ok(None),
    }
}

// --- element containers -----------------------------------------------------


// --- drift guards -----------------------------------------------------------
// The gate above is a FASTER WAY TO COMPUTE THE SAME PREDICATES, and that claim is only worth
// anything if it is checked. Both tests re-derive the historic condition from its own source text
// and compare, so an arm added to `dispatch` without a row in `OWNER_LITERALS`, or a name added
// to the terminal set without a row in `name_key`, fails here rather than losing a native.
#[cfg(test)]
mod gate_tests {
    use super::*;

    /// The nineteen names the old `lazy_name` chain listed, verbatim.
    const LAZY: [&str; 19] = [
        "spliterator", "stream", "iterator", "forEach", "forEachOrdered", "close",
        "count", "findFirst", "findAny", "anyMatch", "allMatch", "noneMatch",
        "sequential", "parallel", "unordered", "onClose", "boxed",
        "asLongStream", "asDoubleStream",
    ];
    /// The four the old `needs_iter` chain listed, verbatim.
    const ITER: [&str; 4] = ["spliterator", "stream", "iterator", "forEach"];

    #[test]
    fn name_key_matches_the_old_chain() {
        // Every served name keys to something other than `Other`, and distinctly.
        let mut seen = Vec::new();
        for n in LAZY {
            let k = name_key(n);
            assert_ne!(k, SName::Other, "{n} lost its key");
            assert!(!seen.contains(&k), "{n} collides with an earlier name");
            seen.push(k);
            assert_eq!(
                matches!(k, SName::Spliterator | SName::Stream | SName::Iterator | SName::ForEach),
                ITER.contains(&n),
                "{n} disagrees about needs_iter"
            );
        }
        // And nothing else does. A corpus of the other names that actually reach this
        // dispatcher — near-misses in length, first byte and last byte included.
        for n in [
            "", "of", "get", "add", "put", "size", "next", "hasNext", "equals", "hashCode",
            "toString", "contains", "isEmpty", "compareTo", "apply", "accept", "remove",
            "copyOf", "empty", "build", "builder", "range", "rangeClosed", "iterate",
            "generate", "concat", "lines", "union", "difference", "intersection", "map",
            "filter", "sorted", "limit", "collect", "reduce", "toList", "toArray",
            "clone", "count0", "boxes", "streams", "forEached", "iterators", "anyMatched",
            "asIntStream", "asDoubleStrea", "spliterators", "sequentiaL", "Parallel",
            "unorderedX", "onClosed", "findFirs", "toImmutableMap", "intStream",
        ] {
            assert_eq!(name_key(n), SName::Other, "{n} keyed as a served name");
        }
    }

    #[test]
    fn owner_len_mask_covers_every_arm() {
        for o in OWNER_LITERALS {
            assert!(owner_may_match(o), "{o} would be gated away");
        }
        // And the mask is a real filter, not `true`. It is a LENGTH test, so it admits some
        // owners no arm matches (`java/util/Random` is as long as `java/util/Arrays`) — that is
        // the over-admission the doc comment allows, and those simply walk the historic route.
        // What matters is that the traffic below, which the stage gate admits wholesale, is
        // rejected without touching an arm.
        for o in [
            "java/util/Objects",
            "java/util/ArrayList",
            "java/util/HashMap",
            "java/util/stream/DoubleStream",
            "java/io/InputStream",
            "com/google/common/collect/ImmutableList",
            "",
        ] {
            assert!(!owner_may_match(o), "{o} passes a mask that should reject it");
        }
    }
}
