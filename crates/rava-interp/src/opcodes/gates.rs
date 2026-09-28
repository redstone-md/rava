//! Which classes and signatures the interpreter serves natively rather than by bytecode.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::opcodes::*;

// ⭐⭐**THE INTERPRETER'S GATE WORD — FOURTEEN PROCESS CONSTANTS, ONE PER-THREAD LOAD.**
//
// KILL SWITCH `RAVA_NO_OP_GATEWORD=1` (also honoured through `crate::flags::emit_more_disabled`).
// Default ON. With it set, `op_gates_cold` resolves ONLY the switch itself, stamps `OG_LIVE`, and
// every accessor below then calls exactly the `OnceLock` it always called, from exactly the site
// it always called it from — an honest A/B, because the off arm IS the historic code.
//
// WHAT WAS WRONG. `run_hot` already hoists its four opcode gates to the top of the dispatch loop
// (see `fast_gates` and `hot.rs`), and that is where the "the opcodes module has a gates word"
// story stopped. But **`run_hot` is not entered once per method — it is entered once per RUN of
// opcodes it can serve**, and the census in `hot.rs` prices that run at **6.63 opcodes**
// (`bench/Allocs.java`, 822,503,037 opcodes over 124,000,057 entries, row 378). So the "hoisted"
// gates were still ~1.4 acquire loads per opcode EXECUTED, and every gate OUTSIDE that loop — the
// call path, the allocation path, the type-test path, the throw path — was never folded at all
// and paid its own acquire load per operation:
//
//     invokestatic / invokespecial     3   (call_plan x2 on the miss path, gson_native x1)
//     invokevirtual / invokeinterface  2   (fork_route_foreach || fork_target_scan)
//     backward branch                  2   (fork_numeric || fork_target_scan)
//     new / anewarray                  2   (alloc_route_memo, re-read on the insert side)
//     instanceof / checkcast           1
//     arena getfield / putfield        1   (trace_stalefield, inside field_diag_armed's arm)
//     native_served_prefix             1   on 10,029,228 calls per boot (counted, mod.rs)
//     run_hot entry                    1   (fast_gates)
//     exception-table entry covering pc, per throw, per frame unwound      2
//
// **The cost is not the `mov`.** `*OnceLock::get_or_init(..)` reaches its value through an
// ACQUIRE load; on x86 that IS a plain `mov`, but as an atomic it is an OPTIMISATION BARRIER —
// LLVM may not hoist it out of the caller's loop, may not fold two sibling reads of one call into
// one, and must assume tracked memory is clobbered across it. Row 409 priced exactly this shape
// from the other side: ONE relaxed atomic taken off the operand-push path measured **-15%**, and
// putting it back **+17%**.
//
// THE CURE, copied from three places in this tree that already have it —
// `natives::entry::entry_gates()` (one `u8`, read at the bridge and PASSED DOWN),
// `frames::POOL_GATES` and `vmframe::FRAME_GATES`: a per-thread `Cell<u32>` whose initialiser is a
// `const` block. That is the one combination `std` lowers to a bare `#[thread_local]` static — no
// lazy-init check, no destructor, nothing to call — so a gate read becomes a plain load LLVM is
// free to hoist, to fold with its siblings and to keep in a register across a whole loop.
//
//     was: one ACQUIRE-ordered load per gate per operation (the table above);
//      is: one plain per-thread load per operation, shared by every gate that operation reads,
//          and zero once LLVM hoists it out of the enclosing loop.
//
// What grows is interpreted operations per boot: 14.1 M method bodies and ~197 M interpreted
// opcodes, at 19.4% of the boot.
//
// **NOTHING ABOUT THE ANSWERS CHANGES.** Every bit is filled from the very same `OnceLock` that
// used to be read at the site, on a cold path that runs once per thread. There is no key, no
// lookup, no miss and no invalidation — this is not a memo and not a side table, it is one word
// per thread holding booleans that are constants of the process.
//
// ⚠**ONLY PURE `env::var_os` GATES ARE IN THE WORD.** The cold path resolves them all eagerly, so
// a gate whose initialiser has a SIDE EFFECT would have that side effect moved earlier.
// `invoke_hooks_armed` is the one deliberately left OUT for that reason (its inputs
// `callcensus::enabled` / `vecgrow::enabled` arm watchdog THREADS) — and it needs nothing here
// anyway: it is ALREADY five gates folded into one read, the shape this word generalises.
// `fork_min_n` is a `usize`, not a bit, and is read only on the cold fork-decision path.
//
// ⚠**NO GATE MOVED.** Every bit is read at the same site, in the same order, guarding the same
// branch as before; only the LOAD changed. The two side-effecting positions in this file
// (`collect_missing`'s SEEN insert, `find_handler`'s trace prints) are untouched.

/// Bit 0 — this thread has resolved. A word of 0 therefore means "not yet", which is what makes
/// the fast read a single compare against zero.
pub(crate) const OG_RESOLVED: u32 = 1 << 0;
/// Bit 1 — the kill switch is engaged: distrust every other bit and call the `OnceLock`s from the
/// sites that used to call them, as the historic code did.
pub(crate) const OG_LIVE: u32 = 1 << 1;
/// Bit 2 — [`fast_gates`]: the hoisted-opcode-gate / namespace-first shape is on.
pub(crate) const OG_FAST: u32 = 1 << 2;
/// Bit 3 — `call_plan_enabled` (`RAVA_NO_CALL_PLAN`).
pub(crate) const OG_CALL_PLAN: u32 = 1 << 3;
/// Bit 4 — `instanceof_memo_enabled` (`RAVA_NO_INSTANCEOF_MEMO`).
pub(crate) const OG_IOF_MEMO: u32 = 1 << 4;
/// Bit 5 — `alloc_route_memo_enabled` (`RAVA_NO_ALLOC_ROUTE_MEMO`).
pub(crate) const OG_ALLOC_MEMO: u32 = 1 << 5;
// The five bits below are read only from `next-gen-gc`-gated accessors; without the feature the
// gates they mirror do not exist, so the constants are legitimately unreferenced.
/// Bit 6 — `gson_native_enabled` (`RAVA_GSON_NATIVE`).
#[allow(dead_code)]
pub(crate) const OG_GSON: u32 = 1 << 6;
/// Bit 7 — `fork_route_foreach` (`RAVA_FORK_SHAPES`).
#[allow(dead_code)]
pub(crate) const OG_FORK_FOREACH: u32 = 1 << 7;
/// Bit 8 — `fork_target_scan` (`RAVA_FORK_TARGET_SCAN`).
#[allow(dead_code)]
pub(crate) const OG_FORK_SCAN: u32 = 1 << 8;
/// Bit 9 — `fork_numeric` (`RAVA_FORK_NUMERIC`).
#[allow(dead_code)]
pub(crate) const OG_FORK_NUMERIC: u32 = 1 << 9;
/// Bit 10 — `fork_parallel` (`RAVA_FORK_PARALLEL`).
#[allow(dead_code)]
pub(crate) const OG_FORK_PARALLEL: u32 = 1 << 10;
/// Bit 11 — `trace_stalefield` (`RAVA_TRACE_STALEFIELD`).
pub(crate) const OG_STALEFIELD: u32 = 1 << 11;
/// Bit 12 — `RAVA_NSN_COUNT`, the `native_served_prefix` call counter.
pub(crate) const OG_NSN_COUNT: u32 = 1 << 12;
/// Bit 13 — `RAVA_TRACE_CATCH`, read inside [`find_handler`]'s table walk.
pub(crate) const OG_TRACE_CATCH: u32 = 1 << 13;
/// Bit 14 — `RAVA_TRACE_MAINCATCH`, its sibling at the same site.
pub(crate) const OG_TRACE_MAIN: u32 = 1 << 14;

thread_local! {
    /// This thread's resolved opcode gates; see the block comment above. `Cell<u32>` with a
    /// `const` initialiser, deliberately — the same shape `frames::POOL_GATES` and
    /// `vmframe::FRAME_GATES` use for a TLS slot with no destructor and no init check.
    static OP_GATES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// The word. One plain per-thread load; never 0 on return.
#[inline(always)]
pub(crate) fn op_gates() -> u32 {
    let g = OP_GATES.get();
    if g != 0 { g } else { op_gates_cold() }
}

/// First touch on this thread: read the process constants through their `OnceLock`s — the ONLY
/// place that still happens under the default — and cache the answers. Once per thread, so the
/// environment lookups behind them stay exactly as many as they were.
#[cold]
#[inline(never)]
fn op_gates_cold() -> u32 {
    static LIVE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let live = *LIVE.get_or_init(|| {
        std::env::var_os("RAVA_NO_OP_GATEWORD").is_some() || crate::flags::emit_more_disabled()
    });
    let mut g = OG_RESOLVED;
    if live {
        // Resolve NOTHING else: with the switch engaged each accessor must reach its own
        // `OnceLock` from its own site, in its own order, exactly as before.
        g |= OG_LIVE;
        OP_GATES.set(g);
        return g;
    }
    if fast_gates_uncached() { g |= OG_FAST; }
    if crate::opcodes::call_plan_uncached() { g |= OG_CALL_PLAN; }
    if crate::opcodes::instanceof_memo_uncached() { g |= OG_IOF_MEMO; }
    if crate::opcodes::alloc_route_memo_uncached() { g |= OG_ALLOC_MEMO; }
    if crate::opcodes::trace_stalefield_uncached() { g |= OG_STALEFIELD; }
    if crate::opcodes::nsn_count_uncached() { g |= OG_NSN_COUNT; }
    #[cfg(feature = "next-gen-gc")]
    {
        if crate::opcodes::gson_native_uncached() { g |= OG_GSON; }
        if crate::opcodes::fork_route_foreach_uncached() { g |= OG_FORK_FOREACH; }
        if crate::opcodes::fork_target_scan_uncached() { g |= OG_FORK_SCAN; }
        if crate::opcodes::fork_numeric_uncached() { g |= OG_FORK_NUMERIC; }
        if crate::opcodes::fork_parallel_uncached() { g |= OG_FORK_PARALLEL; }
    }
    if trace_catch_uncached() { g |= OG_TRACE_CATCH; }
    if trace_main_uncached() { g |= OG_TRACE_MAIN; }
    OP_GATES.set(g);
    g
}

/// Read one bit of an ALREADY-LOADED word — the `entry_gates()` discipline: a callee that needs
/// three gates takes the caller's word as an argument instead of taking three acquire loads.
/// `live` is the historic `OnceLock` reader, called only under the kill switch.
#[inline(always)]
pub(crate) fn op_gate_of(g: u32, bit: u32, live: fn() -> bool) -> bool {
    if g & OG_LIVE != 0 { live() } else { g & bit != 0 }
}

/// Read one bit, loading the word first. Use [`op_gate_of`] wherever one operation reads two or more
/// bits, so the load is shared.
#[inline(always)]
pub(crate) fn op_gate(bit: u32, live: fn() -> bool) -> bool {
    op_gate_of(op_gates(), bit, live)
}

/// ⭐**THE PREVIOUS ROUND'S KILL SWITCH — `RAVA_NO_FAST_GATES=1` (and `RAVA_NO_EMIT_MORE=1`)
/// restore that round's shape exactly.** Default ON.
///
/// Two gates hang off it, and both are the same defect wearing different clothes: a predicate
/// that recomputes, per event, an answer that is a CONSTANT of something much larger than the
/// event.
///
///   * [`force_native_collection`] ran its whole special-case chain — fourteen arms of
///     `starts_with` / `ends_with` / multi-way string `matches!`, plus `map::is_fastutil_objmap`'s
///     own seven — for every class it was ever asked about, including the `net/minecraft/*` and
///     `java/*` classes that no arm can match. **Was O(special cases) string compares per ask; is
///     at most four, and what grows is the number of jar workarounds this engine carries.**
///   * `hot::run_hot` read four `OnceLock` gates from inside the DISPATCH LOOP's match guards, so
///     an acquire atomic load stood in front of every `getfield` / `putfield` / `ldc` / `idiv` the
///     interpreter executed. **Was O(1) atomic per such opcode; is O(1) per entry into the loop.**
///
/// The value is a process constant either way, so the switch selects only WHERE it is read, never
/// what it says. That is the point: nothing observable can differ.
///
/// ⭐**AND NOW IT IS NOT AN ATOMIC AT ALL.** The read is [`OG_FAST`] out of [`op_gates`] — a plain
/// per-thread load, foldable with every other gate the same operation reads. Its two callers are
/// `force_native_collection` below (once per force-route ask) and `run_hot`'s loop preamble (once
/// per ~6.63 opcodes executed, per the census in `hot.rs`), so this alone removes one acquire load
/// per ~6.63 interpreted opcodes.
#[inline]
pub(crate) fn fast_gates() -> bool {
    op_gate(OG_FAST, fast_gates_uncached)
}

/// The historic reader, kept verbatim: the word is filled from it, and the kill-switch arm calls
/// it per ask exactly where [`fast_gates`] used to.
pub(crate) fn fast_gates_uncached() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_GATES").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// `RAVA_TRACE_CATCH` — the historic reader for [`OG_TRACE_CATCH`]. See [`find_handler`].
fn trace_catch_uncached() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_TRACE_CATCH").is_some())
}

/// `RAVA_TRACE_MAINCATCH` — the historic reader for [`OG_TRACE_MAIN`].
fn trace_main_uncached() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_TRACE_MAINCATCH").is_some())
}

/// Collect-mode (RAVA_COLLECT): instead of aborting on an unsupported native,
/// log each distinct one once and let the caller substitute a default return so
/// the boot stumbles forward and surfaces MANY missing natives in one run (for
/// batched, parallel fixing). Returns true when collect-mode is active (caller
/// must then push the default + continue instead of erroring).
pub(crate) fn collect_missing(sig: &str) -> bool {
    thread_local! {
        static ON: std::cell::Cell<i8> = const { std::cell::Cell::new(-1) };
        static SEEN: std::cell::RefCell<std::collections::HashSet<String>> =
            std::cell::RefCell::new(std::collections::HashSet::new());
    }
    let on = ON.with(|c| {
        let v = c.get();
        if v >= 0 { return v == 1; }
        let on = std::env::var_os("RAVA_COLLECT").is_some();
        c.set(on as i8);
        on
    });
    if on {
        SEEN.with(|s| {
            if s.borrow_mut().insert(sig.to_string()) {
                eprintln!("[MISSING] {sig}");
            }
        });
    }
    on
}

/// I/O stream classes whose state is modeled through synthetic heap fields
/// ($buf/$stream/$pos/$path/$sb). They must remain handle-backed; arena streams do not
/// carry the fields read by the native stream layer.
pub(crate) fn is_native_io_class(c: &str) -> bool {
    IO_CLASSES_EXACT.contains(&c)
}


/// The exact names of [`is_native_io_class`]'s JDK arm, shared with owner facts.
pub(crate) const IO_CLASSES_EXACT: &[&str] = &[
    "java/io/InputStream",
    "java/io/FilterInputStream",
    "java/io/BufferedInputStream",
    "java/io/DataInputStream",
    "java/io/ByteArrayInputStream",
    "java/io/FileInputStream",
    "java/io/PushbackInputStream",
    "java/io/SequenceInputStream",
    "java/io/OutputStream",
    "java/io/FilterOutputStream",
    "java/io/BufferedOutputStream",
    "java/io/DataOutputStream",
    "java/io/ByteArrayOutputStream",
    "java/io/FileOutputStream",
    "java/io/Reader",
    "java/io/BufferedReader",
    "java/io/InputStreamReader",
    "java/io/FilterReader",
    "java/io/StringReader",
    "java/io/Writer",
    "java/io/BufferedWriter",
    "java/io/OutputStreamWriter",
    "java/util/zip/GZIPInputStream",
    "java/util/zip/InflaterInputStream",
    "java/util/zip/GZIPOutputStream",
    "java/util/zip/DeflaterOutputStream",
];

pub(crate) fn force_native_collection(vm: &Vm, class: &str, name: &str, _descriptor: &str) -> bool {
    // The netty `execute` rows join `inEventLoop` as the SECOND structural
    // exception to the namespace guard: their population is the exact
    // ThreadPerTaskExecutor tuple plus the `EventExecutorGroup` closure (the
    // provider table in net/mod.rs), so their owners live in no fixed namespace.
    // GIL-FREE WORKERS cannot load, so their `type_closure` can be a fragment —
    // and a fragment can only ever MISS the root. The conservative arm therefore
    // answers false for every ask the exact TPTX compare above did not settle —
    // the only verdict that cannot be wrong without the loader; the structural
    // answer only ever narrows the owning thread's decision.
    if name == "execute" {
        if class == crate::net::THREAD_PER_TASK_EXECUTOR {
            return _descriptor == crate::net::THREAD_PER_TASK_EXECUTE_DESC;
        }
        if !crate::parallel::gilfree()
            && crate::net::dispatch_execute_action(vm, class, name, _descriptor)
                != crate::net::ExecuteAction::Decline
        {
            return true;
        }
    }
    // ⭐**THE NAMESPACE ANSWER FIRST — EVERY ARM BELOW HAS AN EXPLICIT ADMISSION CONDITION.**
    //
    // This predicate is fourteen workarounds deep and grows by one every time a jar needs
    // rescuing, yet every ask paid for all of them: `net/minecraft/world/level/block/Block` walked
    // five `io/netty/` prefixes, an `ends_with`, four guava name compares, `is_fastutil_objmap`'s
    // own seven-way `matches!`, and two more `==` — to answer `false`, which the FIRST byte of the
    // name already decided. Row 185 caught the cost from the other end: profiling ONE interpreted
    // static call put `str::pattern::next_match` at 3.40% and this function itself at 1.03%.
    //
    // ⚠**MIRROR — WIDEN THIS GUARD BEFORE ADDING AN ARM OUTSIDE THESE THREE.** Netty arms live
    // under `io/netty/`, guava arms under `com/google/common/collect/`, and fastutil arms under
    // `it/unimi/dsi/fastutil/`. An arm listed below but not covered here would silently stop firing.
    // The netty `inEventLoop` and `execute` arms are the TWO structural
    // exceptions: their population is the `EventExecutorGroup` type closure plus
    // the exact SslHandler / ThreadPerTaskExecutor tuples (the census table in
    // net/mod.rs), so their owners live in no fixed namespace — the guard
    // consults `net::gate_in_event_loop` and the execute provider before
    // rejecting, in the clause below.
    if fast_gates()
        && !(class.starts_with("io/netty/")
            || class.starts_with("com/google/common/collect/")
            || class.starts_with("it/unimi/dsi/fastutil/"))
        // The structural `inEventLoop` and `execute` arms below admit
        // implementors of `EventExecutorGroup` from ANY package — see the
        // MIRROR note above.
        && !crate::net::gate_in_event_loop(vm, class, name)
        && !(name == "execute"
            && !crate::parallel::gilfree()
            && crate::net::dispatch_execute_action(vm, class, name, _descriptor)
                != crate::net::ExecuteAction::Decline)
    {
        return false;
    }
    // netty `inEventLoop`: this thread IS the event loop (net/mod.rs's inline
    // model). The verdict is structural — the loader's `EventExecutorGroup`
    // closure plus the exact SslHandler tuple — never a package prefix; the
    // census table in net/mod.rs licenses both.
    crate::net::gate_in_event_loop(vm, class, name)
    // Don't spawn netty's event-loop thread (the exact ThreadPerTaskExecutor
    // execute tuple) and run event-executor execute() inline (our loop never
    // runs, so a queued task — e.g. the deferred channel bind — would otherwise
    // hang): both rows are the structural provider at the top of this function —
    // the exact TPTX tuple plus the `EventExecutorGroup` closure, never a
    // package prefix; the census table in net/mod.rs licenses them.
    // Neutralise the event loop / IoHandler infinite run() loops (they are
    // for(;;) pollers that never return; with tasks running inline they have
    // nothing to do). The wrapper Runnable that invokes them runs inline.
    // (The two old string-shape `execute` arms are gone — the structural
    // provider at the top of this function answers both rows now.)
        // guava HashBiMap — ⭐**AN EXACT PROVIDER FACT.** The native map layer
        // completely supplies the exact class (create/create(int)/create(Map) and
        // inverse() both fabricate the exact parent name with a native $map, and
        // map_method serves the whole surface), so `FACT_FORCE_NATIVE_MAP` — exact
        // membership of `map::FORCE_NATIVE_MAP_CLASSES` — is the semantic answer,
        // never a workload-derived allowlist. The nested classes the historic prefix
        // arm admitted (HashBiMap$BiEntry/$Itr/$Inverse/$InverseKeySet) are born only
        // inside HashBiMap bytecode that cannot execute while every method of the
        // exact class force-routes, so none can reach this arm as owner, actual or
        // lambda owner — a full-boot plus probe census proved zero divergent asks
        // before the prefix was deleted (the f02b1c93 Guava cutover precedent).
        || crate::owner_bits::facts(class) & crate::owner_bits::FACT_FORCE_NATIVE_MAP != 0
        // guava MapMaker().…().makeMap() interner (ResourceKey.create): build the
        // map natively and route every method on the result to the native $map —
        // its real bytecode needs sun.misc.Unsafe and mis-executes here.
        || (class == "com/google/common/collect/MapMaker"
            && matches!(name, "makeMap" | "makeComputingMap"))
        || class == "com/google/common/collect/MapMakerInternalMap"
        // ImmutableMap.copyOf(srcMap) run as guava bytecode drops everything when
        // the source is a non-native map (e.g. the fastutil Object2ObjectArrayMap
        // that BaseMapCodec.decode accumulates into) — Codec.unboundedMap then
        // decoded to an empty map. Serve copyOf natively (it drains via drain_map).
        || (name == "copyOf"
            && matches!(class,
                "com/google/common/collect/ImmutableMap"
                | "com/google/common/collect/ImmutableBiMap"
                | "com/google/common/collect/ImmutableSortedMap"))
        // guava `ImmutableMap.toImmutableMap(keyFn, valFn)` run as guava bytecode
        // builds a real guava ImmutableMap whose `get` works but whose
        // `entrySet()/keySet().stream()` are EMPTY in our interp (the view iterator
        // has no drainable bytecode). That emptied `RegistryAccess.registries()` (=
        // `registries.entrySet().stream()`), so `LayeredRegistryAccess.collectRegistries`
        // dropped every registry → `Missing registry: minecraft:dimension` (Wall #16).
        // Serve it natively → our marked toMap collector → a native map that streams
        // its views consistently. Gated so the default build stays byte-identical.
        || (cfg!(feature = "next-gen-gc")
            && name == "toImmutableMap"
            && matches!(class,
                "com/google/common/collect/ImmutableMap"
                | "com/google/common/collect/ImmutableBiMap"))
        // guava Maps.transformValues/transformEntries return a lazy VIEW our generic
        // stream/iterator interception can't drain (came back empty), which emptied
        // MappedRegistry.entrySet() → the static registries never reached the datapack
        // lookup (Wall #11). Force the native eager-map handler. Gated so the default
        // build keeps running the guava bytecode (byte-identical).
        || (cfg!(feature = "next-gen-gc")
            && class == "com/google/common/collect/Maps"
            && matches!(name, "transformValues" | "transformEntries"))
        // fastutil long-keyed object maps (the loaded-chunk map): route EVERY method
        // incl. <init> to the native $map. The invoke seams ALSO route by DECLARING
        // class (see `route_native_map`) so a subclass instance is covered too.
        || crate::map::is_fastutil_objmap(class)
        // Plan A (default ON; opt-out RAVA_NO_R2OMAP=1): route Reference2ObjectArrayMap's
        // entrySet-family ONLY to the native materializer (map::dispatch) — its
        // get/put/size etc. stay on the compiled bytecode (real key/value arrays).
        // Gate-off → false → byte-identical.
        || (cfg!(feature = "next-gen-gc")
            && class == "it/unimi/dsi/fastutil/objects/Reference2ObjectArrayMap"
            && matches!(name, "entrySet" | "reference2ObjectEntrySet" | "keySet" | "values")
            && crate::map::r2omap_native_enabled())
        // netty `MacAddressUtil.{defaultMachineId,bestAvailableMac}` — its guest body
        // walks NetUtil/NetworkInterface enumeration; `NetUtil.<clinit>` unboxes a null
        // Integer in our interp (NPE `Integer.intValue`), aborting it. That abort
        // propagated up to `DefaultChannelId.<clinit>`, leaving `MACHINE_ID` null →
        // `DefaultChannelId.<init>` NPE'd on `arraylength` (Wall #18). Serve a fixed,
        // non-null 8-byte MAC directly (native stub) so the channel-id machinery never
        // touches NetUtil. Gated so the default build runs the netty bytecode.
        || (cfg!(feature = "next-gen-gc")
            && class == "io/netty/util/internal/MacAddressUtil"
            && matches!(name, "defaultMachineId" | "bestAvailableMac"))
        // DataFixerUpper: DataFixers.getDataFixer() has bytecode (DataFixerBuilder.build())
        // that yields a Result whose fixer() is null in our interp → NPE in getDataFixer at
        // pc=3 (Util.fetchChoiceType → FireBlock.bootStrap → Bootstrap). Force it (and the
        // sentinel stubs it returns) onto the dfu_stub native path — a fresh-world boot never
        // migrates data, so the identity DataFixer is correct. Gated so the default build runs
        // the real DFU bytecode (byte-identical).
}

/// Find an exception handler in the current frame's table covering `pc` that
/// catches `exc`. `catch_type` 0 is a catch-all (finally); otherwise the type
/// is checked leniently via `instance_of`.
/// ⚠**`#[inline(never)]` DELIBERATELY** — row 397. At opt-level 3 LLVM folds this into
/// `opcodes::run`, and an inlined body's locals are reserved by `run`'s PROLOGUE, which
/// EVERY interpreted call pays before the callee's first opcode. `run` measured a **2136-byte**
/// stack frame and a framed call ~6.7 KB across the path, against a measured frame cost of
/// **160 ns vs HotSpot's 7.2**. Keeping this out of line trades a call on a cold path for a
/// smaller prologue on the hot one.
#[inline(never)]
pub(crate) fn find_handler(vm: &Vm, frame: &Frame, code: &Code, pc: usize, exc: &Value) -> Result<Option<usize>, VmError> {
    for h in &code.exceptions {
        if (h.start_pc as usize) <= pc && pc < (h.end_pc as usize) {
            if h.catch_type == 0 {
                return Ok(Some(h.handler_pc as usize));
            }
            // ⭐**THE NAME IS BORROWED, NOT BUILT.** `pool.class_name` already hands back a
            // `&str` into the constant pool, and the only three things done with it below are a
            // compare, an `instance_of` and a diagnostic print — nobody keeps it. The `to_string`
            // was a heap allocation and a copy **per exception-table entry covering `pc`, per
            // throw, per frame the unwind walks**, and a throw that escapes N frames pays it in
            // every one. What grows is handlers-covering-pc × frames-unwound; the borrow is O(0).
            // The pool outlives the borrow (`frame` is `&Frame`) and `vm` is a DISJOINT shared
            // borrow, so `instance_of` below still takes it unchanged.
            if let Some(cn) = frame.class.file.pool.class_name(h.catch_type) {
                // Both trace flags are CACHED: a per-catch `env::var` is a real slowdown
                // in exception-heavy phases — enough to shift GC timing and mask the
                // timing-sensitive bugs the tracer exists to surface (observed: a flaky
                // post-recipes boot failure vanished whenever RAVA_TRACE_MAINCATCH ran).
                //
                // ⭐**TWO ACQUIRE LOADS BECAME ONE PLAIN ONE.** They were two independent
                // `OnceLock`s read INSIDE the exception-table walk, so an unwind paid both per
                // table entry covering `pc`, per throw, per frame it escaped — and being atomics
                // LLVM could neither hoist them out of the `for` nor fold them together. They are
                // now two bits of the SAME per-thread word, loaded once here and passed to both
                // reads by `op_gate_of`. The position is unchanged: still resolved lazily inside the
                // matched-entry arm, so a throw whose table has no covering entry still reads
                // nothing at all.
                let gw = op_gates();
                let trace_catch = op_gate_of(gw, OG_TRACE_CATCH, trace_catch_uncached);
                let trace_main = op_gate_of(gw, OG_TRACE_MAIN, trace_main_uncached);
                // `catch (Throwable)` matches EVERYTHING, by construction: athrow
                // only ever hands this function a Throwable, so the subtype walk
                // has nothing to decide. Asking `instance_of` instead made the
                // universal handler depend on the JDK_SUPERS table having a row
                // for the thrown class — and for a class it does not know, the
                // walk goes inconclusive and answers FALSE, so the one clause a
                // program writes to be certain of catching anything was the clause
                // most likely to miss. This is the exception TABLE only; plain
                // `x instanceof Throwable` still has to prove itself.
                let matched = cn == "java/lang/Throwable" || instance_of(vm, exc, cn);
                if !matched && trace_catch {
                    let ec = match exc { Value::Ref(eh) => vm.heap.class_of(*eh).unwrap_or("?").to_string(), _ => "?".into() };
                    eprintln!("[NOCATCH] {ec} not instanceof {cn} (handler pc={} in {})", h.handler_pc, frame.class.file.this_class);
                }
                if matched {
                    // Near-zero-cost frontier hook: the top-level `catch Throwable` in the
                    // bootstrap ENTRY class swallows the boot failure and logs it via
                    // (currently invisible) log4j. Surface it. Generic: compare against the
                    // recorded entry class, not a hard-coded jar-specific name.
                    let is_main = vm
                        .entry_class
                        .as_deref()
                        .is_some_and(|e| &*frame.class.file.this_class == e);
                    if trace_catch || (is_main && trace_main) {
                        let ec = match exc {
                            Value::Ref(eh) => vm.heap.class_of(*eh).unwrap_or("?").to_string(),
                            // An arena-Obj exception: decode its class from the header —
                            // and if the header is zeroed/garbage, that IS the finding
                            // (a stale pointer thrown, the moving-GC bug signature).
                            #[cfg(feature = "next-gen-gc")]
                            Value::Obj(p) => {
                                let hdr = unsafe { (p.addr() as *const u64).read_unaligned() };
                                format!(
                                    "<arena {:?} @{:#x} hdr={hdr:#018x}>",
                                    crate::nextgen::class_name(unsafe { crate::nextgen::obj_class(p.as_ptr()) }),
                                    p.addr()
                                )
                            }
                            other => format!("<non-ref {other:?}>"),
                        };
                        let em = match exc { Value::Ref(eh) => match vm.heap.get_field(*eh, "$message") { Value::Str(s) => s.to_string(), _ => String::new() }, _ => String::new() };
                        eprintln!("[CATCH] {ec}: {em} — as {cn} in {}", frame.class.file.this_class);
                    }
                    return Ok(Some(h.handler_pc as usize));
                }
            }
        }
    }
    Ok(None)
}

/// Build a functional-interface *default* method result (a combinator object)
/// for a lambda receiver. Returns None for SAM calls (apply/test/accept/...),
/// which the caller dispatches normally.
/// Build a functional-interface DEFAULT-method combinator (andThen/compose/negate/
/// reversed/thenComparing/…) as a `$fnkind`/`$cmp` synthetic object. `recv` is the
/// receiver as a VALUE — a 0b01 handle lambda OR a RAVA_ARENA_LAMBDA 0b00 arena
/// lambda — stored verbatim into `$first`/`$inner`, so the combinator is
/// representation-agnostic (the `apply`/`compare` paths re-dispatch it via
/// `vm.apply`/`vm.compare`, both of which handle arena-lambdas).
pub(crate) fn build_fi_default(vm: &mut Vm, recv: &Value, name: &str, args: &[Value]) -> Option<Value> {
    // ⭐**THE CLONE MOVED BEHIND THE QUESTION IT ANSWERS.** `args[0]` was cloned — an `Arc`
    // refcount pair for a ref or a string, plus the drop glue on the way out — BEFORE the `match`
    // that decides whether a combinator exists at all, so `negate`, `reversed` and every name this
    // function does not handle paid for an argument they never read. The call sites now gate on
    // the site's `FI_DEFAULT` bit, which removed the common miss; this removes what was left, and
    // is the shape a gate should have had from the start: decide, THEN own.
    macro_rules! arg0 {
        () => {
            args.first().cloned()
        };
    }
    let func = |vm: &mut Vm, kind: &str, first: Value, second: Option<Value>| -> Value {
        let h = vm.heap.alloc("java/util/function/Function");
        vm.heap.put_field_syn(h, crate::heap::Syn::FnKind, Value::Str(kind.into()));
        vm.heap.put_field(h, "$first", first);
        if let Some(s) = second { vm.heap.put_field(h, "$second", s); }
        Value::Ref(h)
    };
    let cmp = |vm: &mut Vm, kind: &str, fields: &[(&str, Value)]| -> Value {
        let h = vm.heap.alloc("java/util/Comparator");
        vm.heap.put_field(h, "$cmp", Value::Str(kind.into()));
        for (k, v) in fields { vm.heap.put_field(h, k, v.clone()); }
        Value::Ref(h)
    };
    match name {
        "andThen" => Some(func(vm, "andThen", recv.clone(), arg0!())),
        "compose" => Some(func(vm, "compose", recv.clone(), arg0!())),
        "negate" => Some(func(vm, "negate", recv.clone(), None)),
        "and" | "or" => Some(func(vm, name, recv.clone(), arg0!())),
        "reversed" => Some(cmp(vm, "reversed", &[("$inner", recv.clone())])),
        "thenComparing" | "thenComparingInt" | "thenComparingLong" | "thenComparingDouble" => {
            // thenComparing(keyExtractor or Comparator) — the one arm that reads the
            // argument three times, so it owns it once here.
            let arg0 = arg0!();
            let arg_is_cmp = matches!(&arg0, Some(Value::Ref(a)) if matches!(vm.heap.get_field(*a, "$cmp"), Value::Str(_)));
            let arg_is_lam = arg0.as_ref().is_some_and(|a| vm.value_is_lambda(a));
            let second = match arg0 {
                Some(a) if arg_is_cmp => a, // already a comparator
                Some(a) if arg_is_lam && name == "thenComparing" => {
                    // a key-extractor lambda — wrap as a key comparator ($keyfn holds
                    // the extractor by value, applied via vm.apply).
                    let kc = vm.heap.alloc("java/util/Comparator");
                    vm.heap.put_field(kc, "$cmp", Value::Str("key".into()));
                    vm.heap.put_field(kc, "$keyfn", a);
                    Value::Ref(kc)
                }
                Some(other) => other,
                None => Value::Null,
            };
            Some(cmp(vm, "then", &[("$first", recv.clone()), ("$second", second)]))
        }
        _ => None, // SAM (apply/test/accept/get/run/call/compare/...)
    }
}

/// Reference identity comparison for `if_acmp*` (and null checks). `pub(crate)`
/// so the JIT's `rava_acmp` callback shares the exact same semantics.
pub(crate) fn ref_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Ref(x), Value::Ref(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => crate::value::RStr::ptr_eq(x, y) || x == y,
        // Boxed primitives are kept unboxed in our value model and the values
        // that flow through code are singletons (a property's possible values,
        // cached Integers/Booleans, ...), so `==` on equal-valued primitives is
        // reference-equal. (Real Java only guarantees this for the small-int
        // cache, but our model has no per-box identity to distinguish.)
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Long(x), Value::Long(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Double(x), Value::Double(y)) => x == y,
        // Arena object identity = current address (GC moves only at a safepoint,
        // never mid-compare) — checked FIRST: equal addresses are the same object,
        // equal for every class including String, with zero header reads. Needed
        // for MC RegistryID's linear-probe `==`.
        //
        // STRINGS COMPARE BY CONTENT IN BOTH FORMS. `Value::Str` above already does
        // (deliberately — this model has no per-instance String identity), so an
        // arena-STRING must too, or the SAME string compares unequal purely because
        // one copy happens to live in an arena ref cell and the other does not — and
        // the answer would additionally depend on WHICH arena copy, since equal
        // strings are allocated separately. Only differing addresses need the class
        // probe, and `is_arena_string(x)` is checked alone first so the common
        // non-string mismatch pays ONE header read, not two.
        #[cfg(feature = "next-gen-gc")]
        (Value::Obj(x), Value::Obj(y)) => {
            x.addr() == y.addr()
                || (crate::nextgen::is_arena_string(*x)
                    && crate::nextgen::is_arena_string(*y)
                    && crate::nextgen::arena_str_bytes(*x) == crate::nextgen::arena_str_bytes(*y))
        }
        #[cfg(feature = "next-gen-gc")]
        (Value::Obj(p), Value::Str(s)) | (Value::Str(s), Value::Obj(p))
            if crate::nextgen::is_arena_string(*p) =>
        {
            crate::nextgen::arena_str_bytes(*p) == s.as_bytes()
        }
        // B5 IDENTITY BRIDGE: during handle→pointer coexistence the SAME object can
        // be referenced both as Ref(h) and as Obj(pinned-mirror-of-h) — reference
        // identity must hold across the two forms. Inert until a pinned region
        // exists (is_pinned is false → pinned_handle_of is None → arms fall out).
        #[cfg(feature = "next-gen-gc")]
        (Value::Obj(p), Value::Ref(h)) | (Value::Ref(h), Value::Obj(p)) => {
            crate::nextgen::pinned_handle_of(p.addr()) == Some(*h)
        }
        _ => false,
    }
}
