//! The opcode dispatch — one match over the JVM instruction set. Subset for
//! now; each unhandled opcode errors with its hex so we extend against real
//! bytecode. Cross-class calls, fields, and `new` route through the VM's
//! class loader and heap.


















#[cfg(test)]
mod tests;
use rava_classfile::Code;
use std::sync::Arc;

use crate::descriptor;
use crate::value::Value;
use crate::{Frame, Vm, VmError};

/// RAVA_GSON_NATIVE — parse gson JSON natively instead of via the JsonReader
/// bytecode tokenizer (default OFF until boot-frontier-validated).
///
/// ⭐**ONE BIT OF THE GATE WORD, NOT AN ACQUIRE LOAD** (see `gates::op_gates`). Both readers sit
/// in `invoke.rs`'s `invokestatic`: `gson_claimable` on the replay-plan decision and the intercept
/// itself on the miss path. Together with `call_plan_enabled` below that path read THREE
/// independent `OnceLock`s per interpreted static call — the population is 14.1 M method bodies —
/// and they are now three mask tests over one plain per-thread load LLVM can fold and hoist.
#[cfg(feature = "next-gen-gc")]
#[inline]
fn gson_native_enabled() -> bool {
    crate::opcodes::op_gate(crate::opcodes::OG_GSON, gson_native_uncached)
}

/// The historic reader; fills the word's bit and serves the `RAVA_NO_OP_GATEWORD` arm.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn gson_native_uncached() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_GSON_NATIVE").is_some())
}

fn prof_enabled() -> bool {
    diag::PROF_ON.with(|c| {
        let v = c.get();
        if v >= 0 {
            return v == 1;
        }
        let on = std::env::var("RAVA_PROFILE").is_ok();
        c.set(on as i8);
        on
    })
}

#[inline]
fn prof_sample(class: &str) {
    if prof_enabled() {
        diag::PROF.with(|p| *p.borrow_mut().entry(class.into()).or_insert(0) += 1);
    }
}

/// Is `receiver` a genuinely primitive value standing in a slot that should hold
/// a `TypeAdapterFactory`? Only numeric primitives qualify as the "stray
/// primitive slipped into `Gson.factories`" case. A real factory is an object —
/// a heap handle (`Ref`) on the legacy heap, or an arena pointer (`Obj`) under
/// next-gen-gc — and BOTH must dispatch `create` normally. Classifying `Obj` as
/// non-object (the original `!matches!(Ref | Array)` guard did) collapses every
/// arena-backed factory to a null adapter, which is what made `Gson.getAdapter`
/// throw "GSON cannot handle <type>" for every class under the arena.
fn is_stray_primitive_factory(receiver: &Value) -> bool {
    matches!(
        receiver,
        Value::Int(_) | Value::Long(_) | Value::Float(_) | Value::Double(_)
    )
}

/// A native method rava does not implement. Per JVMS §5.6 / §6.5, a native
/// method that cannot be linked throws `java.lang.UnsatisfiedLinkError` — a
/// catchable `LinkageError`, NOT a fatal VM abort. Guest code routinely probes
/// for optional native backends inside `try`/`catch` (e.g. Netty's
/// `Log4JLoggerFactory` probe, epoll/io_uring transport selection) and relies on
/// catching this to fall back to a pure-Java path. Aborting the process here
/// instead would defeat that contract. The diagnostic detail (signature +
/// originating class) rides along in the exception message.
pub(crate) fn unlinked_native(vm: &crate::vm::thread_context::SharedVm, msg: String) -> VmError {
    VmError::Throw(vm.make_exception_shared("java/lang/UnsatisfiedLinkError", &msg))
}

/// The default value for a method's return descriptor (the part after `)`),
/// used by collect-mode to keep the stack consistent after a skipped native.
pub(crate) fn default_for_return(desc: &str) -> Option<Value> {
    let rt = &desc[desc.rfind(')').map(|i| i + 1).unwrap_or(0)..];
    match rt.bytes().next() {
        Some(b'V') | None => None,
        _ => Some(field_or_default(Value::Null, rt)),
    }
}

/// Namespaces whose classes rava's native layer can serve by NAME (JDK, guava,
/// fastutil). Outside these, a class that merely name-matches is_collection's
/// substring heuristic ("FooList", a bench class "Collections", …) runs its own
/// bytecode with real fields — the interpreter resolves bytecode FIRST and only
/// falls to the natives when resolution fails (JDK) or a force-route applies —
/// so denying it the JIT is pure perf loss (see Vm::jit_native_served_deep).
#[cfg(feature = "next-gen-gc")]
/// Call count for `native_served_prefix` (`RAVA_NSN_COUNT`).
///
/// ⚠The first version was thread-local and the periodic dump reads it from the PROFILER thread,
/// which never calls the predicate — it printed a confident zero. A counter and its reader have
/// to live in the same place.
pub(crate) static NSN_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn nsn_calls() -> u64 {
    NSN_CALLS.load(std::sync::atomic::Ordering::Relaxed)
}

/// `RAVA_NSN_COUNT` — the historic reader behind [`native_served_prefix`]'s call counter. Fills
/// `OG_NSN_COUNT` and serves the `RAVA_NO_OP_GATEWORD` arm.
pub(crate) fn nsn_count_uncached() -> bool {
    crate::env_flag!("RAVA_NSN_COUNT")
}

pub(crate) fn native_served_namespace(class: &str) -> bool {
    // ⭐**A CLASS THAT HAS REAL BYTECODE IS NOT NATIVE-SERVED, EVEN IN `java/`.**
    //
    // This is what lets `java.util` migrate out of Rust one class at a time. Without it the
    // two halves fight: a guest `java/util/ArrayList` must share the natives' `$elems` field
    // or the natives fabricate instances its methods cannot read (`Arrays.asList("a","b","c")
    // .size() == 0`, and MC dies in 0.6 s) — but sharing `$elems` is exactly the signal the
    // native fast paths classify on, so they go on serving `get`/`size` and the bytecode never
    // runs. Measured: with the guest class on the classpath and this gate absent, the list
    // benchmark read **203 ns/element against 171 without it** — interop achieved, win zero.
    //
    // The check is free when nothing is overridden: `has_bytecode` reads one relaxed `bool`
    // that stays false unless a classfile has actually been loaded for a native-served name.
    if crate::class_loader::has_bytecode(class) {
        return false;
    }
    native_served_prefix(class)
}

/// The namespace test alone, without the bytecode-override check — the shape this predicate had
/// before overriding existed. `has_bytecode` uses it to decide what is even worth tracking.
/// ⛔**DO NOT "CACHE" THIS. MEASURED: routing it through `owner_bits` made the boot 7x SLOWER.**
///
/// Six `starts_with` calls on short prefixes looked like an obvious win to replace with the
/// per-owner bitmask `owner_bits` already maintains. Interleaved boot A/B, 3 rounds:
/// **cached bit 374.8 s CPU against the string chain's 54.2 — disjoint.**
///
/// The reason is what the cache is keyed on: `owner_bits` fronts its table with a direct-mapped
/// cache keyed on the string's POINTER, and this predicate is called with freshly built owner
/// strings, so the pointer cache misses and every call falls through to hashing the whole name.
/// Six comparisons that fail on the first byte are cheaper than one hash of the string.
pub(crate) fn native_served_prefix(class: &str) -> bool {
    // ⛔**AND THE INFERENCE FROM THAT REGRESSION WAS WRONG.** Slowing this predicate cost the boot
    // 320 seconds, from which I concluded a call count in the billions — the population every
    // event census this session had failed to find. Counted: **10,029,228 calls**, so ~45 ns of
    // added cost is 0.45 s, not 320. The regression is NOT call volume; routing through
    // `owner_bits` did something else expensive (its `compute` runs a substring search per needle
    // and several owner predicates, and those may re-enter this path). Left counted rather than
    // guessed at, because the guess was the interesting-looking one and it was false.
    // ⭐**ONE ACQUIRE LOAD REMOVED FROM 10,029,228 CALLS PER BOOT** — the number counted three
    // paragraphs up, by this very counter. `env_flag!` expands to a `OnceLock` read, i.e. an
    // ACQUIRE atomic, and it stood UNCONDITIONALLY in front of the six `starts_with` below: the
    // whole point of this predicate is that six comparisons failing on the first byte are cheaper
    // than one hash, and an optimisation barrier was sitting in front of all six. It is now
    // `OG_NSN_COUNT` out of the per-thread word — a plain load, and one LLVM may hoist out of any
    // loop that calls this repeatedly. **Was 1 acquire load per call on a 10.03 M population; is
    // 0.** The counter itself is unchanged and still only ticks when the flag is set.
    if crate::opcodes::op_gate(crate::opcodes::OG_NSN_COUNT, nsn_count_uncached) {
        NSN_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    class.starts_with("java/")
        || class.starts_with("javax/")
        || class.starts_with("jdk/")
        || class.starts_with("sun/")
        || class.starts_with("com/google/common/")
        || class.starts_with("it/unimi/")
}

/// guava collection classes whose own bytecode mis-executes in this interpreter
/// (HashBiMap key lookup) and which we serve from the native map/collection
/// layer instead. guava is a third-party library, so a fast-path is allowed.
/// `RAVA_FORK_SHAPES` gate (M3 F3.2) — enables the forkable-`forEach` invoke intercept. Off by
/// default → the invoke seam is byte-identical.
///
/// ⭐**THE HOTTEST PAIR IN THIS FILE.** `ops/invoke.rs` opens BOTH `0xb6 invokevirtual` and
/// `0xb9 invokeinterface` with `fork_route_foreach() || fork_target_scan()` — two independent
/// `OnceLock` acquire loads standing in front of the engine's most frequent call opcodes, on a
/// population of 14.1 M interpreted method bodies, to answer a question that is false in every
/// shipped configuration. As two atomics LLVM could fold neither into the other nor hoist either
/// out of the dispatch loop. **Was 2 acquire loads per invokevirtual / invokeinterface; is one
/// plain per-thread load and two mask tests, and zero once the load is hoisted.**
#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn fork_route_foreach() -> bool {
    crate::opcodes::op_gate(crate::opcodes::OG_FORK_FOREACH, fork_route_foreach_uncached)
}

/// The historic reader; fills the word's bit and serves the `RAVA_NO_OP_GATEWORD` arm.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn fork_route_foreach_uncached() -> bool {
    static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *F.get_or_init(|| std::env::var_os("RAVA_FORK_SHAPES").is_some())
}

/// `RAVA_FORK_PARALLEL` gate (M3 G1): run an eligible offloadable iterator-loop's per-element
/// method on real worker threads (GIL-free). Off by default → the divert stays serial + bit-exact.
#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn fork_parallel() -> bool {
    crate::opcodes::op_gate(crate::opcodes::OG_FORK_PARALLEL, fork_parallel_uncached)
}

/// The historic reader; fills the word's bit and serves the `RAVA_NO_OP_GATEWORD` arm.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn fork_parallel_uncached() -> bool {
    static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *F.get_or_init(|| std::env::var_os("RAVA_FORK_PARALLEL").is_some())
}

/// G1.5 TARGET HUNT (`RAVA_FORK_TARGET_SCAN`): activate the iterator-loop intercept purely to OBSERVE
/// every for-each loop (signature, n, tree-compiled status, work-character) and return to the normal
/// serial path WITHOUT forking. Inert — no codegen change, no fork machinery, determinism untouched.
///
/// ⭐Read on FIVE hot seams, always beside another gate: twice in `ops/invoke.rs` (paired with
/// `fork_route_foreach`, per `invokevirtual` / `invokeinterface`) and once in `run.rs`'s backward
/// branch (paired with `fork_numeric`, per back edge — the edge every loop must take). Both pairs
/// are now two masks over one shared load.
#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn fork_target_scan() -> bool {
    crate::opcodes::op_gate(crate::opcodes::OG_FORK_SCAN, fork_target_scan_uncached)
}

/// The historic reader; fills the word's bit and serves the `RAVA_NO_OP_GATEWORD` arm.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn fork_target_scan_uncached() -> bool {
    static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *F.get_or_init(|| std::env::var_os("RAVA_FORK_TARGET_SCAN").is_some())
}

/// `RAVA_FORK_NUMERIC` (M3 T2 step 3b): actually FORK an offloadable counted loop's `[lo,hi)`
/// across workers (not just probe). Off by default.
///
/// QUARANTINE (GAP-S09, docs/runtime-gap-register.md): this gate arms an
/// INTERPRETER-ON-WORKER experiment (`Vm::try_fork_counted_loop` runs
/// `opcodes::run` on gil-free workers through the loud
/// `WorkerVmRef::vm_mut_unsound_interp` escape). That aliased `&mut Vm` is
/// OUTSIDE the closed GAP-S09 soundness claim — the claim covers the
/// default-off serial path and the typed compiled-worker surfaces only.
/// Residual typed-interpreter work is filed under GAP-A02. Do not enable on a
/// run whose result you intend to trust without the bit-exact hash gate.
#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn fork_numeric() -> bool {
    crate::opcodes::op_gate(crate::opcodes::OG_FORK_NUMERIC, fork_numeric_uncached)
}

/// The historic reader; fills the word's bit and serves the `RAVA_NO_OP_GATEWORD` arm.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn fork_numeric_uncached() -> bool {
    static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *F.get_or_init(|| std::env::var_os("RAVA_FORK_NUMERIC").is_some())
}

/// Minimum element count for a loop to be worth forking (RAVA_FORK_MIN_N, default 2). Diverting a
/// TINY loop off its JIT-compiled path onto the per-element apply machinery DEOPTIMISES it — the
/// fork only pays off past a threshold. initCache's per-block loops are n≈2-16, below any useful
/// threshold, so a high value keeps them on the fast compiled path (the fork win needs a coarse
/// grain — a flat/large loop — not these).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn fork_min_n() -> usize {
    pub(crate) static F: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *F.get_or_init(|| {
        std::env::var("RAVA_FORK_MIN_N").ok().and_then(|s| s.parse().ok()).unwrap_or(2).max(2)
    })
}

/// True if a call that RESOLVED to declaring class `decl` must be served by the
/// native `$map` instead of run as bytecode — i.e. the method is inherited from a
/// fastutil object map. `force_native_collection` matches the receiver's actual
/// class (direct instances + <init>); this declaring-class half covers a Paper
/// SUBCLASS instance (different actual class, inherited put/get resolving INTO the
/// fastutil base) whose bytecode would otherwise run over the never-built internal
/// arrays. Together they close every invoke seam.
pub(crate) fn route_native_map(decl: &crate::class_loader::LoadedClass) -> bool {
    crate::map::is_fastutil_objmap(&decl.file.this_class)
}

/// Pops the current frame's pointer off `Vm::frame_ptrs` on any exit from `run`
/// (return or `?`-propagated error), keeping the GC frame registry balanced.
struct FramePtrGuard;
impl Drop for FramePtrGuard {
    fn drop(&mut self) {
        crate::frames::pop();
    }
}

fn branch_offset(code: &[u8], pc: usize) -> i32 {
    let hi = code[pc + 1] as i8 as i32;
    let lo = code[pc + 2] as i32;
    (hi << 8) | lo
}

fn u16_operand(code: &[u8], pc: usize) -> u16 {
    ((code[pc + 1] as u16) << 8) | code[pc + 2] as u16
}

/// Big-endian i32 at an absolute bytecode offset (switch tables).
fn read_i32(code: &[u8], off: usize) -> i32 {
    i32::from_be_bytes([code[off], code[off + 1], code[off + 2], code[off + 3]])
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn run_stop_set(pc: usize) { exit::RUN_STOP_PC.with(|c| c.set(pc)); }
/// Serve `instanceof` at a call SITE from a per-(site, receiver class) memo instead of
/// re-deciding it every execution. DEFAULT ON; `RAVA_NO_INSTANCEOF_MEMO` is the kill switch.
///
/// The compiled codegen emits NO inline type test (jit.rs `0xc1`), so every
/// `instanceof` in compiled code is a host callback into the walk below, and the
/// walk is the round-1 defect class in full: a constant-pool `String` for the
/// target, a `String` for the receiver's class (an arena receiver additionally
/// takes the class registry MUTEX and clones an `Arc`), a `$fi` field probe, a
/// fresh `HashSet`, and a breadth walk that CLONES a `String` per class and
/// interface visited. None of it depends on anything but (target, receiver class).
///
/// WHY IT SHIPPED ON THE SECOND MEASUREMENT. Its first boot A/B kept it off, and that
/// was the right call for that tree: Done off min 97.109 vs on min 99.402, i.e. WORSE
/// where it mattered (bench/boot_flag_ab_RAVA_INSTANCEOF_MEMO.csv, the original rows).
/// Re-measured on the current tree — Done 97s then, 23s now, and the ring phase since
/// moved off the GIL — the verdict inverted on the metric it was never judged by:
///
///   wall  off 81.9 / 83.0 / 86.6   on 78.3 / 78.8 / 80.4   -4.4%, ON CLEAN (no overlap)
///   done  off 22.776 min           on 22.835 min           +0.26%, OVERLAP -> neutral
///
/// So the memo pays in the COLD blocks (A+B and C, where a datapack/registry walk meets a
/// type it has not classified before) and is neutral in the ring, where the same few sites
/// see the same few classes and the walk was already short. Done being flat both times is
/// consistent; what changed is that wall is now dominated by the blocks the memo helps.
/// This is the recorded rule in one lever: a lever's value is not a constant of the lever.
///
/// ⭐**AND IT IS NOW A BIT OF THE GATE WORD, NOT AN ACQUIRE LOAD.** `typecheck::instance_of_at`
/// opens with this read, so every `instanceof` and every `checkcast` — interpreted AND compiled,
/// since the codegen emits no inline type test and calls back here — paid one acquire-ordered
/// `OnceLock` load before it could even look at its receiver. **Was 1 acquire load per type test;
/// is one mask over the per-thread word.**
#[inline]
fn instanceof_memo_enabled() -> bool {
    crate::opcodes::op_gate(crate::opcodes::OG_IOF_MEMO, instanceof_memo_uncached)
}

/// The historic reader; fills the word's bit and serves the `RAVA_NO_OP_GATEWORD` arm.
pub(crate) fn instanceof_memo_uncached() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_INSTANCEOF_MEMO").is_none())
}

/// Tag bits separating the three receiver-class identity spaces that share a memo
/// key: an interned class-name POINTER (heap `Ref` receivers), a `&'static str`
/// address (the unboxed-primitive wrapper names) and an arena CLASS ID (`Obj`
/// receivers, untagged). User-space pointers and registry ids all leave the top
/// two bits clear, so the three spaces stay disjoint.
const IOF_REF_TAG: u64 = 1 << 63;
/// Memo census, dumped by `fallback_tally` — a memo nobody counted is a memo
/// nobody can refute (round 1's pure-call memo shipped a lever whose real hit
/// count was ZERO). Only touched when the DIAGNOSTIC flag is also set, so the
/// production path pays nothing for it.
pub(crate) static IOF_HIT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static IOF_MISS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// The dispatch class of an unboxed primitive / `Str` receiver. Keyed on the
/// `&'static str`'s ADDRESS — these are compile-time constants, so the pointer is
/// a perfect identity and no hashing of the text is needed.
fn wrapper_class(name: &'static str) -> Arc<str> {
    dispatch_class(name.as_ptr() as u64 | CLASS_KEY_STATIC_TAG, || Arc::from(name))
}

fn jdk_supers(name: &str) -> Option<&'static [&'static str]> {
    JDK_SUPERS.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

/// Category-2 (two-slot) value test — long/double — for the dup/pop family.
fn is_wide(v: &Value) -> bool {
    matches!(v, Value::Long(_) | Value::Double(_))
}

/// ⛔→✅ **THE 2026-07-29 REFUTATION BELOW DID NOT SURVIVE THE LANDSCAPE. DEFAULT-ON since
/// 2026-08-07** (opt out with `RAVA_NO_ALLOC_ROUTE_MEMO`).
///
/// Re-measured on the boot-trained PGO product, **8 ABBA pairs in three independent batches,
/// ZERO overlap in every one of them**:
///
/// ```text
/// off   wall 45.73   Done 14.98   cpu 89.7
/// on    wall 44.87   Done 14.82   cpu 88.6      -> -1.9% wall, -1.1% Done, -1.2% cpu
/// ```
///
/// The old verdict's specific claim was that `Done` regressed 13.4%. It does not: `Done` is
/// now slightly BETTER, and wall and cpu agree with it. Everything the memo competes with has
/// changed since — PGO and boot-trained PGO, the arena-lambda group default-ON, R9,
/// REF_STATIC_CELLS, FU_FIELD_IDS — and the baseline moved from wall 75-78 s to 45.7.
/// ★**A lever's value is not a constant of the lever.** A refutation is a measurement of a
/// landscape, not a property of the code, and this one is kept below rather than deleted so
/// the reversal stays auditable.
///
/// ⛔ SUPERSEDED — original 2026-07-29 verdict, at wall 75-78 s, kept for the record:
/// Interleaved A/B, 8 runs, contended ones (wall > 80s) excluded; among the clean runs EVERY
/// off run beats EVERY on run:
///
/// ```text
/// off (default)  Done 24.074 24.785   wall 75.6 77.1
/// on             Done 27.803 28.339   wall 78.1 78.1      -> off ahead by 13.4% on Done
/// ```
///
/// Notable because the memoised path is NOT short - it holds the `register_class` mutex and does a
/// linear name scan per `new` - and the memo still lost. Sixth instance in one session of the same
/// shape (dispatch-class memo, site-shape memo, drain_map shape memo, shortcut slot hints,
/// per-thread `super_chain` mirror). Keep it OFF; do not re-derive it from an allocation profile.
///
/// RAVA_ALLOC_ROUTE_MEMO (default OFF): per-call-site memo of the `new` /
/// `anewarray` allocation ROUTE. Without it, EVERY allocation re-derives the
/// routing decision from the class NAME: a String allocation for the name, a
/// `get_or_load` hash lookup, the collection-substring scans
/// (`is_collection`'s 7 `contains` → `memchr`/`is_contained_in`), a super-chain
/// walk re-running those scans per ancestor, an interner hash and the
/// `register_class` mutex + linear name scan — all per `new`, per call. The
/// decision inputs are permanent (one name = one class identity for the
/// process lifetime, `super_chain` is cached-frozen, the deciding flags are
/// process-constant OnceLocks), so the route is memoised per call site;
/// unstable cases (class not yet loadable, incomplete layout) are never
/// inserted and keep re-deriving. Read once.
///
/// ⭐**READ TWICE PER ALLOCATION, AND BOTH WERE ACQUIRE LOADS.** `alloc.rs` consults it on the
/// memo PROBE and again on the memo INSERT for both `new` (lines 202/`alloc_route_insert`) and
/// `anewarray` (lines 75/88/103) — so an allocation that misses paid two, on the population every
/// `new` in the boot belongs to. They are now two masks over one plain per-thread load, which LLVM
/// may also hoist out of an allocating loop. **Was 2 acquire loads per allocation; is 0.**
#[inline]
pub(crate) fn alloc_route_memo_enabled() -> bool {
    crate::opcodes::op_gate(crate::opcodes::OG_ALLOC_MEMO, alloc_route_memo_uncached)
}

/// The historic reader; fills the word's bit and serves the `RAVA_NO_OP_GATEWORD` arm.
pub(crate) fn alloc_route_memo_uncached() -> bool {
    static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *F.get_or_init(|| std::env::var_os("RAVA_NO_ALLOC_ROUTE_MEMO").is_none())
}

/// ⭐**ONE BRANCH FOR THE WHOLE DIAGNOSTIC PROLOGUE** of an interpreted call
/// (default ON, opt-out `RAVA_NO_INVOKE_FASTGATE`).
///
/// `invoke_idx` opens with five independent diagnostic hooks — the call census, the
/// vecgrow event counter, the trace/TPS call hook, the shape census and the noise-caller
/// probe. Each is correctly gated behind its own cached `OnceLock`, and each gate is
/// cheap; there are simply five of them plus their argument setup on a path that row 395
/// prices at **105 ns for a call whose callee is free, against the JVM interpreter's 7**.
/// Ablating the block measured **−12 ns, disjoint** on that call.
///
/// Every one of the five is a pure function of the environment, fixed for the process, so
/// their disjunction is too: when it is false not one of them could have fired, and the
/// block is skipped whole. When ANY is set the block runs exactly as before — this
/// changes what is TESTED, never what is DONE.
pub(crate) fn invoke_hooks_armed() -> bool {
    static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *F.get_or_init(|| {
        if std::env::var_os("RAVA_NO_INVOKE_FASTGATE").is_some() {
            return true; // opt-out: always take the full block, as before
        }
        crate::callcensus::enabled()
            || crate::vecgrow::enabled()
            || crate::gates::call_hook_armed()
            || crate::scratch::noise_caller_probe() > 0
            || {
                #[cfg(feature = "next-gen-gc")]
                {
                    crate::shape_census::enabled()
                }
                #[cfg(not(feature = "next-gen-gc"))]
                {
                    false
                }
            }
    })
}

/// ⭐**RESOLVED-SITE CALL REPLAY** — default ON, opt-out `RAVA_NO_CALL_PLAN`.
///
/// Replays a resolved call site's entry decision instead of re-deriving it on every call;
/// `SiteRef::plan` says what is constant and what stays re-checked. Armed on `invokestatic`
/// and on `invokespecial`, which is the constructor path — row 395 measures the two frames
/// of a `new Point(...)` at 400 ns against 40 for the `new` and the allocation together.
///
/// ⛔**IT MEASURED EXACTLY ZERO THE FIRST TIME, AND THAT IS THE LESSON.** The first version
/// skipped the intercept gates, the thread-local resolve lookup and the method-index
/// re-derivation — and still cloned the `Arc<LoadedClass>` to hand to `invoke_idx`, which
/// took it by value. That clone/drop pair is 12 ns of a 105 ns call. **A memo that replays a
/// decision but keeps the decision's most expensive side effect saves nothing.** Once
/// `invoke_idx` was changed to BORROW, the same code measured **−23.4%, every A below every
/// B**, and −15.6% on a second series.
///
/// ⭐**THE CALL PATH READ IT TWICE, AS TWO ACQUIRE LOADS.** `invoke.rs` tests it at the top of
/// both `invokestatic` (line 153) and `invokespecial` (line 349) and AGAIN on the plan-install
/// side of each (lines 297 / 460), so an interpreted static or special call that had not yet
/// installed its plan paid two — plus `gson_native_enabled`'s, making **three acquire loads on the
/// path row 395 prices at 105 ns for a call whose callee is free**, against the JVM
/// interpreter's 7 ns. All three are now mask tests over one plain per-thread load.
#[inline]
pub(crate) fn call_plan_enabled() -> bool {
    crate::opcodes::op_gate(crate::opcodes::OG_CALL_PLAN, call_plan_uncached)
}

/// The historic reader; fills the word's bit and serves the `RAVA_NO_OP_GATEWORD` arm.
pub(crate) fn call_plan_uncached() -> bool {
    static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *F.get_or_init(|| std::env::var_os("RAVA_NO_CALL_PLAN").is_none())
}

/// Insert a route into the memo — miss-path only. A GIL-free worker must not
/// mutate the shared map (this path takes no lock); it simply keeps
/// re-deriving, so the memo stays a main-thread-written, region-frozen table.
#[cfg(feature = "next-gen-gc")]
fn alloc_route_insert(vm: &crate::vm::thread_context::SharedVm, site: (usize, u16), route: NewRoute) {
    if alloc_route_memo_enabled() && !crate::parallel::gilfree() {
        vm.runtime_shared().alloc_route_insert(site, route);
    }
}

/// Encode a lambda CAPTURE into its cell. Identical to `encode_arena_cell` except a
/// primitive value destined for a REFERENCE-typed slot (the descriptor said `Object`/
/// `L…` but the runtime value is a boxed `Integer`/`Long`/… ⇒ `Value::Int`/`Long` in
/// the unboxed model) is BOXED into the 0b11 side table rather than stored raw. A raw
/// primitive in a ref slot would be traced by the per-class `ref_mask` and mis-decoded
/// as a pointer (→ `Null`/garbage → a codec parse failure); boxing makes decode recover
/// the exact Value and the GC skip it. Primitive-typed slots keep the raw form.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn encode_capture_cell(v: &Value, ref_slot: bool) -> i64 {
    if ref_slot && matches!(v, Value::Int(_) | Value::Long(_) | Value::Float(_) | Value::Double(_)) {
        return ((crate::nextgen::arena_box_push(v.clone()) as i64) << 2) | 3;
    }
    encode_arena_cell(v)
}

/// RAVA_TRACE_STALEFIELD, read ONCE. Its main test site is the arena `getfield` receiver
/// decode and stood as the FIRST operand of an `&&`, so every arena field read performed a
/// Win32 environment-block query (a lock plus a UTF-16 encode of the name) before the cheap
/// class-id check could short-circuit it. Measured with the RIP sampler on a real boot:
/// `RtlQueryEnvironmentVariable` was ~1% of Block B's wall with the flag UNSET. This is the
/// same defect the `RAVA_TRACE_NULLREF` gate below already documents and avoids.
///
/// ⭐**NOW A BIT OF THE GATE WORD.** `fields.rs` already folded the field-diagnostic FAMILY behind
/// its own `field_diag_armed()`, but this gate is still read directly at three further sites —
/// `arena_lambda.rs:70` and the two `&& trace_stalefield()` arms in `fields.rs` — and it is one of
/// `field_diag_armed`'s own inputs. One plain load instead of an acquire one, on the arena
/// field-decode path.
#[inline]
fn trace_stalefield() -> bool {
    crate::opcodes::op_gate(crate::opcodes::OG_STALEFIELD, trace_stalefield_uncached)
}

/// The historic reader; fills the word's bit and serves the `RAVA_NO_OP_GATEWORD` arm.
pub(crate) fn trace_stalefield_uncached() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_TRACE_STALEFIELD").is_some())
}

/// GC-SAFETY: root the popped receiver + args in gc_protect for the duration of an invoke
/// dispatch (they are off the frame's operand stack, so no longer GC roots; a collection in
/// a GC-capable dispatch call would sweep a heap object held only here → its handle reused
/// under a new type = a use-after-free stale receiver). Drop truncates on every exit path.
/// The raw ptr aliases nothing live at drop (no vm_ref UB). gc_protect roots BOTH the heap
/// mark-sweep and the arena collector.
#[cfg(feature = "next-gen-gc")]
pub(crate) struct CallRootGuard(*mut Vec<Value>, usize);
#[cfg(feature = "next-gen-gc")]
impl Drop for CallRootGuard {
    fn drop(&mut self) {
        if self.0.is_null() {
            return; // gilfree no-op guard (a fork worker never touched gc_protect)
        }
        // SAFETY: the pointer was minted from the `&mut` the push went through and
        // aliases nothing live at drop. The facade helper routes the truncation to the
        // active context when a worker context is installed, so the guard still
        // truncates the root stack its pushes landed in.
        unsafe {
            crate::vm::thread_context::truncate_roots(&mut *self.0, self.1);
        }
    }
}
/// Depth budget for the JVM-semantics record hash/equals recursion
/// (`RAVA_RECORD_SEMANTICS`): a record component that is itself a record
/// re-enters `record_object_method` through its own virtual `hashCode`/`equals`.
/// Record components are final, so a TRUE cycle can only arise through a mutable
/// non-record object on the path — on the JVM that same shape recurses to
/// StackOverflowError. The cap keeps the HOST stack safe instead: past it the
/// legacy string-render path serves the subtree (deterministic, never recursive).
const RECORD_SEM_DEPTH_CAP: u32 = 256;

thread_local! {
    pub(crate) static RECORD_SEM_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// RAII depth ticket: `enter()` refuses (→ caller uses the legacy path) once the
/// cap is reached; Drop restores the level on every exit, including `?` errors.
struct RecordSemDepth;
impl RecordSemDepth {
    fn enter() -> Option<Self> {
        RECORD_SEM_DEPTH.with(|c| {
            let d = c.get();
            if d >= RECORD_SEM_DEPTH_CAP {
                None
            } else {
                c.set(d + 1);
                Some(RecordSemDepth)
            }
        })
    }
}
impl Drop for RecordSemDepth {
    fn drop(&mut self) {
        RECORD_SEM_DEPTH.with(|c| c.set(c.get().saturating_sub(1)));
    }
}

/// `Float.hashCode` / `floatToIntBits`: canonical NaN, signed zeros distinct.
fn java_float_bits(f: f32) -> u32 {
    if f.is_nan() { 0x7fc0_0000 } else { f.to_bits() }
}

/// What a record call site needs that is a pure function of the site and the
/// receiver's runtime class: the class name, its simple name, and the resolved id
/// of every component field.
///
/// WHY IT EXISTS. `record_object_method` serves every synthesised `toString` /
/// `hashCode` / `equals` — ~90 k calls per `RsProbe` run, and the dominant guest
/// work of `RandomState.create` — and each call rebuilt all three FROM NAMES: a
/// fresh `String` of the ~50-character class name, a fresh `String` of its simple
/// name, and one `resolve_field_id` per component (a thread-local lookup that
/// hashes owner+name even on a hit). That is the boot's recurring defect class,
/// per-call bookkeeping that is O(string), and it is the same fix already applied
/// to `fu_iter_field_ids`: hoist the derivation off the per-call path.
///
/// SOUNDNESS. The indy sits inside the record's own synthesised method and records
/// are final, so the receiver's class at a given site is fixed; the key carries the
/// class id anyway, so a surprise receiver simply misses and rebuilds.
///
/// No epoch guards. Field ids are interned names — neither address- nor
/// handle-keyed — so `GC_EPOCH` / `HANDLE_EPOCH` do not apply. The loader epoch does
/// not either: a record's components are declared final in the record itself, so
/// `resolve_field_id` finds the declaring class at the first level of the chain and
/// no later class load can change the answer. (The engine already takes this
/// position: `tlcache::field_key_get` caches field ids with no loader-epoch tag.)
/// Keying on it would be strictly worse than useless — a boot loads classes
/// continuously, so the epoch would churn and every call would rebuild.
struct RecordLink {
    class: Box<str>,
    simple: Box<str>,
    ids: Box<[u32]>,
}

thread_local! {
    pub(crate) static RECORD_LINKS: std::cell::RefCell<
        crate::FastMap<(usize, u16, u32), std::rc::Rc<RecordLink>>,
    > = std::cell::RefCell::new(crate::FastMap::default());
}




mod exit;
pub(crate) use exit::*;
pub(crate) mod checkpoint;
pub(crate) use checkpoint::*;
pub(crate) mod ops;
mod hot;
pub(crate) use hot::*;
mod run;
pub(crate) use run::*;
pub(crate) mod exception_entry;
mod invoke_virtual;
pub(crate) use invoke_virtual::*;
mod invoke;
pub(crate) use invoke::*;
mod indy;
pub(crate) use indy::*;
pub(crate) mod fields;

/// The single implementation of the pure numeric conversions and float/double arithmetic,
/// shared by the dispatch loop and the frame-free executor. See the module header.
pub(crate) mod pure_ops;
pub(crate) use fields::*;
mod alloc;
pub(crate) use alloc::*;
pub(crate) mod arena_cells;
pub mod hot_census;
pub(crate) use arena_cells::*;
mod typecheck;
pub(crate) use typecheck::*;
mod casts;
pub(crate) use casts::*;
mod records;
pub(crate) use records::*;
mod gates;
pub(crate) use gates::*;
pub(crate) mod statics;
pub(crate) use statics::*;
mod classkey;
pub(crate) use classkey::*;
mod equality;
pub(crate) use equality::*;
mod arena_lambda;
pub(crate) use arena_lambda::*;
mod diag;
pub(crate) use diag::*;
mod native_gate;
pub(crate) use native_gate::*;
mod names;
pub(crate) use names::*;
mod switch;
pub(crate) use switch::*;
