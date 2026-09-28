//! Entry points: what a call is offered to, in what order, and what it means when every stage refuses.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::natives::*;

/// RAII guard rooting a native's arg buffer across the call (Wall #19). Registers the
/// `&[Value]` slot range on construction and pops it on drop — so EVERY early return
/// from `try_invoke` restores the stack. GIL-synchronous, so the arena collector's
/// in-place forwarding of these slots never races the native's reads.
#[cfg(feature = "next-gen-gc")]
pub(crate) struct NativeArgsGuard(usize);
#[cfg(feature = "next-gen-gc")]
impl NativeArgsGuard {
    #[inline]
    pub(crate) fn new(args: &[Value]) -> Self {
        NativeArgsGuard(crate::frames::native_args_push(args.as_ptr(), args.len()))
    }
}
#[cfg(feature = "next-gen-gc")]
impl Drop for NativeArgsGuard {
    #[inline]
    fn drop(&mut self) {
        crate::frames::native_args_truncate(self.0);
    }
}

/// BUG-3 GENERAL FIX (compiled → native string representation barrier):
/// compiled code represents freshly created String constants / concat results
/// as arena-string `Value::Obj` (unbox_ref keeps them pointer-based — B5,
/// "natives/dispatch stay handle-based"). Natives pattern-match `Value::Str`,
/// so an arena-string arg would be silently missed — e.g. a compiled
/// `throw new RuntimeException("m")` reached Throwable.<init> as Value::Obj and
/// its detailMessage was never stored → getMessage()==null. Canonicalize every
/// arena-string arg to `Value::Str` at this single native-dispatch boundary so
/// ALL natives see one uniform string representation. `None` (no clone, no
/// allocation) when no arg is an arena string. General — not Throwable-specific.
///
/// ⛔ MEASURED NULL 2026-07-29. Interleaved A/B, 8 runs, ALL clean (71.1-74.6s wall):
///   on  Done 22.239 22.334 22.709 22.791  mean 22.518  wall min 71.1  mean 72.60
///   off Done 22.116 22.465 22.800 23.214  mean 22.649  wall min 71.6  mean 72.53
/// Done minimum favours OFF, Done mean favours ON, wall minimum favours ON, wall mean favours
/// OFF - no two statistics agree, and every difference is under 0.6%. That is a null, not a
/// small win. Keep OFF; the copy it removes is real but is not where the time is.
/// COST (fix:allocator-traffic): this is the per-call copy the once-per-owner
/// bridge fix left behind — a fresh `Vec<Value>` plus, per string argument, TWO
/// mallocs and TWO copies (`from_utf8_lossy().into_owned()` builds a `String`,
/// `.into()` copies it again into the `Arc<str>`). Under RAVA_CANON_SCRATCH the
/// buffer is checked out of `frames::pool_take` (returned CLEARED on drop) and
/// each string is copied ONCE, straight from the arena bytes into the Arc.
///
/// ⭐**AND THE SCRATCH ARM IS NOW THE DEFAULT ARM (`RAVA_NO_FAST_CASCADE=1` restores the
/// historic one).** This is the bridge PROLOGUE: it runs on every native call carrying a string,
/// before the cascade has decided whether any arm will serve, and it is the reason
/// `try_invoke_chain` is a row of its own in the allocation census (3.5% of sampled allocation
/// stacks). The historic arm spends THREE mallocs per two-argument call with one string — the
/// `Vec`, the `String` temporary, the `Arc<str>` — and frees the temporary immediately; the
/// scratch arm spends ONE, because the `Vec` comes out of the thread's frame pool and the arena
/// bytes go straight into the `Arc`. Nothing about the RESULT differs: the same `Value::Str` with
/// the same text, and for invalid UTF-8 the identical lossy route. Drop order is what makes the
/// pooled buffer safe and it is not incidental — `_na_guard` is declared AFTER `_canon` at both
/// call sites, so the GC rooting is popped before the buffer returns to the pool.
///
/// `gates` is the caller's already-loaded gate word: the arm choice used to be
/// `canon_scratch() || fast_cascade()`, i.e. TWO acquire-ordered `OnceLock` loads from two
/// statics, read on every canonicalised call to pick between two arms that never change for the
/// life of the process. It is now one bit of a word the bridge prologue already holds — and since
/// `RAVA_NO_FAST_CASCADE5` that word is the SINGLE one the whole native entry reads.
///
/// ⭐**THE TEST IS THE HOT HALF AND THE BUILD IS THE COLD ONE, AND THEY USED TO SHARE A BODY.**
/// The scan below answers `None` for the overwhelming majority of crossings — most native calls
/// carry no arena string at all — but it sat at the top of a function that also contains two
/// `Vec` builds, a pool checkout, `from_utf8_lossy`, and `Arc<str>` construction. A caller cannot
/// inline a body that size, so every crossing paid a real call, a frame and the register pressure
/// of code it was about to decline. Split: the scan is `#[inline]` and folds into the two bridge
/// prologues, and everything after it lives in [`canon_arena_strings_build`], out of line.
///
/// Nothing about the ANSWER changed — the same predicate decides, the same arm builds, in the
/// same order, and the kill-switch arm inside the builder is untouched.
#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn canon_arena_strings(gates: u32, args: &[Value]) -> Option<CanonArgs> {
    if !args.iter().any(|a| matches!(a, Value::Obj(p) if crate::nextgen::is_arena_string(*p))) {
        return None;
    }
    Some(canon_arena_strings_build(gates, args))
}

/// The canonicalising half of [`canon_arena_strings`], reached only once the scan has PROVEN at
/// least one argument is an arena string. Verbatim, including the kill-switch arm choice.
#[cfg(feature = "next-gen-gc")]
#[inline(never)]
fn canon_arena_strings_build(gates: u32, args: &[Value]) -> CanonArgs {
    // Under the kill switch each constant is read at its own site again, exactly as before.
    let scratch = if gates & G_DISPTLS_LEGACY != 0 {
        crate::canon_scratch() || fast_cascade()
    } else {
        gates & G_SCRATCH != 0
    };
    if scratch {
        let mut buf = crate::frames::pool_take();
        buf.reserve(args.len());
        let mut strs = 0u64;
        let mut bytes = 0u64;
        for a in args {
            buf.push(match a {
                Value::Obj(p) if crate::nextgen::is_arena_string(*p) => {
                    let b = crate::nextgen::arena_str_bytes(*p);
                    strs += 1;
                    bytes += b.len() as u64;
                    match std::str::from_utf8(b) {
                        // ONE allocation, straight from the borrowed slice. Identical text to
                        // the lossy path for valid UTF-8; invalid sequences take the historic
                        // route. (`RStr` is a thin pointer — see its doc comment.)
                        Ok(s) => Value::Str(s.into()),
                        Err(_) => Value::Str(String::from_utf8_lossy(b).into_owned().into()),
                    }
                }
                other => other.clone(),
            });
        }
        bridgestat::note_canon(strs, bytes);
        return CanonArgs { buf, pooled: true };
    }
    if bridgestat::enabled() {
        let (strs, bytes) = args.iter().fold((0u64, 0u64), |(s, b), a| match a {
            Value::Obj(p) if crate::nextgen::is_arena_string(*p) => {
                (s + 1, b + crate::nextgen::arena_str_bytes(*p).len() as u64)
            }
            _ => (s, b),
        });
        bridgestat::note_canon(strs, bytes);
    }
    let buf: Vec<Value> = args
        .iter()
        .map(|a| match a {
            Value::Obj(p) if crate::nextgen::is_arena_string(*p) => Value::Str(
                String::from_utf8_lossy(crate::nextgen::arena_str_bytes(*p))
                    .into_owned()
                    .into(),
            ),
            other => other.clone(),
        })
        .collect();
    CanonArgs { buf, pooled: false }
}

/// Serve one native call against ONE declared owner. Pays the bridge prologue
/// (profiler frames, arena-string canonicalization, GC rooting of the arg buffer)
/// and then runs the owner dispatch.
pub(crate) fn try_invoke(
    vm: &mut Vm,
    owner: &str,
    name: &str,
    descriptor: &str,
    args: &[Value],
) -> Result<Option<Option<Value>>, VmError> {
    let r = try_invoke_marked(vm, owner, name, descriptor, args);
    // Record the FACT that the natives answered — see `native_claimed`. Inert (one relaxed load)
    // unless the namespace-compile relaxation armed the table.
    if matches!(r, Ok(Some(_))) && crate::native_claimed::armed() {
        crate::native_claimed::mark(owner, name, descriptor);
    }
    r
}

/// ⭐**ONE READ FOR THE WHOLE NATIVE-ENTRY PROLOGUE.**
///
/// ⭐**AND SINCE `RAVA_NO_FAST_CASCADE5`, NOT A PER-CALL READ AT ALL.** The bits this builds
/// (`G_COUNT`, `G_HIST`, `G_BRIDGESTAT`, `G_SCRATCH`, `G_DISPTLS_LEGACY` — all declared in
/// `cascade.rs` beside the rest of the word) are folded into `cascade_gates()`, which
/// `try_invoke_marked` and `try_invoke_chain` now read instead. This function stays as the ONE
/// place those five env vars are consulted, is called once during that word's initialisation,
/// and is on the per-call path again only under the kill switch.
///
/// Each of these was an independent, default-off `OnceLock` living in its own static, and a
/// single native call touched up to four of them: `RAVA_NATIVE_COUNT` and `RAVA_NATIVE_HIST` in
/// `try_invoke_marked`, `bridgestat::enabled()` in `try_invoke_inner` and `try_invoke_chain`, and
/// `canon_scratch()` + `fast_cascade()` inside `canon_arena_strings`. A `OnceLock` read is an
/// ACQUIRE-ordered atomic load: LLVM may not hoist it out of a loop, and it may not forward one
/// across the opaque dispatcher calls that sit between these sites — so four statics meant four
/// loads and four branches, per native call, to learn that no instrument is armed.
///
/// The word is read ONCE at the top of the prologue and threaded down (`try_invoke_inner` and
/// `canon_arena_strings` take it as an argument rather than re-reading), so the default path is
/// one load and a run of predicted bit tests. Every bit is the SAME predicate read from the SAME
/// accessor, so an armed run behaves exactly as it did — this is the `cascade_gates()` shape one
/// layer up, applied to the layer that calls it.
#[inline]
pub(crate) fn entry_gates() -> u32 {
    static W: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *W.get_or_init(|| {
        let mut w = 0u32;
        if crate::env_flag!("RAVA_NATIVE_COUNT") {
            w |= G_COUNT;
        }
        if crate::env_flag!("RAVA_NATIVE_HIST") {
            w |= G_HIST;
        }
        if bridgestat::enabled() {
            w |= G_BRIDGESTAT;
        }
        #[cfg(feature = "next-gen-gc")]
        if crate::canon_scratch() || fast_cascade() {
            w |= G_SCRATCH;
        }
        // Falls together with the other emit-more switches, as every gate in this tree does.
        if std::env::var_os("RAVA_NO_FAST_DISPTLS").is_some() || crate::flags::emit_more_disabled()
        {
            w |= G_DISPTLS_LEGACY;
        }
        w
    })
}

/// The bridge prologue's word, read ONCE per native call.
///
/// ⭐**ONE FACT, THREE STATICS.** "What did the environment say" is fixed for the life of the
/// process, and a native call used to ask it three times: here (`entry_gates`), in
/// `dispatch_owner` (`gate_mode`), and at the top of the cascade (`cascade_gates`) — three
/// acquire-ordered `OnceLock` loads from three addresses, three branches, none of them hoistable
/// or CSE-able because the dispatchers between them are opaque to the optimiser. And the second
/// and third were paid PER SUPER-CHAIN LINK, because `try_invoke_chain` calls `dispatch_owner`
/// once per link. `cascade_gates()` now carries all three sets of bits, so this is the only load,
/// and it is threaded down as an argument the whole way.
///
/// Under `RAVA_NO_FAST_CASCADE5` the entry bits are read back from `entry_gates()`'s own word at
/// this site, exactly as they were; the value is identical (that word is what `cascade_gates`
/// folded in), so the switch restores the READ and nothing else.
///
/// ⭐**AND THAT SECOND READ IS AN IDENTITY, SO IT IS GONE (`RAVA_NO_CALLSEQ`).** Round 5 measured
/// slower and was demoted to opt-in, which leaves `G_LEGACY5` SET in every default run — so THIS
/// is the arm every native crossing actually takes, and it is two acquire `OnceLock` loads from
/// two statics to rebuild a word the first of them already carries. `cascade_gates()` ORs in
/// `entry_gates() & G_ENTRY_BITS`, and `entry_gates()` can set NO bit outside `G_ENTRY_BITS` — so
/// `(w & !G_ENTRY_BITS) | entry_gates() == w`, for every value either function can produce, in
/// every cfg. That premise is not a claim in a comment: `cascade_gates()` asserts it once while
/// its own word initialises, where it costs a boot nothing.
///
/// ⚠**This is NOT round 5 creeping back on.** Round 5 is about which LAYER reads the word, and
/// the two layers below this one — `gate_mode()` in `dispatch_owner` and `cascade_gates()` in the
/// cascade, both paid once per SUPER-CHAIN LINK — are deliberately left exactly as they are, on
/// the strength of that measurement. What changes here is only that the bridge stops deriving
/// the same word twice: historically it read ONE word (`entry_gates()`), and it reads one again.
#[inline]
fn bridge_gates() -> u32 {
    let w = cascade_gates();
    // Kill switch: the historic recomposition, at this site, per call.
    if w & (G_LEGACY5 | G_CALLSEQ) == G_LEGACY5 {
        return (w & !G_ENTRY_BITS) | entry_gates();
    }
    w
}

/// `gate_mode()`'s two bits in the shared word's positions. Called once, while `cascade_gates()`
/// initialises, and again per call only under the kill switch.
#[inline]
pub(crate) fn gate_mode_bits() -> u32 {
    let m = gate_mode();
    (if m & GM_ON != 0 { G_GATE } else { 0 }) | (if m & GM_VERIFY != 0 { G_GATEV } else { 0 })
}

fn try_invoke_marked(
    vm: &mut Vm,
    owner: &str,
    name: &str,
    descriptor: &str,
    args: &[Value],
) -> Result<Option<Option<Value>>, VmError> {
    // ⭐**EVERY PROCESS CONSTANT OF THE NATIVE ENTRY, ONE READ** — see `bridge_gates`. This word
    // is read here, at the top of the prologue, and then THREADED DOWN to `try_invoke_inner`,
    // through it to `canon_arena_strings` AND to `dispatch_owner`, which passes it to
    // `dispatch_owner_cascade` and on to `try_last_arm`. One acquire load per native call, where
    // there were three.
    let dbg = bridge_gates();
    if dbg & G_COUNT != 0 {
        NATIVE_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    // Allocation attribution: the region nests, so a native reached from an interpreted frame
    // is charged HERE and not to the frame — which is the split the 87.6% figure needs.
    let _reg = crate::alloc_sites::RegionGuard::enter(crate::alloc_sites::Region::Native);
    if dbg & G_HIST != 0 {
        return try_invoke_hist(vm, dbg, owner, name, descriptor, args);
    }
    try_invoke_inner(vm, dbg, owner, name, descriptor, args)
}

/// `RAVA_NATIVE_HIST`'s body, lifted OUT of [`try_invoke_marked`] verbatim.
///
/// ⭐**A DISARMED INSTRUMENT STILL COSTS ITS CODE.** This block is ~70 lines — two `Instant`s, a
/// `String` per slow sample, six `format!`s, a `Mutex` — and it sat INSIDE the function every
/// native crossing runs. Its cost when the bit is clear is not the branch, which predicts
/// perfectly; it is that a caller cannot inline a body this size, that its spills widen the
/// frame the default path also pays for, and that its `String` temporaries give the default path
/// drop landing pads it has no use for. `#[cold]` + `#[inline(never)]` moves the whole thing to
/// the far end of the section and leaves the prologue a load, three bit tests and a tail call.
///
/// Nothing inside changed: the same depth gate, the same two checkpoints, the same bucket
/// arithmetic, the same `note_slow` key. The caller's `RegionGuard` is still live across it,
/// exactly as when the block was inline.
#[cold]
#[inline(never)]
fn try_invoke_hist(
    vm: &mut Vm,
    dbg: u32,
    owner: &str,
    name: &str,
    descriptor: &str,
    args: &[Value],
) -> Result<Option<Option<Value>>, VmError> {
    // ⚠**ONLY THE OUTERMOST CALL IS TIMED.** These natives re-enter the guest (`String.valueOf`
    // runs the object's `toString`), which calls natives again — so an inclusive timer at
    // every level bills every child to every ancestor. Depth-gating makes the totals a real
    // partition of wall time: each nanosecond is attributed to exactly one call.
    if HIST_DEPTH.with(|d| d.get()) != 0 {
        return try_invoke_inner(vm, dbg, owner, name, descriptor, args);
    }
    HIST_DEPTH.with(|d| d.set(1));
    // What IS 3.96 ms of work for a sub-64-byte string? Snapshot the two populations that
    // can explain it — interpreted opcodes and nested native calls — across the outermost
    // call, so the answer is "it executes N opcodes" or "it makes N nested natives" rather
    // than a guess.
    let ops0 = crate::opcodes::checkpoint::ops_all();
    let nat0 = NATIVE_CALLS.load(std::sync::atomic::Ordering::Relaxed);
    let t0 = std::time::Instant::now();
    let r = try_invoke_inner(vm, dbg, owner, name, descriptor, args);
    let ns = t0.elapsed().as_nanos().max(1) as u64;
    let b = (63 - ns.leading_zeros() as usize).min(31);
    NATIVE_HIST[b].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if ns >= 8192 {
        // For the slow population the ARGUMENT's class is the question, not the callee's
        // name: `String.valueOf(Object)` re-enters the guest through `toString()`, so this
        // timer is INCLUSIVE and 145 us per call is somebody else's work billed here.
        let arg = match args.first() {
            Some(Value::Ref(h)) => vm.heap.class_of(*h).unwrap_or("?").to_string(),
            #[cfg(feature = "next-gen-gc")]
            Some(Value::Obj(p)) => crate::opcodes::arena_array_descriptor(*p).unwrap_or_else(|| {
                crate::nextgen::class_name(crate::nextgen::arena_class_id(*p))
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "?obj".to_string())
            }),
            Some(Value::Str(_)) => "<string>".to_string(),
            Some(_) => "<prim>".to_string(),
            None => "<none>".to_string(),
        };
        // Is the render objectively huge, or is the path pathological? The produced string's
        // LENGTH separates the two: a 100 KB render of a deeply recursive DFU type is
        // inherent work that only a memo can remove, while a short string means the cost is
        // in how we build it.
        let len = match &r {
            Ok(Some(Some(Value::Str(s)))) => s.as_str().len(),
            _ => usize::MAX,
        };
        let bucket = if len == usize::MAX {
            "?".to_string()
        } else if len < 64 {
            "<64B".to_string()
        } else if len < 1024 {
            "<1KB".to_string()
        } else if len < 16384 {
            "<16KB".to_string()
        } else if len < 262144 {
            "<256KB".to_string()
        } else {
            ">=256KB".to_string()
        };
        let dops = crate::opcodes::checkpoint::ops_all().saturating_sub(ops0);
        let dnat = NATIVE_CALLS
            .load(std::sync::atomic::Ordering::Relaxed)
            .saturating_sub(nat0);
        note_slow(
            owner,
            &format!("{name}({arg}) len{bucket} ops~{} nat~{}", dops / 1000 * 1000, dnat),
            ns,
        );
    }
    HIST_DEPTH.with(|d| d.set(0));
    r
}

/// `gates` is the caller's `bridge_gates()` word (see `try_invoke_marked`), not re-read here —
/// and it now carries the cascade's bits too, so `dispatch_owner` below takes it rather than
/// loading `gate_mode()` and `cascade_gates()` of its own.
fn try_invoke_inner(
    vm: &mut Vm,
    gates: u32,
    owner: &str,
    name: &str,
    descriptor: &str,
    args: &[Value],
) -> Result<Option<Option<Value>>, VmError> {
    // 20.3 wall-profile: time spent inside the native/JDK bridge layer (collections, string ops,
    // IO, math, reflection). Nested guest re-entry (a native that calls back a guest lambda) pushes
    // its own category, so this is native-bridge self-time.
    //
    // ⚠**AUDITED, NOT CHANGED: these three cost NOTHING in a product build.** `wall-profile` is
    // not a default feature, so `crate::rava_sampler` resolves to `rava_sampler_stub`, whose
    // `enter` / `register_bridge` / `bridge_scope` are `#[inline]` no-ops returning ZSTs and whose
    // `enabled()` is a `const false`. They are not `OnceLock` reads on this path and gating them
    // would add a load to remove nothing.
    let _wp = crate::rava_sampler::enter(crate::rava_sampler::NATIVE_BRIDGES);
    // SAMPLED bridge attribution: publish which bridge this is, so the sampler's NATIVE_BRIDGES
    // ticks decompose the same way JIT_EXEC ticks decompose by method. One relaxed store; unlike
    // the timing census below, per-entry overhead is not billed to the bridge as work.
    let _bsc = crate::rava_sampler::bridge_scope(crate::rava_sampler::register_bridge(owner, name));
    // RAVA_BRIDGE_STAT: per-(owner,name) time/count tally (inert when unset). WAS: an acquire
    // `OnceLock` load inside `bridgestat::enter`, on every native call. IS: a bit of the word the
    // caller already holds; the disarmed answer is the same `None` guard, whose `Drop` is a no-op.
    let _bs = if gates & G_BRIDGESTAT != 0 || gates & G_DISPTLS_LEGACY != 0 {
        bridgestat::enter(owner, name)
    } else {
        None
    };
    #[cfg(feature = "next-gen-gc")]
    let _canon = canon_arena_strings(gates, args);
    #[cfg(feature = "next-gen-gc")]
    let args: &[Value] = _canon.as_deref().unwrap_or(args);
    #[cfg(feature = "next-gen-gc")]
    let _na_guard = NativeArgsGuard::new(args);
    dispatch_owner(vm, gates, owner, name, descriptor, args)
}

/// How many NATIVE invocations does a boot make, from every caller — compiled code, the
/// interpreter, and other natives alike?
///
/// ⭐**The arithmetic did not close without this.** Every population counted so far is in the tens
/// of millions (32.0 M compiled->host crossings, 5.9 M compiled entries, 3.4 M interpreted
/// entries, 13.5 M seam callbacks), and `bench/CrossCost.java` prices a crossing at 70-140 ns.
/// That accounts for a few seconds of a FORTY second boot. Either the per-event cost is ten
/// times what it measures, or there is an event nobody has counted.
thread_local! {
    /// Non-zero while a timed native invocation is on this thread's stack.
    static HIST_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

pub(crate) static NATIVE_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Log2 histogram of native-invocation DURATIONS, taken inside the boot (`RAVA_NATIVE_HIST`).
///
/// ⭐**An average cannot separate the three candidate diagnoses; a distribution can.** The boot
/// pays ~727 ns per counted event while the same operation measures 142 ns in a warm
/// microbenchmark. If most calls sit near the microbenchmark and a thin tail carries the mean,
/// the average is an artefact of interference (GC, allocation) billed to whatever call was
/// running. If the whole distribution has moved, the operation really is slower in the boot and
/// the microbenchmark is not measuring the same thing.
///
/// Bucket i holds calls of 2^i..2^(i+1) nanoseconds; bucket 0 is <2 ns, bucket 30 is ~1 s+.
pub(crate) static NATIVE_HIST: [std::sync::atomic::AtomicU64; 32] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 32];

/// Which natives land in the SLOW buckets. Keyed `owner.name`, value = (calls, total ns).
///
/// ⭐**The histogram found the first real concentration of the session: ~1.5% of native calls
/// (those over 8 us) carry roughly 8 of the ~11 seconds native calls cost a boot.** 78,000 calls
/// averaging ~100 us each is a small enough population to NAME, which is what this does.
pub(crate) static SLOW: std::sync::Mutex<Option<std::collections::HashMap<String, (u64, u64)>>> =
    std::sync::Mutex::new(None);

fn note_slow(owner: &str, name: &str, ns: u64) {
    let mut g = SLOW.lock().unwrap_or_else(|e| e.into_inner());
    let map = g.get_or_insert_with(std::collections::HashMap::new);
    let e = map.entry(format!("{owner}.{name}")).or_insert((0, 0));
    e.0 += 1;
    e.1 += ns;
}

pub(crate) fn slow_report(top: usize) -> String {
    let g = SLOW.lock().unwrap_or_else(|e| e.into_inner());
    let Some(map) = g.as_ref() else {
        return "[NATIVE-SLOW] no samples".to_string();
    };
    let mut rows: Vec<(&String, &(u64, u64))> = map.iter().collect();
    rows.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
    let total: u64 = rows.iter().map(|(_, v)| v.1).sum();
    let mut out = format!(
        "[NATIVE-SLOW] calls over 8us: {} distinct, {:.2}s total
",
        rows.len(),
        total as f64 / 1e9
    );
    for (k, (n, ns)) in rows.into_iter().take(top) {
        out.push_str(&format!(
            "[NATIVE-SLOW]   {:>8.2}s  {:>8} calls  {:>9.0} ns avg  {k}
",
            *ns as f64 / 1e9,
            n,
            *ns as f64 / *n as f64
        ));
    }
    out
}

pub(crate) fn hist_report() -> String {
    use std::sync::atomic::Ordering::Relaxed;
    let counts: Vec<u64> = NATIVE_HIST.iter().map(|c| c.load(Relaxed)).collect();
    let total: u64 = counts.iter().sum();
    if total == 0 {
        return "[NATIVE-HIST] no samples".to_string();
    }
    let mut out = format!("[NATIVE-HIST] {total} timed native calls
");
    let mut cum = 0u64;
    for (i, &n) in counts.iter().enumerate() {
        if n == 0 {
            continue;
        }
        cum += n;
        out.push_str(&format!(
            "[NATIVE-HIST]   {:>9} ns  {:>10}  {:>5.1}%  cum {:>5.1}%
",
            1u64 << i,
            n,
            n as f64 * 100.0 / total as f64,
            cum as f64 * 100.0 / total as f64
        ));
    }
    // The mean is what the boot arithmetic uses; print it beside the distribution that produced
    // it, because the whole point is whether the two tell the same story.
    let approx_total_ns: u128 = counts
        .iter()
        .enumerate()
        .map(|(i, &n)| (n as u128) * ((1u128 << i) + (1u128 << i) / 2))
        .sum();
    out.push_str(&format!(
        "[NATIVE-HIST]   approx mean {:.0} ns, approx total {:.2} s",
        approx_total_ns as f64 / total as f64,
        approx_total_ns as f64 / 1e9
    ));
    out
}

pub(crate) fn try_invoke_chain(
    vm: &mut Vm,
    chain: &[Box<str>],
    extra_owner: Option<&str>,
    label_owner: &str,
    name: &str,
    descriptor: &str,
    args: &[Value],
) -> Result<Option<Option<Value>>, VmError> {
    let _wp = crate::rava_sampler::enter(crate::rava_sampler::NATIVE_BRIDGES);
    let _bsc = crate::rava_sampler::bridge_scope(
        crate::rava_sampler::register_bridge(label_owner, name),
    );
    // ONE READ for this entry point's process constants too (see `bridge_gates`): the tally gate
    // and the canonicalization arm were three acquire loads from three statics per chain call, and
    // this is the OTHER of the two bridge prologues, so it pays them at the same rate.
    //
    // ⭐**AND HERE THE SAVING IS PER LINK, NOT PER CALL.** The loop below runs `dispatch_owner`
    // once per super-chain link (`RAVA_CHAIN_CENSUS`: average chain length 2.00), and each of
    // those entries used to load `gate_mode()` and then `cascade_gates()` of its own — so a
    // two-link chain call paid 1 + 2 + 2 = five acquire loads for one immutable fact. The word is
    // hoisted OUT of the loop: one load, threaded into every link.
    let gates = bridge_gates();
    // One tally entry per LOGICAL call, labelled with the declared owner.
    let _bs = if gates & G_BRIDGESTAT != 0 || gates & G_DISPTLS_LEGACY != 0 {
        bridgestat::enter(label_owner, name)
    } else {
        None
    };
    let _canon = canon_arena_strings(gates, args);
    let args: &[Value] = _canon.as_deref().unwrap_or(args);
    let _na_guard = NativeArgsGuard::new(args);
    // ⛔**THE SUPER-CHAIN WALK DOES NOT RE-RUN THE CASCADE — COUNTED, NOT ASSUMED.** A memo of
    // "which link served" was written here on the theory that a `java/util/Map`-declared site
    // pays a declining pass over `HashMap` and `AbstractMap` before one answers. `RAVA_CHAIN_CENSUS`
    // on `bench/SeamCost3.java`: **12,000,016 calls, link 0 serves 12,000,016 = 100.00%**, average
    // chain length 2.00. There is nothing to skip, and the memo measured `memo_hits=0`. What the
    // count DOES pin is the other reading of `ClassLoader::super_chain`'s 4.0% in the probe's host
    // profile: it is the cache LOOKUP — a hash of a class name per crossing — not the walk.
    // ⭐**TWO MORE PROCESS CONSTANTS, READ ON THE WALK ITSELF (`RAVA_NO_CALLSEQ`).**
    // `chain_census::note_index` and `served_census::note` each OPEN with their own
    // `OnceLock<bool>` — two acquire loads on the SERVED path of every super-chain call, and
    // `note_none` a third on the unserved one — to discover that no census is armed. Both are
    // `G_CENSUS` now: a clear bit is the proof that `chain_census::on()` and
    // `served_census::enabled()` are BOTH false, so all three calls are no-ops and not making
    // them is their behaviour. An armed run sets the bit and every call happens exactly where it
    // did, still re-checking its own flag inside.
    //
    // ⚠**AND THE CHAIN CENSUS WAS DEAD.** `note_call` — the only writer of `CALLS`, which
    // `chain_census::report` divides by and RETURNS EARLY on when zero — had no caller anywhere
    // in the tree, so `RAVA_CHAIN_CENSUS` printed nothing at all and the "avg_len 2.00" the
    // comment above quotes could not have come from this build. Wired here, inside the bit, so a
    // default run still pays nothing for it.
    let census = gates & G_CENSUS != 0;
    if census {
        chain_census::note_call(chain.len());
    }
    for (ci, cn) in chain.iter().enumerate() {
        if let Some(handled) = dispatch_owner(vm, gates, cn, name, descriptor, args)? {
            if !census {
                return Ok(Some(handled));
            }
            chain_census::note_index(ci);
            // WHO ACTUALLY SERVES (RAVA_SERVED_CENSUS, inert otherwise). `bridgestat` above
            // tallies the DECLARED owner of every attempt; this tallies the owner that
            // actually answered. The difference is the question the birth rule cannot decide
            // statically: `birth_native_backed_alloc` force-handles a class because its NAME
            // or NAMESPACE looks native-backed, and whether any native ever serves it is an
            // empirical fact this loop is the only place that knows.
            served_census::note(cn);
            return Ok(Some(handled));
        }
    }
    if census {
        chain_census::note_none();
    }
    match extra_owner {
        Some(o) => dispatch_owner(vm, gates, o, name, descriptor, args),
        None => Ok(None),
    }
}

/// `RAVA_DISPATCH_GATE` — skip sub-dispatchers the owner cannot reach.
const GM_ON: u8 = 1;
/// `RAVA_DISPATCH_GATE_VERIFY` — run the gauntlet anyway and report anything the gate would have
/// wrongly skipped.
const GM_VERIFY: u8 = 2;

/// The two owner-gate process constants as one word (see `dispatch_owner`).
#[inline]
fn gate_mode() -> u8 {
    static W: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *W.get_or_init(|| {
        let mut w = 0u8;
        if dispatch_gate_enabled() {
            w |= GM_ON;
        }
        if dispatch_gate_verify() {
            w |= GM_VERIFY;
        }
        w
    })
}

/// `gates` is the bridge prologue's already-loaded word (see `bridge_gates`). It carries this
/// function's own two constants AND the cascade's, so neither this frame nor the one below it
/// loads a word of its own.
pub(crate) fn dispatch_owner(
    vm: &mut Vm,
    gates: u32,
    owner: &str,
    name: &str,
    descriptor: &str,
    args: &[Value],
) -> Result<Option<Option<Value>>, VmError> {
    // OWNER PLAN (fix:block-c-dispatch-owner). Everything below is one linear
    // cascade: five sub-dispatchers, each with its own `match owner`, then ~40
    // owner tests of this function's own — and `try_invoke_chain` runs the whole
    // thing ONCE PER SUPER-CHAIN ENTRY.
    //
    // Which stages an owner can reach is decidable from the owner. `owner_bits`
    // decides it once per distinct name and hands back a word; every stage below
    // then costs a bit test instead of a call into a dispatcher that will decline.
    // The measured shape (RAVA_DISPATCH_GATE_VERIFY census, ChunkFill, 24.9M
    // cascade entries) is why this is the lever and the whole-cascade skip is not:
    // `java/lang/Boolean#booleanValue` and `java/lang/StringBuilder#append` enter
    // and decline ALL FIVE sub-dispatchers — two of which clone every argument into
    // `gc_protect` before they look at the owner — before reaching their arm.
    //
    // The plan is read from the SAME memo `stream::is_collection` already consults
    // on this path, so a served owner pays no extra lookup; flag OFF sets every bit
    // and the cascade runs exactly as it did.
    use std::sync::atomic::Ordering::Relaxed;
    // ⭐**TWO PROCESS CONSTANTS, ONE READ, AND THE DEFAULT PATH IS A TAIL CALL.**
    // `dispatch_gate_enabled` and `dispatch_gate_verify` are separate `OnceLock<bool>`s and BOTH
    // are default-OFF (the gate measured +0.25% and stayed opt-in), so every native call — and
    // `try_invoke_chain` runs this once PER SUPER-CHAIN LINK, two links on average — paid two
    // loads from two addresses and two branches to learn that neither applies. Folded into one
    // word: the default is a single load, one predicted test, and straight into the cascade with
    // the same arguments the code below would have passed (`plan` all-ones, `verify` false).
    // Nothing here is skipped in the armed configurations; they read the bits back out.
    //
    // ⭐**AND SINCE `RAVA_NO_FAST_CASCADE5` IT IS NOT EVEN THIS FUNCTION'S LOAD.** `gate_mode()`
    // was a THIRD `OnceLock` on a path that had already read `entry_gates()` at the bridge and was
    // about to read `cascade_gates()` one frame down — three statics, one fact, and the last two
    // paid once per SUPER-CHAIN LINK. Its two bits live in the caller's word now (`G_GATE`,
    // `G_GATEV`, set from the very same `gate_mode()` while that word initialises). Under the
    // kill switch the historic read happens here again.
    let mode = if gates & G_LEGACY5 != 0 { gate_mode_bits() } else { gates };
    if mode & (G_GATE | G_GATEV) == 0 {
        return dispatch_owner_cascade(vm, gates, u32::MAX, false, owner, name, descriptor, args);
    }
    let on = mode & G_GATE != 0;
    let verify = mode & G_GATEV != 0;
    if verify {
        GATE_TOTAL.fetch_add(1, Relaxed);
        gate_census_note(owner, name);
        if owner_agnostic_name(name) && !crate::owner_bits::may_serve(owner) {
            GATE_NAME_BLOCKED.fetch_add(1, Relaxed);
        }
        let p = crate::owner_bits::plan(owner);
        let entered = (p & crate::owner_bits::STAGE_ANY).count_ones() as u64;
        GATE_STAGES_SKIPPED.fetch_add(STAGE_COUNT - entered, Relaxed);
    }
    let plan = if on { crate::owner_bits::plan(owner) } else { u32::MAX };
    // Whole-cascade skip: no arm of any stage keys on this owner, and the method is
    // not one of the owner-agnostic names. `Ok(None)` is the only reachable answer.
    if on && plan & crate::owner_bits::DISPATCH_ANY == 0 && no_owner_free_arm_applies(name, args) {
        if !verify {
            return Ok(None);
        }
        GATE_SKIPPED.fetch_add(1, Relaxed);
        if let Some(handled) = dispatch_owner_cascade(vm, gates, u32::MAX, false, owner, name, descriptor, args)? {
            GATE_SERVED.fetch_add(1, Relaxed);
            eprintln!("[DISPATCH-GATE] MISS: {owner}.{name}{descriptor} was served after all");
            return Ok(Some(handled));
        }
        return Ok(None);
    }
    dispatch_owner_cascade(vm, gates, plan, verify, owner, name, descriptor, args)
}

/// Report a stage that served an owner its plan bit had excluded. Only reachable
/// under RAVA_DISPATCH_GATE_VERIFY, which runs every stage regardless of the plan.
#[cold]
pub(crate) fn gate_stage_miss(stage: &str, owner: &str, name: &str, descriptor: &str) {
    GATE_SERVED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    eprintln!("[DISPATCH-GATE] MISS: stage {stage} served {owner}.{name}{descriptor}");
}


/// The SHAPE half of `arena_map_receiver`: is the receiver an arena object?
///
/// That predicate is `arena_map::enabled() && matches!(args.first(), Some(Value::Obj(_)))` — one
/// process constant AND one tag test, and the process constant is `G_ARENAMAP` in the gate word
/// every caller on this path already holds. Splitting it lets the flag be a register test; the
/// two halves are recombined at each site in the SAME order, and `arena_map_receiver` stays the
/// single definition the `debug_assert`s check against.
#[inline]
fn arena_map_recv_shape(args: &[Value]) -> bool {
    #[cfg(feature = "next-gen-gc")]
    {
        return matches!(args.first(), Some(Value::Obj(_)));
    }
    #[cfg(not(feature = "next-gen-gc"))]
    {
        let _ = args;
        false
    }
}

/// Try the arm that served the PREVIOUS native call on this thread. `Ok(None)` means "not this
/// arm" and the caller must run the full cascade. `Err` is a guest exception the arm raised —
/// String's range checks throw, so the shortcut must be able to carry that out.
///
/// `gates` is the cascade's already-loaded word (see `bridge_gates`). It carries the two process
/// constants this function used to read for itself: `G_METER` (was `native_armlookup()`'s
/// `none_armed()` TLS probe, unconditional on ~19.9 M calls) and `G_ARENAMAP` (was
/// `arena_map::enabled()`'s relaxed load inside `arena_map_receiver`).
#[inline]
pub(crate) fn try_last_arm(
    vm: &mut Vm,
    gates: u32,
    owner: &str,
    name: &str,
    descriptor: &str,
    args: &[Value],
) -> Result<Option<Option<Value>>, VmError> {
    // ★ ARMS ABOVE THESE MUST NOT BE SKIPPED. The cascade contains an OWNER-AGNOSTIC arm that
    // claims these three names for ANY owner, and it sits above every arm cached here — so a
    // shortcut that only re-checked the owner would jump past it on e.g. `Integer.compareTo`
    // and serve a different implementation. The gates did not catch this; reading the cascade
    // did. Anything arg-dependent above them is refused for the same reason.
    // ⭐**PRICE THE LOOKUP APART FROM THE ARM.** The shortcut carries 6.42% of the boot in self
    // time over 6.26 M calls, and that number is the sum of two very different things: finding
    // the arm, and running it. Finding it means `arm_key(owner, name)` — **hashing two strings,
    // the class name and the method name, on every native call** — which is one of the three root
    // causes the standing verdict names. This meter separates them.
    //
    // ⭐**AND IT WAS THE FIFTH METER HANDLE ON A PATH THE OTHER FOUR ALREADY GATE.** The cascade
    // folded `native_self`/`native_lastarm`/`native_stages`/`native_services` behind `G_METER` —
    // `diag::any_armed()`, ONE read, and a clear bit PROVES every meter is `armed == false`, so
    // not building the guard IS its behaviour. This one was missed, and it sits one frame below
    // them: every call that reaches the shortcut (its own census: hit=1,867,032 miss=18,011,344)
    // re-asked the same question through `none_armed()`, which on Windows is three dependent TLS
    // loads. Same bit, same proof, and the guard is now `Option`-shaped exactly like the four
    // above — a disarmed `MeterGuard` and a `None` both have a no-op `Drop`, so the `drop(_lk)`
    // below still ends the lookup region at the same statement.
    let _lk = if gates & (G_METER | G_LEGACY5) != 0 {
        Some(crate::diag::native_armlookup().start())
    } else {
        None
    };
    let k = arm_key(owner, name);
    let slot = ((k >> 5) as usize) & (ARM_SLOTS - 1);
    let arm = crate::natives::ARM_CACHE.with(|c| {
        let t = c.borrow();
        if t[slot].0 == k { t[slot].1 } else { ARM_NONE }
    });
    if arm == ARM_NONE {
        return Ok(None); // miss pays only the key and one array read — the guards stay behind it
    }
    // `arena_map_receiver` is `arena_map::enabled() && matches!(args.first(), Some(Value::Obj(_)))`
    // — a relaxed load and a three-way match on a process constant (`RAVA_ARENA_MAPS`, default
    // OFF) in front of a tag test, on every call that gets this far. The flag is a bit of the
    // caller's word now, so the default answer is a register test against a constant and the
    // receiver's tag is never even looked at. Same predicate, same operand order.
    let arena_map_recv = if gates & G_LEGACY5 != 0 {
        arena_map_receiver(args)
    } else {
        gates & G_ARENAMAP != 0 && arena_map_recv_shape(args)
    };
    debug_assert_eq!(arena_map_recv, arena_map_receiver(args));
    if matches!(name, "ordinal" | "name" | "compareTo") || arena_map_recv {
        return Ok(None);
    }
    // A native that wants to THROW is REFUSED here (`Ok(None)`) rather than served: this shortcut
    // exists to skip work, and an exception is not work it can safely own. Refusing sends the call
    // down the full cascade, which raises it at the site that has the receiver, the arguments and
    // the `&mut Vm` the throw needs. Two agents gave the natives an error channel independently;
    // this is the conservative half of the two, and the reviewer's reason for preferring it is
    // that a fast path which can raise has to reproduce every guard the slow path applies first.
    drop(_lk); // the arm itself starts here
    Ok(match arm {
        ARM_INT if owner == "java/lang/Integer" || owner == "java/lang/Long" => {
            // `Option<Result<..>>`: None = not this arm, Err = the native wants to throw. Both
            // refuse here, and the cascade re-decides with the machinery a throw needs.
            match int_static(owner == "java/lang/Long", name, args) {
                Some(Ok(v)) => Some(Some(v)),
                Some(Err(_)) | None => None,
            }
        }
        ARM_STR if owner == "java/lang/String" => str_dispatch(vm, name, descriptor, args)?.map(Some),
        ARM_WRITER
            if owner == "java/io/StringWriter"
                || owner == "java/io/PrintWriter"
                || owner == "java/io/CharArrayWriter"
                || owner == "java/io/BufferedWriter"
                || owner == "java/io/Writer"
                || owner == "java/io/PrintStream"
                || owner == "java/io/FileWriter"
                || owner == "java/io/OutputStreamWriter" =>
        {
            writer_dispatch(vm, owner, name, descriptor, args)?
        }
        ARM_SB if owner == "java/lang/StringBuilder" || owner == "java/lang/StringBuffer" => {
            sb_dispatch(vm, name, descriptor, args)?
        }
        _ => None,
    })
}

/// Binary class name of a runtime value (for `getClass`).
pub(crate) fn class_name_of(vm: &Vm, v: Option<&Value>) -> String {
    match v {
        Some(Value::Ref(h)) => vm.heap.class_of(*h).unwrap_or("java/lang/Object").to_string(),
        // An array's class is "[<element-descriptor>" (JVMS 4.3.2). Without this,
        // getClass() of every array returned java/lang/Object — so snakeyaml's
        // `new Object[0].getClass()` registered RepresentArray under Object's
        // class, matching EVERY object and dumping it as a (self-referential)
        // sequence. isArray()/getComponentType() depend on this too.
        Some(Value::Array(h)) => array_class_name(&vm.heap.array_element(*h).unwrap_or_else(|| std::sync::Arc::from("Ljava/lang/Object;"))),
        // An ARENA object carries its class in its header, not in the handle heap.
        // Without this arm it fell through to the catch-all below and every
        // arena-born object reported `java/lang/Object` — silently, and from the two
        // most-observed methods there are: getClass() and Object.toString(). Found
        // via DensityFunctions$ShiftA/ShiftB/ShiftedNoise/Mapped/MulOrAdd, but the
        // defect is in our value model, not in any one class: it grows with arena
        // coverage, which the real-boot census now puts at 63.79% of live ref cells.
        // Gated like every other arena arm — without the feature no arena exists, so
        // `Value::Obj` never occurs and the catch-all below is correct.
        #[cfg(feature = "next-gen-gc")]
        Some(Value::Obj(p)) => crate::opcodes::arena_array_descriptor(*p).unwrap_or_else(|| {
            crate::nextgen::class_name(crate::nextgen::arena_class_id(*p))
                .map(|s| s.to_string())
                .unwrap_or_else(|| "java/lang/Object".to_string())
        }),
        Some(Value::Str(_)) => "java/lang/String".to_string(),
        Some(Value::Int(_)) => "java/lang/Integer".to_string(),
        Some(Value::Long(_)) => "java/lang/Long".to_string(),
        Some(Value::Float(_)) => "java/lang/Float".to_string(),
        Some(Value::Double(_)) => "java/lang/Double".to_string(),
        _ => "java/lang/Object".to_string(),
    }
}

/// `RAVA_CHAIN_CENSUS` — is the super-chain walk actually re-running the cascade? The memo above
/// only pays if links DECLINE before one serves; if index 0 serves every time there is nothing to
/// skip and the 4.0% of `ClassLoader::super_chain` in the profile is the chain LOOKUP, not the walk.
pub(crate) mod chain_census {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    static CALLS: AtomicU64 = AtomicU64::new(0);
    static LINKS: AtomicU64 = AtomicU64::new(0);
    static HITS: AtomicU64 = AtomicU64::new(0);
    static IDX: [AtomicU64; 8] = [
        AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
        AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
    ];
    static NONE: AtomicU64 = AtomicU64::new(0);
    #[inline]
    pub(crate) fn on() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| {
            std::env::var_os("RAVA_CHAIN_CENSUS").is_some()
                || crate::diag::selected("natives", "chain_calls")
        })
    }
    #[inline]
    pub(crate) fn note_call(len: usize) {
        if on() {
            CALLS.fetch_add(1, Relaxed);
            LINKS.fetch_add(len as u64, Relaxed);
        }
    }
    #[inline]
    pub(crate) fn note_hit() {
        if on() {
            HITS.fetch_add(1, Relaxed);
        }
    }
    #[inline]
    pub(crate) fn note_index(i: usize) {
        if on() {
            IDX[i.min(7)].fetch_add(1, Relaxed);
        }
    }
    #[inline]
    pub(crate) fn note_none() {
        if on() {
            NONE.fetch_add(1, Relaxed);
        }
    }
    /// Calls, for the diagnostics registry.
    pub fn calls() -> u64 {
        CALLS.load(Relaxed)
    }

    pub fn report() {
        if !on() {
            return;
        }
        let c = CALLS.load(Relaxed);
        if c == 0 {
            return;
        }
        eprintln!(
            "[CHAIN] calls={c} links={} avg_len={:.2} memo_hits={} none={}",
            LINKS.load(Relaxed),
            LINKS.load(Relaxed) as f64 / c as f64,
            HITS.load(Relaxed),
            NONE.load(Relaxed)
        );
        for (i, a) in IDX.iter().enumerate() {
            let v = a.load(Relaxed);
            if v > 0 {
                eprintln!("[CHAIN]   served at link {i}: {v} ({:.2}%)", v as f64 * 100.0 / c as f64);
            }
        }
    }
}
