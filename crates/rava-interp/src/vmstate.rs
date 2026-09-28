//! The Vm and the frame it runs: the state every module in the crate reaches into.
//!
//! Re-exported from the parent module, so no call site outside it changed.

/// A Java call staged by an invoke opcode for the explicit-stack machine to push.
///
/// `midx` is the callee's index in `decl.file.methods` when the staging site already knew it —
/// the interpreted call sites cache it on the `SiteRef` (`SiteRef::resolved`). Without it
/// `build_frame` finds the method BY NAME on every trampolined call: a name hash plus a
/// descriptor compare across the overload set, for a fact that is fixed per call site.
pub(crate) struct PendingCall {
    pub(crate) decl: Arc<LoadedClass>,
    pub(crate) name: Arc<str>,
    pub(crate) desc: Arc<str>,
    pub(crate) args: Vec<Value>,
    pub(crate) midx: Option<u32>,
}

use crate::value::Value;
use crate::*;
pub struct Vm {
    pub loader: ClassLoader,
    pub heap: Heap,
    /// Captured stdout (also echoed live) so callers/tests can assert output.
    pub output: String,
    /// Logical Java call stack (class, method+desc) for diagnostics.
    pub(crate) call_stack: Vec<(String, String)>,
    pub(crate) trace: TraceFlags,
    /// Monomorphic inline cache for virtual dispatch, keyed by call site
    /// `(declaring-class pointer, constant-pool index)`. Stores the last
    /// receiver class seen there and the method it resolved to; a matching
    /// receiver class skips `resolve_method` (its key formatting + hash) — the
    /// common case, since most call sites see one receiver type. A mismatch
    /// re-resolves and overwrites, so dispatch stays correct.
    /// Reusable `Vec<Value>` buffers for frame locals/operand stacks. Every
    /// method call would otherwise allocate two fresh Vecs; checking them out of
    /// this pool and returning them on frame exit removes that per-call malloc
    /// churn (method dispatch is a dominant interpreter cost).
    // Live JIT cell roots and the box side-table are now PER-THREAD (see the
    // `frames` module: `cells_*` / `box_*`). They were `Vm` fields, but a compiled
    // call runs interpreter bytecode in its callbacks, which can hit the op-loop
    // GIL hand-off — so with a real daemon OS thread two threads interleave with
    // compiled frames live, and a shared Vec's pop/truncate would corrupt across
    // the interleave (the same hazard already fixed for the frame stack).
    /// Heap cap in BYTES (`RAVA_XMX_MB`); 0 = unset. Drives both GC scheduling
    /// and the hard abort backstop.
    pub(crate) mem_limit: usize,
    /// Raw `*const Frame` for every live interpreter frame (pushed in `run`,
    /// popped on return). GC scans these for root `Value`s. Frames live on the
    /// Rust call stack; this registry is how the collector reaches them.
    /// Next `approx_bytes`-scaled level at which to attempt GC (logical Java-heap
    /// pressure — fires early on object-count-heavy churn so the non-shrinking slot
    /// tables stay small).
    pub(crate) gc_next: usize,
    /// Next REAL process-byte level (mem_meter) at which to attempt GC — catches
    /// boxed/Str bytes the estimate undercounts ~5x (autoboxing/HashMap). Independent
    /// of `gc_next` so each pressure shape triggers without the other ratcheting up.
    pub(crate) gc_next_real: usize,
    /// Interned `java/lang/Class` objects by binary name. A class literal /
    /// getClass() must yield the SAME object every time so identity and
    /// equals/hashCode hold (Minecraft keys maps by Class, e.g.
    /// ClassTreeIdRegistry). Values are GC roots (see Vm::gc).
    pub(crate) class_objects: std::collections::HashMap<String, u32>,
    /// Ops remaining until the next heartbeat/GC/cap check. A plain countdown
    /// (decrement + compare per op) replaces a global atomic + two modulos that
    /// the op-loop otherwise paid on every instruction. Lives on the Vm (not a
    /// frame local) so short method calls don't reset it. See `op_total`.
    /// RAVA_JIT — compile hot numeric methods to native code (Cranelift).
    pub(crate) jit_enabled: bool,
    /// RAVA_CNP — copy-and-patch tier-0: pure-int static methods run as inline
    /// native code. Off by default for A/B; supported on Windows and Linux x86-64.
    #[cfg(all(any(windows, target_os = "linux"), target_arch = "x86_64"))]
    pub(crate) cnp_enabled: bool,
    /// CNP compiled-function cache by Code pointer; `None` means ineligible.
    #[cfg(all(windows, target_arch = "x86_64"))]
    pub(crate) cnp_cache: FastMap<usize, Option<cnp::CompiledFn>>,
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    pub(crate) cnp_cache: FastMap<usize, Option<crate::vm::cnp_run::CnpEntry>>,
    /// Cranelift-AOT numeric tier compiled fns (RAVA_AOT_CLIF), keyed by Code ptr —
    /// the run_numeric ABI (int-locals buffer), distinct from cnp_cache's run(frame).
    #[cfg(all(windows, target_arch = "x86_64"))]
    pub(crate) aot_clif_cache: FastMap<usize, Option<cnp::CompiledFn>>,
    /// Full AOT run-path lane (A2): compile_method output linked to (CompiledFn +
    /// per-process constant array), keyed by Code ptr. `None` = method is ineligible
    /// (uncovered opcode / unresolvable const) and stays on the JIT/interpreter.
    #[cfg(all(
        feature = "next-gen-gc",
        any(windows, target_os = "linux"),
        target_arch = "x86_64"
    ))]
    pub(crate) aot_full_cache:
        FastMap<usize, Option<(cnp::CompiledFn, Vec<i64>, crate::vm::aot_arguments::Layout)>>,
    /// Linked prepared direct-leaf functions, kept alive as long as their constant pointers.
    #[cfg(all(feature = "next-gen-gc", target_os = "linux", target_arch = "x86_64"))]
    pub(crate) aot_leaf_cache:
        std::collections::HashMap<(String, String, String), Option<cnp::CompiledFn>>,
    #[cfg(all(
        feature = "next-gen-gc",
        any(windows, target_os = "linux"),
        target_arch = "x86_64"
    ))]
    pub(crate) aot_direct_targets: crate::vm::aot_direct_cache::Cache,
    #[cfg(all(
        feature = "next-gen-gc",
        any(windows, target_os = "linux"),
        target_arch = "x86_64"
    ))]
    pub(crate) aot_cells: crate::vm::aot_cells::Pool,
    /// Per-method call counter for the AOT lane — only AOT-compile once hot (matching
    /// the JIT threshold), so cold one-shot methods aren't compiled for a single call.
    /// (The disk cache — A5 — is what enables true exec-from-call-1 across runs.)
    #[cfg(feature = "next-gen-gc")]
    pub(crate) aot_hot: FastMap<usize, u32>,
    /// RAVA_GC — run the garbage collector (default on; a runtime needs it).
    /// Disable only for debugging short-lived runs (RAVA_GC=0).
    pub(crate) gc_enabled: bool,
    /// RAVA_JIT_VERIFY — run both JIT and interpreter, compare, log mismatches,
    /// and trust the interpreter result (correctness audit of the compiler).
    pub(crate) jit_verify: bool,
    /// Per-`Code` JIT state (hotness counter / compiled function / failed),
    /// keyed by the `Code`'s stable heap address.
    pub(crate) jit_cache: FastMap<usize, jit::JitStatus>,
    /// Optional persistent compiler thread. Its `JITModule` must outlive every
    /// `JitFn` it publishes, so this field deliberately follows `jit_cache`: the
    /// cache drops first, then this owner joins the compiler and unmaps its code.
    pub(crate) background_jit: Option<background_jit::BackgroundJit>,
    /// Register-ABI entries, keyed by the callee `Code`'s stable heap address. MANDATORY,
    /// not an optimisation: `jit_resolve_reg` compiles a SECOND entry for the target, and
    /// a register-compiled body's own call sites resolve through this same path — so a
    /// self-recursive callee would compile a fresh entry per nesting level without a
    /// per-method memo. Values use the caller-side encoding (`p` = GC-leaf, `p|1` = deep,
    /// `1` = permanently ineligible).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) reg_abi_cache: FastMap<usize, i64>,
    /// Memoised offloadability (parallel-worldgen #21): (Code-ptr, relaxed) → can this
    /// method's whole subtree run GIL-free on a worker (see `offload`). `relaxed` is
    /// the upper-bound probe vs the sound check — distinct results, so it's in the key.
    /// GAP-S06/S02: valid only for `offload_epoch` — cleared when the loader hierarchy
    /// changes (a new class can add an override that invalidates a closure verdict).
    /// The third bool is the FIELD-TXN mode bit (write-only-transaction proof, see
    /// `Vm::is_field_txn_safe_devirt`): a txn verdict is a DIFFERENT question than a
    /// normal-offload verdict over the same Code, so txn entries must never collide
    /// with plain/relaxed ones — hence a dedicated key slot, not a relaxed alias.
    pub(crate) offload_cache: FastMap<(usize, bool, bool, bool), bool>,
    /// GAP-S06/S02: the loader hierarchy epoch `offload_cache`/`dispatch_closure_cache`
    /// were derived under (see `ClassLoader::epoch`).
    pub(crate) offload_epoch: u64,
    /// GAP-S06/S02: memoised exact virtual-dispatch closures per (owner, name, desc);
    /// `None` = the closure is unprovable (interface owner / unloadable / native tail).
    /// Valid only for `offload_epoch`.
    pub(crate) dispatch_closure_cache: FastMap<(String, String, String), Option<Arc<[String]>>>,
    /// GAP-A01 (drill iter 3): memoised "closed unloadable owner" verdicts for the
    /// closure-miss contract fallback — owner → (owner is UNLOADABLE and NO loaded
    /// class possibly subtypes it, so every runtime receiver of a virtual call
    /// declared on it is natively served). Valid only for `offload_epoch`.
    pub(crate) unloadable_owner_closed_cache: FastMap<String, bool>,
    /// GAP-A01 STAGE 1 (unloadable-iface closure): memoised MAY-SUBTYPE implementor
    /// enumerations per (owner, name, desc) for owners the class-owner closure refuses
    /// (interface/unloadable). DIAGNOSTIC-ONLY in stage 1 — consumed exclusively by
    /// the drill printer; no admission path reads it. Valid only for `offload_epoch`.
    pub(crate) iface_closure_cache: FastMap<(String, String, String), Arc<[String]>>,
    /// O(1) membership index for `$elems`-backed hash sets (`stream::coll_op`),
    /// keyed by the set's heap handle. Value = (length stamp, set of element
    /// `ekey`s). A Set has no in-place same-length mutation, so any external
    /// change moves the backing-array length — a stamp mismatch forces a rebuild
    /// from the live array, so the index can never be silently stale. Cleared
    /// wholesale past a cap (rebuild-on-miss preserves correctness) so handles of
    /// collected sets can't leak unboundedly. Turns element-by-element set builds
    /// from O(n^2) dedup scans into O(n).
    /// FxHash, not the default hasher. This is the Set MEMBERSHIP index — it is
    /// probed once per `contains`/`add` on every `$elems`-backed Set, and its keys
    /// are element key STRINGS, so the default `RandomState` runs SipHash over
    /// every one of them. `std::hash::random` was a visible 1.38% row in the
    /// ChunkFill gate profile.
    pub(crate) set_index: crate::FastMap<u32, (usize, crate::FastSet<String>)>,
    /// The synchronized memo/cache aggregate — the exact tables that already
    /// ran behind an `RwLock`/`AtomicU64` on `Vm`, extracted so
    /// [`Vm::runtime_shared`] can publish a borrow-only safe view of them.
    /// See `vm/shared_memos.rs`.
    pub(crate) shared_memos: crate::vm::SharedMemos,
    pub(crate) jit_stats: std::collections::BTreeMap<String, JitStats>,
    /// Number of methods compiled so far, and the cap. HISTORICAL: each method once
    /// owned its own Cranelift `JITModule` (~MB-scale mmap each) → compiling thousands
    /// ballooned RSS to ~14GB, forcing a tight cap of 4096. That is NO LONGER TRUE —
    /// all methods now share ONE module (`SHARED_MODULE`, jit.rs), so the per-method
    /// cost is ~the machine-code size: MEASURED ~2.5 KB/method (801 methods → ~2 MB over
    /// base). So the cap exists only to bound the long tail / compile time, not RSS;
    /// 16384 methods cost ~40 MB, well under the RAVA_PROC_CAP_MB backstop. A higher
    /// cap JIT-covers more of the boot's long tail (compiled ~2–3× vs interpreted
    /// 150–400×). Tunable via RAVA_JIT_CAP.
    pub(crate) jit_compiled: usize,
    pub(crate) jit_cap: usize,
    /// Invocations before a method compiles (RAVA_JIT_THRESHOLD). Must be high
    /// enough that the bounded compile slots (`jit_cap`) go to genuinely hot
    /// methods: at the old threshold of 10, hundreds of trivial bootstrap methods
    /// exhausted the whole cap in the first seconds of a server boot, so the
    /// actually-hot code (voxel shapes, worldgen noise — millions of calls) found
    /// the cap empty and never compiled. For a method with millions of calls,
    /// waiting out a few hundred interpreted calls is negligible warmup.
    pub(crate) jit_threshold: u32,
    /// Direct JIT-to-JIT call cache, keyed by call site (caller class ptr, cp
    /// index): a small POLYMORPHIC set (≤ `JIT_PIC_CAP`) of entries, each
    /// (callee native entry, ret kind, max_locals, wide-param bitmask,
    /// ref-return, receiver-class guard — interned class-name data ptr, 0 for
    /// static). One entry per observed receiver class lets a polymorphic site
    /// (e.g. a `Shape.area()` over Circle/Square/Tri) stay on the fast direct
    /// path for EVERY class instead of guard-missing → general path on all but
    /// the one cached class. Positive entries only — a compiled method never
    /// un-compiles; misses retry, since the callee may compile later. A site that
    /// exceeds the cap is megamorphic → general path (correct, just not direct).
    ///
    /// LAMBDA entries (`DirectEntry.shift`): a non-capturing method-ref lambda
    /// receiver is guarded by IDENTITY (its Ref cell bits — every lambda shares
    /// the class name "$Lambda", so a class guard can't distinguish targets).
    /// The call drops the lambda cell and passes raw[1..] straight to the
    /// compiled target (an unbound instance ref's arg0 IS the target receiver;
    /// a static ref's args map 1:1). `arg0_guard` (unbound refs only) re-checks
    /// the dynamic class the virtual target was resolved against.
    /// Direct compiled→compiled dispatch cache. NESTED by (site) → (receiver-class
    /// guard) → entries, so a MEGAMORPHIC site is O(1) by class instead of a linear
    /// Vec scan (the old flat `Vec` capped at 16 → sites with >16 classes fell to the
    /// slow general path AND raising the cap exploded the per-call scan on MC's
    /// 100+-type block-state sites). The inner Vec holds the rare arg0-guard / lambda
    /// collisions for one recv-class (usually length 1). Naturally bounded by the
    /// program's type hierarchy → no cap needed.
    pub(crate) jit_direct_cache: FastMap<(usize, u16), FastMap<usize, Vec<jit::DirectEntry>>>,
    /// Interned "$Lambda" class-name ptr, learned on the first lambda receiver a
    /// direct-call site resolves — lets `jit_direct_call` switch lambda receivers
    /// to identity guards without a per-call string compare. 0 until seen.
    /// Leaked per-Vm slot holding the `frames::JitThreadCtx` address of the
    /// thread currently executing this Vm's compiled code. Baked as a constant
    /// into every function this Vm compiles for the normal GIL-held path.
    pub(crate) jit_ctx_slot: &'static std::sync::atomic::AtomicUsize,
    /// NEGATIVE direct-call cache: (site, receiver-class guard) → miss count. 98% of
    /// direct misses were 'callee not compiled / excluded' RE-RESOLVED on every call
    /// (176M resolves/boot measured); a cached refusal short-circuits, re-probing
    /// every 64th call so a callee that compiles later is still picked up. The
    /// general dispatch path stays correct throughout — this only skips re-resolves.
    pub(crate) jit_direct_neg: FastMap<(usize, u16, usize), u32>,
    /// Interned name-pointer of "java/util/BitSet" (0 until the first BitSet
    /// fast-path call) — the exact-class guard for the compiled-code helpers.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) bitset_name_ptr: usize,
    /// ARENA class id of `java/util/BitSet`, or `u32::MAX` until one is seen. The handle side
    /// keys on an interned name POINTER; an arena object carries a class ID instead, so the
    /// arena twin of that guard needs its own resolved-once key. See `jit_bitset_words`.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) bitset_arena_cid: u32,
    /// Per-indy-site SINGLETON for a NON-CAPTURING lambda/method-ref (JVM
    /// ConstantCallSite parity: metafactory links once and returns the same
    /// instance forever). Allocating a FRESH lambda per invokedynamic run made
    /// every identity-guarded lambda call site megamorphic (4 dead identities →
    /// PIC cap → the general dispatch path on every SAM call: 6M megamorphic
    /// misses in a 2-position RingScan) and churned an allocation per call.
    /// Values are GC roots (see `Vm::gc`).
    pub(crate) indy_singletons: FastMap<(usize, u16), u32>,
    /// Reverse index of `indy_singletons` VALUES — O(1) "is this handle the
    /// registered per-site singleton?" for the λ-PIC fill (`lambda_pic_fill`).
    /// Maintained at the single `indy_singletons.insert` site; `indy_singletons`
    /// is insert-once per site and never removed from, so membership here proves
    /// the handle is rooted on every collect ⇒ IMMORTAL (never swept + reused) —
    /// the invariant that makes an identity-keyed machine-code cache entry sound.
    pub(crate) indy_singleton_handles: FastSet<u32>,
    /// Default-on lambda dedup (RAVA_M3_LAMBDA_DEDUP=0 disables): per-site cache for CAPTURING lambdas of
    /// arity 1, keyed EXACTLY by (indy site, captured-value discriminant, i64 payload). A hit returns
    /// the existing lambda handle instead of allocating a fresh identical closure. [[rava-lambda-capture-verdict]]
    /// measured 45.6% of capturing lambdas are duplicates (~61.7MB). Deduping is spec-legal (JLS
    /// §15.27.4: "a new instance need not be created if an existing instance would suffice") — a
    /// lambda's identity is unspecified. NOT a GC root (unlike indy_singletons): a persistent root
    /// would PIN every distinct capture forever (leak). Instead it is PRUNED at each GC — an entry
    /// whose handle was swept is dropped, so between GCs (no sweep) every cached handle is live, and a
    /// dead closure is reclaimed normally. Key includes the discriminant so `Ref(5)` and `Int(5)`
    /// never alias; only handle/int/long/obj captures are keyed (exact i64), other Value kinds skip.
    pub(crate) lambda_dedup: FastMap<(usize, u16, u8, i64), u32>,
    /// ARENA TWIN of `lambda_dedup` (`RAVA_ARENA_LAMBDA_DEDUP`, default OFF). The map above
    /// stores a heap HANDLE, so under `RAVA_ARENA_LAMBDA` a capturing closure is born in the
    /// arena, never inserted, and every lookup misses — arena closures get NO dedup at all.
    /// Measured: **2,885,078 arena-lambda births against 4,055 live, a 712:1 churn**, with the
    /// handle dedup active the whole time.
    ///
    /// Stores the raw arena ADDRESS, and unlike its twin it is CLEARED at every arena
    /// collection rather than pruned. The twin can `retain` because a heap handle is stable
    /// and `is_ref_alive` answers exactly; an arena address is neither stable nor testable —
    /// the moving collector relocates survivors, so a retained address could name a DIFFERENT
    /// object afterwards. Clearing costs only the hits between collections, which is precisely
    /// the window in which the twin's own argument says nothing moves.
    /// Keyed on the WHOLE capture vector, not one capture: the arity-1 key its heap twin uses
    /// covers 15.5% of arena closure births (histogram: arity ≥ 6 is 61.9%, peaking at 7 and
    /// 8+), so a narrow key cannot reach the churn.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) arena_lambda_dedup: FastMap<(usize, u16, Vec<(u8, i64)>), usize>,
    /// ARENA twin of `indy_singletons` (`RAVA_ARENA_LAMBDA_SINGLETON`): call site → the
    /// PINNED arena lambda minted for it. Unlike `arena_lambda_dedup` this is NEVER cleared:
    /// a pinned object is immortal and never relocated, so the address stays valid for the
    /// life of the process — the property that makes this table sound and that makes the
    /// λ-PIC's stable-address guard stronger here than on a handle.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) arena_indy_singletons: FastMap<(usize, u16), usize>,
    /// Total ops executed, advanced in `op_total += CHECK_INTERVAL` batches at
    /// each check point. Drives the heartbeat cadence.
    pub(crate) op_total: u64,
    /// Current interpreter call depth — a plain counter for the stack-overflow
    /// guard. The detailed `call_stack` (with class/method names) is only
    /// populated under RAVA_TRACE_THROW, since building those strings per call
    /// was a hot-path allocation.
    /// Call-depth ceiling for the stack-overflow guard (RAVA_MAX_DEPTH, default
    /// 20000). A runaway/cyclic recursion (e.g. a self-referential snakeyaml node
    /// whose toString never bottoms out) is bounded here — `-Xmx` cannot catch it
    /// because the cost is the Rust frame stack, not the simulated Java heap. The
    /// JVM itself throws StackOverflowError around this depth for simple methods,
    /// so 20000 stays JVM-realistic while capping the worst-case frame memory.
    pub(crate) max_depth: u32,
    /// Cached singleton `java/lang/Boolean` objects [FALSE, TRUE]. Boxed booleans
    /// must be real heap objects (not Value::Int) so `instanceof Boolean` and
    /// gson's `value instanceof Boolean` (JsonPrimitive.isBoolean) disambiguate
    /// them from boxed ints. Singletons so `Boolean.valueOf(b) == Boolean.valueOf(b)`
    /// and `== Boolean.TRUE` hold. GC roots (see Vm::gc).
    pub(crate) bool_cache: [Option<u32>; 2],
    /// Cached boxed `Character` / `Short` / `Byte` singletons, on the JDK's own ranges.
    /// Boxed like [`Vm::boolean_obj`] and for the same reason: `Value::Int` is the
    /// representation of a boxed `Integer`, so a `Character` stored as one reported
    /// `java.lang.Integer` from `getClass()` and RENDERED as its code point everywhere
    /// the OBJECT was printed (`{"key":{"35":..}}` where a shaped recipe wants `"#"`).
    /// Boxed so the box's own type survives; interned so `valueOf` identity matches the
    /// JDK's documented caches. GC roots (see `Vm::gc`).
    pub(crate) box_cache: Box<crate::boxid::BoxCache>,
    /// The single `java/lang/Thread` object returned by `Thread.currentThread()`.
    /// We run everything on one thread, so this MUST be a stable singleton:
    /// MinecraftServer captures `this.mainThread = Thread.currentThread()` and
    /// later guards `if (Thread.currentThread() != this.mainThread) redispatch` —
    /// a fresh object per call made that always true, so ServerChunkCache.getChunk
    /// re-dispatched to itself forever. GC root (see Vm::gc).
    pub(crate) main_thread: Option<u32>,
    /// The bootstrap entry class (whatever `run_main` was invoked with) — lets a
    /// diagnostic recognise the top-level `catch Throwable` that swallows a boot failure
    /// WITHOUT hard-coding a jar-specific class name (generic; no `net/minecraft`).
    pub(crate) entry_class: Option<String>,
    /// Wall #15: a temporary GC-root stack for heap values held ONLY in a Rust local
    /// across an allocation-heavy nested call (e.g. a CompletableFuture combinator's
    /// freshly-allocated result future across the inline `cf_run_cb`, which builds the
    /// reload graph and triggers a GC). Without it the result is unreachable mid-flight,
    /// gets swept, and its slot is reused — handing the caller a stale/wrong-typed object.
    /// Scanned as roots in `Vm::gc`; only ever populated under the next-gen-gc collector.
    pub(crate) gc_protect: Vec<Value>,
    /// Resident Executor/CompletableFuture queue. Task records own every guest
    /// value until completion and are scanned by both collectors.
    pub(crate) task_runtime: crate::task_runtime::TaskRuntime,
    /// GILFREE-RINGS: `supplyAsync` tasks the off-GIL lane has admitted but not yet
    /// run. They are held here — rooted by both collectors — until the coordinator
    /// parks and executes the whole batch (see `gilfree_cf::drain`). Empty, and
    /// therefore inert, unless the lane is armed.
    pub(crate) lane: crate::gilfree_cf::LaneBatch,
    /// M3 T2 step 3a — sites (code ptr, pc) already reported by RAVA_FORK_NUMERIC_PROBE, so each
    /// counted loop is traced once, not every iteration.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) numeric_probe_seen: std::collections::HashSet<(usize, usize)>,
    /// Set once the per-class layout cache has been pre-warmed for a chunk-fill fork (so workers
    /// only READ `class_layout_cache` — never insert into it concurrently).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) column_prewarmed: bool,
    /// Rust-side OS socket objects backing java.nio channels, keyed by the heap
    /// handle of the channel object. The heap can't hold a TcpListener/TcpStream,
    /// so the channel object carries only the handle and the real socket lives
    /// here. Not a GC root (entries are dropped explicitly on channel close).
    pub(crate) sockets: std::collections::HashMap<u32, crate::net::NetSock>,
    /// Heap handles of every open nio `Selector`. The netty event-loop poll is
    /// neutralised (it's an infinite loop), so networking is pumped one iteration
    /// per main-thread tick (see `crate::net::pump_network`) over these selectors.
    pub(crate) net_selectors: Vec<u32>,
    /// Re-entrancy guard for the network pump (it runs netty bytecode that
    /// re-enters the op checkpoint where the pump is driven).
    pub(crate) in_pump: bool,
    /// Next `op_total` at which to pump networking from the checkpoint, so
    /// connections are serviced continuously (not only on Thread.sleep).
    pub(crate) next_pump_op: u64,
    /// OS connections the selector poll has accepted but netty hasn't drained yet,
    /// keyed by listener-channel handle. The poll only reports OP_ACCEPT (and only
    /// then drives netty's allocating accept path) when one is actually queued —
    /// so idle pumping during worldgen allocates nothing.
    pub(crate) pending_streams:
        std::collections::HashMap<u32, std::collections::VecDeque<std::net::TcpStream>>,
    /// Explicit-stack execution (checkpoint/restore foundation). When `trampoline`
    /// is set, a Java-method invoke stages its resolved target in `pending` and
    /// suspends `run` instead of recursing, so `execute_stackful` can drive a
    /// serializable `Vec<Frame>`. Off by default → the recursive interpreter is
    /// unchanged. See docs/snapshot-roadmap.md (increment 3c).
    pub(crate) trampoline: bool,
    pub(crate) pending: Option<crate::vmstate::PendingCall>,
    /// Cooperative green-thread yield request. Set by a blocking native at a frame
    /// boundary (`Thread.sleep`, `Object.wait`) in trampoline mode: the `run` loop
    /// top then returns `RunExit::Yield` so the scheduler parks the current green
    /// thread and round-robins to the next ready one. `Some(ms)` records the sleep
    /// duration (0 = wait/yield). Single OS thread, so no lock is involved.
    pub(crate) yield_request: Option<i64>,
    /// Set once `execute_stackful` has written a checkpoint (RAVA_CHECKPOINT_OPS),
    /// so the CLI exits cleanly instead of finishing the boot.
    pub checkpointed: bool,
    /// Cooperative green threads (Java `Thread`s started via `Thread.start`). The
    /// recursive main thread keeps running; each green thread holds its own
    /// explicit frame stack and is advanced one slice at a time whenever the main
    /// thread yields (its `Thread.sleep`). Lives as a field (not a `drive_stack`
    /// local) so `gc()` scans every parked green thread's frames directly — no
    /// extra `unsafe`. Single OS thread → no lock, no data race.
    pub(crate) green_threads: Vec<GreenThread>,
    /// True while the primary thread (MC's "Server thread" — the long-lived
    /// boot+tick loop) is running inline as the recursive main. The FIRST
    /// `Thread.start` runs inline and sets this; any `Thread.start` nested inside
    /// it is a daemon → registered as a green thread instead of run inline.
    pub(crate) inline_thread_active: bool,
    /// True once the first daemon has been routed to a real OS thread (the rest
    /// stay green for now — bounds the moonshot's blast radius to one real thread).
    pub(crate) os_thread_spawned: bool,
    /// Re-entrancy guard for `run_green_threads_slice`: a green thread's native
    /// could call back into Java that reaches the `sleep` native; without this the
    /// scheduler would re-enter and see the running thread's moved-out (empty)
    /// frames as "completed". Set while a slice is in flight; re-entry is a no-op.
    pub(crate) in_green_slice: bool,
    /// GC invocation counter (leak diagnostic throttle, RAVA_TRACE_GT).
    pub(crate) gc_calls: u64,
}

impl Drop for Vm {
    fn drop(&mut self) {
        // A worker may already have claimed a task and be waiting for this thread's
        // GIL. Release before join so teardown cannot deadlock or outlive the Vm.
        frames::unregister();
        gil::release();
        self.task_runtime.shutdown_and_join();
    }
}

impl Vm {
    /// The published safe slice: a borrow-only view over the synchronized
    /// memo/cache aggregate. A [`RuntimeShared`] can reach NO other `Vm`
    /// field — no `Vm` reference, no deref, no raw pointer travels with it.
    pub(crate) fn runtime_shared(&self) -> crate::vm::RuntimeShared<'_> {
        crate::vm::RuntimeShared::new(&self.shared_memos)
    }
}
