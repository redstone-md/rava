//! Small helpers over guest collections: rooted iteration, element access, GC points.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::stream::*;

pub(crate) fn make_optional(vm: &mut Vm, val: Option<Value>) -> Value {
    if let Some(shared) = crate::optlite::intercept(vm, &val) { return shared; } // ADDED: `[OPTLITE]` census + the shared empty (RAVA_NO_OPT_LITE=1 declines, historic body below)
    let present = matches!(&val, Some(x) if !matches!(x, Value::Null));
    // ARENA OPTIONAL (RAVA_ARENA_SYNTH): `class_field_layout` injects `$val`/`$present` as
    // real cells, so the synthetic state no longer needs the heap side-table that forced this
    // object to be a handle. `try_arena_alloc_by_name` applies the same four vetoes the `new`
    // opcode does and returns None when any fires, so the handle path below stays the
    // fallback rather than a special case; `synth_put` serves both representations.
    #[cfg(feature = "next-gen-gc")]
    if let Some(p) = vm.try_arena_alloc_by_name("java/util/Optional") {
        let recv = Value::Obj(p);
        vm.synth_put(&recv, "$val", val.unwrap_or(Value::Null));
        vm.synth_put(&recv, "$present", Value::Int(present as i32));
        return recv;
    }
    let h = vm.heap.alloc("java/util/Optional");
    vm.heap.put_field_syn(h, crate::heap::Syn::Val, val.unwrap_or(Value::Null));
    vm.heap.put_field(h, "$present", Value::Int(present as i32));
    Value::Ref(h)
}

/// Read the `$elems` backing vector of a stream/list object.
pub(crate) fn elems_of(vm: &mut Vm, v: Option<&Value>) -> Vec<Value> {
    // A lazy (`$ops`) stream appearing as an ARGUMENT (flatMap sub-stream, concat
    // operand): materialise its fused chain here. Errors fall back to empty — the
    // fallible `stream_elems` covers the receiver path where a throwing mapper must
    // propagate; this arg path is the rare case (documented limitation while gated).
    if lazy_streams_enabled() {
        if let Some(Value::Ref(h)) = v {
            if matches!(vm.heap.get_field(*h, "$ops"), Value::Array(_)) {
                return fuse_materialize(vm, *h).unwrap_or_default();
            }
        }
    }
    if let Some(Value::Ref(h)) = v {
        if let Value::Array(arr) = vm.heap.get_field_syn(*h, crate::heap::Syn::Elems) {
            let mut vals = vm.heap.elems_values(*h, arr);
            // The SECOND raw `$elems` reader (the first is `Vm::iterate_object`), and
            // the one the bulk-copy constructors use: `new ArrayList<>(treeSet)` read
            // the backing array straight out and kept INSERTION order where the JVM
            // gives sorted. Same rule, same helper.
            let _ = crate::stream::collections::coll_op::order_sorted_set(
                vm, &Value::Ref(*h), &mut vals);
            return vals;
        }
        // A real (bytecode) collection — e.g. a guava Sets.union view or any jar
        // Collection — has no $elems; drain it through its own iterator()/size+get
        // so our native collection ops interoperate with it.
        return vm.iterate_object(*h).unwrap_or_default();
    }
    // ⭐**AN ARENA OBJECT IS A SOURCE TOO.** Every arm above tests `Value::Ref`, which was safe
    // while a collection this engine serves natively is always handle-born. A class OVERRIDDEN
    // BY BYTECODE is not native-served, so it is arena-born — and every bulk copy from one read
    // EMPTY: `new LinkedList<>(guestArrayList)` came back `[]` while the source printed
    // `[B, c, d]`. `iterate_recv` is the same drain, over a `Value` rather than a handle.
    #[cfg(feature = "next-gen-gc")]
    if let Some(val @ Value::Obj(_)) = v {
        return vm.iterate_recv(val).unwrap_or_default();
    }
    // A bare array source (varargs surfaces like Collections.addAll(c, T[])):
    // its ELEMENTS, not the array as a single element.
    if let Some(Value::Array(arr)) = v {
        return vm.heap.array_values(*arr).unwrap_or_default();
    }
    Vec::new()
}

/// `Objects.requireNonNull` over an `of()`/`copyOf()` factory's elements — the
/// JDK rejects a null element at the factory, not at first use.
pub(crate) fn reject_nulls(vm: &mut Vm, elems: &[Value]) -> Result<(), VmError> {
    if elems.iter().any(|e| matches!(e, Value::Null)) {
        return Err(crate::immutable::npe(vm));
    }
    Ok(())
}

/// `Set.of`'s duplicate check. Uses the same `ekey` value-identity the Set arms
/// of `coll_op` dedup by, so "duplicate" means here exactly what it means there.
pub(crate) fn reject_duplicate_elements(vm: &mut Vm, elems: &[Value]) -> Result<(), VmError> {
    // `ekey_mut`, not `ekey`: the guest-free half keys any object without a
    // `$str`/`$name`/`$path` by IDENTITY, so `Set.of(new P(1), new P(1))` — two
    // equal records — sailed through the duplicate check that every Set arm of
    // `coll_op` would have caught. Rooted, because deriving it can run a guest
    // `toString()`.
    let (vals, keys) = rooted_keys(vm, elems.to_vec());
    let mut seen = crate::FastSet::with_capacity_and_hasher(keys.len(), Default::default());
    for (v, k) in vals.iter().zip(keys.into_iter()) {
        if !seen.insert(k) {
            // The JDK's message names the ELEMENT, i.e. its `toString()` — not our
            // internal type-tagged key, whose `s:`/`o:` prefix would leak into
            // guest-visible text.
            let shown = vm.value_to_string(v).unwrap_or_else(|_| v.display());
            let msg = format!("duplicate element: {shown}");
            return Err(crate::immutable::iae(vm, &msg));
        }
    }
    Ok(())
}

/// `Set.copyOf`'s deduplication (first occurrence wins), by the same `ekey`.
pub(crate) fn dedup_elements(vm: &mut Vm, elems: Vec<Value>) -> Vec<Value> {
    // Same key as every other Set dedup in the tree (`dedup_by_ekey`), for the
    // same reason `reject_duplicate_elements` uses it.
    dedup_by_ekey(vm, elems)
}

/// `BufferedReader.lines()` — drain the underlying byte stream and split lines.
pub(crate) fn lines(vm: &mut Vm, reader: Option<&Value>) -> Value {
    let stream = match reader {
        Some(Value::Ref(h)) => match vm.heap.get_field(*h, "$stream") {
            Value::Ref(s) => s,
            _ => *h,
        },
        _ => return make_stream(vm, Vec::new()),
    };
    let buf = match vm.heap.get_field(stream, "$buf") {
        Value::Array(b) => b,
        _ => return make_stream(vm, Vec::new()),
    };
    let pos = vm.heap.get_field_syn(stream, crate::heap::Syn::Pos).as_int().unwrap_or(0) as usize;
    let all = vm.heap.array_bytes(buf).unwrap_or_default();
    vm.heap.put_field_syn(stream, crate::heap::Syn::Pos, Value::Int(all.len() as i32));
    let text = String::from_utf8_lossy(if pos < all.len() { &all[pos..] } else { &[] });
    let lines: Vec<Value> = text
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .map(|l| Value::Str(l.into()))
        .collect();
    // Java's lines() yields no trailing empty element for a final newline.
    let mut lines = lines;
    if matches!(lines.last(), Some(Value::Str(s)) if s.is_empty()) {
        lines.pop();
    }
    make_stream(vm, lines)
}

/// Heap-pressure collection safe to call MID-stream-op: the op-loop GC is gated off while a
/// stream/map dispatcher is on the stack (native_depth>0, natives.rs) because their state
/// lives in Rust locals — but `rooted_each` roots its `src` (and its callers root their
/// accumulators) in `gc_protect`, so a collection here forwards/keeps them correctly. This
/// lets the huge transient burst of a density-eval stream sweep (3.6M Iterators/Streams that
/// otherwise never collect until the op returns) be reclaimed incrementally. Fires only over
/// the real-memory re-arm mark, so cost is bounded.
pub(crate) fn stream_maybe_gc(vm: &mut Vm) {
    // OPT-IN (RAVA_STREAM_GC): a mid-stream collection reclaims the density-eval transient
    // burst that otherwise OOMs worldgen — but it runs while OUTER natives (map/worldgen)
    // may hold arena Values in Rust locals we do NOT root, so a nested collection here can
    // sweep one (observed: a Holder.value nulled → NPE in the tick loop). Default OFF keeps
    // the build byte-identical + safe; enabling it needs the enclosing natives GC-rooted too.
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("RAVA_STREAM_GC").is_some()) {
        return;
    }
    // ONLY when THIS stream op is the outermost native on the stack (native_depth==1, the
    // one bump from natives.rs:130). Then the sole non-frame roots are this op's own state,
    // which is fully gc_protect'd — a collection is safe. At depth≥2 an ENCLOSING native
    // (worldgen NoiseChunk, a map op, another stream) may hold an arena Value in a Rust
    // local we do not root; collecting there swept one and nulled a Holder.value (the
    // tick-loop NPE). Skipping depth≥2 keeps it safe; the outer op still collects the
    // accumulated inner-stream burst on its own iterations.
    if crate::frames::native_depth() != 1 {
        return;
    }
    let real = crate::mem_meter::live();
    let (gc_next, next_gc_real) = crate::vm::thread_context::gc_thresholds(vm);
    if real > next_gc_real {
        vm.gc();
        let real_post = crate::mem_meter::live();
        let next_gc_real = real_post + real_post.max(256 * 1024 * 1024);
        crate::vm::thread_context::set_gc_thresholds(vm, gc_next, next_gc_real);
    }
}

pub(crate) fn rooted_each<F>(vm: &mut Vm, elems: Vec<Value>, mut step: F) -> Result<(), VmError>
where
    F: FnMut(&mut Vm, Value) -> Result<std::ops::ControlFlow<()>, VmError>,
{
    let src = vm.heap.alloc_values(OBJ, elems);
    // Root `src` so `stream_maybe_gc` below (which runs while the op-loop GC is gated off)
    // cannot reclaim the not-yet-processed elements — the array is otherwise referenced
    // only by this Rust-local handle.
    let _gp = gc_protect_array(vm, src);
    let n = vm.heap.array_len(src).unwrap_or(0);
    for i in 0..n {
        let e = vm.heap.array_get(src, i).unwrap_or(Value::Null);
        if step(vm, e)?.is_break() {
            break;
        }
        stream_maybe_gc(vm);
    }
    Ok(())
}

/// Every element's `ekey_mut`, derived with the elements ROOTED — and with the
/// (possibly forwarded) elements handed back.
///
/// ⚠ THIS IS A GC BOUNDARY, and it became one the moment collection keys stopped
/// being `Value::display()`. `ekey_mut` escalates a value class to
/// `map::structural_key`, which renders the object's guest `toString()` — an
/// interpreter re-entry that ALLOCATES and can move the arena. A scan that walked
/// a bare Rust `Vec<Value>` while calling it therefore held stale pointers from
/// the first element that had a `toString` onwards: the documented worst failure
/// class in this tree, because it is silent.
///
/// The elements are parked in a heap array (visible to every root scan), read
/// back one at a time so each read is FORWARDED, and returned alongside their
/// keys so the caller can keep using them.
pub(crate) fn rooted_keys(vm: &mut Vm, elems: Vec<Value>) -> (Vec<Value>, Vec<String>) {
    let src = vm.heap.alloc_values(OBJ, elems);
    let _gp = gc_protect_array(vm, src);
    let n = vm.heap.array_len(src).unwrap_or(0);
    let mut keys = Vec::with_capacity(n);
    for i in 0..n {
        let e = vm.heap.array_get(src, i).unwrap_or(Value::Null);
        keys.push(ekey_mut(vm, &e));
    }
    // AFTER the derivations, not before: this is the forwarded view.
    let out = vm.heap.array_values(src).unwrap_or_default();
    (out, keys)
}

/// `rooted_keys` for the CONTENT key — the container-aware derivation the
/// value-equality arms compare by. Same rooting, same reason.
pub(crate) fn rooted_content_keys(vm: &mut Vm, elems: Vec<Value>) -> (Vec<Value>, Vec<String>) {
    let src = vm.heap.alloc_values(OBJ, elems);
    let _gp = gc_protect_array(vm, src);
    let n = vm.heap.array_len(src).unwrap_or(0);
    let mut keys = Vec::with_capacity(n);
    for i in 0..n {
        let e = vm.heap.array_get(src, i).unwrap_or(Value::Null);
        keys.push(crate::map::content_key(vm, &e));
    }
    let out = vm.heap.array_values(src).unwrap_or_default();
    (out, keys)
}

/// `rooted_each` variant handing the closure the ROOTED array + index instead of a
/// pre-read element copy. A closure whose element is used AFTER an `apply` (filter's
/// out-push, toMap's valfn arg, groupingBy's bucket push) must RE-READ it from the
/// array — the pre-read Rust binding goes stale if that apply's guest code triggered
/// a moving collection (the same class as the map.forEach snapshot gap).
pub(crate) fn rooted_each_idx<F>(vm: &mut Vm, elems: Vec<Value>, mut step: F) -> Result<(), VmError>
where
    F: FnMut(&mut Vm, u32, usize) -> Result<std::ops::ControlFlow<()>, VmError>,
{
    let src = vm.heap.alloc_values(OBJ, elems);
    let n = vm.heap.array_len(src).unwrap_or(0);
    for i in 0..n {
        if step(vm, src, i)?.is_break() {
            break;
        }
    }
    Ok(())
}

/// `rooted_each` that also accumulates each element's outputs into a SECOND rooted
/// heap `Object[]`, so the growing result survives the next element's `apply` too.
/// `step` pushes 0..N results per element (map/filter/peek/flatMap). Returns the
/// accumulated values.
pub(crate) fn rooted_pipe<F>(vm: &mut Vm, elems: Vec<Value>, mut step: F) -> Result<Vec<Value>, VmError>
where
    F: FnMut(&mut Vm, Value, &mut Vec<Value>) -> Result<(), VmError>,
{
    let out = vm.heap.alloc_values(OBJ, Vec::new());
    // Root the growing result: `rooted_each`'s mid-sweep GC would otherwise reclaim it.
    let _gp = gc_protect_array(vm, out);
    rooted_each(vm, elems, |vm, e| {
        let mut pushed = Vec::new();
        step(vm, e, &mut pushed)?;
        for v in pushed {
            vm.heap.array_push(out, v);
        }
        Ok(std::ops::ControlFlow::Continue(()))
    })?;
    Ok(vm.heap.array_values(out).unwrap_or_default())
}

/// The deferred-op kind for a linear per-element intermediate op, or None if the op
/// must materialise (terminals, barriers like sorted/distinct/flatMap/limit/skip).
pub(crate) fn lazy_defer_kind(name: &str) -> Option<i32> {
    match name {
        "map" | "mapToObj" | "mapToInt" | "mapToLong" | "mapToDouble" => Some(LAZY_MAP),
        "filter" => Some(LAZY_FILTER),
        "peek" => Some(LAZY_PEEK),
        _ => None,
    }
}

/// Census for the native stream `map` loop - see the call site. `RAVA_STREAM_MAP_STAT`.
///
/// ⛔ ITS ANSWER, 2026-07-29: **160,000 map calls, 629,717 elements = 3.9 elements per call**, the
/// sampled distribution almost entirely n=2..8 with a maximum of 125. Forking a four-element map
/// costs more in task dispatch and region setup than the four lambda calls it replaces, so the
/// "fork the native stream map" idea is refuted by SIZE, before any build. Kept because the number
/// is worth being able to reproduce, not because the lever is open.
///
/// ⚠ Three defects in the first version of this counter, all mine: `fork_min_n()` defaults to 2 so
/// its "big" bucket meant nothing; `value_class_name` on a lambda returns the generic `$Lambda`, so
/// naming the mapper was useless; and doing that lookup plus frequent `eprintln` per call took the
/// boot to 149s. A census must be cheap enough that the run it measures is still the run you meant.
pub(crate) fn stream_map_stat(n: usize) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("RAVA_STREAM_MAP_STAT").is_some()) {
        return;
    }
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    static CALLS: AtomicU64 = AtomicU64::new(0);
    static ELEMS: AtomicU64 = AtomicU64::new(0);
    // Buckets: <4, <16, <64, <256, >=256 - the only question is whether a fork could ever pay.
    static B: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];
    let c = CALLS.fetch_add(1, Relaxed) + 1;
    ELEMS.fetch_add(n as u64, Relaxed);
    let b = match n { 0..=3 => 0, 4..=15 => 1, 16..=63 => 2, 64..=255 => 3, _ => 4 };
    B[b].fetch_add(1, Relaxed);
    if c % 50_000 == 0 {
        eprintln!(
            "[STREAM-MAP] {c} calls, {} elements ({:.1}/call) | n<4:{} n<16:{} n<64:{} n<256:{} n>=256:{}",
            ELEMS.load(Relaxed),
            ELEMS.load(Relaxed) as f64 / c as f64,
            B[0].load(Relaxed), B[1].load(Relaxed), B[2].load(Relaxed),
            B[3].load(Relaxed), B[4].load(Relaxed)
        );
    }
}

/// `collect(collector)` — honour the Collectors marker ($kind); an unmarked
/// collector (a real one, e.g. guava's `ImmutableMap.toImmutableMap`) is driven
/// through its `Collector` interface; default toList.
pub(crate) fn collect(vm: &mut Vm, elems: Vec<Value>, collector: Option<&Value>) -> Result<Value, VmError> {
    // GAP-P02 probe (RAVA_COLLECT_PROBE): per-call kind/size/duration attribution
    // for the dominant Stream#collect bridge bucket. Inert when unset.
    let probe_t = collect_probe_enabled().then(std::time::Instant::now);
    let probe_n = elems.len();
    let probe_kind = if probe_t.is_some() {
        match collector {
            Some(Value::Ref(h)) => match vm.heap.get_field(*h, "$kind") {
                Value::Str(s) => s.to_string(),
                _ => "collector-obj".to_string(), // real Collector driven via bytecode
            },
            _ => "list".to_string(),
        }
    } else {
        String::new()
    };
    let out = collect_inner(vm, elems, collector);
    if let Some(t) = probe_t {
        eprintln!("[COLLECT] kind={probe_kind} n={probe_n} dur_us={}", t.elapsed().as_micros());
    }
    out
}

/// True for JDK collection/queue/deque/list/set types (these reach the native
/// layer only when not present as bytecode in the jar).
pub(crate) fn is_collection(o: &str) -> bool {
    // BitSet contains the substring "Set" but is NOT a java.util.Collection;
    // its bit-vector methods (get/set/...) must not route to collection ops.
    if o == "java/util/BitSet" {
        return false;
    }
    // Seven substring searches over a 40-60 byte class name, asked on the dispatch path, the
    // JIT native-served gate AND once per super-chain level of every object born. RIP
    // sampling puts this call alone at ~3.5% of blocks A+B. `owner_bits` answers all seven
    // from one memo entry per distinct name; with the flag off it runs the same scans.
    if !crate::owner_bits::contains_any(o, crate::owner_bits::COLLECTION_ANY) {
        return false;
    }
    // Same collision as the BitSet row above, one family wider: a JDK THROWABLE is
    // not a collection. `java/lang/StackOverflowError` contains "Stack", so the
    // needle test claimed it and `coll_op` answered `toString()` from an empty
    // `$elems` — the guest read "[]" where HotSpot prints
    // "java.lang.StackOverflowError". The stream stage runs BEFORE the throwable
    // arm, so the collision silently won.
    //
    // The test is the throwable arm's own predicate (natives/cascade_owners.rs),
    // narrowed to the JDK namespaces so no GUEST class changes storage class:
    // those names are unloadable, and the allocator already forces a heap handle
    // for every unloadable class, so declining them here cannot un-handle one.
    !((o.ends_with("Exception") || o.ends_with("Error"))
        && (o.starts_with("java/")
            || o.starts_with("javax/")
            || o.starts_with("jdk/")
            || o.starts_with("sun/")))
}

/// Content-aware element key for collection comparisons — the guest-frame-free
/// half, callable behind `&Vm`.
///
/// WAS `Value::display()` with a heap-String `$str` short-circuit, i.e. the
/// element's RENDERED TEXT. Rendered text is not a Java equality: `Integer 1`
/// and `"1"` both render `1`, so `List.of(1).contains("1")` was TRUE, and
/// `indexOf`/`remove`/`containsAll`/`removeAll`/`retainAll`/`equals` all
/// inherited the same cross-type collision. `java.util.Map` keys in this tree
/// solved exactly this problem years ago by TYPE-TAGGING the key (`map::key_str`
/// -> `i:` int, `j:` long, `s:` string, `o:` value object, `c:`/`m:` structural,
/// `@` identity), so collections now derive their element key with the SAME
/// function instead of a second, weaker one. `Value::Str` and a heap `$str`
/// String still key identically (`s:text`) - the property the old short-circuit
/// existed for, and null keys as the literal `null`, which no other tag emits.
///
/// `&Vm` deliberately: this half runs no guest bytecode, which is what lets
/// `Vm::ws_list_contains` serve a worker-shared `contains` unlocked. Elements
/// that key to the `@` identity fallback here need the guest `toString()`
/// derivation, and that lives in `ekey_mut` below.
pub(crate) fn ekey(vm: &Vm, v: &Value) -> String {
    crate::map::key_str(vm, v)
}

/// Is `key` one whose OWNER CLASS still has to be consulted - the `@h`/`@aN`
/// identity fallback, or the `o:` shortcut `ekey` takes for a synthetic
/// `$name`/`$path` wrapper? Everything else (`s:` `i:` `j:` `c:` `m:` `null`, a
/// float's text) is already a complete value key derived without guest code.
pub(crate) fn ekey_needs_class(key: &str) -> bool {
    key.starts_with('@') || key.starts_with("o:")
}

/// The pure-identity element key, in the exact `@` shapes `ekey` emits: a handle
/// as `@h`, an arena object by its GC-STABLE identity hash (never its address,
/// which moves), and a pinned mirror as the handle it mirrors - so one object
/// keys the same through either representation.
fn identity_ekey(vm: &Vm, v: &Value, cheap: String) -> String {
    match v {
        Value::Ref(h) => format!("@{h}"),
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) => match crate::nextgen::pinned_handle_of(p.addr()) {
            Some(h) => format!("@{h}"),
            None => format!("@a{}", crate::nextgen::arena_identity_hash(*p)),
        },
        _ => {
            let _ = vm;
            cheap
        }
    }
}

/// Element key with the map side's FULL derivation, for the paths that hold
/// `&mut Vm`.
///
/// `ekey` alone still compares a guest object that overrides `equals` (a record,
/// an Identifier-like value type) by its HANDLE, so `set.add(new P(1))` twice
/// left two entries and `list.contains(new P(1))` was false. `map::structural_key`
/// is the existing answer to that: it keys a class with a substantive `equals`
/// override by its `toString()` (`o:...`) and drains a map-shaped key structurally,
/// leaving a true identity class on `@h`.
///
/// The escalation is decided BY THE CLASS'S OWN `equals` BYTECODE
/// (`map::equals_kind`), never by which synthetic field an object carries. That
/// distinction is load-bearing in BOTH directions: `java.lang.Thread` carries a
/// `$name`, so the bare `o:{$name}` shortcut merged two distinct threads both
/// called "worker" into one Set entry; `java.io.File` carries a `$path` and
/// genuinely IS path-equal, so it must keep `o:{$path}`. Composite (`c:`/`m:`) and
/// scalar (`s:`/`i:`/`j:`) keys are complete on their own and never reach this,
/// which is why `ekey` and `ekey_mut` agree on every element the guest-free half
/// can key by value.
/// The content key for `v` **only if it is already derived** — never derives one.
///
/// ⭐**THIS IS THE MISSING HALF OF THE `Objects.equals` FAST PATH, AND BOTH HALVES ARE
/// MEASURED.** Deciding `Objects.equals(a, b)` by comparing content keys is right when the keys
/// exist: the comparison is two string compares and the guest frame is saved. It is wrong when
/// they do not, because deriving one runs the class's `toString()` — for a `DoubleList` that is a
/// full text rendering of every element, through `StringBuilder`, twice per call — where the
/// guest's own `equals` would have compared sizes and short-circuited.
///
/// The two workloads say exactly that, in opposite directions, same binary, same flag:
///
/// | | `bench/ShapeOr.java` | product boot |
/// |---|---|---|
/// | key comparison, deriving on miss | 14.013 / 13.711 s | 48.6 / 48.6 s |
/// | guest `equals` instead | **10.927 / 11.341 s** | 49.9 / 50.8 s |
///
/// −19.7% on one and +3.4% on the other, both disjoint and both bit-exact (re-verified over six
/// runs: 13.670/13.562/13.626 against 10.941/11.246/11.077 = −18.6%).
///
/// ⭐**AND THE CENSUS NAMES IT EXACTLY** (`--features contract-stat`, `RAVA_CONTRACT_STAT=1`):
///
/// ```text
/// Objects.equals 400000 calls | quick 0 (0.0%) | identity-fastpath 400000 (100.0%) | GUEST FRAME 0
/// ekey_mut       500000 calls | cheap 2 (0.0%) | memo-hit 0 (0.0%) | derived 499998 (100.0%)
/// ```
///
/// **Every call derives, and the memo hits ZERO times.** The boot's objects RECUR, so the same
/// derivation is an investment; ShapeOr builds a fresh pair per merge and the memo can never pay.
///
/// ⛔Two candidate causes were built and both measured ZERO, so neither is it: a size bound on
/// the derived key (`RAVA_EQ_KEY_CAP`, since removed — a 32-byte cap changed nothing, so the cost
/// is not the key's LENGTH) and an allocation-free class classification
/// (`RAVA_NO_EQ_CHEAP_KIND`, kept as strictly-less-work — so it is not the class-name `String`
/// either). What is left is the derivation itself: a guest `toString()` re-entry, twice per call.
///
/// ⚠**AND THE CENSUS WAS INERT BEFORE IT SAID ANY OF THIS.** `contract_stat::enabled` opens with
/// `if !cfg!(feature = "contract-stat") { return false }`, so with only the env var set it counts
/// nothing — and "fewer than 5,000 calls" was concluded from that silence before a run with 4 M
/// known calls printed nothing either and gave it away.
///
/// So the rule is not "which workload" — it is **a cache hit is a fast path and a cache miss is
/// not**. Peek at both memos; compare only if both answer.
pub(crate) fn ekey_peek(vm: &Vm, v: &Value) -> Option<String> {
    match v {
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) if crate::nextgen::pinned_handle_of(p.addr()).is_none() => {
            arena_key_memo(crate::nextgen::arena_identity_hash(*p), None)
        }
        Value::Ref(h) => match vm.heap.get_field_syn(*h, crate::heap::Syn::KeyStr) {
            Value::Str(s) => Some(s.to_string()),
            _ => None,
        },
        _ => None,
    }
}

pub(crate) fn ekey_mut(vm: &mut Vm, v: &Value) -> String {
    // MEMOISED FOR ARENA OBJECTS, and the boot is what made it mandatory: a heap
    // `Value::Ref` carries the derived key on its own `$keystr` field, so a value
    // object is rendered once and read back after; an ARENA object has no synthetic
    // field, and `structural_key` says so ("recomputed, which is fine for the rare
    // case") — but a collection element is not the rare case. Escalating an arena
    // object on every scan measured **Done 18.50 -> 26.85 s and wall 59.3 -> 67.5**
    // over three paired boots each, DISJOINT; restricting the escalation to heap
    // handles gave that back in full (Done 20.14, wall 58.3) and lost the record fix,
    // because MC's records ARE arena objects. So the cost is the re-derivation, not
    // the escalation, and the fix is the memo the heap side already has.
    //
    // Keyed by the arena identity hash, which is stamped once and survives every
    // collection — and which `identity_ekey` above ALREADY treats as an
    // object-unique token, so this is no weaker an assumption than what ships. The
    // stored key is the object's value rendering, immutable for exactly the classes
    // that reach here (a substantive `equals` override), the same immutability
    // `$keystr` assumes on the heap side.
    #[cfg(feature = "next-gen-gc")]
    let arena_id = match v {
        Value::Obj(p) if crate::nextgen::pinned_handle_of(p.addr()).is_none() => {
            Some(crate::nextgen::arena_identity_hash(*p))
        }
        _ => None,
    };
    #[cfg(not(feature = "next-gen-gc"))]
    let arena_id: Option<i32> = None;
    if let Some(id) = arena_id {
        if let Some(k) = arena_key_memo(id, None) {
            crate::contract_stat::ekey_outcome(crate::contract_stat::EkeyOutcome::MemoHit);
            return k;
        }
    }
    // THE CHEAP KEY IS BUILT AFTER THE MEMO IS CONSULTED, not before. The census
    // (`RAVA_CONTRACT_STAT`) counted 750,000 calls here in one boot of which
    // **96.8% were memo hits, 2.3% answered cheaply and 0.9% derived** — so in the
    // dominant case the old order rendered a full `key_str` on the way in and threw
    // it away. Reading through to the memo first costs the 2.3% one probe that
    // misses (they never store an entry) and saves the 96.8% a whole derivation.
    let cheap = ekey(vm, v);
    if !ekey_needs_class(&cheap) {
        crate::contract_stat::ekey_outcome(crate::contract_stat::EkeyOutcome::Cheap);
        return cheap;
    }
    let class = vm.value_class_name(v);
    let derived = match crate::map::equals_kind(vm, &class) {
        // Proven content equality -> the map's full derivation (`o:{toString}` for a
        // value class, structural for a map/collection-shaped element).
        crate::map::EqualsKind::Value => crate::map::structural_key(vm, v),
        // Proven `this == that` -> identity, whatever synthetic text it carries.
        crate::map::EqualsKind::Identity => identity_ekey(vm, v, cheap),
        // NOTHING PROVEN - a natively-served class whose `equals` bytecode this VM
        // never loaded. Key it by identity, which is what a collection element of
        // such a class did before any of this. The `o:{$name}`/`o:{$path}` shortcut
        // is NOT usable as the tiebreak: those two synthetic fields carry the value
        // identity of a `Charset`/`File` but only a mutable attribute of a
        // `java.lang.Thread`, and nothing in the bytecode separates the two. Merging
        // two distinct same-named threads into one Set entry is the worse error, so
        // the unproven case stays identity and `Charset`/`File` value equality is
        // reported rather than guessed at.
        crate::map::EqualsKind::Unknown => identity_ekey(vm, v, cheap),
    };
    if let Some(id) = arena_id {
        arena_key_memo(id, Some(&derived));
    }
    crate::contract_stat::ekey_outcome(crate::contract_stat::EkeyOutcome::Derived);
    derived
}

/// Cap on the arena element-key memo. Past this many entries the whole table is
/// dropped: every entry is re-derivable, so a flush costs time and never an
/// answer — the same rebuild-on-miss contract `Vm::set_index` runs under.
const ARENA_KEY_MEMO_CAP: usize = 65_536;

/// Read (`store = None`) or write the arena element-key memo. One thread-local
/// table, because the derivation it caches runs on the element path and must not
/// take a lock.
fn arena_key_memo(id: i32, store: Option<&str>) -> Option<String> {
    thread_local! {
        static MEMO: std::cell::RefCell<crate::FastMap<i32, String>> =
            std::cell::RefCell::new(crate::FastMap::default());
    }
    MEMO.with(|m| match store {
        None => m.borrow().get(&id).cloned(),
        Some(k) => {
            let mut t = m.borrow_mut();
            if t.len() >= ARENA_KEY_MEMO_CAP {
                t.clear();
            }
            t.insert(id, k.to_string());
            None
        }
    })
}

