//! Generic `java.util.stream` + collection plumbing (Stream, List/Collection,
//! Iterator, Optional, Collectors). Eager model: every intermediate op
//! materialises a fresh element vector — correct for finite sources and simple
//! to reason about; laziness is a later optimisation. Keyed only on real JDK
//! names (no jar specifics, per the no-adapt rule).
//!
//! Functional arguments (mappers/predicates/consumers/comparators) are applied
//! via `Vm::apply`, so any lambda or method-ref closure works.







pub(crate) use collect::*;
mod collect;
use crate::value::Value;
use crate::{Vm, VmError};

/// `RAVA_TRACE_FLATMAP` flag, read ONCE — these checks sit on per-stream-op paths
/// (flatMap/forEach during codec build); `std::env::var` per call (global mutex +
/// alloc) is pure waste when tracing is off.
fn trace_flatmap() -> bool {
    static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *F.get_or_init(|| std::env::var_os("RAVA_TRACE_FLATMAP").is_some())
}

/// `RAVA_FORK_SHAPES` gate (M3): the fork-each detection probe (F3.1) at `Collection.forEach`.
/// Read once — off by default, so the serial forEach path stays byte-identical.
#[cfg(feature = "next-gen-gc")]
fn fork_shapes() -> bool {
    static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *F.get_or_init(|| std::env::var_os("RAVA_FORK_SHAPES").is_some())
}

type Handled = Result<Option<Option<Value>>, VmError>;

pub(crate) const OBJ: &str = "Ljava/lang/Object;";

pub(crate) fn make_stream(vm: &mut Vm, elems: Vec<Value>) -> Value {
    let arr = vm.heap.alloc_values(OBJ, elems);
    let h = vm.heap.alloc("java/util/stream/Stream");
    vm.heap.put_field_syn(h, crate::heap::Syn::Elems, Value::Array(arr));
    Value::Ref(h)
}

/// Build a Stream from an explicit element vector (used by other natives, e.g.
/// ServiceLoader.stream).
pub(crate) fn of_values(vm: &mut Vm, elems: Vec<Value>) -> Value {
    make_stream(vm, elems)
}

/// Build an Iterator object over an explicit element vector.
pub(crate) fn iterator_of(vm: &mut Vm, elems: Vec<Value>) -> Value {
    let arr = vm.heap.alloc_values(OBJ, elems);
    let it = vm.heap.alloc("java/util/Iterator");
    vm.heap.put_field_syn(it, crate::heap::Syn::Elems, Value::Array(arr));
    vm.heap.put_field_syn(it, crate::heap::Syn::Pos, Value::Int(0));
    Value::Ref(it)
}

fn make_list(vm: &mut Vm, elems: Vec<Value>) -> Value {
    make_coll(vm, elems, "java/util/ArrayList")
}

/// The SET twin of `make_list`, and the reason it has to exist: every decision
/// this engine makes about Set-vs-List semantics keys on the receiver's class
/// name, and `Set.of` / `Set.copyOf` / `Collections.emptySet` / `singleton` all
/// built their result with `make_list`, i.e. class `java/util/ArrayList`.
///
/// So `Set.of("a")` was a LIST to `is_set_recv`: it compared positionally, and
/// `Set.of("a").equals(new HashSet<>(List.of("a")))` answered FALSE because the
/// two sides disagreed on kind. It also skipped the copy-constructor dedup and
/// the order-independent `hashCode` fold.
///
/// `LinkedHashSet` rather than `HashSet` because the backing store is ordered and
/// the JDK's own `Set.of` iteration order is unspecified — reproducing an
/// insertion order is inside the contract, inventing a hash order is not.
fn make_set(vm: &mut Vm, elems: Vec<Value>) -> Value {
    make_coll(vm, elems, "java/util/LinkedHashSet")
}

fn make_coll(vm: &mut Vm, elems: Vec<Value>, class: &str) -> Value {
    let arr = vm.heap.alloc_values(OBJ, elems);
    let h = vm.heap.alloc(class);
    vm.heap.put_field_syn(h, crate::heap::Syn::Elems, Value::Array(arr));
    Value::Ref(h)
}

/// A list BACKED BY AN EXISTING ARRAY, sharing its handle rather than copying it.
///
/// `Arrays.asList(a)` is specified as a WRITE-THROUGH view: `asList(arr).set(0, x)` stores
/// into `arr`, and a later `arr[0]` reads `x`. Building it with `make_list` copied the
/// elements, so every read answered correctly and only the write-back was missing — which
/// is why nothing caught it until a differential probe looked through the original array
/// (bench/CollSem.java, `asList.write-through`). A silently wrong answer, not an error.
///
/// Sharing the handle is the whole fix: `coll_op`'s O(1) `set` arm already writes straight
/// into the backing array with `array_set`, so the store lands in the caller's array with
/// no view machinery at all. It works because this view is the easy one — offset 0, same
/// length. `subList` is the offset case and needs more than a shared handle.
fn make_list_over(vm: &mut Vm, arr: u32) -> Value {
    let h = vm.heap.alloc("java/util/ArrayList");
    vm.heap.put_field_syn(h, crate::heap::Syn::Elems, Value::Array(arr));
    Value::Ref(h)
}

/// `make_optional` for the early `Optional.of` fast-path in `natives::dispatch_owner`
/// — the same constructor the general `stream::dispatch` arm uses.
pub(crate) fn make_optional_value(vm: &mut Vm, val: Option<Value>) -> Value {
    make_optional(vm, val)
}

fn truthy(v: &Option<Value>) -> bool {
    matches!(v, Some(x) if x.as_int().unwrap_or(0) != 0)
}

// --- sources ----------------------------------------------------------------

fn stream_of(vm: &mut Vm, args: &[Value]) -> Value {
    // Stream.of(T...) — single array arg (varargs) or one element.
    let elems = match args.first() {
        Some(Value::Array(h)) => vm.heap.array_values(*h).unwrap_or_default(),
        Some(v) => vec![v.clone()],
        None => Vec::new(),
    };
    make_stream(vm, elems)
}

fn from_array(vm: &mut Vm, arr: Option<&Value>) -> Value {
    let elems = match arr {
        Some(Value::Array(h)) => vm.heap.array_values(*h).unwrap_or_default(),
        _ => Vec::new(),
    };
    make_stream(vm, elems)
}

/// Recursive `Arrays.deepToString` element rendering: arrays nest as `[a, b]`,
/// nulls render `null`, scalars use their flat display.
fn deep_to_string(vm: &Vm, v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Array(h) => {
            let e = vm.heap.array_values(*h).unwrap_or_default();
            let body = e.iter().map(|x| deep_to_string(vm, x)).collect::<Vec<_>>().join(", ");
            format!("[{body}]")
        }
        other => other.display(),
    }
}

/// GC-safe per-element iteration. The elements (which may be arena `Value::Obj`s)
/// are moved into a HEAP `Object[]` — a collector root whose cells the evacuator
/// forwards IN PLACE — so an allocating `step` lambda cannot strand the not-yet-
/// processed elements in an unrooted Rust `Vec`. Each element is read back
/// FORWARDED via `array_get`; `step` returns `Break` to stop early. (Wall #10: the
/// Climate RTree build stranded `SubTree`s in `map`/`collect`, which the evacuator
/// then reclaimed and reused as `Climate$Parameter`s — a `subTree.children` read on
/// a `Parameter`.)
/// Truncate the root stack back to a saved depth on scope exit — the RAII form of the
/// re-read-after-root discipline used across the map/stream natives. The Drop routes
/// through the context-or-VM truncation helper so the truncation lands on the root
/// stack the push actually landed in (see `thread_context::truncate_roots`).
pub(crate) struct GpScope(pub(crate) *mut Vec<Value>, pub(crate) usize);
impl Drop for GpScope {
    fn drop(&mut self) {
        // SAFETY: `self.0` points to the live Vm fallback vector. The facade uses
        // the current context if installed; caller keeps that context unchanged
        // from the matching push through this guard's drop.
        unsafe { crate::vm::thread_context::truncate_roots(&mut *self.0, self.1) };
    }
}

/// CAN THE COLLECTOR EVER TOUCH THIS VALUE? Only a heap handle, an array handle or an arena
/// pointer is swept, moved or forwarded; an `Int`/`Long`/`Float`/`Double`/`Null`/`StdOut`/`StdErr`
/// is a scalar the root walk skips, and a `Value::Str` owns its own `Arc<String>` — the collector
/// does not manage that allocation at all (`map::keyroot`'s
/// `inline_root_takes_no_slot_and_reads_the_same_values` asserts exactly this).
///
/// So a rooting loop that pushes one of those buys nothing and pays for it: a `Vec` push for a
/// scalar, and for a `Str` TWO ATOMIC READ-MODIFY-WRITES — the `Arc` increment on the clone and
/// the matching decrement when `GpScope` truncates. The caller's `args` slice already holds that
/// `Arc` live across the whole call, so the second handle exists only to be dropped.
///
/// Not a new rule: `opcodes::statics`'s call-root guard has filtered its `gc_protect` pushes with
/// this exact predicate on the hot invoke path for some time. This is the one definition the two
/// collection dispatchers now share.
#[inline]
pub(crate) fn is_gc_rootable(v: &Value) -> bool {
    matches!(v, Value::Ref(_) | Value::Array(_) | Value::Obj(_))
}

/// Push a heap array handle onto `vm.gc_protect` (making it a GC root) and return a scope
/// guard that pops it on drop. Stream helpers hold their `src`/accumulator arrays only via
/// a Rust-local handle; without this a mid-sweep collection (see `stream_maybe_gc`) would
/// reclaim them as unreachable. Caller must keep the guard alive for the array's lifetime.
pub(crate) fn gc_protect_array(vm: &mut Vm, arr: u32) -> GpScope {
    gc_protect_value(vm, Value::Array(arr))
}

/// `gc_protect_array` for any `Value` (e.g. a Rust-local Consumer/container object that
/// would otherwise be swept between element applies).
pub(crate) fn gc_protect_value(vm: &mut Vm, v: Value) -> GpScope {
    let base = crate::vm::thread_context::root_len(&vm.gc_protect);
    crate::vm::thread_context::push_root(&mut vm.gc_protect, v);
    GpScope(&mut vm.gc_protect as *mut _, base)
}

/// Root a per-element stream FUNCTION (map/filter/reduce/… lambda) in `gc_protect`
/// for the whole pass and hand `body` the slot index. Under RAVA_ARENA_LAMBDA a
/// capturing lambda is a MOVING arena `Value::Obj`; the mid-loop `stream_maybe_gc`
/// and each `apply`'s guest GC relocate it, so a pre-loop Rust binding of the
/// function goes stale (a handle lambda stayed valid — a stable u32). `gc_protect`
/// is forwarded by `arena_collect_jit`, so `body` re-reads `vm.gc_protect[slot]`
/// (the FORWARDED function) before each apply. `truncate` restores the stack on
/// every exit path (incl. `?`).
fn with_rooted_fn<T>(
    vm: &mut Vm,
    f: Value,
    body: impl FnOnce(&mut Vm, usize) -> Result<T, VmError>,
) -> Result<T, VmError> {
    let slot = crate::vm::thread_context::root_len(&vm.gc_protect);
    crate::vm::thread_context::push_root(&mut vm.gc_protect, f);
    let r = body(vm, slot);
    crate::vm::thread_context::truncate_roots(&mut vm.gc_protect, slot);
    r
}

/// The forwarded per-element function held at `slot` (see `with_rooted_fn`).
#[inline]
fn rooted_fn(vm: &Vm, slot: usize) -> Value {
    crate::vm::thread_context::root_get(&vm.gc_protect, slot).unwrap_or(Value::Null)
}

// --- lazy/fused pipeline (RAVA_LAZY_STREAMS, default OFF) --------------------
//
// The eager model materialises a fresh element vector at EVERY intermediate op:
// `s.map(f).filter(p).map(g).collect()` walks the elements 3 times and allocates
// 3 intermediate Object[]. OpenJDK fuses the whole chain into ONE pass with no
// intermediate collections. Here we approximate that: the linear per-element ops
// (map/mapToX/filter/peek) DEFER — each appends a {kind,fn} pair to a lazy stream's
// `$ops` list over a shared, immutable `$src` array — and any terminal or barrier op
// (collect/reduce/sum/forEach/sorted/distinct/flatMap/…) materialises the deferred
// chain in a single fused pass (`fuse_materialize`). Barriers thus fall back to the
// eager body, so only the map/filter/peek runs are fused — the dominant codec/registry
// build pattern.
//
// ⭐**AND FUSION IS ALSO THE FAITHFUL ORDER, WHICH IS WHY IT IS NOW THE DEFAULT.** The eager
// model applies `f` to EVERY element before `p` sees the first one; `s.map(f).filter(p)` on a
// real JVM runs `f(e0) p(e0) f(e1) p(e1) …`. Any pipeline whose functions log, register,
// mutate or throw therefore observed a different sequence of calls here than on HotSpot, and
// a mapper that throws on element 3 ran `f` over elements 4..n first. `fuse_materialize` and
// `fuse_drive` produce the JVM's interleaving, so this is a correctness change that happens
// to also delete the intermediates.
fn lazy_streams_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    // `RAVA_LAZY_STREAMS` is kept as the independent opt-IN it always was, so the deferral
    // can still be exercised with fast streams killed; fast streams turn it on by default.
    *ON.get_or_init(|| std::env::var_os("RAVA_LAZY_STREAMS").is_some() || fast_streams_enabled())
}

/// FAST STREAMS — DEFAULT ON, `RAVA_NO_FAST_STREAMS=1` opts out (and so does
/// `flags::emit_more_disabled()`, the tree-wide "emit nothing new" switch).
///
/// Covers three fusions, all of them removals rather than shavings:
///   * **the intermediate list** — a deferred `map`/`filter`/`peek` records a `{kind,fn}` pair
///     over a SHARED source array instead of walking the elements and allocating a fresh
///     `Object[]` plus a fresh `Stream` object per stage (`lazy_append`);
///   * **the terminal's output list** — `count`/`forEach`/`findFirst`/`*Match` consume the
///     fused pass element by element and never build one (`fuse_drive`);
///   * **the materialised source copy** — the generic `Collection.forEach` walked a
///     double copy of its receiver (`direct_elems`).
///
/// Everything it cannot prove falls through to the eager body invisibly: a barrier op
/// (`sorted`/`distinct`/`limit`/`skip`/`flatMap`), a generator receiver, a non-handle
/// receiver, a view, a sorted collection.
pub(crate) fn fast_streams_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        crate::flags::fast_natives()
            && std::env::var_os("RAVA_NO_FAST_STREAMS").is_none()
            && !crate::flags::emit_more_disabled()
    })
}

// ══════════════════ ONE GATE READ PER STREAM/COLLECTION OPERATION ═════════════════════════════
//
// ⭐**`stream::dispatch` READ THREE TO FIVE PROCESS CONSTANTS PER CALL, AND READ ONE OF THEM
// TWICE.** Its own comment already prices the population: the `owner_bits` stage gate admits every
// `java/util/*`, `java/io/*` and `com/google/common/*` owner to this dispatcher, **nineteen
// million calls a boot**, and the overwhelming majority of them leave through the `_ =>` at the
// bottom having served nothing. On that traffic the atomic loads were:
//
//   * `fast_streamdisp()`                      — EVERY call, at the entry;
//   * `lazy_streams_enabled()`                 — via `recv_is_lazy_stream`, every call whose name
//                                                keys to one of the nineteen served names;
//   * `lazy_streams_enabled()` AGAIN           — in the unbounded-generator refusal, on the
//                                                `java/util/stream/Stream*` arm: **the same flag,
//                                                loaded a second time in the same call**;
//   * `fast_streams_enabled()`                 — via `direct_elems`, on every `forEach`;
//   * `fork_shapes()`                          — on every `forEach` over ≥2 elements, twice over
//                                                (once per arm);
//   * `arena_map::enabled()`                   — in the `is_map_recv` probe, per arena receiver.
//
// An atomic load is not a `mov`: it is an OPTIMISATION BARRIER. LLVM may not hoist it out of the
// caller's loop, may not fold two reads of the same flag together — which is why the doubled
// `lazy_streams_enabled()` really was two loads — and must materialise live values across each
// one. **Per operation: `forEach` on a collection 4 → 1, an `Iterable` default (`stream`,
// `iterator`, `spliterator`) 2-3 → 1, a `Stream` pipeline op 3 → 1, and the decline 1 → 1.**
//
// The word is built from the SAME functions, so every bit is the same predicate the site read;
// only the number of fetches changes. None of these is a gate whose POSITION decides which path is
// reachable — each is still consulted where it always was — and none has a side effect, so
// initialising them all on the first dispatch of the process is unobservable.
//
// Kill switch `RAVA_NO_MS_GATEWORD=1` (and the tree-wide `RAVA_NO_EMIT_MORE`) clears `SG_ON`, and
// every accessor then calls the original function at the original site, restoring the N loads.

/// The gate word is live. Clear ⇒ every accessor falls back to its own atomic load.
pub(crate) const SG_ON: u8 = 1 << 0;
/// `dispatch::fast_streamdisp()`.
pub(crate) const SG_FASTDISP: u8 = 1 << 1;
/// `lazy_streams_enabled()`.
pub(crate) const SG_LAZY: u8 = 1 << 2;
/// `fast_streams_enabled()`.
pub(crate) const SG_FASTSTREAMS: u8 = 1 << 3;
/// `fork_shapes()` (always clear without `next-gen-gc`).
#[allow(dead_code)]
pub(crate) const SG_FORK: u8 = 1 << 4;
/// `arena_map::enabled()` (always clear without `next-gen-gc`).
#[allow(dead_code)]
pub(crate) const SG_ARENAMAP: u8 = 1 << 5;

/// THE ONE ATOMIC LOAD. Read once at the top of `stream::dispatch`; every question below it is a
/// register test on the returned byte.
#[inline]
pub(crate) fn stream_gates() -> u8 {
    static W: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *W.get_or_init(|| {
        let mut w = 0u8;
        if std::env::var_os("RAVA_NO_MS_GATEWORD").is_none() && !crate::flags::emit_more_disabled()
        {
            w |= SG_ON;
        }
        if dispatch::fast_streamdisp() {
            w |= SG_FASTDISP;
        }
        if lazy_streams_enabled() {
            w |= SG_LAZY;
        }
        if fast_streams_enabled() {
            w |= SG_FASTSTREAMS;
        }
        #[cfg(feature = "next-gen-gc")]
        {
            if fork_shapes() {
                w |= SG_FORK;
            }
            if crate::arena_map::enabled() {
                w |= SG_ARENAMAP;
            }
        }
        w
    })
}

#[inline]
pub(crate) fn sg_fastdisp(g: u8) -> bool {
    if g & SG_ON != 0 { g & SG_FASTDISP != 0 } else { dispatch::fast_streamdisp() }
}

#[inline]
pub(crate) fn sg_lazy(g: u8) -> bool {
    if g & SG_ON != 0 { g & SG_LAZY != 0 } else { lazy_streams_enabled() }
}

#[allow(dead_code)]
#[inline]
pub(crate) fn sg_fast_streams(g: u8) -> bool {
    if g & SG_ON != 0 { g & SG_FASTSTREAMS != 0 } else { fast_streams_enabled() }
}

/// Only read inside `next-gen-gc` arms today; the allow keeps the default build warning-free.
#[cfg(feature = "next-gen-gc")]
#[allow(dead_code)]
#[inline]
pub(crate) fn sg_fork(g: u8) -> bool {
    if g & SG_ON != 0 { g & SG_FORK != 0 } else { fork_shapes() }
}

#[cfg(feature = "next-gen-gc")]
#[allow(dead_code)]
#[inline]
pub(crate) fn sg_arena_map(g: u8) -> bool {
    if g & SG_ON != 0 { g & SG_ARENAMAP != 0 } else { crate::arena_map::enabled() }
}

const LAZY_MAP: i32 = 0;
const LAZY_FILTER: i32 = 1;
const LAZY_PEEK: i32 = 2;

/// True if `v` is a lazy (`$ops`) stream receiver — must be routed to stream_op
/// (which fuses) rather than the generic Iterable drain paths that read `$elems`.
/// `g` is `stream::dispatch`'s gate word, already in a register at the call site: this was one
/// `OnceLock` acquire load per served-name call into the dispatcher, and is now a bit test.
fn recv_is_lazy_stream(vm: &mut Vm, g: u8, v: Option<&Value>) -> bool {
    sg_lazy(g)
        && matches!(v, Some(Value::Ref(h)) if matches!(vm.heap.get_field(*h, "$ops"), Value::Array(_)))
}

/// Fallible element acquisition for a stream receiver: materialises a lazy (`$ops`)
/// stream in the fused path so a throwing mapper/predicate propagates; otherwise the
/// eager `elems_of`. Used at the terminal/barrier entry so exceptions are not swallowed.
fn stream_elems(vm: &mut Vm, v: Option<&Value>) -> Result<Vec<Value>, VmError> {
    if lazy_streams_enabled() {
        if let Some(Value::Ref(h)) = v {
            if matches!(vm.heap.get_field(*h, "$ops"), Value::Array(_)) {
                return fuse_materialize(vm, *h);
            }
        }
    }
    Ok(elems_of(vm, v))
}

/// `fuse_materialize` WITHOUT THE OUTPUT LIST: run a lazy stream's deferred map/filter/peek
/// chain in one pass and hand each surviving element straight to `sink`, which stops the walk
/// by answering `Break`.
///
/// This is the half of stream fusion the eager engine could not express at all. A terminal
/// that only reads — `count`, `forEach`, `findFirst`, `anyMatch` — was served by materialising
/// the whole chain into an `Object[]`, converting that to a `Vec<Value>`, and then looking at
/// it; the list existed for the length of one loop and was dropped. Worse, the SHORT-CIRCUITING
/// terminals could not short-circuit: `map(f).findFirst()` applied `f` to every element, where
/// a real JVM applies it to exactly one, so the eager path was both slower and a different
/// observable sequence of calls.
///
/// GC discipline is `fuse_materialize`'s, unchanged and load-bearing: the in-flight element
/// rides in a rooted 1-slot heap array and is RE-READ after every guest `apply` (which can move
/// the arena); the op functions are re-read fresh out of the rooted `$ops` array rather than
/// bound before the loop; `$src`/`$ops`/`cur` are roots for the whole pass. `sink` runs guest
/// code too, so the same rule applies to anything it holds — see the 1-slot accumulators at the
/// `findFirst` call site.
pub(crate) fn fuse_drive(
    vm: &mut Vm,
    h: u32,
    mut sink: impl FnMut(&mut Vm, Value) -> Result<std::ops::ControlFlow<()>, VmError>,
) -> Result<(), VmError> {
    let _gph = gc_protect_value(vm, Value::Ref(h)); // roots reachable $src + $ops
    let src = match vm.heap.get_field(h, "$src") {
        Value::Array(a) => a,
        _ => return Ok(()),
    };
    let ops = match vm.heap.get_field(h, "$ops") {
        Value::Array(a) => a,
        _ => return Ok(()),
    };
    let n = vm.heap.array_len(src).unwrap_or(0);
    let nops = vm.heap.array_len(ops).unwrap_or(0) / 2;
    let cur = vm.heap.alloc_values(OBJ, vec![Value::Null]);
    let _gpc = gc_protect_array(vm, cur);
    for i in 0..n {
        let e = vm.heap.array_get(src, i).unwrap_or(Value::Null);
        vm.heap.array_set(cur, 0, e);
        let mut dropped = false;
        for j in 0..nops {
            let kind = vm.heap.array_get(ops, 2 * j).and_then(|v| v.as_int()).unwrap_or(LAZY_MAP);
            let fnv = vm.heap.array_get(ops, 2 * j + 1).unwrap_or(Value::Null);
            let c = vm.heap.array_get(cur, 0).unwrap_or(Value::Null);
            match kind {
                LAZY_FILTER => {
                    if !truthy(&vm.apply(&fnv, vec![c])?) {
                        dropped = true;
                        break;
                    }
                }
                LAZY_PEEK => {
                    vm.apply(&fnv, vec![c])?;
                }
                _ => {
                    let r = vm.apply(&fnv, vec![c])?.unwrap_or(Value::Null);
                    vm.heap.array_set(cur, 0, r);
                }
            }
        }
        if !dropped {
            let keep = vm.heap.array_get(cur, 0).unwrap_or(Value::Null);
            if sink(vm, keep)?.is_break() {
                return Ok(());
            }
        }
        stream_maybe_gc(vm);
    }
    Ok(())
}

/// `count()` over a fused chain — the "filter feeding a count needs no list at all" case.
///
/// ⭐**AND WITH NO FILTER IT NEEDS NO PASS EITHER, WHICH IS THE JDK'S OWN RULE, NOT AN
/// OPTIMISATION.** `Stream.count()` is specified to "not execute the stream pipeline … if it
/// is capable of computing the count directly from the stream source", which is why
/// `list.stream().peek(p).count()` prints nothing on a real JVM. Our `$src` is always a
/// materialised array, i.e. always SIZED, and `map`/`peek` cannot change an element count — so
/// a chain of those answers the source length and runs NOTHING, exactly as HotSpot does. One
/// `filter` anywhere and the whole chain runs, still without building a list.
pub(crate) fn fuse_count(vm: &mut Vm, h: u32) -> Result<i64, VmError> {
    let ops = match vm.heap.get_field(h, "$ops") {
        Value::Array(a) => a,
        _ => return Ok(0),
    };
    let nops = vm.heap.array_len(ops).unwrap_or(0) / 2;
    let mut size_preserving = true;
    for j in 0..nops {
        if vm.heap.array_get(ops, 2 * j).and_then(|v| v.as_int()).unwrap_or(LAZY_MAP) == LAZY_FILTER
        {
            size_preserving = false;
            break;
        }
    }
    if size_preserving {
        let n = match vm.heap.get_field(h, "$src") {
            Value::Array(a) => vm.heap.array_len(a).unwrap_or(0),
            _ => 0,
        };
        return Ok(n as i64);
    }
    let mut n = 0i64;
    fuse_drive(vm, h, |_, _| {
        n += 1;
        Ok(std::ops::ControlFlow::Continue(()))
    })?;
    Ok(n)
}

/// The backing `$elems` array of a receiver whose drain is EXACTLY that array, in that order —
/// or `None` the moment anything could make the two differ.
///
/// ⭐**A `forEach` COPIED ITS SOURCE TWICE.** The generic `Collection.forEach` arm of
/// `stream::dispatch` ran `iterate_recv` (decode every cell into a `Vec<Value>`) and then
/// `alloc_values` (re-encode every one into a fresh heap array whose only job was to be a GC
/// root) — two O(n) copies and two allocations for a terminal that only READS. The receiver is
/// already rooted (every argument is, at the top of `dispatch`), so its own `$elems` array is a
/// root already: when the drain would have answered that array verbatim, walk it.
///
/// Refuses, invisibly, wherever the drain is not the array:
///   * a `subList` / `unmodifiable` / map view — `iterate_object` re-derives a view from its
///     PARENT, so the view's own `$elems` is a snapshot that may already be stale;
///   * a sorted set or a priority queue — its order is its contract and the drain sorts
///     (`order_sorted_set`), so the array order is the wrong answer;
///   * anything with no native `$elems` — a jar Collection drained through its own bytecode,
///     a `Path` walked as its segments, an arena-born overriding class.
///
/// Returns `(array, logical length)`; `elems_len` honours an overriding class's `$size`, so a
/// guest-bytecode collection's spare capacity is not walked.
/// `g` is `stream::dispatch`'s gate word: the `fast_streams_enabled()` acquire load that opened
/// this function is now a bit test on a byte the caller already holds. One load per `forEach`.
fn direct_elems(vm: &mut Vm, g: u8, recv: &Value) -> Option<(u32, usize)> {
    if !sg_fast_streams(g) {
        return None;
    }
    let h = match recv {
        Value::Ref(h) => *h,
        _ => return None,
    };
    if crate::view::is_view(vm, h) {
        return None;
    }
    if vm.heap.class_of(h).is_some_and(crate::stream::collections::coll_op::is_sorted_set_name) {
        return None;
    }
    match vm.heap.get_field_syn(h, crate::heap::Syn::Elems) {
        Value::Array(a) => {
            let n = vm.heap.elems_len(h, a);
            Some((a, n))
        }
        _ => None,
    }
}

/// Composite-comparator key cache (DEFAULT ON, opt-out `RAVA_NO_SORT_KEYCACHE`): cache each
/// element's key tuple once (n × clause applies) instead of re-extracting every clause on
/// every comparison in the O(n log n) heap-array merge sort. Validated bit-exact (ChunkFill
/// 8790, 3 runs) with a measured MC-boot SETUP delta of ~−7.8s (−9.5%) — the ArrayList#sort
/// self-time was the #1 native-bridge boot sink (18.25s / 25%). Same purity assumption as the
/// always-on single-key fast path.
fn sort_keycache_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_SORT_KEYCACHE").is_none())
}

/// LAZY composite key memo (`RAVA_SORT_KEY_MEMO`, default OFF). The eager cache above
/// pays `n × clauses` extractor applies whatever the data does; `thenComparing` only
/// consults clause `k` when every earlier clause tied, so on a first clause that usually
/// decides most of that work is never asked for. `ComparatorPlan::sort_memoised` extracts
/// a clause key on first touch instead — the authoritative extraction set, deduplicated.
fn sort_key_memo_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_SORT_KEY_MEMO").is_some())
}

fn is_fp(v: &Value) -> bool { matches!(v, Value::Double(_) | Value::Float(_)) }

/// If `c` is a plain `comparing*(keyFn)` comparator (no chaining), return its
/// key function and whether it is reversed — so keys can be cached.
fn key_comparator(vm: &Vm, c: &Value) -> Option<(Value, bool)> {
    let h = match c { Value::Ref(h) => *h, _ => return None };
    if vm.heap.lambda_of(h).is_some() { return None; }
    let kind = match vm.heap.get_field(h, "$cmp") { Value::Str(s) => s.to_string(), _ => return None };
    if kind == "key" || kind == "keyrev" {
        let kf = vm.heap.get_field(h, "$keyfn");
        if !matches!(kf, Value::Null) { return Some((kf, kind == "keyrev")); }
    }
    None
}

fn collect_probe_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_COLLECT_PROBE").is_some())
}

fn str_field(vm: &Vm, h: u32, f: &str) -> String {
    match vm.heap.get_field(h, f) {
        Value::Str(s) => s.to_string(),
        _ => String::new(),
    }
}

/// Value equality with `ekey`'s exact string semantics, but WITHOUT allocating a
/// String per element on the common same-type primitive path. `ekey` formats
/// every element via `display()` (a heap String alloc) just to compare — so a
/// `contains`/`indexOf`/dedup scan over an N-element coordinate list did N
/// allocations + N string compares where the JVM does N int/double compares.
/// Same primitive type → compare the scalar directly (floats by bit pattern so
/// `-0.0`≠`0.0` stays consistent with `display`); anything else (cross-type, Str,
/// Ref/Obj) → fall back to the exact `ekey` string compare, so results are
/// bit-identical to the all-string path. `needle_key` is the precomputed
/// `ekey(needle)` reused across the scan.
pub(crate) fn ekey_eq(vm: &Vm, needle: &Value, needle_key: &str, e: &Value) -> bool {
    match (needle, e) {
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::Long(a), Value::Long(b)) => a == b,
        (Value::Float(a), Value::Float(b)) => a.to_bits() == b.to_bits(),
        (Value::Double(a), Value::Double(b)) => a.to_bits() == b.to_bits(),
        _ => ekey(vm, e) == needle_key,
    }
}

/// `ekey_eq` with `ekey_mut`'s derivation — the form every `&mut Vm` collection
/// op uses, so an element that only keys structurally through its guest
/// `toString()` (a class with a real `equals` override) is compared by VALUE and
/// not by handle. The scalar arms are unchanged and still allocate nothing.
/// `needle_key` must be `ekey_mut(needle)`.
pub(crate) fn ekey_eq_mut(vm: &mut Vm, needle: &Value, needle_key: &str, e: &Value) -> bool {
    match (needle, e) {
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::Long(a), Value::Long(b)) => a == b,
        (Value::Float(a), Value::Float(b)) => a.to_bits() == b.to_bits(),
        (Value::Double(a), Value::Double(b)) => a.to_bits() == b.to_bits(),
        _ => ekey_mut(vm, e) == needle_key,
    }
}

/// Drop later duplicates by element key, keeping the FIRST occurrence — Set
/// semantics for a bulk copy (`new HashSet<>(coll)`, `new TreeSet<>(coll)`),
/// where there is no per-element `add` to run the membership test. Same key
/// derivation as `add`/`contains`, so a set built either way has the same
/// members; a sorted set is ordered on read, so first-occurrence order here is
/// only the insertion order the `add` path would also have produced.
pub(crate) fn dedup_by_ekey(vm: &mut Vm, src: Vec<Value>) -> Vec<Value> {
    // ROOTED: `ekey_mut` can render a value class through its guest `toString()`,
    // and a guest call can move the arena — so the survivors must be read back
    // forwarded rather than pushed from the pre-call binding. See `rooted_keys`.
    let (vals, keys) = rooted_keys(vm, src);
    let mut seen = crate::FastSet::with_capacity_and_hasher(vals.len(), Default::default());
    let mut out = Vec::with_capacity(vals.len());
    for (v, k) in vals.into_iter().zip(keys.into_iter()) {
        if seen.insert(k) {
            out.push(v);
        }
    }
    out
}

/// Cap on the set-membership index (`Vm::set_index`). Past this many tracked
/// sets the whole table is dropped (rebuild-on-miss keeps results correct), so
/// the handles of long-dead sets can't accumulate without bound.
const SET_INDEX_CAP: usize = 8192;

/// Rebuild `recv`'s membership index from the live `$elems` array. Reads are
/// pulled out into `ekey`s first (no heap borrow held across the `set_index`
/// mutation), then stamped with `len`.
fn set_index_rebuild(vm: &mut Vm, recv: u32, arr: u32, len: usize) {
    let mut keys = crate::FastSet::with_capacity_and_hasher(len, Default::default());
    for i in 0..len {
        if let Some(e) = vm.heap.array_get(arr, i) {
            // `ekey_mut`, not `ekey`: the index IS the Set's membership test, so it
            // must carry the same value-equality key `add`/`contains` probe it with,
            // or a value object indexes under `@h` and never matches its `o:…` probe.
            keys.insert(ekey_mut(vm, &e));
        }
    }
    if vm.set_index.len() >= SET_INDEX_CAP {
        vm.set_index.clear();
    }
    vm.set_index.insert(recv, (len, keys));
}

/// O(1) set-membership test backed by `Vm::set_index`, rebuilding the index when
/// its stamp no longer matches the backing length (the self-healing invariant).
fn set_index_contains(vm: &mut Vm, recv: u32, arr: u32, len: usize, k: &str) -> bool {
    let synced = matches!(vm.set_index.get(&recv), Some((s, _)) if *s == len);
    if !synced {
        set_index_rebuild(vm, recv, arr, len);
    }
    vm.set_index.get(&recv).is_some_and(|(_, keys)| keys.contains(k))
}

/// Record a freshly-appended element in the index and re-stamp to the new length.
/// Only called right after a successful `array_push`, with the index already
/// synced by the preceding `set_index_contains`.
fn set_index_insert(vm: &mut Vm, recv: u32, k: String, new_len: usize) {
    if let Some((stamp, keys)) = vm.set_index.get_mut(&recv) {
        keys.insert(k);
        *stamp = new_len;
    }
}

/// `store_elems` over a VALUE rather than a handle — the write half of representation-agnostic
/// collection natives.
///
/// ⭐Every `Collections.*` mutator matched `Value::Ref` and wrote through `store_elems`, which is
/// correct only while a collection this engine serves natively is handle-born. An OVERRIDDEN
/// `java.util` class is not native-served and so is arena-born, and the whole family became a
/// silent no-op: `reverse`, `swap`, `rotate`, `fill` and `addAll` all left the list untouched —
/// twelve divergent lines in `ArraysSem` with one cause.
///
/// The arena arm also keeps the guest's `$size`/`$init` in step, exactly as `Heap::put_field`
/// does for the handle arm, and it only fires for a class that actually declares those fields.
pub(crate) fn store_elems_value(vm: &mut Vm, obj: &Value, elems: Vec<Value>) -> bool {
    match obj {
        Value::Ref(h) => {
            store_elems(vm, *h, elems);
            true
        }
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) => {
            let cls = crate::nextgen::class_name(crate::nextgen::arena_class_id(*p));
            if !cls.as_deref().is_some_and(crate::class_loader::has_bytecode) {
                return false;
            }
            let n = elems.len();
            let arr = vm.heap.alloc_values(OBJ, elems);
            let eid = vm.heap.field_id("$elems");
            let sid = vm.heap.field_id("$size");
            let iid = vm.heap.field_id("$init");
            crate::opcodes::arena_cells::arena_putfield(*p, eid, &Value::Array(arr));
            crate::opcodes::arena_cells::arena_putfield(*p, sid, &Value::Int(n as i32));
            crate::opcodes::arena_cells::arena_putfield(*p, iid, &Value::Int(1));
            true
        }
        _ => false,
    }
}

fn store_elems(vm: &mut Vm, obj: u32, elems: Vec<Value>) {
    // A write through a `subList` view is a write into the BACKING list's range, which is
    // what makes `sub.clear()` shorten `base`. Plain lists fall straight through.
    if crate::view::write(vm, obj, &elems) {
        return;
    }
    let arr = vm.heap.alloc_values(OBJ, elems);
    vm.heap.put_field_syn(obj, crate::heap::Syn::Elems, Value::Array(arr));
    // A `Collections.unmodifiable*` view SHARES this object's backing array, so
    // REPLACING it here (rather than mutating in place, as the O(1) arms do) would
    // strand the view on the old contents. Re-point it too — being live is the
    // JDK's contract for that view. One field probe on a path that just allocated
    // and copied the whole array.
    crate::immutable::republish_backing(vm, obj, Value::Array(arr));
}
mod dispatch;
pub(crate) use dispatch::*;
mod statics;
pub(crate) use statics::*;
mod pipeline;
pub(crate) use pipeline::*;
pub(crate) mod collections;
pub(crate) use collections::*;
mod helpers;
pub(crate) use helpers::*;
